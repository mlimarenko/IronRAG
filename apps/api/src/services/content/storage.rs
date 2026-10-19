mod filesystem;
mod s3;
pub mod types;

use std::path::{Component, Path, PathBuf};

use anyhow::anyhow;
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncRead, AsyncReadExt};
use uuid::Uuid;

use crate::{
    app::config::Settings,
    domains::deployment::{ContentStorageProvider, DeploymentTopology},
    services::content::error::ContentServiceError,
};

use self::{
    filesystem::FilesystemContentStorageProvider,
    s3::S3ContentStorageProvider,
    types::{
        ContentStorageDiagnostics, ContentStorageProbe, ContentStorageProbeStatus,
        ContentStorageS3Settings,
    },
};

#[derive(Clone, Debug)]
enum ContentStorageBackend {
    Filesystem(FilesystemContentStorageProvider),
    S3(S3ContentStorageProvider),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoundedRevisionSource {
    Complete(Vec<u8>),
    LimitExceeded { byte_size: u64 },
}

async fn read_bounded_bytes<R>(reader: &mut R, max_bytes: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let detection_limit = max_bytes.saturating_add(1);
    let mut bytes = vec![0; detection_limit];
    let mut byte_count = 0;
    while byte_count < detection_limit {
        let read = reader.read(&mut bytes[byte_count..]).await?;
        if read == 0 {
            break;
        }
        byte_count += read;
    }
    bytes.truncate(byte_count);
    Ok(bytes)
}

#[derive(Clone, Debug)]
pub struct ContentStorageService {
    backend: ContentStorageBackend,
    diagnostics: ContentStorageDiagnostics,
}

#[derive(Debug, Clone)]
pub struct StashedContentDirectory {
    original_path: PathBuf,
    stashed_path: PathBuf,
}

impl StashedContentDirectory {
    #[must_use]
    pub fn original_path(&self) -> &Path {
        &self.original_path
    }

    #[must_use]
    pub fn stashed_path(&self) -> &Path {
        &self.stashed_path
    }
}

impl ContentStorageService {
    pub fn from_settings(settings: &Settings) -> Result<Self, ContentServiceError> {
        let provider = settings.content_storage_provider_kind().map_err(|error| anyhow!(error))?;
        let topology = settings.content_storage_topology_kind().map_err(|error| anyhow!(error))?;
        let key_prefix = settings.content_storage_key_prefix.trim().trim_matches('/').to_string();

        match provider {
            ContentStorageProvider::Filesystem => Ok(Self {
                backend: ContentStorageBackend::Filesystem(FilesystemContentStorageProvider::new(
                    settings.content_storage_root.clone(),
                )),
                diagnostics: ContentStorageDiagnostics {
                    provider,
                    topology,
                    key_prefix,
                    root_path: Some(settings.content_storage_root.clone().into()),
                    endpoint: None,
                    bucket: None,
                },
            }),
            ContentStorageProvider::S3 => {
                let endpoint = required_s3_setting(
                    "content_storage_s3_endpoint",
                    settings.content_storage_s3_endpoint.as_deref(),
                )?;
                let bucket = required_s3_setting(
                    "content_storage_s3_bucket",
                    settings.content_storage_s3_bucket.as_deref(),
                )?;
                let access_key_id = required_s3_setting(
                    "content_storage_s3_access_key_id",
                    settings.content_storage_s3_access_key_id.as_deref(),
                )?;
                let secret_access_key = required_s3_setting(
                    "content_storage_s3_secret_access_key",
                    settings.content_storage_s3_secret_access_key.as_deref(),
                )?;
                let region = settings
                    .content_storage_s3_region
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("us-east-1")
                    .to_string();
                let backend = S3ContentStorageProvider::new(
                    ContentStorageS3Settings {
                        endpoint: endpoint.clone(),
                        bucket: bucket.clone(),
                        region,
                        access_key_id,
                        secret_access_key,
                        session_token: settings
                            .content_storage_s3_session_token
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(std::string::ToString::to_string),
                        force_path_style: settings.content_storage_s3_force_path_style,
                    },
                    key_prefix.clone(),
                )?;
                Ok(Self {
                    backend: ContentStorageBackend::S3(backend),
                    diagnostics: ContentStorageDiagnostics {
                        provider,
                        topology,
                        key_prefix,
                        root_path: None,
                        endpoint: Some(endpoint),
                        bucket: Some(bucket),
                    },
                })
            }
        }
    }

    #[must_use]
    pub const fn diagnostics(&self) -> &ContentStorageDiagnostics {
        &self.diagnostics
    }

    pub async fn prepare_startup(&self) -> Result<ContentStorageProbe, ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.prepare_and_validate().await,
            ContentStorageBackend::S3(provider) => provider.prepare_and_validate().await,
        }
    }

    pub async fn probe(&self) -> ContentStorageProbe {
        if matches!(self.diagnostics.provider, ContentStorageProvider::Filesystem)
            && !matches!(self.diagnostics.topology, DeploymentTopology::SingleNode)
        {
            return ContentStorageProbe {
                status: ContentStorageProbeStatus::Unsupported,
                message: Some(
                    "filesystem content storage is supported only with content_storage_topology=single_node"
                        .to_string(),
                ),
            };
        }
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.probe().await,
            ContentStorageBackend::S3(provider) => provider.probe().await,
        }
    }

    pub async fn persist_revision_source(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        file_name: &str,
        checksum: &str,
        file_bytes: &[u8],
    ) -> Result<String, ContentServiceError> {
        let storage_key =
            Self::build_revision_storage_key(workspace_id, library_id, file_name, checksum);
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.persist(&storage_key, file_bytes).await?;
            }
            ContentStorageBackend::S3(provider) => {
                provider.persist(&storage_key, file_bytes).await?;
            }
        }
        Ok(storage_key)
    }

    pub async fn persist_web_snapshot(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        source_uri: &str,
        checksum: &str,
        file_bytes: &[u8],
    ) -> Result<String, ContentServiceError> {
        let file_name = build_web_snapshot_file_name(source_uri);
        self.persist_revision_source(workspace_id, library_id, &file_name, checksum, file_bytes)
            .await
    }

    #[must_use]
    pub fn build_revision_storage_key(
        workspace_id: Uuid,
        library_id: Uuid,
        file_name: &str,
        checksum: &str,
    ) -> String {
        build_revision_storage_key(workspace_id, library_id, file_name, checksum)
    }

    /// The storage key [`Self::persist_web_snapshot`] will write for the same
    /// arguments, so a caller can arm its reclaim guard before the write.
    #[must_use]
    pub fn build_web_snapshot_storage_key(
        workspace_id: Uuid,
        library_id: Uuid,
        source_uri: &str,
        checksum: &str,
    ) -> String {
        let file_name = build_web_snapshot_file_name(source_uri);
        build_revision_storage_key(workspace_id, library_id, &file_name, checksum)
    }

    pub async fn has_revision_source(
        &self,
        storage_key: &str,
    ) -> Result<bool, ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.has(storage_key).await,
            ContentStorageBackend::S3(provider) => provider.has(storage_key).await,
        }
    }

    /// Removes exactly one revision-source object.
    ///
    /// This is the compensating action for a staged blob whose admission never
    /// committed: `PersistedRevisionSourceGuard` reaches it through the
    /// reclaimer on abnormal drop, and that reclaimer holds the per-library
    /// content storage advisory lock across the reference check and this delete,
    /// so no concurrent admission of the same content-addressed key can be
    /// between its blob write and its commit. The key is accepted only when it
    /// belongs to the supplied workspace/library (revision keys are
    /// `content/{workspace}/{library}/{digest}-{name}`), so a mistaken caller
    /// cannot delete another tenant's object. Returns whether an object was
    /// removed; deleting an absent key is not an error.
    ///
    pub async fn delete_revision_source(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        storage_key: &str,
    ) -> Result<bool, ContentServiceError> {
        let expected_prefix = format!("content/{workspace_id}/{library_id}/");
        if !storage_key.starts_with(&expected_prefix) {
            return Err(ContentServiceError::InvalidRequest {
                message: format!(
                    "content storage key does not belong to workspace {workspace_id} library {library_id}"
                ),
            });
        }
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.delete(storage_key).await,
            ContentStorageBackend::S3(provider) => provider.delete(storage_key).await,
        }
    }

    pub(crate) async fn read_revision_source_bounded(
        &self,
        storage_key: &str,
        max_bytes: usize,
    ) -> Result<BoundedRevisionSource, ContentServiceError> {
        if max_bytes == 0 {
            return Err(ContentServiceError::InvalidRequest {
                message: "content revision source read limit must be greater than zero".to_string(),
            });
        }
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.read_bounded(storage_key, max_bytes).await
            }
            ContentStorageBackend::S3(provider) => {
                provider.read_bounded(storage_key, max_bytes).await
            }
        }
    }

    pub async fn read_revision_source(
        &self,
        storage_key: &str,
    ) -> Result<Vec<u8>, ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.read(storage_key).await,
            ContentStorageBackend::S3(provider) => provider.read(storage_key).await,
        }
    }

    /// Writes bytes to the exact `storage_key` supplied by the caller.
    ///
    /// This is used by the library snapshot import path to
    /// restore blobs under the storage keys recorded in the source
    /// deployment, preserving referential integrity with the
    /// `content_revision.storage_key` column. Callers outside the
    /// snapshot path should use [`Self::persist_revision_source`] which
    /// computes the key deterministically.
    pub async fn write_revision_source_raw(
        &self,
        storage_key: &str,
        file_bytes: &[u8],
    ) -> Result<(), ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.write(storage_key, file_bytes).await
            }
            ContentStorageBackend::S3(provider) => provider.write(storage_key, file_bytes).await,
        }
    }

    pub async fn write_revision_source_raw_stream<R>(
        &self,
        storage_key: &str,
        reader: &mut R,
        max_bytes: u64,
    ) -> Result<(), ContentServiceError>
    where
        R: AsyncRead + Unpin + Send,
    {
        validate_raw_storage_key(storage_key)?;
        if max_bytes == 0 {
            return Err(ContentServiceError::InvalidRequest {
                message: "content revision source stream limit must be greater than zero"
                    .to_string(),
            });
        }
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.write_stream(storage_key, reader, max_bytes).await
            }
            ContentStorageBackend::S3(provider) => {
                provider.write_stream(storage_key, reader, max_bytes).await
            }
        }
    }

    pub async fn resolve_download_redirect_url(
        &self,
        storage_key: &str,
        content_disposition: &str,
        content_type: &str,
    ) -> Result<Option<String>, ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(_) => Ok(None),
            ContentStorageBackend::S3(provider) => provider
                .presign_download(storage_key, content_disposition, content_type)
                .await
                .map(Some),
        }
    }

    pub async fn stash_library_storage(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<Option<StashedContentDirectory>, ContentServiceError> {
        self.stash_relative_directory(&format!("content/{workspace_id}/{library_id}")).await
    }

    pub async fn stash_workspace_storage(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<StashedContentDirectory>, ContentServiceError> {
        self.stash_relative_directory(&format!("content/{workspace_id}")).await
    }

    pub async fn restore_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> Result<(), ContentServiceError> {
        self.restore_stashed_directory_with_policy(stashed_directory, false).await
    }

    pub async fn restore_stashed_directory_replacing_current(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> Result<(), ContentServiceError> {
        self.restore_stashed_directory_with_policy(stashed_directory, true).await
    }

    async fn restore_stashed_directory_with_policy(
        &self,
        stashed_directory: &StashedContentDirectory,
        replace_current: bool,
    ) -> Result<(), ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.restore_stashed_directory(stashed_directory, replace_current).await
            }
            ContentStorageBackend::S3(provider) => {
                provider.restore_stashed_directory(stashed_directory, replace_current).await
            }
        }
    }

    pub async fn purge_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> Result<(), ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.purge_stashed_directory(stashed_directory).await
            }
            ContentStorageBackend::S3(provider) => {
                provider.purge_stashed_directory(stashed_directory).await
            }
        }
    }

    async fn stash_relative_directory(
        &self,
        relative_directory: &str,
    ) -> Result<Option<StashedContentDirectory>, ContentServiceError> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.stash_prefix(relative_directory).await
            }
            ContentStorageBackend::S3(provider) => provider.stash_prefix(relative_directory).await,
        }
    }
}

fn validate_raw_storage_key(storage_key: &str) -> Result<(), ContentServiceError> {
    let relative = Path::new(storage_key);
    if storage_key.trim().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(component, Component::CurDir | Component::ParentDir | Component::RootDir)
        })
    {
        return Err(ContentServiceError::InvalidRequest {
            message: "content storage key must be a non-empty relative path".to_string(),
        });
    }
    Ok(())
}

fn build_revision_storage_key(
    workspace_id: Uuid,
    library_id: Uuid,
    file_name: &str,
    checksum: &str,
) -> String {
    let safe_file_name = sanitize_file_name(file_name);
    let digest = checksum.strip_prefix("sha256:").unwrap_or(checksum);
    format!("content/{workspace_id}/{library_id}/{digest}-{safe_file_name}")
}

const MAX_STORAGE_FILE_NAME_LEN: usize = 140;

/// Prevents path traversal attacks by stripping directory separators, `.` prefixes,
/// and null bytes from user-supplied file names. This is a security-critical function —
/// do not remove or weaken the sanitization logic.
fn sanitize_file_name(file_name: &str) -> String {
    let trimmed = file_name.trim();
    let mut sanitized = trimmed
        .chars()
        .map(
            |ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') { ch } else { '-' }
            },
        )
        .collect::<String>();
    while sanitized.contains("--") {
        sanitized = sanitized.replace("--", "-");
    }
    let sanitized = sanitized.trim_matches('-').trim_matches('.').to_string();
    let sanitized = if sanitized.is_empty() { "document.bin".to_string() } else { sanitized };
    truncate_file_name(&sanitized, MAX_STORAGE_FILE_NAME_LEN)
}

fn build_web_snapshot_file_name(source_uri: &str) -> String {
    let parsed = reqwest::Url::parse(source_uri).ok();
    let file_name = parsed
        .as_ref()
        .and_then(url::Url::path_segments)
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .filter(|segment| !segment.trim().is_empty())
        .map_or_else(
            || "index.html".to_string(),
            |segment| percent_decode_str(segment).decode_utf8_lossy().into_owned(),
        );
    sanitize_file_name(&file_name)
}

fn truncate_file_name(file_name: &str, max_len: usize) -> String {
    if file_name.len() <= max_len {
        return file_name.to_string();
    }

    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            (stem, Some(extension))
        }
        _ => (file_name, None),
    };

    let reserved = extension.map_or(0, |value| value.len() + 1);
    let max_stem_len = max_len.saturating_sub(reserved).max(1);
    let truncated_stem = stem.chars().take(max_stem_len).collect::<String>();

    match extension {
        Some(extension) if !truncated_stem.is_empty() => {
            format!("{truncated_stem}.{extension}")
        }
        _ => truncated_stem,
    }
}

fn required_s3_setting(name: &str, value: Option<&str>) -> anyhow::Result<String> {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(std::string::ToString::to_string)
        .ok_or_else(|| anyhow!("{name} must not be empty"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::io::AsyncWriteExt;
    use uuid::Uuid;

    use crate::domains::deployment::DeploymentTopology;

    use super::{
        BoundedRevisionSource, ContentStorageService, MAX_STORAGE_FILE_NAME_LEN,
        build_web_snapshot_file_name, read_bounded_bytes, truncate_file_name,
        types::ContentStorageDiagnostics,
    };

    fn filesystem_storage(tempdir: &tempfile::TempDir) -> ContentStorageService {
        ContentStorageService {
            backend: super::ContentStorageBackend::Filesystem(
                super::filesystem::FilesystemContentStorageProvider::new(tempdir.path()),
            ),
            diagnostics: ContentStorageDiagnostics {
                provider: crate::domains::deployment::ContentStorageProvider::Filesystem,
                topology: DeploymentTopology::SingleNode,
                key_prefix: String::new(),
                root_path: Some(tempdir.path().to_path_buf()),
                endpoint: None,
                bucket: None,
            },
        }
    }

    #[tokio::test]
    async fn persist_and_read_revision_source_round_trips_bytes() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let bytes = b"hello from storage";

        let storage_key = storage
            .persist_revision_source(
                workspace_id,
                library_id,
                "runtime-upload-check.pdf",
                "sha256:abc123",
                bytes,
            )
            .await
            .expect("persist source");

        assert!(storage_key.contains("content/"));
        assert!(storage_key.ends_with("abc123-runtime-upload-check.pdf"));

        let loaded = storage.read_revision_source(&storage_key).await.expect("read source");
        assert_eq!(loaded, bytes);
    }

    #[tokio::test]
    async fn delete_revision_source_removes_persisted_object() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();

        let storage_key = storage
            .persist_revision_source(
                workspace_id,
                library_id,
                "reference.md",
                "sha256:aa11",
                b"bytes",
            )
            .await
            .expect("persist source");
        assert!(storage.has_revision_source(&storage_key).await.expect("present"));

        let removed = storage
            .delete_revision_source(workspace_id, library_id, &storage_key)
            .await
            .expect("delete source");
        assert!(removed, "the first delete must report the object removed");
        assert!(!storage.has_revision_source(&storage_key).await.expect("absent"));

        let removed_again = storage
            .delete_revision_source(workspace_id, library_id, &storage_key)
            .await
            .expect("delete absent source");
        assert!(!removed_again, "deleting an absent key is idempotent");
    }

    #[tokio::test]
    async fn delete_revision_source_rejects_foreign_scope() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let storage_key = "content/00000000-0000-0000-0000-000000000000/\
             00000000-0000-0000-0000-000000000000/foreign.bin";

        let error = storage
            .delete_revision_source(workspace_id, library_id, storage_key)
            .await
            .expect_err("foreign key must be rejected");
        assert!(matches!(
            error,
            crate::services::content::error::ContentServiceError::InvalidRequest { .. }
        ));
    }

    #[tokio::test]
    async fn bounded_revision_source_read_returns_bytes_within_limit() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let storage_key = "content/test/image.png";
        storage
            .write_revision_source_raw(storage_key, b"image bytes")
            .await
            .expect("persist image");

        let result = storage
            .read_revision_source_bounded(storage_key, 11)
            .await
            .expect("read bounded image");

        assert_eq!(result, BoundedRevisionSource::Complete(b"image bytes".to_vec()));
    }

    #[tokio::test]
    async fn bounded_byte_reader_allocates_only_the_limit_and_detection_byte() {
        let mut source = std::io::Cursor::new(b"image bytes beyond limit".to_vec());

        let bytes = read_bounded_bytes(&mut source, 5).await.expect("read bounded bytes");

        assert_eq!(bytes, b"image ");
        assert_eq!(bytes.capacity(), 6);
    }

    #[tokio::test]
    async fn bounded_revision_source_read_reports_oversized_source_without_returning_bytes() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let storage_key = "content/test/oversized-image.png";
        storage
            .write_revision_source_raw(storage_key, b"image bytes beyond limit")
            .await
            .expect("persist oversized image");

        let result = storage
            .read_revision_source_bounded(storage_key, 5)
            .await
            .expect("inspect bounded image");

        assert_eq!(result, BoundedRevisionSource::LimitExceeded { byte_size: 24 });
    }

    #[tokio::test]
    async fn raw_snapshot_stream_write_overwrites_existing_source_bytes() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let storage_key = storage
            .persist_revision_source(
                workspace_id,
                library_id,
                "runtime-upload-check.pdf",
                "sha256:abc123",
                b"stale bytes",
            )
            .await
            .expect("persist stale source");
        let (mut writer, mut reader) = tokio::io::duplex(16);
        let write_task = tokio::spawn(async move {
            writer.write_all(b"snapshot bytes").await.expect("write snapshot bytes");
        });

        storage
            .write_revision_source_raw_stream(&storage_key, &mut reader, 32)
            .await
            .expect("overwrite raw source from stream");
        write_task.await.expect("writer task");

        let loaded = storage.read_revision_source(&storage_key).await.expect("read source");
        assert_eq!(loaded, b"snapshot bytes");
    }

    #[tokio::test]
    async fn raw_snapshot_stream_write_rejects_input_larger_than_bound_without_promoting() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let storage_key = "content/test/source.bin";
        storage
            .write_revision_source_raw(storage_key, b"existing bytes")
            .await
            .expect("persist existing source");
        let mut reader = std::io::Cursor::new(b"too-large".to_vec());

        let error = storage
            .write_revision_source_raw_stream(storage_key, &mut reader, 3)
            .await
            .expect_err("oversized stream must fail");

        assert!(matches!(
            error,
            crate::services::content::error::ContentServiceError::InvalidRequest { .. }
        ));
        let loaded = storage.read_revision_source(storage_key).await.expect("read existing source");
        assert_eq!(loaded, b"existing bytes");
    }

    #[tokio::test]
    async fn raw_snapshot_stream_write_rejects_zero_byte_bound() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let mut reader = std::io::Cursor::new(b"snapshot bytes".to_vec());

        let error = storage
            .write_revision_source_raw_stream("content/test/source.bin", &mut reader, 0)
            .await
            .expect_err("zero-byte bound must fail");

        assert!(matches!(
            error,
            crate::services::content::error::ContentServiceError::InvalidRequest { .. }
        ));
    }

    #[tokio::test]
    async fn raw_snapshot_stream_write_rejects_invalid_storage_key() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let mut reader = std::io::Cursor::new(b"snapshot bytes".to_vec());

        let error = storage
            .write_revision_source_raw_stream("../outside.bin", &mut reader, 32)
            .await
            .expect_err("invalid storage key must fail");

        assert!(matches!(
            error,
            crate::services::content::error::ContentServiceError::InvalidRequest { .. }
        ));
    }

    #[tokio::test]
    async fn stash_restore_and_purge_library_storage_round_trips_directory() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();

        let storage_key = storage
            .persist_revision_source(
                workspace_id,
                library_id,
                "reference.md",
                "sha256:def456",
                b"demo corpus",
            )
            .await
            .expect("persist source");

        let stashed = storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .expect("stash library")
            .expect("stashed directory");

        assert!(!storage.has_revision_source(&storage_key).await.expect("source inspection"));

        let replacement_key = storage
            .persist_revision_source(
                workspace_id,
                library_id,
                "replacement.md",
                "sha256:replacement",
                b"partial replacement",
            )
            .await
            .expect("persist replacement source");
        assert!(
            storage.has_revision_source(&replacement_key).await.expect("replacement inspection")
        );

        storage
            .restore_stashed_directory_replacing_current(&stashed)
            .await
            .expect("restore stashed directory");
        assert!(storage.has_revision_source(&storage_key).await.expect("source inspection"));
        assert!(
            !storage.has_revision_source(&replacement_key).await.expect("replacement inspection")
        );

        let stashed_again = storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .expect("stash library again")
            .expect("stashed directory");
        storage.purge_stashed_directory(&stashed_again).await.expect("purge stashed directory");

        assert!(!storage.has_revision_source(&storage_key).await.expect("source inspection"));
    }

    #[tokio::test]
    async fn persist_web_snapshot_decodes_and_bounds_long_attachment_file_name() {
        let tempdir = tempdir().expect("tempdir");
        let storage = filesystem_storage(&tempdir);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let source_uri = "https://docs.example.com/download/attachments/40468542/2023_09_01_%D0%BC%D0%B5%D1%82%D0%BE%D0%B4%D0%B8%D1%87%D0%B5%D1%81%D0%BA%D0%B8%D0%B5_%D1%80%D0%B5%D0%BA%D0%BE%D0%BC%D0%B5%D0%BD%D0%B4%D0%B0%D1%86%D0%B8%D0%B8_%D0%B201_3_rev_%D0%B8_%D0%BE%D1%84%D0%BB%D0%B0%D0%B8%CC%86%D0%BD_2.pdf?version=1&modificationDate=1712205956919&api=v2";
        let bytes = b"pdf bytes";

        let storage_key = storage
            .persist_web_snapshot(workspace_id, library_id, source_uri, "sha256:feedbeef", bytes)
            .await
            .expect("persist web snapshot");

        let leaf = storage_key.rsplit('/').next().expect("storage leaf");
        assert!(leaf.len() <= 64 + 1 + MAX_STORAGE_FILE_NAME_LEN);
        assert!(leaf.ends_with("2023_09_01_-_-_-01_3_rev_-_-_2.pdf"));

        let loaded = storage.read_revision_source(&storage_key).await.expect("read source");
        assert_eq!(loaded, bytes);
    }

    #[test]
    fn truncate_file_name_preserves_extension_within_limit() {
        let file_name = format!("{}.pdf", "a".repeat(400));
        let truncated = truncate_file_name(&file_name, MAX_STORAGE_FILE_NAME_LEN);

        assert!(truncated.len() <= MAX_STORAGE_FILE_NAME_LEN);
        assert!(truncated.ends_with(".pdf"));
    }

    #[test]
    fn build_web_snapshot_file_name_decodes_percent_encoded_segments() {
        let file_name = build_web_snapshot_file_name(
            "https://docs.example.com/download/attachments/40468542/2023_09_01_%D0%BC%D0%B5%D1%82%D0%BE%D0%B4%D0%B8%D1%87%D0%B5%D1%81%D0%BA%D0%B8%D0%B5_%D1%80%D0%B5%D0%BA%D0%BE%D0%BC%D0%B5%D0%BD%D0%B4%D0%B0%D1%86%D0%B8%D0%B8_%D0%B201_3_rev_%D0%B8_%D0%BE%D1%84%D0%BB%D0%B0%D0%B8%CC%86%D0%BD_2.pdf?version=1&modificationDate=1712205956919&api=v2",
        );

        assert_eq!(file_name, "2023_09_01_-_-_-01_3_rev_-_-_2.pdf");
    }
}
