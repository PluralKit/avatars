use crate::ImageKind;
use s3::creds::time::OffsetDateTime;
use serde::Serialize;
use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(FromRow)]
pub struct ImageMeta {
    pub id: String,
    pub kind: ImageKind,
    pub url: String,
    pub file_size: i32,
    pub width: i32,
    pub height: i32,
    pub uploaded_at: Option<OffsetDateTime>,

    pub original_url: Option<String>,
    pub original_attachment_id: Option<i64>,
    pub original_file_size: Option<i32>,
    pub original_type: Option<String>,
    pub uploaded_by_account: Option<i64>,
    pub uploaded_by_system: Option<Uuid>,
}

#[derive(FromRow, Serialize)]
pub struct Stats {
    pub total_images: i64,
    pub total_file_size: i64,
}

#[derive(FromRow)]
pub struct ImageQueueEntry {
    pub itemid: i32,
    pub url: String,
    pub kind: ImageKind,
}

pub async fn init(pool: &PgPool) -> anyhow::Result<()> {
    pool.execute(include_str!("./init.sql")).await?;
    Ok(())
}

pub async fn get_by_original_url(
    pool: &PgPool,
    original_url: &str,
) -> anyhow::Result<Option<ImageMeta>> {
    Ok(
        sqlx::query_as("select * from images where original_url = $1")
            .bind(original_url)
            .fetch_optional(pool)
            .await?,
    )
}
pub async fn get_by_attachment_id(
    pool: &PgPool,
    attachment_id: u64,
) -> anyhow::Result<Option<ImageMeta>> {
    Ok(
        sqlx::query_as("select * from images where original_attachment_id = $1")
            .bind(attachment_id as i64)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn pop_queue(
    pool: &PgPool,
) -> anyhow::Result<Option<(Transaction<Postgres>, ImageQueueEntry)>> {
    let mut tx = pool.begin().await?;
    let res: Option<ImageQueueEntry> = sqlx::query_as("delete from image_queue where itemid = (select itemid from image_queue order by itemid for update skip locked limit 1) returning *")
        .fetch_optional(&mut *tx).await?;
    Ok(res.map(|x| (tx, x)))
}

pub async fn get_queue_length(pool: &PgPool) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("select count(*) from image_queue")
        .fetch_one(pool)
        .await?)
}

pub async fn get_stats(pool: &PgPool) -> anyhow::Result<Stats> {
    Ok(sqlx::query_as(
        "select count(*) as total_images, sum(file_size) as total_file_size from images",
    )
    .fetch_one(pool)
    .await?)
}

pub async fn add_image(pool: &PgPool, meta: ImageMeta) -> anyhow::Result<bool> {
    let kind_str = match meta.kind {
        ImageKind::Avatar => "avatar",
        ImageKind::Banner => "banner",
    };

    let res = sqlx::query("insert into images (id, url, original_url, file_size, width, height, original_file_size, original_type, original_attachment_id, kind, uploaded_by_account, uploaded_by_system, uploaded_at) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, (now() at time zone 'utc')) on conflict (id) do nothing")
        .bind(meta.id)
        .bind(meta.url)
        .bind(meta.original_url)
        .bind(meta.file_size)
        .bind(meta.width)
        .bind(meta.height)
        .bind(meta.original_file_size)
        .bind(meta.original_type)
        .bind(meta.original_attachment_id)
        .bind(kind_str)
        .bind(meta.uploaded_by_account)
        .bind(meta.uploaded_by_system)
        .execute(pool).await?;
    Ok(res.rows_affected() > 0)
}

pub async fn push_queue(conn: &mut sqlx::PgConnection, url: &str, kind: ImageKind) -> anyhow::Result<()> {
    sqlx::query("insert into image_queue (url, kind) values ($1, $2)")
        .bind(url)
        .bind(kind)
        .execute(conn).await?;
    Ok(())
}