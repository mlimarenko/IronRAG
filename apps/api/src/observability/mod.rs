use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, anyhow};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{Context as OtelContext, KeyValue};
use opentelemetry_http::{HeaderExtractor, HeaderInjector};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_semantic_conventions::{
    attribute::DEPLOYMENT_ENVIRONMENT_NAME,
    resource::{SERVICE_NAME, SERVICE_VERSION},
};
use tracing::{Span, info, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

const DEFAULT_LOG_FILTER: &str = "info";
const DEFAULT_SERVICE_NAME: &str = "ironrag-backend";
const DEFAULT_DEPLOYMENT_ENVIRONMENT: &str = "development";
const OTEL_EXPORTER_OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";
const OTEL_SERVICE_VERSION: &str = "OTEL_SERVICE_VERSION";
const OTEL_DEPLOYMENT_ENVIRONMENT: &str = "OTEL_DEPLOYMENT_ENVIRONMENT";
const IRONRAG_LOG_FILTER: &str = "IRONRAG_LOG_FILTER";

static TRACER_PROVIDER: OnceLock<Mutex<Option<SdkTracerProvider>>> = OnceLock::new();

/// Initializes canonical process tracing.
///
/// With no OTLP endpoint this installs only the existing formatted tracing subscriber.
/// With an endpoint it adds the OpenTelemetry span exporter layer to the same subscriber.
///
/// # Errors
/// Returns an error when the subscriber or OTLP exporter cannot be installed.
pub fn init_tracing() -> anyhow::Result<()> {
    global::set_text_map_propagator(TraceContextPropagator::new());

    let filter = env_string(IRONRAG_LOG_FILTER).unwrap_or_else(|| DEFAULT_LOG_FILTER.to_string());
    let env_filter = crate::shared::telemetry::compose_env_filter(&filter);
    let endpoint = env_string(OTEL_EXPORTER_OTLP_ENDPOINT);

    let Some(endpoint) = endpoint else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_target(false))
            .try_init()
            .context("failed to initialize tracing subscriber")?;
        info!("observability: skipped (no OTEL endpoint)");
        return Ok(());
    };

    let provider = build_tracer_provider(&endpoint)?;
    let tracer = provider.tracer("ironrag-backend");
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .context("failed to initialize tracing subscriber with OpenTelemetry")?;

    global::set_tracer_provider(provider.clone());
    store_tracer_provider(provider)?;
    info!("observability: enabled");
    Ok(())
}

/// Flushes and shuts down the OpenTelemetry provider, when one was installed.
pub async fn shutdown_tracing() {
    let Some(provider) = take_tracer_provider() else {
        return;
    };

    if let Err(error) = provider.force_flush() {
        warn!(error = %error, "observability force flush failed");
    }
    if let Err(error) = provider.shutdown() {
        warn!(error = %error, "observability shutdown failed");
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

fn build_tracer_provider(endpoint: &str) -> anyhow::Result<SdkTracerProvider> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint.to_string())
        .with_protocol(Protocol::HttpJson)
        .with_timeout(Duration::from_secs(5))
        .build()
        .context("failed to build OTLP span exporter")?;

    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(observability_resource())
        .build())
}

fn observability_resource() -> Resource {
    Resource::builder_empty()
        .with_attributes([
            KeyValue::new(
                SERVICE_NAME,
                env_string(OTEL_SERVICE_NAME).unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string()),
            ),
            KeyValue::new(
                SERVICE_VERSION,
                env_string(OTEL_SERVICE_VERSION)
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
            ),
            KeyValue::new(
                DEPLOYMENT_ENVIRONMENT_NAME,
                env_string(OTEL_DEPLOYMENT_ENVIRONMENT)
                    .unwrap_or_else(|| DEFAULT_DEPLOYMENT_ENVIRONMENT.to_string()),
            ),
        ])
        .build()
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn tracer_provider_slot() -> &'static Mutex<Option<SdkTracerProvider>> {
    TRACER_PROVIDER.get_or_init(|| Mutex::new(None))
}

fn store_tracer_provider(provider: SdkTracerProvider) -> anyhow::Result<()> {
    let mut guard = tracer_provider_slot()
        .lock()
        .map_err(|_| anyhow!("observability tracer provider lock poisoned"))?;
    if guard.is_some() {
        anyhow::bail!("observability tracer provider already initialized");
    }
    *guard = Some(provider);
    Ok(())
}

fn take_tracer_provider() -> Option<SdkTracerProvider> {
    tracer_provider_slot().lock().ok().and_then(|mut guard| guard.take())
}
