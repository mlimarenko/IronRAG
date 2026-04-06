use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow};
use tokio::fs;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ContentStorageService {
    root: PathBuf,
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
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn persist_revision_source(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        file_name: &str,
        checksum: &str,
        file_bytes: &[u8],
    ) -> anyhow::Result<String> {
        let storage_key =
            Self::build_revision_storage_key(workspace_id, library_id, file_name, checksum);
        let target_path = self.resolve_storage_path(&storage_key)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create content storage directory {}", parent.display())
            })?;
        }
        if fs::try_exists(&target_path)
            .await
            .with_context(|| format!("failed to inspect {}", target_path.display()))?
        {
            return Ok(storage_key);
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
        Ok(storage_key)
    }

    pub async fn persist_web_snapshot(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        source_uri: &str,
        checksum: &str,
        file_bytes: &[u8],
    ) -> anyhow::Result<String> {
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

    pub async fn has_revision_source(&self, storage_key: &str) -> anyhow::Result<bool> {
        let path = self.resolve_storage_path(storage_key)?;
        fs::try_exists(&path)
            .await
            .with_context(|| format!("failed to inspect stored content source {}", path.display()))
    }

    pub async fn read_revision_source(&self, storage_key: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.resolve_storage_path(storage_key)?;
        fs::read(&path)
            .await
            .with_context(|| format!("failed to read stored content source {}", path.display()))
    }

    pub async fn stash_library_storage(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<Option<StashedContentDirectory>> {
        self.stash_relative_directory(&format!("content/{workspace_id}/{library_id}")).await
    }

    pub async fn stash_workspace_storage(
        &self,
        workspace_id: Uuid,
    ) -> anyhow::Result<Option<StashedContentDirectory>> {
        self.stash_relative_directory(&format!("content/{workspace_id}")).await
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

    async fn stash_relative_directory(
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

        let stash_root = self.root.join(".trash");
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
    if sanitized.is_empty() { "document.bin".to_string() } else { sanitized }
}

fn build_web_snapshot_file_name(source_uri: &str) -> String {
    let parsed = reqwest::Url::parse(source_uri).ok();
    let file_name = parsed
        .as_ref()
        .and_then(|url| url.path_segments())
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or("index.html");
    sanitize_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::ContentStorageService;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[tokio::test]
    async fn persist_and_read_revision_source_round_trips_bytes() {
        let tempdir = tempdir().expect("tempdir");
        let storage = ContentStorageService::new(tempdir.path());
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
    async fn stash_restore_and_purge_library_storage_round_trips_directory() {
        let tempdir = tempdir().expect("tempdir");
        let storage = ContentStorageService::new(tempdir.path());
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

        storage.restore_stashed_directory(&stashed).await.expect("restore stashed directory");
        assert!(storage.has_revision_source(&storage_key).await.expect("source inspection"));

        let stashed_again = storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .expect("stash library again")
            .expect("stashed directory");
        storage.purge_stashed_directory(&stashed_again).await.expect("purge stashed directory");

        assert!(!storage.has_revision_source(&storage_key).await.expect("source inspection"));
    }
}
