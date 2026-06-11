use std::process::Command;

const CHILD_MODE_ENV: &str = "IRONRAG_OBSERVABILITY_SMOKE_CHILD";

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
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("init_tracing_child_with_fake_endpoint")
        .env(CHILD_MODE_ENV, "with_fake_endpoint")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:0")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .status()?;

    assert!(status.success(), "observability fake-endpoint smoke child failed: {status}");
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
async fn init_tracing_child_with_fake_endpoint() -> anyhow::Result<()> {
    if std::env::var(CHILD_MODE_ENV).as_deref() != Ok("with_fake_endpoint") {
        return Ok(());
    }

    ironrag_backend::observability::init_tracing(None)?;
    ironrag_backend::observability::shutdown_tracing().await;
    Ok(())
}
