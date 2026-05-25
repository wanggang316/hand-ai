//! System prompt generation for the coding agent.

use crate::core::skills::Skill;
use chrono::Local;
use std::path::Path;

/// Render the `# Project Guidelines` section from a single
/// `custom_guidelines` string. The string is split on blank-line
/// separators (so callers can pack multiple guideline entries with
/// `\n\n` between them — that's exactly how
/// `--append-system-prompt` flags compose in session_setup),
/// trimmed, deduplicated, and dropped if empty. Returns `None` when
/// nothing survives the cleanup so the caller can skip emitting the
/// header entirely.
///
/// The rendered shape is a bulleted list under the header so
/// downstream parsers and human readers see each guideline as a
/// distinct item:
///
/// ```text
/// # Project Guidelines
///
/// - First guideline.
/// - Second guideline.
/// ```
fn render_project_guidelines(custom_guidelines: Option<&str>) -> Option<String> {
    let raw = custom_guidelines?;
    let mut seen = std::collections::HashSet::new();
    let mut bullets: Vec<&str> = Vec::new();
    for entry in raw.split("\n\n") {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed) {
            bullets.push(trimmed);
        }
    }
    if bullets.is_empty() {
        return None;
    }
    let body = bullets
        .iter()
        .map(|b| format!("- {b}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("# Project Guidelines\n\n{body}"))
}

/// Options for building the system prompt.
pub struct BuildSystemPromptOptions<'a> {
    /// Working directory.
    pub cwd: &'a Path,
    /// Available tool names.
    pub tools: &'a [String],
    /// Optional per-tool description snippets. When supplied, the
    /// `# Available Tools` section renders each entry as
    /// `- <name>: <snippet>` (vs `- <name>` alone). Tools NOT in the
    /// map fall back to their bare-name rendering OR — for the seven
    /// builtin tool names — to hand's hard-coded usage guidelines
    /// further down the section.
    ///
    /// Used by extensions and dynamic tool registries to advertise
    /// the tool surface to the model without needing the prompt
    /// builder to know each tool's purpose. `None` falls back to the
    /// bare-name listing (no per-tool annotations).
    pub tool_snippets: Option<&'a std::collections::HashMap<String, String>>,
    /// Discovered skills to advertise to the model.
    pub skills: &'a [Skill],
    /// Custom guidelines to append.
    pub custom_guidelines: Option<&'a str>,
    /// Context files content (e.g., HAND.md).
    pub context_files: Vec<String>,
    /// Custom system prompt override.
    pub custom_prompt: Option<&'a str>,
}

/// Build the system prompt for the coding agent.
pub fn build_system_prompt(options: BuildSystemPromptOptions<'_>) -> String {
    // If a custom prompt is provided, use it as the base — but still
    // append project guidelines from --append-system-prompt so the two
    // flags compose. An earlier implementation short-circuited here
    // and silently dropped --append-system-prompt when --system-prompt
    // was set.
    if let Some(custom) = options.custom_prompt {
        let mut out = custom.to_string();
        if let Some(rendered) = render_project_guidelines(options.custom_guidelines) {
            out.push_str("\n\n");
            out.push_str(&rendered);
        }
        return out;
    }

    let mut sections = Vec::new();

    // Core identity
    sections.push(
        "You are Hand, an interactive AI coding assistant. You help users with software \
         engineering tasks including writing code, debugging, refactoring, and explaining code."
            .to_string(),
    );

    // Tool instructions. Always emit the section header — even when no
    // tools are selected — so the model sees an explicit "no tools"
    // signal rather than silently inferring it from absence. An empty
    // list renders as `Available tools:\n(none)`.
    let mut tool_section = String::from("# Available Tools\n\n");
    if options.tools.is_empty() {
        tool_section.push_str("Available tools:\n(none)\n\n");
    } else {
        tool_section.push_str("Available tools:\n");
        for tool in options.tools {
            if let Some(snippet) = options
                .tool_snippets
                .and_then(|m| m.get(tool))
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            {
                tool_section.push_str(&format!("- {}: {}\n", tool, snippet));
            } else {
                tool_section.push_str(&format!("- {}\n", tool));
            }
        }
        tool_section.push('\n');
    }
    tool_section.push_str("## Tool Usage Guidelines\n\n");
    tool_section
        .push_str("- Show file paths clearly when referring to code locations in responses.\n");
    if !options.tools.is_empty() {
        let has_read = options.tools.iter().any(|t| t == "read");
        let has_bash = options.tools.iter().any(|t| t == "bash");
        let has_grep = options.tools.iter().any(|t| t == "grep");
        let has_find = options.tools.iter().any(|t| t == "find");
        let has_ls = options.tools.iter().any(|t| t == "ls");
        let has_edit = options.tools.iter().any(|t| t == "edit");
        let has_write = options.tools.iter().any(|t| t == "write");

        if has_read {
            tool_section.push_str("- Use `read` to examine file contents before making changes.\n");
        }
        if has_bash && has_grep {
            tool_section.push_str(
                "- Prefer `grep` over `bash` with grep/rg for searching file contents.\n",
            );
        }
        if has_bash && has_find {
            tool_section.push_str("- Prefer `find` over `bash` with find/fd for locating files.\n");
        }
        if has_bash && has_ls {
            tool_section.push_str("- Prefer `ls` over `bash` with ls for listing directories.\n");
        }
        if has_edit && has_write {
            tool_section.push_str(
                "- Prefer `edit` for modifying existing files; use `write` only for new files.\n",
            );
        }
        if has_bash {
            tool_section.push_str(
                "- Use `bash` for running tests, builds, git commands, and other shell tasks.\n",
            );
        }
    }
    sections.push(tool_section);

    // Custom guidelines — rendered as a bulleted list under
    // `# Project Guidelines`. Entries are split on blank-line separators
    // (mirrors how `--append-system-prompt` flags compose), trimmed of
    // surrounding whitespace, deduplicated, and dropped if empty.
    if let Some(rendered) = render_project_guidelines(options.custom_guidelines) {
        sections.push(rendered);
    }

    // Context files
    for content in &options.context_files {
        if !content.trim().is_empty() {
            sections.push(format!("# Project Context\n\n{}", content));
        }
    }

    // Skills section. Gate on read tool when tools are explicitly listed
    // (matches TS: skills are only useful if the model can read SKILL.md).
    // An empty tools list is treated as "no restriction" (TS `!selectedTools`).
    let read_available = options.tools.is_empty() || options.tools.iter().any(|t| t == "read");
    if read_available
        && !options.skills.is_empty()
        && let Some(section) = format_skills_section(options.skills)
    {
        sections.push(section);
    }

    // Environment info
    let date = Local::now().format("%Y-%m-%d").to_string();
    let cwd = options.cwd.display();
    sections.push(format!(
        "# Environment\n\n- Date: {}\n- Working directory: {}",
        date, cwd
    ));

    sections.join("\n\n")
}

/// Format the Skills section, mirroring the TS `formatSkillsForPrompt`
/// helper (XML/Agent-Skills layout) with one local addition: skills marked
/// `disable_model_invocation` are still listed but tagged with an
/// `opt-in="true"` attribute so the model knows it must not auto-invoke.
///
/// Returns `None` if the input is empty (no section emitted at all).
/// Skills are sorted alphabetically by name for byte-deterministic output.
fn format_skills_section(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut sorted: Vec<&Skill> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = String::new();
    out.push_str("The following skills provide specialized instructions for specific tasks.\n");
    out.push_str(
        "Use the read tool to load a skill's file when the task matches its description.\n",
    );
    out.push_str(
        "When a skill file references a relative path, resolve it against the skill directory \
         (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n\n",
    );
    out.push_str("<available_skills>\n");
    for skill in sorted {
        if skill.disable_model_invocation {
            out.push_str("  <skill opt-in=\"true\">\n");
        } else {
            out.push_str("  <skill>\n");
        }
        out.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            escape_xml(&skill.description)
        ));
        out.push_str(&format!(
            "    <location>{}</location>\n",
            escape_xml(&skill.source.path.display().to_string())
        ));
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    Some(out)
}

/// Minimal XML entity escape for the skills section. Matches the TS
/// `escapeXml` helper character-for-character so the resulting prompt
/// is byte-equivalent across implementations.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Load context files (HAND.md / HAND.MD) from the working directory.
///
/// Case-insensitive resolution mirrors users who land on a project whose
/// context file shipped with an uppercase extension (e.g. created on a
/// case-insensitive filesystem). The lowercase variant wins when both
/// exist so a project that ships `HAND.md` plus a stray `HAND.MD` from a
/// merge conflict still gets a single, deterministic file.
pub fn load_context_files(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();

    for candidate in ["HAND.md", "HAND.MD"] {
        let path = cwd.join(candidate);
        if let Ok(content) = std::fs::read_to_string(&path) {
            files.push(content);
            break;
        }
    }

    // Check for .hand/context.md
    let context_md = cwd.join(".hand").join("context.md");
    if let Ok(content) = std::fs::read_to_string(&context_md) {
        files.push(content);
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::source_info::SourceInfo;
    use std::path::PathBuf;

    fn make_skill(name: &str, description: &str, disable: bool) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            body: String::new(),
            disable_model_invocation: disable,
            source: SourceInfo::project(format!("/skills/{}/SKILL.md", name)),
        }
    }

    #[test]
    fn test_build_system_prompt_basic() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp/test"),
            tools: &["read".into(), "bash".into(), "edit".into()],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("Hand"));
        assert!(prompt.contains("read"));
        assert!(prompt.contains("bash"));
        assert!(prompt.contains("/tmp/test"));
    }

    /// Empty `tools` slice MUST still emit the Available-tools section
    /// with `(none)` as the explicit no-tools placeholder. Silently
    /// dropping the whole section forces the model to infer absence
    /// from a non-existent header, which has produced confused
    /// behaviour in the past (model assumed tools were forthcoming).
    /// pi anchors this contract in its parse-time tests.
    #[test]
    fn empty_tools_emits_none_placeholder() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(
            prompt.contains("Available tools:\n(none)"),
            "must show `Available tools:\\n(none)` for empty tools, got: {prompt}"
        );
    }

    /// The `Show file paths clearly` guideline is anchored regardless
    /// of which tools are in scope; it's a global UX rule for the
    /// model's responses, not a per-tool tip.
    #[test]
    fn show_file_paths_guideline_always_present() {
        let prompt_empty_tools = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(
            prompt_empty_tools.contains("Show file paths clearly"),
            "missing guideline on empty-tools prompt: {prompt_empty_tools}"
        );
        let prompt_with_tools = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(
            prompt_with_tools.contains("Show file paths clearly"),
            "missing guideline on tools-listed prompt: {prompt_with_tools}"
        );
    }

    #[test]
    fn test_build_system_prompt_custom_override() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: Some("You are a custom bot."),
        });
        assert_eq!(prompt, "You are a custom bot.");
    }

    /// `--system-prompt X --append-system-prompt Y` must produce a
    /// prompt containing BOTH. Previously the custom prompt short-
    /// circuited the builder before guidelines were appended,
    /// silently dropping --append-system-prompt.
    #[test]
    fn test_custom_prompt_composes_with_guidelines() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: Some("Always include M_TOKEN."),
            context_files: vec![],
            custom_prompt: Some("You are CORE."),
        });
        assert!(
            prompt.contains("You are CORE."),
            "must keep custom prompt body, got: {prompt}"
        );
        assert!(
            prompt.contains("Always include M_TOKEN."),
            "must also append guidelines when both flags present, got: {prompt}"
        );
        assert!(
            prompt.contains("# Project Guidelines"),
            "must label the appended section, got: {prompt}"
        );
    }

    /// When `tool_snippets` supplies a description for a custom tool,
    /// the tool surfaces as `- <name>: <snippet>` in the Available
    /// Tools section so the model sees what the tool does without
    /// hand needing to know the tool's purpose at compile time.
    #[test]
    fn tool_snippets_render_custom_tool_with_description() {
        let mut snippets = std::collections::HashMap::new();
        snippets.insert(
            "dynamic_tool".to_string(),
            "Run dynamic test behaviour".to_string(),
        );
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into(), "dynamic_tool".into()],
            tool_snippets: Some(&snippets),
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(
            prompt.contains("- dynamic_tool: Run dynamic test behaviour"),
            "missing annotated custom tool, got: {prompt}"
        );
    }

    /// When NO `tool_snippets` are provided, custom tool names still
    /// surface — just as bare `- <name>` lines, without per-tool
    /// description text. The model still sees the tool is available;
    /// it just doesn't get an annotation.
    #[test]
    fn tool_snippets_absent_falls_back_to_bare_name_listing() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into(), "dynamic_tool".into()],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(
            prompt.contains("- dynamic_tool\n"),
            "bare name should still surface, got: {prompt}"
        );
        // Without a snippet there must NOT be a colon-prefixed
        // description following the name.
        assert!(
            !prompt.contains("- dynamic_tool:"),
            "annotated form leaked, got: {prompt}"
        );
    }

    /// Each `--append-system-prompt` entry renders as its own bulleted
    /// line under `# Project Guidelines`. session_setup.rs joins the
    /// flag's repeated values with `\n\n` before passing them through
    /// `custom_guidelines`; the system-prompt builder splits on that
    /// separator and emits one `-` bullet per entry.
    #[test]
    fn append_system_prompt_entries_render_as_separate_bullets() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: Some("First entry.\n\nSecond entry."),
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(
            prompt.contains("# Project Guidelines"),
            "must emit guidelines header, got: {prompt}"
        );
        assert!(
            prompt.contains("- First entry."),
            "missing first bullet, got: {prompt}"
        );
        assert!(
            prompt.contains("- Second entry."),
            "missing second bullet, got: {prompt}"
        );
    }

    /// Duplicate guideline entries (after trimming surrounding
    /// whitespace) collapse to one bullet. Whitespace-only entries
    /// drop entirely.
    #[test]
    fn append_system_prompt_dedups_and_trims() {
        // Three "entries":
        //   - "Use X for summaries."
        //   - "  Use X for summaries.  " (whitespace differs only)
        //   - "   " (whitespace-only)
        // The first two collapse; the third drops.
        let raw = "Use X for summaries.\n\n  Use X for summaries.  \n\n   ";
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: Some(raw),
            context_files: vec![],
            custom_prompt: None,
        });
        let bullet_count = prompt.matches("- Use X for summaries.").count();
        assert_eq!(
            bullet_count, 1,
            "duplicate entries must collapse to one bullet, got {bullet_count} occurrences in: {prompt}"
        );
    }

    /// When --system-prompt is set but --append-system-prompt is NOT,
    /// the output is the custom prompt verbatim (no trailing section).
    #[test]
    fn test_custom_prompt_alone_is_verbatim() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: Some("Solo prompt."),
        });
        assert_eq!(prompt, "Solo prompt.");
    }

    #[test]
    fn test_build_system_prompt_with_guidelines() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: Some("Always use TypeScript."),
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("Always use TypeScript."));
    }

    #[test]
    fn test_build_system_prompt_with_context_files() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec!["This project uses Rust.".into()],
            custom_prompt: None,
        });
        assert!(prompt.contains("This project uses Rust."));
    }

    #[test]
    fn test_tool_guidelines_generated() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[
                "read".into(),
                "write".into(),
                "edit".into(),
                "bash".into(),
                "grep".into(),
                "find".into(),
                "ls".into(),
            ],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("Prefer `grep` over `bash`"));
        assert!(prompt.contains("Prefer `find` over `bash`"));
        assert!(prompt.contains("Prefer `edit` for modifying"));
    }

    #[test]
    fn test_load_context_files_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let files = load_context_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn test_load_context_files_with_hand_md() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("HAND.md"), "# My Project").unwrap();
        let files = load_context_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "# My Project");
    }

    /// A project that shipped `HAND.MD` (uppercase ext) must still load.
    /// Users hopping between case-insensitive filesystems often end up
    /// with the uppercase variant; silently ignoring it loses context.
    #[test]
    fn test_load_context_files_with_uppercase_hand_md() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("HAND.MD"), "# upper-case ext").unwrap();
        let files = load_context_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "# upper-case ext");
    }

    /// When both `HAND.md` and `HAND.MD` exist, the canonical lowercase
    /// variant wins — the candidate order pins the resolution.
    #[test]
    fn test_load_context_files_prefers_lowercase_when_both_exist() {
        let dir = tempfile::TempDir::new().unwrap();
        // On case-insensitive filesystems (macOS default APFS) both
        // paths point at the same file. Skip the test there because we
        // can't reliably create two distinct entries.
        let lower = dir.path().join("HAND.md");
        let upper = dir.path().join("HAND.MD");
        std::fs::write(&lower, "lowercase wins").unwrap();
        if std::fs::write(&upper, "uppercase loses").is_err() {
            return;
        }
        if std::fs::read_to_string(&lower).ok().as_deref() != Some("lowercase wins") {
            // Filesystem collapsed the two writes — nothing to assert.
            return;
        }
        let files = load_context_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "lowercase wins");
    }

    // T2.5 — Skills section emission.

    /// Empty `skills` produces output identical to pre-T2.5 (no Skills
    /// section, no `<available_skills>` tag).
    #[test]
    fn skills_empty_omits_section() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(!prompt.contains("<available_skills>"));
        assert!(!prompt.contains("specialized instructions"));
    }

    /// One skill produces a full Skills section listing it once.
    #[test]
    fn skills_single_entry_rendered() {
        let skill = make_skill("valid-skill", "Foo bar", false);
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: std::slice::from_ref(&skill),
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>valid-skill</name>"));
        assert!(prompt.contains("<description>Foo bar</description>"));
        assert!(prompt.contains("</available_skills>"));
    }

    /// Skills are emitted in alphabetical order regardless of input order.
    #[test]
    fn skills_sorted_alphabetically() {
        let skills = vec![
            make_skill("zebra", "z", false),
            make_skill("apple", "a", false),
            make_skill("mango", "m", false),
        ];
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: &skills,
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        let apple_pos = prompt.find("<name>apple</name>").expect("apple present");
        let mango_pos = prompt.find("<name>mango</name>").expect("mango present");
        let zebra_pos = prompt.find("<name>zebra</name>").expect("zebra present");
        assert!(apple_pos < mango_pos);
        assert!(mango_pos < zebra_pos);
    }

    /// `disable_model_invocation` skills are still listed but tagged so the
    /// model knows not to auto-invoke them.
    #[test]
    fn skills_opt_in_marker_emitted() {
        let skills = vec![make_skill("manual-only", "Opt-in skill", true)];
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: &skills,
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("<skill opt-in=\"true\">"));
        assert!(prompt.contains("<name>manual-only</name>"));
    }

    /// Multiline descriptions are preserved verbatim (no clipping, no
    /// indentation rewriting); we just emit them XML-escaped as-is.
    #[test]
    fn skills_multiline_description_preserved() {
        let skills = vec![make_skill(
            "multi",
            "Line one.\nLine two.\nLine three.",
            false,
        )];
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: &skills,
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("Line one.\nLine two.\nLine three."));
    }

    /// XML-special chars in a description are escaped, not double-escaped.
    #[test]
    fn skills_special_chars_xml_escaped() {
        let skills = vec![make_skill(
            "edge-cases",
            "uses <tag> & \"quotes\" and 'apos'",
            false,
        )];
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
            tool_snippets: None,
            skills: &skills,
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        // Escaped once.
        assert!(prompt.contains("uses &lt;tag&gt; &amp; &quot;quotes&quot; and &apos;apos&apos;"));
        // Not double-escaped.
        assert!(!prompt.contains("&amp;lt;"));
        assert!(!prompt.contains("&amp;amp;"));
    }

    /// Skills are omitted when the `read` tool is unavailable in an
    /// explicit (non-empty) tool list — matches TS behavior.
    #[test]
    fn skills_gated_on_read_tool() {
        let skill = make_skill("alpha", "desc", false);
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["bash".into(), "edit".into()],
            tool_snippets: None,
            skills: std::slice::from_ref(&skill),
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(!prompt.contains("<available_skills>"));
    }

    /// Empty tools list is treated as "no restriction" (TS `!selectedTools`
    /// short-circuit) — skills are still emitted.
    #[test]
    fn skills_empty_tools_emits_section() {
        let skill = make_skill("alpha", "desc", false);
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            tool_snippets: None,
            skills: std::slice::from_ref(&skill),
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>alpha</name>"));
    }
}
