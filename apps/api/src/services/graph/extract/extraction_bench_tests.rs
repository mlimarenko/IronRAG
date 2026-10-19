//! Live benchmark of graph extraction across chat models (IRG-207).
//!
//! Every arm runs the product path `resolve_graph_extraction_with_gateway`,
//! with its recovery settings, over the frozen public corpus
//! `apps/api/benchmarks/graph_extraction/corpus.json` and writes one JSON
//! report. Bindings come from the canonical catalog seed.
//!
//! There is no labelled graph, so quality is reported as agreement with a
//! reference arm, never as accuracy: entity recall and relation-pair recall of
//! each arm against the reference arm's graph on the same chunk.
//!
//! The run calls paid external APIs, so it is `#[ignore]`d, skipped by
//! `backend-test-lib-ignored`, and started only by
//! `make backend-bench-graph-extraction` with:
//!
//! - `GRAPH_EXTRACTION_BENCH_LITELLM_API_KEY`, `GRAPH_EXTRACTION_BENCH_LITELLM_BASE_URL`
//!   and `GRAPH_EXTRACTION_BENCH_LITELLM_ARMS` (`<model>` or
//!   `<model>@<reasoning_effort>`, comma-separated); the first arm is the
//!   agreement reference;
//! - `GRAPH_EXTRACTION_BENCH_REPEATS` (default 1);
//! - `GRAPH_EXTRACTION_BENCH_REPORT`: required path of the JSON report.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{
    app::config::Settings,
    domains::ai::AiBindingPurpose,
    integrations::llm::{LlmGateway, UnifiedGateway},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ingest::extraction_recovery::ExtractionRecoveryService,
        ops::provider_failure::ProviderFailureClassificationService,
    },
};

use crate::services::bench_tests::{
    Arm, env_value, graph_extraction_request, latency_summary, litellm_arms, usage_tokens,
};

use super::{
    prompt::GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS,
    session::{GraphExtractionLoopRequest, resolve_graph_extraction_with_gateway},
    types::{GraphExtractionCandidateSet, GraphExtractionFailureOutcome, GraphExtractionRequest},
};

const CORPUS_PATH: &str = "benchmarks/graph_extraction/corpus.json";
const ENV_PREFIX: &str = "GRAPH_EXTRACTION_BENCH";

#[derive(Debug, Deserialize)]
struct Corpus {
    version: String,
    chunks: Vec<CorpusChunk>,
}

#[derive(Debug, Deserialize)]
struct CorpusChunk {
    source: String,
    title: String,
    text: String,
}

fn load_corpus() -> Corpus {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS_PATH);
    let text = std::fs::read_to_string(&path).expect("read graph-extraction benchmark corpus");
    serde_json::from_str(&text).expect("decode graph-extraction benchmark corpus")
}

/// The corpus is part of the benchmark contract and is checked on every
/// pipeline, not only on the next paid run.
#[test]
fn extraction_corpus_is_well_formed() {
    let corpus = load_corpus();
    assert!(!corpus.version.is_empty());
    assert!(!corpus.chunks.is_empty());
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().and_then(std::path::Path::parent).expect("workspace root");
    let mut titles = BTreeSet::new();
    for chunk in &corpus.chunks {
        assert!(titles.insert(chunk.title.as_str()), "duplicate title {:?}", chunk.title);
        assert!(chunk.text.len() >= 500, "{:?} is too short to extract a graph", chunk.title);
        assert!(workspace.join(&chunk.source).is_file(), "{} is not vendored", chunk.source);
    }
}

/// Entity labels and undirected relation endpoint pairs, case-folded, so two
/// graphs can be compared independently of label casing and edge direction.
#[derive(Debug, Default, Clone)]
struct GraphShape {
    entities: BTreeSet<String>,
    relations: BTreeSet<(String, String)>,
}

impl GraphShape {
    fn of(set: &GraphExtractionCandidateSet) -> Self {
        let fold = |label: &str| label.trim().to_lowercase();
        Self {
            entities: set.entities.iter().map(|entity| fold(&entity.label)).collect(),
            relations: set
                .relations
                .iter()
                .map(|relation| {
                    let (a, b) = (fold(&relation.source_label), fold(&relation.target_label));
                    if a <= b { (a, b) } else { (b, a) }
                })
                .collect(),
        }
    }
}

fn recall<T: Ord>(candidate: &BTreeSet<T>, reference: &BTreeSet<T>) -> Option<f64> {
    (!reference.is_empty())
        .then(|| candidate.intersection(reference).count() as f64 / reference.len() as f64)
}

/// One pass of one arm over the whole corpus.
#[derive(Default)]
struct Pass {
    /// `None` for a chunk whose extraction failed.
    graphs: Vec<Option<GraphShape>>,
    latencies_ms: Vec<u128>,
    failures: BTreeMap<String, usize>,
    provider_attempts: usize,
    input_tokens: u64,
    output_tokens: u64,
}

struct Recovery {
    enabled: bool,
    max_attempts: usize,
}

/// The result of one product extraction call: everything both benchmarks read
/// from a resolved extraction, or the failure with zeroed counters.
pub(crate) struct ExtractionRun {
    pub(crate) normalized: GraphExtractionCandidateSet,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) provider_attempts: usize,
    pub(crate) elapsed_ms: u128,
    pub(crate) failure: Option<GraphExtractionFailureOutcome>,
}

/// One product extraction call with the benchmark's recovery services wired and
/// its latency and usage accounted. Shared by the graph-extraction benchmark
/// (IRG-207) and the entity-typing benchmark's LLM-only arm (IRG-184), so both
/// invoke the product path the same way. A failure is returned whole; each
/// benchmark classifies it its own way.
pub(crate) async fn run_graph_extraction(
    gateway: &dyn LlmGateway,
    binding: &ResolvedRuntimeBinding,
    request: &GraphExtractionRequest,
    classification: &ProviderFailureClassificationService,
    recovery_enabled: bool,
    max_attempts: usize,
) -> ExtractionRun {
    let started = Instant::now();
    let outcome = resolve_graph_extraction_with_gateway(
        gateway,
        GraphExtractionLoopRequest {
            extraction_recovery: &ExtractionRecoveryService,
            provider_failure_classification: classification,
            runtime_binding: binding,
            request,
            recovery_enabled,
            max_provider_attempts: max_attempts,
            provider_timeout_retry_limit: 1,
        },
        &CancellationToken::new(),
    )
    .await;
    let elapsed_ms = started.elapsed().as_millis();
    match outcome {
        Ok(resolved) => {
            let (input_tokens, output_tokens) = usage_tokens(&resolved.usage_json);
            ExtractionRun {
                normalized: resolved.normalized,
                input_tokens,
                output_tokens,
                provider_attempts: resolved.recovery.provider_attempt_count,
                elapsed_ms,
                failure: None,
            }
        }
        Err(failure) => ExtractionRun {
            normalized: GraphExtractionCandidateSet::default(),
            input_tokens: 0,
            output_tokens: 0,
            provider_attempts: 0,
            elapsed_ms,
            failure: Some(failure),
        },
    }
}

async fn run_pass(
    arm: &Arm,
    corpus: &Corpus,
    gateway: &Arc<dyn LlmGateway>,
    recovery: &Recovery,
) -> Pass {
    let mut pass = Pass::default();
    let classification = ProviderFailureClassificationService::default();
    for (ordinal, chunk) in corpus.chunks.iter().enumerate() {
        let request = graph_extraction_request(
            &chunk.source,
            &chunk.title,
            &chunk.text,
            i32::try_from(ordinal).unwrap_or(i32::MAX),
        );
        let run = run_graph_extraction(
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
        pass.provider_attempts += run.provider_attempts;
        match run.failure {
            None => pass.graphs.push(Some(GraphShape::of(&run.normalized))),
            Some(failure) => {
                let class = failure.provider_failure.as_ref().map_or_else(
                    || "extraction_failed".to_string(),
                    |detail| format!("{:?}", detail.failure_class),
                );
                *pass.failures.entry(class).or_default() += 1;
                pass.graphs.push(None);
            }
        }
    }
    pass
}

fn pass_report(arm: &Arm, pass: &Pass, reference: Option<&Pass>) -> serde_json::Value {
    let extracted = pass.graphs.iter().flatten().collect::<Vec<_>>();
    let (mut entity_recall, mut relation_recall) = (Vec::new(), Vec::new());
    if let Some(reference) = reference {
        for (graph, reference_graph) in pass.graphs.iter().zip(&reference.graphs) {
            if let (Some(graph), Some(reference_graph)) = (graph, reference_graph) {
                entity_recall.extend(recall(&graph.entities, &reference_graph.entities));
                relation_recall.extend(recall(&graph.relations, &reference_graph.relations));
            }
        }
    }
    let mean = |values: &[f64]| {
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    };
    serde_json::json!({
        "chunks": pass.graphs.len(),
        "extracted": extracted.len(),
        "failures": pass.failures,
        "provider_attempts": pass.provider_attempts,
        "entities": extracted.iter().map(|graph| graph.entities.len()).sum::<usize>(),
        "relations": extracted.iter().map(|graph| graph.relations.len()).sum::<usize>(),
        "entity_recall_vs_reference": mean(&entity_recall),
        "relation_recall_vs_reference": mean(&relation_recall),
        "latency_ms_per_chunk": latency_summary(&pass.latencies_ms),
        "input_tokens": pass.input_tokens,
        "output_tokens": pass.output_tokens,
        "cost_usd": arm.cost_usd(pass.input_tokens, pass.output_tokens),
    })
}

#[tokio::test]
#[ignore = "calls paid external providers; run through `make backend-bench-graph-extraction`"]
async fn graph_extraction_model_benchmark() {
    let corpus = load_corpus();
    let report_path = env_value(&format!("{ENV_PREFIX}_REPORT"))
        .expect("GRAPH_EXTRACTION_BENCH_REPORT names the report file");
    let arms = litellm_arms(ENV_PREFIX, AiBindingPurpose::ExtractGraph)
        .filter(|arms| !arms.is_empty())
        .expect("GRAPH_EXTRACTION_BENCH_LITELLM_* configure at least one arm");
    let repeats = env_value(&format!("{ENV_PREFIX}_REPEATS"))
        .map(|value| value.parse::<usize>().expect("GRAPH_EXTRACTION_BENCH_REPEATS is a count"))
        .unwrap_or(1)
        .max(1);
    let settings = Settings::from_env().expect("settings defaults load");
    let recovery = Recovery {
        enabled: settings.runtime_graph_extract_recovery_enabled,
        max_attempts: settings
            .runtime_graph_extract_recovery_max_attempts
            .clamp(1, GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS),
    };
    let gateway: Arc<dyn LlmGateway> =
        Arc::new(UnifiedGateway::from_settings(&settings).expect("build chat gateway"));

    // Arms run concurrently, chunks within an arm sequentially, so each arm's
    // latency is its own provider's and the wall time stays bounded.
    let passes_per_arm = futures::future::join_all(arms.iter().map(|arm| {
        let gateway = Arc::clone(&gateway);
        let corpus = &corpus;
        let recovery = &recovery;
        async move {
            let mut passes = Vec::with_capacity(repeats);
            for _ in 0..repeats {
                passes.push(run_pass(arm, corpus, &gateway, recovery).await);
            }
            passes
        }
    }))
    .await;

    let reference = passes_per_arm.first().and_then(|passes| passes.first());
    let mut arm_reports = serde_json::Map::new();
    for (arm, passes) in arms.iter().zip(&passes_per_arm) {
        arm_reports.insert(
            arm.name.clone(),
            serde_json::json!({
                "model": arm.binding.model_name,
                "rate_source": arm.rate_source,
                "passes": passes
                    .iter()
                    .map(|pass| pass_report(arm, pass, reference))
                    .collect::<Vec<_>>(),
            }),
        );
    }
    let report = serde_json::json!({
        "benchmark": "graph_extraction",
        "corpus_version": corpus.version,
        "chunks": corpus.chunks.len(),
        "chunk_chars": corpus.chunks.iter().map(|chunk| chunk.text.len()).sum::<usize>(),
        "repeats": repeats,
        "recovery": {"enabled": recovery.enabled, "max_attempts": recovery.max_attempts},
        "reference_arm": arms.first().map(|arm| arm.name.clone()),
        "arms": arm_reports,
    });
    let rendered = serde_json::to_string_pretty(&report).expect("render benchmark report");
    std::fs::write(&report_path, rendered).expect("write benchmark report");
}
