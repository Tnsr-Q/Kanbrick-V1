# Probe — RBAC + DLP additive-only overlay (P8.4 / #96)

A **throwaway Phase-8 de-risk spike** for `docs/adr/0010-ironclaw-rbac-dlp.md`.

## What it proves

Ironclaw is a **binary** (no library target — confirmed in Phase 2 / ADR-0002,
where `kanbrick-auth` was built on Ironclaw's *primitives* rather than the crate).
So the de-risked outcome is to **port the RBAC/DLP pattern** into the firm OS as
**additive-only overlays**, not to depend on Ironclaw. This spike proves the two
invariants the overlay must hold:

1. **Roles can only restrict clearance, never elevate.** A role maps to an
   optional clearance *ceiling*; effective clearance is the base lowered by every
   applicable ceiling (`min`), so adding a role is monotonically non-increasing.
   The production overlay reads the existing `kanbrick_core::FirmContext.roles`
   (`pub roles: Vec<String>`) — **no second role store**.
2. **DLP gates (data-class → provider) egress, default-deny** — the gate used at
   the P9.6 provider boundary (ADR-0017).

`Clearance` here mirrors `kanbrick_core::ClearanceLevel` exactly (`L1 < … < L5`,
derived `Ord`), so the logic transfers verbatim onto the real enum.

## Run

```bash
cd probes/rbac-overlay
cargo test          # 6 tests: restrict-only, never-elevate, DLP default-deny, combined gate
```

Dependency-free (std only); compiles and tests in any environment. Excluded from
the root workspace (its own `[workspace]`), so it never enters the firm-OS graph.
