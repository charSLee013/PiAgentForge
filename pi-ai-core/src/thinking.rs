use crate::types::Model;

pub const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];

pub fn is_valid_thinking_level(level: &str) -> bool {
    THINKING_LEVELS.contains(&level)
}

pub fn supported_thinking_levels(model: &Model) -> &'static [&'static str] {
    if model.supports_thinking { THINKING_LEVELS } else { &THINKING_LEVELS[..1] }
}

pub fn default_thinking_level_for_model(model: &Model) -> &'static str {
    if model.supports_thinking { "low" } else { "off" }
}

pub fn clamp_thinking_level(model: &Model, level: &str) -> String {
    let supported = supported_thinking_levels(model);
    if supported.contains(&level) { level.to_string() } else { supported.first().copied().unwrap_or("off").to_string() }
}

pub fn thinking_enabled(level: &str) -> bool {
    level != "off"
}
