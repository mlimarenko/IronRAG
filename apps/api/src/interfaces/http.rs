pub mod admin;
pub mod ai;
pub mod audit;
pub mod auth;
pub mod authorization;
pub mod billing;
pub mod catalog;
pub mod content;
pub mod health;
pub mod iam;
pub mod ingestion;
pub mod knowledge;
pub mod mcp;
pub mod middleware;
pub mod openapi;
pub mod ops;
pub mod query;
pub mod query_session_mutations;
pub mod router_support;
pub mod runtime;
mod ui_support;
pub mod webhook;

use axum::{Router, routing::get};

pub fn probe_router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::readiness))
        .route("/version", get(health::version))
}

pub fn router() -> Router<crate::app::state::AppState> {
    probe_router()
        .route("/version/update", get(health::release_update))
        .merge(openapi::router())
        .merge(iam::router())
        .merge(catalog::router())
        .merge(admin::router())
        .merge(ai::router())
        .merge(ingestion::router())
        .merge(content::router())
        .merge(knowledge::router())
        .merge(query::router())
        .merge(query_session_mutations::router())
        .merge(runtime::router())
        .merge(billing::router())
        .merge(ops::router())
        .merge(audit::router())
        .merge(mcp::router())
        .merge(webhook::router())
}
