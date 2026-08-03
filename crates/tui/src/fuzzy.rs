//! Tokenized substring matching for autocomplete and selector search.
//!
//! The module keeps its historical `fuzzy` name, but the semantics are
//! deliberately *not* subsequence-fuzzy: the query is split on whitespace
//! and every token must appear as a contiguous, case-insensitive substring
//! of the target (token order within the target is irrelevant). Subsequence
//! matching let short queries light up unrelated ids — `glm-5` matched
//! `g`oog`l`e/ge`m`ini-2.`5`-pro — which is exactly wrong for the
//! short-identifier lists these selectors filter.

/// Result of a match, with score and matched character positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Match score (higher is better).
    pub score: i32,
    /// Character indices of the matched substring spans in the target —
    /// one contiguous run per query token (sorted, deduplicated), so
    /// highlight rendering can mark the matched spans.
    pub indices: Vec<usize>,
}

// Score layering: occurrence tier (exact / prefix / word-boundary /
// mid-string) dominates start position, which dominates target length.
// The clamps keep each layer from bleeding into the one above on
// pathological inputs, so `fuzzy_filter`'s ordering stays deterministic.
const TIER_WEIGHT: i32 = 100_000;
const START_WEIGHT: i32 = 100;
const MAX_START_PENALTY: i32 = 999;
const MAX_LEN_PENALTY: i32 = 99;

/// The token equals the whole target.
const TIER_EXACT: i32 = 3;
/// The token is a prefix of the target.
const TIER_PREFIX: i32 = 2;
/// The token starts right after a word separator (`/ - _ . space`).
const TIER_BOUNDARY: i32 = 1;
/// The token starts mid-word.
const TIER_MID: i32 = 0;

/// Match `query` against `target` with whitespace-tokenized substring
/// semantics.
///
/// Returns `Some(FuzzyMatch)` when every whitespace-separated token of
/// `query` occurs as a contiguous, case-insensitive substring of `target`
/// (in any order, overlaps allowed). An empty or all-whitespace query
/// matches everything with score 0. Returns `None` otherwise.
///
/// Scoring (higher is better, deterministic): a token equal to the whole
/// target beats a target-prefix match, which beats a match starting at a
/// word boundary (start of string or after `/ - _ . space`), which beats a
/// mid-word match; earlier starts rank higher within a tier, and shorter
/// targets break the remaining ties.
pub fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    let tokens: Vec<Vec<char>> = query
        .to_lowercase()
        .split_whitespace()
        .map(|t| t.chars().collect())
        .collect();
    if tokens.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: vec![],
        });
    }

    let target_lower: Vec<char> = target.to_lowercase().chars().collect();

    let mut indices: Vec<usize> = Vec::new();
    let mut tier_sum: i32 = 0;
    let mut start_sum: i32 = 0;
    for token in &tokens {
        let (start, tier) = best_occurrence(token, &target_lower)?;
        tier_sum += tier;
        start_sum += start as i32;
        indices.extend(start..start + token.len());
    }
    indices.sort_unstable();
    indices.dedup();

    let token_chars: usize = tokens.iter().map(Vec::len).sum();
    let excess_len = (target_lower.len() as i32 - token_chars as i32).max(0);
    let score = tier_sum * TIER_WEIGHT
        - start_sum.min(MAX_START_PENALTY) * START_WEIGHT
        - excess_len.min(MAX_LEN_PENALTY);

    Some(FuzzyMatch { score, indices })
}

/// The best occurrence of `token` in `target` (both lowercase, in char
/// space): highest tier first, earliest start on tier ties. `None` when the
/// token never occurs as a contiguous substring.
fn best_occurrence(token: &[char], target: &[char]) -> Option<(usize, i32)> {
    if token.is_empty() || token.len() > target.len() {
        return None;
    }
    let mut best: Option<(usize, i32)> = None;
    for start in 0..=(target.len() - token.len()) {
        if target[start..start + token.len()] != *token {
            continue;
        }
        let tier = occurrence_tier(start, token.len(), target);
        // Scanning left to right: a later occurrence only wins with a
        // strictly better tier, so ties resolve to the earliest start.
        if best.is_none_or(|(_, t)| tier > t) {
            best = Some((start, tier));
        }
    }
    best
}

/// The tier of one occurrence: whole-target exact > target prefix > word
/// boundary > mid-word.
fn occurrence_tier(start: usize, len: usize, target: &[char]) -> i32 {
    if start == 0 {
        if len == target.len() {
            TIER_EXACT
        } else {
            TIER_PREFIX
        }
    } else if is_word_boundary(target[start - 1]) {
        TIER_BOUNDARY
    } else {
        TIER_MID
    }
}

/// Whether `c` separates words in the identifiers these selectors filter.
fn is_word_boundary(c: char) -> bool {
    matches!(c, '/' | '-' | '_' | '.' | ' ')
}

/// Filter and sort items by tokenized substring match against `query`.
///
/// Returns `(original_index, FuzzyMatch)` pairs sorted by score (highest
/// first). The sort is stable, so equal scores keep their input order.
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
    fn whitespace_only_query_matches_everything() {
        let m = fuzzy_match("   ", "anything").unwrap();
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
    fn no_match_returns_none() {
        assert!(fuzzy_match("xyz", "hello").is_none());
    }

    #[test]
    fn case_insensitive() {
        let m = fuzzy_match("hello", "HELLO").unwrap();
        assert_eq!(m.indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn single_char_query() {
        let m = fuzzy_match("a", "abc").unwrap();
        assert_eq!(m.indices, vec![0]);
    }

    /// The user-feedback regression this module was rewritten for: the
    /// subsequence matcher found `glm-5` scattered across
    /// `google/gemini-2.5-pro` (g, l, m, -, 5 in order) and surfaced
    /// unrelated models. A token must now be a contiguous substring.
    #[test]
    fn glm_5_does_not_match_gemini() {
        assert!(fuzzy_match("glm-5", "google/gemini-2.5-pro").is_none());
    }

    /// `glm-5` has no literal substring occurrence in `z-ai/glm-4.5-air`
    /// either — an honest empty result beats a misleading hit.
    #[test]
    fn glm_5_does_not_match_glm_4_5() {
        assert!(fuzzy_match("glm-5", "z-ai/glm-4.5-air").is_none());
    }

    /// Multi-token queries: every token must occur somewhere, so `glm 5`
    /// finds `glm-4.5` models but still never gemini.
    #[test]
    fn two_tokens_match_independently() {
        assert!(fuzzy_match("glm 5", "z-ai/glm-4.5-air").is_some());
        assert!(fuzzy_match("glm 5", "google/gemini-2.5-pro").is_none());
    }

    #[test]
    fn token_order_in_target_is_irrelevant() {
        assert!(fuzzy_match("world hello", "hello world").is_some());
    }

    #[test]
    fn scattered_subsequence_is_not_a_match() {
        assert!(fuzzy_match("abc", "aXbXc").is_none());
        assert!(fuzzy_match("abc", "xxabc").is_some());
    }

    /// Whole-target exact > prefix > word boundary > mid-word.
    #[test]
    fn tier_ordering_exact_prefix_boundary_mid() {
        let exact = fuzzy_match("glm", "glm").unwrap();
        let prefix = fuzzy_match("glm", "glm-4.5-air").unwrap();
        let boundary = fuzzy_match("glm", "z-ai/glm-4.5-air").unwrap();
        let mid = fuzzy_match("glm", "biglm").unwrap();
        assert!(
            exact.score > prefix.score,
            "{} {}",
            exact.score,
            prefix.score
        );
        assert!(
            prefix.score > boundary.score,
            "{} {}",
            prefix.score,
            boundary.score
        );
        assert!(
            boundary.score > mid.score,
            "{} {}",
            boundary.score,
            mid.score
        );
    }

    #[test]
    fn earlier_start_wins_within_a_tier() {
        let early = fuzzy_match("glm", "z/glm-x").unwrap();
        let late = fuzzy_match("glm", "z-longer/glm-x").unwrap();
        assert!(early.score > late.score);
    }

    #[test]
    fn shorter_target_breaks_ties() {
        let short = fuzzy_match("glm", "glm-4").unwrap();
        let long = fuzzy_match("glm", "glm-4.5-air").unwrap();
        assert!(short.score > long.score);
    }

    /// Indices cover the matched spans — one contiguous run per token,
    /// using each token's chosen occurrence.
    #[test]
    fn indices_cover_the_matched_spans() {
        let m = fuzzy_match("lo wor", "hello world").unwrap();
        assert_eq!(m.indices, vec![3, 4, 6, 7, 8]);
    }

    #[test]
    fn overlapping_tokens_dedupe_indices() {
        let m = fuzzy_match("aba ab", "abab").unwrap();
        assert_eq!(m.indices, vec![0, 1, 2]);
    }

    /// An exact match must outrank longer items that share the query as
    /// a prefix, so `fuzzy_filter` keeps the exact hit on top.
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
}
