use axum::{
    Router,
    body::Body,
    extract::State,
    http::Request,
    middleware::{self, Next},
    response::Response,
};

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::{self as http_auth, AuthContext},
        router_support::ApiError,
    },
};

#[derive(Clone, Debug)]
pub struct RequestAuth {
    state: RequestAuthState,
}

impl RequestAuth {
    const fn anonymous() -> Self {
        Self { state: RequestAuthState::Anonymous }
    }

    const fn authenticated(auth: AuthContext) -> Self {
        Self { state: RequestAuthState::Authenticated(auth) }
    }

    const fn invalid(error: AuthResolutionError) -> Self {
        Self { state: RequestAuthState::Invalid(error) }
    }

    pub fn required_context(&self) -> Result<AuthContext, ApiError> {
        match &self.state {
            RequestAuthState::Anonymous => Err(ApiError::Unauthorized),
            RequestAuthState::Authenticated(auth) => Ok(auth.clone()),
            RequestAuthState::Invalid(error) => Err(error.to_api_error()),
        }
    }
}

#[derive(Clone, Debug)]
enum RequestAuthState {
    Anonymous,
    Authenticated(AuthContext),
    Invalid(AuthResolutionError),
}

#[derive(Clone, Copy, Debug)]
enum AuthResolutionError {
    Unauthorized,
    Internal,
}

impl AuthResolutionError {
    const fn to_api_error(self) -> ApiError {
        match self {
            Self::Unauthorized => ApiError::Unauthorized,
            Self::Internal => ApiError::Internal,
        }
    }
}

impl From<ApiError> for AuthResolutionError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Unauthorized => Self::Unauthorized,
            _ => Self::Internal,
        }
    }
}

pub fn apply(router: Router<AppState>, state: AppState) -> Router<AppState> {
    router.layer(middleware::from_fn_with_state(state, attach_request_auth))
}

async fn attach_request_auth(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let request_auth = match http_auth::resolve_optional_auth_context_from_headers(
        &state,
        request.headers(),
    )
    .await
    {
        Ok(Some(auth)) => RequestAuth::authenticated(auth),
        Ok(None) => RequestAuth::anonymous(),
        Err(error) => RequestAuth::invalid(AuthResolutionError::from(error)),
    };
    request.extensions_mut().insert(request_auth);
    next.run(request).await
}
