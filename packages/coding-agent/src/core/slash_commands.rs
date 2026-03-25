//! Slash command system — parse and execute `/command` inputs.

use std::collections::HashMap;

/// A registered slash command.
#[derive(Clone)]
pub struct SlashCommand {
    /// Primary command name (without `/`).
    pub name: String,
    /// Alternative names.
    pub aliases: Vec<String>,
    /// Description shown in `/help`.
    pub description: String,
    /// Whether this command accepts arguments.
    pub accepts_args: bool,
}

impl std::fmt::Debug for SlashCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlashCommand")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("description", &self.description)
            .finish()
    }
}

/// Registry that holds all available slash commands.
#[derive(Debug, Default)]
pub struct SlashCommandRegistry {
    commands: Vec<SlashCommand>,
    /// Map from name/alias to command index.
    lookup: HashMap<String, usize>,
}

impl SlashCommandRegistry {
    /// Create a new registry with built-in commands.
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry.register_builtins();
        registry
    }

    /// Register a command.
    pub fn register(&mut self, cmd: SlashCommand) {
        let idx = self.commands.len();
        self.lookup.insert(cmd.name.clone(), idx);
        for alias in &cmd.aliases {
            self.lookup.insert(alias.clone(), idx);
        }
        self.commands.push(cmd);
    }

    /// Look up a command by name or alias.
    pub fn find(&self, name: &str) -> Option<&SlashCommand> {
        self.lookup.get(name).map(|&idx| &self.commands[idx])
    }

    /// Get all registered commands.
    pub fn commands(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// Get help text listing all commands.
    pub fn help_text(&self) -> String {
        let mut lines = vec!["Available commands:".to_string(), String::new()];
        let max_name_len = self
            .commands
            .iter()
            .map(|c| c.name.len())
            .max()
            .unwrap_or(0);

        for cmd in &self.commands {
            let aliases = if cmd.aliases.is_empty() {
                String::new()
            } else {
                format!(" ({})", cmd.aliases.join(", "))
            };
            lines.push(format!(
                "  /{:<width$}{} — {}",
                cmd.name,
                aliases,
                cmd.description,
                width = max_name_len
            ));
        }
        lines.join("\n")
    }

    fn register_builtins(&mut self) {
        let builtins = vec![
            ("help", vec![], "Show available commands", false),
            ("quit", vec!["exit", "q"], "Quit the agent", false),
            ("model", vec![], "Switch model", true),
            ("models", vec![], "List available models", true),
            ("session", vec![], "Show session info", false),
            ("settings", vec![], "Show current settings", false),
            (
                "thinking",
                vec![],
                "Set thinking level (minimal/low/medium/high/xhigh)",
                true,
            ),
            ("compact", vec![], "Manually compact context", true),
            ("new", vec![], "Start a new session", false),
            ("resume", vec![], "Browse and select session", true),
            ("name", vec![], "Set session display name", true),
            ("fork", vec![], "Fork current session", true),
            ("export", vec![], "Export session to file", true),
            (
                "copy",
                vec![],
                "Copy last assistant message to clipboard",
                false,
            ),
            ("hotkeys", vec![], "Show keyboard shortcuts", false),
            ("changelog", vec![], "Display version info", false),
            ("tree", vec![], "Show file tree of directory", true),
        ];

        for (name, aliases, desc, accepts_args) in builtins {
            self.register(SlashCommand {
                name: name.to_string(),
                aliases: aliases.into_iter().map(String::from).collect(),
                description: desc.to_string(),
                accepts_args,
            });
        }
    }
}

/// Parsed slash command with name and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Command name (without `/`).
    pub name: String,
    /// Arguments (everything after the command name).
    pub args: Vec<String>,
}

/// Parse a slash command from user input.
///
/// Returns `None` if the input doesn't start with `/`.
pub fn parse_slash_command(input: &str) -> Option<ParsedCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let without_slash = &trimmed[1..];
    let parts: Vec<&str> = without_slash.splitn(2, char::is_whitespace).collect();

    let name = parts[0].to_lowercase();
    let args = if parts.len() > 1 {
        parts[1].split_whitespace().map(String::from).collect()
    } else {
        Vec::new()
    };

    if name.is_empty() {
        return None;
    }

    Some(ParsedCommand { name, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_command() {
        let cmd = parse_slash_command("/help").unwrap();
        assert_eq!(cmd.name, "help");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn parse_command_with_args() {
        let cmd = parse_slash_command("/model gpt-4o").unwrap();
        assert_eq!(cmd.name, "model");
        assert_eq!(cmd.args, vec!["gpt-4o"]);
    }

    #[test]
    fn parse_command_with_multiple_args() {
        let cmd = parse_slash_command("/name my session name").unwrap();
        assert_eq!(cmd.name, "name");
        assert_eq!(cmd.args, vec!["my", "session", "name"]);
    }

    #[test]
    fn parse_non_command_returns_none() {
        assert!(parse_slash_command("hello").is_none());
        assert!(parse_slash_command("").is_none());
    }

    #[test]
    fn parse_just_slash_returns_none() {
        assert!(parse_slash_command("/").is_none());
    }

    #[test]
    fn parse_command_case_insensitive() {
        let cmd = parse_slash_command("/HELP").unwrap();
        assert_eq!(cmd.name, "help");
    }

    #[test]
    fn parse_command_with_leading_whitespace() {
        let cmd = parse_slash_command("  /help").unwrap();
        assert_eq!(cmd.name, "help");
    }

    #[test]
    fn registry_find_by_name() {
        let registry = SlashCommandRegistry::new();
        assert!(registry.find("help").is_some());
        assert!(registry.find("model").is_some());
    }

    #[test]
    fn registry_find_by_alias() {
        let registry = SlashCommandRegistry::new();
        assert!(registry.find("q").is_some());
        assert!(registry.find("exit").is_some());
    }

    #[test]
    fn registry_find_unknown() {
        let registry = SlashCommandRegistry::new();
        assert!(registry.find("nonexistent").is_none());
    }

    #[test]
    fn registry_help_text() {
        let registry = SlashCommandRegistry::new();
        let help = registry.help_text();
        assert!(help.contains("/help"));
        assert!(help.contains("/model"));
        assert!(help.contains("/quit"));
    }

    #[test]
    fn registry_commands_not_empty() {
        let registry = SlashCommandRegistry::new();
        assert!(registry.commands().len() >= 15);
    }

    #[test]
    fn registry_custom_command() {
        let mut registry = SlashCommandRegistry::new();
        let initial = registry.commands().len();
        registry.register(SlashCommand {
            name: "custom".to_string(),
            aliases: vec!["c".to_string()],
            description: "Custom command".to_string(),
            accepts_args: false,
        });
        assert_eq!(registry.commands().len(), initial + 1);
        assert!(registry.find("custom").is_some());
        assert!(registry.find("c").is_some());
    }

    #[test]
    fn slash_command_debug() {
        let cmd = SlashCommand {
            name: "test".to_string(),
            aliases: vec![],
            description: "Test".to_string(),
            accepts_args: false,
        };
        let debug = format!("{:?}", cmd);
        assert!(debug.contains("test"));
    }
}
