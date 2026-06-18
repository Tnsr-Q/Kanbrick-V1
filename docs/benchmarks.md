# Performance benchmarks (#49)

Measured by `kanbrick-mesh/tests/perf.rs` against the 12-person / 9-company seed.
Run locally with:

```bash
cargo test -p kanbrick-mesh --test perf -- --nocapture
```

The in-test assertions are deliberately **loose** (CI runners are shared/noisy —
they catch gross regressions and hangs, not microsecond drift). The numbers below
are representative of a development run; update them when intentionally changing
the hot paths.

## Representative results

| Metric | Measured | PRD target | Status |
| --- | --- | --- | --- |
| SparrowDB query latency (p99) | ~0.6 ms | < 50 ms | ✅ |
| SparrowDB query latency (p50) | ~0.5 ms | — | ✅ |
| Mesh dispatch overhead — echo, no queries (p99) | ~2.3 ms | < 5 ms | ✅ |
| Module compile + first invoke (cold start) | ~2.3 s | < 500 ms | ⚠️ see note |
| Full compliance-guest execution (16 graph queries) | ~80 ms | — | context |

## Notes

- **Query latency** and **mesh dispatch overhead** comfortably meet their targets.
  The echo guest does no graph queries, so its dispatch number isolates
  instantiate + call + teardown from guest work.
- **Cold start** is dominated by **Cranelift compilation** of the guest module
  (~2 s), not instantiation. In production this is paid **once at startup** when
  the API registers the embedded guests; per-request invocation reuses the
  compiled `Module` and is fast (the dispatch-overhead row). To bring first-load
  under 500 ms, precompile with `wasmtime`'s serialized modules
  (`Module::serialize` / `deserialize`) and embed the artifacts — a self-contained
  follow-up that needs no API/guest changes.
- **Full compliance execution** (~80 ms) is dominated by its **16 re-entrant
  `query_graph` calls** (one per person for `REPORTS_TO`, plus the others), each
  re-resolving the caller's clearance scope and writing an audit entry — it is
  guest *work*, not mesh overhead. Batching the per-person reporting lookups would
  cut it substantially.

## Not yet measured here

- 12-concurrent-user simulation and steady-state memory footprint. The
  concurrency path is exercised functionally by the API E2E suite (`spawn_blocking`
  per request); a dedicated load harness and an RSS sampler are a reasonable
  follow-up for a stable benchmarking host.
