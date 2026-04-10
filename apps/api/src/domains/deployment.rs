use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceRole {
    Api,
    Worker,
    Startup,
}

impl ServiceRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Worker => "worker",
            Self::Startup => "startup",
        }
    }

    #[must_use]
    pub const fn runs_http_api(self) -> bool {
        matches!(self, Self::Api)
    }

    #[must_use]
    pub const fn runs_probe_api(self) -> bool {
        matches!(self, Self::Worker)
    }

    #[must_use]
    pub const fn runs_ingestion_workers(self) -> bool {
        matches!(self, Self::Worker)
    }

    #[must_use]
    pub const fn runs_startup_authority(self) -> bool {
        matches!(self, Self::Startup)
    }
}

impl fmt::Display for ServiceRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ServiceRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "api" => Ok(Self::Api),
            "worker" => Ok(Self::Worker),
            "startup" => Ok(Self::Startup),
            other => Err(format!("service_role must be one of api, worker, startup; got {other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    Postgres,
    Redis,
    ArangoDb,
    ObjectStorage,
}

impl DependencyKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Redis => "redis",
            Self::ArangoDb => "arangodb",
            Self::ObjectStorage => "object_storage",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyMode {
    Bundled,
    External,
    Disabled,
}

impl DependencyMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::External => "external",
            Self::Disabled => "disabled",
        }
    }
}

impl fmt::Display for DependencyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DependencyMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bundled" => Ok(Self::Bundled),
            "external" => Ok(Self::External),
            "disabled" => Ok(Self::Disabled),
            other => Err(format!(
                "dependency mode must be one of bundled, external, disabled; got {other}"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentStorageProvider {
    Filesystem,
    S3,
}

impl ContentStorageProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::S3 => "s3",
        }
    }
}

impl fmt::Display for ContentStorageProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ContentStorageProvider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "filesystem" => Ok(Self::Filesystem),
            "s3" => Ok(Self::S3),
            other => {
                Err(format!("content_storage_provider must be one of filesystem, s3; got {other}"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeploymentTopology {
    SingleNode,
    SharedCluster,
}

impl DeploymentTopology {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleNode => "single_node",
            Self::SharedCluster => "shared_cluster",
        }
    }
}

impl fmt::Display for DeploymentTopology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DeploymentTopology {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "single_node" => Ok(Self::SingleNode),
            "shared_cluster" => Ok(Self::SharedCluster),
            other => Err(format!(
                "content_storage_topology must be one of single_node, shared_cluster; got {other}"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupAuthorityMode {
    NotRequired,
    StartupJob,
    ComposeOneShot,
}

impl StartupAuthorityMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::StartupJob => "startup_job",
            Self::ComposeOneShot => "compose_one_shot",
        }
    }
}

impl fmt::Display for StartupAuthorityMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StartupAuthorityMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "not_required" => Ok(Self::NotRequired),
            "startup_job" => Ok(Self::StartupJob),
            "compose_one_shot" => Ok(Self::ComposeOneShot),
            other => Err(format!(
                "startup_authority_mode must be one of not_required, startup_job, compose_one_shot; got {other}"
            )),
        }
    }
}
