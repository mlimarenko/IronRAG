use std::path::Path;
use std::process::Command;

use anyhow::Context as _;

const CHILD_MODE_ENV: &str = "IRONRAG_OBSERVABILITY_SMOKE_CHILD";

const ENDPOINT_ENV_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
];

#[test]
fn init_tracing_without_opt_in_keeps_export_disabled() -> anyhow::Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--exact")
        .arg("init_tracing_child_without_opt_in")
        .env(CHILD_MODE_ENV, "without_opt_in")
        .env_remove("IRONRAG_OTEL_ENABLED")
        .env_remove("IRONRAG_DEPLOYMENT_ID")
        .env_remove("OTEL_EXPORTER_OTLP_PROTOCOL");
    for name in ENDPOINT_ENV_VARS {
        command.env_remove(name);
    }

    let output = command.output()?;
    assert!(
        output.status.success(),
        "observability default-off smoke child failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        logs.contains("observability: disabled"),
        "fresh install must report OTLP export as disabled; logs:\n{logs}",
    );
    assert!(
        !logs.contains("observability: enabled"),
        "fresh install unexpectedly enabled OTLP export; logs:\n{logs}",
    );
    Ok(())
}

#[test]
fn init_tracing_disabled_by_flag_succeeds() -> anyhow::Result<()> {
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("init_tracing_child_disabled_by_flag")
        .env(CHILD_MODE_ENV, "disabled_by_flag")
        .env("IRONRAG_OTEL_ENABLED", "false")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_PROTOCOL")
        .status()?;

    assert!(status.success(), "observability disabled-flag smoke child failed: {status}");
    Ok(())
}

#[test]
fn init_tracing_with_fake_endpoint_succeeds() -> anyhow::Result<()> {
    let output = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("init_tracing_child_with_fake_endpoint")
        .env(CHILD_MODE_ENV, "with_fake_endpoint")
        .env("IRONRAG_OTEL_ENABLED", "true")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:0")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .output()?;

    assert!(
        output.status.success(),
        "observability fake-endpoint smoke child failed: {}",
        output.status,
    );
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        logs.contains("observability: enabled"),
        "explicit opt-in did not initialize OTLP export; logs:\n{logs}",
    );
    Ok(())
}

#[test]
fn distribution_defaults_do_not_embed_an_owner_collector() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = [
        ".env.example",
        "apps/api/.env.example",
        "apps/web/Dockerfile",
        "charts/ironrag/values.yaml",
        "docker-compose.yml",
    ];
    for relative_path in files {
        let content = std::fs::read_to_string(root.join(relative_path))
            .with_context(|| format!("read distribution config {relative_path}"))?;
        assert!(
            !content.contains("otel.example.invalid"),
            "{relative_path} must not embed the project owner's collector",
        );
    }

    let compose = std::fs::read_to_string(root.join("docker-compose.yml"))?;
    assert!(compose.contains("IRONRAG_OTEL_ENABLED: ${IRONRAG_OTEL_ENABLED:-false}"));
    assert!(
        compose
            .contains("VITE_OTEL_EXPORTER_OTLP_ENDPOINT: ${VITE_OTEL_EXPORTER_OTLP_ENDPOINT:-}",),
    );

    let chart_values = std::fs::read_to_string(root.join("charts/ironrag/values.yaml"))?;
    assert!(chart_values.contains("observability:\n  # OTLP export is opt-in"));
    assert!(chart_values.contains("  enabled: false"));
    assert!(chart_values.contains("  otlpEndpoint: \"\""));

    let dockerfile = std::fs::read_to_string(root.join("apps/web/Dockerfile"))?;
    assert!(dockerfile.contains("ARG VITE_OTEL_EXPORTER_OTLP_ENDPOINT=\n"));
    assert!(!root.join("apps/api/observability.toml").exists());
    Ok(())
}

#[tokio::test]
async fn init_tracing_child_disabled_by_flag() -> anyhow::Result<()> {
    if std::env::var(CHILD_MODE_ENV).as_deref() != Ok("disabled_by_flag") {
        return Ok(());
    }

    ironrag_backend::observability::init_tracing(None)?;
    ironrag_backend::observability::shutdown_tracing().await;
    Ok(())
}

#[tokio::test]
async fn init_tracing_child_without_opt_in() -> anyhow::Result<()> {
    if std::env::var(CHILD_MODE_ENV).as_deref() != Ok("without_opt_in") {
        return Ok(());
    }

    ironrag_backend::observability::init_tracing(Some("must-not-be-exported".to_string()))?;
    ironrag_backend::observability::shutdown_tracing().await;
    Ok(())
}

#[tokio::test]
async fn init_tracing_child_with_fake_endpoint() -> anyhow::Result<()> {
    if std::env::var(CHILD_MODE_ENV).as_deref() != Ok("with_fake_endpoint") {
        return Ok(());
    }

    ironrag_backend::observability::init_tracing(None)?;
    ironrag_backend::observability::shutdown_tracing().await;
    Ok(())
}
