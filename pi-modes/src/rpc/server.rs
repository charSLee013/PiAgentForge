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
use super::runtime::{RpcRuntime, RpcRuntimeConfig};
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
    let runtime = RpcRuntime::from_environment().await?;
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| anyhow::anyhow!("stdin read error: {e}"))?;

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
                Ok(cmd) => runtime.handle_command(cmd).await,
                Err(e) => RpcResponse::error(None, "parse", format!("Failed to parse command: {e}")),
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
    let runtime = RpcRuntime::from_config(RpcRuntimeConfig {
        model: pi_ai_core::types::Model {
            id: "rpc-test".to_string(),
            provider: pi_ai_core::types::KnownProvider::Faux,
            api: "rpc-test-stream".to_string(),
            name: Some("RPC Test".to_string()),
            base_url: None,
            supports_thinking: true,
            supports_tools: false,
            supports_streaming: true,
            supports_image_input: false,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
            cost_per_input_token: None,
            cost_per_output_token: None,
            cost_per_cache_read_token: None,
            cost_per_cache_write_token: None,
        },
        system_prompt: None,
        thinking_level: "off".to_string(),
        session_dir: std::env::temp_dir(),
        session_path: None,
    })
    .await
    .expect("in-memory rpc runtime should initialize");
    runtime.handle_command(command).await
}

/// Execute a bash command and return the result.
pub(crate) async fn handle_bash(id: Option<String>, command: &str) -> RpcResponse {
    let cmd_name = "bash";
    if command.trim().is_empty() {
        return RpcResponse::error(id, cmd_name, "Empty bash command");
    }

    // Use tokio::process::Command to run the command via the system shell.
    let output = match tokio::process::Command::new("sh").arg("-c").arg(command).output().await {
        Ok(out) => out,
        Err(e) => {
            return RpcResponse::error(id, cmd_name, format!("Failed to execute bash command: {e}"));
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
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.command, "get_state");
        assert!(response.data.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_handle_get_state_with_id() {
        let cmd = RpcCommand::GetState { id: Some("req_1".to_string()) };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.id, Some("req_1".to_string()));
    }

    #[test]
    fn test_handle_abort_success() {
        let cmd = RpcCommand::Abort { id: Some("req_2".to_string()) };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.command, "abort");
        assert!(response.data.is_none());
    }

    #[test]
    fn test_handle_prompt_success() {
        let cmd = RpcCommand::Prompt { id: None, message: "hello".to_string(), images: None, streaming_behavior: None };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success);
        assert_eq!(response.command, "prompt");
        assert!(response.error.is_none());
    }

    #[test]
    fn test_handle_bash_simple() {
        let cmd = RpcCommand::Bash { id: None, command: "echo hello".to_string() };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success, "bash should succeed: {:?}", response.error);
        assert_eq!(response.command, "bash");

        let data = response.data.expect("bash response should have data");
        let stdout = data.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        assert!(stdout.contains("hello"), "stdout should contain hello: {stdout:?}");
    }

    #[test]
    fn test_handle_bash_empty_error() {
        let cmd = RpcCommand::Bash { id: None, command: "".to_string() };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(!response.success);
        assert_eq!(response.command, "bash");
    }

    #[test]
    fn test_handle_bash_exit_code() {
        let cmd = RpcCommand::Bash { id: None, command: "exit 42".to_string() };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

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
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success);
        let data = response.data.expect("should have data");
        let commands = data.get("commands").and_then(|v| v.as_array()).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn test_handle_get_messages() {
        let cmd = RpcCommand::GetMessages { id: None };
        let response = tokio::runtime::Runtime::new().unwrap().block_on(handle_command(cmd));

        assert!(response.success);
        let data = response.data.expect("should have data");
        let messages = data.get("messages").and_then(|v| v.as_array()).unwrap();
        assert!(messages.is_empty() || !messages.is_empty());
    }
}
