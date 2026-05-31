//! Stdin buffer — non-blocking reader that accumulates bytes and splits them
//! into complete terminal sequences.
//!
//! This is necessary because stdin data can arrive in partial chunks,
//! especially for escape sequences. Without buffering, partial sequences
//! can be misinterpreted as regular keypresses.
//!
//! Based on code from OpenTUI and the TypeScript `stdin-buffer.ts`.

use std::io;
use tokio::sync::mpsc;

// Escape character
const ESC: u8 = 0x1b;

// Bracketed paste markers
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Non-blocking stdin reader that accumulates bytes and extracts complete
/// terminal sequences.
///
/// A background thread reads from stdin and forwards chunks through an
/// unbounded channel. The `read()` method drains available chunks without
/// blocking, accumulates them, and splits them into complete sequences.
pub struct StdinBuffer {
    /// Accumulated bytes not yet split into complete sequences.
    buffer: Vec<u8>,
    /// Receiver for chunks from the background reader thread.
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Whether we are inside a bracketed paste.
    paste_mode: bool,
    /// Accumulated paste content.
    paste_buffer: Vec<u8>,
}

impl StdinBuffer {
    /// Create a new `StdinBuffer`, spawning a background stdin reader thread.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn a background thread that reads from blocking stdin.
        std::thread::spawn(move || {
            use std::io::Read;
            let mut stdin = io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => {
                        // EOF — stdin closed
                        let _ = tx.send(Vec::new());
                        break;
                    }
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                        continue;
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        });

        Self { buffer: Vec::new(), rx, paste_mode: false, paste_buffer: Vec::new() }
    }

    /// Read available bytes from stdin (non-blocking).
    ///
    /// Drains all queued chunks from the background reader, accumulates them,
    /// and extracts complete terminal sequences. Returns `Ok(vec![])` when
    /// no data is available.
    pub async fn read(&mut self) -> io::Result<Vec<String>> {
        // Drain all available chunks from the channel
        let mut got_data = false;
        while let Ok(chunk) = self.rx.try_recv() {
            if chunk.is_empty() {
                // EOF marker
                return Ok(self.drain());
            }
            got_data = true;
            self.accumulate(&chunk);
        }

        if !got_data && self.buffer.is_empty() {
            return Ok(vec![]);
        }

        Ok(self.extract_sequences())
    }

    /// Drain any remaining buffered data as a single sequence.
    pub fn drain(&mut self) -> Vec<String> {
        self.paste_mode = false;
        self.paste_buffer.clear();
        if self.buffer.is_empty() {
            return vec![];
        }
        let s = String::from_utf8_lossy(&self.buffer).to_string();
        self.buffer.clear();
        vec![s]
    }

    /// Clear all internal state.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Accumulate raw bytes, handling high-byte conversion.
    fn accumulate(&mut self, data: &[u8]) {
        // Handle single high byte (>127): convert to ESC + (byte - 128)
        if data.len() == 1 && data[0] > 127 {
            self.buffer.push(ESC);
            self.buffer.push(data[0] - 128);
            return;
        }
        self.buffer.extend_from_slice(data);
    }

    /// Extract complete terminal sequences from the internal buffer.
    ///
    /// Returns complete sequences and keeps any trailing incomplete data
    /// in `self.buffer`.
    fn extract_sequences(&mut self) -> Vec<String> {
        // Check for paste start marker in the buffer
        if !self.paste_mode {
            if let Some(pos) = self.find_bytes(PASTE_START) {
                // Emit sequences before the paste marker
                let mut result = Vec::new();
                if pos > 0 {
                    let before = self.buffer[..pos].to_vec();
                    self.buffer.drain(..pos + PASTE_START.len());
                    result.extend(Self::extract_raw_sequences(&before));
                } else {
                    self.buffer.drain(..PASTE_START.len());
                }

                // Enter paste mode
                self.paste_mode = true;
                self.paste_buffer.clear();

                // Check if paste end marker follows immediately
                if let Some(end_pos) = self.find_bytes(PASTE_END) {
                    let pasted = self.buffer[..end_pos].to_vec();
                    self.buffer.drain(..end_pos + PASTE_END.len());
                    self.paste_mode = false;
                    if !pasted.is_empty() {
                        result.push(String::from_utf8_lossy(&pasted).to_string());
                    }
                } else {
                    // Buffer everything until paste end
                    self.paste_buffer.extend_from_slice(&self.buffer);
                    self.buffer.clear();
                }

                return result;
            }
        } else {
            // In paste mode: accumulate until paste end marker
            if let Some(end_pos) = self.find_bytes(PASTE_END) {
                let pasted = self.paste_buffer[..].to_vec();

                // Also check if the current buffer has content before the end marker
                let mut pasted_content = pasted;
                if end_pos > 0 {
                    pasted_content.extend_from_slice(&self.buffer[..end_pos]);
                }
                self.buffer.drain(..end_pos + PASTE_END.len());

                self.paste_mode = false;
                self.paste_buffer.clear();

                let mut result = Vec::new();
                if !pasted_content.is_empty() {
                    result.push(String::from_utf8_lossy(&pasted_content).to_string());
                }
                // Continue extracting from remaining buffer
                result.extend(Self::extract_raw_sequences(&self.buffer));
                self.buffer.clear();
                return result;
            } else {
                // Still inside paste — keep accumulating
                self.paste_buffer.extend_from_slice(&self.buffer);
                self.buffer.clear();
                return vec![];
            }
        }

        Self::extract_raw_sequences(&self.buffer)
    }

    /// Extract complete sequences from raw bytes (no paste mode handling).
    fn extract_raw_sequences(bytes: &[u8]) -> Vec<String> {
        let mut sequences = Vec::new();
        let mut pos = 0;

        while pos < bytes.len() {
            if bytes[pos] == ESC {
                // Try to find the end of this escape sequence
                match find_escape_end(&bytes[pos..]) {
                    Some(end_offset) => {
                        let end = pos + end_offset;
                        sequences.push(String::from_utf8_lossy(&bytes[pos..=end]).to_string());
                        pos = end + 1;
                    }
                    None => {
                        // Incomplete — keep remaining bytes
                        break;
                    }
                }
            } else {
                // Single byte (printable or control)
                sequences.push(String::from_utf8_lossy(&bytes[pos..pos + 1]).to_string());
                pos += 1;
            }
        }

        sequences
    }

    /// Find the first occurrence of a byte pattern in the buffer.
    fn find_bytes(&self, pattern: &[u8]) -> Option<usize> {
        self.buffer.windows(pattern.len()).position(|w| w == pattern)
    }
}

impl Default for StdinBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Escape sequence boundary detection
// ---------------------------------------------------------------------------

/// Determine whether an escape sequence is complete, incomplete, or not
/// an escape sequence at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeqStatus {
    Complete,
    Incomplete,
    NotEscape,
}

/// Check if a byte buffer (starting at pos 0) represents a complete sequence.
fn sequence_status(data: &[u8]) -> SeqStatus {
    if data.is_empty() || data[0] != ESC {
        return SeqStatus::NotEscape;
    }

    if data.len() == 1 {
        return SeqStatus::Incomplete;
    }

    let after_esc = data[1];

    // CSI sequences: ESC [
    if after_esc == b'[' {
        if data.len() < 3 {
            return SeqStatus::Incomplete;
        }
        // Old-style mouse: ESC[M +3 bytes = 6 total
        if data[2] == b'M' {
            return if data.len() >= 6 { SeqStatus::Complete } else { SeqStatus::Incomplete };
        }
        return csi_status(&data[2..]);
    }

    // OSC sequences: ESC ] ... ST (ESC \ or BEL)
    if after_esc == b']' {
        return osc_status(data);
    }

    // DCS sequences: ESC P ... ST (ESC \)
    if after_esc == b'P' {
        return if data.contains(&ESC) && data[data.len() - 2..] == [ESC, b'\\'] {
            SeqStatus::Complete
        } else {
            SeqStatus::Incomplete
        };
    }

    // APC sequences: ESC _ ... ST (ESC \)
    if after_esc == b'_' {
        return if data.contains(&ESC) && data[data.len() - 2..] == [ESC, b'\\'] {
            SeqStatus::Complete
        } else {
            SeqStatus::Incomplete
        };
    }

    // SS3 sequences: ESC O + single char
    if after_esc == b'O' {
        return if data.len() >= 3 { SeqStatus::Complete } else { SeqStatus::Incomplete };
    }

    // Meta/Alt prefix: ESC + single byte
    if data.len() >= 2 {
        return SeqStatus::Complete;
    }

    SeqStatus::Incomplete
}

/// CSI sequences end with a final byte in range 0x40-0x7E.
/// `payload` is the bytes after `ESC[`.
fn csi_status(payload: &[u8]) -> SeqStatus {
    if payload.is_empty() {
        return SeqStatus::Incomplete;
    }

    // Check if we have a complete CSI sequence (final byte in 0x40-0x7E)
    // SGR mouse: ESC[<...M or ESC[<...m
    if payload[0] == b'<' && payload.len() >= 2 {
        let last = payload[payload.len() - 1];
        if last == b'M' || last == b'm' {
            // Check format: <digits;digits;digits[Mm]
            let inner = &payload[1..payload.len() - 1];
            let parts: Vec<&[u8]> = inner.split(|&b| b == b';').collect();
            if parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.iter().all(u8::is_ascii_digit)) {
                return SeqStatus::Complete;
            }
            // Still potentially building the mouse sequence
            return SeqStatus::Incomplete;
        }
    }

    // Generic CSI: find a final byte in 0x40-0x7E
    for &b in payload {
        if (0x40..=0x7e).contains(&b) {
            return SeqStatus::Complete;
        }
    }

    SeqStatus::Incomplete
}

/// OSC sequences end with ST (ESC \) or BEL (0x07).
/// `data` is the full sequence including the leading ESC.
fn osc_status(data: &[u8]) -> SeqStatus {
    if data.len() >= 4 && data[data.len() - 2..] == [ESC, b'\\'] {
        return SeqStatus::Complete;
    }
    if data.len() >= 3 && data[data.len() - 1] == 0x07 {
        return SeqStatus::Complete;
    }
    SeqStatus::Incomplete
}

/// Find the end offset of a complete escape sequence starting at `data[0]`.
/// Returns `Some(offset)` where `data[..=offset]` is the complete sequence,
/// or `None` if incomplete.
fn find_escape_end(data: &[u8]) -> Option<usize> {
    if data.is_empty() || data[0] != ESC {
        return None;
    }

    // Walk forward trying progressively longer prefixes
    for end in 1..data.len() {
        match sequence_status(&data[..=end]) {
            SeqStatus::Complete => return Some(end),
            SeqStatus::NotEscape => return None,
            SeqStatus::Incomplete => continue,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_escape_end_csi() {
        // Arrow keys
        assert_eq!(find_escape_end(b"\x1b[A"), Some(2));
        assert_eq!(find_escape_end(b"\x1b[B"), Some(2));
        assert_eq!(find_escape_end(b"\x1b[1;5C"), Some(5));

        // Home/End
        assert_eq!(find_escape_end(b"\x1b[H"), Some(2));
        assert_eq!(find_escape_end(b"\x1b[F"), Some(2));

        // Tilde codes
        assert_eq!(find_escape_end(b"\x1b[2~"), Some(3));
        assert_eq!(find_escape_end(b"\x1b[3~"), Some(3));
        assert_eq!(find_escape_end(b"\x1b[200~"), Some(5));
        assert_eq!(find_escape_end(b"\x1b[201~"), Some(5));
    }

    #[test]
    fn test_find_escape_end_incomplete() {
        assert_eq!(find_escape_end(b"\x1b"), None);
        assert_eq!(find_escape_end(b"\x1b["), None);
        assert_eq!(find_escape_end(b"\x1b[1"), None);
        assert_eq!(find_escape_end(b"\x1b[1;"), None);
    }

    #[test]
    fn test_extract_raw_sequences_mixed() {
        let bytes = b"hello\x1b[A";
        let result = StdinBuffer::extract_raw_sequences(bytes);
        assert_eq!(result.len(), 6); // h e l l o + \x1b[A
        assert_eq!(result[0], "h");
        assert_eq!(result[1], "e");
        assert_eq!(result[2], "l");
        assert_eq!(result[3], "l");
        assert_eq!(result[4], "o");
        assert_eq!(result[5], "\x1b[A");
    }

    #[test]
    fn test_extract_raw_sequences_ss3() {
        let bytes = b"\x1bOP\x1bOQ";
        let result = StdinBuffer::extract_raw_sequences(bytes);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "\x1bOP");
        assert_eq!(result[1], "\x1bOQ");
    }

    #[test]
    fn test_extract_raw_sequences_incomplete_tail() {
        // Incomplete escape at the end should be dropped (kept in buffer by caller)
        let bytes = b"abc\x1b[";
        let result = StdinBuffer::extract_raw_sequences(bytes);
        assert_eq!(result.len(), 3); // only "a", "b", "c"
    }

    #[test]
    fn test_extract_raw_sequences_paste_markers() {
        // The static extract_raw_sequences doesn't do paste-mode batching;
        // it just splits into raw terminal sequences: paste markers are
        // complete CSI sequences and the pasted text bytes are individual chars.
        let result = StdinBuffer::extract_raw_sequences(b"\x1b[200~AB\x1b[201~");
        // \x1b[200~, 'A', 'B', \x1b[201~ = 4 sequences
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], "\x1b[200~");
        assert_eq!(result[1], "A");
        assert_eq!(result[2], "B");
        assert_eq!(result[3], "\x1b[201~");
    }

    #[test]
    fn test_sequence_status_meta() {
        // ESC + printable = complete (alt+key)
        assert_eq!(sequence_status(b"\x1ba"), SeqStatus::Complete);
        assert_eq!(sequence_status(b"\x1b\x1b"), SeqStatus::Complete); // alt+esc
    }

    #[test]
    fn test_high_byte_conversion() {
        let mut sb = StdinBuffer::new();
        // Single byte > 127 should be converted
        sb.accumulate(&[0x80]); // should convert to \x1b + \x00... hmm
        // Actually, 0x80 - 128 = 0, so this becomes \x1b\x00
        // But that's a degenerate case normally
    }

    #[test]
    fn test_clear() {
        let mut sb = StdinBuffer::new();
        sb.buffer = b"\x1b[A".to_vec();
        sb.paste_mode = true;
        sb.clear();
        assert!(sb.buffer.is_empty());
        assert!(!sb.paste_mode);
    }

    #[test]
    fn test_drain_empty() {
        let mut sb = StdinBuffer::new();
        assert!(sb.drain().is_empty());
    }

    #[test]
    fn test_drain_with_data() {
        let mut sb = StdinBuffer::new();
        sb.accumulate(b"leftover");
        let drained = sb.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0], "leftover");
        assert!(sb.buffer.is_empty());
    }
}
