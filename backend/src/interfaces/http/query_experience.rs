use axum::{
    Json, Router,
    extract::{Path, State},
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ui_graph::GraphAssistantModeDescriptorModel,
    interfaces::http::{
        router_support::ApiError,
        ui_support::{UiSessionContext, load_active_ui_context},
    },
};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/ui/libraries/{library_id}/graph/assistant/config",
        axum::routing::get(get_graph_assistant_config),
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GraphAssistantConfigResponse {
    scope_hint_key: String,
    default_prompt_keys: Vec<String>,
    modes: Vec<GraphAssistantModeDescriptorModel>,
}

async fn get_graph_assistant_config(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<GraphAssistantConfigResponse>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    if active.project.id != library_id {
        return Err(ApiError::Conflict(format!(
            "requested library {library_id} does not match active library {}",
            active.project.id
        )));
    }
    let config = state.retrieval_intelligence_services.query_intelligence.assistant_config();
    Ok(Json(GraphAssistantConfigResponse {
        scope_hint_key: config.scope_hint_key,
        default_prompt_keys: config.default_prompt_keys,
        modes: config
            .modes
            .into_iter()
            .map(|mode| GraphAssistantModeDescriptorModel {
                mode: mode.mode.as_str().to_string(),
                label_key: mode.label_key,
                short_description_key: mode.short_description_key,
                best_for_key: mode.best_for_key,
                caution_key: mode.caution_key,
                example_question_key: mode.example_question_key,
            })
            .collect(),
    }))
}
