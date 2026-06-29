//! Loop schema persistence (P11.3, ADR-0013).
//!
//! A `(:Loop {loop_id, name, owner, created_at})` is an owned, ordered pipeline of
//! `(:LoopStep {step_id, loop_id, position, skill_name, scope_id, provider, model,
//! tool, tool_args})` nodes, linked by `(:Loop)-[:HAS_STEP]->(:LoopStep)`. Each step
//! names a *skill* and the `scope_id` it runs under; the run engine (in
//! `kanbrick-api`) compiles the ordered steps onto the mesh `Scheduler`, gating each
//! step at run time through `ScopeGrants::authorize_skill`.
//!
//! A step is **polymorphic** in *kind* (resolved by the run engine, in priority order):
//! a non-empty `tool` makes it an *MCP tool-call step* (P11.5) — an external tool runs
//! via the managed sidecar bridge; else a non-empty `provider`/`model` makes it a
//! *provider step* (P11.4) — an LLM completion runs; else it is a *guest step* — the
//! bound skill's WASM guest runs. The `provider`/`model`/`tool`/`tool_args` are stored
//! as opaque strings so this crate stays free of `kanbrick-providers` and any MCP
//! dependency; the run engine parses, gate-checks, and dispatches them.
//!
//! This module persists the loop **definition** only (the durable schema). A run's
//! per-step history is kept in-process by the run engine for now; persisting it so it
//! survives a restart is P11.5.
//!
//! Writes follow the ADR-0001 dialect: a parameterized `MERGE` on the unique key,
//! then `MATCH … SET` for the mutable fields, and the relationship `MERGE` on the
//! **non-parameterized** path (the pinned SparrowDB rejects a parameterized edge
//! MERGE — ADR-0006 / SPA-233), with the two keys inlined and single-quote-escaped —
//! exactly as [`crate::skill_registry`] writes its `HAS_VERSION` edge. Reads use the
//! bare-node projection `RETURN n.prop` (no `AS` aliases, which would yield nulls).

use kanbrick_core::Result;
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::value::Params;

/// Columns projected by the loop read query, in order. Un-aliased bare-node
/// projection per ADR-0001 (the row mapper strips the `l.` prefix).
const LOOP_PROJECTION: &str = "RETURN l.loop_id, l.name, l.owner, l.created_at";

/// Columns projected by the step read query, in order (bare-node, no aliases).
const STEP_PROJECTION: &str = "RETURN s.loop_id, s.position, s.skill_name, s.scope_id, \
     s.provider, s.model, s.tool, s.tool_args";

/// A persisted loop definition: an owned, ordered pipeline of steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRecord {
    /// Loop identity (UUID, the `(:Loop)` key).
    pub loop_id: String,
    /// Human label.
    pub name: String,
    /// The owning employee's email (host-stamped at create time).
    pub owner: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
}

/// One step of a loop: a skill bound to a scope, at an ordinal position.
///
/// A step is **polymorphic** in *kind* (resolved by the run engine, in priority
/// order): a non-empty `tool` is an *MCP tool-call step* (P11.5) — an external tool
/// runs via the managed sidecar bridge; else a non-empty `provider`/`model` is a
/// *provider step* (P11.4) — an LLM completion runs on the named provider/model; else
/// it is a *guest step* — the bound skill's WASM guest runs. Every kind is authorized
/// by the same skill+scope. The `provider`/`model`/`tool`/`tool_args` are stored as
/// opaque strings so this crate stays free of `kanbrick-providers` and any MCP
/// dependency; the run engine parses and gate-checks them. No kind ever carries a
/// credential or an identity — the host resolves the caller's key from custody (a
/// provider step) and mints a caller-bound capability (an MCP tool step) at run time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopStepRecord {
    /// The loop this step belongs to.
    pub loop_id: String,
    /// Zero-based ordinal position within the loop.
    pub position: i64,
    /// The skill (registry/grant name) this step invokes.
    pub skill_name: String,
    /// The scope the step is authorized + run under (`ScopeGrants::authorize_skill`).
    pub scope_id: String,
    /// Provider kind for a provider step (e.g. `"anthropic"`); empty otherwise.
    #[serde(default)]
    pub provider: String,
    /// Model id for a provider step (e.g. `"claude-opus-4-8"`); empty otherwise.
    #[serde(default)]
    pub model: String,
    /// External tool name for an MCP tool-call step (P11.5); empty otherwise. A
    /// non-empty `tool` selects the MCP-tool kind. Opaque to this crate.
    #[serde(default)]
    pub tool: String,
    /// Static tool arguments for an MCP tool step, as an opaque JSON string (the run
    /// engine parses it and merges the piped payload); empty when none. Never carries
    /// a credential or an identity — the host mints a caller-bound capability instead.
    #[serde(default)]
    pub tool_args: String,
}

/// A step to create — `(skill_name, scope_id)` plus an optional provider OR tool
/// selection. Position is assigned by order. Leave every optional field empty for a
/// guest step; a step is at most one kind (the run engine resolves tool > provider >
/// guest, and the create route rejects setting more than one).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoopStepSpec {
    /// The skill the step invokes (authorization + scope, always present).
    pub skill_name: String,
    /// The scope the step runs under.
    pub scope_id: String,
    /// Provider kind for a provider step; empty otherwise.
    pub provider: String,
    /// Model id for a provider step; empty otherwise.
    pub model: String,
    /// External tool name for an MCP tool-call step; empty otherwise.
    pub tool: String,
    /// Static tool arguments (opaque JSON string) for an MCP tool step; empty when none.
    pub tool_args: String,
}

/// Create a loop owned by `owner` with the given ordered steps. Returns the stored
/// [`LoopRecord`]. Each call mints a fresh `loop_id`, so loops never collide.
pub fn create_loop(
    store: &Store,
    owner: &str,
    name: &str,
    steps: &[LoopStepSpec],
) -> Result<LoopRecord> {
    let loop_id = new_id();
    let created_at = chrono::Utc::now().to_rfc3339();

    store.execute_with(
        "MERGE (l:Loop {loop_id: $loop_id})",
        Params::new().with("loop_id", loop_id.as_str()),
    )?;
    store.execute_with(
        "MATCH (l:Loop {loop_id: $loop_id}) \
         SET l.name = $name, l.owner = $owner, l.created_at = $created_at",
        Params::new()
            .with("loop_id", loop_id.as_str())
            .with("name", name)
            .with("owner", owner)
            .with("created_at", created_at.as_str()),
    )?;

    for (index, step) in steps.iter().enumerate() {
        let step_id = new_id();
        let position = index as i64;
        store.execute_with(
            "MERGE (s:LoopStep {step_id: $step_id})",
            Params::new().with("step_id", step_id.as_str()),
        )?;
        store.execute_with(
            "MATCH (s:LoopStep {step_id: $step_id}) \
             SET s.loop_id = $loop_id, s.position = $position, \
                 s.skill_name = $skill_name, s.scope_id = $scope_id, \
                 s.provider = $provider, s.model = $model, \
                 s.tool = $tool, s.tool_args = $tool_args",
            Params::new()
                .with("step_id", step_id.as_str())
                .with("loop_id", loop_id.as_str())
                .with("position", position)
                .with("skill_name", step.skill_name.as_str())
                .with("scope_id", step.scope_id.as_str())
                .with("provider", step.provider.as_str())
                .with("model", step.model.as_str())
                .with("tool", step.tool.as_str())
                .with("tool_args", step.tool_args.as_str()),
        )?;
        // Link loop → step on the non-parameterized path (edge MERGE; ids are UUIDs).
        store.execute(&format!(
            "MATCH (l:Loop {{loop_id: '{}'}}), (s:LoopStep {{step_id: '{}'}}) \
             MERGE (l)-[:HAS_STEP]->(s)",
            escape(&loop_id),
            escape(&step_id),
        ))?;
    }

    Ok(LoopRecord {
        loop_id,
        name: name.to_string(),
        owner: owner.to_string(),
        created_at,
    })
}

/// Read a loop by id.
pub fn get_loop(store: &Store, loop_id: &str) -> Result<Option<LoopRecord>> {
    let rows = store.query::<LoopRecord>(
        &format!("MATCH (l:Loop {{loop_id: $loop_id}}) {LOOP_PROJECTION}"),
        Params::new().with("loop_id", loop_id),
    )?;
    Ok(rows.into_iter().next())
}

/// All loops owned by `owner`, oldest→newest by creation time.
pub fn list_loops_for_owner(store: &Store, owner: &str) -> Result<Vec<LoopRecord>> {
    let mut rows = store.query::<LoopRecord>(
        &format!("MATCH (l:Loop {{owner: $owner}}) {LOOP_PROJECTION}"),
        Params::new().with("owner", owner),
    )?;
    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(rows)
}

/// The ordered steps of a loop, by ascending `position`.
pub fn loop_steps(store: &Store, loop_id: &str) -> Result<Vec<LoopStepRecord>> {
    let mut rows = store.query::<LoopStepRecord>(
        &format!(
            "MATCH (l:Loop {{loop_id: $loop_id}})-[:HAS_STEP]->(s:LoopStep) {STEP_PROJECTION}"
        ),
        Params::new().with("loop_id", loop_id),
    )?;
    rows.sort_by_key(|r| r.position);
    Ok(rows)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Escape a string for inline use in a Cypher single-quoted literal (the
/// relationship MERGE must use the non-parameterized path). Mirrors
/// `skill_registry::escape`.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn spec(skill: &str, scope: &str) -> LoopStepSpec {
        LoopStepSpec {
            skill_name: skill.to_string(),
            scope_id: scope.to_string(),
            ..Default::default()
        }
    }

    fn provider_spec(skill: &str, scope: &str, provider: &str, model: &str) -> LoopStepSpec {
        LoopStepSpec {
            skill_name: skill.to_string(),
            scope_id: scope.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            ..Default::default()
        }
    }

    fn tool_spec(skill: &str, scope: &str, tool: &str, tool_args: &str) -> LoopStepSpec {
        LoopStepSpec {
            skill_name: skill.to_string(),
            scope_id: scope.to_string(),
            tool: tool.to_string(),
            tool_args: tool_args.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn create_then_read_round_trips_with_ordered_steps() {
        let (_d, store) = store();
        let created = create_loop(
            &store,
            "elena@kanbrick.com",
            "nightly",
            &[spec("ingest", "scope-a"), spec("report", "scope-b")],
        )
        .unwrap();
        assert_eq!(created.owner, "elena@kanbrick.com");
        assert_eq!(created.name, "nightly");

        let read = get_loop(&store, &created.loop_id).unwrap().unwrap();
        assert_eq!(read, created);

        let steps = loop_steps(&store, &created.loop_id).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].position, 0);
        assert_eq!(steps[0].skill_name, "ingest");
        assert_eq!(steps[0].scope_id, "scope-a");
        assert_eq!(steps[1].position, 1);
        assert_eq!(steps[1].skill_name, "report");
        assert!(steps.iter().all(|s| s.loop_id == created.loop_id));
        // Guest steps carry empty provider/model and empty tool/tool_args.
        assert!(steps
            .iter()
            .all(|s| s.provider.is_empty() && s.model.is_empty()));
        assert!(steps
            .iter()
            .all(|s| s.tool.is_empty() && s.tool_args.is_empty()));
    }

    #[test]
    fn a_provider_step_round_trips_its_provider_and_model() {
        let (_d, store) = store();
        let created = create_loop(
            &store,
            "elena@kanbrick.com",
            "mixed",
            &[
                spec("ingest", "scope-a"),
                provider_spec("summarize", "scope-a", "anthropic", "claude-opus-4-8"),
            ],
        )
        .unwrap();
        let steps = loop_steps(&store, &created.loop_id).unwrap();
        assert_eq!(steps[0].provider, "", "step 0 is a guest step");
        assert_eq!(steps[1].provider, "anthropic", "step 1 is a provider step");
        assert_eq!(steps[1].model, "claude-opus-4-8");
        assert_eq!(steps[1].skill_name, "summarize");
    }

    #[test]
    fn an_mcp_tool_step_round_trips_its_tool_and_args() {
        let (_d, store) = store();
        let created = create_loop(
            &store,
            "elena@kanbrick.com",
            "mixed",
            &[
                spec("ingest", "scope-a"),
                tool_spec("fetch", "scope-a", "web.search", r#"{"query":"kanbrick"}"#),
            ],
        )
        .unwrap();
        let steps = loop_steps(&store, &created.loop_id).unwrap();
        // Step 0 is a guest step: every opaque kind-field is empty.
        assert!(steps[0].tool.is_empty() && steps[0].provider.is_empty());
        // Step 1 is an MCP tool step: tool + args round-trip, provider/model stay empty.
        assert_eq!(steps[1].tool, "web.search");
        assert_eq!(steps[1].tool_args, r#"{"query":"kanbrick"}"#);
        assert_eq!(steps[1].skill_name, "fetch");
        assert!(steps[1].provider.is_empty() && steps[1].model.is_empty());
    }

    #[test]
    fn steps_read_back_in_position_order() {
        let (_d, store) = store();
        let created = create_loop(
            &store,
            "u@kanbrick.com",
            "pipe",
            &[spec("a", "s"), spec("b", "s"), spec("c", "s")],
        )
        .unwrap();
        let steps = loop_steps(&store, &created.loop_id).unwrap();
        let names: Vec<&str> = steps.iter().map(|s| s.skill_name.as_str()).collect();
        let positions: Vec<i64> = steps.iter().map(|s| s.position).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(positions, [0, 1, 2]);
    }

    #[test]
    fn list_for_owner_filters_and_orders() {
        let (_d, store) = store();
        create_loop(&store, "elena@kanbrick.com", "one", &[spec("a", "s")]).unwrap();
        create_loop(&store, "elena@kanbrick.com", "two", &[spec("a", "s")]).unwrap();
        create_loop(&store, "peter@kanbrick.com", "other", &[spec("a", "s")]).unwrap();

        let elena = list_loops_for_owner(&store, "elena@kanbrick.com").unwrap();
        let names: Vec<&str> = elena.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["one", "two"], "only Elena's, oldest→newest");

        assert_eq!(
            list_loops_for_owner(&store, "peter@kanbrick.com")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_loop_with_no_steps_is_valid() {
        let (_d, store) = store();
        let created = create_loop(&store, "u@kanbrick.com", "empty", &[]).unwrap();
        assert!(loop_steps(&store, &created.loop_id).unwrap().is_empty());
        assert!(get_loop(&store, &created.loop_id).unwrap().is_some());
    }

    #[test]
    fn an_unknown_loop_reads_none() {
        let (_d, store) = store();
        assert!(get_loop(&store, "ghost").unwrap().is_none());
        assert!(loop_steps(&store, "ghost").unwrap().is_empty());
    }

    #[test]
    fn loops_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let loop_id;
        {
            let store = Store::open(dir.path()).unwrap();
            loop_id = create_loop(&store, "u@kanbrick.com", "durable", &[spec("a", "s")])
                .unwrap()
                .loop_id;
            store.checkpoint().unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(get_loop(&store, &loop_id).unwrap().unwrap().name, "durable");
        assert_eq!(loop_steps(&store, &loop_id).unwrap().len(), 1);
    }
}
