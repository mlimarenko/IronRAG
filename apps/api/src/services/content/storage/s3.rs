use std::time::Duration;

use anyhow::{Context, anyhow};
use aws_sdk_s3::{
    Client,
    config::{Credentials, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
};
use uuid::Uuid;

use super::{
    StashedContentDirectory,
    types::{ContentStorageProbe, ContentStorageProbeStatus, ContentStorageS3Settings},
};

#[derive(Clone, Debug)]
pub struct S3ContentStorageProvider {
    client: Client,
    bucket: String,
    object_key_prefix: String,
    endpoint: String,
}

impl S3ContentStorageProvider {
    const DOWNLOAD_REDIRECT_TTL: Duration = Duration::from_secs(600);

    pub fn new(
        settings: ContentStorageS3Settings,
        object_key_prefix: impl Into<String>,
    ) -> anyhow::Result<Self> {
        if settings.endpoint.trim().is_empty() {
            return Err(anyhow!("content_storage_s3_endpoint must not be empty"));
        }
        if settings.bucket.trim().is_empty() {
            return Err(anyhow!("content_storage_s3_bucket must not be empty"));
        }
        let credentials = Credentials::new(
            settings.access_key_id,
            settings.secret_access_key,
            settings.session_token,
            None,
            "ironrag-content-storage",
        );
        let config = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(Region::new(settings.region))
            .credentials_provider(credentials)
            .endpoint_url(settings.endpoint.trim().trim_end_matches('/').to_string())
            .force_path_style(settings.force_path_style)
            .build();
        Ok(Self {
            client: Client::from_conf(config),
            bucket: settings.bucket.trim().to_string(),
            object_key_prefix: normalize_prefix(object_key_prefix.into()),
            endpoint: settings.endpoint.trim().trim_end_matches('/').to_string(),
        })
    }

    pub async fn prepare_and_validate(&self) -> anyhow::Result<ContentStorageProbe> {
        self.ensure_bucket().await?;
        Ok(ContentStorageProbe { status: ContentStorageProbeStatus::Ok, message: None })
    }

    pub async fn probe(&self) -> ContentStorageProbe {
        match self.head_bucket().await {
            Ok(()) => ContentStorageProbe { status: ContentStorageProbeStatus::Ok, message: None },
            Err(error) => ContentStorageProbe {
                status: ContentStorageProbeStatus::Down,
                message: Some(format!(
                    "failed to access object storage bucket {} at {}: {error}",
                    self.bucket, self.endpoint
                )),
            },
        }
    }

    pub async fn persist(&self, storage_key: &str, file_bytes: &[u8]) -> anyhow::Result<()> {
        if self.has(storage_key).await? {
            return Ok(());
        }
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(self.absolute_object_key(storage_key))
            .body(ByteStream::from(file_bytes.to_vec()))
            .send()
            .await
            .with_context(|| format!("failed to write object storage key {storage_key}"))?;
        Ok(())
    }

    pub async fn has(&self, storage_key: &str) -> anyhow::Result<bool> {
        let response = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(self.absolute_object_key(storage_key))
            .send()
            .await;
        match response {
            Ok(_) => Ok(true),
            Err(error) => {
                if error
                    .as_service_error()
                    .is_some_and(aws_sdk_s3::operation::head_object::HeadObjectError::is_not_found)
                {
                    Ok(false)
                } else {
                    Err(anyhow!(error)).with_context(|| {
                        format!("failed to inspect object storage key {storage_key}")
                    })
                }
            }
        }
    }

    pub async fn read(&self, storage_key: &str) -> anyhow::Result<Vec<u8>> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.absolute_object_key(storage_key))
            .send()
            .await
            .with_context(|| format!("failed to fetch object storage key {storage_key}"))?;
        let bytes = response
            .body
            .collect()
            .await
            .with_context(|| format!("failed to read object storage body for {storage_key}"))?
            .into_bytes();
        Ok(bytes.to_vec())
    }

    pub async fn presign_download(
        &self,
        storage_key: &str,
        content_disposition: &str,
        content_type: &str,
    ) -> anyhow::Result<String> {
        let request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.absolute_object_key(storage_key))
            .response_content_disposition(content_disposition.to_string())
            .response_content_type(content_type.to_string());
        let presigned = request
            .presigned(
                PresigningConfig::builder()
                    .expires_in(Self::DOWNLOAD_REDIRECT_TTL)
                    .build()
                    .context("failed to build S3 download presigning config")?,
            )
            .await
            .with_context(|| format!("failed to presign object storage key {storage_key}"))?;
        Ok(presigned.uri().to_string())
    }

    pub async fn stash_prefix(
        &self,
        relative_directory: &str,
    ) -> anyhow::Result<Option<StashedContentDirectory>> {
        let listed = self.list_prefixed_objects(relative_directory).await?;
        if listed.is_empty() {
            return Ok(None);
        }

        let original_path = relative_directory.into();
        let stashed_path = format!(".trash/{}/{}", Uuid::now_v7(), trim_prefix(relative_directory));
        for source_key in &listed {
            let suffix = source_key
                .strip_prefix(&self.absolute_prefix(relative_directory))
                .ok_or_else(|| anyhow!("failed to derive stash suffix for object {source_key}"))?;
            let target_key = format!("{}{}", self.absolute_prefix(&stashed_path), suffix);
            self.copy_object(source_key, &target_key).await?;
        }
        self.delete_objects(&listed).await?;

        Ok(Some(StashedContentDirectory { original_path, stashed_path: stashed_path.into() }))
    }

    pub async fn restore_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> anyhow::Result<()> {
        let stashed_prefix = stashed_directory.stashed_path.to_string_lossy().to_string();
        let objects = self.list_prefixed_objects(&stashed_prefix).await?;
        for source_key in &objects {
            let suffix =
                source_key.strip_prefix(&self.absolute_prefix(&stashed_prefix)).ok_or_else(
                    || anyhow!("failed to derive restore suffix for object {source_key}"),
                )?;
            let target_key = format!(
                "{}{}",
                self.absolute_prefix(stashed_directory.original_path.to_string_lossy().as_ref()),
                suffix
            );
            self.copy_object(source_key, &target_key).await?;
        }
        self.delete_objects(&objects).await
    }

    pub async fn purge_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> anyhow::Result<()> {
        let stashed_prefix = stashed_directory.stashed_path.to_string_lossy().to_string();
        let objects = self.list_prefixed_objects(&stashed_prefix).await?;
        self.delete_objects(&objects).await
    }

    fn absolute_object_key(&self, storage_key: &str) -> String {
        let trimmed = trim_prefix(storage_key);
        if self.object_key_prefix.is_empty() {
            trimmed.to_string()
        } else if trimmed.is_empty() {
            self.object_key_prefix.clone()
        } else {
            format!("{}/{trimmed}", self.object_key_prefix)
        }
    }

    fn absolute_prefix(&self, storage_key_prefix: &str) -> String {
        let absolute = self.absolute_object_key(storage_key_prefix);
        if absolute.is_empty() || absolute.ends_with('/') {
            absolute
        } else {
            format!("{absolute}/")
        }
    }

    async fn ensure_bucket(&self) -> anyhow::Result<()> {
        if self.head_bucket().await.is_ok() {
            return Ok(());
        }
        self.client
            .create_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .with_context(|| format!("failed to create object storage bucket {}", self.bucket))?;
        self.head_bucket().await
    }

    async fn head_bucket(&self) -> anyhow::Result<()> {
        self.client.head_bucket().bucket(&self.bucket).send().await.with_context(|| {
            format!("failed to access object storage bucket {} at {}", self.bucket, self.endpoint)
        })?;
        Ok(())
    }

    async fn list_prefixed_objects(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let absolute_prefix = self.absolute_prefix(prefix);
        let mut continuation_token = None;
        let mut keys = Vec::new();

        loop {
            let response = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&absolute_prefix)
                .set_continuation_token(continuation_token.clone())
                .send()
                .await
                .with_context(|| {
                    format!(
                        "failed to list object storage prefix {} in bucket {}",
                        absolute_prefix, self.bucket
                    )
                })?;
            for object in response.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_string());
                }
            }
            if response.is_truncated.unwrap_or(false) {
                continuation_token = response.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(keys)
    }

    async fn copy_object(&self, source_key: &str, target_key: &str) -> anyhow::Result<()> {
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .key(target_key)
            .copy_source(format!("{}/{}", self.bucket, source_key))
            .send()
            .await
            .with_context(|| {
                format!("failed to copy object storage key {source_key} to {target_key}")
            })?;
        Ok(())
    }

    async fn delete_objects(&self, object_keys: &[String]) -> anyhow::Result<()> {
        for chunk in object_keys.chunks(1000) {
            if chunk.is_empty() {
                continue;
            }
            let objects = chunk
                .iter()
                .map(|key| ObjectIdentifier::builder().key(key).build())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| anyhow!("failed to build object deletion payload: {error}"))?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .build()
                .map_err(|error| anyhow!("failed to build delete request: {error}"))?;
            self.client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
                .with_context(|| format!("failed to delete {} object storage keys", chunk.len()))?;
        }
        Ok(())
    }
}

fn normalize_prefix(value: String) -> String {
    trim_prefix(&value).trim_end_matches('/').to_string()
}

fn trim_prefix(value: &str) -> &str {
    value.trim().trim_start_matches('/')
}
