mod db;
mod hash;
mod migrate;
mod process;
mod pull;
mod store;

use std::error::Error;
use crate::db::{ImageMeta, Stats};
use crate::pull::Puller;
use crate::store::Storer;
use axum::extract::State;
use axum::routing::get;
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
use sqlx::postgres::PgPoolOptions;
use thiserror::Error;
use tracing::{error, info};
use uuid::Uuid;

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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, sqlx::Type, PartialEq)]
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
    system_id: Option<Uuid>,

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
    let encoded = process::process_async(result.data, req.kind).await?;

    let store_res = state.storer.store(&encoded).await?;
    let final_url = format!("{}{}", state.config.base_url, store_res.path);
    let is_new = db::add_image(
        &state.pool,
        ImageMeta {
            id: store_res.id,
            url: final_url.clone(),
            content_type: encoded.format.mime_type().to_string(),
            original_url: Some(parsed.full_url),
            original_type: Some(result.content_type),
            original_file_size: Some(original_file_size as i32),
            original_attachment_id: Some(parsed.attachment_id as i64),
            file_size: encoded.data.len() as i32,
            width: encoded.width as i32,
            height: encoded.height as i32,
            kind: req.kind,
            uploaded_at: None,
            uploaded_by_account: req.uploaded_by.map(|x| x as i64),
            uploaded_by_system: req.system_id,
        },
    )
    .await?;

    Ok(Json(PullResponse {
        url: final_url,
        new: is_new,
    }))
}

pub async fn stats(State(state): State<AppState>) -> Result<Json<Stats>, PKAvatarError> {
    Ok(Json(db::get_stats(&state.pool).await?))
}

fn load_config() -> anyhow::Result<Config> {
    config::ConfigBuilder::<DefaultState>::default()
        .add_source(config::File::new("config", FileFormat::Toml).required(false))
        .add_source(
            config::Environment::with_prefix("PK_AVATAR")
                .prefix_separator("__")
                .separator("__"),
        )
        .build()?
        .try_deserialize::<Config>()
        .map_err(Into::into)
}

#[derive(Clone)]
pub struct AppState {
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
    let pool = PgPoolOptions::new().max_connections(config.db_connections.unwrap_or(5)).connect(&config.db).await?;
    db::init(&pool).await?;

    let state = AppState {
        storer,
        puller,
        pool,
        config: Arc::new(config),
    };

    migrate::spawn_migrate_workers(Arc::new(state.clone()), state.config.migrate_worker_count);

    let app = Router::new()
        .route("/pull", post(pull))
        .route("/stats", get(stats))
        .with_state(state);

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
        error!("error: {}", self.source().unwrap_or(&self));

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

    #[serde(default)] // default 5
    db_connections: Option<u32>,
    s3: S3Config,
    base_url: String,

    #[serde(default)]
    migrate_worker_count: u32,
}

#[derive(Deserialize, Clone)]
struct S3Config {
    bucket: String,
    application_id: String,
    application_key: String,
    endpoint: String,
}
