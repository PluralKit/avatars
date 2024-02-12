use std::error::Error;
use crate::db::{ImageMeta, ImageQueueEntry};
use crate::pull::parse_url;
use crate::{db, process, AppState, PKAvatarError};
use reqwest::StatusCode;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, instrument, warn};

pub async fn handle_item_inner(
    state: &AppState,
    item: &ImageQueueEntry,
) -> Result<(), PKAvatarError> {
    let parsed = parse_url(&item.url)?;

    if let Some(_) = db::get_by_attachment_id(&state.pool, parsed.attachment_id).await? {
        info!(
            "attachment {} already migrated, skipping",
            parsed.attachment_id
        );
        return Ok(());
    }

    let pulled = state.puller.pull(&parsed).await?;
    let encoded = process::process(&pulled.data, item.kind)?;
    let store_res = state.storer.store(&encoded).await?;
    let final_url = format!("{}{}", state.config.base_url, store_res.path);

    db::add_image(
        &state.pool,
        ImageMeta {
            id: store_res.id,
            url: final_url.clone(),
            original_url: Some(parsed.full_url),
            original_type: Some(pulled.content_type),
            original_file_size: Some(pulled.data.len() as i32),
            original_attachment_id: Some(parsed.attachment_id as i64),
            file_size: encoded.data_webp.len() as i32,
            width: encoded.width as i32,
            height: encoded.height as i32,
            kind: item.kind,
            uploaded_at: None,
            uploaded_by_account: None,
        },
    )
    .await?;

    info!(
        "migrated {} ({}k -> {}k)",
        final_url,
        pulled.data.len(),
        encoded.data_webp.len()
    );
    Ok(())
}

pub async fn handle_item(state: &AppState) -> Result<(), PKAvatarError> {
    let queue_length = db::get_queue_length(&state.pool).await?;
    info!("migrate queue length: {}", queue_length);

    if let Some((tx, item)) = db::pop_queue(&state.pool).await? {
        match handle_item_inner(state, &item).await {
            Ok(_) => {
                tx.commit().await.map_err(Into::<anyhow::Error>::into)?;
                Ok(())
            }
            Err(
                // Errors that mean the image can't be migrated and doesn't need to be retried
                e @ (PKAvatarError::ImageDimensionsTooLarge(_, _)
                | PKAvatarError::UnknownImageFormat
                | PKAvatarError::UnsupportedImageFormat(_)
                | PKAvatarError::ImageFileSizeTooLarge(_, _)
                | PKAvatarError::InvalidCdnUrl
                | PKAvatarError::BadCdnResponse(StatusCode::NOT_FOUND | StatusCode::FORBIDDEN)),
            ) => {
                warn!("error migrating {}, skipping: {}", item.url, e);
                tx.commit().await.map_err(Into::<anyhow::Error>::into)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    } else {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(())
    }
}

#[instrument(skip(state))]
pub async fn worker(worker_id: u32, state: Arc<AppState>) {
    info!("spawned migrate worker with id {}", worker_id);
    loop {
        match handle_item(&state).await {
            Ok(()) => {}
            Err(e) => {
                error!("error in migrate worker {}: {}", worker_id, e.source().unwrap_or(&e));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

pub fn spawn_migrate_workers(state: Arc<AppState>, count: u32) {
    for i in 0..count {
        tokio::spawn(worker(i, state.clone()));
    }
}
