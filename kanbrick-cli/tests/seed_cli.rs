//! Issue #11 — `kanbrick-cli seed` end-to-end against a temp database.

use std::process::Command;

/// Resolve a repo-root-relative path (tests run with CWD = crate dir).
fn repo_path(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(rel)
}

/// `kanbrick-cli seed` loads the firm seed into a fresh database and a second
/// run is a no-op (idempotent migrations).
#[test]
fn seed_command_loads_and_is_idempotent() {
    let bin = env!("CARGO_BIN_EXE_kanbrick-cli");
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("firm.db");
    let seed = repo_path("seed/kanbrick_seed_data.cypher");

    let run = || {
        Command::new(bin)
            .arg("seed")
            .arg("--file")
            .arg(&seed)
            .arg("--db")
            .arg(&db)
            .output()
            .expect("cli must run")
    };

    let first = run();
    assert!(
        first.status.success(),
        "first seed run must succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stdout).contains("applied migrations: [1, 2]"),
        "first run should apply both migrations"
    );

    let second = run();
    assert!(second.status.success(), "second seed run must succeed");
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("already applied"),
        "second run should be a no-op"
    );
}

/// A missing seed file produces a non-zero exit and a clear error.
#[test]
fn seed_command_missing_file_errors() {
    let bin = env!("CARGO_BIN_EXE_kanbrick-cli");
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("firm.db");

    let out = Command::new(bin)
        .arg("seed")
        .arg("--file")
        .arg("/nonexistent/seed.cypher")
        .arg("--db")
        .arg(&db)
        .output()
        .expect("cli must run");

    assert!(
        !out.status.success(),
        "missing seed file must exit non-zero"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot read seed file"),
        "error should explain the missing file"
    );
}
