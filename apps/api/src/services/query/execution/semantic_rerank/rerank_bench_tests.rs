//! Live A/B benchmark of the optional semantic-rerank stage (IRG-186).
//!
//! Every provider arm runs the product dispatch
//! `prepare_semantic_rerank_request` then `call_rerank_provider` over the
//! frozen, labelled corpus `apps/api/benchmarks/semantic_rerank/corpus.json`
//! and the run writes one JSON report. Bindings come from the canonical catalog
//! seed. The `off` arm calls no provider and keeps the corpus order, exactly as
//! a turn without rerank does.
//!
//! Quality is measured against the corpus grades: nDCG@5 (graded gain
//! `2^grade - 1`), MRR over the first fragment of the gold document
//! (`grade >= 2`) and P@3 over the same notion of relevance.
//!
//! The run calls paid external APIs, so it is `#[ignore]`d, skipped by
//! `backend-test-lib-ignored`, and started only by
//! `make backend-bench-semantic-rerank` with:
//!
//! - `SEMANTIC_RERANK_BENCH_TYPESAFE_API_KEY`: the TypeSafe SystemOne arm on the
//!   seeded `typesafe` provider and `jev-latest`;
//! - `SEMANTIC_RERANK_BENCH_LITELLM_API_KEY`,
//!   `SEMANTIC_RERANK_BENCH_LITELLM_BASE_URL` and
//!   `SEMANTIC_RERANK_BENCH_LITELLM_ARMS`: chat arms on the seeded `litellm`
//!   profile; each comma-separated arm is `<model>[@<reasoning_effort>]`;
//! - `SEMANTIC_RERANK_BENCH_REPEATS` (default 3): pass 1 is cold, later passes
//!   warm;
//! - `SEMANTIC_RERANK_BENCH_REPORT`: required path the JSON report is written
//!   to.
//!
//! The variables deliberately avoid the `IRONRAG_` prefix: application settings
//! reject provider credentials under it. An arm whose credential is absent is
//! reported as not measured. TypeSafe publishes no per-token rate, so the Jev
//! arm carries the owner's flat estimate from IRG-184 and says so in the
//! report.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::{config::Settings, seeded_provider},
    domains::ai::AiBindingPurpose,
    integrations::{
        llm::{LlmGateway, UnifiedGateway},
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetRegistry, ProviderBudgetRegistryOptions,
        },
    },
    services::{
        bench_tests::{
            Arm, LITELLM_KIND, env_value, latency_summary, litellm_arms, seeded_binding,
            usage_tokens,
        },
        query::support::RerankCandidate,
    },
};

use super::{
    PreparedSemanticRerankRequest, SemanticRerankPolicy, prepare_semantic_rerank_request,
    provider_dispatch::{
        RerankProviderCall, RerankProviderCallRequest, RerankProviderTransport,
        call_rerank_provider,
    },
};

const CORPUS_PATH: &str = "benchmarks/semantic_rerank/corpus.json";
/// Every corpus `source` is relative to the vendored grounded-query corpus.
const SOURCE_ROOT: &str = "apps/api/benchmarks/grounded_query/corpus";
const ENV_PREFIX: &str = "SEMANTIC_RERANK_BENCH";
const TYPESAFE_KIND: &str = "typesafe";
const TYPESAFE_MODEL: &str = "jev-latest";
const OFF_ARM: &str = "off";
/// Owner estimate from IRG-184 (2026-09-27). TypeSafe publishes no per-token
/// rate, so this flat number is the only price the Jev arm can carry, and the
/// report's `rate_source` names it an estimate.
const JEV_ESTIMATED_RATE_USD_PER_MTOKEN: f64 = 0.03;
const MIN_CANDIDATES_PER_QUERY: usize = 10;
const MAX_CANDIDATES_PER_QUERY: usize = 16;
const MAX_GRADE: u8 = 3;
/// Corpus grades at or above this are one document's fragment and count as
/// relevant for MRR and P@3.
const RELEVANT_GRADE: u8 = 2;

#[derive(Debug, Deserialize)]
struct Corpus {
    version: String,
    queries: Vec<CorpusQuery>,
}

#[derive(Debug, Deserialize)]
struct CorpusQuery {
    id: String,
    query: String,
    gold_source: String,
    candidates: Vec<CorpusCandidate>,
}

#[derive(Debug, Deserialize)]
struct CorpusCandidate {
    id: String,
    source: String,
    grade: u8,
    text: String,
}

fn load_corpus() -> Corpus {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS_PATH);
    let text = std::fs::read_to_string(&path).expect("read semantic-rerank benchmark corpus");
    serde_json::from_str(&text).expect("decode semantic-rerank benchmark corpus")
}

fn workspace_root() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(std::path::Path::parent).expect("workspace root").to_path_buf()
}

/// The corpus is part of the benchmark contract: a broken or drifting file must
/// fail on every pipeline, not on the next paid run.
#[test]
fn semantic_rerank_corpus_is_labelled() {
    let corpus = load_corpus();
    let sources = workspace_root().join(SOURCE_ROOT);
    assert!(!corpus.version.is_empty());
    assert!(
        (20..=30).contains(&corpus.queries.len()),
        "corpus carries {} queries",
        corpus.queries.len()
    );
    let mut query_ids = BTreeSet::new();
    for query in &corpus.queries {
        assert!(query_ids.insert(query.id.as_str()), "duplicate query id {:?}", query.id);
        assert!(!query.query.trim().is_empty(), "{:?} has no query text", query.id);
        assert!(sources.join(&query.gold_source).is_file(), "{:?} gold is not vendored", query.id);
        assert_query_candidates(query, &sources);
    }
}

fn assert_query_candidates(query: &CorpusQuery, sources: &Path) {
    let count = query.candidates.len();
    assert!(
        (MIN_CANDIDATES_PER_QUERY..=MAX_CANDIDATES_PER_QUERY).contains(&count),
        "{:?} has {count} candidates",
        query.id
    );
    let mut ids = BTreeSet::new();
    let mut gold = 0;
    for candidate in &query.candidates {
        assert!(ids.insert(candidate.id.as_str()), "{:?} duplicate candidate", candidate.id);
        assert!(!candidate.text.trim().is_empty(), "{:?} has empty text", candidate.id);
        assert!(candidate.grade <= MAX_GRADE, "{:?} grade {}", candidate.id, candidate.grade);
        assert!(
            sources.join(&candidate.source).is_file(),
            "{:?} source not vendored",
            candidate.id
        );
        gold += usize::from(candidate.grade == MAX_GRADE);
    }
    assert!(gold >= 1, "{:?} has no gold candidate", query.id);
}

/// Chat arms plus the always-present `off` arm, and the arms left unmeasured.
fn configured_arms() -> (Vec<Arm>, Vec<String>) {
    let mut arms = vec![off_arm()];
    let mut unmeasured = Vec::new();
    match typesafe_arm() {
        Some(arm) => arms.push(arm),
        None => unmeasured
            .push(format!("{TYPESAFE_KIND}/{TYPESAFE_MODEL}: no {ENV_PREFIX}_TYPESAFE_API_KEY")),
    }
    match litellm_arms(ENV_PREFIX, AiBindingPurpose::QueryRerank) {
        Some(chat_arms) => arms.extend(chat_arms),
        None => unmeasured
            .push(format!("{LITELLM_KIND}: {ENV_PREFIX}_LITELLM_API_KEY/_BASE_URL/_ARMS not set")),
    }
    (arms, unmeasured)
}

fn off_arm() -> Arm {
    Arm {
        name: OFF_ARM.to_string(),
        binding: seeded_binding(
            AiBindingPurpose::QueryRerank,
            LITELLM_KIND,
            "http://127.0.0.1:9/v1",
            String::new(),
            OFF_ARM,
            serde_json::Map::new(),
        ),
        rate: Some((0.0, 0.0)),
        rate_source: "off: no provider call, the corpus order is kept".to_string(),
    }
}

fn typesafe_arm() -> Option<Arm> {
    let api_key = env_value(&format!("{ENV_PREFIX}_TYPESAFE_API_KEY"))?;
    let base_url = seeded_provider(TYPESAFE_KIND)
        .and_then(|seed| seed.default_base_url)
        .expect("typesafe seed declares its base URL");
    Some(Arm {
        name: format!("{TYPESAFE_KIND}/{TYPESAFE_MODEL}"),
        binding: seeded_binding(
            AiBindingPurpose::QueryRerank,
            TYPESAFE_KIND,
            base_url,
            api_key,
            TYPESAFE_MODEL,
            serde_json::Map::new(),
        ),
        rate: Some((JEV_ESTIMATED_RATE_USD_PER_MTOKEN, JEV_ESTIMATED_RATE_USD_PER_MTOKEN)),
        rate_source: format!(
            "estimate: TypeSafe publishes no per-token rate; ≈ ${JEV_ESTIMATED_RATE_USD_PER_MTOKEN} per 1M tokens (owner estimate, IRG-184)"
        ),
    })
}

fn rerank_policy(settings: &Settings) -> SemanticRerankPolicy {
    SemanticRerankPolicy::bounded(
        settings.query_semantic_rerank_timeout_ms,
        settings.query_semantic_rerank_candidate_limit,
        settings.query_semantic_rerank_candidate_text_chars,
        settings.query_semantic_rerank_total_text_chars,
    )
}

fn candidates_of(query: &CorpusQuery) -> Vec<RerankCandidate> {
    query
        .candidates
        .iter()
        .map(|candidate| RerankCandidate {
            id: candidate.id.clone(),
            text: candidate.text.clone(),
            score: None,
        })
        .collect()
}

fn identity_ranking(query: &CorpusQuery) -> Vec<String> {
    query.candidates.iter().map(|candidate| candidate.id.clone()).collect()
}

struct Bench {
    gateway: Arc<dyn LlmGateway>,
    budget: Arc<ProviderBudgetRegistry>,
    transport: RerankProviderTransport,
    policy: SemanticRerankPolicy,
}

#[derive(Default)]
struct QueryRun {
    ranking: Vec<String>,
    latency_ms: u128,
    failure: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Default)]
struct Pass {
    rankings: Vec<Vec<String>>,
    latencies_ms: Vec<u128>,
    failures: BTreeMap<String, usize>,
    input_tokens: u64,
    output_tokens: u64,
    over_turn_wait: usize,
}

async fn run_provider_pass(arm: &Arm, corpus: &Corpus, bench: &Bench) -> Pass {
    let mut pass = Pass::default();
    for query in &corpus.queries {
        let run = run_query(arm, query, bench).await;
        pass.latencies_ms.push(run.latency_ms);
        pass.input_tokens += run.input_tokens;
        pass.output_tokens += run.output_tokens;
        if run.latency_ms > bench.policy.turn_wait.as_millis() {
            pass.over_turn_wait += 1;
        }
        if let Some(failure) = run.failure {
            *pass.failures.entry(failure).or_default() += 1;
        }
        pass.rankings.push(run.ranking);
    }
    pass
}

async fn run_query(arm: &Arm, query: &CorpusQuery, bench: &Bench) -> QueryRun {
    let fallback = identity_ranking(query);
    let candidates = candidates_of(query);
    let Some(prepared) =
        prepare_semantic_rerank_request(&query.query, &[], &[], &candidates, bench.policy)
    else {
        return QueryRun {
            ranking: fallback,
            failure: Some("prepare_none".to_string()),
            ..QueryRun::default()
        };
    };
    let started = Instant::now();
    let call = call_rerank_provider(RerankProviderCallRequest {
        llm_gateway: &bench.gateway,
        provider_budget: &bench.budget,
        transport: bench.transport,
        binding: &arm.binding,
        prepared: &prepared,
        provider_call_id: Uuid::now_v7(),
        provider_timeout: bench.policy.timeout,
    })
    .await;
    let latency_ms = started.elapsed().as_millis();
    match call {
        Ok(call) => finish_call(call, &prepared, fallback, latency_ms),
        Err(failure) => QueryRun {
            ranking: fallback,
            latency_ms,
            failure: Some(format!("{failure:?}")),
            ..QueryRun::default()
        },
    }
}

fn finish_call(
    call: RerankProviderCall,
    prepared: &PreparedSemanticRerankRequest,
    fallback: Vec<String>,
    latency_ms: u128,
) -> QueryRun {
    let (input_tokens, output_tokens) = usage_tokens(call.usage_json());
    let mut run = QueryRun { latency_ms, input_tokens, output_tokens, ..QueryRun::default() };
    match call.into_ranking() {
        Ok(ranking) => run.ranking = prepared.map_ranking_to_candidate_order(&ranking).chunks,
        Err(_) => {
            run.ranking = fallback;
            run.failure = Some("invalid_response".to_string());
        }
    }
    run
}

fn run_off_pass(corpus: &Corpus) -> Pass {
    Pass { rankings: corpus.queries.iter().map(identity_ranking).collect(), ..Pass::default() }
}

/// Grades of the ranking, in rank order; an unknown id scores zero.
fn graded_order(query: &CorpusQuery, ranking: &[String]) -> Vec<u8> {
    let grades: BTreeMap<&str, u8> =
        query.candidates.iter().map(|candidate| (candidate.id.as_str(), candidate.grade)).collect();
    ranking.iter().map(|id| grades.get(id.as_str()).copied().unwrap_or(0)).collect()
}

fn discounted_gain(grades: &[u8]) -> f64 {
    grades
        .iter()
        .enumerate()
        .map(|(position, grade)| {
            (2f64.powi(i32::from(*grade)) - 1.0) / ((position as f64) + 2.0).log2()
        })
        .sum()
}

fn ndcg_at_five(grades: &[u8]) -> f64 {
    let actual: Vec<u8> = grades.iter().copied().take(5).collect();
    let mut ideal = grades.to_vec();
    ideal.sort_unstable_by(|left, right| right.cmp(left));
    let ideal: Vec<u8> = ideal.into_iter().take(5).collect();
    let ideal_gain = discounted_gain(&ideal);
    if ideal_gain == 0.0 { 0.0 } else { discounted_gain(&actual) / ideal_gain }
}

fn reciprocal_rank(grades: &[u8]) -> f64 {
    grades
        .iter()
        .position(|grade| *grade >= RELEVANT_GRADE)
        .map_or(0.0, |position| 1.0 / (position as f64 + 1.0))
}

fn precision_at_three(grades: &[u8]) -> f64 {
    let hits = grades.iter().take(3).filter(|grade| **grade >= RELEVANT_GRADE).count();
    hits as f64 / 3.0
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn pass_report(arm: &Arm, corpus: &Corpus, pass: &Pass) -> serde_json::Value {
    let mut ndcg = Vec::with_capacity(corpus.queries.len());
    let mut mrr = Vec::with_capacity(corpus.queries.len());
    let mut precision = Vec::with_capacity(corpus.queries.len());
    for (query, ranking) in corpus.queries.iter().zip(&pass.rankings) {
        let grades = graded_order(query, ranking);
        ndcg.push(ndcg_at_five(&grades));
        mrr.push(reciprocal_rank(&grades));
        precision.push(precision_at_three(&grades));
    }
    serde_json::json!({
        "queries": corpus.queries.len(),
        "ndcg_at_5": mean(&ndcg),
        "mrr": mean(&mrr),
        "precision_at_3": mean(&precision),
        "latency_ms": latency_summary(&pass.latencies_ms),
        "over_turn_wait": pass.over_turn_wait,
        "failures": pass.failures,
        "input_tokens": pass.input_tokens,
        "output_tokens": pass.output_tokens,
        "cost_usd": arm.cost_usd(pass.input_tokens, pass.output_tokens),
    })
}

#[tokio::test]
#[ignore = "calls paid external providers; run through `make backend-bench-semantic-rerank`"]
async fn semantic_rerank_model_benchmark() {
    let corpus = load_corpus();
    let report_path = env_value(&format!("{ENV_PREFIX}_REPORT"))
        .expect("SEMANTIC_RERANK_BENCH_REPORT names the report file");
    let (arms, unmeasured) = configured_arms();
    assert!(
        arms.iter().any(|arm| arm.name != OFF_ARM),
        "no provider arm is configured: {unmeasured:?}"
    );
    let repeats = env_value(&format!("{ENV_PREFIX}_REPEATS"))
        .map(|value| value.parse::<usize>().expect("SEMANTIC_RERANK_BENCH_REPEATS is a count"))
        .unwrap_or(3)
        .max(1);
    let settings = Settings::from_env().expect("settings defaults load");
    let policy = rerank_policy(&settings);
    let bench = Bench {
        gateway: Arc::new(UnifiedGateway::from_settings(&settings).expect("build chat gateway")),
        budget: Arc::new(
            ProviderBudgetRegistry::uniform(
                ProviderBudgetConfig::unlimited(),
                ProviderBudgetRegistryOptions::default(),
            )
            .expect("build unlimited provider budget registry"),
        ),
        transport: RerankProviderTransport::from_settings(&settings),
        policy,
    };

    let mut arm_reports = serde_json::Map::new();
    for arm in &arms {
        let mut passes = Vec::with_capacity(repeats);
        for _ in 0..repeats {
            if arm.name == OFF_ARM {
                passes.push(run_off_pass(&corpus));
            } else {
                passes.push(run_provider_pass(arm, &corpus, &bench).await);
            }
        }
        arm_reports.insert(
            arm.name.clone(),
            serde_json::json!({
                "model": arm.binding.model_name,
                "rate_source": arm.rate_source,
                "passes": passes.iter().map(|pass| pass_report(arm, &corpus, pass)).collect::<Vec<_>>(),
            }),
        );
    }
    let report = serde_json::json!({
        "benchmark": "semantic_rerank",
        "corpus_version": corpus.version,
        "queries": corpus.queries.len(),
        "repeats": repeats,
        "policy": {
            "provider_timeout_ms": policy.timeout.as_millis(),
            "turn_wait_ms": policy.turn_wait.as_millis(),
            "candidate_limit": policy.candidate_limit,
            "candidate_text_chars": policy.candidate_text_chars,
            "total_text_chars": policy.total_text_chars,
        },
        "transport": {
            "http_timeout_s": bench.transport.http_timeout.as_secs(),
            "max_attempts": bench.transport.max_attempts,
        },
        "arms": arm_reports,
        "unmeasured": unmeasured,
    });
    let rendered = serde_json::to_string_pretty(&report).expect("render benchmark report");
    std::fs::write(&report_path, rendered).expect("write benchmark report");
}
