use super::*;

#[test]
fn rejects_invalid_mcp_memory_ranges() {
    let mut settings = sample_settings();
    settings.mcp_memory_default_read_window_chars = 10_000;
    settings.mcp_memory_max_read_window_chars = 100;

    let error = validate_mcp_memory_settings(&settings).expect_err("range should fail");
    assert!(error.contains("mcp_memory_default_read_window_chars"));
}

#[test]
fn rejects_invalid_runtime_agent_limits() {
    let mut settings = sample_settings();
    settings.runtime_agent_max_turns = 0;

    let error =
        validate_runtime_agent_settings(&settings).expect_err("runtime settings should fail");
    assert!(error.contains("runtime_agent_max_turns"));
}

#[test]
fn rejects_unenforced_runtime_policy_reject_target_kinds() {
    for unenforced in ["model_request", "tool_request", "tool_result"] {
        let mut settings = sample_settings();
        settings.runtime_policy_reject_target_kinds = Some(unenforced.to_string());

        let error = validate_runtime_agent_settings(&settings)
            .expect_err("unenforced target kind should fail validation");
        assert!(error.contains(unenforced));
    }

    for enforced in ["stage_transition", "final_outcome"] {
        let mut settings = sample_settings();
        settings.runtime_policy_reject_target_kinds = Some(enforced.to_string());

        validate_runtime_agent_settings(&settings).expect("enforced target kind should validate");
    }
}

#[test]
fn service_role_helpers_match_role() {
    let mut settings = sample_settings();

    settings.service_role = "api".into();
    assert!(settings.runs_http_api());
    assert!(!settings.runs_probe_http_api());
    assert!(!settings.runs_ingestion_workers());
    assert!(!settings.runs_startup_authority());

    settings.service_role = "worker".into();
    assert!(!settings.runs_http_api());
    assert!(settings.runs_probe_http_api());
    assert!(settings.runs_ingestion_workers());
    assert!(!settings.runs_startup_authority());

    settings.service_role = "startup".into();
    assert!(!settings.runs_http_api());
    assert!(!settings.runs_probe_http_api());
    assert!(!settings.runs_ingestion_workers());
    assert!(settings.runs_startup_authority());
}

#[test]
fn rejects_invalid_service_roles() {
    let mut settings = sample_settings();
    settings.service_role = "scheduler".into();

    let error = validate_service_role(&settings).expect_err("invalid role should fail");
    assert!(error.contains("service_role"));
}

#[test]
fn rejects_filesystem_cluster_topology() {
    let mut settings = sample_settings();
    settings.content_storage_topology = "shared_cluster".into();

    let error = validate_content_storage_settings(&settings).expect_err("shared cluster must fail");
    assert!(error.contains("content_storage_topology"));
}

#[test]
fn rejects_s3_provider_without_credentials() {
    let mut settings = sample_settings();
    settings.content_storage_provider = "s3".into();
    settings.dependency_object_storage_mode = "bundled".into();

    let error = validate_content_storage_settings(&settings).expect_err("s3 settings must fail");
    assert!(error.contains("content_storage_s3_bucket"));
}

#[test]
fn accepts_service_names_with_identity_safe_characters() {
    let mut settings = sample_settings();
    settings.service_name = "ironrag.worker_01-api".into();

    validate_service_name(&settings).expect("valid service name should pass");
}

#[test]
fn rejects_invalid_service_names() {
    let mut settings = sample_settings();
    settings.service_name = "worker:api".into();

    let error = validate_service_name(&settings).expect_err("invalid service name should fail");
    assert!(error.contains("service_name"));
}

#[test]
fn rejects_invalid_release_check_repository_slug() {
    let mut settings = sample_settings();
    settings.release_check_repository = "https://github.com/mlimarenko/IronRAG".into();

    let error = validate_release_monitor_settings(&settings)
        .expect_err("full urls should fail release repository validation");
    assert!(error.contains("release_check_repository"));
}

#[test]
fn rejects_zero_release_check_interval() {
    let mut settings = sample_settings();
    settings.release_check_interval_hours = 0;

    let error = validate_release_monitor_settings(&settings)
        .expect_err("zero interval should fail release monitor validation");
    assert!(error.contains("release_check_interval_hours"));
}

#[test]
fn rejects_zero_graph_gc_interval() {
    let mut settings = sample_settings();
    settings.graph_gc_hours = 0;

    let error =
        validate_graph_gc_settings(&settings).expect_err("zero interval should fail graph GC");
    assert!(error.contains("graph_gc_hours"));
}
