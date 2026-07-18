use std::collections::HashMap;

use uuid::Uuid;

use crate::domains::query_ir::{
    EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope,
};
use crate::services::query::execution::types::RuntimeVectorSearchContext;

use super::*;

fn sample_embedding_result(profile_key: &str, embedding: Vec<f32>) -> QuestionEmbeddingResult {
    QuestionEmbeddingResult { embedding, embedding_profile_key: profile_key.to_string() }
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

    let initial_planning = derive_query_planning_metadata(&IntentResolutionRequest {
        question: retrieval_question.to_string(),
        explicit_mode: RuntimeQueryMode::Hybrid,
    });
    let initial_plan = build_task_query_plan(&QueryPlanTaskInput {
        question: retrieval_question.to_string(),
        top_k: Some(8),
        explicit_mode: Some(RuntimeQueryMode::Hybrid),
        metadata: Some(initial_planning.clone()),
        query_ir: None,
    })
    .expect("initial planning should build");
    let question_embedding = vec![0.1, 0.2, 0.3];
    let mut planning = StructuredQueryPlanningStage {
        planning: initial_planning,
        plan: initial_plan,
        technical_literal_intent: TechnicalLiteralIntent::default(),
        question_embedding: question_embedding.clone(),
        hyde_embedding: Some(vec![0.4, 0.5, 0.6]),
        vector_search_context: Some(RuntimeVectorSearchContext {
            embedding_profile_key: "embedding-profile:v1:existing".to_string(),
            dimensions: 3,
            active_vector_generation: 1,
            source_truth_version: 1,
        }),
        graph_index: QueryGraphIndex::empty(),
        document_index: HashMap::new(),
        candidate_limit: 1,
    };
    let hyde_calls = std::cell::Cell::new(0usize);

    refresh_query_plan_for_compiled_ir_with_hyde(
        &mut planning,
        retrieval_question,
        RuntimeQueryMode::Hybrid,
        8,
        &query_ir,
        false,
        32,
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
    let query_ir = QueryIR {
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
    };
    assert!(!query_ir.is_exact_literal_technical());
    assert!(query_ir.is_multi_document());
    assert!(!query_ir.is_follow_up());

    let initial_metadata = derive_query_planning_metadata(&IntentResolutionRequest {
        question: retrieval_question.to_string(),
        explicit_mode: RuntimeQueryMode::Hybrid,
    });
    let initial_plan = build_task_query_plan(&QueryPlanTaskInput {
        question: retrieval_question.to_string(),
        top_k: Some(8),
        explicit_mode: Some(RuntimeQueryMode::Hybrid),
        metadata: Some(initial_metadata.clone()),
        query_ir: None,
    })
    .expect("provider-free planning should build");
    assert!(!initial_plan.hyde_recommended);

    let question_embedding = vec![0.1, 0.2, 0.3];
    let expected_hyde_embedding = vec![0.4, 0.5, 0.6];
    let mut planning = StructuredQueryPlanningStage {
        planning: initial_metadata,
        plan: initial_plan,
        technical_literal_intent: TechnicalLiteralIntent::default(),
        question_embedding: question_embedding.clone(),
        hyde_embedding: None,
        vector_search_context: Some(RuntimeVectorSearchContext {
            embedding_profile_key: "embedding-profile:v1:question".to_string(),
            dimensions: 3,
            active_vector_generation: 1,
            source_truth_version: 1,
        }),
        graph_index: QueryGraphIndex::empty(),
        document_index: HashMap::new(),
        candidate_limit: 1,
    };
    let hyde_calls = std::cell::Cell::new(0usize);

    refresh_query_plan_for_compiled_ir_with_hyde(
        &mut planning,
        retrieval_question,
        RuntimeQueryMode::Hybrid,
        8,
        &query_ir,
        false,
        32,
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
        &mut planning,
        retrieval_question,
        RuntimeQueryMode::Hybrid,
        8,
        &query_ir,
        false,
        32,
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
