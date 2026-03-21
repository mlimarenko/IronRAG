pub mod ai;
pub mod audit;
pub mod auth;
pub mod authorization;
pub mod billing;
pub mod catalog;
pub mod content;
mod health;
pub mod iam;
pub mod ingestion;
pub mod knowledge;
pub mod mcp;
mod openapi;
pub mod ops;
pub mod query;
pub mod router_support;
mod ui_support;

use axum::{Router, routing::get};

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::readiness))
        .route("/version", get(health::version))
        .merge(openapi::router())
        .merge(iam::router())
        .merge(catalog::router())
        .merge(ai::router())
        .merge(ingestion::router())
        .merge(content::router())
        .merge(knowledge::router())
        .merge(query::router())
        .merge(billing::router())
        .merge(ops::router())
        .merge(audit::router())
        .merge(mcp::router())
}
