//! Additive live upgrade adding the `query_rerank` AI binding purpose.
//!
//! The database enum and the Rust [`ironrag_contracts::ai::AiBindingPurpose`]
//! must agree, so introducing the purpose adds one enum value to the baseline.
//! A second migration file is forbidden
//! (`docs/ru/CLI.md`), so an existing database reaches it through
//! `ironrag-maintenance migrate query-rerank-binding-purpose --apply`, which
//! shares the transition skeleton in
//! [`crate::services::maintenance::baseline_delta`].

use sqlx::PgPool;

use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_IRG173, REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK,
};
use crate::services::maintenance::baseline_delta::{
    AdditiveBaselineDeltaSpec, apply_additive_baseline_delta,
};

const ADDITIVE_SCHEMA_SQL: &str =
    include_str!("../../../ops/apply-query-rerank-binding-purpose.sql");
/// SHA-384 of the baseline revision that preceded the `query_rerank` purpose.
///
/// That predecessor is the IRG-173 revision — the deployed baseline after the
/// integration-provisioning delta, its FK indexes and the `failure_disposition`
/// column — not the revision immediately after integration-provisioning. It is
/// exposed so the integration test can reproduce that exact predecessor state
/// instead of duplicating the digest.
pub const PREDECESSOR_BASELINE_SHA384: &str =
    crate::services::maintenance::baseline_delta::BASELINE_REVISION_IRG173_SHA384;

/// The immutable query-rerank baseline delta: the IRG-173 revision → the
/// `query_rerank` revision.
pub(crate) const QUERY_RERANK_DELTA: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
    operator_command: "ironrag-maintenance migrate query-rerank-binding-purpose",
    predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
    target_sha384_hex:
        crate::services::maintenance::baseline_delta::BASELINE_REVISION_QUERY_RERANK_SHA384,
    required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK,
    prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_IRG173,
    additive_sql: ADDITIVE_SCHEMA_SQL,
    precondition: None,
    delta_present: (
        "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_binding_purpose'::regtype
               and enumlabel = 'query_rerank'
         )",
        "the query_rerank binding purpose is already present with a stale baseline checksum; inspect and update it explicitly",
    ),
    predecessor_error: "baseline checksum is not the exact query-rerank predecessor; refusing to apply or stamp",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRerankBindingPurposeMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_query_rerank_binding_purpose(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<QueryRerankBindingPurposeMigrationReport> {
    let transition = apply_additive_baseline_delta(postgres, &QUERY_RERANK_DELTA, apply).await?;
    Ok(QueryRerankBindingPurposeMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}

#[cfg(test)]
mod tests {
    use ironrag_contracts::ai::AiBindingPurpose;

    use crate::services::maintenance::baseline_delta::{BASELINE_SQL, enum_labels_in_order};

    fn consumer_purpose_order() -> Vec<String> {
        AiBindingPurpose::ALL.iter().map(|purpose| purpose.as_str().to_string()).collect()
    }

    /// The runtime orders rows by the database enum (`order by
    /// binding_purpose`), so the live upgrade must leave the enum in the same
    /// order a fresh install declares. Both paths are pinned to the consumer's
    /// canonical order [`AiBindingPurpose::ALL`], not to each other, so this
    /// fails if the `BEFORE 'query_answer'` placement is dropped from the ops
    /// DDL and the value is appended instead.
    #[test]
    fn binding_purpose_enum_order_matches_the_consumer_on_both_paths() {
        let baseline_order = enum_labels_in_order(BASELINE_SQL, "ai_binding_purpose");
        assert_eq!(
            baseline_order,
            consumer_purpose_order(),
            "the baseline must declare ai_binding_purpose in the consumer order",
        );

        let mut upgraded_order = baseline_order;
        upgraded_order.retain(|label| label != "query_rerank");
        assert_eq!(
            upgraded_order.len() + 1,
            consumer_purpose_order().len(),
            "the predecessor enum must lack exactly the one new value",
        );

        let ddl = include_str!("../../../ops/apply-query-rerank-binding-purpose.sql");
        let statement = ddl
            .lines()
            .find(|line| line.contains("ALTER TYPE public.ai_binding_purpose ADD VALUE"))
            .expect("the ops DDL must add the ai_binding_purpose value");
        let added = statement
            .split_once("EXISTS '")
            .and_then(|(_, suffix)| suffix.split_once('\''))
            .map(|(label, _)| label)
            .expect("the ops DDL must name the added value");
        let insert_at = if let Some((_, suffix)) = statement.split_once(" BEFORE '") {
            let anchor = suffix
                .split_once('\'')
                .map(|(label, _)| label)
                .expect("the BEFORE clause must name an anchor");
            upgraded_order
                .iter()
                .position(|label| label == anchor)
                .unwrap_or_else(|| panic!("{anchor} must be a predecessor label"))
        } else if let Some((_, suffix)) = statement.split_once(" AFTER '") {
            let anchor = suffix
                .split_once('\'')
                .map(|(label, _)| label)
                .expect("the AFTER clause must name an anchor");
            upgraded_order
                .iter()
                .position(|label| label == anchor)
                .unwrap_or_else(|| panic!("{anchor} must be a predecessor label"))
                + 1
        } else {
            upgraded_order.len()
        };
        upgraded_order.insert(insert_at, added.to_string());

        assert_eq!(
            upgraded_order,
            consumer_purpose_order(),
            "the live upgrade must keep the enum in the consumer order, not append",
        );
    }
}
