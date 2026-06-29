# ADR 0021 — Skill-publish trust gate: bind-time review + author-pinned names

- **Status:** Accepted
- **Date:** 2026-06-29
- **Context:** Phase 11 (Skill/Loop Ecosystem), slice **P11.8** — the skill-publish
  trust gate, and closing the P11.2b cross-author re-publish gap. Builds on ADR-0007
  (`ScopeGrants` dual-gate + the per-scope `Skill`), ADR-0012 (skill model), ADR-0002/
  0016 (host-authoritative identity), and the P11.2b registry⇄grant bridge
  (`kanbrick-api/src/skills.rs`). Pairs with ADR-0019/0020 (the loop step kinds that
  *run* a bound skill).
- **Deciders:** P11 agent + **operator** (HITL — a publish/trust model is a one-way
  security door; the operator chose *bind-time enforcement*, *author-pinned names*, and
  *backend + Cockpit reviewer UI* this session).

## Context

P11.2b shipped an **open** skill catalogue: any L3+ caller may `publish` a `SKILL.md`
edition, and any scope owner may `bind` any published edition onto their scope, which a
loop step then runs through `authorize_skill`. Two holes followed:

1. **No trust gate.** An authored skill is immediately invocable by *anyone* who owns a
   scope — there is no review before a skill becomes part of others' workflows.
2. **Cross-author overwrite (the P11.2b gap).** `publish_skill_version` MERGEs by
   `name@version`, re-stamping `source`/`guest`/`min_clearance` in place. A skill name
   is firm-global, so any L3+ caller could re-publish an existing name and overwrite
   another author's edition.

The substrate already has the right primitive: `ScopeGrants::eligible_grantor`
(clearance ≥ L4 **and** in the requester's management chain, or an L5 cofounder) — the
firm's existing "dual-gate lead" check. The run gate (`authorize_skill`) reads the
**bound `(:Skill)` snapshot**, not the registry, so gating at *bind* time controls
"invocable by others" without disturbing already-bound loops.

## Decision

1. **Bind-time trust gate (not publish-time).** Each `(:SkillVersion)` carries a
   `review_status` (`pending` | `approved` | `rejected`, reset to `pending` on every
   (re-)publish) plus `reviewed_by`/`reviewed_at`. **Binding** an edition onto a scope
   requires `approved` — **except** the edition's **author** (its host-stamped
   `source`) may bind/run their own skill freely (solo iteration), and an L5 cofounder
   may always bind. So an author can publish → bind → run their own skill with no
   ceremony, but no one *else* can adopt it until a lead approves. Publishing stays open
   (an author iterates freely); review governs adoption-by-others. A missing
   `review_status` (a pre-P11.8 edition) is treated as **pending** — **fail-closed**.

2. **Dual-gate lead review.** `POST /me/skill-reviews/{name}/{version}` (decision
   `approve`/`reject`) sets the edition's review state. The reviewer must clear the L4
   floor **and** be an `eligible_grantor` over the edition's **author** (in the author's
   management chain, or an L5 cofounder), and may **not** review their own skill. The
   org-graph is built fresh per decision (as the scope-grant approve path does).
   `GET /me/skill-reviews` is the L4-gated pending queue.

3. **Author-pinned names (closes the gap).** `(:Skill)` gains an `owner` — its first
   publisher. Only the owner (or an L5) may publish further editions of an existing
   name; a different author is refused at publish (`403`), so they can no longer
   overwrite another author's `source`/`guest`/`min_clearance`. A brand-new name is open
   to any L3+ (the owner is recorded on first publish, and preserved thereafter).

4. **Identity stays host-authoritative.** The author (`source`), the owner, and the
   reviewer all come from the validated `FirmContext`, never a body field. The store
   keeps `owner`/`review_status` queryable; the **enforcement** (publish pin, bind gate,
   review eligibility) lives in `kanbrick-api`. The store reads tolerate an absent/`null`
   column (→ treated as pending / no-owner) for backward compatibility.

## Consequences

- **The two holes are closed and verified.** Integration tests prove: a pending edition
  cannot be bound by a non-author (`403`), approval makes it bindable, the author may
  bind their own unreviewed skill, a different author cannot re-publish an owned name
  (`403`), the owner/an L5 can, self-review is forbidden, an L4 outside the author's
  chain cannot approve, and the queue is L4-gated and reflects decisions. Store tests
  cover owner-preservation and the review round-trip + republish-resets-to-pending.
- **Fail-closed migration.** Pre-P11.8 editions read as `pending`, so after deploy no
  one but their author (or an L5) may bind them until a lead approves — the intended
  tightening. Already-bound grants are unaffected (the run gate reads the snapshot). A
  pre-P11.8 name with no recorded `owner` is claimed by its next publisher (a documented
  migration edge; new deployments always record the owner on first publish).
- **Reviewer UI.** A Cockpit reviewer surface (pending queue + approve/reject) ships
  with this slice (ADR-0011 frontend + ADR-0016 IPC), and the library shows each
  edition's review status. The webview supplies only names/decisions; identity and the
  eligibility check stay host-side.
- **Scope.** This gate is orthogonal to clearance and the loop run gate; it governs
  *who may adopt an authored skill*, not what a bound skill may see (that remains the
  `ProjectScope`/clearance composition). Token accounting stays Phase 12.
