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

/// Offline interchange stays beside the live benchmark so native preparation,
/// locators, bounded decision requests and quality metrics have one owner.
mod comparison {
    use super::*;
    use anyhow::{Context, Result, ensure};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::io::Write;

    const INPUT_PROTOCOL: &str = "semantic-rerank-comparison-input/1";
    const SCORE_PROTOCOL: &str = "semantic-rerank-comparison-scores/1";
    const CORPUS_SHA: &str = "488c687de57ee97a23caf6bc4916b91f851e3758c360fbcd2f0281cb3dd1edee";

    fn hash(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn required(name: &str) -> String {
        env_value(&format!("SEMANTIC_RERANK_COMPARISON_{name}"))
            .unwrap_or_else(|| panic!("SEMANTIC_RERANK_COMPARISON_{name} is required"))
    }

    fn create_json(path: &str, value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        writeln!(
            std::io::stdout().lock(),
            "artifact={path} bytes={} sha256={}",
            bytes.len(),
            hash(&bytes)
        )?;
        Ok(())
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Variants {
        version: String,
        corpus_sha256: String,
        variants: Vec<Variant>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Variant {
        id: String,
        parent_query_id: String,
        language: String,
        query: String,
    }

    fn export_query(parent: &CorpusQuery, id: &str, language: &str, query: &str) -> Result<Value> {
        // The comparison freezes the documented default policy, not mutable env settings.
        let policy = SemanticRerankPolicy::bounded(1_000, 16, 1_200, 18_000);
        let prepared =
            prepare_semantic_rerank_request(query, &[], &[], &candidates_of(parent), policy)
                .context("native preparation rejected query")?;
        let binding = seeded_binding(
            AiBindingPurpose::QueryRerank,
            TYPESAFE_KIND,
            seeded_provider(TYPESAFE_KIND)
                .and_then(|seed| seed.default_base_url)
                .context("missing seeded TypeSafe URL")?,
            String::new(),
            TYPESAFE_MODEL,
            serde_json::Map::new(),
        );
        let levels = binding.decision_score_criteria().context("missing seeded rubric")?;
        let (decision, count) = super::super::provider_dispatch::build_systemone_decision_request(
            &binding, &prepared, &levels,
        )
        .context("bounded SystemOne request rejected")?;
        ensure!(
            count == prepared.prepared_candidate_count(),
            "SystemOne drops tails: refusing unequal candidate sets for {id}"
        );
        let state: Value = serde_json::from_str(&decision.state)?;
        ensure!(state == prepared.provider_payload_json(), "SystemOne/chat input mismatch");
        let mut grades = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut changed = Vec::new();
        let mut candidates = Vec::new();
        for candidate in &prepared.provider_candidates {
            let locator =
                prepared.locators.get(candidate.index).context("missing native locator")?;
            let original = parent
                .candidates
                .iter()
                .find(|c| c.id == locator.id)
                .context("native locator missing from corpus")?;
            grades.insert(locator.id.clone(), original.grade);
            sources.insert(locator.id.clone(), original.source.clone());
            if original.text != candidate.text {
                changed.push(locator.id.clone());
            }
            candidates.push(json!({"id":locator.id,"index":candidate.index,"text":candidate.text}));
        }
        Ok(json!({
            "id":id,"parentQueryId":parent.id,"language":language,"query":prepared.question,
            "candidates":candidates,
            "preparation": {"originalCandidateCount":parent.candidates.len(),
                "effectiveCandidateCount":count,"changedCandidateIds":changed,
                "droppedCandidateIds":parent.candidates.iter().filter(|c| !grades.contains_key(&c.id))
                    .map(|c| &c.id).collect::<Vec<_>>(),
                "queryChanged":query != prepared.question,
                "chatUserMessageBytes":prepared.user_message_bytes(),
                "systemOneSerializedBytes":serde_json::to_vec(&decision)?.len()},
            "evaluation":{"grades":grades,"goldSource":parent.gold_source,"sources":sources,
                "gradeProvenance":"weak source/English term-overlap proxy; not human judged",
                "grade3InheritedFromEnglishOverlapProxy":language == "ru"}
        }))
    }

    fn build_export(source_sha: &str, variants_path: Option<&str>) -> Result<Value> {
        ensure!(
            source_sha.len() == 40 && source_sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "source SHA must be full 40 hexadecimal characters"
        );
        let root = workspace_root();
        let corpus_bytes = std::fs::read(root.join("apps/api").join(CORPUS_PATH))?;
        ensure!(hash(&corpus_bytes) == CORPUS_SHA, "original corpus hash mismatch");
        let corpus: Corpus = serde_json::from_slice(&corpus_bytes)?;
        let mut queries = Vec::new();
        let mut ids = BTreeSet::new();
        for parent in &corpus.queries {
            ensure!(ids.insert(parent.id.clone()), "duplicate corpus query");
            queries.push(export_query(parent, &parent.id, "en", &parent.query)?);
        }
        let mut variants_hash = None;
        if let Some(path) = variants_path {
            let bytes = std::fs::read(path)?;
            variants_hash = Some(hash(&bytes));
            let variants: Variants = serde_json::from_slice(&bytes)?;
            ensure!(
                !variants.version.trim().is_empty() && variants.corpus_sha256 == CORPUS_SHA,
                "variant version/corpus mismatch"
            );
            for variant in variants.variants {
                ensure!(
                    !variant.id.trim().is_empty() && ids.insert(variant.id.clone()),
                    "duplicate/empty variant ID"
                );
                ensure!(
                    variant.language == "ru" && !variant.query.trim().is_empty(),
                    "invalid Russian variant"
                );
                let parent = corpus
                    .queries
                    .iter()
                    .find(|q| q.id == variant.parent_query_id)
                    .context("unknown variant parent")?;
                queries.push(export_query(parent, &variant.id, "ru", &variant.query)?);
            }
        }
        let mut source_hashes = BTreeMap::new();
        for path in [
            "apps/api/src/services/query/execution/semantic_rerank.rs",
            "apps/api/src/services/query/execution/semantic_rerank/preparation.rs",
            "apps/api/src/services/query/execution/semantic_rerank/provider_dispatch.rs",
            "apps/api/src/services/query/execution/semantic_rerank/rerank_bench_tests.rs",
            "apps/api/src/services/bench_tests.rs",
        ] {
            source_hashes.insert(path.to_string(), hash(&std::fs::read(root.join(path))?));
        }
        let mut corpus_source_hashes = BTreeMap::new();
        for candidate in corpus.queries.iter().flat_map(|q| &q.candidates) {
            if !corpus_source_hashes.contains_key(&candidate.source) {
                corpus_source_hashes.insert(
                    candidate.source.clone(),
                    hash(&std::fs::read(root.join(SOURCE_ROOT).join(&candidate.source))?),
                );
            }
        }
        Ok(json!({"protocol":INPUT_PROTOCOL,"corpusSha256":CORPUS_SHA,"sourceSha":source_sha,
            "sourceIdentity":"base commit only; sourceFileSha256 identifies dirty source bytes",
            "sourceFileSha256":source_hashes,"corpusSourceSha256":corpus_source_hashes,
            "variantsSha256":variants_hash,
            "policy":{"timeoutMs":1000,"candidateLimit":16,"candidateTextChars":1200,"totalTextChars":18000,
                "order":"frozen corpus shuffle; NOT production retrieval order",
                "unequalSystemOneCandidates":"fail closed"},
            "inputSha256":hash(&serde_json::to_vec(&queries)?),"queries":queries}))
    }

    #[test]
    #[ignore = "offline export writes an explicitly named create-new artifact"]
    fn semantic_rerank_comparison_export() -> Result<()> {
        let source_sha = env_value("SEMANTIC_RERANK_COMPARISON_SOURCE_SHA")
            .or_else(|| env_value("IRONRAG_VERSION"))
            .context("explicit source SHA required")?;
        let variants = env_value("SEMANTIC_RERANK_COMPARISON_VARIANTS");
        create_json(&required("EXPORT"), &build_export(&source_sha, variants.as_deref())?)
    }

    fn rows(value: &Value) -> Result<&Vec<Value>> {
        value["queries"].as_array().context("queries must be an array")
    }

    fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
        value[key].as_str().with_context(|| format!("missing string {key}"))
    }

    fn validate_input(bytes: &[u8]) -> Result<Value> {
        let input: Value = serde_json::from_slice(bytes)?;
        ensure!(text(&input, "protocol")? == INPUT_PROTOCOL, "input protocol mismatch");
        ensure!(
            text(&input, "inputSha256")? == hash(&serde_json::to_vec(rows(&input)?)?),
            "input hash mismatch"
        );
        let mut ids = BTreeSet::new();
        ensure!(!rows(&input)?.is_empty(), "empty input");
        for query in rows(&input)? {
            ensure!(ids.insert(text(query, "id")?), "duplicate input query");
            ensure!(!text(query, "query")?.trim().is_empty(), "empty query");
            ensure!(["en", "ru"].contains(&text(query, "language")?), "unknown language");
            let candidates = query["candidates"].as_array().context("missing candidates")?;
            ensure!(!candidates.is_empty(), "empty candidates");
            let mut candidate_ids = BTreeSet::new();
            for (index, candidate) in candidates.iter().enumerate() {
                let id = text(candidate, "id")?;
                ensure!(candidate_ids.insert(id), "duplicate input candidate");
                ensure!(candidate["index"].as_u64() == Some(index as u64), "invalid opaque index");
                ensure!(!text(candidate, "text")?.is_empty(), "empty candidate text");
                ensure!(
                    query["evaluation"]["grades"][id]
                        .as_u64()
                        .is_some_and(|g| g <= u64::from(MAX_GRADE)),
                    "invalid grade"
                );
            }
            ensure!(
                query["evaluation"]["grades"].as_object().context("missing grades")?.len()
                    == candidates.len(),
                "grade coverage mismatch"
            );
        }
        Ok(input)
    }

    fn ranking(query: &Value, row: &Value) -> Result<Vec<usize>> {
        let candidates = query["candidates"].as_array().context("missing candidates")?;
        let scores = row["scores"].as_array().context("missing scores")?;
        ensure!(scores.len() == candidates.len(), "partial score coverage");
        let mut values = BTreeMap::new();
        for score in scores {
            let id = text(score, "id")?;
            let value = score["score"].as_f64().context("score must be numeric")?;
            ensure!(value.is_finite(), "nonfinite score");
            ensure!(values.insert(id, value).is_none(), "duplicate score ID");
        }
        let mut ordered = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            ordered.push((
                index,
                *values.get(text(candidate, "id")?).context("missing/unknown score ID")?,
            ));
        }
        ordered.sort_by(|a, b| {
            // IEEE signed zeroes are equal scores, not a ranking signal.
            if a.1 == b.1 { a.0.cmp(&b.0) } else { b.1.total_cmp(&a.1) }
        });
        Ok(ordered.into_iter().map(|(i, _)| i).collect())
    }

    fn metrics(query: &Value, order: &[usize]) -> Result<[f64; 3]> {
        let mut grades = Vec::new();
        for index in order {
            let id = text(&query["candidates"][*index], "id")?;
            grades.push(query["evaluation"]["grades"][id].as_u64().context("missing grade")? as u8);
        }
        Ok([ndcg_at_five(&grades), reciprocal_rank(&grades), precision_at_three(&grades)])
    }

    fn quantile(values: &[f64], percent: usize) -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        Some(sorted[(percent * sorted.len()).div_ceil(100).saturating_sub(1)])
    }

    fn replay(bytes: &[u8], scores: &Value) -> Result<Value> {
        let input = validate_input(bytes)?;
        ensure!(text(scores, "protocol")? == SCORE_PROTOCOL, "score protocol mismatch");
        ensure!(
            text(scores, "inputSha256")? == text(&input, "inputSha256")?,
            "input identity mismatch"
        );
        ensure!(text(scores, "exportedInputFileSha256")? == hash(bytes), "export bytes mismatch");
        ensure!(
            !text(scores, "arm")?.trim().is_empty() && scores["modelIdentity"].is_object(),
            "missing arm/model identity"
        );
        ensure!(rows(&input)?.len() == rows(scores)?.len(), "query count mismatch");
        let mut results = Vec::new();
        let mut latencies = Vec::new();
        for (query, row) in rows(&input)?.iter().zip(rows(scores)?) {
            ensure!(text(query, "id")? == text(row, "id")?, "query order/ID mismatch");
            match text(row, "status")? {
                "ok" => {
                    let order = ranking(query, row)?;
                    let latency = row["latencyMs"].as_f64().context("missing latency")?;
                    ensure!(latency.is_finite() && latency >= 0.0, "invalid latency");
                    latencies.push(latency);
                    let measured = metrics(query, &order)?;
                    let baseline = metrics(query, &(0..order.len()).collect::<Vec<_>>())?;
                    let delta: Vec<f64> =
                        measured.iter().zip(baseline).map(|(a, b)| a - b).collect();
                    results.push(json!({"id":query["id"],"language":query["language"],"status":"ok",
                        "metrics":measured,"off":baseline,"delta":delta,"latencyMs":latency,
                        "measurement":row,
                        "ranking":order.iter().map(|i| &query["candidates"][*i]["id"]).collect::<Vec<_>>()}));
                }
                "failed" => {
                    ensure!(row["failure"].is_object(), "failed row requires failure object");
                    ensure!(
                        !text(&row["failure"], "type")?.trim().is_empty(),
                        "failed row requires failure type"
                    );
                    // Some exceptions have an empty string representation.
                    text(&row["failure"], "message")?;
                    results.push(json!({"id":query["id"],"language":query["language"],"status":"failed","failure":row}));
                }
                "not_run" => {
                    results.push(json!({"id":query["id"],"language":query["language"],"status":"not_run","failure":row}));
                }
                _ => anyhow::bail!("unknown query status"),
            }
        }
        let mut groups = BTreeMap::new();
        for language in ["all", "en", "ru"] {
            let group: Vec<_> =
                results.iter().filter(|q| language == "all" || q["language"] == language).collect();
            let completed: Vec<_> = group.iter().filter(|q| q["status"] == "ok").collect();
            let average = |field: &str| -> Vec<Option<f64>> {
                (0..3)
                    .map(|i| {
                        mean(
                            &completed
                                .iter()
                                .filter_map(|q| q[field][i].as_f64())
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect()
            };
            groups.insert(
                language,
                json!({"total":group.len(),"successes":completed.len(),
                "failures":group.len()-completed.len(),"commonCompletedDenominator":completed.len(),
                "failed":group.iter().filter(|q| q["status"] == "failed").count(),
                "notRun":group.iter().filter(|q| q["status"] == "not_run").count(),
                "metrics":average("metrics"),"off":average("off"),"delta":average("delta")}),
            );
        }
        let mut producer_metadata = scores.as_object().context("scores must be an object")?.clone();
        producer_metadata.remove("queries");
        Ok(
            json!({"protocol":"semantic-rerank-comparison-report/1","inputSha256":input["inputSha256"],
            "exportedInputFileSha256":hash(bytes),"arm":scores["arm"],"modelIdentity":scores["modelIdentity"],
            "metricOrder":["ndcgAt5","mrr","precisionAt3"],"relevantGradeMinimum":RELEVANT_GRADE,
            "gain":"2^grade-1","groups":groups,"queries":results,
            "producerMetadata":producer_metadata,
            "modelLoadMs":scores["modelLoadMs"],"peakRssBytes":scores["peakRssBytes"],
            "documentPrecomputation":scores["documentPrecomputation"],"failure":scores["failure"],
            "latency":{"scope":"score artifact latencyMs; successful cases only; encoding/cache setup excluded unless producer explicitly includes it",
                "producerScope":scores["latencyScope"],"p50Ms":quantile(&latencies,50),"p95Ms":quantile(&latencies,95),
                "quantile":"nearest rank: sorted[ceil(p*n)-1]","count":latencies.len(),"offMs":Value::Null},
            "cost":{"providerUsd":0,"basis":"local arm: no provider calls","hardwareCost":"unmeasured; compute is not free"}}),
        )
    }

    #[test]
    #[ignore = "offline replay reads explicit input/scores and writes a create-new report"]
    fn semantic_rerank_comparison_replay() -> Result<()> {
        let bytes = std::fs::read(required("INPUT"))?;
        let scores = serde_json::from_slice(&std::fs::read(required("SCORES"))?)?;
        create_json(&required("REPORT"), &replay(&bytes, &scores)?)
    }

    fn fixture() -> (Vec<u8>, Value) {
        let queries = json!([{"id":"q","language":"en","query":"test",
            "candidates":[{"id":"a","index":0,"text":"A"},{"id":"b","index":1,"text":"B"},{"id":"c","index":2,"text":"C"}],
            "evaluation":{"grades":{"a":1,"b":2,"c":3}}}]);
        let bytes = serde_json::to_vec(&json!({"protocol":INPUT_PROTOCOL,"inputSha256":hash(&serde_json::to_vec(&queries).unwrap()),"queries":queries})).unwrap();
        let input: Value = serde_json::from_slice(&bytes).unwrap();
        let scores = json!({"protocol":SCORE_PROTOCOL,"inputSha256":input["inputSha256"],
            "exportedInputFileSha256":hash(&bytes),"arm":"test-local","modelIdentity":{},
            "queries":[{"id":"q","status":"ok","latencyMs":12.5,
                "scores":[{"id":"c","score":1.0},{"id":"b","score":1.0},{"id":"a","score":1.0}]}]});
        (bytes, scores)
    }

    #[test]
    fn comparison_ties_reuse_frozen_order_and_native_metrics() {
        let (bytes, scores) = fixture();
        let report = replay(&bytes, &scores).unwrap();
        assert_eq!(report["queries"][0]["ranking"], json!(["a", "b", "c"]));
        assert_eq!(
            report["queries"][0]["metrics"],
            json!([ndcg_at_five(&[1, 2, 3]), 0.5, 2.0 / 3.0])
        );
        assert_eq!(reciprocal_rank(&[1, 0, 2]), 1.0 / 3.0);
        assert_eq!(precision_at_three(&[1, 1, 1]), 0.0);
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0], 50), Some(2.0));
        let mut zeroes = scores;
        zeroes["queries"][0]["scores"] = json!([
            {"id":"c","score":0.0},{"id":"b","score":0.0},{"id":"a","score":-0.0}
        ]);
        assert_eq!(
            replay(&bytes, &zeroes).unwrap()["queries"][0]["ranking"],
            json!(["a", "b", "c"])
        );
    }

    #[test]
    fn comparison_rejects_identity_order_and_coverage_mismatch() {
        let (bytes, scores) = fixture();
        for key in ["inputSha256", "exportedInputFileSha256", "protocol"] {
            let mut bad = scores.clone();
            bad[key] = json!("wrong");
            assert!(replay(&bytes, &bad).is_err(), "{key}");
        }
        let mut bad = scores.clone();
        bad["queries"][0]["id"] = json!("unknown");
        assert!(replay(&bytes, &bad).is_err());
        for ids in [vec!["a", "a", "c"], vec!["a", "b"], vec!["a", "b", "unknown"]] {
            let mut bad = scores.clone();
            bad["queries"][0]["scores"] =
                json!(ids.iter().map(|id| json!({"id":id,"score":1.0})).collect::<Vec<_>>());
            assert!(replay(&bytes, &bad).is_err());
        }
        let mut bad = scores.clone();
        bad["queries"][0]["scores"][0]["score"] = Value::Null;
        assert!(replay(&bytes, &bad).is_err());
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert!(replay(&changed, &scores).is_err());
    }

    #[test]
    fn comparison_failures_do_not_fabricate_quality_or_latency() {
        let (bytes, mut scores) = fixture();
        scores["queries"][0] = json!({"id":"q","status":"failed","scores":[],"latencyMs":null,
            "failure":{"type":"RuntimeError","message":"OOM"}});
        let report = replay(&bytes, &scores).unwrap();
        assert_eq!(report["groups"]["all"]["commonCompletedDenominator"], 0);
        assert_eq!(report["groups"]["all"]["failures"], 1);
        assert!(report["latency"]["p50Ms"].is_null());
        assert!(report["queries"][0]["ranking"].is_null());
        scores["queries"][0] = json!({"id":"q","status":"not_run","scores":[],"latencyMs":null});
        scores["failure"] = json!({"type":"RuntimeError","message":"startup failed"});
        scores["modelLoadMs"] = json!(12.5);
        scores["documentPrecomputation"] = json!({"cache":"not created"});
        let report = replay(&bytes, &scores).unwrap();
        assert_eq!(report["groups"]["all"]["notRun"], 1);
        assert_eq!(report["groups"]["all"]["failed"], 0);
        assert_eq!(report["failure"], scores["failure"]);
        assert_eq!(report["documentPrecomputation"], scores["documentPrecomputation"]);
        assert_eq!(report["modelLoadMs"], 12.5);
        assert!(report["latency"]["p50Ms"].is_null());
    }

    #[test]
    fn comparison_export_uses_native_locators_and_separates_labels() {
        let parent = CorpusQuery {
            id: "parent".into(),
            query: "  question  ".into(),
            gold_source: "gold".into(),
            candidates: vec![
                CorpusCandidate {
                    id: "opaque:z".into(),
                    source: "gold".into(),
                    grade: 3,
                    text: "  evidence  ".into(),
                },
                CorpusCandidate {
                    id: "opaque:a".into(),
                    source: "other".into(),
                    grade: 1,
                    text: "x".repeat(1_300),
                },
            ],
        };
        let query = export_query(&parent, "translated", "ru", "  вопрос  ").unwrap();
        assert_eq!(query["query"], "вопрос");
        assert_eq!(query["candidates"][0], json!({"id":"opaque:z","index":0,"text":"evidence"}));
        assert_eq!(query["candidates"][1]["id"], "opaque:a");
        assert_eq!(query["candidates"][1]["text"].as_str().unwrap().chars().count(), 1_200);
        assert!(query["candidates"][0].get("grade").is_none());
        assert_eq!(query["evaluation"]["grades"]["opaque:z"], 3);
        assert_eq!(query["evaluation"]["grade3InheritedFromEnglishOverlapProxy"], true);
        assert_eq!(query["preparation"]["changedCandidateIds"], json!(["opaque:z", "opaque:a"]));
    }
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
