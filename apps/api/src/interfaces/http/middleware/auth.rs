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

    /// The natively authenticated financial caller of this request.
    ///
    /// A cookie session names its own `iam_session` row; everything else — a
    /// direct API token, an anonymous or failed resolution — names nothing, so a
    /// fenced grant refuses it instead of treating a shared credential as
    /// custody of a physical client.
    fn dispatch_caller(&self) -> crate::shared::provider_http::DispatchCaller {
        match &self.state {
            RequestAuthState::Authenticated(auth) => dispatch_caller_for(auth),
            RequestAuthState::Anonymous | RequestAuthState::Invalid(_) => {
                crate::shared::provider_http::DispatchCaller::Unattributed
            }
        }
    }
}

/// The financial caller one already-resolved authentication proves.
///
/// This is the single owner of the rule: a cookie session names its own
/// `iam_session` row; a direct API token, a failed resolution or an anonymous
/// request names nothing and is refused under a fenced grant. Callers that
/// detach work into a spawned task re-scope with this value instead of
/// rebuilding the decision from headers or policy.
pub(crate) fn dispatch_caller_for(
    auth: &AuthContext,
) -> crate::shared::provider_http::DispatchCaller {
    use crate::shared::provider_http::DispatchCaller;
    if auth.token_kind.is_session() {
        DispatchCaller::IamSession(auth.token_id)
    } else {
        DispatchCaller::Unattributed
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
    let caller = request_auth.dispatch_caller();
    request.extensions_mut().insert(request_auth);
    // The financial caller fence reads this scope at each provider attestation.
    // Only a browser session carries a per-login identity here; a direct API
    // token is deliberately left unattributed, and an MCP session re-scopes the
    // value to its own validated session digest inside the MCP transport.
    crate::shared::provider_http::with_dispatch_caller(caller, next.run(request)).await
}
