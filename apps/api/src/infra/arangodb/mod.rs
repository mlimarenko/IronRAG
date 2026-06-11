// LEGACY-SHIM(arango-era, remove>=0.7.0): entire module is the ArangoDB
// knowledge-plane backend superseded by PostgreSQL in 0.5.0 — safe to delete
// once the `IRONRAG_KNOWLEDGE_PLANE_BACKEND=arango` compatibility path and all
// 0.4.x snapshot-import tooling are retired.
//!
//! DEPRECATED ArangoDB knowledge-plane backend.
//!
//! As of 0.5.0 the canonical knowledge plane is PostgreSQL (pgvector + FTS);
//! see `apps/api/src/infra/postgres`. This module is retained ONLY as a
//! compatibility path, selected by `IRONRAG_KNOWLEDGE_PLANE_BACKEND=arango`,
//! so that older v5 snapshot archives can still be read during the manual
//! 0.4.x -> 0.5.0 upgrade. It is NOT provisioned by the Compose files or the
//! Helm chart, and it is expected to be removed entirely in a future release
//! (ArangoDB's free/community tier caps a deployment at 100 GiB, which the
//! PostgreSQL knowledge plane removes). Do not build new functionality on this
//! backend; all new code targets the PostgreSQL stores.

pub mod bootstrap;
pub mod client;
pub mod collections;
pub mod context_store;
pub mod document_store;
pub mod graph_store;
pub mod search_store;
