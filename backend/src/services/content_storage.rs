use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow};
use tokio::fs;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ContentStorageService {
    root: PathBuf,
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
}
