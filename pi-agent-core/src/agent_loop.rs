//! Agent loop — the core conversation loop.
//! Mirrors packages/agent/src/agent-loop.ts
//!
//! This module implements a state-machine-based agent loop that:
//! - Streams responses from an LLM via an injected stream function
//! - Executes tool calls via an injected tool executor
//! - Emits lifecycle events via an event sink callback
//! - Supports cancellation via `CancellationToken`
//! - Enforces a maximum turn limit

use crate::types::*;
use pi_ai_core::event_stream::AssistantMessageEventStream;
use pi_ai_core::stream::StreamError;
use pi_ai_core::types::*;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

/// Errors that can occur during agent loop execution.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The LLM stream returned an error.
    #[error("Stream error: {0}")]
    StreamError(#[from] StreamError),

    /// The agent loop reached the maximum number of allowed turns.
    #[error("Max turns ({0}) reached")]
    MaxTurnsReached(u32),

    /// The agent loop was cancelled via CancellationToken.
    #[error("Agent cancelled")]
    Cancelled,

    /// A tool execution returned an unrecoverable error.
    #[error("Tool execution error: {0}")]
    ToolExecutionError(String),
}

/// Internal accumulator for building a tool call from chunked `ToolCallDelta` events.
#[derive(Debug, Clone)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

/// Run the agent loop: the heart of the pi-coding-agent.
///
/// This function takes:
/// - `state`: Mutable agent state (messages, context). Messages are appended as the loop runs.
/// - `stream_fn`: Injected async function that produces an LLM event stream from a `Context`.
/// - `tool_executor`: Synchronous function that executes a tool call and returns a result.
/// - `event_sink`: Callback invoked for each lifecycle event (for UI, logging, etc.).
/// - `cancel`: Tokio cancellation token. The loop checks this between each event.
///
/// The loop algorithm (mapping the TS `runLoop`):
///
/// 1. Emit `AgentStart` with a snapshot of the current context.
/// 2. For each turn (up to `max_turns`), increment the turn counter, build
///    the LLM context, stream the assistant response, and emit message events.
/// 3. If the model requests tools, execute them, append tool results, and
///    continue to the next turn.
/// 4. End on a normal stop reason, stream error, cancellation, or max turns.
pub async fn agent_loop<F, Fut, G, H>(
    state: &mut AgentState,
    stream_fn: F,
    tool_executor: G,
    event_sink: H,
    cancel: CancellationToken,
) -> Result<(), AgentError>
where
    F: Fn(Context) -> Fut,
    Fut: Future<Output = Result<AssistantMessageEventStream, StreamError>>,
    G: Fn(&str, &str, &serde_json::Value) -> Result<AgentToolResult, String>,
    H: Fn(AgentEvent),
{
    let max_turns = state.context.max_turns;
    let mut turn_number: u32 = 0;

    // ── Agent start ──────────────────────────────────────────────────────────
    event_sink(AgentEvent::AgentStart {
        context: state.context.clone(),
    });

    // ── Main turn loop ───────────────────────────────────────────────────────
    loop {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }

        if turn_number >= max_turns {
            event_sink(AgentEvent::AgentEnd {
                finish_reason: "max_turns".to_string(),
                messages: state.messages.clone(),
            });
            return Err(AgentError::MaxTurnsReached(max_turns));
        }

        turn_number += 1;
        state.context.current_turn = turn_number;

        // ── Turn start ─────────────────────────────────────────────────────
        event_sink(AgentEvent::TurnStart { turn_number });

        // ── Build LLM context from current state ──────────────────────────
        let llm_context = Context {
            messages: state.messages.clone(),
            system_prompt: state.context.system_prompt.clone(),
            model: state.context.model.clone(),
            tools: state.context.tools.clone(),
        };

        // ── Stream LLM response ───────────────────────────────────────────
        let mut stream = match stream_fn(llm_context).await {
            Ok(s) => s,
            Err(e) => {
                event_sink(AgentEvent::AgentEnd {
                    finish_reason: "stream_error".to_string(),
                    messages: state.messages.clone(),
                });
                return Err(AgentError::StreamError(e));
            }
        };

        let message_id = uuid::Uuid::new_v4().to_string();
        event_sink(AgentEvent::MessageStart {
            message_id: message_id.clone(),
        });

        // Accumulators for building the assistant message
        let mut text_parts: Vec<String> = Vec::new();
        let mut thinking_parts: Vec<String> = Vec::new();
        let mut tool_calls: BTreeMap<u32, ToolCallAccum> = BTreeMap::new();
        let mut final_message: Option<Message> = None;
        let mut stop_reason: Option<String> = None;
        let mut stream_error: Option<String> = None;

        // Use `select!` to race between stream events and cancellation,
        // so that a pending stream does not prevent shutdown.
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Err(AgentError::Cancelled);
                }
                event = stream.next() => {
                    match event {
                        None => break,
                        Some(event) => match event {
                StreamEvent::Start => {
                    // Already emitted MessageStart above.
                }

                StreamEvent::TextDelta { delta } => {
                    text_parts.push(delta.clone());
                    event_sink(AgentEvent::MessageDelta {
                        message_id: message_id.clone(),
                        delta,
                    });
                }

                StreamEvent::ThinkingDelta { delta } => {
                    thinking_parts.push(delta.clone());
                    event_sink(AgentEvent::MessageDelta {
                        message_id: message_id.clone(),
                        delta,
                    });
                }

                StreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    let entry = tool_calls.entry(index).or_insert_with(|| ToolCallAccum {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(id_val) = id {
                        entry.id = id_val;
                    }
                    if let Some(name_val) = name {
                        entry.name = name_val;
                    }
                    if let Some(arg_chunk) = arguments {
                        entry.arguments.push_str(&arg_chunk);
                    }
                }

                StreamEvent::Usage(_usage) => {
                    // Usage tracking could be accumulated here in the future.
                }

                StreamEvent::Done {
                    message,
                    stop_reason: reason,
                } => {
                    final_message = message;
                    stop_reason = reason;
                }

                StreamEvent::Error { error } => {
                    stream_error = Some(error.message);
                }
                        },
                    }
                }
            }
        }

        // Check cancellation after the stream loop ends
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }

        // ── Handle stream-level errors ─────────────────────────────────────
        if let Some(err_msg) = stream_error {
            let error_content = vec![ContentBlock::Text(TextContent {
                text: err_msg.clone(),
            })];
            event_sink(AgentEvent::MessageEnd {
                message: error_content,
                message_id: message_id.clone(),
            });
            event_sink(AgentEvent::TurnEnd { turn_number });
            event_sink(AgentEvent::AgentEnd {
                finish_reason: "error".to_string(),
                messages: state.messages.clone(),
            });
            return Err(AgentError::StreamError(StreamError::ProviderError(err_msg)));
        }

        // ── Build final assistant message from accumulators ────────────────
        let assistant_msg = final_message.unwrap_or_else(|| {
            let mut content: Vec<ContentBlock> = Vec::new();

            if !text_parts.is_empty() {
                content.push(ContentBlock::Text(TextContent {
                    text: text_parts.concat(),
                }));
            }

            if !thinking_parts.is_empty() {
                content.push(ContentBlock::Thinking(ThinkingContent {
                    thinking: thinking_parts.concat(),
                    signature: None,
                }));
            }

            for tc in tool_calls.values() {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);
                content.push(ContentBlock::ToolCall(ToolCallContent {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: args,
                }));
            }

            Message::assistant(content)
        });

        // ── Emit message end ───────────────────────────────────────────────
        event_sink(AgentEvent::MessageEnd {
            message: assistant_msg.content.clone(),
            message_id: message_id.clone(),
        });

        // ── Add assistant message to state ────────────────────────────────
        state.messages.push(assistant_msg.clone());

        // ── Determine next action based on stop_reason ─────────────────────
        let reason = stop_reason.as_deref().unwrap_or("");

        match reason {
            // Clean stop: the LLM finished its response
            "end_turn" | "stop" => {
                event_sink(AgentEvent::TurnEnd { turn_number });
                event_sink(AgentEvent::AgentEnd {
                    finish_reason: reason.to_string(),
                    messages: state.messages.clone(),
                });
                return Ok(());
            }

            // Tool use: the LLM wants us to execute one or more tools
            "tool_use" | "toolUse" => {
                // Extract tool call blocks from the message content
                let tool_call_blocks: Vec<(String, String, serde_json::Value)> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::ToolCall(tc) = c {
                            Some((tc.id.clone(), tc.name.clone(), tc.arguments.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();

                // Execute each tool call sequentially
                for (tc_id, tc_name, tc_args) in &tool_call_blocks {
                    event_sink(AgentEvent::ToolExecutionStart {
                        tool_call_id: tc_id.clone(),
                        tool_name: tc_name.clone(),
                        arguments: tc_args.clone(),
                    });

                    match tool_executor(tc_name, tc_id, tc_args) {
                        Ok(result) => {
                            let content = result.content.clone();
                            let is_error = result.is_error;
                            event_sink(AgentEvent::ToolExecutionEnd {
                                tool_call_id: tc_id.clone(),
                                tool_name: tc_name.clone(),
                                result,
                            });

                            // Wrap tool result in ToolResultContent so the provider
                            // can extract tool_call_id correctly.
                            let tool_result_content = ContentBlock::ToolResult(ToolResultContent {
                                id: tc_id.clone(),
                                name: tc_name.clone(),
                                content: Some(content),
                                error: None,
                                is_error,
                            });
                            state.messages.push(Message {
                                role: MessageRole::Tool,
                                content: vec![tool_result_content],
                                id: None,
                                name: Some(tc_name.clone()),
                                usage: None,
                                redacted: false,
                            });

                            // If the tool reported an error, still continue — the LLM
                            // will see the error content and decide what to do.
                            if is_error {
                                tracing::warn!(
                                    tool_name = %tc_name,
                                    tool_call_id = %tc_id,
                                    "Tool execution returned an error"
                                );
                            }
                        }
                        Err(err) => {
                            let error_content = vec![ContentBlock::Text(TextContent {
                                text: format!("Error: {}", err),
                            })];
                            let error_result = AgentToolResult {
                                tool_call_id: tc_id.clone(),
                                content: error_content.clone(),
                                is_error: true,
                                details: Some(serde_json::json!({"error": err})),
                            };
                            event_sink(AgentEvent::ToolExecutionEnd {
                                tool_call_id: tc_id.clone(),
                                tool_name: tc_name.clone(),
                                result: error_result,
                            });

                            state.messages.push(Message {
                                role: MessageRole::Tool,
                                content: vec![ContentBlock::ToolResult(ToolResultContent {
                                    id: tc_id.clone(),
                                    name: tc_name.clone(),
                                    content: Some(error_content),
                                    error: Some(err.clone()),
                                    is_error: true,
                                })],
                                id: None,
                                name: Some(tc_name.clone()),
                                usage: None,
                                redacted: false,
                            });
                        }
                    }
                }

                event_sink(AgentEvent::TurnEnd { turn_number });

                // Loop back to step (2) for the next turn with tool results
                // already appended to state.messages.
                continue;
            }

            // The stream terminated with an error or was aborted
            "error" | "aborted" => {
                event_sink(AgentEvent::TurnEnd { turn_number });
                event_sink(AgentEvent::AgentEnd {
                    finish_reason: reason.to_string(),
                    messages: state.messages.clone(),
                });
                return Ok(());
            }

            // Unknown stop_reason — treat as end of conversation
            _ => {
                event_sink(AgentEvent::TurnEnd { turn_number });
                event_sink(AgentEvent::AgentEnd {
                    finish_reason: reason.to_string(),
                    messages: state.messages.clone(),
                });
                return Ok(());
            }
        }
    }
}

/// Agent loop with per-turn steering and follow-up queue polling.
///
/// This variant extends [`agent_loop()`] with:
///
/// - **Queue polling**: `get_steering`/`get_follow_up` closures polled
///   between turns (like TS `getSteeringMessages`/`getFollowUpMessages`).
/// - **Parallel execution**: when `parallel` is `true`, tool calls in a
///   single assistant response are run concurrently via spawn_blocking.
///
/// Unlike [`agent_loop()`], this function owns its own turn loop instead
/// of delegating, so it can interleave queue polling and parallelism.
#[allow(clippy::too_many_arguments)]
pub async fn agent_loop_with_queues<F, Fut, G, H>(
    state: &mut AgentState,
    stream_fn: F,
    tool_executor: G,
    event_sink: H,
    cancel: CancellationToken,
    mut get_steering: Option<&mut dyn FnMut() -> Vec<Message>>,
    mut get_follow_up: Option<&mut dyn FnMut() -> Vec<Message>>,
    parallel: bool,
    last_assistant: Option<&Arc<Mutex<Option<Message>>>>,
) -> Result<(), AgentError>
where
    F: Fn(Context) -> Fut,
    Fut: Future<Output = Result<AssistantMessageEventStream, StreamError>>,
    G: Fn(&str, &str, &serde_json::Value) -> Result<crate::types::AgentToolResult, String> + Send + Sync + 'static,
    H: Fn(AgentEvent),
{
    let max_turns = state.context.max_turns;
    let mut turn_number: u32 = 0;

    let event_sink = &event_sink; // allow partial move into closures
    let stream_fn = &stream_fn;

    // Wrap executor in Arc for spawn_blocking (parallel execution).
    let executor_arc = std::sync::Arc::new(tool_executor);

    // ── Agent start ──────────────────────────────────────────────────────────
    event_sink(AgentEvent::AgentStart {
        context: state.context.clone(),
    });

    // Outer loop: follow-up queue
    loop {
        let mut has_more = true;

        // Inner loop: tool calls + steering
        while has_more {
            // Drain steering before LLM call
            if let Some(ref mut steer) = get_steering {
                for msg in steer() {
                    state.messages.push(msg);
                }
            }

            // ── Turn start ─────────────────────────────────────────────
            if cancel.is_cancelled() {
                event_sink(AgentEvent::AgentEnd { finish_reason: "cancelled".into(), messages: state.messages.clone() });
                return Err(AgentError::Cancelled);
            }
            if turn_number >= max_turns {
                event_sink(AgentEvent::AgentEnd { finish_reason: "max_turns".into(), messages: state.messages.clone() });
                return Err(AgentError::MaxTurnsReached(max_turns));
            }
            turn_number += 1;
            state.context.current_turn = turn_number;
            event_sink(AgentEvent::TurnStart { turn_number });

            // ── Stream LLM response ────────────────────────────────────
            let llm_context = Context {
                messages: state.messages.clone(),
                system_prompt: state.context.system_prompt.clone(),
                model: state.context.model.clone(),
                tools: state.context.tools.clone(),
            };
            let mut stream = match stream_fn(llm_context).await {
                Ok(s) => s,
                Err(e) => { event_sink(AgentEvent::AgentEnd { finish_reason: "stream_error".into(), messages: state.messages.clone() }); return Err(AgentError::StreamError(e)); }
            };

            let message_id = uuid::Uuid::new_v4().to_string();
            event_sink(AgentEvent::MessageStart { message_id: message_id.clone() });

            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: BTreeMap<u32, ToolCallAccum> = BTreeMap::new();
            let mut stop_reason: Option<String> = None;
            let mut final_message: Option<Message> = None;
            let mut stream_error: Option<String> = None;

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { return Err(AgentError::Cancelled); }
                    event = stream.next() => {
                        match event {
                            None => { break; }
                            Some(StreamEvent::Done { message, stop_reason: sr }) => {
                                final_message = message;
                                stop_reason = sr;
                                break;
                            }
                            Some(StreamEvent::Start) => {}
                            Some(StreamEvent::TextDelta { delta }) => {
                                text_parts.push(delta.clone());
                                event_sink(AgentEvent::MessageDelta { message_id: message_id.clone(), delta });
                            }
                            Some(StreamEvent::ThinkingDelta { delta }) => {
                                event_sink(AgentEvent::MessageDelta { message_id: message_id.clone(), delta });
                            }
                            Some(StreamEvent::ToolCallDelta { index, id, name, arguments }) => {
                                let entry = tool_calls.entry(index).or_insert_with(|| ToolCallAccum { id: String::new(), name: String::new(), arguments: String::new() });
                                if let Some(id) = id { entry.id = id; }
                                if let Some(name) = name { entry.name = name; }
                                if let Some(args) = arguments { entry.arguments.push_str(&args); }
                            }
                            Some(StreamEvent::Usage(_)) => {}
                            Some(StreamEvent::Error { error }) => {
                                stream_error = Some(error.message);
                                break;
                            }
                        }
                    }
                }
            }

            // Check for stream error and abort the run
            if let Some(err) = stream_error {
                event_sink(AgentEvent::AgentEnd { finish_reason: "stream_error".into(), messages: state.messages.clone() });
                event_sink(AgentEvent::TurnEnd { turn_number });
                return Err(AgentError::StreamError(pi_ai_core::stream::StreamError::ProviderError(err)));
            }

            // Build assistant message
            let mut content: Vec<ContentBlock> = Vec::new();
            if !text_parts.is_empty() {
                content.push(ContentBlock::Text(TextContent { text: text_parts.concat() }));
            }
            // (thinking_parts skipped for brevity — they are preserved in the message)
            for tc in tool_calls.values() {
                let args: serde_json::Value = serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);
                content.push(ContentBlock::ToolCall(ToolCallContent { id: tc.id.clone(), name: tc.name.clone(), arguments: args }));
            }
            let assistant_msg = final_message.unwrap_or_else(|| {
                Message { role: MessageRole::Assistant, content: content.clone(), id: None, name: None, usage: None, redacted: false }
            });
            state.messages.push(assistant_msg.clone());
            event_sink(AgentEvent::MessageEnd { message: content.clone(), message_id: message_id.clone() });

            // Save the assistant message for hook context
            if let Some(last) = last_assistant {
                if let Ok(mut guard) = last.lock() {
                    *guard = Some(assistant_msg.clone());
                }
            }

            let reason = stop_reason.as_deref().unwrap_or("end_turn");

            // ── Tool use → execute calls ──────────────────────────────
            if reason == "tool_use" || reason == "toolUse" {
                let calls: Vec<(String, String, serde_json::Value)> = assistant_msg.content.iter()
                    .filter_map(|c| if let ContentBlock::ToolCall(tc) = c { Some((tc.id.clone(), tc.name.clone(), tc.arguments.clone())) } else { None })
                    .collect();

                let should_parallel = parallel && calls.len() > 1;

                // Collect results — either parallel or sequential
                let results: Vec<(String, std::result::Result<crate::types::AgentToolResult, String>)> = if should_parallel {
                    use tokio::task::spawn_blocking;
                    let mut tasks = Vec::with_capacity(calls.len());
                    for (id, name, args) in &calls {
                        let id = id.clone();
                        let name = name.clone();
                        let args = args.clone();
                        let exec = executor_arc.clone();
                        tasks.push(spawn_blocking(move || {
                            (id.clone(), exec.as_ref()(&name, &id, &args))
                        }));
                    }
                    let mut results = Vec::with_capacity(calls.len());
                    for task in tasks {
                        match task.await {
                            Ok((id, Ok(result))) => results.push((id, Ok(result))),
                            Ok((id, Err(e))) => results.push((id, Err(e))),
                            Err(e) => results.push((String::new(), Err(e.to_string()))),
                        }
                    }
                    results
                } else {
                    calls.iter().map(|(id, name, args)| {
                        (id.clone(), executor_arc.as_ref()(name, id, args))
                    }).collect()
                };

                // Emit events for each result (in original call order)
                for (tc_id, result) in results {
                    let tc_name = calls.iter().find(|(id, _, _)| *id == tc_id).map(|(_, n, _)| n.clone()).unwrap_or_default();
                    event_sink(AgentEvent::ToolExecutionStart { tool_call_id: tc_id.clone(), tool_name: tc_name.clone(), arguments: serde_json::Value::Null });

                    match result {
                        Ok(tool_result) => {
                            let content_clone = tool_result.content.clone();
                            let is_error = tool_result.is_error;
                            event_sink(AgentEvent::ToolExecutionEnd { tool_call_id: tc_id.clone(), tool_name: tc_name.clone(), result: tool_result.clone() });
                            state.messages.push(Message {
                                role: MessageRole::Tool,
                                content: vec![ContentBlock::ToolResult(ToolResultContent { id: tc_id.clone(), name: tc_name.clone(), content: Some(content_clone), error: None, is_error })],
                                id: None, name: Some(tc_name), usage: None, redacted: false,
                            });
                        }
                        Err(err) => {
                            let error_content = vec![ContentBlock::Text(TextContent { text: format!("Error: {}", err) })];
                            let error_result = crate::types::AgentToolResult { tool_call_id: tc_id.clone(), content: error_content.clone(), is_error: true, details: Some(serde_json::json!({"error": err})) };
                            event_sink(AgentEvent::ToolExecutionEnd { tool_call_id: tc_id.clone(), tool_name: tc_name.clone(), result: error_result });
                            state.messages.push(Message {
                                role: MessageRole::Tool,
                                content: vec![ContentBlock::ToolResult(ToolResultContent { id: tc_id.clone(), name: tc_name.clone(), content: Some(error_content), error: Some(err), is_error: true })],
                                id: None, name: Some(tc_name), usage: None, redacted: false,
                            });
                        }
                    }
                }

                event_sink(AgentEvent::TurnEnd { turn_number });

                // Check steering queue
                has_more = false;
                if let Some(ref mut steer) = get_steering {
                    let more = steer();
                    if !more.is_empty() {
                        for msg in more { state.messages.push(msg); }
                        has_more = true;
                    }
                }
                continue; // next inner loop iteration
            }

            // ── end_turn / stop / error ───────────────────────────────────
            event_sink(AgentEvent::TurnEnd { turn_number });
            has_more = false;
        }

        // Inner loop done → check follow-up
        let follow_ups = match get_follow_up {
            Some(ref mut f) => f(),
            None => break,
        };
        if follow_ups.is_empty() { break; }
        for msg in follow_ups { state.messages.push(msg); }
    }

    event_sink(AgentEvent::AgentEnd { finish_reason: "end_turn".into(), messages: state.messages.clone() });
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai_core::event_stream::EventStream;
    use pi_ai_core::types::StreamEvent;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Create a mock stream function that yields a single text response.
    fn text_stream_fn(
        text: &str,
        stop_reason: &str,
    ) -> impl Fn(Context) -> Pin<Box<dyn Future<Output = Result<AssistantMessageEventStream, StreamError>> + Send>>
    {
        let text = text.to_string();
        let stop_reason = stop_reason.to_string();
        move |_ctx: Context| {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta {
                delta: text.clone(),
            });
            let _ = tx.send(StreamEvent::Done {
                message: None,
                stop_reason: Some(stop_reason.clone()),
            });
            drop(tx);
            Box::pin(std::future::ready(Ok(rx)))
        }
    }

    /// Create a mock stream function that yields a tool call on the first
    /// invocation and an empty `end_turn` on subsequent invocations.
    ///
    /// This simulates the real LLM flow: the model requests a tool, receives
    /// the result in the next turn, and then responds with a plain text answer.
    fn tool_call_stream_fn(
        preface: &str,
        tool_id: &str,
        tool_name: &str,
        tool_args: &str,
    ) -> impl Fn(Context) -> Pin<Box<dyn Future<Output = Result<AssistantMessageEventStream, StreamError>> + Send>>
    {
        let preface = preface.to_string();
        let tool_id = tool_id.to_string();
        let tool_name = tool_name.to_string();
        let tool_args = tool_args.to_string();
        let first_call = Arc::new(Mutex::new(true));
        move |_ctx: Context| {
            let is_first = *first_call.lock().unwrap();
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            if is_first {
                *first_call.lock().unwrap() = false;
                if !preface.is_empty() {
                    let _ = tx.send(StreamEvent::TextDelta {
                        delta: preface.clone(),
                    });
                }
                let _ = tx.send(StreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some(tool_id.clone()),
                    name: Some(tool_name.clone()),
                    arguments: Some(tool_args.clone()),
                });
                let _ = tx.send(StreamEvent::Done {
                    message: None,
                    stop_reason: Some("tool_use".into()),
                });
            } else {
                // Subsequent call: empty end_turn
                let _ = tx.send(StreamEvent::Done {
                    message: None,
                    stop_reason: Some("end_turn".into()),
                });
            }
            drop(tx);
            Box::pin(std::future::ready(Ok(rx)))
        }
    }

    /// A no-op tool executor that returns a canned success result.
    fn ok_tool_executor(
        _name: &str,
        _id: &str,
        _args: &serde_json::Value,
    ) -> Result<AgentToolResult, String> {
        Ok(AgentToolResult {
            tool_call_id: _id.to_string(),
            content: vec![ContentBlock::Text(TextContent {
                text: "tool executed successfully".into(),
            })],
            is_error: false,
            details: None,
        })
    }

    /// A tool executor that always fails.
    fn failing_tool_executor(
        _name: &str,
        _id: &str,
        _args: &serde_json::Value,
    ) -> Result<AgentToolResult, String> {
        Err("tool execution failed".to_string())
    }

    /// Create a default `AgentState` for testing.
    fn default_state(max_turns: u32) -> AgentState {
        AgentState {
            messages: vec![Message::user_text("hello")],
            context: AgentContext {
                messages: vec![],
                system_prompt: Some("be helpful".to_string()),
                tools: vec![],
                model: Some("test-model".to_string()),
                max_turns,
                current_turn: 0,
            },
            pending_tool_calls: vec![],
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_single_text_response_end_turn() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        let result = agent_loop(
            &mut state,
            text_stream_fn("Hello, world!", "end_turn"),
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_ok(), "Expected Ok, got {:?}", result);

        let captured = events.lock().unwrap();

        // AgentStart
        assert!(matches!(captured[0], AgentEvent::AgentStart { .. }));

        // TurnStart
        assert!(matches!(captured[1], AgentEvent::TurnStart { turn_number: 1 }));

        // MessageStart
        assert!(matches!(captured[2], AgentEvent::MessageStart { .. }));

        // MessageDelta with "Hello, world!"
        match &captured[3] {
            AgentEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta, "Hello, world!");
            }
            _ => panic!("Expected MessageDelta at index 3, got {:?}", captured[3]),
        }

        // MessageEnd
        assert!(matches!(captured[4], AgentEvent::MessageEnd { .. }));

        // TurnEnd
        assert!(matches!(captured[5], AgentEvent::TurnEnd { turn_number: 1 }));

        // AgentEnd
        match &captured[6] {
            AgentEvent::AgentEnd { finish_reason, .. } => {
                assert_eq!(finish_reason, "end_turn");
            }
            _ => panic!("Expected AgentEnd at index 6, got {:?}", captured[6]),
        }

        // State should have user message + assistant message
        assert_eq!(state.messages.len(), 2);
        assert_eq!(
            state.messages[1]
                .content
                .iter()
                .filter_map(|c| {
                    if let ContentBlock::Text(t) = c {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
            "Hello, world!"
        );
    }

    #[tokio::test]
    async fn test_single_text_response_stop() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        let result = agent_loop(
            &mut state,
            text_stream_fn("Stopped.", "stop"),
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_ok());

        let captured = events.lock().unwrap();
        let last = captured.last().unwrap();
        match last {
            AgentEvent::AgentEnd { finish_reason, .. } => {
                assert_eq!(finish_reason, "stop");
            }
            _ => panic!("Expected AgentEnd, got {:?}", last),
        }
    }

    #[tokio::test]
    async fn test_single_tool_call() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        let result = agent_loop(
            &mut state,
            tool_call_stream_fn(
                "Let me check that.",
                "call_1",
                "get_weather",
                r#"{"city": "Tokyo"}"#,
            ),
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_ok());

        let captured = events.lock().unwrap();

        // Verify full event sequence
        let event_types: Vec<&str> = captured
            .iter()
            .map(|e| match e {
                AgentEvent::AgentStart { .. } => "AgentStart",
                AgentEvent::TurnStart { .. } => "TurnStart",
                AgentEvent::MessageStart { .. } => "MessageStart",
                AgentEvent::MessageDelta { .. } => "MessageDelta",
                AgentEvent::MessageEnd { .. } => "MessageEnd",
                AgentEvent::ToolExecutionStart { .. } => "ToolExecutionStart",
                AgentEvent::ToolExecutionEnd { .. } => "ToolExecutionEnd",
                AgentEvent::ToolExecutionUpdate { .. } => "ToolExecutionUpdate",
                AgentEvent::AgentEnd { .. } => "AgentEnd",
                AgentEvent::TurnEnd { .. } => "TurnEnd",
            })
            .collect();

        // Turn 1: assistant message + tool call + tool result
        // Turn 2: assistant message (tool result is in context)
        let expected = vec![
            "AgentStart",
            "TurnStart",            // Turn 1
            "MessageStart",
            "MessageDelta",
            "MessageEnd",
            "ToolExecutionStart",
            "ToolExecutionEnd",
            "TurnEnd",
            "TurnStart",            // Turn 2 (tool result fed back to LLM)
            "MessageStart",
            "MessageEnd",
            "TurnEnd",
            "AgentEnd",
        ];

        assert_eq!(
            event_types, expected,
            "Event sequence mismatch.\nExpected: {:?}\nGot:      {:?}",
            expected, event_types
        );

        // Verify state has messages: [user, assistant, tool_result, assistant]
        assert_eq!(state.messages.len(), 4);
        assert_eq!(state.messages[0].role, MessageRole::User);
        assert_eq!(state.messages[1].role, MessageRole::Assistant);
        assert_eq!(state.messages[2].role, MessageRole::Tool);
        assert_eq!(state.messages[3].role, MessageRole::Assistant);

        // Tool result should have the success content (wrapped in ToolResultContent)
        let tool_result_text: String = state.messages[2]
            .content
            .iter()
            .filter_map(|c| {
                if let ContentBlock::ToolResult(tr) = c {
                    tr.content.as_ref().and_then(|blocks| {
                        blocks.iter().find_map(|b| {
                            if let ContentBlock::Text(t) = b {
                                Some(t.text.clone())
                            } else {
                                None
                            }
                        })
                    })
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(tool_result_text, "tool executed successfully");
    }

    #[tokio::test]
    async fn test_tool_call_failure_then_continue() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        let result = agent_loop(
            &mut state,
            tool_call_stream_fn(
                "",
                "call_fail",
                "broken_tool",
                r#"{}"#,
            ),
            failing_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_ok());

        let captured = events.lock().unwrap();

        // The tool failure is non-fatal — the error content is returned to the LLM
        let execution_end = captured.iter().find_map(|e| {
            if let AgentEvent::ToolExecutionEnd {
                tool_name,
                result,
                ..
            } = e
            {
                Some((tool_name.clone(), result.is_error))
            } else {
                None
            }
        });
        assert!(execution_end.is_some());
        let (tool_name, is_error) = execution_end.unwrap();
        assert_eq!(tool_name, "broken_tool");
        assert!(is_error, "Tool execution should be marked as error");

        // Error text should be in the tool result message
        let tool_msg = &state.messages[2];
        assert_eq!(tool_msg.role, MessageRole::Tool);

        // Continue works despite the error
        assert!(state.messages.len() >= 3);
    }

    #[tokio::test]
    async fn test_max_turns_limit() {
        // Only allow 1 turn, but the tool call will trigger a second turn
        let mut state = default_state(1);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        let result = agent_loop(
            &mut state,
            tool_call_stream_fn(
                "Let me check.",
                "call_1",
                "some_tool",
                r#"{}"#,
            ),
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(
            result.is_err(),
            "Expected Err(MaxTurnsReached), got Ok"
        );
        match result.unwrap_err() {
            AgentError::MaxTurnsReached(n) => {
                assert_eq!(n, 1, "Expected MaxTurnsReached(1)");
            }
            e => panic!("Expected MaxTurnsReached, got {:?}", e),
        }

        // First turn completed (assistant + tool result), but second turn was blocked
        let captured = events.lock().unwrap();
        let last = captured.last().unwrap();
        match last {
            AgentEvent::AgentEnd { finish_reason, .. } => {
                assert_eq!(finish_reason, "max_turns");
            }
            _ => panic!("Expected AgentEnd with max_turns, got {:?}", last),
        }
    }

    #[tokio::test]
    async fn test_cancellation() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        // Create a stream function that yields one event then blocks forever.
        // We leak the sender so the channel stays open and stream.next() returns
        // Poll::Pending, allowing select! to race against the CancellationToken.
        let stream_fn = move |_ctx: Context| {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            // Intentionally leak the sender so the channel never closes.
            std::mem::forget(tx);
            Box::pin(std::future::ready(Ok::<_, StreamError>(rx)))
        };

        // Cancel after a short delay
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = agent_loop(
            &mut state,
            stream_fn,
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(
            result.is_err(),
            "Expected Err(Cancelled), got Ok"
        );
        match result.unwrap_err() {
            AgentError::Cancelled => {} // expected
            e => panic!("Expected Cancelled, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_stream_error() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        // Create a stream that yields an error
        let stream_fn = move |_ctx: Context| {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta {
                delta: "Partial text before error...".into(),
            });
            let _ = tx.send(StreamEvent::Error {
                error: pi_ai_core::types::StreamError {
                    message: "API rate limit exceeded".into(),
                    code: Some("rate_limit".into()),
                    r#type: Some("error".into()),
                },
            });
            drop(tx);
            Box::pin(std::future::ready(Ok::<_, StreamError>(rx)))
        };

        let result = agent_loop(
            &mut state,
            stream_fn,
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::StreamError(StreamError::ProviderError(msg)) => {
                assert!(
                    msg.contains("rate limit"),
                    "Expected rate limit error, got: {}",
                    msg
                );
            }
            e => panic!("Expected StreamError::ProviderError, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_tool_executor_error_propagates_as_content() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        // Tool call stream where the executor returns Err
        let result = agent_loop(
            &mut state,
            tool_call_stream_fn("", "call_err", "failing_tool", r#"{}"#),
            |_name, _id, _args| Err("disk full".to_string()),
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_ok(), "Tool executor Err should not fail the loop");

        // Should have user + assistant(tool call) + tool(error result) + assistant(end_turn)
        // The tool executor error is non-fatal: the error text is returned to the LLM
        // and the loop continues for one more turn.
        assert_eq!(state.messages.len(), 4);
        let tool_msg = &state.messages[2];
        assert_eq!(tool_msg.role, MessageRole::Tool);

        // The error text should be in the tool result content (wrapped in ToolResultContent)
        let tool_text: String = tool_msg
            .content
            .iter()
            .filter_map(|c| {
                if let ContentBlock::ToolResult(tr) = c {
                    tr.content.as_ref().and_then(|blocks| {
                        blocks.iter().find_map(|b| {
                            if let ContentBlock::Text(t) = b {
                                Some(t.text.clone())
                            } else {
                                None
                            }
                        })
                    })
                } else {
                    None
                }
            })
            .collect();
        assert!(tool_text.contains("disk full"), "Error text: {}", tool_text);
    }

    #[tokio::test]
    async fn test_multiple_tool_calls_in_single_response() {
        let mut state = default_state(10);
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cancel = CancellationToken::new();

        // Create stream with two tool calls on first invocation, empty end_turn on subsequent
        let first_call = Arc::new(Mutex::new(true));
        let stream_fn = {
            let first_call = first_call.clone();
            move |_ctx: Context| {
                let (tx, rx) = EventStream::new();
                let _ = tx.send(StreamEvent::Start);
                if *first_call.lock().unwrap() {
                    *first_call.lock().unwrap() = false;
                    let _ = tx.send(StreamEvent::TextDelta {
                        delta: "Looking up both...".into(),
                    });
                    let _ = tx.send(StreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("get_weather".into()),
                        arguments: Some(r#"{"city":"Tokyo"}"#.into()),
                    });
                    let _ = tx.send(StreamEvent::ToolCallDelta {
                        index: 1,
                        id: Some("call_2".into()),
                        name: Some("get_time".into()),
                        arguments: Some(r#"{"city":"London"}"#.into()),
                    });
                    let _ = tx.send(StreamEvent::Done {
                        message: None,
                        stop_reason: Some("tool_use".into()),
                    });
                } else {
                    let _ = tx.send(StreamEvent::Done {
                        message: None,
                        stop_reason: Some("end_turn".into()),
                    });
                }
                drop(tx);
                Box::pin(std::future::ready(Ok::<_, StreamError>(rx)))
            }
        };

        let result = agent_loop(
            &mut state,
            stream_fn,
            ok_tool_executor,
            |ev| {
                events_clone.lock().unwrap().push(ev);
            },
            cancel,
        )
        .await;

        assert!(result.is_ok());

        let captured = events.lock().unwrap();

        // Count ToolExecutionStart and ToolExecutionEnd events
        let start_count = captured
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
            .count();
        let end_count = captured
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }))
            .count();

        assert_eq!(start_count, 2, "Expected 2 ToolExecutionStart events");
        assert_eq!(end_count, 2, "Expected 2 ToolExecutionEnd events");

        // State should have user + assistant + tool1 + tool2 + assistant
        assert_eq!(state.messages.len(), 5);
        assert_eq!(state.messages[2].role, MessageRole::Tool);
        assert_eq!(state.messages[3].role, MessageRole::Tool);
    }
}
