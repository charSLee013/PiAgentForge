//! Output accumulator — streaming output capture with temp-file spillover.
//!
//! Mirrors the `OutputAccumulator` class in packages/coding-agent/src/core/tools/output-accumulator.ts

/// Result from finalizing an [`OutputAccumulator`].
pub struct OutputAccumulatorResult {
    pub output: String,
    pub truncated: bool,
    pub full_output_path: Option<std::path::PathBuf>,
}

/// Captures streaming command output with bounded memory and optional
/// temp-file spillover for oversized output.
pub struct OutputAccumulator {
    chunks: Vec<String>,
    total_bytes: usize,
    max_bytes: usize,
    truncated: bool,
    full_output_path: Option<std::path::PathBuf>,
}

impl OutputAccumulator {
    pub fn new(max_bytes: usize) -> Self {
        Self { chunks: Vec::new(), total_bytes: 0, max_bytes, truncated: false, full_output_path: None }
    }

    /// Append a chunk of output data.
    pub fn push(&mut self, chunk: &str) {
        if self.truncated {
            if let Some(ref path) = self.full_output_path {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
                    let _ = writeln!(f, "{}", chunk);
                }
            }
            return;
        }

        let chunk_len = chunk.len();
        if self.full_output_path.is_none() && self.total_bytes + chunk_len > self.max_bytes {
            // Spill to temp file
            let pid = std::process::id();
            let tmp = std::env::temp_dir().join(format!("pi-output-{}.log", pid));
            if let Ok(mut f) = std::fs::File::create(&tmp) {
                use std::io::Write;
                for c in &self.chunks {
                    let _ = writeln!(f, "{}", c);
                }
                let _ = writeln!(f, "{}", chunk);
                self.full_output_path = Some(tmp);
            }
            self.truncated = true;
            return;
        }

        self.chunks.push(chunk.to_string());
        self.total_bytes += chunk_len;
    }

    /// Finalize and return the accumulated output.
    pub fn finalize(&self) -> OutputAccumulatorResult {
        let output = self.chunks.concat();
        let truncated = self.truncated;
        let full_path = self.full_output_path.clone();
        OutputAccumulatorResult { output, truncated, full_output_path: full_path }
    }

    /// Clean up the temp spill file.
    fn cleanup(&self) {
        if let Some(ref path) = self.full_output_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for OutputAccumulator {
    fn drop(&mut self) {
        self.cleanup();
    }
}
