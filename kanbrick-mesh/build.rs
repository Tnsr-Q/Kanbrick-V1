//! Build script: compile the WASM guest fixtures to `wasm32-wasip1` so the
//! integration tests have real guests to load (issue #21/#39, ADR-0002).
//!
//! Each fixture is excluded from the workspace build graph and built here with
//! its own isolated target directory (under `OUT_DIR`) so it never contends with
//! the parent workspace build lock. The artifact path is exported to the crate
//! (and its tests) as an env var.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A guest fixture to compile: (relative crate dir, wasm artifact stem, env var).
const GUESTS: &[(&str, &str, &str)] = &[
    // The raw-ABI echo fixture (#21).
    (
        "../guests/echo",
        "kanbrick_guest_echo",
        "KANBRICK_ECHO_GUEST_WASM",
    ),
    // The guest-SDK reference guest (#39).
    (
        "../guests/sdk-example",
        "kanbrick_guest_sdk_example",
        "KANBRICK_SDK_EXAMPLE_GUEST_WASM",
    ),
];

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    for (rel_dir, stem, env_var) in GUESTS {
        build_guest(&manifest_dir, &out_dir, &cargo, rel_dir, stem, env_var);
    }
}

/// Build one guest crate to `wasm32-wasip1` and export its artifact path.
fn build_guest(
    manifest_dir: &Path,
    out_dir: &Path,
    cargo: &str,
    rel_dir: &str,
    stem: &str,
    env_var: &str,
) {
    let guest_dir = manifest_dir.join(rel_dir);
    let guest_manifest = guest_dir.join("Cargo.toml");

    // Rebuild the guest only when its sources change.
    println!("cargo:rerun-if-changed={}", guest_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("src/lib.rs").display()
    );

    let guest_target = out_dir.join(format!("{stem}-target"));

    let status = Command::new(cargo)
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
        // flags) and inherited target-dir so each guest builds deterministically.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke `{cargo}` to build {rel_dir}: {e}"));

    assert!(
        status.success(),
        "building {rel_dir} for wasm32-wasip1 failed (is the wasm32-wasip1 target \
         installed? it is pinned in rust-toolchain.toml)"
    );

    let wasm = guest_target
        .join("wasm32-wasip1")
        .join("release")
        .join(format!("{stem}.wasm"));
    assert!(
        wasm.exists(),
        "{rel_dir} built but artifact not found at {}",
        wasm.display()
    );

    println!("cargo:rustc-env={env_var}={}", wasm.display());
}
