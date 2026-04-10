use std::path::PathBuf;

use crate::domains::deployment::{ContentStorageProvider, DeploymentTopology};

#[derive(Clone, Debug)]
pub struct ContentStorageS3Settings {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub force_path_style: bool,
}

#[derive(Clone, Debug)]
pub struct ContentStorageDiagnostics {
    pub provider: ContentStorageProvider,
    pub topology: DeploymentTopology,
    pub key_prefix: String,
    pub root_path: Option<PathBuf>,
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentStorageProbeStatus {
    Ok,
    Down,
    Unsupported,
    Misconfigured,
}

#[derive(Clone, Debug)]
pub struct ContentStorageProbe {
    pub status: ContentStorageProbeStatus,
    pub message: Option<String>,
}
