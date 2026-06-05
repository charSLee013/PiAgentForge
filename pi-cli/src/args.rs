//! CLI argument parsing.
//! Mirrors packages/coding-agent/src/cli/args.ts

use clap::Parser;

/// Pi Coding Agent — a self-extensible coding agent CLI.
#[derive(Parser, Debug, Clone)]
#[command(name = "pi", version, about)]
pub struct Args {
    /// Continue the most recent session
    #[arg(long = "continue", short = 'c')]
    pub continue_recent: bool,

    /// Open a session selector to resume a session
    #[arg(long, short = 'r')]
    pub resume: bool,

    /// Provider to use (e.g., "openai", "anthropic")
    #[arg(long, short = 'p')]
    pub provider: Option<String>,

    /// Model to use
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// API key
    #[arg(long)]
    pub api_key: Option<String>,

    /// Base URL for the API provider (for OpenAI-compatible endpoints)
    #[arg(long)]
    pub base_url: Option<String>,

    /// System prompt
    #[arg(long)]
    pub system_prompt: Option<String>,

    /// Enable thinking mode
    #[arg(long)]
    pub thinking: bool,

    /// Thinking level to use: off, minimal, low, medium, high, xhigh
    #[arg(long, value_name = "LEVEL")]
    pub thinking_level: Option<String>,

    /// Comma-separated allowlist of built-in tools to enable
    #[arg(long, value_delimiter = ',', value_name = "TOOL", conflicts_with_all = ["no_tools", "no_builtin_tools"])]
    pub tools: Option<Vec<String>>,

    /// Disable all tools
    #[arg(long)]
    pub no_tools: bool,

    /// Disable built-in tools
    #[arg(long)]
    pub no_builtin_tools: bool,

    /// Interactive mode
    #[arg(long)]
    pub interactive: bool,

    /// Print mode (single response to stdout)
    #[arg(long)]
    pub print: bool,

    /// JSON output mode
    #[arg(long)]
    pub json: bool,

    /// Maximum number of agent turns before stopping
    #[arg(long, default_value_t = 200)]
    pub max_turns: u32,

    /// Stream bash stdout/stderr to stdout while the agent is running
    #[arg(long)]
    pub stream_stdout: bool,

    /// RPC mode
    #[arg(long)]
    pub rpc: bool,

    /// List available models
    #[arg(long, short = 'l')]
    pub list_models: bool,

    /// Session to resume
    #[arg(long)]
    pub session: Option<String>,

    /// Fork a specific session path or ID into a new session
    #[arg(
        long,
        value_name = "PATH|ID",
        conflicts_with_all = ["session", "continue_recent", "resume", "no_session"]
    )]
    pub fork: Option<String>,

    /// Session storage directory
    #[arg(long)]
    pub session_dir: Option<String>,

    /// Run without session persistence
    #[arg(long)]
    pub no_session: bool,

    /// Export a session JSONL file to HTML
    #[arg(long, value_name = "PATH|ID")]
    pub export: Option<String>,

    /// Force verbose startup logging
    #[arg(long)]
    pub verbose: bool,

    /// Login to an OAuth provider ("anthropic", "github-copilot", "openai-codex")
    #[arg(long, value_name = "PROVIDER")]
    pub login: Option<String>,

    /// The prompt to send
    pub prompt: Vec<String>,
}
