//! Shared per-provider outbound concurrency budget with query-lane priority.
//!
//! Both ingest fan-out and latency-sensitive query turns dispatch their
//! provider HTTP calls through the same [`crate::integrations::llm::LlmGateway`]
//! chokepoint. Without an aggregate cap, a burst of ingest embedding/extraction
//! calls can overwhelm a self-hosted provider (e.g. a local Ollama) or trigger
//! 429 storms from a cloud provider, and concurrent query turns hitting the
//! same endpoint make it worse. This module is the pure, network-free core of
//! the budget: a process-wide registry that maps a structural provider identity
//! to a two-tier semaphore, plus a permit guard acquired around each outbound
//! call.
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
//! ## Default = no behavior change
//!
//! When a provider has no configured cap (the default), [`acquire`] returns an
//! unlimited guard that holds no semaphore permit. No registry entry is created
//! and no waiting ever happens, so without an explicit operator opt-in the
//! budget is fully transparent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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
        Self { provider_kind: provider_kind.into(), base_url: base_url.into() }
    }
}

/// Per-provider budget configuration. `max_outbound == 0` means unlimited (the
/// default), which yields zero behavior change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBudgetConfig {
    /// Maximum concurrent outbound calls to one provider endpoint. `0` =
    /// unlimited.
    pub max_outbound: usize,
    /// Permits reserved exclusively for the query lane. Clamped to
    /// `max_outbound` so the shared pool is never negative.
    pub query_reserved: usize,
}

impl ProviderBudgetConfig {
    /// The canonical unlimited (default) config: no cap, no reserve.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self { max_outbound: 0, query_reserved: 0 }
    }

    /// True when this config imposes no cap (the default).
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.max_outbound == 0
    }
}

impl Default for ProviderBudgetConfig {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Two-tier limiter for one provider endpoint: a shared pool plus a
/// query-reserved pool.
#[derive(Debug)]
struct ProviderLimiter {
    /// `max_outbound - query_reserved` permits; both lanes may use it.
    shared: Arc<Semaphore>,
    /// `query_reserved` permits; only the query lane may use it.
    reserved: Arc<Semaphore>,
}

impl ProviderLimiter {
    fn new(config: ProviderBudgetConfig) -> Self {
        let reserved = config.query_reserved.min(config.max_outbound);
        let shared = config.max_outbound - reserved;
        Self {
            shared: Arc::new(Semaphore::new(shared)),
            reserved: Arc::new(Semaphore::new(reserved)),
        }
    }

    async fn acquire(&self, lane: ProviderLane) -> ProviderBudgetGuard {
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
                if let Ok(permit) = Arc::clone(&self.shared).try_acquire_owned() {
                    return ProviderBudgetGuard::Limited(permit);
                }
                if let Ok(permit) = Arc::clone(&self.reserved).try_acquire_owned() {
                    return ProviderBudgetGuard::Limited(permit);
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
}

/// Maps an `acquire_owned` result to a guard. The semaphores backing the
/// registry are never closed (the registry holds them for the whole process
/// lifetime), so the `Err` branch is unreachable; degrade open with an
/// unlimited guard rather than panicking if that invariant is ever violated.
fn guard_from_acquire(
    permit: Result<OwnedSemaphorePermit, tokio::sync::AcquireError>,
) -> ProviderBudgetGuard {
    match permit {
        Ok(permit) => ProviderBudgetGuard::Limited(permit),
        Err(_) => ProviderBudgetGuard::Unlimited,
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

/// Process-wide registry mapping a [`ProviderIdentity`] to its limiter.
///
/// The registry is config-driven: it resolves the per-provider config through a
/// pluggable lookup so the hot path carries no magic numbers. Providers with an
/// unlimited config never get a registry entry.
pub struct ProviderBudgetRegistry {
    limiters: Mutex<HashMap<ProviderIdentity, Arc<ProviderLimiter>>>,
    resolver: Box<dyn Fn(&ProviderIdentity) -> ProviderBudgetConfig + Send + Sync>,
}

impl ProviderBudgetRegistry {
    /// Builds a registry whose per-provider config comes from `resolver`.
    pub fn new<R>(resolver: R) -> Self
    where
        R: Fn(&ProviderIdentity) -> ProviderBudgetConfig + Send + Sync + 'static,
    {
        Self { limiters: Mutex::new(HashMap::new()), resolver: Box::new(resolver) }
    }

    /// Builds a registry that applies the same config to every provider.
    #[must_use]
    pub fn uniform(config: ProviderBudgetConfig) -> Self {
        Self::new(move |_identity| config)
    }

    /// Acquires a budget permit for `identity` on `lane`, awaiting if the
    /// provider is at its cap. Returns immediately with an unlimited guard when
    /// the provider has no configured cap.
    pub async fn acquire(
        &self,
        identity: &ProviderIdentity,
        lane: ProviderLane,
    ) -> ProviderBudgetGuard {
        let Some(limiter) = self.limiter_for(identity) else {
            return ProviderBudgetGuard::Unlimited;
        };
        limiter.acquire(lane).await
    }

    /// Returns the limiter for `identity`, creating it on first use. Returns
    /// `None` when the provider is unlimited so no entry is ever allocated for
    /// the default path.
    fn limiter_for(&self, identity: &ProviderIdentity) -> Option<Arc<ProviderLimiter>> {
        let config = (self.resolver)(identity);
        if config.is_unlimited() {
            return None;
        }
        let mut limiters = self.limiters.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = limiters.get(identity) {
            return Some(Arc::clone(existing));
        }
        let limiter = Arc::new(ProviderLimiter::new(config));
        limiters.insert(identity.clone(), Arc::clone(&limiter));
        Some(limiter)
    }
}

/// Global registry handle. Installed once at startup; defaults to an unlimited
/// registry so any call before installation is a transparent no-op.
static GLOBAL_REGISTRY: OnceLock<Arc<ProviderBudgetRegistry>> = OnceLock::new();

/// Installs the process-wide registry. Idempotent: the first install wins and
/// later calls are ignored (returns `false` when an earlier registry was kept).
pub fn install_global_registry(registry: Arc<ProviderBudgetRegistry>) -> bool {
    GLOBAL_REGISTRY.set(registry).is_ok()
}

/// Acquires a budget permit from the global registry for the current lane.
/// Falls back to an unlimited guard when no registry is installed.
pub async fn acquire(identity: &ProviderIdentity) -> ProviderBudgetGuard {
    let lane = current_lane();
    match GLOBAL_REGISTRY.get() {
        Some(registry) => registry.acquire(identity, lane).await,
        None => ProviderBudgetGuard::Unlimited,
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

    #[tokio::test]
    async fn same_identity_shares_one_limiter_distinct_identities_are_independent() {
        let registry = ProviderBudgetRegistry::uniform(ProviderBudgetConfig {
            max_outbound: 1,
            query_reserved: 0,
        });
        let a1 = identity("alpha", "https://endpoint-a.example/v1");
        let a2 = identity("alpha", "https://endpoint-a.example/v1");
        let b = identity("beta", "https://endpoint-b.example/v1");

        // Same identity -> same limiter: the second acquire must wait while the
        // first guard is held (cap == 1).
        let guard_a1 = registry.acquire(&a1, ProviderLane::Ingest).await;
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
        let registry = Arc::new(ProviderBudgetRegistry::uniform(ProviderBudgetConfig {
            max_outbound: CAP,
            query_reserved: 0,
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
                let _guard = registry.acquire(&id, ProviderLane::Ingest).await;
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

    #[tokio::test]
    async fn query_lane_reserve_is_reachable_under_saturating_ingest_load() {
        // max=4, reserve=2 -> shared pool=2. Saturate the shared pool with two
        // long-held ingest guards, then prove the query lane still gets up to
        // `reserved` concurrent permits.
        let registry = Arc::new(ProviderBudgetRegistry::uniform(ProviderBudgetConfig {
            max_outbound: 4,
            query_reserved: 2,
        }));
        let id = identity("alpha", "https://endpoint.example/v1");

        let ingest_one = registry.acquire(&id, ProviderLane::Ingest).await;
        let ingest_two = registry.acquire(&id, ProviderLane::Ingest).await;

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
    async fn default_unlimited_config_lets_a_large_burst_proceed_without_capping() {
        const TASKS: usize = 256;
        let registry = Arc::new(ProviderBudgetRegistry::uniform(ProviderBudgetConfig::unlimited()));
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
                let guard = registry.acquire(&id, ProviderLane::Ingest).await;
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
            "the default unlimited config must not cap concurrency",
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
}
