//! Build script: compile the `guests/echo` crate to `wasm32-wasip1` so the
//! integration tests have a real WASM guest to load (issue #21, ADR-0002).
//!
//! The guest is excluded from the workspace build graph and built here with its
//! own isolated target directory (under `OUT_DIR`) so it never contends with the
//! parent workspace build lock. The resulting artifact path is exported to the
//! crate (and its tests) as `KANBRICK_ECHO_GUEST_WASM`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let guest_dir = manifest_dir.join("../guests/echo");
    let guest_manifest = guest_dir.join("Cargo.toml");

    // Rebuild the guest only when its sources change.
    println!("cargo:rerun-if-changed={}", guest_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("src/lib.rs").display()
    );

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let guest_target = out_dir.join("echo-guest-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let status = Command::new(&cargo)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "--manifest-path",
        ])
        .arg(&guest_manifest)
        .arg("--target-dir")
        .arg(&guest_target)
        // Isolate from the parent build's rustflags (`-D warnings`, encoded
        // flags) and any inherited target-dir so the guest builds
        // deterministically with its own settings.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke `{cargo}` to build the echo guest: {e}"));

    assert!(
        status.success(),
        "building guests/echo for wasm32-wasip1 failed (is the wasm32-wasip1 \
         target installed? it is pinned in rust-toolchain.toml)"
    );

    let wasm = guest_target
        .join("wasm32-wasip1")
        .join("release")
        .join("kanbrick_guest_echo.wasm");
    assert!(
        wasm.exists(),
        "echo guest built but artifact not found at {}",
        wasm.display()
    );

    expose(&wasm);
}

/// Export the artifact path to the crate and its tests.
fn expose(wasm: &Path) {
    println!(
        "cargo:rustc-env=KANBRICK_ECHO_GUEST_WASM={}",
        wasm.display()
    );
}
