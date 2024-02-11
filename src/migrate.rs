use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, instrument, warn};
use crate::{AppState, db, process};
use crate::db::ImageMeta;
use crate::pull::parse_url;

pub async fn handle_item(state: &AppState) -> anyhow::Result<()> {
    let queue_length = db::get_queue_length(&state.pool).await?;
    info!("migrate queue length: {}", queue_length);

    if let Some((tx, item)) = db::pop_queue(&state.pool).await? {
        let Ok(parsed) = parse_url(&item.url) else {
            // if invalid url, consume and skip it
            warn!("skipping invalid url: {}", item.url);
            tx.commit().await?;
            return Ok(());
        };

        if db::get_by_attachment_id(&state.pool, parsed.attachment_id) {
            info!("attachment {} already migrated, skipping", parsed.attachment_id);
            tx.commit().await?;
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

        info!("migrated {} ({}k -> {}k)", final_url, pulled.data.len(), encoded.data_webp.len());
        tx.commit().await?;
    } else {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    Ok(())
}

#[instrument(skip(state))]
pub async fn worker(worker_id: u32, state: Arc<AppState>) {
    info!("spawned migrate worker with id {}", worker_id);
    loop {
        match handle_item(&state).await {
            Ok(()) => {},
            Err(e) => {
                error!("error in migrate worker {}: {}", worker_id, e);
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