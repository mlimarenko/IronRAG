use std::collections::BTreeMap;
use std::future::Future;
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

pub mod metrics;

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
const HOSTNAME_ENV: &str = "HOSTNAME";
const IRONRAG_LOG_FILTER: &str = "IRONRAG_LOG_FILTER";
/// Bounded queue depth of the non-blocking stdout appender. Once full, further
/// `tracing` events are dropped instead of blocking the emitting thread.
const LOG_APPENDER_BUFFER_LINES: usize = 8192;

/// Resolved per-deployment telemetry identity, set once during `init_tracing`.
static DEPLOYMENT_ID: OnceLock<Option<String>> = OnceLock::new();
/// Namespace UUID for `IronRAG` deployment identities (fixed, telemetry-only).
const DEPLOYMENT_ID_NAMESPACE: Uuid = Uuid::from_u128(0xa3f1_c2b4_5d6e_4f80_9a1b_2c3d_4e5f_6071);

/// Opaque database-cluster identity shared by runtime telemetry and benchmark
/// evidence. The raw PostgreSQL system identifier is never exposed.
pub fn deployment_id() -> Option<&'static str> {
    DEPLOYMENT_ID.get().and_then(|value| value.as_deref())
}

static TRACER_PROVIDER: OnceLock<Mutex<Option<SdkTracerProvider>>> = OnceLock::new();
static METER_PROVIDER: OnceLock<Mutex<Option<SdkMeterProvider>>> = OnceLock::new();
static LOGGER_PROVIDER: OnceLock<Mutex<Option<SdkLoggerProvider>>> = OnceLock::new();
/// Keeps the non-blocking stdout writer thread alive for the process lifetime,
/// and hands it to `shutdown_tracing` for a bounded final flush.
static LOG_APPENDER_GUARD: OnceLock<Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    OnceLock::new();

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

/// Bounded, lossy, non-blocking appender shared by both `init_tracing` branches.
/// The dedicated writer thread performs the blocking `write(2)`; callers only
/// enqueue and drop once the buffer is full, so a stalled sink cannot block a
/// tokio worker.
fn lossy_non_blocking_writer<W>(
    sink: W,
) -> (tracing_appender::non_blocking::NonBlocking, tracing_appender::non_blocking::WorkerGuard)
where
    W: std::io::Write + Send + 'static,
{
    tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(LOG_APPENDER_BUFFER_LINES)
        .finish(sink)
}

/// Initializes process tracing.
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
    // The runtime API and telemetry use the same opaque identity derived from
    // PostgreSQL. Keeping it independently of exporter configuration lets an
    // evidence collector prove that it benchmarked the intended database.
    let _ = DEPLOYMENT_ID.set(deployment_id);

    let (log_writer, log_guard) = lossy_non_blocking_writer(std::io::stdout());

    let Some(endpoint) = endpoint.filter(|_| enabled) else {
        return init_without_exporter(env_filter, log_writer, log_guard, enabled);
    };

    // Trace-context extraction and propagation are part of the explicit OTLP
    // opt-in as well. Without export, outbound requests carry no telemetry
    // headers added by IronRAG.
    global::set_text_map_propagator(TraceContextPropagator::new());
    let protocol = resolve_otlp_protocol();
    let traces_enabled = signal_enabled(OtlpSignal::Traces);
    let metrics_enabled = signal_enabled(OtlpSignal::Metrics);
    let logs_enabled = signal_enabled(OtlpSignal::Logs);
    let providers = build_otlp_providers(
        endpoint.as_str(),
        protocol,
        traces_enabled,
        metrics_enabled,
        logs_enabled,
    )?;
    install_otlp_subscriber(env_filter, log_writer, &filter, &providers)?;
    store_log_appender(log_guard)?;
    install_otlp_providers(providers)?;

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

/// Installs the formatted stdout subscriber when OTLP export is off or the
/// endpoint is absent. Kept separate so [`init_tracing`] reads as a decision
/// between two installation paths rather than one long branch.
fn init_without_exporter(
    env_filter: EnvFilter,
    log_writer: tracing_appender::non_blocking::NonBlocking,
    log_guard: tracing_appender::non_blocking::WorkerGuard,
    enabled: bool,
) -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(false).with_writer(log_writer))
        .try_init()
        .context("failed to initialize tracing subscriber")?;
    store_log_appender(log_guard)?;
    let message = exporter_disabled_message(enabled);
    info!("{message}");
    Ok(())
}

fn exporter_disabled_message(enabled: bool) -> &'static str {
    if enabled {
        "observability: no OTLP endpoint configured; exporter disabled"
    } else {
        "observability: disabled by IRONRAG_OTEL_ENABLED=false"
    }
}

/// The OTLP providers selected by the signal opt-ins, if any.
struct OtlpProviders {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
    logger: Option<SdkLoggerProvider>,
}

/// Builds the enabled signal exporters in one place. Signals are resolved by
/// the caller so the same booleans drive the startup log line.
fn build_otlp_providers(
    endpoint: &str,
    protocol: OtlpProtocol,
    traces: bool,
    metrics: bool,
    logs: bool,
) -> anyhow::Result<OtlpProviders> {
    Ok(OtlpProviders {
        tracer: traces.then(|| build_tracer_provider(endpoint, protocol)).transpose()?,
        meter: metrics.then(|| build_meter_provider(endpoint, protocol)).transpose()?,
        logger: logs.then(|| build_logger_provider(endpoint, protocol)).transpose()?,
    })
}

/// Installs the subscriber registry with the OTLP layers for the providers
/// that were built. The main log filter still gates what reaches each layer;
/// log export additionally applies its own signal-specific filter.
fn install_otlp_subscriber(
    env_filter: EnvFilter,
    log_writer: tracing_appender::non_blocking::NonBlocking,
    filter: &str,
    providers: &OtlpProviders,
) -> anyhow::Result<()> {
    let tracer_layer = providers
        .tracer
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer("ironrag")));
    let metrics_layer =
        providers.meter.as_ref().map(|provider| MetricsLayer::new(provider.clone()));
    let logs_layer = providers.logger.as_ref().map(|provider| {
        OpenTelemetryTracingBridge::new(provider).with_filter(otel_log_export_filter(filter))
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(false).with_writer(log_writer))
        .with(tracer_layer)
        .with(metrics_layer)
        .with(logs_layer)
        .try_init()
        .context("failed to initialize tracing subscriber with OpenTelemetry")?;
    Ok(())
}

/// Publishes the built providers as globals and stores them for shutdown.
fn install_otlp_providers(providers: OtlpProviders) -> anyhow::Result<()> {
    if let Some(provider) = providers.tracer {
        global::set_tracer_provider(provider.clone());
        store_tracer_provider(provider)?;
    }
    if let Some(provider) = providers.meter {
        global::set_meter_provider(provider.clone());
        store_meter_provider(provider)?;
    }
    if let Some(provider) = providers.logger {
        store_logger_provider(provider)?;
    }
    Ok(())
}

/// Flushes and shuts down the OpenTelemetry providers, when installed.
///
/// The three providers are flushed concurrently so an unreachable endpoint
/// stalls shutdown by at most one exporter timeout (~5s), not the sum across
/// all three signals. Each exporter's own `with_timeout` bounds the wait.
#[allow(clippy::print_stderr)] // a stalled tracing appender would swallow the timeout notice
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

    // `WorkerGuard::drop` does not join the writer thread. It sends a shutdown message with a
    // ~100 ms send timeout, then waits up to ~1 s; a full bounded channel may abandon queued lines
    // after ~100 ms even on a healthy sink, and its failure path prints directly to stdout — so a
    // stalled stdout can block the drop, hence the blocking pool and the bounded wait below.
    if let Some(guard) = take_log_appender() {
        let flush = tokio::task::spawn_blocking(move || drop(guard));
        if tokio::time::timeout(Duration::from_secs(2), flush).await.is_err() {
            // The stalled appender would swallow a `tracing` event, so report on stderr directly.
            eprintln!(
                "observability: log appender flush timed out after 2s; queued log lines dropped and the writer thread is abandoned"
            );
        }
    }
}

async fn shutdown_observability_provider<F>(signal: &'static str, shutdown: F)
where
    F: FnOnce() -> Vec<(&'static str, Result<(), String>)> + Send + 'static,
{
    match tokio::task::spawn_blocking(shutdown).await {
        Ok(results) => report_observability_shutdown(signal, &results),
        Err(error) => warn!(signal, error = %error, "observability provider shutdown task failed"),
    }
}

/// Reports the per-operation results of one provider's blocking flush and
/// shutdown. Extracted so the async shutdown wrapper stays a single match.
fn report_observability_shutdown(
    signal: &'static str,
    results: &[(&'static str, Result<(), String>)],
) {
    for (operation, result) in results {
        if let Err(error) = result {
            warn!(signal, operation, error, "observability provider shutdown failed");
        }
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
/// The identity is a `UUIDv5` derived from the Postgres cluster
/// `system_identifier`, so the runtime endpoint, telemetry and lifecycle
/// inventory all bind to the same database without exposing its raw identifier.
/// Database failures remain non-fatal for normal service startup; lifecycle
/// evidence collection rejects a runtime where the identity is absent.
pub async fn resolve_deployment_id(postgres: &sqlx::PgPool) -> Option<String> {
    let identity =
        match tokio::time::timeout(Duration::from_secs(8), read_pg_system_identifier(postgres))
            .await
        {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    "observability: deployment id resolution timed out; attribution will omit it"
                );
                return None;
            }
        };
    deployment_id_from_database_result(identity)
}

fn deployment_id_from_database_result(
    identity: anyhow::Result<(String, i64, String)>,
) -> Option<String> {
    match identity {
        Ok(database_identity) => Some(derive_deployment_id(&database_identity)),
        Err(error) => {
            warn!(
                error = %error,
                "observability: deployment id unresolved from database; attribution will omit it",
            );
            None
        }
    }
}

#[cfg(test)]
const fn is_otel_export_requested(enabled: bool, endpoint: Option<&str>) -> bool {
    enabled && endpoint.is_some()
}

async fn read_pg_system_identifier(
    postgres: &sqlx::PgPool,
) -> anyhow::Result<(String, i64, String)> {
    sqlx::query_as::<_, (String, i64, String)>(
        "SELECT system_identifier::text, oid::bigint, datname::text \
         FROM pg_control_system(), pg_database WHERE datname = current_database()",
    )
    .fetch_one(postgres)
    .await
    .context("read PostgreSQL cluster and database identity")
}

fn derive_deployment_id(
    (system_identifier, database_oid, database_name): &(String, i64, String),
) -> String {
    let seed = format!("{system_identifier}\0{database_oid}\0{database_name}");
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

fn log_appender_slot() -> &'static Mutex<Option<tracing_appender::non_blocking::WorkerGuard>> {
    LOG_APPENDER_GUARD.get_or_init(|| Mutex::new(None))
}

fn store_log_appender(guard: tracing_appender::non_blocking::WorkerGuard) -> anyhow::Result<()> {
    let mut slot = log_appender_slot()
        .lock()
        .map_err(|_| anyhow!("observability log appender lock poisoned"))?;
    if slot.is_some() {
        anyhow::bail!("observability log appender already initialized");
    }
    *slot = Some(guard);
    drop(slot);
    Ok(())
}

fn take_log_appender() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    log_appender_slot().lock().ok().and_then(|mut guard| guard.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tracing_subscriber::fmt::MakeWriter as _;

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
        let first =
            derive_deployment_id(&("7401234567890123456".to_owned(), 16_384, "ironrag".to_owned()));
        let second =
            derive_deployment_id(&("7401234567890123456".to_owned(), 16_384, "ironrag".to_owned()));
        let other = derive_deployment_id(&(
            "7401234567890123456".to_owned(),
            16_385,
            "ironrag_test".to_owned(),
        ));
        assert_eq!(first, second, "same system_identifier must map to the same id");
        assert_ne!(first, other, "different database must map to a different id");
        assert!(Uuid::parse_str(&first).is_ok(), "derived id must be a valid UUID");
    }

    #[test]
    fn deployment_identity_is_omitted_only_on_database_error() {
        let identity = ("test-cluster".to_owned(), 16_384, "test-database".to_owned());
        assert_eq!(
            deployment_id_from_database_result(Ok(identity.clone())),
            Some(derive_deployment_id(&identity)),
        );
        assert_eq!(deployment_id_from_database_result(Err(anyhow!("database unavailable"))), None);
    }

    #[test]
    fn disabled_exporter_message_distinguishes_flag_from_missing_endpoint() {
        assert_eq!(
            exporter_disabled_message(true),
            "observability: no OTLP endpoint configured; exporter disabled",
        );
        assert_eq!(
            exporter_disabled_message(false),
            "observability: disabled by IRONRAG_OTEL_ENABLED=false",
        );
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

    /// Write end of a pipe whose read end is held open but never drained, so the
    /// kernel pipe buffer fills and every `write(2)` blocks. This is the stalled
    /// log sink from the IRG-135 incident.
    #[derive(Clone)]
    struct StalledSink {
        file: std::sync::Arc<std::fs::File>,
    }

    impl std::io::Write for StalledSink {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            let mut file: &std::fs::File = &self.file;
            file.write_all(buffer)?;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut file: &std::fs::File = &self.file;
            file.flush()
        }
    }

    /// The read end is forgotten so it stays open for the test's lifetime, and
    /// the write end is wrapped as a `File` so a clone shares the same pipe.
    fn stalled_sink() -> StalledSink {
        let (reader, writer) = std::io::pipe().expect("create stalled pipe");
        std::mem::forget(reader);
        let file = std::fs::File::from(std::os::fd::OwnedFd::from(writer));
        StalledSink { file: std::sync::Arc::new(file) }
    }

    #[test]
    fn stalled_log_sink_does_not_stall_the_runtime() {
        let sink = stalled_sink();
        let (non_blocking, guard) = lossy_non_blocking_writer(sink);
        // Dropping the guard on a permanently stalled sink can block and, when
        // the bounded channel is full, prints the library's own diagnostic to
        // the test's real stdout; leak the test guard to keep the test
        // deterministic and quiet.
        std::mem::forget(guard);
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_target(false).with_writer(non_blocking));
        let dispatch = tracing::Dispatch::new(subscriber);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build multi-thread runtime");

        let (tx, rx) = std::sync::mpsc::channel::<&'static str>();
        for _ in 0..4 {
            let dispatch = dispatch.clone();
            runtime.spawn(async move {
                for line in 0..100_000_u64 {
                    tracing::dispatcher::with_default(&dispatch, || {
                        tracing::info!(line, "noisy request log line");
                    });
                    if line % 32 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            });
        }
        // Let the producers actually run and fill the kernel pipe buffer before
        // the probe is scheduled. A synchronous writer then blocks both workers
        // inside write(2), so the probe can never run; the bounded appender
        // keeps them enqueueing and schedulable.
        std::thread::sleep(std::time::Duration::from_millis(200));
        runtime.spawn(async move {
            let _ = tx.send("health-ok");
        });

        let scheduled = rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok();
        // Leak the runtime before asserting: if a regression makes the writer
        // synchronous and the assertion below fails, unwinding must not enter
        // `Runtime::drop`, which would join worker threads blocked in `write(2)`
        // and hang the harness.
        std::mem::forget(runtime);
        assert!(
            scheduled,
            "runtime stalled: probe task did not run within 3s while the log sink was blocked",
        );
    }

    #[test]
    fn lossy_appender_drops_instead_of_blocking() {
        let sink = stalled_sink();
        let (writer, guard) = lossy_non_blocking_writer(sink);
        // Dropping the guard on a permanently stalled sink can block and, when
        // the bounded channel is full, prints the library's own diagnostic to
        // the test's real stdout; leak it to keep the test deterministic and
        // quiet.
        std::mem::forget(guard);
        let started = std::time::Instant::now();
        {
            let mut handle = writer.make_writer();
            for line in 0..(LOG_APPENDER_BUFFER_LINES * 4) {
                handle
                    .write_all(format!("noisy log line {line}\n").as_bytes())
                    .expect("lossy appender write");
            }
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "lossy appender blocked on a stalled sink for {:?}",
            started.elapsed(),
        );
    }
}
tokio::task_local! {
    static PROVIDER_CALL_ID: String;
}

pub async fn with_provider_call_id<T>(
    provider_call_id: Uuid,
    future: impl Future<Output = T>,
) -> T {
    PROVIDER_CALL_ID.scope(provider_call_id.to_string(), future).await
}

#[must_use]
pub fn current_provider_call_id() -> Option<String> {
    PROVIDER_CALL_ID.try_with(Clone::clone).ok()
}
