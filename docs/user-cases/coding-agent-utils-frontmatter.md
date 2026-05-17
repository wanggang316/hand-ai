# User-Cases: utils/frontmatter

**Upstream source:** `pi-mono/packages/coding-agent/test/frontmatter.test.ts` (8 cases — 6 `parseFrontmatter`, 2 `stripFrontmatter`)
**hand-ai source:**   `crates/coding-agent/src/utils/frontmatter.rs`
**Surface:**          `parse_frontmatter::<T>(input) -> Result<ParsedFrontmatter<T>, FrontmatterError>` — splits the leading `---\n<yaml>\n---\n` envelope from the body. Accepts CRLF, tolerates a UTF-8 BOM, returns the body verbatim when no frontmatter is present.

## API delta

pi exposes two helpers; hand exposes one (richer) helper plus an explicit error type:

| pi capability | hand |
|---|---|
| `parseFrontmatter<T>(input)` returns `{ frontmatter, body }` always — throws only on invalid YAML; an unterminated `---` opener silently returns the body | `parse_frontmatter::<T>(input) -> Result<…, FrontmatterError>` — unterminated frontmatter is an explicit error variant (`UnterminatedFrontmatter`) so callers can decide whether to recover or surface |
| `stripFrontmatter(input)` — removes frontmatter, `.trim()`s body | no dedicated helper; callers compose `parse_frontmatter(...).body.trim()` |

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-fm-001 | ✅ pass | `parses_basic_frontmatter` + `quoted_string_with_colons` — keys parsed, surrounding quotes stripped |
| UC-fm-002 | ✅ pass | `crlf_line_endings` + `crlf_closer_at_eof` — CRLF opener and closer both accepted; body normalised |
| UC-fm-003 | ✅ pass | `invalid_yaml_errors` — malformed YAML surfaces `FrontmatterError::InvalidYaml(serde_yaml::Error)` carrying the underlying line/column |
| UC-fm-004 | ✅ pass | `multiline_literal_block_scalar` (and `multiline_folded_block_scalar`) — `|` and `>` block scalars round-trip through `serde_yaml` |
| UC-fm-005 | ✅ pass (no-frontmatter half) / 🚫 N/A (unterminated half) — `no_frontmatter_returns_body_only` covers the "no opener" branch; the "opener but no closer" branch deliberately returns `UnterminatedFrontmatter` rather than silently returning the body. pi's silent fallback was a footgun — hand wants the caller to opt into recovery. |
| UC-fm-006 | ✅ pass | `comment_only_frontmatter_yields_null` — comment-only YAML deserializes to `serde_yaml::Value::Null`; semantically equivalent to pi's `{}` (no parseable keys) |
| UC-fm-007 | 🚫 N/A | hand has no dedicated `strip_frontmatter` helper. Callers compose `parse_frontmatter(input).body.trim_matches('\n')`; adding a separate helper would duplicate logic. |
| UC-fm-008 | 🚫 N/A | same — no `strip_frontmatter` helper. |

## Bonus coverage hand carries beyond pi

- `bom_prefixed_frontmatter_parses` — UTF-8 BOM at byte 0 is stripped before the `---\n` opener check.
- `body_contains_triple_dash` — `---` inside the body (not at line start) is preserved verbatim.
- `body_preserves_leading_newlines` — multiple blank lines after the closer survive.
- `just_open_and_close` — `---\n---` with no body or YAML.
- `single_line_no_newlines_is_body` — bare `---name: foo---` is body-only (no opener).

## Cases (load-bearing)

### UC-fm-001 — keys parse, surrounding quotes strip, body returns verbatim

**Given** the input
```
---
name: "skill-name"
description: 'A desc'
foo-bar: value
---

Body text
```

**When** `parse_frontmatter::<HashMap<String, String>>(input)` runs.
**Then** the metadata `HashMap` has the three keys (quotes stripped); the body equals `"\nBody text"` (the blank line between `---` and content is preserved as a leading `\n`; pi's `.toBe("Body text")` reflects a tighter trim that hand's caller can apply with `.trim_matches('\n')` if desired).

- Probe: `cargo test -p hand-coding-agent parses_basic_frontmatter quoted_string_with_colons -- --exact`.

### UC-fm-003 — invalid YAML surfaces a precise error

**Given** `---\nfoo: [bar\n---\nBody`.
**When** `parse_frontmatter` runs.
**Then** it returns `Err(FrontmatterError::InvalidYaml(_))` carrying the `serde_yaml::Error` which encodes the line/column for the broken `[bar` flow-sequence.

- Probe: `cargo test -p hand-coding-agent invalid_yaml_errors -- --exact`.

### UC-fm-005 — divergence on unterminated frontmatter (deliberate)

**Given** an input that opens with `---\n` but has no closing `---` line.
**Then** hand returns `Err(FrontmatterError::UnterminatedFrontmatter)`. pi silently returns the body. The hand-side explicit error is the documented behaviour; callers wanting pi's fallback compose `.unwrap_or_else(|_| ParsedFrontmatter { metadata: None, body: input.to_string() })`.

- Probe: `cargo test -p hand-coding-agent unterminated_frontmatter_errors -- --exact`.
