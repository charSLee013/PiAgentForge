//! Session search utilities.
//!
//! Provides parsing of search queries (tokens, regex, phrases),
//! matching of sessions against queries, and sorting modes.

/// Sort mode for session display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortMode {
    /// Sorted by most recent first.
    Recent,
    /// Fuzzy relevance scoring.
    Relevance,
}

/// Filter for session names.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NameFilter {
    /// Show all sessions.
    All,
    /// Only show sessions with a user-assigned name.
    Named,
}

/// A parsed search query.
#[derive(Debug, Clone)]
pub struct ParsedSearchQuery {
    pub mode: SearchMode,
    pub tokens: Vec<SearchToken>,
    pub regex: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchMode {
    Tokens,
    Regex,
}

#[derive(Debug, Clone)]
pub enum SearchToken {
    Fuzzy(String),
    Phrase(String),
}

/// Match result for a session.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub matches: bool,
    /// Lower is better; only meaningful when matches is true.
    pub score: i32,
}

/// Parse a search query string into a structured query.
///
/// Supports:
/// - Space-separated fuzzy tokens
/// - Quoted phrases: `"hello world"`
/// - Regex mode: `re:pattern`
pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSearchQuery {
            mode: SearchMode::Tokens,
            tokens: vec![],
            regex: None,
            error: None,
        };
    }

    // Regex mode
    if let Some(pattern) = trimmed.strip_prefix("re:") {
        let p = pattern.trim();
        if p.is_empty() {
            return ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: vec![],
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        return ParsedSearchQuery {
            mode: SearchMode::Regex,
            tokens: vec![],
            regex: Some(p.to_string()),
            error: None,
        };
    }

    // Token mode with quote support
    let mut tokens: Vec<SearchToken> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut had_unclosed = false;

    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                // Close quote -> phrase token
                let v = buf.trim().to_string();
                buf.clear();
                if !v.is_empty() {
                    tokens.push(SearchToken::Phrase(v));
                }
                in_quote = false;
            } else {
                // Open quote: flush current fuzzy buffer
                let v = buf.trim().to_string();
                buf.clear();
                if !v.is_empty() {
                    tokens.push(SearchToken::Fuzzy(v));
                }
                in_quote = true;
            }
            continue;
        }

        if !in_quote && ch.is_whitespace() {
            let v = buf.trim().to_string();
            buf.clear();
            if !v.is_empty() {
                tokens.push(SearchToken::Fuzzy(v));
            }
            continue;
        }

        buf.push(ch);
    }

    if in_quote {
        had_unclosed = true;
    }

    // Flush remaining buffer
    let v = buf.trim().to_string();
    if !v.is_empty() {
        if in_quote && had_unclosed {
            // If we ended inside an unclosed quote, treat as fuzzy
            tokens.push(SearchToken::Fuzzy(v));
        } else {
            tokens.push(if in_quote { SearchToken::Phrase(v) } else { SearchToken::Fuzzy(v) });
        }
    }

    ParsedSearchQuery {
        mode: SearchMode::Tokens,
        tokens,
        regex: None,
        error: if had_unclosed {
            // Unclosed quotes fall back to plain tokenization
            let plain: Vec<&str> = trimmed.split_whitespace().collect();
            Some(format!("unclosed quote, using: {}", plain.join(" ")))
        } else {
            None
        },
    }
}

/// Match a session's search text against a parsed query.
///
/// `search_text` is the concatenation of session fields (id, name, messages, cwd).
pub fn match_session(search_text: &str, parsed: &ParsedSearchQuery) -> MatchResult {
    match &parsed.mode {
        SearchMode::Regex => {
            let re_str = match &parsed.regex {
                Some(r) => r,
                None => return MatchResult { matches: false, score: 0 },
            };
            // Simple substring-based regex emulation (case-insensitive)
            let re_lower = re_str.to_lowercase();
            let text_lower = search_text.to_lowercase();
            match text_lower.find(&re_lower) {
                Some(pos) => MatchResult { matches: true, score: pos as i32 },
                None => MatchResult { matches: false, score: 0 },
            }
        }
        SearchMode::Tokens => {
            if parsed.tokens.is_empty() {
                return MatchResult { matches: true, score: 0 };
            }

            let text_lower = search_text.to_lowercase();
            let mut score = 0i32;

            for token in &parsed.tokens {
                match token {
                    SearchToken::Phrase(phrase) => {
                        let p = phrase.to_lowercase();
                        match text_lower.find(&p) {
                            Some(pos) => score += pos as i32,
                            None => return MatchResult { matches: false, score: 0 },
                        }
                    }
                    SearchToken::Fuzzy(fuzzy) => {
                        // All characters must appear in order (simple fuzzy)
                        let q = fuzzy.to_lowercase();
                        let qbytes = q.as_bytes();
                        let tbytes = text_lower.as_bytes();
                        let mut ti = 0;
                        let mut matched = 0;
                        for &qb in qbytes {
                            while ti < tbytes.len() && tbytes[ti] != qb {
                                ti += 1;
                            }
                            if ti < tbytes.len() {
                                matched += 1;
                                ti += 1;
                            } else {
                                break;
                            }
                        }
                        if matched != qbytes.len() {
                            return MatchResult { matches: false, score: 0 };
                        }
                        score += ti as i32;
                    }
                }
            }

            MatchResult { matches: true, score }
        }
    }
}

/// Filter and sort sessions by query, sort mode, and name filter.
pub fn filter_and_sort_sessions(
    session_search_texts: &[(/*id*/ &str, /*search_text*/ &str, /*has_name*/ bool)],
    query: &str,
    sort_mode: SortMode,
    name_filter: NameFilter,
) -> Vec<(usize, /*score*/ i32)> {
    let parsed = parse_search_query(query);

    // Filter by name
    let name_filtered: Vec<(usize, &str, bool)> = session_search_texts
        .iter()
        .enumerate()
        .filter(|(_, (_, _, has_name))| match name_filter {
            NameFilter::All => true,
            NameFilter::Named => *has_name,
        })
        .map(|(idx, (_id, text, has_name))| (idx, *text, *has_name))
        .collect();

    // Score all
    let mut results: Vec<(usize, i32)> = Vec::new();
    for (idx, text, _) in &name_filtered {
        let result = match_session(text, &parsed);
        if !result.matches {
            continue;
        }
        results.push((*idx, result.score));
    }

    // Sort
    match sort_mode {
        SortMode::Recent => {
            // Preserve insertion order (most recent first in input)
        }
        SortMode::Relevance => {
            results.sort_by_key(|&(_, score)| score);
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_query() {
        let q = parse_search_query("");
        assert!(q.tokens.is_empty());
        assert!(q.error.is_none());
    }

    #[test]
    fn test_parse_simple_tokens() {
        let q = parse_search_query("hello world");
        assert_eq!(q.tokens.len(), 2);
    }

    #[test]
    fn test_parse_phrase() {
        let q = parse_search_query("\"hello world\"");
        assert_eq!(q.tokens.len(), 1);
        if let SearchToken::Phrase(p) = &q.tokens[0] {
            assert_eq!(p, "hello world");
        } else {
            panic!("expected phrase token");
        }
    }

    #[test]
    fn test_parse_regex() {
        let q = parse_search_query("re:^hello");
        assert_eq!(q.mode, SearchMode::Regex);
        assert_eq!(q.regex.as_deref(), Some("^hello"));
    }

    #[test]
    fn test_match_session_tokens() {
        let parsed = parse_search_query("claude");
        let result = match_session("Anthropic Claude Opus", &parsed);
        assert!(result.matches);
    }

    #[test]
    fn test_match_session_no_match() {
        let parsed = parse_search_query("xyz123");
        let result = match_session("Anthropic Claude Opus", &parsed);
        assert!(!result.matches);
    }

    #[test]
    fn test_filter_and_sort() {
        let sessions = vec![
            ("s1", "Anthropic Claude Opus", true),
            ("s2", "OpenAI GPT-4", false),
            ("s3", "Anthropic Claude Haiku", true),
        ];
        let results = filter_and_sort_sessions(&sessions, "claude", SortMode::Relevance, NameFilter::All);
        assert_eq!(results.len(), 2); // s1 and s3 match "claude"
    }

    #[test]
    fn test_name_filter_excludes_unnamed() {
        let sessions = vec![
            ("s1", "session one", true),
            ("s2", "session two", false),
        ];
        let results = filter_and_sort_sessions(&sessions, "", SortMode::Recent, NameFilter::Named);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 0); // only s1 has a name
    }
}
