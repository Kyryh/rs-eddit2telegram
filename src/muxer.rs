fn bytes_to_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
fn bytes_to_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}
fn u32_to_bytes(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_be_bytes());
}

fn make_box(name: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + content.len());
    out.extend_from_slice(&((8 + content.len()) as u32).to_be_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(content);
    out
}

fn make_fullbox(name: &[u8; 4], version: u8, content: &[u8]) -> Vec<u8> {
    let mut c = vec![version, 0, 0, 0];
    c.extend_from_slice(content);
    make_box(name, &c)
}

fn bsize(data: &[u8], pos: usize) -> Option<usize> {
    if pos + 8 > data.len() {
        return None;
    }
    Some(match bytes_to_u32(&data[pos..]) as usize {
        1 => {
            if pos + 16 > data.len() {
                return None;
            }
            bytes_to_u64(&data[pos + 8..]) as usize
        }
        0 => data.len() - pos,
        n => n,
    })
}

fn bhdr(data: &[u8], pos: usize) -> usize {
    if bytes_to_u32(&data[pos..]) == 1 {
        16
    } else {
        8
    }
}

fn btype(data: &[u8], pos: usize) -> [u8; 4] {
    data[pos + 4..pos + 8].try_into().unwrap()
}

fn find_box(data: &[u8], from: usize, to: usize, target: &[u8; 4]) -> Option<(usize, usize)> {
    let mut pos = from;
    while pos + 8 <= to {
        let size = bsize(data, pos)?;
        if &btype(data, pos) == target {
            return Some((pos, size));
        }
        pos += size;
    }
    None
}

/// Find the first trak box; returns (trak_pos, trak_size, track_id)
fn find_trak(data: &[u8], moov_pos: usize, moov_size: usize) -> Option<(usize, usize, u32)> {
    let mc = moov_pos + bhdr(data, moov_pos);
    let me = moov_pos + moov_size;
    let mut pos = mc;
    while pos + 8 <= me {
        let size = bsize(data, pos)?;
        if &btype(data, pos) == b"trak" {
            let tc = pos + bhdr(data, pos);
            let te = pos + size;
            if let Some((tkhd_pos, _)) = find_box(data, tc, te, b"tkhd") {
                let c = tkhd_pos + bhdr(data, tkhd_pos);
                // tkhd FullBox: version(1) flags(3) creation(4/8) modification(4/8) track_ID(4)
                let tid_off = c + if data[c] == 0 { 12 } else { 20 };
                return Some((pos, size, bytes_to_u32(&data[tid_off..])));
            }
        }
        pos += size;
    }
    None
}

/// Read duration from tkhd (already in movie timescale).
/// tkhd v0: ver+flags(4)+creation(4)+modification(4)+track_id(4)+reserved(4)+duration(4) → dur at c+20
/// tkhd v1: ver+flags(4)+creation(8)+modification(8)+track_id(4)+reserved(4)+duration(8) → dur at c+28
fn tkhd_duration(data: &[u8], trak_pos: usize, trak_size: usize) -> u64 {
    let tc = trak_pos + bhdr(data, trak_pos);
    let te = trak_pos + trak_size;
    let Some((tkhd_pos, _)) = find_box(data, tc, te, b"tkhd") else {
        return 0;
    };
    let c = tkhd_pos + bhdr(data, tkhd_pos);
    if data[c] == 0 {
        bytes_to_u32(&data[c + 20..]) as u64
    } else {
        bytes_to_u64(&data[c + 28..])
    }
}

// ─── sample extraction ────────────────────────────────────────────────────

struct Sample {
    duration: u32,
    size: u32,
    flags: u32,
    composition_offset: i32,
}

impl Sample {
    fn is_sync(&self) -> bool {
        // sample_is_non_sync_sample is bit 16; clear means sync (keyframe)
        self.flags & 0x00010000 == 0
    }
}

fn parse_trex_defaults(file: &[u8], moov_pos: usize, moov_size: usize) -> (u32, u32, u32) {
    let mc = moov_pos + bhdr(file, moov_pos);
    let me = moov_pos + moov_size;
    if let Some((mvex_pos, mvex_size)) = find_box(file, mc, me, b"mvex") {
        let ec = mvex_pos + bhdr(file, mvex_pos);
        let ee = mvex_pos + mvex_size;
        let mut pos = ec;
        while pos + 8 <= ee {
            if let Some(size) = bsize(file, pos) {
                if &btype(file, pos) == b"trex" {
                    let c = pos + bhdr(file, pos);
                    // trex: ver+flags(4) track_id(4) sdi(4) duration(4) size(4) flags(4)
                    return (
                        bytes_to_u32(&file[c + 12..]),
                        bytes_to_u32(&file[c + 16..]),
                        bytes_to_u32(&file[c + 20..]),
                    );
                }
                pos += size;
            } else {
                break;
            }
        }
    }
    (0, 0, 0)
}

fn extract_samples(
    file: &[u8],
    moov_pos: usize,
    moov_size: usize,
) -> Result<(Vec<Sample>, Vec<u8>), String> {
    let (def_dur, def_sz, def_fl) = parse_trex_defaults(file, moov_pos, moov_size);
    let mut samples = Vec::new();
    let mut media_data = Vec::new();

    let mut pos = 0;
    while pos + 8 <= file.len() {
        let size = bsize(file, pos).ok_or_else(|| format!("bad box at {pos}"))?;
        if &btype(file, pos) == b"moof" {
            let moof_start = pos;
            let next = pos + size;
            if next + 8 > file.len() || &btype(file, next) != b"mdat" {
                pos += size;
                continue;
            }
            let mdat_size = bsize(file, next).ok_or("bad mdat")?;

            let mc = moof_start + bhdr(file, moof_start);
            let me = moof_start + size;
            let mut p = mc;
            while p + 8 <= me {
                let fs = bsize(file, p).ok_or("bad box in moof")?;
                if &btype(file, p) == b"traf" {
                    let tc = p + bhdr(file, p);
                    let te = p + fs;

                    let mut frag_dur = def_dur;
                    let mut frag_sz = def_sz;
                    let mut frag_fl = def_fl;

                    if let Some((tfhd_pos, _)) = find_box(file, tc, te, b"tfhd") {
                        let c = tfhd_pos + bhdr(file, tfhd_pos);
                        let tf = ((file[c + 1] as u32) << 16)
                            | ((file[c + 2] as u32) << 8)
                            | file[c + 3] as u32;
                        let mut o = c + 8;
                        if tf & 0x000001 != 0 {
                            o += 8;
                        } // base-data-offset
                        if tf & 0x000002 != 0 {
                            o += 4;
                        } // sample-description-index
                        if tf & 0x000008 != 0 {
                            frag_dur = bytes_to_u32(&file[o..]);
                            o += 4;
                        }
                        if tf & 0x000010 != 0 {
                            frag_sz = bytes_to_u32(&file[o..]);
                            o += 4;
                        }
                        if tf & 0x000020 != 0 {
                            frag_fl = bytes_to_u32(&file[o..]);
                        }
                    }

                    if let Some((trun_pos, _)) = find_box(file, tc, te, b"trun") {
                        let c = trun_pos + bhdr(file, trun_pos);
                        let tf = ((file[c + 1] as u32) << 16)
                            | ((file[c + 2] as u32) << 8)
                            | file[c + 3] as u32;
                        let count = bytes_to_u32(&file[c + 4..]) as usize;
                        let mut o = c + 8;

                        // data_offset is relative to start of moof
                        let data_off: i64 = if tf & 0x001 != 0 {
                            let v = bytes_to_u32(&file[o..]) as i32 as i64;
                            o += 4;
                            v
                        } else {
                            0
                        };
                        let first_flags = if tf & 0x004 != 0 {
                            let v = bytes_to_u32(&file[o..]);
                            o += 4;
                            v
                        } else {
                            0
                        };

                        let mut cur = (moof_start as i64 + data_off) as usize;

                        for i in 0..count {
                            let dur = if tf & 0x100 != 0 {
                                let v = bytes_to_u32(&file[o..]);
                                o += 4;
                                v
                            } else {
                                frag_dur
                            };
                            let sz = if tf & 0x200 != 0 {
                                let v = bytes_to_u32(&file[o..]);
                                o += 4;
                                v
                            } else {
                                frag_sz
                            };
                            let fl = if tf & 0x400 != 0 {
                                let v = bytes_to_u32(&file[o..]);
                                o += 4;
                                v
                            } else if i == 0 && tf & 0x004 != 0 {
                                first_flags
                            } else {
                                frag_fl
                            };
                            let cts = if tf & 0x800 != 0 {
                                let v = bytes_to_u32(&file[o..]) as i32;
                                o += 4;
                                v
                            } else {
                                0
                            };

                            media_data.extend_from_slice(&file[cur..cur + sz as usize]);
                            samples.push(Sample {
                                duration: dur,
                                size: sz,
                                flags: fl,
                                composition_offset: cts,
                            });
                            cur += sz as usize;
                        }
                    }
                }
                p += fs;
            }
            pos = next + mdat_size;
        } else {
            pos += size;
        }
    }
    Ok((samples, media_data))
}

// ─── sample table builders ────────────────────────────────────────────────

fn build_stts(samples: &[Sample]) -> Vec<u8> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for s in samples {
        match runs.last_mut() {
            Some(r) if r.1 == s.duration => r.0 += 1,
            _ => runs.push((1, s.duration)),
        }
    }
    let mut c = (runs.len() as u32).to_be_bytes().to_vec();
    for (n, d) in &runs {
        c.extend_from_slice(&n.to_be_bytes());
        c.extend_from_slice(&d.to_be_bytes());
    }
    make_fullbox(b"stts", 0, &c)
}

fn build_ctts(samples: &[Sample]) -> Option<Vec<u8>> {
    if samples.iter().all(|s| s.composition_offset == 0) {
        return None;
    }
    let mut runs: Vec<(u32, i32)> = Vec::new();
    for s in samples {
        match runs.last_mut() {
            Some(r) if r.1 == s.composition_offset => r.0 += 1,
            _ => runs.push((1, s.composition_offset)),
        }
    }
    let ver = u8::from(runs.iter().any(|(_, o)| *o < 0));
    let mut c = (runs.len() as u32).to_be_bytes().to_vec();
    for (n, o) in &runs {
        c.extend_from_slice(&n.to_be_bytes());
        c.extend_from_slice(&(*o as u32).to_be_bytes());
    }
    Some(make_fullbox(b"ctts", ver, &c))
}

fn build_stss(samples: &[Sample]) -> Vec<u8> {
    let idxs: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_sync())
        .map(|(i, _)| (i + 1) as u32)
        .collect();
    let mut c = (idxs.len() as u32).to_be_bytes().to_vec();
    for i in &idxs {
        c.extend_from_slice(&i.to_be_bytes());
    }
    make_fullbox(b"stss", 0, &c)
}

fn build_stsc(n_samples: u32) -> Vec<u8> {
    // 0 samples → 0 chunks → 0 entries; otherwise 1 chunk containing all samples
    let mut c = Vec::new();
    if n_samples > 0 {
        c.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
        c.extend_from_slice(&1u32.to_be_bytes()); // first_chunk = 1
        c.extend_from_slice(&n_samples.to_be_bytes());
        c.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index = 1
    } else {
        c.extend_from_slice(&0u32.to_be_bytes()); // entry_count = 0
    }
    make_fullbox(b"stsc", 0, &c)
}

fn build_stsz(samples: &[Sample]) -> Vec<u8> {
    let first = samples.first().map_or(0, |s| s.size);
    let uniform = samples.iter().all(|s| s.size == first);
    let mut c = Vec::new();
    c.extend_from_slice(&(if uniform { first } else { 0 }).to_be_bytes());
    c.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    if !uniform {
        for s in samples {
            c.extend_from_slice(&s.size.to_be_bytes());
        }
    }
    make_fullbox(b"stsz", 0, &c)
}

fn build_stco(offset: u32, has_chunk: bool) -> Vec<u8> {
    let mut c = Vec::new();
    if has_chunk {
        c.extend_from_slice(&1u32.to_be_bytes());
        c.extend_from_slice(&offset.to_be_bytes());
    } else {
        c.extend_from_slice(&0u32.to_be_bytes());
    }
    make_fullbox(b"stco", 0, &c)
}

// ─── dimension helpers ───────────────────────────────────────────────────

/// Read video width/height from the first entry in stsd (VisualSampleEntry).
/// VisualSampleEntry layout after its 8-byte box header:
///   6 reserved + 2 data_ref_index + 2 pre_defined + 2 reserved + 12 pre_defined = 24 bytes
///   then width(2) + height(2)
fn read_stsd_dimensions(data: &[u8], trak_pos: usize, trak_size: usize) -> Option<(u16, u16)> {
    let tc = trak_pos + bhdr(data, trak_pos);
    let te = trak_pos + trak_size;
    let (mdia_p, mdia_s) = find_box(data, tc, te, b"mdia")?;
    let mc = mdia_p + bhdr(data, mdia_p);
    let (minf_p, minf_s) = find_box(data, mc, mdia_p + mdia_s, b"minf")?;
    let mic = minf_p + bhdr(data, minf_p);
    let (stbl_p, stbl_s) = find_box(data, mic, minf_p + minf_s, b"stbl")?;
    let sbc = stbl_p + bhdr(data, stbl_p);
    let (stsd_p, _) = find_box(data, sbc, stbl_p + stbl_s, b"stsd")?;
    // stsd FullBox: box_header(8) + ver+flags(4) + entry_count(4) = 16 bytes total
    let ep = stsd_p + 16; // first entry start
    if ep + 34 > data.len() {
        return None;
    }
    let ec = ep + 8; // entry content (past box header)
    let w = u16::from_be_bytes(data[ec + 24..ec + 26].try_into().ok()?);
    let h = u16::from_be_bytes(data[ec + 26..ec + 28].try_into().ok()?);
    if w > 0 && h > 0 { Some((w, h)) } else { None }
}

// ─── timescale helpers ───────────────────────────────────────────────────

fn read_mvhd_timescale(data: &[u8], moov_pos: usize, moov_size: usize) -> u32 {
    let mc = moov_pos + bhdr(data, moov_pos);
    let me = moov_pos + moov_size;
    let Some((p, _)) = find_box(data, mc, me, b"mvhd") else {
        return 90000;
    };
    let c = p + bhdr(data, p);
    if data[c] == 0 {
        bytes_to_u32(&data[c + 12..])
    } else {
        bytes_to_u32(&data[c + 20..])
    }
}

fn read_mdhd_timescale(data: &[u8], trak_pos: usize, trak_size: usize) -> u32 {
    let tc = trak_pos + bhdr(data, trak_pos);
    let te = trak_pos + trak_size;
    let Some((mdia_p, mdia_s)) = find_box(data, tc, te, b"mdia") else {
        return 90000;
    };
    let mc = mdia_p + bhdr(data, mdia_p);
    let me = mdia_p + mdia_s;
    let Some((p, _)) = find_box(data, mc, me, b"mdhd") else {
        return 90000;
    };
    let c = p + bhdr(data, p);
    if data[c] == 0 {
        bytes_to_u32(&data[c + 12..])
    } else {
        bytes_to_u32(&data[c + 20..])
    }
}

fn read_mdhd_duration(data: &[u8], trak_pos: usize, trak_size: usize) -> u64 {
    let tc = trak_pos + bhdr(data, trak_pos);
    let te = trak_pos + trak_size;
    let Some((mdia_p, mdia_s)) = find_box(data, tc, te, b"mdia") else {
        return 0;
    };
    let mc = mdia_p + bhdr(data, mdia_p);
    let me = mdia_p + mdia_s;
    let Some((p, _)) = find_box(data, mc, me, b"mdhd") else {
        return 0;
    };
    let c = p + bhdr(data, p);
    if data[c] == 0 {
        bytes_to_u32(&data[c + 16..]) as u64
    } else {
        bytes_to_u64(&data[c + 24..])
    }
}

// ─── trak rebuilder ───────────────────────────────────────────────────────

fn rebuild_stbl(
    data: &[u8],
    pos: usize,
    size: usize,
    samples: &[Sample],
    chunk_off: u32,
    is_video: bool,
) -> Result<Vec<u8>, String> {
    let c = pos + bhdr(data, pos);
    let e = pos + size;
    let (stsd_pos, stsd_size) = find_box(data, c, e, b"stsd").ok_or("no stsd in stbl")?;
    let mut content = data[stsd_pos..stsd_pos + stsd_size].to_vec();
    content.extend_from_slice(&build_stts(samples));
    if let Some(ctts) = build_ctts(samples) {
        content.extend_from_slice(&ctts);
    }
    if is_video {
        content.extend_from_slice(&build_stss(samples));
    }
    content.extend_from_slice(&build_stsc(samples.len() as u32));
    content.extend_from_slice(&build_stsz(samples));
    content.extend_from_slice(&build_stco(chunk_off, !samples.is_empty()));
    Ok(make_box(b"stbl", &content))
}

fn rebuild_minf(
    data: &[u8],
    pos: usize,
    size: usize,
    samples: &[Sample],
    chunk_off: u32,
    is_video: bool,
) -> Result<Vec<u8>, String> {
    let c = pos + bhdr(data, pos);
    let e = pos + size;
    let mut content = Vec::new();
    // Spec order: media_header (vmhd/smhd) → dinf → stbl.
    // Walk the source three times to enforce this regardless of source ordering.
    for pass in 0..3usize {
        let mut p = c;
        while p + 8 <= e {
            let s = bsize(data, p).ok_or("bad box in minf")?;
            let t = btype(data, p);
            let is_mhd = matches!(&t, b"vmhd" | b"smhd" | b"nmhd" | b"hmhd" | b"sthd");
            match pass {
                0 if is_mhd => content.extend_from_slice(&data[p..p + s]),
                1 if !is_mhd && &t != b"stbl" => content.extend_from_slice(&data[p..p + s]),
                2 if &t == b"stbl" => content
                    .extend_from_slice(&rebuild_stbl(data, p, s, samples, chunk_off, is_video)?),
                _ => {}
            }
            p += s;
        }
    }
    Ok(make_box(b"minf", &content))
}

fn rebuild_mdia(
    data: &[u8],
    pos: usize,
    size: usize,
    samples: &[Sample],
    chunk_off: u32,
    is_video: bool,
    media_dur: u64,
) -> Result<Vec<u8>, String> {
    let c = pos + bhdr(data, pos);
    let e = pos + size;
    let mut content = Vec::new();
    let mut p = c;
    while p + 8 <= e {
        let s = bsize(data, p).ok_or("bad box in mdia")?;
        match &btype(data, p) {
            b"mdhd" => {
                let mut mdhd = data[p..p + s].to_vec();
                let hc = bhdr(&mdhd, 0);
                let ver = mdhd[hc];
                // mdhd v0: dur at hc+16 (4); v1: dur at hc+24 (8)
                if ver == 0 {
                    u32_to_bytes(&mut mdhd[hc + 16..], media_dur as u32);
                } else {
                    mdhd[hc + 24..hc + 32].copy_from_slice(&media_dur.to_be_bytes());
                }
                content.extend_from_slice(&mdhd);
            }
            b"minf" => {
                content.extend_from_slice(&rebuild_minf(data, p, s, samples, chunk_off, is_video)?);
            }
            _ => content.extend_from_slice(&data[p..p + s]),
        }
        p += s;
    }
    Ok(make_box(b"mdia", &content))
}

/// Rebuild a trak: updates track_id and duration in tkhd, updates mdhd duration, rebuilds stbl.
fn rebuild_trak(
    data: &[u8],
    pos: usize,
    size: usize,
    new_id: u32,
    samples: &[Sample],
    chunk_off: u32,
    is_video: bool,
    tkhd_dur: u64,
    media_dur: u64,
) -> Result<Vec<u8>, String> {
    let c = pos + bhdr(data, pos);
    let e = pos + size;
    // Collect into separate buffers so we can enforce spec order:
    // tkhd → edts → mdia → (everything else)
    let mut tkhd_out = Vec::new();
    let mut edts_out = Vec::new();
    let mut mdia_out = Vec::new();
    let mut rest_out = Vec::new();
    let mut p = c;
    while p + 8 <= e {
        let s = bsize(data, p).ok_or("bad box in trak")?;
        match &btype(data, p) {
            b"tkhd" => {
                let mut tkhd = data[p..p + s].to_vec();
                let hc = bhdr(&tkhd, 0);
                let ver = tkhd[hc];
                let tid_off = hc + if ver == 0 { 12 } else { 20 };
                let dur_off = hc + if ver == 0 { 20 } else { 28 };
                let (w_off, h_off) = if ver == 0 {
                    (hc + 76, hc + 80)
                } else {
                    (hc + 88, hc + 92)
                };
                u32_to_bytes(&mut tkhd[tid_off..], new_id);
                if ver == 0 {
                    u32_to_bytes(&mut tkhd[dur_off..], tkhd_dur as u32);
                } else {
                    tkhd[dur_off..dur_off + 8].copy_from_slice(&tkhd_dur.to_be_bytes());
                }
                if is_video {
                    let cw = bytes_to_u32(&tkhd[w_off..]);
                    let ch = bytes_to_u32(&tkhd[h_off..]);

                    if (cw == 0 || ch == 0)
                        && let Some((sw, sh)) = read_stsd_dimensions(data, pos, size)
                    {
                        u32_to_bytes(&mut tkhd[w_off..], (sw as u32) << 16);
                        u32_to_bytes(&mut tkhd[h_off..], (sh as u32) << 16);
                    }
                }
                tkhd_out.extend_from_slice(&tkhd);
            }
            b"edts" => {
                // Fix elst segment_duration: source fMP4 often has 0 ("unknown").
                let ec = p + bhdr(data, p);
                let mut ec_buf = Vec::new();
                let mut ep = ec;
                while ep + 8 <= p + s {
                    let es = bsize(data, ep).ok_or("bad box in edts")?;
                    if &btype(data, ep) == b"elst" {
                        let mut elst = data[ep..ep + es].to_vec();
                        let lc = bhdr(&elst, 0);
                        let ver = elst[lc];
                        let n = bytes_to_u32(&elst[lc + 4..]) as usize;
                        let entry_size: usize = if ver == 0 { 12 } else { 20 };
                        let mut o = lc + 8;
                        for _ in 0..n {
                            let mt: i64 = if ver == 0 {
                                i32::from_be_bytes(elst[o + 4..o + 8].try_into().unwrap()) as i64
                            } else {
                                i64::from_be_bytes(elst[o + 8..o + 16].try_into().unwrap())
                            };
                            if mt >= 0 {
                                if ver == 0 {
                                    elst[o..o + 4]
                                        .copy_from_slice(&(tkhd_dur as u32).to_be_bytes());
                                } else {
                                    elst[o..o + 8].copy_from_slice(&tkhd_dur.to_be_bytes());
                                }
                            }
                            o += entry_size;
                        }
                        ec_buf.extend_from_slice(&elst);
                    } else {
                        ec_buf.extend_from_slice(&data[ep..ep + es]);
                    }
                    ep += es;
                }
                edts_out.extend_from_slice(&make_box(b"edts", &ec_buf));
            }
            b"mdia" => {
                mdia_out.extend_from_slice(&rebuild_mdia(
                    data, p, s, samples, chunk_off, is_video, media_dur,
                )?);
            }
            _ => rest_out.extend_from_slice(&data[p..p + s]),
        }
        p += s;
    }
    // Assemble in spec order: tkhd → edts → mdia → rest
    let mut content =
        Vec::with_capacity(tkhd_out.len() + edts_out.len() + mdia_out.len() + rest_out.len());
    content.extend_from_slice(&tkhd_out);
    content.extend_from_slice(&edts_out);
    content.extend_from_slice(&mdia_out);
    content.extend_from_slice(&rest_out);
    Ok(make_box(b"trak", &content))
}

fn assemble_moov(
    video: &[u8],
    vtp: usize,
    vts: usize,
    v_samples: &[Sample],
    v_off: u32,
    v_tkhd_dur: u64,
    v_media_dur: u64,
    audio: &[u8],
    atp: usize,
    ats: usize,
    a_samples: &[Sample],
    a_off: u32,
    a_tkhd_dur: u64,
    a_media_dur: u64,
    mvhd_pos: usize,
    mvhd_size: usize,
    movie_dur: u64,
) -> Result<Vec<u8>, String> {
    // mvhd v0: ver+flags(4)+creation(4)+modification(4)+timescale(4)+duration(4) → dur at hc+16, nti at hc+96
    // mvhd v1: ver+flags(4)+creation(8)+modification(8)+timescale(4)+duration(8) → dur at hc+24, nti at hc+108
    let mut mvhd = video[mvhd_pos..mvhd_pos + mvhd_size].to_vec();
    let hc = bhdr(&mvhd, 0);
    let ver = mvhd[hc];
    if ver == 0 {
        u32_to_bytes(&mut mvhd[hc + 16..], movie_dur as u32);
        u32_to_bytes(&mut mvhd[hc + 96..], 3);
    } else {
        mvhd[hc + 24..hc + 32].copy_from_slice(&movie_dur.to_be_bytes());
        u32_to_bytes(&mut mvhd[hc + 108..], 3);
    }

    let vtrak = rebuild_trak(
        video,
        vtp,
        vts,
        1,
        v_samples,
        v_off,
        true,
        v_tkhd_dur,
        v_media_dur,
    )?;
    let atrak = rebuild_trak(
        audio,
        atp,
        ats,
        2,
        a_samples,
        a_off,
        false,
        a_tkhd_dur,
        a_media_dur,
    )?;

    let mut content = Vec::new();
    content.extend_from_slice(&mvhd);
    content.extend_from_slice(&vtrak);
    content.extend_from_slice(&atrak);
    Ok(make_box(b"moov", &content))
}

pub fn mux_video_audio(video: &[u8], audio: &[u8]) -> Result<Vec<u8>, String> {
    if audio.is_empty() {
        return Ok(video.to_vec());
    }
    let (vm_pos, vm_size) =
        find_box(&video, 0, video.len(), b"moov").ok_or("no moov in VIDEO.mp4")?;
    let (am_pos, am_size) =
        find_box(&audio, 0, audio.len(), b"moov").ok_or("no moov in AUDIO.mp4")?;

    let (vtp, vts, _) = find_trak(&video, vm_pos, vm_size).ok_or("no trak in VIDEO.mp4")?;
    let (atp, ats, _) = find_trak(&audio, am_pos, am_size).ok_or("no trak in AUDIO.mp4")?;

    let vc = vm_pos + bhdr(&video, vm_pos);
    let (mvhd_pos, mvhd_size) = find_box(&video, vc, vm_pos + vm_size, b"mvhd").ok_or("no mvhd")?;

    let (v_samples, v_data) = extract_samples(&video, vm_pos, vm_size)?;
    let (a_samples, a_data) = extract_samples(&audio, am_pos, am_size)?;

    let mvhd_ts = read_mvhd_timescale(&video, vm_pos, vm_size);
    let v_mdhd_ts = read_mdhd_timescale(&video, vtp, vts).max(1);
    let a_mdhd_ts = read_mdhd_timescale(&audio, atp, ats).max(1);

    let v_media_dur: u64 = v_samples.iter().map(|s| s.duration as u64).sum();
    let a_media_dur: u64 = a_samples.iter().map(|s| s.duration as u64).sum();

    let v_tkhd_dur = {
        let c = v_media_dur * mvhd_ts as u64 / v_mdhd_ts as u64;
        if c > 0 {
            c
        } else {
            tkhd_duration(&video, vtp, vts)
                .max(read_mdhd_duration(&video, vtp, vts) * mvhd_ts as u64 / v_mdhd_ts as u64)
        }
    };
    let a_tkhd_dur = {
        let c = a_media_dur * mvhd_ts as u64 / a_mdhd_ts as u64;
        if c > 0 {
            c
        } else {
            tkhd_duration(&audio, atp, ats)
                .max(read_mdhd_duration(&audio, atp, ats) * mvhd_ts as u64 / a_mdhd_ts as u64)
        }
    };
    let movie_dur = v_tkhd_dur.max(a_tkhd_dur);

    // The source ftyp is for a fragmented MP4 (iso5/iso6/dash brands).
    // Using it on a progressive file makes Telegram's parser think the file
    // is a corrupt fMP4 (expected moof boxes, found none).
    // Use a standard progressive ftyp instead.
    let ftyp = {
        let mut f = Vec::new();
        f.extend_from_slice(b"mp42");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(b"mp42");
        f.extend_from_slice(b"isom");
        f.extend_from_slice(b"mp41");
        make_box(b"ftyp", &f)
    };

    // First pass: build moov with placeholder offsets to determine its size
    let moov_draft = assemble_moov(
        &video,
        vtp,
        vts,
        &v_samples,
        0,
        v_tkhd_dur,
        v_media_dur,
        &audio,
        atp,
        ats,
        &a_samples,
        0,
        a_tkhd_dur,
        a_media_dur,
        mvhd_pos,
        mvhd_size,
        movie_dur,
    )?;

    // mdat = 8-byte header + video data + audio data; video chunk comes first
    let v_off = (ftyp.len() + moov_draft.len() + 8) as u32;
    let a_off = v_off + v_data.len() as u32;

    // Second pass: build moov with correct chunk offsets
    let moov = assemble_moov(
        &video,
        vtp,
        vts,
        &v_samples,
        v_off,
        v_tkhd_dur,
        v_media_dur,
        &audio,
        atp,
        ats,
        &a_samples,
        a_off,
        a_tkhd_dur,
        a_media_dur,
        mvhd_pos,
        mvhd_size,
        movie_dur,
    )?;
    let mdat_size = 8 + v_data.len() + a_data.len();
    let mut out = Vec::with_capacity(ftyp.len() + moov.len() + mdat_size);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&(mdat_size as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(&v_data);
    out.extend_from_slice(&a_data);

    Ok(out)
}
