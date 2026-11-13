//! Demand-driven topology preparation, independent of recurring maintenance.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, bail};
use tokio::{sync::broadcast, time};
use tokio_util::sync::CancellationToken;

use crate::{app::state::AppState, infra::repositories};

use super::{
    graph_stream::artifact,
    preparation::{self, Claim, PreparationStatus},
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// An operational attempt limit, not an HTTP timeout or graph-size limit.
pub const MAX_ATTEMPT_DURATION: Duration = Duration::from_secs(60 * 60);

pub async fn run_topology_preparation_worker(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let mut poll = time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    loop {
        if wait_for_poll(&mut shutdown, &mut poll).await {
            break;
        }
        if process_next(&state, &mut shutdown).await {
            break;
        }
    }
}

async fn process_next(state: &AppState, shutdown: &mut broadcast::Receiver<()>) -> bool {
    cleanup(state).await;
    match preparation::claim_next(state).await {
        Ok(Some(claim)) => Box::pin(run_claim(state, claim, shutdown)).await,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(%error, "graph preparation claim failed");
            false
        }
    }
}

async fn cleanup(state: &AppState) {
    if let Err(error) = preparation::cleanup_due(state).await {
        tracing::warn!(%error, "graph preparation cleanup deferred");
    }
}

async fn wait_for_poll(shutdown: &mut broadcast::Receiver<()>, poll: &mut time::Interval) -> bool {
    tokio::select! {
        _ = shutdown.recv() => true,
        _ = poll.tick() => false,
    }
}

async fn run_claim(state: &AppState, claim: Claim, shutdown: &mut broadcast::Receiver<()>) -> bool {
    let cancellation = CancellationToken::new();
    let work = Box::pin(build_validated_artifact(state, &claim, cancellation.clone()));
    tokio::pin!(work);
    let mut heartbeat = time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let deadline = time::sleep(MAX_ATTEMPT_DURATION);
    tokio::pin!(deadline);
    let mut stopping = false;
    let mut failure_code = None;

    let result = loop {
        tokio::select! {
            result = &mut work => break result,
            _ = shutdown.recv(), if !stopping => {
                stopping = true;
                failure_code = Some("worker_shutdown");
                cancellation.cancel();
            }
            _ = &mut deadline, if failure_code.is_none() => {
                failure_code = Some("attempt_timeout");
                cancellation.cancel();
            }
            _ = heartbeat.tick() => {
                if let Some(code) = heartbeat_failure(state, &claim).await {
                    failure_code.get_or_insert(code);
                    cancellation.cancel();
                }
            }
        }
    };

    // The work future is drained even on shutdown or ownership loss. In
    // particular, its blocking renderer may not outlive lease finalization.
    finish_claim(state, &claim, result, failure_code).await;
    stopping
}

async fn finish_claim(
    state: &AppState,
    claim: &Claim,
    result: anyhow::Result<artifact::ArtifactManifest>,
    failure_code: Option<&str>,
) {
    let finish = match (result, failure_code) {
        (Ok(manifest), None) => finish_success(state, claim, manifest).await,
        (result, code) => {
            log_attempt_failure(claim, result);
            preparation::finish_error(state, claim, code.unwrap_or("preparation_failed")).await
        }
    };
    if let Err(error) = finish {
        // Leave durable ownership for expiry recovery rather than pretending a
        // failed database finalization released the attempt.
        tracing::warn!(%error, "graph preparation finalization deferred to lease recovery");
    }
}

fn log_attempt_failure(claim: &Claim, result: anyhow::Result<artifact::ArtifactManifest>) {
    if let Err(error) = result {
        tracing::warn!(preparation_id = %claim.record.id, %error, "graph preparation attempt failed");
    }
}

async fn finish_success(
    state: &AppState,
    claim: &Claim,
    manifest: artifact::ArtifactManifest,
) -> Result<(), preparation::PreparationError> {
    match preparation::finish_success(state, claim, manifest).await {
        Ok(true) => Ok(()),
        Ok(false) => preparation::finish_error(state, claim, "superseded").await,
        Err(error) => Err(error),
    }
}

async fn heartbeat_failure(state: &AppState, claim: &Claim) -> Option<&'static str> {
    match preparation::heartbeat(state, claim).await {
        Ok(true) => source_fence_failure(state, claim).await,
        Ok(false) => Some("lease_lost"),
        Err(error) => {
            tracing::warn!(%error, "graph preparation heartbeat failed");
            Some("heartbeat_unavailable")
        }
    }
}

async fn source_fence_failure(state: &AppState, claim: &Claim) -> Option<&'static str> {
    match preparation::identity_is_current(&state.persistence.postgres, &claim.record.identity)
        .await
    {
        Ok(true) => None,
        Ok(false) => Some("superseded"),
        Err(error) => {
            tracing::warn!(%error, "graph preparation source fence unavailable");
            Some("source_fence_unavailable")
        }
    }
}

async fn build_validated_artifact(
    state: &AppState,
    claim: &Claim,
    cancellation: CancellationToken,
) -> anyhow::Result<artifact::ArtifactManifest> {
    let identity = &claim.record.identity;
    if !preparation::identity_is_current(&state.persistence.postgres, identity).await? {
        bail!("graph preparation identity was superseded before validation");
    }
    let current = if identity.topology_generation == 0 {
        sqlx::query_scalar::<_, bool>(
            "select not exists (select 1 from runtime_graph_node where library_id = $1)
                and not exists (select 1 from runtime_graph_edge where library_id = $1)
                and not exists (select 1 from runtime_graph_evidence where library_id = $1)",
        )
        .bind(identity.library_id)
        .fetch_one(&state.persistence.postgres)
        .await?
    } else {
        repositories::content_fingerprint_repository::graph_projection_is_current(
            &state.persistence.postgres,
            repositories::RuntimeGraphProjectionIdentity {
                library_id: identity.library_id,
                projection_version: identity.projection_version,
                topology_generation: identity.topology_generation,
            },
        )
        .await
        .context("validate graph preparation owning sources")?
    };
    if !current
        || !preparation::identity_is_current(&state.persistence.postgres, identity).await?
        || cancellation.is_cancelled()
    {
        bail!("graph preparation has no current validated projection");
    }
    if !preparation::set_phase(state, claim, PreparationStatus::Building).await? {
        bail!("graph preparation lost its attempt before building");
    }
    Box::pin(artifact::build_artifact(state, claim, cancellation)).await
}
