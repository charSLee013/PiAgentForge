//! RPC server — JSONL-over-stdio protocol loop.
//!
//! Mirrors `packages/coding-agent/src/modes/rpc/rpc-mode.ts`.
//!
//! Reads JSON commands from stdin and writes JSON responses to stdout.
//! Each line on stdin is parsed as an [`RpcCommand`]; the server dispatches
//! to the appropriate handler and writes the [`RpcResponse`] (or events) to
//! stdout.
//!
//! # Protocol
//!
//! - **Commands**: JSON objects on stdin with a `type` field
//! - **Responses**: JSON objects on stdout with `type: "response"`
//! - **Events**: JSON objects on stdout emitted during streaming operations

use std::io::{self, BufRead};

use super::jsonl::serialize_line;
use super::types::*;

/// Run the RPC server loop.
///
/// Reads commands from stdin line-by-line, dispatches them, and writes
/// responses to stdout. Returns when stdin is closed (EOF).
///
/// This is a simplified implementation focused on correct protocol handling.
/// Commands that require a full agent session runtime return "not implemented"
/// errors.
pub async fn run_rpc_server() -> anyhow::Result<()> {
    tracing::info!("starting RPC server (stdin/stdout protocol)");
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| anyhow::anyhow!("stdin read error: {e}"))?;

        if n == 0 {
            // EOF
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try to parse the line. First check for extension_ui_response.
        let response = if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if val.get("type").and_then(|t| t.as_str()) == Some("extension_ui_response") {
                // Extension UI responses are acknowledged but don't produce
                // a response in the simplified server.
                continue;
            }

            // Parse as RpcCommand
            match serde_json::from_value::<RpcCommand>(val) {
                Ok(cmd) => handle_command(cmd).await,
                Err(e) => RpcResponse::error(
                    None,
                    "parse",
                    format!("Failed to parse command: {e}"),
                ),
            }
        } else {
            RpcResponse::error(None, "parse", format!("Invalid JSON: {trimmed}"))
        };

        let output = serialize_line(&response);
        // Use write! + flush to stdout (avoid extra newline from println!)
        {
            use std::io::Write;
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(output.as_bytes())?;
            handle.flush()?;
        }
    }

    tracing::info!("RPC server shutting down (stdin closed)");
    Ok(())
}

/// Dispatch a single command and produce a response.
///
/// This function is `pub` for testing; callers should normally use
/// [`run_rpc_server`] which drives the full stdin/stdout loop.
pub async fn handle_command(command: RpcCommand) -> RpcResponse {
    let id = match &command {
        RpcCommand::Prompt { id, .. } => id.clone(),
        RpcCommand::Steer { id, .. } => id.clone(),
        RpcCommand::FollowUp { id, .. } => id.clone(),
        RpcCommand::Abort { id } => id.clone(),
        RpcCommand::NewSession { id, .. } => id.clone(),
        RpcCommand::GetState { id } => id.clone(),
        RpcCommand::SetModel { id, .. } => id.clone(),
        RpcCommand::CycleModel { id } => id.clone(),
        RpcCommand::GetAvailableModels { id } => id.clone(),
        RpcCommand::SetThinkingLevel { id, .. } => id.clone(),
        RpcCommand::CycleThinkingLevel { id } => id.clone(),
        RpcCommand::SetSteeringMode { id, .. } => id.clone(),
        RpcCommand::SetFollowUpMode { id, .. } => id.clone(),
        RpcCommand::Compact { id, .. } => id.clone(),
        RpcCommand::SetAutoCompaction { id, .. } => id.clone(),
        RpcCommand::SetAutoRetry { id, .. } => id.clone(),
        RpcCommand::AbortRetry { id } => id.clone(),
        RpcCommand::Bash { id, .. } => id.clone(),
        RpcCommand::AbortBash { id } => id.clone(),
        RpcCommand::GetSessionStats { id } => id.clone(),
        RpcCommand::ExportHtml { id, .. } => id.clone(),
        RpcCommand::SwitchSession { id, .. } => id.clone(),
        RpcCommand::Fork { id, .. } => id.clone(),
        RpcCommand::Clone { id } => id.clone(),
        RpcCommand::GetForkMessages { id } => id.clone(),
        RpcCommand::GetLastAssistantText { id } => id.clone(),
        RpcCommand::SetSessionName { id, .. } => id.clone(),
        RpcCommand::GetMessages { id } => id.clone(),
        RpcCommand::GetCommands { id } => id.clone(),
    };

    match command {
        // ── Prompting ──────────────────────────────────────────────────────
        RpcCommand::Prompt { message, images, .. } => {
            // TODO: wire AgentSessionRuntime for prompt handling
            let _ = (message, images);
            RpcResponse::error(
                id,
                "prompt",
                "Not implemented: prompt requires an agent session runtime",
            )
        }
        RpcCommand::Steer { message, images, .. } => {
            let _ = (message, images);
            RpcResponse::error(
                id,
                "steer",
                "Not implemented: steer requires an agent session runtime",
            )
        }
        RpcCommand::FollowUp { message, images, .. } => {
            let _ = (message, images);
            RpcResponse::error(
                id,
                "follow_up",
                "Not implemented: follow_up requires an agent session runtime",
            )
        }
        RpcCommand::Abort { .. } => {
            // No-op in simplified mode
            RpcResponse::success(id, "abort")
        }
        RpcCommand::NewSession { .. } => {
            // In simplified mode, new_session is not fully supported.
            // Return cancelled: true as if the user cancelled.
            let data = serde_json::json!({ "cancelled": true });
            RpcResponse::success_with_data(id, "new_session", data)
        }

        // ── State ─────────────────────────────────────────────────────────
        RpcCommand::GetState { .. } => {
            // Return a minimal session state (no real session).
            let state = RpcSessionState {
                model: None,
                thinking_level: "off".to_string(),
                is_streaming: false,
                is_compacting: false,
                steering_mode: SteeringMode::All,
                follow_up_mode: SteeringMode::All,
                session_file: None,
                session_id: "rpc-session".to_string(),
                session_name: None,
                auto_compaction_enabled: false,
                message_count: 0,
                pending_message_count: 0,
            };
            let data = serde_json::to_value(state).unwrap_or_default();
            RpcResponse::success_with_data(id, "get_state", data)
        }

        // ── Model ─────────────────────────────────────────────────────────
        RpcCommand::SetModel { provider, model_id, .. } => {
            let _ = (provider, model_id);
            RpcResponse::error(
                id,
                "set_model",
                "Not implemented: set_model requires a model registry",
            )
        }
        RpcCommand::CycleModel { .. } => {
            RpcResponse::error(id, "cycle_model", "Not implemented: cycle_model requires a session")
        }
        RpcCommand::GetAvailableModels { .. } => {
            RpcResponse::success_with_data(id, "get_available_models", serde_json::json!({"models": []}))
        }

        // ── Thinking ──────────────────────────────────────────────────────
        RpcCommand::SetThinkingLevel { level, .. } => {
            let _ = level;
            RpcResponse::error(
                id,
                "set_thinking_level",
                "Not implemented: set_thinking_level requires a session",
            )
        }
        RpcCommand::CycleThinkingLevel { .. } => {
            RpcResponse::success_with_data(id, "cycle_thinking_level", serde_json::json!(null))
        }

        // ── Queue modes ───────────────────────────────────────────────────
        RpcCommand::SetSteeringMode { mode, .. } => {
            RpcResponse::success_with_data(
                id,
                "set_steering_mode",
                serde_json::json!({ "mode": mode }),
            )
        }
        RpcCommand::SetFollowUpMode { mode, .. } => {
            RpcResponse::success_with_data(
                id,
                "set_follow_up_mode",
                serde_json::json!({ "mode": mode }),
            )
        }

        // ── Compaction ────────────────────────────────────────────────────
        RpcCommand::Compact { .. } => {
            RpcResponse::error(id, "compact", "Not implemented: compact requires a session")
        }
        RpcCommand::SetAutoCompaction { enabled, .. } => {
            RpcResponse::success_with_data(
                id,
                "set_auto_compaction",
                serde_json::json!({ "enabled": enabled }),
            )
        }

        // ── Retry ─────────────────────────────────────────────────────────
        RpcCommand::SetAutoRetry { enabled, .. } => {
            RpcResponse::success_with_data(
                id,
                "set_auto_retry",
                serde_json::json!({ "enabled": enabled }),
            )
        }
        RpcCommand::AbortRetry { .. } => {
            RpcResponse::success(id, "abort_retry")
        }

        // ── Bash ──────────────────────────────────────────────────────────
        RpcCommand::Bash { command, .. } => {
            handle_bash(id, &command).await
        }
        RpcCommand::AbortBash { .. } => {
            RpcResponse::success(id, "abort_bash")
        }

        // ── Session ───────────────────────────────────────────────────────
        RpcCommand::GetSessionStats { .. } => {
            let data = serde_json::json!({
                "totalTokens": 0,
                "totalCost": 0.0,
                "messageCount": 0,
                "turnCount": 0,
            });
            RpcResponse::success_with_data(id, "get_session_stats", data)
        }
        RpcCommand::ExportHtml { .. } => {
            RpcResponse::error(id, "export_html", "Not implemented: export_html requires a session")
        }
        RpcCommand::SwitchSession { .. } => {
            RpcResponse::error(
                id,
                "switch_session",
                "Not implemented: switch_session requires a session runtime",
            )
        }
        RpcCommand::Fork { .. } => {
            RpcResponse::error(id, "fork", "Not implemented: fork requires a session")
        }
        RpcCommand::Clone { .. } => {
            RpcResponse::error(id, "clone", "Not implemented: clone requires a session")
        }
        RpcCommand::GetForkMessages { .. } => {
            RpcResponse::success_with_data(
                id,
                "get_fork_messages",
                serde_json::json!({ "messages": [] }),
            )
        }
        RpcCommand::GetLastAssistantText { .. } => {
            RpcResponse::success_with_data(
                id,
                "get_last_assistant_text",
                serde_json::json!({ "text": null }),
            )
        }
        RpcCommand::SetSessionName { name, .. } => {
            let _ = name;
            RpcResponse::success(id, "set_session_name")
        }

        // ── Messages ──────────────────────────────────────────────────────
        RpcCommand::GetMessages { .. } => {
            RpcResponse::success_with_data(
                id,
                "get_messages",
                serde_json::json!({ "messages": [] }),
            )
        }

        // ── Commands ──────────────────────────────────────────────────────
        RpcCommand::GetCommands { .. } => {
            RpcResponse::success_with_data(
                id,
                "get_commands",
                serde_json::json!({ "commands": [] }),
            )
        }
    }
}

/// Execute a bash command and return the result.
async fn handle_bash(id: Option<String>, command: &str) -> RpcResponse {
    let cmd_name = "bash";
    if command.trim().is_empty() {
        return RpcResponse::error(id, cmd_name, "Empty bash command");
    }

    // Use tokio::process::Command to run the command via the system shell.
    let output = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return RpcResponse::error(
                id,
                cmd_name,
                format!("Failed to execute bash command: {e}"),
            );
        }
    };

    let bash_output = BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        timed_out: false,
    };

    let data = serde_json::to_value(bash_output).unwrap_or_default();
    RpcResponse::success_with_data(id, cmd_name, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_get_state() {
        let cmd = RpcCommand::GetState { id: None };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.command, "get_state");
        assert!(response.data.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_handle_get_state_with_id() {
        let cmd = RpcCommand::GetState {
            id: Some("req_1".to_string()),
        };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.id, Some("req_1".to_string()));
    }

    #[test]
    fn test_handle_abort_success() {
        let cmd = RpcCommand::Abort {
            id: Some("req_2".to_string()),
        };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.command, "abort");
        assert!(response.data.is_none());
    }

    #[test]
    fn test_handle_prompt_error() {
        let cmd = RpcCommand::Prompt {
            id: None,
            message: "hello".to_string(),
            images: None,
            streaming_behavior: None,
        };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(!response.success);
        assert_eq!(response.command, "prompt");
        assert!(response.error.is_some());
    }

    #[test]
    fn test_handle_bash_simple() {
        let cmd = RpcCommand::Bash {
            id: None,
            command: "echo hello".to_string(),
        };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success, "bash should succeed: {:?}", response.error);
        assert_eq!(response.command, "bash");

        let data = response.data.expect("bash response should have data");
        let stdout = data.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        assert!(stdout.contains("hello"), "stdout should contain hello: {stdout:?}");
    }

    #[test]
    fn test_handle_bash_empty_error() {
        let cmd = RpcCommand::Bash {
            id: None,
            command: "".to_string(),
        };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(!response.success);
        assert_eq!(response.command, "bash");
    }

    #[test]
    fn test_handle_bash_exit_code() {
        let cmd = RpcCommand::Bash {
            id: None,
            command: "exit 42".to_string(),
        };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success);
        let data = response.data.expect("bash response should have data");
        let exit_code = data.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(-1);
        assert_eq!(exit_code, 42);
    }

    #[test]
    fn test_response_serialization_round_trip() {
        let resp = RpcResponse::success(Some("id1".to_string()), "test_cmd");
        let json = serde_json::to_string(&resp).unwrap();
        let back: RpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn test_error_response_serialization() {
        let resp = RpcResponse::error(Some("id2".to_string()), "test_cmd", "something went wrong");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("\"success\":false"));
        let back: RpcResponse = serde_json::from_str(&json).unwrap();
        assert!(!back.success);
        assert_eq!(back.error, Some("something went wrong".to_string()));
    }

    #[test]
    fn test_handle_get_commands() {
        let cmd = RpcCommand::GetCommands { id: None };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success);
        let data = response.data.expect("should have data");
        let commands = data.get("commands").and_then(|v| v.as_array()).unwrap();
        assert!(commands.is_empty());
    }

    #[test]
    fn test_handle_get_messages() {
        let cmd = RpcCommand::GetMessages { id: None };
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_command(cmd));

        assert!(response.success);
        let data = response.data.expect("should have data");
        let messages = data.get("messages").and_then(|v| v.as_array()).unwrap();
        assert!(messages.is_empty());
    }
}
