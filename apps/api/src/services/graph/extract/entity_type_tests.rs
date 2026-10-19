//! Behavioural tests for the optional entity-typing stage (IRG-185).
//!
//! The decision leg runs against an in-process `TcpListener`, so the real
//! `SystemOneClient` transport, request validation, response validation and
//! usage extraction are exercised without a live endpoint. The chat leg is
//! driven by a recording `LlmGateway`, which also proves that a decision
//! binding never reaches the generative path. The pure application and identity
//! tests need neither.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    domains::{agent_runtime::RuntimeOverrideBudget, runtime_graph::RuntimeNodeType},
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
        graph::{
            extract::{
                GraphEntityCandidate, GraphEntityTypeClassifierIdentity,
                GraphExtractionCandidateSet, GraphRelationCandidate,
                graph_entity_type_classifier_identity,
            },
            identity::{
                GraphLabelNodeTypeIndex, graph_edge_identity_key, graph_node_identity_key,
                runtime_node_type_slug,
            },
        },
        ingest::runtime::RuntimeTaskExecutionContext,
    },
};

use super::{
    GRAPH_ENTITY_TYPE_ALLOWED, GRAPH_ENTITY_TYPE_CONFIDENCE_FLOOR,
    GRAPH_ENTITY_TYPE_MAX_CANDIDATES, GraphEntityTypeFailure, GraphEntityTypeProviderCall,
    GraphEntityTypeProviderContext, GraphEntityTypeTransport, IndexedEntityType,
    apply_entity_type_assignments, call_entity_type_provider, parse_entity_type_chat_output,
};

const TEST_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_OUTCOME_TIMEOUT: Duration = Duration::from_millis(400);

fn test_outcome_timeout() -> Duration {
    TEST_OUTCOME_TIMEOUT
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

fn decision_binding(base_url: &str) -> ResolvedRuntimeBinding {
    systemone_decision_binding_base(base_url)
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

fn budget() -> Arc<ProviderBudgetRegistry> {
    Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig::unlimited(),
            ProviderBudgetRegistryOptions::default(),
        )
        .expect("build unlimited provider budget registry"),
    )
}

fn transport() -> GraphEntityTypeTransport {
    GraphEntityTypeTransport {
        http_timeout: TEST_HTTP_TIMEOUT,
        max_attempts: 1,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
    }
}

fn provider_context<'a>(
    llm_gateway: &'a Arc<dyn LlmGateway>,
    provider_budget: &'a Arc<ProviderBudgetRegistry>,
    binding: &'a ResolvedRuntimeBinding,
    provider_timeout: Duration,
) -> GraphEntityTypeProviderContext<'a> {
    GraphEntityTypeProviderContext {
        llm_gateway,
        provider_budget,
        transport: transport(),
        binding,
        provider_timeout,
    }
}

fn candidate(label: &str, node_type: RuntimeNodeType) -> GraphEntityCandidate {
    GraphEntityCandidate {
        label: label.to_string(),
        node_type,
        sub_type: None,
        aliases: Vec::new(),
        summary: None,
    }
}

fn candidates(entries: &[(&str, RuntimeNodeType)]) -> GraphExtractionCandidateSet {
    GraphExtractionCandidateSet {
        entities: entries.iter().map(|(label, node_type)| candidate(label, *node_type)).collect(),
        relations: Vec::new(),
    }
}

fn runtime_context(
    entity_type_binding: Option<ResolvedRuntimeBinding>,
) -> RuntimeTaskExecutionContext {
    RuntimeTaskExecutionContext {
        runtime_binding: decision_binding("http://127.0.0.1:9"),
        runtime_overrides: RuntimeOverrideBudget { max_turns: None, max_parallel_actions: None },
        entity_type_binding,
    }
}

/// A `Choice` answer whose whole probability mass sits on `selected`, exactly
/// keyed like the classifier's closed criteria map so response validation
/// accepts it.
fn choice_answer(selected: &str, confidence: f64) -> serde_json::Value {
    let mut probabilities = serde_json::Map::new();
    for node_type in GRAPH_ENTITY_TYPE_ALLOWED {
        let slug = runtime_node_type_slug(&node_type);
        probabilities
            .insert(slug.to_string(), serde_json::json!(if slug == selected { 1.0 } else { 0.0 }));
    }
    serde_json::json!({
        "type": "choice",
        "choice": selected,
        "probabilities": probabilities,
        "confidence": confidence,
    })
}

fn decision_response(answers: &[(&str, f64)]) -> String {
    let mut map = serde_json::Map::new();
    for (index, (slug, confidence)) in answers.iter().enumerate() {
        map.insert(format!("entity-{index}"), choice_answer(slug, *confidence));
    }
    serde_json::json!({
        "model": "jev-latest",
        "answers": map,
        "usage": {"input_tokens": 41, "output_tokens": 7},
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Chat gateway fixture.
// ---------------------------------------------------------------------------

enum ChatOutcome {
    Text { output: String, usage: serde_json::Value },
    Failure,
}

#[derive(Default)]
struct RecordingGateway {
    chats: Mutex<usize>,
    outcomes: Mutex<Vec<ChatOutcome>>,
}

impl RecordingGateway {
    fn with_outcomes(outcomes: Vec<ChatOutcome>) -> Self {
        Self { chats: Mutex::new(0), outcomes: Mutex::new(outcomes) }
    }
}

#[async_trait]
impl LlmGateway for RecordingGateway {
    async fn generate(&self, mut request: ChatRequest) -> anyhow::Result<ChatResponse> {
        *self.chats.lock().expect("chat counter") += 1;
        let outcome = {
            let mut outcomes = self.outcomes.lock().expect("chat outcomes");
            assert!(!outcomes.is_empty(), "the gateway received more calls than were scripted");
            outcomes.remove(0)
        };
        match outcome {
            ChatOutcome::Text { output, usage } => Ok(ChatResponse {
                provider_kind: std::mem::take(&mut request.provider_kind),
                model_name: std::mem::take(&mut request.model_name),
                output_text: output,
                usage_json: usage,
            }),
            ChatOutcome::Failure => Err(anyhow::anyhow!("scripted chat provider failure")),
        }
    }

    async fn embed(&self, _request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        Err(anyhow::anyhow!("embed is not used by the entity-typing dispatch"))
    }

    async fn embed_many(
        &self,
        _request: EmbeddingBatchRequest,
    ) -> anyhow::Result<EmbeddingBatchResponse> {
        Err(anyhow::anyhow!("embed_many is not used by the entity-typing dispatch"))
    }

    async fn vision_extract(&self, _request: VisionRequest) -> anyhow::Result<VisionResponse> {
        Err(anyhow::anyhow!("vision_extract is not used by the entity-typing dispatch"))
    }
}

// ---------------------------------------------------------------------------
// Decision (TypeSafe SystemOne) leg.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn decision_branch_rewrites_every_entity_and_propagates_usage() {
    let server = MockServer::start_single(
        decision_response(&[("organization", 0.9), ("location", 0.9), ("artifact", 0.9)]),
        None,
    )
    .await;
    let gateway = Arc::new(RecordingGateway::default());
    let as_trait: Arc<dyn LlmGateway> = gateway.clone();
    let binding = decision_binding(&server.base_url);
    let mut set = GraphExtractionCandidateSet {
        entities: vec![
            GraphEntityCandidate {
                label: "Alpha".to_string(),
                node_type: RuntimeNodeType::Person,
                sub_type: Some("subtype-leak".to_string()),
                aliases: vec!["alias-leak".to_string()],
                summary: Some("first entity".to_string()),
            },
            candidate("Beta", RuntimeNodeType::Person),
            GraphEntityCandidate {
                label: "Gamma".to_string(),
                node_type: RuntimeNodeType::Concept,
                sub_type: None,
                aliases: Vec::new(),
                summary: Some("third entity".to_string()),
            },
        ],
        relations: Vec::new(),
    };

    let call = call_entity_type_provider(
        provider_context(&as_trait, &budget(), &binding, test_outcome_timeout()),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect("decision call succeeds");

    let usage = call.usage_json().clone();
    let GraphEntityTypeProviderCall::Decision { assignments: Some(assignments), .. } = call else {
        panic!("a decision binding must return decision assignments");
    };
    assert_eq!(assignments.len(), 3, "one question per entity");
    let report = apply_entity_type_assignments(&mut set, &assignments);
    assert_eq!(report.applied, 3);
    assert_eq!(set.entities[0].node_type, RuntimeNodeType::Organization);
    assert_eq!(set.entities[1].node_type, RuntimeNodeType::Location);
    assert_eq!(set.entities[2].node_type, RuntimeNodeType::Artifact);
    assert_eq!(usage["input_tokens"], 41);
    assert_eq!(usage["output_tokens"], 7);

    assert_eq!(*gateway.chats.lock().expect("chat counter"), 0, "TypeSafe is never chat");

    server.wait_for_requests(1).await;
    assert_eq!(server.request_count(), 1, "one bounded batch is one POST");
    let requests = server.requests();
    assert_eq!(requests[0].path, "/v1/systemone");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is JSON");
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["questions"].as_object().map(serde_json::Map::len), Some(3));
    assert!(body["questions"].get("entity-0").is_some());
    assert!(body["questions"].get("entity-2").is_some());

    // Privacy: labels and the entity summary are the only candidate fields sent;
    // aliases, sub-types and every binding/catalog id stay local.
    let rendered = String::from_utf8_lossy(&requests[0].body).to_string();
    assert!(rendered.contains("Alpha"));
    assert!(rendered.contains("first entity"));
    assert!(!rendered.contains("alias-leak"), "an alias leaked into the provider payload");
    assert!(!rendered.contains("subtype-leak"), "a sub-type leaked into the provider payload");
    for internal_id in [
        binding.binding_id,
        binding.provider_catalog_id,
        binding.model_catalog_id,
        binding.library_id,
    ] {
        assert!(
            !rendered.contains(&internal_id.to_string()),
            "internal id {internal_id} leaked into the provider payload"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn decision_batch_is_one_post_capped_at_the_bound_and_keeps_the_tail() {
    let submitted = GRAPH_ENTITY_TYPE_MAX_CANDIDATES;
    let total = submitted + 6;
    let answers = vec![("person", 0.9); submitted];
    let server = MockServer::start_single(decision_response(&answers), None).await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let binding = decision_binding(&server.base_url);
    let mut set = GraphExtractionCandidateSet {
        entities: (0..total)
            .map(|index| candidate(&format!("Entity {index}"), RuntimeNodeType::Concept))
            .collect(),
        relations: Vec::new(),
    };

    let call = call_entity_type_provider(
        provider_context(&gateway, &budget(), &binding, test_outcome_timeout()),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect("decision call succeeds");
    let GraphEntityTypeProviderCall::Decision { assignments: Some(assignments), .. } = call else {
        panic!("a decision binding must return decision assignments");
    };
    assert_eq!(assignments.len(), submitted, "the batch is capped at the candidate bound");
    let report = apply_entity_type_assignments(&mut set, &assignments);
    assert_eq!(report.applied, submitted);
    assert!(
        set.entities[..submitted].iter().all(|entity| entity.node_type == RuntimeNodeType::Person),
        "every submitted entity is classified"
    );
    assert!(
        set.entities[submitted..].iter().all(|entity| entity.node_type == RuntimeNodeType::Concept),
        "the tail beyond the bound keeps its extract-time type"
    );
    assert_eq!(server.request_count(), 1, "a bounded batch is exactly one POST");
    server.shutdown().await;
}

#[tokio::test]
async fn decision_choice_below_the_confidence_floor_keeps_the_extract_time_type() {
    const {
        assert!(0.1 < GRAPH_ENTITY_TYPE_CONFIDENCE_FLOOR);
    };
    let server =
        MockServer::start_single(decision_response(&[("person", 0.1), ("location", 0.9)]), None)
            .await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let binding = decision_binding(&server.base_url);
    let mut set =
        candidates(&[("Alpha", RuntimeNodeType::Concept), ("Beta", RuntimeNodeType::Concept)]);

    let call = call_entity_type_provider(
        provider_context(&gateway, &budget(), &binding, test_outcome_timeout()),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect("decision call succeeds");
    let GraphEntityTypeProviderCall::Decision { assignments: Some(assignments), .. } = call else {
        panic!("a validated response maps assignments");
    };
    let report = apply_entity_type_assignments(&mut set, &assignments);

    assert_eq!(report.applied, 1);
    assert_eq!(report.low_confidence, 1);
    assert_eq!(
        set.entities[0].node_type,
        RuntimeNodeType::Concept,
        "a choice below the confidence floor leaves the extract-time type in place"
    );
    assert_eq!(set.entities[1].node_type, RuntimeNodeType::Location);
    server.shutdown().await;
}

#[tokio::test]
async fn decodable_but_invalid_decision_settles_usage_then_degrades() {
    let missing = {
        let mut answers = serde_json::Map::new();
        answers.insert("entity-0".to_string(), choice_answer("person", 0.9));
        serde_json::json!({
            "model": "jev-latest",
            "answers": answers,
            "usage": {"input_tokens": 41, "output_tokens": 7},
        })
        .to_string()
    };
    let extra = {
        let mut answers = serde_json::Map::new();
        for index in 0..3 {
            answers.insert(format!("entity-{index}"), choice_answer("person", 0.9));
        }
        serde_json::json!({
            "model": "jev-latest",
            "answers": answers,
            "usage": {"input_tokens": 41, "output_tokens": 7},
        })
        .to_string()
    };
    let wrong_type = serde_json::json!({
        "model": "jev-latest",
        "answers": {
            "entity-0": {"type": "noul", "noul": 0.5},
            "entity-1": choice_answer("person", 0.9),
        },
        "usage": {"input_tokens": 41, "output_tokens": 7},
    })
    .to_string();

    for body in [missing, extra, wrong_type] {
        let server = MockServer::start_single(body, None).await;
        let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
        let binding = decision_binding(&server.base_url);
        let mut set =
            candidates(&[("Alpha", RuntimeNodeType::Person), ("Beta", RuntimeNodeType::Person)]);

        let call = call_entity_type_provider(
            provider_context(&gateway, &budget(), &binding, test_outcome_timeout()),
            &set,
            Uuid::now_v7(),
        )
        .await
        .expect("a decodable 200 completes even when its decision is invalid");

        assert!(
            call.usage_json()["input_tokens"].as_u64().is_some_and(|tokens| tokens > 0),
            "a paid decision that fails validation must still carry usage to settle"
        );
        let GraphEntityTypeProviderCall::Decision { assignments, .. } = call else {
            panic!("a decision binding must return a decision call");
        };
        assert!(assignments.is_none(), "an invalid decision maps no assignments");

        // The caller's degrade path applies nothing: the extract-time types stay.
        let report = apply_entity_type_assignments(&mut set, &assignments.unwrap_or_default());
        assert_eq!(report.applied, 0);
        assert!(set.entities.iter().all(|entity| entity.node_type == RuntimeNodeType::Person));
        server.shutdown().await;
    }
}

#[tokio::test]
async fn malformed_decision_body_is_invalid_response_with_no_override() {
    let server = MockServer::start_single("this is not json at all".to_string(), None).await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let binding = decision_binding(&server.base_url);
    let mut set =
        candidates(&[("Alpha", RuntimeNodeType::Person), ("Beta", RuntimeNodeType::Person)]);

    let failure = call_entity_type_provider(
        provider_context(&gateway, &budget(), &binding, test_outcome_timeout()),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect_err("a body that does not decode must fail the call");

    assert_eq!(failure, GraphEntityTypeFailure::InvalidResponse);
    assert_eq!(server.request_count(), 1);
    let report = apply_entity_type_assignments(&mut set, &[]);
    assert_eq!(report.applied, 0);
    assert!(set.entities.iter().all(|entity| entity.node_type == RuntimeNodeType::Person));
    server.shutdown().await;
}

#[tokio::test]
async fn hanging_decision_server_times_out_with_no_override() {
    let server = MockServer::start_single(
        decision_response(&[("person", 0.9)]),
        Some(Duration::from_secs(3)),
    )
    .await;
    let gateway: Arc<dyn LlmGateway> = Arc::new(RecordingGateway::default());
    let binding = decision_binding(&server.base_url);
    let mut set = candidates(&[("Alpha", RuntimeNodeType::Person)]);

    let failure = call_entity_type_provider(
        provider_context(&gateway, &budget(), &binding, Duration::from_millis(120)),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect_err("a hanging server must time out");

    assert_eq!(failure, GraphEntityTypeFailure::TimedOut);
    let report = apply_entity_type_assignments(&mut set, &[]);
    assert_eq!(report.applied, 0);
    assert_eq!(set.entities[0].node_type, RuntimeNodeType::Person);
    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Pure application validation.
// ---------------------------------------------------------------------------

#[test]
fn document_is_never_an_assignable_entity_type() {
    assert!(!GRAPH_ENTITY_TYPE_ALLOWED.contains(&RuntimeNodeType::Document));
    assert_eq!(GRAPH_ENTITY_TYPE_ALLOWED.len(), 10);
}

#[test]
fn apply_rejections_are_counted_per_entity_and_never_override() {
    let mut set = candidates(&[
        ("Alpha", RuntimeNodeType::Person),
        ("Beta", RuntimeNodeType::Concept),
        ("Gamma", RuntimeNodeType::Organization),
        ("Delta", RuntimeNodeType::Event),
    ]);
    let assignments = vec![
        IndexedEntityType { index: 0, node_type: RuntimeNodeType::Document, confidence: None },
        IndexedEntityType { index: 9, node_type: RuntimeNodeType::Person, confidence: None },
        IndexedEntityType { index: 1, node_type: RuntimeNodeType::Location, confidence: None },
        IndexedEntityType { index: 1, node_type: RuntimeNodeType::Artifact, confidence: None },
        IndexedEntityType {
            index: 2,
            node_type: RuntimeNodeType::Event,
            confidence: Some(f64::NAN),
        },
        IndexedEntityType {
            index: 3,
            node_type: RuntimeNodeType::Attribute,
            confidence: Some(0.4),
        },
    ];

    let report = apply_entity_type_assignments(&mut set, &assignments);

    assert_eq!(report.considered, 6);
    assert_eq!(report.applied, 1);
    assert_eq!(report.unknown_type, 1);
    assert_eq!(report.out_of_range, 1);
    assert_eq!(report.duplicate_index, 1);
    assert_eq!(report.low_confidence, 2);
    assert_eq!(
        report.applied
            + report.unknown_type
            + report.out_of_range
            + report.duplicate_index
            + report.low_confidence,
        report.considered
    );
    assert_eq!(set.entities[0].node_type, RuntimeNodeType::Person, "a document type is rejected");
    assert_eq!(set.entities[1].node_type, RuntimeNodeType::Location, "the first valid entry wins");
    assert_eq!(set.entities[2].node_type, RuntimeNodeType::Organization, "non-finite is rejected");
    assert_eq!(set.entities[3].node_type, RuntimeNodeType::Event, "low confidence is rejected");
}

// ---------------------------------------------------------------------------
// Chat (generative) leg.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_branch_reapplies_strict_json_and_propagates_usage() {
    let gateway = Arc::new(RecordingGateway::with_outcomes(vec![ChatOutcome::Text {
        output: r#"{"types":[{"index":0,"node_type":"organization"},{"index":1,"node_type":"location"}]}"#
            .to_string(),
        usage: serde_json::json!({"input_tokens": 12, "output_tokens": 4}),
    }]));
    let as_trait: Arc<dyn LlmGateway> = gateway.clone();
    let binding = chat_binding();
    let mut set =
        candidates(&[("Alpha", RuntimeNodeType::Person), ("Beta", RuntimeNodeType::Person)]);

    let call = call_entity_type_provider(
        provider_context(&as_trait, &budget(), &binding, test_outcome_timeout()),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect("chat call succeeds");

    assert_eq!(call.usage_json()["input_tokens"], 12);
    let GraphEntityTypeProviderCall::Chat { output_text, .. } = call else {
        panic!("a non-decision binding must use the chat route");
    };
    let assignments = parse_entity_type_chat_output(&output_text).expect("strict JSON decodes");
    let report = apply_entity_type_assignments(&mut set, &assignments);
    assert_eq!(report.applied, 2);
    assert_eq!(set.entities[0].node_type, RuntimeNodeType::Organization);
    assert_eq!(set.entities[1].node_type, RuntimeNodeType::Location);
    assert_eq!(*gateway.chats.lock().expect("chat counter"), 1, "one batch is one chat call");
}

#[tokio::test]
async fn chat_provider_failure_degrades_without_overrides() {
    let gateway = Arc::new(RecordingGateway::with_outcomes(vec![ChatOutcome::Failure]));
    let as_trait: Arc<dyn LlmGateway> = gateway.clone();
    let binding = chat_binding();
    let mut set = candidates(&[("Alpha", RuntimeNodeType::Person)]);

    let failure = call_entity_type_provider(
        provider_context(&as_trait, &budget(), &binding, test_outcome_timeout()),
        &set,
        Uuid::now_v7(),
    )
    .await
    .expect_err("a provider error fails the call");

    assert_eq!(failure, GraphEntityTypeFailure::ProviderFailure);
    let report = apply_entity_type_assignments(&mut set, &[]);
    assert_eq!(report.applied, 0);
    assert_eq!(set.entities[0].node_type, RuntimeNodeType::Person);
}

#[test]
fn chat_output_validation_rejects_non_assignable_and_malformed_entries() {
    // A known but non-assignable slug and an unrecognized slug both keep the
    // extract-time type; neither may silently override it.
    for node_type in ["document", "wibble"] {
        let encoded = format!(r#"{{"types":[{{"index":0,"node_type":"{node_type}"}}]}}"#);
        let assignments =
            parse_entity_type_chat_output(&encoded).expect("the envelope still decodes");
        let mut set = candidates(&[("Alpha", RuntimeNodeType::Person)]);
        let report = apply_entity_type_assignments(&mut set, &assignments);
        assert_eq!(report.applied, 0, "{node_type} must not be applied");
        assert_eq!(report.unknown_type, 1, "{node_type} must be counted as an unknown type");
        assert_eq!(set.entities[0].node_type, RuntimeNodeType::Person);
    }

    // A duplicate index applies once and counts the repeat.
    let duplicate =
        r#"{"types":[{"index":0,"node_type":"location"},{"index":0,"node_type":"event"}]}"#;
    let assignments = parse_entity_type_chat_output(duplicate).expect("decodes");
    let mut set = candidates(&[("Alpha", RuntimeNodeType::Person)]);
    let report = apply_entity_type_assignments(&mut set, &assignments);
    assert_eq!(report.applied, 1);
    assert_eq!(report.duplicate_index, 1);
    assert_eq!(set.entities[0].node_type, RuntimeNodeType::Location);

    // Whole-batch failures: nothing is applied.
    for malformed in [
        r#"{"types":[{"index":0}]}"#,
        r#"{"types":[{"index":0,"node_type":"person","extra":1}]}"#,
        r#"{"types":[{"index":0,"node_type":"person"}],"extra":1}"#,
        r#"{"types":"person"}"#,
        r#"{"nodes":[]}"#,
        "not json",
    ] {
        assert_eq!(
            parse_entity_type_chat_output(malformed),
            Err(GraphEntityTypeFailure::InvalidResponse),
            "{malformed} must fail the whole batch closed"
        );
    }
}

// ---------------------------------------------------------------------------
// Identity and edge coherence (pure).
// ---------------------------------------------------------------------------

#[test]
fn classified_type_drives_node_and_edge_identity_keys() {
    let mut set = GraphExtractionCandidateSet {
        entities: vec![
            candidate("Alpha", RuntimeNodeType::Person),
            candidate("Beta", RuntimeNodeType::Person),
        ],
        relations: vec![GraphRelationCandidate {
            source_label: "Alpha".to_string(),
            target_label: "Beta".to_string(),
            relation_type: "works_at".to_string(),
            summary: None,
        }],
    };
    assert_eq!(
        graph_node_identity_key(set.entities[0].node_type, &set.entities[0].label),
        "person:alpha"
    );

    let report = apply_entity_type_assignments(
        &mut set,
        &[IndexedEntityType {
            index: 0,
            node_type: RuntimeNodeType::Organization,
            confidence: None,
        }],
    );
    assert_eq!(report.applied, 1);

    let mut index = GraphLabelNodeTypeIndex::new();
    for entity in &set.entities {
        index.insert(&entity.label, entity.node_type);
    }
    assert_eq!(index.node_identity_key_for_label("Alpha"), "organization:alpha");
    assert_eq!(
        graph_node_identity_key(set.entities[1].node_type, &set.entities[1].label),
        "person:beta"
    );

    let relation = &set.relations[0];
    let from_key = index.node_identity_key_for_label(&relation.source_label);
    let to_key = index.node_identity_key_for_label(&relation.target_label);
    let edge_key = graph_edge_identity_key(&from_key, &relation.relation_type, &to_key);
    assert_eq!(edge_key, "organization:alpha--works_at--person:beta");
    assert_ne!(
        edge_key, "person:alpha--works_at--person:beta",
        "a classified endpoint type must move the edge identity key"
    );
}

#[test]
fn same_label_types_converge_through_the_priority_index() {
    let mut concept_first = GraphLabelNodeTypeIndex::new();
    concept_first.insert("Alpha", RuntimeNodeType::Concept);
    concept_first.insert("Alpha", RuntimeNodeType::Person);
    assert_eq!(concept_first.node_type_for_identity_label("Alpha"), RuntimeNodeType::Person);

    let mut typed_first = GraphLabelNodeTypeIndex::new();
    typed_first.insert("Alpha", RuntimeNodeType::Person);
    typed_first.insert("Alpha", RuntimeNodeType::Concept);
    assert_eq!(typed_first.node_type_for_identity_label("Alpha"), RuntimeNodeType::Person);

    // Document has the lowest priority and never outranks a classified type.
    let mut with_document = GraphLabelNodeTypeIndex::new();
    with_document.insert("Alpha", RuntimeNodeType::Person);
    with_document.insert("Alpha", RuntimeNodeType::Document);
    assert_eq!(with_document.node_type_for_identity_label("Alpha"), RuntimeNodeType::Person);

    assert_eq!(
        concept_first.node_identity_key_for_label("Alpha"),
        typed_first.node_identity_key_for_label("Alpha"),
        "both insertion orders converge on one identity key"
    );
}

// ---------------------------------------------------------------------------
// Off mode.
// ---------------------------------------------------------------------------

#[test]
fn classifier_identity_is_off_without_a_binding_and_on_with_one() {
    assert!(matches!(
        graph_entity_type_classifier_identity(&runtime_context(None)),
        GraphEntityTypeClassifierIdentity::Off
    ));

    let on = runtime_context(Some(decision_binding("http://127.0.0.1:9")));
    assert!(matches!(
        graph_entity_type_classifier_identity(&on),
        GraphEntityTypeClassifierIdentity::On(_)
    ));
}

// ---------------------------------------------------------------------------
// Request contract (IRG-184).
// ---------------------------------------------------------------------------

#[test]
fn decision_options_carry_the_canonical_type_definitions() {
    let input = super::EntityTypeProviderInput {
        candidates: vec![super::EntityTypeProviderInputEntry {
            index: 0,
            label: "Alpha".to_string(),
            summary: None,
        }],
    };
    let request =
        super::build_systemone_entity_type_request(&decision_binding("http://127.0.0.1:9"), &input)
            .expect("one candidate builds a request");
    let Some(crate::integrations::typesafe::SystemOneQuestion::Choice { criteria, .. }) =
        request.questions.get("entity-0")
    else {
        panic!("the entity question is a Choice");
    };
    assert_eq!(criteria.len(), super::super::type_reference::ENTITY_TYPE_REFERENCE.len());
    for reference in super::super::type_reference::ENTITY_TYPE_REFERENCE {
        assert_eq!(
            criteria.get(reference.slug()).cloned().flatten().as_deref(),
            Some(reference.definition),
            "option {} must describe the type, not repeat its name",
            reference.slug()
        );
    }
}

#[test]
fn chat_classifier_prompt_carries_every_type_definition() {
    let prompt = super::entity_type_chat_system_prompt();
    for reference in super::super::type_reference::ENTITY_TYPE_REFERENCE {
        assert!(prompt.contains(&reference.line()), "missing {}", reference.slug());
    }
    let contract = super::entity_type_request_contract();
    assert_eq!(contract.first(), Some(&prompt));
    assert!(
        contract.iter().any(|entry| entry == super::super::type_reference::ENTITY_TYPE_REFERENCE[0].definition),
        "the decision option descriptions are part of the request contract"
    );
}
