use super::answer_construction::{
    broad_procedure_preflight_requires_synthesis, build_setup_configuration_anchor_answer_override,
    preflight_requires_synthesis,
};
use super::*;
use crate::domains::query_ir::{
    ConversationRefKind, DocumentHint, EntityMention, EntityRole, QueryLanguage, QueryTargetKind,
    UnresolvedRef,
};
use crate::services::query::execution::consolidation::query_has_multi_document_setup_anchors;

#[test]
fn broad_multi_variant_procedure_requires_synthesis() {
    let mut query_ir = query_ir(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        &["configuration_file", "config_key", "procedure"],
    );
    query_ir
        .target_entities
        .push(EntityMention { label: "Subject".to_string(), role: EntityRole::Subject });
    let chunks = vec![
        chunk(
            "Subject Alpha setup",
            "Install artifact-alpha in /opt/subject/alpha/alpha.ini and set primaryKey=true.",
        ),
        chunk(
            "Subject Beta setup",
            "Install artifact-beta in /opt/subject/beta/beta.conf and set secondaryKey=true.",
        ),
    ];

    assert!(broad_procedure_preflight_requires_synthesis(
        "how to configure Subject?",
        &query_ir,
        &chunks,
    ));
}

#[test]
fn focused_procedure_keeps_deterministic_preflight_candidate() {
    let mut query_ir = query_ir(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        &["configuration_file", "config_key", "procedure"],
    );
    query_ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
    let chunks = vec![chunk(
        "Subject Alpha setup",
        "Install artifact-alpha in /opt/subject/alpha/alpha.ini and set primaryKey=true.",
    )];

    assert!(!broad_procedure_preflight_requires_synthesis(
        "how to configure Subject Alpha?",
        &query_ir,
        &chunks,
    ));
}

#[test]
fn setup_configuration_anchor_override_skips_single_value_query() {
    let query_ir = query_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument, &["port"]);
    let chunk = chunk(
        "Subject Alpha setup",
        "Component configuration\napply artifact-alpha\n\
         Settings are defined in /opt/subject/alpha/alpha.ini.\n\
         port = 443",
    );

    assert!(
        build_setup_configuration_anchor_answer_override(
            "which port does Subject Alpha use?",
            &query_ir,
            &[chunk],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_override_skips_retrieve_value_multi_document_query() {
    let query_ir = query_ir(QueryAct::RetrieveValue, QueryScope::MultiDocument, &["parameter"]);
    let chunks = vec![
        chunk(
            "Subject Alpha setup",
            "Component configuration\napply artifact-alpha\n\
             Settings are defined in /opt/subject/alpha/alpha.ini.\n\
             primaryKey = \"\"",
        ),
        chunk(
            "Subject Beta setup",
            "Component configuration\napply artifact-beta\n\
             Settings are defined in /opt/subject/beta/beta.conf.\n\
             secondaryKey = \"\"",
        ),
    ];

    assert!(
        build_setup_configuration_anchor_answer_override(
            "which parameter configures Subject?",
            &query_ir,
            &chunks,
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_override_yields_multi_document_setup_to_synthesis() {
    let mut query_ir = query_ir(QueryAct::ConfigureHow, QueryScope::MultiDocument, &["package"]);
    query_ir
        .target_entities
        .push(EntityMention { label: "Subject".to_string(), role: EntityRole::Subject });
    let chunks = vec![
        chunk(
            "Subject Alpha setup",
            "Component configuration\napply artifact-alpha\n\
             Settings are defined in /opt/subject/alpha/alpha.ini.\n\
             primaryKey = \"\"",
        ),
        chunk(
            "Subject Beta setup",
            "Component configuration\napply artifact-beta\n\
             Settings are defined in /opt/subject/beta/beta.conf.\n\
             secondaryKey = \"\"",
        ),
    ];

    assert!(
        build_setup_configuration_anchor_answer_override(
            "how to configure Subject?",
            &query_ir,
            &chunks,
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_override_yields_soft_multi_variant_setup_to_synthesis() {
    let mut query_ir = query_ir(QueryAct::ConfigureHow, QueryScope::MultiDocument, &["parameter"]);
    query_ir
        .target_entities
        .push(EntityMention { label: "Subject".to_string(), role: EntityRole::Subject });
    let chunks = vec![
        chunk(
            "Subject Alpha setup",
            "Settings are defined in /opt/subject/alpha/alpha.ini.\n\
             [AlphaSubject]\n\
             primaryKey = \"\"",
        ),
        chunk(
            "Subject Beta setup",
            "Settings are defined in /opt/subject/beta/beta.conf.\n\
             [BetaSubject]\n\
             secondaryKey = \"\"",
        ),
    ];

    assert!(!query_has_multi_document_setup_anchors(&query_ir, &chunks));
    assert!(
        build_setup_configuration_anchor_answer_override(
            "how to configure Subject?",
            &query_ir,
            &chunks,
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_override_skips_soft_multi_variant_document_focus() {
    let mut query_ir = query_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument, &["parameter"]);
    query_ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
    let chunks = vec![
        chunk(
            "Subject Alpha setup",
            "Settings are defined in /opt/subject/alpha/alpha.ini.\n\
             [AlphaSubject]\n\
             primaryKey = \"\"",
        ),
        chunk(
            "Subject Beta setup",
            "Settings are defined in /opt/subject/beta/beta.conf.\n\
             [BetaSubject]\n\
             secondaryKey = \"\"",
        ),
    ];

    assert!(
        build_setup_configuration_anchor_answer_override(
            "how to configure Subject Alpha?",
            &query_ir,
            &chunks,
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_override_allows_single_variant_with_parameter_details() {
    let mut query_ir = query_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument, &["parameter"]);
    query_ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut anchor = chunk(
        "Subject Alpha setup",
        "Settings are defined in /opt/subject/alpha/alpha.ini.\n\
         [AlphaSubject]",
    );
    anchor.document_id = document_id;
    anchor.revision_id = revision_id;
    let mut parameter_row = chunk(
        "Subject Alpha setup",
        "Sheet: Component settings | Row 12 | Name: primaryKey | Type: string | \
         Description: Primary identifier | Notes: Required",
    );
    parameter_row.document_id = document_id;
    parameter_row.revision_id = revision_id;

    let answer = build_setup_configuration_anchor_answer_override(
        "how to configure Subject Alpha?",
        &query_ir,
        &[anchor, parameter_row],
    )
    .expect("single-variant parameter table override");

    assert!(answer.contains("**Parameter details:**"));
    assert!(answer.contains("primaryKey"));
    assert!(answer.contains("Primary identifier"));
}

#[test]
fn setup_configuration_anchor_override_skips_describe_parameter_inventory() {
    let mut query_ir = query_ir(QueryAct::Describe, QueryScope::MultiDocument, &["parameter"]);
    query_ir
        .target_entities
        .push(EntityMention { label: "S1".to_string(), role: EntityRole::Subject });
    let chunks = vec![
        chunk(
            "S1 Alpha reference",
            "Settings are defined in /x/s1-alpha.conf.\n\
             [Alpha]\n\
             firstValue = \"\"",
        ),
        chunk(
            "S1 Beta reference",
            "Sheet: Parameters | Row 4 | Name: secondValue | Type: string | \
             Description: Secondary value | Notes: Optional",
        ),
    ];

    assert!(
        build_setup_configuration_anchor_answer_override("describe S1 values", &query_ir, &chunks)
            .is_none()
    );
}

#[test]
fn evidence_preflight_does_not_skip_deterministic_candidates_only_for_follow_up_state() {
    let mut query_ir = query_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument, &["procedure"]);
    query_ir.conversation_refs.push(UnresolvedRef {
        surface: "previous turn".to_string(),
        kind: ConversationRefKind::Deictic,
    });

    assert!(query_ir.is_follow_up());
    assert!(!preflight_requires_synthesis(
        "how to update Alpha Service?",
        &query_ir,
        &QueryIntentProfile::default()
    ));
}

fn query_ir(act: QueryAct, scope: QueryScope, target_types: &[&str]) -> QueryIR {
    QueryIR {
        act,
        scope,
        language: QueryLanguage::Auto,
        target_types: target_types
            .iter()
            .map(|value| {
                QueryTargetKind::from_wire(value).expect("test target kind must use the wire enum")
            })
            .collect(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    }
}

fn chunk(label: &str, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: Some("paragraph".to_string()),
        document_label: label.to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(3.0),
        source_text: text.to_string(),
    }
}
