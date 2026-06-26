//! `/me/provider-keys` — per-employee BYO-AI provider-key custody (P9.3, #103).
//!
//! CRUD over the **caller's own** provider keys. Every action is gated by
//! [`require_clearance`] and written to the [`AuditLog`] under the caller's
//! host-authoritative identity. The store is namespaced by `FirmContext.user_id`
//! (ADR-0009), so a caller can only ever touch their own keys — the path is
//! `/me/...`, never `/users/{id}/...`. `GET` returns [`KeyMetadata`] only; the
//! plaintext secret never crosses to the webview or the audit log (ADR-0016).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog};
use kanbrick_core::ClearanceLevel;
use kanbrick_providers::{KeyId, KeyMetadata, KeyStoreError, ProviderKind};
use serde::Deserialize;
use uuid::Uuid;

use crate::{ApiError, AppState, AuthedContext};

/// Minimum clearance to manage one's own provider keys.
///
/// The floor (L1) — any authenticated employee may BYO their own key. The real
/// isolation is the per-user namespace, not a clearance step; the gate is here for
/// a uniform audited path and future tightening. Unauthenticated callers are
/// already rejected with `401` by the [`AuthedContext`] extractor before this runs.
const MANAGE_KEYS_CLEARANCE: ClearanceLevel = ClearanceLevel::L1;

/// `POST /me/provider-keys` body. The `secret` is write-only — it is stored and
/// never returned by any route.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateProviderKeyRequest {
    /// Which provider the key authenticates to.
    provider: ProviderKind,
    /// A human label for the key.
    label: String,
    /// The plaintext provider secret (write-only).
    secret: String,
}

/// `POST /me/provider-keys` — store a key for the caller; returns its metadata.
pub(crate) async fn create_key(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Json(req): Json<CreateProviderKeyRequest>,
) -> Result<Json<KeyMetadata>, ApiError> {
    require_clearance(&ctx, MANAGE_KEYS_CLEARANCE)?;
    let metadata = state
        .provider_keys
        .put(ctx.user_id, req.provider, &req.label, &req.secret)
        .map_err(key_store_error)?;
    // Metadata only — the secret is never logged.
    AuditLog::new(&state.store).record(
        &ctx,
        &format!("provider-key:create:{}:{}", metadata.provider, metadata.id),
    )?;
    Ok(Json(metadata))
}

/// `GET /me/provider-keys` — list the caller's key metadata (never the secrets).
pub(crate) async fn list_keys(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
) -> Result<Json<Vec<KeyMetadata>>, ApiError> {
    require_clearance(&ctx, MANAGE_KEYS_CLEARANCE)?;
    let keys = state
        .provider_keys
        .list(ctx.user_id)
        .map_err(key_store_error)?;
    AuditLog::new(&state.store).record(&ctx, "provider-key:list")?;
    Ok(Json(keys))
}

/// `DELETE /me/provider-keys/{id}` — remove one of the caller's keys.
pub(crate) async fn delete_key(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_clearance(&ctx, MANAGE_KEYS_CLEARANCE)?;
    let key_id = KeyId(id);
    let removed = state
        .provider_keys
        .delete(ctx.user_id, key_id)
        .map_err(key_store_error)?;
    AuditLog::new(&state.store).record(&ctx, &format!("provider-key:delete:{key_id}"))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no provider key {key_id}"),
        ))
    }
}

/// A custody-backend failure is an internal `500`.
fn key_store_error(err: KeyStoreError) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        err.to_string(),
    )
}
