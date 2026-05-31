//! Settings persistence — user configuration stored in `~/.pi/settings.json`.
//!
//! Mirrors `packages/coding-agent/src/core/settings-manager.ts`
//!
//! The settings file is a flat JSON object.  Unknown fields are preserved
//! through round-trips via `#[serde(flatten)]` on an extra map.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during settings operations.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// Underlying filesystem error (also covers lock failures).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialisation / deserialisation error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// User settings loaded from `~/.pi/settings.json`.
///
/// Only the most commonly-used fields are modelled explicitly.  Any
/// additional fields present in the file are captured in `extra` and
/// preserved on save so that other tools (or future versions) do not
/// lose data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Path to the settings file (never serialised).
    #[serde(skip)]
    pub path: PathBuf,

    /// Default model identifier (e.g. `"gpt-4o"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,

    /// Default provider (e.g. `"openai"`, `"anthropic"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,

    /// UI theme: `"dark"` or `"light"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Base URL for the API provider (for OpenAI-compatible endpoints).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// API key for the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Any extra fields from the JSON file that are not modelled above.
    /// These are preserved through round-trips so that data is not lost.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Settings {
    /// Default path: `~/.pi/settings.json`.
    pub fn default_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".pi").join("settings.json")
    }

    /// Load settings from the default path (`~/.pi/settings.json`).
    ///
    /// If the file does not exist, returns default settings (all fields
    /// set to `None`).
    pub fn load() -> Result<Self, SettingsError> {
        Self::load_from(Self::default_path())
    }

    /// Load settings from an explicit path (useful for tests).
    pub fn load_from(path: PathBuf) -> Result<Self, SettingsError> {
        match fs::File::open(&path) {
            Ok(file) => {
                fs2::FileExt::lock_shared(&file)?;
                let mut buf = String::new();
                (&file).read_to_string(&mut buf)?;
                let mut settings: Settings = serde_json::from_str(&buf)?;
                settings.path = path;
                Ok(settings)
            }
            Err(_) => Ok(Self {
                path,
                default_model: None,
                default_provider: None,
                theme: None,
                base_url: None,
                api_key: None,
                extra: HashMap::new(),
            }),
        }
    }

    /// Save settings to the file under an exclusive lock.
    ///
    /// Creates the parent directory (`~/.pi`) if it does not exist.
    pub fn save(&self) -> Result<(), SettingsError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self)?;
        let file = fs::File::create(&self.path)?;
        file.lock_exclusive()?;
        (&file).write_all(json.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scratch_path() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir creation failed");
        let path = dir.path().join("settings.json");
        (dir, path)
    }

    #[test]
    fn test_default_values() {
        let (_td, path) = scratch_path();
        let settings = Settings::load_from(path).expect("load on missing file should return defaults");
        assert!(settings.default_model.is_none());
        assert!(settings.default_provider.is_none());
        assert!(settings.theme.is_none());
        assert!(settings.base_url.is_none());
        assert!(settings.api_key.is_none());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let (_td, path) = scratch_path();
        {
            let settings = Settings {
                path: path.clone(),
                default_model: Some("gpt-4o".into()),
                default_provider: Some("openai".into()),
                theme: Some("dark".into()),
                base_url: Some("https://custom.api.com/v1".into()),
                api_key: Some("sk-custom-key".into()),
                extra: HashMap::new(),
            };
            settings.save().expect("save");
        }
        {
            let loaded = Settings::load_from(path).expect("reload");
            assert_eq!(loaded.default_model.as_deref(), Some("gpt-4o"));
            assert_eq!(loaded.default_provider.as_deref(), Some("openai"));
            assert_eq!(loaded.theme.as_deref(), Some("dark"));
            assert_eq!(loaded.base_url.as_deref(), Some("https://custom.api.com/v1"));
            assert_eq!(loaded.api_key.as_deref(), Some("sk-custom-key"));
        }
    }

    #[test]
    fn test_unknown_fields_preserved() {
        let (_td, path) = scratch_path();
        // Write a file with an extra field that is not modelled.
        let json = r#"{"default_model":"gpt-4o","theme":"dark","compaction":{"enabled":true,"reserveTokens":16384}}"#;
        fs::write(&path, json).expect("write sample");

        let settings = Settings::load_from(path.clone()).expect("load");
        assert_eq!(settings.default_model.as_deref(), Some("gpt-4o"));
        // The extra compaction field should be preserved.
        let compaction = settings.extra.get("compaction").expect("extra compaction field");
        assert_eq!(compaction["enabled"], serde_json::Value::Bool(true));
        assert_eq!(compaction["reserveTokens"], serde_json::Value::Number(16384.into()));

        // Save and verify the extra field survives.
        settings.save().expect("save");
        let content = fs::read_to_string(&path).expect("read saved file");
        assert!(content.contains("compaction"), "extra field should survive save: {content}");
    }

    #[test]
    fn test_save_only_non_none_fields_serialized() {
        let (_td, path) = scratch_path();
        let settings = Settings {
            path: path.clone(),
            default_model: Some("claude-sonnet-4".into()),
            default_provider: None,
            theme: None,
            base_url: None,
            api_key: None,
            extra: HashMap::new(),
        };
        settings.save().expect("save");
        let content = fs::read_to_string(&path).expect("read saved file");
        assert!(content.contains("default_model"));
        assert!(!content.contains("default_provider"), "None field should not appear");
        assert!(!content.contains("theme"), "None field should not appear");
        assert!(!content.contains("base_url"), "None field should not appear");
        assert!(!content.contains("api_key"), "None field should not appear");
    }

    #[test]
    fn test_base_url_field() {
        let (_td, path) = scratch_path();
        let json = r#"{"base_url":"https://custom.api.com/v1"}"#;
        fs::write(&path, json).expect("write sample");

        let settings = Settings::load_from(path).expect("load");
        assert_eq!(settings.base_url.as_deref(), Some("https://custom.api.com/v1"));
    }

    #[test]
    fn test_api_key_field() {
        let (_td, path) = scratch_path();
        let json = r#"{"api_key":"sk-saved-key"}"#;
        fs::write(&path, json).expect("write sample");

        let settings = Settings::load_from(path).expect("load");
        assert_eq!(settings.api_key.as_deref(), Some("sk-saved-key"));
    }

    #[test]
    fn test_default_path_looks_reasonable() {
        let path = Settings::default_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains(".pi"), "path should contain .pi: {path_str}");
        assert!(path_str.ends_with("settings.json"), "path should end with settings.json: {path_str}");
    }

    #[test]
    fn test_load_default_path_does_not_panic() {
        let _ = Settings::load();
    }
}
