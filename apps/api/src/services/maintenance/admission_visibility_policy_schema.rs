//! Additive live upgrade adding the typed REQUIRED-admission visibility policy.
//!
//! IRG-92 makes the REQUIRED replacement suppression a durable, typed fact: the
//! `content_mutation.visibility_policy` column (`public.admission_visibility_policy`)
//! defaults to the single canonical legacy value `'readable'` and is written
//! `'suppressed_until_ready'` only by an explicit REQUIRED admission. A second
//! migration file is forbidden (`docs/ru/CLI.md`), so an existing
//! database reaches the object through
//! `ironrag-maintenance migrate admission-visibility-policy-schema --apply`,
//! which shares the transition skeleton in
//! [`crate::services::maintenance::baseline_delta`].

use sqlx::PgPool;

use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY,
    REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG,
};
use crate::services::maintenance::baseline_delta::{
    AdditiveBaselineDeltaSpec, apply_additive_baseline_delta,
};

const ADDITIVE_SCHEMA_SQL: &str =
    include_str!("../../../ops/apply-required-admission-visibility-policy-schema.sql");

/// SHA-384 of the baseline revision that preceded the visibility-policy column:
/// the merged decision-catalog baseline. Exposed so the integration test can
/// reproduce that exact predecessor state instead of duplicating the digest.
pub const PREDECESSOR_BASELINE_SHA384: &str =
    crate::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384;

/// The immutable visibility-policy baseline delta: the decision-catalog revision
/// → the IRG-92 shipped baseline.
pub(crate) const ADMISSION_VISIBILITY_POLICY_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate admission-visibility-policy-schema",
        predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
        target_sha384_hex:
            crate::services::maintenance::baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG,
        additive_sql: ADDITIVE_SCHEMA_SQL,
        precondition: None,
        delta_present: (
            "select exists (
                 select 1 from information_schema.columns
                 where table_schema = 'public'
                   and table_name = 'content_mutation'
                   and column_name = 'visibility_policy'
             )",
            "the content_mutation.visibility_policy column is already present with a stale baseline checksum; inspect and update it explicitly",
        ),
        predecessor_error: "baseline checksum is not the exact admission-visibility-policy predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionVisibilityPolicySchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_admission_visibility_policy_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<AdmissionVisibilityPolicySchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &ADMISSION_VISIBILITY_POLICY_DELTA, apply).await?;
    Ok(AdmissionVisibilityPolicySchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}
