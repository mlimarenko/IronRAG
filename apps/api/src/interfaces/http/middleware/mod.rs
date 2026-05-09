//! Canonical HTTP middleware stack.
//!
//! The request path is ordered outer-most first:
//! tracing -> CORS -> compression -> auth -> handler.
//!
//! Prometheus metrics, request body limits, and request-id propagation are part of this
//! same canonical stack; they are kept here so router construction has one middleware path.
//! The concrete stack places body-limit and Prometheus inside compression, before auth.

pub mod auth;
pub mod compression;
pub mod cors;
pub mod prometheus;
pub mod tracing_layer;

use axum::{Router, extract::DefaultBodyLimit};

use crate::app::state::AppState;

pub fn apply_canonical_middleware(router: Router<AppState>, state: AppState) -> Router {
    let public_origin_settings = state.settings.public_origin_settings();
    let max_request_body_bytes = state.mcp_memory.max_request_body_bytes();

    let router = auth::apply(router, state.clone());
    let router = prometheus::apply(router);
    let router = router.layer(DefaultBodyLimit::max(max_request_body_bytes));
    let router = compression::apply(router);
    let router = cors::apply(router, &public_origin_settings);
    let router = tracing_layer::apply(router);

    router.with_state(state)
}
