use crate::ImageKind;
use s3::creds::time::OffsetDateTime;
use sqlx::{Executor, FromRow, PgPool};

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

pub async fn add_image(pool: &PgPool, meta: ImageMeta) -> anyhow::Result<bool> {
    let kind_str = match meta.kind {
        ImageKind::Avatar => "avatar",
        ImageKind::Banner => "banner",
    };

    let res = sqlx::query("insert into images (id, url, original_url, file_size, width, height, original_file_size, original_type, original_attachment_id, kind, uploaded_by_account, uploaded_at) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, (now() at time zone 'utc')) on conflict (id) do nothing")
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
        .execute(pool).await?;
    Ok(res.rows_affected() > 0)
}
