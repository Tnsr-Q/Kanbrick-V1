//! # Host-Guest ABI (issue #22, ADR-0002)
//!
//! The stable contract across the WASM boundary between the host
//! (`kanbrick-mesh`) and guest modules (Phase 5). It is deliberately a *one-way
//! door*: every layer above is wired against these types, so the surface is
//! reviewed before anything builds on it.
//!
//! ## Shape
//!
//! * [`HostFunctions`] — the capabilities the host exposes to a running guest:
//!   read the caller's [`FirmContext`], run a [`GraphQuery`], emit an [`Event`],
//!   and [`log`](HostFunctions::log). Implemented host-side; inside the guest
//!   these appear as imported functions (wired in #23/#24).
//! * [`GuestModule`] — the contract every guest implements: [`name`], [`version`],
//!   [`execute`], and an optional [`health_check`].
//! * DTOs ([`GraphQuery`], [`GraphRows`], [`Event`], [`GuestRequest`],
//!   [`GuestResponse`]) — all `serde` types, carried across the boundary as
//!   **JSON** (the format chosen in ADR-0002).
//!
//! ## Security invariant (host-authoritative identity, #23)
//!
//! [`GuestRequest`] carries the guest-specific payload **only** — never a
//! [`FirmContext`]. A guest can *read* identity via
//! [`HostFunctions::get_firm_context`] but can never supply or forge it. The
//! type system makes the spoofing path unrepresentable. Likewise every
//! [`HostFunctions::query_graph`] runs under the host's context and is routed
//! through the clearance-enforcing `GuardedStore` in #24.
//!
//! [`name`]: GuestModule::name
//! [`version`]: GuestModule::version
//! [`execute`]: GuestModule::execute
//! [`health_check`]: GuestModule::health_check

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{Error, FirmContext, Result};

/// Version of this ABI. Bumped on any breaking change to the boundary contract
/// so the host can refuse guests built against an incompatible surface.
pub const ABI_VERSION: u32 = 1;

/// A parameterized graph query a guest asks the host to run on its behalf.
///
/// Parameters are bound by the host, never interpolated into `cypher`, so guest
/// input cannot alter the parsed structure of the query (injection prevention,
/// mirroring the store layer's `Params`, #9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphQuery {
    /// The Cypher statement, with `$name` placeholders for parameters.
    pub cypher: String,
    /// Named parameter bindings, JSON-typed.
    #[serde(default)]
    pub params: BTreeMap<String, JsonValue>,
}

impl GraphQuery {
    /// A query with no parameters.
    pub fn new(cypher: impl Into<String>) -> Self {
        GraphQuery {
            cypher: cypher.into(),
            params: BTreeMap::new(),
        }
    }

    /// Bind `name` to `value` (builder style).
    pub fn param(mut self, name: impl Into<String>, value: impl Into<JsonValue>) -> Self {
        self.params.insert(name.into(), value.into());
        self
    }
}

/// Rows returned from a [`GraphQuery`]. Each row is a JSON object of
/// `column -> value`. Rows are already **clearance-filtered by the host** before
/// they cross the boundary (#24), so a guest only ever sees what its caller may.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphRows {
    /// One JSON value (typically an object) per result row.
    pub rows: Vec<JsonValue>,
}

impl GraphRows {
    /// Wrap a vector of rows.
    pub fn new(rows: Vec<JsonValue>) -> Self {
        GraphRows { rows }
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the result set is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Severity of a guest log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// A failure the guest could not handle.
    Error,
    /// A recoverable anomaly worth surfacing.
    Warn,
    /// Routine progress information.
    Info,
    /// Detail useful when diagnosing the guest.
    Debug,
    /// Very fine-grained tracing.
    Trace,
}

/// An event a guest emits onto the host event bus (#27).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Dotted event kind, e.g. `"valuation.completed"`.
    pub kind: String,
    /// Arbitrary JSON payload describing the event.
    #[serde(default)]
    pub payload: JsonValue,
}

impl Event {
    /// An event of `kind` with an empty payload.
    pub fn new(kind: impl Into<String>) -> Self {
        Event {
            kind: kind.into(),
            payload: JsonValue::Null,
        }
    }

    /// An event of `kind` carrying `payload`.
    pub fn with_payload(kind: impl Into<String>, payload: impl Into<JsonValue>) -> Self {
        Event {
            kind: kind.into(),
            payload: payload.into(),
        }
    }
}

// ── Messenger (Cockpit req 2.2, P10.1) ────────────────────────────────────────
//
// The internal messenger is a typed payload carried over the existing
// [`Event`] bus (`kanbrick-mesh`'s `EventBus`), not a new fabric. A send emits
// one [`Event`] of kind [`MESSENGER_EVENT_KIND`]; the replayable bus log gives
// the message history for free. `actor` is always the host-authoritative sender
// (resolved from the caller's `FirmContext`, never the request body) per ADR-0002.

/// The `kind` under which [`MessengerEvent`]s are emitted on the event bus.
pub const MESSENGER_EVENT_KIND: &str = "messenger.message";

/// Who a [`MessengerEvent`] is addressed to.
///
/// A serde **internally-tagged** union (`{ "kind": "public" }` /
/// `{ "kind": "group", "name": "…" }`) so it mirrors a TypeScript discriminated
/// union 1:1 on the Cockpit side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessengerScope {
    /// Firm-wide — visible to every employee.
    #[default]
    Public,
    /// Addressed to a named group.
    ///
    /// At P10.1 this is an **addressing label only**: the message log is
    /// firm-wide and not yet filtered by group membership. Per-group read ACLs
    /// land in a later slice — do not treat `group` as a confidentiality
    /// boundary today.
    Group {
        /// The group's name.
        name: String,
    },
}

impl MessengerScope {
    /// A short, stable label for logs/audit, e.g. `public` or `group:engineering`.
    pub fn label(&self) -> String {
        match self {
            MessengerScope::Public => "public".to_string(),
            MessengerScope::Group { name } => format!("group:{name}"),
        }
    }
}

/// One internal message: who sent it, the text, and who it is addressed to.
///
/// Carried as the JSON payload of an [`Event`] of kind [`MESSENGER_EVENT_KIND`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessengerEvent {
    /// The host-authoritative sender (the caller's `FirmContext` handle).
    pub actor: String,
    /// The message body.
    pub text: String,
    /// Who the message is addressed to.
    pub scope: MessengerScope,
}

impl MessengerEvent {
    /// Build a message from its sender, text, and scope.
    pub fn new(actor: impl Into<String>, text: impl Into<String>, scope: MessengerScope) -> Self {
        MessengerEvent {
            actor: actor.into(),
            text: text.into(),
            scope,
        }
    }

    /// Wrap this message as an [`Event`] for emission on the bus.
    pub fn to_event(&self) -> Event {
        Event::with_payload(
            MESSENGER_EVENT_KIND,
            serde_json::to_value(self).expect("MessengerEvent always serializes"),
        )
    }
}

/// Input handed to [`GuestModule::execute`].
///
/// Carries the guest-specific `payload` **only** — never a [`FirmContext`]. See
/// the module-level security invariant: identity is host-authoritative and read
/// back through [`HostFunctions::get_firm_context`], never passed in here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuestRequest {
    /// The guest-specific request payload.
    pub payload: JsonValue,
}

impl GuestRequest {
    /// Build a request from a JSON payload.
    pub fn new(payload: impl Into<JsonValue>) -> Self {
        GuestRequest {
            payload: payload.into(),
        }
    }

    /// Decode a request from its JSON wire bytes (the `kbk_run` input, #21).
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes)
            .map_err(|e| Error::InvalidInput(format!("guest request: {e}")))
    }

    /// Encode this request to its JSON wire bytes.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| Error::Internal(format!("guest request: {e}")))
    }
}

/// Output produced by [`GuestModule::execute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuestResponse {
    /// The guest-specific response payload.
    pub payload: JsonValue,
}

impl GuestResponse {
    /// Build a response from a JSON payload.
    pub fn new(payload: impl Into<JsonValue>) -> Self {
        GuestResponse {
            payload: payload.into(),
        }
    }

    /// Decode a response from its JSON wire bytes (the `kbk_run` output, #21).
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes)
            .map_err(|e| Error::InvalidInput(format!("guest response: {e}")))
    }

    /// Encode this response to its JSON wire bytes.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| Error::Internal(format!("guest response: {e}")))
    }
}

/// Capabilities the host exposes to a running guest.
///
/// Implemented host-side. Inside the guest these are imported host functions
/// (wired in #23/#24). Every call is made on behalf of the host-authoritative
/// [`FirmContext`]; a guest can neither supply nor forge identity, and
/// [`query_graph`](HostFunctions::query_graph) is clearance-filtered by the host.
pub trait HostFunctions {
    /// The caller's identity for this invocation. Host-authoritative (#23).
    fn get_firm_context(&self) -> FirmContext;

    /// Run a graph query under the caller's clearance and return the rows they
    /// are permitted to see (routed through `GuardedStore` in #24).
    fn query_graph(&self, query: GraphQuery) -> Result<GraphRows>;

    /// Emit an event onto the host event bus (#27).
    fn emit_event(&self, event: Event) -> Result<()>;

    /// Record a log line from the guest at `level`.
    fn log(&self, level: LogLevel, message: &str);
}

/// The contract every WASM guest module implements.
///
/// In Phase 5 a guest's exported `kbk_run` (the #21 calling convention) decodes a
/// [`GuestRequest`], drives an implementation of this trait — calling back into
/// the host through [`HostFunctions`] as needed — and encodes the
/// [`GuestResponse`]. The guest SDK (#39) generates that glue.
pub trait GuestModule {
    /// The guest's stable name (its registry key).
    fn name(&self) -> &str;

    /// The guest's semantic version.
    fn version(&self) -> &str;

    /// Process one request, using `host` for any graph/event/log capabilities.
    fn execute(&mut self, host: &dyn HostFunctions, request: GuestRequest)
        -> Result<GuestResponse>;

    /// Liveness probe. Defaults to healthy.
    fn health_check(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClearanceLevel;
    use serde_json::json;
    use std::cell::RefCell;
    use uuid::Uuid;

    /// Assert a value survives a JSON round trip across the boundary.
    fn round_trip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let bytes = serde_json::to_vec(value).unwrap();
        let back: T = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value, &back);
    }

    #[test]
    fn dtos_round_trip_as_json() {
        round_trip(&GraphQuery::new("MATCH (c:Company) RETURN c").param("seg", "alpha"));
        round_trip(&GraphRows::new(vec![
            json!({"name": "Acme"}),
            json!({"name": "Globex"}),
        ]));
        round_trip(&Event::with_payload(
            "valuation.completed",
            json!({"company": "Acme"}),
        ));
        round_trip(&GuestRequest::new(json!({"company_id": 7})));
        round_trip(&GuestResponse::new(json!({"npv": 1234.5})));
        round_trip(&LogLevel::Warn);
    }

    #[test]
    fn messenger_event_round_trips_and_tags_scope() {
        round_trip(&MessengerEvent::new(
            "elena@kanbrick.com",
            "hi",
            MessengerScope::Public,
        ));
        round_trip(&MessengerEvent::new(
            "elena@kanbrick.com",
            "hi team",
            MessengerScope::Group {
                name: "engineering".to_string(),
            },
        ));
        // Scope is a discriminated union keyed on `kind`.
        assert_eq!(
            serde_json::to_value(MessengerScope::Public).unwrap(),
            json!({"kind": "public"})
        );
        assert_eq!(
            serde_json::to_value(MessengerScope::Group {
                name: "engineering".to_string()
            })
            .unwrap(),
            json!({"kind": "group", "name": "engineering"})
        );
        // The default scope is public.
        assert_eq!(MessengerScope::default(), MessengerScope::Public);
    }

    #[test]
    fn messenger_event_to_event_uses_the_messenger_kind() {
        let msg = MessengerEvent::new("elena@kanbrick.com", "hi", MessengerScope::Public);
        let event = msg.to_event();
        assert_eq!(event.kind, MESSENGER_EVENT_KIND);
        let decoded: MessengerEvent = serde_json::from_value(event.payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn messenger_scope_label_is_stable() {
        assert_eq!(MessengerScope::Public.label(), "public");
        assert_eq!(
            MessengerScope::Group {
                name: "engineering".to_string()
            }
            .label(),
            "group:engineering"
        );
    }

    #[test]
    fn log_level_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&LogLevel::Error).unwrap(),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&LogLevel::Trace).unwrap(),
            "\"trace\""
        );
    }

    #[test]
    fn request_response_wire_bytes_round_trip() {
        let req = GuestRequest::new(json!({"x": 1}));
        assert_eq!(
            GuestRequest::from_json_bytes(&req.to_json_bytes().unwrap()).unwrap(),
            req
        );
        let resp = GuestResponse::new(json!([1, 2, 3]));
        assert_eq!(
            GuestResponse::from_json_bytes(&resp.to_json_bytes().unwrap()).unwrap(),
            resp
        );
    }

    #[test]
    fn malformed_request_bytes_are_a_validation_error() {
        let err = GuestRequest::from_json_bytes(b"not json").unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::ValidationError);
    }

    /// A `GuestRequest` has no field that could carry a `FirmContext`: callers
    /// cannot inject identity. This test documents the invariant — a JSON object
    /// with an extra `context` key still deserializes, ignoring it.
    #[test]
    fn guest_request_cannot_smuggle_identity() {
        let with_context = r#"{"payload": {"a": 1}, "context": {"clearance": "L5"}}"#;
        let req: GuestRequest = serde_json::from_str(with_context).unwrap();
        assert_eq!(req.payload, json!({"a": 1}));
        // The smuggled `context` is simply not part of the type.
    }

    // ---- Mock-boundary end-to-end: a host + a guest exercised in-process. ----

    /// A host that records emitted events and log lines and answers queries from
    /// a fixed table — standing in for the real wasmtime-backed host (#23/#24).
    struct MockHost {
        ctx: FirmContext,
        events: RefCell<Vec<Event>>,
        logs: RefCell<Vec<(LogLevel, String)>>,
    }

    impl MockHost {
        fn new(clearance: ClearanceLevel) -> Self {
            MockHost {
                ctx: FirmContext::new(Uuid::nil(), "analyst@kanbrick.com", clearance),
                events: RefCell::new(Vec::new()),
                logs: RefCell::new(Vec::new()),
            }
        }
    }

    impl HostFunctions for MockHost {
        fn get_firm_context(&self) -> FirmContext {
            self.ctx.clone()
        }

        fn query_graph(&self, query: GraphQuery) -> Result<GraphRows> {
            // Echo the bound parameter back as a single row, proving params flow.
            let seg = query
                .params
                .get("segment")
                .cloned()
                .unwrap_or(JsonValue::Null);
            Ok(GraphRows::new(vec![json!({"segment": seg, "count": 3})]))
        }

        fn emit_event(&self, event: Event) -> Result<()> {
            self.events.borrow_mut().push(event);
            Ok(())
        }

        fn log(&self, level: LogLevel, message: &str) {
            self.logs.borrow_mut().push((level, message.to_string()));
        }
    }

    /// A guest that reports its caller's clearance, runs a query, and emits an
    /// event — touching every `HostFunctions` capability.
    struct ReportingGuest;

    impl GuestModule for ReportingGuest {
        fn name(&self) -> &str {
            "reporting"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn execute(
            &mut self,
            host: &dyn HostFunctions,
            request: GuestRequest,
        ) -> Result<GuestResponse> {
            host.log(LogLevel::Info, "reporting guest started");
            let ctx = host.get_firm_context();
            let segment = request
                .payload
                .get("segment")
                .cloned()
                .unwrap_or(JsonValue::Null);
            let rows = host.query_graph(
                GraphQuery::new("MATCH (c:Company) RETURN c").param("segment", segment.clone()),
            )?;
            host.emit_event(Event::with_payload(
                "reporting.completed",
                json!({"rows": rows.len()}),
            ))?;
            Ok(GuestResponse::new(json!({
                "clearance": ctx.clearance,
                "segment": segment,
                "row_count": rows.len(),
            })))
        }
    }

    #[test]
    fn guest_drives_host_over_the_mock_boundary() {
        let host = MockHost::new(ClearanceLevel::L3);
        let mut guest = ReportingGuest;

        assert_eq!(guest.name(), "reporting");
        assert_eq!(guest.version(), "0.1.0");
        assert!(guest.health_check().is_ok());

        // Encode -> decode the request as it would cross the wire, then execute.
        let request = GuestRequest::new(json!({"segment": "alpha"}));
        let wire = request.to_json_bytes().unwrap();
        let decoded = GuestRequest::from_json_bytes(&wire).unwrap();

        let response = guest.execute(&host, decoded).unwrap();

        // The guest read its caller's (host-authoritative) clearance...
        assert_eq!(response.payload["clearance"], json!("L3"));
        // ...passed its parameter through the host query...
        assert_eq!(response.payload["segment"], json!("alpha"));
        assert_eq!(response.payload["row_count"], json!(1));
        // ...emitted exactly one event and logged once.
        assert_eq!(host.events.borrow().len(), 1);
        assert_eq!(host.events.borrow()[0].kind, "reporting.completed");
        assert_eq!(host.logs.borrow().len(), 1);

        // And the response itself survives the return trip across the boundary.
        round_trip(&response);
    }
}
