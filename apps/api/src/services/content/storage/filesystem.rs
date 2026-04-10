use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow};
use tokio::fs;
use uuid::Uuid;

use super::{
    StashedContentDirectory,
    types::{ContentStorageProbe, ContentStorageProbeStatus},
};

/// Subdirectory under storage root used to stash deleted documents
/// before permanent removal.
const CONTENT_STASH_DIRECTORY_NAME: &str = ".trash";

#[derive(Clone, Debug)]
pub struct FilesystemContentStorageProvider {
    root: PathBuf,
}

impl FilesystemContentStorageProvider {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub async fn prepare_and_validate(&self) -> anyhow::Result<ContentStorageProbe> {
        fs::create_dir_all(&self.root).await.with_context(|| {
            format!("failed to create content storage root {}", self.root.display())
        })?;
        Ok(ContentStorageProbe { status: ContentStorageProbeStatus::Ok, message: None })
    }

    pub async fn probe(&self) -> ContentStorageProbe {
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

    pub async fn persist(&self, storage_key: &str, file_bytes: &[u8]) -> anyhow::Result<()> {
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

    pub async fn has(&self, storage_key: &str) -> anyhow::Result<bool> {
        let path = self.resolve_storage_path(storage_key)?;
        fs::try_exists(&path)
            .await
            .with_context(|| format!("failed to inspect stored content source {}", path.display()))
    }

    pub async fn read(&self, storage_key: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.resolve_storage_path(storage_key)?;
        fs::read(&path)
            .await
            .with_context(|| format!("failed to read stored content source {}", path.display()))
    }

    pub async fn stash_prefix(
        &self,
        relative_directory: &str,
    ) -> anyhow::Result<Option<StashedContentDirectory>> {
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

    pub async fn restore_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> anyhow::Result<()> {
        if let Some(parent) = stashed_directory.original_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
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

    pub async fn purge_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> anyhow::Result<()> {
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
        self.prune_empty_content_parents(&stashed_directory.original_path).await
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
