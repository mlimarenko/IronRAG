use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    domains::{
        query::RuntimeQueryMode,
        query_ir::{EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope},
    },
    services::query::{
        execution::{
            embed::QuestionEmbeddingResult,
            technical_literals::TechnicalLiteralIntent,
            types::{QueryGraphIndex, RuntimeVectorSearchContext, StructuredQueryPlanningStage},
        },
        planner::{QueryPlanTaskInput, build_task_query_plan},
        support::{IntentResolutionRequest, derive_query_planning_metadata},
    },
};

use super::planning::{
    CompiledIrPlanRefresh, build_compiled_ir_query_plan,
    refresh_query_plan_for_compiled_ir_with_hyde, validate_prepared_query_embedding,
};

fn sample_embedding_result(profile_key: &str, embedding: Vec<f32>) -> QuestionEmbeddingResult {
    QuestionEmbeddingResult { embedding, embedding_profile_key: profile_key.to_string() }
}

/// The hybrid refresh shape every compiled-IR HyDE test shares.
fn compiled_ir_refresh<'a>(
    planning: &'a mut StructuredQueryPlanningStage,
    retrieval_question: &'a str,
    query_ir: &'a QueryIR,
) -> CompiledIrPlanRefresh<'a> {
    CompiledIrPlanRefresh {
        planning,
        retrieval_question,
        mode: RuntimeQueryMode::Hybrid,
        top_k: 8,
        query_ir,
        rerank_enabled: false,
        rerank_candidate_limit: 32,
    }
}

/// Multi-document comparison IR whose refreshed plan recommends HyDE.
fn multi_document_hyde_query_ir(retrieval_question: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: vec![
            EntityMention { label: "Alpha Node".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Gamma Node".to_string(), role: EntityRole::Subject },
        ],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: Some(retrieval_question.to_string()),
        confidence: 0.9,
    }
}

/// Provider-free planning stage for `retrieval_question`, ready for a
/// compiled-IR refresh.
fn provider_free_planning_stage(
    retrieval_question: &str,
    question_embedding: Vec<f32>,
    hyde_embedding: Option<Vec<f32>>,
    embedding_profile_key: &str,
) -> StructuredQueryPlanningStage {
    let metadata = derive_query_planning_metadata(&IntentResolutionRequest {
        question: retrieval_question.to_string(),
        explicit_mode: RuntimeQueryMode::Hybrid,
    });
    let plan = build_task_query_plan(&QueryPlanTaskInput {
        question: retrieval_question.to_string(),
        top_k: Some(8),
        explicit_mode: Some(RuntimeQueryMode::Hybrid),
        metadata: Some(metadata.clone()),
        query_ir: None,
    })
    .expect("provider-free planning should build");
    StructuredQueryPlanningStage {
        planning: metadata,
        plan,
        technical_literal_intent: TechnicalLiteralIntent::default(),
        question_embedding,
        hyde_embedding,
        vector_search_context: Some(RuntimeVectorSearchContext {
            embedding_profile_key: embedding_profile_key.to_string(),
            dimensions: 3,
            active_vector_generation: 1,
            source_truth_version: 1,
        }),
        graph_index: QueryGraphIndex::empty(),
        document_index: HashMap::new(),
        candidate_limit: 1,
    }
}

#[test]
fn prepared_embedding_must_match_request_scoped_profile_and_dimensions() {
    let library_id = Uuid::from_u128(71);
    let context = RuntimeVectorSearchContext {
        embedding_profile_key: "embedding-profile:v1:ready".to_string(),
        dimensions: 3,
        active_vector_generation: 1,
        source_truth_version: 1,
    };

    assert!(
        validate_prepared_query_embedding(
            library_id,
            &context,
            &sample_embedding_result("embedding-profile:v1:ready", vec![0.1, 0.2, 0.3]),
        )
        .is_ok()
    );
    assert!(
        validate_prepared_query_embedding(
            library_id,
            &context,
            &sample_embedding_result("embedding-profile:v1:changed", vec![0.1, 0.2, 0.3]),
        )
        .is_err()
    );
    assert!(
        validate_prepared_query_embedding(
            library_id,
            &context,
            &sample_embedding_result("embedding-profile:v1:ready", vec![0.1, 0.2]),
        )
        .is_err()
    );
}

#[tokio::test]
async fn compiled_ir_query_plan_refresh_without_hyde_is_provider_free() {
    let retrieval_question = "Common prefix shared alpha gamma";
    let query_ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: vec![EntityMention {
            label: "Gamma Node".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };

    let (metadata, plan) =
        build_compiled_ir_query_plan(retrieval_question, RuntimeQueryMode::Hybrid, 8, &query_ir)
            .expect("compiled IR planning should be pure");

    assert_eq!(metadata.keywords.high_level, vec!["gamma".to_string()]);
    assert_eq!(plan.high_level_keywords, vec!["gamma".to_string()]);
    assert!(plan.low_level_keywords.contains(&"common".to_string()));

    let question_embedding = vec![0.1, 0.2, 0.3];
    let mut planning = provider_free_planning_stage(
        retrieval_question,
        question_embedding.clone(),
        Some(vec![0.4, 0.5, 0.6]),
        "embedding-profile:v1:existing",
    );
    let hyde_calls = std::cell::Cell::new(0usize);

    refresh_query_plan_for_compiled_ir_with_hyde(
        compiled_ir_refresh(&mut planning, retrieval_question, &query_ir),
        || {
            hyde_calls.set(hyde_calls.get() + 1);
            std::future::ready(Ok(vec![9.0, 9.0, 9.0]))
        },
    )
    .await
    .expect("compiled IR refresh should not require embeddings");

    assert_eq!(planning.planning.keywords.high_level, vec!["gamma".to_string()]);
    assert_eq!(planning.plan.high_level_keywords, vec!["gamma".to_string()]);
    assert_eq!(planning.question_embedding, question_embedding);
    assert_eq!(planning.hyde_embedding, None);
    assert_eq!(hyde_calls.get(), 0);
    assert_eq!(planning.candidate_limit, 24);
}

#[tokio::test]
async fn common_path_compiled_multidocument_ir_materializes_missing_hyde_only() {
    let retrieval_question = "Describe evidence shared by Alpha Node and Gamma Node";
    let query_ir = multi_document_hyde_query_ir(retrieval_question);
    assert!(!query_ir.is_exact_literal_technical());
    assert!(query_ir.is_multi_document());
    assert!(!query_ir.is_follow_up());

    let question_embedding = vec![0.1, 0.2, 0.3];
    let expected_hyde_embedding = vec![0.4, 0.5, 0.6];
    let mut planning = provider_free_planning_stage(
        retrieval_question,
        question_embedding.clone(),
        None,
        "embedding-profile:v1:question",
    );
    assert!(!planning.plan.hyde_recommended);
    let hyde_calls = std::cell::Cell::new(0usize);

    refresh_query_plan_for_compiled_ir_with_hyde(
        compiled_ir_refresh(&mut planning, retrieval_question, &query_ir),
        || {
            hyde_calls.set(hyde_calls.get() + 1);
            std::future::ready(Ok(expected_hyde_embedding.clone()))
        },
    )
    .await
    .expect("compiled IR refresh should materialize its missing HyDE artifact");

    assert!(planning.plan.hyde_recommended);
    assert_eq!(planning.question_embedding, question_embedding);
    assert_eq!(planning.hyde_embedding.as_deref(), Some(expected_hyde_embedding.as_slice()));
    assert_eq!(
        planning
            .vector_search_context
            .as_ref()
            .map(|context| context.embedding_profile_key.as_str()),
        Some("embedding-profile:v1:question")
    );
    assert_eq!(planning.graph_index.node_count(), 0);
    assert_eq!(planning.document_index.len(), 0);
    assert_eq!(hyde_calls.get(), 1);

    refresh_query_plan_for_compiled_ir_with_hyde(
        compiled_ir_refresh(&mut planning, retrieval_question, &query_ir),
        || {
            hyde_calls.set(hyde_calls.get() + 1);
            std::future::ready(Ok(vec![9.0, 9.0, 9.0]))
        },
    )
    .await
    .expect("unchanged compiled IR planning should reuse its HyDE artifact");

    assert_eq!(planning.question_embedding, question_embedding);
    assert_eq!(planning.hyde_embedding.as_deref(), Some(expected_hyde_embedding.as_slice()));
    assert_eq!(hyde_calls.get(), 1);
}

#[tokio::test]
async fn failed_hyde_creation_degrades_to_question_only_retrieval() {
    let retrieval_question = "Describe evidence shared by Alpha Node and Gamma Node";
    let query_ir = multi_document_hyde_query_ir(retrieval_question);

    let question_embedding = vec![0.1, 0.2, 0.3];
    let mut planning = provider_free_planning_stage(
        retrieval_question,
        question_embedding.clone(),
        None,
        "embedding-profile:v1:question",
    );
    let hyde_calls = std::cell::Cell::new(0usize);

    refresh_query_plan_for_compiled_ir_with_hyde(
        compiled_ir_refresh(&mut planning, retrieval_question, &query_ir),
        || {
            hyde_calls.set(hyde_calls.get() + 1);
            std::future::ready(Err(anyhow::anyhow!("HyDE LLM call returned empty output")))
        },
    )
    .await
    .expect("a failed HyDE artifact must degrade the plan, not fail the turn");

    assert!(planning.plan.hyde_recommended);
    assert_eq!(planning.question_embedding, question_embedding);
    assert_eq!(planning.hyde_embedding, None, "failed HyDE must leave question-only retrieval");
    assert_eq!(hyde_calls.get(), 1);
}
