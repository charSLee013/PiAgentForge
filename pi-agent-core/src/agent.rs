//! High-level Agent API wrapping the low-level agent loop.
//!
//! The `Agent` struct manages message queues, lifecycle events, and optional
//! hooks — mirroring the TypeScript `Agent` class in packages/agent/src/agent.ts.

use crate::agent_loop::{self, AgentError};
use crate::hook::{AfterToolCallContext, AfterToolHook, BeforeToolCallContext, BeforeToolHook};
use crate::queue::{MessageQueue, QueueMode};
use crate::types::{AgentContext, AgentEvent, AgentState, AgentToolResult};
use pi_ai_core::event_stream::AssistantMessageEventStream;
use pi_ai_core::stream::StreamError;
use pi_ai_core::types::{ContentBlock, Context, Message, TextContent};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// A listener called for every [`AgentEvent`] during a run.
pub type AgentListener = Box<dyn Fn(AgentEvent) + Send + Sync>;

/// High-level agent wrapping the low-level `agent_loop`.
pub struct Agent {
    state: AgentState,
    before_hook: Option<BeforeToolHook>,
    after_hook: Option<AfterToolHook>,
    steering_queue: MessageQueue,
    follow_up_queue: MessageQueue,
    listeners: Arc<RwLock<HashMap<u64, AgentListener>>>,
    next_listener_id: u64,
    /// Per-run cancellation token. Replaced at start of each `run()`.
    cancel: CancellationToken,
    is_streaming: Arc<AtomicBool>,
    /// Last assistant message built during a turn (for hook context).
    last_assistant: Arc<Mutex<Option<Message>>>,
}

impl Agent {
    pub fn new(
        state: AgentState,
        before_tool_call: Option<BeforeToolHook>,
        after_tool_call: Option<AfterToolHook>,
    ) -> Self {
        Self {
            state,
            before_hook: before_tool_call,
            after_hook: after_tool_call,
            steering_queue: MessageQueue::new(QueueMode::OneAtATime),
            follow_up_queue: MessageQueue::new(QueueMode::OneAtATime),
            listeners: Arc::new(RwLock::new(HashMap::new())),
            next_listener_id: 0,
            cancel: CancellationToken::new(),
            is_streaming: Arc::new(AtomicBool::new(false)),
            last_assistant: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_system_prompt(system_prompt: &str, max_turns: u32) -> Self {
        let state = AgentState {
            messages: vec![],
            context: AgentContext {
                messages: vec![],
                system_prompt: Some(system_prompt.to_string()),
                tools: vec![],
                model: None,
                max_turns,
                current_turn: 0,
            },
            pending_tool_calls: vec![],
        };
        Self::new(state, None, None)
    }

    // ── Lifecycle API ───────────────────────────────────────────────────────

    pub fn subscribe(&mut self, listener: AgentListener) -> u64 {
        let id = self.next_listener_id;
        self.next_listener_id += 1;
        if let Ok(mut list) = self.listeners.write() {
            list.insert(id, listener);
        }
        id
    }

    pub fn unsubscribe(&mut self, token: u64) -> bool {
        if let Ok(mut list) = self.listeners.write() { list.remove(&token).is_some() } else { false }
    }

    pub fn steer(&mut self, message: Message) {
        self.steering_queue.enqueue(message);
    }
    pub fn follow_up(&mut self, message: Message) {
        self.follow_up_queue.enqueue(message);
    }
    pub fn abort(&self) {
        self.cancel.cancel();
    }
    pub fn is_streaming(&self) -> bool {
        self.is_streaming.load(Ordering::SeqCst)
    }
    pub fn has_queued_messages(&self) -> bool {
        self.steering_queue.has_items() || self.follow_up_queue.has_items()
    }
    pub fn state(&self) -> &AgentState {
        &self.state
    }
    pub fn state_mut(&mut self) -> &mut AgentState {
        &mut self.state
    }

    fn reset_cancel(&mut self) {
        self.cancel = CancellationToken::new();
    }

    // ── Run ─────────────────────────────────────────────────────────────────

    /// Run the agent loop with per-turn steering and follow-up queue polling.
    ///
    /// Resets `is_streaming` on error. Allocates a fresh CancellationToken
    /// per run so `abort()` does not permanently brick the instance.
    pub async fn run<F, Fut, G>(&mut self, stream_fn: F, tool_executor: G) -> Result<(), AgentError>
    where
        F: Fn(Context) -> Fut,
        Fut: Future<Output = Result<AssistantMessageEventStream, StreamError>>,
        G: Fn(&str, &str, &serde_json::Value) -> Result<AgentToolResult, String> + Send + Sync + 'static,
    {
        self.is_streaming.store(true, Ordering::SeqCst);
        self.reset_cancel();
        let result = self.run_inner(stream_fn, tool_executor).await;
        self.is_streaming.store(false, Ordering::SeqCst);
        result
    }

    async fn run_inner<F, Fut, G>(&mut self, stream_fn: F, tool_executor: G) -> Result<(), AgentError>
    where
        F: Fn(Context) -> Fut,
        Fut: Future<Output = Result<AssistantMessageEventStream, StreamError>>,
        G: Fn(&str, &str, &serde_json::Value) -> Result<AgentToolResult, String> + Send + Sync + 'static,
    {
        // Clone Arc<hook> so hooks survive multiple loop iterations
        let before = self.before_hook.clone();
        let after = self.after_hook.clone();
        let last_assistant = self.last_assistant.clone();

        // Build the hook-wrapped tool executor once (it's Fn via Arc<Mutex>).
        // Wrap in Arc for 'static lifetime (needed by spawn_blocking parallelism).
        let hooked_executor = Arc::new(build_hooked_executor(before, after, tool_executor, last_assistant));

        // ── Per-turn queue polling ──────────────────────────────────────
        //
        // We split borrows: queue closures capture &mut Queue for per-turn
        // polling, while agent_loop takes &mut state.
        let steer_queue = &mut self.steering_queue;
        let follow_queue = &mut self.follow_up_queue;

        // Drain pre-existing steering messages before entering the loop.
        for msg in steer_queue.drain() {
            self.state.messages.push(msg);
        }

        // Create per-turn polling closures (FnMut, capture &mut Queue).
        let steer_fn = || steer_queue.drain();
        let follow_fn = || follow_queue.drain();

        // Build the event sink from listeners
        let listeners = self.listeners.clone();
        let event_sink = move |event: AgentEvent| {
            if let Ok(list) = listeners.read() {
                for l in list.values() {
                    l(event.clone());
                }
            }
        };

        // Call agent_loop_with_queues with per-turn polling closures.
        // Clone the Arc<hooked_executor> so it can be shared across 'static bounds.
        let exec_for_loop = hooked_executor.clone();
        agent_loop::agent_loop_with_queues(
            &mut self.state,
            |ctx: Context| stream_fn(ctx),
            move |name: &str, id: &str, args: &serde_json::Value| exec_for_loop.as_ref()(name, id, args),
            event_sink,
            self.cancel.clone(),
            Some(steer_fn),
            Some(follow_fn),
            false, // parallel
            Some(&self.last_assistant),
        )
        .await
    }
}

/// Build a hook-wrapped tool executor closure.
///
/// The closure is `Fn` (not `FnMut`) because it captures `Arc<Mutex<dyn FnMut>>`,
/// using interior mutability. This satisfies `agent_loop`'s `G: Fn(...)` bound.
fn build_hooked_executor<G>(
    before: Option<BeforeToolHook>,
    after: Option<AfterToolHook>,
    tool_executor: G,
    last_assistant: Arc<Mutex<Option<Message>>>,
) -> impl Fn(&str, &str, &serde_json::Value) -> Result<AgentToolResult, String>
where
    G: Fn(&str, &str, &serde_json::Value) -> Result<AgentToolResult, String>,
{
    move |name: &str, id: &str, args: &serde_json::Value| {
        // ── before_tool_call hook ──────────────────────────────────────
        if let Some(ref hook) = before {
            if let Ok(mut guard) = hook.lock() {
                let msg = last_assistant.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| Message::user_text(""));
                let ctx = BeforeToolCallContext {
                    message: msg,
                    tool_name: name.to_string(),
                    tool_call_id: id.to_string(),
                    args: args.clone(),
                };
                match guard(ctx) {
                    Ok(Some(r)) if r.block => {
                        return Ok(AgentToolResult {
                            tool_call_id: id.to_string(),
                            content: vec![ContentBlock::Text(TextContent {
                                text: r.reason.unwrap_or_else(|| "blocked by before_tool_call".into()),
                            })],
                            is_error: true,
                            details: None,
                        });
                    }
                    Err(e) => {
                        return Ok(AgentToolResult {
                            tool_call_id: id.to_string(),
                            content: vec![ContentBlock::Text(TextContent { text: e })],
                            is_error: true,
                            details: None,
                        });
                    }
                    _ => {}
                }
            }
        }

        // ── Execute the real tool ──────────────────────────────────────
        let tool_result = tool_executor(name, id, args);

        // ── after_tool_call hook ───────────────────────────────────────
        if let Some(ref hook) = after {
            if let Ok(mut guard) = hook.lock() {
                if let Ok(tr) = &tool_result {
                    let msg =
                        last_assistant.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| Message::user_text(""));
                    let ctx = AfterToolCallContext {
                        message: msg,
                        tool_name: name.to_string(),
                        tool_call_id: id.to_string(),
                        args: args.clone(),
                        result: tr.content.clone(),
                        is_error: tr.is_error,
                    };
                    match guard(ctx) {
                        Ok(Some(or)) => {
                            return Ok(AgentToolResult {
                                tool_call_id: id.to_string(),
                                content: or.content.unwrap_or_else(|| tr.content.clone()),
                                is_error: or.is_error.unwrap_or(tr.is_error),
                                details: tr.details.clone(),
                            });
                        }
                        Err(e) => {
                            return Ok(AgentToolResult {
                                tool_call_id: id.to_string(),
                                content: vec![ContentBlock::Text(TextContent { text: e })],
                                is_error: true,
                                details: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        tool_result
    }
}
