//! Proxy transport for routing LLM calls through a server that holds provider
//! credentials.
//!
//! The proxy forwards model requests to an HTTP endpoint that performs the
//! actual provider authentication, and streams events back to the client. As
//! part of that streaming, it strips the `partial` field from delta events so
//! that downstream consumers see a normalized event shape.
//!
//! Mirrors the TypeScript implementation at
//! `pi-mono/packages/agent/src/proxy.ts`.
//!
//! Scaffold-only: types and functions land in subsequent tasks of the
//! agent-proxy port (see `docs/exec-plans/agent-proxy-port.md`).

use serde::{Deserialize, Serialize};

/// Wire-format event from a proxy server. The proxy strips the `partial`
/// field from `AssistantMessageEvent` to save bandwidth; the client
/// reconstructs `partial` locally (see `process_proxy_event` in T4).
///
/// Mirrors `ProxyAssistantMessageEvent` from
/// `pi-mono/packages/agent/src/proxy.ts:36-57`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyAssistantMessageEvent {
    Start {},
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
    },
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
    },
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    ToolcallStart {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    ToolcallDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
    },
    ToolcallEnd {
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    Done {
        reason: model::StopReason,
        usage: model::Usage,
    },
    Error {
        reason: model::StopReason,
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        usage: model::Usage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `model::Usage` does not derive `PartialEq`, so we cannot derive
    /// `PartialEq` on `ProxyAssistantMessageEvent` without modifying the
    /// `model` crate. Instead, each round-trip test compares the JSON
    /// produced by serializing the original value with the JSON produced by
    /// re-serializing the deserialized value. If the wire format is stable
    /// under round-trip, those JSON strings must be identical.
    fn assert_round_trip(value: &ProxyAssistantMessageEvent) -> String {
        let json = serde_json::to_string(value).expect("serialize");
        let parsed: ProxyAssistantMessageEvent =
            serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string(&parsed).expect("re-serialize");
        assert_eq!(json, json2, "round-trip JSON mismatch");
        json
    }

    fn sample_usage() -> model::Usage {
        model::Usage {
            input: 10,
            output: 20,
            cache_read: 1,
            cache_write: 2,
            total_tokens: 33,
            cost: model::UsageCost::default(),
        }
    }

    #[test]
    fn round_trip_start() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Start {});
        assert!(json.contains(r#""type":"start""#), "json={json}");
    }

    #[test]
    fn round_trip_text_start() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextStart { content_index: 0 });
        assert!(json.contains(r#""type":"text_start""#), "json={json}");
        assert!(json.contains(r#""contentIndex":0"#), "json={json}");
    }

    #[test]
    fn round_trip_text_delta() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextDelta {
            content_index: 1,
            delta: "hello".into(),
        });
        assert!(json.contains(r#""type":"text_delta""#), "json={json}");
        assert!(json.contains(r#""contentIndex":1"#), "json={json}");
        assert!(json.contains(r#""delta":"hello""#), "json={json}");
    }

    #[test]
    fn round_trip_text_end() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextEnd {
            content_index: 2,
            content_signature: Some("sig".into()),
        });
        assert!(json.contains(r#""type":"text_end""#), "json={json}");
        assert!(json.contains(r#""contentIndex":2"#), "json={json}");
        assert!(json.contains(r#""contentSignature":"sig""#), "json={json}");

        // None should be omitted.
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextEnd {
            content_index: 2,
            content_signature: None,
        });
        assert!(!json.contains("contentSignature"), "json={json}");
    }

    #[test]
    fn round_trip_thinking_start() {
        let json =
            assert_round_trip(&ProxyAssistantMessageEvent::ThinkingStart { content_index: 3 });
        assert!(json.contains(r#""type":"thinking_start""#), "json={json}");
        assert!(json.contains(r#""contentIndex":3"#), "json={json}");
    }

    #[test]
    fn round_trip_thinking_delta() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ThinkingDelta {
            content_index: 4,
            delta: "thinking...".into(),
        });
        assert!(json.contains(r#""type":"thinking_delta""#), "json={json}");
        assert!(json.contains(r#""contentIndex":4"#), "json={json}");
        assert!(json.contains(r#""delta":"thinking...""#), "json={json}");
    }

    #[test]
    fn round_trip_thinking_end() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 5,
            content_signature: Some("tsig".into()),
        });
        assert!(json.contains(r#""type":"thinking_end""#), "json={json}");
        assert!(json.contains(r#""contentIndex":5"#), "json={json}");
        assert!(json.contains(r#""contentSignature":"tsig""#), "json={json}");

        let json = assert_round_trip(&ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 5,
            content_signature: None,
        });
        assert!(!json.contains("contentSignature"), "json={json}");
    }

    #[test]
    fn round_trip_toolcall_start() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 6,
            id: "call_1".into(),
            tool_name: "search".into(),
        });
        assert!(json.contains(r#""type":"toolcall_start""#), "json={json}");
        assert!(json.contains(r#""contentIndex":6"#), "json={json}");
        assert!(json.contains(r#""id":"call_1""#), "json={json}");
        assert!(json.contains(r#""toolName":"search""#), "json={json}");
    }

    #[test]
    fn round_trip_toolcall_delta() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 7,
            delta: "{\"q\":".into(),
        });
        assert!(json.contains(r#""type":"toolcall_delta""#), "json={json}");
        assert!(json.contains(r#""contentIndex":7"#), "json={json}");
        assert!(json.contains(r#""delta":"#), "json={json}");
    }

    #[test]
    fn round_trip_toolcall_end() {
        let json =
            assert_round_trip(&ProxyAssistantMessageEvent::ToolcallEnd { content_index: 8 });
        assert!(json.contains(r#""type":"toolcall_end""#), "json={json}");
        assert!(json.contains(r#""contentIndex":8"#), "json={json}");
    }

    #[test]
    fn round_trip_done() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Done {
            reason: model::StopReason::Stop,
            usage: sample_usage(),
        });
        assert!(json.contains(r#""type":"done""#), "json={json}");
        assert!(json.contains(r#""reason":"stop""#), "json={json}");
        assert!(json.contains(r#""usage":"#), "json={json}");
    }

    #[test]
    fn round_trip_error() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Error {
            reason: model::StopReason::Error,
            error_message: Some("boom".into()),
            usage: sample_usage(),
        });
        assert!(json.contains(r#""type":"error""#), "json={json}");
        assert!(json.contains(r#""reason":"error""#), "json={json}");
        assert!(json.contains(r#""errorMessage":"boom""#), "json={json}");
        assert!(json.contains(r#""usage":"#), "json={json}");

        // Missing errorMessage should be omitted on serialization.
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Error {
            reason: model::StopReason::Error,
            error_message: None,
            usage: sample_usage(),
        });
        assert!(!json.contains("errorMessage"), "json={json}");
    }

    #[test]
    fn rejects_unknown_field() {
        let result = serde_json::from_str::<ProxyAssistantMessageEvent>(
            r#"{"type":"start","extra":"x"}"#,
        );
        assert!(
            result.is_err(),
            "deny_unknown_fields should reject extra fields, got: {result:?}"
        );
    }

    #[test]
    fn rejects_unknown_field_struct_variant() {
        let result = serde_json::from_str::<ProxyAssistantMessageEvent>(
            r#"{"type":"text_start","contentIndex":0,"extra":"x"}"#,
        );
        assert!(
            result.is_err(),
            "deny_unknown_fields should reject extra fields on struct variant, got: {result:?}"
        );
    }
}
