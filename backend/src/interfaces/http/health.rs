use axum::{Json, extract::State};
use serde::Serialize;
use sqlx::Row;

use crate::app::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: String,
    pub environment: String,
}

#[derive(Serialize)]
pub struct ReadinessResponse {
    pub status: &'static str,
    pub postgres: &'static str,
    pub redis: &'static str,
    pub arangodb: &'static str,
}

#[derive(Serialize)]
pub struct VersionResponse {
    pub service: String,
    pub version: String,
    pub environment: String,
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: state.settings.service_name,
        environment: state.settings.environment,
    })
}

pub async fn readiness(State(state): State<AppState>) -> Json<ReadinessResponse> {
    let postgres_ok = sqlx::query("select 1")
        .fetch_one(&state.persistence.postgres)
        .await
        .map(|row| row.get::<i32, _>(0) == 1)
        .unwrap_or(false);

    let redis_ok = match state.persistence.redis.get_multiplexed_tokio_connection().await {
        Ok(mut conn) => redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map(|v| v == "PONG")
            .unwrap_or(false),
        Err(_) => false,
    };
    let arangodb_ok = state.arango_client.ping().await.is_ok();

    Json(ReadinessResponse {
        status: if postgres_ok && redis_ok && arangodb_ok { "ready" } else { "degraded" },
        postgres: if postgres_ok { "ok" } else { "down" },
        redis: if redis_ok { "ok" } else { "down" },
        arangodb: if arangodb_ok { "ok" } else { "down" },
    })
}

pub async fn version(State(state): State<AppState>) -> Json<VersionResponse> {
    Json(VersionResponse {
        service: state.settings.service_name,
        version: env!("CARGO_PKG_VERSION").to_string(),
        environment: state.settings.environment,
    })
}
