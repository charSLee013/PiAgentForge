//! Tool registry — dispatch table for all built-in tools.
//!
//! Provides:
//! - `tool_definitions()` — returns [`ToolDefinition`] for all 7 built-in tools
//! - `execute_tool()` — dispatches a tool call by name to the correct implementation

use pi_agent_core::types::AgentToolResult;
use pi_ai_core::types::{ContentBlock, TextContent, ToolDefinition};
use tokio_util::sync::CancellationToken;

use crate::tools::bash::{execute_bash, BashInput};
use crate::tools::edit::{execute_edit, EditInput};
use crate::tools::find::{execute_find, FindInput};
use crate::tools::grep::{execute_grep, GrepInput};
use crate::tools::ls::{execute_ls, LsInput};
use crate::tools::read::{execute_read, ReadInput};
use crate::tools::write::{execute_write, WriteInput};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// Return [`ToolDefinition`] for all seven built-in tools.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "Bash".into(),
            description: "Execute a shell command and capture its output.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Optional timeout in seconds."
                    }
                },
                "required": ["command"]
            }),
            strict: None,
        },
        ToolDefinition {
            name: "Read".into(),
            description: "Read the contents of a file. Supports line offsets and limits.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)."
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line number to start reading from (1-indexed)."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read."
                    }
                },
                "required": ["path"]
            }),
            strict: None,
        },
        ToolDefinition {
            name: "Write".into(),
            description: "Write content to a file, creating parent directories as needed.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative or absolute)."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file."
                    }
                },
                "required": ["path", "content"]
            }),
            strict: None,
        },
        ToolDefinition {
            name: "Edit".into(),
            description: "Make targeted edits to a file by replacing exact text matches.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative or absolute)."
                    },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": {
                                    "type": "string",
                                    "description": "The exact text to find (must match exactly once or zero times)."
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "The replacement text."
                                }
                            },
                            "required": ["oldText", "newText"]
                        },
                        "description": "One or more targeted replacements."
                    }
                },
                "required": ["path", "edits"]
            }),
            strict: None,
        },
        ToolDefinition {
            name: "Grep".into(),
            description: "Search file contents for a regex or literal pattern.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The regex or literal string to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (default: current directory)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Glob filter (e.g. '*.rs' or '**/*.spec.ts')."
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Case-insensitive search."
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat pattern as literal string instead of regex."
                    },
                    "context": {
                        "type": "number",
                        "description": "Lines of context before and after each match."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of matches to return."
                    }
                },
                "required": ["pattern"]
            }),
            strict: None,
        },
        ToolDefinition {
            name: "Find".into(),
            description: "Search for files by glob pattern. Respects .gitignore.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (default: current directory)."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of results."
                    }
                },
                "required": ["pattern"]
            }),
            strict: None,
        },
        ToolDefinition {
            name: "Ls".into(),
            description: "List directory contents.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (default: current directory)."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of entries to show."
                    }
                }
            }),
            strict: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

/// Execute a tool by name with the given JSON arguments.
///
/// Returns an `AgentToolResult` (with `tool_call_id` empty — the caller should
/// set it to the correct value).
pub async fn execute_tool(
    name: &str,
    args: serde_json::Value,
    cancel: CancellationToken,
) -> Result<AgentToolResult, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("Failed to get current directory: {e}"))?;

    match name {
        "Bash" | "bash" => cmd_bash(args, cancel).await,
        "Read" | "read" => cmd_read(args, &cwd).await,
        "Write" | "write" => cmd_write(args, &cwd).await,
        "Edit" | "edit" => cmd_edit(args, &cwd).await,
        "Grep" | "grep" => cmd_grep(args, &cwd).await,
        "Find" | "find" => cmd_find(args, &cwd).await,
        "Ls" | "ls" => cmd_ls(args, &cwd).await,
        _ => Err(format!("Unknown tool: {name}")),
    }
}

// ---------------------------------------------------------------------------
// Individual tool handlers
// ---------------------------------------------------------------------------

async fn cmd_bash(args: serde_json::Value, cancel: CancellationToken) -> Result<AgentToolResult, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'command' for Bash tool".to_string())?
        .to_string();
    let timeout = args.get("timeout").and_then(|v| v.as_u64());

    let input = BashInput { command, timeout };
    let result = execute_bash(&input, cancel)
        .await
        .map_err(|e| format!("Bash error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: result.output,
        })],
        is_error: result.exit_code != 0,
        details: Some(serde_json::json!({
            "exit_code": result.exit_code,
            "truncated": result.truncated,
        })),
    })
}

async fn cmd_read(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'path' for Read tool".to_string())?
        .to_string();
    let offset = args.get("offset").and_then(|v| v.as_u64()).map(|v| v as usize);
    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);

    let input = ReadInput { path, offset, limit };
    let result = execute_read(&input, cwd)
        .await
        .map_err(|e| format!("Read error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: result.content,
        })],
        is_error: false,
        details: None,
    })
}

async fn cmd_write(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'path' for Write tool".to_string())?
        .to_string();
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'content' for Write tool".to_string())?
        .to_string();

    let input = WriteInput { path, content };
    let result = execute_write(&input, cwd)
        .await
        .map_err(|e| format!("Write error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: result.message,
        })],
        is_error: false,
        details: None,
    })
}

async fn cmd_edit(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'path' for Edit tool".to_string())?
        .to_string();

    // Deserialize edits array (camelCase oldText/newText).
    let edits_raw = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing required field 'edits' for Edit tool".to_string())?
        .clone();

    let edits: Vec<crate::tools::edit_diff::Edit> = edits_raw
        .iter()
        .map(|e| {
            let old_text = e
                .get("oldText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let new_text = e
                .get("newText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            crate::tools::edit_diff::Edit { old_text, new_text }
        })
        .collect();

    if edits.is_empty() {
        return Err("Edit tool requires at least one edit entry".to_string());
    }

    let input = EditInput { path, edits };
    let result = execute_edit(&input, cwd)
        .await
        .map_err(|e| format!("Edit error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: format!("{}\n\n```diff\n{}\n```", result.message, result.diff),
        })],
        is_error: false,
        details: None,
    })
}

async fn cmd_grep(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'pattern' for Grep tool".to_string())?
        .to_string();
    let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
    let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
    let ignore_case = args.get("ignoreCase").and_then(|v| v.as_bool());
    let literal = args.get("literal").and_then(|v| v.as_bool());
    let context = args.get("context").and_then(|v| v.as_u64()).map(|v| v as usize);
    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);

    let input = GrepInput {
        pattern,
        path,
        glob,
        ignore_case,
        literal,
        context,
        limit,
    };
    let result = execute_grep(&input, cwd)
        .await
        .map_err(|e| format!("Grep error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: result.output,
        })],
        is_error: false,
        details: None,
    })
}

async fn cmd_find(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'pattern' for Find tool".to_string())?
        .to_string();
    let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);

    let input = FindInput { pattern, path, limit };
    let result = execute_find(&input, cwd)
        .await
        .map_err(|e| format!("Find error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: result.output,
        })],
        is_error: false,
        details: None,
    })
}

async fn cmd_ls(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);

    let input = LsInput { path, limit };
    let result = execute_ls(&input, cwd)
        .await
        .map_err(|e| format!("Ls error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent {
            text: result.output,
        })],
        is_error: false,
        details: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definitions_returns_all_7_tools() {
        let defs = tool_definitions();
        assert_eq!(defs.len(), 7, "expected 7 tool definitions");

        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Bash"), "Bash missing: {names:?}");
        assert!(names.contains(&"Read"), "Read missing: {names:?}");
        assert!(names.contains(&"Write"), "Write missing: {names:?}");
        assert!(names.contains(&"Edit"), "Edit missing: {names:?}");
        assert!(names.contains(&"Grep"), "Grep missing: {names:?}");
        assert!(names.contains(&"Find"), "Find missing: {names:?}");
        assert!(names.contains(&"Ls"), "Ls missing: {names:?}");
    }

    #[tokio::test]
    async fn test_execute_bash_echo() {
        let args = serde_json::json!({"command": "echo hello_tool_test"});
        let cancel = CancellationToken::new();
        let result = execute_tool("Bash", args, cancel).await;

        assert!(result.is_ok(), "Bash should succeed: {:?}", result.err());
        let tool_result = result.unwrap();

        // Extract text content
        let text: String = tool_result
            .content
            .iter()
            .filter_map(|c| {
                if let ContentBlock::Text(t) = c {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(text.contains("hello_tool_test"), "Output: {text}");
        assert!(!tool_result.is_error, "exit code should be 0");
    }

    #[tokio::test]
    async fn test_execute_unknown_tool_returns_error() {
        let args = serde_json::json!({});
        let cancel = CancellationToken::new();
        let result = execute_tool("NonExistentTool", args, cancel).await;
        assert!(result.is_err(), "expected Err for unknown tool");
        assert!(
            result.unwrap_err().contains("Unknown tool"),
            "should mention Unknown tool"
        );
    }
}
