//! RPC wire protocol types.
//!
//! # Wire format
//!
//! - One JSON object per line, **LF terminator only** (the framing codec
//!   lives outside this module).
//! - Field names are **camelCase** on the wire. Every struct/enum that
//!   crosses the boundary uses `#[serde(rename_all = "camelCase")]`.
//! - Commands carry a discriminator on the `type` field (snake_case
//!   variant tags, e.g. `"prompt"`, `"new_session"`).
//! - Optional `id` correlates request and response; absent when the
//!   request omitted it. We `skip_serializing_if = "Option::is_none"`
//!   so a `None` never produces a wire field.
//!
//! # Why two response models?
//!
//! The TS source models responses as a discriminated union with **two**
//! discriminators (`type: "response"` plus `command: <name>`) and a
//! third boolean (`success`) that switches the payload between
//! `{success: true, data?}` and `{success: false, error}`.
//!
//! Serde supports a single tag at a time, so this module splits the
//! envelope from the body:
//!
//! - [`RpcResponse`] is a struct that owns the outer envelope: optional
//!   `id`, the constant `type: "response"` (encoded by [`ResponseTag`]),
//!   and a flattened [`RpcResponseBody`].
//! - [`RpcResponseBody`] is `#[serde(tag = "command", rename_all =
//!   "snake_case")]`, so each variant emits its own `command` value.
//! - Each variant carries either [`RpcResultEmpty`] (success has no
//!   `data`) or [`RpcResultWithData<T>`] (success carries a typed `data`
//!   payload). Both are `#[serde(untagged)]` enums whose Success arm
//!   carries `success: true` and whose Failure arm carries `success:
//!   false, error: String`. Round-trip is byte-identical with the TS
//!   shape.
//!
//! Commands and extension UI events use the simpler single-tag form
//! (`#[serde(tag = "type")]` / `tag = "method"`).
//!
//! # Type stubs
//!
//! Several payloads reference structures that are not yet ported from
//! TS (`AgentMessage`, `CompactionResult`, `SessionStats`,
//! `Model<any>`, etc.). Those slots are typed as `serde_json::Value`
//! placeholders and tagged with `// TODO:` markers. The protocol
//! envelope still round-trips losslessly; the inner shape is a
//! follow-up to tighten on a per-payload basis.

use model::types::{ImageContent, ThinkingLevel};
use serde::{Deserialize, Serialize};

// =============================================================================
// Constant string discriminators
// =============================================================================

/// Marker for the `"type": "response"` literal on every response envelope.
///
/// Modeled as a single-variant enum so that serde emits the exact string
/// `"response"` and rejects anything else on deserialization.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum ResponseTag {
    #[default]
    #[serde(rename = "response")]
    Response,
}

// =============================================================================
// Shared sub-types
// =============================================================================

/// Behavior when issuing a [`RpcCommand::Prompt`] while the agent is
/// already streaming: enqueue as a steer or as a follow-up.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

/// Queue delivery mode for steer / follow-up messages.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

/// Severity tag for [`RpcExtensionUiRequest::Notify`] popups.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyType {
    Info,
    Warning,
    Error,
}

/// Placement for [`RpcExtensionUiRequest::SetWidget`] surfaces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WidgetPlacement {
    AboveEditor,
    BelowEditor,
}

/// Source of a slash command listed in
/// [`RpcResponseBody::GetCommands`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlashCommandSource {
    Builtin,
    Extension,
    Prompt,
    Skill,
}

/// Slash command record returned by `get_commands`.
///
/// `sourceInfo` mirrors the TS `SourceInfo` struct, which is not yet
/// ported into Rust; it is stubbed as `serde_json::Value` so the
/// envelope round-trips losslessly. TODO: type when source-info port lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: SlashCommandSource,
    /// TODO: typed in the source-info port (currently opaque JSON).
    pub source_info: serde_json::Value,
}

// =============================================================================
// RPC commands (stdin)
// =============================================================================

/// Commands sent from the client to the agent on stdin.
///
/// Variants mirror the TS `RpcCommand` discriminated union one-for-one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcCommand {
    // ---- Prompting ----
    Prompt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    FollowUp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    Abort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    NewSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
    },

    // ---- State ----
    GetState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ---- Model ----
    SetModel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        provider: String,
        model_id: String,
    },
    CycleModel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetAvailableModels {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ---- Thinking ----
    SetThinkingLevel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        level: ThinkingLevel,
    },
    CycleThinkingLevel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ---- Queue modes ----
    SetSteeringMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: QueueMode,
    },
    SetFollowUpMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: QueueMode,
    },

    // ---- Compaction ----
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },

    // ---- Retry ----
    SetAutoRetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },
    AbortRetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ---- Bash ----
    Bash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        command: String,
    },
    AbortBash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ---- Session ----
    GetSessionStats {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    ExportHtml {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
    },
    SwitchSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        session_path: String,
    },
    Fork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        entry_id: String,
    },
    Clone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetForkMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetLastAssistantText {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetSessionName {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
    },

    // ---- Messages ----
    GetMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ---- Commands ----
    GetCommands {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

// =============================================================================
// RPC session state
// =============================================================================

/// Snapshot returned by [`RpcCommand::GetState`].
///
/// `model` is typed as `serde_json::Value` (rather than `model::Model`)
/// to keep the envelope `PartialEq`-friendly and to defer the
/// `Model<any>` shape question; the model crate's `Model` struct
/// already serializes with the right wire shape so handlers can produce
/// it via `serde_json::to_value(...)`.
/// TODO: tighten to `Option<model::types::Model>` once `Model` derives
/// `PartialEq` (or once we drop the test-side `PartialEq` requirement).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<serde_json::Value>,
    pub thinking_level: ThinkingLevel,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: u64,
    pub pending_message_count: u64,
}

// =============================================================================
// Response result helpers
// =============================================================================

/// `{success: true}` or `{success: false, error}` with no `data` field.
///
/// Untagged so that the discriminator is the *presence* of `error`.
/// Round-trips byte-identical with the TS shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcResultEmpty {
    // Failure must come first: untagged deserialization tries variants
    // in order, and `Success { success }` would otherwise greedily
    // accept any object containing `success` (including ones with
    // `error`), dropping the error field on round-trip.
    Failure {
        /// Always `false` on this arm.
        success: bool,
        error: String,
    },
    Success {
        /// Always `true` on this arm; emitter helpers enforce that.
        success: bool,
    },
}

impl RpcResultEmpty {
    pub fn ok() -> Self {
        RpcResultEmpty::Success { success: true }
    }

    pub fn err(message: impl Into<String>) -> Self {
        RpcResultEmpty::Failure {
            success: false,
            error: message.into(),
        }
    }
}

/// `{success: true, data: T}` or `{success: false, error}`.
///
/// Untagged: deserialization picks `Success` when `data` is present,
/// `Failure` when `error` is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcResultWithData<T> {
    // Failure first for the same reason as `RpcResultEmpty`: order
    // matters under `#[serde(untagged)]`.
    Failure { success: bool, error: String },
    Success { success: bool, data: T },
}

impl<T> RpcResultWithData<T> {
    pub fn ok(data: T) -> Self {
        RpcResultWithData::Success {
            success: true,
            data,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        RpcResultWithData::Failure {
            success: false,
            error: message.into(),
        }
    }
}

// =============================================================================
// Per-command response data payloads
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionData {
    pub cancelled: bool,
}

/// Data for `cycle_model`. `model` is opaque JSON pending the
/// `Model<any>` port; see [`RpcSessionState::model`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleModelData {
    /// TODO: `model::types::Model` once PartialEq lands upstream.
    pub model: serde_json::Value,
    pub thinking_level: ThinkingLevel,
    pub is_scoped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModelsData {
    /// TODO: `Vec<model::types::Model>` once PartialEq lands upstream.
    pub models: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleThinkingLevelData {
    pub level: ThinkingLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHtmlData {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSessionData {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkData {
    pub text: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneData {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessageEntry {
    pub entry_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessagesData {
    pub messages: Vec<ForkMessageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastAssistantTextData {
    pub text: Option<String>,
}

/// Data for `get_messages`. `messages` is opaque JSON pending the
/// `AgentMessage` port from `pi-agent-core`. TODO: typed in the agent
/// port phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesData {
    pub messages: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandsData {
    pub commands: Vec<RpcSlashCommand>,
}

/// Data for the `bash` response.
///
/// Wire shape diverges intentionally from the TS `BashResult` interface
/// (`{ output, exitCode, cancelled, truncated, fullOutputPath }`): we
/// expose `stdout` / `stderr` separately so a future executor port can
/// split the streams without another wire break. Today the executor
/// returns a single combined `output` buffer, which is mapped onto
/// `stdout` with `stderr` left empty. When the call was aborted via
/// `abort_bash`, `stdout` is empty and `stderr` carries the
/// `"[bash aborted]"` marker instead, with `truncated == true` and
/// `exit_code == None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashRpcData {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

// =============================================================================
// RPC response body
// =============================================================================

/// Body of an [`RpcResponse`], discriminated by the `command` field.
///
/// Each variant carries either a [`RpcResultEmpty`] (success has no
/// `data`) or a [`RpcResultWithData<T>`] (typed `data` payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum RpcResponseBody {
    Prompt(RpcResultEmpty),
    Steer(RpcResultEmpty),
    FollowUp(RpcResultEmpty),
    Abort(RpcResultEmpty),
    NewSession(RpcResultWithData<NewSessionData>),

    GetState(RpcResultWithData<RpcSessionState>),

    /// `Model<any>` data is opaque JSON; see notes on
    /// [`RpcSessionState::model`].
    SetModel(RpcResultWithData<serde_json::Value>),
    /// `data` is `null` when no model is selected. Modeled as
    /// `Option<CycleModelData>` so JSON `null` round-trips.
    CycleModel(RpcResultWithData<Option<CycleModelData>>),
    GetAvailableModels(RpcResultWithData<AvailableModelsData>),

    SetThinkingLevel(RpcResultEmpty),
    CycleThinkingLevel(RpcResultWithData<Option<CycleThinkingLevelData>>),

    SetSteeringMode(RpcResultEmpty),
    SetFollowUpMode(RpcResultEmpty),

    /// TODO: typed `CompactionResult` once compaction port lands.
    Compact(RpcResultWithData<serde_json::Value>),
    SetAutoCompaction(RpcResultEmpty),

    SetAutoRetry(RpcResultEmpty),
    AbortRetry(RpcResultEmpty),

    Bash(RpcResultWithData<BashRpcData>),
    AbortBash(RpcResultEmpty),

    /// TODO: typed `SessionStats` once session-stats port lands.
    GetSessionStats(RpcResultWithData<serde_json::Value>),
    ExportHtml(RpcResultWithData<ExportHtmlData>),
    SwitchSession(RpcResultWithData<SwitchSessionData>),
    Fork(RpcResultWithData<ForkData>),
    Clone(RpcResultWithData<CloneData>),
    GetForkMessages(RpcResultWithData<ForkMessagesData>),
    GetLastAssistantText(RpcResultWithData<LastAssistantTextData>),
    SetSessionName(RpcResultEmpty),

    GetMessages(RpcResultWithData<MessagesData>),

    GetCommands(RpcResultWithData<CommandsData>),

    /// Used when the dispatcher cannot determine the command kind (e.g.
    /// JSON parse failure or non-UTF-8 frame). Distinct from `prompt` so
    /// clients don't mistake a parse error for a prompt failure.
    #[serde(rename = "invalid")]
    Invalid(RpcResultEmpty),
}

// =============================================================================
// RPC response envelope
// =============================================================================

/// Envelope for every response emitted on stdout.
///
/// Wire shape: `{id?, type: "response", command: <name>, success,
/// data?, error?}`. The `command` discriminator lives inside
/// [`RpcResponseBody`] and is flattened into this struct so that the
/// outer JSON object has the canonical TS shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub response_type: ResponseTag,
    #[serde(flatten)]
    pub body: RpcResponseBody,
}

impl RpcResponse {
    pub fn new(id: Option<String>, body: RpcResponseBody) -> Self {
        Self {
            id,
            response_type: ResponseTag::Response,
            body,
        }
    }
}

// =============================================================================
// Extension UI events (stdout request, stdin response)
// =============================================================================

/// Request from an extension to the host UI. Discriminated by the
/// `method` field nested inside `type: "extension_ui_request"`.
///
/// The outer `type` is fixed by [`RpcExtensionUiRequest`] always
/// serializing under that tag (see the manual `Serialize`/`Deserialize`
/// pair via the wrapper struct).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RpcExtensionUiRequestKind {
    Select {
        title: String,
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Confirm {
        title: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Input {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Editor {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
    },
    Notify {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify_type: Option<NotifyType>,
    },
    SetStatus {
        status_key: String,
        /// Mirrors the TS `string | undefined` shape. Sending `None`
        /// emits a JSON `null` (matching the TS source which writes the
        /// raw value, not `skip`); deserialization accepts both `null`
        /// and missing for safety.
        status_text: Option<String>,
    },
    SetWidget {
        widget_key: String,
        /// `null` clears the widget; matches TS `string[] | undefined`.
        widget_lines: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        widget_placement: Option<WidgetPlacement>,
    },
    SetTitle {
        title: String,
    },
    #[serde(rename = "set_editor_text")]
    SetEditorText {
        text: String,
    },
}

/// Wire envelope: `{type: "extension_ui_request", id, method, ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcExtensionUiRequest {
    #[serde(rename = "type")]
    pub envelope_type: ExtensionUiRequestTag,
    pub id: String,
    #[serde(flatten)]
    pub kind: RpcExtensionUiRequestKind,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum ExtensionUiRequestTag {
    #[default]
    #[serde(rename = "extension_ui_request")]
    ExtensionUiRequest,
}

/// Response from the host UI back to the extension.
///
/// The TS union picks a variant by which payload field is present
/// (`value`, `confirmed`, or `cancelled`). Modeled here as
/// `#[serde(untagged)]` over the three shapes; round-trip is exact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcExtensionUiResponseBody {
    Value {
        value: String,
    },
    Confirmed {
        confirmed: bool,
    },
    Cancelled {
        /// Always `true` on the wire when present.
        cancelled: bool,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum ExtensionUiResponseTag {
    #[default]
    #[serde(rename = "extension_ui_response")]
    ExtensionUiResponse,
}

/// Wire envelope: `{type: "extension_ui_response", id, ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcExtensionUiResponse {
    #[serde(rename = "type")]
    pub envelope_type: ExtensionUiResponseTag,
    pub id: String,
    #[serde(flatten)]
    pub body: RpcExtensionUiResponseBody,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// Round-trip helper: parse JSON, re-serialize, re-parse, assert
    /// the two `Value` representations are equal. Avoids needing
    /// `PartialEq` on the typed enums (some embedded types like
    /// `model::Model` do not derive it yet).
    fn roundtrip<T>(json_str: &str) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let parsed: T = serde_json::from_str(json_str).expect("parse");
        let re_emitted = serde_json::to_string(&parsed).expect("serialize");
        let _reparsed: T = serde_json::from_str(&re_emitted).expect("reparse");
        let original_value: Value = serde_json::from_str(json_str).expect("orig value");
        let reemitted_value: Value = serde_json::from_str(&re_emitted).expect("re value");
        assert_eq!(
            original_value, reemitted_value,
            "round-trip mismatch:\n  in:  {}\n  out: {}",
            json_str, re_emitted
        );
        parsed
    }

    // ---- In-scope commands (5 tests) -------------------------------------

    #[test]
    fn prompt_minimal_roundtrips() {
        let json = r#"{"type":"prompt","message":"hi","id":"42"}"#;
        let cmd: RpcCommand = roundtrip(json);
        assert!(matches!(cmd, RpcCommand::Prompt { .. }));
    }

    #[test]
    fn prompt_with_images_and_streaming_behavior_roundtrips() {
        // Image content uses model::types::ImageContent; pull a literal
        // wire payload so we exercise the camelCase fields exactly.
        let json = r#"{"type":"prompt","id":"7","message":"see image","images":[{"type":"image","data":"AAAA","mime_type":"image/png"}],"streamingBehavior":"followUp"}"#;
        let cmd: RpcCommand = roundtrip(json);
        match cmd {
            RpcCommand::Prompt {
                streaming_behavior,
                images,
                ..
            } => {
                assert!(matches!(
                    streaming_behavior,
                    Some(StreamingBehavior::FollowUp)
                ));
                assert_eq!(images.unwrap().len(), 1);
            }
            _ => panic!("expected Prompt"),
        }
    }

    #[test]
    fn abort_roundtrips_without_id() {
        let json = r#"{"type":"abort"}"#;
        let cmd: RpcCommand = roundtrip(json);
        assert!(matches!(cmd, RpcCommand::Abort { id: None }));
    }

    #[test]
    fn new_session_with_and_without_parent_roundtrip() {
        let bare = r#"{"type":"new_session"}"#;
        let cmd: RpcCommand = roundtrip(bare);
        assert!(matches!(
            cmd,
            RpcCommand::NewSession {
                id: None,
                parent_session: None
            }
        ));

        let with_parent = r#"{"type":"new_session","id":"x","parentSession":"sess-abc"}"#;
        let cmd: RpcCommand = roundtrip(with_parent);
        match cmd {
            RpcCommand::NewSession { parent_session, .. } => {
                assert_eq!(parent_session.as_deref(), Some("sess-abc"));
            }
            _ => panic!("expected NewSession"),
        }
    }

    #[test]
    fn get_state_and_get_messages_roundtrip() {
        let s = r#"{"type":"get_state","id":"1"}"#;
        let cmd: RpcCommand = roundtrip(s);
        assert!(matches!(cmd, RpcCommand::GetState { id: Some(_) }));

        let m = r#"{"type":"get_messages"}"#;
        let cmd: RpcCommand = roundtrip(m);
        assert!(matches!(cmd, RpcCommand::GetMessages { id: None }));
    }

    // ---- In-scope responses (6 tests) ------------------------------------
    //
    // The first three samples are lifted directly from the TS source
    // (`rpc-mode.ts` `success(id, command)` / `error(id, command, msg)`
    // helpers, see lines 57-70 of that file).

    #[test]
    fn prompt_success_response_roundtrips() {
        // From rpc-mode.ts:391: output(success(id, "prompt"));
        // success() returns: { id, type: "response", command: "prompt", success: true }
        let json = r#"{"id":"42","type":"response","command":"prompt","success":true}"#;
        let resp: RpcResponse = roundtrip(json);
        assert!(matches!(
            resp.body,
            RpcResponseBody::Prompt(RpcResultEmpty::Success { .. })
        ));
    }

    #[test]
    fn abort_success_response_roundtrips() {
        // From rpc-mode.ts:415: success(id, "abort") => same envelope, no data.
        let json = r#"{"id":"7","type":"response","command":"abort","success":true}"#;
        let resp: RpcResponse = roundtrip(json);
        assert!(matches!(
            resp.body,
            RpcResponseBody::Abort(RpcResultEmpty::Success { .. })
        ));
    }

    #[test]
    fn new_session_success_with_data_roundtrips() {
        // From rpc-mode.ts:424: success(id, "new_session", { cancelled: false }).
        let json = r#"{"id":"3","type":"response","command":"new_session","success":true,"data":{"cancelled":false}}"#;
        let resp: RpcResponse = roundtrip(json);
        match resp.body {
            RpcResponseBody::NewSession(RpcResultWithData::Success { data, .. }) => {
                assert!(!data.cancelled);
            }
            _ => panic!("expected NewSession success with data"),
        }
    }

    #[test]
    fn get_state_full_session_state_roundtrips() {
        let payload = json!({
            "id": "9",
            "type": "response",
            "command": "get_state",
            "success": true,
            "data": {
                "thinkingLevel": "medium",
                "isStreaming": false,
                "isCompacting": false,
                "steeringMode": "all",
                "followUpMode": "one-at-a-time",
                "sessionId": "sess-1",
                "sessionName": "scratch",
                "autoCompactionEnabled": true,
                "messageCount": 12,
                "pendingMessageCount": 0
            }
        });
        let json_str = payload.to_string();
        let resp: RpcResponse = roundtrip(&json_str);
        match resp.body {
            RpcResponseBody::GetState(RpcResultWithData::Success { data, .. }) => {
                assert_eq!(data.session_id, "sess-1");
                assert_eq!(data.message_count, 12);
                assert!(matches!(data.thinking_level, ThinkingLevel::Medium));
                assert!(matches!(data.follow_up_mode, QueueMode::OneAtATime));
            }
            _ => panic!("expected GetState success"),
        }
    }

    #[test]
    fn get_messages_empty_list_roundtrips() {
        let json =
            r#"{"type":"response","command":"get_messages","success":true,"data":{"messages":[]}}"#;
        let resp: RpcResponse = roundtrip(json);
        match resp.body {
            RpcResponseBody::GetMessages(RpcResultWithData::Success { data, .. }) => {
                assert!(data.messages.is_empty());
            }
            _ => panic!("expected GetMessages success"),
        }
    }

    #[test]
    fn error_response_roundtrips() {
        // From rpc-mode.ts:397: output(error(id, "prompt", e.message));
        // error() returns: { id, type: "response", command, success: false, error: msg }
        let json =
            r#"{"id":"42","type":"response","command":"prompt","success":false,"error":"boom"}"#;
        let resp: RpcResponse = roundtrip(json);
        match resp.body {
            RpcResponseBody::Prompt(RpcResultEmpty::Failure { error, .. }) => {
                assert_eq!(error, "boom");
            }
            _ => panic!("expected Prompt failure"),
        }
    }

    // ---- Extension UI request / response (2 tests) -----------------------

    #[test]
    fn extension_ui_request_confirm_roundtrips() {
        let json = r#"{"type":"extension_ui_request","id":"u1","method":"confirm","title":"Proceed?","message":"Continue with edits?"}"#;
        let req: RpcExtensionUiRequest = roundtrip(json);
        assert_eq!(req.id, "u1");
        assert!(matches!(
            req.kind,
            RpcExtensionUiRequestKind::Confirm { .. }
        ));
    }

    #[test]
    fn extension_ui_response_confirmed_roundtrips() {
        let json = r#"{"type":"extension_ui_response","id":"u1","confirmed":true}"#;
        let resp: RpcExtensionUiResponse = roundtrip(json);
        assert_eq!(resp.id, "u1");
        assert!(matches!(
            resp.body,
            RpcExtensionUiResponseBody::Confirmed { confirmed: true }
        ));
    }

    // ---- id field handling (1 test) --------------------------------------

    #[test]
    fn optional_id_serializes_only_when_present() {
        // Some(id): id appears
        let with_id = RpcCommand::Abort {
            id: Some("abc".into()),
        };
        let s = serde_json::to_string(&with_id).unwrap();
        assert!(s.contains(r#""id":"abc""#), "expected id field in {}", s);

        // None: id MUST NOT appear in the wire
        let no_id = RpcCommand::Abort { id: None };
        let s = serde_json::to_string(&no_id).unwrap();
        assert!(
            !s.contains(r#""id""#),
            "id field must be skipped when None: {}",
            s
        );

        // Same for responses
        let resp_no_id = RpcResponse::new(
            None,
            RpcResponseBody::Prompt(RpcResultEmpty::Success { success: true }),
        );
        let s = serde_json::to_string(&resp_no_id).unwrap();
        assert!(
            !s.contains(r#""id""#),
            "id must be skipped on response: {}",
            s
        );

        let resp_with_id = RpcResponse::new(
            Some("z".into()),
            RpcResponseBody::Prompt(RpcResultEmpty::Success { success: true }),
        );
        let s = serde_json::to_string(&resp_with_id).unwrap();
        assert!(s.contains(r#""id":"z""#), "expected id in response: {}", s);
    }
}
