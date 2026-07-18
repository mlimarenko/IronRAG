use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use uuid::Uuid;

use super::{
    document_ids_with_focus_value_contained_in_hint,
    explicit_document_reference_literal_is_present, explicit_document_reference_literals,
    explicit_target_document_ids_from_values, normalize_document_target_text,
    query_ir_allows_document_focus_scope, resolve_scoped_target_document_ids,
};
use crate::domains::query_ir::{
    ConversationRefKind, DocumentHint, EntityMention, EntityRole, LiteralKind, LiteralSpan,
    QueryAct, QueryIR, QueryLanguage, QueryScope, UnresolvedRef,
};
use crate::services::query::effective_query::{
    EFFECTIVE_QUERY_QUESTION_PREFIX, EFFECTIVE_QUERY_SCOPE_PREFIX,
};

fn effective_query_text(scope: &str, question: &str) -> String {
    format!("{EFFECTIVE_QUERY_SCOPE_PREFIX} {scope}\n{EFFECTIVE_QUERY_QUESTION_PREFIX} {question}")
}

fn scoped_query_ir(
    scope: QueryScope,
    document_focus: Option<&str>,
    target_entities: &[&str],
) -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: target_entities
            .iter()
            .map(|value| EntityMention { label: (*value).to_string(), role: EntityRole::Subject })
            .collect(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: document_focus.map(|hint| DocumentHint { hint: hint.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn scoped_document_index<'a>(
    entries: impl IntoIterator<Item = (Uuid, &'a str, Option<&'a str>, &'a str)>,
) -> HashMap<Uuid, crate::infra::knowledge_rows::KnowledgeDocumentRow> {
    let mut index = HashMap::new();
    let library_id = Uuid::now_v7();
    let workspace_id = Uuid::now_v7();
    for (document_id, file_name, title, external_key) in entries {
        index.insert(
            document_id,
            crate::infra::knowledge_rows::KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: external_key.to_string(),
                title: title.map(std::string::ToString::to_string),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(Uuid::now_v7()),
                readable_revision_id: None,
                latest_revision_no: Some(1),
                parent_document_id: None,
                document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                deleted_at: None,
                file_name: Some(file_name.to_string()),
            },
        );
    }
    index
}

#[test]
fn resolve_scoped_target_document_ids_prefers_explicit_reference() {
    let scoped_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (scoped_document_id, "graphql-api.pdf", Some("GraphQL API"), "graphql-api.pdf"),
        (other_document_id, "rest-api.pdf", Some("REST API"), "rest-api.pdf"),
    ]);

    let ir = scoped_query_ir(QueryScope::SingleDocument, Some("REST API"), &["rest"]);
    let target_ids = resolve_scoped_target_document_ids(
        "Read graphql-api.pdf for the auth setup section",
        Some(&ir),
        &index,
    );

    assert_eq!(target_ids, BTreeSet::from([scoped_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_keeps_explicit_reference_for_follow_up() {
    let scoped_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (scoped_document_id, "alpha-guide.pdf", Some("Alpha Guide"), "alpha-guide.pdf"),
        (other_document_id, "beta-guide.pdf", Some("Beta Guide"), "beta-guide.pdf"),
    ]);

    let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("Beta Guide"), &["beta"]);
    ir.act = QueryAct::FollowUp;
    ir.conversation_refs.push(UnresolvedRef {
        surface: "that document".to_string(),
        kind: ConversationRefKind::Deictic,
    });
    let target_ids = resolve_scoped_target_document_ids(
        "Use alpha-guide.pdf for the setup details",
        Some(&ir),
        &index,
    );

    assert_eq!(target_ids, BTreeSet::from([scoped_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_does_not_hard_scope_follow_up_focus() {
    let alpha_document_id = Uuid::now_v7();
    let beta_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (alpha_document_id, "alpha-guide.md", Some("alpha service handbook"), "alpha-guide.md"),
        (beta_document_id, "beta-guide.md", Some("beta service handbook"), "beta-guide.md"),
    ]);

    let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("alpha service"), &["alpha"]);
    ir.conversation_refs.push(UnresolvedRef {
        surface: "that one".to_string(),
        kind: ConversationRefKind::Deictic,
    });
    let target_ids = resolve_scoped_target_document_ids("What about that one?", Some(&ir), &index);

    assert!(target_ids.is_empty());
    assert!(!query_ir_allows_document_focus_scope(&ir));
}

#[test]
fn resolve_scoped_target_document_ids_does_not_treat_follow_up_subject_as_document_literal() {
    let alpha_document_id = Uuid::now_v7();
    let beta_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (alpha_document_id, "alpha-guide.md", Some("Alpha Provider Guide"), "alpha-guide.md"),
        (beta_document_id, "beta-guide.md", Some("Beta Provider Guide"), "beta-guide.md"),
    ]);

    let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("Beta Provider"), &["Beta"]);
    ir.conversation_refs
        .push(UnresolvedRef { surface: "Beta".to_string(), kind: ConversationRefKind::Elliptic });
    let target_ids = resolve_scoped_target_document_ids("Beta", Some(&ir), &index);

    assert!(target_ids.is_empty());
}

#[test]
fn resolve_scoped_target_document_ids_selects_single_match_from_query_ir_scope() {
    let alpha_document_id = Uuid::now_v7();
    let beta_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (alpha_document_id, "alpha-guide.md", Some("alpha service handbook"), "alpha-guide.md"),
        (beta_document_id, "beta-guide.md", Some("beta service handbook"), "beta-guide.md"),
    ]);
    let ir = scoped_query_ir(QueryScope::SingleDocument, Some("alpha service"), &["alpha"]);

    let target_ids =
        resolve_scoped_target_document_ids("Where are the auth requirements?", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([alpha_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_uses_unambiguous_title_stem_for_typed_query() {
    let focused_document_id = Uuid::now_v7();
    let distractor_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            focused_document_id,
            "alpha_matrix_records.yaml",
            Some("Alpha Matrix Records"),
            "alpha_matrix_records.yaml",
        ),
        (
            distractor_document_id,
            "alpha_matrix_summary.yaml",
            Some("Alpha Matrix Summary"),
            "alpha_matrix_summary.yaml",
        ),
    ]);
    let mut ir = scoped_query_ir(QueryScope::SingleDocument, None, &["Alpha Matrix"]);
    ir.act = QueryAct::Describe;
    ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
        crate::domains::query_ir::QueryTargetKind::Parameter,
        crate::domains::query_ir::QueryTargetKind::Procedure,
    ];

    let target_ids = resolve_scoped_target_document_ids(
        "Which values does Alpha Matrix Records expose?",
        Some(&ir),
        &index,
    );

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_prefers_exact_focus_title_over_prefixed_variants() {
    let focused_document_id = Uuid::now_v7();
    let image_document_id = Uuid::now_v7();
    let appendix_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (focused_document_id, "provider-alpha.md", Some("Provider Alpha"), "provider-alpha.md"),
        (
            image_document_id,
            "provider-alpha-screen.png",
            Some("Provider Alpha: payment screen.png"),
            "provider-alpha-screen.png",
        ),
        (
            appendix_document_id,
            "provider-alpha-appendix.md",
            Some("Provider Alpha appendix"),
            "provider-alpha-appendix.md",
        ),
    ]);
    let ir = scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha"), &["Alpha"]);

    let target_ids =
        resolve_scoped_target_document_ids("How do I configure Provider Alpha?", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_prefers_focus_title_inside_longer_hint() {
    let focused_document_id = Uuid::now_v7();
    let image_document_id = Uuid::now_v7();
    let appendix_document_id = Uuid::now_v7();
    let tangential_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (focused_document_id, "provider-alpha.md", Some("Provider Alpha"), "provider-alpha.md"),
        (
            image_document_id,
            "provider-alpha-screen.png",
            Some("Provider Alpha: payment screen.png"),
            "provider-alpha-screen.png",
        ),
        (
            appendix_document_id,
            "provider-alpha-appendix.md",
            Some("Provider Alpha appendix"),
            "provider-alpha-appendix.md",
        ),
        (tangential_document_id, "alpha-suite.md", Some("Alpha in Retail Suite"), "alpha-suite.md"),
    ]);
    let ir =
        scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha in Retail Suite"), &[]);
    let mut ir = ir;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Document];

    let target_ids =
        resolve_scoped_target_document_ids("How do I configure Provider Alpha?", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_prefers_current_question_over_scope_history() {
    let focused_document_id = Uuid::now_v7();
    let stale_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (focused_document_id, "provider-alpha.md", Some("Provider Alpha"), "provider-alpha.md"),
        (
            stale_document_id,
            "provider-beta-deprecated.md",
            Some("Provider Beta Deprecated"),
            "provider-beta-deprecated.md",
        ),
    ]);
    let ir = scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha"), &["Alpha"]);

    let target_ids = resolve_scoped_target_document_ids(
        &effective_query_text(
            "How do I configure payments?\nOptions: Provider Alpha; Provider Beta Deprecated.",
            "Alpha Provider setup",
        ),
        Some(&ir),
        &index,
    );

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_keeps_follow_up_subject_out_of_fast_path() {
    let focused_document_id = Uuid::now_v7();
    let index = scoped_document_index([(
        focused_document_id,
        "provider-alpha.md",
        Some("Provider Alpha"),
        "provider-alpha.md",
    )]);
    let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha"), &["Alpha"]);
    ir.act = QueryAct::FollowUp;

    let target_ids = resolve_scoped_target_document_ids(
        &effective_query_text("Prior answer discussed Provider Alpha.", "Provider Alpha"),
        Some(&ir),
        &index,
    );

    assert!(target_ids.is_empty());
}

#[test]
fn resolve_scoped_target_document_ids_uses_related_focus_prefix() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            focused_document_id,
            "acmealpha-guide.md",
            Some("Acmealpha payment setup guide"),
            "acmealpha-guide.md",
        ),
        (other_document_id, "beta-guide.md", Some("Beta payment setup guide"), "beta-guide.md"),
    ]);
    let ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Acmew"),
        &["installation", "configuration file", "parameters"],
    );

    let target_ids =
        resolve_scoped_target_document_ids("Show the setup details.", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_keeps_document_focus_when_entities_are_values() {
    let alpha_document_id = Uuid::now_v7();
    let beta_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (alpha_document_id, "alpha-guide.md", Some("alpha service handbook"), "alpha-guide.md"),
        (beta_document_id, "beta-guide.md", Some("beta service handbook"), "beta-guide.md"),
    ]);
    let ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("alpha service"),
        &["renewal policy", "escalation target"],
    );

    let target_ids =
        resolve_scoped_target_document_ids("What is the renewal policy?", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([alpha_document_id]));
}

#[test]
fn compare_concept_query_ir_does_not_enable_document_focus_scope() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite"),
        &["connector options", "fallback behavior", "regional limits"],
    );
    ir.act = QueryAct::Compare;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];

    assert!(
        !query_ir_allows_document_focus_scope(&ir),
        "broad compare over concepts must preserve cross-document recall"
    );
}

#[test]
fn describe_concept_query_ir_does_not_enable_document_focus_scope() {
    let mut ir =
        scoped_query_ir(QueryScope::SingleDocument, Some("Alpha Suite"), &["connector options"]);
    ir.act = QueryAct::Describe;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];

    assert!(
        !query_ir_allows_document_focus_scope(&ir),
        "open-content descriptions must preserve source coverage unless the IR explicitly targets a document"
    );
}

#[test]
fn configure_multi_target_query_ir_does_not_enable_document_focus_scope() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite"),
        &["connector options", "fallback behavior", "regional limits"],
    );
    ir.act = QueryAct::ConfigureHow;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Procedure];

    assert!(
        !query_ir_allows_document_focus_scope(&ir),
        "multi-topic procedural questions must not collapse to one hinted document"
    );
}

#[test]
fn broad_content_literal_other_does_not_enable_document_focus_scope() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite connector options"),
        &["connector options", "fallback behavior", "regional limits"],
    );
    ir.act = QueryAct::Enumerate;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];
    ir.literal_constraints =
        vec![LiteralSpan { text: "Alpha Suite".to_string(), kind: LiteralKind::Other }];

    assert!(
        !query_ir_allows_document_focus_scope(&ir),
        "broad open-content literals must not force single-document packing"
    );
}

#[test]
fn plain_alphabetic_identifier_does_not_enable_document_focus_scope() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite fallback behavior"),
        &["path", "condition"],
    );
    ir.act = QueryAct::RetrieveValue;
    ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Path,
        crate::domains::query_ir::QueryTargetKind::Concept,
    ];
    ir.literal_constraints =
        vec![LiteralSpan { text: "alpha".to_string(), kind: LiteralKind::Identifier }];

    assert!(
        !query_ir_allows_document_focus_scope(&ir),
        "plain alphabetic literals are weak topic echoes and must not force single-document packing"
    );
}

#[test]
fn plain_alphabetic_identifier_does_not_block_enumerate_broad_recall() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite connector options"),
        &["connector options", "fallback behavior"],
    );
    ir.act = QueryAct::Enumerate;
    ir.literal_constraints =
        vec![LiteralSpan { text: "alpha".to_string(), kind: LiteralKind::Identifier }];

    assert!(
        !query_ir_allows_document_focus_scope(&ir),
        "plain alphabetic identifier literals must not cancel broad enumerate recall"
    );
}

#[test]
fn exact_lookup_query_ir_keeps_document_focus_scope() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite Admin Guide"),
        &["callback URL"],
    );
    ir.act = QueryAct::RetrieveValue;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Url];
    ir.literal_constraints =
        vec![LiteralSpan { text: "callbackUrl".to_string(), kind: LiteralKind::Identifier }];

    assert!(
        query_ir_allows_document_focus_scope(&ir),
        "exact lookup intents may use the single-document focus for precision and speed"
    );
}

#[test]
fn compare_document_query_ir_keeps_explicit_document_focus_scope() {
    let mut ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite Admin Guide"),
        &["current section", "previous section"],
    );
    ir.act = QueryAct::Compare;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Document];

    assert!(
        query_ir_allows_document_focus_scope(&ir),
        "compare may pack one document only when the typed IR explicitly targets a document"
    );
}

#[test]
fn resolve_scoped_target_document_ids_refines_focus_with_entity_prefix_overlap() {
    let catalog_document_id = Uuid::now_v7();
    let generic_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            catalog_document_id,
            "catalog-options.md",
            Some("Alpha Suite integrated connector catalog"),
            "catalog-options.md",
        ),
        (
            generic_document_id,
            "alpha-overview.md",
            Some("Alpha Suite overview"),
            "alpha-overview.md",
        ),
    ]);
    let ir = scoped_query_ir(
        QueryScope::SingleDocument,
        Some("Alpha Suite"),
        &["integration variants", "connected catalog"],
    );

    let target_ids =
        resolve_scoped_target_document_ids("Enumerate the variants.", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([catalog_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_does_not_hard_scope_enumerate_focus() {
    let focused_document_id = Uuid::now_v7();
    let companion_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            focused_document_id,
            "alpha-overview.md",
            Some("Alpha Suite overview"),
            "alpha-overview.md",
        ),
        (
            companion_document_id,
            "alpha-connectors.md",
            Some("Alpha Suite connector catalog"),
            "alpha-connectors.md",
        ),
    ]);
    let mut ir =
        scoped_query_ir(QueryScope::SingleDocument, Some("Alpha Suite"), &["connector catalog"]);
    ir.act = QueryAct::Enumerate;

    let target_ids =
        resolve_scoped_target_document_ids("Enumerate the connector options.", Some(&ir), &index);

    assert!(
        target_ids.is_empty(),
        "enumeration questions must keep library-wide recall unless the user names a concrete document"
    );
}

#[test]
fn resolve_scoped_target_document_ids_keeps_enumerate_document_target() {
    let focused_document_id = Uuid::now_v7();
    let companion_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            focused_document_id,
            "alpha-overview.md",
            Some("Alpha Suite overview"),
            "alpha-overview.md",
        ),
        (
            companion_document_id,
            "alpha-connectors.md",
            Some("Alpha Suite connector catalog"),
            "alpha-connectors.md",
        ),
    ]);
    let mut ir =
        scoped_query_ir(QueryScope::SingleDocument, Some("Alpha Suite"), &["connector catalog"]);
    ir.act = QueryAct::Enumerate;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Document];

    let target_ids = resolve_scoped_target_document_ids(
        "Enumerate the sections in Alpha Suite overview.",
        Some(&ir),
        &index,
    );

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_keeps_focus_anchor_before_entity_refine() {
    let focused_document_id = Uuid::now_v7();
    let entity_collision_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            focused_document_id,
            "alpha-overview.md",
            Some("Alpha Suite overview"),
            "alpha-overview.md",
        ),
        (
            entity_collision_document_id,
            "beta-connectors.md",
            Some("Beta Suite integrated connector catalog"),
            "beta-connectors.md",
        ),
    ]);
    let ir =
        scoped_query_ir(QueryScope::SingleDocument, Some("Alpha Suite"), &["connector catalog"]);

    let target_ids =
        resolve_scoped_target_document_ids("Enumerate the connector catalog.", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_does_not_prefix_loosen_primary_focus() {
    let exact_document_id = Uuid::now_v7();
    let prefix_collision_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            exact_document_id,
            "alpha-integrated.md",
            Some("Alpha integrated connector guide"),
            "alpha-integrated.md",
        ),
        (
            prefix_collision_document_id,
            "alpha-integration.md",
            Some("Alpha integration connector guide"),
            "alpha-integration.md",
        ),
    ]);
    let ir = scoped_query_ir(QueryScope::SingleDocument, Some("Alpha integrated"), &[]);

    let target_ids =
        resolve_scoped_target_document_ids("Open Alpha integrated.", Some(&ir), &index);

    assert_eq!(target_ids, BTreeSet::from([exact_document_id]));
}

#[test]
fn resolve_scoped_target_document_ids_rejects_ambiguous_focus_refine() {
    let first_document_id = Uuid::now_v7();
    let second_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (
            first_document_id,
            "alpha-connectors-a.md",
            Some("Alpha Suite integrated connector catalog"),
            "alpha-connectors-a.md",
        ),
        (
            second_document_id,
            "alpha-connectors-b.md",
            Some("Alpha Suite connector catalog matrix"),
            "alpha-connectors-b.md",
        ),
    ]);
    let ir =
        scoped_query_ir(QueryScope::SingleDocument, Some("Alpha Suite"), &["connector catalog"]);

    let target_ids =
        resolve_scoped_target_document_ids("Enumerate the connector catalog.", Some(&ir), &index);

    assert!(target_ids.is_empty());
}

#[test]
fn resolve_scoped_target_document_ids_returns_empty_for_ambiguous_query_ir_focus() {
    let alpha_document_id = Uuid::now_v7();
    let beta_document_id = Uuid::now_v7();
    let index = scoped_document_index([
        (alpha_document_id, "service-overview.md", Some("service overview"), "service-overview.md"),
        (beta_document_id, "service-notes.md", Some("service notes"), "service-notes.md"),
    ]);
    let ir = scoped_query_ir(QueryScope::SingleDocument, Some("service"), &["service"]);

    let target_ids =
        resolve_scoped_target_document_ids("What does the service handle?", Some(&ir), &index);

    assert!(target_ids.is_empty());
}

#[test]
fn resolve_scoped_target_document_ids_ignores_focus_when_not_single_document_scope() {
    let scoped_document_id = Uuid::now_v7();
    let index = scoped_document_index([(
        scoped_document_id,
        "platform-notes.md",
        Some("platform notes"),
        "platform-notes.md",
    )]);
    let ir = scoped_query_ir(QueryScope::MultiDocument, Some("platform"), &["platform"]);

    assert!(
        resolve_scoped_target_document_ids("Which two services...?", Some(&ir), &index).is_empty()
    );
}

#[test]
fn explicit_target_document_ids_prefer_exact_extension_match() {
    let csv_id = Uuid::now_v7();
    let xlsx_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "In people-100.csv what is Shelby Terrell's job title?",
        [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
    );
    assert_eq!(matched, [csv_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_do_not_fuzzy_match_different_file_reference() {
    let organizations_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "In people-100.csv what is Shelby Terrell's job title?",
        [(organizations_id, "organizations-100.csv")],
    );
    assert!(matched.is_empty());
}

#[test]
fn explicit_target_document_ids_keep_stem_ambiguous_without_extension() {
    let csv_id = Uuid::now_v7();
    let xlsx_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "What is in people-100?",
        [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
    );
    assert_eq!(matched, [csv_id, xlsx_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_prefer_format_marker_with_same_stem() {
    let pdf_id = Uuid::now_v7();
    let docx_id = Uuid::now_v7();
    let pptx_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "What report name appears in the runtime PDF upload check?",
        [
            (pdf_id, "runtime_upload_check.pdf"),
            (docx_id, "runtime_upload_check.docx"),
            (pptx_id, "runtime_upload_check.pptx"),
        ],
    );
    assert_eq!(matched, [pdf_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_keep_stem_ambiguous_without_format_marker() {
    let pdf_id = Uuid::now_v7();
    let docx_id = Uuid::now_v7();
    let pptx_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "What report name appears in the runtime upload check?",
        [
            (pdf_id, "runtime_upload_check.pdf"),
            (docx_id, "runtime_upload_check.docx"),
            (pptx_id, "runtime_upload_check.pptx"),
        ],
    );
    assert_eq!(matched, [pdf_id, docx_id, pptx_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_match_unicode_title_phrase_inside_long_question() {
    let menu_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "How do I complete the café menu update before opening?",
        [(menu_id, "Café menu")],
    );
    assert_eq!(matched, [menu_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_match_separator_normalized_document_stems() {
    let monitoring_id = Uuid::now_v7();
    let schema_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "What alert rules are defined in the monitoring dashboard documentation?",
        [(monitoring_id, "monitoring_dashboard.pdf"), (schema_id, "database_schema.pdf")],
    );
    assert_eq!(matched, [monitoring_id].into_iter().collect());
}

#[test]
fn normalize_document_target_text_splits_colon_boundaries_canonically() {
    assert_eq!(normalize_document_target_text("Provider:Alpha"), "provider alpha");
    assert_eq!(normalize_document_target_text("Provider : Alpha"), "provider alpha");
    assert_eq!(
        normalize_document_target_text("Provider:Alpha:Guide.md"),
        "provider alpha guide.md"
    );
    assert_eq!(normalize_document_target_text("Provider\tAlpha"), "provider alpha");
}

#[test]
fn focus_value_contained_in_hint_prefers_longer_document_candidate() {
    let short_id = Uuid::now_v7();
    let long_id = Uuid::now_v7();
    let unrelated_prefix_id = Uuid::now_v7();
    let matched = document_ids_with_focus_value_contained_in_hint(
        "Open guide for Provider Alpha Configuration",
        &[
            (short_id, "Provider Alpha".to_string()),
            (long_id, "Provider Alpha Configuration".to_string()),
            (unrelated_prefix_id, "Open guide".to_string()),
        ],
    );

    assert_eq!(matched, [long_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_keep_longest_separator_match_canonical() {
    let generic_id = Uuid::now_v7();
    let specific_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "Summarize the monitoring dashboard guide.",
        [(generic_id, "monitoring_dashboard.pdf"), (specific_id, "monitoring_dashboard_guide.pdf")],
    );
    assert_eq!(matched, [specific_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_prefers_current_question_segment() {
    let focused_id = Uuid::now_v7();
    let stale_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        &effective_query_text(
            "Prior assistant listed Provider Alpha Admin Guide.",
            "Provider Alpha setup",
        ),
        [(focused_id, "Provider Alpha"), (stale_id, "Provider Alpha Admin Guide")],
    );
    assert_eq!(matched, [focused_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_reject_partial_title_token_overlap() {
    let opening_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "How should operators register opening time at the store?",
        [(opening_id, "Opening time registration")],
    );
    assert!(matched.is_empty());
}

#[test]
fn explicit_target_document_ids_keep_ambiguous_exact_title_matches_tied() {
    let return_container_id = Uuid::now_v7();
    let return_product_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "Explain return process",
        [(return_container_id, "Return process"), (return_product_id, "Return process")],
    );
    assert_eq!(matched, [return_container_id, return_product_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_reject_one_token_generic_overlap() {
    let policy_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "What status should I use?",
        [(policy_id, "Status Policy")],
    );
    assert!(matched.is_empty());
}

#[test]
fn explicit_target_document_ids_measure_unicode_title_length_in_chars() {
    let short_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values(
        "How do I configure αλφα checkout?",
        [(short_id, "αλφ")],
    );
    assert!(matched.is_empty());
}

#[test]
fn explicit_target_document_ids_keep_exact_short_unicode_title_match() {
    let short_id = Uuid::now_v7();
    let matched = explicit_target_document_ids_from_values("αλφ", [(short_id, "αλφ")]);
    assert_eq!(matched, [short_id].into_iter().collect());
}

#[test]
fn explicit_target_document_ids_require_token_boundaries() {
    let short_id = Uuid::now_v7();
    let matched =
        explicit_target_document_ids_from_values("Explain storage setup.", [(short_id, "RAG")]);
    assert!(matched.is_empty());
}

#[test]
fn resolve_scoped_target_document_ids_keeps_broad_configure_query_unscoped() {
    let generic_document_id = Uuid::now_v7();
    let index = scoped_document_index([(
        generic_document_id,
        "configure.md",
        Some("Configure"),
        "configure.md",
    )]);
    let mut ir = scoped_query_ir(QueryScope::SingleDocument, None, &["checkout setup"]);
    ir.act = QueryAct::ConfigureHow;
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Procedure];

    let target_ids =
        resolve_scoped_target_document_ids("How do I configure checkout setup?", Some(&ir), &index);

    assert!(target_ids.is_empty());
}

#[test]
fn extracts_explicit_document_reference_literals_from_question() {
    assert_eq!(
        explicit_document_reference_literals(
            "What is Shelby Terrell's job title in people-100.csv and what is in sample-heavy-1.xls?"
        ),
        vec!["people-100.csv".to_string(), "sample-heavy-1.xls".to_string()]
    );
}

#[test]
fn explicit_document_reference_literals_ignore_scope_history() {
    assert_eq!(
        explicit_document_reference_literals(&effective_query_text(
            "Prior assistant cited report.pdf.",
            "open the other one"
        )),
        Vec::<String>::new()
    );
}

#[test]
fn explicit_document_reference_literal_matches_path_basename() {
    assert!(explicit_document_reference_literal_is_present(
        "people-100.csv",
        ["exports/archive/people-100.csv"]
    ));
}
