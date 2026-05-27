//! JSONL framing helpers.
//!
//! Mirrors `packages/coding-agent/src/modes/rpc/jsonl.ts`.
//!
//! Framing is LF-only. Each record is a single line terminated by `\n`.
//! Clients must split records on `\n` only (not Unicode separators).

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Serialize a value to a single JSONL record (with trailing `\n`).
pub fn serialize_line(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()) + "\n"
}

/// Deserialize a JSONL record, trimming trailing `\n` and `\r`.
pub fn deserialize_line<T: DeserializeOwned>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim_end_matches('\n').trim_end_matches('\r'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestPayload {
        value: String,
    }

    #[test]
    fn test_serialize_line_appends_newline() {
        let payload = TestPayload {
            value: "hello".to_string(),
        };
        let line = serialize_line(&payload);
        assert!(line.ends_with('\n'), "line should end with newline");
        assert_eq!(
            line.trim_end_matches('\n'),
            r#"{"value":"hello"}"#,
            "serialized JSON should match"
        );
    }

    #[test]
    fn test_deserialize_line_handles_newline() {
        let line = r#"{"value":"hello"}
"#;
        let payload: TestPayload = deserialize_line(line).unwrap();
        assert_eq!(payload.value, "hello");
    }

    #[test]
    fn test_deserialize_line_handles_crlf() {
        let line = "{\"value\":\"hello\"}\r\n";
        let payload: TestPayload = deserialize_line(line).unwrap();
        assert_eq!(payload.value, "hello");
    }

    #[test]
    fn test_deserialize_line_no_newline() {
        let line = r#"{"value":"hello"}"#;
        let payload: TestPayload = deserialize_line(line).unwrap();
        assert_eq!(payload.value, "hello");
    }

    #[test]
    fn test_round_trip() {
        let payload = TestPayload {
            value: "world".to_string(),
        };
        let line = serialize_line(&payload);
        let back: TestPayload = deserialize_line(&line).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn test_deserialize_invalid_json() {
        let result: Result<TestPayload, _> = deserialize_line("not json\n");
        assert!(result.is_err());
    }
}
