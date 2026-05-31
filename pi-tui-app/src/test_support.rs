use std::path::PathBuf;
use std::time::Duration;

use pi_agent_core::session::types::create_session_id;
use pi_ai_core::api_registry::ApiProvider;
use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStream};
use pi_ai_core::types::{ContentBlock, Context, KnownProvider, MessageRole, Model, StreamEvent, StreamOptions};

use crate::InteractiveMode;

pub struct DelayedEchoProvider {
    pub api_id: &'static str,
    pub delay_ms: u64,
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

pub struct StaticPlanProvider {
    pub api_id: &'static str,
}

impl ApiProvider for StaticPlanProvider {
    fn api_id(&self) -> &str {
        self.api_id
    }

    fn stream(&self, _model: &Model, _context: Context, _options: StreamOptions) -> AssistantMessageEventStream {
        let (tx, rx) = EventStream::new();
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta {
                delta: "Plan:\n1. Inspect the repository.\n2. Summarize the result.".to_string(),
            });
            let _ = tx.send(StreamEvent::Done { message: None, stop_reason: Some("end_turn".to_string()) });
        });
        rx
    }
}

pub fn static_plan_model() -> &'static Model {
    Box::leak(Box::new(Model {
        id: "tui-plan-model".to_string(),
        provider: KnownProvider::Faux,
        api: "tui-plan-stream".to_string(),
        name: Some("TUI Plan Test".to_string()),
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

pub fn extract_text_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn unique_test_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}", create_session_id()))
}

pub fn delayed_echo_model() -> &'static Model {
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

pub fn create_im_for_runtime_model(model: &'static Model) -> InteractiveMode {
    tokio::runtime::Runtime::new()
        .expect("failed to create tokio runtime for test")
        .block_on(InteractiveMode::new(&model.id, model, None, None, None, std::env::temp_dir()))
        .expect("InteractiveMode::new() should succeed")
}

pub fn wait_for_background_run(rt: &tokio::runtime::Runtime, im: &mut InteractiveMode) {
    let finished = rt.block_on(async {
        for _ in 0..100 {
            let _ = im.poll_background_run_for_test().await;
            if !im.is_streaming_for_test() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    });
    assert!(finished, "background run should finish");
}
