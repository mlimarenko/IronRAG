use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::ProjectRow,
    interfaces::http::{
        auth::AuthContext, authorization::load_project_and_authorize, router_support::ApiError,
    },
};

#[must_use]
pub fn normalize_runtime_library_label(project: &ProjectRow) -> String {
    project.name.clone()
}

pub async fn load_library_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    accepted_scopes: &[&str],
) -> Result<ProjectRow, ApiError> {
    load_project_and_authorize(auth, state, library_id, accepted_scopes).await
}
