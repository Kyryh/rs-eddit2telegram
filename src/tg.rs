use crate::{err::Error, muxer, re};
use reqwest::{
    Client,
    multipart::{Form, Part},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::json;
use std::{borrow::Cow, fmt, marker::PhantomData};

pub struct TelegramResponse<T>(pub Result<T, Error>);

impl<T> From<TelegramResponse<T>> for Result<T, Error> {
    fn from(value: TelegramResponse<T>) -> Self {
        value.0
    }
}

impl<'de, T> Deserialize<'de> for TelegramResponse<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct V<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> de::Visitor<'de> for V<T> {
            type Value = TelegramResponse<T>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("Telegram API response object")
            }

            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut ok: Option<bool> = None;
                let mut result: Option<T> = None;
                let mut error_code: Option<i32> = None;
                let mut description: Option<String> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "ok" => ok = Some(map.next_value()?),
                        "result" => result = Some(map.next_value()?),
                        "error_code" => error_code = Some(map.next_value()?),
                        "description" => description = Some(map.next_value()?),
                        _ => {
                            map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }

                match ok {
                    Some(true) => Ok(TelegramResponse(Ok(
                        result.ok_or_else(|| de::Error::missing_field("result"))?
                    ))),
                    Some(false) => Ok(TelegramResponse(Err(Error::TelegramError {
                        error_code: error_code
                            .ok_or_else(|| de::Error::missing_field("error_code"))?,
                        description: description
                            .ok_or_else(|| de::Error::missing_field("description"))?,
                    }))),
                    None => Err(de::Error::missing_field("ok")),
                }
            }
        }

        deserializer.deserialize_map(V(PhantomData))
    }
}

pub struct TelegramClient {
    req_client: reqwest::Client,
    token: String,
}

#[derive(Debug)]
pub enum TelegramMedia {
    URL(String),
    Bytes(Vec<u8>, String),
}

impl TelegramMedia {
    pub async fn from_url(re_client: &re::RedditClient, url: &str) -> Result<Option<Self>, Error> {
        if let Ok(url) = reqwest::Url::parse(url)
            && let Some(path_segments) = url.path_segments()
            && let Some(filename) = path_segments.last()
        {
            Ok(Some(Self::Bytes(
                re_client.download_file(url.as_str()).await?.into(),
                filename.to_owned(),
            )))
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum InputMedia {
    #[serde(rename = "photo")]
    Photo {
        #[serde(skip)]
        media: Option<TelegramMedia>,
        #[serde(rename = "media")]
        attach_name: String,
        #[serde(skip)]
        id: String,
        caption: String,
        parse_mode: &'static str,
        has_spoiler: bool,
    },
    #[serde(rename = "video")]
    Video {
        #[serde(skip)]
        media: Option<TelegramMedia>,
        #[serde(rename = "media")]
        attach_name: String,
        #[serde(skip)]
        id: String,
        caption: String,
        parse_mode: &'static str,
        supports_streaming: bool,
        has_spoiler: bool,
    },
}

impl InputMedia {
    pub async fn from_reddit_media_metadata(
        re_client: &re::RedditClient,
        media: re::MediaMetadata,
        id: String,
        caption: String,
        base_url: &str,
        spoiler: bool,
    ) -> Result<Self, Error> {
        match media {
            re::MediaMetadata::Image { ref previews, .. } => {
                if let Some(preview) = &previews.last()
                    && let Some(url) = &preview.url
                    && let Ok(url) = reqwest::Url::parse(url)
                    && let Some(path_segments) = url.path_segments()
                    && let Some(filename) = path_segments.last()
                {
                    Ok(InputMedia::Photo {
                        media: Some(TelegramMedia::Bytes(
                            re_client.download_file(url.as_str()).await?.into(),
                            filename.to_owned(),
                        )),
                        attach_name: format!("attach://{id}"),
                        caption,
                        parse_mode: "HTML",
                        has_spoiler: spoiler,
                        id,
                    })
                } else {
                    Err(Error::Custom(format!("Invalid media {media:?}")))
                }
            }
            re::MediaMetadata::AnimatedImage { ref source, .. } => {
                if let Some(gif) = &source.gif
                    && let Ok(gif) = reqwest::Url::parse(gif)
                    && let Some(path_segments) = gif.path_segments()
                    && let Some(filename) = path_segments.last()
                {
                    Ok(InputMedia::Video {
                        media: Some(TelegramMedia::Bytes(
                            re_client.download_file(gif.as_str()).await?.into(),
                            filename.to_owned(),
                        )),
                        attach_name: format!("attach://{id}"),
                        caption,
                        parse_mode: "HTML",
                        has_spoiler: spoiler,
                        supports_streaming: true,
                        id,
                    })
                } else {
                    Err(Error::Custom(format!("Invalid media {media:?}")))
                }
            }
            re::MediaMetadata::RedditVideo { dash_url, id, .. } => {
                let mpd = re_client.get_dash_info(&dash_url).await?;

                let (mut videos, audios) = re_client.get_video_audio_streams(mpd, base_url).await?;

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
                Ok(InputMedia::Video {
                    media: Some(TelegramMedia::Bytes(muxed_video, format!("{id}.mp4"))),
                    attach_name: format!("attach://{id}"),
                    caption,
                    parse_mode: "HTML",
                    has_spoiler: spoiler,
                    supports_streaming: true,
                    id,
                })
            }
        }
    }
}

impl From<TelegramMedia> for Part {
    fn from(value: TelegramMedia) -> Self {
        match value {
            TelegramMedia::URL(url) => Part::text(url),
            TelegramMedia::Bytes(bytes, file_name) => Part::bytes(bytes).file_name(file_name),
        }
    }
}

impl TelegramClient {
    pub fn new(token: String) -> Self {
        Self {
            req_client: Client::builder().http2_prior_knowledge().build().unwrap(),
            token,
        }
    }

    async fn make_request(&self, method: &str, form: Form) -> Result<serde_json::Value, Error> {
        // let res = loop {
        //     let res: TelegramResponse<serde_json::Value> = self
        //         .req_client
        //         .post(format!(
        //             "https://api.telegram.org/bot{}/{}",
        //             self.token, method
        //         ))
        //         .multipart(form.clone())
        //         .send()
        //         .await?
        //         .json()
        //         .await?;
        //     if let Err(err) = res.0 && let Error::TelegramError(429, retry_after_description) {

        //     } else {
        //         break res;
        //     }
        // };
        let res: TelegramResponse<serde_json::Value> = self
            .req_client
            .post(format!(
                "https://api.telegram.org/bot{}/{}",
                self.token, method
            ))
            .multipart(form)
            .send()
            .await?
            .json()
            .await?;
        res.into()
    }

    pub async fn send_message(
        &self,
        chat_id: impl Into<Cow<'static, str>>,
        message: String,
    ) -> Result<serde_json::Value, Error> {
        self.make_request(
            "sendMessage",
            Form::new()
                .text("chat_id", chat_id)
                .text("text", message)
                .text("parse_mode", "HTML")
                .text("link_preview_options", r#"{"is_disabled": true}"#),
        )
        .await
    }

    pub async fn send_rich_message(
        &self,
        chat_id: impl Into<Cow<'static, str>>,
        message: String,
    ) -> Result<serde_json::Value, Error> {
        self.make_request(
            "sendRichMessage",
            Form::new()
                .text("chat_id", chat_id)
                .text("rich_message", json!({"html": message}).to_string())
                .text("parse_mode", "HTML")
                .text("link_preview_options", r#"{"is_disabled": true}"#),
        )
        .await
    }

    pub async fn send_photo(
        &self,
        chat_id: impl Into<Cow<'static, str>>,
        photo: TelegramMedia,
        caption: String,
        spoiler: bool,
    ) -> Result<serde_json::Value, Error> {
        self.make_request(
            "sendPhoto",
            Form::new()
                .text("chat_id", chat_id)
                .text("caption", caption)
                .text("show_caption_above_media", "true")
                .text("has_spoiler", spoiler.to_string())
                .text("parse_mode", "HTML")
                .part("photo", photo.into()),
        )
        .await
    }

    pub async fn send_animation(
        &self,
        chat_id: impl Into<Cow<'static, str>>,
        animation: TelegramMedia,
        caption: String,
        spoiler: bool,
        width: Option<usize>,
        height: Option<usize>,
        thumbnail: Option<TelegramMedia>,
    ) -> Result<serde_json::Value, Error> {
        let mut form = Form::new()
            .text("chat_id", chat_id)
            .text("caption", caption)
            .text("show_caption_above_media", "true")
            .text("has_spoiler", spoiler.to_string())
            .text("parse_mode", "HTML")
            .part("animation", animation.into());
        if let Some(thumbnail) = thumbnail {
            form = form.part("thumbnail", thumbnail.into());
        }
        if let Some(width) = width {
            form = form.text("width", width.to_string());
        }
        if let Some(height) = height {
            form = form.text("height", height.to_string());
        }
        self.make_request("sendAnimation", form).await
    }

    pub async fn send_video(
        &self,
        chat_id: impl Into<Cow<'static, str>>,
        video: TelegramMedia,
        caption: String,
        spoiler: bool,
        duration: usize,
        width: usize,
        height: usize,
        thumbnail: Option<TelegramMedia>,
    ) -> Result<serde_json::Value, Error> {
        let mut form = Form::new()
            .text("chat_id", chat_id)
            .text("caption", caption)
            .text("show_caption_above_media", "true")
            .text("has_spoiler", spoiler.to_string())
            .text("parse_mode", "HTML")
            .text("supports_streaming", "true")
            .text("duration", duration.to_string())
            .text("width", width.to_string())
            .text("height", height.to_string())
            .part("video", video.into());
        if let Some(thumbnail) = thumbnail {
            form = form.part("thumbnail", thumbnail.into());
        }
        self.make_request("sendVideo", form).await
    }

    pub async fn send_media_group(
        &self,
        chat_id: impl Into<Cow<'static, str>>,
        media_group: &mut [InputMedia],
    ) -> Result<serde_json::Value, Error> {
        let mut form = Form::new().text("chat_id", chat_id);
        for media in media_group.iter_mut() {
            match media {
                InputMedia::Photo { media, id, .. } => {
                    form = form.part(id.clone(), media.take().unwrap().into())
                }
                InputMedia::Video { media, id, .. } => {
                    form = form.part(id.clone(), media.take().unwrap().into())
                }
            }
        }
        self.make_request(
            "sendMediaGroup",
            form.text("media", serde_json::to_string(media_group).unwrap()),
        )
        .await
    }
}
