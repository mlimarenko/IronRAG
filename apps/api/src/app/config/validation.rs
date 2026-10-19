use super::*;

pub(super) fn validate_credential_master_key(settings: &Settings) -> Result<(), String> {
    // A fully absent credential keyring is accepted at the configuration
    // boundary: keyless admin/maintenance loads and repository tests never
    // build a credential cipher. The serving process still fails closed because
    // `CredentialCipher::from_keyring_base64` rejects a missing active key when
    // `AppState` construction builds the mandatory cipher. A partially supplied
    // keyring (key id or previous-key map without an active key) is rejected
    // here so a misconfiguration surfaces at load time.
    if settings.credential_master_key.as_deref().is_none_or(str::is_empty)
        && settings.credential_master_key_id.is_none()
        && settings.credential_previous_master_keys.is_none()
    {
        return Ok(());
    }
    crate::shared::secret_encryption::CredentialCipher::from_keyring_base64(
        settings.credential_master_key_id.as_deref(),
        settings.credential_master_key.as_deref(),
        settings.credential_previous_master_keys.as_deref(),
    )
    .map(|_| ())
    .map_err(|_| {
        "IRONRAG_CREDENTIAL_MASTER_KEY, IRONRAG_CREDENTIAL_MASTER_KEY_ID and IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS must define a strict bounded credential keyring"
            .to_string()
    })
}

pub(super) fn validate_service_role(settings: &Settings) -> Result<(), String> {
    settings.service_role.parse::<ServiceRole>().map(|_| ())
}

pub(super) fn validate_startup_authority_mode(settings: &Settings) -> Result<(), String> {
    settings.startup_authority_mode.parse::<StartupAuthorityMode>().map(|_| ())
}

pub(super) fn validate_dependency_modes(settings: &Settings) -> Result<(), String> {
    for kind in [DependencyKind::Postgres, DependencyKind::Redis, DependencyKind::ObjectStorage] {
        let mode = settings.dependency_mode(kind)?;
        if matches!(kind, DependencyKind::Postgres | DependencyKind::Redis)
            && matches!(mode, DependencyMode::Disabled)
        {
            return Err(format!("{} must not use disabled mode", kind.as_str()));
        }
    }
    Ok(())
}

/// Validates OIDC as one atomic configuration unit. The resolver owns the
/// detailed URL/origin checks so callers that construct `Settings` directly
/// receive exactly the same fail-closed behavior as environment loading.
pub(super) fn validate_oidc_settings(settings: &Settings) -> Result<(), String> {
    settings.resolved_oidc_settings().map(|_| ())
}

pub(super) fn validate_database_settings(settings: &Settings) -> Result<(), String> {
    let runtime_replicas = settings.api_replicas.saturating_add(settings.worker_replicas);
    if settings.runs_http_api() && settings.api_replicas == 0 {
        return Err("api_replicas must be at least 1 when service_role=api".into());
    }
    if settings.runs_ingestion_workers() && settings.worker_replicas == 0 {
        return Err("worker_replicas must be at least 1 when service_role=worker".into());
    }
    if runtime_replicas == 0 {
        return Err("api_replicas and worker_replicas must not both be zero".into());
    }

    let runtime_replicas = u32::try_from(runtime_replicas).unwrap_or(u32::MAX);
    let minimum_budget =
        MIN_DATABASE_CONNECTIONS_PER_RUNTIME_REPLICA.saturating_mul(runtime_replicas);
    if settings.database_max_connections < minimum_budget {
        return Err(format!(
            "database_max_connections must be at least {minimum_budget} for api_replicas={} and worker_replicas={}",
            settings.api_replicas, settings.worker_replicas
        ));
    }
    Ok(())
}

pub(super) fn validate_knowledge_plane_backend(settings: &Settings) -> Result<(), String> {
    match settings.knowledge_plane_backend.as_str() {
        "postgres" => Ok(()),
        _ => Err("knowledge_plane_backend must be postgres".into()),
    }
}

pub(super) fn validate_content_storage_settings(settings: &Settings) -> Result<(), String> {
    let provider = settings.content_storage_provider_kind()?;
    let topology = settings.content_storage_topology_kind()?;
    if settings.content_storage_key_prefix.trim().contains("..") {
        return Err("content_storage_key_prefix must not contain '..'".into());
    }

    match provider {
        ContentStorageProvider::Filesystem => {
            if settings.content_storage_root.trim().is_empty() {
                return Err("content_storage_root must not be empty".into());
            }
            if !matches!(topology, DeploymentTopology::SingleNode) {
                return Err(
                    "filesystem storage is supported only with content_storage_topology=single_node"
                        .into(),
                );
            }
            if !matches!(
                settings.dependency_mode(DependencyKind::ObjectStorage)?,
                DependencyMode::Disabled
            ) {
                return Err(
                    "dependency_object_storage_mode must be disabled when content_storage_provider=filesystem"
                        .into(),
                );
            }
        }
        ContentStorageProvider::S3 => {
            if matches!(
                settings.dependency_mode(DependencyKind::ObjectStorage)?,
                DependencyMode::Disabled
            ) {
                return Err(
                    "dependency_object_storage_mode must be bundled or external when content_storage_provider=s3"
                        .into(),
                );
            }
            for (field, value) in [
                ("content_storage_s3_bucket", settings.content_storage_s3_bucket.as_deref()),
                ("content_storage_s3_endpoint", settings.content_storage_s3_endpoint.as_deref()),
                (
                    "content_storage_s3_access_key_id",
                    settings.content_storage_s3_access_key_id.as_deref(),
                ),
                (
                    "content_storage_s3_secret_access_key",
                    settings.content_storage_s3_secret_access_key.as_deref(),
                ),
            ] {
                if value.map(str::trim).as_ref().is_none_or(|item| item.is_empty()) {
                    return Err(format!(
                        "{field} must not be empty when content_storage_provider=s3"
                    ));
                }
            }
        }
    }

    Ok(())
}

pub(super) fn validate_service_name(settings: &Settings) -> Result<(), String> {
    let value = settings.service_name.as_str();
    if value.is_empty() {
        return Err("service_name must not be empty".into());
    }
    if value
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        return Err("service_name must contain only ASCII letters, digits, '.', '_' or '-'".into());
    }
    Ok(())
}

pub(super) fn validate_ingestion_settings(settings: &Settings) -> Result<(), String> {
    if settings.ingestion_max_parallel_jobs_global == 0 {
        return Err("ingestion_max_parallel_jobs_global must be greater than zero".into());
    }
    if settings.ingestion_max_parallel_jobs_per_workspace == 0 {
        return Err("ingestion_max_parallel_jobs_per_workspace must be greater than zero".into());
    }
    if settings.ingestion_max_parallel_jobs_per_library == 0 {
        return Err("ingestion_max_parallel_jobs_per_library must be greater than zero".into());
    }
    if settings.ingestion_graph_extract_parallelism_per_doc == 0 {
        return Err("ingestion_graph_extract_parallelism_per_doc must be greater than zero".into());
    }
    if settings.ingestion_max_parallel_jobs_per_workspace
        > settings.ingestion_max_parallel_jobs_global
    {
        return Err(
            "ingestion_max_parallel_jobs_per_workspace must be less than or equal to ingestion_max_parallel_jobs_global"
                .into(),
        );
    }
    if settings.ingestion_max_parallel_jobs_per_library
        > settings.ingestion_max_parallel_jobs_per_workspace
    {
        return Err(
            "ingestion_max_parallel_jobs_per_library must be less than or equal to ingestion_max_parallel_jobs_per_workspace"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn resolve_ingestion_graph_extract_parallelism(
    configured_per_doc: usize,
    parallel_jobs: usize,
    max_outbound: usize,
    query_reserved: usize,
) -> Result<usize, String> {
    if configured_per_doc == 0 {
        return Err("ingestion_graph_extract_parallelism_per_doc must be greater than zero".into());
    }
    if parallel_jobs == 0 {
        return Err("ingestion_max_parallel_jobs_global must be greater than zero".into());
    }
    if max_outbound == 0 {
        return if query_reserved == 0 {
            Ok(configured_per_doc)
        } else {
            Err(
                "provider_concurrency_query_reserved must be zero when provider_concurrency_max_outbound is zero"
                    .into(),
            )
        };
    }
    let ingest_lane_capacity = max_outbound.checked_sub(query_reserved).filter(|value| *value > 0)
        .ok_or_else(|| {
            "provider_concurrency_query_reserved must be smaller than provider_concurrency_max_outbound"
                .to_string()
        })?;
    let maximum_extract_calls =
        ingest_lane_capacity.saturating_mul(INGEST_GRAPH_EXTRACT_WAITING_FACTOR);
    if parallel_jobs > maximum_extract_calls {
        return Err(format!(
            "ingestion_max_parallel_jobs_global resolves to {parallel_jobs} jobs per worker, which exceeds the provider ingest waiting bound of {maximum_extract_calls}; lower ingestion job parallelism or raise provider_concurrency_max_outbound"
        ));
    }
    let per_doc_bound = maximum_extract_calls.checked_div(parallel_jobs).unwrap_or(1).max(1);
    Ok(configured_per_doc.min(per_doc_bound))
}

pub(super) fn validate_provider_resilience_settings(settings: &Settings) -> Result<(), String> {
    match (settings.provider_concurrency_max_outbound, settings.provider_concurrency_query_reserved)
    {
        (0, 0) => {}
        (0, _) => {
            return Err(
                "provider_concurrency_query_reserved must be zero when provider_concurrency_max_outbound is zero"
                    .into(),
            );
        }
        (max, reserved) if reserved >= max => {
            return Err(
                "provider_concurrency_query_reserved must be smaller than provider_concurrency_max_outbound"
                    .into(),
            );
        }
        _ => {}
    }
    if settings.provider_concurrency_acquire_timeout_ms == 0 {
        return Err("provider_concurrency_acquire_timeout_ms must be greater than zero".into());
    }
    if settings.provider_concurrency_registry_max_entries == 0 {
        return Err("provider_concurrency_registry_max_entries must be greater than zero".into());
    }
    if settings.provider_concurrency_registry_idle_ttl_seconds == 0 {
        return Err(
            "provider_concurrency_registry_idle_ttl_seconds must be greater than zero".into()
        );
    }
    match (settings.provider_rate_limit_requests_per_second, settings.provider_rate_limit_burst) {
        (0, 0) => {}
        (0, _) => {
            return Err(
                "provider_rate_limit_burst must be zero when provider_rate_limit_requests_per_second is zero"
                    .into(),
            );
        }
        (_, 0) => {
            return Err(
                "provider_rate_limit_burst must be greater than zero when provider rate limiting is enabled"
                    .into(),
            );
        }
        _ => {}
    }
    if settings.provider_rate_limit_requests_per_second
        > MAX_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND
    {
        return Err(format!(
            "provider_rate_limit_requests_per_second must not exceed {MAX_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND}"
        ));
    }
    if settings.provider_rate_limit_burst > MAX_PROVIDER_RATE_LIMIT_BURST {
        return Err(format!(
            "provider_rate_limit_burst must not exceed {MAX_PROVIDER_RATE_LIMIT_BURST}"
        ));
    }
    if !(1..=MAX_PROVIDER_RETRY_ATTEMPTS).contains(&settings.provider_retry_max_attempts) {
        return Err(format!(
            "provider_retry_max_attempts must be between 1 and {MAX_PROVIDER_RETRY_ATTEMPTS}"
        ));
    }
    if settings.provider_retry_max_backoff_ms == 0 {
        return Err("provider_retry_max_backoff_ms must be greater than zero".into());
    }
    if settings.provider_retry_initial_backoff_ms > settings.provider_retry_max_backoff_ms {
        return Err(
            "provider_retry_initial_backoff_ms must not exceed provider_retry_max_backoff_ms"
                .into(),
        );
    }
    if settings.provider_retry_max_backoff_ms > MAX_PROVIDER_RETRY_BACKOFF_MS {
        return Err(format!(
            "provider_retry_max_backoff_ms must not exceed {MAX_PROVIDER_RETRY_BACKOFF_MS}"
        ));
    }
    Ok(())
}

pub(super) fn validate_query_turn_admission_settings(settings: &Settings) -> Result<(), String> {
    if settings.query_turn_max_concurrent == 0 {
        return Err("query_turn_max_concurrent must be greater than zero".into());
    }
    if settings.query_turn_admission_timeout_ms == 0 {
        return Err("query_turn_admission_timeout_ms must be greater than zero".into());
    }
    Ok(())
}

pub(super) fn validate_login_rate_limit_settings(settings: &Settings) -> Result<(), String> {
    if settings.login_rate_limit_per_login_max_attempts == 0 {
        return Err("login_rate_limit_per_login_max_attempts must be greater than zero".into());
    }
    if settings.login_rate_limit_per_login_window_seconds == 0 {
        return Err("login_rate_limit_per_login_window_seconds must be greater than zero".into());
    }
    if settings.login_rate_limit_per_source_max_attempts == 0 {
        return Err("login_rate_limit_per_source_max_attempts must be greater than zero".into());
    }
    if settings.login_rate_limit_per_source_window_seconds == 0 {
        return Err("login_rate_limit_per_source_window_seconds must be greater than zero".into());
    }
    if settings.login_rate_limit_max_concurrent_verifications == 0 {
        return Err(
            "login_rate_limit_max_concurrent_verifications must be greater than zero".into()
        );
    }
    if settings.login_rate_limit_admission_timeout_ms == 0 {
        return Err("login_rate_limit_admission_timeout_ms must be greater than zero".into());
    }
    if settings.login_rate_limit_registry_max_entries == 0 {
        return Err("login_rate_limit_registry_max_entries must be greater than zero".into());
    }
    if settings.login_rate_limit_registry_idle_ttl_seconds == 0 {
        return Err("login_rate_limit_registry_idle_ttl_seconds must be greater than zero".into());
    }
    if settings.login_rate_limit_per_login_max_attempts > MAX_LOGIN_RATE_LIMIT_MAX_ATTEMPTS {
        return Err(format!(
            "login_rate_limit_per_login_max_attempts must not exceed {MAX_LOGIN_RATE_LIMIT_MAX_ATTEMPTS}"
        ));
    }
    if settings.login_rate_limit_per_source_max_attempts > MAX_LOGIN_RATE_LIMIT_MAX_ATTEMPTS {
        return Err(format!(
            "login_rate_limit_per_source_max_attempts must not exceed {MAX_LOGIN_RATE_LIMIT_MAX_ATTEMPTS}"
        ));
    }
    if settings.login_rate_limit_trusted_proxy_hops > MAX_LOGIN_RATE_LIMIT_TRUSTED_PROXY_HOPS {
        return Err(format!(
            "login_rate_limit_trusted_proxy_hops must not exceed {MAX_LOGIN_RATE_LIMIT_TRUSTED_PROXY_HOPS}"
        ));
    }
    Ok(())
}

pub(super) fn validate_recognition_settings(settings: &Settings) -> Result<(), String> {
    let engine = settings
        .recognition_default_raster_image_engine
        .parse::<RecognitionEngine>()
        .map_err(|error| format!("recognition_default_raster_image_engine: {error}"))?;
    let policy = LibraryRecognitionPolicy { raster_image_engine: engine };
    policy.validate().map_err(|error| format!("recognition_default_raster_image_engine: {error}"))
}

pub(super) fn validate_runtime_agent_settings(settings: &Settings) -> Result<(), String> {
    if settings.runtime_agent_max_turns == 0 {
        return Err("runtime_agent_max_turns must be greater than zero".into());
    }
    if settings.runtime_agent_max_parallel_actions == 0 {
        return Err("runtime_agent_max_parallel_actions must be greater than zero".into());
    }
    if settings.runtime_trace_payload_budget_bytes == 0 {
        return Err("runtime_trace_payload_budget_bytes must be greater than zero".into());
    }
    if settings.runtime_policy_reason_budget_chars == 0 {
        return Err("runtime_policy_reason_budget_chars must be greater than zero".into());
    }
    for task_kind in parse_runtime_policy_csv(settings.runtime_policy_reject_task_kinds.as_ref()) {
        task_kind
            .parse::<crate::domains::agent_runtime::RuntimeTaskKind>()
            .map_err(|error| format!("runtime_policy_reject_task_kinds contains {error}"))?;
    }
    for target_kind in
        parse_runtime_policy_csv(settings.runtime_policy_reject_target_kinds.as_ref())
    {
        let parsed = target_kind
            .parse::<crate::domains::agent_runtime::RuntimeDecisionTargetKind>()
            .map_err(|error| format!("runtime_policy_reject_target_kinds contains {error}"))?;
        if matches!(
            parsed,
            crate::domains::agent_runtime::RuntimeDecisionTargetKind::ModelRequest
                | crate::domains::agent_runtime::RuntimeDecisionTargetKind::ToolRequest
                | crate::domains::agent_runtime::RuntimeDecisionTargetKind::ToolResult
        ) {
            return Err(format!(
                "runtime_policy_reject_target_kinds contains {target_kind}, which is not \
                 evaluated against any live dispatch path; only stage_transition and \
                 final_outcome are currently enforced"
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_query_rerank_settings(settings: &Settings) -> Result<(), String> {
    if !settings.query_rerank_enabled
        && settings.query_semantic_rerank_mode != SemanticRerankMode::Off
    {
        return Err(
            "query_semantic_rerank_mode requires query_rerank_enabled=true for shadow or active"
                .into(),
        );
    }
    // An `active` rerank only attempts the provider when its decision budget
    // fits the compile-time turn wait budget. A larger configured timeout would
    // make every turn fall back to lexical ordering without a provider call, so
    // the deployment is rejected at startup instead of degrading silently.
    if settings.query_semantic_rerank_mode == SemanticRerankMode::Active
        && settings.query_semantic_rerank_timeout_ms
            > crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS
    {
        return Err(format!(
            "query_semantic_rerank_timeout_ms={} exceeds the {} ms turn wait budget; an active \
             semantic rerank would never call the provider and degrade silently. Lower \
             IRONRAG_QUERY_SEMANTIC_RERANK_TIMEOUT_MS to at most {} or set mode to shadow/off",
            settings.query_semantic_rerank_timeout_ms,
            crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS,
            crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS,
        ));
    }
    Ok(())
}

pub(super) fn validate_release_monitor_settings(settings: &Settings) -> Result<(), String> {
    let repository = settings.release_check_repository.trim();
    let mut components = repository.split('/');
    let owner = components.next().unwrap_or_default();
    let repo = components.next().unwrap_or_default();
    let has_exactly_two_components = components.next().is_none();
    let is_valid_component = |value: &str| {
        !value.is_empty()
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
    };

    if !(has_exactly_two_components && is_valid_component(owner) && is_valid_component(repo)) {
        return Err(
            "release_check_repository must be a GitHub repository slug like owner/repo".into()
        );
    }
    if settings.release_check_interval_hours == 0 {
        return Err("release_check_interval_hours must be greater than zero".into());
    }

    Ok(())
}

pub(super) fn validate_graph_gc_settings(settings: &Settings) -> Result<(), String> {
    if settings.graph_gc_hours == 0 {
        return Err("graph_gc_hours must be greater than zero".into());
    }
    Ok(())
}

fn parse_runtime_policy_csv(value: Option<&String>) -> Vec<&str> {
    value
        .map(std::string::String::as_str)
        .map(|raw| {
            raw.split(',').map(str::trim).filter(|item| !item.is_empty()).collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn validate_mcp_memory_settings(settings: &Settings) -> Result<(), String> {
    if settings.mcp_memory_default_read_window_chars == 0 {
        return Err("mcp_memory_default_read_window_chars must be greater than zero".into());
    }
    if settings.mcp_memory_max_read_window_chars == 0 {
        return Err("mcp_memory_max_read_window_chars must be greater than zero".into());
    }
    if settings.mcp_memory_default_read_window_chars > settings.mcp_memory_max_read_window_chars {
        return Err(
            "mcp_memory_default_read_window_chars must be less than or equal to mcp_memory_max_read_window_chars"
                .into(),
        );
    }
    if settings.mcp_memory_default_search_limit == 0 {
        return Err("mcp_memory_default_search_limit must be greater than zero".into());
    }
    if settings.mcp_memory_max_search_limit == 0 {
        return Err("mcp_memory_max_search_limit must be greater than zero".into());
    }
    if settings.mcp_memory_default_search_limit > settings.mcp_memory_max_search_limit {
        return Err(
            "mcp_memory_default_search_limit must be less than or equal to mcp_memory_max_search_limit"
                .into(),
        );
    }
    if settings.mcp_memory_idempotency_retention_hours == 0 {
        return Err("mcp_memory_idempotency_retention_hours must be greater than zero".into());
    }
    Ok(())
}

pub(super) fn validate_ui_bootstrap_admin_settings(settings: &Settings) -> Result<(), String> {
    let login_set = settings
        .ui_bootstrap_admin_login
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let password_set = settings
        .ui_bootstrap_admin_password
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let email_set = settings
        .ui_bootstrap_admin_email
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if login_set == password_set && password_set == email_set {
        return validate_bootstrap_admin_api_token(settings);
    }
    let missing_field = if !login_set {
        "ui_bootstrap_admin_login"
    } else if !password_set {
        "ui_bootstrap_admin_password"
    } else {
        "ui_bootstrap_admin_email"
    };
    Err(format!(
        "ui_bootstrap_admin_login, ui_bootstrap_admin_email, and ui_bootstrap_admin_password must all be set or all unset; {missing_field} is missing"
    ))
}

fn validate_bootstrap_admin_api_token(settings: &Settings) -> Result<(), String> {
    if settings
        .ui_bootstrap_admin_api_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|token| !is_well_formed_api_token(token))
    {
        return Err("ui_bootstrap_admin_api_token must use the inrg_ token format".into());
    }

    Ok(())
}
