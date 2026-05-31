use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use pi_agent_core::agent_loop::agent_loop_with_queues;
use pi_agent_core::queue::{MessageQueue, QueueMode};
use pi_agent_core::session::{
    SessionManager, append, build_session_file_path, clone_active_path_to_file, export_session_as_html,
    fork_path_to_file, read_all, resolve_session_id_prefix,
};
use pi_agent_core::{AgentContext, AgentState, call_llm_for_text, estimate_message_tokens, prepare_compaction};
use pi_ai_core::stream;
use pi_ai_core::thinking::{
    THINKING_LEVELS, clamp_thinking_level, default_thinking_level_for_model, is_valid_thinking_level,
    supported_thinking_levels, thinking_enabled,
};
use pi_ai_core::types::{
    ContentBlock, Context, KnownProvider, Message, MessageRole, Model, StreamOptions, TextContent,
};
use pi_cli::register_builtin_providers;
use pi_core::auth::AuthStorage;
use pi_core::settings::Settings;
use pi_core::tool_registry;
use pi_model_catalog::models;
use tokio_util::sync::CancellationToken;

use super::types::{
    CommandSource, RpcCommand, RpcResponse, RpcSessionState, RpcSlashCommand, RpcSourceInfo, SteeringMode,
};

const COMPACTION_KEEP_RECENT_TOKENS: u64 = 512;

#[derive(Clone)]
pub struct RpcRuntime {
    shared: Arc<RpcRuntimeShared>,
}

pub(crate) struct RpcRuntimeConfig {
    pub model: Model,
    pub system_prompt: Option<String>,
    pub thinking_level: String,
    pub session_dir: PathBuf,
    pub session_path: Option<PathBuf>,
}

struct RpcRuntimeShared {
    session: Mutex<SessionManager>,
    session_path: Mutex<Option<PathBuf>>,
    session_dir: PathBuf,
    model: Mutex<Model>,
    thinking_level: Mutex<String>,
    steering_mode: Mutex<SteeringMode>,
    follow_up_mode: Mutex<SteeringMode>,
    steering_queue: Mutex<MessageQueue>,
    follow_up_queue: Mutex<MessageQueue>,
    system_prompt: Option<String>,
    auto_compaction_enabled: AtomicBool,
    is_streaming: AtomicBool,
    is_compacting: AtomicBool,
    cancel: Mutex<Option<CancellationToken>>,
}

impl RpcRuntime {
    pub async fn from_config_for_test(model: Model, session_dir: PathBuf, session_path: Option<PathBuf>) -> Self {
        Self::from_config(RpcRuntimeConfig {
            model,
            system_prompt: None,
            thinking_level: "off".to_string(),
            session_dir,
            session_path,
        })
        .await
        .expect("test runtime should initialize")
    }

    pub(crate) async fn from_environment() -> anyhow::Result<Self> {
        let settings = Settings::load().unwrap_or_else(|_| Settings {
            path: Settings::default_path(),
            default_model: None,
            default_provider: None,
            theme: None,
            base_url: None,
            api_key: None,
            extra: std::collections::HashMap::new(),
        });
        let model = default_model(&settings)?;
        let session_dir = default_session_dir();
        let session_path = Some(build_session_file_path(&session_dir, &model.id));
        Self::from_config(RpcRuntimeConfig {
            model: model.clone(),
            system_prompt: None,
            thinking_level: default_thinking_level_for_model(model).to_string(),
            session_dir,
            session_path,
        })
        .await
    }

    pub(crate) async fn from_config(config: RpcRuntimeConfig) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).to_string_lossy().to_string();

        let session = match &config.session_path {
            Some(path) if path.exists() => {
                let (header, entries, _) =
                    read_all(path).await.with_context(|| format!("failed to read session file {}", path.display()))?;
                SessionManager::from_entries(header, entries)
            }
            Some(path) => {
                let header = pi_agent_core::session::types::SessionHeader::new(
                    cwd,
                    pi_agent_core::session::types::create_session_id(),
                );
                pi_agent_core::session::create(path, &header)
                    .await
                    .with_context(|| format!("failed to create session file {}", path.display()))?;
                SessionManager::new(header)
            }
            None => SessionManager::in_memory(cwd),
        };

        let restored = restore_model_and_thinking(&session, &config.model, &config.thinking_level);
        Ok(Self {
            shared: Arc::new(RpcRuntimeShared {
                session: Mutex::new(session),
                session_path: Mutex::new(config.session_path),
                session_dir: config.session_dir,
                model: Mutex::new(restored.0),
                thinking_level: Mutex::new(restored.1),
                steering_mode: Mutex::new(SteeringMode::All),
                follow_up_mode: Mutex::new(SteeringMode::All),
                steering_queue: Mutex::new(MessageQueue::new(QueueMode::All)),
                follow_up_queue: Mutex::new(MessageQueue::new(QueueMode::All)),
                system_prompt: config.system_prompt,
                auto_compaction_enabled: AtomicBool::new(false),
                is_streaming: AtomicBool::new(false),
                is_compacting: AtomicBool::new(false),
                cancel: Mutex::new(None),
            }),
        })
    }

    pub(crate) async fn handle_command(&self, command: RpcCommand) -> RpcResponse {
        let id = command_id(&command);
        match command {
            RpcCommand::Prompt { message, images, streaming_behavior, .. } => {
                self.handle_prompt(id, message, images, streaming_behavior).await
            }
            RpcCommand::Steer { message, images, .. } => self.enqueue_message(id, "steer", message, images, true),
            RpcCommand::FollowUp { message, images, .. } => {
                self.enqueue_message(id, "follow_up", message, images, false)
            }
            RpcCommand::Abort { .. } => {
                if let Some(cancel) = self.shared.cancel.lock().expect("cancel poisoned").clone() {
                    cancel.cancel();
                }
                RpcResponse::success(id, "abort")
            }
            RpcCommand::NewSession { parent_session, .. } => self.new_session(id, parent_session).await,
            RpcCommand::GetState { .. } => self.get_state(id),
            RpcCommand::SetModel { provider, model_id, .. } => self.set_model(id, &provider, &model_id).await,
            RpcCommand::CycleModel { .. } => self.cycle_model(id).await,
            RpcCommand::GetAvailableModels { .. } => self.get_available_models(id),
            RpcCommand::SetThinkingLevel { level, .. } => self.set_thinking_level(id, &level).await,
            RpcCommand::CycleThinkingLevel { .. } => self.cycle_thinking_level(id).await,
            RpcCommand::SetSteeringMode { mode, .. } => self.set_queue_mode(id, mode, true),
            RpcCommand::SetFollowUpMode { mode, .. } => self.set_queue_mode(id, mode, false),
            RpcCommand::Compact { custom_instructions, .. } => self.compact(id, custom_instructions).await,
            RpcCommand::SetAutoCompaction { enabled, .. } => {
                self.shared.auto_compaction_enabled.store(enabled, Ordering::SeqCst);
                RpcResponse::success(id, "set_auto_compaction")
            }
            RpcCommand::SetAutoRetry { .. } => RpcResponse::success(id, "set_auto_retry"),
            RpcCommand::AbortRetry { .. } => RpcResponse::success(id, "abort_retry"),
            RpcCommand::Bash { command, .. } => super::server::handle_bash(id, &command).await,
            RpcCommand::AbortBash { .. } => RpcResponse::success(id, "abort_bash"),
            RpcCommand::GetSessionStats { .. } => self.get_session_stats(id),
            RpcCommand::ExportHtml { output_path, .. } => self.export_html(id, output_path).await,
            RpcCommand::SwitchSession { session_path, .. } => self.switch_session(id, &session_path).await,
            RpcCommand::Fork { entry_id, .. } => self.fork(id, &entry_id).await,
            RpcCommand::Clone { .. } => self.clone_session(id).await,
            RpcCommand::GetForkMessages { .. } => self.get_fork_messages(id),
            RpcCommand::GetLastAssistantText { .. } => self.get_last_assistant_text(id),
            RpcCommand::SetSessionName { name, .. } => self.set_session_name(id, &name).await,
            RpcCommand::GetMessages { .. } => self.get_messages(id),
            RpcCommand::GetCommands { .. } => self.get_commands(id),
        }
    }

    pub async fn handle_command_for_test(&self, command: RpcCommand) -> RpcResponse {
        self.handle_command(command).await
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if !self.shared.is_streaming.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        !self.shared.is_streaming.load(Ordering::SeqCst)
    }

    pub async fn wait_for_idle_for_test(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if !self.shared.is_streaming.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        !self.shared.is_streaming.load(Ordering::SeqCst)
    }

    fn get_state(&self, id: Option<String>) -> RpcResponse {
        let session = self.shared.session.lock().expect("session poisoned");
        let model = self.shared.model.lock().expect("model poisoned").clone();
        let thinking_level = self.shared.thinking_level.lock().expect("thinking level poisoned").clone();
        let steering_mode = self.shared.steering_mode.lock().expect("steering mode poisoned").clone();
        let follow_up_mode = self.shared.follow_up_mode.lock().expect("follow-up mode poisoned").clone();
        let state = RpcSessionState {
            model: Some(model),
            thinking_level,
            is_streaming: self.shared.is_streaming.load(Ordering::SeqCst),
            is_compacting: self.shared.is_compacting.load(Ordering::SeqCst),
            steering_mode,
            follow_up_mode,
            session_file: self.session_file_string(),
            session_id: session.session_id().to_string(),
            session_name: session.get_session_name().map(str::to_string),
            auto_compaction_enabled: self.shared.auto_compaction_enabled.load(Ordering::SeqCst),
            message_count: session.build_context().messages.len() as u64,
            pending_message_count: self.pending_message_count() as u64,
        };
        RpcResponse::success_with_data(id, "get_state", serde_json::to_value(state).unwrap_or_default())
    }

    async fn handle_prompt(
        &self,
        id: Option<String>,
        message: String,
        images: Option<Vec<pi_ai_core::types::ImageContent>>,
        streaming_behavior: Option<super::types::StreamingBehavior>,
    ) -> RpcResponse {
        if self.shared.is_streaming.load(Ordering::SeqCst) {
            let command = match streaming_behavior {
                Some(super::types::StreamingBehavior::Steer) => "steer",
                Some(super::types::StreamingBehavior::FollowUp) => "follow_up",
                None => {
                    return RpcResponse::error(
                        id,
                        "prompt",
                        "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
                    );
                }
            };
            return self.enqueue_message(id, command, message, images, command == "steer");
        }

        let model = self.current_model();
        let api_key = match self.preflight_model(&model) {
            Ok(value) => value,
            Err(err) => return RpcResponse::error(id, "prompt", err),
        };
        register_builtin_providers().await;

        let user_message = user_message(&message, images);
        let (session_messages, start_len) = match self.append_user_message(user_message).await {
            Ok(value) => value,
            Err(err) => return RpcResponse::error(id, "prompt", err.to_string()),
        };

        self.start_prompt_run(model, api_key, session_messages, start_len).await;
        RpcResponse::success(id, "prompt")
    }

    fn enqueue_message(
        &self,
        id: Option<String>,
        command: &str,
        message: String,
        images: Option<Vec<pi_ai_core::types::ImageContent>>,
        steering: bool,
    ) -> RpcResponse {
        let queued = user_message(&message, images);
        if steering {
            self.shared.steering_queue.lock().expect("steering queue poisoned").enqueue(queued);
        } else {
            self.shared.follow_up_queue.lock().expect("follow-up queue poisoned").enqueue(queued);
        }
        RpcResponse::success(id, command)
    }

    async fn new_session(&self, id: Option<String>, parent_session: Option<String>) -> RpcResponse {
        if let Err(err) = self.ensure_idle("new_session") {
            return RpcResponse::error(id, "new_session", err);
        }
        let model = self.current_model();
        let path = build_session_file_path(&self.shared.session_dir, &model.id);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).to_string_lossy().to_string();
        let header = match parent_session {
            Some(parent) => pi_agent_core::session::types::SessionHeader::with_parent(
                cwd,
                pi_agent_core::session::types::create_session_id(),
                parent,
            ),
            None => pi_agent_core::session::types::SessionHeader::new(
                cwd,
                pi_agent_core::session::types::create_session_id(),
            ),
        };
        if let Err(err) = pi_agent_core::session::create(&path, &header).await {
            return RpcResponse::error(id, "new_session", err.to_string());
        }
        self.load_session_state(SessionManager::new(header), Some(path));
        RpcResponse::success_with_data(id, "new_session", serde_json::json!({ "cancelled": false }))
    }

    async fn set_model(&self, id: Option<String>, provider: &str, model_id: &str) -> RpcResponse {
        if let Err(err) = self.ensure_idle("set_model") {
            return RpcResponse::error(id, "set_model", err);
        }

        let Some(model) = find_model(provider, model_id) else {
            return RpcResponse::error(id, "set_model", format!("Model not found: {provider}/{model_id}"));
        };

        let entry = {
            let mut session = self.shared.session.lock().expect("session poisoned");
            let entry_id = session.append_model_change(provider.to_string(), model_id.to_string());
            session.get_entry(&entry_id).cloned()
        };
        if let Err(err) = self.persist_entry(entry).await {
            return RpcResponse::error(id, "set_model", err.to_string());
        }
        *self.shared.model.lock().expect("model poisoned") = model.clone();
        let thinking_level = match self.sync_thinking_level_to_model(&model).await {
            Ok(level) => level,
            Err(err) => return RpcResponse::error(id, "set_model", err),
        };
        RpcResponse::success_with_data(
            id,
            "set_model",
            serde_json::json!({
                "model": model,
                "thinkingLevel": thinking_level,
                "isScoped": false,
            }),
        )
    }

    async fn cycle_model(&self, id: Option<String>) -> RpcResponse {
        if let Err(err) = self.ensure_idle("cycle_model") {
            return RpcResponse::error(id, "cycle_model", err);
        }
        let all = self.available_models();
        if all.len() <= 1 {
            return RpcResponse::success_with_data(id, "cycle_model", serde_json::json!(null));
        }

        let current = self.current_model();
        let current_index =
            all.iter().position(|model| model.id == current.id && model.provider == current.provider).unwrap_or(0);
        let next = all[(current_index + 1) % all.len()].clone();
        let entry = {
            let mut session = self.shared.session.lock().expect("session poisoned");
            let entry_id = session.append_model_change(provider_name(next.provider).to_string(), next.id.clone());
            session.get_entry(&entry_id).cloned()
        };
        if let Err(err) = self.persist_entry(entry).await {
            return RpcResponse::error(id, "cycle_model", err.to_string());
        }
        *self.shared.model.lock().expect("model poisoned") = next.clone();
        let thinking_level = match self.sync_thinking_level_to_model(&next).await {
            Ok(level) => level,
            Err(err) => return RpcResponse::error(id, "cycle_model", err),
        };

        RpcResponse::success_with_data(
            id,
            "cycle_model",
            serde_json::json!({
                "model": next,
                "thinkingLevel": thinking_level,
                "isScoped": false,
            }),
        )
    }

    fn get_available_models(&self, id: Option<String>) -> RpcResponse {
        RpcResponse::success_with_data(
            id,
            "get_available_models",
            serde_json::json!({ "models": self.available_models() }),
        )
    }

    async fn set_thinking_level(&self, id: Option<String>, level: &str) -> RpcResponse {
        if let Err(err) = self.ensure_idle("set_thinking_level") {
            return RpcResponse::error(id, "set_thinking_level", err);
        }
        match self.apply_thinking_level(level).await {
            Ok(_) => RpcResponse::success(id, "set_thinking_level"),
            Err(err) => RpcResponse::error(id, "set_thinking_level", err),
        }
    }

    async fn cycle_thinking_level(&self, id: Option<String>) -> RpcResponse {
        if let Err(err) = self.ensure_idle("cycle_thinking_level") {
            return RpcResponse::error(id, "cycle_thinking_level", err);
        }
        let model = self.current_model();
        let levels = supported_thinking_levels(&model);
        if levels.len() <= 1 {
            return RpcResponse::success_with_data(id, "cycle_thinking_level", serde_json::json!(null));
        }
        let current = self.shared.thinking_level.lock().expect("thinking level poisoned").clone();
        let current_index = levels.iter().position(|value| *value == current).unwrap_or(0);
        let next = levels[(current_index + 1) % levels.len()];
        match self.apply_thinking_level(next).await {
            Ok(level) => {
                RpcResponse::success_with_data(id, "cycle_thinking_level", serde_json::json!({ "level": level }))
            }
            Err(err) => RpcResponse::error(id, "cycle_thinking_level", err),
        }
    }

    fn set_queue_mode(&self, id: Option<String>, mode: SteeringMode, steering: bool) -> RpcResponse {
        let queue_mode = queue_mode(&mode);
        if steering {
            *self.shared.steering_mode.lock().expect("steering mode poisoned") = mode.clone();
            self.shared.steering_queue.lock().expect("steering queue poisoned").set_mode(queue_mode);
            RpcResponse::success(id, "set_steering_mode")
        } else {
            *self.shared.follow_up_mode.lock().expect("follow-up mode poisoned") = mode.clone();
            self.shared.follow_up_queue.lock().expect("follow-up queue poisoned").set_mode(queue_mode);
            RpcResponse::success(id, "set_follow_up_mode")
        }
    }

    async fn compact(&self, id: Option<String>, custom_instructions: Option<String>) -> RpcResponse {
        if let Err(err) = self.ensure_idle("compact") {
            return RpcResponse::error(id, "compact", err);
        }
        let model = self.current_model();
        let api_key = match self.preflight_model(&model) {
            Ok(value) => value,
            Err(err) => return RpcResponse::error(id, "compact", err),
        };

        let session = self.shared.session.lock().expect("session poisoned").clone();
        let active_entries: Vec<_> = session.path_to_root(None).into_iter().cloned().collect();
        let Some(prep) = prepare_compaction(&active_entries, COMPACTION_KEEP_RECENT_TOKENS) else {
            return RpcResponse::error(id, "compact", "Nothing to compact (session too small)");
        };
        let Some(first_kept_entry) = prep.entries_to_keep.first() else {
            return RpcResponse::error(id, "compact", "Nothing to compact (no kept entries)");
        };

        register_builtin_providers().await;
        self.shared.is_compacting.store(true, Ordering::SeqCst);
        let thinking = self.shared.thinking_level.lock().expect("thinking level poisoned").clone();
        let entries_text = prep.entries_to_summarize.iter().filter_map(entry_to_compaction_text).collect::<Vec<_>>();
        let prompt = build_compaction_prompt(&entries_text, custom_instructions.as_deref());
        let options = StreamOptions { thinking: Some(thinking_enabled(&thinking)), api_key, ..Default::default() };
        let summary_result =
            call_llm_for_text(&prompt, "You are a helpful assistant that summarizes conversations.", |ctx: Context| {
                stream::stream(&model, ctx, options.clone())
            })
            .await;
        self.shared.is_compacting.store(false, Ordering::SeqCst);

        let summary = match summary_result {
            Ok(summary) => summary,
            Err(err) => return RpcResponse::error(id, "compact", err.to_string()),
        };
        let tokens_before = prep.entries_to_summarize.iter().map(entry_tokens).sum();
        let first_kept_entry_id = first_kept_entry.id().to_string();

        let entry = {
            let mut session = self.shared.session.lock().expect("session poisoned");
            let entry_id = session.append_compaction(summary.clone(), first_kept_entry_id.clone(), tokens_before);
            session.get_entry(&entry_id).cloned()
        };
        if let Err(err) = self.persist_entry(entry).await {
            return RpcResponse::error(id, "compact", err.to_string());
        }

        RpcResponse::success_with_data(
            id,
            "compact",
            serde_json::json!({
                "summary": summary,
                "firstKeptEntryId": first_kept_entry_id,
                "tokensBefore": tokens_before,
            }),
        )
    }

    fn get_session_stats(&self, id: Option<String>) -> RpcResponse {
        let session = self.shared.session.lock().expect("session poisoned");
        let context = session.build_context();
        let model = self.current_model();
        let mut user_messages = 0u64;
        let mut assistant_messages = 0u64;
        let mut tool_messages = 0u64;
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        let mut cache_read = 0u64;
        let mut cache_write = 0u64;
        let mut cost = 0.0;

        for message in &context.messages {
            match message.role {
                MessageRole::User => user_messages += 1,
                MessageRole::Assistant => assistant_messages += 1,
                MessageRole::Tool => tool_messages += 1,
                MessageRole::System => {}
            }
            if let Some(usage) = &message.usage {
                input_tokens += usage.input;
                output_tokens += usage.output;
                cache_read += usage.cache_read.unwrap_or(0);
                cache_write += usage.cache_write.unwrap_or(0);
                cost += pi_ai_core::types::calculate_cost(&model, usage);
            }
        }

        RpcResponse::success_with_data(
            id,
            "get_session_stats",
            serde_json::json!({
                "sessionFile": self.session_file_string(),
                "sessionId": session.session_id(),
                "userMessages": user_messages,
                "assistantMessages": assistant_messages,
                "toolCalls": tool_messages,
                "toolResults": tool_messages,
                "totalMessages": context.messages.len() as u64,
                "tokens": {
                    "input": input_tokens,
                    "output": output_tokens,
                    "cacheRead": cache_read,
                    "cacheWrite": cache_write,
                    "total": input_tokens + output_tokens + cache_read + cache_write,
                },
                "cost": cost,
            }),
        )
    }

    async fn export_html(&self, id: Option<String>, output_path: Option<String>) -> RpcResponse {
        let session = self.shared.session.lock().expect("session poisoned").clone();
        let path = match output_path {
            Some(path) => PathBuf::from(path),
            None => default_export_path(self.session_path(), &self.shared.session_dir, &session),
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return RpcResponse::error(id, "export_html", err.to_string());
            }
        }

        let html = export_session_as_html(session.header(), &session.entries());
        if let Err(err) = tokio::fs::write(&path, html).await {
            return RpcResponse::error(id, "export_html", err.to_string());
        }
        RpcResponse::success_with_data(
            id,
            "export_html",
            serde_json::json!({ "path": path.to_string_lossy().to_string() }),
        )
    }

    async fn switch_session(&self, id: Option<String>, session_path: &str) -> RpcResponse {
        if let Err(err) = self.ensure_idle("switch_session") {
            return RpcResponse::error(id, "switch_session", err);
        }

        let resolved = match self.resolve_session_reference(session_path).await {
            Ok(Some(path)) => path,
            Ok(None) => {
                return RpcResponse::error(id, "switch_session", format!("No session found matching '{session_path}'"));
            }
            Err(err) => return RpcResponse::error(id, "switch_session", err.to_string()),
        };

        let loaded = match load_session_manager(&resolved).await {
            Ok(session) => session,
            Err(err) => return RpcResponse::error(id, "switch_session", err.to_string()),
        };
        self.load_session_state(loaded, Some(resolved));
        RpcResponse::success_with_data(id, "switch_session", serde_json::json!({ "cancelled": false }))
    }

    async fn fork(&self, id: Option<String>, entry_id: &str) -> RpcResponse {
        if let Err(err) = self.ensure_idle("fork") {
            return RpcResponse::error(id, "fork", err);
        }
        let source_session = self.shared.session.lock().expect("session poisoned").clone();
        let source_path = self.session_path();
        let Some(source_path_ref) = source_path.as_deref() else {
            return RpcResponse::error(id, "fork", "Cannot fork an in-memory session");
        };
        let Ok((resolved_id, selected_text)) = resolve_fork_entry(&source_session, entry_id) else {
            return RpcResponse::error(id, "fork", format!("No user message on the active path matches '{entry_id}'"));
        };
        let dest_path = build_session_file_path(&self.shared.session_dir, &self.current_model().id);
        if let Err(err) = fork_path_to_file(&source_session, &resolved_id, &dest_path, Some(source_path_ref)).await {
            return RpcResponse::error(id, "fork", err.to_string());
        }
        let loaded = match load_session_manager(&dest_path).await {
            Ok(session) => session,
            Err(err) => return RpcResponse::error(id, "fork", err.to_string()),
        };
        self.load_session_state(loaded, Some(dest_path));
        RpcResponse::success_with_data(id, "fork", serde_json::json!({ "text": selected_text, "cancelled": false }))
    }

    async fn clone_session(&self, id: Option<String>) -> RpcResponse {
        if let Err(err) = self.ensure_idle("clone") {
            return RpcResponse::error(id, "clone", err);
        }
        let source_session = self.shared.session.lock().expect("session poisoned").clone();
        let source_path = self.session_path();
        let Some(source_path_ref) = source_path.as_deref() else {
            return RpcResponse::error(id, "clone", "Cannot clone an in-memory session");
        };
        let dest_path = build_session_file_path(&self.shared.session_dir, &self.current_model().id);
        if let Err(err) = clone_active_path_to_file(&source_session, &dest_path, Some(source_path_ref)).await {
            return RpcResponse::error(id, "clone", err.to_string());
        }
        let loaded = match load_session_manager(&dest_path).await {
            Ok(session) => session,
            Err(err) => return RpcResponse::error(id, "clone", err.to_string()),
        };
        self.load_session_state(loaded, Some(dest_path));
        RpcResponse::success_with_data(id, "clone", serde_json::json!({ "cancelled": false }))
    }

    fn get_fork_messages(&self, id: Option<String>) -> RpcResponse {
        let session = self.shared.session.lock().expect("session poisoned").clone();
        let messages = user_messages_for_fork(&session)
            .into_iter()
            .map(|(entry_id, text)| serde_json::json!({ "entryId": entry_id, "text": text }))
            .collect::<Vec<_>>();
        RpcResponse::success_with_data(id, "get_fork_messages", serde_json::json!({ "messages": messages }))
    }

    fn get_last_assistant_text(&self, id: Option<String>) -> RpcResponse {
        let session = self.shared.session.lock().expect("session poisoned").clone();
        RpcResponse::success_with_data(
            id,
            "get_last_assistant_text",
            serde_json::json!({ "text": last_assistant_text(&session) }),
        )
    }

    async fn set_session_name(&self, id: Option<String>, name: &str) -> RpcResponse {
        if let Err(err) = self.ensure_idle("set_session_name") {
            return RpcResponse::error(id, "set_session_name", err);
        }
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return RpcResponse::error(id, "set_session_name", "Session name cannot be empty");
        }

        let entry = {
            let mut session = self.shared.session.lock().expect("session poisoned");
            let entry_id = session.append_session_info(trimmed.to_string());
            session.get_entry(&entry_id).cloned()
        };
        if let Err(err) = self.persist_entry(entry).await {
            return RpcResponse::error(id, "set_session_name", err.to_string());
        }
        RpcResponse::success(id, "set_session_name")
    }

    fn get_messages(&self, id: Option<String>) -> RpcResponse {
        let session = self.shared.session.lock().expect("session poisoned");
        let messages = session.build_context().messages;
        RpcResponse::success_with_data(id, "get_messages", serde_json::json!({ "messages": messages }))
    }

    fn get_commands(&self, id: Option<String>) -> RpcResponse {
        let source_info = RpcSourceInfo { name: "builtin".to_string(), extension_id: None, path: None };
        let commands = vec![
            builtin_command("help", "Show available commands", &source_info),
            builtin_command("model", "Switch model or open selector", &source_info),
            builtin_command("compact", "Compact session context", &source_info),
            builtin_command("fork", "Fork from a previous user message", &source_info),
            builtin_command("clone", "Clone the current active branch", &source_info),
            builtin_command("export", "Export session to HTML", &source_info),
            builtin_command("resume", "Resume a previous session", &source_info),
        ];
        RpcResponse::success_with_data(id, "get_commands", serde_json::json!({ "commands": commands }))
    }

    async fn append_user_message(&self, message: Message) -> anyhow::Result<(Vec<Message>, usize)> {
        let entry = {
            let mut session = self.shared.session.lock().expect("session poisoned");
            let entry_id = session.append_message(serde_json::to_value(&message)?);
            let entry = session.get_entry(&entry_id).cloned();
            let context = session.build_context();
            let messages = context.messages;
            let len = messages.len();
            (entry, messages, len)
        };
        self.persist_entry(entry.0).await?;
        Ok((entry.1, entry.2))
    }

    async fn start_prompt_run(
        &self,
        model: Model,
        api_key: Option<String>,
        session_messages: Vec<Message>,
        start_len: usize,
    ) {
        let shared = self.shared.clone();
        let cancel = CancellationToken::new();
        *shared.cancel.lock().expect("cancel poisoned") = Some(cancel.clone());
        shared.is_streaming.store(true, Ordering::SeqCst);
        let thinking_level = shared.thinking_level.lock().expect("thinking level poisoned").clone();
        let system_prompt = shared.system_prompt.clone();
        let session_model_id = model.id.clone();

        tokio::spawn(async move {
            let tools = tool_registry::tool_definitions();
            let mut state = AgentState {
                messages: session_messages,
                context: AgentContext {
                    messages: vec![],
                    system_prompt,
                    tools,
                    model: Some(session_model_id),
                    max_turns: 200,
                    current_turn: 0,
                },
                pending_tool_calls: vec![],
            };

            let stream_model = model.clone();
            let stream_options =
                StreamOptions { thinking: Some(thinking_enabled(&thinking_level)), api_key, ..Default::default() };
            let cancel_for_tools = cancel.clone();
            let tool_executor = move |name: &str, _id: &str, args: &serde_json::Value| {
                let cancel = cancel_for_tools.clone();
                let name = name.to_string();
                let args = args.clone();
                let rt_handle = tokio::runtime::Handle::current();
                tokio::task::block_in_place(move || {
                    rt_handle.block_on(async move {
                        tool_registry::execute_tool(&name, args, cancel).await.map_err(|err| err.to_string())
                    })
                })
            };

            let shared_for_steer = shared.clone();
            let shared_for_follow = shared.clone();
            let mut skip_initial_steer_drain = true;
            let steer_fn = move || {
                if skip_initial_steer_drain {
                    skip_initial_steer_drain = false;
                    Vec::new()
                } else {
                    shared_for_steer.steering_queue.lock().expect("steering queue poisoned").drain()
                }
            };
            let follow_fn = || shared_for_follow.follow_up_queue.lock().expect("follow-up queue poisoned").drain();

            let result = agent_loop_with_queues(
                &mut state,
                |ctx: Context| stream::stream(&stream_model, ctx, stream_options.clone()),
                tool_executor,
                |_| {},
                cancel.clone(),
                Some(steer_fn),
                Some(follow_fn),
                false,
                None,
            )
            .await;

            let mut suffix = state.messages[start_len..].to_vec();
            if let Err(err) = &result {
                suffix.push(Message::assistant(vec![ContentBlock::Text(TextContent {
                    text: format!("Error: {}", err),
                })]));
            }
            let _ = persist_messages(&shared, &suffix).await;
            *shared.cancel.lock().expect("cancel poisoned") = None;
            shared.is_streaming.store(false, Ordering::SeqCst);
        });
    }

    async fn apply_thinking_level(&self, level: &str) -> Result<String, String> {
        if !is_valid_thinking_level(level) {
            return Err(format!("Invalid thinking level '{}'. Valid values: {}", level, THINKING_LEVELS.join(", ")));
        }
        let model = self.current_model();
        let supported = supported_thinking_levels(&model);
        let effective = if supported.contains(&level) {
            level.to_string()
        } else {
            supported.first().copied().unwrap_or("off").to_string()
        };
        let changed = {
            let mut current = self.shared.thinking_level.lock().expect("thinking level poisoned");
            if *current == effective {
                false
            } else {
                *current = effective.clone();
                true
            }
        };
        if changed {
            let entry = {
                let mut session = self.shared.session.lock().expect("session poisoned");
                let entry_id = session.append_thinking_level_change(effective.clone());
                session.get_entry(&entry_id).cloned()
            };
            self.persist_entry(entry).await.map_err(|err| err.to_string())?;
        }
        Ok(effective)
    }

    async fn sync_thinking_level_to_model(&self, model: &Model) -> Result<String, String> {
        let effective = {
            let current = self.shared.thinking_level.lock().expect("thinking level poisoned").clone();
            clamp_thinking_level(model, &current)
        };
        let changed = {
            let mut current = self.shared.thinking_level.lock().expect("thinking level poisoned");
            if *current == effective {
                false
            } else {
                *current = effective.clone();
                true
            }
        };
        if changed {
            let entry = {
                let mut session = self.shared.session.lock().expect("session poisoned");
                let entry_id = session.append_thinking_level_change(effective.clone());
                session.get_entry(&entry_id).cloned()
            };
            self.persist_entry(entry).await.map_err(|err| err.to_string())?;
        }
        Ok(effective)
    }

    fn available_models(&self) -> Vec<Model> {
        let mut models = models::all_models().to_vec();
        let current = self.current_model();
        if !models.iter().any(|model| model.id == current.id && model.provider == current.provider) {
            models.insert(0, current);
        }
        models
    }

    fn current_model(&self) -> Model {
        self.shared.model.lock().expect("model poisoned").clone()
    }

    fn ensure_idle(&self, command: &str) -> Result<(), String> {
        if self.shared.is_streaming.load(Ordering::SeqCst) {
            return Err(format!("Cannot run {} while a prompt is streaming", command));
        }
        Ok(())
    }

    fn pending_message_count(&self) -> usize {
        self.shared.steering_queue.lock().expect("steering queue poisoned").len()
            + self.shared.follow_up_queue.lock().expect("follow-up queue poisoned").len()
    }

    fn session_path(&self) -> Option<PathBuf> {
        self.shared.session_path.lock().expect("session path poisoned").clone()
    }

    fn session_file_string(&self) -> Option<String> {
        self.session_path().map(|path| path.to_string_lossy().to_string())
    }

    async fn persist_entry(&self, entry: Option<pi_agent_core::session::types::SessionEntry>) -> anyhow::Result<()> {
        let Some(entry) = entry else {
            return Ok(());
        };
        if let Some(path) = self.session_path() {
            append(&path, &entry).await?;
        }
        Ok(())
    }

    async fn resolve_session_reference(&self, spec: &str) -> anyhow::Result<Option<PathBuf>> {
        if looks_like_session_path(spec) {
            return Ok(Some(PathBuf::from(spec)));
        }
        resolve_session_id_prefix(&self.shared.session_dir, spec).await.map_err(anyhow::Error::from)
    }

    fn load_session_state(&self, session: SessionManager, path: Option<PathBuf>) {
        let current_model = self.current_model();
        let current_thinking = self.shared.thinking_level.lock().expect("thinking level poisoned").clone();
        let restored = restore_model_and_thinking(&session, &current_model, &current_thinking);
        *self.shared.session.lock().expect("session poisoned") = session;
        *self.shared.session_path.lock().expect("session path poisoned") = path;
        *self.shared.model.lock().expect("model poisoned") = restored.0;
        *self.shared.thinking_level.lock().expect("thinking level poisoned") = restored.1;
        self.shared.steering_queue.lock().expect("steering queue poisoned").clear();
        self.shared.follow_up_queue.lock().expect("follow-up queue poisoned").clear();
    }

    fn preflight_model(&self, model: &Model) -> Result<Option<String>, String> {
        if !provider_requires_api_key(model.provider) {
            return Ok(None);
        }
        resolve_api_key(model.provider)
            .ok_or_else(|| {
                format!(
                    "No API key for {}. Set {}_API_KEY or configure ~/.pi/auth.json.",
                    provider_name(model.provider),
                    provider_name(model.provider).to_uppercase()
                )
            })
            .map(Some)
    }
}

fn command_id(command: &RpcCommand) -> Option<String> {
    match command {
        RpcCommand::Prompt { id, .. }
        | RpcCommand::Steer { id, .. }
        | RpcCommand::FollowUp { id, .. }
        | RpcCommand::Abort { id }
        | RpcCommand::NewSession { id, .. }
        | RpcCommand::GetState { id }
        | RpcCommand::SetModel { id, .. }
        | RpcCommand::CycleModel { id }
        | RpcCommand::GetAvailableModels { id }
        | RpcCommand::SetThinkingLevel { id, .. }
        | RpcCommand::CycleThinkingLevel { id }
        | RpcCommand::SetSteeringMode { id, .. }
        | RpcCommand::SetFollowUpMode { id, .. }
        | RpcCommand::Compact { id, .. }
        | RpcCommand::SetAutoCompaction { id, .. }
        | RpcCommand::SetAutoRetry { id, .. }
        | RpcCommand::AbortRetry { id }
        | RpcCommand::Bash { id, .. }
        | RpcCommand::AbortBash { id }
        | RpcCommand::GetSessionStats { id }
        | RpcCommand::ExportHtml { id, .. }
        | RpcCommand::SwitchSession { id, .. }
        | RpcCommand::Fork { id, .. }
        | RpcCommand::Clone { id }
        | RpcCommand::GetForkMessages { id }
        | RpcCommand::GetLastAssistantText { id }
        | RpcCommand::SetSessionName { id, .. }
        | RpcCommand::GetMessages { id }
        | RpcCommand::GetCommands { id } => id.clone(),
    }
}

fn user_message(text: &str, images: Option<Vec<pi_ai_core::types::ImageContent>>) -> Message {
    let mut content = vec![ContentBlock::Text(TextContent { text: text.to_string() })];
    if let Some(images) = images {
        content.extend(images.into_iter().map(ContentBlock::Image));
    }
    Message { role: MessageRole::User, content, id: None, name: None, usage: None, redacted: false }
}

fn queue_mode(mode: &SteeringMode) -> QueueMode {
    match mode {
        SteeringMode::All => QueueMode::All,
        SteeringMode::OneAtATime => QueueMode::OneAtATime,
    }
}

fn provider_requires_api_key(provider: KnownProvider) -> bool {
    !matches!(provider, KnownProvider::Bedrock | KnownProvider::Faux)
}

fn provider_name(provider: KnownProvider) -> &'static str {
    match provider {
        KnownProvider::OpenAi => "openai",
        KnownProvider::Anthropic => "anthropic",
        KnownProvider::Google => "google",
        KnownProvider::Mistral => "mistral",
        KnownProvider::Bedrock => "bedrock",
        KnownProvider::Faux => "faux",
    }
}

fn resolve_api_key(provider: KnownProvider) -> Option<String> {
    let settings = Settings::load().ok();
    if let Some(key) = settings.as_ref().and_then(|value| value.api_key.clone()) {
        return Some(key);
    }
    let env_var = format!("{}_API_KEY", provider_name(provider).to_uppercase());
    if let Ok(key) = std::env::var(&env_var) {
        return Some(key);
    }
    AuthStorage::load().ok().and_then(|storage| storage.get_api_key(provider_name(provider)))
}

fn default_model(settings: &Settings) -> anyhow::Result<&'static Model> {
    if let Some(model_id) = &settings.default_model {
        if let Some(provider) = &settings.default_provider {
            if let Some(parsed) = parse_provider(provider) {
                if let Some(model) = models::get_model(parsed, model_id) {
                    return Ok(model);
                }
            }
        }
        if let Some(model) = models::find_model(model_id) {
            return Ok(model);
        }
    }
    if let Some(provider) = &settings.default_provider {
        if let Some(parsed) = parse_provider(provider) {
            if let Some(model) = models::get_models(parsed).first().copied() {
                return Ok(model);
            }
        }
    }
    models::get_models(KnownProvider::OpenAi)
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("No OpenAI models available"))
}

fn parse_provider(provider: &str) -> Option<KnownProvider> {
    match provider.to_ascii_lowercase().as_str() {
        "openai" => Some(KnownProvider::OpenAi),
        "anthropic" => Some(KnownProvider::Anthropic),
        "google" => Some(KnownProvider::Google),
        "mistral" => Some(KnownProvider::Mistral),
        "bedrock" => Some(KnownProvider::Bedrock),
        "faux" => Some(KnownProvider::Faux),
        _ => None,
    }
}

fn find_model(provider: &str, model_id: &str) -> Option<Model> {
    let parsed = parse_provider(provider)?;
    models::get_model(parsed, model_id).cloned()
}

fn default_session_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PI_SESSION_DIR") {
        return PathBuf::from(dir);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("sessions")
}

fn restore_model_and_thinking(
    session: &SessionManager,
    fallback_model: &Model,
    fallback_thinking: &str,
) -> (Model, String) {
    let context = session.build_context();
    let model = context
        .model
        .and_then(|(_provider, model_id)| models::find_model(&model_id).cloned())
        .unwrap_or_else(|| fallback_model.clone());
    let thinking = if is_valid_thinking_level(&context.thinking_level) {
        context.thinking_level
    } else {
        fallback_thinking.to_string()
    };
    let effective_thinking = clamp_thinking_level(&model, &thinking);
    (model, effective_thinking)
}

fn looks_like_session_path(spec: &str) -> bool {
    spec.contains('/') || spec.contains('\\') || spec.ends_with(".jsonl")
}

async fn load_session_manager(path: &Path) -> anyhow::Result<SessionManager> {
    let (header, entries, _) = read_all(path).await?;
    Ok(SessionManager::from_entries(header, entries))
}

async fn persist_messages(shared: &RpcRuntimeShared, messages: &[Message]) -> anyhow::Result<()> {
    if messages.is_empty() {
        return Ok(());
    }

    let mut entries_to_persist = Vec::new();
    {
        let mut session = shared.session.lock().expect("session poisoned");
        for message in messages {
            let entry_id = session.append_message(serde_json::to_value(message)?);
            if let Some(entry) = session.get_entry(&entry_id).cloned() {
                entries_to_persist.push(entry);
            }
        }
    }

    let path = shared.session_path.lock().expect("session path poisoned").clone();
    if let Some(path) = path {
        for entry in entries_to_persist {
            append(&path, &entry).await?;
        }
    }
    Ok(())
}

fn entry_to_compaction_text(entry: &pi_agent_core::session::types::SessionEntry) -> Option<String> {
    match entry {
        pi_agent_core::session::types::SessionEntry::Message(message) => {
            let role = message.message.get("role").and_then(|value| value.as_str()).unwrap_or("unknown");
            let text = message_text(&message.message);
            if text.is_empty() { None } else { Some(format!("{role}: {text}")) }
        }
        pi_agent_core::session::types::SessionEntry::BranchSummary(summary) => {
            Some(format!("[branch summary] {}", summary.summary))
        }
        pi_agent_core::session::types::SessionEntry::Compaction(compaction) => {
            Some(format!("[compaction] {}", compaction.summary))
        }
        _ => None,
    }
}

fn entry_tokens(entry: &pi_agent_core::session::types::SessionEntry) -> u64 {
    match entry {
        pi_agent_core::session::types::SessionEntry::Message(message) => {
            serde_json::from_value::<Message>(message.message.clone())
                .map(|value| estimate_message_tokens(&value))
                .unwrap_or(0)
        }
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

fn default_export_path(session_path: Option<PathBuf>, session_dir: &Path, session: &SessionManager) -> PathBuf {
    match session_path {
        Some(path) => path.with_extension("html"),
        None => session_dir.join(format!("{}-export.html", session.session_id())),
    }
}

fn user_messages_for_fork(session: &SessionManager) -> Vec<(String, String)> {
    session
        .path_to_root(None)
        .into_iter()
        .filter_map(|entry| match entry {
            pi_agent_core::session::types::SessionEntry::Message(message)
                if message.message.get("role").and_then(|value| value.as_str()) == Some("user") =>
            {
                Some((message.id.clone(), message_text(&message.message)))
            }
            _ => None,
        })
        .filter(|(_, text)| !text.is_empty())
        .collect()
}

fn resolve_fork_entry(session: &SessionManager, entry_id: &str) -> Result<(String, String), ()> {
    session
        .path_to_root(None)
        .into_iter()
        .find_map(|entry| match entry {
            pi_agent_core::session::types::SessionEntry::Message(message)
                if message.message.get("role").and_then(|value| value.as_str()) == Some("user")
                    && message.id.starts_with(entry_id) =>
            {
                Some((message.id.clone(), message_text(&message.message)))
            }
            _ => None,
        })
        .ok_or(())
}

fn message_text(message: &serde_json::Value) -> String {
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

fn last_assistant_text(session: &SessionManager) -> Option<String> {
    session.entries().into_iter().rev().find_map(|entry| match entry {
        pi_agent_core::session::types::SessionEntry::Message(message)
            if message.message.get("role").and_then(|value| value.as_str()) == Some("assistant") =>
        {
            let text = message_text(&message.message);
            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    })
}

fn builtin_command(name: &str, description: &str, source_info: &RpcSourceInfo) -> RpcSlashCommand {
    RpcSlashCommand {
        name: name.to_string(),
        description: Some(description.to_string()),
        source: CommandSource::Prompt,
        source_info: source_info.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai_core::api_registry::{ApiProvider, clear_api_providers, register_api_provider};
    use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStream};
    use pi_ai_core::types::{Message, StreamEvent};
    use std::time::Duration;
    use tempfile::{TempDir, tempdir};

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
                    if message.role == MessageRole::User {
                        message.content.iter().find_map(|block| {
                            if let ContentBlock::Text(text) = block { Some(text.text.clone()) } else { None }
                        })
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

    fn test_model() -> Model {
        Model {
            id: "rpc-test-model".to_string(),
            provider: KnownProvider::Faux,
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
        }
    }

    #[test]
    fn test_provider_requires_api_key_allows_bedrock_and_faux() {
        assert!(!provider_requires_api_key(KnownProvider::Bedrock));
        assert!(!provider_requires_api_key(KnownProvider::Faux));
        assert!(provider_requires_api_key(KnownProvider::OpenAi));
    }

    struct TestRuntime {
        runtime: RpcRuntime,
        _dir: TempDir,
    }

    async fn test_runtime() -> TestRuntime {
        clear_api_providers().await;
        register_api_provider(Box::new(DelayedEchoProvider { api_id: "rpc-test-stream", delay_ms: 50 })).await;

        let dir = tempdir().unwrap();
        let path = dir.path().join("rpc-session.jsonl");
        let runtime = RpcRuntime::from_config(RpcRuntimeConfig {
            model: test_model(),
            system_prompt: None,
            thinking_level: "off".to_string(),
            session_dir: dir.path().to_path_buf(),
            session_path: Some(path),
        })
        .await
        .unwrap();
        TestRuntime { runtime, _dir: dir }
    }

    #[tokio::test]
    async fn test_prompt_and_queue_flow() {
        let test_runtime = test_runtime().await;
        let runtime = &test_runtime.runtime;

        let prompt = runtime
            .handle_command(RpcCommand::Prompt {
                id: Some("req1".to_string()),
                message: "first".to_string(),
                images: None,
                streaming_behavior: None,
            })
            .await;
        assert!(prompt.success);

        let state = runtime.handle_command(RpcCommand::GetState { id: None }).await;
        assert!(state.success);
        let data = state.data.unwrap();
        assert_eq!(data["isStreaming"], serde_json::Value::Bool(true));

        assert!(
            runtime
                .handle_command(RpcCommand::Steer { id: None, message: "steer next".to_string(), images: None })
                .await
                .success
        );
        assert!(
            runtime
                .handle_command(RpcCommand::FollowUp { id: None, message: "follow later".to_string(), images: None })
                .await
                .success
        );

        let queued_state = runtime.handle_command(RpcCommand::GetState { id: None }).await;
        let queued = queued_state.data.unwrap();
        assert_eq!(queued["pendingMessageCount"], serde_json::json!(2));

        assert!(runtime.wait_for_idle(Duration::from_secs(2)).await);

        let messages_response = runtime.handle_command(RpcCommand::GetMessages { id: None }).await;
        assert!(messages_response.success);
        let messages =
            serde_json::from_value::<Vec<Message>>(messages_response.data.unwrap()["messages"].clone()).unwrap();

        let texts = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .iter()
                    .find_map(|block| if let ContentBlock::Text(text) = block { Some(text.text.clone()) } else { None })
            })
            .collect::<Vec<_>>();
        assert!(texts.iter().any(|text| text == "first"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "steer next"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "follow later"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "echo:first"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "echo:steer next"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "echo:follow later"), "{texts:?}");
    }

    #[tokio::test]
    async fn test_session_commands_work() {
        let test_runtime = test_runtime().await;
        let runtime = &test_runtime.runtime;
        assert!(
            runtime
                .handle_command(RpcCommand::Prompt {
                    id: None,
                    message: "alpha".to_string(),
                    images: None,
                    streaming_behavior: None,
                })
                .await
                .success
        );
        assert!(runtime.wait_for_idle(Duration::from_secs(2)).await);

        let fork_messages = runtime.handle_command(RpcCommand::GetForkMessages { id: None }).await;
        assert!(fork_messages.success);
        let first_entry_id = fork_messages.data.unwrap()["messages"][0]["entryId"].as_str().unwrap().to_string();

        assert!(
            runtime
                .handle_command(RpcCommand::SetSessionName { id: None, name: "Named Session".to_string() })
                .await
                .success
        );

        let export = runtime.handle_command(RpcCommand::ExportHtml { id: None, output_path: None }).await;
        assert!(export.success);
        let export_path = export.data.unwrap()["path"].as_str().unwrap().to_string();
        assert!(Path::new(&export_path).exists());

        let last_text = runtime.handle_command(RpcCommand::GetLastAssistantText { id: None }).await;
        assert_eq!(last_text.data.unwrap()["text"], serde_json::json!("echo:alpha"));

        let clone = runtime.handle_command(RpcCommand::Clone { id: None }).await;
        assert!(clone.success);

        let fork = runtime.handle_command(RpcCommand::Fork { id: None, entry_id: first_entry_id }).await;
        assert!(fork.success);
        assert_eq!(fork.data.unwrap()["text"], serde_json::json!("alpha"));

        let commands = runtime.handle_command(RpcCommand::GetCommands { id: None }).await;
        let commands_data = commands.data.unwrap();
        assert!(!commands_data["commands"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_thinking_and_compaction_commands_work() {
        let test_runtime = test_runtime().await;
        let runtime = &test_runtime.runtime;

        let set = runtime.handle_command(RpcCommand::SetThinkingLevel { id: None, level: "high".to_string() }).await;
        assert!(set.success);

        let cycled = runtime.handle_command(RpcCommand::CycleThinkingLevel { id: None }).await;
        assert!(cycled.success);
        assert_eq!(cycled.data.unwrap()["level"], serde_json::json!("xhigh"));

        for i in 0..6 {
            let message = format!("message {i} {}", "x".repeat(400));
            assert!(
                runtime
                    .handle_command(RpcCommand::Prompt { id: None, message, images: None, streaming_behavior: None })
                    .await
                    .success
            );
            assert!(runtime.wait_for_idle(Duration::from_secs(2)).await);
        }

        let compact = runtime
            .handle_command(RpcCommand::Compact {
                id: None,
                custom_instructions: Some("Keep the summary terse".to_string()),
            })
            .await;
        assert!(compact.success, "{:?}", compact.error);
        let data = compact.data.unwrap();
        assert!(data["summary"].as_str().unwrap().contains("echo:"));
    }

    #[tokio::test]
    async fn test_set_model_clamps_thinking_level() {
        let test_runtime = test_runtime().await;
        let runtime = &test_runtime.runtime;

        assert!(
            runtime.handle_command(RpcCommand::SetThinkingLevel { id: None, level: "high".to_string() }).await.success
        );

        let response = runtime
            .handle_command(RpcCommand::SetModel {
                id: None,
                provider: "openai".to_string(),
                model_id: "gpt-4o".to_string(),
            })
            .await;
        assert!(response.success, "{:?}", response.error);
        assert_eq!(response.data.as_ref().unwrap()["thinkingLevel"], serde_json::json!("off"));

        let state = runtime.handle_command(RpcCommand::GetState { id: None }).await;
        assert_eq!(state.data.unwrap()["thinkingLevel"], serde_json::json!("off"));
    }

    #[tokio::test]
    async fn test_cycle_model_keeps_response_thinking_in_sync() {
        let test_runtime = test_runtime().await;
        let runtime = &test_runtime.runtime;

        let models = runtime.available_models();
        let pair = models
            .windows(2)
            .find(|window| window[0].supports_thinking && !window[1].supports_thinking)
            .expect("expected a thinking->non-thinking model transition in the catalog");
        let current = &pair[0];
        let next = &pair[1];

        if find_model(provider_name(current.provider), &current.id).is_some() {
            assert!(
                runtime
                    .handle_command(RpcCommand::SetModel {
                        id: None,
                        provider: provider_name(current.provider).to_string(),
                        model_id: current.id.clone(),
                    })
                    .await
                    .success
            );
        }
        assert!(
            runtime.handle_command(RpcCommand::SetThinkingLevel { id: None, level: "high".to_string() }).await.success
        );

        let response = runtime.handle_command(RpcCommand::CycleModel { id: None }).await;
        assert!(response.success, "{:?}", response.error);
        let data = response.data.unwrap();
        assert_eq!(data["model"]["id"], serde_json::json!(next.id));
        assert_eq!(data["thinkingLevel"], serde_json::json!("off"));

        let state = runtime.handle_command(RpcCommand::GetState { id: None }).await;
        assert_eq!(state.data.unwrap()["thinkingLevel"], serde_json::json!("off"));
    }
}
