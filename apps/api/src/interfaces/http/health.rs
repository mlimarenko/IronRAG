use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, sync::OnceLock};

use crate::{
    app::state::AppState,
    services::{
        ops::deployment_diagnostics::DeploymentReadinessSnapshot,
        ops::release_monitor::{ReleaseUpdateSnapshot, ReleaseUpdateStatus},
    },
};

#[derive(Serialize, utoipa::ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: String,
    pub environment: String,
    pub role: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct VersionResponse {
    #[serde(rename = "contentVisibilityContract")]
    pub content_visibility_contract: &'static str,
    pub service: String,
    pub version: String,
    pub environment: String,
    pub role: String,
    #[serde(rename = "deploymentId")]
    pub deployment_id: Option<String>,
    #[serde(rename = "binarySha256")]
    pub binary_sha256: Option<String>,
    #[serde(rename = "queryTurnMaxConcurrent")]
    pub query_turn_max_concurrent: usize,
    #[serde(rename = "resolvedConfigSha256")]
    pub resolved_config_sha256: String,
    #[serde(rename = "runtimeResources")]
    pub runtime_resources: RuntimeResources,
    #[serde(rename = "runtimeResourcesSha256")]
    pub runtime_resources_sha256: String,
    #[serde(rename = "redisEndpointSha256")]
    pub redis_endpoint_sha256: Option<String>,
    #[serde(rename = "redisServiceSha256")]
    pub redis_service_sha256: Option<String>,
    #[serde(rename = "databaseTlsLeafSha256")]
    pub database_tls_leaf_sha256: Option<String>,
    #[serde(rename = "redisTlsLeafSha256")]
    pub redis_tls_leaf_sha256: Option<String>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeResources {
    pub available_parallelism: usize,
    pub cgroup_cpu_max: Option<String>,
    pub cgroup_cpuset_cpus_effective: Option<String>,
    pub cgroup_memory_max: Option<String>,
    pub host_memory_total_bytes: Option<u64>,
}

static BINARY_SHA256: OnceLock<Option<String>> = OnceLock::new();
const QUERY_RUNTIME_ENV_KEYS: [&str; 4] = [
    "IRONRAG_PG_HNSW_EF_SEARCH",
    "IRONRAG_PG_HNSW_EXACT_FALLBACK_MAX_ROWS",
    "IRONRAG_PG_HNSW_MAX_SCAN_TUPLES",
    "IRONRAG_PG_HNSW_SCAN_MEM_MULTIPLIER",
];

fn running_binary_sha256() -> Option<String> {
    BINARY_SHA256
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let file = File::open("/proc/self/exe").ok()?;
            #[cfg(not(target_os = "linux"))]
            let file = File::open(std::env::current_exe().ok()?).ok()?;
            open_file_sha256(file)
        })
        .clone()
}

fn open_file_sha256(mut file: File) -> Option<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Some(hex::encode(digest.finalize()))
}

fn resolved_config_sha256(state: &AppState) -> String {
    // This is deliberately an allow-list of non-secret settings that can
    // change query latency, throughput, or answer selection. Adding such a
    // setting without adding it here makes benchmark runs falsely comparable.
    let settings = &state.settings;
    let performance_config = serde_json::json!({
        "service": settings.service_name,
        "environment": settings.environment,
        "role": settings.service_role,
        "databaseMaxConnections": settings.database_max_connections,
        "apiReplicas": settings.api_replicas,
        "workerReplicas": settings.worker_replicas,
        "ingestionMaxParallelJobsGlobal": settings.ingestion_max_parallel_jobs_global,
        "postgresPoolPlan": crate::infra::persistence::postgres_pool_plan_attestation(settings),
        "databaseTlsLeafSha256": state.persistence.dependency_tls.database_leaf_sha256.as_deref(),
        "redisTlsLeafSha256": state.persistence.dependency_tls.redis_leaf_sha256.as_deref(),
        "llmHttpTimeoutSeconds": settings.llm_http_timeout_seconds,
        "runtimeAgentMaxTurns": settings.runtime_agent_max_turns,
        "runtimeAgentMaxParallelActions": settings.runtime_agent_max_parallel_actions,
        "runtimeTracePayloadBudgetBytes": settings.runtime_trace_payload_budget_bytes,
        "runtimePolicyReasonBudgetChars": settings.runtime_policy_reason_budget_chars,
        "runtimePolicyRejectTaskKinds": settings.runtime_policy_reject_task_kinds,
        "runtimePolicyRejectTargetKinds": settings.runtime_policy_reject_target_kinds,
        "queryIntentCacheTtlHours": settings.query_intent_cache_ttl_hours,
        "queryIntentCacheMaxEntriesPerLibrary": settings.query_intent_cache_max_entries_per_library,
        "queryRerankEnabled": settings.query_rerank_enabled,
        "queryRerankCandidateLimit": settings.query_rerank_candidate_limit,
        "querySemanticRerankMode": settings.query_semantic_rerank_mode.as_str(),
        "querySemanticRerankTimeoutMs": settings.query_semantic_rerank_timeout_ms,
        "querySemanticRerankCandidateLimit": settings.query_semantic_rerank_candidate_limit,
        "querySemanticRerankCandidateTextChars": settings.query_semantic_rerank_candidate_text_chars,
        "querySemanticRerankTotalTextChars": settings.query_semantic_rerank_total_text_chars,
        "queryBalancedContextEnabled": settings.query_balanced_context_enabled,
        "retrievalPolicyFingerprint": crate::services::query::result_cache::retrieval_policy_fingerprint(
            &state.retrieval_intelligence,
            &state.bulk_ingest_hardening,
        ),
        "runtimeGraphFilterEmptyRelations": settings.runtime_graph_filter_empty_relations,
        "runtimeGraphFilterDegenerateSelfLoops": settings.runtime_graph_filter_degenerate_self_loops,
        "runtimeGraphConvergenceWarningBacklogThreshold": settings.runtime_graph_convergence_warning_backlog_threshold,
        "runtimeGraphProjectionPrewarmEnabled": settings.runtime_graph_projection_prewarm_enabled,
        "runtimeGraphProjectionPrewarmMaxLibraries": settings.runtime_graph_projection_prewarm_max_libraries,
        "providerConcurrencyMaxOutbound": settings.provider_concurrency_max_outbound,
        "providerConcurrencyQueryReserved": settings.provider_concurrency_query_reserved,
        "providerConcurrencyAcquireTimeoutMs": settings.provider_concurrency_acquire_timeout_ms,
        "providerConcurrencyRegistryMaxEntries": settings.provider_concurrency_registry_max_entries,
        "providerConcurrencyRegistryIdleTtlSeconds": settings.provider_concurrency_registry_idle_ttl_seconds,
        "providerRateLimitRequestsPerSecond": settings.provider_rate_limit_requests_per_second,
        "providerRateLimitBurst": settings.provider_rate_limit_burst,
        "providerRetryMaxAttempts": settings.provider_retry_max_attempts,
        "providerRetryInitialBackoffMs": settings.provider_retry_initial_backoff_ms,
        "providerRetryMaxBackoffMs": settings.provider_retry_max_backoff_ms,
        "queryTurnMaxConcurrent": settings.query_turn_max_concurrent,
        "queryTurnAdmissionQueue": settings.query_turn_admission_queue,
        "queryTurnAdmissionTimeoutMs": settings.query_turn_admission_timeout_ms,
        "dependencyRedisMode": settings.dependency_redis_mode,
        "queryRuntimeEnvironment": QUERY_RUNTIME_ENV_KEYS
            .into_iter()
            .map(|key| (key, std::env::var(key).ok()))
            .collect::<BTreeMap<_, _>>(),
    });
    hex::encode(Sha256::digest(performance_config.to_string().as_bytes()))
}

#[cfg(test)]
mod runtime_config_tests {
    use super::{open_file_sha256, resolved_config_sha256};
    use crate::{
        app::{config::Settings, state::AppState},
        infra::persistence::Persistence,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    use std::process::Command;

    #[test]
    fn every_direct_query_runtime_environment_key_is_attested() {
        const HNSW_INPUTS: [&str; 4] = [
            "IRONRAG_PG_HNSW_EF_SEARCH",
            "IRONRAG_PG_HNSW_EXACT_FALLBACK_MAX_ROWS",
            "IRONRAG_PG_HNSW_MAX_SCAN_TUPLES",
            "IRONRAG_PG_HNSW_SCAN_MEM_MULTIPLIER",
        ];
        fn fingerprint(override_key: Option<&str>) -> String {
            let mut child = Command::new(std::env::current_exe().expect("test executable"));
            child
                .args([
                    "--exact",
                    "interfaces::http::health::runtime_config_tests::fingerprint_child",
                    "--nocapture",
                ])
                .env("IRONRAG_FINGERPRINT_TEST_CHILD", "1");
            for key in HNSW_INPUTS {
                child.env_remove(key);
            }
            if let Some(key) = override_key {
                child.env(key, "123");
            }
            let output = child.output().expect("isolated fingerprint child");
            assert!(output.status.success(), "fingerprint child failed");
            String::from_utf8(output.stdout)
                .expect("UTF-8 test output")
                .lines()
                .find_map(|line| line.strip_prefix("fingerprint=").map(str::to_owned))
                .expect("child fingerprint receipt")
        }
        let baseline = fingerprint(None);
        for key in HNSW_INPUTS {
            assert_ne!(fingerprint(Some(key)), baseline, "{key} must affect the resolved digest");
        }
    }

    fn fingerprint_state(mut settings: Settings) -> AppState {
        settings.discard_credential_master_key();
        settings.credential_master_key = Some(STANDARD.encode([71_u8; 32]));
        settings.credential_master_key_id = None;
        settings.credential_previous_master_keys = None;
        settings.ui_bootstrap_admin_login = None;
        settings.ui_bootstrap_admin_email = None;
        settings.ui_bootstrap_admin_name = None;
        settings.ui_bootstrap_admin_password = None;
        settings.ui_bootstrap_admin_api_token = None;
        let postgres = sqlx::PgPool::connect_lazy("postgres://127.0.0.1/ironrag").unwrap();
        let redis = redis::Client::open("redis://127.0.0.1/").unwrap();
        AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis)).unwrap()
    }

    #[tokio::test]
    #[expect(clippy::print_stdout, reason = "communicates fingerprint receipt to parent process")]
    async fn fingerprint_child() {
        if std::env::var_os("IRONRAG_FINGERPRINT_TEST_CHILD").is_none() {
            return;
        }
        let state = fingerprint_state(Settings::from_env().unwrap());
        let digest = resolved_config_sha256(&state);
        assert_eq!(digest, resolved_config_sha256(&state), "same inputs must be stable");
        println!("fingerprint={digest}");
    }

    #[tokio::test]
    async fn every_postgres_pool_input_and_derived_plan_is_attested() {
        for role in ["api", "worker"] {
            let mut settings = Settings::from_env().unwrap();
            settings.service_role = role.to_string();
            let state = fingerprint_state(settings);
            let baseline = resolved_config_sha256(&state);
            assert_eq!(baseline, resolved_config_sha256(&state));
            for input in 0..4 {
                let mut changed = state.settings.clone();
                match input {
                    0 => changed.database_max_connections += 1,
                    1 => changed.api_replicas += 1,
                    2 => changed.worker_replicas += 1,
                    _ => changed.ingestion_max_parallel_jobs_global += 1,
                }
                assert_ne!(
                    resolved_config_sha256(&fingerprint_state(changed)),
                    baseline,
                    "pool input {input} must affect {role} attestation"
                );
            }
        }
    }

    #[test]
    fn executable_hash_uses_the_open_running_image_after_path_replacement() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let executable_path = root.path().join("ironrag");
        let replacement_path = root.path().join("replacement");
        std::fs::write(&executable_path, b"running-image")?;
        std::fs::write(&replacement_path, b"replacement-image")?;
        let running_handle = std::fs::File::open(&executable_path)?;
        std::fs::rename(&replacement_path, &executable_path)?;

        assert_eq!(
            open_file_sha256(running_handle),
            Some(hex::encode(Sha256::digest(b"running-image")))
        );
        assert_eq!(std::fs::read(executable_path)?, b"replacement-image".to_vec());
        Ok(())
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn host_memory_total_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kilobytes = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1024)
}

fn runtime_resources() -> RuntimeResources {
    RuntimeResources {
        available_parallelism: std::thread::available_parallelism().map(usize::from).unwrap_or(1),
        cgroup_cpu_max: read_trimmed("/sys/fs/cgroup/cpu.max"),
        cgroup_cpuset_cpus_effective: read_trimmed("/sys/fs/cgroup/cpuset.cpus.effective"),
        cgroup_memory_max: read_trimmed("/sys/fs/cgroup/memory.max"),
        host_memory_total_bytes: host_memory_total_bytes(),
    }
}

fn runtime_resources_sha256(resources: &RuntimeResources) -> String {
    let identity = BTreeMap::from([
        ("availableParallelism", serde_json::json!(resources.available_parallelism)),
        ("cgroupCpuMax", serde_json::json!(resources.cgroup_cpu_max)),
        ("cgroupCpusetCpusEffective", serde_json::json!(resources.cgroup_cpuset_cpus_effective)),
        ("cgroupMemoryMax", serde_json::json!(resources.cgroup_memory_max)),
        ("hostMemoryTotalBytes", serde_json::json!(resources.host_memory_total_bytes)),
    ]);
    let Ok(encoded) = serde_json::to_vec(&identity) else {
        tracing::error!("failed to serialize runtime resource identity");
        return String::new();
    };
    hex::encode(Sha256::digest(encoded))
}

fn redis_identities(redis_url: &str) -> Option<(String, String)> {
    let parsed = url::Url::parse(redis_url.trim()).ok()?;
    if parsed.scheme() != "rediss"
        || parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let database_text = parsed.path().trim_start_matches('/');
    let database = if database_text.is_empty() { 0 } else { database_text.parse::<u32>().ok()? };
    let authority =
        format!("{}:{}", parsed.host_str()?.to_ascii_lowercase(), parsed.port().unwrap_or(6379));
    let endpoint = format!("{}://{authority}/{database}", parsed.scheme());
    let service = format!("{}://{authority}", parsed.scheme());
    Some((
        hex::encode(Sha256::digest(endpoint.as_bytes())),
        hex::encode(Sha256::digest(service.as_bytes())),
    ))
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseUpdateResponse {
    pub status: ReleaseUpdateStatus,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub repository_url: String,
    pub checked_at: String,
}

/// Reported when the binary carries no build identity, which is the case for
/// every plain `cargo build`. It is deliberately unparseable as a release so a
/// developer binary can never be mistaken for a deployed one. Image builds
/// cannot produce it: `apps/api/Dockerfile` refuses to compile without a
/// non-empty `APP_VERSION` build argument.
const UNIDENTIFIED_LOCAL_BUILD: &str = "unversioned-local-build";

/// Normalizes the build identity baked in at compile time from the
/// `APP_VERSION` build argument.
///
/// An absent or blank argument means the build was never told which commit or
/// release it is. That gap must surface as [`UNIDENTIFIED_LOCAL_BUILD`] rather
/// than as the crate version, which reads exactly like a real release and
/// therefore destroys deployment traceability instead of reporting its loss.
fn resolve_release_version(baked_version: Option<&str>) -> &str {
    match baked_version.map(str::trim) {
        Some(version) if !version.is_empty() => version.trim_start_matches('v'),
        _ => UNIDENTIFIED_LOCAL_BUILD,
    }
}

fn current_release_version() -> String {
    resolve_release_version(option_env!("APP_VERSION")).to_string()
}

#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "system",
    operation_id = "getHealth",
    responses(
        (status = 200, description = "Service is up", body = HealthResponse),
    ),
    security(),
)]
pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: state.settings.service_name,
        environment: state.settings.environment,
        role: state.settings.service_role,
    })
}

#[utoipa::path(
    get,
    path = "/v1/ready",
    tag = "system",
    operation_id = "getReadiness",
    responses(
        (status = 200, description = "All dependencies reachable", body = DeploymentReadinessSnapshot),
        (status = 503, description = "One or more dependencies are degraded", body = DeploymentReadinessSnapshot),
    ),
    security(),
)]
pub async fn readiness(
    State(state): State<AppState>,
) -> (StatusCode, Json<DeploymentReadinessSnapshot>) {
    let (ready, snapshot) = state.deployment_diagnostics.readiness_snapshot(&state).await;
    if ready {
        (StatusCode::OK, Json(snapshot))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(snapshot))
    }
}

#[utoipa::path(
    get,
    path = "/v1/version",
    tag = "system",
    operation_id = "getVersion",
    responses(
        (status = 200, description = "Build identifier and runtime metadata", body = VersionResponse),
    ),
    security(),
)]
pub async fn version(State(state): State<AppState>) -> Json<VersionResponse> {
    let binary_sha256 = running_binary_sha256();
    let resolved_config_sha256 = resolved_config_sha256(&state);
    let deployment_id = crate::observability::deployment_id().map(str::to_owned);
    let query_turn_max_concurrent = state.settings.query_turn_max_concurrent;
    let runtime_resources = runtime_resources();
    let runtime_resources_sha256 = runtime_resources_sha256(&runtime_resources);
    let (redis_endpoint_sha256, redis_service_sha256) = redis_identities(&state.settings.redis_url)
        .map(|(endpoint, service)| (Some(endpoint), Some(service)))
        .unwrap_or((None, None));
    Json(VersionResponse {
        content_visibility_contract: ironrag_contracts::documents::CONTENT_VISIBILITY_CONTRACT,
        service: state.settings.service_name,
        version: current_release_version(),
        environment: state.settings.environment,
        role: state.settings.service_role,
        deployment_id,
        binary_sha256,
        query_turn_max_concurrent,
        resolved_config_sha256,
        runtime_resources,
        runtime_resources_sha256,
        redis_endpoint_sha256,
        redis_service_sha256,
        database_tls_leaf_sha256: state.persistence.dependency_tls.database_leaf_sha256.clone(),
        redis_tls_leaf_sha256: state.persistence.dependency_tls.redis_leaf_sha256.clone(),
    })
}

impl From<ReleaseUpdateSnapshot> for ReleaseUpdateResponse {
    fn from(snapshot: ReleaseUpdateSnapshot) -> Self {
        Self {
            status: snapshot.status,
            current_version: snapshot.current_version,
            latest_version: snapshot.latest_version,
            release_url: snapshot.release_url,
            repository_url: snapshot.repository_url,
            checked_at: snapshot.checked_at.to_rfc3339(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/version/latest",
    tag = "system",
    operation_id = "getLatestVersion",
    responses(
        (status = 200, description = "Latest published release the service is aware of", body = ReleaseUpdateResponse),
    ),
    security(),
)]
pub async fn release_update(State(state): State<AppState>) -> Json<ReleaseUpdateResponse> {
    Json(state.release_monitor.get_release_update(&current_release_version()).await.into())
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeResources, UNIDENTIFIED_LOCAL_BUILD, VersionResponse, redis_identities,
        resolve_release_version,
    };

    #[test]
    fn baked_build_identity_is_reported_without_its_tag_prefix() {
        assert_eq!(resolve_release_version(Some("v0.6.0")), "0.6.0");
        assert_eq!(resolve_release_version(Some("0.6.0")), "0.6.0");
        assert_eq!(
            resolve_release_version(Some("  stage-20260815T0000Z-7f3ab1c9  ")),
            "stage-20260815T0000Z-7f3ab1c9"
        );
    }

    #[test]
    fn absent_build_identity_is_never_reported_as_the_crate_version() {
        assert_eq!(resolve_release_version(None), UNIDENTIFIED_LOCAL_BUILD);
        assert_ne!(resolve_release_version(None), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn blank_build_argument_is_an_absent_identity() {
        // A build argument declared but passed through empty (`APP_VERSION=`)
        // reaches the compiler as `Some("")`. That is a missing identity, not
        // an identity that happens to be the empty string.
        assert_eq!(resolve_release_version(Some("")), UNIDENTIFIED_LOCAL_BUILD);
        assert_eq!(resolve_release_version(Some("   ")), UNIDENTIFIED_LOCAL_BUILD);
    }

    #[test]
    fn version_response_exposes_the_opaque_runtime_deployment_identity() {
        let response = VersionResponse {
            content_visibility_contract: ironrag_contracts::documents::CONTENT_VISIBILITY_CONTRACT,
            service: "ironrag-api".to_owned(),
            version: "0.6.0".to_owned(),
            environment: "test".to_owned(),
            role: "all".to_owned(),
            deployment_id: Some("315991c5-89f1-59af-9f95-4161b88063a4".to_owned()),
            binary_sha256: Some("a".repeat(64)),
            query_turn_max_concurrent: 100,
            resolved_config_sha256: "b".repeat(64),
            runtime_resources: RuntimeResources {
                available_parallelism: 8,
                cgroup_cpu_max: Some("800000 100000".to_owned()),
                cgroup_cpuset_cpus_effective: Some("0-7".to_owned()),
                cgroup_memory_max: Some("17179869184".to_owned()),
                host_memory_total_bytes: Some(17_179_869_184),
            },
            runtime_resources_sha256: "c".repeat(64),
            redis_endpoint_sha256: Some("d".repeat(64)),
            redis_service_sha256: Some("e".repeat(64)),
            database_tls_leaf_sha256: Some("f".repeat(64)),
            redis_tls_leaf_sha256: Some("1".repeat(64)),
        };

        let value = serde_json::to_value(response).expect("version response must serialize");
        assert_eq!(
            value.get("deploymentId").and_then(serde_json::Value::as_str),
            Some("315991c5-89f1-59af-9f95-4161b88063a4")
        );
        assert!(value.get("deployment_id").is_none());
        assert_eq!(
            value.get("redisEndpointSha256").and_then(serde_json::Value::as_str),
            Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
        );
    }

    #[test]
    fn redis_identity_excludes_credentials_and_binds_logical_database() {
        assert!(redis_identities("redis://redis.invalid/2").is_none());
        let first = redis_identities("rediss://user:secret@REDIS.invalid/2").unwrap();
        let same = redis_identities("rediss://other:changed@redis.invalid:6379/2").unwrap();
        let another_database = redis_identities("rediss://redis.invalid/3").unwrap();
        assert_eq!(first, same);
        assert_ne!(first.0, another_database.0);
        assert_eq!(first.1, another_database.1);
    }
}
