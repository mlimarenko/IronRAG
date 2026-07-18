//! API reference surface contracts.

use serde::{Deserialize, Serialize};

use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Serialization format used for a rendered API description.
pub enum ApiReferenceFormat {
    /// `OpenAPI` document encoded as YAML.
    OpenApiYaml,
    /// `OpenAPI` document encoded as JSON.
    OpenApiJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Availability of the generated API description.
pub enum ApiReferenceStatus {
    /// Generation or retrieval is still in progress.
    Loading,
    /// The description is available in `body`.
    Ready,
    /// The description could not be produced or retrieved.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// State returned to clients that display the interactive API reference.
pub struct ApiReferenceSurface {
    /// Current availability of the description.
    pub status: ApiReferenceStatus,
    /// Public route from which the raw description can be fetched.
    pub document_path: String,
    /// Origin clients should use when trying operations from the reference UI.
    pub server_origin: Option<String>,
    /// Encoding used by `body` and the document route.
    pub document_format: ApiReferenceFormat,
    /// Renderable API description when generation succeeded.
    pub body: Option<String>,
    /// Operator-facing explanation when the description is not ready.
    pub message: Option<String>,
    /// Non-fatal conditions that may affect reference accuracy or availability.
    pub warnings: Vec<OperatorWarning>,
}
