#![allow(dead_code)]

use pi_agent_core::session::types::SessionEntry;
use pi_ai_core::api_registry::{clear_api_providers, register_api_provider};
use pi_ai_core::types::Message;
use pi_tui_app::test_support::{
    DelayedEchoProvider, StaticPlanProvider, create_im_for_runtime_model, delayed_echo_model, extract_text_from_blocks,
    static_plan_model, unique_test_dir, wait_for_background_run,
};

pub fn setup_provider(rt: &tokio::runtime::Runtime, delay_ms: u64) {
    rt.block_on(async {
        clear_api_providers().await;
        register_api_provider(Box::new(DelayedEchoProvider { api_id: "tui-test-stream", delay_ms })).await;
    });
}

pub fn setup_plan_provider(rt: &tokio::runtime::Runtime) {
    rt.block_on(async {
        clear_api_providers().await;
        register_api_provider(Box::new(StaticPlanProvider { api_id: "tui-plan-stream" })).await;
    });
}

pub fn collect_texts(messages: Vec<Message>) -> Vec<String> {
    messages
        .into_iter()
        .map(|message| extract_text_from_blocks(&message.content))
        .filter(|text| !text.is_empty())
        .collect()
}

pub fn spin_until_idle(rt: &tokio::runtime::Runtime, im: &mut pi_tui_app::InteractiveMode) {
    wait_for_background_run(rt, im);
}

pub fn delayed_model() -> &'static pi_ai_core::types::Model {
    delayed_echo_model()
}

pub fn plan_model() -> &'static pi_ai_core::types::Model {
    static_plan_model()
}

pub fn create_runtime_im() -> pi_tui_app::InteractiveMode {
    create_im_for_runtime_model(delayed_echo_model())
}

pub fn unique_dir(prefix: &str) -> std::path::PathBuf {
    unique_test_dir(prefix)
}

pub fn has_compaction(entries: &[SessionEntry]) -> bool {
    entries.iter().any(|entry| matches!(entry, SessionEntry::Compaction(_)))
}
