use std::{collections::HashMap, env, fs, ops::Deref, path::PathBuf};

use crate::err::Error;

mod db;
mod err;
mod muxer;
mod re;
mod tg;

fn block_on<F>(future: F) -> F::Output
where
    F: Future,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

// Tracks which HTML tags are currently open, accumulating opens/closes from `s`.
// Each entry is (tag_name_lowercase, full_opening_tag) to preserve attributes.
fn update_open_tags(open_tags: &mut Vec<(String, String)>, s: &str) {
    const VOID: &[&str] = &[
        "br", "hr", "img", "input", "link", "meta", "area", "base", "col", "embed", "param",
        "source", "track", "wbr",
    ];
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'<' {
            if let Some(rel_end) = s[i + 1..].find('>') {
                let tag_content = &s[i + 1..i + 1 + rel_end];
                if tag_content.starts_with('/') {
                    let name = tag_content[1..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_lowercase();
                    if let Some(pos) = open_tags.iter().rposition(|(n, _)| n == &name) {
                        open_tags.remove(pos);
                    }
                } else if !tag_content.ends_with('/') && !tag_content.starts_with('!') {
                    let name = tag_content
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_lowercase();
                    if !name.is_empty() && !VOID.contains(&name.as_str()) {
                        open_tags.push((name, format!("<{}>", tag_content)));
                    }
                }
                i += rel_end + 2;
                continue;
            }
        }
        i += 1;
    }
}

// Returns how many bytes of `s` are safe to keep: splits at the last whitespace
// outside any tag, and never splits in the middle of an unclosed `<tag`.
fn safe_split_len(s: &str) -> usize {
    let mut in_tag = false;
    let mut last_ws: Option<usize> = None;
    let mut unclosed_tag_start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match c {
            '<' => {
                in_tag = true;
                unclosed_tag_start = Some(i);
            }
            '>' => {
                in_tag = false;
                unclosed_tag_start = None;
            }
            c if c.is_whitespace() && !in_tag => last_ws = Some(i),
            _ => {}
        }
    }
    let max_end = unclosed_tag_start.unwrap_or(s.len());
    last_ws.filter(|&ws| ws < max_end).unwrap_or(max_end)
}

fn textwrap(string: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut context_tags: Vec<(String, String)> = Vec::new();
    while start < string.len() {
        let end = (start + limit).min(string.len());
        let chunk = &string[start..end];
        let split = if end == string.len() {
            end - start
        } else {
            let s = safe_split_len(chunk);
            if s > 0 {
                s
            } else {
                // The window starts with a tag longer than limit (e.g. <a href="very-long-url">).
                // Extend past the tag's closing '>' so we never advance into tag internals.
                string[start..].find('>').map_or(end - start, |e| e + 1)
            }
        };
        let piece = string[start..start + split].trim();
        if !piece.is_empty() {
            let prefix: String = context_tags.iter().map(|(_, t)| t.as_str()).collect();
            let mut next_tags = context_tags.clone();
            update_open_tags(&mut next_tags, piece);
            let mut full_piece = format!("{}{}", prefix, piece);
            for (name, _) in next_tags.iter().rev() {
                full_piece.push_str(&format!("</{}>", name));
            }
            chunks.push(full_piece);
            context_tags = next_tags;
        }
        start += split.max(1);
        while start < string.len() && string.as_bytes()[start].is_ascii_whitespace() {
            start += 1;
        }
    }
    chunks
}

fn shorten(string: &str, limit: usize) -> String {
    if string.len() <= limit {
        return string.to_owned();
    }
    let truncated = &string[..limit];
    let split = safe_split_len(truncated);
    let piece = truncated[..split].trim_end();
    let mut open_tags: Vec<(String, String)> = Vec::new();
    update_open_tags(&mut open_tags, piece);
    let mut result = piece.to_owned();
    for (name, _) in open_tags.iter().rev() {
        result.push_str(&format!("</{}>", name));
    }
    result.push_str(" [...]");
    result
}

async fn send_selftext_gallery_post(
    tg_client: &tg::TelegramClient,
    re_client: &re::RedditClient,
    submission: re::Submission,
    poster: &mut Poster<'_>,
    chat: String,
) -> Result<(), Error> {
    let text = poster.get_text(&submission, false)?;
    let text_messages = textwrap(&text, 4096);

    for text in text_messages {
        tg_client.send_message(chat.clone(), text).await?;
    }

    let spoiler = poster.should_hide(&submission)?;

    // send multiple medias if this is a gallery post
    if let Some(mut m) = submission.media_metadata {
        let mut media_group = Vec::new();

        if let Some(g) = submission.gallery_data {
            for data in g.items {
                let caption = data.caption.unwrap_or_default();
                let id = data.media_id;

                let media = m.remove(&id);
                match media {
                    Some(media) => {
                        media_group.push(
                            tg::InputMedia::from_reddit_media_metadata(
                                &re_client, media, id, caption, spoiler,
                            )
                            .await?,
                        );
                    }
                    None => {
                        println!("This shouldn't happen");
                    }
                }
            }
        }

        for (id, media) in m {
            media_group.push(
                tg::InputMedia::from_reddit_media_metadata(
                    &re_client,
                    media,
                    id,
                    "".to_owned(),
                    spoiler,
                )
                .await?,
            );
        }

        for medias in media_group.chunks_mut(10) {
            tg_client.send_media_group(chat.clone(), medias).await?;
        }
    }
    Ok(())
}

async fn send_video_post(
    tg_client: &tg::TelegramClient,
    re_client: &re::RedditClient,
    submission: re::Submission,
    poster: &mut Poster<'_>,
    chat: String,
) -> Result<(), Error> {
    if let Some(re::Media::Video { ref reddit_video }) = submission.media {
        let mpd = re_client.get_dash_info(&reddit_video.dash_url).await?;
        let (mut videos, audios) = re_client
            .get_video_audio_streams(mpd, &submission.url)
            .await?;

        let audio = audios.last().map(|s| s.as_str()).unwrap_or_default();
        let audio_size = if audio.is_empty() {
            0
        } else {
            re_client.get_file_size(audio).await?
        };

        let video = loop {
            if let Some(video) = videos.pop() {
                if audio_size + re_client.get_file_size(&video).await? < 50_000_000 {
                    break video;
                }
            } else {
                return Err(Error::Custom(
                    "No video found smaller than 50 MB".to_owned(),
                ));
            }
        };
        let muxed_video = muxer::mux_video_audio(
            &re_client.download_file(&video).await?,
            &(if audio.is_empty() {
                vec![]
            } else {
                re_client.download_file(&audio).await?
            }),
        )?;

        let thumbnail = {
            if let Some(preview) = &submission.preview
                && let Some(url) = &preview.images[0].source.url
                && let Some(thumb) = tg::TelegramMedia::from_url(&re_client, url).await?
            {
                Some(thumb)
            } else {
                None
            }
        };

        tg_client
            .send_video(
                chat,
                tg::TelegramMedia::new(muxed_video, format!("{}.mp4", submission.id)),
                poster.get_text(&submission, true)?,
                poster.should_hide(&submission)?,
                reddit_video.duration,
                reddit_video.width,
                reddit_video.height,
                thumbnail,
            )
            .await?;
        Ok(())
    } else {
        Err(Error::Custom(
            "Embed videos not supported (for now?)\nNot even sure this is reachable tbh".to_owned(),
        ))
    }
}

async fn send_image_post(
    tg_client: &tg::TelegramClient,
    re_client: &re::RedditClient,
    submission: re::Submission,
    poster: &mut Poster<'_>,
    chat: String,
) -> Result<(), Error> {
    if let Some(mut photo) = tg::TelegramMedia::from_url(&re_client, &submission.url).await?
        && let Ok(()) = photo.downscale_photo()
    {
        tg_client
            .send_photo(
                chat,
                photo,
                poster.get_text(&submission, true)?,
                poster.should_hide(&submission)?,
            )
            .await?;
        Ok(())
    } else {
        Err(Error::Custom(format!(
            "Invalid submission: {:?}",
            submission
        )))
    }
}

async fn send_gif_post(
    tg_client: &tg::TelegramClient,
    re_client: &re::RedditClient,
    submission: re::Submission,
    poster: &mut Poster<'_>,
    chat: String,
) -> Result<(), Error> {
    if let Some(gif) = tg::TelegramMedia::from_url(&re_client, &submission.url).await? {
        let (width, height, thumbnail) = {
            if let Some(preview) = &submission.preview
                && let Some(url) = &preview.images[0].source.url
                && let Some(thumb) = tg::TelegramMedia::from_url(&re_client, url).await?
            {
                (
                    Some(preview.images[0].source.width),
                    Some(preview.images[0].source.height),
                    Some(thumb),
                )
            } else {
                (None, None, None)
            }
        };

        tg_client
            .send_animation(
                chat,
                gif,
                poster.get_text(&submission, true)?,
                poster.should_hide(&submission)?,
                width,
                height,
                thumbnail,
            )
            .await?;
        Ok(())
    } else {
        Err(Error::Custom(format!(
            "Invalid submission: {:?}",
            submission
        )))
    }
}

struct Poster<'a> {
    rhai: rhai::Engine,
    ast: rhai::AST,
    scope: rhai::Scope<'a>,
}

impl Poster<'_> {
    pub fn new(script_path: PathBuf) -> Result<Self, Box<rhai::EvalAltResult>> {
        let mut rhai = rhai::Engine::new();

        rhai.register_fn("is_spoiler", re::Submission::is_spoiler)
            .register_fn("is_nsfw", re::Submission::is_nsfw)
            .register_fn("text", re::Submission::text)
            .register_fn("title", re::Submission::title)
            .register_fn("flair", re::Submission::flair)
            .register_fn("url", re::Submission::url)
            .register_fn("score", re::Submission::score);

        let ast = rhai.compile_file(script_path)?;

        let scope = rhai::Scope::new();

        Ok(Self { rhai, ast, scope })
    }

    pub fn should_hide(
        &mut self,
        submission: &re::Submission,
    ) -> Result<bool, Box<rhai::EvalAltResult>> {
        self.rhai.call_fn::<bool>(
            &mut self.scope,
            &self.ast,
            "should_hide",
            (submission.clone(),),
        )
    }

    pub fn should_post(
        &mut self,
        submission: &re::Submission,
    ) -> Result<bool, Box<rhai::EvalAltResult>> {
        self.rhai.call_fn::<bool>(
            &mut self.scope,
            &self.ast,
            "should_post",
            (submission.clone(),),
        )
    }

    pub fn get_text(
        &mut self,
        submission: &re::Submission,
        short: bool,
    ) -> Result<String, Box<rhai::EvalAltResult>> {
        self.rhai.call_fn::<String>(
            &mut self.scope,
            &self.ast,
            "get_text",
            (submission.clone(), short),
        )
    }

    pub fn get_consts(&self) -> HashMap<String, String> {
        self.ast
            .iter_literal_variables(true, false)
            .map(|(name, _, value)| (name.to_owned(), value.to_string()))
            .collect::<HashMap<_, _>>()
    }
}

fn get_posters<'a>() -> Result<Vec<Poster<'a>>, Error> {
    let mut posters = Vec::new();
    for entry in fs::read_dir("posters")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && let Some(extension) = path.extension()
            && extension == "rhai"
        {
            posters.push(Poster::new(path)?);
        }
    }
    if posters.is_empty() {
        println!("No poster detected");
    }
    Ok(posters)
}

fn main() -> Result<(), Error> {
    block_on(async {
        dotenv::dotenv().unwrap();

        let db = db::PostedSubmissions::new("db.sqlite")?;

        let mut re_client = re::RedditClient::new(
            "kyryh/reddit2telegram",
            env::var("REDDIT_CLIENT_ID").expect(".env variables should be set"),
            env::var("REDDIT_CLIENT_SECRET").expect(".env variables should be set"),
        );

        re_client.update_access_token().await?;

        let tg_client =
            tg::TelegramClient::new(env::var("BOT_TOKEN").expect(".env variables should be set"));

        for mut poster in get_posters()? {
            let consts = poster.get_consts();
            for submission in re_client
                .get_subreddit_submissions(
                    &consts["SUBREDDIT"],
                    &consts["SORT_BY"],
                    &consts["LIMIT"],
                )
                .await?
            {
                if db.submission_is_posted(&consts["CHAT"], &submission.id)? {
                    continue;
                }
                if submission.removed_by_category.is_some() {
                    continue;
                }
                if !poster.should_post(&submission)? {
                    continue;
                }
                let submission_id = submission.id.clone();
                let result = if submission.is_video {
                    // single video post
                    send_video_post(
                        &tg_client,
                        &re_client,
                        submission,
                        &mut poster,
                        consts["CHAT"].clone(),
                    )
                    .await
                } else if submission.url.starts_with("https://i.redd.it/") {
                    if submission.url.ends_with(".gif") {
                        // single gif post
                        send_gif_post(
                            &tg_client,
                            &re_client,
                            submission,
                            &mut poster,
                            consts["CHAT"].clone(),
                        )
                        .await
                    } else {
                        // single image post
                        send_image_post(
                            &tg_client,
                            &re_client,
                            submission,
                            &mut poster,
                            consts["CHAT"].clone(),
                        )
                        .await
                    }
                } else {
                    // selftext or gallery post
                    send_selftext_gallery_post(
                        &tg_client,
                        &re_client,
                        submission,
                        &mut poster,
                        consts["CHAT"].clone(),
                    )
                    .await
                };

                match result {
                    Ok(()) => {
                        db.add_submission(&consts["CHAT"], &submission_id)?;
                    }
                    Err(err) => {
                        println!("Error for submission {}: {:?}", submission_id, err);
                    }
                }
            }
        }
        Ok(())
    })
}
