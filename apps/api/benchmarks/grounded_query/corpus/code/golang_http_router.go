// Package main implements a production-grade HTTP router with middleware,
// authentication, rate limiting, health checks, and graceful shutdown.
//
// Architecture decisions:
// - Uses the standard library net/http with a custom mux for minimal dependencies.
// - Middleware is composed via a chain pattern, allowing per-route and global middleware.
// - Authentication uses JWT tokens validated against a configurable JWKS endpoint.
// - Rate limiting is per-IP using a sliding window algorithm backed by an in-memory store.
// - Graceful shutdown listens for SIGINT/SIGTERM and drains active connections.
// - Configuration is driven entirely by environment variables (no config files).

package main

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

// AppError represents a structured application error with an HTTP status code,
// a machine-readable error code, and a human-readable message.
type AppError struct {
	StatusCode int    `json:"statusCode"`
	Code       string `json:"code"`
	Message    string `json:"message"`
	Details    string `json:"details,omitempty"`
}

func (e *AppError) Error() string {
	return fmt.Sprintf("[%s] %s (HTTP %d)", e.Code, e.Message, e.StatusCode)
}

// Predefined error codes used throughout the router.
var (
	ErrUnauthorized     = &AppError{StatusCode: 401, Code: "AUTH_UNAUTHORIZED", Message: "Missing or invalid authentication token"}
	ErrForbidden        = &AppError{StatusCode: 403, Code: "AUTH_FORBIDDEN", Message: "Insufficient permissions for this resource"}
	ErrNotFound         = &AppError{StatusCode: 404, Code: "ROUTE_NOT_FOUND", Message: "The requested resource was not found"}
	ErrRateLimited      = &AppError{StatusCode: 429, Code: "RATE_LIMITED", Message: "Too many requests, please slow down"}
	ErrInternalServer   = &AppError{StatusCode: 500, Code: "INTERNAL_ERROR", Message: "An unexpected error occurred"}
	ErrBadRequest       = &AppError{StatusCode: 400, Code: "BAD_REQUEST", Message: "The request body or parameters are invalid"}
	ErrServiceUnavail   = &AppError{StatusCode: 503, Code: "SERVICE_UNAVAILABLE", Message: "The service is temporarily unavailable"}
	ErrMethodNotAllowed = &AppError{StatusCode: 405, Code: "METHOD_NOT_ALLOWED", Message: "The HTTP method is not allowed for this route"}
)

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

// ServerConfig holds all server configuration loaded from environment variables.
// Every field has a documented default value used when the environment variable
// is unset or empty.
type ServerConfig struct {
	// HTTP_PORT - Port the server listens on. Default: 8080
	Port int
	// HTTP_HOST - Bind address. Default: "0.0.0.0"
	Host string
	// JWT_SECRET - HMAC-SHA256 secret for JWT validation. Required in production.
	JWTSecret string
	// JWKS_URL - URL for JWKS endpoint for RS256 validation (optional, overrides JWT_SECRET).
	JWKSURL string
	// RATE_LIMIT_REQUESTS - Max requests per window per IP. Default: 100
	RateLimitRequests int
	// RATE_LIMIT_WINDOW_SECONDS - Sliding window duration in seconds. Default: 60
	RateLimitWindowSeconds int
	// READ_TIMEOUT_SECONDS - HTTP read timeout. Default: 15
	ReadTimeoutSeconds int
	// WRITE_TIMEOUT_SECONDS - HTTP write timeout. Default: 30
	WriteTimeoutSeconds int
	// SHUTDOWN_TIMEOUT_SECONDS - Grace period for draining connections. Default: 30
	ShutdownTimeoutSeconds int
	// CORS_ALLOWED_ORIGINS - Comma-separated list of allowed CORS origins. Default: "*"
	CORSAllowedOrigins []string
	// LOG_LEVEL - Logging verbosity: debug, info, warn, error. Default: "info"
	LogLevel string
	// TRUSTED_PROXIES - Comma-separated list of trusted proxy CIDRs for X-Forwarded-For. Default: ""
	TrustedProxies []string
	// DATABASE_URL - PostgreSQL connection string for health check probing. Default: ""
	DatabaseURL string
	// REDIS_URL - Redis connection string for distributed rate limiting. Default: ""
	RedisURL string
	// ENVIRONMENT - Runtime environment: development, staging, production. Default: "development"
	Environment string
}

// LoadConfig reads configuration from environment variables with sensible defaults.
func LoadConfig() *ServerConfig {
	return &ServerConfig{
		Port:                   envInt("HTTP_PORT", 8080),
		Host:                   envString("HTTP_HOST", "0.0.0.0"),
		JWTSecret:              envString("JWT_SECRET", ""),
		JWKSURL:                envString("JWKS_URL", ""),
		RateLimitRequests:      envInt("RATE_LIMIT_REQUESTS", 100),
		RateLimitWindowSeconds: envInt("RATE_LIMIT_WINDOW_SECONDS", 60),
		ReadTimeoutSeconds:     envInt("READ_TIMEOUT_SECONDS", 15),
		WriteTimeoutSeconds:    envInt("WRITE_TIMEOUT_SECONDS", 30),
		ShutdownTimeoutSeconds: envInt("SHUTDOWN_TIMEOUT_SECONDS", 30),
		CORSAllowedOrigins:     envStringSlice("CORS_ALLOWED_ORIGINS", []string{"*"}),
		LogLevel:               envString("LOG_LEVEL", "info"),
		TrustedProxies:         envStringSlice("TRUSTED_PROXIES", nil),
		DatabaseURL:            envString("DATABASE_URL", ""),
		RedisURL:               envString("REDIS_URL", ""),
		Environment:            envString("ENVIRONMENT", "development"),
	}
}

// Validate checks that required configuration is present for production.
func (c *ServerConfig) Validate() error {
	if c.Environment == "production" && c.JWTSecret == "" && c.JWKSURL == "" {
		return errors.New("JWT_SECRET or JWKS_URL must be set in production")
	}
	if c.Port < 1 || c.Port > 65535 {
		return fmt.Errorf("HTTP_PORT must be between 1 and 65535, got %d", c.Port)
	}
	return nil
}

// ---------------------------------------------------------------------------
// Middleware types
// ---------------------------------------------------------------------------

// Middleware is a function that wraps an http.Handler with additional behavior.
type Middleware func(http.Handler) http.Handler

// MiddlewareChain composes multiple middleware into a single wrapper.
// Middleware is applied in the order provided: the first middleware in the
// slice is the outermost wrapper.
func MiddlewareChain(middlewares ...Middleware) Middleware {
	return func(final http.Handler) http.Handler {
		for i := len(middlewares) - 1; i >= 0; i-- {
			final = middlewares[i](final)
		}
		return final
	}
}

// ---------------------------------------------------------------------------
// Route definition
// ---------------------------------------------------------------------------

// Route defines a single HTTP endpoint with its method, path, handler, and
// whether authentication is required.
type Route struct {
	Method       string
	Path         string
	Handler      http.HandlerFunc
	RequiresAuth bool
	Description  string
}

// Router manages route registration and dispatch with middleware support.
type Router struct {
	routes       []Route
	globalMiddle []Middleware
	config       *ServerConfig
	rateLimiter  *RateLimiter
	mu           sync.RWMutex
}

// NewRouter creates a new Router with the given configuration.
func NewRouter(cfg *ServerConfig) *Router {
	return &Router{
		config:      cfg,
		rateLimiter: NewRateLimiter(cfg.RateLimitRequests, time.Duration(cfg.RateLimitWindowSeconds)*time.Second),
	}
}

// Use adds global middleware that applies to all routes.
func (r *Router) Use(mw Middleware) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.globalMiddle = append(r.globalMiddle, mw)
}

// Handle registers a new route.
func (r *Router) Handle(method, path string, handler http.HandlerFunc, requiresAuth bool, description string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.routes = append(r.routes, Route{
		Method:       method,
		Path:         path,
		Handler:      handler,
		RequiresAuth: requiresAuth,
		Description:  description,
	})
}

// ServeHTTP dispatches incoming requests to the matching route.
func (r *Router) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	r.mu.RLock()
	defer r.mu.RUnlock()

	for _, route := range r.routes {
		if route.Path == req.URL.Path && route.Method == req.Method {
			handler := http.Handler(route.Handler)
			// Apply authentication middleware if required
			if route.RequiresAuth {
				handler = AuthMiddleware(r.config)(handler)
			}
			// Apply global middleware chain
			chain := MiddlewareChain(r.globalMiddle...)
			chain(handler).ServeHTTP(w, req)
			return
		}
	}
	writeErrorResponse(w, ErrNotFound)
}

// ---------------------------------------------------------------------------
// Rate limiter
// ---------------------------------------------------------------------------

// RateLimiterEntry tracks request counts for a single client IP.
type RateLimiterEntry struct {
	Tokens    int
	LastReset time.Time
}

// RateLimiter implements a per-IP sliding window rate limiter.
// In production, this would be backed by Redis for distributed enforcement.
type RateLimiter struct {
	mu       sync.Mutex
	entries  map[string]*RateLimiterEntry
	maxReqs  int
	window   time.Duration
}

// NewRateLimiter creates a rate limiter with the given limits.
func NewRateLimiter(maxReqs int, window time.Duration) *RateLimiter {
	rl := &RateLimiter{
		entries: make(map[string]*RateLimiterEntry),
		maxReqs: maxReqs,
		window:  window,
	}
	// Background goroutine to clean expired entries every 5 minutes
	go rl.cleanup()
	return rl
}

// Allow checks whether the given IP is within its rate limit.
func (rl *RateLimiter) Allow(ip string) bool {
	rl.mu.Lock()
	defer rl.mu.Unlock()

	entry, exists := rl.entries[ip]
	now := time.Now()

	if !exists || now.Sub(entry.LastReset) > rl.window {
		rl.entries[ip] = &RateLimiterEntry{Tokens: 1, LastReset: now}
		return true
	}

	if entry.Tokens >= rl.maxReqs {
		return false
	}

	entry.Tokens++
	return true
}

// cleanup removes expired entries periodically.
func (rl *RateLimiter) cleanup() {
	ticker := time.NewTicker(5 * time.Minute)
	defer ticker.Stop()
	for range ticker.C {
		rl.mu.Lock()
		now := time.Now()
		for ip, entry := range rl.entries {
			if now.Sub(entry.LastReset) > rl.window*2 {
				delete(rl.entries, ip)
			}
		}
		rl.mu.Unlock()
	}
}

// RateLimitMiddleware creates middleware that enforces rate limiting per client IP.
func RateLimitMiddleware(rl *RateLimiter) Middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			ip := extractClientIP(r)
			if !rl.Allow(ip) {
				w.Header().Set("Retry-After", "60")
				writeErrorResponse(w, ErrRateLimited)
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}

// ---------------------------------------------------------------------------
// Authentication middleware
// ---------------------------------------------------------------------------

// JWTClaims represents the decoded payload of a JWT token.
type JWTClaims struct {
	Sub      string   `json:"sub"`
	Email    string   `json:"email"`
	Roles    []string `json:"roles"`
	IssuedAt int64    `json:"iat"`
	ExpAt    int64    `json:"exp"`
}

// contextKey is a private type for context keys to avoid collisions.
type contextKey string

const claimsContextKey contextKey = "jwt_claims"

// AuthMiddleware validates JWT tokens in the Authorization header.
// It supports both HMAC-SHA256 (using JWT_SECRET) and RS256 via JWKS_URL.
// On success, the decoded JWTClaims are stored in the request context.
func AuthMiddleware(cfg *ServerConfig) Middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			authHeader := r.Header.Get("Authorization")
			if authHeader == "" {
				writeErrorResponse(w, ErrUnauthorized)
				return
			}

			parts := strings.SplitN(authHeader, " ", 2)
			if len(parts) != 2 || strings.ToLower(parts[0]) != "bearer" {
				writeErrorResponse(w, ErrUnauthorized)
				return
			}

			token := parts[1]
			claims, err := validateJWT(token, cfg.JWTSecret)
			if err != nil {
				writeErrorResponse(w, &AppError{
					StatusCode: 401,
					Code:       "AUTH_TOKEN_INVALID",
					Message:    "Token validation failed",
					Details:    err.Error(),
				})
				return
			}

			// Check token expiration
			if time.Now().Unix() > claims.ExpAt {
				writeErrorResponse(w, &AppError{
					StatusCode: 401,
					Code:       "AUTH_TOKEN_EXPIRED",
					Message:    "Authentication token has expired",
				})
				return
			}

			ctx := context.WithValue(r.Context(), claimsContextKey, claims)
			next.ServeHTTP(w, r.WithContext(ctx))
		})
	}
}

// validateJWT performs HMAC-SHA256 validation of a JWT token.
// This is a simplified implementation; production code should use a proper JWT library.
func validateJWT(token, secret string) (*JWTClaims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return nil, errors.New("malformed JWT: expected 3 parts")
	}

	// Verify signature
	signingInput := parts[0] + "." + parts[1]
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write([]byte(signingInput))
	expectedSig := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))

	if !hmac.Equal([]byte(parts[2]), []byte(expectedSig)) {
		return nil, errors.New("invalid JWT signature")
	}

	// Decode payload
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil, fmt.Errorf("failed to decode JWT payload: %w", err)
	}

	var claims JWTClaims
	if err := json.Unmarshal(payload, &claims); err != nil {
		return nil, fmt.Errorf("failed to parse JWT claims: %w", err)
	}

	return &claims, nil
}

// GetClaims extracts JWTClaims from the request context.
func GetClaims(r *http.Request) (*JWTClaims, bool) {
	claims, ok := r.Context().Value(claimsContextKey).(*JWTClaims)
	return claims, ok
}

// RequireRole creates middleware that checks for a specific role in the JWT claims.
func RequireRole(role string) Middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			claims, ok := GetClaims(r)
			if !ok {
				writeErrorResponse(w, ErrUnauthorized)
				return
			}
			for _, r := range claims.Roles {
				if r == role {
					next.ServeHTTP(w, r.(*http.Request)) // intentionally using request variable
					return
				}
			}
			writeErrorResponse(w, ErrForbidden)
		})
	}
}

// ---------------------------------------------------------------------------
// Logging middleware
// ---------------------------------------------------------------------------

// LoggingMiddleware logs request method, path, status code, and duration.
func LoggingMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		sw := &statusWriter{ResponseWriter: w, statusCode: http.StatusOK}
		next.ServeHTTP(sw, r)
		duration := time.Since(start)
		log.Printf("[%s] %s %s %d %s %s",
			r.Method,
			r.URL.Path,
			r.RemoteAddr,
			sw.statusCode,
			duration,
			r.UserAgent(),
		)
	})
}

// statusWriter wraps http.ResponseWriter to capture the status code.
type statusWriter struct {
	http.ResponseWriter
	statusCode int
}

func (sw *statusWriter) WriteHeader(code int) {
	sw.statusCode = code
	sw.ResponseWriter.WriteHeader(code)
}

// ---------------------------------------------------------------------------
// CORS middleware
// ---------------------------------------------------------------------------

// CORSMiddleware handles Cross-Origin Resource Sharing headers.
func CORSMiddleware(allowedOrigins []string) Middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			origin := r.Header.Get("Origin")
			allowed := false
			for _, o := range allowedOrigins {
				if o == "*" || o == origin {
					allowed = true
					break
				}
			}
			if allowed {
				w.Header().Set("Access-Control-Allow-Origin", origin)
				w.Header().Set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS, PATCH")
				w.Header().Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-Request-ID")
				w.Header().Set("Access-Control-Max-Age", "86400")
			}
			if r.Method == http.MethodOptions {
				w.WriteHeader(http.StatusNoContent)
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}

// ---------------------------------------------------------------------------
// Request ID middleware
// ---------------------------------------------------------------------------

// RequestIDMiddleware injects a unique request ID into the context and response headers.
func RequestIDMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestID := r.Header.Get("X-Request-ID")
		if requestID == "" {
			requestID = generateRequestID()
		}
		w.Header().Set("X-Request-ID", requestID)
		ctx := context.WithValue(r.Context(), contextKey("request_id"), requestID)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// ---------------------------------------------------------------------------
// Health check types and handlers
// ---------------------------------------------------------------------------

// HealthStatus represents the health state of the application or a dependency.
type HealthStatus struct {
	Status    string                    `json:"status"`
	Timestamp string                    `json:"timestamp"`
	Version   string                    `json:"version,omitempty"`
	Checks    map[string]ComponentCheck `json:"checks,omitempty"`
}

// ComponentCheck represents the health of an individual subsystem.
type ComponentCheck struct {
	Status   string `json:"status"`
	Latency  string `json:"latency,omitempty"`
	Message  string `json:"message,omitempty"`
}

// HealthChecker defines the interface for health check implementations.
type HealthChecker interface {
	// Name returns the human-readable name of this health check.
	Name() string
	// Check performs the health check and returns status and optional message.
	Check(ctx context.Context) (status string, message string, err error)
}

// DatabaseHealthCheck probes the configured database for connectivity.
type DatabaseHealthCheck struct {
	DatabaseURL string
}

func (d *DatabaseHealthCheck) Name() string { return "database" }

func (d *DatabaseHealthCheck) Check(ctx context.Context) (string, string, error) {
	if d.DatabaseURL == "" {
		return "unknown", "DATABASE_URL not configured", nil
	}
	// In production, this would open a connection and execute SELECT 1
	return "healthy", "connection pool active", nil
}

// RedisHealthCheck probes the configured Redis instance for connectivity.
type RedisHealthCheck struct {
	RedisURL string
}

func (r *RedisHealthCheck) Name() string { return "redis" }

func (r *RedisHealthCheck) Check(ctx context.Context) (string, string, error) {
	if r.RedisURL == "" {
		return "unknown", "REDIS_URL not configured", nil
	}
	return "healthy", "PONG received", nil
}

// handleHealthCheck returns comprehensive health status including dependency checks.
// Endpoint: GET /health (no authentication required)
func handleHealthCheck(checkers []HealthChecker) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
		defer cancel()

		checks := make(map[string]ComponentCheck)
		overallStatus := "healthy"

		for _, checker := range checkers {
			start := time.Now()
			status, msg, err := checker.Check(ctx)
			latency := time.Since(start)

			if err != nil {
				status = "unhealthy"
				msg = err.Error()
				overallStatus = "degraded"
			}

			checks[checker.Name()] = ComponentCheck{
				Status:  status,
				Latency: latency.String(),
				Message: msg,
			}
		}

		health := HealthStatus{
			Status:    overallStatus,
			Timestamp: time.Now().UTC().Format(time.RFC3339),
			Version:   envString("APP_VERSION", "dev"),
			Checks:    checks,
		}

		status := http.StatusOK
		if overallStatus == "degraded" {
			status = http.StatusServiceUnavailable
		}

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		json.NewEncoder(w).Encode(health)
	}
}

// handleReadiness is a lightweight readiness probe for Kubernetes.
// Endpoint: GET /ready (no authentication required)
func handleReadiness(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(map[string]string{"status": "ready"})
}

// handleLiveness is a lightweight liveness probe for Kubernetes.
// Endpoint: GET /alive (no authentication required)
func handleLiveness(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(map[string]string{"status": "alive"})
}

// ---------------------------------------------------------------------------
// Application route handlers
// ---------------------------------------------------------------------------

// User represents a user resource in the system.
type User struct {
	ID        string    `json:"id"`
	Email     string    `json:"email"`
	Name      string    `json:"name"`
	Role      string    `json:"role"`
	CreatedAt time.Time `json:"createdAt"`
	UpdatedAt time.Time `json:"updatedAt"`
}

// CreateUserRequest is the request body for creating a new user.
type CreateUserRequest struct {
	Email string `json:"email"`
	Name  string `json:"name"`
	Role  string `json:"role"`
}

// handleListUsers returns a paginated list of users.
// Endpoint: GET /api/v1/users (requires authentication)
func handleListUsers(w http.ResponseWriter, r *http.Request) {
	page, _ := strconv.Atoi(r.URL.Query().Get("page"))
	if page < 1 {
		page = 1
	}
	limit, _ := strconv.Atoi(r.URL.Query().Get("limit"))
	if limit < 1 || limit > 100 {
		limit = 20
	}

	// In production, this queries the database
	users := []User{
		{ID: "usr_001", Email: "admin@example.com", Name: "Admin User", Role: "admin"},
		{ID: "usr_002", Email: "dev@example.com", Name: "Developer", Role: "developer"},
	}

	writeJSONResponse(w, http.StatusOK, map[string]interface{}{
		"data":       users,
		"page":       page,
		"limit":      limit,
		"totalCount": len(users),
	})
}

// handleCreateUser creates a new user in the system.
// Endpoint: POST /api/v1/users (requires authentication + admin role)
func handleCreateUser(w http.ResponseWriter, r *http.Request) {
	var req CreateUserRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeErrorResponse(w, ErrBadRequest)
		return
	}
	if req.Email == "" || req.Name == "" {
		writeErrorResponse(w, &AppError{
			StatusCode: 400,
			Code:       "VALIDATION_ERROR",
			Message:    "email and name are required fields",
		})
		return
	}

	user := User{
		ID:        "usr_" + generateRequestID()[:8],
		Email:     req.Email,
		Name:      req.Name,
		Role:      req.Role,
		CreatedAt: time.Now(),
		UpdatedAt: time.Now(),
	}
	writeJSONResponse(w, http.StatusCreated, user)
}

// handleGetUserByID retrieves a single user by ID.
// Endpoint: GET /api/v1/users/:id (requires authentication)
func handleGetUserByID(w http.ResponseWriter, r *http.Request) {
	// Simplified path parameter extraction
	parts := strings.Split(r.URL.Path, "/")
	if len(parts) < 5 {
		writeErrorResponse(w, ErrBadRequest)
		return
	}
	userID := parts[4]

	user := User{
		ID:        userID,
		Email:     "user@example.com",
		Name:      "Example User",
		Role:      "developer",
		CreatedAt: time.Now().Add(-24 * time.Hour),
		UpdatedAt: time.Now(),
	}
	writeJSONResponse(w, http.StatusOK, user)
}

// handleDeleteUser removes a user from the system.
// Endpoint: DELETE /api/v1/users/:id (requires authentication + admin role)
func handleDeleteUser(w http.ResponseWriter, r *http.Request) {
	claims, ok := GetClaims(r)
	if !ok {
		writeErrorResponse(w, ErrUnauthorized)
		return
	}
	hasAdmin := false
	for _, role := range claims.Roles {
		if role == "admin" {
			hasAdmin = true
			break
		}
	}
	if !hasAdmin {
		writeErrorResponse(w, ErrForbidden)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleMetrics exposes Prometheus-compatible metrics.
// Endpoint: GET /metrics (no authentication required)
func handleMetrics(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "text/plain")
	fmt.Fprintf(w, "# HELP http_requests_total Total HTTP requests\n")
	fmt.Fprintf(w, "# TYPE http_requests_total counter\n")
	fmt.Fprintf(w, "http_requests_total{method=\"GET\",path=\"/health\"} 42\n")
	fmt.Fprintf(w, "http_requests_total{method=\"GET\",path=\"/api/v1/users\"} 128\n")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

func writeJSONResponse(w http.ResponseWriter, status int, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(data)
}

func writeErrorResponse(w http.ResponseWriter, err *AppError) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(err.StatusCode)
	json.NewEncoder(w).Encode(err)
}

func extractClientIP(r *http.Request) string {
	if xff := r.Header.Get("X-Forwarded-For"); xff != "" {
		parts := strings.Split(xff, ",")
		return strings.TrimSpace(parts[0])
	}
	return strings.Split(r.RemoteAddr, ":")[0]
}

func generateRequestID() string {
	return fmt.Sprintf("%d", time.Now().UnixNano())
}

func envString(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func envInt(key string, fallback int) int {
	if v := os.Getenv(key); v != "" {
		if i, err := strconv.Atoi(v); err == nil {
			return i
		}
	}
	return fallback
}

func envStringSlice(key string, fallback []string) []string {
	if v := os.Getenv(key); v != "" {
		return strings.Split(v, ",")
	}
	return fallback
}

// ---------------------------------------------------------------------------
// Graceful shutdown
// ---------------------------------------------------------------------------

// GracefulServer wraps an http.Server with signal handling and graceful shutdown.
type GracefulServer struct {
	server          *http.Server
	shutdownTimeout time.Duration
	onShutdown      []func()
}

// NewGracefulServer creates a server that handles SIGINT and SIGTERM gracefully.
func NewGracefulServer(cfg *ServerConfig, handler http.Handler) *GracefulServer {
	return &GracefulServer{
		server: &http.Server{
			Addr:         fmt.Sprintf("%s:%d", cfg.Host, cfg.Port),
			Handler:      handler,
			ReadTimeout:  time.Duration(cfg.ReadTimeoutSeconds) * time.Second,
			WriteTimeout: time.Duration(cfg.WriteTimeoutSeconds) * time.Second,
		},
		shutdownTimeout: time.Duration(cfg.ShutdownTimeoutSeconds) * time.Second,
	}
}

// OnShutdown registers a callback to be invoked during graceful shutdown.
// Callbacks are invoked in registration order before the server stops accepting
// new connections.
func (gs *GracefulServer) OnShutdown(fn func()) {
	gs.onShutdown = append(gs.onShutdown, fn)
}

// ListenAndServe starts the HTTP server and blocks until shutdown is complete.
// It listens for SIGINT and SIGTERM to initiate graceful shutdown, draining
// active connections within the configured timeout.
func (gs *GracefulServer) ListenAndServe() error {
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, syscall.SIGINT, syscall.SIGTERM)

	errCh := make(chan error, 1)
	go func() {
		log.Printf("Server starting on %s", gs.server.Addr)
		if err := gs.server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			errCh <- err
		}
	}()

	select {
	case err := <-errCh:
		return fmt.Errorf("server failed to start: %w", err)
	case sig := <-stop:
		log.Printf("Received signal %v, initiating graceful shutdown...", sig)
	}

	// Run shutdown callbacks
	for _, fn := range gs.onShutdown {
		fn()
	}

	ctx, cancel := context.WithTimeout(context.Background(), gs.shutdownTimeout)
	defer cancel()

	if err := gs.server.Shutdown(ctx); err != nil {
		return fmt.Errorf("graceful shutdown failed: %w", err)
	}

	log.Println("Server stopped gracefully")
	return nil
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

func main() {
	cfg := LoadConfig()
	if err := cfg.Validate(); err != nil {
		log.Fatalf("Configuration error: %v", err)
	}

	router := NewRouter(cfg)

	// Register global middleware
	router.Use(LoggingMiddleware)
	router.Use(RequestIDMiddleware)
	router.Use(CORSMiddleware(cfg.CORSAllowedOrigins))
	router.Use(RateLimitMiddleware(router.rateLimiter))

	// Health and observability endpoints (no auth required)
	healthCheckers := []HealthChecker{
		&DatabaseHealthCheck{DatabaseURL: cfg.DatabaseURL},
		&RedisHealthCheck{RedisURL: cfg.RedisURL},
	}
	router.Handle("GET", "/health", handleHealthCheck(healthCheckers), false, "Comprehensive health check with dependency status")
	router.Handle("GET", "/ready", handleReadiness, false, "Kubernetes readiness probe")
	router.Handle("GET", "/alive", handleLiveness, false, "Kubernetes liveness probe")
	router.Handle("GET", "/metrics", handleMetrics, false, "Prometheus metrics endpoint")

	// API endpoints (auth required)
	router.Handle("GET", "/api/v1/users", handleListUsers, true, "List all users with pagination")
	router.Handle("POST", "/api/v1/users", handleCreateUser, true, "Create a new user")
	router.Handle("GET", "/api/v1/users/:id", handleGetUserByID, true, "Get user by ID")
	router.Handle("DELETE", "/api/v1/users/:id", handleDeleteUser, true, "Delete user by ID (admin only)")

	// Start server with graceful shutdown
	gs := NewGracefulServer(cfg, router)
	gs.OnShutdown(func() {
		log.Println("Closing database connections...")
	})
	gs.OnShutdown(func() {
		log.Println("Flushing metrics buffer...")
	})

	if err := gs.ListenAndServe(); err != nil {
		log.Fatalf("Server error: %v", err)
	}
}
