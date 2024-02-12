use std::str::FromStr;
use std::time::Duration;

use crate::PKAvatarError;
use anyhow::Context;
use reqwest::{Client, ClientBuilder, StatusCode, Url};
use time::Instant;
use tracing::{error, instrument};

const MAX_SIZE: u64 = 4_000_000;

pub struct PullResult {
    pub data: Vec<u8>,
    pub content_type: String,
    pub last_modified: Option<String>,
}

pub struct Puller {
    client: Client,
}

impl Puller {
    pub fn new() -> anyhow::Result<Puller> {
        let client = ClientBuilder::new()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(3))
            .build()
            .context("error making client")?;
        Ok(Puller { client })
    }

    #[instrument(skip_all)]
    pub async fn pull(&self, parsed_url: &ParsedUrl) -> Result<PullResult, PKAvatarError> {
        let time_before = Instant::now();
        let response = self
            .client
            .get(&parsed_url.full_url)
            .send()
            .await
            .map_err(|e| {
                error!("network error for {}: {}", parsed_url.full_url, e);
                PKAvatarError::NetworkError(e)
            })?;
        let time_after_headers = Instant::now();
        let status = response.status();

        if status != StatusCode::OK {
            return Err(PKAvatarError::BadCdnResponse(status));
        }

        let size = match response.content_length() {
            None => return Err(PKAvatarError::MissingHeader("Content-Length")),
            Some(size) if size > MAX_SIZE => {
                return Err(PKAvatarError::ImageFileSizeTooLarge(size, MAX_SIZE))
            }
            Some(size) => size,
        };

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|x| x.to_str().ok()) // invalid (non-unicode) header = missing, why not
            .map(|mime| mime.split(';').next().unwrap_or("")) // cut off at ;
            .ok_or(PKAvatarError::MissingHeader("Content-Type"))?
            .to_owned();
        let mime = match content_type.as_str() {
            mime @ ("image/jpeg" | "image/png" | "image/gif" | "image/webp") => mime,
            _ => return Err(PKAvatarError::UnsupportedContentType(content_type)),
        };

        let last_modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|x| x.to_str().ok())
            .map(|x| x.to_string());

        let body = response.bytes().await.map_err(|e| {
            error!("network error for {}: {}", parsed_url.full_url, e);
            PKAvatarError::NetworkError(e)
        })?;
        if body.len() != size as usize {
            // ???does this ever happen?
            return Err(PKAvatarError::InternalError(anyhow::anyhow!(
                "server responded with wrong length"
            )));
        }
        let time_after_body = Instant::now();

        let headers_time = time_after_headers - time_before;
        let body_time = time_after_body - time_after_headers;

        // can't do dynamic log level lmao
        if status != StatusCode::OK {
            tracing::warn!("{}: {} (headers: {}ms, body: {}ms)", status, &parsed_url.full_url, headers_time.whole_milliseconds(), body_time.whole_milliseconds());
        } else {
            tracing::info!("{}: {} (headers: {}ms, body: {}ms)", status, &parsed_url.full_url, headers_time.whole_milliseconds(), body_time.whole_milliseconds());
        };

        Ok(PullResult {
            data: body.to_vec(),
            content_type: mime.to_string(),
            last_modified,
        })
    }
}

#[derive(Debug)]
pub struct ParsedUrl {
    pub channel_id: u64,
    pub attachment_id: u64,
    pub filename: String,
    pub full_url: String,
}

pub fn parse_url(url: &str) -> anyhow::Result<ParsedUrl> {
    // todo: should this return PKAvatarError::InvalidCdnUrl?
    let url = Url::from_str(url).context("invalid url")?;

    match (url.scheme(), url.domain()) {
        ("https", Some("media.discordapp.net" | "cdn.discordapp.com")) => {}
        _ => anyhow::bail!("not a discord cdn url"),
    }

    match url
        .path_segments()
        .map(|x| x.collect::<Vec<_>>())
        .as_deref()
    {
        Some([_, channel_id, attachment_id, filename]) => {
            let channel_id = u64::from_str(channel_id).context("invalid channel id")?;
            let attachment_id = u64::from_str(attachment_id).context("invalid channel id")?;

            Ok(ParsedUrl {
                channel_id,
                attachment_id,
                filename: filename.to_string(),
                full_url: url.to_string(),
            })
        }
        _ => anyhow::bail!("invaild discord cdn url"),
    }
}
