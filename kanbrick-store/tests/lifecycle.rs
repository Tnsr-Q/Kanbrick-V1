//! Issue #6 — SparrowDB embedded lifecycle: init, open, close, durability,
//! concurrent readers.

use std::sync::Arc;

use kanbrick_store::{Params, Store};

/// Opening a fresh path creates a durable, file-backed database, and a trivial
/// `RETURN 1` evaluates to `1`.
#[test]
fn open_fresh_and_return_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let one = store.scalar_i64("RETURN 1", Params::new()).unwrap();
    assert_eq!(one, Some(1));

    // The store path is backed by the filesystem location we opened.
    assert_eq!(store.path(), dir.path());
}

/// Closing and reopening the same path preserves previously written state.
#[test]
fn state_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = Store::open(dir.path()).unwrap();
        store
            .execute("CREATE (:Marker {id: 1, label: 'persisted'})")
            .unwrap();
        // Graceful close checkpoints, making the write durable.
        store.close().unwrap();
    }

    let reopened = Store::open(dir.path()).unwrap();
    let count = reopened
        .scalar_i64("MATCH (m:Marker) RETURN count(m)", Params::new())
        .unwrap();
    assert_eq!(
        count,
        Some(1),
        "marker written before close must survive reopen"
    );
}

/// Two concurrent reader threads do not deadlock.
#[test]
fn concurrent_readers_do_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    store.execute("CREATE (:Marker {id: 1})").unwrap();

    let mut handles = Vec::new();
    for _ in 0..2 {
        let s = Arc::clone(&store);
        handles.push(std::thread::spawn(move || {
            for _ in 0..50 {
                let n = s
                    .scalar_i64("MATCH (m:Marker) RETURN count(m)", Params::new())
                    .unwrap();
                assert_eq!(n, Some(1));
            }
        }));
    }
    for h in handles {
        h.join().expect("reader thread must not panic or deadlock");
    }
}
