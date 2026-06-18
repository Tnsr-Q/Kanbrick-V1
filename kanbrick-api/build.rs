//! Build script: compile the three business guests to `wasm32-wasip1` (release)
//! so the API binary can **embed** them via `include_bytes!` (#47/#53). Each
//! guest builds in its own isolated target dir (under `OUT_DIR`), insulated from
//! the parent build's flags and any clippy/rustc wrapper, and its artifact path
//! is exported as an env var the crate reads with `include_bytes!(env!(...))`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// (relative crate dir, wasm artifact stem, env var the crate reads).
const GUESTS: &[(&str, &str, &str)] = &[
    (
        "../guests/valuation",
        "kanbrick_guest_valuation",
        "KANBRICK_VALUATION_GUEST_WASM",
    ),
    (
        "../guests/reporting",
        "kanbrick_guest_reporting",
        "KANBRICK_REPORTING_GUEST_WASM",
    ),
    (
        "../guests/compliance",
        "kanbrick_guest_compliance",
        "KANBRICK_COMPLIANCE_GUEST_WASM",
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
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
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
