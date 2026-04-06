use serde::{Deserialize, Serialize};

use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiReferenceFormat {
    OpenApiYaml,
    OpenApiJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiReferenceStatus {
    Loading,
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiReferenceSurface {
    pub status: ApiReferenceStatus,
    pub document_path: String,
    pub server_origin: Option<String>,
    pub document_format: ApiReferenceFormat,
    pub body: Option<String>,
    pub message: Option<String>,
    pub warnings: Vec<OperatorWarning>,
}
