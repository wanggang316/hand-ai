//! System prompt generation for the coding agent.

use chrono::Local;
use std::path::Path;

/// Options for building the system prompt.
pub struct BuildSystemPromptOptions<'a> {
    /// Working directory.
    pub cwd: &'a Path,
    /// Available tool names.
    pub tools: &'a [String],
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
            tool_section.push_str(
                "- Use `read` to examine file contents before making changes.\n",
            );
        }
        if has_bash && has_grep {
            tool_section.push_str(
                "- Prefer `grep` over `bash` with grep/rg for searching file contents.\n",
            );
        }
        if has_bash && has_find {
            tool_section.push_str(
                "- Prefer `find` over `bash` with find/fd for locating files.\n",
            );
        }
        if has_bash && has_ls {
            tool_section.push_str(
                "- Prefer `ls` over `bash` with ls for listing directories.\n",
            );
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

    // Environment info
    let date = Local::now().format("%Y-%m-%d").to_string();
    let cwd = options.cwd.display();
    sections.push(format!(
        "# Environment\n\n- Date: {}\n- Working directory: {}",
        date, cwd
    ));

    sections.join("\n\n")
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
    use std::path::PathBuf;

    #[test]
    fn test_build_system_prompt_basic() {
        let prompt = build_system_prompt(BuildSystemPromptOptions {
            cwd: &PathBuf::from("/tmp/test"),
            tools: &["read".into(), "bash".into(), "edit".into()],
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
}
