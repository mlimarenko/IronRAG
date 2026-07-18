//! Canonical maintenance pipeline: durable leases, sweepers, scheduler.
//!
//! Layout:
//! * [`lease`] — durable lease infrastructure backed by `maintenance_job_run`.
//!   Every sweeper acquires a lease through this module; the scheduler loop
//!   and the operator-facing CLI share the same primitives.
//!
//! Sweepers (`gc`, `audit`, `repair`, `retention`, `migrate`, `rebuild`,
//! `index`) land in sibling modules as later phases come in. Keeping them in
//! one parent module means the scheduler tick and the `ironrag-maintenance`
//! CLI both depend on a single canonical surface, not on the legacy one-off
//! binaries.

pub mod audit;
pub mod credential_secrets;
pub mod gc;
pub mod knowledge_projection_metadata;
pub mod lease;
pub mod migrate;
pub mod orphan_knowledge_documents;
pub mod rebuild;
pub mod repair;
pub mod retention;
pub mod scheduler;
pub mod vector_profile_keys;
pub mod webhook_outbox_ops;
