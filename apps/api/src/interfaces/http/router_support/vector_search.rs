use uuid::Uuid;

use super::{ApiError, map_runtime_lifecycle_error};
use crate::{
    app::state::AppState,
    services::query::{
        search::VectorPlaneReadGuard,
        vector_dimensions::{
            EmbeddingProfileInventoryVersion, ensure_active_embedding_profile_key,
            ensure_embedding_profile_inventory_version_current,
        },
    },
};

/// Keep the vector plane stable while checking the profile selected before embedding.
/// The caller holds the returned guard through its ANN reads.
pub(crate) async fn current_embedding_search_guard(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    inventory_version: EmbeddingProfileInventoryVersion,
) -> Result<VectorPlaneReadGuard, ApiError> {
    let guard = state
        .application_services
        .search
        .vector_plane_read_guard(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    ensure_active_embedding_profile_key(state, library_id, embedding_profile_key)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    ensure_embedding_profile_inventory_version_current(state, library_id, inventory_version)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    Ok(guard)
}
