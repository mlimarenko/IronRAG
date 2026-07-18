use anyhow::anyhow;
use serde_json::json;

use crate::domains::query_ir::{
    EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind,
};

use super::retrieval_plan::{
    LaneCriticality, LaneResolution, LaneSpec, LatestSelectionKind, RetrievalLane, RetrievalPlan,
    RetrievalPlanningContext, RetrievalPlanningError, explicit_content_anchor_requested,
    resolve_lane_result, resolve_text_search_config, reuse_or_execute_primary,
};

fn query_ir(act: QueryAct, target_types: &[&str], with_entity: bool) -> QueryIR {
    QueryIR {
        act,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: target_types
            .iter()
            .map(|value| {
                QueryTargetKind::from_wire(value).expect("test target kind must use the wire enum")
            })
            .collect(),
        target_entities: if with_entity {
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }]
        } else {
            Vec::new()
        },
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn plan(
    query_ir: Option<&QueryIR>,
    latest_selection: LatestSelectionKind,
    versioned_update_intent: bool,
    setup_intent: bool,
) -> RetrievalPlan {
    RetrievalPlan::compile(RetrievalPlanningContext {
        query_ir,
        has_target_documents: false,
        has_focus_queries: query_ir.is_some_and(|query_ir| !query_ir.target_entities.is_empty()),
        has_content_anchor: explicit_content_anchor_requested(query_ir, false),
        has_document_evidence_anchor: false,
        latest_selection,
        versioned_update_intent,
        setup_intent,
    })
}

fn lanes(plan: &RetrievalPlan) -> Vec<RetrievalLane> {
    plan.lanes().iter().map(|spec| spec.lane).collect()
}

#[test]
fn generic_query_skips_all_specialized_companion_lanes() {
    let query_ir = query_ir(QueryAct::RetrieveValue, &["metric"], false);
    assert!(lanes(&plan(Some(&query_ir), LatestSelectionKind::None, false, false)).is_empty());
}

#[test]
fn unresolved_entity_does_not_enable_corpus_wide_content_anchor_lane() {
    let mut query_ir = query_ir(QueryAct::ConfigureHow, &["procedure"], true);

    assert!(!explicit_content_anchor_requested(Some(&query_ir), false));
    assert!(explicit_content_anchor_requested(Some(&query_ir), true));

    query_ir.literal_constraints.push(crate::domains::query_ir::LiteralSpan {
        text: "literal-value".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Other,
    });
    assert!(explicit_content_anchor_requested(Some(&query_ir), false));
}

#[test]
fn procedure_plan_does_not_launch_unresolved_entity_content_anchor() {
    let query_ir = query_ir(QueryAct::ConfigureHow, &["procedure", "version"], true);
    assert_eq!(
        lanes(&plan(Some(&query_ir), LatestSelectionKind::None, true, false)),
        vec![
            RetrievalLane::QueryIrFocus,
            RetrievalLane::VersionedUpdateProcedure,
            RetrievalLane::VersionedUpdateSourceLocal,
            RetrievalLane::ArtifactSiblingSource,
        ]
    );
}

#[test]
fn setup_plan_launches_setup_lanes_without_entity_bio() {
    let query_ir = query_ir(QueryAct::ConfigureHow, &["procedure", "configuration_file"], true);
    let lanes = lanes(&plan(Some(&query_ir), LatestSelectionKind::None, false, true));
    assert!(lanes.contains(&RetrievalLane::SetupFocus));
    assert!(lanes.contains(&RetrievalLane::SetupVariant));
    assert!(!lanes.contains(&RetrievalLane::EntityBio));
    assert!(!lanes.contains(&RetrievalLane::VersionedUpdateProcedure));
}

#[test]
fn latest_plan_requires_explicit_typed_selection() {
    let query_ir = query_ir(QueryAct::Enumerate, &["version"], true);
    let explicit = lanes(&plan(Some(&query_ir), LatestSelectionKind::Explicit, false, false));
    let absent = lanes(&plan(Some(&query_ir), LatestSelectionKind::None, false, false));
    assert!(explicit.contains(&RetrievalLane::LatestVersion));
    assert!(explicit.contains(&RetrievalLane::LatestVersionSemantic));
    assert!(!absent.contains(&RetrievalLane::LatestVersion));
    assert!(!absent.contains(&RetrievalLane::LatestVersionSemantic));
}

#[test]
fn entity_bio_is_limited_to_describe_entity_intent() {
    let describe = query_ir(QueryAct::Describe, &["person"], true);
    let enumerate = query_ir(QueryAct::Enumerate, &["person"], true);
    assert!(
        lanes(&plan(Some(&describe), LatestSelectionKind::None, false, false))
            .contains(&RetrievalLane::EntityBio)
    );
    assert!(
        !lanes(&plan(Some(&enumerate), LatestSelectionKind::None, false, false))
            .contains(&RetrievalLane::EntityBio)
    );
}

#[test]
fn required_lane_failure_aborts_even_with_primary_evidence() {
    let spec =
        LaneSpec { lane: RetrievalLane::DocumentIdentity, criticality: LaneCriticality::Required };
    let error = resolve_lane_result::<()>(spec, Err(anyhow!("fault")), true)
        .expect_err("required failure must abort");
    assert!(error.to_string().contains("required retrieval lane document_identity failed"));
}

#[test]
fn optional_lane_failure_degrades_only_with_primary_evidence() {
    let spec =
        LaneSpec { lane: RetrievalLane::ContentAnchor, criticality: LaneCriticality::Optional };
    assert!(matches!(
        resolve_lane_result::<()>(spec, Err(anyhow!("fault")), true)
            .expect("primary evidence permits degradation"),
        LaneResolution::Degraded(_)
    ));
    let error = resolve_lane_result::<()>(spec, Err(anyhow!("fault")), false)
        .expect_err("optional failure without primary must abort");
    assert!(
        error.to_string().contains("optional-without-primary retrieval lane content_anchor failed")
    );
}

#[test]
fn absent_retrieval_config_defaults_but_invalid_stored_config_is_typed_error() {
    assert_eq!(resolve_text_search_config(None).expect("absent config"), "simple");
    assert_eq!(
        resolve_text_search_config(Some(json!({ "lexical": { "textSearchConfig": 3 } }))),
        Err(RetrievalPlanningError::InvalidStoredConfig(
            "invalid retrieval config: invalid type: integer `3`, expected a string".to_string()
        ))
    );
}

#[tokio::test]
async fn focus_broaden_reuses_primary_search_without_a_second_call() {
    let calls = std::cell::Cell::new(0usize);
    let (initial, reused) = reuse_or_execute_primary::<_, anyhow::Error, _, _>(None, || async {
        calls.set(calls.get() + 1);
        Ok(vec!["primary-evidence"])
    })
    .await
    .expect("initial primary execution");
    assert!(!reused);

    let (broadened, reused) = reuse_or_execute_primary(Some(initial), || async {
        calls.set(calls.get() + 1);
        Ok::<Vec<&str>, anyhow::Error>(Vec::new())
    })
    .await
    .expect("broaden replan");
    assert!(reused);
    assert_eq!(broadened, vec!["primary-evidence"]);
    assert_eq!(calls.get(), 1, "focus broaden must not rerun primary retrieval");
}
