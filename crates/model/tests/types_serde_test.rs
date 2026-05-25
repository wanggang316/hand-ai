use model::types::*;

#[test]
fn transport_serde_kebab_case() {
    assert_eq!(
        serde_json::to_string(&Transport::WebsocketCached).unwrap(),
        "\"websocket-cached\""
    );
    assert_eq!(
        serde_json::from_str::<Transport>("\"sse\"").unwrap(),
        Transport::Sse
    );
}

#[test]
fn cache_retention_serde_lowercase() {
    assert_eq!(
        serde_json::to_string(&CacheRetention::Long).unwrap(),
        "\"long\""
    );
}

#[test]
fn compat_anthropic_messages_roundtrip() {
    let c = Compat::AnthropicMessages(AnthropicMessagesCompat {
        supports_eager_tool_input_streaming: Some(true),
        supports_long_cache_retention: Some(false),
    });
    let s = serde_json::to_string(&c).unwrap();
    assert!(s.contains(r#""type":"anthropic-messages""#));
    assert!(s.contains(r#""supportsEagerToolInputStreaming":true"#));
    let back: Compat = serde_json::from_str(&s).unwrap();
    assert!(matches!(back, Compat::AnthropicMessages(_)));
}

#[test]
fn stream_options_callbacks_clone_arc() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let c2 = counter.clone();
    let mut opts = StreamOptions::default();
    opts.on_payload = Some(Arc::new(move |_v, _m| {
        c2.fetch_add(1, Ordering::SeqCst);
    }));
    let cloned = opts.clone();
    let p1 = opts.on_payload.as_ref().unwrap();
    let p2 = cloned.on_payload.as_ref().unwrap();
    assert!(Arc::ptr_eq(p1, p2));
    let dbg = format!("{cloned:?}");
    assert!(dbg.contains("<Fn>"));
}

#[test]
fn thinking_level_map_distinguishes_null_and_missing() {
    let m: ThinkingLevelMap = serde_json::from_str(r#"{"low":null,"high":"detailed"}"#).unwrap();
    assert_eq!(m.get("low"), Some(&None));
    assert_eq!(m.get("medium"), None);
    assert_eq!(m.get("high"), Some(&Some("detailed".to_string())));
}
