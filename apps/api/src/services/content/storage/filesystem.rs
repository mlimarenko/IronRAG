use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt},
};
use uuid::Uuid;

use super::{
    BoundedRevisionSource, StashedContentDirectory, read_bounded_bytes,
    types::{ContentStorageProbe, ContentStorageProbeStatus},
};
use crate::services::content::error::ContentServiceError;

/// Subdirectory under storage root used to stash deleted documents
/// before permanent removal.
const CONTENT_STASH_DIRECTORY_NAME: &str = ".trash";

#[derive(Clone, Debug)]
pub(super) struct FilesystemContentStorageProvider {
    root: PathBuf,
}

impl FilesystemContentStorageProvider {
    #[must_use]
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(super) async fn prepare_and_validate(
        &self,
    ) -> Result<ContentStorageProbe, ContentServiceError> {
        fs::create_dir_all(&self.root).await.with_context(|| {
            format!("failed to create content storage root {}", self.root.display())
        })?;
        Ok(ContentStorageProbe { status: ContentStorageProbeStatus::Ok, message: None })
    }

    pub(super) async fn probe(&self) -> ContentStorageProbe {
        match fs::try_exists(&self.root).await {
            Ok(true) => {
                ContentStorageProbe { status: ContentStorageProbeStatus::Ok, message: None }
            }
            Ok(false) => ContentStorageProbe {
                status: ContentStorageProbeStatus::Misconfigured,
                message: Some(format!(
                    "filesystem content storage root {} does not exist",
                    self.root.display()
                )),
            },
            Err(error) => ContentStorageProbe {
                status: ContentStorageProbeStatus::Down,
                message: Some(format!(
                    "failed to inspect filesystem content storage root {}: {error}",
                    self.root.display()
                )),
            },
        }
    }

    pub(super) async fn persist(
        &self,
        storage_key: &str,
        file_bytes: &[u8],
    ) -> Result<(), ContentServiceError> {
        let target_path = self.resolve_storage_path(storage_key)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create content storage directory {}", parent.display())
            })?;
        }
        if fs::try_exists(&target_path)
            .await
            .with_context(|| format!("failed to inspect {}", target_path.display()))?
        {
            return Ok(());
        }

        self.write(storage_key, file_bytes).await
    }

    pub(super) async fn write(
        &self,
        storage_key: &str,
        file_bytes: &[u8],
    ) -> Result<(), ContentServiceError> {
        let target_path = self.resolve_storage_path(storage_key)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create content storage directory {}", parent.display())
            })?;
        }

        let temp_path = target_path.with_extension(format!("tmp-{}", Uuid::now_v7()));
        fs::write(&temp_path, file_bytes)
            .await
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        fs::rename(&temp_path, &target_path).await.with_context(|| {
            format!(
                "failed to promote temporary content source {} to {}",
                temp_path.display(),
                target_path.display()
            )
        })?;
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
        let target_path = self.resolve_storage_path(storage_key)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create content storage directory {}", parent.display())
            })?;
        }

        let temp_path = target_path.with_extension(format!("tmp-{}", Uuid::now_v7()));
        let write_result = async {
            let mut temp_file = fs::File::create(&temp_path)
                .await
                .with_context(|| format!("failed to create {}", temp_path.display()))?;
            let copied =
                tokio::io::copy(&mut reader.take(max_bytes.saturating_add(1)), &mut temp_file)
                    .await
                    .with_context(|| format!("failed to write {}", temp_path.display()))?;
            if copied > max_bytes {
                return Err(ContentServiceError::InvalidRequest {
                    message: format!("content revision source exceeds the {max_bytes}-byte limit"),
                });
            }
            temp_file
                .sync_all()
                .await
                .with_context(|| format!("failed to sync {}", temp_path.display()))?;
            fs::rename(&temp_path, &target_path).await.with_context(|| {
                format!(
                    "failed to promote temporary content source {} to {}",
                    temp_path.display(),
                    target_path.display()
                )
            })?;
            Ok(())
        }
        .await;
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path).await;
        }
        write_result
    }

    pub(super) async fn has(&self, storage_key: &str) -> Result<bool, ContentServiceError> {
        let path = self.resolve_storage_path(storage_key)?;
        Ok(fs::try_exists(&path).await.with_context(|| {
            format!("failed to inspect stored content source {}", path.display())
        })?)
    }

    pub(super) async fn read_bounded(
        &self,
        storage_key: &str,
        max_bytes: usize,
    ) -> Result<BoundedRevisionSource, ContentServiceError> {
        let path = self.resolve_storage_path(storage_key)?;
        let mut file = fs::File::open(&path).await.map_err(|error| {
            content_source_read_error(error, &path, "failed to read stored content source")
        })?;
        let metadata = file.metadata().await.with_context(|| {
            format!("failed to inspect stored content source {}", path.display())
        })?;
        let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        if metadata.len() > max_bytes_u64 {
            return Ok(BoundedRevisionSource::LimitExceeded { byte_size: metadata.len() });
        }

        let bytes = read_bounded_bytes(&mut file, max_bytes)
            .await
            .with_context(|| format!("failed to read stored content source {}", path.display()))?;
        if bytes.len() > max_bytes {
            let detected_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let byte_size = file
                .metadata()
                .await
                .map_or(detected_size, |current| current.len().max(detected_size));
            return Ok(BoundedRevisionSource::LimitExceeded { byte_size });
        }
        Ok(BoundedRevisionSource::Complete(bytes))
    }

    pub(super) async fn read(&self, storage_key: &str) -> Result<Vec<u8>, ContentServiceError> {
        let path = self.resolve_storage_path(storage_key)?;
        fs::read(&path).await.map_err(|error| {
            content_source_read_error(error, &path, "failed to read stored content source")
        })
    }

    /// Removes exactly one stored object. Returns whether a file was removed;
    /// a missing object is not an error, so reclaiming an already-deleted or
    /// never-written blob is idempotent.
    pub(super) async fn delete(&self, storage_key: &str) -> Result<bool, ContentServiceError> {
        let path = self.resolve_storage_path(storage_key)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(anyhow!(error)
                .context(format!("failed to delete stored content source {}", path.display()))
                .into()),
        }
    }

    pub(super) async fn stash_prefix(
        &self,
        relative_directory: &str,
    ) -> Result<Option<StashedContentDirectory>, ContentServiceError> {
        let original_path = self.resolve_storage_path(relative_directory)?;
        if !fs::try_exists(&original_path)
            .await
            .with_context(|| format!("failed to inspect {}", original_path.display()))?
        {
            return Ok(None);
        }

        let stash_root = self.root.join(CONTENT_STASH_DIRECTORY_NAME);
        fs::create_dir_all(&stash_root)
            .await
            .with_context(|| format!("failed to create {}", stash_root.display()))?;
        let stashed_path = stash_root.join(Uuid::now_v7().to_string());
        fs::rename(&original_path, &stashed_path).await.with_context(|| {
            format!(
                "failed to stash content directory {} into {}",
                original_path.display(),
                stashed_path.display()
            )
        })?;

        Ok(Some(StashedContentDirectory { original_path, stashed_path }))
    }

    pub(super) async fn restore_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
        replace_current: bool,
    ) -> Result<(), ContentServiceError> {
        if let Some(parent) = stashed_directory.original_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if replace_current
            && fs::try_exists(&stashed_directory.original_path).await.with_context(|| {
                format!("failed to inspect {}", stashed_directory.original_path.display())
            })?
        {
            fs::remove_dir_all(&stashed_directory.original_path).await.with_context(|| {
                format!(
                    "failed to remove replacement content directory {} before stash restore",
                    stashed_directory.original_path.display()
                )
            })?;
        }
        fs::rename(&stashed_directory.stashed_path, &stashed_directory.original_path)
            .await
            .with_context(|| {
                format!(
                    "failed to restore stashed content directory {} to {}",
                    stashed_directory.stashed_path.display(),
                    stashed_directory.original_path.display()
                )
            })?;
        Ok(())
    }

    pub(super) async fn purge_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> Result<(), ContentServiceError> {
        if fs::try_exists(&stashed_directory.stashed_path).await.with_context(|| {
            format!("failed to inspect {}", stashed_directory.stashed_path.display())
        })? {
            fs::remove_dir_all(&stashed_directory.stashed_path).await.with_context(|| {
                format!(
                    "failed to remove stashed content directory {}",
                    stashed_directory.stashed_path.display()
                )
            })?;
        }
        self.prune_empty_content_parents(&stashed_directory.original_path).await.map_err(Into::into)
    }

    fn resolve_storage_path(&self, storage_key: &str) -> anyhow::Result<PathBuf> {
        let relative = Path::new(storage_key);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(anyhow!("invalid content storage key {storage_key}"));
        }
        Ok(self.root.join(relative))
    }

    async fn prune_empty_content_parents(&self, original_path: &Path) -> anyhow::Result<()> {
        let content_root = self.root.join("content");
        let mut cursor = original_path.parent().map(Path::to_path_buf);
        while let Some(path) = cursor {
            if path == content_root || path == self.root {
                break;
            }
            let mut entries = fs::read_dir(&path)
                .await
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if entries
                .next_entry()
                .await
                .with_context(|| format!("failed to read {}", path.display()))?
                .is_some()
            {
                break;
            }
            fs::remove_dir(&path)
                .await
                .with_context(|| format!("failed to remove empty directory {}", path.display()))?;
            cursor = path.parent().map(Path::to_path_buf);
        }
        Ok(())
    }
}

/// Classifies a stored-source read failure so a missing blob surfaces as
/// [`ContentServiceError::NotFound`] instead of an opaque `Internal` error.
///
/// The caller can then fall back to a reconstructed source (for example the
/// revision's stored normalized text) instead of failing the whole request —
/// distinct from a permission or disk fault, which stays `Internal` because no
/// fallback can compensate for it.
fn content_source_read_error(
    error: std::io::Error,
    path: &Path,
    context: &str,
) -> ContentServiceError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return ContentServiceError::NotFound { message: format!("{context} {}", path.display()) };
    }
    ContentServiceError::Internal(anyhow!(error).context(format!("{context} {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_root() -> PathBuf {
        std::env::temp_dir().join(format!("ironrag-fs-storage-test-{}", Uuid::now_v7()))
    }

    #[tokio::test]
    async fn read_of_a_missing_blob_is_not_found_not_internal() {
        let root = scratch_root();
        let provider = FilesystemContentStorageProvider::new(&root);
        provider.prepare_and_validate().await.expect("prepare storage root");

        let error = provider
            .read("content/ws/lib/does-not-exist.html")
            .await
            .expect_err("missing blob must not read as bytes");

        assert!(
            matches!(error, ContentServiceError::NotFound { .. }),
            "expected NotFound, got {error:?}"
        );

        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn read_bounded_of_a_missing_blob_is_not_found_not_internal() {
        let root = scratch_root();
        let provider = FilesystemContentStorageProvider::new(&root);
        provider.prepare_and_validate().await.expect("prepare storage root");

        let error = provider
            .read_bounded("content/ws/lib/does-not-exist.html", 1024)
            .await
            .expect_err("missing blob must not read as bytes");

        assert!(
            matches!(error, ContentServiceError::NotFound { .. }),
            "expected NotFound, got {error:?}"
        );

        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn read_returns_bytes_that_were_actually_persisted() {
        let root = scratch_root();
        let provider = FilesystemContentStorageProvider::new(&root);
        provider.prepare_and_validate().await.expect("prepare storage root");
        let storage_key = "content/ws/lib/present.html";
        provider.persist(storage_key, b"hello world").await.expect("persist blob");

        let bytes = provider.read(storage_key).await.expect("read persisted blob");

        assert_eq!(bytes, b"hello world");
        let _ = fs::remove_dir_all(&root).await;
    }
}
