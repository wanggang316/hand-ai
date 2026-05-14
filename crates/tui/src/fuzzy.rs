//! Fuzzy string matching for autocomplete and search.

/// Result of a fuzzy match, with score and matched character positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Match score (higher is better).
    pub score: i32,
    /// Indices of matched characters in the target string.
    pub indices: Vec<usize>,
}

/// Perform fuzzy matching of `query` against `target`.
///
/// Returns `Some(FuzzyMatch)` if all characters in `query` appear in `target`
/// in order (case-insensitive). Returns `None` if no match.
pub fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: vec![],
        });
    }

    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let target_chars: Vec<char> = target.chars().collect();
    let target_lower: Vec<char> = target.to_lowercase().chars().collect();

    let mut indices = Vec::with_capacity(query_lower.len());
    let mut qi = 0;

    for (ti, &tc) in target_lower.iter().enumerate() {
        if qi < query_lower.len() && tc == query_lower[qi] {
            indices.push(ti);
            qi += 1;
        }
    }

    if qi != query_lower.len() {
        return None;
    }

    // Scoring
    let mut score: i32 = 0;

    // Exact prefix bonus
    if target_lower
        .iter()
        .zip(query_lower.iter())
        .all(|(a, b)| a == b)
    {
        score += 10;
    }

    // Consecutive match bonus
    for window in indices.windows(2) {
        if window[1] == window[0] + 1 {
            score += 5;
        }
    }

    // Start-of-word bonus
    for &idx in &indices {
        if idx == 0 {
            score += 3;
        } else {
            let prev = target_chars[idx - 1];
            if prev == '_' || prev == '-' || prev == '/' || prev == '.' || prev == ' ' {
                score += 3;
            } else if target_chars[idx].is_uppercase() && prev.is_lowercase() {
                // camelCase boundary
                score += 2;
            }
        }
    }

    // Case-exact bonus
    for &idx in &indices {
        let qi_pos = indices.iter().position(|&i| i == idx).unwrap();
        if qi_pos < query.chars().count() {
            let qc = query.chars().nth(qi_pos).unwrap();
            if target_chars[idx] == qc {
                score += 1;
            }
        }
    }

    // Penalty for distance between first and last match
    if indices.len() > 1 {
        let span = indices.last().unwrap() - indices[0];
        score -= (span as i32 - indices.len() as i32 + 1).max(0);
    }

    // Shorter targets get a slight bonus (more relevant)
    score -= (target_chars.len() as i32 - query_lower.len() as i32).max(0) / 4;

    // Exact-match bonus. Without this, queries like "cl" tie with longer
    // prefix candidates ("clone") on every other dimension and unstable
    // sort decides the order. A flat +100 keeps the exact hit on top
    // even after the length penalty cancels out.
    if query_lower == target_lower {
        score += 100;
    }

    Some(FuzzyMatch { score, indices })
}

/// Filter and sort items by fuzzy match against `query`.
///
/// Returns `(original_index, FuzzyMatch)` pairs sorted by score (highest first).
pub fn fuzzy_filter(query: &str, items: &[&str]) -> Vec<(usize, FuzzyMatch)> {
    let mut matches: Vec<(usize, FuzzyMatch)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| fuzzy_match(query, item).map(|m| (i, m)))
        .collect();

    matches.sort_by_key(|m| std::cmp::Reverse(m.1.score));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        let m = fuzzy_match("", "anything").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.indices.is_empty());
    }

    #[test]
    fn exact_match() {
        let m = fuzzy_match("hello", "hello").unwrap();
        assert!(m.score > 0);
        assert_eq!(m.indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn prefix_match_scores_high() {
        let prefix = fuzzy_match("hel", "hello_world").unwrap();
        let scattered = fuzzy_match("hel", "has_each_letter").unwrap();
        assert!(prefix.score > scattered.score);
    }

    #[test]
    fn no_match_returns_none() {
        assert!(fuzzy_match("xyz", "hello").is_none());
    }

    #[test]
    fn case_insensitive() {
        let m = fuzzy_match("hello", "HELLO").unwrap();
        assert_eq!(m.indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn camel_case_boundary_bonus() {
        let m = fuzzy_match("gc", "getCamelCase").unwrap();
        assert!(m.score > 0);
    }

    #[test]
    fn snake_case_boundary_bonus() {
        let m = fuzzy_match("gm", "get_model").unwrap();
        assert!(m.indices.contains(&0)); // 'g' at start
    }

    #[test]
    fn consecutive_chars_score_higher() {
        let consecutive = fuzzy_match("abc", "abcdef").unwrap();
        let scattered = fuzzy_match("abc", "axbxcx").unwrap();
        assert!(consecutive.score > scattered.score);
    }

    /// An exact match must outrank longer items that share the query as
    /// a prefix. Without an explicit exact-match bonus, "cl" and "clone"
    /// both earn the prefix + consecutive + start-of-word bonuses and
    /// tie, leaving the order at the mercy of an unstable sort.
    #[test]
    fn exact_match_outranks_longer_prefix_match() {
        let exact = fuzzy_match("cl", "cl").unwrap();
        let longer = fuzzy_match("cl", "clone").unwrap();
        assert!(
            exact.score > longer.score,
            "exact={} should beat prefix={}",
            exact.score,
            longer.score
        );
    }

    #[test]
    fn fuzzy_filter_prioritizes_exact_over_longer_prefix() {
        let items = vec!["clone", "cl"];
        let results = fuzzy_filter("cl", &items);
        assert_eq!(
            results.first().map(|(i, _)| *i),
            Some(1),
            "exact match 'cl' must come first"
        );
    }

    #[test]
    fn fuzzy_filter_returns_sorted() {
        let items = vec!["apply", "app", "append", "banana"];
        let results = fuzzy_filter("app", &items);
        assert!(!results.is_empty());
        // All "app*" items should match, banana should not
        assert!(results.len() >= 3);
        assert!(!results.iter().any(|(i, _)| *i == 3)); // banana excluded
        // Best match should have highest score
        assert!(results[0].1.score >= results[1].1.score);
    }

    #[test]
    fn fuzzy_filter_excludes_non_matches() {
        let items = vec!["hello", "world", "xyz"];
        let results = fuzzy_filter("abc", &items);
        assert!(results.is_empty());
    }

    #[test]
    fn unicode_characters() {
        let m = fuzzy_match("你好", "你好世界");
        assert!(m.is_some());
    }

    #[test]
    fn single_char_query() {
        let m = fuzzy_match("a", "abc").unwrap();
        assert_eq!(m.indices, vec![0]);
    }
}
