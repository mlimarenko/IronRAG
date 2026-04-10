mod filesystem;
mod s3;
pub mod types;

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use uuid::Uuid;

use crate::{
    app::config::Settings,
    domains::deployment::{ContentStorageProvider, DeploymentTopology},
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
    pub fn from_settings(settings: &Settings) -> anyhow::Result<Self> {
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
    pub fn diagnostics(&self) -> &ContentStorageDiagnostics {
        &self.diagnostics
    }

    pub async fn prepare_startup(&self) -> anyhow::Result<ContentStorageProbe> {
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
    ) -> anyhow::Result<String> {
        let storage_key =
            Self::build_revision_storage_key(workspace_id, library_id, file_name, checksum);
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.persist(&storage_key, file_bytes).await?
            }
            ContentStorageBackend::S3(provider) => {
                provider.persist(&storage_key, file_bytes).await?
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
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.has(storage_key).await,
            ContentStorageBackend::S3(provider) => provider.has(storage_key).await,
        }
    }

    pub async fn read_revision_source(&self, storage_key: &str) -> anyhow::Result<Vec<u8>> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => provider.read(storage_key).await,
            ContentStorageBackend::S3(provider) => provider.read(storage_key).await,
        }
    }

    pub async fn resolve_download_redirect_url(
        &self,
        storage_key: &str,
        content_disposition: &str,
        content_type: &str,
    ) -> anyhow::Result<Option<String>> {
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
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.restore_stashed_directory(stashed_directory).await
            }
            ContentStorageBackend::S3(provider) => {
                provider.restore_stashed_directory(stashed_directory).await
            }
        }
    }

    pub async fn purge_stashed_directory(
        &self,
        stashed_directory: &StashedContentDirectory,
    ) -> anyhow::Result<()> {
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
    ) -> anyhow::Result<Option<StashedContentDirectory>> {
        match &self.backend {
            ContentStorageBackend::Filesystem(provider) => {
                provider.stash_prefix(relative_directory).await
            }
            ContentStorageBackend::S3(provider) => provider.stash_prefix(relative_directory).await,
        }
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
    use uuid::Uuid;

    use crate::domains::deployment::DeploymentTopology;

    use super::{ContentStorageService, types::ContentStorageDiagnostics};

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
