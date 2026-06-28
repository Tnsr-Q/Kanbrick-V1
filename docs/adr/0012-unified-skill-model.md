# ADR 0012 — Unified skill model: SKILL.md ⇄ (:Skill) + (:SkillVersion)

- **Status:** Accepted
- **Date:** 2026-06-28
- **Context:** Phase 11 (Skill/Loop Ecosystem), slice **P11.1** — the first slice of
  the ecosystem. It fixes the skill **authoring format** and **persisted schema**
  that P11.2 (the grant + skill-authorization HTTP surface) and P11.3 (the loop
  run-engine) build on. Builds on the existing per-scope `Skill` primitive
  (`kanbrick-discovery` `grants.rs`), the SparrowDB write dialect (ADR-0001), and the
  host-authoritative identity invariant (ADR-0002/0007/0016).
- **Deciders:** P11 agent + **operator** (the skill model is a one-way door — it
  fixes the authoring format and graph schema for the whole ecosystem; the operator
  chose "guest-backed skill, polymorphic loop-step" this session).

## Context

Phase 10 finished the messenger + visualizer. Phase 11 builds the **skill/loop
ecosystem** (Requirement 2.3/2.5) so employees can author, publish, discover, and
run skills and compose them into loops.

Today a "skill" already exists, but narrowly: `grants.rs::Skill { id, name, scope_id,
guest, required_clearance }` is a *flat, per-scope* binding, authorized at runtime by
`ScopeGrants::authorize_skill`. There is **no authoring format**, **no versioning or
provenance**, and **no library** decoupled from a single project scope. The ecosystem
needs all three.

The load-bearing question is *what a skill is*. The operator decision (this session):
a skill is a **versioned, grant-gated wrapper over a WASM guest**; the **loop step**
— not the skill — is the polymorphic unit (a step is a guest invocation now;
provider/LLM and external MCP tool steps arrive in P11.4/P11.5). Keeping the skill
guest-backed keeps each slice clean and matches the existing primitive.

## Decision

1. **Authoring format — `SKILL.md`.** A skill is authored as a Markdown file with a
   `---`-fenced frontmatter block plus a Markdown body, extending the existing
   Claude-style skill docs (`docs/agents/skills/*.md`) with the fields a firm-OS skill
   needs. Frontmatter keys: `name`, `version`, `guest`, `clearance` (`L1`..`L5`), and
   optional `description`; the body is the human-facing instructions. It is parsed by
   `kanbrick_loops::parse_skill_md` into a `SkillManifest`.

2. **Persisted schema — two node kinds.** `(:Skill {name})` is the stable identity
   (one node per skill name). `(:SkillVersion {version_id, skill_name, version, guest,
   min_clearance, description, source, created_at, seq})` is one node per published
   edition, keyed by `version_id = "{name}@{version}"`, linked
   `(:Skill)-[:HAS_VERSION]->(:SkillVersion)`. Writes use the ADR-0001 dialect (`MERGE`
   on the key, then `MATCH … SET`), so re-publishing a version updates in place.
   `min_clearance` round-trips via `ClearanceLevel`'s `Display`/serde form (`"L3"`),
   exactly like `(:GuestPolicy)`. An append-only `seq` (node count at write) gives a
   deterministic publish order; the most recently published edition is "latest".

3. **The registry confers no access.** It is the *catalogue* of publishable, versioned
   skill definitions. `ScopeGrants` (ADR-0007) remains the **sole authorization gate**:
   P11.2 connects them — authorizing a skill resolves a registry edition **and** an
   active `ProjectScope` grant **and** the clearance floor. The registry alone grants
   nothing.

4. **Crate placement / layering.** SKILL.md parsing + the `SkillManifest` domain type
   live in the new **`kanbrick-loops`** crate, which depends only on `kanbrick-core`
   (no store/HTTP/async) — keeping it out of the SparrowDB dependency graph so the
   parser builds and tests standalone. The `(:Skill)`/`(:SkillVersion)` persistence
   lives in **`kanbrick-store`** (`skill_registry`), mirroring `guest_policy`/
   `messenger_log`. **`kanbrick-api`** composes the two (P11.2). The two layers stay
   decoupled (the store has no dependency on `kanbrick-loops`).

## Consequences

- **Versioning + provenance from day one**, decoupled from any single scope — a real
  library, not a per-project binding.
- **The parser is independently testable** (pure `kanbrick-core`), a rare local compile
  gate in a workspace whose store layer can't build without the `sparrowdb` submodule.
- **Clean layering**: domain (`kanbrick-loops`) and persistence (`kanbrick-store`) are
  independent; the HTTP layer composes them. Guest-backed skills keep the early slices
  small; the polymorphic loop step is deferred to P11.4/P11.5.
- **Re-publishing a version bumps its `seq`** (publish-recency ordering), and
  `version_id` assumes no `@` in a skill name or version (kebab names, semver versions).
- **Authorization is still owed**: the composed "registry edition + active grant +
  clearance" check is P11.2 work; this slice lands only the schema + registry, with no
  HTTP surface and no access path.
