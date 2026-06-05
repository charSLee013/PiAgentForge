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

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent_core::agent_loop::agent_loop_with_queues;
use pi_agent_core::queue::{MessageQueue, QueueMode};
use pi_agent_core::session::session_manager::SessionManager;
use pi_agent_core::session::storage;
use pi_agent_core::session::types::{SessionEntry, SessionTreeNode, create_session_id};
use pi_agent_core::session::{
    build_session_file_path, clone_active_path_to_file, export_session_as_html, fork_path_to_file, list_sessions,
};
use pi_agent_core::{AgentContext, AgentState, call_llm_for_text, estimate_message_tokens, prepare_compaction};
use pi_ai_core::stream;
use pi_ai_core::thinking::{
    THINKING_LEVELS, clamp_thinking_level, default_thinking_level_for_model, is_valid_thinking_level,
    supported_thinking_levels, thinking_enabled,
};
use pi_ai_core::types::{ContentBlock, Context, Message, Model, StreamOptions, TextContent};
use pi_core::settings::Settings;
use pi_core::skills::discover_skills;
use pi_core::tool_registry::{ToolPreset, ToolSelection, execute_tool_for_selection, tool_definitions_for_selection};
use pi_tui_core::components::editor::Editor;
use pi_tui_core::components::select_list::{SelectItem, SelectList};
use pi_tui_core::{
    Component, Container, Terminal,
    keys::{KeyCode, parse_key},
    stdin_buffer::StdinBuffer,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::Theme;
use crate::components::assistant_message::{AssistantContentBlock, AssistantMessage};
use crate::components::footer::Footer;
use crate::components::model_selector::{ModelEntry, ModelSelector};
use crate::components::session_selector::{SessionEntry as SessionListEntry, SessionSelector};
use crate::components::theme_selector::ThemeSelector;
use crate::components::user_message::UserMessage;

#[cfg(feature = "feat-extensions")]
use crate::components::extension_selector::ExtensionSelector;
#[cfg(feature = "feat-extensions")]
use pi_extension_system::types::ExtensionManifest;

const COMPACTION_KEEP_RECENT_TOKENS: u64 = 512;
const PLAN_MODE_PROMPT_APPEND: &str = "You are in plan mode. Use only read-only tools and allowed read-only bash commands. Do not edit files or execute write actions. Explore the codebase, then reply with:\nPlan:\n1. <step one>\n2. <step two>\n...\nDo not execute the plan yet.";
const PLAN_EXECUTION_PROMPT_PREFIX: &str = "Execute the approved plan below. You may use the full toolset. As each step is completed, include a standalone marker in the form [DONE:n] where n is the completed step number.";

#[derive(Debug, Clone)]
struct PlanStep {
    index: usize,
    text: String,
    done: bool,
}

#[derive(Debug, Clone)]
struct PlanState {
    steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubagentMode {
    Single,
    Parallel,
    Chain,
}

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

/// Result from the session selector component.
#[derive(Debug)]
enum SessionSelectorAction {
    /// A session file path was selected.
    Selected { path: PathBuf },
    /// The user cancelled the selection.
    Cancelled,
}

/// Result from the tree selector component.
#[derive(Debug)]
enum TreeSelectorAction {
    /// A session entry was selected.
    Selected { entry_id: String },
    /// The user cancelled the selection.
    Cancelled,
}

#[derive(Debug)]
struct BackgroundRunResult {
    messages: Vec<Message>,
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
    /// Whether a background agent run is currently active.
    is_streaming: bool,
    /// Whether an inline compaction flow is currently active.
    is_compacting: bool,
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
    /// Current thinking level for this interactive session.
    thinking_level: String,
    /// Maximum number of turns allowed for the main interactive agent run.
    max_turns: u32,
    /// Whether plan mode is enabled for the next interactive run.
    plan_mode: bool,
    /// The most recently captured plan awaiting user action.
    pending_plan: Option<PlanState>,
    /// Progress tracker for an approved plan being executed.
    plan_progress: Option<PlanState>,
    /// Count of discovered skills after the last reload.
    loaded_skill_count: usize,
    /// Built-in tool selection derived from CLI startup flags.
    tool_selection: ToolSelection,
    /// Queue for steering messages injected during an active run.
    steering_queue: Arc<Mutex<MessageQueue>>,
    /// Queue for follow-up messages injected after the current turn completes.
    follow_up_queue: Arc<Mutex<MessageQueue>>,
    /// Receiver for the active background run result.
    background_run_rx: Option<mpsc::UnboundedReceiver<BackgroundRunResult>>,
    /// Path to the JSONL session file on disk (None = in-memory only).
    session_path: Option<PathBuf>,
    /// Preferred session directory for selection and new session files.
    session_dir: PathBuf,
    /// Model selector overlay (active when Some).
    model_selector: Option<ModelSelector>,
    /// Channel for receiving model-selection results from the overlay.
    model_selector_rx: Option<mpsc::UnboundedReceiver<ModelSelectorAction>>,
    /// Theme selector overlay (active when Some).
    theme_selector: Option<ThemeSelector>,
    /// Channel for receiving theme-selection results from the overlay.
    theme_selector_rx: Option<mpsc::UnboundedReceiver<ThemeSelectorAction>>,
    /// Session selector overlay (active when Some).
    session_selector: Option<SessionSelector>,
    /// Channel for receiving session-selection results from the overlay.
    session_selector_rx: Option<mpsc::UnboundedReceiver<SessionSelectorAction>>,
    /// Tree selector overlay (active when Some).
    tree_selector: Option<SelectList>,
    /// Channel for receiving tree-selection results from the overlay.
    tree_selector_rx: Option<mpsc::UnboundedReceiver<TreeSelectorAction>>,
    /// Whether cancelling the selector should exit the TUI.
    quit_on_session_selector_cancel: bool,
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
        session_dir: PathBuf,
    ) -> io::Result<Self> {
        Self::new_with_thinking_level(model_id, model, system_prompt, api_key, session_path, None, session_dir).await
    }

    pub async fn new_with_thinking_level(
        model_id: &str,
        model: &'static Model,
        system_prompt: Option<String>,
        api_key: Option<String>,
        session_path: Option<PathBuf>,
        requested_thinking_level: Option<String>,
        session_dir: PathBuf,
    ) -> io::Result<Self> {
        let terminal = Terminal::new()?;
        let theme = Theme::dark();
        let theme_name = "dark".to_string();

        let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();

        let footer = Footer::new(
            cwd.clone(),
            None,            // git_branch
            0,               // input_tokens
            0,               // output_tokens
            0,               // cache_read
            0,               // cache_write
            model_id.into(), // model_name
            0.0,             // context_percent
            100000,          // context_window
            false,           // auto_compact
            &theme,
        );

        let mut editor = Editor::new();
        editor.focused = true;
        editor.max_visible_lines = 5;

        let messages = Container::new();
        let stdin_buffer = StdinBuffer::new();

        // ── Session initialisation ────────────────────────────────────────
        let (session, resolved_path, resolved_model_id, resolved_model, restored_thinking_level) =
            if let Some(ref path) = session_path {
                if path.exists() {
                    // Resume session from disk
                    let (header, entries, _) =
                        storage::read_all(path).await.map_err(|e| io::Error::other(e.to_string()))?;
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

                    (
                        sm,
                        Some(path.clone()),
                        mid,
                        m,
                        clamp_thinking_level(m, requested_thinking_level.as_deref().unwrap_or(&ctx.thinking_level)),
                    )
                } else {
                    // Create new session file
                    let id = create_session_id();
                    let header = pi_agent_core::session::types::SessionHeader::new(&cwd, id);
                    storage::create(path, &header).await.map_err(|e| io::Error::other(e.to_string()))?;
                    let sm = SessionManager::new(header);
                    (
                        sm,
                        Some(path.clone()),
                        model_id.to_string(),
                        model,
                        clamp_thinking_level(
                            model,
                            requested_thinking_level.as_deref().unwrap_or(default_thinking_level_for_model(model)),
                        ),
                    )
                }
            } else {
                // In-memory session
                let sm = SessionManager::in_memory(cwd);
                (
                    sm,
                    None,
                    model_id.to_string(),
                    model,
                    clamp_thinking_level(
                        model,
                        requested_thinking_level.as_deref().unwrap_or(default_thinking_level_for_model(model)),
                    ),
                )
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
            is_streaming: false,
            is_compacting: false,
            model: resolved_model,
            session,
            system_prompt,
            providers_registered: false,
            model_id: resolved_model_id,
            api_key,
            thinking_level: restored_thinking_level,
            max_turns: 200,
            plan_mode: false,
            pending_plan: None,
            plan_progress: None,
            loaded_skill_count: 0,
            tool_selection: ToolSelection::all(),
            steering_queue: Arc::new(Mutex::new(MessageQueue::new(QueueMode::All))),
            follow_up_queue: Arc::new(Mutex::new(MessageQueue::new(QueueMode::All))),
            background_run_rx: None,
            session_path: resolved_path,
            session_dir,
            model_selector: None,
            model_selector_rx: None,
            theme_selector: None,
            theme_selector_rx: None,
            session_selector: None,
            session_selector_rx: None,
            tree_selector: None,
            tree_selector_rx: None,
            quit_on_session_selector_cancel: false,
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

    /// Override the built-in tool selection for this interactive session.
    pub fn set_tool_selection(&mut self, selection: ToolSelection) {
        self.tool_selection = selection;
    }

    pub fn set_max_turns(&mut self, max_turns: u32) {
        self.max_turns = max_turns;
    }

    pub fn is_streaming_for_test(&self) -> bool {
        self.is_streaming
    }

    pub fn session_for_test(&self) -> &SessionManager {
        &self.session
    }

    pub fn session_for_test_mut(&mut self) -> &mut SessionManager {
        &mut self.session
    }

    pub fn plan_mode_for_test(&self) -> bool {
        self.plan_mode
    }

    pub fn has_pending_plan_for_test(&self) -> bool {
        self.pending_plan.is_some()
    }

    pub fn latest_assistant_text_for_test(&self) -> Option<String> {
        self.latest_assistant_text()
    }

    pub async fn run_subagent_command_for_test(&mut self, spec: Option<&str>) -> io::Result<String> {
        self.run_subagent_command(spec).await
    }

    pub fn set_editor_text_for_test(&mut self, text: &str) {
        self.editor.set_text(text);
    }

    pub async fn send_message_for_test(&mut self) {
        self.send_message().await;
    }

    pub async fn poll_background_run_for_test(&mut self) -> bool {
        self.poll_background_run().await
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
                let background_changed = self.poll_background_run().await;
                // Re-render if we are still running.
                if self.running && (!sequences.is_empty() || background_changed) {
                    self.render_all()?;
                }
            } else {
                // Avoid busy-looping when no input is available.
                let background_changed = self.poll_background_run().await;
                if self.running && background_changed {
                    self.render_all()?;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        // ── Cleanup ──────────────────────────────────────────────────────
        self.terminal.stop()?; // disable raw mode
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
        if self.session_selector.is_some() {
            self.handle_session_selector_input(data).await;
            return;
        }
        if self.tree_selector.is_some() {
            self.handle_tree_selector_input(data).await;
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

    /// Route input to the tree selector overlay and process its result.
    async fn handle_tree_selector_input(&mut self, data: &str) {
        if let Some(ref mut selector) = self.tree_selector {
            selector.handle_input(data);
        }

        if let Some(ref mut rx) = self.tree_selector_rx {
            if let Ok(action) = rx.try_recv() {
                match action {
                    TreeSelectorAction::Selected { entry_id } => {
                        if let Err(err) = self.navigate_tree_to_entry(&entry_id).await {
                            let err_msg = AssistantMessage::new(
                                vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                                false,
                                "Thinking...".into(),
                                Some("error".into()),
                                None,
                                &self.theme,
                            );
                            self.messages.add(err_msg);
                        }
                    }
                    TreeSelectorAction::Cancelled => {}
                }
                self.tree_selector = None;
                self.tree_selector_rx = None;
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

    /// Route input to the session selector overlay and process its result.
    async fn handle_session_selector_input(&mut self, data: &str) {
        if let Some(ref mut selector) = self.session_selector {
            selector.handle_input(data);
        }

        if let Some(ref mut rx) = self.session_selector_rx {
            if let Ok(action) = rx.try_recv() {
                match action {
                    SessionSelectorAction::Selected { path } => {
                        if let Err(err) = self.load_session_from_path(path).await {
                            let err_msg = AssistantMessage::new(
                                vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                                false,
                                "Thinking...".into(),
                                Some("error".into()),
                                None,
                                &self.theme,
                            );
                            self.messages.add(err_msg);
                        }
                    }
                    SessionSelectorAction::Cancelled => {
                        if self.quit_on_session_selector_cancel {
                            self.running = false;
                        }
                    }
                }

                self.quit_on_session_selector_cancel = false;
                self.session_selector = None;
                self.session_selector_rx = None;
            }
        }
    }

    async fn poll_background_run(&mut self) -> bool {
        let Some(rx) = self.background_run_rx.as_mut() else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };

        self.background_run_rx = None;
        self.is_streaming = false;
        self.steering_queue.lock().expect("steering queue poisoned").clear();
        self.follow_up_queue.lock().expect("follow-up queue poisoned").clear();

        for message in result.messages {
            self.append_session_message(&message).await;
            self.add_message_component(&message);
        }

        if self.plan_mode {
            self.capture_pending_plan();
        }
        self.refresh_plan_progress_from_session();

        true
    }

    fn current_tool_preset(&self) -> ToolPreset {
        if self.plan_mode { ToolPreset::PlanReadOnly } else { ToolPreset::Full }
    }

    fn effective_system_prompt(&self) -> Option<String> {
        if self.plan_mode {
            let mut prompt = self.system_prompt.clone().unwrap_or_default();
            if !prompt.is_empty() {
                prompt.push_str("\n\n");
            }
            prompt.push_str(PLAN_MODE_PROMPT_APPEND);
            Some(prompt)
        } else {
            self.system_prompt.clone()
        }
    }

    async fn start_background_run(&mut self, session_messages: Vec<Message>, start_len: usize) {
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let system_prompt = self.effective_system_prompt();
        let thinking_level = self.thinking_level.clone();
        let steering_queue = self.steering_queue.clone();
        let follow_up_queue = self.follow_up_queue.clone();
        let tool_preset = self.current_tool_preset();
        let tool_selection = self.tool_selection.clone();
        let max_turns = self.max_turns;
        let (tx, rx) = mpsc::unbounded_channel::<BackgroundRunResult>();
        self.background_run_rx = Some(rx);
        self.is_streaming = true;

        tokio::spawn(async move {
            let tools = tool_definitions_for_selection(tool_preset, &tool_selection);
            let mut state = AgentState {
                messages: session_messages,
                context: AgentContext {
                    messages: vec![],
                    system_prompt,
                    tools,
                    model: Some(model.id.clone()),
                    max_turns,
                    current_turn: 0,
                },
                pending_tool_calls: vec![],
            };

            let stream_model = model.clone();
            let stream_options =
                StreamOptions { api_key, thinking: Some(thinking_enabled(&thinking_level)), ..Default::default() };
            let cancel = tokio_util::sync::CancellationToken::new();
            let cancel_for_tools = cancel.clone();
            let tool_executor = move |name: &str, _id: &str, args: &serde_json::Value| {
                let cancel = cancel_for_tools.clone();
                let name = name.to_string();
                let args = args.clone();
                let rt_handle = tokio::runtime::Handle::current();
                let tool_selection = tool_selection.clone();
                tokio::task::block_in_place(move || {
                    rt_handle.block_on(async move {
                        execute_tool_for_selection(&name, args, cancel, tool_preset, &tool_selection)
                            .await
                            .map_err(|err| err.to_string())
                    })
                })
            };

            let mut skip_initial_steer_drain = true;
            let steer_fn = move || {
                if skip_initial_steer_drain {
                    skip_initial_steer_drain = false;
                    Vec::new()
                } else {
                    steering_queue.lock().expect("steering queue poisoned").drain()
                }
            };
            let follow_fn = move || follow_up_queue.lock().expect("follow-up queue poisoned").drain();

            let result = agent_loop_with_queues(
                &mut state,
                |ctx: Context| stream::stream(&stream_model, ctx, stream_options.clone()),
                tool_executor,
                |_| {},
                cancel,
                Some(steer_fn),
                Some(follow_fn),
                false,
                None,
            )
            .await;

            let mut messages = state.messages[start_len..].to_vec();
            if let Err(err) = result {
                messages.push(Message::assistant(vec![ContentBlock::Text(TextContent {
                    text: format!("Error: {}", err),
                })]));
            }

            let _ = tx.send(BackgroundRunResult { messages });
        });
    }

    async fn submit_user_prompt(&mut self, trimmed: &str) {
        if self.is_streaming {
            let msg = AssistantMessage::new(
                vec![AssistantContentBlock::Text(
                    "Agent is still processing. Use /steer <message> or /follow-up <message> while streaming.".into(),
                )],
                false,
                "Thinking...".into(),
                Some("info".into()),
                None,
                &self.theme,
            );
            self.messages.add(msg);
            self.render_all().ok();
            return;
        }

        // Add the user message to the messages container.
        let user_message = Message::user_text(trimmed);
        self.add_message_component(&user_message);

        // Clear the editor for the next prompt.
        self.editor.set_text("");

        // Register providers on first call.
        if !self.providers_registered {
            pi_cli::register_builtin_providers().await;
            self.providers_registered = true;
        }

        // Append user message to session history.
        self.append_session_message(&user_message).await;

        // Build conversation context from session history.
        let session_context = self.session.build_context();
        let start_len = session_context.messages.len();
        self.start_background_run(session_context.messages, start_len).await;

        // Re-render after response is complete.
        self.render_all().ok();
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

        self.submit_user_prompt(&trimmed).await;
    }

    async fn append_session_message(&mut self, message: &Message) {
        let msg_value = serde_json::to_value(message).expect("session message should serialize");
        let entry_id = self.session.append_message(msg_value);
        let _ = self.persist_entry(&entry_id).await;
    }

    fn add_message_component(&mut self, message: &Message) {
        match message.role {
            pi_ai_core::types::MessageRole::User => {
                let text = extract_text_from_blocks(&message.content);
                if !text.trim().is_empty() {
                    self.messages.add(UserMessage::new(text, &self.theme));
                }
            }
            pi_ai_core::types::MessageRole::Assistant => {
                let blocks = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(AssistantContentBlock::Text(text.text.clone())),
                        ContentBlock::Thinking(thinking) => {
                            Some(AssistantContentBlock::Thinking(thinking.thinking.clone()))
                        }
                        ContentBlock::ToolCall(tool_call) => Some(AssistantContentBlock::ToolCall {
                            name: tool_call.name.clone(),
                            args: serde_json::to_string_pretty(&tool_call.arguments)
                                .unwrap_or_else(|_| tool_call.arguments.to_string()),
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !blocks.is_empty() {
                    self.messages.add(AssistantMessage::new(
                        blocks,
                        false,
                        "Thinking...".into(),
                        None,
                        None,
                        &self.theme,
                    ));
                }
            }
            pi_ai_core::types::MessageRole::Tool => {
                let text = render_tool_message(message);
                if !text.trim().is_empty() {
                    self.messages.add(AssistantMessage::new(
                        vec![AssistantContentBlock::Text(text)],
                        false,
                        "Thinking...".into(),
                        None,
                        None,
                        &self.theme,
                    ));
                }
            }
            pi_ai_core::types::MessageRole::System => {
                let text = extract_text_from_blocks(&message.content);
                if !text.trim().is_empty() {
                    self.messages.add(AssistantMessage::new(
                        vec![AssistantContentBlock::Text(text)],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    ));
                }
            }
        }
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
                    "  /steer <message>    Queue a steering message while streaming",
                    "  /follow-up <msg>    Queue a follow-up message while streaming",
                    "  /thinking [level]   Show or set thinking level",
                    "  /plan [action]      Enable, execute, refine, or disable plan mode",
                    "  /subagent <mode>    Run single, parallel, or chain subagents",
                    "  /theme <name>       Switch theme: dark | light (opens selector if no name)",
                    "  /clear              Clear all messages (keeps session)",
                    "  /session            Show session info (model, theme, message count)",
                    "  /copy               Copy the last assistant message",
                    "  /reload             Reload settings, skills, and extensions",
                    "  /compact [prompt]   Compact session context",
                    "  /fork [entry-id]    Fork from latest or specified user message",
                    "  /clone              Clone the current active path into a new session",
                    "  /tree               Show session entry tree",
                    "  /new                Start a new session",
                    "  /quit               Exit pi",
                    "  /settings           Quick settings reference",
                    "  /login              Provider login instructions",
                    "  /logout             Remove stored credentials",
                    "  /name <name>        Set session display name",
                    "  /hotkeys            Show keyboard shortcuts",
                    "  /scoped-models      Open model selector",
                    "  /export [file]      Export current session to HTML",
                    "  /import             Import session (not yet implemented)",
                    "  /resume             Open the session selector",
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
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot change model while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                if parts.len() > 1 {
                    let model_name = parts[1];
                    if let Some(model) = pi_model_catalog::models::find_model(model_name) {
                        let provider = format!("{:?}", model.provider).to_lowercase();
                        self.apply_model_change(&provider, model_name).await;
                    } else {
                        let err_msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!("Unknown model: {}", model_name))],
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
            "/steer" => {
                if !self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("No active run to steer.".into())],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                } else if let Some(message) = parts.get(1).map(|value| value.trim()).filter(|value| !value.is_empty()) {
                    self.steering_queue.lock().expect("steering queue poisoned").enqueue(Message::user_text(message));
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Queued steering message.".into())],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                } else {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Usage: /steer <message>".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            }
            "/follow-up" | "/followup" => {
                if !self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("No active run to continue.".into())],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                } else if let Some(message) = parts.get(1).map(|value| value.trim()).filter(|value| !value.is_empty()) {
                    self.follow_up_queue.lock().expect("follow-up queue poisoned").enqueue(Message::user_text(message));
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Queued follow-up message.".into())],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                } else {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Usage: /follow-up <message>".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            }
            "/theme" => {
                if parts.len() > 1 {
                    self.apply_theme_change(parts[1]);
                } else {
                    self.show_theme_selector();
                }
            }
            "/thinking" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(
                            "Cannot change thinking level while a run is streaming.".into(),
                        )],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                if parts.len() > 1 {
                    match self.apply_thinking_level(parts[1]).await {
                        Ok(level) => {
                            let msg = AssistantMessage::new(
                                vec![AssistantContentBlock::Text(format!("Thinking level set to {}.", level))],
                                false,
                                "Thinking...".into(),
                                Some("info".into()),
                                None,
                                &self.theme,
                            );
                            self.messages.add(msg);
                        }
                        Err(err) => {
                            let msg = AssistantMessage::new(
                                vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                                false,
                                "Thinking...".into(),
                                Some("error".into()),
                                None,
                                &self.theme,
                            );
                            self.messages.add(msg);
                        }
                    }
                } else {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!(
                            "Current thinking level: {}\nSupported levels: {}",
                            self.thinking_level,
                            supported_thinking_levels_text(self.model),
                        ))],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            }
            "/clear" => {
                self.messages = Container::new();
            }
            "/plan" => match self.handle_plan_command(parts.get(1).copied()).await {
                Ok(info) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(info)],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
                Err(err) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            },
            "/subagent" => match self.run_subagent_command(parts.get(1).copied()).await {
                Ok(info) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(info)],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
                Err(err) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            },
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
                            let caps =
                                if e.capabilities.is_empty() { "none".to_string() } else { e.capabilities.join(", ") };
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
                let queued = self.steering_queue.lock().expect("steering queue poisoned").len()
                    + self.follow_up_queue.lock().expect("follow-up queue poisoned").len();
                let plan_progress = self
                    .plan_progress
                    .as_ref()
                    .map(|plan| {
                        let done = plan.steps.iter().filter(|step| step.done).count();
                        format!("{done}/{}", plan.steps.len())
                    })
                    .unwrap_or_else(|| "none".into());
                let info = format!(
                    "Session info:\n  Messages: {}\n  Model: {}\n  Thinking: {}\n  Streaming: {}\n  Compacting: {}\n  Queued: {}\n  Theme: {}\n  Plan mode: {}\n  Pending plan: {}\n  Plan progress: {}\n  Reloaded skills: {}",
                    entries.len(),
                    self.model_id,
                    self.thinking_level,
                    self.is_streaming,
                    self.is_compacting,
                    queued,
                    self.theme_name,
                    self.plan_mode,
                    self.pending_plan.is_some(),
                    plan_progress,
                    self.loaded_skill_count,
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
            "/copy" => match self.copy_last_assistant_message().await {
                Ok(info) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(info)],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
                Err(err) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            },
            "/reload" => match self.reload_runtime_resources().await {
                Ok(info) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(info)],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
                Err(err) => {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            },
            "/quit" => self.running = false,
            "/new" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(
                            "Cannot start a new session while a run is streaming.".into(),
                        )],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                self.messages = Container::new();
                self.session = pi_agent_core::session::session_manager::SessionManager::in_memory(".");
                self.background_run_rx = None;
                self.is_streaming = false;
                self.is_compacting = false;
                self.plan_mode = false;
                self.pending_plan = None;
                self.plan_progress = None;
                self.steering_queue.lock().expect("steering queue poisoned").clear();
                self.follow_up_queue.lock().expect("follow-up queue poisoned").clear();
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Started a new session.".into())],
                    false,
                    "Thinking...".into(),
                    None,
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/compact" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot compact while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                match self
                    .compact_current_session(parts.get(1).copied().map(str::trim).filter(|value| !value.is_empty()))
                    .await
                {
                    Ok(_) => {}
                    Err(err) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                            false,
                            "Thinking...".into(),
                            Some("error".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                }
            }
            "/fork" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot fork while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                match self.fork_current_session(parts.get(1).copied()).await {
                    Ok(info) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(info)],
                            false,
                            "Thinking...".into(),
                            Some("info".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                    Err(err) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                            false,
                            "Thinking...".into(),
                            Some("error".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                }
            }
            "/clone" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot clone while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                match self.clone_current_session().await {
                    Ok(info) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(info)],
                            false,
                            "Thinking...".into(),
                            Some("info".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                    Err(err) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                            false,
                            "Thinking...".into(),
                            Some("error".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                }
            }
            "/tree" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot navigate the tree while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                if let Err(err) = self.show_tree_selector() {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            }
            "/settings" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Settings: Ctrl+P model, Ctrl+T theme.".into())],
                    false,
                    "Thinking...".into(),
                    None,
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/login" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Login: run `pi --login <provider>` from CLI.".into())],
                    false,
                    "Thinking...".into(),
                    Some("info".into()),
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/logout" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("Logout: remove credentials from ~/.pi/auth.json.".into())],
                    false,
                    "Thinking...".into(),
                    Some("info".into()),
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/name" => {
                let name = if parts.len() > 1 { parts[1] } else { "unnamed" };
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("Session name: {}", name))],
                    false,
                    "Thinking...".into(),
                    None,
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/hotkeys" => {
                let hk = ["Ctrl+P model", "Ctrl+T theme", "Escape quit", "/help commands"];
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text(format!("Keys:\n{}", hk.join("\n")))],
                    false,
                    "Thinking...".into(),
                    None,
                    None,
                    &self.theme,
                );
                self.messages.add(msg);
            }
            "/scoped-models" => self.show_model_selector(),
            "/export" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot export while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                match self.export_current_session(parts.get(1).copied()).await {
                    Ok(path) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!("Exported session to {}", path.display()))],
                            false,
                            "Thinking...".into(),
                            Some("info".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                    Err(err) => {
                        let msg = AssistantMessage::new(
                            vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                            false,
                            "Thinking...".into(),
                            Some("error".into()),
                            None,
                            &self.theme,
                        );
                        self.messages.add(msg);
                    }
                }
            }
            "/resume" => {
                if self.is_streaming {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text("Cannot switch sessions while a run is streaming.".into())],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    return;
                }
                if let Err(err) = self.show_session_selector(false).await {
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!("Error: {}", err))],
                        false,
                        "Thinking...".into(),
                        Some("error".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                }
            }
            "/import" => {
                let msg = AssistantMessage::new(
                    vec![AssistantContentBlock::Text("/import: not yet implemented.".into())],
                    false,
                    "Thinking...".into(),
                    Some("info".into()),
                    None,
                    &self.theme,
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
    async fn persist_entry(&self, entry_id: &str) -> io::Result<()> {
        let path = match self.session_path {
            Some(ref p) => p.clone(),
            None => return Ok(()),
        };
        let entry = self
            .session
            .get_entry(entry_id)
            .ok_or_else(|| io::Error::other(format!("Entry {} not found", entry_id)))?;
        storage::append(&path, entry).await.map_err(|err| io::Error::other(err.to_string()))
    }

    /// Rebuild the in-memory message container from session entries.
    ///
    /// Called when resuming a session from disk so that the user can see
    /// previous conversation history.
    fn load_entries_into_container(&mut self) {
        let context = self.session.build_context();
        for message in &context.messages {
            self.add_message_component(message);
        }
    }

    /// Replace the current in-memory session state with a persisted session file.
    async fn load_session_from_path(&mut self, path: PathBuf) -> io::Result<()> {
        let (header, entries, _) = storage::read_all(&path).await.map_err(|e| io::Error::other(e.to_string()))?;
        let session = SessionManager::from_entries(header, entries);

        let ctx = session.build_context();
        let (resolved_model_id, resolved_model) = match ctx.model {
            Some((_provider, ref mid)) => {
                if let Some(model) = pi_model_catalog::models::find_model(mid) {
                    (mid.clone(), model)
                } else {
                    (self.model_id.clone(), self.model)
                }
            }
            None => (self.model_id.clone(), self.model),
        };

        self.messages = Container::new();
        self.session = session;
        self.session_path = Some(path);
        self.model_id = resolved_model_id.clone();
        self.model_name = resolved_model_id;
        self.model = resolved_model;
        self.thinking_level = clamp_thinking_level(self.model, &ctx.thinking_level);
        self.pending_plan = None;
        self.plan_progress = None;
        self.editor.set_text("");
        self.load_entries_into_container();
        Ok(())
    }

    /// Open the session selector overlay.
    pub async fn show_session_selector(&mut self, quit_on_cancel: bool) -> io::Result<()> {
        let busy = self.session_selector.is_some()
            || self.tree_selector.is_some()
            || self.model_selector.is_some()
            || self.theme_selector.is_some();
        #[cfg(feature = "feat-extensions")]
        let busy = busy || self.extension_selector.is_some();
        if busy {
            return Ok(());
        }

        let summaries = list_sessions(&self.session_dir).await.map_err(|e| io::Error::other(e.to_string()))?;
        let sessions: Vec<SessionListEntry> = summaries
            .into_iter()
            .map(|summary| {
                let display_name = summary.name.clone().unwrap_or_else(|| summary.id.clone());
                let search_text = format!(
                    "{} {} {} {} {}",
                    summary.id, summary.cwd, display_name, summary.first_message, summary.all_messages_text
                );
                SessionListEntry {
                    id: summary.path.to_string_lossy().to_string(),
                    name: Some(display_name),
                    search_text,
                    has_name: summary.name.as_deref().is_some_and(|name| !name.is_empty()),
                }
            })
            .collect();

        let (tx, rx) = mpsc::unbounded_channel::<SessionSelectorAction>();
        let mut selector = SessionSelector::new(sessions, &self.theme);
        let select_tx = tx;
        let cancel_tx = select_tx.clone();
        selector.on_select = Some(Box::new(move |path| {
            let _ = select_tx.send(SessionSelectorAction::Selected { path: PathBuf::from(path) });
        }));
        selector.on_cancel = Some(Box::new(move || {
            let _ = cancel_tx.send(SessionSelectorAction::Cancelled);
        }));

        self.quit_on_session_selector_cancel = quit_on_cancel;
        self.session_selector = Some(selector);
        self.session_selector_rx = Some(rx);
        Ok(())
    }

    fn show_tree_selector(&mut self) -> io::Result<()> {
        let busy = self.session_selector.is_some()
            || self.tree_selector.is_some()
            || self.model_selector.is_some()
            || self.theme_selector.is_some();
        #[cfg(feature = "feat-extensions")]
        let busy = busy || self.extension_selector.is_some();
        if busy {
            return Ok(());
        }

        let tree = self.session.get_tree();
        if tree.is_empty() {
            return Err(io::Error::other("No entries in session"));
        }

        let items = build_tree_select_items(&tree);
        let current_leaf = self.session.leaf_id().cloned();
        let selected_index = current_leaf
            .as_deref()
            .and_then(|leaf_id| items.iter().position(|item| item.value == leaf_id))
            .unwrap_or_else(|| items.len().saturating_sub(1));

        let (tx, rx) = mpsc::unbounded_channel::<TreeSelectorAction>();
        let mut selector = SelectList::new(items, 16, self.theme.to_select_list_theme());
        selector.set_selected_index(selected_index);
        let select_tx = tx;
        let cancel_tx = select_tx.clone();
        selector.on_select = Some(Box::new(move |item| {
            let _ = select_tx.send(TreeSelectorAction::Selected { entry_id: item.value.clone() });
        }));
        selector.on_cancel = Some(Box::new(move || {
            let _ = cancel_tx.send(TreeSelectorAction::Cancelled);
        }));

        self.tree_selector = Some(selector);
        self.tree_selector_rx = Some(rx);
        Ok(())
    }

    fn default_export_path(&self) -> PathBuf {
        match &self.session_path {
            Some(path) => path.with_extension("html"),
            None => self.session_dir.join(format!("{}-export.html", self.session.session_id())),
        }
    }

    async fn export_current_session(&self, output: Option<&str>) -> io::Result<PathBuf> {
        let output_path = output.map(PathBuf::from).unwrap_or_else(|| self.default_export_path());
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let html = export_session_as_html(self.session.header(), &self.session.entries());
        tokio::fs::write(&output_path, html).await?;
        Ok(output_path)
    }

    fn latest_user_message_on_active_path(&self) -> Option<(String, String)> {
        let path = self.session.path_to_root(None);
        for entry in path.into_iter().rev() {
            if let SessionEntry::Message(msg) = entry {
                if msg.message.get("role").and_then(|v| v.as_str()) == Some("user") {
                    let text = msg
                        .message
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter_map(|block| {
                                    if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                                        block.get("text").and_then(|v| v.as_str()).map(str::to_string)
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    return Some((msg.id.clone(), text));
                }
            }
        }
        None
    }

    fn resolve_fork_target(&self, spec: Option<&str>) -> io::Result<(String, String)> {
        if let Some(raw) = spec.map(str::trim).filter(|s| !s.is_empty()) {
            for entry in self.session.path_to_root(None).into_iter().rev() {
                if let SessionEntry::Message(msg) = entry {
                    let is_user = msg.message.get("role").and_then(|v| v.as_str()) == Some("user");
                    if is_user && msg.id.starts_with(raw) {
                        let text = msg
                            .message
                            .get("content")
                            .and_then(|c| c.as_array())
                            .map(|blocks| {
                                blocks
                                    .iter()
                                    .filter_map(|block| {
                                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                                            block.get("text").and_then(|v| v.as_str()).map(str::to_string)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_default();
                        return Ok((msg.id.clone(), text));
                    }
                }
            }
            return Err(io::Error::other(format!("No user message on the active path matches '{}'", raw)));
        }

        self.latest_user_message_on_active_path()
            .ok_or_else(|| io::Error::other("No user message available to fork from"))
    }

    async fn clone_current_session(&mut self) -> io::Result<String> {
        let dest_path = build_session_file_path(&self.session_dir, &self.model_id);
        clone_active_path_to_file(&self.session, &dest_path, self.session_path.as_deref())
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.load_session_from_path(dest_path.clone()).await?;
        self.editor.set_text("");
        Ok(format!("Cloned active path into {}", dest_path.display()))
    }

    async fn fork_current_session(&mut self, spec: Option<&str>) -> io::Result<String> {
        let (entry_id, text) = self.resolve_fork_target(spec)?;
        let dest_path = build_session_file_path(&self.session_dir, &self.model_id);
        fork_path_to_file(&self.session, &entry_id, &dest_path, self.session_path.as_deref())
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.load_session_from_path(dest_path.clone()).await?;
        self.editor.set_text(&text);
        Ok(format!("Forked session from user entry {} into {}", entry_id, dest_path.display()))
    }

    async fn navigate_tree_to_entry(&mut self, target_id: &str) -> io::Result<()> {
        if self.session.leaf_id().is_some_and(|leaf_id| leaf_id == target_id) {
            return Ok(());
        }

        let target_entry = self
            .session
            .get_entry(target_id)
            .cloned()
            .ok_or_else(|| io::Error::other(format!("Entry {} not found", target_id)))?;

        let editor_text = match &target_entry {
            SessionEntry::Message(message)
                if message.message.get("role").and_then(|value| value.as_str()) == Some("user") =>
            {
                match message.parent_id.as_deref() {
                    Some(parent_id) => {
                        self.session.branch(parent_id).map_err(|err| io::Error::other(err.to_string()))?
                    }
                    None => self.session.reset_leaf(),
                }
                extract_message_text(&message.message)
            }
            _ => {
                self.session.branch(target_id).map_err(|err| io::Error::other(err.to_string()))?;
                String::new()
            }
        };

        self.messages = Container::new();
        self.load_entries_into_container();
        self.editor.set_text(&editor_text);
        Ok(())
    }

    async fn compact_current_session(&mut self, custom_instructions: Option<&str>) -> io::Result<String> {
        if !self.providers_registered {
            pi_cli::register_builtin_providers().await;
            self.providers_registered = true;
        }

        self.is_compacting = true;
        let active_entries: Vec<SessionEntry> = self.session.path_to_root(None).into_iter().cloned().collect();
        let Some(preparation) = prepare_compaction(&active_entries, COMPACTION_KEEP_RECENT_TOKENS) else {
            self.is_compacting = false;
            return Err(io::Error::other("Nothing to compact (session too small)"));
        };
        let first_kept_entry = match preparation.entries_to_keep.first() {
            Some(entry) => entry,
            None => {
                self.is_compacting = false;
                return Err(io::Error::other("Nothing to compact (no kept entries)"));
            }
        };

        let entries_text =
            preparation.entries_to_summarize.iter().filter_map(compaction_entry_text).collect::<Vec<_>>();
        let prompt = build_compaction_prompt(&entries_text, custom_instructions);
        let options = StreamOptions {
            api_key: self.api_key.clone(),
            thinking: Some(thinking_enabled(&self.thinking_level)),
            ..Default::default()
        };
        let summary =
            call_llm_for_text(&prompt, "You are a helpful assistant that summarizes conversations.", |ctx: Context| {
                stream::stream(self.model, ctx, options.clone())
            })
            .await
            .map_err(|err| {
                self.is_compacting = false;
                io::Error::other(err.to_string())
            })?;
        let tokens_before = preparation.entries_to_summarize.iter().map(compaction_entry_tokens).sum();

        let previous_session = self.session.clone();
        let entry_id = self.session.append_compaction(summary, first_kept_entry.id().to_string(), tokens_before);
        if let Err(err) = self.persist_entry(&entry_id).await {
            self.session = previous_session;
            self.is_compacting = false;
            return Err(io::Error::other(format!("Failed to persist compaction entry: {}", err)));
        }
        self.messages = Container::new();
        self.load_entries_into_container();
        self.editor.set_text("");
        self.is_compacting = false;

        Ok("Compacted current session context.".to_string())
    }

    fn latest_assistant_text(&self) -> Option<String> {
        self.session.build_context().messages.into_iter().rev().find_map(|message| {
            if message.role == pi_ai_core::types::MessageRole::Assistant {
                let text = extract_text_from_blocks(&message.content).trim().to_string();
                if text.is_empty() { None } else { Some(text) }
            } else {
                None
            }
        })
    }

    async fn copy_last_assistant_message(&self) -> io::Result<String> {
        let text =
            self.latest_assistant_text().ok_or_else(|| io::Error::other("No assistant message available to copy"))?;
        copy_text_to_clipboard(&text)?;
        Ok("Copied the last assistant message to the clipboard.".to_string())
    }

    fn capture_pending_plan(&mut self) {
        let Some(text) = self.latest_assistant_text() else {
            return;
        };
        let steps = parse_plan_steps(&text);
        if steps.is_empty() {
            return;
        }
        self.pending_plan = Some(PlanState {
            steps: steps
                .into_iter()
                .enumerate()
                .map(|(idx, text)| PlanStep { index: idx + 1, text, done: false })
                .collect(),
        });
        let msg = AssistantMessage::new(
            vec![AssistantContentBlock::Text(
                "Plan captured. Use /plan execute to run it, /plan stay to keep planning, or /plan refine to revise it."
                    .into(),
            )],
            false,
            "Thinking...".into(),
            Some("info".into()),
            None,
            &self.theme,
        );
        self.messages.add(msg);
    }

    fn refresh_plan_progress_from_session(&mut self) {
        let Some(text) = self.latest_assistant_text() else {
            return;
        };
        let Some(progress) = self.plan_progress.as_mut() else {
            return;
        };
        let done_markers = parse_done_markers(&text);
        if done_markers.is_empty() {
            return;
        }
        let mut changed = false;
        for marker in done_markers {
            if let Some(step) = progress.steps.iter_mut().find(|step| step.index == marker && !step.done) {
                step.done = true;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        let done = progress.steps.iter().filter(|step| step.done).count();
        let total = progress.steps.len();
        let msg = AssistantMessage::new(
            vec![AssistantContentBlock::Text(format!("Plan progress: {done}/{total} completed."))],
            false,
            "Thinking...".into(),
            Some("info".into()),
            None,
            &self.theme,
        );
        self.messages.add(msg);
    }

    async fn handle_plan_command(&mut self, action: Option<&str>) -> io::Result<String> {
        if self.is_streaming || self.is_compacting {
            return Err(io::Error::other("Cannot change plan mode while a run is active."));
        }
        match action.map(str::trim).filter(|value| !value.is_empty()) {
            None => {
                self.plan_mode = true;
                self.pending_plan = None;
                Ok("Plan mode enabled. The next prompt will use read-only tools and should return a numbered Plan:"
                    .into())
            }
            Some("off") | Some("disable") => {
                self.plan_mode = false;
                self.pending_plan = None;
                Ok("Plan mode disabled.".into())
            }
            Some("stay") => Ok(if self.plan_mode {
                "Plan mode is still enabled.".into()
            } else {
                "No active plan mode to keep.".into()
            }),
            Some("refine") => {
                let Some(plan) = self.pending_plan.take() else {
                    return Err(io::Error::other("No captured plan available to refine."));
                };
                self.plan_mode = true;
                self.editor.set_text(&build_plan_refine_prompt(&plan));
                Ok("Prefilled the editor with a refine prompt for the captured plan.".into())
            }
            Some("execute") => {
                let Some(plan) = self.pending_plan.take() else {
                    return Err(io::Error::other("No captured plan available to execute."));
                };
                self.plan_mode = false;
                self.plan_progress = Some(plan.clone());
                self.submit_user_prompt(&build_plan_execution_prompt(&plan)).await;
                Ok("Started executing the approved plan.".into())
            }
            Some(other) => Err(io::Error::other(format!(
                "Unknown /plan action '{}'. Use /plan, /plan execute, /plan stay, /plan refine, or /plan off.",
                other
            ))),
        }
    }

    async fn reload_runtime_resources(&mut self) -> io::Result<String> {
        if self.is_streaming || self.is_compacting {
            return Err(io::Error::other("Cannot reload while a run is active."));
        }

        let settings = load_settings_for_reload().map_err(|err| io::Error::other(err.to_string()))?;
        if let Some(theme) = settings.theme.as_deref() {
            self.apply_theme_change(theme);
        }

        let cwd_skills = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".pi").join("skills");
        let skills = discover_skills(&[cwd_skills]);
        self.loaded_skill_count = skills.skills.len();

        #[cfg(feature = "feat-extensions")]
        {
            self.extensions = reload_extension_manifests();
        }

        let mut parts = vec![format!("theme={}", self.theme_name), format!("skills={}", self.loaded_skill_count)];
        if !skills.errors.is_empty() {
            parts.push(format!("skill_errors={}", skills.errors.len()));
        }
        #[cfg(feature = "feat-extensions")]
        parts.push(format!("extensions={}", self.extensions.len()));

        Ok(format!("Reloaded {}.", parts.join(", ")))
    }

    async fn run_subagent_command(&mut self, spec: Option<&str>) -> io::Result<String> {
        if self.is_streaming || self.is_compacting {
            return Err(io::Error::other("Cannot run subagents while a run is active."));
        }
        if !self.providers_registered {
            pi_cli::register_builtin_providers().await;
            self.providers_registered = true;
        }

        let (mode, tasks) = parse_subagent_spec(spec)?;
        let summary = match mode {
            SubagentMode::Single => {
                let task = tasks.first().cloned().ok_or_else(|| io::Error::other("Missing subagent task"))?;
                let output = run_ephemeral_agent_task(
                    self.model.clone(),
                    self.api_key.clone(),
                    self.thinking_level.clone(),
                    build_subagent_prompt(&task),
                    ToolPreset::Full,
                    self.tool_selection.clone(),
                )
                .await?;
                format!("Subagent (single)\n\nTask:\n{task}\n\nResult:\n{output}")
            }
            SubagentMode::Parallel => {
                let total = tasks.len();
                let mut results: Vec<Option<(String, io::Result<String>)>> = (0..total).map(|_| None).collect();
                let mut join_set = JoinSet::new();
                for (idx, task) in tasks.iter().cloned().enumerate() {
                    let model = self.model.clone();
                    let api_key = self.api_key.clone();
                    let thinking_level = self.thinking_level.clone();
                    let tool_selection = self.tool_selection.clone();
                    join_set.spawn(async move {
                        let result = run_ephemeral_agent_task(
                            model,
                            api_key,
                            thinking_level,
                            build_subagent_prompt(&task),
                            ToolPreset::Full,
                            tool_selection,
                        )
                        .await;
                        (idx, task, result)
                    });
                }

                let mut completed = 0usize;
                while let Some(joined) = join_set.join_next().await {
                    let (idx, task, result) = joined.map_err(|err| io::Error::other(err.to_string()))?;
                    completed += 1;
                    let running = total.saturating_sub(completed);
                    results[idx] = Some((task.clone(), result));
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!(
                            "Subagent progress: {completed}/{total} completed, {running} running."
                        ))],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    self.render_all().ok();
                }

                let sections = results
                    .into_iter()
                    .flatten()
                    .map(|(task, result)| match result {
                        Ok(output) => format!("Task:\n{task}\n\nResult:\n{output}"),
                        Err(err) => format!("Task:\n{task}\n\nError:\n{}", err),
                    })
                    .collect::<Vec<_>>();
                format!("Subagent (parallel)\n\n{}", sections.join("\n\n---\n\n"))
            }
            SubagentMode::Chain => {
                let mut previous = String::new();
                let mut sections = Vec::new();
                for (idx, task) in tasks.iter().enumerate() {
                    let rendered_task = task.replace("{previous}", previous.trim());
                    let output = run_ephemeral_agent_task(
                        self.model.clone(),
                        self.api_key.clone(),
                        self.thinking_level.clone(),
                        build_subagent_prompt(&rendered_task),
                        ToolPreset::Full,
                        self.tool_selection.clone(),
                    )
                    .await
                    .map_err(|err| io::Error::other(format!("Chain step {} failed: {}", idx + 1, err)))?;
                    previous = output.clone();
                    sections.push(format!("Step {} task:\n{}\n\nResult:\n{}", idx + 1, rendered_task, output));
                    let msg = AssistantMessage::new(
                        vec![AssistantContentBlock::Text(format!(
                            "Subagent chain progress: {}/{} completed.",
                            idx + 1,
                            tasks.len()
                        ))],
                        false,
                        "Thinking...".into(),
                        Some("info".into()),
                        None,
                        &self.theme,
                    );
                    self.messages.add(msg);
                    self.render_all().ok();
                }
                format!("Subagent (chain)\n\n{}", sections.join("\n\n---\n\n"))
            }
        };

        let message = Message::assistant(vec![ContentBlock::Text(TextContent { text: summary })]);
        self.append_session_message(&message).await;
        self.add_message_component(&message);
        Ok("Subagent workflow completed.".into())
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
        if self.tree_selector.is_some() || self.model_selector.is_some() || self.theme_selector.is_some() {
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
            let _ = select_tx
                .send(ModelSelectorAction::Selected { provider: provider.to_string(), model_id: id.to_string() });
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

            let _ = self.persist_entry(&entry_id).await;

            // Update InteractiveMode state.
            self.model = new_model;
            self.model_id = model_id.to_string();
            self.model_name = model_id.to_string();

            let clamped = clamp_thinking_level(self.model, &self.thinking_level);
            if clamped != self.thinking_level {
                self.thinking_level = clamped.clone();
                let thinking_entry_id = self.session.append_thinking_level_change(clamped);
                let _ = self.persist_entry(&thinking_entry_id).await;
            }
        }
    }

    async fn apply_thinking_level(&mut self, level: &str) -> Result<String, String> {
        if !is_valid_thinking_level(level) {
            return Err(format!("Invalid thinking level '{}'. Valid values: {}", level, THINKING_LEVELS.join(", ")));
        }

        let effective = clamp_thinking_level(self.model, level);
        if effective != self.thinking_level {
            self.thinking_level = effective.clone();
            let entry_id = self.session.append_thinking_level_change(effective.clone());
            let _ = self.persist_entry(&entry_id).await;
        }
        Ok(effective)
    }

    // ------------------------------------------------------------------
    // Theme selector
    // ------------------------------------------------------------------

    /// Open the theme selector overlay.
    ///
    /// Shows available themes ("dark", "light") and pre-selects the current
    /// theme. The result is delivered via an mpsc channel.
    pub fn show_theme_selector(&mut self) {
        if self.tree_selector.is_some() || self.theme_selector.is_some() || self.model_selector.is_some() {
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
            || self.tree_selector.is_some()
            || self.model_selector.is_some()
            || self.theme_selector.is_some()
        {
            return; // Already showing a selector
        }

        let (tx, rx) = mpsc::unbounded_channel::<ExtensionSelectorAction>();

        let options: Vec<String> = self.extensions.iter().map(|e| format!("{} v{}", e.name, e.version)).collect();

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
            all_lines.push(self.theme.ansi(&self.theme.muted, "  Viewing loaded extensions (Esc to cancel)"));
            all_lines.push(String::new());
            let selector_lines = selector.render(width);
            all_lines.extend(selector_lines);
            selector_rendered = true;
        }

        if !selector_rendered {
            if let Some(ref selector) = self.session_selector {
                all_lines.push(self.theme.ansi(&self.theme.primary, "  Session Selector"));
                all_lines.push(self.theme.ansi(&self.theme.muted, "  Resume a previous session (Esc to cancel)"));
                all_lines.push(String::new());
                let selector_lines = selector.render(width);
                all_lines.extend(selector_lines);
            } else if let Some(ref selector) = self.tree_selector {
                all_lines.push(self.theme.ansi(&self.theme.primary, "  Tree Navigation"));
                all_lines.push(self.theme.ansi(
                    &self.theme.muted,
                    "  Select a session point to continue from there (Enter to jump, Esc to cancel)",
                ));
                all_lines.push(String::new());
                let selector_lines = selector.render(width);
                all_lines.extend(selector_lines);
            } else if let Some(ref selector) = self.model_selector {
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

fn supported_thinking_levels_text(model: &Model) -> String {
    supported_thinking_levels(model).join(", ")
}

fn extract_text_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| if let ContentBlock::Text(text) = block { Some(text.text.clone()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_tool_message(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolResult(result) = block {
                let body = result
                    .content
                    .as_deref()
                    .map(extract_text_from_blocks)
                    .filter(|text| !text.trim().is_empty())
                    .or_else(|| result.error.clone())
                    .unwrap_or_else(|| "(no output)".to_string());
                Some(format!("[{}]\n{}", result.name, body))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn extract_message_text(message: &serde_json::Value) -> String {
    message
        .get("content")
        .and_then(|content| {
            if let Some(text) = content.as_str() {
                return Some(text.to_string());
            }
            content.as_array().map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| {
                        if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                            block.get("text").and_then(|value| value.as_str()).map(str::to_string)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        })
        .unwrap_or_default()
}

fn build_tree_select_items(roots: &[SessionTreeNode]) -> Vec<SelectItem> {
    let mut items = Vec::new();
    flatten_tree_select_items(roots, 0, &mut items);
    items
}

fn flatten_tree_select_items(nodes: &[SessionTreeNode], depth: usize, items: &mut Vec<SelectItem>) {
    for node in nodes {
        items.push(SelectItem {
            value: node.entry.id().to_string(),
            label: format!("{}{}", "  ".repeat(depth), tree_entry_label(node),),
            description: Some(tree_entry_description(node)),
        });
        flatten_tree_select_items(&node.children, depth + 1, items);
    }
}

fn tree_entry_label(node: &SessionTreeNode) -> String {
    let label = node
        .label
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("@{} ", value.trim()))
        .unwrap_or_default();
    match &node.entry {
        SessionEntry::Message(message) => {
            let role = message.message.get("role").and_then(|value| value.as_str()).unwrap_or("msg");
            let preview = extract_message_text(&message.message);
            if preview.is_empty() {
                format!("{}[{}] {}", label, role, node.entry.id())
            } else {
                format!("{}[{}] {}", label, role, preview)
            }
        }
        SessionEntry::Compaction(compaction) => format!("{}[compaction] {}", label, compaction.summary),
        SessionEntry::BranchSummary(summary) => format!("{}[branch] {}", label, summary.summary),
        SessionEntry::ModelChange(change) => format!("{}[model] {}", label, change.model_id),
        SessionEntry::ThinkingLevelChange(change) => format!("{}[thinking] {}", label, change.thinking_level),
        SessionEntry::SessionInfo(info) => {
            format!("{}[session] {}", label, info.name.as_deref().unwrap_or("(unnamed)"))
        }
        SessionEntry::Label(label_entry) => {
            format!("{}[label] {}", label, label_entry.label.as_deref().unwrap_or("(cleared)"))
        }
        SessionEntry::Custom(custom) => format!("{}[custom:{}] {}", label, custom.custom_type, node.entry.id()),
        SessionEntry::CustomMessage(custom) => format!("{}[custom:{}] {}", label, custom.custom_type, node.entry.id()),
    }
}

fn tree_entry_description(node: &SessionTreeNode) -> String {
    match &node.entry {
        SessionEntry::Message(message) => format!(
            "{} {}",
            message.message.get("role").and_then(|value| value.as_str()).unwrap_or("msg"),
            node.entry.id()
        ),
        SessionEntry::Compaction(compaction) => {
            format!("{} | {} tokens", node.entry.id(), compaction.tokens_before)
        }
        _ => node.entry.id().to_string(),
    }
}

fn compaction_entry_text(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Message(message) => {
            let role = message.message.get("role").and_then(|value| value.as_str()).unwrap_or("unknown");
            let text = extract_message_text(&message.message);
            if text.is_empty() { None } else { Some(format!("{role}: {text}")) }
        }
        SessionEntry::BranchSummary(summary) => Some(format!("[branch summary] {}", summary.summary)),
        SessionEntry::Compaction(compaction) => Some(format!("[compaction] {}", compaction.summary)),
        _ => None,
    }
}

fn compaction_entry_tokens(entry: &SessionEntry) -> u64 {
    match entry {
        SessionEntry::Message(message) => serde_json::from_value::<Message>(message.message.clone())
            .map(|value| estimate_message_tokens(&value))
            .unwrap_or(0),
        _ => 0,
    }
}

fn build_compaction_prompt(entries_text: &[String], custom_instructions: Option<&str>) -> String {
    let serialized = entries_text
        .iter()
        .enumerate()
        .map(|(index, text)| format!("--- Entry {} ---\n{}", index + 1, text))
        .collect::<Vec<_>>()
        .join("\n");
    match custom_instructions {
        Some(instructions) if !instructions.trim().is_empty() => format!(
            "Summarize the following conversation context.\n\nCustom instructions:\n{}\n\nConversation to summarize:\n\n{}",
            instructions.trim(),
            serialized
        ),
        _ => format!("Summarize the following conversation context.\n\nConversation to summarize:\n\n{}", serialized),
    }
}

fn parse_plan_steps(text: &str) -> Vec<String> {
    let mut in_plan = false;
    let mut steps = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("plan:") {
            in_plan = true;
            continue;
        }
        if !in_plan {
            continue;
        }
        if let Some(step) = strip_numbered_step(trimmed) {
            steps.push(step.to_string());
            continue;
        }
        if trimmed.is_empty() && !steps.is_empty() {
            break;
        }
        if let Some(last) = steps.last_mut() {
            last.push(' ');
            last.push_str(trimmed);
        }
    }
    steps
}

fn strip_numbered_step(line: &str) -> Option<&str> {
    let digits_len = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits_len == 0 || digits_len >= line.len() {
        return None;
    }
    let suffix = &line[digits_len..];
    if let Some(rest) = suffix.strip_prefix(". ") {
        return Some(rest.trim());
    }
    if let Some(rest) = suffix.strip_prefix(") ") {
        return Some(rest.trim());
    }
    None
}

fn parse_done_markers(text: &str) -> Vec<usize> {
    let mut markers = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find("[DONE:") {
        let after = &rest[idx + 6..];
        let Some(end_idx) = after.find(']') else {
            break;
        };
        if let Ok(value) = after[..end_idx].trim().parse::<usize>() {
            markers.push(value);
        }
        rest = &after[end_idx + 1..];
    }
    markers
}

fn build_plan_execution_prompt(plan: &PlanState) -> String {
    let steps = plan.steps.iter().map(|step| format!("{}. {}", step.index, step.text)).collect::<Vec<_>>().join("\n");
    format!("{PLAN_EXECUTION_PROMPT_PREFIX}\n\nPlan:\n{steps}")
}

fn build_plan_refine_prompt(plan: &PlanState) -> String {
    let steps = plan.steps.iter().map(|step| format!("{}. {}", step.index, step.text)).collect::<Vec<_>>().join("\n");
    format!("Please refine the following plan and return an updated Plan:\n\n{steps}")
}

fn parse_subagent_spec(spec: Option<&str>) -> io::Result<(SubagentMode, Vec<String>)> {
    let raw = spec
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other("Usage: /subagent <single|parallel|chain> <task>"))?;
    let mut parts = raw.splitn(2, ' ');
    let mode = match parts.next().unwrap_or_default() {
        "single" => SubagentMode::Single,
        "parallel" => SubagentMode::Parallel,
        "chain" => SubagentMode::Chain,
        other => {
            return Err(io::Error::other(format!(
                "Unknown subagent mode '{}'. Use single, parallel, or chain.",
                other
            )));
        }
    };
    let task_spec = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other("Subagent task list must not be empty"))?;
    let tasks = match mode {
        SubagentMode::Single => vec![task_spec.to_string()],
        SubagentMode::Parallel | SubagentMode::Chain => {
            task_spec.split("||").map(str::trim).filter(|task| !task.is_empty()).map(str::to_string).collect::<Vec<_>>()
        }
    };
    if mode != SubagentMode::Single && tasks.len() < 2 {
        return Err(io::Error::other("Parallel and chain subagents require at least two tasks separated by '||'."));
    }
    Ok((mode, tasks))
}

fn build_subagent_prompt(task: &str) -> String {
    format!(
        "You are a focused subagent. Complete only the requested task, use tools when needed, and reply concisely.\n\nTask:\n{}",
        task.trim()
    )
}

async fn run_ephemeral_agent_task(
    model: Model,
    api_key: Option<String>,
    thinking_level: String,
    prompt: String,
    tool_preset: ToolPreset,
    tool_selection: ToolSelection,
) -> io::Result<String> {
    let tools = tool_definitions_for_selection(tool_preset, &tool_selection);
    let mut state = AgentState {
        messages: vec![Message::user_text(&prompt)],
        context: AgentContext {
            messages: vec![],
            system_prompt: None,
            tools,
            model: Some(model.id.clone()),
            max_turns: 100,
            current_turn: 0,
        },
        pending_tool_calls: vec![],
    };
    let options = StreamOptions { api_key, thinking: Some(thinking_enabled(&thinking_level)), ..Default::default() };
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_for_tools = cancel.clone();
    let stream_model = model.clone();
    let tool_executor = move |name: &str, _id: &str, args: &serde_json::Value| {
        let cancel = cancel_for_tools.clone();
        let name = name.to_string();
        let args = args.clone();
        let rt_handle = tokio::runtime::Handle::current();
        let tool_selection = tool_selection.clone();
        tokio::task::block_in_place(move || {
            rt_handle.block_on(async move {
                execute_tool_for_selection(&name, args, cancel, tool_preset, &tool_selection)
                    .await
                    .map_err(|err| err.to_string())
            })
        })
    };

    pi_agent_core::agent_loop(
        &mut state,
        |ctx: Context| stream::stream(&stream_model, ctx, options.clone()),
        tool_executor,
        |_| {},
        cancel,
    )
    .await
    .map_err(|err| io::Error::other(err.to_string()))?;

    state
        .messages
        .into_iter()
        .rev()
        .find_map(|message| {
            if message.role == pi_ai_core::types::MessageRole::Assistant {
                let text = extract_text_from_blocks(&message.content).trim().to_string();
                if text.is_empty() { None } else { Some(text) }
            } else {
                None
            }
        })
        .ok_or_else(|| io::Error::other("Subagent returned no assistant text"))
}

fn load_settings_for_reload() -> Result<Settings, pi_core::settings::SettingsError> {
    if let Some(path) = std::env::var_os("PI_SETTINGS_FILE") {
        Settings::load_from(PathBuf::from(path))
    } else {
        Settings::load()
    }
}

fn copy_text_to_clipboard(text: &str) -> io::Result<()> {
    if let Some(test_path) = std::env::var_os("PI_CLIPBOARD_TEST_FILE") {
        std::fs::write(test_path, text)?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        write_clipboard_via_command("pbcopy", &[], text)
    }
    #[cfg(target_os = "windows")]
    {
        write_clipboard_via_command("clip", &[], text)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for (program, args) in [
            ("wl-copy", Vec::<&str>::new()),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ] {
            if write_clipboard_via_command(program, &args, text).is_ok() {
                return Ok(());
            }
        }
        Err(io::Error::other("No clipboard command found (tried wl-copy, xclip, xsel)."))
    }
}

fn write_clipboard_via_command(program: &str, args: &[&str], text: &str) -> io::Result<()> {
    let mut child =
        Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("{} exited with {}", program, status))) }
}

#[cfg(feature = "feat-extensions")]
fn reload_extension_manifests() -> Vec<ExtensionManifest> {
    let paths: Vec<PathBuf> =
        vec![dirs::home_dir().map(|home| home.join(".pi").join("extensions")), Some(PathBuf::from(".pi/extensions"))]
            .into_iter()
            .flatten()
            .collect();
    pi_extension_system::loader::discover_extensions(&paths)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::session::types::MessageEntryData;
    use pi_ai_core::api_registry::{ApiProvider, clear_api_providers, register_api_provider};
    use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStream};
    use pi_ai_core::types::{KnownProvider, StreamEvent};

    struct DelayedEchoProvider {
        api_id: &'static str,
        delay_ms: u64,
    }

    impl ApiProvider for DelayedEchoProvider {
        fn api_id(&self) -> &str {
            self.api_id
        }

        fn stream(&self, _model: &Model, context: Context, _options: StreamOptions) -> AssistantMessageEventStream {
            let text = context
                .messages
                .iter()
                .rev()
                .find_map(|message| {
                    if message.role == pi_ai_core::types::MessageRole::User {
                        Some(extract_text_from_blocks(&message.content))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let delay_ms = self.delay_ms;
            let (tx, rx) = EventStream::new();
            tokio::spawn(async move {
                let _ = tx.send(StreamEvent::Start);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                let _ = tx.send(StreamEvent::TextDelta { delta: format!("echo:{text}") });
                let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some("end_turn".to_string()) });
            });
            rx
        }
    }

    fn make_message_entry(id: &str, parent_id: Option<&str>, role: &str, text: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntryData {
            id: id.to_string(),
            parent_id: parent_id.map(str::to_string),
            timestamp: pi_agent_core::session::types::now_timestamp(),
            message: serde_json::json!({
                "role": role,
                "content": [{ "type": "text", "text": text }]
            }),
        })
    }

    fn unique_test_dir(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", create_session_id()))
    }

    /// Helper to create an InteractiveMode for testing (blocks on the async
    /// constructor). Uses the default gpt-4o model from the catalog.
    fn create_im() -> InteractiveMode {
        create_im_for_model("gpt-4o")
    }

    fn create_im_for_model(model_id: &str) -> InteractiveMode {
        let model = pi_model_catalog::models::find_model(model_id).expect("test model should exist in catalog");
        tokio::runtime::Runtime::new()
            .expect("failed to create tokio runtime for test")
            .block_on(InteractiveMode::new(model_id, model, None, None, None, std::env::temp_dir()))
            .expect("InteractiveMode::new() should succeed")
    }

    fn create_im_for_runtime_model(model: &'static Model) -> InteractiveMode {
        tokio::runtime::Runtime::new()
            .expect("failed to create tokio runtime for test")
            .block_on(InteractiveMode::new(&model.id, model, None, None, None, std::env::temp_dir()))
            .expect("InteractiveMode::new() should succeed")
    }

    fn delayed_echo_model() -> &'static Model {
        Box::leak(Box::new(Model {
            id: "tui-test-model".to_string(),
            provider: KnownProvider::Faux,
            api: "tui-test-stream".to_string(),
            name: Some("TUI Test".to_string()),
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
        }))
    }

    fn wait_for_background_run(rt: &tokio::runtime::Runtime, im: &mut InteractiveMode) {
        let finished = rt.block_on(async {
            for _ in 0..100 {
                let _ = im.poll_background_run().await;
                if !im.is_streaming {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        });
        assert!(finished, "background run should finish");
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
        tokio::runtime::Runtime::new().expect("failed to create tokio runtime for test").block_on(im.send_message());

        // Editor should be cleared.
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
        // Messages should contain the user message (the LLM call may add an
        // error message on failure, so count >= 1).
        assert!(im.messages.child_count() >= 1, "at least one message should be added");
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
        assert!(im.messages.child_count() > count_before, "second send should add more messages");
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
        assert_eq!(im.messages.child_count(), 0, "whitespace-only should not add a message");
    }

    // ── Key dispatching ──────────────────────────────────────────────

    #[test]
    fn test_handle_escape_sets_running_false() {
        let mut im = create_im();
        im.running = true;
        // handle_input is async; Escape handler is synchronous (no await needed).
        tokio::runtime::Runtime::new().unwrap().block_on(im.handle_input("\x1b"));
        assert!(!im.running, "Escape should set running = false");
    }

    #[test]
    fn test_handle_enter_sends_message() {
        let mut im = create_im();
        im.editor.set_text("test message");
        tokio::runtime::Runtime::new().unwrap().block_on(im.handle_input("\r"));

        assert!(im.editor.get_text().is_empty(), "editor should be cleared after Enter");
        assert!(im.messages.child_count() >= 1, "Enter should add at least one message");
    }

    #[test]
    fn test_handle_enter_empty_editor_does_nothing() {
        let mut im = create_im();
        tokio::runtime::Runtime::new().unwrap().block_on(im.handle_input("\r"));
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

        let model = pi_model_catalog::models::find_model("gpt-4o").expect("gpt-4o should exist");
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        // Create InteractiveMode with session path (new session).
        let mut im = rt
            .block_on(InteractiveMode::new("gpt-4o", model, None, None, Some(path.clone()), dir.clone()))
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
        assert!(content.contains("hello world"), "session file should contain message text: {content}");
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

        let model = pi_model_catalog::models::find_model("gpt-4o").expect("gpt-4o should exist");
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        // Phase 1: Create a session and send a message.
        {
            let mut im = rt
                .block_on(InteractiveMode::new("gpt-4o", model, None, None, Some(path.clone()), dir.clone()))
                .expect("new() should succeed");
            im.editor.set_text("persist me");
            rt.block_on(im.send_message());
        }

        // Phase 2: Resume the session and verify the message is visible.
        {
            let im = rt
                .block_on(InteractiveMode::new("gpt-4o", model, None, None, Some(path.clone()), dir.clone()))
                .expect("resume should succeed");

            assert!(im.messages.child_count() >= 1, "resumed session should have at least one message");
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
        assert_eq!(im.model_name, "claude-sonnet-4-20250514", "model_name should update");
        assert_eq!(format!("{:?}", im.model.provider).to_lowercase(), "anthropic", "provider should be anthropic");

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
        assert_eq!(format!("{:?}", im.model.provider).to_lowercase(), original_provider, "provider should not change");
    }

    #[test]
    fn test_apply_thinking_level_records_session_change() {
        let mut im = create_im_for_model("o3-mini");
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        let level = rt.block_on(im.apply_thinking_level("high")).expect("o3-mini should accept high thinking");
        assert_eq!(level, "high");
        assert_eq!(im.thinking_level, "high");

        let has_thinking_change =
            im.session.entries().iter().any(|entry| matches!(entry, SessionEntry::ThinkingLevelChange(_)));
        assert!(has_thinking_change, "session should record a thinking_level_change entry");
    }

    #[test]
    fn test_apply_thinking_level_clamps_for_unsupported_model() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

        let level = rt.block_on(im.apply_thinking_level("high")).expect("unsupported models should clamp, not error");
        assert_eq!(level, "off");
        assert_eq!(im.thinking_level, "off");
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
        assert_ne!(im.theme.background, original_bg, "background should change");
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
        assert!(im.theme_selector.is_none(), "theme selector should not open when model selector is active");

        // Close model selector and open theme.
        im.model_selector = None;
        im.model_selector_rx = None;
        im.show_theme_selector();
        assert!(im.theme_selector.is_some(), "theme selector should now be active");

        // Trying to open model selector while theme is active should be a no-op.
        im.show_model_selector();
        assert!(im.model_selector.is_none(), "model selector should not open when theme selector is active");
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

        let im =
            rt.block_on(InteractiveMode::new("gpt-4o", model, None, None, Some(path.clone()), dir.clone())).unwrap();

        assert!(path.exists(), "session file should be created");
        assert!(im.session_path.is_some(), "session_path should be stored");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_new_without_session_path_uses_in_memory() {
        let im = create_im();
        assert!(im.session_path.is_none(), "no session_path when None is passed");
    }

    // ── Slash commands ───────────────────────────────────────────────

    #[test]
    fn test_slash_help_adds_message() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/help");
        rt.block_on(im.send_message());

        // Should add a help message (and no user message or LLM call)
        assert!(im.messages.child_count() >= 1, "/help should add at least one message");
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
    }

    #[test]
    fn test_slash_unknown_shows_error() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/nonexistent");
        rt.block_on(im.send_message());

        assert!(im.messages.child_count() >= 1, "unknown command should add an error message");
        assert!(im.editor.get_text().is_empty(), "editor should be cleared");
    }

    #[test]
    fn test_slash_clear_removes_messages() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        // First, send a real-ish message (it will fail on LLM but add a user msg)
        im.editor.set_text("hello");
        rt.block_on(im.send_message());
        assert!(im.messages.child_count() >= 1, "should have messages before clear");

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

        assert!(im.messages.child_count() >= 1, "/session should add an info message");
    }

    #[test]
    fn test_slash_copy_without_assistant_errors() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/copy");
        rt.block_on(im.send_message());

        assert!(im.messages.child_count() >= 1, "/copy should report when nothing is available");
    }

    #[test]
    fn test_slash_copy_writes_clipboard_file() {
        let dir = unique_test_dir("pi_test_slash_copy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clipboard_path = dir.join("clipboard.txt");
        unsafe {
            std::env::set_var("PI_CLIPBOARD_TEST_FILE", &clipboard_path);
        }

        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();
        im.session.append_entry(make_message_entry("a1", None, "assistant", "copy me"));

        im.editor.set_text("/copy");
        rt.block_on(im.send_message());

        let copied = std::fs::read_to_string(&clipboard_path).unwrap();
        assert_eq!(copied, "copy me");

        unsafe {
            std::env::remove_var("PI_CLIPBOARD_TEST_FILE");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_reload_applies_theme_from_settings_file() {
        let dir = unique_test_dir("pi_test_slash_reload");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings_path = dir.join("settings.json");
        std::fs::write(&settings_path, r#"{"theme":"light"}"#).unwrap();
        unsafe {
            std::env::set_var("PI_SETTINGS_FILE", &settings_path);
        }

        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(im.theme_name, "dark");

        im.editor.set_text("/reload");
        rt.block_on(im.send_message());

        assert_eq!(im.theme_name, "light", "/reload should apply the reloaded theme");

        unsafe {
            std::env::remove_var("PI_SETTINGS_FILE");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_reload_is_rejected_while_streaming() {
        let dir = unique_test_dir("pi_test_slash_reload_busy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings_path = dir.join("settings.json");
        std::fs::write(&settings_path, r#"{"theme":"light"}"#).unwrap();
        unsafe {
            std::env::set_var("PI_SETTINGS_FILE", &settings_path);
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 50 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        assert_eq!(im.theme_name, "dark");
        im.editor.set_text("first");
        rt.block_on(im.send_message());
        assert!(im.is_streaming);

        im.editor.set_text("/reload");
        rt.block_on(im.send_message());
        assert_eq!(im.theme_name, "dark", "/reload should not apply changes while streaming");

        wait_for_background_run(&rt, &mut im);
        unsafe {
            std::env::remove_var("PI_SETTINGS_FILE");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_plan_enables_mode() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/plan");
        rt.block_on(im.send_message());
        assert!(im.plan_mode, "/plan should enable plan mode");
    }

    #[test]
    fn test_slash_plan_off_disables_mode() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/plan");
        rt.block_on(im.send_message());
        assert!(im.plan_mode);

        im.editor.set_text("/plan off");
        rt.block_on(im.send_message());
        assert!(!im.plan_mode);
    }

    #[test]
    fn test_plan_capture_and_progress_helpers() {
        let mut im = create_im();
        im.plan_mode = true;
        im.session.append_entry(make_message_entry("u1", None, "user", "plan this"));
        im.session.append_entry(make_message_entry(
            "a1",
            Some("u1"),
            "assistant",
            "Plan:\n1. Inspect the repository layout.\n2. Summarize the implementation tasks.",
        ));
        im.capture_pending_plan();

        let pending = im.pending_plan.clone().expect("plan mode should capture a numbered plan");
        assert_eq!(pending.steps.len(), 2);

        im.plan_progress = Some(pending);
        im.session.append_entry(make_message_entry("a2", Some("a1"), "assistant", "[DONE:1]\n[DONE:2]\nAll done."));
        im.refresh_plan_progress_from_session();

        let progress = im.plan_progress.as_ref().expect("plan progress should exist");
        assert!(progress.steps.iter().all(|step| step.done), "all plan steps should be marked done");
    }

    #[test]
    fn test_plan_execute_starts_background_run() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 10 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        im.pending_plan = Some(PlanState {
            steps: vec![
                PlanStep { index: 1, text: "inspect".into(), done: false },
                PlanStep { index: 2, text: "summarize".into(), done: false },
            ],
        });

        im.editor.set_text("/plan execute");
        rt.block_on(im.send_message());

        assert!(im.is_streaming, "/plan execute should start a run");
        assert!(!im.plan_mode, "plan mode should switch off during execution");
        assert!(im.plan_progress.is_some(), "plan progress should be initialized");
        wait_for_background_run(&rt, &mut im);
    }

    #[test]
    fn test_plan_refine_requires_pending_plan() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let err = rt
            .block_on(im.handle_plan_command(Some("refine")))
            .expect_err("refine should fail without a captured plan");
        assert!(err.to_string().contains("No captured plan"));
    }

    #[test]
    fn test_slash_subagent_single_persists_summary() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 0 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        im.editor.set_text("/subagent single inspect repo layout");
        rt.block_on(im.send_message());

        let text = im.latest_assistant_text().unwrap_or_default();
        assert!(text.contains("Subagent (single)"), "{text:?}");
        assert!(text.contains("inspect repo layout"), "{text:?}");
    }

    #[test]
    fn test_subagent_rejects_malformed_parallel_spec() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut im = create_im();

        let err = rt
            .block_on(im.run_subagent_command(Some("parallel only-one-task")))
            .expect_err("parallel subagent should require at least two tasks");
        assert!(err.to_string().contains("at least two tasks"));
    }

    #[test]
    fn test_slash_subagent_parallel_summarizes_all_tasks() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 0 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        im.editor.set_text("/subagent parallel inspect repo || inspect tests");
        rt.block_on(im.send_message());

        let text = im.latest_assistant_text().unwrap_or_default();
        assert!(text.contains("Subagent (parallel)"), "{text:?}");
        assert!(text.contains("inspect repo"), "{text:?}");
        assert!(text.contains("inspect tests"), "{text:?}");
    }

    #[test]
    fn test_subagent_is_rejected_while_streaming() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 50 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        im.editor.set_text("first");
        rt.block_on(im.send_message());
        assert!(im.is_streaming);

        let err = rt
            .block_on(im.run_subagent_command(Some("single inspect repo")))
            .expect_err("subagent should reject while streaming");
        assert!(err.to_string().contains("while a run is active"));
        wait_for_background_run(&rt, &mut im);
    }

    #[test]
    fn test_slash_subagent_chain_replaces_previous_output() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 0 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        im.editor.set_text("/subagent chain inspect repo || summarize {previous}");
        rt.block_on(im.send_message());

        let text = im.latest_assistant_text().unwrap_or_default();
        assert!(text.contains("Subagent (chain)"), "{text:?}");
        assert!(text.contains("Step 2 task:"), "{text:?}");
        assert!(!text.contains("{previous}"), "{text:?}");
    }

    #[test]
    fn test_slash_thinking_updates_state() {
        let mut im = create_im_for_model("o3-mini");
        let rt = tokio::runtime::Runtime::new().unwrap();

        im.editor.set_text("/thinking high");
        rt.block_on(im.send_message());

        assert_eq!(im.thinking_level, "high");
        assert!(im.messages.child_count() >= 1, "/thinking should add a confirmation message");
    }

    #[test]
    fn test_normal_message_not_affected_by_slash() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();

        // A message starting with a word, not a slash
        im.editor.set_text("hello world");
        rt.block_on(im.send_message());

        // The user message should be added (LLM call will fail but user msg is there)
        assert!(im.messages.child_count() >= 1, "normal message should add messages");
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

    #[test]
    fn test_slash_resume_opens_session_selector() {
        let dir = unique_test_dir("pi_test_slash_resume");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session_path = dir.join("saved.jsonl");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let header = pi_agent_core::session::types::SessionHeader::new("/tmp", "resume1234".to_string());
        rt.block_on(storage::create(&session_path, &header)).unwrap();

        let model = pi_model_catalog::models::find_model("gpt-4o").unwrap();
        let mut im = rt.block_on(InteractiveMode::new("gpt-4o", model, None, None, None, dir.clone())).unwrap();

        im.editor.set_text("/resume");
        rt.block_on(im.send_message());

        assert!(im.session_selector.is_some(), "/resume should open the selector");
        assert!(im.session_selector_rx.is_some(), "selector channel should be active");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_tree_opens_tree_selector() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();
        im.session.append_entry(make_message_entry("u1", None, "user", "first prompt"));
        im.session.append_entry(make_message_entry("a1", Some("u1"), "assistant", "answer"));
        im.load_entries_into_container();

        im.editor.set_text("/tree");
        rt.block_on(im.send_message());

        assert!(im.tree_selector.is_some(), "/tree should open the tree selector");
        assert!(im.tree_selector_rx.is_some(), "tree selector channel should be active");
    }

    #[test]
    fn test_navigate_tree_to_user_message_prefills_editor() {
        let mut im = create_im();
        let rt = tokio::runtime::Runtime::new().unwrap();
        im.session.append_entry(make_message_entry("u1", None, "user", "first prompt"));
        im.session.append_entry(make_message_entry("a1", Some("u1"), "assistant", "answer"));
        im.session.append_entry(make_message_entry("u2", Some("a1"), "user", "second prompt"));

        rt.block_on(im.navigate_tree_to_entry("u1")).unwrap();

        assert_eq!(im.editor.get_text(), "first prompt");
        assert!(im.session.leaf_id().is_none(), "navigating to the first user message should reset the leaf");
        assert_eq!(
            im.messages.child_count(),
            0,
            "navigating to the first prompt should rebuild to an empty active context"
        );
    }

    #[test]
    fn test_slash_clone_switches_to_new_session_file() {
        let dir = unique_test_dir("pi_test_slash_clone");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("clone-source.jsonl");
        let model = pi_model_catalog::models::find_model("gpt-4o").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut im =
            rt.block_on(InteractiveMode::new("gpt-4o", model, None, None, Some(path.clone()), dir.clone())).unwrap();
        im.session.append_entry(make_message_entry("u1", None, "user", "hello"));

        im.editor.set_text("/clone");
        rt.block_on(im.send_message());

        let cloned_path = im.session_path.clone().expect("cloned session should persist");
        assert_ne!(cloned_path, path, "clone should switch to a new session file");
        assert!(cloned_path.exists(), "cloned session file should exist");
        let (_, entries, _) = rt.block_on(storage::read_all(&cloned_path)).unwrap();
        assert_eq!(entries.len(), 1, "clone should keep the active path history");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_fork_prefills_editor_and_truncates_history() {
        let dir = unique_test_dir("pi_test_slash_fork");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("fork-source.jsonl");
        let model = pi_model_catalog::models::find_model("gpt-4o").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut im =
            rt.block_on(InteractiveMode::new("gpt-4o", model, None, None, Some(path.clone()), dir.clone())).unwrap();
        im.session.append_entry(make_message_entry("u1", None, "user", "first prompt"));
        im.session.append_entry(make_message_entry("a1", Some("u1"), "assistant", "answer"));
        im.session.append_entry(make_message_entry("u2", Some("a1"), "user", "second prompt"));

        im.editor.set_text("/fork u1");
        rt.block_on(im.send_message());

        assert_eq!(im.editor.get_text(), "first prompt");
        let forked_path = im.session_path.clone().expect("forked session should persist");
        assert_ne!(forked_path, path, "fork should switch to a new session file");
        let (_, entries, _) = rt.block_on(storage::read_all(&forked_path)).unwrap();
        assert_eq!(entries.len(), 1, "fork should keep entries up to the requested user message");
        assert_eq!(entries[0].id(), "u1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_export_writes_html_file() {
        let dir = unique_test_dir("pi_test_slash_export");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model = pi_model_catalog::models::find_model("gpt-4o").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut im = rt.block_on(InteractiveMode::new("gpt-4o", model, None, None, None, dir.clone())).unwrap();
        im.session.append_entry(make_message_entry("u1", None, "user", "hello export"));
        let export_path = im.default_export_path();

        im.editor.set_text("/export");
        rt.block_on(im.send_message());

        assert!(export_path.exists(), "export should write the default HTML file");
        let html = std::fs::read_to_string(&export_path).unwrap();
        assert!(html.contains("hello export"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_compact_writes_entry_and_rebuilds_context() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 10 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());
        for i in 0..6 {
            im.editor.set_text(&format!("message {i} {}", "x".repeat(400)));
            rt.block_on(im.send_message());
            wait_for_background_run(&rt, &mut im);
        }

        im.editor.set_text("/compact Keep the summary terse");
        rt.block_on(im.send_message());

        let has_compaction_entry =
            im.session.entries().iter().any(|entry| matches!(entry, SessionEntry::Compaction(_)));
        assert!(has_compaction_entry, "compact should append a compaction entry");

        let context_messages = im.session.build_context().messages;
        let first_text =
            context_messages.first().map(|message| extract_text_from_blocks(&message.content)).unwrap_or_default();
        assert!(first_text.contains("[Compaction:"), "{first_text:?}");
        assert!(im.messages.child_count() > 0, "reloaded chat should contain rebuilt context");
    }

    #[test]
    fn test_slash_compact_persists_and_survives_reload() {
        let dir = unique_test_dir("pi_test_slash_compact_persist");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("compact-session.jsonl");
        let model = delayed_echo_model();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 10 })).await;
        });

        let mut im =
            rt.block_on(InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone())).unwrap();
        for i in 0..6 {
            im.editor.set_text(&format!("message {i} {}", "x".repeat(400)));
            rt.block_on(im.send_message());
            wait_for_background_run(&rt, &mut im);
        }

        im.editor.set_text("/compact Keep the summary terse");
        rt.block_on(im.send_message());

        let (_, entries, _) = rt.block_on(storage::read_all(&path)).unwrap();
        assert!(
            entries.iter().any(|entry| matches!(entry, SessionEntry::Compaction(_))),
            "compaction entry should be persisted to disk"
        );

        let resumed =
            rt.block_on(InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone())).unwrap();
        let first_text = resumed
            .session
            .build_context()
            .messages
            .first()
            .map(|message| extract_text_from_blocks(&message.content))
            .unwrap_or_default();
        assert!(first_text.contains("[Compaction:"), "{first_text:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_streaming_queue_commands_complete_in_order() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            clear_api_providers().await;
            register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms: 50 })).await;
        });

        let mut im = create_im_for_runtime_model(delayed_echo_model());

        im.editor.set_text("first");
        rt.block_on(im.send_message());
        assert!(im.is_streaming, "background run should stay active after send_message returns");

        im.editor.set_text("/steer steer next");
        rt.block_on(im.send_message());
        im.editor.set_text("/follow-up follow later");
        rt.block_on(im.send_message());

        let finished = rt.block_on(async {
            for _ in 0..100 {
                if im.poll_background_run().await && !im.is_streaming {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        });
        assert!(finished, "background run should finish");

        let texts = im
            .session
            .build_context()
            .messages
            .into_iter()
            .map(|message| extract_text_from_blocks(&message.content))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        assert!(texts.iter().any(|text| text == "first"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "steer next"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "follow later"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "echo:first"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "echo:steer next"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "echo:follow later"), "{texts:?}");
    }
}
