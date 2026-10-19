use std::time::Duration;

use anyhow::{Context, anyhow};
use aws_sdk_s3::{
    Client,
    config::{Credentials, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt},
};
use uuid::Uuid;

use super::{
    BoundedRevisionSource, StashedContentDirectory, read_bounded_bytes,
    types::{ContentStorageProbe, ContentStorageProbeStatus, ContentStorageS3Settings},
};
use crate::services::content::error::ContentServiceError;

#[derive(Clone, Debug)]
pub(super) struct S3ContentStorageProvider {
    client: Client,
    bucket: String,
    object_key_prefix: String,
    endpoint: String,
}

impl S3ContentStorageProvider {
    const DOWNLOAD_REDIRECT_TTL: Duration = Duration::from_mins(10);

    pub(super) fn new(
        settings: ContentStorageS3Settings,
        object_key_prefix: impl Into<String>,
    ) -> Result<Self, ContentServiceError> {
        if settings.endpoint.trim().is_empty() {
            return Err(ContentServiceError::InvalidRequest {
                message: "content_storage_s3_endpoint must not be empty".to_string(),
            });
        }
        if settings.bucket.trim().is_empty() {
            return Err(ContentServiceError::InvalidRequest {
                message: "content_storage_s3_bucket must not be empty".to_string(),
            });
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

    pub(super) async fn prepare_and_validate(
        &self,
    ) -> Result<ContentStorageProbe, ContentServiceError> {
        self.ensure_bucket().await?;
        Ok(ContentStorageProbe { status: ContentStorageProbeStatus::Ok, message: None })
    }

    pub(super) async fn probe(&self) -> ContentStorageProbe {
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

    pub(super) async fn persist(
        &self,
        storage_key: &str,
        file_bytes: &[u8],
    ) -> Result<(), ContentServiceError> {
        if self.has(storage_key).await? {
            return Ok(());
        }
        self.write(storage_key, file_bytes).await
    }

    pub(super) async fn write(
        &self,
        storage_key: &str,
        file_bytes: &[u8],
    ) -> Result<(), ContentServiceError> {
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

    pub(super) async fn write_stream<R>(
        &self,
        storage_key: &str,
        reader: &mut R,
        max_bytes: u64,
    ) -> Result<(), ContentServiceError>
    where
        R: AsyncRead + Unpin,
    {
        let (file, temp_path) = tempfile::NamedTempFile::new()
            .context("failed to create temporary content source for object storage upload")?
            .into_parts();
        let temp_path_buf = temp_path.to_path_buf();
        let write_result = async {
            let mut output = fs::File::from_std(file);
            let copied =
                tokio::io::copy(&mut reader.take(max_bytes.saturating_add(1)), &mut output)
                    .await
                    .context("failed to stage content revision source for object storage upload")?;
            if copied > max_bytes {
                return Err(ContentServiceError::InvalidRequest {
                    message: format!("content revision source exceeds the {max_bytes}-byte limit"),
                });
            }
            output.sync_all().await.context("failed to sync staged content revision source")?;
            drop(output);
            let body = ByteStream::from_path(&temp_path_buf)
                .await
                .context("failed to stream staged content revision source")?;
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(self.absolute_object_key(storage_key))
                .body(body)
                .send()
                .await
                .with_context(|| format!("failed to write object storage key {storage_key}"))?;
            Ok(())
        }
        .await;
        drop(temp_path);
        write_result
    }

    pub(super) async fn has(&self, storage_key: &str) -> Result<bool, ContentServiceError> {
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
                    Err(anyhow!(error)
                        .context(format!("failed to inspect object storage key {storage_key}"))
                        .into())
                }
            }
        }
    }

    /// Removes exactly one stored object, reusing [`Self::delete_objects`].
    /// Returns whether the object existed before deletion; a missing object is
    /// not an error, so reclaiming an orphan is idempotent.
    pub(super) async fn delete(&self, storage_key: &str) -> Result<bool, ContentServiceError> {
        let existed = self.has(storage_key).await?;
        let absolute_key = self.absolute_object_key(storage_key);
        self.delete_objects(std::slice::from_ref(&absolute_key)).await?;
        Ok(existed)
    }

    pub(super) async fn read_bounded(
        &self,
        storage_key: &str,
        max_bytes: usize,
    ) -> Result<BoundedRevisionSource, ContentServiceError> {
        let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.absolute_object_key(storage_key))
            .range(format!("bytes=0-{max_bytes_u64}"))
            .send()
            .await
            .with_context(|| format!("failed to fetch object storage key {storage_key}"))?;
        let (range_start, range_end, byte_size) =
            response.content_range().and_then(parse_content_range).ok_or_else(|| {
                anyhow!("object storage returned an invalid ranged response for {storage_key}")
            })?;
        let response_length =
            response.content_length().and_then(|value| u64::try_from(value).ok()).ok_or_else(
                || anyhow!("object storage omitted ranged response length for {storage_key}"),
            )?;
        let expected_length = range_end.saturating_sub(range_start).saturating_add(1);
        if range_start != 0
            || range_end > max_bytes_u64
            || range_end >= byte_size
            || response_length != expected_length
        {
            return Err(anyhow!(
                "object storage returned an inconsistent ranged response for {storage_key}"
            )
            .into());
        }
        if byte_size > max_bytes_u64 {
            return Ok(BoundedRevisionSource::LimitExceeded { byte_size });
        }

        let mut body = response.body.into_async_read();
        let bytes = read_bounded_bytes(&mut body, max_bytes)
            .await
            .with_context(|| format!("failed to read object storage body for {storage_key}"))?;
        if u64::try_from(bytes.len()).ok() != Some(response_length) {
            return Err(anyhow!(
                "object storage body length did not match ranged response for {storage_key}"
            )
            .into());
        }
        Ok(BoundedRevisionSource::Complete(bytes))
    }

    pub(super) async fn read(&self, storage_key: &str) -> Result<Vec<u8>, ContentServiceError> {
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

    pub(super) async fn presign_download(
        &self,
        storage_key: &str,
        content_disposition: &str,
        content_type: &str,
    ) -> Result<String, ContentServiceError> {
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

    pub(super) async fn stash_prefix(
        &self,
        relative_directory: &str,
    ) -> Result<Option<StashedContentDirectory>, ContentServiceError> {
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

    pub(super) async fn restore_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
        replace_current: bool,
    ) -> Result<(), ContentServiceError> {
        let original_prefix = stashed_directory.original_path.to_string_lossy().to_string();
        if replace_current {
            let current_objects = self.list_prefixed_objects(&original_prefix).await?;
            if !current_objects.is_empty() {
                self.delete_objects(&current_objects).await?;
            }
        }
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
        self.delete_objects(&objects).await.map_err(Into::into)
    }

    pub(super) async fn purge_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> Result<(), ContentServiceError> {
        let stashed_prefix = stashed_directory.stashed_path.to_string_lossy().to_string();
        let objects = self.list_prefixed_objects(&stashed_prefix).await?;
        self.delete_objects(&objects).await.map_err(Into::into)
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

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn normalize_prefix(value: String) -> String {
    trim_prefix(&value).trim_end_matches('/').to_string()
}

fn trim_prefix(value: &str) -> &str {
    value.trim().trim_start_matches('/')
}

#[cfg(test)]
mod tests {
    use super::parse_content_range;

    #[test]
    fn parses_valid_s3_content_range() {
        assert_eq!(parse_content_range("bytes 0-5/24"), Some((0, 5, 24)));
    }

    #[test]
    fn rejects_missing_or_unknown_content_range() {
        assert_eq!(parse_content_range("bytes 0-5/*"), None);
        assert_eq!(parse_content_range("0-5/24"), None);
        assert_eq!(parse_content_range("bytes */24"), None);
    }
}
