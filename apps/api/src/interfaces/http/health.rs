use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::{
    app::state::AppState,
    services::{
        ops::deployment_diagnostics::DeploymentReadinessSnapshot,
        ops::release_monitor::ReleaseUpdateSnapshot,
    },
};

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: String,
    pub environment: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct VersionResponse {
    pub service: String,
    pub version: String,
    pub environment: String,
    pub role: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseUpdateResponse {
    pub status: &'static str,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub repository_url: String,
    pub checked_at: String,
}

fn current_release_version() -> String {
    option_env!("APP_VERSION")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .trim()
        .trim_start_matches('v')
        .to_string()
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: state.settings.service_name,
        environment: state.settings.environment,
        role: state.settings.service_role,
    })
}

pub async fn readiness(
    State(state): State<AppState>,
) -> (StatusCode, Json<DeploymentReadinessSnapshot>) {
    let (ready, snapshot) = state.deployment_diagnostics.readiness_snapshot(&state).await;
    if ready {
        (StatusCode::OK, Json(snapshot))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(snapshot))
    }
}

pub async fn version(State(state): State<AppState>) -> Json<VersionResponse> {
    Json(VersionResponse {
        service: state.settings.service_name,
        version: current_release_version(),
        environment: state.settings.environment,
        role: state.settings.service_role,
    })
}

impl From<ReleaseUpdateSnapshot> for ReleaseUpdateResponse {
    fn from(snapshot: ReleaseUpdateSnapshot) -> Self {
        Self {
            status: snapshot.status.as_str(),
            current_version: snapshot.current_version,
            latest_version: snapshot.latest_version,
            release_url: snapshot.release_url,
            repository_url: snapshot.repository_url,
            checked_at: snapshot.checked_at.to_rfc3339(),
        }
    }
}

pub async fn release_update(State(state): State<AppState>) -> Json<ReleaseUpdateResponse> {
    Json(state.release_monitor.get_release_update(&current_release_version()).await.into())
}
