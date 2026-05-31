//! Fuzzy matching utilities.
//!
//! Matches if all query characters appear in order (not necessarily consecutive).
//! Lower score = better match. Returns `None` if no match.

/// Score bonus constants.
const CONSECUTIVE_BONUS: i32 = 5; // points off per consecutive match
const WORD_BOUNDARY_BONUS: i32 = 10;
const EXACT_MATCH_BONUS: i32 = 100;

/// Check if character `c` at position `i` in `text` is at a word boundary.
fn is_word_boundary(_c: u8, prev_c: u8) -> bool {
    matches!(prev_c, b' ' | b'-' | b'_' | b'.' | b'/' | b':')
}

/// Match a normalized (lowercase) query against normalized text.
/// Returns `Some(score)` if all characters match in order, `None` otherwise.
fn match_normalized(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    if query.len() > text.len() {
        return None;
    }

    let qbytes = query.as_bytes();
    let tbytes = text.as_bytes();

    let mut qi = 0;
    let mut score: i32 = 0;
    let mut last_match: i32 = -1;
    let mut consecutive = 0;

    for (ti, &tb) in tbytes.iter().enumerate() {
        if qi >= qbytes.len() {
            break;
        }
        if tb == qbytes[qi] {
            let at_word_boundary = ti == 0 || is_word_boundary(tb, tbytes[ti.saturating_sub(1)]);

            // Consecutive-match bonus
            if last_match >= 0 && ti as i32 == last_match + 1 {
                consecutive += 1;
                score -= CONSECUTIVE_BONUS * consecutive;
            } else {
                consecutive = 0;
                // Penalty for skipping characters
                if last_match >= 0 {
                    score += (ti as i32 - last_match - 1) * 2;
                }
            }

            // Word-boundary bonus
            if at_word_boundary {
                score -= WORD_BOUNDARY_BONUS;
            }

            // Slight penalty for later match positions (favour early matches)
            score += (ti as f32 * 0.1) as i32;

            last_match = ti as i32;
            qi += 1;
        }
    }

    if qi < qbytes.len() {
        return None;
    }

    // Exact-match bonus
    if query == text {
        score -= EXACT_MATCH_BONUS;
    }

    Some(score)
}

/// Fuzzy-match `pattern` against `text`.
///
/// Returns `Some(score)` where a lower score is a better match.
/// Returns `None` if the pattern does not match.
///
/// The match is case-insensitive. Supports null pattern (empty string),
/// which always matches with score 0.
pub fn fuzzy_match(pattern: &str, text: &str) -> Option<i32> {
    let query_lower = pattern.to_lowercase();
    let text_lower = text.to_lowercase();

    let primary = match_normalized(&query_lower, &text_lower);

    // If the primary doesn't match, try a swapped order
    // (letters/digits reversed, e.g. "abc123" → "123abc").
    if primary.is_none() {
        if let Some(swapped_score) = try_swapped(&query_lower, &text_lower) {
            return Some(swapped_score);
        }
        return None;
    }

    let primary_score = primary.unwrap();
    let swapped = try_swapped(&query_lower, &text_lower);
    match swapped {
        Some(swapped_score) if swapped_score < primary_score => Some(swapped_score),
        _ => Some(primary_score),
    }
}

/// Try matching with letters/digits swapped, e.g. "abc123" as "123abc".
fn try_swapped(query: &str, text: &str) -> Option<i32> {
    let letters: String = query.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = query.chars().filter(|c| c.is_ascii_digit()).collect();

    if letters.is_empty() || digits.is_empty() {
        return None;
    }

    // Detect the original order: if query is "abc123", swap to "123abc";
    // if query is "123abc", swap to "abc123".
    let is_alpha_first = query.starts_with(|c: char| c.is_ascii_alphabetic());
    let swapped = if is_alpha_first { format!("{}{}", digits, letters) } else { format!("{}{}", letters, digits) };

    // Don't bother if the swapped form is the same as the original.
    if swapped == query {
        return None;
    }

    let swapped_match = match_normalized(&swapped, text)?;
    Some(swapped_match + 5) // small penalty for the swap
}

/// Filter and score items by fuzzy match.
///
/// Returns a vector of `(index, score)` pairs for all matching items,
/// sorted by score (best match first).
pub fn fuzzy_filter<T: AsRef<str>>(pattern: &str, items: &[T]) -> Vec<(usize, i32)> {
    if pattern.is_empty() {
        return items.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    }

    // Support space-separated tokens: all tokens must match
    let tokens: Vec<&str> = pattern.split_whitespace().filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return items.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    }

    let mut results: Vec<(usize, i32)> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        let text = item.as_ref();
        let mut total_score: i32 = 0;
        let mut all_match = true;

        for token in &tokens {
            match fuzzy_match(token, text) {
                Some(s) => total_score += s,
                None => {
                    all_match = false;
                    break;
                }
            }
        }

        if all_match {
            results.push((i, total_score));
        }
    }

    results.sort_by_key(|&(_, score)| score);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_pattern() {
        assert_eq!(fuzzy_match("", "anything"), Some(0));
    }

    #[test]
    fn test_exact_match() {
        let score = fuzzy_match("hello", "hello");
        assert!(score.is_some());
        assert!(score.unwrap() < 0); // exact match bonus makes it negative
    }

    #[test]
    fn test_substring_match() {
        assert!(fuzzy_match("hel", "hello").is_some());
    }

    #[test]
    fn test_case_insensitive() {
        assert!(fuzzy_match("HEL", "hello").is_some());
        assert!(fuzzy_match("hel", "HELLO").is_some());
    }

    #[test]
    fn test_no_match() {
        assert_eq!(fuzzy_match("xyz", "hello"), None);
    }

    #[test]
    fn test_partial_match() {
        assert!(fuzzy_match("hw", "hello world").is_some());
    }

    #[test]
    fn test_consecutive_bonus() {
        let consecutive = fuzzy_match("el", "hello").unwrap();
        let scattered = fuzzy_match("ho", "hello").unwrap();
        // consecutive match should score lower (better)
        assert!(consecutive < scattered, "consecutive={consecutive} scattered={scattered}");
    }

    #[test]
    fn test_word_boundary_bonus() {
        let boundary = fuzzy_match("hw", "hello world").unwrap();
        let no_boundary = fuzzy_match("hl", "hello").unwrap();
        assert!(boundary < no_boundary, "boundary={boundary} no_boundary={no_boundary}");
    }

    #[test]
    fn test_filter_empty_pattern() {
        let items = vec!["apple", "banana", "cherry"];
        let result = fuzzy_filter("", &items);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_filter_basic() {
        let items = vec!["apple", "banana", "cherry", "apricot"];
        let result = fuzzy_filter("ap", &items);
        assert_eq!(result.len(), 2);
        // Both "apple" and "apricot" start with "ap"
        assert!(result.iter().any(|(i, _)| *i == 0)); // apple
        assert!(result.iter().any(|(i, _)| *i == 3)); // apricot
    }

    #[test]
    fn test_filter_tokenized() {
        let items = vec!["hello world", "hello there", "goodbye world"];
        let result = fuzzy_filter("he wo", &items);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 0);
    }

    #[test]
    fn test_score_ordering() {
        let items = vec!["banana", "apple", "apricot", "cherry"];
        let result = fuzzy_filter("ap", &items);
        // apple should rank above apricot (starts with exact "ap")
        let apple_pos = result.iter().position(|(i, _)| *i == 1).unwrap();
        let apricot_pos = result.iter().position(|(i, _)| *i == 2).unwrap();
        assert!(apple_pos < apricot_pos, "apple should rank above apricot");
    }

    #[test]
    fn test_query_longer_than_text() {
        assert_eq!(fuzzy_match("hello world", "hello"), None);
    }

    #[test]
    fn test_swapped_order() {
        // "abc123" as query should also match "123abc" (letters/digits swapped)
        assert!(fuzzy_match("abc123", "123abc").is_some());
        // "123abc" as query should also match "abc123" (digits/letters swapped)
        assert!(fuzzy_match("123abc", "abc123").is_some());
        // Normal exact match still works
        assert!(fuzzy_match("abc123", "abc123").is_some());
    }
}
