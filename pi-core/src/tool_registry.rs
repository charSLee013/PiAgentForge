//! Tool registry — dispatch table for all built-in tools.
//!
//! Provides:
//! - `tool_definitions()` — returns [`ToolDefinition`] for all 7 built-in tools
//! - `execute_tool()` — dispatches a tool call by name to the correct implementation

use pi_agent_core::types::{AgentToolResult, ToolUpdateCallback};
use pi_ai_core::types::{ContentBlock, TextContent, ToolDefinition};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::io::{ShellOutputCallback, ShellOutputChunk};
use crate::tools::bash::{BashInput, execute_bash, execute_bash_with_output_callback};
use crate::tools::edit::{EditInput, execute_edit};
use crate::tools::find::{FindInput, execute_find};
use crate::tools::grep::{GrepInput, execute_grep};
use crate::tools::ls::{LsInput, execute_ls};
use crate::tools::read::{ReadInput, execute_read};
use crate::tools::write::{WriteInput, execute_write};

/// Tool exposure preset for user-facing workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPreset {
    Full,
    PlanReadOnly,
}

/// CLI-visible built-in tool selection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSelection {
    disable_builtin: bool,
    allowlist: Option<BTreeSet<String>>,
}

impl Default for ToolSelection {
    fn default() -> Self {
        Self::all()
    }
}

impl ToolSelection {
    /// Enable all built-in tools.
    pub fn all() -> Self {
        Self { disable_builtin: false, allowlist: None }
    }

    /// Disable all built-in tools.
    pub fn disable_builtin() -> Self {
        Self { disable_builtin: true, allowlist: None }
    }

    /// Allow only the named built-in tools.
    pub fn allow_only(names: &[String]) -> Result<Self, String> {
        let mut allowlist = BTreeSet::new();
        for name in names {
            let normalized = normalize_tool_name(name).ok_or_else(|| {
                format!("Unknown tool '{}'. Supported built-in tools: {}", name, supported_tool_names().join(", "))
            })?;
            allowlist.insert(normalized.to_string());
        }
        Ok(Self { disable_builtin: false, allowlist: Some(allowlist) })
    }

    fn allows(&self, name: &str) -> bool {
        if self.disable_builtin {
            return false;
        }
        let Some(normalized) = normalize_tool_name(name) else {
            return false;
        };
        match &self.allowlist {
            Some(allowlist) => allowlist.contains(normalized),
            None => true,
        }
    }
}

const PLAN_BASH_ALLOWLIST: &[&str] =
    &["pwd", "ls", "eza", "grep", "rg", "cat", "head", "tail", "git status", "git ls-files"];

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// Return [`ToolDefinition`] for all seven built-in tools.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    tool_definitions_for_preset(ToolPreset::Full)
}

/// Return tool definitions filtered by a user-facing preset.
pub fn tool_definitions_for_preset(preset: ToolPreset) -> Vec<ToolDefinition> {
    tool_definitions_for_selection(preset, &ToolSelection::all())
}

/// Return tool definitions filtered by preset and CLI tool selection.
pub fn tool_definitions_for_selection(preset: ToolPreset, selection: &ToolSelection) -> Vec<ToolDefinition> {
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
    .into_iter()
    .filter(|definition| match preset {
        ToolPreset::Full => true,
        ToolPreset::PlanReadOnly => matches!(definition.name.as_str(), "Bash" | "Read" | "Grep" | "Find" | "Ls"),
    })
    .filter(|definition| selection.allows(&definition.name))
    .collect()
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
    execute_tool_for_preset(name, args, cancel, ToolPreset::Full).await
}

/// Execute a tool with preset-based policy filtering.
pub async fn execute_tool_for_preset(
    name: &str,
    args: serde_json::Value,
    cancel: CancellationToken,
    preset: ToolPreset,
) -> Result<AgentToolResult, String> {
    execute_tool_for_selection(name, args, cancel, preset, &ToolSelection::all()).await
}

/// Execute a tool with preset and CLI tool selection filtering.
pub async fn execute_tool_for_selection(
    name: &str,
    args: serde_json::Value,
    cancel: CancellationToken,
    preset: ToolPreset,
    selection: &ToolSelection,
) -> Result<AgentToolResult, String> {
    execute_tool_for_selection_with_updates(name, args, cancel, preset, selection, None).await
}

/// Execute a tool with preset, CLI selection, and streaming update callback support.
pub async fn execute_tool_for_selection_with_updates(
    name: &str,
    args: serde_json::Value,
    cancel: CancellationToken,
    preset: ToolPreset,
    selection: &ToolSelection,
    update_callback: Option<ToolUpdateCallback>,
) -> Result<AgentToolResult, String> {
    if normalize_tool_name(name).is_none() {
        return Err(format!("Unknown tool: {name}"));
    }
    if !selection.allows(name) {
        return Err(format!("Tool '{name}' is disabled by the current tool selection"));
    }

    let cwd = std::env::current_dir().map_err(|e| format!("Failed to get current directory: {e}"))?;

    match (preset, name) {
        (ToolPreset::PlanReadOnly, "Write" | "write" | "Edit" | "edit") => {
            Err(format!("Tool '{name}' is disabled in plan mode"))
        }
        (ToolPreset::PlanReadOnly, "Bash" | "bash") => cmd_bash_plan_mode(args, cancel, update_callback).await,
        (_, "Bash" | "bash") => cmd_bash(args, cancel, update_callback).await,
        (_, "Read" | "read") => cmd_read(args, &cwd).await,
        (_, "Write" | "write") => cmd_write(args, &cwd).await,
        (_, "Edit" | "edit") => cmd_edit(args, &cwd).await,
        (_, "Grep" | "grep") => cmd_grep(args, &cwd).await,
        (_, "Find" | "find") => cmd_find(args, &cwd).await,
        (_, "Ls" | "ls") => cmd_ls(args, &cwd).await,
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn normalize_tool_name(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "bash" => Some("bash"),
        "read" => Some("read"),
        "write" => Some("write"),
        "edit" => Some("edit"),
        "grep" => Some("grep"),
        "find" => Some("find"),
        "ls" => Some("ls"),
        _ => None,
    }
}

fn supported_tool_names() -> [&'static str; 7] {
    ["bash", "read", "write", "edit", "grep", "find", "ls"]
}

// ---------------------------------------------------------------------------
// Individual tool handlers
// ---------------------------------------------------------------------------

fn is_allowed_plan_bash(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.chars().any(|ch| matches!(ch, '\n' | '\r' | '\t')) {
        return false;
    }
    let forbidden_fragments = ["&&", "||", ";", "|", ">", "<", "$(", "`"];
    if forbidden_fragments.iter().any(|fragment| trimmed.contains(fragment)) {
        return false;
    }
    PLAN_BASH_ALLOWLIST.iter().any(|prefix| trimmed == *prefix || trimmed.starts_with(&format!("{prefix} ")))
}

async fn cmd_bash(
    args: serde_json::Value,
    cancel: CancellationToken,
    update_callback: Option<ToolUpdateCallback>,
) -> Result<AgentToolResult, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'command' for Bash tool".to_string())?
        .to_string();
    let timeout = args.get("timeout").and_then(|v| v.as_u64());

    let input = BashInput { command, timeout };
    let streaming_callback: Option<ShellOutputCallback> = update_callback.map(|callback| {
        Arc::new(move |chunk: ShellOutputChunk| {
            callback(serde_json::json!({
                "stream": chunk.stream.as_str(),
                "chunk": chunk.text,
            }));
        }) as ShellOutputCallback
    });
    let result = if streaming_callback.is_some() {
        execute_bash_with_output_callback(&input, cancel, streaming_callback)
            .await
            .map_err(|e| format!("Bash error: {e}"))?
    } else {
        execute_bash(&input, cancel).await.map_err(|e| format!("Bash error: {e}"))?
    };

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent { text: result.output })],
        is_error: result.exit_code != 0,
        details: Some(serde_json::json!({
            "exit_code": result.exit_code,
            "truncated": result.truncated,
        })),
    })
}

async fn cmd_bash_plan_mode(
    args: serde_json::Value,
    cancel: CancellationToken,
    update_callback: Option<ToolUpdateCallback>,
) -> Result<AgentToolResult, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field 'command' for Bash tool".to_string())?;
    if !is_allowed_plan_bash(command) {
        return Err(format!(
            "Bash command '{}' is blocked in plan mode. Allowed prefixes: {}",
            command,
            PLAN_BASH_ALLOWLIST.join(", ")
        ));
    }
    cmd_bash(args, cancel, update_callback).await
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
    let result = execute_read(&input, cwd).await.map_err(|e| format!("Read error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent { text: result.content })],
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
    let result = execute_write(&input, cwd).await.map_err(|e| format!("Write error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent { text: result.message })],
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
            let old_text = e.get("oldText").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let new_text = e.get("newText").and_then(|v| v.as_str()).unwrap_or("").to_string();
            crate::tools::edit_diff::Edit { old_text, new_text }
        })
        .collect();

    if edits.is_empty() {
        return Err("Edit tool requires at least one edit entry".to_string());
    }

    let input = EditInput { path, edits };
    let result = execute_edit(&input, cwd).await.map_err(|e| format!("Edit error: {e}"))?;

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

    let input = GrepInput { pattern, path, glob, ignore_case, literal, context, limit };
    let result = execute_grep(&input, cwd).await.map_err(|e| format!("Grep error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent { text: result.output })],
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
    let result = execute_find(&input, cwd).await.map_err(|e| format!("Find error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent { text: result.output })],
        is_error: false,
        details: None,
    })
}

async fn cmd_ls(args: serde_json::Value, cwd: &std::path::Path) -> Result<AgentToolResult, String> {
    let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);

    let input = LsInput { path, limit };
    let result = execute_ls(&input, cwd).await.map_err(|e| format!("Ls error: {e}"))?;

    Ok(AgentToolResult {
        tool_call_id: String::new(),
        content: vec![ContentBlock::Text(TextContent { text: result.output })],
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

    #[test]
    fn test_plan_tool_preset_is_read_only() {
        let defs = tool_definitions_for_preset(ToolPreset::PlanReadOnly);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(defs.len(), 5, "plan mode should only expose read-only tools");
        assert!(names.contains(&"Bash"), "Bash missing: {names:?}");
        assert!(names.contains(&"Read"), "Read missing: {names:?}");
        assert!(names.contains(&"Grep"), "Grep missing: {names:?}");
        assert!(names.contains(&"Find"), "Find missing: {names:?}");
        assert!(names.contains(&"Ls"), "Ls missing: {names:?}");
        assert!(!names.contains(&"Write"), "Write should be hidden: {names:?}");
        assert!(!names.contains(&"Edit"), "Edit should be hidden: {names:?}");
    }

    #[test]
    fn test_plan_bash_allowlist_blocks_shell_operators() {
        assert!(is_allowed_plan_bash("rg plan src"));
        assert!(is_allowed_plan_bash("git status --short"));
        assert!(!is_allowed_plan_bash("find . -delete"));
        assert!(!is_allowed_plan_bash("sed -i 's/a/b/' file.txt"));
        assert!(!is_allowed_plan_bash("git diff --output=patch.txt"));
        assert!(!is_allowed_plan_bash("git status\ntouch /tmp/pwned"));
        assert!(!is_allowed_plan_bash("rg plan src && rm -rf /tmp/foo"));
        assert!(!is_allowed_plan_bash("python hack.py"));
    }

    #[test]
    fn test_tool_selection_allow_only_filters_definitions() {
        let selection = ToolSelection::allow_only(&["read".to_string(), "bash".to_string()]).unwrap();
        let defs = tool_definitions_for_selection(ToolPreset::Full, &selection);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Bash", "Read"]);
    }

    #[test]
    fn test_tool_selection_rejects_unknown_tool_name() {
        let err = ToolSelection::allow_only(&["unknown".to_string()]).unwrap_err();
        assert!(err.contains("Unknown tool"));
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
            .filter_map(|c| if let ContentBlock::Text(t) = c { Some(t.text.as_str()) } else { None })
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
        assert!(result.unwrap_err().contains("Unknown tool"), "should mention Unknown tool");
    }

    #[tokio::test]
    async fn test_execute_tool_for_plan_preset_blocks_write_tools() {
        let args = serde_json::json!({"path": "foo.txt", "content": "bar"});
        let cancel = CancellationToken::new();
        let result = execute_tool_for_preset("Write", args, cancel, ToolPreset::PlanReadOnly).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disabled in plan mode"));
    }

    #[tokio::test]
    async fn test_execute_tool_for_selection_blocks_disabled_tool() {
        let selection = ToolSelection::allow_only(&["read".to_string()]).unwrap();
        let cancel = CancellationToken::new();
        let err = execute_tool_for_selection(
            "Bash",
            serde_json::json!({ "command": "pwd" }),
            cancel,
            ToolPreset::Full,
            &selection,
        )
        .await
        .unwrap_err();
        assert!(err.contains("disabled by the current tool selection"));
    }

    #[tokio::test]
    async fn test_execute_tool_with_updates_streams_bash_chunks() {
        let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks_clone = chunks.clone();
        let callback: ToolUpdateCallback = Arc::new(move |partial| {
            if let Some(chunk) = partial.get("chunk").and_then(|value| value.as_str()) {
                chunks_clone.lock().unwrap().push(chunk.to_string());
            }
        });

        let result = execute_tool_for_selection_with_updates(
            "Bash",
            serde_json::json!({ "command": "printf streamed_tool_output" }),
            CancellationToken::new(),
            ToolPreset::Full,
            &ToolSelection::all(),
            Some(callback),
        )
        .await
        .unwrap();

        assert!(!result.is_error);
        let streamed = chunks.lock().unwrap().concat();
        assert!(streamed.contains("streamed_tool_output"), "streamed chunks: {streamed:?}");
    }
}
