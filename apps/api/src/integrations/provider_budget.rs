//! Shared per-provider outbound concurrency and request-rate budgets.
//!
//! Both ingest fan-out and latency-sensitive query turns dispatch their
//! provider HTTP calls through the same [`crate::integrations::llm::LlmGateway`]
//! chokepoint. Without an aggregate cap, a burst of ingest embedding/extraction
//! calls can overwhelm a self-hosted provider (e.g. a local Ollama) or trigger
//! 429 storms from a cloud provider, and concurrent query turns hitting the
//! same endpoint make it worse. This module is the pure, network-free core of
//! the budget: a gateway-local registry that maps a structural provider identity
//! to a two-tier semaphore and a token bucket. A semaphore guard covers each
//! outbound HTTP attempt, and every attempt consumes a token. Retry backoff
//! holds neither resource, so a throttled lane cannot occupy capacity needed by
//! the other lane. The registry is injected into the gateway that owns it:
//! there is no process-global first-writer state, so tests and multiple app
//! instances cannot accidentally inherit another instance's limits.
//!
//! ## Identity key
//!
//! The budget guards a provider *endpoint*, so the key is the structural
//! endpoint identity available at the gateway chokepoint:
//! `(provider_kind, base_url)`. Both fields are present on every outbound
//! request the gateway dispatches; the credential value is a secret (not a
//! stable id) and is deliberately excluded from the key. Two bindings that
//! resolve to the same `provider_kind` + `base_url` share one budget, which is
//! exactly the shared physical resource (the upstream endpoint) we are
//! protecting.
//!
//! ## Query-lane priority
//!
//! The per-provider limiter is split into two pools sized from configuration:
//! a shared pool of `max - query_reserved` permits and a query-reserved pool of
//! `query_reserved` permits. The ingest lane may only draw from the shared
//! pool, so it can never consume more than `max - query_reserved` permits. The
//! query lane tries the shared pool opportunistically and, when it is
//! saturated, falls back to the reserved pool. Because ingest can never touch
//! the reserved pool, the query lane is guaranteed at least `query_reserved`
//! concurrent permits even under a fully saturating ingest load — no custom
//! scheduler required.
//!
use super::duration_millis;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use opentelemetry::{KeyValue, global, metrics::Counter};
use thiserror::Error;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tracing::Instrument as _;

const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_REGISTRY_MAX_ENTRIES: usize = 64;
const DEFAULT_REGISTRY_IDLE_TTL: Duration = Duration::from_mins(15);

/// Value-free provider admission refusal counters. Labels are structural
/// (`provider.kind`, `provider.lane`); endpoint URLs and credentials never
/// reach a label.
struct ProviderBudgetMetrics {
    acquire_refusals: Counter<u64>,
}

fn provider_budget_metrics() -> &'static ProviderBudgetMetrics {
    static METRICS: OnceLock<ProviderBudgetMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ironrag.provider_budget");
        ProviderBudgetMetrics {
            acquire_refusals: meter
                .u64_counter("ironrag.provider_budget.acquire_refusals")
                .with_description("Provider concurrency permit waits that ended in a typed timeout")
                .with_unit("{refusal}")
                .build(),
        }
    })
}

/// Lane a provider call belongs to. The query lane is prioritized over the
/// ingest lane so latency-sensitive turns never starve behind an ingest burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLane {
    /// Background ingest fan-out (embedding, graph/fact extraction). May only
    /// draw from the shared permit pool.
    Ingest,
    /// Latency-sensitive query turn. May draw from the shared pool and, when it
    /// is saturated, from the query-reserved pool.
    Query,
}

impl ProviderLane {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Query => "query",
        }
    }
}

tokio::task_local! {
    /// Lane marker for the current async scope. Set at the small number of
    /// top-level lane entry points (query-turn entry, ingest-job entry) and
    /// inherited by every inline `.await` below it within the same task. Unset
    /// (the default) is treated as [`ProviderLane::Ingest`] so background work
    /// without an explicit lane never competes for the query reserve.
    static CURRENT_LANE: ProviderLane;
}

/// Runs `future` with the current provider lane set to `lane`.
///
/// Use at top-level lane boundaries only. The lane propagates to every inline
/// awaited future within the same task; it does **not** cross a
/// `tokio::spawn` boundary, so any spawned job that issues provider calls must
/// re-establish its lane with its own scope.
pub async fn with_lane<F, T>(lane: ProviderLane, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    CURRENT_LANE.scope(lane, future).await
}

/// The lane for the current async scope, defaulting to [`ProviderLane::Ingest`]
/// when no lane has been established.
#[must_use]
pub fn current_lane() -> ProviderLane {
    CURRENT_LANE.try_with(|lane| *lane).unwrap_or(ProviderLane::Ingest)
}

/// Structural identity of a provider endpoint guarded by one budget.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderIdentity {
    pub provider_kind: String,
    pub base_url: String,
}

impl ProviderIdentity {
    pub fn new(provider_kind: impl Into<String>, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        Self {
            provider_kind: provider_kind.into(),
            base_url: base_url.trim().trim_end_matches('/').to_string(),
        }
    }
}

/// Per-provider concurrency and token-bucket configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBudgetConfig {
    /// Maximum concurrent outbound calls to one provider endpoint. `0` =
    /// unlimited.
    pub max_outbound: usize,
    /// Permits reserved exclusively for the query lane. A bounded
    /// configuration must leave at least one shared permit for ingest.
    pub query_reserved: usize,
    /// Tokens replenished per second. `0` disables rate limiting and requires
    /// `burst == 0`.
    pub requests_per_second: u32,
    /// Maximum accumulated tokens available to a burst.
    pub burst: u32,
}

impl ProviderBudgetConfig {
    /// The explicit unlimited config: no cap, no reserve.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self { max_outbound: 0, query_reserved: 0, requests_per_second: 0, burst: 0 }
    }

    /// True when this config imposes neither a concurrency nor a rate cap.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.max_outbound == 0 && self.requests_per_second == 0
    }

    /// Validates that unlimited mode is explicit and bounded mode cannot
    /// deadlock the ingest lane.
    pub const fn validate(self) -> Result<Self, ProviderBudgetError> {
        match (self.max_outbound, self.query_reserved) {
            (0, 0) => {}
            (0, _) => {
                return Err(ProviderBudgetError::InvalidConfiguration(
                    "query_reserved must be zero when max_outbound is zero",
                ));
            }
            (max, reserved) if reserved >= max => {
                return Err(ProviderBudgetError::InvalidConfiguration(
                    "query_reserved must be smaller than max_outbound",
                ));
            }
            _ => {}
        }
        match (self.requests_per_second, self.burst) {
            (0, 0) => {}
            (0, _) => {
                return Err(ProviderBudgetError::InvalidConfiguration(
                    "burst must be zero when requests_per_second is zero",
                ));
            }
            (_, 0) => {
                return Err(ProviderBudgetError::InvalidConfiguration(
                    "burst must be greater than zero when requests_per_second is bounded",
                ));
            }
            _ => {}
        }
        Ok(self)
    }
}

impl Default for ProviderBudgetConfig {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Operational bounds for a provider limiter registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBudgetRegistryOptions {
    /// Maximum time any provider call may wait for a permit.
    pub acquire_timeout: Duration,
    /// Maximum number of endpoint identities retained by one gateway.
    pub max_entries: usize,
    /// Idle entries older than this are removed before admission.
    pub idle_ttl: Duration,
}

impl ProviderBudgetRegistryOptions {
    pub const fn validate(self) -> Result<Self, ProviderBudgetError> {
        if self.acquire_timeout.is_zero() {
            return Err(ProviderBudgetError::InvalidConfiguration(
                "acquire_timeout must be greater than zero",
            ));
        }
        if self.max_entries == 0 {
            return Err(ProviderBudgetError::InvalidConfiguration(
                "registry max_entries must be greater than zero",
            ));
        }
        if self.idle_ttl.is_zero() {
            return Err(ProviderBudgetError::InvalidConfiguration(
                "registry idle_ttl must be greater than zero",
            ));
        }
        Ok(self)
    }
}

impl Default for ProviderBudgetRegistryOptions {
    fn default() -> Self {
        Self {
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            max_entries: DEFAULT_REGISTRY_MAX_ENTRIES,
            idle_ttl: DEFAULT_REGISTRY_IDLE_TTL,
        }
    }
}

/// Fail-closed provider budget failures. None of these errors silently bypass
/// the configured upstream protection.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderBudgetError {
    #[error("invalid provider budget configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("provider concurrency permit wait timed out after {timeout_ms} ms")]
    AcquireTimeout { timeout_ms: u64 },
    #[error("provider rate-limit token wait timed out after {timeout_ms} ms")]
    ThrottleTimeout { timeout_ms: u64 },
    #[error("provider concurrency limiter is closed")]
    LimiterClosed,
    #[error("provider concurrency registry is full ({max_entries} active endpoints)")]
    RegistryCapacity { max_entries: usize },
    #[error("provider concurrency registry lock is poisoned")]
    RegistryPoisoned,
}

impl ProviderBudgetError {
    /// Value-free classification token for operator-facing redacted storage.
    /// Derived from the variant alone — never from configuration values or
    /// timing payloads.
    #[must_use]
    pub const fn redacted_detail(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration(_) => "admission_invalid_configuration",
            Self::AcquireTimeout { .. } => "admission_acquire_timeout",
            Self::ThrottleTimeout { .. } => "admission_throttle_timeout",
            Self::LimiterClosed => "admission_limiter_closed",
            Self::RegistryCapacity { .. } => "admission_registry_capacity",
            Self::RegistryPoisoned => "admission_registry_poisoned",
        }
    }
}

/// Limits one provider endpoint across both concurrent operations and request
/// starts.
#[derive(Debug)]
struct ProviderLimiter {
    concurrency: Option<ProviderConcurrencyLimiter>,
    rate: Option<ProviderTokenBucket>,
}

/// Two-tier semaphore with a query-reserved pool.
#[derive(Debug)]
struct ProviderConcurrencyLimiter {
    /// `max_outbound - query_reserved` permits; both lanes may use it.
    shared: Arc<Semaphore>,
    /// `query_reserved` permits; only the query lane may use it.
    reserved: Arc<Semaphore>,
    shared_capacity: usize,
    reserved_capacity: usize,
}

impl ProviderConcurrencyLimiter {
    fn new(config: ProviderBudgetConfig) -> Self {
        let reserved = config.query_reserved;
        let shared = config.max_outbound - reserved;
        Self {
            shared: Arc::new(Semaphore::new(shared)),
            reserved: Arc::new(Semaphore::new(reserved)),
            shared_capacity: shared,
            reserved_capacity: reserved,
        }
    }

    async fn acquire(
        &self,
        lane: ProviderLane,
    ) -> Result<ProviderBudgetGuard, ProviderBudgetError> {
        match lane {
            ProviderLane::Ingest => {
                // Ingest may only ever draw from the shared pool, so it can
                // never consume the query reserve.
                guard_from_acquire(Arc::clone(&self.shared).acquire_owned().await)
            }
            ProviderLane::Query => {
                // Prefer the shared pool opportunistically; fall back to the
                // reserved pool only when shared is saturated. Because ingest
                // never touches `reserved`, query is guaranteed at least
                // `query_reserved` concurrent permits under full ingest load.
                match Arc::clone(&self.shared).try_acquire_owned() {
                    Ok(permit) => return Ok(ProviderBudgetGuard::Limited(permit)),
                    Err(TryAcquireError::Closed) => {
                        return Err(ProviderBudgetError::LimiterClosed);
                    }
                    Err(TryAcquireError::NoPermits) => {}
                }
                match Arc::clone(&self.reserved).try_acquire_owned() {
                    Ok(permit) => return Ok(ProviderBudgetGuard::Limited(permit)),
                    Err(TryAcquireError::Closed) => {
                        return Err(ProviderBudgetError::LimiterClosed);
                    }
                    Err(TryAcquireError::NoPermits) => {}
                }
                // Both pools are momentarily full. Wait on whichever frees a
                // permit first so a query call never deadlocks behind ingest.
                let shared = Arc::clone(&self.shared);
                let reserved = Arc::clone(&self.reserved);
                tokio::select! {
                    biased;
                    permit = reserved.acquire_owned() => guard_from_acquire(permit),
                    permit = shared.acquire_owned() => guard_from_acquire(permit),
                }
            }
        }
    }

    fn is_idle(&self) -> bool {
        self.shared.available_permits().saturating_add(self.reserved.available_permits())
            == self.shared_capacity.saturating_add(self.reserved_capacity)
    }
}

#[derive(Debug)]
struct ProviderTokenBucket {
    /// Synchronous, not `tokio::sync::Mutex`: the critical section is a pure
    /// numeric update with no `.await` inside it, and a synchronous mutex is
    /// what lets [`ProviderRateToken::drop`] refund a token from a `Drop` impl
    /// when the surrounding future is cancelled.
    state: Mutex<ProviderTokenBucketState>,
    token_returned: Notify,
    requests_per_second: u32,
    capacity: u32,
}

#[derive(Debug)]
struct ProviderTokenBucketState {
    tokens: f64,
    last_refill: tokio::time::Instant,
}

impl ProviderTokenBucket {
    fn new(requests_per_second: u32, capacity: u32) -> Self {
        Self {
            state: Mutex::new(ProviderTokenBucketState {
                tokens: f64::from(capacity),
                last_refill: tokio::time::Instant::now(),
            }),
            token_returned: Notify::new(),
            requests_per_second,
            capacity,
        }
    }

    /// The state is a bounded numeric update, so a poisoned mutex cannot hold a
    /// structurally invalid value; recover the guard instead of panicking so a
    /// single panic elsewhere cannot stall every provider call permanently.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, ProviderTokenBucketState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Consumes one token, waiting for refill when the burst is exhausted.
    ///
    /// This is the bare consumption step; [`ProviderLimiter::throttle`] wraps it
    /// in a [`ProviderRateToken`] so a cancelled or rejected admission can
    /// return the token.
    async fn throttle(&self) {
        loop {
            let token_returned = self.token_returned.notified();
            let delay = {
                let mut state = self.lock_state();
                let now = tokio::time::Instant::now();
                let replenished = now.duration_since(state.last_refill).as_secs_f64()
                    * f64::from(self.requests_per_second);
                state.tokens = (state.tokens + replenished).min(f64::from(self.capacity));
                state.last_refill = now;
                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return;
                }
                let missing_tokens = 1.0 - state.tokens;
                Duration::from_secs_f64(missing_tokens / f64::from(self.requests_per_second))
                    .max(Duration::from_nanos(1))
            };
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = token_returned => {}
            }
        }
    }

    /// Returns one token synchronously so a `Drop` impl can call it. Wakes one
    /// waiter blocked on the refill delay.
    fn refund_sync(&self) {
        let mut state = self.lock_state();
        let now = tokio::time::Instant::now();
        let replenished = now.duration_since(state.last_refill).as_secs_f64()
            * f64::from(self.requests_per_second);
        state.tokens = (state.tokens + replenished + 1.0).min(f64::from(self.capacity));
        state.last_refill = now;
        drop(state);
        self.token_returned.notify_one();
    }
}

impl ProviderLimiter {
    fn new(config: ProviderBudgetConfig) -> Self {
        Self {
            concurrency: (config.max_outbound > 0).then(|| ProviderConcurrencyLimiter::new(config)),
            rate: (config.requests_per_second > 0)
                .then(|| ProviderTokenBucket::new(config.requests_per_second, config.burst)),
        }
    }

    async fn acquire_concurrency(
        &self,
        lane: ProviderLane,
    ) -> Result<ProviderBudgetGuard, ProviderBudgetError> {
        match &self.concurrency {
            Some(concurrency) => concurrency.acquire(lane).await,
            None => Ok(ProviderBudgetGuard::Unlimited),
        }
    }

    /// Reserves one rate token and returns its RAII guard. Holding the
    /// reservation across the concurrency acquire is what makes token
    /// consumption cancellation-safe: if the caller's future is dropped before
    /// the token is committed, the guard refunds it.
    async fn throttle(self: &Arc<Self>) -> ProviderRateToken {
        let Some(rate) = &self.rate else {
            return ProviderRateToken::unlimited();
        };
        rate.throttle().await;
        ProviderRateToken::reserved(Arc::clone(self))
    }

    fn refund_throttle_sync(&self) {
        if let Some(rate) = &self.rate {
            rate.refund_sync();
        }
    }

    const fn has_concurrency_limit(&self) -> bool {
        self.concurrency.is_some()
    }

    fn rate_config(&self) -> Option<(u32, u32)> {
        self.rate.as_ref().map(|rate| (rate.requests_per_second, rate.capacity))
    }

    fn is_idle(&self) -> bool {
        self.concurrency.as_ref().is_none_or(ProviderConcurrencyLimiter::is_idle)
    }
}

/// Maps an `acquire_owned` result to a guard and preserves fail-closed behavior
/// if an internal semaphore is unexpectedly closed.
fn guard_from_acquire(
    permit: Result<OwnedSemaphorePermit, tokio::sync::AcquireError>,
) -> Result<ProviderBudgetGuard, ProviderBudgetError> {
    match permit {
        Ok(permit) => Ok(ProviderBudgetGuard::Limited(permit)),
        Err(_) => Err(ProviderBudgetError::LimiterClosed),
    }
}

/// RAII guard held for the duration of one outbound provider call. Dropping it
/// releases the permit back to its pool. The `Unlimited` variant holds no
/// permit and exists so the default (uncapped) path allocates nothing.
#[derive(Debug)]
#[must_use = "dropping the guard immediately releases the provider budget permit"]
pub enum ProviderBudgetGuard {
    Unlimited,
    Limited(OwnedSemaphorePermit),
}

/// RAII reservation of one rate-limit token.
///
/// A token is consumed when the reservation is created and returned to its
/// bucket when the guard is dropped, unless [`Self::commit`] disarms the refund.
/// This is what keeps token consumption cancellation-safe: the admission path
/// holds the reservation across the concurrency acquire, so a future dropped
/// mid-acquire — an execution deadline, a shutdown — returns the token
/// synchronously instead of leaking it. Committing happens only once the HTTP
/// attempt holds its concurrency permit, so a token is never speculatively
/// released after an attempt has been dispatched.
///
/// Dropping the guard without [`Self::commit`] refunds the token, so a caller
/// that discards the returned value — for example `registry.throttle(id).await?;`
/// — silently returns the token and disables the rate limit. The `#[must_use]`
/// below exists to turn that silent leak into a compile warning.
#[derive(Debug)]
#[must_use = "dropping a ProviderRateToken immediately refunds its rate token and silently disables the rate limit; hold it until the attempt is dispatched and call ProviderRateToken::commit"]
pub struct ProviderRateToken {
    /// The owning limiter, held so the registry cannot evict a limiter whose
    /// token reservation is still outstanding. `None` for an uncapped provider.
    limiter: Option<Arc<ProviderLimiter>>,
    committed: bool,
}

impl ProviderRateToken {
    const fn unlimited() -> Self {
        Self { limiter: None, committed: true }
    }

    fn reserved(limiter: Arc<ProviderLimiter>) -> Self {
        Self { limiter: Some(limiter), committed: false }
    }

    /// Spends the token for a dispatched attempt. After this the guard no
    /// longer refunds, so an attempt that reached the wire is never
    /// speculatively un-counted.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ProviderRateToken {
    fn drop(&mut self) {
        if !self.committed
            && let Some(limiter) = &self.limiter
        {
            limiter.refund_throttle_sync();
        }
    }
}

#[derive(Debug)]
struct ProviderLimiterEntry {
    limiter: Arc<ProviderLimiter>,
    last_used: Instant,
}

/// Resolves the per-provider budget config for the registry. Injected as a
/// trait object so the hot path carries no hard-coded policy.
type ProviderConfigResolver = Box<dyn Fn(&ProviderIdentity) -> ProviderBudgetConfig + Send + Sync>;

/// Gateway-local registry mapping a [`ProviderIdentity`] to its limiter.
///
/// The registry is config-driven: it resolves the per-provider config through a
/// pluggable lookup so the hot path carries no magic numbers. Providers with an
/// unlimited config never get a registry entry.
pub struct ProviderBudgetRegistry {
    limiters: Mutex<HashMap<ProviderIdentity, ProviderLimiterEntry>>,
    resolver: ProviderConfigResolver,
    options: ProviderBudgetRegistryOptions,
}

impl ProviderBudgetRegistry {
    /// Builds a registry whose per-provider config comes from `resolver`.
    pub fn new<R>(
        resolver: R,
        options: ProviderBudgetRegistryOptions,
    ) -> Result<Self, ProviderBudgetError>
    where
        R: Fn(&ProviderIdentity) -> ProviderBudgetConfig + Send + Sync + 'static,
    {
        Ok(Self {
            limiters: Mutex::new(HashMap::new()),
            resolver: Box::new(resolver),
            options: options.validate()?,
        })
    }

    /// Builds a registry that applies the same config to every provider.
    pub fn uniform(
        config: ProviderBudgetConfig,
        options: ProviderBudgetRegistryOptions,
    ) -> Result<Self, ProviderBudgetError> {
        let config = config.validate()?;
        Self::new(move |_identity| config, options)
    }

    /// Acquires a budget permit for `identity` on `lane`, awaiting if the
    /// provider is at its cap. Returns immediately with an unlimited guard when
    /// the provider has no configured cap.
    pub async fn acquire(
        &self,
        identity: &ProviderIdentity,
        lane: ProviderLane,
    ) -> Result<ProviderBudgetGuard, ProviderBudgetError> {
        let Some(limiter) = self.limiter_for(identity)? else {
            return Ok(ProviderBudgetGuard::Unlimited);
        };
        if !limiter.has_concurrency_limit() {
            return Ok(ProviderBudgetGuard::Unlimited);
        }
        let span = tracing::info_span!(
            "provider.throttle_wait",
            throttle_kind = "concurrency",
            provider_kind = %identity.provider_kind,
            provider_lane = lane.as_str(),
            wait_ms = tracing::field::Empty,
        );
        let wait = async {
            let started = Instant::now();
            // Both lanes wait under the same bounded admission timeout: an
            // ingest worker whose permits never free must fail with a typed
            // refusal instead of hanging without a trace.
            let result = tokio::time::timeout(
                self.options.acquire_timeout,
                limiter.acquire_concurrency(lane),
            )
            .await;
            let wait_ms = duration_millis(started.elapsed());
            tracing::Span::current().record("wait_ms", wait_ms);
            if result.is_err() {
                let timeout_ms = duration_millis(self.options.acquire_timeout);
                provider_budget_metrics().acquire_refusals.add(
                    1,
                    &[
                        KeyValue::new("provider.kind", identity.provider_kind.clone()),
                        KeyValue::new("provider.lane", lane.as_str()),
                        KeyValue::new("reason", "acquire_timeout"),
                    ],
                );
                tracing::warn!(
                    provider_kind = %identity.provider_kind,
                    provider_lane = lane.as_str(),
                    timeout_ms,
                    wait_ms,
                    "provider concurrency permit wait timed out"
                );
            }
            result
        }
        .instrument(span)
        .await;
        match wait {
            Ok(result) => result,
            Err(_) => Err(ProviderBudgetError::AcquireTimeout {
                timeout_ms: duration_millis(self.options.acquire_timeout),
            }),
        }
    }

    /// Reserves one token for an outbound HTTP attempt, waiting for refill when
    /// the endpoint has exhausted its configured burst.
    ///
    /// The returned [`ProviderRateToken`] owns the reservation: hold it until
    /// the attempt is dispatched (then [`ProviderRateToken::commit`]) or drop it
    /// to return the token. This is the cancellation-safe primitive both the LLM
    /// gateway and the SystemOne client admit through.
    pub async fn throttle(
        &self,
        identity: &ProviderIdentity,
    ) -> Result<ProviderRateToken, ProviderBudgetError> {
        let Some(limiter) = self.limiter_for(identity)? else {
            return Ok(ProviderRateToken::unlimited());
        };
        let Some((requests_per_second, burst)) = limiter.rate_config() else {
            return Ok(ProviderRateToken::unlimited());
        };
        let span = tracing::info_span!(
            "provider.throttle_wait",
            throttle_kind = "rate",
            provider_kind = %identity.provider_kind,
            requests_per_second,
            burst,
            wait_ms = tracing::field::Empty,
        );
        let wait = async {
            let started = Instant::now();
            let result =
                tokio::time::timeout(self.options.acquire_timeout, limiter.throttle()).await;
            tracing::Span::current().record("wait_ms", duration_millis(started.elapsed()));
            result
        }
        .instrument(span)
        .await;
        match wait {
            Ok(token) => Ok(token),
            Err(_) => Err(ProviderBudgetError::ThrottleTimeout {
                timeout_ms: duration_millis(self.options.acquire_timeout),
            }),
        }
    }

    /// Returns the limiter for `identity`, creating it on first use. Returns
    /// `None` when the provider is unlimited so no entry is ever allocated for
    /// the default path.
    fn limiter_for(
        &self,
        identity: &ProviderIdentity,
    ) -> Result<Option<Arc<ProviderLimiter>>, ProviderBudgetError> {
        let config = (self.resolver)(identity).validate()?;
        if config.is_unlimited() {
            return Ok(None);
        }
        let now = Instant::now();
        let mut limiters =
            self.limiters.lock().map_err(|_| ProviderBudgetError::RegistryPoisoned)?;
        limiters.retain(|_, entry| {
            !(entry.limiter.is_idle()
                && Arc::strong_count(&entry.limiter) == 1
                && now.saturating_duration_since(entry.last_used) >= self.options.idle_ttl)
        });
        if let Some(existing) = limiters.get_mut(identity) {
            existing.last_used = now;
            return Ok(Some(Arc::clone(&existing.limiter)));
        }
        if limiters.len() >= self.options.max_entries {
            let eviction_key = limiters
                .iter()
                .filter(|(_, entry)| {
                    entry.limiter.is_idle() && Arc::strong_count(&entry.limiter) == 1
                })
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(identity, _)| identity.clone());
            let Some(eviction_key) = eviction_key else {
                return Err(ProviderBudgetError::RegistryCapacity {
                    max_entries: self.options.max_entries,
                });
            };
            limiters.remove(&eviction_key);
        }
        let limiter = Arc::new(ProviderLimiter::new(config));
        limiters.insert(
            identity.clone(),
            ProviderLimiterEntry { limiter: Arc::clone(&limiter), last_used: now },
        );
        drop(limiters);
        Ok(Some(limiter))
    }

    #[cfg(test)]
    fn entry_count(&self) -> Result<usize, ProviderBudgetError> {
        self.limiters
            .lock()
            .map(|entries| entries.len())
            .map_err(|_| ProviderBudgetError::RegistryPoisoned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Barrier;

    fn identity(kind: &str, url: &str) -> ProviderIdentity {
        ProviderIdentity::new(kind, url)
    }

    fn registry(config: ProviderBudgetConfig) -> ProviderBudgetRegistry {
        ProviderBudgetRegistry::uniform(config, ProviderBudgetRegistryOptions::default())
            .expect("test provider budget config must be valid")
    }

    /// A cap-1 registry whose shared pool is exactly one permit, with an
    /// explicit admission timeout. Used by the lanes' timeout tests.
    fn saturated_registry(acquire_timeout: Duration) -> ProviderBudgetRegistry {
        ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 1,
                query_reserved: 0,
                ..ProviderBudgetConfig::unlimited()
            },
            ProviderBudgetRegistryOptions {
                acquire_timeout,
                ..ProviderBudgetRegistryOptions::default()
            },
        )
        .expect("test provider budget config must be valid")
    }

    #[tokio::test]
    async fn same_identity_shares_one_limiter_distinct_identities_are_independent() {
        let registry = registry(ProviderBudgetConfig {
            max_outbound: 1,
            query_reserved: 0,
            ..ProviderBudgetConfig::unlimited()
        });
        let a1 = identity("alpha", "https://endpoint-a.example/v1");
        let a2 = identity("alpha", "https://endpoint-a.example/v1");
        let b = identity("beta", "https://endpoint-b.example/v1");

        // Same identity -> same limiter: the second acquire must wait while the
        // first guard is held (cap == 1).
        let guard_a1 = registry.acquire(&a1, ProviderLane::Ingest).await.unwrap();
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            registry.acquire(&a2, ProviderLane::Ingest),
        )
        .await;
        assert!(blocked.is_err(), "second acquire on the same identity must block at cap 1");

        // Different identity -> independent limiter: acquires immediately even
        // while `a1` is saturated.
        let guard_b = tokio::time::timeout(
            Duration::from_millis(50),
            registry.acquire(&b, ProviderLane::Ingest),
        )
        .await;
        assert!(guard_b.is_ok(), "a distinct identity has an independent budget");

        drop(guard_a1);
        drop(guard_b);
    }

    #[tokio::test]
    async fn budget_caps_concurrency_at_configured_max() {
        const CAP: usize = 3;
        const TASKS: usize = 24;
        let registry = Arc::new(registry(ProviderBudgetConfig {
            max_outbound: CAP,
            query_reserved: 0,
            ..ProviderBudgetConfig::unlimited()
        }));
        let id = identity("alpha", "https://endpoint.example/v1");
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(TASKS));

        let mut handles = Vec::new();
        for _ in 0..TASKS {
            let registry = Arc::clone(&registry);
            let id = id.clone();
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let _guard = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert!(
            peak.load(Ordering::SeqCst) <= CAP,
            "max in-flight {} exceeded the configured cap {CAP}",
            peak.load(Ordering::SeqCst),
        );
        assert!(peak.load(Ordering::SeqCst) >= 1, "at least one call must have run");
    }

    #[tokio::test(start_paused = true)]
    async fn rate_budget_serializes_requests_beyond_the_configured_burst() {
        const REQUESTS_PER_SECOND: u32 = 20;
        const TOKEN_INTERVAL: Duration = Duration::from_millis(50);
        let registry = Arc::new(registry(ProviderBudgetConfig {
            max_outbound: 0,
            query_reserved: 0,
            requests_per_second: REQUESTS_PER_SECOND,
            burst: 1,
        }));
        let id = identity("alpha", "https://endpoint.example/v1");

        // Hold the reservation: the token is only spent while this guard lives,
        // so the request beyond the burst must wait for a refill.
        let _first = registry.throttle(&id).await.unwrap();
        let waiting = tokio::spawn({
            let registry = Arc::clone(&registry);
            let id = id.clone();
            async move { registry.throttle(&id).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "the burst must contain exactly one request");

        tokio::time::advance(TOKEN_INTERVAL - Duration::from_nanos(1)).await;
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "the request beyond the burst must wait for a complete token interval",
        );

        tokio::time::advance(Duration::from_nanos(1)).await;
        let second = waiting
            .await
            .expect("rate-limited request task should finish")
            .expect("rate-limited request should receive the replenished token");
        // Commit the reservation: the attempt got its token, so it must not be
        // refunded by an implicit drop of the must-use guard.
        second.commit();
    }

    #[tokio::test]
    async fn query_lane_reserve_is_reachable_under_saturating_ingest_load() {
        // max=4, reserve=2 -> shared pool=2. Saturate the shared pool with two
        // long-held ingest guards, then prove the query lane still gets up to
        // `reserved` concurrent permits.
        let registry = Arc::new(registry(ProviderBudgetConfig {
            max_outbound: 4,
            query_reserved: 2,
            ..ProviderBudgetConfig::unlimited()
        }));
        let id = identity("alpha", "https://endpoint.example/v1");

        let ingest_one = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();
        let ingest_two = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();

        // A third ingest acquire must now block: the shared pool (size 2) is
        // exhausted and ingest may not touch the reserve.
        let third_ingest = tokio::time::timeout(
            Duration::from_millis(50),
            registry.acquire(&id, ProviderLane::Ingest),
        )
        .await;
        assert!(third_ingest.is_err(), "ingest must be capped at the shared pool size");

        // The query lane still acquires `reserved` (2) concurrent permits.
        let query_one = tokio::time::timeout(
            Duration::from_millis(50),
            registry.acquire(&id, ProviderLane::Query),
        )
        .await;
        assert!(query_one.is_ok(), "query must reach the reserve under full ingest load");
        let query_two = tokio::time::timeout(
            Duration::from_millis(50),
            registry.acquire(&id, ProviderLane::Query),
        )
        .await;
        assert!(query_two.is_ok(), "query must reach the full reserve under full ingest load");

        // The reserve is now also exhausted, so a third query acquire blocks.
        let query_three = tokio::time::timeout(
            Duration::from_millis(50),
            registry.acquire(&id, ProviderLane::Query),
        )
        .await;
        assert!(query_three.is_err(), "query is bounded by max once the reserve is consumed");

        drop(ingest_one);
        drop(ingest_two);
        drop(query_one);
        drop(query_two);
    }

    #[tokio::test]
    async fn explicit_unlimited_config_lets_a_large_burst_proceed_without_capping() {
        const TASKS: usize = 256;
        let registry = Arc::new(registry(ProviderBudgetConfig::unlimited()));
        let id = identity("alpha", "https://endpoint.example/v1");
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(TASKS));

        let mut handles = Vec::new();
        for _ in 0..TASKS {
            let registry = Arc::clone(&registry);
            let id = id.clone();
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                let guard = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();
                assert!(matches!(guard, ProviderBudgetGuard::Unlimited));
                barrier.wait().await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        // With no cap, every task in the barrier-synchronized burst is in
        // flight at once.
        assert_eq!(
            peak.load(Ordering::SeqCst),
            TASKS,
            "the explicit unlimited config must not cap concurrency",
        );
    }

    #[tokio::test]
    async fn current_lane_defaults_to_ingest_and_propagates_through_with_lane() {
        assert_eq!(current_lane(), ProviderLane::Ingest, "default lane is ingest");
        let observed = with_lane(ProviderLane::Query, async { current_lane() }).await;
        assert_eq!(observed, ProviderLane::Query, "with_lane establishes the lane");
        // Lane scope is restored after the future completes.
        assert_eq!(current_lane(), ProviderLane::Ingest);
    }

    #[test]
    fn invalid_reserve_cannot_remove_every_ingest_permit() {
        let error = ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 4,
                query_reserved: 4,
                ..ProviderBudgetConfig::unlimited()
            },
            ProviderBudgetRegistryOptions::default(),
        )
        .err()
        .expect("max == reserve must fail closed");
        assert!(matches!(error, ProviderBudgetError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn saturated_ingest_budget_returns_a_typed_timeout() {
        let registry = saturated_registry(Duration::from_millis(20));
        let id = identity("alpha", "https://endpoint.example/v1");
        let _held = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();

        // RED before the fix: the ingest acquire ignored `acquire_timeout` and
        // stayed pending, so this outer bound elapsed. GREEN after the fix: the
        // registry itself returns a typed refusal.
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            registry.acquire(&id, ProviderLane::Ingest),
        )
        .await
        .expect("ingest acquire must be bounded by the registry acquire_timeout")
        .unwrap_err();
        assert_eq!(error, ProviderBudgetError::AcquireTimeout { timeout_ms: 20 });
    }

    #[tokio::test]
    async fn ingest_contention_acquires_released_capacity_within_the_timeout() {
        let registry = saturated_registry(Duration::from_secs(5));
        let id = identity("alpha", "https://endpoint.example/v1");
        let held = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();
        let mut waiting = Box::pin(registry.acquire(&id, ProviderLane::Ingest));

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting).await.is_err(),
            "ingest must queue while the only permit is held",
        );

        drop(held);
        let _guard = waiting
            .await
            .expect("ingest waiter should acquire released capacity within the timeout");
    }

    #[tokio::test]
    async fn saturated_query_budget_returns_a_typed_timeout() {
        let registry = saturated_registry(Duration::from_millis(20));
        let id = identity("alpha", "https://endpoint.example/v1");
        let _held = registry.acquire(&id, ProviderLane::Ingest).await.unwrap();

        let error = registry.acquire(&id, ProviderLane::Query).await.unwrap_err();
        assert_eq!(error, ProviderBudgetError::AcquireTimeout { timeout_ms: 20 });
    }

    #[tokio::test]
    async fn registry_is_bounded_and_only_evicts_idle_limiters() {
        let registry = ProviderBudgetRegistry::uniform(
            ProviderBudgetConfig {
                max_outbound: 1,
                query_reserved: 0,
                ..ProviderBudgetConfig::unlimited()
            },
            ProviderBudgetRegistryOptions {
                max_entries: 2,
                ..ProviderBudgetRegistryOptions::default()
            },
        )
        .unwrap();
        let first = identity("alpha", "https://one.example/v1");
        let second = identity("alpha", "https://two.example/v1");
        let third = identity("alpha", "https://three.example/v1");
        let held_first = registry.acquire(&first, ProviderLane::Ingest).await.unwrap();
        let held_second = registry.acquire(&second, ProviderLane::Ingest).await.unwrap();

        let error = registry.acquire(&third, ProviderLane::Ingest).await.unwrap_err();
        assert_eq!(error, ProviderBudgetError::RegistryCapacity { max_entries: 2 });
        assert_eq!(registry.entry_count().unwrap(), 2);

        drop(held_first);
        let third_guard = registry.acquire(&third, ProviderLane::Ingest).await.unwrap();
        assert_eq!(registry.entry_count().unwrap(), 2);
        drop(third_guard);
        drop(held_second);
    }

    #[tokio::test]
    async fn a_closed_limiter_fails_closed() {
        let limiter = ProviderLimiter::new(ProviderBudgetConfig {
            max_outbound: 1,
            query_reserved: 0,
            ..ProviderBudgetConfig::unlimited()
        });
        limiter
            .concurrency
            .as_ref()
            .expect("bounded config should create a concurrency limiter")
            .shared
            .close();

        let error = limiter.acquire_concurrency(ProviderLane::Ingest).await.unwrap_err();
        assert_eq!(error, ProviderBudgetError::LimiterClosed);
    }
}
