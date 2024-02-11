mod db;
mod hash;
mod process;
mod pull;
mod store;
use crate::db::ImageMeta;
use crate::pull::Puller;
use crate::store::Storer;
use axum::extract::State;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use config::builder::DefaultState;
use config::FileFormat;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use thiserror::Error;
use tracing::{error, info};

#[derive(Error, Debug)]
pub enum PKAvatarError {
    // todo: split off into logical groups (cdn/url error, image format error, etc)
    #[error("invalid cdn url")]
    InvalidCdnUrl,

    #[error("discord cdn responded with status code: {0}")]
    BadCdnResponse(reqwest::StatusCode),

    #[error("network error: {0}")]
    NetworkError(reqwest::Error),

    #[error("response is missing header: {0}")]
    MissingHeader(&'static str),

    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),

    #[error("image file size too large ({0} > {1})")]
    ImageFileSizeTooLarge(u64, u64),

    #[error("unsupported image format: {0:?}")]
    UnsupportedImageFormat(image::ImageFormat),

    #[error("could not detect image format")]
    UnknownImageFormat,

    #[error("original image dimensions too large: {0:?} > {1:?}")]
    ImageDimensionsTooLarge((u32, u32), (u32, u32)),

    #[error("could not decode image, is it corrupted?")]
    ImageFormatError(#[from] image::ImageError),

    #[error("unknown error")]
    InternalError(#[from] anyhow::Error),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case", type_name = "text")]
pub enum ImageKind {
    Avatar,
    Banner,
}

impl ImageKind {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::Avatar => (512, 512),
            Self::Banner => (1024, 1024),
        }
    }
}
#[derive(Deserialize, Debug)]
pub struct PullRequest {
    url: String,
    kind: ImageKind,
    uploaded_by: Option<u64>, // should be String? serde makes this hard :/

    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
pub struct PullResponse {
    url: String,
    new: bool,
}

async fn pull(
    State(state): State<AppState>,
    Json(req): Json<PullRequest>,
) -> Result<Json<PullResponse>, PKAvatarError> {
    let parsed = pull::parse_url(&req.url) // parsing beforehand to "normalize"
        .map_err(|_| PKAvatarError::InvalidCdnUrl)?;

    if !req.force {
        if let Some(existing) = db::get_by_attachment_id(&state.pool, parsed.attachment_id).await? {
            return Ok(Json(PullResponse {
                url: existing.url,
                new: false,
            }));
        }
    }

    let result = state.puller.pull(&parsed).await?;

    let original_file_size = result.data.len();
    let encoded = tokio::task::spawn_blocking(move || process::process(&result.data, req.kind))
        .await
        .map_err(|je| PKAvatarError::InternalError(je.into()))??;

    let store_res = state.storer.store(&encoded).await?;

    let final_url = format!("{}{}", state.config.base_url, store_res.path);
    let is_new = db::add_image(
        &state.pool,
        ImageMeta {
            id: store_res.id,
            url: final_url.clone(),
            original_url: Some(parsed.full_url),
            original_type: Some(result.content_type),
            original_file_size: Some(original_file_size as i32),
            original_attachment_id: Some(parsed.attachment_id as i64),
            file_size: encoded.data_webp.len() as i32,
            width: encoded.width as i32,
            height: encoded.height as i32,
            kind: req.kind,
            uploaded_at: None,
            uploaded_by_account: req.uploaded_by.map(|x| x as i64),
        },
    )
    .await?;

    Ok(Json(PullResponse {
        url: final_url,
        new: is_new,
    }))
}

fn load_config() -> anyhow::Result<Config> {
    config::ConfigBuilder::<DefaultState>::default()
        .add_source(config::File::new("config", FileFormat::Toml).required(false))
        .add_source(config::Environment::with_prefix("PK_AVATAR").prefix_separator("__").separator("__"))
        .build()?
        .try_deserialize::<Config>()
        .map_err(Into::into)
}

#[derive(Clone)]
struct AppState {
    storer: Arc<Storer>,
    puller: Arc<Puller>,
    pool: PgPool,
    config: Arc<Config>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = load_config()?;

    let storer = Arc::new(Storer::new(&config)?);
    let puller = Arc::new(Puller::new()?);

    info!("connecting to database...");
    let pool = sqlx::PgPool::connect(&config.db).await?;
    db::init(&pool).await?;

    let state = AppState {
        storer,
        puller,
        pool,
        config: Arc::new(config),
    };

    let app = Router::new().route("/pull", post(pull)).with_state(state);

    let host = "0.0.0.0:3000";
    info!("starting server on {}!", host);
    let listener = tokio::net::TcpListener::bind(host).await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

struct AppError(anyhow::Error);

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        error!("error handling request: {}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

impl IntoResponse for PKAvatarError {
    fn into_response(self) -> Response {
        let status_code = match self {
            PKAvatarError::InternalError(_) | PKAvatarError::NetworkError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            _ => StatusCode::BAD_REQUEST,
        };

        // print inner error if otherwise hidden
        match self {
            PKAvatarError::InternalError(ref e) => error!("error: {}", e),
            PKAvatarError::NetworkError(ref e) => error!("error: {}", e),
            PKAvatarError::ImageFormatError(ref e) => error!("error: {}", e),
            _ => error!("error: {}", &self)
        }

        (
            status_code,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[derive(Deserialize, Clone)]
struct Config {
    db: String,
    s3: S3Config,
    base_url: String,
}

#[derive(Deserialize, Clone)]
struct S3Config {
    bucket: String,
    application_id: String,
    application_key: String,
    endpoint: String,
}
