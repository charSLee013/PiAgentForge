//! Skills system — load and format skills from markdown files with YAML frontmatter.
//!
//! Mirrors packages/coding-agent/src/core/skills.ts

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Maximum allowed name length (matching TS spec).
const MAX_NAME_LENGTH: usize = 64;
/// Maximum allowed description length.
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Frontmatter parsed from a skill `.md` file.
#[derive(Debug, Deserialize)]
pub struct SkillFrontmatter {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub disable_model_invocation: Option<bool>,
}

/// A loaded skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub disable_model_invocation: bool,
}

/// Result of loading skills from a directory.
#[derive(Debug, Default)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub errors: Vec<String>,
}

/// Parse YAML frontmatter from a markdown file content.
///
/// Frontmatter is delimited by `---` on its own line at the start.
/// Returns `(frontmatter, body_content)`.
pub fn parse_frontmatter(content: &str) -> Result<(SkillFrontmatter, String), String> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return Err("No frontmatter found (file must start with `---`)".into());
    }

    // Find closing `\n---` (the `\n` before closing `---`, after the first `---`)
    let rest = &content[3..]; // skip opening `---`

    // Find "\n---" as the closing delimiter marker
    let close_pos = rest.find("\n---").ok_or_else(|| String::from("Unclosed frontmatter: missing closing `---`"))?;

    // close_pos is the index of `\n` before `---` in `rest`.
    // yaml is from rest[0..close_pos] (trim leading \n).
    // body starts after `---` (+3) plus the closing `\n` if present (+1).
    let yaml_str = rest[..close_pos].trim_start();
    let body_start = close_pos + 4; // skip `\n---` (4 chars)
    let body = rest[body_start..].trim().to_string();

    let frontmatter: SkillFrontmatter =
        serde_yaml::from_str(yaml_str).map_err(|e| format!("Invalid frontmatter YAML: {e}"))?;

    Ok((frontmatter, body))
}

/// Validate a skill name.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Skill name must not be empty".into());
    }
    if name.len() > MAX_NAME_LENGTH {
        return Err(format!("Skill name too long (max {MAX_NAME_LENGTH} chars)"));
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("Skill name must contain only lowercase letters, digits, and hyphens".into());
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("Skill name must not start or end with a hyphen".into());
    }
    if name.contains("--") {
        return Err("Skill name must not contain consecutive hyphens".into());
    }
    Ok(())
}

/// Validate a skill description.
pub fn validate_description(desc: &str) -> Result<(), String> {
    if desc.is_empty() {
        return Err("Skill description must not be empty".into());
    }
    if desc.len() > MAX_DESCRIPTION_LENGTH {
        return Err(format!("Skill description too long (max {MAX_DESCRIPTION_LENGTH} chars)"));
    }
    Ok(())
}

/// Discover skills from multiple directory paths.
///
/// Paths are searched in order: global `~/.pi/skills/`, project `.pi/skills/`,
/// and any explicitly configured paths. Skills with the same name are deduped
/// (first occurrence wins).
pub fn discover_skills(extra_paths: &[PathBuf]) -> LoadSkillsResult {
    let mut result = LoadSkillsResult::default();
    let mut seen_names = std::collections::HashSet::new();

    // 1. Global skills: ~/.pi/skills/
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(".pi").join("skills");
        if global_dir.exists() {
            load_skills_from_dir(&global_dir, &mut result, &mut seen_names);
        }
    }

    // 2. Project-level skills (extra paths)
    for path in extra_paths {
        if path.exists() {
            load_skills_from_dir(path, &mut result, &mut seen_names);
        }
    }

    result
}

/// Load all `.md` files from a single directory (non-recursive).
fn load_skills_from_dir(dir: &Path, result: &mut LoadSkillsResult, seen_names: &mut std::collections::HashSet<String>) {
    let dir_name = dir.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            result.errors.push(format!("Cannot read dir {}: {e}", dir.display()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                result.errors.push(format!("Cannot read {}: {e}", path.display()));
                continue;
            }
        };

        let (frontmatter, _body) = match parse_frontmatter(&content) {
            Ok(f) => f,
            Err(e) => {
                result.errors.push(format!("Frontmatter error in {}: {e}", path.display()));
                continue;
            }
        };

        let name = frontmatter.name.unwrap_or_else(|| dir_name.clone());
        if let Err(e) = validate_name(&name) {
            result.errors.push(format!("Invalid skill name in {}: {e}", path.display()));
            continue;
        }

        let desc = frontmatter.description.unwrap_or_default();
        if let Err(e) = validate_description(&desc) {
            result.errors.push(format!("Invalid description in {}: {e}", path.display()));
            continue;
        }

        // Dedup by name
        if seen_names.contains(&name) {
            continue;
        }
        seen_names.insert(name.clone());

        result.skills.push(Skill {
            name,
            description: desc,
            file_path: path,
            base_dir: dir.to_path_buf(),
            disable_model_invocation: frontmatter.disable_model_invocation.unwrap_or(false),
        });
    }
}

/// Format skills as XML for injection into the system prompt.
///
/// Output: `<available_skills><skill>...</skill></available_skills>`
/// Skills with `disable_model_invocation: true` are excluded.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let enabled: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();

    if enabled.is_empty() {
        return String::new();
    }

    let items: String = enabled
        .iter()
        .map(|s| {
            let location = s.file_path.parent().and_then(|p| p.to_str()).unwrap_or("");
            format!(
                "    <skill>\n      <name>{}</name>\n      <description>{}</description>\n      <location>{}</location>\n    </skill>",
                s.name, s.description, location
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!("<available_skills>\n{}\n</available_skills>", items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = r#"---
name: test-skill
description: A test skill
---
This is the skill body."#;
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.name.as_deref(), Some("test-skill"));
        assert_eq!(fm.description.as_deref(), Some("A test skill"));
        assert_eq!(body, "This is the skill body.");
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let err = parse_frontmatter("just text").unwrap_err();
        assert!(err.contains("No frontmatter"));
    }

    #[test]
    fn test_parse_frontmatter_unclosed() {
        let content = "---\nname: x\n";
        let err = parse_frontmatter(content).unwrap_err();
        assert!(err.contains("Unclosed"));
    }

    #[test]
    fn test_parse_frontmatter_with_options() {
        let content = r#"---
name: stealth
description: Stealth browsing skill
disable_model_invocation: true
---
Body"#;
        let (fm, _) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.disable_model_invocation, Some(true));
    }

    #[test]
    fn test_validate_name_ok() {
        assert!(validate_name("my-skill").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("skill-42").is_ok());
    }

    #[test]
    fn test_validate_name_invalid() {
        assert!(validate_name("").is_err());
        assert!(validate_name("-bad").is_err());
        assert!(validate_name("bad-").is_err());
        assert!(validate_name("BAD").is_err());
        assert!(validate_name("has spaces").is_err());
        assert!(validate_name("double--hyphen").is_err());
    }

    #[test]
    fn test_validate_description_empty() {
        assert!(validate_description("").is_err());
    }

    #[test]
    fn test_format_skills_for_prompt() {
        let skills = vec![Skill {
            name: "test".into(),
            description: "A test skill".into(),
            file_path: PathBuf::from("/tmp/skills/test/SKILL.md"),
            base_dir: PathBuf::from("/tmp/skills"),
            disable_model_invocation: false,
        }];
        let xml = format_skills_for_prompt(&skills);
        assert!(xml.contains("<available_skills>"));
        assert!(xml.contains("<name>test</name>"));
        assert!(xml.contains("</available_skills>"));
    }

    #[test]
    fn test_format_skills_skips_disabled() {
        let skills = vec![
            Skill {
                name: "visible".into(),
                description: "visible".into(),
                file_path: PathBuf::from("a.md"),
                base_dir: PathBuf::from("."),
                disable_model_invocation: false,
            },
            Skill {
                name: "hidden".into(),
                description: "hidden".into(),
                file_path: PathBuf::from("b.md"),
                base_dir: PathBuf::from("."),
                disable_model_invocation: true,
            },
        ];
        let xml = format_skills_for_prompt(&skills);
        assert!(xml.contains("visible"));
        assert!(!xml.contains("hidden"));
    }

    #[test]
    fn test_discover_skills_no_dirs() {
        let result = discover_skills(&[]);
        // No paths given → no errors, empty result (global dir may or may not exist)
        assert!(result.errors.is_empty() || result.skills.is_empty());
    }
}
