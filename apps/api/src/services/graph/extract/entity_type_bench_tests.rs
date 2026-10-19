//! Live A/B benchmark of the optional entity-typing stage (IRG-184).
//!
//! Every arm runs the product path — `call_entity_type_provider`, then
//! `entity_type_assignments`, then `apply_entity_type_assignments` — over the
//! frozen, labelled corpus `apps/api/benchmarks/graph_entity_type/corpus.json`,
//! and the run prints one JSON report. Bindings are built from the canonical
//! catalog seed (profile, base URL, model rate), never from a hand copy.
//!
//! The same report carries an `llm_only` section: for every batch of the same
//! corpus it concatenates the batch's labels and summaries into one neutral
//! text fragment and runs the product extraction path
//! `resolve_graph_extraction_with_gateway` over it, so the entity types the
//! extractor assigns without the typing stage can be compared with the typing
//! arms. This is a proxy mode over a synthetic fragment, not over source
//! documents.
//!
//! The run calls paid external APIs, so it is `#[ignore]`d, skipped by
//! `backend-test-lib-ignored`, and started only by
//! `make backend-bench-entity-type` with the arm credentials exported:
//!
//! - `ENTITY_TYPE_BENCH_TYPESAFE_API_KEY`: the TypeSafe SystemOne arm on the seeded
//!   `typesafe` provider and model;
//! - `ENTITY_TYPE_BENCH_LITELLM_API_KEY`, `ENTITY_TYPE_BENCH_LITELLM_BASE_URL` and
//!   `ENTITY_TYPE_BENCH_LITELLM_ARMS`: chat arms on the seeded `litellm` profile;
//!   each comma-separated arm is `<model>` or `<model>@<reasoning_effort>`;
//! - `ENTITY_TYPE_BENCH_REPEATS` (default 2): pass 1 is cold, later passes warm;
//! - `ENTITY_TYPE_BENCH_REPORT`: required path the JSON report is written to.
//!
//! The variables deliberately avoid the `IRONRAG_` prefix: application settings
//! reject provider credentials under it. An arm whose credential is absent is
//! reported as not measured; with no arm configured the run fails instead of
//! passing empty.

use std::{collections::BTreeMap, sync::Arc, time::Instant};

use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::{config::Settings, seeded_provider},
    domains::{ai::AiBindingPurpose, runtime_graph::RuntimeNodeType},
    integrations::{
        llm::{LlmGateway, UnifiedGateway},
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetRegistry, ProviderBudgetRegistryOptions,
        },
    },
    services::{
        graph::{
            extract::{GraphEntityCandidate, GraphExtractionCandidateSet},
            identity::runtime_node_type_slug,
        },
        ops::provider_failure::ProviderFailureClassificationService,
    },
};

use super::super::prompt::GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS;
use super::{
    GRAPH_ENTITY_TYPE_ALLOWED, GRAPH_ENTITY_TYPE_MAX_CANDIDATES, GraphEntityTypeApplyReport,
    GraphEntityTypeProviderContext, GraphEntityTypeTransport, apply_entity_type_assignments,
    call_entity_type_provider, entity_type_assignments,
};
use crate::services::bench_tests::{
    Arm, LITELLM_KIND, env_value, graph_extraction_request, latency_summary, litellm_arms,
    seeded_binding, usage_tokens,
};

const CORPUS_PATH: &str = "benchmarks/graph_entity_type/corpus.json";
/// Extract-time type every corpus entity starts with. The stage never assigns
/// `document`, so an entity still typed `document` afterwards is one the
/// classifier left untyped (failed call, rejection or low confidence), while a
/// deliberate catch-all answer shows up as `entity`.
const PLACEHOLDER_TYPE: RuntimeNodeType = RuntimeNodeType::Document;
const ABSTAIN: &str = "abstain";
const TYPESAFE_KIND: &str = "typesafe";
const TYPESAFE_MODEL: &str = "jev-latest";
const ENV_PREFIX: &str = "ENTITY_TYPE_BENCH";
/// Report note: the LLM-only arm is a proxy measurement.
const LLM_ONLY_NOTE: &str = "proxy mode: extract_graph runs over a synthetic fragment that joins each batch's `label: summary`, not over source documents; extracted entities are matched to corpus entities by case-folded label.";

#[derive(Debug, Deserialize)]
struct Corpus {
    version: String,
    batch_size: usize,
    entities: Vec<CorpusEntity>,
}

#[derive(Debug, Deserialize)]
struct CorpusEntity {
    label: String,
    summary: String,
    gold: String,
    #[serde(default)]
    accept: Vec<String>,
}

impl CorpusEntity {
    fn accepts(&self, predicted: &str) -> bool {
        predicted == self.gold || self.accept.iter().any(|alternative| alternative == predicted)
    }
}

fn load_corpus() -> Corpus {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS_PATH);
    let text = std::fs::read_to_string(&path).expect("read entity-type benchmark corpus");
    serde_json::from_str(&text).expect("decode entity-type benchmark corpus")
}

fn allowed_slugs() -> Vec<&'static str> {
    GRAPH_ENTITY_TYPE_ALLOWED.iter().map(runtime_node_type_slug).collect()
}

/// The corpus is part of the benchmark contract: a broken or drifting file must
/// fail on every pipeline, not on the next paid run.
#[test]
fn corpus_is_labelled_with_the_closed_type_list() {
    let corpus = load_corpus();
    let allowed = allowed_slugs();
    assert!(!corpus.version.is_empty());
    assert!(corpus.batch_size > 0 && corpus.batch_size <= GRAPH_ENTITY_TYPE_MAX_CANDIDATES);
    assert!(!corpus.entities.is_empty());
    let mut labels = std::collections::BTreeSet::new();
    for entity in &corpus.entities {
        assert!(labels.insert(entity.label.as_str()), "duplicate label {:?}", entity.label);
        assert!(!entity.summary.trim().is_empty(), "{:?} has no summary", entity.label);
        assert!(
            allowed.contains(&entity.gold.as_str()),
            "{:?} gold {:?}",
            entity.label,
            entity.gold
        );
        assert_ne!(entity.gold, runtime_node_type_slug(&PLACEHOLDER_TYPE));
        for alternative in &entity.accept {
            assert!(allowed.contains(&alternative.as_str()) && *alternative != entity.gold);
        }
    }
}

/// Arms configured in the environment, plus the names of arms left unmeasured.
fn configured_arms() -> (Vec<Arm>, Vec<String>) {
    let mut arms = Vec::new();
    let mut unmeasured = Vec::new();
    match typesafe_arm() {
        Some(arm) => arms.push(arm),
        None => unmeasured.push(format!("{TYPESAFE_KIND}/{TYPESAFE_MODEL}: no credential")),
    }
    match litellm_arms(ENV_PREFIX, AiBindingPurpose::GraphEntityType) {
        Some(chat_arms) => arms.extend(chat_arms),
        None => unmeasured.push(format!("{LITELLM_KIND}: credential, base URL or arms not set")),
    }
    (arms, unmeasured)
}

fn typesafe_arm() -> Option<Arm> {
    let api_key = env_value(&format!("{ENV_PREFIX}_TYPESAFE_API_KEY"))?;
    let base_url = seeded_provider(TYPESAFE_KIND)
        .and_then(|seed| seed.default_base_url)
        .expect("typesafe seed declares its base URL");
    // The seed stores zero for "unpriced": TypeSafe publishes no per-token
    // rate, so no cost is derived for this arm.
    Some(Arm {
        name: format!("{TYPESAFE_KIND}/{TYPESAFE_MODEL}"),
        binding: seeded_binding(
            AiBindingPurpose::GraphEntityType,
            TYPESAFE_KIND,
            base_url,
            api_key,
            TYPESAFE_MODEL,
            serde_json::Map::new(),
        ),
        rate: None,
        rate_source: "unpriced: TypeSafe publishes no per-token rate (catalog seed)".to_string(),
    })
}

/// One pass of one arm over the whole corpus.
struct Pass {
    predictions: Vec<String>,
    latencies_ms: Vec<u128>,
    failures: BTreeMap<String, usize>,
    /// Sum of the stage's own per-batch application reports.
    applied: GraphEntityTypeApplyReport,
    input_tokens: u64,
    output_tokens: u64,
}

fn add_report(total: &mut GraphEntityTypeApplyReport, batch: GraphEntityTypeApplyReport) {
    total.considered += batch.considered;
    total.applied += batch.applied;
    total.unknown_type += batch.unknown_type;
    total.out_of_range += batch.out_of_range;
    total.duplicate_index += batch.duplicate_index;
    total.low_confidence += batch.low_confidence;
}

async fn run_pass(
    arm: &Arm,
    corpus: &Corpus,
    gateway: &Arc<dyn LlmGateway>,
    budget: &Arc<ProviderBudgetRegistry>,
    transport: GraphEntityTypeTransport,
) -> Pass {
    let mut pass = Pass {
        predictions: Vec::with_capacity(corpus.entities.len()),
        latencies_ms: Vec::new(),
        failures: BTreeMap::new(),
        applied: GraphEntityTypeApplyReport::default(),
        input_tokens: 0,
        output_tokens: 0,
    };
    for batch in corpus.entities.chunks(corpus.batch_size) {
        let mut candidates = GraphExtractionCandidateSet {
            entities: batch
                .iter()
                .map(|entity| GraphEntityCandidate {
                    label: entity.label.clone(),
                    node_type: PLACEHOLDER_TYPE,
                    sub_type: None,
                    aliases: Vec::new(),
                    summary: Some(entity.summary.clone()),
                })
                .collect(),
            relations: Vec::new(),
        };
        let started = Instant::now();
        let call = call_entity_type_provider(
            GraphEntityTypeProviderContext {
                llm_gateway: gateway,
                provider_budget: budget,
                transport,
                binding: &arm.binding,
                provider_timeout: transport.http_timeout,
            },
            &candidates,
            Uuid::now_v7(),
        )
        .await;
        pass.latencies_ms.push(started.elapsed().as_millis());
        match call {
            Ok(call) => {
                let (input, output) = usage_tokens(call.usage_json());
                pass.input_tokens += input;
                pass.output_tokens += output;
                match entity_type_assignments(call) {
                    Ok(assignments) => add_report(
                        &mut pass.applied,
                        apply_entity_type_assignments(&mut candidates, &assignments),
                    ),
                    Err(failure) => {
                        *pass.failures.entry(format!("{failure:?}")).or_default() += 1;
                    }
                }
            }
            Err(failure) => *pass.failures.entry(format!("{failure:?}")).or_default() += 1,
        }
        pass.predictions.extend(candidates.entities.iter().map(|entity| {
            if entity.node_type == PLACEHOLDER_TYPE {
                ABSTAIN.to_string()
            } else {
                runtime_node_type_slug(&entity.node_type).to_string()
            }
        }));
    }
    pass
}

/// `(strict, accepted, abstained)` over the corpus order, shared by the typing
/// and LLM-only arms.
fn accuracy(corpus: &Corpus, predictions: &[String]) -> (usize, usize, usize) {
    let (mut strict, mut accepted, mut abstained) = (0usize, 0usize, 0usize);
    for (entity, predicted) in corpus.entities.iter().zip(predictions) {
        if predicted == ABSTAIN {
            abstained += 1;
        } else {
            strict += usize::from(*predicted == entity.gold);
            accepted += usize::from(entity.accepts(predicted));
        }
    }
    (strict, accepted, abstained)
}

fn pass_report(arm: &Arm, corpus: &Corpus, pass: &Pass) -> serde_json::Value {
    let total = corpus.entities.len();
    let (strict, accepted, abstained) = accuracy(corpus, &pass.predictions);
    serde_json::json!({
        "entities": total,
        "batches": pass.latencies_ms.len(),
        "accuracy_strict": strict as f64 / total as f64,
        "accuracy_accept": accepted as f64 / total as f64,
        "wrong": total - accepted - abstained,
        "abstained": abstained,
        "failed_batches": pass.failures,
        "stage_rejections": {
            "answers_considered": pass.applied.considered,
            "applied": pass.applied.applied,
            "low_confidence": pass.applied.low_confidence,
            "unknown_type": pass.applied.unknown_type,
            "out_of_range": pass.applied.out_of_range,
            "duplicate_index": pass.applied.duplicate_index,
        },
        "latency_ms_per_batch": latency_summary(&pass.latencies_ms),
        "input_tokens": pass.input_tokens,
        "output_tokens": pass.output_tokens,
        "cost_usd": arm.cost_usd(pass.input_tokens, pass.output_tokens),
    })
}

fn quality_detail(corpus: &Corpus, predictions: &[String]) -> serde_json::Value {
    let mut per_type: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut confusion: BTreeMap<String, usize> = BTreeMap::new();
    let mut misses = Vec::new();
    for (entity, predicted) in corpus.entities.iter().zip(predictions) {
        let slot = per_type.entry(entity.gold.as_str()).or_default();
        slot.1 += 1;
        if entity.accepts(predicted) {
            slot.0 += 1;
        } else {
            *confusion.entry(format!("{} -> {predicted}", entity.gold)).or_default() += 1;
            misses.push(format!("{} ({} -> {predicted})", entity.label, entity.gold));
        }
    }
    serde_json::json!({
        "accuracy_accept_by_gold_type": per_type
            .into_iter()
            .map(|(gold, (hit, all))| (gold.to_string(), serde_json::json!(format!("{hit}/{all}"))))
            .collect::<serde_json::Map<_, _>>(),
        "confusion": confusion,
        "misses": misses,
    })
}

fn agreement(left: &[String], right: &[String]) -> f64 {
    let same = left.iter().zip(right).filter(|(a, b)| a == b).count();
    same as f64 / left.len().max(1) as f64
}

/// One pass of the LLM-only arm: `extract_graph` over the whole corpus, one
/// synthetic fragment per batch.
#[derive(Default)]
struct LlmOnlyPass {
    /// One prediction per corpus entity, in corpus order: the matched
    /// extraction's type slug, or [`ABSTAIN`] when the label was not extracted.
    predictions: Vec<String>,
    latencies_ms: Vec<u128>,
    failures: BTreeMap<String, usize>,
    covered: usize,
    extracted: usize,
    entity_fallback: usize,
    input_tokens: u64,
    output_tokens: u64,
}

/// Recovery settings of the extraction product path, as the graph-extraction
/// benchmark uses them.
struct LlmOnlyRecovery {
    enabled: bool,
    max_attempts: usize,
}

/// Neutral fragment for one batch: `label: summary` per entity, joined with a
/// space. It carries no type vocabulary, so the extractor's own types are
/// measured rather than a hint.
fn synthetic_batch_text(batch: &[CorpusEntity]) -> String {
    batch
        .iter()
        .map(|entity| format!("{}: {}", entity.label, entity.summary))
        .collect::<Vec<_>>()
        .join(" ")
}

/// How one extraction batch matched the corpus batch.
struct BatchMatch {
    predictions: Vec<String>,
    covered: usize,
    entity_fallback: usize,
}

/// Match extracted entities to the corpus batch by case-folded label; the first
/// extraction of a label wins.
fn match_extracted(batch: &[CorpusEntity], extracted: &[GraphEntityCandidate]) -> BatchMatch {
    let mut by_label: BTreeMap<String, &GraphEntityCandidate> = BTreeMap::new();
    let mut entity_fallback = 0usize;
    for candidate in extracted {
        if candidate.node_type == RuntimeNodeType::Entity {
            entity_fallback += 1;
        }
        by_label.entry(candidate.label.trim().to_lowercase()).or_insert(candidate);
    }
    let mut predictions = Vec::with_capacity(batch.len());
    let mut covered = 0usize;
    for entity in batch {
        match by_label.get(&entity.label.trim().to_lowercase()) {
            Some(candidate) => {
                covered += 1;
                predictions.push(runtime_node_type_slug(&candidate.node_type).to_string());
            }
            None => predictions.push(ABSTAIN.to_string()),
        }
    }
    BatchMatch { predictions, covered, entity_fallback }
}

async fn run_llm_only_pass(
    arm: &Arm,
    corpus: &Corpus,
    gateway: &Arc<dyn LlmGateway>,
    recovery: &LlmOnlyRecovery,
) -> LlmOnlyPass {
    let classification = ProviderFailureClassificationService::default();
    let mut pass = LlmOnlyPass::default();
    for (index, batch) in corpus.entities.chunks(corpus.batch_size).enumerate() {
        let request = graph_extraction_request(
            "benchmarks/graph_entity_type/synthetic-fragment",
            &format!("Entity type benchmark batch {}", index + 1),
            &synthetic_batch_text(batch),
            i32::try_from(index).unwrap_or(i32::MAX),
        );
        let run = super::super::extraction_bench_tests::run_graph_extraction(
            gateway.as_ref(),
            &arm.binding,
            &request,
            &classification,
            recovery.enabled,
            recovery.max_attempts,
        )
        .await;
        pass.latencies_ms.push(run.elapsed_ms);
        pass.input_tokens += run.input_tokens;
        pass.output_tokens += run.output_tokens;
        match run.failure {
            None => {
                let matched = match_extracted(batch, &run.normalized.entities);
                pass.covered += matched.covered;
                pass.entity_fallback += matched.entity_fallback;
                pass.extracted += run.normalized.entities.len();
                pass.predictions.extend(matched.predictions);
            }
            Some(failure) => {
                *pass.failures.entry(format!("{failure:?}")).or_default() += 1;
                pass.predictions.extend((0..batch.len()).map(|_| ABSTAIN.to_string()));
            }
        }
    }
    pass
}

fn llm_only_pass_report(arm: &Arm, corpus: &Corpus, pass: &LlmOnlyPass) -> serde_json::Value {
    let total = corpus.entities.len();
    let (strict, accepted, abstained) = accuracy(corpus, &pass.predictions);
    serde_json::json!({
        "entities": total,
        "batches": pass.latencies_ms.len(),
        "covered": pass.covered,
        "coverage": pass.covered as f64 / total as f64,
        "extracted_entities": pass.extracted,
        "entity_fallback_share": (pass.extracted > 0)
            .then(|| pass.entity_fallback as f64 / pass.extracted as f64),
        "accuracy_strict": strict as f64 / total as f64,
        "accuracy_accept": accepted as f64 / total as f64,
        "wrong": total - accepted - abstained,
        "uncovered": abstained,
        "failed_batches": pass.failures,
        "latency_ms_per_batch": latency_summary(&pass.latencies_ms),
        "input_tokens": pass.input_tokens,
        "output_tokens": pass.output_tokens,
        "cost_usd": arm.cost_usd(pass.input_tokens, pass.output_tokens),
    })
}

/// The `llm_only` report section: `extract_graph` over the corpus batches,
/// without the typing stage. Chat arms are the same `ENTITY_TYPE_BENCH_LITELLM_*`
/// arms as the typing arms, bound for `ExtractGraph`.
async fn llm_only_section(
    corpus: &Corpus,
    gateway: &Arc<dyn LlmGateway>,
    recovery: &LlmOnlyRecovery,
    repeats: usize,
) -> serde_json::Value {
    let Some(arms) = litellm_arms(ENV_PREFIX, AiBindingPurpose::ExtractGraph) else {
        return serde_json::json!({
            "note": LLM_ONLY_NOTE,
            "arms": {},
            "unmeasured": ["extract_graph: credential, base URL or arms not set"],
        });
    };
    let mut arm_reports = serde_json::Map::new();
    for arm in &arms {
        let mut passes = Vec::with_capacity(repeats);
        for _ in 0..repeats {
            passes.push(run_llm_only_pass(arm, corpus, gateway, recovery).await);
        }
        let cold = &passes[0];
        arm_reports.insert(
            arm.name.clone(),
            serde_json::json!({
                "model": arm.binding.model_name,
                "rate_source": arm.rate_source,
                "passes": passes
                    .iter()
                    .map(|pass| llm_only_pass_report(arm, corpus, pass))
                    .collect::<Vec<_>>(),
                "cold_quality": quality_detail(corpus, &cold.predictions),
                "warm_agreement_with_cold": passes
                    .iter()
                    .skip(1)
                    .map(|pass| agreement(&cold.predictions, &pass.predictions))
                    .collect::<Vec<_>>(),
            }),
        );
    }
    serde_json::json!({
        "note": LLM_ONLY_NOTE,
        "batch_size": corpus.batch_size,
        "entities": corpus.entities.len(),
        "arms": arm_reports,
        "unmeasured": [],
    })
}

#[tokio::test]
#[ignore = "calls paid external providers; run through `make backend-bench-entity-type`"]
async fn entity_type_classifier_benchmark() {
    let corpus = load_corpus();
    let report_path = env_value("ENTITY_TYPE_BENCH_REPORT")
        .expect("ENTITY_TYPE_BENCH_REPORT names the report file");
    let (arms, unmeasured) = configured_arms();
    assert!(!arms.is_empty(), "no benchmark arm is configured: {unmeasured:?}");
    let repeats = env_value("ENTITY_TYPE_BENCH_REPEATS")
        .map(|value| value.parse::<usize>().expect("ENTITY_TYPE_BENCH_REPEATS is a count"))
        .unwrap_or(2)
        .max(1);
    let settings = Settings::from_env().expect("settings defaults load");
    let transport = GraphEntityTypeTransport::from_settings(&settings);
    let llm_recovery = LlmOnlyRecovery {
        enabled: settings.runtime_graph_extract_recovery_enabled,
        max_attempts: settings
            .runtime_graph_extract_recovery_max_attempts
            .clamp(1, GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS),
    };
    let gateway: Arc<dyn LlmGateway> =
        Arc::new(UnifiedGateway::from_settings(&settings).expect("build chat gateway"));
    let budget = Arc::new(
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig::unlimited(),
            ProviderBudgetRegistryOptions::default(),
        )
        .expect("build unlimited provider budget registry"),
    );

    let mut arm_reports = serde_json::Map::new();
    let mut cold_predictions: Vec<(String, Vec<String>)> = Vec::new();
    for arm in &arms {
        let mut passes = Vec::with_capacity(repeats);
        for _ in 0..repeats {
            passes.push(run_pass(arm, &corpus, &gateway, &budget, transport).await);
        }
        let cold = &passes[0];
        let pass_reports =
            passes.iter().map(|pass| pass_report(arm, &corpus, pass)).collect::<Vec<_>>();
        let stability = passes
            .iter()
            .skip(1)
            .map(|pass| agreement(&cold.predictions, &pass.predictions))
            .collect::<Vec<_>>();
        arm_reports.insert(
            arm.name.clone(),
            serde_json::json!({
                "model": arm.binding.model_name,
                "rate_source": arm.rate_source,
                "passes": pass_reports,
                "cold_quality": quality_detail(&corpus, &cold.predictions),
                "warm_agreement_with_cold": stability,
            }),
        );
        cold_predictions.push((arm.name.clone(), cold.predictions.clone()));
    }

    let mut pairwise = serde_json::Map::new();
    for (index, (left_name, left)) in cold_predictions.iter().enumerate() {
        for (right_name, right) in cold_predictions.iter().skip(index + 1) {
            pairwise.insert(
                format!("{left_name} vs {right_name}"),
                serde_json::json!({ "agreement": agreement(left, right) }),
            );
        }
    }
    let llm_only = llm_only_section(&corpus, &gateway, &llm_recovery, repeats).await;
    let report = serde_json::json!({
        "benchmark": "graph_entity_type",
        "corpus_version": corpus.version,
        "entities": corpus.entities.len(),
        "batch_size": corpus.batch_size,
        "repeats": repeats,
        "transport": {
            "http_timeout_s": transport.http_timeout.as_secs(),
            "max_attempts": transport.max_attempts,
        },
        "placeholder_extract_time_type": runtime_node_type_slug(&PLACEHOLDER_TYPE),
        "arms": arm_reports,
        "llm_only": llm_only,
        "cold_pairwise": pairwise,
        "unmeasured": unmeasured,
    });
    let rendered = serde_json::to_string_pretty(&report).expect("render benchmark report");
    std::fs::write(&report_path, rendered).expect("write benchmark report");
}

fn corpus_entity(label: &str, summary: &str, gold: &str) -> CorpusEntity {
    CorpusEntity {
        label: label.to_string(),
        summary: summary.to_string(),
        gold: gold.to_string(),
        accept: Vec::new(),
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

#[test]
fn synthetic_batch_text_joins_labels_and_summaries_neutrally() {
    let batch = vec![
        corpus_entity("Grace Hopper", "US Navy rear admiral.", "person"),
        corpus_entity("Kubernetes", "Container orchestration system.", "artifact"),
    ];
    assert_eq!(
        synthetic_batch_text(&batch),
        "Grace Hopper: US Navy rear admiral. Kubernetes: Container orchestration system."
    );
}

#[test]
fn llm_only_match_folds_labels_and_leaves_uncovered_entities_abstained() {
    let batch = vec![
        corpus_entity("Grace Hopper", "US Navy rear admiral.", "person"),
        corpus_entity("Kubernetes", "Container orchestration system.", "artifact"),
        corpus_entity("eventual consistency", "A consistency model.", "concept"),
    ];
    let extracted = vec![
        candidate("GRACE HOPPER", RuntimeNodeType::Person),
        candidate("kubernetes", RuntimeNodeType::Entity),
    ];
    let matched = match_extracted(&batch, &extracted);
    assert_eq!(matched.covered, 2);
    assert_eq!(matched.entity_fallback, 1);
    assert_eq!(
        matched.predictions,
        vec!["person".to_string(), "entity".to_string(), ABSTAIN.to_string()]
    );
}
