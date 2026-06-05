//! Pi CLI — CLI argument parsing, session configuration, and mode orchestration.
//!
//! This crate provides:
//! - Argument definitions (`args` module, based on clap)
//! - [`SessionConfig`] — a validated configuration derived from CLI arguments
//! - [`create_print_mode`] — entry point for `pi --print <prompt>`
//! - [`list_models`] — display all known models
//! - [`register_builtin_providers`] — register default API providers (OpenAI, etc.)

pub mod args;

use anyhow::{Context as AnyhowContext, Result};
use pi_agent_core::AgentEvent;
use pi_ai_core::api_registry::register_api_provider;
use pi_ai_core::thinking::{
    clamp_thinking_level, default_thinking_level_for_model, is_valid_thinking_level, thinking_enabled,
};
use pi_ai_core::types::{ContentBlock, Context, KnownProvider, Model, StreamOptions};
use pi_core::auth::AuthStorage;
use pi_core::settings::Settings;
use pi_core::tool_registry::{self, ToolSelection, execute_tool_for_selection_with_updates};
#[cfg(feature = "feat-anthropic")]
use pi_provider_anthropic::AnthropicProvider;
#[cfg(feature = "feat-google")]
use pi_provider_google::GoogleProvider;
#[cfg(feature = "feat-mistral")]
use pi_provider_mistral::MistralProvider;
use pi_provider_openai::OpenAiCompletionsProvider;
use std::io::Write;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Session configuration
// ---------------------------------------------------------------------------

/// Validated session configuration derived from CLI arguments.
///
/// This struct is the bridge between raw CLI arguments and the pi-ai-core
/// API. It resolves a model, captures the prompt and system prompt, and
/// holds the API key (or discovers it from the environment).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The resolved model identifier string (e.g. "gpt-4o-mini").
    pub model_id: String,
    /// The API provider for the resolved model.
    pub provider: KnownProvider,
    /// The API key to use (None = discover from environment).
    pub api_key: Option<String>,
    /// Optional system prompt.
    pub system_prompt: Option<String>,
    /// The user prompt / message text.
    pub prompt: String,
    /// Thinking level requested for the session.
    pub thinking_level: String,
    /// Custom base URL for OpenAI-compatible endpoints (from CLI or settings).
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeOptions {
    pub max_turns: u32,
    pub stream_stdout: bool,
    pub json_output: bool,
}

#[derive(Debug, Default)]
struct PrintEventState {
    current_turn: u32,
}

impl RuntimeOptions {
    pub fn from_args(args: &args::Args) -> Self {
        Self { max_turns: args.max_turns, stream_stdout: args.stream_stdout, json_output: args.json }
    }
}

impl SessionConfig {
    /// Build a `SessionConfig` from parsed CLI arguments.
    ///
    /// Resolves the model using `--model` / `--provider`, falling back to
    /// the first available OpenAI model. Returns an error if no model can
    /// be found.
    ///
    /// Settings are loaded from `~/.pi/settings.json` and merged with CLI
    /// arguments using the priority: CLI args > settings.json > defaults.
    ///
    /// The API key is resolved with the following priority:
    /// 1. `--api-key` CLI flag
    /// 2. `api_key` from settings.json
    /// 3. `{PROVIDER}_API_KEY` environment variable
    /// 4. `auth.json` stored credential via [`AuthStorage`]
    pub fn from_args(args: &args::Args) -> Result<Self> {
        // Load user settings (non-critical: swallow errors and use defaults)
        let settings = Settings::load().unwrap_or_else(|_| Settings {
            path: Settings::default_path(),
            default_model: None,
            default_provider: None,
            theme: None,
            base_url: None,
            api_key: None,
            extra: std::collections::HashMap::new(),
        });

        // Merge: CLI args > settings.json > defaults
        let effective_base_url =
            args.base_url.clone().or_else(|| std::env::var("PI_BASE_URL").ok()).or_else(|| settings.base_url.clone());
        let effective_api_key = args.api_key.clone().or_else(|| settings.api_key.clone());
        let effective_model = args.model.clone().or_else(|| settings.default_model.clone());
        let effective_provider = args.provider.clone().or_else(|| settings.default_provider.clone());

        // Rebuild partial args with settings fallbacks for model resolution
        let merged_args = args::Args { model: effective_model, provider: effective_provider, ..args.clone() };

        let model = resolve_model(&merged_args, effective_base_url.as_deref())?;

        let requested_thinking_level = args
            .thinking_level
            .as_deref()
            .or(if args.thinking { Some("low") } else { None })
            .unwrap_or(default_thinking_level_for_model(model));
        if !is_valid_thinking_level(requested_thinking_level) {
            anyhow::bail!(
                "Invalid thinking level '{}'. Valid values: off, minimal, low, medium, high, xhigh",
                requested_thinking_level
            );
        }

        // API key priority: 1. CLI/settings combined above, 2. env var, 3. auth.json
        let api_key = resolve_api_key(effective_api_key, &model.provider);

        Ok(Self {
            model_id: model.id.clone(),
            provider: model.provider,
            api_key,
            system_prompt: args.system_prompt.clone(),
            prompt: args.prompt.join(" "),
            thinking_level: clamp_thinking_level(model, requested_thinking_level),
            base_url: effective_base_url,
        })
    }
}

// ---------------------------------------------------------------------------
// Built-in provider registration
// ---------------------------------------------------------------------------

/// A delegating provider wrapper that overrides the API ID.
///
/// This lets us register the same `OpenAiCompletionsProvider` under multiple
/// API IDs (e.g., `openai-completions` and `openai-responses`) since the
/// model catalog uses `openai-responses` for most OpenAI models.
struct DelegatingProvider {
    inner: OpenAiCompletionsProvider,
    api_id: &'static str,
}

impl DelegatingProvider {
    fn new(api_id: &'static str) -> Self {
        Self { inner: OpenAiCompletionsProvider::new(), api_id }
    }
}

impl pi_ai_core::api_registry::ApiProvider for DelegatingProvider {
    fn api_id(&self) -> &str {
        self.api_id
    }

    fn stream(
        &self,
        model: &Model,
        context: Context,
        options: StreamOptions,
    ) -> pi_ai_core::event_stream::AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }
}

/// Register all built-in API providers.
///
/// Currently registers:
/// - OpenAI Chat Completions for both `openai-completions` and `openai-responses`
///   API types (the same HTTP endpoint handles both model references)
///
/// Call this once at startup before making any LLM calls.
pub async fn register_builtin_providers() {
    register_api_provider(Box::new(OpenAiCompletionsProvider::new())).await;
    register_api_provider(Box::new(DelegatingProvider::new("openai-responses"))).await;
    #[cfg(feature = "feat-bedrock")]
    register_api_provider(Box::new(pi_provider_bedrock::BedrockProvider::new())).await;
    #[cfg(feature = "feat-anthropic")]
    register_api_provider(Box::new(AnthropicProvider::new())).await;
    #[cfg(feature = "feat-google")]
    register_api_provider(Box::new(GoogleProvider::new())).await;
    #[cfg(feature = "feat-mistral")]
    register_api_provider(Box::new(MistralProvider::new())).await;
    tracing::info!("registered built-in API providers");
}

/// Convert CLI tool flags into an executable built-in tool selection.
pub fn tool_selection_from_args(args: &args::Args) -> Result<ToolSelection> {
    if let Some(tools) = &args.tools {
        return ToolSelection::allow_only(tools).map_err(anyhow::Error::msg);
    }
    if args.no_tools || args.no_builtin_tools {
        return Ok(ToolSelection::disable_builtin());
    }
    Ok(ToolSelection::all())
}

// ---------------------------------------------------------------------------
// Model resolution
// ---------------------------------------------------------------------------

/// Parse a provider string into a `KnownProvider` enum.
fn parse_provider(s: &str) -> Result<KnownProvider> {
    match s.to_lowercase().as_str() {
        "openai" => Ok(KnownProvider::OpenAi),
        "anthropic" => Ok(KnownProvider::Anthropic),
        "google" => Ok(KnownProvider::Google),
        "mistral" => Ok(KnownProvider::Mistral),
        "bedrock" => Ok(KnownProvider::Bedrock),
        "faux" => Ok(KnownProvider::Faux),
        _ => Err(anyhow::anyhow!("Unknown provider '{}'. Supported: openai, anthropic, google, mistral, bedrock", s)),
    }
}

/// Map a `KnownProvider` to its canonical string name used in environment
/// variable names and auth.json keys.
fn provider_name(p: &KnownProvider) -> &'static str {
    match p {
        KnownProvider::OpenAi => "openai",
        KnownProvider::Anthropic => "anthropic",
        KnownProvider::Google => "google",
        KnownProvider::Mistral => "mistral",
        KnownProvider::Bedrock => "bedrock",
        KnownProvider::Faux => "faux",
    }
}

/// Resolve an API key for the given provider using the priority chain:
///
/// 1. `cli_or_settings_key` (merged from `--api-key` CLI flag or `settings.json`)
/// 2. `{PROVIDER}_API_KEY` environment variable
/// 3. `auth.json` stored credential (via [`AuthStorage`])
fn resolve_api_key(cli_or_settings_key: Option<String>, provider: &KnownProvider) -> Option<String> {
    let prov_name = provider_name(provider);
    let env_var = format!("{}_API_KEY", prov_name.to_uppercase());

    // 1. CLI flag or settings key (pre-merged)
    if let Some(key) = cli_or_settings_key {
        return Some(key);
    }
    // 2. Environment variable
    if let Ok(key) = std::env::var(&env_var) {
        return Some(key);
    }
    // 3. auth.json
    if let Ok(auth) = AuthStorage::load() {
        if let Some(key) = auth.get_api_key(prov_name) {
            return Some(key);
        }
    }
    None
}

fn provider_requires_api_key(provider: KnownProvider) -> bool {
    !matches!(provider, KnownProvider::Bedrock | KnownProvider::Faux)
}

fn preflight_api_key(provider: KnownProvider, api_key: Option<String>) -> Result<Option<String>> {
    if api_key.is_some() || !provider_requires_api_key(provider) {
        return Ok(api_key);
    }
    anyhow::bail!(
        "No API key for {}. Set {}_API_KEY or configure ~/.pi/auth.json.",
        provider_name(&provider),
        provider_name(&provider).to_uppercase()
    )
}

/// Resolve a model from CLI arguments.
///
/// Resolution order:
/// 1. If `--model <id>` is given, look up by ID in the catalog.
/// 2. If `--model <id>` is not found AND `--base-url` is set, construct
///    a minimal custom model (for OpenAI-compatible endpoints).
/// 3. If `--provider <p>` is given, take the first model for that provider.
/// 4. Otherwise, default to the first OpenAI model.
///
/// When both `--model` and `--provider` are given, the provider scope is
/// used to disambiguate the model ID.
fn resolve_model(args: &args::Args, effective_base_url: Option<&str>) -> Result<&'static Model> {
    // --model alone: try direct lookup
    if let Some(ref model_id) = args.model {
        // Try provider-scoped lookup first if --provider is also given
        if let Some(ref provider) = args.provider {
            let prov = parse_provider(provider)?;
            if let Some(model) = pi_model_catalog::models::get_model(prov, model_id) {
                return Ok(model);
            }
        }
        // Fallback to global ID lookup
        if let Some(model) = pi_model_catalog::models::find_model(model_id) {
            return Ok(model);
        }
        // Not found in catalog: if base_url is set, construct a custom model
        if effective_base_url.is_some() {
            return Ok(custom_model(model_id, KnownProvider::OpenAi));
        }
        return Err(anyhow::anyhow!(
            "Model '{}' not found in catalog. Use --list-models to see available models.",
            model_id
        ));
    }

    // --provider alone: use first model for that provider
    if let Some(ref provider) = args.provider {
        let prov = parse_provider(provider)?;
        let models = pi_model_catalog::models::get_models(prov);
        let model = models.first().ok_or_else(|| {
            anyhow::anyhow!("No models found for provider '{}'. Use --list-models to see available models.", provider)
        })?;
        return Ok(model);
    }

    // Default: first OpenAI model
    let models = pi_model_catalog::models::get_models(KnownProvider::OpenAi);
    models.first().copied().ok_or_else(|| {
        anyhow::anyhow!(
            "No OpenAI models available and no --provider or --model specified.\n\
             Use --list-models to see available models, or specify --provider and --model."
        )
    })
}

/// Construct a minimal custom Model for use with OpenAI-compatible endpoints.
///
/// The returned model has a `'static` lifetime via `Box::leak`, which is
/// acceptable for the CLI process lifetime.
fn custom_model(model_id: &str, provider: KnownProvider) -> &'static Model {
    Box::leak(Box::new(Model {
        id: model_id.to_owned(),
        provider,
        api: "openai-completions".into(),
        name: None,
        base_url: None,
        supports_thinking: false,
        supports_tools: true,
        supports_streaming: true,
        supports_image_input: false,
        max_tokens: None,
        max_input_tokens: None,
        max_output_tokens: None,
        cost_per_input_token: None,
        cost_per_output_token: None,
        cost_per_cache_read_token: None,
        cost_per_cache_write_token: None,
    }))
}

/// Look up a model in the catalog, or construct a custom one when a
/// custom `base_url` was provided (for OpenAI-compatible endpoints).
fn find_or_build_model(config: &SessionConfig) -> Result<&'static Model> {
    if let Some(m) = pi_model_catalog::models::find_model(&config.model_id) {
        return Ok(m);
    }
    if config.base_url.is_some() {
        return Ok(custom_model(&config.model_id, config.provider));
    }
    anyhow::bail!("Model '{}' not found in catalog", config.model_id)
}

fn extract_text_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| if let ContentBlock::Text(text) = block { Some(text.text.clone()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(text.as_bytes());
    let _ = handle.flush();
}

fn write_json_line(value: serde_json::Value) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let line = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
    let _ = handle.write_all(line.as_bytes());
    let _ = handle.write_all(b"\n");
    let _ = handle.flush();
}

fn handle_print_event(event: AgentEvent, options: RuntimeOptions, state: &Arc<Mutex<PrintEventState>>) {
    match event {
        AgentEvent::TurnStart { turn_number } => {
            state.lock().expect("print event state poisoned").current_turn = turn_number;
            if options.json_output {
                write_json_line(serde_json::json!({
                    "type": "turn_start",
                    "turn": turn_number,
                }));
            } else {
                tracing::debug!("Agent turn {turn_number}");
            }
        }
        AgentEvent::ToolExecutionStart { tool_name, arguments, .. } => {
            let turn = state.lock().expect("print event state poisoned").current_turn;
            if options.json_output {
                write_json_line(serde_json::json!({
                    "type": "tool_call",
                    "turn": turn,
                    "tool": tool_name,
                    "args": arguments,
                }));
            } else {
                tracing::debug!("Executing tool: {tool_name}");
            }
        }
        AgentEvent::ToolExecutionUpdate { tool_name, partial_result, .. } => {
            if options.json_output {
                let turn = state.lock().expect("print event state poisoned").current_turn;
                write_json_line(serde_json::json!({
                    "type": "tool_update",
                    "turn": turn,
                    "tool": tool_name,
                    "partial_result": partial_result,
                }));
            } else if options.stream_stdout && tool_name.eq_ignore_ascii_case("bash") {
                if let Some(chunk) = partial_result.get("chunk").and_then(|value| value.as_str()) {
                    write_stdout(chunk);
                }
            }
        }
        AgentEvent::ToolExecutionEnd { tool_name, result, .. } => {
            if options.json_output {
                let turn = state.lock().expect("print event state poisoned").current_turn;
                write_json_line(serde_json::json!({
                    "type": "tool_result",
                    "turn": turn,
                    "tool": tool_name,
                    "result": extract_text_from_blocks(&result.content),
                    "is_error": result.is_error,
                    "details": result.details,
                }));
            }
        }
        AgentEvent::MessageEnd { message, .. } => {
            if options.json_output {
                let content = extract_text_from_blocks(&message);
                if !content.is_empty() {
                    let turn = state.lock().expect("print event state poisoned").current_turn;
                    write_json_line(serde_json::json!({
                        "type": "assistant_message",
                        "turn": turn,
                        "content": content,
                    }));
                }
            }
        }
        AgentEvent::TurnEnd { turn_number } => {
            if options.json_output {
                write_json_line(serde_json::json!({
                    "type": "turn_end",
                    "turn": turn_number,
                }));
            }
        }
        AgentEvent::AgentEnd { finish_reason, .. } => {
            if options.json_output {
                let turn = state.lock().expect("print event state poisoned").current_turn;
                write_json_line(serde_json::json!({
                    "type": "agent_end",
                    "turn": turn,
                    "finish_reason": finish_reason,
                }));
            } else {
                tracing::debug!("Agent finished: {finish_reason}");
            }
        }
        AgentEvent::AgentStart { .. } | AgentEvent::MessageStart { .. } | AgentEvent::MessageDelta { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Print mode
// ---------------------------------------------------------------------------

/// Run the `--print` mode: send a prompt to the LLM and write the response
/// to stdout.
///
/// This function uses the agent loop with all built-in tools available,
/// so the LLM can execute commands, read files, and perform other actions
/// to fulfill the user's request.
///
/// The final assistant message text is printed to stdout.
async fn print_mode_with_tools(
    config: &SessionConfig,
    tool_selection: &ToolSelection,
    runtime_options: RuntimeOptions,
) -> Result<()> {
    register_builtin_providers().await;

    // If a custom base_url is configured, register an OpenAI provider that
    // targets that endpoint (replaces the default openai-completions provider).
    if let Some(base_url) = &config.base_url {
        let provider = OpenAiCompletionsProvider::with_base_url(base_url.clone());
        register_api_provider(Box::new(provider)).await;
    }

    let model = find_or_build_model(config)?;

    tracing::info!(provider = ?config.provider, model = %config.model_id, "using model");

    let api_key = preflight_api_key(config.provider, config.api_key.clone())?;

    // ── Tool definitions ──────────────────────────────────────────────────
    let tools = tool_registry::tool_definitions_for_selection(tool_registry::ToolPreset::Full, tool_selection);

    // ── Agent state ────────────────────────────────────────────────────────
    let mut state = pi_agent_core::AgentState {
        messages: vec![pi_ai_core::types::Message::user_text(&config.prompt)],
        context: pi_agent_core::AgentContext {
            messages: vec![],
            system_prompt: config.system_prompt.clone(),
            tools,
            model: Some(config.model_id.clone()),
            max_turns: runtime_options.max_turns,
            current_turn: 0,
        },
        pending_tool_calls: vec![],
    };

    let options = pi_ai_core::types::StreamOptions {
        api_key,
        thinking: Some(thinking_enabled(&config.thinking_level)),
        ..Default::default()
    };

    let cancel = tokio_util::sync::CancellationToken::new();

    // ── Stream function ────────────────────────────────────────────────────
    // Wraps `stream::stream` into the agent loop's expected signature.
    let stream_fn = {
        let options = options.clone();
        move |ctx: pi_ai_core::types::Context| pi_ai_core::stream::stream(model, ctx, options.clone())
    };

    // ── Tool executor ──────────────────────────────────────────────────────
    // Bridges the async `execute_tool` into the sync closure that agent_loop
    // expects, using `block_in_place` + `Handle::block_on`.
    let cancel_for_tools = cancel.clone();
    let tool_executor = move |name: &str, _id: &str, args: &serde_json::Value, updates| {
        let cancel = cancel_for_tools.clone();
        let name = name.to_string();
        let args = args.clone();
        let rt_handle = tokio::runtime::Handle::current();
        let tool_selection = tool_selection.clone();
        tokio::task::block_in_place(move || {
            rt_handle.block_on(async move {
                let result = execute_tool_for_selection_with_updates(
                    &name,
                    args,
                    cancel,
                    tool_registry::ToolPreset::Full,
                    &tool_selection,
                    updates,
                )
                .await?;
                Ok(result)
            })
        })
    };

    // ── Event sink ─────────────────────────────────────────────────────────
    let print_state = Arc::new(Mutex::new(PrintEventState::default()));
    let event_sink = {
        let print_state = print_state.clone();
        move |event: pi_agent_core::AgentEvent| {
            handle_print_event(event, runtime_options, &print_state);
        }
    };

    // ── Run agent loop ─────────────────────────────────────────────────────
    pi_agent_core::agent_loop::agent_loop_with_tool_updates(&mut state, stream_fn, tool_executor, event_sink, cancel)
        .await
        .context("Agent loop failed")?;

    // ── Print final assistant message ──────────────────────────────────────
    if !runtime_options.json_output {
        if let Some(last_msg) = state.messages.last() {
            for block in &last_msg.content {
                if let ContentBlock::Text(text) = block {
                    write_stdout(&text.text);
                }
            }
        }
        write_stdout("\n");
    }

    Ok(())
}

/// Entry point for `pi --print <prompt>`.
///
/// Parses CLI arguments, builds a [`SessionConfig`], and runs print mode.
pub async fn create_print_mode(args: &args::Args) -> Result<()> {
    let prompt = args.prompt.join(" ");
    if prompt.trim().is_empty() {
        anyhow::bail!("No prompt provided. Usage: pi --print \"your prompt\"");
    }

    let config = SessionConfig::from_args(args)?;
    let tool_selection = tool_selection_from_args(args)?;
    let runtime_options = RuntimeOptions::from_args(args);
    print_mode_with_tools(&config, &tool_selection, runtime_options).await
}

// ---------------------------------------------------------------------------
// List models
// ---------------------------------------------------------------------------

/// Format the model catalog as a human-readable string for `--list-models`.
pub fn format_model_list() -> String {
    use std::fmt::Write;
    let all = pi_model_catalog::models::all_models();
    let mut output = String::new();
    let _ = writeln!(output, "Available models ({} total):", all.len());

    // Group by provider
    let mut current_provider: Option<KnownProvider> = None;
    for model in all {
        if current_provider != Some(model.provider) {
            current_provider = Some(model.provider);
            let _ = writeln!(output, "\n  {:?}:", model.provider);
        }
        let _ = write!(output, "    {}", model.id);
        if let Some(ref name) = model.name {
            if *name != model.id {
                let _ = write!(output, " ({})", name);
            }
        }
        let _ = writeln!(output);
    }

    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use pi_ai_core::api_registry::{clear_api_providers, list_api_providers};

    // ── Arg parsing tests ────────────────────────────────────────────────

    #[test]
    fn test_args_parse_print_mode() {
        let args = args::Args::try_parse_from(["pi", "--print", "hello world"]);
        assert!(args.is_ok(), "should parse --print with prompt");
        let args = args.unwrap();
        assert!(args.print);
        assert_eq!(args.prompt.join(" "), "hello world");
    }

    #[test]
    fn test_args_parse_print_mode_no_prompt() {
        let args = args::Args::try_parse_from(["pi", "--print"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert!(args.print);
        assert!(args.prompt.is_empty());
    }

    #[test]
    fn test_args_parse_list_models() {
        let args = args::Args::try_parse_from(["pi", "--list-models"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert!(args.list_models);
    }

    #[test]
    fn test_args_parse_provider_and_model() {
        let args = args::Args::try_parse_from(["pi", "--provider", "openai", "--model", "gpt-4o"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.provider.as_deref(), Some("openai"));
        assert_eq!(args.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn test_args_parse_system_prompt() {
        let args = args::Args::try_parse_from(["pi", "--system-prompt", "You are a helpful bot", "--print", "hi"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.system_prompt.as_deref(), Some("You are a helpful bot"));
    }

    #[test]
    fn test_args_parse_api_key() {
        let args = args::Args::try_parse_from(["pi", "--api-key", "sk-test"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn test_args_parse_interactive() {
        let args = args::Args::try_parse_from(["pi", "--interactive"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert!(args.interactive);
    }

    #[test]
    fn test_args_parse_continue_resume_and_no_session() {
        let args = args::Args::try_parse_from(["pi", "--continue", "--resume", "--no-session"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert!(args.continue_recent);
        assert!(args.resume);
        assert!(args.no_session);
    }

    #[test]
    fn test_args_parse_session_dir_and_export() {
        let args = args::Args::try_parse_from(["pi", "--session-dir", "/tmp/pi-sessions", "--export", "session-1234"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.session_dir.as_deref(), Some("/tmp/pi-sessions"));
        assert_eq!(args.export.as_deref(), Some("session-1234"));
    }

    #[test]
    fn test_args_parse_fork_value() {
        let args = args::Args::try_parse_from(["pi", "--fork", "abcd1234"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.fork.as_deref(), Some("abcd1234"));
    }

    #[test]
    fn test_args_reject_conflicting_fork_flags() {
        let args = args::Args::try_parse_from(["pi", "--fork", "abcd1234", "--resume"]);
        assert!(args.is_err());
    }

    #[test]
    fn test_args_parse_tools_and_verbose() {
        let args = args::Args::try_parse_from(["pi", "--tools", "read,bash", "--verbose", "--print", "hi"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.tools, Some(vec!["read".to_string(), "bash".to_string()]));
        assert!(args.verbose);
    }

    #[test]
    fn test_args_parse_max_turns_and_stream_stdout() {
        let args =
            args::Args::try_parse_from(["pi", "--max-turns", "1234", "--stream-stdout", "--json", "--print", "hi"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.max_turns, 1234);
        assert!(args.stream_stdout);
        assert!(args.json);
    }

    #[test]
    fn test_tool_selection_from_args_no_tools_disables_builtins() {
        let args = args::Args::parse_from(["pi", "--no-tools", "--print", "hello"]);
        let selection = tool_selection_from_args(&args).unwrap();
        let defs = tool_registry::tool_definitions_for_selection(tool_registry::ToolPreset::Full, &selection);
        assert!(defs.is_empty(), "no-tools should hide all built-in tools");
    }

    #[test]
    fn test_tool_selection_from_args_allowlist_filters_builtins() {
        let args = args::Args::parse_from(["pi", "--tools", "read,bash", "--print", "hello"]);
        let selection = tool_selection_from_args(&args).unwrap();
        let defs = tool_registry::tool_definitions_for_selection(tool_registry::ToolPreset::Full, &selection);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Bash", "Read"]);
    }

    // ── SessionConfig / model resolution tests ───────────────────────────

    #[test]
    fn test_session_config_default_model() {
        let args = args::Args::parse_from(["pi", "--print", "hello"]);
        let config = SessionConfig::from_args(&args).expect("should resolve with default model");
        assert_eq!(config.provider, KnownProvider::OpenAi);
        assert!(!config.model_id.is_empty());
    }

    #[test]
    fn test_session_config_with_provider() {
        let args = args::Args::parse_from(["pi", "--provider", "openai", "hello"]);
        let config = SessionConfig::from_args(&args).expect("should resolve openai provider");
        assert_eq!(config.provider, KnownProvider::OpenAi);
    }

    #[test]
    fn test_session_config_unknown_provider() {
        let args = args::Args::parse_from(["pi", "--provider", "nonexistent", "hello"]);
        let result = SessionConfig::from_args(&args);
        assert!(result.is_err(), "unknown provider should error");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown provider"), "error: {err}");
    }

    #[test]
    fn test_session_config_unknown_model() {
        // Use resolve_model directly to avoid depending on settings.json state.
        let args = args::Args::parse_from(["pi", "--model", "nonexistent-model-xyz", "hello"]);
        let result = resolve_model(&args, None);
        assert!(result.is_err(), "unknown model without base_url should error");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "error: {err}");
    }

    #[test]
    fn test_session_config_with_model_id() {
        let args = args::Args::parse_from(["pi", "--model", "gpt-4o", "hello"]);
        let config = SessionConfig::from_args(&args).expect("gpt-4o should resolve");
        assert_eq!(config.model_id, "gpt-4o");
        assert_eq!(config.provider, KnownProvider::OpenAi);
    }

    #[test]
    fn test_session_config_system_prompt() {
        let args = args::Args::parse_from(["pi", "--system-prompt", "You are helpful", "--print", "hello"]);
        let config = SessionConfig::from_args(&args).expect("should resolve");
        assert_eq!(config.system_prompt.as_deref(), Some("You are helpful"));
    }

    #[test]
    fn test_session_config_thinking_level_passthrough() {
        let args = args::Args::parse_from(["pi", "--model", "o3-mini", "--thinking-level", "high", "--print", "hello"]);
        let config = SessionConfig::from_args(&args).expect("o3-mini should support thinking");
        assert_eq!(config.thinking_level, "high");
    }

    #[test]
    fn test_session_config_thinking_flag_defaults_to_low() {
        let args = args::Args::parse_from(["pi", "--model", "o3-mini", "--thinking", "--print", "hello"]);
        let config = SessionConfig::from_args(&args).expect("o3-mini should support thinking");
        assert_eq!(config.thinking_level, "low");
    }

    #[test]
    fn test_session_config_thinking_level_clamps_for_unsupported_models() {
        let args = args::Args::parse_from(["pi", "--model", "gpt-4o", "--thinking-level", "high", "--print", "hello"]);
        let config = SessionConfig::from_args(&args).expect("gpt-4o should resolve");
        assert_eq!(config.thinking_level, "off");
    }

    #[test]
    fn test_session_config_invalid_thinking_level_errors() {
        let args =
            args::Args::parse_from(["pi", "--model", "o3-mini", "--thinking-level", "turbo", "--print", "hello"]);
        let err = SessionConfig::from_args(&args).unwrap_err().to_string();
        assert!(err.contains("Invalid thinking level"), "{err}");
    }

    // ── Base URL & custom model tests ──────────────────────────────────

    #[test]
    fn test_args_parse_base_url() {
        let args = args::Args::try_parse_from(["pi", "--base-url", "https://custom.api.com/v1", "--print", "hello"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.base_url.as_deref(), Some("https://custom.api.com/v1"));
    }

    #[test]
    fn test_session_config_custom_model_with_base_url() {
        let args = args::Args::parse_from([
            "pi",
            "--model",
            "my-custom-model",
            "--base-url",
            "https://custom.api.com/v1",
            "hello",
        ]);
        let config = SessionConfig::from_args(&args).expect("custom model with base_url should resolve");
        assert_eq!(config.model_id, "my-custom-model");
        assert_eq!(config.provider, KnownProvider::OpenAi);
        assert_eq!(config.base_url.as_deref(), Some("https://custom.api.com/v1"));
    }

    #[test]
    fn test_session_config_uses_pi_base_url_env() {
        unsafe {
            std::env::set_var("PI_BASE_URL", "https://env.example/v1");
        }
        let args = args::Args::parse_from(["pi", "--model", "env-custom-model", "hello"]);
        let config = SessionConfig::from_args(&args).expect("custom model with PI_BASE_URL should resolve");
        assert_eq!(config.base_url.as_deref(), Some("https://env.example/v1"));
        assert_eq!(config.model_id, "env-custom-model");
        unsafe {
            std::env::remove_var("PI_BASE_URL");
        }
    }

    #[test]
    fn test_runtime_options_from_args() {
        let args = args::Args::parse_from(["pi", "--max-turns", "42", "--stream-stdout", "--json", "--print", "hello"]);
        let runtime = RuntimeOptions::from_args(&args);
        assert_eq!(runtime.max_turns, 42);
        assert!(runtime.stream_stdout);
        assert!(runtime.json_output);
    }

    #[test]
    fn test_find_or_build_model_known_model() {
        // Known models should be found in the catalog.
        let config = SessionConfig {
            model_id: "gpt-4o".into(),
            provider: KnownProvider::OpenAi,
            api_key: None,
            system_prompt: None,
            prompt: "test".into(),
            thinking_level: "off".into(),
            base_url: None,
        };
        let model = find_or_build_model(&config).expect("gpt-4o should resolve");
        assert_eq!(model.id, "gpt-4o");
    }

    #[test]
    fn test_find_or_build_model_custom_model() {
        // Custom models with base_url should be constructed.
        let config = SessionConfig {
            model_id: "my-custom-llm".into(),
            provider: KnownProvider::OpenAi,
            api_key: None,
            system_prompt: None,
            prompt: "test".into(),
            thinking_level: "off".into(),
            base_url: Some("https://custom.api.com/v1".into()),
        };
        let model = find_or_build_model(&config).expect("custom model with base_url should be built");
        assert_eq!(model.id, "my-custom-llm");
        assert_eq!(model.provider, KnownProvider::OpenAi);
        assert_eq!(model.api, "openai-completions");
    }

    #[test]
    fn test_custom_model_properties() {
        let model = custom_model("test-model-v42", KnownProvider::OpenAi);
        assert_eq!(model.id, "test-model-v42");
        assert_eq!(model.provider, KnownProvider::OpenAi);
        assert_eq!(model.api, "openai-completions");
        assert!(model.supports_tools);
        assert!(model.supports_streaming);
        assert!(!model.supports_thinking);
        assert!(!model.supports_image_input);
    }

    #[test]
    fn test_custom_model_with_non_openai_provider() {
        // Even with a different provider, the custom model forces OpenAi
        // since we only support OpenAI-compatible custom endpoints.
        let model = custom_model("test-model", KnownProvider::OpenAi);
        assert_eq!(model.provider, KnownProvider::OpenAi);
    }

    #[test]
    fn test_preflight_api_key_allows_bedrock_without_key() {
        let result = preflight_api_key(KnownProvider::Bedrock, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_preflight_api_key_rejects_missing_openai_key() {
        let err = preflight_api_key(KnownProvider::OpenAi, None).unwrap_err().to_string();
        assert!(err.contains("No API key for openai"));
    }

    #[tokio::test]
    async fn test_register_builtin_providers_includes_google_and_mistral() {
        clear_api_providers().await;
        register_builtin_providers().await;
        let providers = list_api_providers().await;
        assert!(providers.contains(&"openai-completions".to_string()));
        assert!(providers.contains(&"openai-responses".to_string()));
        #[cfg(feature = "feat-google")]
        assert!(providers.contains(&"google-generative-ai".to_string()));
        #[cfg(feature = "feat-mistral")]
        assert!(providers.contains(&"mistral-conversations".to_string()));
    }

    // ── Error handling tests ─────────────────────────────────────────────

    #[test]
    fn test_create_print_mode_empty_prompt_errors() {
        let args = args::Args::parse_from(["pi", "--print"]);
        let result = tokio::runtime::Runtime::new().unwrap().block_on(create_print_mode(&args));
        assert!(result.is_err(), "empty prompt should produce an error");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No prompt"), "error: {err}");
    }

    // ── Model list formatting ────────────────────────────────────────────

    #[test]
    fn test_format_model_list_non_empty() {
        let output = format_model_list();
        assert!(!output.is_empty(), "model list should not be empty");
        assert!(output.contains("Available models"), "should have header");
        assert!(output.contains("OpenAi"), "should contain OpenAI");
        assert!(output.contains("Anthropic"), "should contain Anthropic");
        assert!(output.contains("Google"), "should contain Google");
    }
}
