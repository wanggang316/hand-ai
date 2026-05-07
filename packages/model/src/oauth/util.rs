//! Small parsing helpers shared between provider implementations.

/// Split a request-target like `/callback?code=abc&state=xyz` into
/// `(path, query)` components. Anything past the first `?` is the query.
pub(crate) fn split_path_query(url: &str) -> (&str, &str) {
    match url.find('?') {
        Some(i) => (&url[..i], &url[i + 1..]),
        None => (url, ""),
    }
}

/// Parse an `application/x-www-form-urlencoded` query string into key/value
/// pairs. Performs full percent-decoding (and `+` -> space) on both sides.
pub(crate) fn parse_query(query: &str) -> Vec<(String, String)> {
    if query.is_empty() {
        return Vec::new();
    }
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("");
            let v = it.next().unwrap_or("");
            (decode_form(k), decode_form(v))
        })
        .collect()
}

fn decode_form(input: &str) -> String {
    let replaced = input.replace('+', " ");
    urlencoding::decode(&replaced)
        .map(|cow| cow.into_owned())
        .unwrap_or(replaced)
}
