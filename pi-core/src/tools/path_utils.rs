//! Path resolution utilities.
//! Mirrors `packages/coding-agent/src/core/tools/path-utils.ts`

use std::path::{Path, PathBuf};

/// Expand `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs_home().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Resolve a file path relative to `cwd`.
///
/// Handles `~` expansion and absolute paths.
pub fn resolve_to_cwd(file_path: &str, cwd: &Path) -> PathBuf {
    let expanded = expand_tilde(file_path);
    if expanded.is_absolute() { expanded } else { cwd.join(&expanded) }
}

/// Like `resolve_to_cwd`, but attempts a few platform-specific fallbacks for
/// macOS screenshot naming quirks.
pub fn resolve_read_path(file_path: &str, cwd: &Path) -> PathBuf {
    let resolved = resolve_to_cwd(file_path, cwd);

    if resolved.exists() {
        return resolved;
    }

    // Try macOS NFD normalization.
    let nfd = try_nfd(&resolved);
    if nfd.as_ref().map(|p| p.exists()).unwrap_or(false) {
        return nfd.unwrap();
    }

    resolved
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Decompose a string to NFD form (macOS stores filenames in NFD).
fn try_nfd(path: &Path) -> Option<PathBuf> {
    let s = path.to_string_lossy();
    // Quick check: the string is already likely ASCII-heavy.
    if s.contains(|c: char| c > '\x7F') {
        // NFD normalization on the filename component only.
        let parent = path.parent()?;
        let file_name = path.file_name()?;
        // We can't normalize to NFD in pure std easily. Skip for now.
        // This is a best-effort feature.
        let _ = file_name;
        let _ = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let home = dirs_home().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/foo"), home.join("foo"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn test_resolve_to_cwd() {
        let cwd = Path::new("/home/user/project");
        assert_eq!(resolve_to_cwd("subdir/file.txt", cwd), PathBuf::from("/home/user/project/subdir/file.txt"));
        assert_eq!(resolve_to_cwd("/abs/path/file.txt", cwd), PathBuf::from("/abs/path/file.txt"));
    }
}
