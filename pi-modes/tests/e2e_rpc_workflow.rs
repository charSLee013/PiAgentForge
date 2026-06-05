use std::path::Path;
use std::time::Duration;

use pi_ai_core::api_registry::{ApiProvider, clear_api_providers, register_api_provider};
use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStream};
use pi_ai_core::types::{
    ContentBlock, Context, KnownProvider, Message, MessageRole, Model, StreamEvent, StreamOptions,
};
use pi_modes::rpc::runtime::RpcRuntime;
use pi_modes::rpc::types::RpcCommand;

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
        let (tx, rx) = EventStream::new();
        let delay_ms = self.delay_ms;
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

#[tokio::test]
async fn e2e_rpc_workflow_smoke() {
    clear_api_providers().await;
    register_api_provider(Box::new(DelayedEchoProvider { api_id: "rpc-test-stream", delay_ms: 50 })).await;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rpc-session.jsonl");
    let runtime = RpcRuntime::from_config_for_test(test_model(), dir.path().to_path_buf(), Some(path.clone())).await;

    assert!(
        runtime
            .handle_command_for_test(RpcCommand::Prompt {
                id: None,
                message: "first".to_string(),
                images: None,
                streaming_behavior: None,
            })
            .await
            .success
    );
    assert!(
        runtime
            .handle_command_for_test(RpcCommand::Steer { id: None, message: "steer next".to_string(), images: None })
            .await
            .success
    );
    assert!(
        runtime
            .handle_command_for_test(RpcCommand::FollowUp {
                id: None,
                message: "follow later".to_string(),
                images: None
            })
            .await
            .success
    );
    assert!(runtime.wait_for_idle_for_test(Duration::from_secs(2)).await);

    assert!(
        runtime
            .handle_command_for_test(RpcCommand::SetSessionName { id: None, name: "Named Session".to_string() })
            .await
            .success
    );

    let export = runtime.handle_command_for_test(RpcCommand::ExportHtml { id: None, output_path: None }).await;
    assert!(export.success);
    let export_path = export.data.unwrap()["path"].as_str().unwrap().to_string();
    assert!(Path::new(&export_path).exists());

    let messages_response = runtime.handle_command_for_test(RpcCommand::GetMessages { id: None }).await;
    let messages = serde_json::from_value::<Vec<Message>>(messages_response.data.unwrap()["messages"].clone()).unwrap();
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

    let clone = runtime.handle_command_for_test(RpcCommand::Clone { id: None }).await;
    assert!(clone.success);

    let fork_messages = runtime.handle_command_for_test(RpcCommand::GetForkMessages { id: None }).await;
    let first_entry_id = fork_messages.data.unwrap()["messages"][0]["entryId"].as_str().unwrap().to_string();
    let fork = runtime.handle_command_for_test(RpcCommand::Fork { id: None, entry_id: first_entry_id }).await;
    assert!(fork.success);

    for i in 0..6 {
        let message = format!("message {i} {}", "x".repeat(400));
        assert!(
            runtime
                .handle_command_for_test(RpcCommand::Prompt {
                    id: None,
                    message,
                    images: None,
                    streaming_behavior: None,
                })
                .await
                .success
        );
        assert!(runtime.wait_for_idle_for_test(Duration::from_secs(2)).await);
    }

    let compact = runtime
        .handle_command_for_test(RpcCommand::Compact {
            id: None,
            custom_instructions: Some("Keep the summary terse".to_string()),
        })
        .await;
    assert!(compact.success, "{:?}", compact.error);

    let retry = runtime.handle_command_for_test(RpcCommand::SetAutoRetry { id: None, enabled: true }).await;
    assert!(retry.success, "placeholder command should stay visible until implemented");
}
