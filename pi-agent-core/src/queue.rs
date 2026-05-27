//! Message queues for agent steering and follow-up messages.
//!
//! Two independent queues let callers inject messages into the agent loop:
//!
//! - **Steering queue**: messages are injected *during* a run (mid-turn).
//! - **Follow-up queue**: messages are processed *after* the agent would
//!   otherwise stop.
//!
//! Each queue has a [`QueueMode`] that controls how many messages are drained
//! at each poll point, and each enqueued message carries a [`QueuePriority`]
//! that determines ordering within the drain batch.

use pi_ai_core::types::Message;
use std::collections::VecDeque;

/// Controls how many queued user messages are injected at a drain point.
///
/// Mirrors `QueueMode` in the TS types (packages/agent/src/types.ts:44).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    /// Drain and inject every queued message at that point.
    All,
    /// Drain and inject only the oldest message, leaving the rest queued.
    OneAtATime,
}

/// Priority of a queued message.
///
/// High-priority messages are drained before normal ones. This prevents
/// priority inversion when a hook (e.g. `beforeToolCall`) enqueues a
/// steering message while the agent is executing tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QueuePriority {
    /// Normal user or follow-up message.
    Normal,
    /// High-priority (steering injected from hooks).
    High,
}

/// A FIFO queue for agent messages with configurable drain mode and priority.
///
/// Mirrors `PendingMessageQueue` in the TS types
/// (packages/agent/src/agent.ts:118-149).
#[derive(Debug, Clone)]
pub struct MessageQueue {
    mode: QueueMode,
    messages: VecDeque<(Message, QueuePriority)>,
}

impl MessageQueue {
    /// Create a new queue with the given drain mode.
    pub fn new(mode: QueueMode) -> Self {
        Self {
            mode,
            messages: VecDeque::new(),
        }
    }

    /// Enqueue a message with normal priority.
    pub fn enqueue(&mut self, message: Message) {
        self.messages.push_back((message, QueuePriority::Normal));
    }

    /// Enqueue a message with the given priority.
    pub fn enqueue_with_priority(&mut self, message: Message, priority: QueuePriority) {
        self.messages.push_back((message, priority));
    }

    /// Drain messages according to the current mode.
    ///
    /// - `All`: returns all messages, sorted by priority (High first).
    /// - `OneAtATime`: returns the oldest message (respecting priority).
    pub fn drain(&mut self) -> Vec<Message> {
        if self.messages.is_empty() {
            return vec![];
        }

        match self.mode {
            QueueMode::All => {
                // Sort: high priority first
                let mut drained: Vec<(Message, QueuePriority)> = self.messages.drain(..).collect();
                drained.sort_by_key(|(_, p)| std::cmp::Reverse(*p));
                self.messages.clear();
                drained.into_iter().map(|(m, _)| m).collect()
            }
            QueueMode::OneAtATime => {
                // Find the first high-priority message, or take the front
                let high_idx = self.messages.iter().position(|(_, p)| *p == QueuePriority::High);
                let idx = high_idx.unwrap_or(0);
                let (msg, _) = self.messages.remove(idx).expect("non-empty checked above");
                vec![msg]
            }
        }
    }

    /// Returns true when the queue has at least one message.
    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    /// Remove all messages.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Number of messages in the queue.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// The current drain mode.
    pub fn mode(&self) -> QueueMode {
        self.mode
    }

    /// Change the drain mode at runtime.
    pub fn set_mode(&mut self, mode: QueueMode) {
        self.mode = mode;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai_core::types::ContentBlock;

    fn msg(text: &str) -> Message {
        Message::user_text(text)
    }

    fn assert_text(msg: &Message, expected: &str) {
        if let Some(ContentBlock::Text(t)) = msg.content.first() {
            assert_eq!(t.text, expected);
        } else {
            panic!("Expected Text content block, got {:?}", msg.content.first());
        }
    }

    #[test]
    fn test_enqueue_drain_all() {
        let mut q = MessageQueue::new(QueueMode::All);
        q.enqueue(msg("a"));
        q.enqueue(msg("b"));
        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert!(!q.has_items());
    }

    #[test]
    fn test_enqueue_drain_one_at_a_time() {
        let mut q = MessageQueue::new(QueueMode::OneAtATime);
        q.enqueue(msg("a"));
        q.enqueue(msg("b"));
        let first = q.drain();
        assert_eq!(first.len(), 1);
        assert_text(&first[0], "a");
        assert!(q.has_items());
    }

    #[test]
    fn test_high_priority_drained_first() {
        let mut q = MessageQueue::new(QueueMode::All);
        q.enqueue(msg("normal"));
        q.enqueue_with_priority(msg("urgent"), QueuePriority::High);
        let drained = q.drain();
        assert_text(&drained[0], "urgent");
    }

    #[test]
    fn test_high_priority_one_at_a_time() {
        let mut q = MessageQueue::new(QueueMode::OneAtATime);
        q.enqueue(msg("first"));
        q.enqueue_with_priority(msg("urgent"), QueuePriority::High);
        // High priority should be drained first, even though "first" was enqueued earlier
        let first = q.drain();
        assert_text(&first[0], "urgent");
    }

    #[test]
    fn test_clear() {
        let mut q = MessageQueue::new(QueueMode::All);
        q.enqueue(msg("a"));
        q.clear();
        assert!(!q.has_items());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_empty_drain() {
        let mut q = MessageQueue::new(QueueMode::All);
        assert!(q.drain().is_empty());
    }

    #[test]
    fn test_len() {
        let mut q = MessageQueue::new(QueueMode::All);
        assert_eq!(q.len(), 0);
        q.enqueue(msg("a"));
        assert_eq!(q.len(), 1);
        q.enqueue(msg("b"));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_set_mode_runtime() {
        let mut q = MessageQueue::new(QueueMode::OneAtATime);
        q.enqueue(msg("a"));
        q.enqueue(msg("b"));
        q.set_mode(QueueMode::All);
        let drained = q.drain();
        assert_eq!(drained.len(), 2);
    }
}
