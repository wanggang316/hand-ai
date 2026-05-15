//! Search-query parser and session matcher used by the session-selector UI.
//!
//! A pure-logic module with no UI dependencies. The interactive
//! `SessionSelectorComponent` that consumes it is still in flight.
//!
//! Implementation notes:
//!
//! - [`hand_tui::fuzzy::fuzzy_match`] returns `Option<FuzzyMatch>`
//!   where *higher* is better. The relevance sort feeds `-score` into
//!   the comparator so the composite ordering still picks the best
//!   matches first.
//! - Date/scope filters use case-insensitive `regex_lite::Regex`.
//
// TODO: port the full SessionSelectorComponent interactive UI
// alongside the user-message-selector and the main interactive driver.

use hand_tui::fuzzy::fuzzy_match;
use regex_lite::Regex;

use crate::core::session_manager::SessionInfo;

/// Sort policy applied after filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// Threaded view (the caller passes already-grouped sessions).
    Threaded,
    /// Most recently modified first; filter only, preserve incoming order.
    Recent,
    /// Sort by match score, breaking ties by `modified` desc.
    Relevance,
}

/// Whether to keep all sessions or only those with an explicit name/label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameFilter {
    /// Keep every session.
    #[default]
    All,
    /// Keep only sessions with a non-empty `name`.
    Named,
}

/// Token in a parsed search query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchToken {
    pub kind: TokenKind,
    pub value: String,
}

/// Token classification — fuzzy-match by default, exact phrase when wrapped
/// in `"…"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Fuzzy,
    Phrase,
}

/// Parsed search query, either a list of tokens or a single regex.
#[derive(Debug, Clone)]
pub enum ParsedSearchQuery {
    /// Token-mode (fuzzy + phrase tokens).
    Tokens(Vec<SearchToken>),
    /// `re:<pattern>` mode. `regex` is `None` if compilation failed (the
    /// caller should treat parse-errored queries as matching nothing).
    Regex {
        regex: Option<Regex>,
        error: Option<String>,
    },
}

/// Outcome of matching a session against a parsed query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchResult {
    pub matches: bool,
    /// Lower is better; only meaningful when `matches == true`.
    pub score: f64,
}

impl MatchResult {
    fn no_match() -> Self {
        Self {
            matches: false,
            score: 0.0,
        }
    }
}

/// True iff the session has a non-blank name.
pub fn has_session_name(session: &SessionInfo) -> bool {
    session
        .name
        .as_deref()
        .map(|n| !n.trim().is_empty())
        .unwrap_or(false)
}

fn matches_name_filter(session: &SessionInfo, filter: NameFilter) -> bool {
    match filter {
        NameFilter::All => true,
        NameFilter::Named => has_session_name(session),
    }
}

/// Build the haystack string used for free-text search across a session.
///
/// Mirrors `getSessionSearchText` — concatenates id, name, all message
/// text, and cwd separated by single spaces.
fn session_search_text(session: &SessionInfo) -> String {
    let name = session.name.as_deref().unwrap_or("");
    format!(
        "{} {} {} {}",
        session.id, name, session.all_messages_text, session.cwd
    )
}

fn normalize_whitespace_lower(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Parse a raw query string into a [`ParsedSearchQuery`].
///
/// - empty / whitespace-only → empty token list.
/// - `re:<pattern>` → regex mode (case-insensitive).
/// - otherwise quoted runs become phrase tokens, unquoted
///   whitespace-separated runs become fuzzy tokens.
/// - if quotes are unbalanced, fall back to plain whitespace
///   tokenisation.
pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSearchQuery::Tokens(Vec::new());
    }

    if let Some(rest) = trimmed.strip_prefix("re:") {
        let pattern = rest.trim();
        if pattern.is_empty() {
            return ParsedSearchQuery::Regex {
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        return match Regex::new(&format!("(?i){}", pattern)) {
            Ok(re) => ParsedSearchQuery::Regex {
                regex: Some(re),
                error: None,
            },
            Err(err) => ParsedSearchQuery::Regex {
                regex: None,
                error: Some(err.to_string()),
            },
        };
    }

    let mut tokens: Vec<SearchToken> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut had_unclosed_quote = false;

    fn flush(tokens: &mut Vec<SearchToken>, buf: &mut String, kind: TokenKind) {
        let v = buf.trim();
        if !v.is_empty() {
            tokens.push(SearchToken {
                kind,
                value: v.to_string(),
            });
        }
        buf.clear();
    }

    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                flush(&mut tokens, &mut buf, TokenKind::Phrase);
                in_quote = false;
            } else {
                flush(&mut tokens, &mut buf, TokenKind::Fuzzy);
                in_quote = true;
            }
            continue;
        }
        if !in_quote && ch.is_whitespace() {
            flush(&mut tokens, &mut buf, TokenKind::Fuzzy);
            continue;
        }
        buf.push(ch);
    }

    if in_quote {
        had_unclosed_quote = true;
    }

    if had_unclosed_quote {
        // Fall back to plain whitespace tokenisation; quotes are
        // treated literally as part of fuzzy tokens.
        let tokens = trimmed
            .split_whitespace()
            .map(|t| SearchToken {
                kind: TokenKind::Fuzzy,
                value: t.to_string(),
            })
            .collect();
        return ParsedSearchQuery::Tokens(tokens);
    }

    flush(&mut tokens, &mut buf, TokenKind::Fuzzy);
    ParsedSearchQuery::Tokens(tokens)
}

/// Match a session against a parsed query.
pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchResult {
    let text = session_search_text(session);
    match parsed {
        ParsedSearchQuery::Regex { regex, .. } => match regex {
            None => MatchResult::no_match(),
            Some(re) => match re.find(&text) {
                None => MatchResult::no_match(),
                Some(m) => MatchResult {
                    matches: true,
                    score: m.start() as f64 * 0.1,
                },
            },
        },
        ParsedSearchQuery::Tokens(tokens) => {
            if tokens.is_empty() {
                return MatchResult {
                    matches: true,
                    score: 0.0,
                };
            }
            let mut total_score = 0.0;
            let mut normalized_text: Option<String> = None;
            for token in tokens {
                match token.kind {
                    TokenKind::Phrase => {
                        if normalized_text.is_none() {
                            normalized_text = Some(normalize_whitespace_lower(&text));
                        }
                        let phrase = normalize_whitespace_lower(&token.value);
                        if phrase.is_empty() {
                            continue;
                        }
                        let haystack = normalized_text.as_ref().expect("set above");
                        match haystack.find(&phrase) {
                            None => return MatchResult::no_match(),
                            Some(idx) => total_score += idx as f64 * 0.1,
                        }
                    }
                    TokenKind::Fuzzy => match fuzzy_match(&token.value, &text) {
                        None => return MatchResult::no_match(),
                        // The fuzzy matcher returns higher = better.
                        // Invert so the composite sort can rank by
                        // ascending score and still pick the best
                        // matches first.
                        Some(m) => total_score += -(m.score as f64),
                    },
                }
            }
            MatchResult {
                matches: true,
                score: total_score,
            }
        }
    }
}

/// Combine name-filter + query parsing + sort policy. Returns the
/// sessions that match, in the order the sort policy requests.
pub fn filter_and_sort_sessions(
    sessions: &[SessionInfo],
    query: &str,
    sort_mode: SortMode,
    name_filter: NameFilter,
) -> Vec<SessionInfo> {
    let name_filtered: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|s| matches_name_filter(s, name_filter))
        .collect();

    if query.trim().is_empty() {
        return name_filtered.into_iter().cloned().collect();
    }

    let parsed = parse_search_query(query);
    if let ParsedSearchQuery::Regex {
        regex: None,
        error: Some(_),
    } = &parsed
    {
        return Vec::new();
    }

    if matches!(sort_mode, SortMode::Recent) {
        return name_filtered
            .into_iter()
            .filter(|s| match_session(s, &parsed).matches)
            .cloned()
            .collect();
    }

    let mut scored: Vec<(SessionInfo, f64)> = name_filtered
        .into_iter()
        .filter_map(|s| {
            let r = match_session(s, &parsed);
            if r.matches {
                Some((s.clone(), r.score))
            } else {
                None
            }
        })
        .collect();

    // Sort by score asc; tie-break by `modified` desc.
    scored.sort_by(
        |a, b| match a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal) {
            std::cmp::Ordering::Equal => b.0.modified.cmp(&a.0.modified),
            ord => ord,
        },
    );

    scored.into_iter().map(|(s, _)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(id: &str, name: Option<&str>, body: &str, modified: i64) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from(format!("/tmp/{}.jsonl", id)),
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            timestamp: 0,
            modified,
            message_count: 0,
            name: name.map(str::to_string),
            parent_session_path: None,
            first_message: String::new(),
            all_messages_text: body.to_string(),
        }
    }

    #[test]
    fn parse_empty_query_yields_no_tokens() {
        match parse_search_query("") {
            ParsedSearchQuery::Tokens(t) => assert!(t.is_empty()),
            _ => panic!(),
        }
        match parse_search_query("    ") {
            ParsedSearchQuery::Tokens(t) => assert!(t.is_empty()),
            _ => panic!(),
        }
    }

    #[test]
    fn parse_fuzzy_and_phrase_tokens() {
        let q = parse_search_query(r#"foo "node cve" bar"#);
        match q {
            ParsedSearchQuery::Tokens(tokens) => {
                assert_eq!(tokens.len(), 3);
                assert_eq!(tokens[0].kind, TokenKind::Fuzzy);
                assert_eq!(tokens[0].value, "foo");
                assert_eq!(tokens[1].kind, TokenKind::Phrase);
                assert_eq!(tokens[1].value, "node cve");
                assert_eq!(tokens[2].kind, TokenKind::Fuzzy);
                assert_eq!(tokens[2].value, "bar");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_regex_query() {
        let q = parse_search_query("re:foo|bar");
        match q {
            ParsedSearchQuery::Regex {
                regex: Some(_),
                error: None,
            } => {}
            other => panic!("unexpected: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn parse_invalid_regex_records_error() {
        let q = parse_search_query("re:[");
        match q {
            ParsedSearchQuery::Regex {
                regex: None,
                error: Some(_),
            } => {}
            _ => panic!(),
        }
    }

    #[test]
    fn parse_unbalanced_quote_falls_back_to_plain_split() {
        let q = parse_search_query(r#"foo "bar baz"#);
        match q {
            ParsedSearchQuery::Tokens(tokens) => {
                let values: Vec<&str> = tokens.iter().map(|t| t.value.as_str()).collect();
                assert_eq!(values, vec!["foo", "\"bar", "baz"]);
                assert!(tokens.iter().all(|t| t.kind == TokenKind::Fuzzy));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn empty_query_matches_everything() {
        let s = session("1", None, "hello world", 1);
        let r = match_session(&s, &ParsedSearchQuery::Tokens(Vec::new()));
        assert!(r.matches);
    }

    #[test]
    fn phrase_must_appear_verbatim() {
        let s = session("1", Some("hi"), "the quick brown fox jumped", 1);
        let q = parse_search_query("\"quick brown\"");
        assert!(match_session(&s, &q).matches);
        let q = parse_search_query("\"quick green\"");
        assert!(!match_session(&s, &q).matches);
    }

    #[test]
    fn regex_match_uses_index() {
        let s = session("1", None, "apple banana cherry", 1);
        let q = parse_search_query("re:banana");
        let r = match_session(&s, &q);
        assert!(r.matches);
        assert!(r.score > 0.0);
    }

    #[test]
    fn name_filter_named_skips_unnamed() {
        let unnamed = session("1", None, "x", 1);
        let named = session("2", Some("alpha"), "x", 2);
        let result = filter_and_sort_sessions(
            &[unnamed.clone(), named.clone()],
            "",
            SortMode::Recent,
            NameFilter::Named,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "2");
    }

    #[test]
    fn relevance_sort_prefers_lower_score_then_newer() {
        // "apple" matches once early in s1, later in s2; same modified — s1 wins.
        let s1 = session("1", None, "apple banana", 100);
        let s2 = session("2", None, "banana apple", 200);
        let result = filter_and_sort_sessions(
            &[s1.clone(), s2.clone()],
            "apple",
            SortMode::Relevance,
            NameFilter::All,
        );
        assert_eq!(result.len(), 2);
        // s1 should come first because "apple" appears earlier in the haystack.
        assert_eq!(result[0].id, "1");
    }

    #[test]
    fn recent_mode_keeps_input_order() {
        let s1 = session("1", None, "foo", 100);
        let s2 = session("2", None, "foo", 200);
        // Pass in original order; "Recent" preserves it.
        let result = filter_and_sort_sessions(
            &[s2.clone(), s1.clone()],
            "foo",
            SortMode::Recent,
            NameFilter::All,
        );
        assert_eq!(
            result.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec!["2", "1"]
        );
    }

    #[test]
    fn errored_regex_returns_empty() {
        let s = session("1", None, "x", 1);
        let result = filter_and_sort_sessions(&[s], "re:[", SortMode::Recent, NameFilter::All);
        assert!(result.is_empty());
    }

    #[test]
    fn has_session_name_distinguishes_blank() {
        let s_blank = session("1", Some("   "), "x", 1);
        let s_named = session("2", Some("alpha"), "x", 1);
        let s_none = session("3", None, "x", 1);
        assert!(!has_session_name(&s_blank));
        assert!(has_session_name(&s_named));
        assert!(!has_session_name(&s_none));
    }
}
