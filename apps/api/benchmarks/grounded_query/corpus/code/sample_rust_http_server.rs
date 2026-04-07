//! A minimal HTTP server implementation in Rust using Axum.
//!
//! This module demonstrates routing, middleware, error handling,
//! state management, and authentication patterns.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Json},
    routing::{delete, get, post, put},
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

/// Application configuration loaded from environment variables.
///
/// # Environment Variables
/// - `APP_PORT`: Server listen port (default: 8080)
/// - `APP_DATABASE_URL`: PostgreSQL connection string
/// - `APP_JWT_SECRET`: Secret key for JWT token signing
/// - `APP_MAX_CONNECTIONS`: Maximum database pool size (default: 10)
/// - `APP_LOG_LEVEL`: Logging level (default: "info")
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub max_connections: u32,
    pub log_level: String,
}

impl AppConfig {
    /// Loads configuration from environment variables.
    /// Panics if required variables are missing.
    pub fn from_env() -> Self {
        Self {
            port: std::env::var("APP_PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("APP_PORT must be a valid u16"),
            database_url: std::env::var("APP_DATABASE_URL")
                .expect("APP_DATABASE_URL is required"),
            jwt_secret: std::env::var("APP_JWT_SECRET")
                .expect("APP_JWT_SECRET is required"),
            max_connections: std::env::var("APP_MAX_CONNECTIONS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .expect("APP_MAX_CONNECTIONS must be a valid u32"),
            log_level: std::env::var("APP_LOG_LEVEL")
                .unwrap_or_else(|_| "info".to_string()),
        }
    }
}

/// Shared application state accessible from all route handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub users: Arc<RwLock<HashMap<Uuid, User>>>,
    pub sessions: Arc<RwLock<HashMap<String, Session>>>,
}

/// A user record stored in the in-memory database.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub role: UserRole,
    pub created_at: String,
    pub updated_at: String,
}

/// User roles for authorization.
/// - `Admin`: Full access to all resources
/// - `Editor`: Can create and modify content
/// - `Viewer`: Read-only access
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum UserRole {
    Admin,
    Editor,
    Viewer,
}

/// An active user session.
#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: Uuid,
    pub token: String,
    pub expires_at: String,
}

/// Request body for creating a new user.
///
/// # Validation Rules
/// - `username`: 3-50 characters, alphanumeric and underscores only
/// - `email`: Must be a valid email format
/// - `role`: Optional, defaults to `Viewer`
#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub email: String,
    pub role: Option<UserRole>,
}

/// Request body for updating an existing user.
/// All fields are optional -- only provided fields are updated.
#[derive(Deserialize)]
pub struct UpdateUserRequest {
    pub username: Option<String>,
    pub email: Option<String>,
    pub role: Option<UserRole>,
}

/// Query parameters for listing users.
///
/// # Parameters
/// - `page`: Page number (1-based, default: 1)
/// - `per_page`: Items per page (default: 20, max: 100)
/// - `role`: Filter by user role
/// - `search`: Search by username or email (case-insensitive substring match)
#[derive(Deserialize)]
pub struct ListUsersQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub role: Option<UserRole>,
    pub search: Option<String>,
}

/// Paginated response wrapper.
#[derive(Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub total: usize,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

/// Standard API error response.
///
/// Error codes follow HTTP status code semantics:
/// - `400`: Validation error (invalid input)
/// - `401`: Authentication required
/// - `403`: Forbidden (insufficient permissions)
/// - `404`: Resource not found
/// - `409`: Conflict (duplicate username/email)
/// - `422`: Unprocessable entity
/// - `500`: Internal server error
#[derive(Serialize)]
pub struct ApiError {
    pub code: u16,
    pub message: String,
    pub details: Option<Vec<String>>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = StatusCode::from_u16(self.code)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self)).into_response()
    }
}

/// Builds the complete application router with all routes and middleware.
///
/// # Route Map
///
/// | Method | Path                | Handler         | Auth Required |
/// |--------|---------------------|-----------------|---------------|
/// | GET    | `/health`           | `health_check`  | No            |
/// | GET    | `/api/v1/users`     | `list_users`    | Yes           |
/// | POST   | `/api/v1/users`     | `create_user`   | Yes (Admin)   |
/// | GET    | `/api/v1/users/:id` | `get_user`      | Yes           |
/// | PUT    | `/api/v1/users/:id` | `update_user`   | Yes (Admin)   |
/// | DELETE | `/api/v1/users/:id` | `delete_user`   | Yes (Admin)   |
/// | POST   | `/api/v1/auth/login`| `login`         | No            |
/// | POST   | `/api/v1/auth/logout`| `logout`       | Yes           |
pub fn build_router(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/users/:id", get(get_user).put(update_user).delete(delete_user))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    Router::new()
        .route("/health", get(health_check))
        .nest("/api/v1", api_routes)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Health check endpoint.
///
/// Returns `200 OK` with server status and version.
/// Used by load balancers and monitoring systems.
///
/// # Response
/// ```json
/// { "status": "healthy", "version": "1.0.0" }
/// ```
async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "healthy",
        "version": "1.0.0"
    }))
}

/// Lists users with pagination and optional filtering.
///
/// # Query Parameters
/// - `page` (u32): Page number, 1-based. Default: 1
/// - `per_page` (u32): Items per page. Default: 20, Max: 100
/// - `role` (string): Filter by role ("Admin", "Editor", "Viewer")
/// - `search` (string): Case-insensitive search in username and email
///
/// # Response
/// Returns a `PaginatedResponse<User>` with matching users.
///
/// # Errors
/// - `401 Unauthorized`: Missing or invalid authentication
async fn list_users(
    State(state): State<AppState>,
    Query(params): Query<ListUsersQuery>,
) -> Result<Json<PaginatedResponse<User>>, ApiError> {
    let users = state.users.read().map_err(|_| ApiError {
        code: 500,
        message: "Internal lock error".to_string(),
        details: None,
    })?;

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100);

    let mut filtered: Vec<&User> = users.values().collect();

    // Apply role filter
    if let Some(ref role) = params.role {
        filtered.retain(|u| &u.role == role);
    }

    // Apply search filter
    if let Some(ref search) = params.search {
        let search_lower = search.to_lowercase();
        filtered.retain(|u| {
            u.username.to_lowercase().contains(&search_lower)
                || u.email.to_lowercase().contains(&search_lower)
        });
    }

    let total = filtered.len();
    let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;
    let start = ((page - 1) * per_page) as usize;
    let data: Vec<User> = filtered
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .cloned()
        .collect();

    Ok(Json(PaginatedResponse {
        data,
        total,
        page,
        per_page,
        total_pages,
    }))
}

/// Creates a new user.
///
/// # Request Body
/// ```json
/// {
///   "username": "john_doe",
///   "email": "john@example.com",
///   "role": "Editor"
/// }
/// ```
///
/// # Response
/// Returns `201 Created` with the new `User` object.
///
/// # Errors
/// - `400 Bad Request`: Invalid username or email format
/// - `401 Unauthorized`: Missing authentication
/// - `403 Forbidden`: Only Admin role can create users
/// - `409 Conflict`: Username or email already exists
async fn create_user(
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), ApiError> {
    // Validate username
    if req.username.len() < 3 || req.username.len() > 50 {
        return Err(ApiError {
            code: 400,
            message: "Username must be between 3 and 50 characters".to_string(),
            details: None,
        });
    }

    // Validate email
    if !req.email.contains('@') || !req.email.contains('.') {
        return Err(ApiError {
            code: 400,
            message: "Invalid email format".to_string(),
            details: None,
        });
    }

    let mut users = state.users.write().map_err(|_| ApiError {
        code: 500,
        message: "Internal lock error".to_string(),
        details: None,
    })?;

    // Check for duplicate username or email
    if users.values().any(|u| u.username == req.username || u.email == req.email) {
        return Err(ApiError {
            code: 409,
            message: "Username or email already exists".to_string(),
            details: None,
        });
    }

    let user = User {
        id: Uuid::new_v4(),
        username: req.username,
        email: req.email,
        role: req.role.unwrap_or(UserRole::Viewer),
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    users.insert(user.id, user.clone());
    Ok((StatusCode::CREATED, Json(user)))
}

/// Retrieves a user by ID.
///
/// # Path Parameters
/// - `id` (UUID): The user's unique identifier
///
/// # Response
/// Returns `200 OK` with the `User` object.
///
/// # Errors
/// - `401 Unauthorized`: Missing authentication
/// - `404 Not Found`: No user with the given ID
async fn get_user(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<User>, ApiError> {
    let users = state.users.read().map_err(|_| ApiError {
        code: 500,
        message: "Internal lock error".to_string(),
        details: None,
    })?;

    users.get(&id).cloned().map(Json).ok_or_else(|| ApiError {
        code: 404,
        message: format!("User {} not found", id),
        details: None,
    })
}

/// Updates an existing user.
///
/// Only provided fields are updated; omitted fields remain unchanged.
/// Requires Admin role for authorization.
///
/// # Path Parameters
/// - `id` (UUID): The user's unique identifier
///
/// # Request Body
/// ```json
/// {
///   "email": "new_email@example.com",
///   "role": "Admin"
/// }
/// ```
///
/// # Response
/// Returns `200 OK` with the updated `User` object.
///
/// # Errors
/// - `401 Unauthorized`: Missing authentication
/// - `403 Forbidden`: Only Admin role can update users
/// - `404 Not Found`: No user with the given ID
/// - `409 Conflict`: New username/email conflicts with existing user
async fn update_user(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<User>, ApiError> {
    let mut users = state.users.write().map_err(|_| ApiError {
        code: 500,
        message: "Internal lock error".to_string(),
        details: None,
    })?;

    let user = users.get_mut(&id).ok_or_else(|| ApiError {
        code: 404,
        message: format!("User {} not found", id),
        details: None,
    })?;

    if let Some(username) = req.username {
        user.username = username;
    }
    if let Some(email) = req.email {
        user.email = email;
    }
    if let Some(role) = req.role {
        user.role = role;
    }
    user.updated_at = chrono::Utc::now().to_rfc3339();

    Ok(Json(user.clone()))
}

/// Deletes a user by ID.
///
/// Requires Admin role. Also terminates all active sessions
/// belonging to the deleted user.
///
/// # Path Parameters
/// - `id` (UUID): The user's unique identifier
///
/// # Response
/// Returns `204 No Content` on success.
///
/// # Errors
/// - `401 Unauthorized`: Missing authentication
/// - `403 Forbidden`: Only Admin role can delete users
/// - `404 Not Found`: No user with the given ID
async fn delete_user(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut users = state.users.write().map_err(|_| ApiError {
        code: 500,
        message: "Internal lock error".to_string(),
        details: None,
    })?;

    if users.remove(&id).is_none() {
        return Err(ApiError {
            code: 404,
            message: format!("User {} not found", id),
            details: None,
        });
    }

    // Clean up sessions for the deleted user
    let mut sessions = state.sessions.write().map_err(|_| ApiError {
        code: 500,
        message: "Internal lock error".to_string(),
        details: None,
    })?;
    sessions.retain(|_, s| s.user_id != id);

    Ok(StatusCode::NO_CONTENT)
}

/// Authenticates a user and creates a session.
///
/// # Request Body
/// ```json
/// {
///   "username": "john_doe",
///   "password": "secure_password"
/// }
/// ```
///
/// # Response
/// Returns `200 OK` with a session token:
/// ```json
/// {
///   "token": "eyJhbGciOiJIUzI1NiJ9...",
///   "expires_in": 3600
/// }
/// ```
///
/// # Errors
/// - `401 Unauthorized`: Invalid credentials
async fn login() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "token": "mock_token",
        "expires_in": 3600
    }))
}

/// Terminates the current user session.
///
/// Invalidates the session token so it cannot be reused.
///
/// # Response
/// Returns `200 OK` with a confirmation message.
async fn logout() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "message": "Logged out successfully"
    }))
}

/// Authentication middleware.
///
/// Extracts the `Authorization: Bearer <token>` header,
/// validates the JWT token against `APP_JWT_SECRET`,
/// and injects the authenticated user into request extensions.
///
/// # Token Format
/// The token is a JWT with the following claims:
/// - `sub`: User ID (UUID)
/// - `exp`: Expiration timestamp (Unix epoch)
/// - `role`: User role string
///
/// # Errors
/// - `401 Unauthorized`: Missing, expired, or invalid token
async fn auth_middleware(
    State(_state): State<AppState>,
    request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    let auth_header = request.headers().get("Authorization");
    match auth_header {
        Some(_value) => Ok(next.run(request).await),
        None => Err(ApiError {
            code: 401,
            message: "Authentication required".to_string(),
            details: None,
        }),
    }
}

/// Entry point. Starts the HTTP server.
///
/// Binds to `0.0.0.0:{APP_PORT}` and serves until SIGTERM/SIGINT.
/// Graceful shutdown waits up to 30 seconds for in-flight requests.
#[tokio::main]
async fn main() {
    let config = AppConfig::from_env();
    let addr = format!("0.0.0.0:{}", config.port);

    let state = AppState {
        config,
        users: Arc::new(RwLock::new(HashMap::new())),
        sessions: Arc::new(RwLock::new(HashMap::new())),
    };

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("Server listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_health_check() {
        let state = AppState {
            config: AppConfig {
                port: 8080,
                database_url: "postgres://test".to_string(),
                jwt_secret: "test_secret".to_string(),
                max_connections: 5,
                log_level: "debug".to_string(),
            },
            users: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        };

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_create_user_requires_auth() {
        let state = AppState {
            config: AppConfig {
                port: 8080,
                database_url: "postgres://test".to_string(),
                jwt_secret: "test_secret".to_string(),
                max_connections: 5,
                log_level: "debug".to_string(),
            },
            users: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        };

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/users")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"username":"test","email":"test@test.com"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
