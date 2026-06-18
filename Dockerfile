# Multi-stage build → a minimal runtime image for the Kanbrick-V1 API (#53).
#
# Stage 1 builds the release binaries (the API embeds the three WASM guests via
# its build.rs, so the runtime image needs no separate guest files). Stage 2 is a
# slim Debian runtime with just the binaries + seed data, running as non-root.
#
# Note: the `crates/sparrowdb` submodule must be checked out in the build context
# (kanbrick-store path-depends on it):
#     git submodule update --init --depth 1 crates/sparrowdb

# ── Stage 1: builder ──────────────────────────────────────────────────────────
FROM rust:bookworm AS builder
WORKDIR /build

# The toolchain (incl. the wasm32-wasip1 target the guest build.rs needs) is
# pinned by rust-toolchain.toml and auto-installed by rustup on first build.
COPY . .
RUN cargo build --release --bin kanbrick-api --bin kanbrick-cli

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --create-home --home-dir /var/lib/kanbrick kanbrick

COPY --from=builder /build/target/release/kanbrick-api /usr/local/bin/kanbrick-api
COPY --from=builder /build/target/release/kanbrick-cli /usr/local/bin/kanbrick-cli
COPY --from=builder /build/seed /opt/kanbrick/seed

USER kanbrick
WORKDIR /var/lib/kanbrick
EXPOSE 8080

# Seed once, then run:
#   docker run --rm -it kanbrick-api \
#     sh -c "kanbrick-cli seed --file /opt/kanbrick/seed/kanbrick_seed_data.cypher \
#            --db /var/lib/kanbrick/firm.db && kanbrick-api --db /var/lib/kanbrick/firm.db"
ENTRYPOINT ["/usr/local/bin/kanbrick-api"]
CMD ["--port", "8080", "--db", "/var/lib/kanbrick/firm.db"]
