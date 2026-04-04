#[allow(clippy::uninlined_format_args)]
pub mod ai;
pub mod audit;
#[allow(
    clippy::items_after_test_module,
    clippy::map_unwrap_or,
    clippy::must_use_candidate,
    clippy::option_if_let_else,
    clippy::result_large_err
)]
pub mod auth;
#[allow(clippy::missing_errors_doc, clippy::result_large_err)]
pub mod authorization;
pub mod billing;
pub mod catalog;
#[allow(
    clippy::large_futures,
    clippy::map_unwrap_or,
    clippy::needless_pass_by_value,
    clippy::result_large_err
)]
pub mod content;
mod health;
#[allow(
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::result_large_err,
    clippy::unnecessary_wraps
)]
pub mod iam;
pub mod ingestion;
#[allow(
    clippy::cast_precision_loss,
    clippy::map_unwrap_or,
    clippy::needless_pass_by_value,
    clippy::redundant_clone,
    clippy::result_large_err,
    clippy::struct_field_names,
    clippy::too_many_lines
)]
pub mod knowledge;
#[allow(
    clippy::large_futures,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::result_large_err,
    clippy::too_many_lines
)]
pub mod mcp;
mod openapi;
pub mod ops;
#[allow(clippy::large_enum_variant, clippy::needless_pass_by_value, clippy::redundant_clone)]
pub mod query;
#[allow(
    clippy::map_unwrap_or,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::result_large_err
)]
pub mod router_support;
pub mod runtime;
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
        .merge(runtime::router())
        .merge(billing::router())
        .merge(ops::router())
        .merge(audit::router())
        .merge(mcp::router())
}
