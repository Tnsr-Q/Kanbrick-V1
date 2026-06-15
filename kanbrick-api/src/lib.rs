//! # kanbrick-api
//!
//! HTTP surface for Kanbrick-V1. This crate wires the auth layer to Axum:
//!
//! * `POST /login` — email + password → JWT (issue #15).
//! * `GET  /me` — returns the caller's identity; requires a valid JWT.
//! * `GET  /admin` — a clearance-gated route requiring L4+ (issue #16).
//!
//! A missing/invalid/expired JWT yields a structured `401`; insufficient
//! clearance yields a structured `403`.

use std::sync::Arc;

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kanbrick_auth::{require_clearance, JwtAuthenticator, LoginService};
use kanbrick_core::{ClearanceLevel, Error, ErrorKind, FirmContext};
use kanbrick_store::Store;
use serde::{Deserialize, Serialize};

/// Shared application state, cheaply cloneable (everything behind `Arc`).
#[derive(Clone)]
pub struct AppState {
    /// The embedded graph store.
    pub store: Arc<Store>,
    /// JWT issuer/validator.
    pub jwt: Arc<JwtAuthenticator>,
}

impl AppState {
    /// Build state from a store and JWT authenticator.
    pub fn new(store: Store, jwt: JwtAuthenticator) -> Self {
        AppState {
            store: Arc::new(store),
            jwt: Arc::new(jwt),
        }
    }
}

/// Assemble the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/me", get(me))
        .route("/admin", get(admin))
        .with_state(state)
}

// ── Error responses ───────────────────────────────────────────────────────────

/// A structured API error rendered as JSON `{ "error": { "kind", "message" } }`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    kind: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            kind,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        let status = match err.kind() {
            ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::ValidationError => StatusCode::BAD_REQUEST,
            ErrorKind::QueryError | ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // AccessDenied is an authorization failure: surface it as 403.
        let status = if matches!(err, Error::AccessDenied { .. }) {
            StatusCode::FORBIDDEN
        } else {
            status
        };
        let kind = match err.kind() {
            ErrorKind::Unauthorized if status == StatusCode::FORBIDDEN => "forbidden",
            ErrorKind::Unauthorized => "unauthorized",
            ErrorKind::NotFound => "not_found",
            ErrorKind::ValidationError => "invalid_request",
            ErrorKind::QueryError | ErrorKind::Internal => "internal",
        };
        ApiError::new(status, kind, err.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "error": { "kind": self.kind, "message": self.message }
        }));
        (self.status, body).into_response()
    }
}

// ── Auth extractor ────────────────────────────────────────────────────────────

/// Extractor that authenticates the request from its `Authorization: Bearer`
/// JWT and yields the caller's [`FirmContext`]. Rejects with `401` on any
/// missing/malformed/invalid token.
pub struct AuthedContext(pub FirmContext);

impl FromRequestParts<AppState> for AuthedContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::unauthorized("expected a Bearer token"))?;
        let ctx = state
            .jwt
            .validate(token)
            .map_err(|_| ApiError::unauthorized("invalid or expired token"))?;
        Ok(AuthedContext(ctx))
    }
}

// ── Request/response bodies ───────────────────────────────────────────────────

/// `POST /login` request body.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Login email.
    pub email: String,
    /// Plaintext password.
    pub password: String,
}

/// `POST /login` success body.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The signed JWT.
    pub token: String,
}

/// `GET /me` body — the caller's identity.
#[derive(Debug, Serialize)]
pub struct MeResponse {
    /// Caller email.
    pub email: String,
    /// Caller clearance.
    pub clearance: ClearanceLevel,
    /// Caller role tags.
    pub roles: Vec<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let svc = LoginService::new(&state.store, &state.jwt);
    let token = svc.login(&req.email, &req.password)?;
    Ok(Json(LoginResponse { token }))
}

async fn me(AuthedContext(ctx): AuthedContext) -> Json<MeResponse> {
    Json(MeResponse {
        email: ctx.email,
        clearance: ctx.clearance,
        roles: ctx.roles,
    })
}

async fn admin(AuthedContext(ctx): AuthedContext) -> Result<Json<MeResponse>, ApiError> {
    // Coarse clearance gate: this route requires strategic (L4) clearance.
    require_clearance(&ctx, ClearanceLevel::L4)?;
    Ok(Json(MeResponse {
        email: ctx.email,
        clearance: ctx.clearance,
        roles: ctx.roles,
    }))
}
