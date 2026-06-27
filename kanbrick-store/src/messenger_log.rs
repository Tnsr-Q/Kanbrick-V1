//! Durable messenger history (#114, P10.2).
//!
//! Persists each messenger message as an append-only `(:MessengerMessage)` node so
//! history survives a process restart and outlives the bounded in-memory
//! `EventBus` replay window. A unique `msg_id` per node means the ADR-0001 `MERGE`
//! always **creates** (never matches an existing node), giving append-only
//! semantics — the same shape as `(:AuditEntry)`.
//!
//! Ordering is by a stored append-only `seq` sorted on read, so the durable read
//! returns messages oldest→newest independent of the graph's storage order. `seq`
//! is the current node count at insert time: strictly increasing for sequential
//! sends. A rare concurrent tie is benign — a unique `msg_id` still yields distinct
//! nodes, so no message is lost; only the relative order of a same-`seq` pair is
//! left unspecified.

use kanbrick_core::abi::{MessengerEvent, MessengerScope};
use kanbrick_core::Result;
use serde::Deserialize;
use uuid::Uuid;

use crate::store::Store;
use crate::value::Params;

/// Persist `message` (emitted under event `kind`) as a durable
/// `(:MessengerMessage)`.
///
/// Append-only: a fresh `msg_id` guarantees the `MERGE` creates a new node rather
/// than matching an existing one. Follows the ADR-0001 dialect — `MERGE` on the
/// unique key alone, then `MATCH … SET` the fields (the same upsert shape as
/// [`crate::guest_policy`]/[`crate::registry_meta`]; `SET` is what carries the
/// integer `seq`). `scope` is stored as its serde JSON form so it round-trips
/// exactly; `seq` is the ordering key and `timestamp` an RFC 3339 forensic capture.
pub fn persist_message(store: &Store, message: &MessengerEvent, kind: &str) -> Result<()> {
    // The current count is the next append-only index. Sequential sends get a
    // strictly increasing `seq`; see the module docs on the benign concurrent tie.
    let seq = count_messages(store)?;
    let scope_json =
        serde_json::to_string(&message.scope).expect("MessengerScope always serializes");
    let msg_id = Uuid::new_v4().to_string();
    store.execute_with(
        "MERGE (m:MessengerMessage {msg_id: $msg_id})",
        Params::new().with("msg_id", msg_id.as_str()),
    )?;
    store.execute_with(
        "MATCH (m:MessengerMessage {msg_id: $msg_id}) \
         SET m.actor = $actor, m.text = $text, m.scope_json = $scope_json, \
             m.kind = $kind, m.seq = $seq, m.timestamp = $timestamp",
        Params::new()
            .with("msg_id", msg_id.as_str())
            .with("actor", message.actor.as_str())
            .with("text", message.text.as_str())
            .with("scope_json", scope_json)
            .with("kind", kind)
            .with("seq", seq)
            .with("timestamp", chrono::Utc::now().to_rfc3339()),
    )?;
    Ok(())
}

/// Internal row shape for the durable read. Field names mirror the projected
/// property names — the row mapper strips the `m.` prefix (bare-node projection per
/// ADR-0001, as in [`crate::guest_policy`]); aliasing would yield nulls.
#[derive(Debug, Deserialize)]
struct MessageRow {
    actor: String,
    text: String,
    scope_json: String,
    seq: i64,
}

/// Read persisted messages of `kind`, oldest→newest, optionally limited to the
/// most recent `limit`.
///
/// This is the **authoritative** history: it is unaffected by the bounded
/// in-memory `EventBus` window, so it survives both eviction and a process
/// restart. A row whose stored scope fails to decode is skipped (not fatal),
/// mirroring the bus replay's tolerance for a malformed payload.
pub fn list_messages(
    store: &Store,
    kind: &str,
    limit: Option<usize>,
) -> Result<Vec<MessengerEvent>> {
    let mut rows: Vec<MessageRow> = store.query::<MessageRow>(
        "MATCH (m:MessengerMessage {kind: $kind}) \
         RETURN m.actor, m.text, m.scope_json, m.seq",
        Params::new().with("kind", kind),
    )?;
    // Order by the append-only sequence; the graph's storage order is unspecified.
    rows.sort_by_key(|r| r.seq);
    // Keep only the most recent `limit`, dropping the oldest — matching the P10.1
    // HTTP semantics for `?limit`.
    if let Some(limit) = limit {
        if rows.len() > limit {
            rows.drain(0..rows.len() - limit);
        }
    }
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let scope: MessengerScope = serde_json::from_str(&r.scope_json).ok()?;
            Some(MessengerEvent::new(r.actor, r.text, scope))
        })
        .collect())
}

/// Count persisted messenger messages. Used to assign the next `seq` and as a
/// test/inspection helper.
pub fn count_messages(store: &Store) -> Result<i64> {
    Ok(store
        .scalar_i64("MATCH (m:MessengerMessage) RETURN count(m)", Params::new())?
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::abi::MESSENGER_EVENT_KIND;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn public(actor: &str, text: &str) -> MessengerEvent {
        MessengerEvent::new(actor, text, MessengerScope::Public)
    }

    #[test]
    fn persist_then_list_preserves_order_and_counts() {
        let (_d, store) = store();
        assert_eq!(count_messages(&store).unwrap(), 0);
        for text in ["first", "second", "third"] {
            persist_message(
                &store,
                &public("elena@kanbrick.com", text),
                MESSENGER_EVENT_KIND,
            )
            .unwrap();
        }
        assert_eq!(count_messages(&store).unwrap(), 3, "one node per send");

        let all = list_messages(&store, MESSENGER_EVENT_KIND, None).unwrap();
        let texts: Vec<&str> = all.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["first", "second", "third"], "oldest→newest");

        // `limit` returns the most recent N, oldest dropped.
        let recent = list_messages(&store, MESSENGER_EVENT_KIND, Some(2)).unwrap();
        let recent_texts: Vec<&str> = recent.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(recent_texts, ["second", "third"]);
    }

    #[test]
    fn group_scope_round_trips_through_the_store() {
        let (_d, store) = store();
        let scope = MessengerScope::Group {
            name: "engineering".to_string(),
        };
        persist_message(
            &store,
            &MessengerEvent::new("elena@kanbrick.com", "standup", scope.clone()),
            MESSENGER_EVENT_KIND,
        )
        .unwrap();

        let all = list_messages(&store, MESSENGER_EVENT_KIND, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].scope, scope, "scope survives serialization");
    }

    #[test]
    fn an_unrelated_kind_reads_empty() {
        let (_d, store) = store();
        persist_message(
            &store,
            &public("a@kanbrick.com", "hi"),
            MESSENGER_EVENT_KIND,
        )
        .unwrap();
        assert!(list_messages(&store, "valuation.completed", None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn history_survives_reopen() {
        // A reopen is a fresh process with an empty in-memory bus: durable history
        // proves the read path survives beyond the in-memory replay window.
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            persist_message(
                &store,
                &public("a@kanbrick.com", "persisted"),
                MESSENGER_EVENT_KIND,
            )
            .unwrap();
            store.checkpoint().unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        let all = list_messages(&store, MESSENGER_EVENT_KIND, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "persisted");
    }
}
