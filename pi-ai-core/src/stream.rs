//! Streaming entry points for the pi AI core.
//!
//! This module provides the main public API for streaming and completing
//! LLM responses. Mirrors packages/ai/src/stream.ts

use crate::api_registry::with_provider;
use crate::event_stream::{collect_stream, AssistantMessageEventStream};
use crate::types::{Context, Model, StreamOptions, StreamResult};
use thiserror::Error;

/// Errors that can occur during streaming.
#[derive(Debug, Error)]
pub enum StreamError {
    /// No API provider is registered for the given API ID.
    #[error("No API provider registered for: {0}")]
    NoProvider(String),

    /// The provider returned an error.
    #[error("Provider error: {0}")]
    ProviderError(String),

    /// The stream ended without a completion signal.
    #[error("Stream ended without completion")]
    IncompleteStream,
}

/// Stream a response from the given model with the given context and options.
///
/// Returns an `AssistantMessageEventStream` that yields `StreamEvent` items.
/// This is the Rust equivalent of the TypeScript `stream()` function.
pub async fn stream(
    model: &Model,
    context: Context,
    options: StreamOptions,
) -> Result<AssistantMessageEventStream, StreamError> {
    let api_id = &model.api;

    let result = with_provider(api_id, |provider| {
        provider.stream(model, context, options)
    }).await;

    result.ok_or_else(|| StreamError::NoProvider(api_id.clone()))
}

/// Stream a response and collect all events into a final `StreamResult`.
///
/// This is the Rust equivalent of the TypeScript `complete()` function.
pub async fn complete(
    model: &Model,
    context: Context,
    options: StreamOptions,
) -> Result<StreamResult, StreamError> {
    let event_stream = stream(model, context, options).await?;
    collect_stream(event_stream, model).await.map_err(StreamError::ProviderError)
}

/// Stream a response using the simple (default-options) interface.
///
/// Mirrors `streamSimple` in TypeScript.
pub async fn stream_simple(
    model: &Model,
    context: Context,
) -> Result<AssistantMessageEventStream, StreamError> {
    stream(model, context, StreamOptions::default()).await
}

/// Complete a response using the simple (default-options) interface.
///
/// Mirrors `completeSimple` in TypeScript.
pub async fn complete_simple(
    model: &Model,
    context: Context,
) -> Result<StreamResult, StreamError> {
    complete(model, context, StreamOptions::default()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_registry::{clear_api_providers, register_api_provider, ApiProvider};
    use crate::event_stream::EventStream;
    use crate::types::{KnownProvider, Message, StreamEvent};

    /// A test provider that returns a simple canned stream.
    struct TestProvider;

    impl ApiProvider for TestProvider {
        fn api_id(&self) -> &str {
            "test-stream"
        }

        fn stream(&self, _model: &Model, _context: Context, _options: StreamOptions) -> AssistantMessageEventStream {
            let (tx, rx) = EventStream::new();
            let _ = tx.send(StreamEvent::Start);
            let _ = tx.send(StreamEvent::TextDelta {
                delta: "Hello, world!".into(),
            });
            let _ = tx.send(StreamEvent::Done {
                message: None,
                stop_reason: Some("end_turn".into()),
            });
            drop(tx);
            rx
        }
    }

    fn test_model(api: &str) -> Model {
        Model {
            id: "test-model".into(),
            provider: KnownProvider::Faux,
            api: api.into(),
            name: None,
            base_url: None,
            supports_thinking: false,
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

    fn test_context() -> Context {
        Context {
            messages: vec![Message::user_text("hello")],
            system_prompt: None,
            model: None,
            tools: vec![],
        }
    }

    #[tokio::test]
    async fn test_stream_returns_no_provider_error() {
        let model = test_model("nonexistent");
        let result = stream(&model, test_context(), StreamOptions::default()).await;
        match result {
            Err(StreamError::NoProvider(api)) => assert_eq!(api, "nonexistent"),
            _other => panic!("expected Err(NoProvider), got an Ok result"),
        }
    }

    #[tokio::test]
    async fn test_stream_returns_stream_for_registered_provider() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let model = test_model("test-stream");
        let result = stream(&model, test_context(), StreamOptions::default()).await;
        assert!(result.is_ok(), "expected Ok stream, got Err");
    }

    #[tokio::test]
    async fn test_complete_returns_result() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let model = test_model("test-stream");
        let result = complete(&model, test_context(), StreamOptions::default()).await;
        assert!(result.is_ok(), "expected Ok StreamResult, got {result:?}");

        let stream_result = result.unwrap();
        assert_eq!(stream_result.stop_reason, Some("end_turn".to_string()));
    }

    #[tokio::test]
    async fn test_stream_simple_delegates_to_stream() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let model = test_model("test-stream");
        let result = stream_simple(&model, test_context()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_complete_simple_delegates_to_complete() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let model = test_model("test-stream");
        let result = complete_simple(&model, test_context()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stream_options_passthrough() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let model = test_model("test-stream");
        let opts = StreamOptions {
            timeout: Some(30),
            max_tokens: Some(1024),
            thinking: Some(true),
            api_key: Some("test-key".into()),
        };
        let result = stream(&model, test_context(), opts).await;
        assert!(result.is_ok());
    }
}
