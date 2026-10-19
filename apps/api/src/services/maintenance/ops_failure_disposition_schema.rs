//! Additive live upgrade adding the IRG-173 `failure_disposition` column.
//!
//! `ops_async_operation.failure_disposition` is a nullable text column with no
//! default, so every existing row stays valid under it and the previous binary
//! keeps working with it present. A second migration file is forbidden
//! (`docs/ru/CLI.md`), so an existing database reaches it through
//! `ironrag-maintenance migrate ops-failure-disposition-schema --apply`, which
//! shares the transition skeleton in
//! [`crate::services::maintenance::baseline_delta`].

use sqlx::PgPool;

use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_IRG112, REQUIRED_BASELINE_OBJECT_GROUPS_IRG173,
};
use crate::services::maintenance::baseline_delta::{
    AdditiveBaselineDeltaSpec, apply_additive_baseline_delta,
};

const ADDITIVE_SCHEMA_SQL: &str =
    include_str!("../../../ops/apply-ops-failure-disposition-schema.sql");

/// The immutable IRG-173 baseline delta: the IRG-112 revision → the
/// `failure_disposition` revision.
pub(crate) const OPS_FAILURE_DISPOSITION_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate ops-failure-disposition-schema",
        predecessor_sha384_hex:
            crate::services::maintenance::baseline_delta::BASELINE_REVISION_IRG112_SHA384,
        target_sha384_hex:
            crate::services::maintenance::baseline_delta::BASELINE_REVISION_IRG173_SHA384,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_IRG173,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_IRG112,
        additive_sql: ADDITIVE_SCHEMA_SQL,
        precondition: None,
        delta_present: (
            "select exists (
                 select 1 from information_schema.columns
                 where table_schema = 'public'
                   and table_name = 'ops_async_operation'
                   and column_name = 'failure_disposition'
             )",
            "ops_async_operation.failure_disposition is already present with a stale baseline checksum; inspect and update it explicitly",
        ),
        predecessor_error: "baseline checksum is not the exact ops-failure-disposition predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsFailureDispositionSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_ops_failure_disposition_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<OpsFailureDispositionSchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &OPS_FAILURE_DISPOSITION_DELTA, apply).await?;
    Ok(OpsFailureDispositionSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}
