pub mod graph_store;
pub mod neo4j_store;
pub mod persistence;
// Canonical domain repositories now live under `infra::repositories::{catalog_repository, ...}`.
// The remaining flat helpers inside `infra::repositories` are a temporary removal boundary for the
// greenfield rewrite and must disappear once the canonical cutover is complete.
pub mod repositories;
pub mod ui_queries;
pub mod vector_search;
