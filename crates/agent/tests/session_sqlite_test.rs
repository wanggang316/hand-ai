//! Integration tests for the SQLite session backend: the shared
//! behavioral suite (tests/common/session_suite.rs) instantiated for
//! `SqliteStore`, plus SQLite-specific import, ordering, concurrency,
//! and durability tests.

#![cfg(feature = "sqlite")]

mod common;

use common::session_suite::{
    custom_entry, header, message_entry, suite_create_duplicate_id_is_invalid,
    suite_fork_all_and_up_to, suite_missing_session_is_not_found, suite_round_trip,
    write_raw_session,
};
use hand_agent::session::{JsonlStore, SessionStore, SessionStoreError, SqliteStore};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Shared behavioral suite
// ---------------------------------------------------------------------------

#[test]
fn sqlite_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    suite_round_trip(&SqliteStore::open(dir.path().join("sessions.db")).unwrap());
}

#[test]
fn sqlite_create_duplicate_id_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    suite_create_duplicate_id_is_invalid(
        &SqliteStore::open(dir.path().join("sessions.db")).unwrap(),
    );
}

#[test]
fn sqlite_missing_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    suite_missing_session_is_not_found(&SqliteStore::open(dir.path().join("sessions.db")).unwrap());
}

#[test]
fn sqlite_fork_all_and_up_to() {
    let dir = tempfile::tempdir().unwrap();
    suite_fork_all_and_up_to(&SqliteStore::open(dir.path().join("sessions.db")).unwrap());
}

// ---------------------------------------------------------------------------
// JSONL import
// ---------------------------------------------------------------------------

#[test]
fn sqlite_import_on_open_imports_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let jsonl_dir = dir.path().join("sessions");
    let db_path = dir.path().join("sessions.db");

    let jsonl = JsonlStore::new(&jsonl_dir);
    jsonl.create(&header("s_a", 100)).unwrap();
    // Hold the fixture entries: message_entry stamps a wall-clock
    // timestamp inside the wrapped message, so rebuilt fixtures would
    // not compare equal.
    let entries = vec![message_entry("e_1", 101, "hi"), custom_entry("e_2", 102)];
    for entry in &entries {
        jsonl.append("s_a", entry).unwrap();
    }
    jsonl.create(&header("s_b", 200)).unwrap();

    let store = SqliteStore::open_with_import(&db_path, &jsonl_dir).unwrap();
    let (imported_header, imported_entries) = store.load("s_a").unwrap();
    assert_eq!(imported_header, header("s_a", 100));
    assert_eq!(imported_entries, entries);
    assert!(store.load("s_b").unwrap().1.is_empty());
    assert_eq!(store.list().unwrap().len(), 2);
    drop(store);

    // Second import is a no-op: nothing re-imported, nothing duplicated.
    let store = SqliteStore::open_with_import(&db_path, &jsonl_dir).unwrap();
    assert_eq!(store.load("s_a").unwrap().1, entries);
    assert_eq!(store.list().unwrap().len(), 2);
    drop(store);

    // A jsonl session added later is picked up by a subsequent import.
    jsonl.create(&header("s_c", 300)).unwrap();
    let store = SqliteStore::open_with_import(&db_path, &jsonl_dir).unwrap();
    assert_eq!(store.list().unwrap().len(), 3);
    assert_eq!(store.read_header("s_c").unwrap(), header("s_c", 300));
}

#[test]
fn sqlite_import_skips_corrupt_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let jsonl_dir = dir.path().join("sessions");
    let db_path = dir.path().join("sessions.db");

    let jsonl = JsonlStore::new(&jsonl_dir);
    jsonl.create(&header("s_good", 100)).unwrap();

    // First line is not a session envelope: skipped by the header scan.
    write_raw_session(&jsonl_dir, "s_noheader.jsonl", "not a session\n");
    // Valid header but malformed middle line: load is Corrupt, skipped.
    write_raw_session(
        &jsonl_dir,
        "s_midbad.jsonl",
        concat!(
            r#"{"type":"session","data":{"version":3,"id":"s_midbad","timestamp":1,"cwd":"/tmp/x"}}"#,
            "\n",
            "garbage\n",
            r#"{"type":"message","data":{"id":"e_2","timestamp":3}}"#,
            "\n",
        ),
    );
    // Truncated tail is within load tolerance: imported up to the tear.
    write_raw_session(
        &jsonl_dir,
        "s_tail.jsonl",
        concat!(
            r#"{"type":"session","data":{"version":3,"id":"s_tail","timestamp":1,"cwd":"/tmp/x"}}"#,
            "\n",
            r#"{"type":"label","data":{"id":"e_1","timestamp":2}}"#,
            "\n",
            r#"{"type":"mess"#,
        ),
    );

    let store = SqliteStore::open_with_import(&db_path, &jsonl_dir).unwrap();
    let ids: Vec<String> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|s| s.header.id)
        .collect();
    assert!(ids.contains(&"s_good".to_string()));
    assert!(ids.contains(&"s_tail".to_string()));
    assert_eq!(ids.len(), 2);
    assert_eq!(store.load("s_tail").unwrap().1.len(), 1);
    assert!(matches!(
        store.load("s_midbad").unwrap_err(),
        SessionStoreError::NotFound(_)
    ));
}

#[test]
fn sqlite_import_leaves_jsonl_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let jsonl_dir = dir.path().join("sessions");
    let db_path = dir.path().join("sessions.db");

    let jsonl = JsonlStore::new(&jsonl_dir);
    jsonl.create(&header("s_a", 100)).unwrap();
    jsonl
        .append("s_a", &message_entry("e_1", 101, "hi"))
        .unwrap();
    write_raw_session(&jsonl_dir, "junk.jsonl", "not a session\n");

    let snapshot: Vec<(String, Vec<u8>)> = {
        let mut files: Vec<_> = std::fs::read_dir(&jsonl_dir)
            .unwrap()
            .map(|d| d.unwrap().path())
            .collect();
        files.sort();
        files
            .iter()
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(p).unwrap(),
                )
            })
            .collect()
    };

    let store = SqliteStore::open_with_import(&db_path, &jsonl_dir).unwrap();
    assert_eq!(store.list().unwrap().len(), 1);

    // Byte-for-byte identical source directory after the import.
    for (name, bytes_before) in &snapshot {
        let bytes_after = std::fs::read(jsonl_dir.join(name)).unwrap();
        assert_eq!(&bytes_after, bytes_before, "file {name} was modified");
    }
    assert_eq!(
        std::fs::read_dir(&jsonl_dir).unwrap().count(),
        snapshot.len(),
        "files were added or removed"
    );
}

// ---------------------------------------------------------------------------
// Ordering, concurrency, durability
// ---------------------------------------------------------------------------

#[test]
fn sqlite_list_orders_newest_first_by_updated_ms() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("sessions.db")).unwrap();
    store.create(&header("s_a", 100)).unwrap();
    store.create(&header("s_b", 200)).unwrap();
    // Appending an entry with a later timestamp moves s_a to the front.
    store
        .append("s_a", &message_entry("e_1", 300, "hi"))
        .unwrap();

    let summaries = store.list().unwrap();
    assert_eq!(
        summaries
            .iter()
            .map(|s| s.header.id.as_str())
            .collect::<Vec<_>>(),
        vec!["s_a", "s_b"]
    );
    assert_eq!(summaries[0].updated_ms, 300);
    assert_eq!(summaries[1].updated_ms, 200);
}

#[test]
fn sqlite_concurrent_appends_to_two_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("sessions.db")).unwrap());
    store.create(&header("s_t1", 1)).unwrap();
    store.create(&header("s_t2", 1)).unwrap();

    let handles = ["s_t1", "s_t2"].map(|session_id| {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            for i in 0..50 {
                store
                    .append(session_id, &message_entry(&format!("e_{i}"), i, "x"))
                    .unwrap();
            }
        })
    });
    for handle in handles {
        handle.join().unwrap();
    }

    for session_id in ["s_t1", "s_t2"] {
        let (_, entries) = store.load(session_id).unwrap();
        assert_eq!(entries.len(), 50);
        // Append order survives interleaving with the other thread.
        let ids: Vec<String> = entries
            .iter()
            .map(|e| e.id().unwrap().to_string())
            .collect();
        let expected: Vec<String> = (0..50).map(|i| format!("e_{i}")).collect();
        assert_eq!(ids, expected);
    }
}

#[test]
fn sqlite_reopen_after_drop_sees_persisted_data() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("sessions.db");
    let entry = message_entry("e_1", 101, "hi");

    {
        let store = SqliteStore::open(&db_path).unwrap();
        store.create(&header("s_1", 100)).unwrap();
        store.append("s_1", &entry).unwrap();
    }

    let store = SqliteStore::open(&db_path).unwrap();
    let (loaded_header, entries) = store.load("s_1").unwrap();
    assert_eq!(loaded_header, header("s_1", 100));
    assert_eq!(entries, vec![entry]);
}
