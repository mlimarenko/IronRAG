pub mod auth;
pub mod authorization;
pub mod chunks;
pub mod content;
pub mod content_support;
pub mod documents;
pub mod graph;
pub mod health;
pub mod ingestion;
pub mod integrations;
pub mod projects;
pub mod providers;
pub mod retrieval;
pub mod router_support;
pub mod uploads;
pub mod usage;
pub mod workspaces;

use axum::{Router, routing::get};

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::readiness))
        .route("/version", get(health::version))
        .merge(auth::router())
        .merge(workspaces::router())
        .merge(projects::router())
        .merge(providers::router())
        .merge(integrations::router())
        .merge(ingestion::router())
        .merge(documents::router())
        .merge(content::router())
        .merge(chunks::router())
        .merge(uploads::router())
        .merge(retrieval::router())
        .merge(graph::router())
        .merge(usage::router())
}
