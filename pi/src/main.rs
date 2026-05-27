//! Pi binary — main entry point.
//!
//! Usage:
//!   pi --print "hello"          Non-interactive: send prompt and print response
//!   pi --list-models            List all available models from the catalog
//!   pi --interactive            Interactive TUI mode
//!   pi --login anthropic        Login to an OAuth provider
//!   pi --help                   Show help
//!   pi --version                Show version

use std::path::PathBuf;

use clap::Parser;
use pi_core::auth::AuthStorage;
use pi_oauth::OAuthProvider;
use tracing_subscriber::EnvFilter;

/// CLI entry point.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with sensible defaults.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = pi_cli::args::Args::parse();

    // Handle --login before routing to other modes.
    if let Some(provider) = &args.login {
        return handle_login(provider).await;
    }

    // Route to the appropriate mode.
    if args.rpc {
        return pi_modes::rpc::server::run_rpc_server().await;
    } else if args.list_models {
        list_models().await?;
    } else if args.print {
        pi_cli::create_print_mode(&args).await?;
    } else if args.interactive {
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
/// 2. The `PI_SESSION_DIR` environment variable sets the directory.
/// 3. Defaults to `~/.pi/sessions/` via the `dirs` crate.
///
/// The file name uses the current timestamp and the model ID, e.g.
/// `20260515_143022-gpt-4o-mini.jsonl`.
fn resolve_session_path(args: &pi_cli::args::Args, model_id: &str) -> PathBuf {
    // Explicit --session path takes priority.
    if let Some(session) = &args.session {
        return PathBuf::from(session);
    }

    // Determine the session directory.
    let session_dir = match std::env::var("PI_SESSION_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            home.join(".pi").join("sessions")
        }
    };

    // Build a timestamped filename with the model ID.
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let safe_model: String = model_id
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let filename = format!("{}-{}.jsonl", timestamp, safe_model);

    session_dir.join(filename)
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
    let model = pi_model_catalog::models::find_model(&config.model_id)
        .ok_or_else(|| anyhow::anyhow!("Model '{}' not found", config.model_id))?;

    // Determine the session file path.
    let session_path = resolve_session_path(args, &config.model_id);

    let mut im = pi_tui_app::InteractiveMode::new(
        &config.model_id,
        model,
        config.system_prompt.clone(),
        config.api_key.clone(),
        Some(session_path),
    )
    .await?;

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
            anyhow::bail!(
                "Unknown provider '{}'. Supported: anthropic, github-copilot, openai-codex",
                provider
            );
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
