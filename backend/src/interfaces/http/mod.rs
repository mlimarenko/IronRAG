pub mod auth;
pub mod authorization;
pub mod chat;
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
pub mod query_experience;
pub mod retrieval;
pub mod router_support;
pub mod runtime_documents;
pub mod runtime_graph;
pub mod runtime_pricing;
pub mod runtime_providers;
pub mod runtime_query;
pub mod runtime_support;
pub mod ui_admin;
pub mod ui_auth;
pub mod ui_documents;
pub mod ui_graph;
pub mod ui_shell;
pub mod ui_support;
pub mod upload_support;
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
        .merge(chat::router())
        .merge(chunks::router())
        .merge(runtime_documents::router())
        .merge(runtime_graph::router())
        .merge(runtime_pricing::router())
        .merge(runtime_query::router())
        .merge(runtime_providers::router())
        .merge(query_experience::router())
        .merge(ui_auth::router())
        .merge(ui_shell::router())
        .merge(ui_documents::router())
        .merge(ui_graph::router())
        .merge(ui_admin::router())
        .merge(uploads::router())
        .merge(retrieval::router())
        .merge(graph::router())
        .merge(usage::router())
}
