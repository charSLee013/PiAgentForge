//! Event stream primitive for push-based async iteration.
//! Mirrors packages/ai/src/utils/event-stream.ts

use crate::types::{StreamEvent, StreamResult};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio_stream::Stream;

/// A push-based async event stream.
///
/// Producer pushes events via `send`; consumer iterates via `Stream`.
/// Maps to the TypeScript `EventStream<T, R>` class.
pub struct EventStream<T> {
    rx: mpsc::UnboundedReceiver<T>,
}

impl<T> EventStream<T> {
    /// Create a new event stream, returning (sender, receiver).
    pub fn new() -> (EventStreamSender<T>, Self) {
        let (tx, rx) = mpsc::unbounded_channel();
        (EventStreamSender { tx }, Self { rx })
    }
}

impl<T> Stream for EventStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// The sending half of an event stream.
pub struct EventStreamSender<T> {
    tx: mpsc::UnboundedSender<T>,
}

impl<T> EventStreamSender<T> {
    /// Push an event into the stream.
    pub fn send(&self, event: T) -> Result<(), T> {
        self.tx.send(event).map_err(|e| e.0)
    }

    /// Check if the receiver has been dropped.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

impl<T> Clone for EventStreamSender<T> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone() }
    }
}

/// An `EventStream` specialized for assistant message streaming.
///
/// Produces `StreamEvent` items and yields a final `StreamResult`.
pub type AssistantMessageEventStream = EventStream<StreamEvent>;

impl AssistantMessageEventStream {
    /// Create a new assistant message event stream.
    pub fn new_assistant() -> (EventStreamSender<StreamEvent>, Self) {
        Self::new()
    }
}

/// Collect all events from a stream into a final `StreamResult`.
///
/// This is the Rust equivalent of `for await...of` in TS `agent-loop.ts`.
pub async fn collect_stream(
    mut stream: EventStream<StreamEvent>,
    model: &crate::types::Model,
) -> Result<StreamResult, String> {
    use tokio_stream::StreamExt;

    let mut message = None;
    let mut stop_reason = None;
    let mut error_msg = None;
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Start => {}
            StreamEvent::TextDelta { delta } => text_parts.push(delta),
            StreamEvent::ThinkingDelta { delta } => thinking_parts.push(delta),
            StreamEvent::ToolCallDelta { .. } => {
                // TODO: tool call accumulation when tools are implemented
            }
            StreamEvent::Usage(_usage) => {
                // TODO: accumulate usage
            }
            StreamEvent::Done { message: msg, stop_reason: reason } => {
                message = msg;
                stop_reason = reason;
            }
            StreamEvent::Error { error } => {
                error_msg = Some(error.message);
            }
        }
    }

    if let Some(err) = error_msg {
        return Err(err);
    }

    let content = crate::types::ContentBlock::Text(crate::types::TextContent { text: text_parts.concat() });

    let msg = message.unwrap_or_else(|| crate::types::Message::assistant(vec![content]));

    Ok(StreamResult {
        message: msg,
        usage: crate::types::Usage { input: 0, output: 0, cache_read: None, cache_write: None, total_tokens: None },
        stop_reason,
        api: model.api.clone(),
        provider: model.provider,
        model: model.id.clone(),
        error_message: None,
        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
            as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn test_event_stream_basic() {
        let (tx, mut rx) = EventStream::<i32>::new();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);

        let mut results = Vec::new();
        while let Some(val) = rx.next().await {
            results.push(val);
        }
        assert_eq!(results, vec![1, 2]);
    }

    #[tokio::test]
    async fn test_stream_closed() {
        let (tx, rx) = EventStream::<i32>::new();
        assert!(!tx.is_closed());
        drop(rx);
        // Small yield to allow drop to propagate
        tokio::task::yield_now().await;
    }
}
