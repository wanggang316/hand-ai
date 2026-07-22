//! Generic behavioral suite for [`SessionStore`] backends. Each
//! backend's integration test file instantiates these against its own
//! store so every backend proves the same contract.

use hand_agent::session::{
    SESSION_FORMAT_VERSION, SessionEntry, SessionHeader, SessionStore, SessionStoreError,
};
use model::{Message, UserMessage};
use serde_json::json;
use std::io::Write;
use std::path::Path;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub fn header(id: &str, ts: i64) -> SessionHeader {
    SessionHeader {
        version: SESSION_FORMAT_VERSION,
        id: id.to_string(),
        timestamp: ts,
        cwd: "/tmp/x".to_string(),
        parent_session: None,
        extra: serde_json::Map::new(),
    }
}

pub fn message_entry(id: &str, ts: i64, text: &str) -> SessionEntry {
    let message = serde_json::to_value(Message::User(UserMessage::new_text(text))).unwrap();
    SessionEntry::new(
        "message",
        json!({"id": id, "message": message, "timestamp": ts}),
    )
}

pub fn model_change_entry(id: &str, ts: i64) -> SessionEntry {
    SessionEntry::new(
        "model_change",
        json!({"id": id, "provider": "acme", "model_id": "acme-large", "timestamp": ts}),
    )
}

pub fn custom_entry(id: &str, ts: i64) -> SessionEntry {
    SessionEntry::new(
        "acme_custom",
        json!({
            "id": id,
            "timestamp": ts,
            "weird": {"nested": [1, 2, 3], "unicode": "héllo"},
            "flag": true,
            "no_schema_here": null
        }),
    )
}

pub fn write_raw_session(dir: &Path, name: &str, content: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let mut file = std::fs::File::create(dir.join(name)).unwrap();
    file.write_all(content.as_bytes()).unwrap();
}

// ---------------------------------------------------------------------------
// Behavioral suite
// ---------------------------------------------------------------------------

pub fn suite_round_trip(store: &dyn SessionStore) {
    store.create(&header("s_1", 100)).unwrap();
    let entries = vec![
        message_entry("e_1", 101, "hi"),
        model_change_entry("e_2", 102),
        custom_entry("e_3", 103),
    ];
    for entry in &entries {
        store.append("s_1", entry).unwrap();
    }

    let (loaded_header, loaded_entries) = store.load("s_1").unwrap();
    assert_eq!(loaded_header, header("s_1", 100));
    // Identical entries back, in append order; the unknown-kind payload
    // survives verbatim (Value equality covers every nested field).
    assert_eq!(loaded_entries, entries);

    let read = store.read_header("s_1").unwrap();
    assert_eq!(read, header("s_1", 100));
}

pub fn suite_create_duplicate_id_is_invalid(store: &dyn SessionStore) {
    store.create(&header("s_dup", 100)).unwrap();
    let err = store.create(&header("s_dup", 200)).unwrap_err();
    assert!(matches!(err, SessionStoreError::Invalid(_)), "got {err:?}");
}

pub fn suite_missing_session_is_not_found(store: &dyn SessionStore) {
    assert!(matches!(
        store.read_header("s_missing").unwrap_err(),
        SessionStoreError::NotFound(_)
    ));
    assert!(matches!(
        store.load("s_missing").unwrap_err(),
        SessionStoreError::NotFound(_)
    ));
    assert!(matches!(
        store
            .append("s_missing", &message_entry("e_1", 1, "hi"))
            .unwrap_err(),
        SessionStoreError::NotFound(_)
    ));
    assert!(matches!(
        store.fork("s_missing", "s_new", 1, None).unwrap_err(),
        SessionStoreError::NotFound(_)
    ));
}

pub fn suite_fork_all_and_up_to(store: &dyn SessionStore) {
    store.create(&header("s_src", 100)).unwrap();
    store
        .append("s_src", &message_entry("e_1", 101, "one"))
        .unwrap();
    store
        .append("s_src", &model_change_entry("e_2", 102))
        .unwrap();
    store
        .append("s_src", &message_entry("e_3", 103, "three"))
        .unwrap();

    // up_to = None copies everything, ids preserved verbatim.
    let forked = store.fork("s_src", "s_all", 500, None).unwrap();
    assert_eq!(forked.id, "s_all");
    assert_eq!(forked.timestamp, 500);
    assert_eq!(forked.cwd, "/tmp/x");
    assert_eq!(forked.parent_session.as_deref(), Some("s_src"));
    let (loaded_header, entries) = store.load("s_all").unwrap();
    assert_eq!(loaded_header, forked);
    assert_eq!(
        entries.iter().map(|e| e.id().unwrap()).collect::<Vec<_>>(),
        vec!["e_1", "e_2", "e_3"]
    );

    // up_to = Some(id) copies strictly BEFORE that entry.
    store.fork("s_src", "s_cut", 501, Some("e_2")).unwrap();
    let (_, entries) = store.load("s_cut").unwrap();
    assert_eq!(
        entries.iter().map(|e| e.id().unwrap()).collect::<Vec<_>>(),
        vec!["e_1"]
    );

    // Forking onto an existing id is Invalid.
    let err = store.fork("s_src", "s_all", 502, None).unwrap_err();
    assert!(matches!(err, SessionStoreError::Invalid(_)), "got {err:?}");

    // An up_to id that matches no entry is Invalid.
    let err = store
        .fork("s_src", "s_nope", 503, Some("e_missing"))
        .unwrap_err();
    assert!(matches!(err, SessionStoreError::Invalid(_)), "got {err:?}");
}
