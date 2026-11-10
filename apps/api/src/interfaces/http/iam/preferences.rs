use axum::{Json, extract::State};
use tracing::error;

use crate::{
    app::state::AppState,
    infra::repositories::iam_repository::{self, IamUserPreferenceRow},
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

use super::types::{
    IamPreferencesResponse, IamTelemetryConsent, SeedIamPreferencesRequest,
    UpdateIamPreferencesRequest,
};

fn map_preference(row: Option<IamUserPreferenceRow>) -> IamPreferencesResponse {
    IamPreferencesResponse {
        telemetry_consent: row
            .and_then(|row| IamTelemetryConsent::parse_wire(&row.telemetry_consent)),
    }
}

fn preference_read_error(principal_id: uuid::Uuid, error: sqlx::Error) -> ApiError {
    error!(
        auth_principal_id = %principal_id,
        ?error,
        "failed to access account telemetry preference",
    );
    ApiError::Internal
}

fn require_expected_principal(expected: uuid::Uuid, actual: uuid::Uuid) -> Result<(), ApiError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ApiError::Conflict("account session changed; reload preference".into()))
    }
}

#[utoipa::path(
    get,
    path = "/v1/iam/preferences",
    tag = "iam",
    operation_id = "getIamPreferences",
    responses(
        (status = 200, description = "Current account preference; null telemetryConsent means unset", body = IamPreferencesResponse),
        (status = 401, description = "Caller has no browser session"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_iam_preferences", skip_all)]
pub(crate) async fn get_iam_preferences(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<IamPreferencesResponse>, ApiError> {
    auth.require_session_token()?;

    let row = iam_repository::get_user_preference(&state.persistence.postgres, auth.principal_id)
        .await
        .map_err(|error| preference_read_error(auth.principal_id, error))?;

    Ok(Json(map_preference(row)))
}

#[utoipa::path(
    put,
    path = "/v1/iam/preferences",
    tag = "iam",
    operation_id = "updateIamPreferences",
    request_body = UpdateIamPreferencesRequest,
    responses(
        (status = 200, description = "Stored account preference", body = IamPreferencesResponse),
        (status = 401, description = "Caller has no browser session"),
        (status = 409, description = "The authenticated account does not match the expected principal"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.update_iam_preferences", skip_all)]
pub(crate) async fn update_iam_preferences(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<UpdateIamPreferencesRequest>,
) -> Result<Json<IamPreferencesResponse>, ApiError> {
    auth.require_session_token()?;

    require_expected_principal(request.expected_principal_id, auth.principal_id)?;
    let row = iam_repository::upsert_user_preference(
        &state.persistence.postgres,
        auth.principal_id,
        request.telemetry_consent.as_str(),
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            ?error,
            "failed to store account telemetry preference",
        );
        ApiError::Internal
    })?;

    Ok(Json(map_preference(Some(row))))
}

#[utoipa::path(
    post,
    path = "/v1/iam/preferences/seed",
    tag = "iam",
    operation_id = "seedIamPreferences",
    request_body = SeedIamPreferencesRequest,
    responses(
        (status = 200, description = "Durable account preference after seeding; an existing row is never overwritten", body = IamPreferencesResponse),
        (status = 401, description = "Caller has no browser session"),
        (status = 409, description = "The authenticated account does not match the expected principal"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.seed_iam_preferences", skip_all)]
pub(crate) async fn seed_iam_preferences(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<SeedIamPreferencesRequest>,
) -> Result<Json<IamPreferencesResponse>, ApiError> {
    auth.require_session_token()?;

    require_expected_principal(request.expected_principal_id, auth.principal_id)?;
    let row = iam_repository::seed_user_preference_if_absent(
        &state.persistence.postgres,
        auth.principal_id,
        request.telemetry_consent.as_str(),
    )
    .await
    .map_err(|error| preference_read_error(auth.principal_id, error))?;

    Ok(Json(map_preference(row)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_principal_is_a_precondition_not_a_target() {
        let principal = uuid::Uuid::now_v7();
        assert!(require_expected_principal(principal, principal).is_ok());
        assert!(matches!(
            require_expected_principal(uuid::Uuid::now_v7(), principal),
            Err(ApiError::Conflict(_))
        ));
    }
}
