//! Provider API registry.
//! Mirrors packages/ai/src/api-registry.ts

use crate::event_stream::AssistantMessageEventStream;
use crate::types::{ApiId, Context, Model, StreamOptions};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::RwLock;

/// A registered API provider that can stream LLM responses.
pub trait ApiProvider: Send + Sync {
    /// The unique identifier for this provider (e.g., "anthropic-messages").
    fn api_id(&self) -> &str;

    /// Stream a response from the given model with the given context and options.
    fn stream(&self, model: &Model, context: Context, options: StreamOptions) -> AssistantMessageEventStream;
}

type ProviderMap = HashMap<ApiId, Box<dyn ApiProvider>>;

static API_PROVIDER_REGISTRY: OnceLock<RwLock<ProviderMap>> = OnceLock::new();

fn registry() -> &'static RwLock<ProviderMap> {
    API_PROVIDER_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register an API provider.
pub async fn register_api_provider(provider: Box<dyn ApiProvider>) {
    let id = provider.api_id().to_string();
    registry().write().await.insert(id, provider);
}

/// Get a reference to a provider by API ID and call a function on it.
pub async fn with_provider<F, R>(api_id: &str, f: F) -> Option<R>
where
    F: FnOnce(&dyn ApiProvider) -> R,
{
    let guard = registry().read().await;
    guard.get(api_id).map(|p| f(p.as_ref()))
}

/// Check if a provider is registered.
pub async fn has_api_provider(api_id: &str) -> bool {
    registry().read().await.contains_key(api_id)
}

/// Clear all registered providers (for testing).
pub async fn clear_api_providers() {
    registry().write().await.clear();
}

/// List all registered API IDs.
pub async fn list_api_providers() -> Vec<ApiId> {
    registry().read().await.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Model;

    struct TestProvider;

    impl ApiProvider for TestProvider {
        fn api_id(&self) -> &str {
            "test-provider"
        }

        fn stream(&self, _model: &Model, _context: Context, _options: StreamOptions) -> AssistantMessageEventStream {
            let (tx, rx) = AssistantMessageEventStream::new();
            drop(tx);
            rx
        }
    }

    #[tokio::test]
    async fn test_register_and_list() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let providers = list_api_providers().await;
        assert!(providers.contains(&"test-provider".to_string()));
        assert!(has_api_provider("test-provider").await);
    }

    #[tokio::test]
    async fn test_with_provider() {
        clear_api_providers().await;
        register_api_provider(Box::new(TestProvider)).await;

        let id = with_provider("test-provider", |p| p.api_id().to_string()).await;
        assert_eq!(id, Some("test-provider".to_string()));

        let missing = with_provider("nonexistent", |p| p.api_id().to_string()).await;
        assert!(missing.is_none());
    }
}
