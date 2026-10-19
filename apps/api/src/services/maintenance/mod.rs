//! Maintenance pipeline: durable leases, sweepers, scheduler.
//!
//! Layout:
//! * [`lease`] — durable lease infrastructure backed by `maintenance_job_run`.
//!   Every sweeper acquires a lease through this module; the scheduler loop
//!   and the operator-facing CLI share the same primitives.
//!
//! Sibling sweeper modules (`gc`, `repair`, `retention`, `rebuild`, …) hang
//! off this parent so the scheduler tick and the `ironrag-maintenance` CLI
//! both depend on a single shared surface rather than on ad-hoc one-off
//! binaries.

pub mod admission_visibility_policy_schema;
pub mod ai_binding_purpose_schema;
pub mod baseline_delta;
pub mod content_visibility_intent_schema;
pub mod credential_secrets;
pub mod decision_catalog_schema;
pub mod gc;
pub mod knowledge_projection_metadata;
pub mod lease;
pub mod ops_failure_disposition_schema;
pub mod orphan_knowledge_documents;
pub mod orphan_runtime_reconcile;
pub mod rebuild;
pub mod repair;
pub mod retention;
pub mod scheduler;
pub mod service_identity_schema;
pub mod vector_lifecycle_inventory;
pub mod vector_lifecycle_metrics;
pub mod vector_lifecycle_observability;
pub mod vector_orphan_reconcile;
pub mod webhook_outbox_ops;
