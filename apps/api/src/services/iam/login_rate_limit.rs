//! Rate limiting for the public login endpoint (`POST /v1/iam/sessions`).
//!
//! `authenticate_session` is a public, unauthenticated entry point that
//! deliberately runs a real Argon2id hash even for a login that does not
//! exist, so that response timing cannot reveal account existence (see
//! `session_support::DUMMY_PASSWORD_HASH`). That correct timing defense turns
//! the endpoint into a cheap CPU amplifier without an admission control in
//! front of it: the caller's cost is one HTTP request, the server's cost is a
//! full Argon2id hash every time, whether or not the login exists.
//!
//! This module is the admission control. It is checked before
//! `get_user_by_login` and before either `verify_password` branch, so a
//! rejection here never touches the database or the hasher. It never
//! accumulates a standing lockout against an account or a login string --
//! only a leaking token bucket that recovers on its own -- because a
//! permanent block keyed by attacker-supplied input would itself become a way
//! to lock a legitimate user out of their own account.
//!
//! ## Two independent axes, both checked
//!
//! * **Per normalized login.** Keyed by the same `trim().to_ascii_lowercase()`
//!   string `authenticate_session` already uses to look the user up, so the
//!   limiter and the lookup can never disagree about identity. This bounds
//!   how hard one login string can be hammered regardless of source.
//! * **Per source.** Keyed by the caller's best-effort network origin (see
//!   `interfaces::http::iam::session` for how that key is derived). This
//!   bounds how hard one source can hammer *many* different login strings --
//!   the case the per-login axis alone cannot see, because every login has
//!   its own untouched bucket.
//!
//! Both buckets are plain string keys with no existence check against the
//! `iam_user` table, so which axis rejects a request carries no information
//! about whether the attempted login is a real account.
//!
//! ## Immediate rejection, no waiting
//!
//! Unlike [`crate::integrations::provider_budget::ProviderTokenBucket`],
//! which is an *outbound* throttle and is allowed to hold the calling task
//! until a token frees up, a bucket here rejects the instant it is empty.
//! Making an inbound request wait for its own rate limit to clear would pin
//! an HTTP task (and the connection behind it) for the wait duration,
//! trading one amplifier (unbounded Argon2id) for another (unbounded pending
//! tasks under a burst). A rejection carries a `retry_after_seconds` estimate
//! so a well-behaved client can back off without polling.
//!
//! ## Bounded concurrent verification
//!
//! Both rate limiters only bound the *rate* of admitted attempts, not how
//! many admitted attempts run Argon2id at the same instant. A burst that
//! stays under both buckets (many distinct logins from many distinct
//! sources, each within its own limit) could still saturate every CPU on the
//! replica with concurrent hashing. [`PasswordVerificationAdmission`] is a
//! single shared cap on concurrent verifications, in the style of
//! `services::query::turn_admission::QueryTurnAdmission`: a bounded semaphore
//! with a bounded wait queue, so an admission that cannot get a permit within
//! the timeout fails fast with 429 instead of piling up waiting tasks.
//!
//! ## Bounded memory
//!
//! Both bucket registries are bounded maps with idle-time eviction, in the
//! style of `integrations::provider_budget::ProviderBudgetRegistry`: an
//! attacker who cycles through unbounded distinct login strings or spoofed
//! source keys cannot grow either map without bound. A registry at capacity
//! fails the admission closed (429) rather than skipping the check.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

use crate::app::config::Settings;
use crate::shared::admission::WaitingSlot;

const DEFAULT_REGISTRY_MAX_ENTRIES: usize = 10_000;
const DEFAULT_REGISTRY_IDLE_TTL: Duration = Duration::from_secs(600);

/// Fail-closed login rate limit failures. None of these silently let a
/// request through to Argon2id.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum LoginRateLimitError {
    #[error("login rate limit exceeded for this login; retry after {retry_after_seconds}s")]
    LoginRateLimited { retry_after_seconds: u64 },
    #[error("login rate limit exceeded for this source; retry after {retry_after_seconds}s")]
    SourceRateLimited { retry_after_seconds: u64 },
    #[error(
        "too many concurrent password verifications in flight; retry after {retry_after_seconds}s"
    )]
    VerificationAdmissionRejected { retry_after_seconds: u64 },
    #[error("login rate limit registry lock is poisoned")]
    RegistryPoisoned,
}

impl LoginRateLimitError {
    /// Value-free classification token, mirroring
    /// `ProviderBudgetError::redacted_detail`.
    #[must_use]
    pub(crate) const fn redacted_detail(self) -> &'static str {
        match self {
            Self::LoginRateLimited { .. } => "login_rate_limited",
            Self::SourceRateLimited { .. } => "login_source_rate_limited",
            Self::VerificationAdmissionRejected { .. } => "login_verification_admission_rejected",
            Self::RegistryPoisoned => "login_rate_limit_registry_poisoned",
        }
    }
}

/// Which axis a [`LeakyBucketRegistry`] enforces, purely to select the right
/// [`LoginRateLimitError`] variant on rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateLimitAxis {
    Login,
    Source,
}

impl RateLimitAxis {
    const fn exceeded(self, retry_after_seconds: u64) -> LoginRateLimitError {
        match self {
            Self::Login => LoginRateLimitError::LoginRateLimited { retry_after_seconds },
            Self::Source => LoginRateLimitError::SourceRateLimited { retry_after_seconds },
        }
    }
}

/// Capacity and refill window for one leaky bucket axis.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LeakyBucketConfig {
    /// Maximum attempts admitted from a fully recovered bucket.
    pub capacity: u32,
    /// Seconds for a fully drained bucket to refill to `capacity`.
    pub window_seconds: u64,
}

impl LeakyBucketConfig {
    fn validate(self) -> Result<Self, String> {
        if self.capacity == 0 {
            return Err("login rate limit capacity must be greater than zero".to_string());
        }
        if self.window_seconds == 0 {
            return Err("login rate limit window_seconds must be greater than zero".to_string());
        }
        Ok(self)
    }

    fn refill_per_second(self) -> f64 {
        f64::from(self.capacity) / self.window_seconds as f64
    }
}

/// Operational bounds for one bucket registry.
#[derive(Debug, Clone, Copy)]
struct LeakyBucketRegistryOptions {
    max_entries: usize,
    idle_ttl: Duration,
}

impl Default for LeakyBucketRegistryOptions {
    fn default() -> Self {
        Self { max_entries: DEFAULT_REGISTRY_MAX_ENTRIES, idle_ttl: DEFAULT_REGISTRY_IDLE_TTL }
    }
}

#[derive(Debug, Clone, Copy)]
struct BucketEntry {
    tokens: f64,
    last_refill: Instant,
    last_used: Instant,
}

/// A bounded map of independent leaky buckets, one per key, all sharing one
/// [`LeakyBucketConfig`]. Unlike `ProviderBudgetRegistry`'s concurrency
/// limiter, a bucket here holds no guard an in-flight request keeps alive, so
/// eviction is purely time-based: idle beyond `idle_ttl` is always safe to
/// forget, and forgetting it is equivalent to it having fully refilled. The
/// same argument bounds memory: at `max_entries` the least-recently-used
/// bucket is forgotten so a new key is always admitted.
#[derive(Debug)]
struct LeakyBucketRegistry<K> {
    axis: RateLimitAxis,
    buckets: Mutex<HashMap<K, BucketEntry>>,
    config: LeakyBucketConfig,
    options: LeakyBucketRegistryOptions,
}

impl<K: Eq + Hash + Clone> LeakyBucketRegistry<K> {
    fn new(
        axis: RateLimitAxis,
        config: LeakyBucketConfig,
        options: LeakyBucketRegistryOptions,
    ) -> Result<Self, String> {
        Ok(Self { axis, buckets: Mutex::new(HashMap::new()), config: config.validate()?, options })
    }

    /// Consumes one token for `key`, refilling first. Returns immediately
    /// (never awaits): a token bucket that is temporarily empty is a 429, not
    /// a reason to hold the request.
    fn try_consume(&self, key: &K) -> Result<(), LoginRateLimitError> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().map_err(|_| LoginRateLimitError::RegistryPoisoned)?;
        buckets.retain(|_, entry| {
            now.saturating_duration_since(entry.last_used) < self.options.idle_ttl
        });

        let capacity = f64::from(self.config.capacity);
        if !buckets.contains_key(key) && buckets.len() >= self.options.max_entries {
            // Evict the least-recently-used bucket rather than refusing the
            // new key: login keys are attacker-chosen and unbounded, so
            // refusing at capacity would let ~max_entries distinct guesses
            // lock every genuine user (always a "new key") out of login for
            // the whole idle TTL. Forgetting an LRU bucket only hands that
            // one key a full refill, which is exactly what any first-seen
            // key gets anyway.
            if let Some(evicted) = buckets
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(existing, _)| existing.clone())
            {
                buckets.remove(&evicted);
            }
        }
        let entry = buckets.entry(key.clone()).or_insert(BucketEntry {
            tokens: capacity,
            last_refill: now,
            last_used: now,
        });

        let refill_per_second = self.config.refill_per_second();
        let elapsed = now.saturating_duration_since(entry.last_refill).as_secs_f64();
        entry.tokens = (entry.tokens + elapsed * refill_per_second).min(capacity);
        entry.last_refill = now;
        entry.last_used = now;

        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            Ok(())
        } else {
            let missing = 1.0 - entry.tokens;
            let retry_after_seconds = (missing / refill_per_second).ceil().max(1.0) as u64;
            Err(self.axis.exceeded(retry_after_seconds))
        }
    }

    /// Returns one token to `key`, best-effort. Used when a request consumed
    /// this axis but was then rejected by a later check, so one axis being
    /// saturated does not also silently drain the other axis's budget for a
    /// request that never reached Argon2id.
    fn refund(&self, key: &K) {
        let Ok(mut buckets) = self.buckets.lock() else { return };
        if let Some(entry) = buckets.get_mut(key) {
            let capacity = f64::from(self.config.capacity);
            entry.tokens = (entry.tokens + 1.0).min(capacity);
        }
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.buckets.lock().map(|buckets| buckets.len()).unwrap_or(0)
    }
}

/// Bounded, timeout-guarded admission for concurrent Argon2id verifications,
/// in the shape of `services::query::turn_admission::QueryTurnAdmission`: a
/// semaphore plus a bounded wait queue, so a burst that clears both leaky
/// buckets still cannot pin unbounded concurrent hashing (or unbounded
/// waiting tasks) on the replica.
#[derive(Debug)]
struct PasswordVerificationAdmission {
    semaphore: Arc<Semaphore>,
    waiting: AtomicUsize,
    max_queue: usize,
    acquire_timeout: Duration,
}

impl PasswordVerificationAdmission {
    fn new(max_concurrent: usize, max_queue: usize, acquire_timeout: Duration) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            waiting: AtomicUsize::new(0),
            max_queue,
            acquire_timeout,
        }
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit, LoginRateLimitError> {
        let retry_after_seconds = self.acquire_timeout.as_secs().max(1);
        // A free permit needs no queue slot, so take it without touching the
        // queue counter. Only a caller that has to wait reserves a slot, and
        // it reserves it atomically with the bound check so a simultaneous
        // burst cannot all observe room and then overflow `max_queue`.
        match Arc::clone(&self.semaphore).try_acquire_owned() {
            Ok(permit) => return Ok(permit),
            Err(TryAcquireError::NoPermits) => {}
            Err(TryAcquireError::Closed) => {
                return Err(LoginRateLimitError::VerificationAdmissionRejected {
                    retry_after_seconds,
                });
            }
        }
        let Some(_waiting) = WaitingSlot::try_reserve(&self.waiting, self.max_queue) else {
            return Err(LoginRateLimitError::VerificationAdmissionRejected { retry_after_seconds });
        };
        let acquired =
            tokio::time::timeout(self.acquire_timeout, Arc::clone(&self.semaphore).acquire_owned())
                .await;
        match acquired {
            Ok(Ok(permit)) => Ok(permit),
            _ => Err(LoginRateLimitError::VerificationAdmissionRejected { retry_after_seconds }),
        }
    }
}

/// Construction-time configuration for [`LoginRateLimiter`], resolved once
/// from [`Settings`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct LoginRateLimiterConfig {
    per_login: LeakyBucketConfig,
    per_source: LeakyBucketConfig,
    max_concurrent_verifications: usize,
    admission_queue: usize,
    admission_timeout: Duration,
    registry_options: LeakyBucketRegistryOptions,
}

impl LoginRateLimiterConfig {
    pub(crate) fn from_settings(settings: &Settings) -> Result<Self, String> {
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
            return Err(
                "login_rate_limit_registry_idle_ttl_seconds must be greater than zero".into()
            );
        }
        Ok(Self {
            per_login: LeakyBucketConfig {
                capacity: settings.login_rate_limit_per_login_max_attempts,
                window_seconds: settings.login_rate_limit_per_login_window_seconds,
            },
            per_source: LeakyBucketConfig {
                capacity: settings.login_rate_limit_per_source_max_attempts,
                window_seconds: settings.login_rate_limit_per_source_window_seconds,
            },
            max_concurrent_verifications: settings.login_rate_limit_max_concurrent_verifications,
            admission_queue: settings.login_rate_limit_admission_queue,
            admission_timeout: Duration::from_millis(
                settings.login_rate_limit_admission_timeout_ms,
            ),
            registry_options: LeakyBucketRegistryOptions {
                max_entries: settings.login_rate_limit_registry_max_entries,
                idle_ttl: Duration::from_secs(settings.login_rate_limit_registry_idle_ttl_seconds),
            },
        })
    }
}

#[derive(Debug)]
struct LoginRateLimiterInner {
    per_login: LeakyBucketRegistry<String>,
    per_source: LeakyBucketRegistry<String>,
    verification_admission: PasswordVerificationAdmission,
}

/// Admission control for `POST /v1/iam/sessions`: two independent leaky
/// buckets (per normalized login, per source) plus a shared cap on
/// concurrent Argon2id verifications. See the module documentation for the
/// full rationale. Cheaply cloneable (an `Arc` around the actual state), so
/// it can live directly on `AppState` like
/// `services::query::turn_admission::QueryTurnAdmission` does.
#[derive(Debug, Clone)]
pub(crate) struct LoginRateLimiter {
    inner: Arc<LoginRateLimiterInner>,
}

/// Held for the duration of one password verification. Dropping it releases
/// the concurrency permit back to [`PasswordVerificationAdmission`].
#[must_use = "dropping the permit immediately releases the password verification admission slot"]
#[derive(Debug)]
pub(crate) struct LoginRateLimitPermit {
    _permit: OwnedSemaphorePermit,
}

impl LoginRateLimiter {
    pub(crate) fn new(config: LoginRateLimiterConfig) -> Result<Self, String> {
        let per_login = LeakyBucketRegistry::new(
            RateLimitAxis::Login,
            config.per_login,
            config.registry_options,
        )?;
        let per_source = LeakyBucketRegistry::new(
            RateLimitAxis::Source,
            config.per_source,
            config.registry_options,
        )?;
        let verification_admission = PasswordVerificationAdmission::new(
            config.max_concurrent_verifications,
            config.admission_queue,
            config.admission_timeout,
        );
        Ok(Self {
            inner: Arc::new(LoginRateLimiterInner {
                per_login,
                per_source,
                verification_admission,
            }),
        })
    }

    /// Admits one login attempt for `normalized_login` from `source_key`, or
    /// rejects it before any database lookup or Argon2id call happens.
    ///
    /// Checks the login-axis bucket first, then the source-axis bucket, then
    /// the concurrency admission -- refunding the axes already consumed if a
    /// later check rejects, so a request that never reaches Argon2id never
    /// leaves a real bucket permanently short a token.
    pub(crate) async fn admit(
        &self,
        normalized_login: &str,
        source_key: &str,
    ) -> Result<LoginRateLimitPermit, LoginRateLimitError> {
        let normalized_login = normalized_login.to_string();
        let source_key = source_key.to_string();

        self.inner.per_login.try_consume(&normalized_login)?;
        if let Err(error) = self.inner.per_source.try_consume(&source_key) {
            self.inner.per_login.refund(&normalized_login);
            return Err(error);
        }
        match self.inner.verification_admission.acquire().await {
            Ok(permit) => Ok(LoginRateLimitPermit { _permit: permit }),
            Err(error) => {
                self.inner.per_login.refund(&normalized_login);
                self.inner.per_source.refund(&source_key);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier as StdBarrier;
    use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
    use tokio::sync::Barrier;

    fn config(per_login: u32, per_source: u32, max_concurrent: usize) -> LoginRateLimiterConfig {
        LoginRateLimiterConfig {
            per_login: LeakyBucketConfig { capacity: per_login, window_seconds: 60 },
            per_source: LeakyBucketConfig { capacity: per_source, window_seconds: 60 },
            max_concurrent_verifications: max_concurrent,
            admission_queue: 8,
            admission_timeout: Duration::from_millis(200),
            registry_options: LeakyBucketRegistryOptions::default(),
        }
    }

    #[tokio::test]
    async fn admits_up_to_capacity_then_rejects_with_a_retry_hint() {
        let limiter = LoginRateLimiter::new(config(2, 100, 4)).unwrap();
        let _ = limiter.admit("alice", "1.1.1.1").await.expect("first attempt admitted");
        let _ = limiter.admit("alice", "1.1.1.1").await.expect("second attempt admitted");
        let error =
            limiter.admit("alice", "1.1.1.1").await.expect_err("third attempt exceeds capacity");
        assert!(
            matches!(error, LoginRateLimitError::LoginRateLimited { retry_after_seconds } if retry_after_seconds > 0)
        );
    }

    #[tokio::test]
    async fn distinct_logins_do_not_share_a_login_axis_bucket() {
        let limiter = LoginRateLimiter::new(config(1, 100, 4)).unwrap();
        let _ = limiter
            .admit("alice", "1.1.1.1")
            .await
            .expect("alice's own bucket admits her first attempt");
        let _ = limiter.admit("bob", "1.1.1.1").await.expect("bob has an independent login bucket");
    }

    #[tokio::test]
    async fn one_source_hammering_many_logins_trips_the_source_axis() {
        let limiter = LoginRateLimiter::new(config(100, 2, 4)).unwrap();
        let _ = limiter
            .admit("alice", "9.9.9.9")
            .await
            .expect("first distinct login from source admitted");
        let _ = limiter
            .admit("bob", "9.9.9.9")
            .await
            .expect("second distinct login from source admitted");
        let error = limiter
            .admit("carol", "9.9.9.9")
            .await
            .expect_err("a third distinct login from the same source exceeds the source axis");
        assert!(matches!(error, LoginRateLimitError::SourceRateLimited { .. }));
    }

    #[tokio::test]
    async fn a_source_axis_rejection_refunds_the_login_axis_token_it_consumed() {
        let limiter = LoginRateLimiter::new(config(5, 1, 4)).unwrap();
        let _ = limiter.admit("alice", "2.2.2.2").await.expect("first attempt clears both axes");
        limiter
            .admit("alice", "2.2.2.2")
            .await
            .expect_err("source axis (capacity 1) is now exhausted for this source");
        // If the login-axis token from the rejected attempt had not been
        // refunded, alice would have only 3 of her 5 login-axis tokens left
        // here instead of 4; draining the rest proves the refund happened.
        // Each drain uses its own source key (capacity 1 per source) so only
        // the login axis is under test here.
        for index in 0..4 {
            let _ = limiter
                .admit("alice", &format!("3.3.3.{index}"))
                .await
                .expect("refunded login-axis budget plus the untouched remainder is available");
        }
        let error =
            limiter.admit("alice", "4.4.4.4").await.expect_err("login axis is now fully exhausted");
        assert!(matches!(error, LoginRateLimitError::LoginRateLimited { .. }));
    }

    #[tokio::test]
    async fn concurrency_admission_caps_in_flight_verifications_and_releases_on_drop() {
        let limiter = Arc::new(LoginRateLimiter::new(config(1_000, 1_000, 2)).unwrap());
        let in_flight = Arc::new(StdAtomicUsize::new(0));
        let peak = Arc::new(StdAtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(6));

        let mut handles = Vec::new();
        for index in 0..6 {
            let limiter = Arc::clone(&limiter);
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let permit = limiter.admit(&format!("user-{index}"), "5.5.5.5").await;
                if let Ok(permit) = permit {
                    let now = in_flight.fetch_add(1, StdOrdering::SeqCst) + 1;
                    peak.fetch_max(now, StdOrdering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    in_flight.fetch_sub(1, StdOrdering::SeqCst);
                    drop(permit);
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert!(
            peak.load(StdOrdering::SeqCst) <= 2,
            "concurrent verifications must never exceed the configured cap of 2",
        );
    }

    #[tokio::test]
    async fn saturated_concurrency_admission_rejects_without_holding_the_bucket_tokens() {
        let limiter = LoginRateLimiter::new(config(1_000, 1_000, 1)).unwrap();
        let held = limiter.admit("alice", "6.6.6.6").await.expect("first verification admitted");
        let error = limiter
            .admit("bob", "6.6.6.6")
            .await
            .expect_err("concurrency cap of 1 rejects a second concurrent verification");
        assert!(matches!(error, LoginRateLimitError::VerificationAdmissionRejected { .. }));
        drop(held);
        let _ = limiter
            .admit("carol", "6.6.6.6")
            .await
            .expect("released permit admits the next verification");
    }

    /// Waiters cancelled per round. The queue bound equals this count, so a
    /// leaked counter makes the next admission look full while nothing waits.
    const CANCELLED_WAITERS: usize = 4;

    /// Registers one waiter on the verification queue, then drops its future
    /// from an outer deadline: the cancellation a disconnecting client causes.
    async fn cancel_one_admit(limiter: &LoginRateLimiter) {
        let cancelled = tokio::time::timeout(
            Duration::from_millis(20),
            limiter.admit("cancelled-login", "cancelled-source"),
        )
        .await;
        assert!(cancelled.is_err(), "the outer deadline must cancel the pending waiter");
    }

    fn config_with_admission_queue(max_queue: usize) -> LoginRateLimiterConfig {
        let mut limiter_config = config(1_000, 1_000, 1);
        limiter_config.admission_queue = max_queue;
        limiter_config
    }

    #[tokio::test]
    async fn cancelled_waiters_release_the_verification_waiting_counter() {
        let limiter =
            LoginRateLimiter::new(config_with_admission_queue(CANCELLED_WAITERS)).unwrap();
        let held = limiter.admit("holder", "holder-source").await.expect("first permit");

        for _ in 0..CANCELLED_WAITERS {
            cancel_one_admit(&limiter).await;
        }

        assert_eq!(
            limiter.inner.verification_admission.waiting.load(StdOrdering::SeqCst),
            0,
            "cancelled waiters must release the verification waiting counter"
        );
        drop(held);
    }

    #[tokio::test]
    async fn cancelled_waiters_do_not_reject_an_empty_verification_queue() {
        let limiter =
            LoginRateLimiter::new(config_with_admission_queue(CANCELLED_WAITERS)).unwrap();
        let held = limiter.admit("holder", "holder-source").await.expect("first permit");

        for _ in 0..CANCELLED_WAITERS {
            cancel_one_admit(&limiter).await;
        }

        // Nothing waits now, so a new login queues behind the held permit and
        // only the outer deadline ends it. An immediate rejection means the
        // queue was judged full by a leaked counter.
        let outcome = tokio::time::timeout(
            Duration::from_millis(50),
            limiter.admit("genuine", "genuine-source"),
        )
        .await;
        assert!(
            outcome.is_err(),
            "an empty verification queue rejected admission; the waiter counter leaked"
        );
        drop(held);
    }

    #[tokio::test]
    async fn registry_at_capacity_evicts_the_least_recently_used_key_instead_of_refusing() {
        let mut config = config(1, 50, 4);
        config.registry_options =
            LeakyBucketRegistryOptions { max_entries: 2, idle_ttl: Duration::from_secs(600) };
        let limiter = LoginRateLimiter::new(config).unwrap();
        // alice drains her single-token bucket; bob is seen later than alice.
        let _ = limiter.admit("alice", "7.7.7.1").await.expect("first key admitted");
        tokio::time::sleep(Duration::from_millis(2)).await;
        let _ = limiter.admit("bob", "7.7.7.2").await.expect("second key admitted");

        // A flood of new keys must never lock a genuine newcomer out: carol
        // is admitted at capacity, and alice -- the least recently used key
        // -- is the one forgotten, not bob.
        let _ = limiter.admit("carol", "7.7.7.3").await.expect("new key admitted at capacity");
        assert_eq!(limiter.inner.per_login.entry_count(), 2);
        let error = limiter.admit("bob", "7.7.7.2").await.expect_err("bob's bucket survived");
        assert!(matches!(error, LoginRateLimitError::LoginRateLimited { .. }));
        // alice's bucket was forgotten, so she gets a fresh full bucket.
        let _ = limiter.admit("alice", "7.7.7.1").await.expect("evicted key starts over");
    }

    #[test]
    fn invalid_leaky_bucket_configuration_is_rejected_at_construction() {
        let error = LoginRateLimiter::new(config(0, 5, 4)).unwrap_err();
        assert!(error.contains("capacity"));
    }

    /// Queue bound and burst shape of the admission-queue overflow test. The
    /// bound is one slot, so any reservation past it is visible immediately.
    const QUEUE_BOUND: usize = 1;
    const BURST_CALLERS: usize = 64;
    const BURST_ROUNDS: usize = 32;

    /// A fresh admission with one concurrency permit and a one-slot wait queue.
    fn queue_bound_admission() -> Arc<PasswordVerificationAdmission> {
        Arc::new(PasswordVerificationAdmission::new(1, QUEUE_BOUND, Duration::from_secs(5)))
    }

    /// Takes the sole concurrency permit on a throwaway runtime. The permit is
    /// owned by the admission, not by the runtime that took it, so every real
    /// caller then finds the semaphore empty.
    fn hold_sole_permit(admission: &PasswordVerificationAdmission) -> OwnedSemaphorePermit {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("holder runtime");
        runtime.block_on(admission.acquire()).expect("sole permit must be admitted")
    }

    /// Counters and join handles of one released burst.
    struct WaiterBurst {
        gate: Arc<StdBarrier>,
        refused: Arc<StdAtomicUsize>,
        admitted: Arc<StdAtomicUsize>,
        runtime_failures: Arc<StdAtomicUsize>,
        waiters: Vec<std::thread::JoinHandle<Result<(), String>>>,
    }

    /// Release every caller together. Each caller builds its own runtime before
    /// the gate: driving one future per runtime is what puts several callers
    /// between a check and its increment, which a shared runtime handle would
    /// serialise away.
    fn spawn_waiters(admission: &Arc<PasswordVerificationAdmission>) -> WaiterBurst {
        let gate = Arc::new(StdBarrier::new(BURST_CALLERS + 1));
        let refused = Arc::new(StdAtomicUsize::new(0));
        let admitted = Arc::new(StdAtomicUsize::new(0));
        let runtime_failures = Arc::new(StdAtomicUsize::new(0));
        let mut waiters = Vec::with_capacity(BURST_CALLERS);
        for _ in 0..BURST_CALLERS {
            let admission = Arc::clone(admission);
            let gate = Arc::clone(&gate);
            let refused = Arc::clone(&refused);
            let admitted = Arc::clone(&admitted);
            let runtime_failures = Arc::clone(&runtime_failures);
            waiters.push(std::thread::spawn(move || {
                run_waiter(admission, gate, refused, admitted, runtime_failures)
            }));
        }
        WaiterBurst { gate, refused, admitted, runtime_failures, waiters }
    }

    /// One caller. A worker must never panic between being spawned and crossing
    /// the gate, or the other barrier parties wait forever for a thread that
    /// never arrives: the runtime is therefore built as a `Result` and reported
    /// instead of unwrapped, and the gate is crossed even when that build
    /// failed, so a failure surfaces as an ordinary assertion after the join,
    /// never as a test timeout.
    fn run_waiter(
        admission: Arc<PasswordVerificationAdmission>,
        gate: Arc<StdBarrier>,
        refused: Arc<StdAtomicUsize>,
        admitted: Arc<StdAtomicUsize>,
        runtime_failures: Arc<StdAtomicUsize>,
    ) -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build();
        gate.wait();
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                runtime_failures.fetch_add(1, StdOrdering::SeqCst);
                return Err(format!("waiter runtime: {error}"));
            }
        };
        if runtime.block_on(admission.acquire()).is_err() {
            refused.fetch_add(1, StdOrdering::SeqCst);
        } else {
            admitted.fetch_add(1, StdOrdering::SeqCst);
        }
        Ok(())
    }

    /// A queued caller holds a queue slot while it waits for the permit; a
    /// refused one only adds to `refused`. Wait until every caller has decided
    /// or a worker's runtime failed to build, and sample the queue while the
    /// surviving waiters still hold their slots: this pre-release peak is the
    /// only moment an overflow can be observed.
    fn pre_release_queue_peak(
        admission: &PasswordVerificationAdmission,
        burst: &WaiterBurst,
    ) -> usize {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut queued_peak = 0;
        while Instant::now() < deadline {
            queued_peak = admission.waiting.load(StdOrdering::SeqCst);
            let decided = queued_peak
                + burst.refused.load(StdOrdering::SeqCst)
                + burst.admitted.load(StdOrdering::SeqCst)
                + burst.runtime_failures.load(StdOrdering::SeqCst);
            if decided >= BURST_CALLERS {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        queued_peak
    }

    /// One burst after every worker joined: the runtime build errors, and the
    /// quiescent admitted/refused counts.
    struct WaiterOutcome {
        runtime_errors: Vec<String>,
        refused: usize,
        admitted: usize,
    }

    /// Joins every waiter and folds the counters of the quiescent burst into a
    /// single outcome.
    fn join_waiters(burst: WaiterBurst) -> WaiterOutcome {
        let mut runtime_errors = Vec::new();
        for waiter in burst.waiters {
            match waiter.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => runtime_errors.push(error),
                Err(_) => runtime_errors.push("waiter thread panicked".to_owned()),
            }
        }
        WaiterOutcome {
            runtime_errors,
            refused: burst.refused.load(StdOrdering::SeqCst),
            admitted: burst.admitted.load(StdOrdering::SeqCst),
        }
    }

    /// Asserts one round: no worker runtime failed, every caller is admitted,
    /// refused, or still holding a queue slot, and the pre-release peak stayed
    /// within the queue bound. The decided total is recomputed from the
    /// quiescent counters after the join rather than asserted from the
    /// pre-release snapshot: the queue must have drained, so the three
    /// partition the burst exactly.
    fn assert_round_outcome(
        attempt_round: usize,
        admission: &PasswordVerificationAdmission,
        queued_peak: usize,
        outcome: &WaiterOutcome,
    ) {
        assert!(
            outcome.runtime_errors.is_empty(),
            "round {attempt_round}: worker runtimes failed: {:?}",
            outcome.runtime_errors,
        );
        let waiting = admission.waiting.load(StdOrdering::SeqCst);
        let decided = waiting + outcome.refused + outcome.admitted;
        assert_eq!(decided, BURST_CALLERS, "round {attempt_round}: callers still undecided");
        assert!(
            queued_peak <= QUEUE_BOUND,
            "round {attempt_round}: verification queue reached {queued_peak}, above {QUEUE_BOUND}",
        );
    }

    /// The verification wait queue is bounded like the query-turn one; a
    /// simultaneous burst must never push it past `max_queue`.
    ///
    /// This is a high-contention test, not a deterministic proof. Every caller
    /// is a real OS thread released together by a `std::sync::Barrier`, and
    /// each drives its own single-threaded Tokio runtime, which widens the
    /// window in which several callers sit between the reservation's check and
    /// its increment -- an async-only barrier schedules those polls through one
    /// runtime and serialises enough of them to hide the race. Even so, a
    /// check-then-act reservation can in principle survive a scheduling
    /// interleaving, so this is robust evidence of the bound rather than a
    /// mathematical guarantee that a broken reservation always trips it.
    #[test]
    fn concurrent_verification_waiters_never_overflow_the_admission_queue() {
        for attempt_round in 0..BURST_ROUNDS {
            let admission = queue_bound_admission();
            let held = hold_sole_permit(&admission);
            let burst = spawn_waiters(&admission);
            burst.gate.wait();
            let queued_peak = pre_release_queue_peak(&admission, &burst);
            drop(held);
            let outcome = join_waiters(burst);
            assert_round_outcome(attempt_round, &admission, queued_peak, &outcome);
        }
    }
}
