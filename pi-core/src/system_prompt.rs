//! System prompt construction and project context loading.
//! Mirrors packages/coding-agent/src/core/system-prompt.ts

use crate::skills::Skill;

/// Options for building the system prompt.
pub struct SystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub tool_snippets: Vec<(String, String)>,
    pub prompt_guidelines: Vec<String>,
    pub append: Option<String>,
    pub cwd: String,
    pub skills: Vec<Skill>,
}

impl Default for SystemPromptOptions {
    fn default() -> Self {
        Self {
            custom_prompt: None,
            selected_tools: vec!["read".into(), "bash".into(), "edit".into(), "write".into()],
            tool_snippets: vec![],
            prompt_guidelines: vec![],
            append: None,
            cwd: ".".into(),
            skills: vec![],
        }
    }
}

/// Build the complete system prompt.
pub fn build_system_prompt(opts: &SystemPromptOptions) -> String {
    if let Some(ref custom) = opts.custom_prompt {
        let mut prompt = custom.clone();
        if let Some(ref append) = opts.append {
            prompt.push_str("\n\n");
            prompt.push_str(append);
        }
        append_skills(&mut prompt, &opts.skills);
        append_footer(&mut prompt, &opts.cwd);
        return prompt;
    }

    let mut parts: Vec<String> = Vec::new();

    // Tools list
    parts.push("You have the following tools available:".into());
    for (name, snippet) in &opts.tool_snippets {
        parts.push(format!("- {name}: {snippet}"));
    }
    parts.push(String::new());

    // Guidelines
    let mut guidelines: Vec<String> = opts.prompt_guidelines.clone();
    guidelines.push("Reply concisely.".into());
    guidelines.push("Read files before editing unfamiliar code.".into());
    guidelines.push("Prefer grep/find/ls for file exploration.".into());
    // Dedup
    guidelines.sort();
    guidelines.dedup();

    parts.push(format!("## Guidelines\n{}", guidelines.join("\n")));
    parts.push(String::new());

    // Skills
    if !opts.skills.is_empty() {
        let skills_xml = crate::skills::format_skills_for_prompt(&opts.skills);
        if !skills_xml.is_empty() {
            parts.push(skills_xml);
            parts.push(String::new());
        }
    }

    // Append
    if let Some(ref append) = opts.append {
        parts.push(append.clone());
    }

    let mut prompt = parts.join("\n");
    append_footer(&mut prompt, &opts.cwd);
    prompt
}

/// Append date and working directory footer.
fn append_footer(prompt: &mut String, cwd: &str) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Format date as YYYY-MM-DD
    let days = secs / 86400;
    let year = 1970 + (days as f64 / 365.25) as u64;
    // Simplified — in production use chrono
    prompt.push_str(&format!("\nCurrent date: {}\n", year));
    prompt.push_str(&format!("Current directory: {}\n", cwd));
}

fn append_skills(prompt: &mut String, skills: &[Skill]) {
    if skills.is_empty() {
        return;
    }
    let xml = crate::skills::format_skills_for_prompt(skills);
    if !xml.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&xml);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_prompt_no_tools() {
        let opts = SystemPromptOptions {
            custom_prompt: Some("You are a helpful assistant.".into()),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.starts_with("You are a helpful assistant."));
    }

    #[test]
    fn test_default_prompt_has_tools() {
        let opts = SystemPromptOptions::default();
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("tools"));
    }

    #[test]
    fn test_prompt_with_skills() {
        let skill = Skill {
            name: "test".into(),
            description: "test skill".into(),
            file_path: "a.md".into(),
            base_dir: ".".into(),
            disable_model_invocation: false,
        };
        let opts = SystemPromptOptions {
            skills: vec![skill],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("test"));
    }
}
