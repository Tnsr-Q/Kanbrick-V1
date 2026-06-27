//! `/me/messenger` — the internal messenger over the `EventBus` (P10.1, #113).
//!
//! A send emits a typed [`MessengerEvent`] onto the shared bus
//! ([`AppState::bus`]); the bus's replayable in-memory log is the message
//! history. Every action is gated by [`require_clearance`] and written to the
//! [`AuditLog`] under the caller's host-authoritative identity. The `actor` is
//! always resolved from the validated `FirmContext` — never accepted from the
//! request body (ADR-0002/0016), so a caller cannot post as someone else.

use axum::extract::{Query, State};
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog};
use kanbrick_core::abi::{MessengerEvent, MessengerScope, MESSENGER_EVENT_KIND};
use kanbrick_core::ClearanceLevel;
use serde::Deserialize;

use crate::{ApiError, AppState, AuthedContext};

/// Minimum clearance to use the messenger.
///
/// The floor (L1) — any authenticated employee may send and read firm messages.
/// Unauthenticated callers are already rejected with `401` by the
/// [`AuthedContext`] extractor before this runs.
const MESSENGER_CLEARANCE: ClearanceLevel = ClearanceLevel::L1;

/// `POST /me/messenger/send` body.
///
/// The `actor` is intentionally absent — it is derived host-side from the
/// validated token, so a client cannot spoof the sender.
#[derive(Debug, Deserialize)]
pub(crate) struct SendMessageRequest {
    /// The message body.
    text: String,
    /// Who the message is addressed to. Defaults to `public`.
    #[serde(default)]
    scope: MessengerScope,
}

/// `POST /me/messenger/send` — emit a message onto the bus; audited.
pub(crate) async fn send_message(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<MessengerEvent>, ApiError> {
    require_clearance(&ctx, MESSENGER_CLEARANCE)?;
    // `actor` is host-authoritative: taken from the validated identity, not the body.
    let message = MessengerEvent::new(ctx.email.clone(), req.text, req.scope);
    state.bus.emit(message.to_event());
    AuditLog::new(&state.store)
        .record(&ctx, &format!("messenger:send:{}", message.scope.label()))?;
    Ok(Json(message))
}

/// `GET /me/messenger/log?kind&limit` query parameters.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct LogQuery {
    /// Event kind to replay; defaults to [`MESSENGER_EVENT_KIND`].
    #[serde(default)]
    kind: Option<String>,
    /// Return only the most recent `limit` messages, if set.
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /me/messenger/log` — replay messages from the bus's in-memory log; audited.
pub(crate) async fn message_log(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Query(q): Query<LogQuery>,
) -> Result<Json<Vec<MessengerEvent>>, ApiError> {
    require_clearance(&ctx, MESSENGER_CLEARANCE)?;
    let kind = q.kind.as_deref().unwrap_or(MESSENGER_EVENT_KIND);
    // The bus log is ordered oldest→newest; keep events that decode as messages.
    let mut messages: Vec<MessengerEvent> = state
        .bus
        .history()
        .into_iter()
        .filter(|event| event.kind == kind)
        .filter_map(|event| serde_json::from_value::<MessengerEvent>(event.payload).ok())
        .collect();
    // Bound to the most recent `limit` if requested (drops the oldest).
    if let Some(limit) = q.limit {
        if messages.len() > limit {
            messages.drain(0..messages.len() - limit);
        }
    }
    AuditLog::new(&state.store).record(&ctx, "messenger:log")?;
    Ok(Json(messages))
}
