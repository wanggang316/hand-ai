//! System prompt generation for the coding agent.

use crate::core::skills::Skill;
use chrono::Local;
use std::path::Path;

/// Options for building the system prompt.
pub struct BuildSystemPromptOptions<'a> {
    /// Working directory.
    pub cwd: &'a Path,
    /// Available tool names.
    pub tools: &'a [String],
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
    // If custom prompt provided, use it directly
    if let Some(custom) = options.custom_prompt {
        return custom.to_string();
    }

    let mut sections = Vec::new();

    // Core identity
    sections.push(
        "You are Hand, an interactive AI coding assistant. You help users with software \
         engineering tasks including writing code, debugging, refactoring, and explaining code."
            .to_string(),
    );

    // Tool instructions
    if !options.tools.is_empty() {
        let mut tool_section = String::from("# Available Tools\n\n");
        tool_section.push_str("You have access to the following tools:\n");
        for tool in options.tools {
            tool_section.push_str(&format!("- {}\n", tool));
        }
        tool_section.push_str("\n## Tool Usage Guidelines\n\n");

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

        sections.push(tool_section);
    }

    // Custom guidelines
    if let Some(guidelines) = options.custom_guidelines {
        sections.push(format!("# Project Guidelines\n\n{}", guidelines));
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
    if read_available && !options.skills.is_empty()
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
    out.push_str("Use the read tool to load a skill's file when the task matches its description.\n");
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

/// Load context files (HAND.md) from the working directory.
pub fn load_context_files(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();

    // Check for HAND.md in cwd
    let hand_md = cwd.join("HAND.md");
    if let Ok(content) = std::fs::read_to_string(&hand_md) {
        files.push(content);
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

    #[test]
    fn test_build_system_prompt_custom_override() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
            skills: &[],
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: Some("You are a custom bot."),
        });
        assert_eq!(prompt, "You are a custom bot.");
    }

    #[test]
    fn test_build_system_prompt_with_guidelines() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &[],
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

    // T2.5 — Skills section emission.

    /// Empty `skills` produces output identical to pre-T2.5 (no Skills
    /// section, no `<available_skills>` tag).
    #[test]
    fn skills_empty_omits_section() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp"),
            tools: &["read".into()],
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
            skills: std::slice::from_ref(&skill),
            custom_guidelines: None,
            context_files: vec![],
            custom_prompt: None,
        });
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>alpha</name>"));
    }
}
