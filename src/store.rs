use crate::process::ProcessOutput;
use crate::Config;
use tracing::error;

pub struct Storer {
    bucket: s3::Bucket,
}

pub struct StoreResult {
    pub id: String,
    pub path: String,
}

impl Storer {
    pub fn new(config: &Config) -> anyhow::Result<Storer> {
        let region = s3::Region::Custom {
            region: "s3".to_string(),
            endpoint: config.s3.endpoint.to_string(),
        };

        let credentials = s3::creds::Credentials::new(
            Some(&config.s3.application_id),
            Some(&config.s3.application_key),
            None,
            None,
            None,
        )
        .unwrap();

        let bucket = s3::Bucket::new(&config.s3.bucket, region, credentials)?;

        Ok(Storer { bucket })
    }

    pub async fn store(&self, res: &ProcessOutput) -> anyhow::Result<StoreResult> {
        // errors here are all going to be internal
        let encoded_hash = res.hash.to_string();
        let path = format!("images/{}/{}.{}", &encoded_hash[..2], &encoded_hash[2..], res.format.extension());
        let res = self
            .bucket
            .put_object_with_content_type(&path, &res.data, res.format.mime_type())
            .await?;
        if res.status_code() != 200 {
            error!(
                "storage backend responded status code {}",
                res.status_code()
            );
            anyhow::bail!("error uploading image to cdn") // nicer user-facing error?
        }
        tracing::debug!("uploaded image to {}", &path);

        Ok(StoreResult {
            id: encoded_hash,
            path,
        })
    }
}
