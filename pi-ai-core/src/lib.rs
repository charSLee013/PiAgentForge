pub mod api_registry;
pub mod event_stream;
pub mod stream;
pub mod thinking;
pub mod types;

/// Test utilities for mocking LLM streams.
///
/// Only available in test builds (behind `#[cfg(test)]`).
///
/// Public so that downstream crates (e.g. `pi-agent-core`) can use the
/// mock factories in their own tests.
#[cfg(feature = "test-utils")]
pub mod test_utils;
