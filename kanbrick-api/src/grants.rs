//! `/me/scope-requests` + `/me/scopes` — the ScopeGrants dual-gate over HTTP
//! (P11.2). Exposes the project-scope grant lifecycle the firm already implements
//! in [`kanbrick_discovery::ScopeGrants`]: request → approve/deny → list → revoke,
//! fully audited (the domain methods record the audit markers themselves).
//!
//! Identity is host-authoritative (ADR-0002/0016): the actor is always the
//! [`AuthedContext`]'s validated [`FirmContext`], never a field in the request body.
//! `approve`/`deny` additionally need the firm org-graph (the eligible-grantor
//! management chain), which is built per-request from the store via
//! [`DiscoveryGraph::from_store`] — always fresh (correct even after a reorg), at the
//! cost of a privileged full-graph read on each (rare) approval. The grant domain
//! types are not serializable, so the responses use the thin DTOs defined here.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use kanbrick_auth::require_clearance;
use kanbrick_core::ClearanceLevel;
use kanbrick_discovery::{
    DiscoveryGraph, GrantedScope, RequestStatus, ScopeGrants, ScopeRequest, ScopeStatus,
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState, AuthedContext};

/// Minimum clearance to approve or deny a scope request. The dual-gate's clearance
/// floor (L4 strategic); `approve`/`deny` additionally re-check that the grantor is
/// in the requester's management chain (or an L5 cofounder), so this is a cheap
/// pre-filter that avoids the privileged graph load for an unqualified caller.
const GRANTOR_CLEARANCE: ClearanceLevel = ClearanceLevel::L4;

// ── DTOs (the domain types are not serializable) ─────────────────────────────

/// Serializable mirror of [`ScopeRequest`]; `status` is the lowercase lifecycle
/// state (`"requested"` | `"granted"` | `"denied"` | `"expired"`).
#[derive(Debug, Serialize)]
pub(crate) struct ScopeRequestDto {
    id: String,
    project: String,
    requester: String,
    justification: String,
    persons: Vec<String>,
    companies: Vec<String>,
    status: &'static str,
}

impl From<ScopeRequest> for ScopeRequestDto {
    fn from(r: ScopeRequest) -> Self {
        ScopeRequestDto {
            id: r.id,
            project: r.project,
            requester: r.requester,
            justification: r.justification,
            persons: r.persons,
            companies: r.companies,
            status: request_status_str(r.status),
        }
    }
}

/// Serializable mirror of [`GrantedScope`]; `status` is the lowercase lifecycle
/// state (`"pending"` | `"active"` | `"expired"` | `"revoked"`).
#[derive(Debug, Serialize)]
pub(crate) struct GrantedScopeDto {
    id: String,
    project: String,
    requester: String,
    granted_by: String,
    granted_persons: Vec<String>,
    granted_companies: Vec<String>,
    expires_at: Option<String>,
    status: &'static str,
}

impl From<GrantedScope> for GrantedScopeDto {
    fn from(s: GrantedScope) -> Self {
        GrantedScopeDto {
            id: s.id,
            project: s.project,
            requester: s.requester,
            granted_by: s.granted_by,
            granted_persons: s.granted_persons,
            granted_companies: s.granted_companies,
            expires_at: s.expires_at,
            status: scope_status_str(s.status),
        }
    }
}

fn request_status_str(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Requested => "requested",
        RequestStatus::Granted => "granted",
        RequestStatus::Denied => "denied",
        RequestStatus::Expired => "expired",
    }
}

fn scope_status_str(status: ScopeStatus) -> &'static str {
    match status {
        ScopeStatus::Pending => "pending",
        ScopeStatus::Active => "active",
        ScopeStatus::Expired => "expired",
        ScopeStatus::Revoked => "revoked",
    }
}

// ── Request bodies / query ───────────────────────────────────────────────────

/// `POST /me/scope-requests` body. No requester field — identity is host-side.
#[derive(Debug, Deserialize)]
pub(crate) struct RequestScopeBody {
    project: String,
    #[serde(default)]
    persons: Vec<String>,
    #[serde(default)]
    companies: Vec<String>,
    #[serde(default)]
    justification: String,
}

/// `POST /me/scope-requests/{id}/approve` body.
#[derive(Debug, Deserialize)]
pub(crate) struct ApproveBody {
    /// Optional grant lifetime in days (`None` = no expiry).
    #[serde(default)]
    ttl_days: Option<i64>,
}

/// `POST /me/scope-requests/{id}/deny` body.
#[derive(Debug, Deserialize)]
pub(crate) struct DenyBody {
    #[serde(default)]
    reason: String,
}

/// `POST /me/scopes/{id}/revoke` body.
#[derive(Debug, Deserialize)]
pub(crate) struct RevokeBody {
    #[serde(default)]
    reason: String,
}

/// `GET /me/scopes` query — the project whose active grants to list.
#[derive(Debug, Deserialize)]
pub(crate) struct ScopesQuery {
    project: String,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /me/scope-requests` — submit a scope request as the authenticated caller.
pub(crate) async fn create_scope_request(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Json(body): Json<RequestScopeBody>,
) -> Result<Json<ScopeRequestDto>, ApiError> {
    let grants = ScopeGrants::new(&state.store);
    let request = grants.request_scope(
        &ctx,
        &body.project,
        &body.persons,
        &body.companies,
        &body.justification,
    )?;
    Ok(Json(request.into()))
}

/// `GET /me/scope-requests/{id}` — read a request. Visible to its requester or to a
/// grantor-clearance (L4+) reviewer.
pub(crate) async fn read_scope_request(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<String>,
) -> Result<Json<ScopeRequestDto>, ApiError> {
    let grants = ScopeGrants::new(&state.store);
    let request = grants.request(&id)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("scope request {id}"),
        )
    })?;
    if request.requester != ctx.email && !ctx.clearance.satisfies(GRANTOR_CLEARANCE) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "not your scope request",
        ));
    }
    Ok(Json(request.into()))
}

/// `POST /me/scope-requests/{id}/approve` — approve a request (grantor-gated). The
/// L4 floor is a pre-filter; `approve` re-checks the management chain and returns
/// `403` if the caller is not an eligible grantor.
pub(crate) async fn approve_scope_request(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<String>,
    Json(body): Json<ApproveBody>,
) -> Result<Json<GrantedScopeDto>, ApiError> {
    require_clearance(&ctx, GRANTOR_CLEARANCE)?;
    let graph = DiscoveryGraph::from_store(&state.store)?;
    let grants = ScopeGrants::new(&state.store);
    let granted = grants.approve(&id, &ctx, &graph, body.ttl_days)?;
    Ok(Json(granted.into()))
}

/// `POST /me/scope-requests/{id}/deny` — deny a request (grantor-gated, as approve).
pub(crate) async fn deny_scope_request(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<String>,
    Json(body): Json<DenyBody>,
) -> Result<StatusCode, ApiError> {
    require_clearance(&ctx, GRANTOR_CLEARANCE)?;
    let graph = DiscoveryGraph::from_store(&state.store)?;
    let grants = ScopeGrants::new(&state.store);
    grants.deny(&id, &ctx, &graph, &body.reason)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/scopes?project=…` — the caller's own active grants for a project.
pub(crate) async fn list_scopes(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Query(query): Query<ScopesQuery>,
) -> Result<Json<Vec<GrantedScopeDto>>, ApiError> {
    let grants = ScopeGrants::new(&state.store);
    let scopes = grants.active_scopes_for(&ctx.email, &query.project, chrono::Utc::now())?;
    let dtos: Vec<GrantedScopeDto> = scopes.into_iter().map(GrantedScopeDto::from).collect();
    Ok(Json(dtos))
}

/// `POST /me/scopes/{id}/revoke` — revoke a granted scope. `revoke` enforces that the
/// caller is the granting grantor or an L5 cofounder (returns `403` otherwise).
pub(crate) async fn revoke_scope(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<String>,
    Json(body): Json<RevokeBody>,
) -> Result<StatusCode, ApiError> {
    let grants = ScopeGrants::new(&state.store);
    // No discovery answer-cache is wired into the API yet, so nothing to invalidate.
    grants.revoke(&id, &ctx, &body.reason, None)?;
    Ok(StatusCode::NO_CONTENT)
}
