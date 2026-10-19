//! Additive live upgrade adding the decision-catalog baseline objects.
//!
//! IRG-182 introduces two enum values: the `graph_entity_type` AI binding
//! purpose and the `typesafe_system_one` provider API style. The database enums
//! and their Rust consumers
//! ([`ironrag_contracts::ai::AiBindingPurpose`] and the provider runtime kinds)
//! must agree, so both values are added to the baseline. A second migration file
//! is forbidden (`docs/ru/CLI.md`), so an existing database reaches
//! them through `ironrag-maintenance migrate decision-catalog-schema --apply`,
//! which shares the transition skeleton in
//! [`crate::services::maintenance::baseline_delta`].

use sqlx::PgPool;

use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG, REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK,
};
use crate::services::maintenance::baseline_delta::{
    AdditiveBaselineDeltaSpec, apply_additive_baseline_delta,
};

const ADDITIVE_SCHEMA_SQL: &str = include_str!("../../../ops/apply-decision-catalog-schema.sql");

/// SHA-384 of the baseline revision that preceded the decision-catalog values.
///
/// That predecessor is the IRG-163 revision — the merged baseline after
/// integration provisioning, the IRG-112 FK indexes, the `failure_disposition`
/// column and the `query_rerank` purpose — not the revision immediately before
/// the `query_rerank` hop. It is exposed so the integration test can reproduce
/// that exact predecessor state instead of duplicating the digest.
pub const PREDECESSOR_BASELINE_SHA384: &str =
    crate::services::maintenance::baseline_delta::BASELINE_REVISION_QUERY_RERANK_SHA384;

/// The immutable decision-catalog baseline delta: the `query_rerank` revision →
/// the decision-catalog shipped baseline.
pub(crate) const DECISION_CATALOG_DELTA: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
    operator_command: "ironrag-maintenance migrate decision-catalog-schema",
    predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
    target_sha384_hex:
        crate::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384,
    required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG,
    prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK,
    additive_sql: ADDITIVE_SCHEMA_SQL,
    precondition: None,
    delta_present: (
        "select exists (
             select 1 from pg_enum
             where (enumtypid = 'public.ai_binding_purpose'::regtype
                    and enumlabel = 'graph_entity_type')
                or (enumtypid = 'public.ai_provider_api_style'::regtype
                    and enumlabel = 'typesafe_system_one')
         )",
        "the decision-catalog enum values are already present with a stale baseline checksum; inspect and update them explicitly",
    ),
    predecessor_error: "baseline checksum is not the exact decision-catalog predecessor; refusing to apply or stamp",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCatalogSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_decision_catalog_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<DecisionCatalogSchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &DECISION_CATALOG_DELTA, apply).await?;
    Ok(DecisionCatalogSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}

#[cfg(test)]
mod tests {
    use ironrag_contracts::ai::AiBindingPurpose;

    use crate::domains::provider_profiles::{
        ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
        OPENAI_COMPATIBLE_RUNTIME_KIND, TYPESAFE_SYSTEM_ONE_RUNTIME_KIND,
    };
    use crate::services::maintenance::baseline_delta::{BASELINE_SQL, enum_labels_in_order};

    /// The `(added, anchor)` pair of one `<type>` statement in the ops DDL: the
    /// value the delta adds and the label it must follow.
    fn added_after(ddl: &str, type_name: &str) -> (String, String) {
        let marker = format!("ALTER TYPE public.{type_name} ADD VALUE IF NOT EXISTS '");
        let statement = ddl
            .lines()
            .find(|line| line.contains(marker.as_str()))
            .unwrap_or_else(|| panic!("the ops DDL must add the {type_name} value"));
        let added = statement
            .split_once("EXISTS '")
            .and_then(|(_, suffix)| suffix.split_once('\''))
            .map(|(label, _)| label.to_string())
            .expect("the ops DDL must name the added value");
        let anchor = statement
            .split_once(" AFTER '")
            .and_then(|(_, suffix)| suffix.split_once('\''))
            .map(|(label, _)| label.to_string())
            .expect("the ops DDL must place the added value AFTER an anchor");
        (added, anchor)
    }

    /// The consumer order of `ai_provider_api_style`: the provider runtime kinds
    /// the profile resolver matches on, declared once in
    /// [`crate::domains::provider_profiles`].
    fn consumer_api_style_order() -> Vec<String> {
        [
            OPENAI_COMPATIBLE_RUNTIME_KIND,
            ANTHROPIC_MESSAGES_RUNTIME_KIND,
            GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
            TYPESAFE_SYSTEM_ONE_RUNTIME_KIND,
        ]
        .iter()
        .map(|kind| (*kind).to_string())
        .collect()
    }

    /// Replays the ops DDL's placement onto the consumer order: removes the one
    /// value the delta adds, then re-inserts it after its anchor. Equality with
    /// the consumer order means the live upgrade leaves the enum exactly as a
    /// fresh install declares, not appended.
    fn order_after_add(ddl: &str, type_name: &str, consumer: &[String]) -> Vec<String> {
        let (added, anchor) = added_after(ddl, type_name);
        let mut upgraded: Vec<String> =
            consumer.iter().filter(|label| **label != added).cloned().collect();
        assert_eq!(
            upgraded.len() + 1,
            consumer.len(),
            "the ops DDL must add exactly one value to {type_name}",
        );
        let at = upgraded
            .iter()
            .position(|label| *label == anchor)
            .unwrap_or_else(|| panic!("{anchor} must be a consumer label of {type_name}"));
        upgraded.insert(at + 1, added);
        upgraded
    }

    /// Both enum orders, on both paths, must equal their consumers. The live
    /// upgrade parses the same ops DDL the operator runs, so a dropped `AFTER`
    /// clause (which would append the value) fails here rather than in
    /// production. `ai_provider_api_style` is pinned to the runtime-kind consts,
    /// not to a copy of the strings.
    #[test]
    fn decision_catalog_enum_orders_match_the_consumers_on_both_paths() {
        let ddl = include_str!("../../../ops/apply-decision-catalog-schema.sql");

        let baseline_binding = enum_labels_in_order(BASELINE_SQL, "ai_binding_purpose");
        let consumer_binding: Vec<String> =
            AiBindingPurpose::ALL.iter().map(|purpose| purpose.as_str().to_string()).collect();
        assert_eq!(
            baseline_binding, consumer_binding,
            "the baseline must declare ai_binding_purpose in the consumer order",
        );
        assert_eq!(
            order_after_add(ddl, "ai_binding_purpose", &consumer_binding),
            consumer_binding,
            "the live upgrade must insert graph_entity_type after extract_graph",
        );

        let baseline_style = enum_labels_in_order(BASELINE_SQL, "ai_provider_api_style");
        let consumer_style = consumer_api_style_order();
        assert_eq!(
            baseline_style, consumer_style,
            "the baseline must declare ai_provider_api_style in the runtime-kind order",
        );
        assert_eq!(
            order_after_add(ddl, "ai_provider_api_style", &consumer_style),
            consumer_style,
            "the live upgrade must insert typesafe_system_one after gemini_generate_content",
        );
        assert_eq!(
            added_after(ddl, "ai_provider_api_style"),
            (
                TYPESAFE_SYSTEM_ONE_RUNTIME_KIND.to_string(),
                GEMINI_GENERATE_CONTENT_RUNTIME_KIND.to_string()
            ),
            "typesafe_system_one must follow gemini_generate_content",
        );
    }
}
