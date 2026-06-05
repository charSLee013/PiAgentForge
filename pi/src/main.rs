//! Pi binary — main entry point.
//!
//! Usage:
//!   pi --print "hello"          Non-interactive: send prompt and print response
//!   pi --list-models            List all available models from the catalog
//!   pi --interactive            Interactive TUI mode
//!   pi --login anthropic        Login to an OAuth provider
//!   pi --help                   Show help
//!   pi --version                Show version

use std::path::{Path, PathBuf};

use clap::Parser;
use pi_agent_core::session::{
    SessionManager, clone_active_path_to_file, export_session_as_html, find_most_recent_session, read_all,
    resolve_session_id_prefix,
};
use pi_core::auth::AuthStorage;
use pi_oauth::OAuthProvider;
use tracing_subscriber::EnvFilter;

/// CLI entry point.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let force_verbose = std::env::args().any(|arg| arg == "--verbose");
    // Initialize tracing with sensible defaults.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| if force_verbose { EnvFilter::new("debug") } else { EnvFilter::new("info") }),
        )
        .init();

    let args = pi_cli::args::Args::parse();

    // Handle --login before routing to other modes.
    if let Some(provider) = &args.login {
        return handle_login(provider).await;
    }

    if let Some(export_spec) = &args.export {
        return export_session(export_spec, &args).await;
    }

    // Route to the appropriate mode.
    if args.rpc {
        return pi_modes::rpc::server::run_rpc_server_with_max_turns(args.max_turns).await;
    } else if args.list_models {
        list_models().await?;
    } else if args.print {
        pi_cli::create_print_mode(&args).await?;
    } else if args.interactive
        || args.continue_recent
        || args.resume
        || args.no_session
        || args.session.is_some()
        || args.fork.is_some()
        || args.session_dir.is_some()
    {
        run_interactive(&args).await?;
    } else if !args.prompt.is_empty() {
        // Fallback: positional prompt without --print flag → treat as print mode
        pi_cli::create_print_mode(&args).await?;
    } else {
        // No recognised mode and no prompt — show help.
        anyhow::bail!(
            "No command specified.\n\
             Usage: pi --print \"your prompt\"\n\
                    pi --list-models\n\
                    pi --interactive\n\
                    pi --login <provider>\n\
                    pi --help"
        );
    }

    tracing::info!("pi agent finished successfully");
    Ok(())
}

/// Display all available models from the catalog and exit.
async fn list_models() -> anyhow::Result<()> {
    let output = pi_cli::format_model_list();
    print!("{output}");
    Ok(())
}

/// Determine the session file path for interactive mode.
///
/// Resolution order:
/// 1. `--session <path>` overrides everything and returns that path as-is.
/// 2. `--session-dir <dir>` chooses the session directory.
/// 3. The `PI_SESSION_DIR` environment variable sets the directory.
/// 4. Defaults to `~/.pi/sessions/` via the `dirs` crate.
///
/// The file name uses the current timestamp and the model ID, e.g.
/// `20260515_143022-gpt-4o-mini.jsonl`.
fn resolve_session_dir(args: &pi_cli::args::Args) -> PathBuf {
    if let Some(dir) = &args.session_dir {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("PI_SESSION_DIR") {
        return PathBuf::from(dir);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("sessions")
}

fn looks_like_session_path(spec: &str) -> bool {
    spec.contains('/') || spec.contains('\\') || spec.ends_with(".jsonl")
}

async fn resolve_session_reference(spec: &str, session_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    if looks_like_session_path(spec) {
        return Ok(Some(PathBuf::from(spec)));
    }
    resolve_session_id_prefix(session_dir, spec).await.map_err(anyhow::Error::from)
}

async fn resolve_interactive_session_path(
    args: &pi_cli::args::Args,
    session_dir: &Path,
    model_id: &str,
) -> anyhow::Result<Option<PathBuf>> {
    if args.no_session {
        return Ok(None);
    }

    if let Some(source_spec) = &args.fork {
        let Some(source_path) = resolve_session_reference(source_spec, session_dir).await? else {
            anyhow::bail!("No session found matching '{}'", source_spec);
        };
        let (header, entries, _) = read_all(&source_path).await?;
        let source = SessionManager::from_entries(header, entries);
        let dest_path = pi_agent_core::session::build_session_file_path(session_dir, model_id);
        clone_active_path_to_file(&source, &dest_path, Some(&source_path)).await?;
        return Ok(Some(dest_path));
    }

    if let Some(session_spec) = &args.session {
        if looks_like_session_path(session_spec) {
            return Ok(Some(PathBuf::from(session_spec)));
        }
        if let Some(path) = resolve_session_reference(session_spec, session_dir).await? {
            return Ok(Some(path));
        }
        anyhow::bail!("No session found matching '{}'", session_spec);
    }

    if should_open_resume_selector(args) {
        return Ok(None);
    }

    if args.continue_recent {
        if let Some(path) = find_most_recent_session(session_dir).await? {
            return Ok(Some(path));
        }
    }

    Ok(Some(pi_agent_core::session::build_session_file_path(session_dir, model_id)))
}

fn should_open_resume_selector(args: &pi_cli::args::Args) -> bool {
    args.resume && !args.no_session && args.fork.is_none() && args.session.is_none()
}

fn export_output_path(source_path: &Path) -> PathBuf {
    source_path.with_extension("html")
}

async fn export_session(export_spec: &str, args: &pi_cli::args::Args) -> anyhow::Result<()> {
    let session_dir = resolve_session_dir(args);
    let Some(source_path) = resolve_session_reference(export_spec, &session_dir).await? else {
        anyhow::bail!("No session found matching '{}'", export_spec);
    };
    let (header, entries, _) = read_all(&source_path).await?;
    let html = export_session_as_html(&header, &entries);
    let output_path = export_output_path(&source_path);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output_path, html)?;
    println!("{}", output_path.display());
    Ok(())
}

/// Load WASM extensions from standard search paths.
///
/// Scans `~/.pi/extensions/` and `.pi/extensions/` (relative to CWD) for
/// `.wasm` files and returns their manifests. Only available when the
/// `feat-extensions` feature is enabled (wasmtime is heavy).
#[cfg(feature = "feat-extensions")]
fn load_extensions() -> Vec<pi_extension_system::types::ExtensionManifest> {
    let paths: Vec<std::path::PathBuf> = vec![
        dirs::home_dir().map(|h| h.join(".pi").join("extensions")),
        Some(std::path::PathBuf::from(".pi/extensions")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let manifests = pi_extension_system::loader::discover_extensions(&paths);
    if manifests.is_empty() {
        tracing::info!("no WASM extensions found in search paths");
    } else {
        tracing::info!("discovered {} WASM extension(s)", manifests.len());
        for m in &manifests {
            tracing::info!("  extension: {} v{}", m.name, m.version);
        }
    }
    manifests
}

/// Run interactive TUI mode.
///
/// Parses CLI arguments into a [`SessionConfig`], resolves the model from the
/// catalog, determines the session file path, initialises the [`InteractiveMode`]
/// orchestrator with the resolved model and session path, and enters the main
/// event loop. The terminal is put into raw mode; input is read
/// character-by-character and dispatched to the editor or action handlers.
///
/// When the `feat-extensions` feature is enabled, WASM extensions are loaded
/// from the standard search paths and passed to the TUI for display in the
/// extension selector and via the `/extensions` slash command.
async fn run_interactive(args: &pi_cli::args::Args) -> anyhow::Result<()> {
    let config = pi_cli::SessionConfig::from_args(args)?;
    let tool_selection = pi_cli::tool_selection_from_args(args)?;
    let model = pi_model_catalog::models::find_model(&config.model_id)
        .ok_or_else(|| anyhow::anyhow!("Model '{}' not found", config.model_id))?;
    let session_dir = resolve_session_dir(args);

    // Determine the session file path.
    let session_path = resolve_interactive_session_path(args, &session_dir, &config.model_id).await?;

    let mut im = pi_tui_app::InteractiveMode::new_with_thinking_level(
        &config.model_id,
        model,
        config.system_prompt.clone(),
        config.api_key.clone(),
        session_path,
        Some(config.thinking_level.clone()),
        session_dir.clone(),
    )
    .await?;
    im.set_tool_selection(tool_selection);
    im.set_max_turns(args.max_turns);

    if should_open_resume_selector(args) {
        im.show_session_selector(true).await?;
    }

    // Load and pass WASM extensions (gated behind feat-extensions).
    #[cfg(feature = "feat-extensions")]
    {
        let extensions = load_extensions();
        im.set_extensions(extensions);
    }

    im.run().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Login handler
// ---------------------------------------------------------------------------

/// Minimal OAuth callbacks that print to stdout and read from stdin.
struct CliOAuthCallbacks;

#[async_trait::async_trait]
impl pi_oauth::OAuthCallbacks for CliOAuthCallbacks {
    fn on_auth(&self, info: &pi_oauth::OAuthAuthInfo) {
        println!();
        println!("Open this URL in your browser to log in:");
        println!("  {}", info.url);
        if let Some(instructions) = &info.instructions {
            println!();
            println!("{}", instructions);
        }
        println!();
    }

    fn on_progress(&self, message: &str) {
        println!("{}", message);
    }

    async fn on_prompt(&self, prompt: &pi_oauth::OAuthPrompt) -> String {
        use std::io::Write;
        print!("{} ", prompt.message);
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        input.trim().to_string()
    }
}

/// Run the OAuth login flow for `provider` and persist the credentials.
async fn handle_login(provider: &str) -> anyhow::Result<()> {
    let callbacks = CliOAuthCallbacks;

    let credentials = match provider {
        "anthropic" => {
            tracing::info!("Starting Anthropic OAuth login");
            pi_oauth::AnthropicOAuth
                .login(&callbacks)
                .await
                .map_err(|e| anyhow::anyhow!("Anthropic OAuth login failed: {}", e))?
        }
        "github-copilot" => {
            tracing::info!("Starting GitHub Copilot OAuth login");
            pi_oauth::GitHubCopilotOAuth
                .login(&callbacks)
                .await
                .map_err(|e| anyhow::anyhow!("GitHub Copilot OAuth login failed: {}", e))?
        }
        "openai-codex" => {
            tracing::info!("Starting OpenAI Codex OAuth login");
            pi_oauth::OpenAICodexOAuth
                .login(&callbacks)
                .await
                .map_err(|e| anyhow::anyhow!("OpenAI Codex OAuth login failed: {}", e))?
        }
        _ => {
            anyhow::bail!("Unknown provider '{}'. Supported: anthropic, github-copilot, openai-codex", provider);
        }
    };

    // Persist credentials.
    let mut auth = AuthStorage::load()?;
    auth.set_oauth(
        provider,
        &credentials.access_token,
        credentials.refresh_token,
        credentials.expires_at,
        &credentials.token_type,
        credentials.account_id,
    );
    auth.save()?;

    println!("Successfully logged in to '{}'.", provider);
    tracing::info!("OAuth credentials for '{}' saved to auth.json", provider);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::session::storage;
    use pi_agent_core::session::types::{MessageEntryData, SessionEntry, now_timestamp};

    fn user_entry(id: &str, text: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntryData {
            id: id.to_string(),
            parent_id: None,
            timestamp: now_timestamp(),
            message: serde_json::json!({
                "role": "user",
                "content": [{ "type": "text", "text": text }]
            }),
        })
    }

    #[test]
    fn test_resolve_session_dir_prefers_cli_arg() {
        let args = pi_cli::args::Args::parse_from(["pi", "--session-dir", "/tmp/custom-sessions", "--interactive"]);
        assert_eq!(resolve_session_dir(&args), PathBuf::from("/tmp/custom-sessions"));
    }

    #[test]
    fn test_export_output_path_uses_html_extension() {
        let path = PathBuf::from("/tmp/example-session.jsonl");
        assert_eq!(export_output_path(&path), PathBuf::from("/tmp/example-session.html"));
    }

    #[tokio::test]
    async fn test_resolve_session_reference_by_id_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let header = pi_agent_core::session::types::SessionHeader::new("/tmp", "resume1234".to_string());
        storage::create(&path, &header).await.unwrap();

        let resolved = resolve_session_reference("resume", dir.path()).await.unwrap();
        assert_eq!(resolved.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn test_should_open_resume_selector_respects_higher_priority_flags() {
        let resume_only = pi_cli::args::Args::parse_from(["pi", "--resume"]);
        assert!(should_open_resume_selector(&resume_only));

        let with_session = pi_cli::args::Args::parse_from(["pi", "--resume", "--session", "abc"]);
        assert!(!should_open_resume_selector(&with_session));

        let no_session = pi_cli::args::Args::parse_from(["pi", "--resume", "--no-session"]);
        assert!(!should_open_resume_selector(&no_session));
    }

    #[tokio::test]
    async fn test_resolve_interactive_session_path_resume_yields_none() {
        let args = pi_cli::args::Args::parse_from(["pi", "--resume"]);
        let path = resolve_interactive_session_path(&args, Path::new("/tmp/unused"), "gpt-4o").await.unwrap();
        assert!(path.is_none());
    }

    #[tokio::test]
    async fn test_resolve_interactive_session_path_session_beats_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.jsonl");
        let header = pi_agent_core::session::types::SessionHeader::new("/tmp", "resume1234".to_string());
        storage::create(&path, &header).await.unwrap();

        let args = pi_cli::args::Args::parse_from(["pi", "--resume", "--session", path.to_str().unwrap()]);
        let resolved = resolve_interactive_session_path(&args, dir.path(), "gpt-4o").await.unwrap();
        assert_eq!(resolved.as_deref(), Some(path.as_path()));
    }

    #[tokio::test]
    async fn test_resolve_interactive_session_path_continue_uses_recent_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent.jsonl");
        let header = pi_agent_core::session::types::SessionHeader::new("/tmp", "recent1234".to_string());
        storage::create(&path, &header).await.unwrap();

        let args = pi_cli::args::Args::parse_from(["pi", "--continue"]);
        let resolved = resolve_interactive_session_path(&args, dir.path(), "gpt-4o").await.unwrap();
        assert_eq!(resolved.as_deref(), Some(path.as_path()));
    }

    #[tokio::test]
    async fn test_resolve_interactive_session_path_fork_creates_new_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.jsonl");
        let header = pi_agent_core::session::types::SessionHeader::new("/tmp", "fork1234".to_string());
        storage::rewrite(&source_path, &header, &[user_entry("u1", "hello fork")]).await.unwrap();

        let args = pi_cli::args::Args::parse_from(["pi", "--fork", source_path.to_str().unwrap()]);
        let resolved = resolve_interactive_session_path(&args, dir.path(), "gpt-4o").await.unwrap().unwrap();
        assert_ne!(resolved, source_path);
        assert!(resolved.exists());

        let (_, entries, _) = storage::read_all(&resolved).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id(), "u1");
    }
}
