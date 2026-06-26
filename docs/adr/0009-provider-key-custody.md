# ADR 0009 — Provider-key custody: IOTA Stronghold enclave, OS-keychain fallback

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** P8.2 (#94), **Phase 8 — Upstream De-Risk** (#79), L5 Cockpit
  program (#77). Builds on ADR-0014 (single runtime / bounded primitives) and
  contrasts the existing `ApiKeyService`.
- **Deciders:** P8 de-risk agent + operator (HITL — the secret-custody backend is
  a one-way door).

## Context

BYO-AI (P9) lets each employee plug in their own provider key
(Claude / OpenAI / Cerebras). To make an outbound call the host must hold a
**retrievable** secret. The existing `ApiKeyService`
(`kanbrick-auth/src/apikey.rs`) cannot serve this: it stores **hashes only**
(SHA-256 via `query_hash`, compared with `constant_time_eq`) and its
`IssuedKey { key_id, secret }` is "show once, store nowhere". It can *validate* a
presented secret and return a `FirmContext`, but it can never *return* a stored
secret — the **hash-only gap**. Phase 7 left exactly this seam: the in-memory
`auth::Session` (P7.3) was always meant to gain durable backing here (ADR-0016
names P8.2 / ADR-0009 as the follow-up).

Tachyon-Mesh ships **IOTA Stronghold**, an at-rest encrypted secret enclave. Per
ADR-0014 we may adopt it only if it is usable as a **standalone library**, without
`core-host`.

## Probe evidence

`cargo add iota_stronghold` (the crates.io **index** is reachable through the
proxy) resolved cleanly:

| Property | Result |
| --- | --- |
| Version | `iota_stronghold` v2.1.0 |
| Transitive crates | **179**, all "Rust 1.94.1 compatible" → **zero toolchain bump** (matches our pin) |
| Pulls `core-host`? | **No** |
| Heavy infra (`tokio` / `ring` / `openssl` / `wasmtime` / `aya` / `candle` / ONNX)? | **none present** |
| Crypto closure | `aes-gcm`, `chacha20poly1305`, `x25519-dalek`, `curve25519-dalek`, `blake2`, `hkdf`, `hmac`, `scrypt`, `pbkdf2`, `rust-argon2`, `k256`/`ecdsa`/`ed25519`, `iota-crypto`, `zeroize` (+`_derive`), `stronghold_engine` / `-runtime` / `-utils` / `-derive` |
| Async runtime | none (only `futures-core` / `-task` / `-util`) |
| **Build friction** | one native `-sys` dep: **`libsodium-sys-stable`** (+ `cc` / `pkg-config` / `vcpkg`) → a C toolchain / libsodium is needed at build time |

**Honest environment note:** the live store → read **round-trip could not be
executed here** — crate *tarball* downloads are proxy-blocked (HTTP 403); the
index resolves but sources do not download. The round-trip is deferred to a
network-capable machine / the cockpit CI. The dependency-closure evidence above is
what the adoption rests on: Stronghold is a **minimal-dependency standalone
enclave**, satisfying ADR-0014. Full notes:
[`docs/probes/p8.2-stronghold-spike.md`](../probes/p8.2-stronghold-spike.md).

## Decision

1. **Adopt IOTA Stronghold** as the per-employee provider-key enclave, taken as a
   standalone library (no `core-host`).
2. **Namespacing by JWT-derived `user_id`.** Each employee gets a Stronghold
   vault / snapshot keyed by their `FirmContext.user_id` (Uuid). Secrets are
   written / read **host-side only**; the **webview never sees plaintext**,
   consistent with the host-authoritative identity of ADR-0016. The unlock key is
   host-derived; a compromise of one user's unlock material cannot read another
   user's vault.
3. **`auth::Session` is the seam.** The P7.3 in-memory session is where durable
   custody slots in without changing callers — `Session` grows a Stronghold-backed
   store behind the same interface.
4. **OS keychain kept as a documented fallback.** Because Stronghold carries a
   native `-sys` dependency, environments where `libsodium` is unavailable fall
   back to the OS keychain / Tauri secure store; the `user_id`-namespacing model is
   identical.

## Alternatives considered

- **OS keychain only.** Simpler, but per-OS behaviour and a weaker cross-platform
  story; kept as the fallback, not the primary.
- **Encrypt secrets in SparrowDB.** Rejected: the graph is single-writer and
  clearance-*read*; retrievable per-user secrets do not belong in the shared graph.
- **Reuse `ApiKeyService`.** Rejected: hash-only by design — it cannot return a
  secret.

## Consequences

- A native build dependency (`libsodium-sys-stable`) enters the **Cockpit build**
  (not the firm-OS workspace); the cockpit CI runner must provide it. The keychain
  fallback covers environments that cannot.
- P9.3 (per-employee key custody) builds directly on this; it informs P11.4
  (per-step key injection).
- The store → read round-trip and the cross-user isolation test are first exercised
  on a network-capable machine / CI, per the note above.
