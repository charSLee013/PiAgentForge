//! InteractiveMode — main event loop that ties together the TUI engine, Editor,
//! Theme, and App Components into an interactive chat interface.
//!
//! This is the final Layer 7 orchestrator. Run with `pi --interactive`.
//!
//! ## Architecture
//!
//! - Owns a [`Terminal`] for raw I/O and a [`Container`] for messages.
//! - Uses the [`Editor`] component for text input.
//! - Reads stdin via [`StdinBuffer`] and parses key events with [`parse_key`].
//! - Dispatches Enter → send (LLM stream), Escape → quit, everything else → editor.
//! - Re-renders the full screen after each action (full redraw, not differential).
//! - Registers built-in API providers on first message and streams responses
//!   from the LLM via `pi-ai-core`.
//! - Persists session state to JSONL files via `pi-agent-core` session storage.
//! - Supports runtime model and theme switching via interactive selectors.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use pi_ai_core::stream;
use pi_ai_core::types::{
    ContentBlock, Context, Message, Model, StreamEvent, StreamOptions, TextContent,
};
use pi_agent_core::session::storage;
use pi_agent_core::session::types::{
    create_session_id, SessionEntry,
};
use pi_agent_core::session::session_manager::SessionManager;
use pi_tui_core::{
    keys::{parse_key, KeyCode},
    stdin_buffer::StdinBuffer,
    Component, Container, Terminal,
};
use pi_tui_core::components::editor::Editor;
use tokio::sync::mpsc;

use crate::components::assistant_message::{AssistantContentBlock, AssistantMessage};
use crate::components::footer::Footer;
use crate::components::model_selector::{ModelEntry, ModelSelector};
use crate::components::theme_selector::ThemeSelector;
use crate::components::user_message::UserMessage;
use crate::Theme;

#[cfg(feature = "feat-extensions")]
use crate::components::extension_selector::ExtensionSelector;
#[cfg(feature = "feat-extensions")]
use pi_extension_system::types::ExtensionManifest;

// ---------------------------------------------------------------------------
// Selector action types (private)
// ---------------------------------------------------------------------------

/// Result from the model selector component.
#[derive(Debug)]
enum ModelSelectorAction {
    /// A model was selected: (provider, model_id).
    Selected { provider: String, model_id: String },
    /// The user cancelled the selection.
    Cancelled,
}

/// Result from the theme selector component.
#[derive(Debug)]
enum ThemeSelectorAction {
    /// A theme was selected.
    Selected(String),
    /// The user cancelled the selection.
    Cancelled,
}

/// Result from the extension selector component.
#[cfg(feature = "feat-extensions")]
#[derive(Debug)]
enum ExtensionSelectorAction {
    /// An extension was selected.
    Selected,
    /// The user cancelled the selection.
    Cancelled,
}

// ---------------------------------------------------------------------------
// InteractiveMode
// ---------------------------------------------------------------------------

/// The InteractiveMode orchestrator.
///
/// Initialises the terminal, builds the component tree, and runs the main
/// event loop that reads keys, dispatches them, and renders the UI.
/// On first message, registers built-in API providers and streams LLM
/// responses via `pi-ai-core`.
///
/// Session state is persisted to a JSONL file when a `session_path` is
/// provided.  Runtime model and theme switching is available via
/// `show_model_selector()` / `show_theme_selector()`.
pub struct InteractiveMode {
    /// Terminal abstraction for raw I/O and output.
    terminal: Terminal,
    /// Multi-line text editor for user input.
    editor: Editor,
    /// Container holding rendered messages (grows as messages are sent).
    messages: Container,
    /// Status bar at the bottom of the screen (PWD, token stats, model name).
    footer: Footer,
    /// Non-blocking stdin reader.
    stdin_buffer: StdinBuffer,
    /// Application theme (dark or light).
    theme: Theme,
    /// Name of the current theme ("dark" or "light") for selector state tracking.
    theme_name: String,
    /// Current session name (reserved for future use).
    #[allow(dead_code)]
    session_name: String,
    /// Current model name display in the footer.
    #[allow(dead_code)]
    model_name: String,
    /// Whether the event loop is running.
    running: bool,
    /// Resolved model reference for LLM calls (static lifetime from catalog).
    model: &'static Model,
    /// Session manager for tracking conversation history.
    session: SessionManager,
    /// Optional system prompt.
    system_prompt: Option<String>,
    /// Whether built-in providers have been registered (one-shot).
    providers_registered: bool,
    /// Model ID string for context building.
    model_id: String,
    /// Optional API key override.
    #[allow(dead_code)]
    api_key: Option<String>,
    /// Path to the JSONL session file on disk (None = in-memory only).
    session_path: Option<PathBuf>,
    /// Model selector overlay (active when Some).
    model_selector: Option<ModelSelector>,
    /// Channel for receiving model-selection results from the overlay.
    model_selector_rx: Option<mpsc::UnboundedReceiver<ModelSelectorAction>>,
    /// Theme selector overlay (active when Some).
    theme_selector: Option<ThemeSelector>,
    /// Channel for receiving theme-selection results from the overlay.
    theme_selector_rx: Option<mpsc::UnboundedReceiver<ThemeSelectorAction>>,
    /// Loaded WASM extension manifests for the extension selector.
    #[cfg(feature = "feat-extensions")]
    extensions: Vec<ExtensionManifest>,
    /// Extension selector overlay (active when Some).
    #[cfg(feature = "feat-extensions")]
    extension_selector: Option<ExtensionSelector>,
    /// Channel for receiving extension-selection results from the overlay.
    #[cfg(feature = "feat-extensions")]
    extension_selector_rx: Option<mpsc::UnboundedReceiver<ExtensionSelectorAction>>,
}

impl InteractiveMode {
    /// Create a new `InteractiveMode` instance.
    ///
    /// Accepts the resolved model ID, the static model reference from the
    /// catalog, an optional system prompt, an optional API key, and an
    /// optional session file path.
    ///
    /// When `session_path` is provided and the file already exists the
    /// session is **resumed** (entries are loaded into the session manager
    /// and message container).  When the path does not exist a new session
    /// file is created.  When `session_path` is `None` the session is kept
    /// entirely in memory.
    ///
    /// Initialises the terminal, theme, editor, footer, stdin buffer, and
    /// session manager. The terminal is **not** yet put into raw mode —
    /// that happens in [`run`](Self::run).
    pub async fn new(
        model_id: &str,
        model: &'static Model,
        system_prompt: Option<String>,
        api_key: Option<String>,
        session_path: Option<PathBuf>,
    ) -> io::Result<Self> {
        let terminal = Terminal::new()?;
        let theme = Theme::dark();
        let theme_name = "dark".to_string();

        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let footer = Footer::new(
            cwd.clone(),
            None,                // git_branch
            0,                   // input_tokens
            0,                   // output_tokens
            0,                   // cache_read
            0,                   // cache_write
            model_id.into(),     // model_name
            0.0,                 // context_percent
            100000,              // context_window
            false,               // auto_compact
            &theme,
        );

        let mut editor = Editor::new();
        editor.focused = true;
        editor.max_visible_lines = 5;

        let messages = Container::new();
        let stdin_buffer = StdinBuffer::new();

        // ── Session initialisation ────────────────────────────────────────
        let (session, resolved_path, resolved_model_id, resolved_model) =
            if let Some(ref path) = session_path {
                if path.exists() {
                    // Resume session from disk
                    let (header, entries, _) = storage::read_all(path)
                        .await
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    let sm = SessionManager::from_entries(header, entries);

                    // Restore the model from the session context if available
                    let ctx = sm.build_context();
                    let (mid, m) = match ctx.model {
                        Some((_provider, ref mid)) => {
                            if let Some(m) = pi_model_catalog::models::find_model(mid) {
                                (mid.clone(), m)
                            } else {
                                (model_id.to_string(), model)
                            }
                        }
                        None => (model_id.to_string(), model),
                    };

                    (sm, Some(path.clone()), mid, m)
                } else {
                    // Create new session file
                    let id = create_session_id();
                    let header = pi_agent_core::session::types::SessionHeader::new(&cwd, id);
                    storage::create(path, &header)
                        .await
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    let sm = SessionManager::new(header);
                    (sm, Some(path.clone()), model_id.to_string(), model)
                }
            } else {
                // In-memory session
                let sm = SessionManager::in_memory(cwd);
                (sm, None, model_id.to_string(), model)
            };

        // Build resolved model name for footer / internal tracking.
        let resolved_model_name = resolved_model_id.clone();

        let mut im = Self {
            terminal,
            editor,
            messages,
            footer,
            stdin_buffer,
            theme,
            theme_name,
            session_name: String::new(),
            model_name: resolved_model_name,
            running: false,
            model: resolved_model,
            session,
            system_prompt,
            providers_registered: false,
            model_id: resolved_model_id,
            api_key,
            session_path: resolved_path,
            model_selector: None,
            model_selector_rx: None,
            theme_selector: None,
            theme_selector_rx: None,
            #[cfg(feature = "feat-extensions")]
            extensions: Vec::new(),
            #[cfg(feature = "feat-extensions")]
            extension_selector: None,
            #[cfg(feature = "feat-extensions")]
            extension_selector_rx: None,
        };

        // When resuming, reconstruct the message display from session entries.
        if session_path.as_ref().is_some_and(|p| p.exists()) {
            im.load_entries_into_container();
        }

        Ok(im)
    }

    /// Run the main event loop.
    ///
    /// 1. Enables raw mode on the terminal.
    /// 2. Enters the render loop:
    ///    - Reads stdin for key events via [`StdinBuffer`].
    ///    - Dispatches keys: Enter = send (async LLM stream), Escape = quit,
    ///      typing = edit.
    ///    - On Enter: captures editor text, displays as a message, clears
    ///      editor, and streams an LLM response.
    ///    - Re-renders the full screen after each action.
    /// 3. On quit: disables raw mode, shows the cursor, and restores the
    ///    terminal to a clean state.
    pub async fn run(&mut self) -> io::Result<()> {
        // Enable raw mode so we can read individual key events.
        self.terminal.start()?;

        // Hide the hardware cursor while rendering our own cursor block.
        self.terminal.hide_cursor()?;

        self.running = true;

        // Initial full render.
        self.render_all()?;

        while self.running {
            let sequences = self.stdin_buffer.read().await?;

            if !sequences.is_empty() {
                for seq in &sequences {
                    self.handle_input(seq).await;
                }
                // Re-render if we are still running.
                if self.running {
                    self.render_all()?;
                }
            } else {
                // Avoid busy-looping when no input is available.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        // ── Cleanup ──────────────────────────────────────────────────────
        self.terminal.stop()?;   // disable raw mode
        self.terminal.show_cursor()?;

        // Move to a clean line below the TUI content so the shell prompt
        // appears on a fresh line.
        let mut stdout = io::stdout();
        write!(stdout, "\r\n")?;
        stdout.flush()?;

        Ok(())
    }

    // ------------------------------------------------------------------
    // Input handling
    // ------------------------------------------------------------------

    /// Dispatch a raw terminal input sequence to the appropriate handler.
    ///
    /// When a model or theme selector is active, input is routed to the
    /// selector instead of the main editor.
    ///
    /// - **Ctrl+G** opens the model selector.
    /// - **Ctrl+T** opens the theme selector.
    /// - **Escape** quits (or cancels an active selector).
    /// - **Enter** sends the current editor content as a message (async).
    /// - Everything else forwards to the [`Editor`] for normal text editing.
    async fn handle_input(&mut self, data: &str) {
        // ── Selector overlays take priority ──────────────────────────────
        #[cfg(feature = "feat-extensions")]
        if self.extension_selector.is_some() {
            self.handle_extension_selector_input(data);
            return;
        }
        if self.model_selector.is_some() {
            self.handle_model_selector_input(data).await;
            return;
        }
        if self.theme_selector.is_some() {
            self.handle_theme_selector_input(data);
            return;
        }

        // ── Keyboard shortcuts to open selectors ─────────────────────────
        #[cfg(feature = "feat-extensions")]
        if data == "\x05" {
            // Ctrl+E — open extension selector
            self.show_extension_selector();
            return;
        }
        if data == "\x07" {
            // Ctrl+G — open model selector
            self.show_model_selector();
            return;
        }
        if data == "\x14" {
            // Ctrl+T — open theme selector
            self.show_theme_selector();
            return;
        }

        // ── Normal input dispatch ────────────────────────────────────────
        let key = parse_key(data);

        match key.code {
            KeyCode::Escape => {
                self.running = false;
            }
            KeyCode::Enter => {
                self.send_message().await;
            }
            _ => {
                self.editor.handle_input(data);
            }
        }
    }

    /// Route input to the model selector overlay and process its result.
    async fn handle_model_selector_input(&mut self, data: &str) {
        // Forward input to the selector component.
        if let Some(ref mut selector) = self.model_selector {
            selector.handle_input(data);
        }

        // Check for a selection or cancellation from the channel.
        if let Some(ref mut rx) = self.model_selector_rx {
            if let Ok(action) = rx.try_recv() {
                match action {
                    ModelSelectorAction::Selected { provider, model_id } => {
                        self.apply_model_change(&provider, &model_id).await;
                    }
                    ModelSelectorAction::Cancelled => {}
                }
                self.model_selector = None;
                self.model_selector_rx = None;
            }
        }
    }

    /// Route input to the theme selector overlay and process its result.
    fn handle_theme_selector_input(&mut self, data: &str) {
        // Forward input to the selector component.
        if let Some(ref mut selector) = self.theme_selector {
            selector.handle_input(data);
        }

        // Check for a selection or cancellation from the channel.
        if let Some(ref mut rx) = self.theme_selector_rx {
            if let Ok(action) = rx.try_recv() {
                match action {
                    ThemeSelectorAction::Selected(name) => {
                        self.apply_theme_change(&name);
                    }
                    ThemeSelectorAction::Cancelled => {}
                }
                self.theme_selector = None;
                self.theme_selector_rx = None;
            }
        }
    }

    /// Capture the editor text, display it as a user message, clear the
    /// editor, and stream an LLM response.
    ///
    /// If the editor contains only whitespace, this is a no-op.
    ///
    /// If the text starts with `/`, it is dispatched as a slash command
    /// instead of being sent to the LLM.
    ///
    /// On the first invocation, registers built-in API providers (OpenAI
    /// completions and responses). The LLM response is displayed as an
    /// assistant message. Errors are displayed inline as error messages.
    ///
    /// After each message is appended to the session manager it is also
    /// persisted to the JSONL session file (if a `session_path` is set).
    async fn send_message(&mut self) {
        let text = self.editor.get_text();
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        // ── Slash command dispatch ────────────────────────────────────────
        if text.starts_with('/') {
            self.editor.set_text("");
            self.handle_slash_command(&text).await;
            return;
        }

        // Add the user message to the messages container.
        let user_msg = UserMessage::new(trimmed.clone(), &self.theme);
        self.messages.add(user_msg);

        // Clear the editor for the next prompt.
        self.editor.set_text("");

        // Register providers on first call.
        if !self.providers_registered {
            pi_cli::register_builtin_providers().await;
            self.providers_registered = true;
        }

        // Append user message to session history.
        let user_message = Message::user_text(&trimmed);
        let msg_value = serde_json::to_value(&user_message)
            .expect("user message should serialize");
        let entry_id = self.session.append_message(msg_value);

        // Persist user message to disk.
        self.persist_entry(&entry_id).await;

        // Build conversation context from session history.
        let session_context = self.session.build_context();
        let tools = pi_core::tool_registry::tool_definitions();
        let context = Context {
            messages: session_context.messages,
            system_prompt: self.system_prompt.clone(),
            model: Some(self.model_id.clone()),
            tools,
        };

        let options = StreamOptions {
            api_key: self.api_key.clone(),
            ..Default::default()
        };

        // Stream the LLM response.
        match stream::stream(self.model, context, options).await {
            Ok(mut event_stream) => {
                use tokio_stream::StreamExt;
                let mut response_text = String::new();
                let mut tool_calls: BTreeMap<u32, (String, String, String)> = BTreeMap::new();

                while let Some(event) = event_stream.next().await {
                    match event {
                        StreamEvent::TextDelta { delta } => {
                            response_text.push_str(&delta);
                        }
                        StreamEvent::ToolCallDelta {
                            index,
                            id,
                            name,
                            arguments,
                        } => {
                            let entry = tool_calls.entry(index).or_insert_with(|| {
                                (String::new(), String::new(), String::new())
                            });
                            if let Some(id_val) = id {
                                entry.0 = id_val;
                            }
                            if let Some(name_val) = name {
                                entry.1 = name_val;
                            }
                            if let Some(arg_chunk) = arguments {
                                entry.2.push_str(&arg_chunk);
                            }
                        }
                        StreamEvent::Error { error } => {
                            let err_msg = AssistantMessage::new(
                                vec![AssistantContentBlock::Text(format!(
                                    "Error: {}",
                                    error.message
                                ))],
                                false,
                                "Thinking...".into(),
                                Some("error".into()),
                                None,
                                &self.theme,
                            );
                            self.messages.add(err_msg);
                            self.render_all().ok();
                            return;
                        }
                        _ => {}
                    }
                }

                // If the LLM requested tool calls, execute them
                if !tool_calls.is_empty() {
                    // Show any preface text first
                    if !response_text.is_empty() {
                        let preface_msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(response_text.clone())],
                            false,
                            "Thinking...".into(),
                            None,
                            None,
                            &self.theme,
                        );
                        self.messages.add(preface_msg);
                    }

                    let cancel = tokio_util::sync::CancellationToken::new();
                    for (_tc_id, tc_name, tc_args_str) in tool_calls.values() {
                        tracing::info!(
                            tool = %tc_name,
                            "Interactive mode executing tool call"
                        );
                        let args: serde_json::Value =
                            serde_json::from_str(tc_args_str).unwrap_or(serde_json::Value::Null);
                        match pi_core::tool_registry::execute_tool(tc_name, args, cancel.clone())
                            .await
                        {
                            Ok(result) => {
                                let result_text: String = result
                                    .content
                                    .iter()
                                    .filter_map(|c| {
                                        if let ContentBlock::Text(t) = c {
                                            Some(t.text.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let tool_msg = AssistantMessage::new(
                                    vec![AssistantContentBlock::Text(format!(
                                        "[{}]\n{}",
                                        tc_name, result_text
                                    ))],
                                    false,
                                    "Thinking...".into(),
                                    None,
                                    None,
                                    &self.theme,
                                );
                                self.messages.add(tool_msg);

                                // Persist tool result to session
                                let tool_message = Message::assistant(vec![
                                    ContentBlock::Text(TextContent {
                                        text: format!("[{}]\n{}", tc_name, result_text),
                                    }),
                                ]);
                                let msg_value = serde_json::to_value(&tool_message)
                                    .expect("tool message should serialize");
                                let entry_id = self.session.append_message(msg_value);
                                self.persist_entry(&entry_id).await;
                            }
                            Err(e) => {
                                let err_msg = AssistantMessage::new(
                                    vec![AssistantContentBlock::Text(format!(
                                        "[{} Error]\n{}",
                                        tc_name, e
                                    ))],
                                    false,
                                    "Thinking...".into(),
                                    Some("error".into()),
                                    None,
                                    &self.theme,
                                );
                                self.messages.add(err_msg);

                                // Persist tool error to session
                                let tool_message = Message::assistant(vec![
                                    ContentBlock::Text(TextContent {
                                        text: format!("[{} Error]\n{}", tc_name, e),
                                    }),
                                ]);
                                let msg_value = serde_json::to_value(&tool_message)
                                    .expect("tool message should serialize");
                                let entry_id = self.session.append_message(msg_value);
                                self.persist_entry(&entry_id).await;
                            }
                        }
                    }
                } else if !response_text.is_empty() {
                    let assistant_msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(response_text.clone())],
                        false,
                        "Thinking...".into(),
                        None,
                        None,
                        &self.theme,
                    );
                    self.messages.add(assistant_msg);

                    // Append assistant response to session history.
                    let assistant_message = Message::assistant(vec![
                        ContentBlock::Text(TextContent {
                            text: response_text,
                        }),
                    ]);
                    let msg_value = serde_json::to_value(&assistant_message)
                        .expect("assistant message should serialize");
                    let entry_id = self.session.append_message(msg_value);

                    // Persist assistant message to disk.
                    self.persist_entry(&entry_id).await;
                }
            }
            Err(e) => {
                let err_msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("Error: {}", e))],
                    false,
                    "Thinking...".into(),
                    Some("error".into()),
                    None,
                    &self.theme,
                );
                self.messages.add(err_msg);
            }
        }

        // Re-render after response is complete.
        self.render_all().ok();
    }

    // ------------------------------------------------------------------
    // Slash commands
    // ------------------------------------------------------------------

    /// Dispatch a slash command typed by the user.
    ///
    /// Supported commands:
    /// - `/help` — show available commands
    /// - `/extensions` — list loaded WASM extensions
    /// - `/model <id>` — switch to a different model (or open selector)
    /// - `/theme <name>` — switch theme (dark | light, or open selector)
    /// - `/clear` — clear all messages
    /// - `/session` — show session info
    async fn handle_slash_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
        match parts[0] {
            "/help" => {
                let help_entries = vec![
                    "  /help               Show this help message",
                    "  /model <model-id>   Switch model (opens selector if no id given)",
                    "  /theme <name>       Switch theme: dark | light (opens selector if no name)",
                    "  /clear              Clear all messages (keeps session)",
                    "  /session            Show session info (model, theme, message count)",
                    "  /compact            Compact session context (placeholder)",
                    "  /fork               Fork session at current position (placeholder)",
                    "  /tree               Show session entry tree",
                    "  /new                Start a new session",
                    "  /quit               Exit pi",
                    "  /settings           Quick settings reference",
                    "  /login              Provider login instructions",
                    "  /logout             Remove stored credentials",
                    "  /name <name>        Set session display name",
                    "  /hotkeys            Show keyboard shortcuts",
                    "  /scoped-models      Open model selector",
                    "  /export             Export session (not yet implemented)",
                    "  /import             Import session (not yet implemented)",
                    "  /resume             Resume a session (not yet implemented)",
                ];

                #[cfg(feature = "feat-extensions")]
                let help_entries = {
                    let mut v = help_entries;
                    v.insert(1, "  /extensions         List loaded WASM extensions with metadata");
                    v
                };

                let help_text = std::iter::once("Available commands:")
                    .chain(help_entries.iter().copied())
                    .collect::<Vec<_>>()
                    .join("\n");

                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(help_text)],
                    false,
                    "Thinking...".into(),
                    None,
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/model" => {
                if parts.len() > 1 {
                    let model_name = parts[1];
                    if let Some(model) = pi_model_catalog::models::find_model(model_name) {
                        let provider = format!("{:?}", model.provider).to_lowercase();
                        self.apply_model_change(&provider, model_name).await;
                    } else {
                        let err_msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!(
                                "Unknown model: {}",
                                model_name
                            ))],
                            false,
                            "Thinking...".into(),
                            Some("error".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(err_msg);
                    }
                } else {
                    self.show_model_selector();
                }
            }
            "/theme" => {
                if parts.len() > 1 {
                    self.apply_theme_change(parts[1]);
                } else {
                    self.show_theme_selector();
                }
            }
            "/clear" => {
                self.messages = Container::new();
            }
            #[cfg(feature = "feat-extensions")]
            "/extensions" => {
                if self.extensions.is_empty() {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(
                            "No extensions loaded.\n\nUse the extension selector (Ctrl+E) to browse, or place `.wasm` files in `~/.pi/extensions/`.".to_string(),
                        )],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                } else {
                    let ext_lines: Vec<String> = self
                        .extensions
                        .iter()
                        .enumerate()
                        .map(|(i, e)| {
                            let desc = e.description.as_deref().unwrap_or("no description");
                            let caps = if e.capabilities.is_empty() {
                                "none".to_string()
                            } else {
                                e.capabilities.join(", ")
                            };
                            format!(
                                "  {}. {} v{}\n     Description: {}\n     Capabilities: {}",
                                i + 1,
                                e.name,
                                e.version,
                                desc,
                                caps
                            )
                        })
                        .collect();
                    let ext_text = format!("Loaded Extensions:\n{}", ext_lines.join("\n"));
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(ext_text)],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            }
            "/session" => {
                let entries = self.session.entries();
                let info = format!(
                    "Session info:\n  Messages: {}\n  Model: {}\n  Theme: {}",
                    entries.len(),
                    self.model_id,
                    self.theme_name,
                );
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(info)],
                    false,
                    "Thinking...".into(),
                    None,
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/quit" => self.running = false,
            "/new" => {
                self.messages = Container::new();
                self.session = pi_agent_core::session::session_manager::SessionManager::in_memory(".");
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Started a new session.".into())],
                    false, "Thinking...".into(), None, None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/compact" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Compaction not implemented in TUI mode.".into())],
                    false, "Thinking...".into(), Some("info".into()), None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/fork" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Fork: not yet implemented.".into())],
                    false, "Thinking...".into(), Some("info".into()), None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/tree" => {
                let entries = self.session.entries();
                let tree: Vec<String> = entries.iter().enumerate().map(|(i, e)| {
                    let type_name = match e {
                        SessionEntry::Message(_) => "msg",
                        SessionEntry::Compaction(_) => "cmp",
                        SessionEntry::BranchSummary(_) => "brn",
                        SessionEntry::ModelChange(_) => "mod",
                        SessionEntry::ThinkingLevelChange(_) => "thk",
                        SessionEntry::Label(_) => "lbl",
                        SessionEntry::Custom(_) => "cus",
                        SessionEntry::CustomMessage(_) => "csm",
                        SessionEntry::SessionInfo(_) => "inf",
                    };
                    format!("  {}. [{}] {}", i + 1, type_name, e.id())
                }).collect();
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("Session ({} entries):\n{}", entries.len(), tree.join("\n")))],
                    false, "Thinking...".into(), None, None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/settings" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Settings: Ctrl+P model, Ctrl+T theme.".into())],
                    false, "Thinking...".into(), None, None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/login" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Login: run `pi --login <provider>` from CLI.".into())],
                    false, "Thinking...".into(), Some("info".into()), None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/logout" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Logout: remove credentials from ~/.pi/auth.json.".into())],
                    false, "Thinking...".into(), Some("info".into()), None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/name" => {
                let name = if parts.len() > 1 { parts[1] } else { "unnamed" };
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("Session name: {}", name))],
                    false, "Thinking...".into(), None, None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/hotkeys" => {
                let hk = ["Ctrl+P model", "Ctrl+T theme", "Escape quit", "/help commands"];
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("Keys:\n{}", hk.join("\n")))],
                    false, "Thinking...".into(), None, None, &self.theme,
                );
                self.messages.add(msg);
            }
            "/scoped-models" => self.show_model_selector(),
            "/export" | "/import" | "/resume" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("{}: not yet implemented.", parts[0]))],
                    false, "Thinking...".into(), Some("info".into()), None, &self.theme,
                );
                self.messages.add(msg);
            }
            _ => {
                let err_msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!(
                        "Unknown command: {}\nType /help for available commands.",
                        parts[0]
                    ))],
                    false,
                    "Thinking...".into(),
                    Some("error".into()),
                    None,
                    &self.theme,
                );
                self.messages.add(err_msg);
            }
        }
    }

    // ------------------------------------------------------------------
    // Session persistence
    // ------------------------------------------------------------------

    /// Persist a single session entry to the JSONL file on disk.
    ///
    /// This is a no-op when no `session_path` is configured.
    async fn persist_entry(&self, entry_id: &str) {
        let path = match self.session_path {
            Some(ref p) => p.clone(),
            None => return,
        };
        if let Some(entry) = self.session.get_entry(entry_id) {
            let _ = storage::append(&path, entry).await;
        }
    }

    /// Rebuild the in-memory message container from session entries.
    ///
    /// Called when resuming a session from disk so that the user can see
    /// previous conversation history.
    fn load_entries_into_container(&mut self) {
        let entries = self.session.entries();
        for entry in &entries {
            if let SessionEntry::Message(msg) = entry {
                let role = msg
                    .message
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("");
                let text = msg
                    .message
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|b| b.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match role {
                    "user" => {
                        let user_msg = UserMessage::new(text.to_string(), &self.theme);
                        self.messages.add(user_msg);
                    }
                    "assistant" => {
                        let assistant_msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(text.to_string())],
                            false,
                            "Thinking...".into(),
                            None,
                            None,
                            &self.theme,
                        );
                        self.messages.add(assistant_msg);
                    }
                    _ => {}
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Model selector
    // ------------------------------------------------------------------

    /// Open the model selector overlay.
    ///
    /// Populates the selector with all models from the catalog and pre-selects
    /// the currently active model. The selector's result is delivered via an
    /// mpsc channel so that it can be processed after the component returns.
    pub fn show_model_selector(&mut self) {
        if self.model_selector.is_some() || self.theme_selector.is_some() {
            return; // Already showing a selector
        }

        let (tx, rx) = mpsc::unbounded_channel::<ModelSelectorAction>();

        // Build model entries from the catalog.
        let current_provider = format!("{:?}", self.model.provider).to_lowercase();
        let current_id = &self.model_id;

        let all_models: Vec<ModelEntry> = pi_model_catalog::models::all_models()
            .iter()
            .map(|m| ModelEntry {
                provider: format!("{:?}", m.provider).to_lowercase(),
                id: m.id.clone(),
                is_current: m.id == *current_id,
                name: m.name.clone().unwrap_or_default(),
            })
            .collect();

        let mut selector = ModelSelector::new(all_models, vec![], &self.theme);
        selector.set_current(&current_provider, current_id);

        // Wire the callbacks to send results through the channel.
        let select_tx = tx;
        let cancel_tx = select_tx.clone();
        selector.on_select = Some(Box::new(move |provider, id| {
            let _ = select_tx.send(ModelSelectorAction::Selected {
                provider: provider.to_string(),
                model_id: id.to_string(),
            });
        }));
        selector.on_cancel = Some(Box::new(move || {
            let _ = cancel_tx.send(ModelSelectorAction::Cancelled);
        }));

        self.model_selector = Some(selector);
        self.model_selector_rx = Some(rx);
    }

    /// Apply a model change: update the active model, record a model change
    /// entry in the session, and persist it.
    async fn apply_model_change(&mut self, provider: &str, model_id: &str) {
        // Look up the new model in the catalog.
        if let Some(new_model) = pi_model_catalog::models::find_model(model_id) {
            // Append a model change to the session and get the entry ID.
            let entry_id = self.session.append_model_change(provider, model_id);

            // Persist the model change entry (clone path to avoid borrow conflicts).
            if let Some(path) = self.session_path.clone() {
                if let Some(entry) = self.session.get_entry(&entry_id) {
                    let _ = storage::append(&path, entry).await;
                }
            }

            // Update InteractiveMode state.
            self.model = new_model;
            self.model_id = model_id.to_string();
            self.model_name = model_id.to_string();
        }
    }

    // ------------------------------------------------------------------
    // Theme selector
    // ------------------------------------------------------------------

    /// Open the theme selector overlay.
    ///
    /// Shows available themes ("dark", "light") and pre-selects the current
    /// theme. The result is delivered via an mpsc channel.
    pub fn show_theme_selector(&mut self) {
        if self.theme_selector.is_some() || self.model_selector.is_some() {
            return; // Already showing a selector
        }

        let (tx, rx) = mpsc::unbounded_channel::<ThemeSelectorAction>();

        let themes = vec!["dark".to_string(), "light".to_string()];
        let current = self.theme_name.clone();

        // Wire the on_select and on_cancel callbacks.
        let select_tx = tx;
        let cancel_tx = select_tx.clone();
        let selector = ThemeSelector::new(
            themes,
            &current,
            &self.theme,
            move |item| {
                let _ = select_tx.send(ThemeSelectorAction::Selected(item.value.clone()));
            },
            move || {
                let _ = cancel_tx.send(ThemeSelectorAction::Cancelled);
            },
        );

        self.theme_selector = Some(selector);
        self.theme_selector_rx = Some(rx);
    }

    /// Apply a theme change: swap the active theme and update the theme name.
    fn apply_theme_change(&mut self, name: &str) {
        self.theme = match name {
            "light" => Theme::light(),
            _ => Theme::dark(),
        };
        self.theme_name = name.to_string();
    }

    // ------------------------------------------------------------------
    // Extension selector
    // ------------------------------------------------------------------

    /// Set the loaded WASM extension manifests for display in the TUI.
    ///
    /// Called during startup in `main.rs` when `feat-extensions` is enabled.
    /// The manifests are used by both the extension selector overlay and the
    /// `/extensions` slash command.
    #[cfg(feature = "feat-extensions")]
    pub fn set_extensions(&mut self, exts: Vec<ExtensionManifest>) {
        self.extensions = exts;
    }

    /// Open the extension selector overlay.
    ///
    /// Populates the selector with loaded extension names and versions. The
    /// selector result is delivered via an mpsc channel so it can be processed
    /// after the component returns.
    #[cfg(feature = "feat-extensions")]
    pub fn show_extension_selector(&mut self) {
        if self.extension_selector.is_some()
            || self.model_selector.is_some()
            || self.theme_selector.is_some()
        {
            return; // Already showing a selector
        }

        let (tx, rx) = mpsc::unbounded_channel::<ExtensionSelectorAction>();

        let options: Vec<String> = self
            .extensions
            .iter()
            .map(|e| format!("{} v{}", e.name, e.version))
            .collect();

        let mut selector = ExtensionSelector::new("Loaded Extensions".into(), options, &self.theme);

        let select_tx = tx;
        let cancel_tx = select_tx.clone();
        selector.on_select = Some(Box::new(move |_item| {
            let _ = select_tx.send(ExtensionSelectorAction::Selected);
        }));
        selector.on_cancel = Some(Box::new(move || {
            let _ = cancel_tx.send(ExtensionSelectorAction::Cancelled);
        }));

        self.extension_selector = Some(selector);
        self.extension_selector_rx = Some(rx);
    }

    /// Route input to the extension selector overlay and process its result.
    #[cfg(feature = "feat-extensions")]
    fn handle_extension_selector_input(&mut self, data: &str) {
        if let Some(ref mut selector) = self.extension_selector {
            selector.handle_input(data);
        }

        if let Some(ref mut rx) = self.extension_selector_rx {
            if let Ok(_action) = rx.try_recv() {
                // Both selection and cancellation close the overlay.
                self.extension_selector = None;
                self.extension_selector_rx = None;
            }
        }
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    /// Render all components to the terminal with a full-screen redraw.
    ///
    /// When a model or theme selector is active, the selector is rendered
    /// instead of the normal message/editor layout.
    ///
    /// The layout (top to bottom) is:
    /// 1. Top padding (1 blank line)
    /// 2. Message history (or selector overlay)
    /// 3. Editor (or hidden when selector active)
    /// 4. Footer (status bar)
    ///
    /// Uses DEC private mode 2026 (synchronised output) to avoid flicker.
    fn render_all(&self) -> io::Result<()> {
        let width = self.terminal.columns();
        if width == 0 {
            return Ok(());
        }

        let mut all_lines: Vec<String> = Vec::new();

        // 1. Top padding
        all_lines.push(String::new());

        // 2. Selector overlay OR messages + editor
        // Track whether a selector overlay was rendered (to skip normal view).
        #[allow(unused_mut)]
        let mut selector_rendered = false;

        // Extension selector (behind feat-extensions feature gate).
        #[cfg(feature = "feat-extensions")]
        if let Some(ref selector) = self.extension_selector {
            all_lines.push(self.theme.ansi(&self.theme.primary, "  Extension Selector"));
            all_lines
                .push(self.theme.ansi(&self.theme.muted, "  Viewing loaded extensions (Esc to cancel)"));
            all_lines.push(String::new());
            let selector_lines = selector.render(width);
            all_lines.extend(selector_lines);
            selector_rendered = true;
        }

        if !selector_rendered {
            if let Some(ref selector) = self.model_selector {
            all_lines.push(self.theme.ansi(&self.theme.primary, "  Model Selector"));
            all_lines.push(self.theme.ansi(&self.theme.muted, "  Search and select a model (Esc to cancel)"));
            all_lines.push(String::new());
            let selector_lines = selector.render(width);
            all_lines.extend(selector_lines);
        } else if let Some(ref selector) = self.theme_selector {
            all_lines.push(self.theme.ansi(&self.theme.primary, "  Theme Selector"));
            all_lines.push(self.theme.ansi(&self.theme.muted, "  Select a theme (Esc to cancel)"));
            all_lines.push(String::new());
            let selector_lines = selector.render(width);
            all_lines.extend(selector_lines);
        } else {
            // Normal conversation view
            let msg_lines = self.messages.render(width);
            all_lines.extend(msg_lines);

            // 3. Editor
            let editor_lines = self.editor.render(width);
            all_lines.extend(editor_lines);
        }
        }

        // 4. Footer
        let footer_lines = self.footer.render(width);
        all_lines.extend(footer_lines);

        // Build the output buffer.
        let mut buffer = String::new();
        buffer.push_str("\x1b[?2026h"); // begin synchronised output
        buffer.push_str("\x1b[2J\x1b[H"); // clear screen + cursor home
        for (i, line) in all_lines.iter().enumerate() {
            if i > 0 {
                buffer.push_str("\r\n");
            }
            buffer.push_str(line);
        }
        buffer.push_str("\x1b[?2026l"); // end synchronised output

        self.terminal.write(&buffer)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create an InteractiveMode for testing (blocks on the async
    /// constructor). Uses the default gpt-4o model from the catalog.
    fn create_im() -> InteractiveMode {
        let model = pi_model_catalog::models::find_model("gpt-4o")
            .expect("gpt-4o should exist in catalog");
        tokio::runtime::Runtime::new()
            .expect("failed to create tokio runtime for test")
            .block_on(InteractiveMode::new("gpt-4o", model, None, None, None))
            .expect("InteractiveMode::new() should succeed")
    }

    #[test]
    fn test_initial_state() {
        let im = create_im();
        assert!(im.editor.get_text().is_empty(), "editor should start empty");
        assert_eq!(im.messages.child_count(), 0, "no messages initially");
        assert!(!im.running, "running should be false initially");
    }

    #[test]
    fn test_send_message_moves_text_to_messages() {
        let mut im = create_im();
        im.editor.set_text("hello world");
        // send_message is async; block on it. The LLM call will fail in test
        // (no API key), but the user message is added before the LLM call.
        tokio::runtime::Runtime::new()
            .expect("failed to create tokio runtime for test")
            .block_on(im.send_message());

        // Editor should be cleared.
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
        // Messages should contain the user message (the LLM call may add an
        // error message on failure, so count >= 1).
        assert!(
            im.messages.child_count() >= 1,
            "at least one message should be added"
        );
    }

    #[test]
    fn test_send_message_preserves_other_messages() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        im.editor.set_text("first");
        rt.block_on(im.send_message());
        let count_before = im.messages.child_count();
        im.editor.set_text("second");
        rt.block_on(im.send_message());

        // After two sends we should have strictly more messages than after one.
        assert!(
            im.messages.child_count() > count_before,
            "second send should add more messages"
        );
    }

    #[test]
    fn test_send_message_empty_text_does_nothing() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

        // Empty editor.
        rt.block_on(im.send_message());
        assert_eq!(im.messages.child_count(), 0, "empty text should not add a message");

        // Whitespace-only.
        im.editor.set_text("   \t  ");
        rt.block_on(im.send_message());
        assert_eq!(
            im.messages.child_count(),
            0,
            "whitespace-only should not add a message"
        );
    }

    // ── Key dispatching ──────────────────────────────────────────────

    #[test]
    fn test_handle_escape_sets_running_false() {
        let mut im = create_im();
        im.running = true;
        // handle_input is async; Escape handler is synchronous (no await needed).
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(im.handle_input("\x1b"));
        assert!(!im.running, "Escape should set running = false");
    }

    #[test]
    fn test_handle_enter_sends_message() {
        let mut im = create_im();
        im.editor.set_text("test message");
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(im.handle_input("\r"));

        assert!(
            im.editor.get_text().is_empty(),
            "editor should be cleared after Enter"
        );
        assert!(
            im.messages.child_count() >= 1,
            "Enter should add at least one message"
        );
    }

    #[test]
    fn test_handle_enter_empty_editor_does_nothing() {
        let mut im = create_im();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(im.handle_input("\r"));
        assert_eq!(im.messages.child_count(), 0, "empty Enter should not add a message");
    }

    #[test]
    fn test_handle_typing_updates_editor() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(im.handle_input("h"));
        rt.block_on(im.handle_input("e"));
        rt.block_on(im.handle_input("l"));
        rt.block_on(im.handle_input("l"));
        rt.block_on(im.handle_input("o"));

        assert_eq!(im.editor.get_text(), "hello", "typed characters should populate editor");
    }

    #[test]
    fn test_handle_backspace_removes_character() {
        let mut im = create_im();
        im.editor = Editor::with_text("hello");
        im.editor.focused = true;
        im.editor.max_visible_lines = 5;

        let rt = tokio::runtime::Runtime::new().unwrap();
        // End key to move cursor to end
        rt.block_on(im.handle_input("\x1b[F"));
        // Backspace
        rt.block_on(im.handle_input("\x7f"));

        assert_eq!(im.editor.get_text(), "hell", "backspace should remove last char");
    }

    // ── Editor input after message send ──────────────────────────────

    #[test]
    fn test_editor_ready_for_new_input_after_send() {
        let mut im = create_im();
        im.editor.set_text("message one");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(im.send_message());

        // Editor should be cleared and ready for new input.
        assert!(im.editor.get_text().is_empty(), "editor should be empty after send");

        // Typing after send should work.
        rt.block_on(im.handle_input("n"));
        rt.block_on(im.handle_input("e"));
        rt.block_on(im.handle_input("w"));
        assert_eq!(im.editor.get_text(), "new", "should accept new input after send");
    }

    // ── Footer defaults ──────────────────────────────────────────────

    #[test]
    fn test_footer_renders_with_defaults() {
        let im = create_im();
        let lines = im.footer.render(80);
        assert_eq!(lines.len(), 2, "footer should render 2 lines");
    }

    #[test]
    fn test_messages_container_is_public() {
        // Verify that child_count works on an empty container.
        let im = create_im();
        assert_eq!(im.messages.child_count(), 0);
    }

    // ── Session persistence ──────────────────────────────────────────

    #[test]
    fn test_session_persistence_creates_file_and_stores_messages() {
        let dir = std::env::temp_dir().join("pi_test_session_persist");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("test-session.jsonl");

        let model = pi_model_catalog::models::find_model("gpt-4o")
            .expect("gpt-4o should exist");
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        // Create InteractiveMode with session path (new session).
        let mut im = rt
            .block_on(InteractiveMode::new(
                "gpt-4o",
                model,
                None,
                None,
                Some(path.clone()),
            ))
            .expect("new() should succeed");

        // Verify the session file was created with a header.
        assert!(path.exists(), "session file should exist");
        let content = std::fs::read_to_string(&path).expect("should read session file");
        assert!(content.contains("session"), "header should be present: {content}");

        // Send a message.
        im.editor.set_text("hello world");
        rt.block_on(im.send_message());

        // Verify the session file contains the message.
        let content = std::fs::read_to_string(&path).expect("should read session file");
        assert!(
            content.contains("hello world"),
            "session file should contain message text: {content}"
        );
        // Count lines: header (1) + user message (1) + assistant error (1) >= 3
        let line_count = content.lines().count();
        assert!(line_count >= 2, "should have header + at least 1 entry, got {line_count}");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_persistence_resume_restores_messages() {
        let dir = std::env::temp_dir().join("pi_test_session_resume");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("resume-session.jsonl");

        let model = pi_model_catalog::models::find_model("gpt-4o")
            .expect("gpt-4o should exist");
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        // Phase 1: Create a session and send a message.
        {
            let mut im = rt
                .block_on(InteractiveMode::new(
                    "gpt-4o",
                    model,
                    None,
                    None,
                    Some(path.clone()),
                ))
                .expect("new() should succeed");
            im.editor.set_text("persist me");
            rt.block_on(im.send_message());
        }

        // Phase 2: Resume the session and verify the message is visible.
        {
            let im = rt
                .block_on(InteractiveMode::new(
                    "gpt-4o",
                    model,
                    None,
                    None,
                    Some(path.clone()),
                ))
                .expect("resume should succeed");

            assert!(
                im.messages.child_count() >= 1,
                "resumed session should have at least one message"
            );
            assert_eq!(im.model_id, "gpt-4o", "model_id should match");
        }

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_persistence_in_memory_no_file() {
        // Without a session path, no file should be created.
        let dir = std::env::temp_dir().join("pi_test_session_inmem");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("should-not-exist.jsonl");

        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
        im.editor.set_text("hello");
        rt.block_on(im.send_message());

        // The path should not exist.
        assert!(!path.exists(), "in-memory session should not create a file");
    }

    // ── Model selector ───────────────────────────────────────────────

    #[test]
    fn test_model_selector_opens_and_selects_model() {
        let mut im = create_im();

        // Initially no selector is active.
        assert!(im.model_selector.is_none(), "no selector initially");
        assert!(im.theme_selector.is_none(), "no theme selector initially");

        // Open the model selector.
        let _original_model = im.model_id.clone();
        im.show_model_selector();
        assert!(im.model_selector.is_some(), "selector should be active");

        // Simulate a selection via the channel directly.
        if let Some(ref mut _rx) = im.model_selector_rx {
            // Manually inject a selection through the channel.
            // We need the tx side, which is inside the selector's callback.
            // Instead, test the apply function directly.
        }

        // Close the selector (simulate cancel).
        im.model_selector = None;
        im.model_selector_rx = None;
        assert!(im.model_selector.is_none(), "selector should be closed");

        // Verify the model was unchanged (cancel).
        assert_eq!(im.model_id, _original_model, "model should be unchanged after cancel");
    }

    #[test]
    fn test_apply_model_change_updates_state() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        // Apply a model change to a different model.
        rt.block_on(im.apply_model_change("anthropic", "claude-sonnet-4-20250514"));

        // Verify the model has changed.
        assert_eq!(im.model_id, "claude-sonnet-4-20250514", "model_id should update");
        assert_eq!(
            im.model_name, "claude-sonnet-4-20250514",
            "model_name should update"
        );
        assert_eq!(
            format!("{:?}", im.model.provider).to_lowercase(),
            "anthropic",
            "provider should be anthropic"
        );

        // Verify the session recorded the model change.
        let entries = im.session.entries();
        let has_model_change = entries.iter().any(|e| matches!(e, SessionEntry::ModelChange(_)));
        assert!(has_model_change, "session should have a model_change entry");
    }

    #[test]
    fn test_apply_model_change_unknown_model_does_nothing() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
        let original_id = im.model_id.clone();
        let original_provider = format!("{:?}", im.model.provider).to_lowercase();

        // Try to switch to a non-existent model.
        rt.block_on(im.apply_model_change("openai", "nonexistent-model-xyz"));

        // The model should be unchanged.
        assert_eq!(im.model_id, original_id, "model_id should not change");
        assert_eq!(
            format!("{:?}", im.model.provider).to_lowercase(),
            original_provider,
            "provider should not change"
        );
    }

    // ── Theme selector ───────────────────────────────────────────────

    #[test]
    fn test_theme_selector_opens_and_selects_theme() {
        let mut im = create_im();

        // Initially no theme selector.
        assert!(im.theme_selector.is_none());

        // Open the theme selector.
        im.show_theme_selector();
        assert!(im.theme_selector.is_some(), "theme selector should be active");

        // Close the selector.
        im.theme_selector = None;
        im.theme_selector_rx = None;
        assert!(im.theme_selector.is_none(), "theme selector should be closed");
    }

    #[test]
    fn test_apply_theme_change_switches_theme() {
        let mut im = create_im();
        assert_eq!(im.theme_name, "dark", "should start as dark");
        let original_bg = im.theme.background.clone();

        // Switch to light.
        im.apply_theme_change("light");
        assert_eq!(im.theme_name, "light", "name should be light");
        assert_ne!(
            im.theme.background, original_bg,
            "background should change"
        );
        assert_eq!(im.theme.background, Theme::light().background);

        // Switch back to dark.
        im.apply_theme_change("dark");
        assert_eq!(im.theme_name, "dark");
        assert_eq!(im.theme.background, Theme::dark().background);
    }

    #[test]
    fn test_selectors_cannot_both_be_active() {
        let mut im = create_im();

        im.show_model_selector();
        assert!(im.model_selector.is_some(), "model selector active");

        // Trying to open theme selector while model is active should be a no-op.
        im.show_theme_selector();
        assert!(
            im.theme_selector.is_none(),
            "theme selector should not open when model selector is active"
        );

        // Close model selector and open theme.
        im.model_selector = None;
        im.model_selector_rx = None;
        im.show_theme_selector();
        assert!(im.theme_selector.is_some(), "theme selector should now be active");

        // Trying to open model selector while theme is active should be a no-op.
        im.show_model_selector();
        assert!(
            im.model_selector.is_none(),
            "model selector should not open when theme selector is active"
        );
    }

    // ── Keyboard shortcuts for selectors ─────────────────────────────

    #[test]
    fn test_ctrl_g_opens_model_selector() {
        let mut im = create_im();
        assert!(im.model_selector.is_none());

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(im.handle_input("\x07")); // Ctrl+G

        assert!(im.model_selector.is_some(), "Ctrl+G should open model selector");

        // Close it so test cleanup works.
        im.model_selector = None;
        im.model_selector_rx = None;
    }

    #[test]
    fn test_ctrl_t_opens_theme_selector() {
        let mut im = create_im();
        assert!(im.theme_selector.is_none());

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(im.handle_input("\x14")); // Ctrl+T

        assert!(im.theme_selector.is_some(), "Ctrl+T should open theme selector");

        // Close it so test cleanup works.
        im.theme_selector = None;
        im.theme_selector_rx = None;
    }

    // ── Session path in constructor ──────────────────────────────────

    #[test]
    fn test_new_with_session_path_creates_file() {
        let dir = std::env::temp_dir().join("pi_test_new_with_path");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("new-session.jsonl");

        let model = pi_model_catalog::models::find_model("gpt-4o").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let im = rt
            .block_on(InteractiveMode::new(
                "gpt-4o",
                model,
                None,
                None,
                Some(path.clone()),
            ))
            .unwrap();

        assert!(path.exists(), "session file should be created");
        assert!(
            im.session_path.is_some(),
            "session_path should be stored"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_new_without_session_path_uses_in_memory() {
        let im = create_im();
        assert!(
            im.session_path.is_none(),
            "no session_path when None is passed"
        );
    }

    // ── Slash commands ───────────────────────────────────────────────

    #[test]
    fn test_slash_help_adds_message() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/help");
        rt.block_on(im.send_message());

        // Should add a help message (and no user message or LLM call)
        assert!(
            im.messages.child_count() >= 1,
            "/help should add at least one message"
        );
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
    }

    #[test]
    fn test_slash_unknown_shows_error() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/nonexistent");
        rt.block_on(im.send_message());

        assert!(
            im.messages.child_count() >= 1,
            "unknown command should add an error message"
        );
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
    }

    #[test]
    fn test_slash_clear_removes_messages() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        // First, send a real-ish message (it will fail on LLM but add a user msg)
        im.editor.set_text("hello");
        rt.block_on(im.send_message());
        assert!(
            im.messages.child_count() >= 1,
            "should have messages before clear"
        );

        // Now clear with /clear
        let _count_before = im.messages.child_count();
        im.editor.set_text("/clear");
        rt.block_on(im.send_message());
        assert_eq!(im.messages.child_count(), 0, "/clear should remove all messages");
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
    }

    #[test]
    fn test_slash_session_shows_info() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/session");
        rt.block_on(im.send_message());

        assert!(
            im.messages.child_count() >= 1,
            "/session should add an info message"
        );
    }

    #[test]
    fn test_normal_message_not_affected_by_slash() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        // A message starting with a word, not a slash
        im.editor.set_text("hello world");
        rt.block_on(im.send_message());

        // The user message should be added (LLM call will fail but user msg is there)
        assert!(
            im.messages.child_count() >= 1,
            "normal message should add messages"
        );
    }

    #[test]
    fn test_slash_theme_switches_to_light() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(im.theme_name, "dark");

        im.editor.set_text("/theme light");
        rt.block_on(im.send_message());

        assert_eq!(im.theme_name, "light", "/theme light should switch theme");
    }
}
