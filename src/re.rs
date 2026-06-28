use std::collections::HashMap;

use regex_lite::Regex;
use reqwest::Client;
use serde::{Deserialize, de::DeserializeOwned};

use crate::err::Error;
use crate::shorten;

#[derive(Debug)]
pub struct RedditClient {
    req_client: reqwest::Client,
    reddit_client_id: String,
    reddit_client_secret: String,
    access_token: Option<String>,
}

impl RedditClient {
    pub fn new(user_agent: &str, reddit_client_id: String, reddit_client_secret: String) -> Self {
        Self {
            req_client: Client::builder().user_agent(user_agent).build().unwrap(),
            reddit_client_id,
            reddit_client_secret,
            access_token: None,
        }
    }

    pub async fn update_access_token(&mut self) -> reqwest::Result<()> {
        let response: AccessTokenResponse = self
            .req_client
            .post("https://www.reddit.com/api/v1/access_token")
            .body("grant_type=client_credentials")
            .basic_auth(&self.reddit_client_id, Some(&self.reddit_client_secret))
            .send()
            .await?
            .json()
            .await?;
        self.access_token = Some(response.access_token);
        Ok(())
    }

    async fn send_request<T>(&self, endpoint: &str) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let response = self
            .req_client
            .get(endpoint)
            .bearer_auth(self.access_token.as_deref().unwrap_or_default())
            .send()
            .await?;

        match response.status() {
            reqwest::StatusCode::FORBIDDEN => Err(Error::InvalidRedditAccessToken),
            _ => Ok(response.json().await?),
        }
    }

    pub async fn get_subreddits_info(
        &self,
        subreddits: &[&str],
    ) -> Result<Vec<SubredditInfo>, Error> {
        let response: Listing<ListingData<Listing<SubredditInfo>>> = self
            .send_request(&format!(
                "https://oauth.reddit.com/api/info.json?sr_name={}",
                subreddits.join(",")
            ))
            .await?;

        Ok(response
            .data
            .children
            .into_iter()
            .map(|listing| listing.data)
            .collect())
    }

    pub async fn get_subreddit_submissions(
        &self,
        subreddit: &str,
        sort_by: &str,
        limit: &str,
    ) -> Result<Vec<Submission>, Error> {
        let response: Listing<ListingData<Listing<Submission>>> = self
            .send_request(&format!(
                "https://oauth.reddit.com/r/{subreddit}/{sort_by}?limit={limit}&raw_json=1",
            ))
            .await?;
        let mut submissions: Vec<_> = response
            .data
            .children
            .into_iter()
            .map(|listing| listing.data)
            .collect();
        for submission in submissions.iter_mut() {
            submission.selftext_html = submission
                .selftext_html
                .as_mut()
                .map(|string| re_html_to_tg_html(&string).trim().to_owned());
            if let Some(mut crossposts) = submission.crosspost_parent_list.take() {
                let crosspost = crossposts.swap_remove(0);
                submission.is_video = crosspost.is_video;
                submission.is_gallery = crosspost.is_gallery;
                submission.preview = crosspost.preview;
                submission.media = crosspost.media;
                submission.media_metadata = crosspost.media_metadata;
                submission.gallery_data = crosspost.gallery_data;
                submission.url_overridden_by_dest = crosspost.url_overridden_by_dest;
                submission.removed_by_category = crosspost.removed_by_category;
            }
        }
        Ok(submissions)
    }

    pub async fn get_submission(&self, submission_id: &str) -> Result<Submission, Error> {
        let mut response: (
            Listing<ListingData<Listing<Submission>>>,
            Listing<ListingData<Listing<Comment>>>,
        ) = self
            .send_request(&format!(
                "https://oauth.reddit.com/comments/{submission_id}?raw_json=1",
            ))
            .await?;
        let mut submission = response.0.data.children.pop().unwrap().data;
        submission.selftext_html = submission
            .selftext_html
            .as_mut()
            .map(|string| re_html_to_tg_html(&string).trim().to_owned());
        if let Some(mut crossposts) = submission.crosspost_parent_list.take() {
            let crosspost = crossposts.swap_remove(0);
            submission.is_video = crosspost.is_video;
            submission.is_gallery = crosspost.is_gallery;
            submission.preview = crosspost.preview;
            submission.media = crosspost.media;
            submission.media_metadata = crosspost.media_metadata;
            submission.gallery_data = crosspost.gallery_data;
            submission.url_overridden_by_dest = crosspost.url_overridden_by_dest;
            submission.removed_by_category = crosspost.removed_by_category;
        }
        Ok(submission)
    }

    pub async fn get_dash_info(&self, dash_url: &str) -> Result<MPD, Error> {
        Ok(serde_xml_rs::from_str(
            &self.req_client.get(dash_url).send().await?.text().await?,
        )?)
    }

    pub async fn get_video_audio_streams(
        &self,
        mpd: MPD,
        base_url: &str,
    ) -> Result<(Vec<String>, Vec<String>), Error> {
        let videos: Option<Vec<String>> = mpd
            .period
            .adaptation_sets
            .iter()
            .filter(|set| set.content_type == "video")
            .next()
            .map(|set| {
                set.representations
                    .iter()
                    .map(|r| format!("{}/{}", base_url, r.base_url))
                    .collect()
            });

        let audios: Option<Vec<String>> = mpd
            .period
            .adaptation_sets
            .iter()
            .filter(|set| set.content_type == "audio")
            .next()
            .map(|set| {
                set.representations
                    .iter()
                    .map(|r| format!("{}/{}", base_url, r.base_url))
                    .collect()
            });
        Ok((videos.unwrap_or_default(), audios.unwrap_or_default()))
    }

    pub async fn download_file(&self, url: &str) -> Result<Vec<u8>, reqwest::Error> {
        self.req_client
            .get(url)
            .send()
            .await?
            .bytes()
            .await
            .map(|bytes| Vec::from(bytes))
    }

    pub async fn get_file_size(&self, url: &str) -> Result<u64, reqwest::Error> {
        Ok(self
            .req_client
            .head(url)
            .send()
            .await?
            .content_length()
            .unwrap_or_default())
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Submission {
    pub url: String,
    pub title: String,
    pub id: String,
    pub permalink: String,
    pub score: isize,
    pub link_flair_text: Option<String>,
    pub selftext_html: Option<String>,
    pub spoiler: bool,
    pub over_18: bool,
    pub crosspost_parent_list: Option<Vec<Submission>>,
    pub is_video: bool,
    #[serde(default)]
    pub is_gallery: bool,
    pub preview: Option<Preview>,
    pub media: Option<Media>,
    pub media_metadata: Option<HashMap<String, MediaMetadata>>,
    pub gallery_data: Option<GalleryData>,
    pub url_overridden_by_dest: Option<String>,
    pub removed_by_category: Option<String>,
}

fn escape_html(string: &str) -> String {
    string
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
}

static TELEGRAM_HTML_TAGS: [&str; 17] = [
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ins",
    "s",
    "strike",
    "del",
    "span",
    "tg-spoiler",
    "a",
    "tg-emoji",
    "tg-time",
    "code",
    "pre",
    "blockquote",
];

fn re_html_to_tg_html(string: &str) -> String {
    let regex = Regex::new(r"<\/?([\w-]+).*?>").unwrap();
    let string = string
        .replace("<!-- SC_OFF -->", "")
        .replace("<!-- SC_ON -->", "")
        .replace(
            "<span class=\"md-spoiler-text\">",
            "<span class=\"tg-spoiler\">",
        );

    regex
        .replace_all(&string, |caps: &regex_lite::Captures| {
            let tag = caps[1].to_lowercase();
            if TELEGRAM_HTML_TAGS.contains(&tag.as_str()) {
                caps[0].to_string()
            } else {
                String::new()
            }
        })
        .into_owned()
}

impl Submission {
    pub fn is_spoiler(&mut self) -> bool {
        self.spoiler
    }

    pub fn is_nsfw(&mut self) -> bool {
        self.over_18
    }

    pub fn text(&mut self, short: bool) -> String {
        if let Some(selftext) = &self.selftext_html {
            if short {
                shorten(&selftext, 1024 - 256)
            } else {
                selftext.clone()
            }
        } else {
            "".to_owned()
        }
    }

    pub fn title(&mut self) -> String {
        escape_html(&self.title)
    }

    pub fn flair(&mut self) -> String {
        self.link_flair_text.clone().unwrap_or_default()
    }

    pub fn url(&mut self) -> String {
        format!(
            "<a href=\"https://www.reddit.com{}\">https://redd.it/{}</a>",
            escape_html(&self.permalink),
            escape_html(&self.id)
        )
    }

    pub fn score(&mut self) -> i64 {
        self.score as i64
    }
}

#[derive(Debug, Deserialize)]
pub struct MPD {
    #[serde(rename = "Period")]
    period: Period,
}

#[derive(Debug, Deserialize)]
pub struct Period {
    #[serde(rename = "AdaptationSet")]
    adaptation_sets: Vec<AdaptationSet>,
}

#[derive(Debug, Deserialize)]
pub struct AdaptationSet {
    #[serde(rename = "@contentType")]
    content_type: String,
    #[serde(rename = "Representation")]
    representations: Vec<Representation>,
}

#[derive(Debug, Deserialize)]
pub struct Representation {
    #[serde(rename = "BaseURL")]
    base_url: String,
}

#[derive(Debug, Deserialize)]
pub struct Comment {}

#[derive(Debug, Deserialize)]
pub struct AccessTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u32,
    scope: String,
}

#[derive(Debug, Deserialize)]
pub struct Listing<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
pub struct ListingData<T> {
    children: Vec<T>,
}

#[derive(Debug, Deserialize)]
pub struct SubredditInfo {
    title: String,
    display_name: String,
    public_description: String,
    description: String,
    primary_color: String,
    over18: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Preview {
    pub images: Vec<Image>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Image {
    pub source: MediaData,
    pub resolutions: Vec<MediaData>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MediaData {
    #[serde(alias = "u")]
    pub url: Option<String>,
    pub gif: Option<String>,
    pub mp4: Option<String>,
    #[serde(alias = "x")]
    pub width: usize,
    #[serde(alias = "y")]
    pub height: usize,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Media {
    Video {
        reddit_video: Video,
    },
    Embed {
        #[serde(rename = "type")]
        ty: String,
        oembed: Embed,
    },
}

#[derive(Debug, Deserialize, Clone)]
pub struct Embed {
    provider_url: String,
    description: Option<String>,
    title: String,
    #[serde(rename = "type")]
    ty: String,
    author_name: Option<String>,
    height: usize,
    width: usize,
    html: String,
    version: String,
    provider_name: String,
    thumbnail_url: String,
    thumbnail_width: usize,
    thumbnail_height: usize,
    author_url: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Video {
    pub bitrate_kbps: usize,
    pub fallback_url: String,
    pub has_audio: bool,
    pub height: usize,
    pub width: usize,
    pub scrubber_media_url: String,
    pub dash_url: String,
    pub duration: usize,
    pub hls_url: String,
    pub is_gif: bool,
    pub transcoding_status: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GalleryData {
    pub items: Vec<GalleryDataInstance>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GalleryDataInstance {
    pub caption: Option<String>,
    pub media_id: String,
    pub is_deleted: bool,
    pub id: usize,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "e")]
pub enum MediaMetadata {
    RedditVideo {
        status: String,
        #[serde(rename = "dashUrl")]
        dash_url: String,
        #[serde(rename = "x")]
        width: usize,
        #[serde(rename = "y")]
        height: usize,
        #[serde(rename = "hlsUrl")]
        hls_url: String,
        id: String,
        #[serde(rename = "isGif")]
        is_gif: bool,
    },
    Image {
        status: String,
        #[serde(rename = "m")]
        format: Option<String>,
        #[serde(rename = "s")]
        source: MediaData,
        #[serde(rename = "p")]
        previews: Vec<MediaData>,
    },
    AnimatedImage {
        status: String,
        #[serde(rename = "m")]
        format: Option<String>,
        #[serde(rename = "s")]
        source: MediaData,
        #[serde(rename = "p")]
        previews: Vec<MediaData>,
        id: String,
    },
}
