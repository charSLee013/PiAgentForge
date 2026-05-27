//! Extension discovery and manifest loading.
//!
//! Scans directories for `.wasm` extension files and produces manifests.

use std::path::{Path, PathBuf};

use crate::types::{ExtensionManifest, Result};

/// Discover all WASM extension files in the given search paths.
///
/// Scans each directory for files with a `.wasm` extension and builds
/// a default [`ExtensionManifest`] for each one.
pub fn discover_extensions(paths: &[PathBuf]) -> Vec<ExtensionManifest> {
    let mut manifests = Vec::new();
    for path in paths {
        if !path.exists() || !path.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if entry_path.extension().is_some_and(|e| e == "wasm") {
                    manifests.push(load_extension_manifest(&entry_path));
                }
            }
        }
    }
    manifests
}

/// Load (or synthesize) an extension manifest from a file path.
///
/// For bare `.wasm` files a default manifest is generated using the
/// file stem as the extension name. Future iterations may support
/// companion `extension.toml` files.
pub fn load_extension_manifest(path: &Path) -> ExtensionManifest {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    ExtensionManifest {
        name,
        version: "0.1.0".to_string(),
        description: None,
        capabilities: vec!["tools".to_string()],
    }
}

/// Read a WASM binary from disk for loading into the runtime.
pub fn read_wasm_bytes(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read(path)?)
}
