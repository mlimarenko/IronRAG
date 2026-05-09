use std::{collections::BTreeSet, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct BenchmarkSuite {
    #[serde(rename = "suiteId")]
    suite_id: String,
    #[serde(rename = "strictBlocking")]
    strict_blocking: bool,
    documents: Vec<String>,
    cases: Vec<BenchmarkCase>,
}

#[derive(Debug, Deserialize)]
struct BenchmarkCase {
    id: String,
    question: String,
    #[serde(rename = "searchQuery")]
    search_query: String,
    #[serde(rename = "expectedDocumentsContains")]
    expected_documents_contains: Vec<String>,
    #[serde(rename = "searchRequiredAll", default)]
    search_required_all: Vec<String>,
    #[serde(rename = "answerRequiredAll", default)]
    answer_required_all: Vec<String>,
    #[serde(rename = "answerRequiredAny", default)]
    answer_required_any: Vec<String>,
    #[serde(rename = "answerForbiddenAny", default)]
    answer_forbidden_any: Vec<String>,
    #[serde(rename = "minChunkReferenceCount")]
    min_chunk_reference_count: usize,
    #[serde(rename = "minEntityReferenceCount", default)]
    min_entity_reference_count: usize,
    #[serde(rename = "minRelationReferenceCount", default)]
    min_relation_reference_count: usize,
    #[serde(rename = "expectedEntityReferenceLabelsContains", default)]
    expected_entity_reference_labels_contains: Vec<String>,
    #[serde(rename = "expectedRelationReferenceTextContains", default)]
    expected_relation_reference_text_contains: Vec<String>,
    #[serde(rename = "allowedVerificationStates", default)]
    allowed_verification_states: Vec<String>,
}

fn technical_suite_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("grounded_query")
        .join("technical_contract_suite.json")
}

fn graph_multihop_suite_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("grounded_query")
        .join("graph_multihop_suite.json")
}

#[test]
fn technical_contract_suite_is_consistent_and_release_blocking() -> Result<()> {
    let suite_path = technical_suite_path();
    let suite_bytes = fs::read(&suite_path).with_context(|| {
        format!("failed to read technical benchmark suite {}", suite_path.display())
    })?;
    let suite: BenchmarkSuite = serde_json::from_slice(&suite_bytes).with_context(|| {
        format!("failed to parse technical benchmark suite {}", suite_path.display())
    })?;

    assert_eq!(suite.suite_id, "technical_contract_grounded");
    assert!(suite.strict_blocking, "technical contract suite must block release regressions");
    assert_eq!(suite.documents.len(), 3, "technical suite should stay intentionally small");
    assert_eq!(
        suite.cases.len(),
        6,
        "technical suite should cover the canonical six contract cases"
    );

    let expected_case_ids = BTreeSet::from([
        "checkout_server_system_info",
        "graphql_absent",
        "inventory_wsdl",
        "page_number_param",
        "protocol_comparison",
        "with_cards_param",
    ]);
    let actual_case_ids = suite.cases.iter().map(|case| case.id.as_str()).collect::<BTreeSet<_>>();
    assert_eq!(actual_case_ids, expected_case_ids);

    for relative_document_path in &suite.documents {
        let document_path = suite_path
            .parent()
            .context("technical benchmark suite must live under grounded_query")?
            .join(relative_document_path);
        assert!(
            document_path.exists(),
            "technical benchmark document {} must exist",
            document_path.display()
        );
    }

    for case in &suite.cases {
        assert!(
            !case.question.trim().is_empty(),
            "case {} must define a non-empty question",
            case.id
        );
        assert!(
            !case.search_query.trim().is_empty(),
            "case {} must define a non-empty search query",
            case.id
        );
        assert!(
            !case.expected_documents_contains.is_empty(),
            "case {} must constrain retrieval to a canonical document",
            case.id
        );
        assert!(
            case.min_chunk_reference_count >= 1,
            "case {} must require at least one chunk reference",
            case.id
        );
        assert!(
            case.allowed_verification_states.iter().any(|state| state == "verified"),
            "case {} must require verified execution",
            case.id
        );

        if case.search_required_all.is_empty()
            && case.answer_required_all.is_empty()
            && case.answer_required_any.is_empty()
            && case.answer_forbidden_any.is_empty()
        {
            bail!("case {} has no semantic quality expectations", case.id);
        }
    }

    Ok(())
}

#[test]
fn associative_graph_ranker_suite_is_consistent_and_release_blocking() -> Result<()> {
    let suite_path = graph_multihop_suite_path();
    let suite_bytes = fs::read(&suite_path).with_context(|| {
        format!("failed to read graph benchmark suite {}", suite_path.display())
    })?;
    let suite: BenchmarkSuite = serde_json::from_slice(&suite_bytes).with_context(|| {
        format!("failed to parse graph benchmark suite {}", suite_path.display())
    })?;

    assert_eq!(suite.suite_id, "synthetic_associative_graph_ranker");
    assert!(suite.strict_blocking, "graph multihop suite must block release regressions");
    assert_eq!(suite.documents.len(), 3, "graph suite should stay intentionally small");
    assert_eq!(suite.cases.len(), 4, "graph suite should cover the canonical graph cases");

    let expected_case_ids = BTreeSet::from([
        "graph_backed_answer_does_not_pass_with_chunks_only",
        "multi_anchor_event_beats_single_anchor_noise",
        "relationship_text_disambiguates_same_source",
        "two_hop_bridge_returns_endpoint",
    ]);
    let actual_case_ids = suite.cases.iter().map(|case| case.id.as_str()).collect::<BTreeSet<_>>();
    assert_eq!(actual_case_ids, expected_case_ids);

    for relative_document_path in &suite.documents {
        let document_path = suite_path
            .parent()
            .context("graph benchmark suite must live under grounded_query")?
            .join(relative_document_path);
        assert!(
            document_path.exists(),
            "graph benchmark document {} must exist",
            document_path.display()
        );
    }

    for case in &suite.cases {
        assert!(
            !case.question.trim().is_empty(),
            "case {} must define a non-empty question",
            case.id
        );
        assert!(
            !case.search_query.trim().is_empty(),
            "case {} must define a non-empty search query",
            case.id
        );
        assert!(
            !case.expected_documents_contains.is_empty(),
            "case {} must constrain retrieval to a canonical document",
            case.id
        );
        assert!(
            case.min_chunk_reference_count >= 1,
            "case {} must require chunk grounding",
            case.id
        );
        assert!(
            case.min_entity_reference_count >= 2,
            "case {} must require graph entity references",
            case.id
        );
        assert!(
            case.min_relation_reference_count >= 1,
            "case {} must require graph relation references",
            case.id
        );
        assert!(
            !case.expected_entity_reference_labels_contains.is_empty(),
            "case {} must assert graph entity labels",
            case.id
        );
        assert!(
            !case.expected_relation_reference_text_contains.is_empty(),
            "case {} must assert graph relation reference text",
            case.id
        );
        assert!(
            case.allowed_verification_states.iter().any(|state| state == "verified"),
            "case {} must require verified execution",
            case.id
        );

        if case.search_required_all.is_empty()
            && case.answer_required_all.is_empty()
            && case.answer_required_any.is_empty()
            && case.answer_forbidden_any.is_empty()
        {
            bail!("case {} has no semantic quality expectations", case.id);
        }
    }

    Ok(())
}
