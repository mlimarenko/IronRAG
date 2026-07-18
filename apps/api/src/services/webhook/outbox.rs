//! Bounded dispatcher for durable content-lifecycle webhook events.
//!
//! Each scheduler tick leases a small batch. Fanout delegates to the existing
//! per-subscription atomic enqueue path, whose queue dedupe makes replay after
//! a lease expiry or partial failure safe.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{Instant as TokioInstant, interval, interval_at},
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::deployment::ServiceRole,
    domains::webhook::WebhookEvent,
    infra::repositories::webhook_outbox_repository,
    services::webhook::outbound::{
        WebhookFanoutFailure, WebhookFanoutFailureKind, WebhookTargetFanoutOutcome,
        publish_webhook_event_to_targets_cancellable,
    },
};

pub const DEFAULT_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT: i64 = 16;
const WEBHOOK_LIFECYCLE_OUTBOX_POLL_INTERVAL: Duration = Duration::from_secs(1);
const WEBHOOK_LIFECYCLE_OUTBOX_LEASE_DURATION: Duration = Duration::from_mins(5);
const WEBHOOK_LIFECYCLE_OUTBOX_HEARTBEAT_INTERVAL: Duration = Duration::from_mins(1);
const WEBHOOK_LIFECYCLE_OUTBOX_MAX_ATTEMPTS: i32 = 12;
const WEBHOOK_LIFECYCLE_OUTBOX_ERROR_CHARS: usize = 2_000;
const WEBHOOK_LIFECYCLE_OUTBOX_MAX_BACKOFF_SECONDS: i64 = 30 * 60;
const WEBHOOK_LIFECYCLE_OUTBOX_RETENTION_DAYS: i64 = 30;
const WEBHOOK_LIFECYCLE_OUTBOX_PRUNE_INTERVAL: Duration = Duration::from_hours(1);
const WEBHOOK_LIFECYCLE_OUTBOX_PRUNE_BATCH: i64 = 1_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WebhookLifecycleOutboxDrainReport {
    pub leased: u64,
    pub dispatched: u64,
    pub retried: u64,
    pub dead_lettered: u64,
    pub lease_conflicts: u64,
    pub state_update_failures: u64,
    pub cancelled: u64,
}

struct WebhookLifecycleOutboxMetrics {
    event_age_seconds: Histogram<f64>,
    drain_duration_seconds: Histogram<f64>,
    lease_conflicts: Counter<u64>,
    lease_renewals: Counter<u64>,
    outcomes: Counter<u64>,
}

fn webhook_lifecycle_outbox_metrics() -> &'static WebhookLifecycleOutboxMetrics {
    static METRICS: OnceLock<WebhookLifecycleOutboxMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ironrag.webhook");
        WebhookLifecycleOutboxMetrics {
            event_age_seconds: meter
                .f64_histogram("ironrag.webhook.lifecycle_outbox.event_age_seconds")
                .with_description("Age of a lifecycle outbox event when relay processing starts")
                .with_unit("s")
                .build(),
            drain_duration_seconds: meter
                .f64_histogram("ironrag.webhook.lifecycle_outbox.drain_duration_seconds")
                .with_description("Wall-clock duration of a bounded lifecycle outbox drain")
                .with_unit("s")
                .build(),
            lease_conflicts: meter
                .u64_counter("ironrag.webhook.lifecycle_outbox.lease_conflicts")
                .with_description("Lifecycle outbox fenced state updates rejected by lease CAS")
                .with_unit("{conflict}")
                .build(),
            lease_renewals: meter
                .u64_counter("ironrag.webhook.lifecycle_outbox.lease_renewals")
                .with_description("Lifecycle outbox lease-heartbeat outcomes")
                .with_unit("{renewal}")
                .build(),
            outcomes: meter
                .u64_counter("ironrag.webhook.lifecycle_outbox.outcomes")
                .with_description("Terminal outcome of lifecycle outbox relay processing")
                .with_unit("{event}")
                .build(),
        }
    })
}

/// Starts the correctness-critical worker-role relay independently from the
/// optional maintenance kill switch.
#[must_use]
pub fn spawn_webhook_lifecycle_outbox_relay(
    state: AppState,
    mut shutdown: broadcast::Receiver<()>,
) -> Option<JoinHandle<()>> {
    let worker_role =
        state.settings.service_role_kind().ok().is_some_and(ServiceRole::runs_ingestion_workers);
    if !worker_role {
        return None;
    }
    let lease_owner = std::env::var("HOSTNAME")
        .map_or_else(
            |_| format!("webhook-outbox-{}", Uuid::now_v7()),
            |hostname| format!("webhook-outbox-{hostname}"),
        )
        .chars()
        .take(255)
        .collect::<String>();
    Some(tokio::spawn(async move {
        let cancellation_token = CancellationToken::new();
        let shutdown_token = cancellation_token.clone();
        let shutdown_watcher = tokio::spawn(async move {
            let _ = shutdown.recv().await;
            shutdown_token.cancel();
        });
        info!(
            poll_interval_ms = WEBHOOK_LIFECYCLE_OUTBOX_POLL_INTERVAL.as_millis(),
            batch_limit = DEFAULT_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT,
            lease_owner = %lease_owner,
            "webhook lifecycle outbox relay starting",
        );
        drain_and_log(&state, &lease_owner, &cancellation_token).await;
        let mut poll = interval(WEBHOOK_LIFECYCLE_OUTBOX_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        poll.tick().await;
        let mut prune = interval(WEBHOOK_LIFECYCLE_OUTBOX_PRUNE_INTERVAL);
        prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        prune.tick().await;
        loop {
            tokio::select! {
                () = cancellation_token.cancelled() => {
                    shutdown_watcher.abort();
                    return;
                },
                _ = poll.tick() => {
                    drain_and_log(&state, &lease_owner, &cancellation_token).await;
                },
                _ = prune.tick() => prune_and_log(&state).await,
            }
        }
    }))
}

async fn prune_and_log(state: &AppState) {
    let cutoff = Utc::now() - ChronoDuration::days(WEBHOOK_LIFECYCLE_OUTBOX_RETENTION_DAYS);
    match webhook_outbox_repository::prune_dispatched_webhook_lifecycle_outbox(
        &state.persistence.postgres,
        cutoff,
        WEBHOOK_LIFECYCLE_OUTBOX_PRUNE_BATCH,
    )
    .await
    {
        Ok(0) => {}
        Ok(pruned) => info!(pruned, "pruned dispatched webhook lifecycle outbox rows"),
        Err(error) => warn!(?error, "webhook lifecycle outbox prune failed; continuing"),
    }
}

async fn drain_and_log(
    state: &AppState,
    lease_owner: &str,
    cancellation_token: &CancellationToken,
) {
    match drain_webhook_lifecycle_outbox_once_with_cancellation(
        state,
        lease_owner,
        DEFAULT_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT,
        cancellation_token,
    )
    .await
    {
        Ok(report) if report.leased > 0 => info!(
            leased = report.leased,
            dispatched = report.dispatched,
            retried = report.retried,
            dead_lettered = report.dead_lettered,
            lease_conflicts = report.lease_conflicts,
            state_update_failures = report.state_update_failures,
            cancelled = report.cancelled,
            "webhook lifecycle outbox drain completed",
        ),
        Ok(_) => {}
        Err(error) => warn!(?error, "webhook lifecycle outbox drain failed; continuing"),
    }
}

/// Drains at most `batch_limit` due lifecycle events once.
///
/// One row is leased at a time. This prevents later rows in a batch from
/// expiring while a large recipient set is still being fanned out. The active
/// row is renewed under its token fence until fanout completes.
pub async fn drain_webhook_lifecycle_outbox_once(
    state: &AppState,
    lease_owner: &str,
    batch_limit: i64,
) -> anyhow::Result<WebhookLifecycleOutboxDrainReport> {
    drain_webhook_lifecycle_outbox_once_with_cancellation(
        state,
        lease_owner,
        batch_limit,
        &CancellationToken::new(),
    )
    .await
}

async fn drain_webhook_lifecycle_outbox_once_with_cancellation(
    state: &AppState,
    lease_owner: &str,
    batch_limit: i64,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<WebhookLifecycleOutboxDrainReport> {
    let started_at = Instant::now();
    let result =
        drain_webhook_lifecycle_outbox_impl(state, lease_owner, batch_limit, cancellation_token)
            .await;
    let status = if cancellation_token.is_cancelled() {
        "cancelled"
    } else if result.is_ok() {
        "completed"
    } else {
        "failed"
    };
    webhook_lifecycle_outbox_metrics()
        .drain_duration_seconds
        .record(started_at.elapsed().as_secs_f64(), &[KeyValue::new("status", status)]);
    result
}

async fn drain_webhook_lifecycle_outbox_impl(
    state: &AppState,
    lease_owner: &str,
    batch_limit: i64,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<WebhookLifecycleOutboxDrainReport> {
    let lease_duration = ChronoDuration::from_std(WEBHOOK_LIFECYCLE_OUTBOX_LEASE_DURATION)
        .unwrap_or_else(|_| ChronoDuration::minutes(5));
    let limit =
        batch_limit.clamp(1, webhook_outbox_repository::MAX_WEBHOOK_LIFECYCLE_OUTBOX_BATCH_LIMIT);
    let mut report = WebhookLifecycleOutboxDrainReport::default();

    for _ in 0..limit {
        if cancellation_token.is_cancelled() {
            break;
        }
        let lease = tokio::select! {
            () = cancellation_token.cancelled() => break,
            result = webhook_outbox_repository::lease_webhook_lifecycle_outbox_batch(
                &state.persistence.postgres,
                lease_owner,
                lease_duration,
                1,
            ) => result?,
        };
        let Some(row) = lease.events.into_iter().next() else {
            break;
        };
        report.leased += 1;
        record_outbox_event_age(&row);

        let failures = match fanout_leased_webhook_lifecycle_event(
            state,
            &row,
            lease.lease_token,
            lease_duration,
            cancellation_token,
        )
        .await
        {
            LeasedWebhookFanoutOutcome::Completed(failures) => failures,
            LeasedWebhookFanoutOutcome::LeaseLost => {
                report.lease_conflicts += 1;
                record_lease_conflict("heartbeat");
                record_outbox_outcome("lease_lost");
                continue;
            }
            LeasedWebhookFanoutOutcome::HeartbeatFailed => {
                report.state_update_failures += 1;
                record_outbox_outcome("heartbeat_failed");
                continue;
            }
            LeasedWebhookFanoutOutcome::Cancelled => {
                report.cancelled += 1;
                record_outbox_outcome("cancelled");
                break;
            }
        };

        if failures.is_empty() {
            match webhook_outbox_repository::mark_webhook_lifecycle_outbox_dispatched(
                &state.persistence.postgres,
                row.id,
                lease.lease_token,
            )
            .await
            {
                Ok(true) => {
                    report.dispatched += 1;
                    record_outbox_outcome("dispatched");
                }
                Ok(false) => {
                    report.lease_conflicts += 1;
                    record_lease_conflict("completion");
                    record_outbox_outcome("lease_lost");
                }
                Err(error) => {
                    report.state_update_failures += 1;
                    record_outbox_outcome("state_update_failed");
                    warn!(
                        outbox_id = %row.id,
                        event_id = %row.event_id,
                        ?error,
                        "webhook lifecycle outbox dispatch succeeded but completion update failed",
                    );
                }
            }
            continue;
        }

        let retry_disposition = webhook_fanout_retry_disposition(&failures);
        let retry_at = next_dispatch_retry_at(row.dispatch_attempts);
        let failure = redacted_dispatch_failure(&failures);
        match webhook_outbox_repository::fail_webhook_lifecycle_outbox_dispatch(
            &state.persistence.postgres,
            row.id,
            lease.lease_token,
            retry_at,
            failure.code,
            &failure.message,
            WEBHOOK_LIFECYCLE_OUTBOX_MAX_ATTEMPTS,
            retry_disposition.is_retryable(),
        )
        .await
        {
            Ok(Some(state)) if state == "pending" => {
                report.retried += 1;
                record_outbox_outcome("retried");
            }
            Ok(Some(state)) if state == "dead_letter" => {
                report.dead_lettered += 1;
                record_outbox_outcome("dead_lettered");
                warn!(
                    outbox_id = %row.id,
                    event_id = %row.event_id,
                    dispatch_attempts = row.dispatch_attempts,
                    error_code = failure.code,
                    ?retry_disposition,
                    "webhook lifecycle outbox event entered the dead-letter state",
                );
            }
            Ok(Some(state)) => {
                report.state_update_failures += 1;
                record_outbox_outcome("state_update_failed");
                warn!(
                    outbox_id = %row.id,
                    event_id = %row.event_id,
                    dispatch_state = %state,
                    "webhook lifecycle outbox failure update returned an unexpected state",
                );
            }
            Ok(None) => {
                report.lease_conflicts += 1;
                record_lease_conflict("failure_release");
                record_outbox_outcome("lease_lost");
            }
            Err(error) => {
                report.state_update_failures += 1;
                record_outbox_outcome("state_update_failed");
                warn!(
                    outbox_id = %row.id,
                    event_id = %row.event_id,
                    ?error,
                    "webhook lifecycle outbox failure update failed",
                );
            }
        }
    }

    Ok(report)
}

enum LeasedWebhookFanoutOutcome {
    Completed(Vec<WebhookFanoutFailure>),
    LeaseLost,
    HeartbeatFailed,
    Cancelled,
}

async fn fanout_leased_webhook_lifecycle_event(
    state: &AppState,
    row: &webhook_outbox_repository::WebhookLifecycleOutboxRow,
    lease_token: Uuid,
    lease_duration: ChronoDuration,
    cancellation_token: &CancellationToken,
) -> LeasedWebhookFanoutOutcome {
    let fanout_cancellation = cancellation_token.child_token();
    let event = WebhookEvent {
        event_type: row.event_type.clone(),
        event_id: row.event_id.clone(),
        occurred_at: row.occurred_at,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        payload_json: row.payload_json.clone(),
    };
    let fanout = async {
        let targets = tokio::select! {
            () = fanout_cancellation.cancelled() => {
                return WebhookTargetFanoutOutcome::Cancelled;
            }
            result = webhook_outbox_repository::list_active_webhook_lifecycle_recipient_targets(
                &state.persistence.postgres,
                row.id,
            ) => match result {
                Ok(targets) => targets,
                Err(error) => {
                    warn!(
                        outbox_id = %row.id,
                        event_id = %row.event_id,
                        ?error,
                        "webhook lifecycle outbox recipient lookup failed",
                    );
                    return WebhookTargetFanoutOutcome::Completed(vec![
                        WebhookFanoutFailure::new(
                            WebhookFanoutFailureKind::RecipientSnapshotLookup,
                            error.to_string(),
                        ),
                    ]);
                }
            },
        };
        publish_webhook_event_to_targets_cancellable(
            &state.persistence.postgres,
            &event,
            &targets,
            &fanout_cancellation,
        )
        .await
    };
    tokio::pin!(fanout);

    let first_heartbeat = TokioInstant::now() + WEBHOOK_LIFECYCLE_OUTBOX_HEARTBEAT_INTERVAL;
    let mut heartbeat = interval_at(first_heartbeat, WEBHOOK_LIFECYCLE_OUTBOX_HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            () = cancellation_token.cancelled() => {
                fanout_cancellation.cancel();
                return LeasedWebhookFanoutOutcome::Cancelled;
            }
            outcome = &mut fanout => {
                return match outcome {
                    WebhookTargetFanoutOutcome::Completed(failures) => {
                        LeasedWebhookFanoutOutcome::Completed(failures)
                    }
                    WebhookTargetFanoutOutcome::Cancelled => {
                        LeasedWebhookFanoutOutcome::Cancelled
                    }
                };
            }
            _ = heartbeat.tick() => {
                let renewal = tokio::select! {
                    () = cancellation_token.cancelled() => {
                        fanout_cancellation.cancel();
                        return LeasedWebhookFanoutOutcome::Cancelled;
                    }
                    result = webhook_outbox_repository::renew_webhook_lifecycle_outbox_lease(
                        &state.persistence.postgres,
                        row.id,
                        lease_token,
                        lease_duration,
                    ) => result,
                };
                match renewal {
                    Ok(true) => {
                        webhook_lifecycle_outbox_metrics().lease_renewals.add(
                            1,
                            &[KeyValue::new("outcome", "renewed")],
                        );
                    }
                    Ok(false) => {
                        webhook_lifecycle_outbox_metrics().lease_renewals.add(
                            1,
                            &[KeyValue::new("outcome", "lease_lost")],
                        );
                        fanout_cancellation.cancel();
                        return LeasedWebhookFanoutOutcome::LeaseLost;
                    }
                    Err(error) => {
                        webhook_lifecycle_outbox_metrics().lease_renewals.add(
                            1,
                            &[KeyValue::new("outcome", "error")],
                        );
                        fanout_cancellation.cancel();
                        warn!(
                            outbox_id = %row.id,
                            event_id = %row.event_id,
                            ?error,
                            "webhook lifecycle outbox lease heartbeat failed; fanout cancelled",
                        );
                        return LeasedWebhookFanoutOutcome::HeartbeatFailed;
                    }
                }
            }
        }
    }
}

fn record_outbox_event_age(row: &webhook_outbox_repository::WebhookLifecycleOutboxRow) {
    let observed_at = row.leased_at.unwrap_or(row.updated_at);
    let age_milliseconds = (observed_at - row.created_at).num_milliseconds().max(0);
    webhook_lifecycle_outbox_metrics()
        .event_age_seconds
        .record(age_milliseconds as f64 / 1_000.0, &[]);
}

fn record_lease_conflict(phase: &'static str) {
    webhook_lifecycle_outbox_metrics().lease_conflicts.add(1, &[KeyValue::new("phase", phase)]);
}

fn record_outbox_outcome(outcome: &'static str) {
    webhook_lifecycle_outbox_metrics().outcomes.add(1, &[KeyValue::new("outcome", outcome)]);
}

fn next_dispatch_retry_at(dispatch_attempts: i32) -> DateTime<Utc> {
    let exponent = u32::try_from(dispatch_attempts.saturating_sub(1).clamp(0, 8)).unwrap_or(0);
    let delay_seconds = (5_i64.saturating_mul(2_i64.saturating_pow(exponent)))
        .min(WEBHOOK_LIFECYCLE_OUTBOX_MAX_BACKOFF_SECONDS);
    Utc::now() + ChronoDuration::seconds(delay_seconds)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebhookFanoutRetryDisposition {
    Retry,
    DeadLetter,
}

impl WebhookFanoutRetryDisposition {
    const fn is_retryable(self) -> bool {
        matches!(self, Self::Retry)
    }
}

const fn webhook_fanout_failure_retry_disposition(
    kind: WebhookFanoutFailureKind,
) -> WebhookFanoutRetryDisposition {
    match kind {
        WebhookFanoutFailureKind::InvalidEventPayload => WebhookFanoutRetryDisposition::DeadLetter,
        WebhookFanoutFailureKind::SubscriptionLookup
        | WebhookFanoutFailureKind::RecipientSnapshotLookup
        | WebhookFanoutFailureKind::DeliveryEnqueue
        | WebhookFanoutFailureKind::Cancelled
        | WebhookFanoutFailureKind::Unclassified => WebhookFanoutRetryDisposition::Retry,
    }
}

fn webhook_fanout_retry_disposition(
    failures: &[WebhookFanoutFailure],
) -> WebhookFanoutRetryDisposition {
    failures
        .iter()
        .map(|failure| webhook_fanout_failure_retry_disposition(failure.kind()))
        .find(|disposition| *disposition == WebhookFanoutRetryDisposition::DeadLetter)
        .unwrap_or(WebhookFanoutRetryDisposition::Retry)
}

struct RedactedDispatchFailure {
    code: &'static str,
    message: String,
}

fn redacted_dispatch_failure(failures: &[WebhookFanoutFailure]) -> RedactedDispatchFailure {
    let mut lookup_failure_count = 0;
    let mut enqueue_failure_count = 0;
    let mut other_failure_count = 0;
    for failure in failures {
        match failure.kind() {
            WebhookFanoutFailureKind::SubscriptionLookup
            | WebhookFanoutFailureKind::RecipientSnapshotLookup => lookup_failure_count += 1,
            WebhookFanoutFailureKind::DeliveryEnqueue => enqueue_failure_count += 1,
            WebhookFanoutFailureKind::InvalidEventPayload
            | WebhookFanoutFailureKind::Cancelled
            | WebhookFanoutFailureKind::Unclassified => other_failure_count += 1,
        }
    }
    let code = match (lookup_failure_count > 0, enqueue_failure_count > 0) {
        (true, true) => "fanout_mixed_failure",
        (true, false) => "recipient_lookup_failed",
        (false, true) => "delivery_enqueue_failed",
        (false, false) => "fanout_failed",
    };
    let message = format!(
        "webhook fanout failed: lookup_failures={lookup_failure_count}, \
         enqueue_failures={enqueue_failure_count}, other_failures={other_failure_count}, \
         total_failures={}",
        failures.len(),
    );
    RedactedDispatchFailure {
        code,
        message: message.chars().take(WEBHOOK_LIFECYCLE_OUTBOX_ERROR_CHARS).collect(),
    }
}

#[cfg(test)]
mod tests {
    use crate::services::webhook::outbound::{WebhookFanoutFailure, WebhookFanoutFailureKind};

    use super::{
        WEBHOOK_LIFECYCLE_OUTBOX_ERROR_CHARS, WEBHOOK_LIFECYCLE_OUTBOX_HEARTBEAT_INTERVAL,
        WEBHOOK_LIFECYCLE_OUTBOX_LEASE_DURATION, WebhookFanoutRetryDisposition,
        next_dispatch_retry_at, redacted_dispatch_failure, webhook_fanout_retry_disposition,
    };

    #[test]
    fn heartbeat_has_multiple_chances_before_lease_expiry() {
        assert!(WEBHOOK_LIFECYCLE_OUTBOX_HEARTBEAT_INTERVAL > std::time::Duration::ZERO);
        assert!(
            WEBHOOK_LIFECYCLE_OUTBOX_HEARTBEAT_INTERVAL.saturating_mul(3)
                < WEBHOOK_LIFECYCLE_OUTBOX_LEASE_DURATION,
            "heartbeat cadence must leave multiple renewal opportunities before expiry",
        );
    }

    #[test]
    fn dispatch_error_is_redacted_and_bounded() {
        let secret = "credential-that-must-not-be-persisted";
        let failure = redacted_dispatch_failure(&[
            WebhookFanoutFailure::new(
                WebhookFanoutFailureKind::RecipientSnapshotLookup,
                format!("database error containing {secret}"),
            ),
            WebhookFanoutFailure::new(
                WebhookFanoutFailureKind::DeliveryEnqueue,
                format!("failure containing {secret}"),
            ),
        ]);

        assert_eq!(failure.code, "fanout_mixed_failure");
        assert!(failure.message.chars().count() <= WEBHOOK_LIFECYCLE_OUTBOX_ERROR_CHARS);
        assert!(!failure.message.contains(secret));
        assert!(failure.message.contains("lookup_failures=1"));
        assert!(failure.message.contains("enqueue_failures=1"));
    }

    #[test]
    fn misleading_diagnostic_does_not_override_typed_failure_kind() {
        let failure = redacted_dispatch_failure(&[WebhookFanoutFailure::new(
            WebhookFanoutFailureKind::DeliveryEnqueue,
            "list_snapshot_recipients: misleading prose",
        )]);

        assert_eq!(failure.code, "delivery_enqueue_failed");
        assert!(failure.message.contains("lookup_failures=0"));
        assert!(failure.message.contains("enqueue_failures=1"));
    }

    #[test]
    fn unclassified_diagnostic_cannot_impersonate_a_known_failure_kind() {
        let failure = redacted_dispatch_failure(&[WebhookFanoutFailure::new(
            WebhookFanoutFailureKind::Unclassified,
            "enqueue_delivery[id]: misleading prose",
        )]);

        assert_eq!(failure.code, "fanout_failed");
        assert!(failure.message.contains("other_failures=1"));
    }

    #[test]
    fn invalid_event_payload_is_dead_lettered_regardless_of_diagnostic_prose() {
        let disposition = webhook_fanout_retry_disposition(&[WebhookFanoutFailure::new(
            WebhookFanoutFailureKind::InvalidEventPayload,
            "temporary recipient lookup failure",
        )]);

        assert_eq!(disposition, WebhookFanoutRetryDisposition::DeadLetter);
        assert!(!disposition.is_retryable());
    }

    #[test]
    fn retryable_kind_is_not_overridden_by_permanent_failure_prose() {
        let disposition = webhook_fanout_retry_disposition(&[WebhookFanoutFailure::new(
            WebhookFanoutFailureKind::DeliveryEnqueue,
            "invalid event payload",
        )]);

        assert_eq!(disposition, WebhookFanoutRetryDisposition::Retry);
        assert!(disposition.is_retryable());
    }

    #[test]
    fn retry_backoff_increases_and_remains_bounded() {
        let now = chrono::Utc::now();
        let first = next_dispatch_retry_at(1) - now;
        let later = next_dispatch_retry_at(8) - now;
        let capped = next_dispatch_retry_at(i32::MAX) - now;

        assert!(first < later);
        assert!(later <= capped);
        assert!(capped <= chrono::Duration::minutes(31));
    }
}
