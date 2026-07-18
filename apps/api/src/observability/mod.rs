use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, anyhow};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{Context as OtelContext, KeyValue};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_http::{HeaderExtractor, HeaderInjector};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, Temporality};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_semantic_conventions::{
    attribute::DEPLOYMENT_ENVIRONMENT_NAME,
    resource::{SERVICE_NAME, SERVICE_VERSION},
};
use tracing::{Span, info, warn};
use tracing_opentelemetry::MetricsLayer;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use url::Url;
use uuid::Uuid;

const DEFAULT_LOG_FILTER: &str = "info";
const DEFAULT_SERVICE_NAME: &str = "ironrag-backend";
const DEFAULT_DEPLOYMENT_ENVIRONMENT: &str = "development";
const DEFAULT_OTEL_METRIC_INTERVAL_SECONDS: u64 = 30;
const OTEL_EXPORTER_OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const OTEL_EXPORTER_OTLP_PROTOCOL: &str = "OTEL_EXPORTER_OTLP_PROTOCOL";
const OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
const OTEL_EXPORTER_OTLP_METRICS_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT";
const OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT";
const OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE: &str =
    "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE";
const OTEL_RESOURCE_ATTRIBUTES: &str = "OTEL_RESOURCE_ATTRIBUTES";
const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";
const OTEL_SERVICE_VERSION: &str = "OTEL_SERVICE_VERSION";
const OTEL_DEPLOYMENT_ENVIRONMENT: &str = "OTEL_DEPLOYMENT_ENVIRONMENT";
const OTEL_TRACES_EXPORTER: &str = "OTEL_TRACES_EXPORTER";
const OTEL_METRICS_EXPORTER: &str = "OTEL_METRICS_EXPORTER";
const OTEL_LOGS_EXPORTER: &str = "OTEL_LOGS_EXPORTER";
const IRONRAG_OTEL_ENABLED: &str = "IRONRAG_OTEL_ENABLED";
const IRONRAG_SERVICE_NAME: &str = "IRONRAG_SERVICE_NAME";
const IRONRAG_SERVICE_ROLE: &str = "IRONRAG_SERVICE_ROLE";
const IRONRAG_ENVIRONMENT: &str = "IRONRAG_ENVIRONMENT";
const IRONRAG_DEPLOYMENT_ID: &str = "IRONRAG_DEPLOYMENT_ID";
const HOSTNAME_ENV: &str = "HOSTNAME";
const IRONRAG_LOG_FILTER: &str = "IRONRAG_LOG_FILTER";

/// Resolved per-deployment telemetry identity, set once during `init_tracing`.
static DEPLOYMENT_ID: OnceLock<Option<String>> = OnceLock::new();
/// Namespace UUID for `IronRAG` deployment identities (fixed, telemetry-only).
const DEPLOYMENT_ID_NAMESPACE: Uuid = Uuid::from_u128(0xa3f1_c2b4_5d6e_4f80_9a1b_2c3d_4e5f_6071);

static TRACER_PROVIDER: OnceLock<Mutex<Option<SdkTracerProvider>>> = OnceLock::new();
static METER_PROVIDER: OnceLock<Mutex<Option<SdkMeterProvider>>> = OnceLock::new();
static LOGGER_PROVIDER: OnceLock<Mutex<Option<SdkLoggerProvider>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
    HttpJson,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OtlpSignal {
    Traces,
    Metrics,
    Logs,
}

impl OtlpSignal {
    const fn endpoint_env(self) -> &'static str {
        match self {
            Self::Traces => OTEL_EXPORTER_OTLP_TRACES_ENDPOINT,
            Self::Metrics => OTEL_EXPORTER_OTLP_METRICS_ENDPOINT,
            Self::Logs => OTEL_EXPORTER_OTLP_LOGS_ENDPOINT,
        }
    }

    const fn exporter_env(self) -> &'static str {
        match self {
            Self::Traces => OTEL_TRACES_EXPORTER,
            Self::Metrics => OTEL_METRICS_EXPORTER,
            Self::Logs => OTEL_LOGS_EXPORTER,
        }
    }

    const fn http_path(self) -> &'static str {
        match self {
            Self::Traces => "/v1/traces",
            Self::Metrics => "/v1/metrics",
            Self::Logs => "/v1/logs",
        }
    }
}

/// Initializes canonical process tracing.
///
/// Without the explicit flag-plus-endpoint opt-in this installs only the existing formatted
/// tracing subscriber. After opt-in it adds the configured OpenTelemetry export layers to the
/// same subscriber; log export still requires its signal-specific opt-in.
///
/// # Errors
/// Returns an error when the subscriber or OTLP exporter cannot be installed.
pub fn init_tracing(deployment_id: Option<String>) -> anyhow::Result<()> {
    let filter = env_string(IRONRAG_LOG_FILTER).unwrap_or_else(|| DEFAULT_LOG_FILTER.to_string());
    let env_filter = crate::shared::telemetry::compose_env_filter(&filter);
    let endpoint = env_string(OTEL_EXPORTER_OTLP_ENDPOINT);
    let enabled = env_bool(IRONRAG_OTEL_ENABLED, false);
    let export_requested = is_otel_export_requested(enabled, endpoint.as_deref());

    // A stable deployment identifier exists only inside explicitly enabled
    // exports. Local formatted logs do not need a cross-process identifier,
    // and fresh installs must not derive one merely by starting the service.
    let deployment_id =
        export_requested.then(|| env_string(IRONRAG_DEPLOYMENT_ID).or(deployment_id)).flatten();
    let _ = DEPLOYMENT_ID.set(deployment_id);

    let Some(endpoint) = endpoint.filter(|_| enabled) else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_target(false))
            .try_init()
            .context("failed to initialize tracing subscriber")?;
        if enabled {
            info!("observability: no OTLP endpoint configured; exporter disabled");
        } else {
            info!("observability: disabled by IRONRAG_OTEL_ENABLED=false");
        }
        return Ok(());
    };

    // Trace-context extraction and propagation are part of the explicit OTLP
    // opt-in as well. Without export, outbound requests carry no telemetry
    // headers added by IronRAG.
    global::set_text_map_propagator(TraceContextPropagator::new());
    let protocol = resolve_otlp_protocol();
    let traces_enabled = signal_enabled(OtlpSignal::Traces);
    let metrics_enabled = signal_enabled(OtlpSignal::Metrics);
    let logs_enabled = signal_enabled(OtlpSignal::Logs);

    let tracer_provider =
        traces_enabled.then(|| build_tracer_provider(endpoint.as_str(), protocol)).transpose()?;
    let meter_provider =
        metrics_enabled.then(|| build_meter_provider(endpoint.as_str(), protocol)).transpose()?;
    let logger_provider =
        logs_enabled.then(|| build_logger_provider(endpoint.as_str(), protocol)).transpose()?;

    let tracer_layer = tracer_provider
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer("ironrag")));
    let metrics_layer = meter_provider.as_ref().map(|provider| MetricsLayer::new(provider.clone()));
    let logs_layer = logger_provider.as_ref().map(|provider| {
        OpenTelemetryTracingBridge::new(provider).with_filter(otel_log_export_filter(&filter))
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(tracer_layer)
        .with(metrics_layer)
        .with(logs_layer)
        .try_init()
        .context("failed to initialize tracing subscriber with OpenTelemetry")?;

    if let Some(provider) = tracer_provider {
        global::set_tracer_provider(provider.clone());
        store_tracer_provider(provider)?;
    }
    if let Some(provider) = meter_provider {
        global::set_meter_provider(provider.clone());
        store_meter_provider(provider)?;
    }
    if let Some(provider) = logger_provider {
        store_logger_provider(provider)?;
    }

    info!(
        monotonic_counter.ironrag.telemetry.events = 1_u64,
        endpoint_configured = true,
        protocol = ?protocol,
        traces = traces_enabled,
        metrics = metrics_enabled,
        logs = logs_enabled,
        service_name = %resolved_service_name(),
        service_role = ?env_string(IRONRAG_SERVICE_ROLE),
        "observability: enabled"
    );
    Ok(())
}

/// Flushes and shuts down the OpenTelemetry providers, when installed.
///
/// The three providers are flushed concurrently so an unreachable endpoint
/// stalls shutdown by at most one exporter timeout (~5s), not the sum across
/// all three signals. Each exporter's own `with_timeout` bounds the wait.
pub async fn shutdown_tracing() {
    let trace_shutdown = take_tracer_provider().map(|provider| {
        shutdown_observability_provider("trace", move || {
            let flush_result = provider.force_flush().map_err(|error| error.to_string());
            let shutdown_result = provider.shutdown().map_err(|error| error.to_string());
            vec![("force flush", flush_result), ("shutdown", shutdown_result)]
        })
    });
    let metric_shutdown = take_meter_provider().map(|provider| {
        shutdown_observability_provider("metric", move || {
            let flush_result = provider.force_flush().map_err(|error| error.to_string());
            let shutdown_result = provider.shutdown().map_err(|error| error.to_string());
            vec![("force flush", flush_result), ("shutdown", shutdown_result)]
        })
    });
    let log_shutdown = take_logger_provider().map(|provider| {
        shutdown_observability_provider("log", move || {
            let flush_result = provider.force_flush().map_err(|error| error.to_string());
            let shutdown_result = provider.shutdown().map_err(|error| error.to_string());
            vec![("force flush", flush_result), ("shutdown", shutdown_result)]
        })
    });
    tokio::join!(
        async {
            if let Some(future) = trace_shutdown {
                future.await;
            }
        },
        async {
            if let Some(future) = metric_shutdown {
                future.await;
            }
        },
        async {
            if let Some(future) = log_shutdown {
                future.await;
            }
        },
    );
}

async fn shutdown_observability_provider<F>(signal: &'static str, shutdown: F)
where
    F: FnOnce() -> Vec<(&'static str, Result<(), String>)> + Send + 'static,
{
    match tokio::task::spawn_blocking(shutdown).await {
        Ok(results) => {
            for (operation, result) in results {
                if let Err(error) = result {
                    warn!(signal, operation, error, "observability provider shutdown failed");
                }
            }
        }
        Err(error) => warn!(signal, error = %error, "observability provider shutdown task failed"),
    }
}

pub(crate) struct Tracer;

impl Tracer {
    pub(crate) fn set_span_parent_from_headers(span: &Span, headers: &http::HeaderMap) {
        let parent_context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderExtractor(headers))
        });
        let _ = span.set_parent(parent_context);
    }
}

pub fn inject_trace_context(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let mut headers = http::HeaderMap::new();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&OtelContext::current(), &mut HeaderInjector(&mut headers));
    });

    if headers.is_empty() { request } else { request.headers(headers) }
}

fn build_tracer_provider(
    endpoint: &str,
    protocol: OtlpProtocol,
) -> anyhow::Result<SdkTracerProvider> {
    let signal_endpoint = resolved_signal_endpoint(endpoint, protocol, OtlpSignal::Traces);
    let exporter = match protocol {
        OtlpProtocol::Grpc => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP gRPC span exporter")?,
        OtlpProtocol::HttpProtobuf => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::HttpBinary)
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP HTTP/protobuf span exporter")?,
        OtlpProtocol::HttpJson => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::HttpJson)
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP HTTP/json span exporter")?,
    };

    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(observability_resource())
        .build())
}

fn build_meter_provider(
    endpoint: &str,
    protocol: OtlpProtocol,
) -> anyhow::Result<SdkMeterProvider> {
    let signal_endpoint = resolved_signal_endpoint(endpoint, protocol, OtlpSignal::Metrics);
    let exporter = match protocol {
        OtlpProtocol::Grpc => opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::Grpc)
            .with_temporality(resolve_metrics_temporality())
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP gRPC metric exporter")?,
        OtlpProtocol::HttpProtobuf => opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::HttpBinary)
            .with_temporality(resolve_metrics_temporality())
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP HTTP/protobuf metric exporter")?,
        OtlpProtocol::HttpJson => opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::HttpJson)
            .with_temporality(resolve_metrics_temporality())
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP HTTP/json metric exporter")?,
    };
    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(DEFAULT_OTEL_METRIC_INTERVAL_SECONDS))
        .build();
    Ok(SdkMeterProvider::builder()
        .with_resource(observability_resource())
        .with_reader(reader)
        .build())
}

fn build_logger_provider(
    endpoint: &str,
    protocol: OtlpProtocol,
) -> anyhow::Result<SdkLoggerProvider> {
    let signal_endpoint = resolved_signal_endpoint(endpoint, protocol, OtlpSignal::Logs);
    let exporter = match protocol {
        OtlpProtocol::Grpc => opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP gRPC log exporter")?,
        OtlpProtocol::HttpProtobuf => opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::HttpBinary)
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP HTTP/protobuf log exporter")?,
        OtlpProtocol::HttpJson => opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint)
            .with_protocol(Protocol::HttpJson)
            .with_timeout(Duration::from_secs(5))
            .build()
            .context("failed to build OTLP HTTP/json log exporter")?,
    };
    Ok(SdkLoggerProvider::builder()
        .with_resource(observability_resource())
        .with_batch_exporter(exporter)
        .build())
}

fn observability_resource() -> Resource {
    let mut attributes = parse_otel_resource_attributes();
    attributes.insert(SERVICE_NAME.to_string(), resolved_service_name());
    attributes.insert(
        SERVICE_VERSION.to_string(),
        env_string(OTEL_SERVICE_VERSION).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
    );
    attributes.insert(DEPLOYMENT_ENVIRONMENT_NAME.to_string(), resolved_deployment_environment());
    if let Some(Some(deployment_id)) = DEPLOYMENT_ID.get() {
        attributes.insert("ironrag.deployment.id".to_string(), deployment_id.clone());
    }
    for (key, value) in inferred_runtime_resource_attributes() {
        attributes.entry(key).or_insert(value);
    }
    Resource::builder_empty()
        .with_attributes(attributes.into_iter().map(|(key, value)| KeyValue::new(key, value)))
        .build()
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn env_bool(name: &str, default: bool) -> bool {
    let Some(value) = env_string(name) else {
        return default;
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn resolve_otlp_protocol() -> OtlpProtocol {
    let Some(raw_value) = env_string(OTEL_EXPORTER_OTLP_PROTOCOL) else {
        return OtlpProtocol::Grpc;
    };
    let value = raw_value.to_ascii_lowercase();
    match value.as_str() {
        "grpc" => OtlpProtocol::Grpc,
        "http/protobuf" | "http/proto" | "protobuf" => OtlpProtocol::HttpProtobuf,
        "http/json" | "json" => OtlpProtocol::HttpJson,
        _ => {
            warn!(
                protocol = %raw_value,
                "observability: unsupported OTLP protocol, falling back to grpc",
            );
            OtlpProtocol::Grpc
        }
    }
}

fn resolve_metrics_temporality() -> Temporality {
    let Some(raw_value) = env_string(OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE) else {
        return Temporality::Cumulative;
    };
    match raw_value.to_ascii_lowercase().as_str() {
        "cumulative" => Temporality::Cumulative,
        "delta" => Temporality::Delta,
        "lowmemory" | "low_memory" => Temporality::LowMemory,
        _ => {
            warn!(
                temporality = %raw_value,
                "observability: unsupported OTLP metric temporality, falling back to cumulative",
            );
            Temporality::Cumulative
        }
    }
}

fn signal_enabled(signal: OtlpSignal) -> bool {
    let raw_value = env_string(signal.exporter_env());
    let Some(raw_value) = raw_value.as_deref() else {
        // Traces and metrics carry operational data. Logs may contain query or
        // document content, so exporting them always requires a second,
        // signal-specific opt-in even after OTLP itself has been enabled.
        return signal_enabled_by_default(signal);
    };
    let enabled = resolve_signal_exporter(signal, Some(raw_value));
    if !matches!(raw_value.to_ascii_lowercase().as_str(), "otlp" | "none") {
        warn!(
            exporter = %raw_value,
            signal = ?signal,
            "observability: unsupported OTEL exporter; signal disabled",
        );
    }
    enabled
}

fn resolve_signal_exporter(signal: OtlpSignal, raw_value: Option<&str>) -> bool {
    let Some(raw_value) = raw_value else {
        return signal_enabled_by_default(signal);
    };
    match raw_value.to_ascii_lowercase().as_str() {
        "otlp" => true,
        "none" => false,
        // Never turn a typo into data export. This matters most for logs,
        // which can contain user content, and is a safer policy for every
        // signal.
        _ => false,
    }
}

const fn signal_enabled_by_default(signal: OtlpSignal) -> bool {
    !matches!(signal, OtlpSignal::Logs)
}

fn resolved_signal_endpoint(endpoint: &str, protocol: OtlpProtocol, signal: OtlpSignal) -> String {
    if let Some(signal_endpoint) = env_string(signal.endpoint_env()) {
        return signal_endpoint;
    }
    match protocol {
        OtlpProtocol::Grpc => endpoint.to_string(),
        OtlpProtocol::HttpProtobuf | OtlpProtocol::HttpJson => {
            append_http_signal_path(endpoint, signal)
        }
    }
}

fn append_http_signal_path(endpoint: &str, signal: OtlpSignal) -> String {
    let Ok(mut url) = Url::parse(endpoint) else {
        return endpoint.to_string();
    };
    if matches!(url.path(), "" | "/") {
        url.set_path(signal.http_path());
    }
    url.to_string()
}

fn parse_otel_resource_attributes() -> BTreeMap<String, String> {
    let Some(raw) = env_string(OTEL_RESOURCE_ATTRIBUTES) else {
        return BTreeMap::new();
    };

    raw.split(',').filter_map(parse_resource_attribute).collect()
}

fn parse_resource_attribute(entry: &str) -> Option<(String, String)> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

fn inferred_runtime_resource_attributes() -> Vec<(String, String)> {
    let mut attributes = Vec::new();
    if let Some(instance_id) = resolved_instance_id() {
        attributes.push(("service.instance.id".to_string(), instance_id));
    }
    if let Some(role) = env_string(IRONRAG_SERVICE_ROLE) {
        attributes.push(("ironrag.service.role".to_string(), role));
    }
    if let Some(hostname) = env_string(HOSTNAME_ENV) {
        attributes.push(("host.name".to_string(), hostname));
    }
    if let Some(machine_id) = read_non_empty_file("/etc/machine-id")
        .or_else(|| read_non_empty_file("/var/lib/dbus/machine-id"))
    {
        attributes.push(("host.id".to_string(), machine_id));
    }
    attributes.push(("process.pid".to_string(), std::process::id().to_string()));
    attributes
}

fn resolved_service_name() -> String {
    env_string(OTEL_SERVICE_NAME)
        .or_else(|| env_string(IRONRAG_SERVICE_NAME))
        .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string())
}

fn resolved_deployment_environment() -> String {
    env_string(OTEL_DEPLOYMENT_ENVIRONMENT)
        .or_else(|| env_string(IRONRAG_ENVIRONMENT))
        .unwrap_or_else(|| DEFAULT_DEPLOYMENT_ENVIRONMENT.to_string())
}

/// Resolves the stable per-deployment telemetry identity before `init_tracing`.
///
/// Returns immediately when OTLP export has not been explicitly enabled with
/// an endpoint. After opt-in, resolution uses the `IRONRAG_DEPLOYMENT_ID`
/// operator override or a `UUIDv5` derived from the Postgres cluster
/// `system_identifier`. Database failures remain non-fatal.
pub async fn resolve_deployment_id(database_url: &str) -> Option<String> {
    if !otel_export_requested() {
        return None;
    }
    if let Some(override_id) = env_string(IRONRAG_DEPLOYMENT_ID) {
        return Some(override_id);
    }
    match tokio::time::timeout(Duration::from_secs(8), read_pg_system_identifier(database_url))
        .await
    {
        Ok(Ok(system_identifier)) => Some(derive_deployment_id(&system_identifier)),
        Ok(Err(error)) => {
            warn!(
                error = %error,
                "observability: deployment id unresolved from database; attribution will omit it",
            );
            None
        }
        Err(_) => {
            warn!("observability: deployment id resolution timed out; attribution will omit it");
            None
        }
    }
}

fn otel_export_requested() -> bool {
    let endpoint = env_string(OTEL_EXPORTER_OTLP_ENDPOINT);
    is_otel_export_requested(env_bool(IRONRAG_OTEL_ENABLED, false), endpoint.as_deref())
}

const fn is_otel_export_requested(enabled: bool, endpoint: Option<&str>) -> bool {
    enabled && endpoint.is_some()
}

async fn read_pg_system_identifier(database_url: &str) -> anyhow::Result<String> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
        .context("connect to database for deployment id")?;
    let system_identifier =
        sqlx::query_scalar::<_, String>("SELECT system_identifier::text FROM pg_control_system()")
            .fetch_one(&pool)
            .await
            .context("read pg_control_system().system_identifier");
    pool.close().await;
    system_identifier
}

fn derive_deployment_id(seed: &str) -> String {
    Uuid::new_v5(&DEPLOYMENT_ID_NAMESPACE, seed.as_bytes()).to_string()
}

fn resolved_instance_id() -> Option<String> {
    let service_name = resolved_service_name();
    let service_role = env_string(IRONRAG_SERVICE_ROLE).unwrap_or_else(|| "unknown".to_string());
    let host_fingerprint = resolved_runtime_fingerprint()?;
    Some(format!(
        "{}:{}:{}",
        sanitize_resource_component(&service_name),
        sanitize_resource_component(&service_role),
        sanitize_resource_component(&host_fingerprint),
    ))
}

fn resolved_runtime_fingerprint() -> Option<String> {
    env_string(HOSTNAME_ENV)
        .or_else(|| read_non_empty_file("/etc/hostname"))
        .or_else(|| read_non_empty_file("/etc/machine-id"))
        .or_else(|| read_non_empty_file("/var/lib/dbus/machine-id"))
}

fn read_non_empty_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn sanitize_resource_component(value: &str) -> String {
    let mut sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        sanitized = "unknown".to_string();
    }
    sanitized
}

fn otel_log_export_filter(filter: &str) -> EnvFilter {
    let mut directives = String::from(filter);
    for directive in [
        "opentelemetry=off",
        "opentelemetry_otlp=off",
        "opentelemetry_sdk=off",
        "tonic=off",
        "h2=off",
        "hyper=off",
        "hyper_util=off",
        "reqwest=off",
    ] {
        directives.push(',');
        directives.push_str(directive);
    }
    EnvFilter::new(directives)
}

fn tracer_provider_slot() -> &'static Mutex<Option<SdkTracerProvider>> {
    TRACER_PROVIDER.get_or_init(|| Mutex::new(None))
}

fn meter_provider_slot() -> &'static Mutex<Option<SdkMeterProvider>> {
    METER_PROVIDER.get_or_init(|| Mutex::new(None))
}

fn logger_provider_slot() -> &'static Mutex<Option<SdkLoggerProvider>> {
    LOGGER_PROVIDER.get_or_init(|| Mutex::new(None))
}

fn store_tracer_provider(provider: SdkTracerProvider) -> anyhow::Result<()> {
    let mut guard = tracer_provider_slot()
        .lock()
        .map_err(|_| anyhow!("observability tracer provider lock poisoned"))?;
    if guard.is_some() {
        anyhow::bail!("observability tracer provider already initialized");
    }
    *guard = Some(provider);
    drop(guard);
    Ok(())
}

fn store_meter_provider(provider: SdkMeterProvider) -> anyhow::Result<()> {
    let mut guard = meter_provider_slot()
        .lock()
        .map_err(|_| anyhow!("observability meter provider lock poisoned"))?;
    if guard.is_some() {
        anyhow::bail!("observability meter provider already initialized");
    }
    *guard = Some(provider);
    drop(guard);
    Ok(())
}

fn store_logger_provider(provider: SdkLoggerProvider) -> anyhow::Result<()> {
    let mut guard = logger_provider_slot()
        .lock()
        .map_err(|_| anyhow!("observability logger provider lock poisoned"))?;
    if guard.is_some() {
        anyhow::bail!("observability logger provider already initialized");
    }
    *guard = Some(provider);
    drop(guard);
    Ok(())
}

fn take_tracer_provider() -> Option<SdkTracerProvider> {
    tracer_provider_slot().lock().ok().and_then(|mut guard| guard.take())
}

fn take_meter_provider() -> Option<SdkMeterProvider> {
    meter_provider_slot().lock().ok().and_then(|mut guard| guard.take())
}

fn take_logger_provider() -> Option<SdkLoggerProvider> {
    logger_provider_slot().lock().ok().and_then(|mut guard| guard.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_http_signal_path_only_for_collector_base_endpoint() {
        assert_eq!(
            append_http_signal_path("http://collector:4318", OtlpSignal::Traces),
            "http://collector:4318/v1/traces",
        );
        assert_eq!(
            append_http_signal_path("http://collector:4318/", OtlpSignal::Logs),
            "http://collector:4318/v1/logs",
        );
        assert_eq!(
            append_http_signal_path("http://collector:4318/custom/logs", OtlpSignal::Logs),
            "http://collector:4318/custom/logs",
        );
    }

    #[test]
    fn sanitizes_resource_components_for_instance_id() {
        assert_eq!(sanitize_resource_component("api:worker/1"), "api_worker_1");
        assert_eq!(sanitize_resource_component(""), "unknown");
    }

    #[test]
    fn derives_stable_deployment_id_from_seed() {
        let first = derive_deployment_id("7401234567890123456");
        let second = derive_deployment_id("7401234567890123456");
        let other = derive_deployment_id("7409999999999999999");
        assert_eq!(first, second, "same system_identifier must map to the same id");
        assert_ne!(first, other, "different system_identifier must map to a different id");
        assert!(Uuid::parse_str(&first).is_ok(), "derived id must be a valid UUID");
    }

    #[test]
    fn log_export_requires_signal_specific_opt_in() {
        assert!(signal_enabled_by_default(OtlpSignal::Traces));
        assert!(signal_enabled_by_default(OtlpSignal::Metrics));
        assert!(!signal_enabled_by_default(OtlpSignal::Logs));
        assert!(resolve_signal_exporter(OtlpSignal::Logs, Some("otlp")));
        assert!(!resolve_signal_exporter(OtlpSignal::Logs, Some("none")));
        assert!(!resolve_signal_exporter(OtlpSignal::Logs, Some("typo")));
        assert!(!resolve_signal_exporter(OtlpSignal::Traces, Some("typo")));
    }

    #[test]
    fn otlp_export_requires_flag_and_endpoint() {
        assert!(!is_otel_export_requested(false, None));
        assert!(!is_otel_export_requested(true, None));
        assert!(!is_otel_export_requested(false, Some("http://collector:4317")));
        assert!(is_otel_export_requested(true, Some("http://collector:4317")));
    }
}
