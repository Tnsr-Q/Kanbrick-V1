# tests/e2e/

End-to-end tests exercising the full stack (HTTP → Auth → Mesh → Guest →
SparrowDB).

The suite lives in **`kanbrick-api/tests/e2e.rs`** (issue #47): the API is the
canonical integration surface, so the E2E tests drive the real router — logging
in representative users at every clearance tier (L1–L5) and invoking each
embedded WASM guest through `POST /guests/{name}`, asserting known-correct seed
outputs and the clearance rejections. Run with:

```bash
cargo test -p kanbrick-api --test e2e
```
