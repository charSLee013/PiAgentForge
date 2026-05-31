//! Test utilities for mocking LLM streams.
//!
//! Provides factory functions that create mock stream functions for use in
//! unit tests without real API keys or network access.
//!
//! # Usage
//!
//! ```ignore
//! use pi_ai_core::test_utils::mock_stream_fixed;
//!
//! let stream_fn = mock_stream_fixed("Hello, world!", "end_turn");
//! let stream = stream_fn(context).await.unwrap();
//! ```

use crate::event_stream::EventStream;
use crate::stream::StreamError;
use crate::types::{self, Context, StreamEvent};
use std::future::Future;
use std::pin::Pin;

/// The signature required by the agent loop's `stream_fn` parameter.
type BoxedStreamFn = Box<
    dyn Fn(Context) -> Pin<Box<dyn Future<Output = Result<EventStream<StreamEvent>, StreamError>> + Send>>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Mock factories
// ---------------------------------------------------------------------------

/// Mock stream that produces a single text response.
///
/// Emits: `Start → TextDelta(text) → Done(stop_reason)`
pub fn mock_stream_fixed(text: &str, stop_reason: &str) -> BoxedStreamFn {
    let text = text.to_string();
    let stop_reason = stop_reason.to_string();
    Box::new(move |_ctx: Context| {
        let text = text.clone();
        let stop_reason = stop_reason.clone();
        Box::pin(async move {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta { delta: text });
            let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some(stop_reason) });
            drop(tx);
            Ok(rx)
        })
    })
}

/// Mock stream that produces tool-calling responses.
///
/// First call: emits `ToolCallDelta` for each tool, then `Done("tool_use")`.
/// Subsequent calls: emits `Done("end_turn")` (empty summary turn).
pub fn mock_stream_tool_calls(tools: Vec<(&str, &str)>) -> BoxedStreamFn {
    let tools: Vec<(String, String)> = tools.into_iter().map(|(n, a)| (n.to_string(), a.to_string())).collect();
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    Box::new(move |_ctx: Context| {
        let tools = tools.clone();
        let call_count = call_count.clone();
        Box::pin(async move {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);

            let count = call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                for (i, (name, args)) in tools.iter().enumerate() {
                    let _ = tx.send(StreamEvent::ToolCallDelta {
                        index: i as u32,
                        id: Some(format!("call_{}", i)),
                        name: Some(name.clone()),
                        arguments: Some(args.clone()),
                    });
                }
                let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some("tool_use".to_string()) });
            } else {
                let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some("end_turn".to_string()) });
            }

            drop(tx);
            Ok(rx)
        })
    })
}

/// Mock stream with a configurable delay before emitting.
pub fn mock_stream_delayed(text: &str, stop_reason: &str, delay_ms: u64) -> BoxedStreamFn {
    let text = text.to_string();
    let stop_reason = stop_reason.to_string();
    Box::new(move |_ctx: Context| {
        let text = text.clone();
        let stop_reason = stop_reason.clone();
        Box::pin(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta { delta: text });
            let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some(stop_reason) });
            drop(tx);
            Ok(rx)
        })
    })
}

/// Mock stream that emits an error event.
pub fn mock_stream_error(error_msg: &str) -> BoxedStreamFn {
    let error_msg = error_msg.to_string();
    Box::new(move |_ctx: Context| {
        let error_msg = error_msg.clone();
        Box::pin(async move {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Error {
                error: types::StreamError { message: error_msg, code: None, r#type: None },
            });
            drop(tx);
            Ok(rx)
        })
    })
}

/// Mock stream that inspects the context it receives.
pub fn mock_stream_inspect<F>(inspector: F) -> BoxedStreamFn
where
    F: Fn(&Context) -> (String, String) + Send + Sync + 'static,
{
    Box::new(move |ctx: Context| {
        let (text, stop_reason) = inspector(&ctx);
        Box::pin(async move {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta { delta: text });
            let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some(stop_reason) });
            drop(tx);
            Ok(rx)
        })
    })
}

/// Common stop reason constants.
pub mod stop_reasons {
    pub const END_TURN: &str = "end_turn";
    pub const STOP: &str = "stop";
    pub const TOOL_USE: &str = "tool_use";
    pub const ERROR: &str = "error";
    pub const ABORTED: &str = "aborted";
    pub const MAX_TURNS: &str = "max_turns";
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    fn dummy_context() -> Context {
        Context { messages: vec![], system_prompt: None, model: None, tools: vec![] }
    }

    #[tokio::test]
    async fn test_mock_stream_fixed() {
        let stream_fn = mock_stream_fixed("hello", "end_turn");
        let mut stream: EventStream<StreamEvent> = stream_fn(dummy_context()).await.unwrap();

        let mut events = vec![];
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }

        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::Start));
        assert!(matches!(&events[1], StreamEvent::TextDelta { delta } if delta == "hello"));
        assert!(
            matches!(&events[2], StreamEvent::Done { stop_reason, .. } if stop_reason.as_deref() == Some("end_turn"))
        );
    }

    #[tokio::test]
    async fn test_mock_stream_tool_calls() {
        let stream_fn = mock_stream_tool_calls(vec![("read", r#"{"path":"x"}"#)]);
        let stream: EventStream<StreamEvent> = stream_fn(dummy_context()).await.unwrap();

        let events: Vec<_> = stream.collect().await;
        assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolCallDelta { .. })));
    }

    #[tokio::test]
    async fn test_mock_stream_error() {
        let stream_fn = mock_stream_error("rate_limit");
        let stream: EventStream<StreamEvent> = stream_fn(dummy_context()).await.unwrap();

        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Error { error } if error.message.contains("rate_limit")));
    }

    #[tokio::test]
    async fn test_mock_stream_delayed() {
        let stream_fn = mock_stream_delayed("hi", "end_turn", 50);
        let start = std::time::Instant::now();
        let mut stream: EventStream<StreamEvent> = stream_fn(dummy_context()).await.unwrap();
        while stream.next().await.is_some() {}
        let elapsed = start.elapsed();
        assert!(elapsed >= std::time::Duration::from_millis(40));
    }

    #[tokio::test]
    async fn test_mock_stream_inspect_receives_context() {
        let stream_fn = mock_stream_inspect(|ctx| {
            assert!(ctx.messages.is_empty());
            ("inspected".to_string(), "end_turn".to_string())
        });
        let mut stream: EventStream<StreamEvent> = stream_fn(dummy_context()).await.unwrap();
        while stream.next().await.is_some() {}
    }

    #[tokio::test]
    async fn test_mock_stream_second_tool_call_is_empty() {
        let stream_fn = mock_stream_tool_calls(vec![("bash", r#"{"cmd":"ls"}"#)]);
        let ctx = dummy_context();

        // First call: tool use
        let s1: EventStream<StreamEvent> = stream_fn(ctx.clone()).await.unwrap();
        let _e1: Vec<_> = s1.collect().await;

        // Second call: empty summary
        let s2: EventStream<StreamEvent> = stream_fn(ctx).await.unwrap();
        let e2: Vec<_> = s2.collect().await;
        assert!(!e2.iter().any(|e| matches!(e, StreamEvent::ToolCallDelta { .. })));
    }
}
