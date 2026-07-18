//! Shared transport contracts for `IronRAG` HTTP and `OpenAPI` surfaces.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Administration and operator-facing transport contracts.
pub mod admin;
/// Canonical AI binding purpose contracts.
pub mod ai;
/// API reference surface contracts.
pub mod apiref;
/// Assistant conversation, evidence, and execution contracts.
pub mod assistant;
/// Authentication, authorization, and bootstrap contracts.
pub mod auth;
/// Health, warning, and diagnostic state contracts.
pub mod diagnostics;
/// Document lifecycle and ingestion contracts.
pub mod documents;
/// Knowledge-graph topology and inspection contracts.
pub mod graph;
/// AI provider capability and credential-policy contracts.
pub mod provider;
/// Application shell, viewer, and scope-selection contracts.
pub mod shell;
