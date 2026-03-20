use axum::{
    Router,
    routing::{get, post},
};

use crate::app::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mcp", post(super::mcp_memory::handle_mcp))
        .route("/mcp/capabilities", get(super::mcp_memory::get_capabilities))
}
