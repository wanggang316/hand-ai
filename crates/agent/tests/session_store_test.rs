//! Integration tests for the session storage module: the shared
//! behavioral suite (tests/common/session_suite.rs) instantiated for
//! the JSONL and in-memory backends, plus JSONL-specific on-disk
//! format, bounded-read, and corruption tests.

mod common;

use common::session_suite::{
    custom_entry, header, message_entry, model_change_entry, suite_create_duplicate_id_is_invalid,
    suite_fork_all_and_up_to, suite_missing_session_is_not_found, suite_round_trip,
    write_raw_session,
};
use hand_agent::session::{
    ContextProjection, InMemoryStore, JsonlStore, Projector, SessionHeader, SessionStore,
    SessionStoreError,
};
use model::{Message, UserMessage};
use serde_json::json;
use std::io::Write;

// ---------------------------------------------------------------------------
// Suite instantiation per backend
// ---------------------------------------------------------------------------

#[test]
fn jsonl_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    suite_round_trip(&JsonlStore::new(dir.path()));
}

#[test]
fn memory_round_trip() {
    suite_round_trip(&InMemoryStore::new());
}

#[test]
fn jsonl_create_duplicate_id_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    suite_create_duplicate_id_is_invalid(&JsonlStore::new(dir.path()));
}

#[test]
fn memory_create_duplicate_id_is_invalid() {
    suite_create_duplicate_id_is_invalid(&InMemoryStore::new());
}

#[test]
fn jsonl_missing_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    suite_missing_session_is_not_found(&JsonlStore::new(dir.path()));
}

#[test]
fn memory_missing_session_is_not_found() {
    suite_missing_session_is_not_found(&InMemoryStore::new());
}

#[test]
fn jsonl_fork_all_and_up_to() {
    let dir = tempfile::tempdir().unwrap();
    suite_fork_all_and_up_to(&JsonlStore::new(dir.path()));
}

#[test]
fn memory_fork_all_and_up_to() {
    suite_fork_all_and_up_to(&InMemoryStore::new());
}

// ---------------------------------------------------------------------------
// JSONL on-disk format compatibility
// ---------------------------------------------------------------------------

#[test]
fn jsonl_reads_hand_written_session_file() {
    let dir = tempfile::tempdir().unwrap();
    // Literal lines in the shape the hand binary writes: header
    // envelope, a message entry wrapping a model::Message, a label
    // entry.
    let content = concat!(
        r#"{"type":"session","data":{"version":3,"id":"s_test","timestamp":1721000000000,"cwd":"/tmp/x"}}"#,
        "\n",
        r#"{"type":"message","data":{"id":"e_1","message":{"role":"user","content":"hi","timestamp":1721000000001},"timestamp":1721000000001}}"#,
        "\n",
        r#"{"type":"label","data":{"id":"e_2","target_id":"s_test","label":"greeting","timestamp":1721000000002}}"#,
        "\n",
    );
    write_raw_session(dir.path(), "s_test.jsonl", content);

    let store = JsonlStore::new(dir.path());
    let (loaded_header, entries) = store.load("s_test").unwrap();
    assert_eq!(loaded_header.version, 3);
    assert_eq!(loaded_header.id, "s_test");
    assert_eq!(loaded_header.cwd, "/tmp/x");
    assert_eq!(loaded_header.parent_session, None);
    assert!(loaded_header.extra.is_empty());

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, "message");
    assert_eq!(entries[0].id(), Some("e_1"));
    assert_eq!(entries[1].kind, "label");
    assert_eq!(entries[1].payload["target_id"], json!("s_test"));

    // The wrapped message deserializes as a genuine model::Message.
    let message: Message = serde_json::from_value(entries[0].payload["message"].clone()).unwrap();
    assert!(matches!(message, Message::User(_)));
}

#[test]
fn jsonl_written_file_uses_envelope_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlStore::new(dir.path());
    store.create(&header("s_shape", 100)).unwrap();
    store
        .append("s_shape", &message_entry("e_1", 101, "hi"))
        .unwrap();

    let content = std::fs::read_to_string(dir.path().join("s_shape.jsonl")).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["type"], "session");
    assert_eq!(first["data"]["version"], 3);
    assert_eq!(first["data"]["id"], "s_shape");
    assert_eq!(first["data"]["cwd"], "/tmp/x");

    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["type"], "message");
    assert_eq!(second["data"]["id"], "e_1");
    assert_eq!(second["data"]["message"]["role"], "user");
}

#[test]
fn jsonl_header_with_unknown_field_survives_read_and_append() {
    let dir = tempfile::tempdir().unwrap();
    let content = concat!(
        r#"{"type":"session","data":{"version":3,"id":"s_future","timestamp":1721000000000,"cwd":"/tmp/x","future_field":true}}"#,
        "\n",
    );
    write_raw_session(dir.path(), "s_future.jsonl", content);

    let store = JsonlStore::new(dir.path());
    let read = store.read_header("s_future").unwrap();
    assert_eq!(read.extra["future_field"], json!(true));

    // Appending never rewrites the header line; the unknown field is
    // still there afterwards.
    store
        .append("s_future", &message_entry("e_1", 1, "hi"))
        .unwrap();
    let (loaded_header, entries) = store.load("s_future").unwrap();
    assert_eq!(loaded_header.extra["future_field"], json!(true));
    assert_eq!(entries.len(), 1);

    // And re-serializing the header elsewhere doesn't drop it either.
    let reserialized = serde_json::to_string(&read).unwrap();
    assert!(reserialized.contains("future_field"));
    let round: SessionHeader = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(round, read);
}

// ---------------------------------------------------------------------------
// Bounded header read
// ---------------------------------------------------------------------------

#[test]
fn jsonl_header_over_scan_cap_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    // A single valid header line that is bigger than the 64 KiB scan
    // cap: the bounded reader truncates it mid-line and reports
    // corruption instead of reading the whole file.
    let huge_cwd = "x".repeat(80 * 1024);
    let line = format!(
        r#"{{"type":"session","data":{{"version":3,"id":"s_huge","timestamp":1,"cwd":"{huge_cwd}"}}}}"#
    );
    write_raw_session(dir.path(), "s_huge.jsonl", &format!("{line}\n"));

    let store = JsonlStore::new(dir.path());
    let err = store.read_header("s_huge").unwrap_err();
    assert!(
        matches!(err, SessionStoreError::Corrupt { .. }),
        "got {err:?}"
    );
}

#[test]
fn jsonl_long_header_under_scan_cap_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let long_cwd = "y".repeat(40 * 1024);
    let line = format!(
        r#"{{"type":"session","data":{{"version":3,"id":"s_long","timestamp":1,"cwd":"{long_cwd}"}}}}"#
    );
    write_raw_session(dir.path(), "s_long.jsonl", &format!("{line}\n"));

    let store = JsonlStore::new(dir.path());
    let read = store.read_header("s_long").unwrap();
    assert_eq!(read.id, "s_long");
    assert_eq!(read.cwd.len(), 40 * 1024);
}

#[test]
fn jsonl_header_scan_skips_leading_blank_lines() {
    let dir = tempfile::tempdir().unwrap();
    let content = concat!(
        "\n",
        "   \n",
        r#"{"type":"session","data":{"version":3,"id":"s_blank","timestamp":1,"cwd":"/tmp/x"}}"#,
        "\n",
    );
    write_raw_session(dir.path(), "s_blank.jsonl", content);

    let store = JsonlStore::new(dir.path());
    assert_eq!(store.read_header("s_blank").unwrap().id, "s_blank");
}

#[test]
fn jsonl_non_session_first_line_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let content = concat!(
        r#"{"type":"message","data":{"id":"e_1","timestamp":1}}"#,
        "\n",
    );
    write_raw_session(dir.path(), "s_headless.jsonl", content);

    let store = JsonlStore::new(dir.path());
    assert!(matches!(
        store.read_header("s_headless").unwrap_err(),
        SessionStoreError::Corrupt { .. }
    ));
    assert!(matches!(
        store.load("s_headless").unwrap_err(),
        SessionStoreError::Corrupt { .. }
    ));
}

// ---------------------------------------------------------------------------
// Truncated tail vs mid-file corruption
// ---------------------------------------------------------------------------

#[test]
fn jsonl_truncated_tail_loads_complete_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlStore::new(dir.path());
    store.create(&header("s_trunc", 100)).unwrap();
    store
        .append("s_trunc", &message_entry("e_1", 101, "hi"))
        .unwrap();

    // Simulate a crash mid-append: a partial line with no trailing
    // newline at the very end of the file.
    let path = dir.path().join("s_trunc.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(br#"{"type":"mess"#).unwrap();
    drop(file);

    let (loaded_header, entries) = store.load("s_trunc").unwrap();
    assert_eq!(loaded_header.id, "s_trunc");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id(), Some("e_1"));
}

#[test]
fn jsonl_malformed_middle_line_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let content = concat!(
        r#"{"type":"session","data":{"version":3,"id":"s_bad","timestamp":1,"cwd":"/tmp/x"}}"#,
        "\n",
        "this is not json\n",
        r#"{"type":"message","data":{"id":"e_2","timestamp":3}}"#,
        "\n",
    );
    write_raw_session(dir.path(), "s_bad.jsonl", content);

    let store = JsonlStore::new(dir.path());
    let err = store.load("s_bad").unwrap_err();
    assert!(
        matches!(err, SessionStoreError::Corrupt { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

#[test]
fn jsonl_list_orders_newest_first_and_skips_unparseable() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlStore::new(dir.path());

    store.create(&header("s_old", 100)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    store.create(&header("s_new", 200)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    // Appending bumps the file mtime, moving s_old back to the front.
    store
        .append("s_old", &message_entry("e_1", 300, "hi"))
        .unwrap();

    // An unparseable .jsonl file in the directory is skipped, not an
    // error; unrelated extensions are ignored outright.
    write_raw_session(dir.path(), "junk.jsonl", "not a session\n");
    write_raw_session(dir.path(), "notes.txt", "ignored\n");

    let summaries = store.list().unwrap();
    assert_eq!(
        summaries
            .iter()
            .map(|s| s.header.id.as_str())
            .collect::<Vec<_>>(),
        vec!["s_old", "s_new"]
    );
    assert!(summaries[0].updated_ms >= summaries[1].updated_ms);
}

#[test]
fn jsonl_list_on_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlStore::new(dir.path().join("never-written"));
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn memory_list_orders_newest_first_by_updated_ms() {
    let store = InMemoryStore::new();
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

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

#[test]
fn projection_default_projects_messages_in_order_and_skips_others() {
    let entries = vec![
        message_entry("e_1", 1, "first"),
        model_change_entry("e_2", 2),
        custom_entry("e_3", 3),
        message_entry("e_4", 4, "second"),
    ];

    let messages = ContextProjection::new().project(&entries);
    assert_eq!(messages.len(), 2);
    let texts: Vec<String> = messages
        .iter()
        .map(|m| match m {
            // Text content serializes as a plain string.
            Message::User(u) => serde_json::to_value(&u.content)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            other => panic!("expected user message, got {other:?}"),
        })
        .collect();
    assert_eq!(texts, vec!["first", "second"]);
}

#[test]
fn projection_custom_projector_extends_per_kind_behavior() {
    let entries = vec![message_entry("e_1", 1, "hello"), custom_entry("e_2", 2)];

    let custom: Projector = Box::new(|entry| {
        vec![Message::User(UserMessage::new_text(format!(
            "custom:{}",
            entry.id().unwrap_or("?")
        )))]
    });
    let projection = ContextProjection::new().with_projector("acme_custom", custom);

    let messages = projection.project(&entries);
    assert_eq!(messages.len(), 2);
    assert!(matches!(&messages[1], Message::User(_)));
    let Message::User(user) = &messages[1] else {
        unreachable!()
    };
    assert_eq!(
        serde_json::to_value(&user.content).unwrap(),
        json!("custom:e_2")
    );
}
