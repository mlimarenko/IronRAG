//! Behavioural tests for the semantic-rerank provider dispatch (IRG-187).
//!
//! The SystemOne leg runs against an in-process `TcpListener`, so the real
//! `SystemOneClient` transport, request validation, response validation and
//! normalisation are exercised without a live endpoint. The chat leg is driven
//! by a recording `LlmGateway` so a decision binding can be proven never to
//! touch it.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    domains::ai::AiBindingPurpose,
    integrations::{
        llm::{
            ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse,
            EmbeddingRequest, EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
        },
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetRegistry, ProviderBudgetRegistryOptions,
        },
        typesafe::MockServer,
    },
    services::{
        ai_catalog_service::{ResolvedRuntimeBinding, systemone_decision_binding_base},
        query::support::RerankCandidate,
    },
};

use super::super::{
    HARD_MAX_USER_MESSAGE_BYTES, PreparedSemanticRerankRequest, SemanticRerankFailure,
    SemanticRerankPolicy, prepare_semantic_rerank_request,
};
use super::{
    RerankProviderCall, RerankProviderCallRequest, RerankProviderTransport,
    build_systemone_decision_request, call_rerank_provider, semantic_scores_from_systemone,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_OUTCOME_TIMEOUT: Duration = Duration::from_millis(400);

/// The five-level rubric the decision-binding fixtures declare, mirroring the
/// seeded provider profile: ordered relevance levels of one `Score` question.
const TEST_SCORE_LEVELS: [&str; 5] = [
    "not relevant to the query",
    "weakly relevant to the query",
    "moderately relevant to the query",
    "strongly relevant to the query",
    "directly and completely answers the query",
];

/// A scripted SystemOne answer whose probability mass sits on one level, so the
/// reported `score` equals that level index and passes response validation.
fn score_answer(level: usize) -> serde_json::Value {
    let mut legend = BTreeMap::new();
    let mut probabilities = BTreeMap::new();
    for (index, level_text) in TEST_SCORE_LEVELS.iter().enumerate() {
        legend.insert(index.to_string(), (*level_text).to_string());
        probabilities.insert(index.to_string(), if index == level { 1.0 } else { 0.0 });
    }
    serde_json::json!({
        "type": "score",
        "score": level as f64,
        "legend": legend,
        "probabilities": probabilities,
        "confidence": 0.9,
    })
}

fn score_response(levels: &[usize]) -> String {
    let mut answers = serde_json::Map::new();
    for (index, level) in levels.iter().enumerate() {
        answers.insert(format!("candidate-{index}"), score_answer(*level));
    }
    serde_json::json!({
        "model": "jev-latest",
        "answers": answers,
        "usage": {"input_tokens": 41, "output_tokens": 7},
    })
    .to_string()
}

fn decision_binding(base_url: &str) -> ResolvedRuntimeBinding {
    let mut binding = systemone_decision_binding_base(base_url);
    binding.binding_purpose = AiBindingPurpose::QueryRerank;
    binding.extra_parameters_json = serde_json::json!({
        "_providerProfile": {
            "runtime": {
                "kind": "typesafe_system_one",
                "scoreCriteria": [
                    "not relevant to the query",
                    "weakly relevant to the query",
                    "moderately relevant to the query",
                    "strongly relevant to the query",
                    "directly and completely answers the query",
                ],
            },
            "baseUrl": {"allowPrivateNetwork": true},
        }
    });
    binding
}

fn chat_binding() -> ResolvedRuntimeBinding {
    let mut binding = decision_binding("https://api.openai.invalid/v1");
    binding.provider_kind = "openai".to_string();
    binding.provider_api_style = "openai_compatible".to_string();
    binding.model_name = "gpt-6-astra".to_string();
    binding.extra_parameters_json = serde_json::json!({
        "_providerProfile": {"runtime": {"kind": "openai_compatible"}}
    });
    binding
}

fn prepared(chunks: &[(&str, &str)]) -> PreparedSemanticRerankRequest {
    let candidates = chunks
        .iter()
        .map(|(id, text)| RerankCandidate {
            id: (*id).to_string(),
            text: (*text).to_string(),
            score: Some(0.5),
        })
        .collect::<Vec<_>>();
    prepare_semantic_rerank_request(
        "find the best candidate",
        &[],
        &[],
        &candidates,
        SemanticRerankPolicy::bounded(5_000, 32, 2_400, 32_000),
    )
    .expect("candidate evidence should produce a prepared request")
}

fn budget() -> Arc<ProviderBudgetRegistry> {
    Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig::unlimited(),
            ProviderBudgetRegistryOptions::default(),
        )
        .expect("build unlimited provider budget registry"),
    )
}

fn test_transport() -> RerankProviderTransport {
    RerankProviderTransport {
        http_timeout: TEST_TIMEOUT,
        max_attempts: 1,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
    }
}

/// Records every chat call so a decision binding can be proven never to reach
/// the generative path, and returns a canned strict `scores` response.
#[derive(Default)]
struct RecordingGateway {
    chats: Mutex<usize>,
}

#[async_trait]
impl LlmGateway for RecordingGateway {
    async fn generate(&self, mut request: ChatRequest) -> anyhow::Result<ChatResponse> {
        *self.chats.lock().expect("chat counter") += 1;
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: serde_json::json!({"scores": [{"index": 0, "score": 0.9}]}).to_string(),
            usage_json: serde_json::json!({"input_tokens": 12, "output_tokens": 4}),
        })
    }

    async fn embed(&self, _request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        Err(anyhow::anyhow!("embed is not used by the rerank dispatch"))
    }

    async fn embed_many(
        &self,
        _request: EmbeddingBatchRequest,
    ) -> anyhow::Result<EmbeddingBatchResponse> {
        Err(anyhow::anyhow!("embed_many is not used by the rerank dispatch"))
    }

    async fn vision_extract(&self, _request: VisionRequest) -> anyhow::Result<VisionResponse> {
        Err(anyhow::anyhow!("vision_extract is not used by the rerank dispatch"))
    }
}

#[test]
fn provider_runtime_kind_reads_the_same_profile_source_as_the_gateway() {
    let binding = decision_binding("http://127.0.0.1:9");
    assert_eq!(binding.provider_runtime_kind(), Some("typesafe_system_one"));
    assert!(binding.provider_allows_private_network());

    let chat = chat_binding();
    assert_eq!(chat.provider_runtime_kind(), Some("openai_compatible"));
    assert!(!chat.provider_allows_private_network(), "a missing policy fails closed");

    let mut missing = decision_binding("http://127.0.0.1:9");
    missing.extra_parameters_json = serde_json::json!({});
    assert_eq!(missing.provider_runtime_kind(), None);
}

#[tokio::test]
async fn decision_branch_normalises_the_ranking_and_propagates_usage() {
    let server = MockServer::start_single(score_response(&[4, 0, 2]), None).await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let prepared = prepared(&[("chunk-a", "first"), ("chunk-b", "second"), ("chunk-c", "third")]);

    let call = call_rerank_provider(RerankProviderCallRequest {
        llm_gateway: &gateway,
        provider_budget: &budget(),
        transport: test_transport(),
        binding: &decision_binding(&server.base_url),
        prepared: &prepared,
        provider_call_id: Uuid::now_v7(),
        provider_timeout: TEST_OUTCOME_TIMEOUT,
    })
    .await
    .expect("decision call succeeds");

    let usage = call.usage_json().clone();
    let ranking = call.into_ranking().expect("scores validate");
    assert_eq!(ranking.ordered_indices(), &[0, 2, 1], "level 4/0/2 must rank 1.0/0.0/0.5");
    assert_eq!(usage["input_tokens"], 41);
    assert_eq!(usage["output_tokens"], 7);

    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "one shortlist is one POST");
    let requests = server.requests();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is JSON");
    assert_eq!(requests[0].path, "/v1/systemone");
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["questions"].as_object().map(serde_json::Map::len), Some(3));
    assert!(body["questions"].get("candidate-0").is_some());
    assert!(body["questions"].get("candidate-2").is_some());
    let rendered = String::from_utf8_lossy(&requests[0].body).to_string();
    for catalog_id in ["chunk-a", "chunk-b", "chunk-c"] {
        assert!(!rendered.contains(catalog_id), "catalog id {catalog_id} leaked into the payload");
    }
    server.shutdown().await;
}

#[tokio::test]
async fn decision_binding_never_touches_the_chat_gateway() {
    let server = MockServer::start_single(score_response(&[1]), None).await;
    let gateway = Arc::new(RecordingGateway::default());
    let as_trait: Arc<dyn LlmGateway> = gateway.clone();
    let prepared = prepared(&[("chunk-a", "first")]);

    call_rerank_provider(RerankProviderCallRequest {
        llm_gateway: &as_trait,
        provider_budget: &budget(),
        transport: test_transport(),
        binding: &decision_binding(&server.base_url),
        prepared: &prepared,
        provider_call_id: Uuid::now_v7(),
        provider_timeout: TEST_OUTCOME_TIMEOUT,
    })
    .await
    .expect("decision call succeeds");

    assert_eq!(*gateway.chats.lock().expect("chat counter"), 0, "TypeSafe is never chat");
    server.shutdown().await;
}

#[tokio::test]
async fn decision_branch_maps_structural_response_failures_to_invalid_response() {
    let missing = {
        let mut answers = serde_json::Map::new();
        answers.insert("candidate-0".to_string(), score_answer(3));
        serde_json::json!({
            "model": "jev-latest",
            "answers": answers,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        })
        .to_string()
    };
    let extra = {
        let mut answers = serde_json::Map::new();
        for index in 0..3 {
            answers.insert(format!("candidate-{index}"), score_answer(2));
        }
        serde_json::json!({
            "model": "jev-latest",
            "answers": answers,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        })
        .to_string()
    };
    let wrong_type = serde_json::json!({
        "model": "jev-latest",
        "answers": {
            "candidate-0": {"type": "noul", "noul": 0.5},
            "candidate-1": score_answer(2),
        },
        "usage": {"input_tokens": 1, "output_tokens": 1},
    })
    .to_string();
    let out_of_range = score_response(&[9, 2]);
    let non_finite = score_response(&[2, 2]).replace("\"score\":2.0", "\"score\":1e400");

    for body in [missing, extra, wrong_type, out_of_range, non_finite] {
        let server = MockServer::start_single(body, None).await;
        let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
        let prepared = prepared(&[("chunk-a", "first"), ("chunk-b", "second")]);
        match call_rerank_provider(RerankProviderCallRequest {
            llm_gateway: &gateway,
            provider_budget: &budget(),
            transport: test_transport(),
            binding: &decision_binding(&server.base_url),
            prepared: &prepared,
            provider_call_id: Uuid::now_v7(),
            provider_timeout: TEST_OUTCOME_TIMEOUT,
        })
        .await
        {
            Ok(call) => {
                assert!(
                    call.usage_json()["input_tokens"].as_u64().is_some_and(|tokens| tokens > 0),
                    "a decodable but invalid decision still settles its usage"
                );
                assert!(
                    call.into_ranking().is_err(),
                    "an invalid decision must degrade the ranking after billing"
                );
            }
            // A body that does not even decode carries no usage to settle.
            Err(SemanticRerankFailure::InvalidResponse) => {}
            Err(other) => panic!("unexpected failure for an invalid decision: {other:?}"),
        }
        server.shutdown().await;
    }
}

#[tokio::test]
async fn decision_branch_times_out_a_hanging_server() {
    let server = MockServer::start_single(score_response(&[1]), Some(Duration::from_secs(3))).await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let prepared = prepared(&[("chunk-a", "first")]);

    let failure = call_rerank_provider(RerankProviderCallRequest {
        llm_gateway: &gateway,
        provider_budget: &budget(),
        transport: test_transport(),
        binding: &decision_binding(&server.base_url),
        prepared: &prepared,
        provider_call_id: Uuid::now_v7(),
        provider_timeout: Duration::from_millis(120),
    })
    .await
    .expect_err("a hanging server must time out");

    assert_eq!(failure, SemanticRerankFailure::TimedOut);
    server.shutdown().await;
}

#[test]
fn semantic_scores_from_systemone_rejects_missing_extra_and_wrong_shapes() {
    let valid = serde_json::from_value::<super::SystemOneDecisionResponse>(serde_json::json!({
        "model": "jev-latest",
        "answers": {"candidate-0": score_answer(4), "candidate-1": score_answer(0)},
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }))
    .expect("valid response decodes");
    let scores = semantic_scores_from_systemone(&valid, 2, 5).expect("valid scores map");
    assert_eq!(scores[0].score, 1.0);
    assert_eq!(scores[1].score, 0.0);

    let missing = serde_json::from_value::<super::SystemOneDecisionResponse>(serde_json::json!({
        "model": "jev-latest",
        "answers": {"candidate-0": score_answer(1)},
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }))
    .expect("decodes");
    assert_eq!(
        semantic_scores_from_systemone(&missing, 2, 5),
        Err(SemanticRerankFailure::InvalidResponse)
    );

    let extra = serde_json::from_value::<super::SystemOneDecisionResponse>(serde_json::json!({
        "model": "jev-latest",
        "answers": {
            "candidate-0": score_answer(1),
            "candidate-1": score_answer(1),
            "candidate-2": score_answer(1),
        },
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }))
    .expect("decodes");
    assert_eq!(
        semantic_scores_from_systemone(&extra, 2, 5),
        Err(SemanticRerankFailure::InvalidResponse)
    );

    let wrong_type =
        serde_json::from_value::<super::SystemOneDecisionResponse>(serde_json::json!({
            "model": "jev-latest",
            "answers": {"candidate-0": {"type": "noul", "noul": 0.5}},
            "usage": {"input_tokens": 1, "output_tokens": 1},
        }))
        .expect("decodes");
    assert_eq!(
        semantic_scores_from_systemone(&wrong_type, 1, 5),
        Err(SemanticRerankFailure::InvalidResponse)
    );
}

#[test]
fn decision_ranking_maps_back_to_internal_ids_and_keeps_the_unsubmitted_tail() {
    let prepared = prepared(&[("chunk-a", "first"), ("chunk-b", "second"), ("chunk-c", "third")]);
    let response = serde_json::from_value::<super::SystemOneDecisionResponse>(serde_json::json!({
        "model": "jev-latest",
        "answers": {
            "candidate-0": score_answer(4),
            "candidate-1": score_answer(0),
            "candidate-2": score_answer(2),
        },
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }))
    .expect("valid response decodes");
    let scores = semantic_scores_from_systemone(&response, 3, 5).expect("scores map");
    let ranking = super::super::scoring::validate_semantic_scores(scores, 3).expect("validate");

    let order = prepared.map_ranking_to_candidate_order(&ranking);

    assert_eq!(order.chunks, vec!["chunk-a", "chunk-c", "chunk-b"]);
}

#[test]
fn a_truncated_decision_ranking_keeps_the_unsubmitted_tail_in_source_order() {
    let prepared = prepared(&[("chunk-a", "first"), ("chunk-b", "second"), ("chunk-c", "third")]);
    let call = RerankProviderCall::Decision {
        scores: Some(vec![
            super::super::scoring::ProviderSemanticScore { index: 0, score: 0.1 },
            super::super::scoring::ProviderSemanticScore { index: 1, score: 0.9 },
        ]),
        usage_json: serde_json::json!({"input_tokens": 1, "output_tokens": 1}),
        expected_count: 2,
    };

    let ranking = call.into_ranking().expect("the submitted prefix validates");
    let order = prepared.map_ranking_to_candidate_order(&ranking);

    assert_eq!(order.chunks, vec!["chunk-b", "chunk-a", "chunk-c"]);
    assert_eq!(
        order.provider_ranked_chunk_ids().to_vec(),
        vec!["chunk-b".to_string(), "chunk-a".to_string()]
    );
}

#[test]
fn decision_request_is_bounded_by_the_hard_byte_cap_and_keeps_indices_contiguous() {
    let chunks =
        (0..32).map(|index| (format!("chunk-{index}"), "\0\"\\".repeat(800))).collect::<Vec<_>>();
    let chunks = chunks.iter().map(|(id, text)| (id.as_str(), text.as_str())).collect::<Vec<_>>();
    let prepared = prepared(&chunks);
    let total = prepared.prepared_candidate_count();
    assert!(total > 1, "the fixture must prepare more than one candidate");

    let binding = decision_binding("http://127.0.0.1:9");
    let levels = binding.decision_score_criteria().expect("the fixture declares a rubric");
    let (request, submitted) = build_systemone_decision_request(&binding, &prepared, &levels)
        .expect("a bounded request must still be buildable");

    let serialized = serde_json::to_vec(&request).expect("serialize");
    assert!(
        serialized.len() <= HARD_MAX_USER_MESSAGE_BYTES,
        "the decision request must obey the same hard cap as the chat message"
    );
    assert!(submitted < total, "the tail must be dropped to fit the cap");
    assert!(
        submitted <= crate::integrations::typesafe::MAX_QUESTIONS,
        "the builder must respect the protocol question cap"
    );
    assert_eq!(request.questions.len(), submitted);
    for index in 0..submitted {
        assert!(request.questions.contains_key(&format!("candidate-{index}")));
    }
    assert!(!request.questions.contains_key(&format!("candidate-{submitted}")));
}

#[tokio::test]
async fn chat_binding_still_uses_the_chat_gateway() {
    let gateway = Arc::new(RecordingGateway::default());
    let as_trait: Arc<dyn LlmGateway> = gateway.clone();
    let prepared = prepared(&[("chunk-a", "first")]);

    let call = call_rerank_provider(RerankProviderCallRequest {
        llm_gateway: &as_trait,
        provider_budget: &budget(),
        transport: test_transport(),
        binding: &chat_binding(),
        prepared: &prepared,
        provider_call_id: Uuid::now_v7(),
        provider_timeout: TEST_OUTCOME_TIMEOUT,
    })
    .await
    .expect("chat call succeeds");

    assert!(matches!(call, RerankProviderCall::Chat { .. }));
    assert_eq!(*gateway.chats.lock().expect("chat counter"), 1);
}

#[tokio::test]
async fn decision_binding_without_a_score_rubric_fails_closed() {
    let server = MockServer::start_single(score_response(&[1]), None).await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let mut binding = decision_binding(&server.base_url);
    binding.extra_parameters_json = serde_json::json!({
        "_providerProfile": {
            "runtime": {"kind": "typesafe_system_one"},
            "baseUrl": {"allowPrivateNetwork": true},
        }
    });
    let prepared = prepared(&[("chunk-a", "first")]);

    let failure = call_rerank_provider(RerankProviderCallRequest {
        llm_gateway: &gateway,
        provider_budget: &budget(),
        transport: test_transport(),
        binding: &binding,
        prepared: &prepared,
        provider_call_id: Uuid::now_v7(),
        provider_timeout: TEST_OUTCOME_TIMEOUT,
    })
    .await
    .expect_err("a decision binding without a rubric must fail closed");

    assert_eq!(failure, SemanticRerankFailure::ProviderFailure);
    assert_eq!(server.request_count(), 0, "no request may be sent without a rubric");
    server.shutdown().await;
}
