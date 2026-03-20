pub mod ai;
pub mod audit;
pub mod auth;
pub mod authorization;
pub mod billing;
pub mod catalog;
pub mod chunks;
pub mod content;
pub mod content_support;
pub mod graph_search;
pub mod health;
pub mod iam;
pub mod ingestion;
pub mod mcp;
mod mcp_memory;
pub mod openapi;
pub mod ops;
pub mod query;
pub mod router_support;
pub mod runtime_support;
pub mod ui_auth;
pub mod ui_shell;
pub mod ui_support;

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
        .merge(chunks::router())
        .merge(graph_search::router())
        .merge(query::router())
        .merge(billing::router())
        .merge(ops::router())
        .merge(audit::router())
        .merge(mcp::router())
        .merge(ui_auth::router())
        .merge(ui_shell::router())
}
