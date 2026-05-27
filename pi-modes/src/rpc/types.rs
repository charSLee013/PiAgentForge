//! RPC protocol types.
//!
//! Mirrors `packages/coding-agent/src/modes/rpc/rpc-types.ts`.
//!
//! Commands are sent as JSON lines on stdin. Responses and events are emitted
//! as JSON lines on stdout.
//!
//! Field names use camelCase to match the TypeScript protocol spec.

use pi_ai_core::types::{ImageContent, Model};
use serde::{Deserialize, Serialize};

// ============================================================================
// Supporting enums
// ============================================================================

/// Streaming behaviour for the `prompt` command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StreamingBehavior {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp")]
    FollowUp,
}

/// Steering / follow-up queue mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SteeringMode {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// Thinking / reasoning level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Low,
    Medium,
    High,
}

/// Source kind for a slash command entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandSource {
    Extension,
    Prompt,
    Skill,
}

// ============================================================================
// RPC Commands (stdin)
// ============================================================================

/// All supported RPC commands.
///
/// The `type` field discriminates the variant. Each variant carries an optional
/// `id` for correlating requests with responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcCommand {
    // ── Prompting ──────────────────────────────────────────────────────────
    #[serde(rename_all = "camelCase")]
    Prompt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    #[serde(rename_all = "camelCase")]
    Steer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    #[serde(rename_all = "camelCase")]
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
    #[serde(rename_all = "camelCase")]
    NewSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
    },

    // ── State ─────────────────────────────────────────────────────────────
    GetState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ── Model ─────────────────────────────────────────────────────────────
    #[serde(rename_all = "camelCase")]
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

    // ── Thinking ──────────────────────────────────────────────────────────
    SetThinkingLevel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        level: String,
    },
    CycleThinkingLevel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ── Queue modes ───────────────────────────────────────────────────────
    SetSteeringMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: SteeringMode,
    },
    SetFollowUpMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: SteeringMode,
    },

    // ── Compaction ────────────────────────────────────────────────────────
    #[serde(rename_all = "camelCase")]
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

    // ── Retry ─────────────────────────────────────────────────────────────
    SetAutoRetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },
    AbortRetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ── Bash ──────────────────────────────────────────────────────────────
    Bash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        command: String,
    },
    AbortBash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ── Session ───────────────────────────────────────────────────────────
    GetSessionStats {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ExportHtml {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SwitchSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        session_path: String,
    },
    #[serde(rename_all = "camelCase")]
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

    // ── Messages ──────────────────────────────────────────────────────────
    GetMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // ── Commands ──────────────────────────────────────────────────────────
    GetCommands {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

// ============================================================================
// Slash command descriptor
// ============================================================================

/// A command available for invocation via prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
    /// Command name (without leading slash).
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What kind of command this is.
    pub source: CommandSource,
    /// Source metadata for the owning resource.
    pub source_info: RpcSourceInfo,
}

/// Minimal source info for a command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RpcSourceInfo {
    /// Display name of the source.
    pub name: String,
    /// Extension ID, if the command comes from an extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    /// File path of the source resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

// ============================================================================
// RPC Session State
// ============================================================================

/// Snapshot of the current session state.
///
/// `PartialEq` is not derived because `Model` does not implement it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    /// The active model, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// Current thinking / reasoning level.
    pub thinking_level: String,
    /// Whether the agent is currently streaming a response.
    pub is_streaming: bool,
    /// Whether a compaction is in progress.
    pub is_compacting: bool,
    /// Steering queue mode.
    pub steering_mode: SteeringMode,
    /// Follow-up queue mode.
    pub follow_up_mode: SteeringMode,
    /// Path to the session file, if persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    /// Unique session identifier.
    pub session_id: String,
    /// Human-readable session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Whether auto-compaction is enabled.
    pub auto_compaction_enabled: bool,
    /// Number of messages in the session.
    pub message_count: u64,
    /// Number of pending (queued) messages.
    pub pending_message_count: u64,
}

// ============================================================================
// RPC Responses (stdout)
// ============================================================================

/// A response emitted after processing a command.
///
/// This is a single struct that covers all response variants in the TS union.
/// The `command` field echoes the originating command's type. `success` indicates
/// whether the command completed without error. On success, `data` carries the
/// result payload. On failure, `error` carries the error message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RpcResponse {
    /// Correlates with the request's `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Always `"response"`.
    pub r#type: String,
    /// The command type that produced this response.
    pub command: String,
    /// Whether the command completed successfully.
    pub success: bool,
    /// Response payload for successful commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Error message for failed commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    /// Create a success response.
    pub fn success(id: Option<String>, command: impl Into<String>) -> Self {
        Self {
            id,
            r#type: "response".to_string(),
            command: command.into(),
            success: true,
            data: None,
            error: None,
        }
    }

    /// Create a success response with data.
    pub fn success_with_data(
        id: Option<String>,
        command: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            id,
            r#type: "response".to_string(),
            command: command.into(),
            success: true,
            data: Some(data),
            error: None,
        }
    }

    /// Create an error response.
    pub fn error(
        id: Option<String>,
        command: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id,
            r#type: "response".to_string(),
            command: command.into(),
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

// ============================================================================
// Extension UI Events (stdout)
// ============================================================================

/// Request emitted when an extension needs user input.
///
/// The client must respond with an `RpcExtensionUiResponse` on stdin.
///
/// The `method` field discriminates the variant. All variants share
/// `type: "extension_ui_response"`. Field names use camelCase via
/// per-variant `rename_all`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "method")]
pub enum RpcExtensionUiRequest {
    /// Show a selection list.
    #[serde(rename = "select")]
    #[serde(rename_all = "camelCase")]
    Select {
        /// Always `"extension_ui_request"`.
        r#type: String,
        id: String,
        title: String,
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    /// Show a confirmation dialog.
    #[serde(rename = "confirm")]
    #[serde(rename_all = "camelCase")]
    Confirm {
        r#type: String,
        id: String,
        title: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    /// Request text input.
    #[serde(rename = "input")]
    #[serde(rename_all = "camelCase")]
    Input {
        r#type: String,
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    /// Open a text editor.
    #[serde(rename = "editor")]
    #[serde(rename_all = "camelCase")]
    Editor {
        r#type: String,
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
    },
    /// Show a notification (fire-and-forget).
    #[serde(rename = "notify")]
    #[serde(rename_all = "camelCase")]
    Notify {
        r#type: String,
        id: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify_type: Option<String>,
    },
    /// Set a status bar item.
    #[serde(rename = "setStatus")]
    #[serde(rename_all = "camelCase")]
    SetStatus {
        r#type: String,
        id: String,
        status_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_text: Option<String>,
    },
    /// Set a widget in the TUI.
    #[serde(rename = "setWidget")]
    #[serde(rename_all = "camelCase")]
    SetWidget {
        r#type: String,
        id: String,
        widget_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        widget_lines: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        widget_placement: Option<String>,
    },
    /// Set the terminal title.
    #[serde(rename = "setTitle")]
    #[serde(rename_all = "camelCase")]
    SetTitle {
        r#type: String,
        id: String,
        title: String,
    },
    /// Set editor text.
    #[serde(rename = "set_editor_text")]
    #[serde(rename_all = "camelCase")]
    SetEditorText {
        r#type: String,
        id: String,
        text: String,
    },
}

impl RpcExtensionUiRequest {
    /// Create a `select` request.
    pub fn select(id: String, title: String, options: Vec<String>) -> Self {
        Self::Select {
            r#type: "extension_ui_request".to_string(),
            id,
            title,
            options,
            timeout: None,
        }
    }

    /// Create a `confirm` request.
    pub fn confirm(id: String, title: String, message: String) -> Self {
        Self::Confirm {
            r#type: "extension_ui_request".to_string(),
            id,
            title,
            message,
            timeout: None,
        }
    }

    /// Create a `notify` request (fire-and-forget).
    pub fn notify(id: String, message: String, notify_type: Option<String>) -> Self {
        Self::Notify {
            r#type: "extension_ui_request".to_string(),
            id,
            message,
            notify_type,
        }
    }
}

// ============================================================================
// Extension UI Responses (stdin)
// ============================================================================

/// Response to an extension UI request.
///
/// The three variants share `type: "extension_ui_response"` and are
/// distinguished by which payload field is present. Serde untagged
/// deserialization tries each variant in order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RpcExtensionUiResponse {
    /// Response carrying a string value.
    Value {
        r#type: String,
        id: String,
        value: String,
    },
    /// Response carrying a confirmation boolean.
    Confirmed {
        r#type: String,
        id: String,
        confirmed: bool,
    },
    /// Response indicating cancellation.
    Cancelled {
        r#type: String,
        id: String,
        cancelled: bool,
    },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── RpcCommand round-trip tests ────────────────────────────────────────

    #[test]
    fn test_command_prompt_round_trip() {
        let cmd = RpcCommand::Prompt {
            id: Some("req_1".to_string()),
            message: "hello world".to_string(),
            images: None,
            streaming_behavior: Some(StreamingBehavior::Steer),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_steer_round_trip() {
        let cmd = RpcCommand::Steer {
            id: None,
            message: "stop".to_string(),
            images: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_follow_up_round_trip() {
        let cmd = RpcCommand::FollowUp {
            id: Some("req_2".to_string()),
            message: "also do this".to_string(),
            images: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_abort_round_trip() {
        let cmd = RpcCommand::Abort { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"type\":\"abort\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_new_session_round_trip() {
        let cmd = RpcCommand::NewSession {
            id: Some("req_3".to_string()),
            parent_session: Some("parent-session-id".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);

        // Also test without parent_session
        let cmd2 = RpcCommand::NewSession {
            id: None,
            parent_session: None,
        };
        let json2 = serde_json::to_string(&cmd2).unwrap();
        let back2: RpcCommand = serde_json::from_str(&json2).unwrap();
        assert_eq!(cmd2, back2);
    }

    #[test]
    fn test_command_get_state_round_trip() {
        let cmd = RpcCommand::GetState {
            id: Some("req_4".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"type\":\"get_state\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_set_model_round_trip() {
        let cmd = RpcCommand::SetModel {
            id: None,
            provider: "openai".to_string(),
            model_id: "gpt-4o".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        // Verify camelCase field names
        assert!(json.contains("\"modelId\""));
        assert!(!json.contains("\"model_id\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_cycle_model_round_trip() {
        let cmd = RpcCommand::CycleModel { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_get_available_models_round_trip() {
        let cmd = RpcCommand::GetAvailableModels { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_set_thinking_level_round_trip() {
        let cmd = RpcCommand::SetThinkingLevel {
            id: None,
            level: "high".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_cycle_thinking_level_round_trip() {
        let cmd = RpcCommand::CycleThinkingLevel { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_set_steering_mode_round_trip() {
        let cmd = RpcCommand::SetSteeringMode {
            id: None,
            mode: SteeringMode::OneAtATime,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_set_follow_up_mode_round_trip() {
        let cmd = RpcCommand::SetFollowUpMode {
            id: Some("req_5".to_string()),
            mode: SteeringMode::All,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_compact_round_trip() {
        let cmd = RpcCommand::Compact {
            id: Some("req_6".to_string()),
            custom_instructions: Some("summarize key points".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"customInstructions\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);

        // Without custom instructions
        let cmd2 = RpcCommand::Compact {
            id: None,
            custom_instructions: None,
        };
        let json2 = serde_json::to_string(&cmd2).unwrap();
        let back2: RpcCommand = serde_json::from_str(&json2).unwrap();
        assert_eq!(cmd2, back2);
    }

    #[test]
    fn test_command_set_auto_compaction_round_trip() {
        let cmd = RpcCommand::SetAutoCompaction {
            id: None,
            enabled: true,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_set_auto_retry_round_trip() {
        let cmd = RpcCommand::SetAutoRetry {
            id: None,
            enabled: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_abort_retry_round_trip() {
        let cmd = RpcCommand::AbortRetry {
            id: Some("req_7".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_bash_round_trip() {
        let cmd = RpcCommand::Bash {
            id: None,
            command: "ls -la".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_abort_bash_round_trip() {
        let cmd = RpcCommand::AbortBash {
            id: Some("req_8".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_get_session_stats_round_trip() {
        let cmd = RpcCommand::GetSessionStats { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_export_html_round_trip() {
        let cmd = RpcCommand::ExportHtml {
            id: Some("req_9".to_string()),
            output_path: Some("/tmp/session.html".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"outputPath\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);

        // Without output path
        let cmd2 = RpcCommand::ExportHtml {
            id: None,
            output_path: None,
        };
        let json2 = serde_json::to_string(&cmd2).unwrap();
        let back2: RpcCommand = serde_json::from_str(&json2).unwrap();
        assert_eq!(cmd2, back2);
    }

    #[test]
    fn test_command_switch_session_round_trip() {
        let cmd = RpcCommand::SwitchSession {
            id: Some("req_10".to_string()),
            session_path: "/home/user/.pi/sessions/session.jsonl".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"sessionPath\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_fork_round_trip() {
        let cmd = RpcCommand::Fork {
            id: Some("req_11".to_string()),
            entry_id: "abc12345".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"entryId\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_clone_round_trip() {
        let cmd = RpcCommand::Clone {
            id: Some("req_12".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"type\":\"clone\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_get_fork_messages_round_trip() {
        let cmd = RpcCommand::GetForkMessages { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_get_last_assistant_text_round_trip() {
        let cmd = RpcCommand::GetLastAssistantText { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_set_session_name_round_trip() {
        let cmd = RpcCommand::SetSessionName {
            id: None,
            name: "My Session".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_get_messages_round_trip() {
        let cmd = RpcCommand::GetMessages { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn test_command_get_commands_round_trip() {
        let cmd = RpcCommand::GetCommands { id: None };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    // ── Serialization edge cases ─────────────────────────────────────────

    #[test]
    fn test_json_tag_always_first_field() {
        // Verify the `type` field comes first in serialized JSON
        let cmd = RpcCommand::Prompt {
            id: Some("test".to_string()),
            message: "hello".to_string(),
            images: None,
            streaming_behavior: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        // The JSON should start with {"type":"prompt", ...}
        assert!(json.starts_with(r#"{"type":"prompt""#));
    }

    #[test]
    fn test_camel_case_fields_in_prompt() {
        let cmd = RpcCommand::Prompt {
            id: None,
            message: "test".to_string(),
            images: None,
            streaming_behavior: Some(StreamingBehavior::FollowUp),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"streamingBehavior\""));
        assert!(json.contains("\"followUp\""));
    }

    #[test]
    fn test_camel_case_fields_in_set_model() {
        let cmd = RpcCommand::SetModel {
            id: None,
            provider: "anthropic".to_string(),
            model_id: "claude-3-opus".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"modelId\""), "camelCase modelId: {json}");
        assert!(!json.contains("\"model_id\""), "no snake_case model_id: {json}");
    }

    // ── Extension UI serialization tests ─────────────────────────────────

    #[test]
    fn test_extension_ui_request_select_round_trip() {
        let req = RpcExtensionUiRequest::select(
            "ui_1".to_string(),
            "Choose an option".to_string(),
            vec!["opt1".to_string(), "opt2".to_string()],
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"extension_ui_request\""));
        let back: RpcExtensionUiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn test_extension_ui_request_confirm_round_trip() {
        let req = RpcExtensionUiRequest::confirm(
            "ui_2".to_string(),
            "Confirm?".to_string(),
            "Are you sure?".to_string(),
        );
        let json = serde_json::to_string(&req).unwrap();
        let back: RpcExtensionUiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn test_extension_ui_request_notify_round_trip() {
        let req = RpcExtensionUiRequest::notify(
            "ui_3".to_string(),
            "Hello".to_string(),
            Some("info".to_string()),
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"notifyType\""));
        let back: RpcExtensionUiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn test_extension_ui_request_set_widget_round_trip() {
        let req = RpcExtensionUiRequest::SetWidget {
            r#type: "extension_ui_request".to_string(),
            id: "ui_4".to_string(),
            widget_key: "toolbar".to_string(),
            widget_lines: Some(vec!["line1".to_string()]),
            widget_placement: Some("aboveEditor".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"widgetLines\""));
        assert!(json.contains("\"widgetPlacement\""));
        let back: RpcExtensionUiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn test_extension_ui_request_set_editor_text_round_trip() {
        let req = RpcExtensionUiRequest::SetEditorText {
            r#type: "extension_ui_request".to_string(),
            id: "ui_5".to_string(),
            text: "hello world".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: RpcExtensionUiRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn test_extension_ui_response_value_round_trip() {
        let resp = RpcExtensionUiResponse::Value {
            r#type: "extension_ui_response".to_string(),
            id: "ui_1".to_string(),
            value: "selected_option".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"extension_ui_response\""));
        let back: RpcExtensionUiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn test_extension_ui_response_confirmed_round_trip() {
        let resp = RpcExtensionUiResponse::Confirmed {
            r#type: "extension_ui_response".to_string(),
            id: "ui_2".to_string(),
            confirmed: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: RpcExtensionUiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn test_extension_ui_response_cancelled_round_trip() {
        let resp = RpcExtensionUiResponse::Cancelled {
            r#type: "extension_ui_response".to_string(),
            id: "ui_3".to_string(),
            cancelled: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: RpcExtensionUiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    // ── Slash command serialization ──────────────────────────────────────

    #[test]
    fn test_slash_command_serialization() {
        let cmd = RpcSlashCommand {
            name: "help".to_string(),
            description: Some("Show help".to_string()),
            source: CommandSource::Prompt,
            source_info: RpcSourceInfo {
                name: "builtin".to_string(),
                extension_id: None,
                path: None,
            },
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"name\""));
        assert!(json.contains("\"sourceInfo\""));
        let back: RpcSlashCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    // ── RpcResponse builder tests ────────────────────────────────────────

    #[test]
    fn test_response_helpers() {
        let s = RpcResponse::success(Some("id".to_string()), "cmd");
        assert!(s.success);
        assert_eq!(s.command, "cmd");
        assert_eq!(s.r#type, "response");

        let e = RpcResponse::error(Some("id".to_string()), "cmd", "msg");
        assert!(!e.success);
        assert_eq!(e.error, Some("msg".to_string()));

        let sd = RpcResponse::success_with_data(
            None,
            "cmd",
            serde_json::json!({"key": "val"}),
        );
        assert!(sd.success);
        assert_eq!(sd.data, Some(serde_json::json!({"key": "val"})));
    }

    #[test]
    fn test_response_serialization_omits_empty_fields() {
        let resp = RpcResponse::success(None, "test");
        let json = serde_json::to_string(&resp).unwrap();
        // When there's no id and no data, those fields should be omitted
        assert!(!json.contains("\"id\""));
        assert!(!json.contains("\"data\""));
        assert!(!json.contains("\"error\""));
        assert!(json.contains("\"success\":true"));
    }

    #[test]
    fn test_response_serialization_includes_id() {
        let resp = RpcResponse::success(Some("req_1".to_string()), "test");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"id\":\"req_1\""));
    }

    // ── Serialization field name verification ────────────────────────────

    #[test]
    fn test_rpc_response_field_names() {
        let resp = RpcResponse::success(Some("id".to_string()), "bash");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "response");
        assert_eq!(parsed["command"], "bash");
        assert_eq!(parsed["id"], "id");
        assert!(parsed.get("data").is_none());
    }

    #[test]
    fn test_prompt_command_with_images() {
        let img = ImageContent {
            source: pi_ai_core::types::ImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "base64data".to_string(),
            },
        };
        let cmd = RpcCommand::Prompt {
            id: Some("req_img".to_string()),
            message: "describe this".to_string(),
            images: Some(vec![img]),
            streaming_behavior: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"images\""));
        let back: RpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }
}


// ============================================================================
// Bash Result
// ============================================================================

/// Result of executing a bash command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BashOutput {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Exit code.
    pub exit_code: i32,
    /// Whether the command timed out.
    pub timed_out: bool,
}
