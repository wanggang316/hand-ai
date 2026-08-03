//! Slash command system — parse and execute `/command` inputs.

use crate::core::extensions::api::{
    Extension, ExtensionContextFactory, ExtensionError, SlashCommandSpec,
};
use std::collections::HashMap;
use std::sync::Arc;

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

/// An extension-contributed slash command and the extension that owns it.
#[derive(Clone)]
pub struct ExtensionSlashCommand {
    pub spec: SlashCommandSpec,
    pub extension: Arc<dyn Extension>,
}

impl std::fmt::Debug for ExtensionSlashCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionSlashCommand")
            .field("name", &self.spec.name)
            .field("extension", &self.extension.manifest().name)
            .finish()
    }
}

/// Registry that holds all available slash commands.
#[derive(Default)]
pub struct SlashCommandRegistry {
    commands: Vec<SlashCommand>,
    /// Map from name/alias to command index.
    lookup: HashMap<String, usize>,
    /// Extension-contributed commands, indexed by primary name. Built-in
    /// commands take precedence: a lookup checks `lookup` first and only
    /// falls back to `extension_commands` on miss.
    extension_commands: HashMap<String, ExtensionSlashCommand>,
}

impl std::fmt::Debug for SlashCommandRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlashCommandRegistry")
            .field("commands", &self.commands)
            .field(
                "extension_commands",
                &self.extension_commands.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
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
        // Keep this list in sync with `SlashCommandTable::dispatch` in
        // `modes::interactive::slash_commands` and with the static
        // `help_text` rendered by `/help`. The autocomplete provider
        // (driver.rs) and `/help` both read from this single source,
        // so a command that's dispatched but missing here is invisible.
        let builtins = vec![
            ("help", vec!["h"], "Show available commands", false),
            ("quit", vec!["exit", "q"], "Quit the agent", false),
            ("clear", vec![], "Clear the chat scrollback", false),
            (
                "model",
                vec![],
                "Select or switch model (pattern resolves inline)",
                true,
            ),
            (
                "models",
                vec![],
                "Select or switch model (alias for /model)",
                true,
            ),
            (
                "session",
                vec![],
                "Show session info (id, model, tokens, duration)",
                false,
            ),
            ("settings", vec![], "Show current settings", false),
            (
                "thinking",
                vec![],
                "Set thinking level (minimal/low/medium/high/xhigh/max)",
                true,
            ),
            (
                "compact",
                vec![],
                "Compact context (optional `[text]` custom focus)",
                true,
            ),
            ("new", vec![], "Start a new session", false),
            ("resume", vec![], "Browse and select session", true),
            ("name", vec![], "Set session display name", true),
            ("fork", vec![], "Fork current session", true),
            ("clone", vec![], "Clone the current session", false),
            ("export", vec![], "Export session to file", true),
            (
                "import",
                vec![],
                "Replace current session with a JSONL file",
                true,
            ),
            (
                "copy",
                vec![],
                "Copy last assistant message to clipboard (or last [n] with `/copy n`)",
                true,
            ),
            (
                "hotkeys",
                vec!["keybindings"],
                "Show keyboard shortcuts",
                false,
            ),
            ("changelog", vec![], "Show CHANGELOG.md", false),
            ("skills", vec![], "List discovered skills", false),
            ("extensions", vec![], "List loaded Tier 1 extensions", false),
            ("theme", vec![], "Select or set a theme", true),
            ("login", vec![], "Open the login dialog", true),
            ("logout", vec![], "Clear stored auth credentials", false),
            (
                "diagnostics",
                vec![],
                "Run diagnostics into the chat",
                false,
            ),
            ("tree", vec![], "Show file tree of directory", true),
            ("reload", vec![], "Reload settings and keybindings", false),
            (
                "scoped-models",
                vec!["scoped_models"],
                "Toggle which models the `/model` quick-cycle reaches",
                false,
            ),
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

    /// Register a slash command contributed by a Tier 1 or Tier 2 extension.
    ///
    /// Built-in commands always take precedence: if `spec.name` shadows a
    /// built-in, the built-in is what `find` and `dispatch` resolve to. The
    /// extension command is still recorded so `/help` can list it.
    ///
    /// Names that are empty, contain whitespace, or contain `/` are silently
    /// rejected — these would never match the parser's tokenizer (which splits
    /// on whitespace and strips a leading `/`) so accepting them would create
    /// dead entries that pollute `/help` without ever being routable.
    pub fn register_extension_command(
        &mut self,
        spec: SlashCommandSpec,
        extension: Arc<dyn Extension>,
    ) {
        if spec.name.is_empty()
            || spec.name.contains(char::is_whitespace)
            || spec.name.contains('/')
        {
            tracing::warn!(
                extension = %extension.manifest().name,
                name = %spec.name,
                "rejecting extension slash command with invalid name"
            );
            return;
        }
        let key = spec.name.clone();
        self.extension_commands
            .insert(key, ExtensionSlashCommand { spec, extension });
    }

    /// Look up an extension-contributed command by name.
    ///
    /// Returns `None` if the name shadows a built-in command (built-ins win)
    /// or if no extension registered that name.
    pub fn find_extension_command(&self, name: &str) -> Option<&ExtensionSlashCommand> {
        if self.lookup.contains_key(name) {
            // Built-in shadowing: the dispatcher must not route to the
            // extension version of a built-in command.
            return None;
        }
        self.extension_commands.get(name)
    }

    /// All extension-contributed slash commands, sorted by extension name
    /// then command name. Used by `/help` to list them grouped per extension.
    pub fn extension_commands(&self) -> Vec<&ExtensionSlashCommand> {
        let mut items: Vec<&ExtensionSlashCommand> = self.extension_commands.values().collect();
        items.sort_by(|a, b| {
            a.extension
                .manifest()
                .name
                .cmp(&b.extension.manifest().name)
                .then_with(|| a.spec.name.cmp(&b.spec.name))
        });
        items
    }

    /// Dispatch a slash command to an extension. Looks up the command via
    /// [`Self::find_extension_command`] (so built-ins shadow extensions),
    /// then calls `Extension::handle_slash_command`.
    ///
    /// The context is built from `contexts` for the extension that owns the
    /// command, so a command handler sees the same per-extension `data_dir`
    /// its hooks do.
    ///
    /// Returns `Ok(Some(output))` on success, `Ok(None)` if the name doesn't
    /// match any extension command (caller should treat as "unknown"), or
    /// the extension's `Err` on failure.
    pub async fn dispatch_extension_command(
        &self,
        name: &str,
        args: &str,
        contexts: &ExtensionContextFactory,
    ) -> Result<Option<String>, ExtensionError> {
        let Some(entry) = self.find_extension_command(name) else {
            return Ok(None);
        };
        let cx = contexts.for_extension(&entry.extension.manifest().name);
        let output = entry
            .extension
            .handle_slash_command(&cx, name, args)
            .await?;
        Ok(Some(output))
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

    /// Autocomplete dropdowns surface the registry description directly,
    /// so a misleading entry (e.g. "/models — List available models" when
    /// `/models` actually opens a selector) trains users to expect the
    /// wrong behaviour. Pin the descriptions that previously drifted.
    #[test]
    fn registry_descriptions_match_actual_behaviour() {
        let registry = SlashCommandRegistry::new();
        let models = registry.find("models").expect("/models registered");
        assert!(
            !models.description.to_ascii_lowercase().contains("list"),
            "/models description still implies listing: {}",
            models.description
        );
        let changelog = registry.find("changelog").expect("/changelog registered");
        assert!(
            !changelog
                .description
                .to_ascii_lowercase()
                .contains("version info"),
            "/changelog description still says version info: {}",
            changelog.description
        );
        let compact = registry.find("compact").expect("/compact registered");
        assert!(
            compact.description.to_ascii_lowercase().contains("text")
                || compact.description.contains("[text]"),
            "/compact description does not mention the optional arg: {}",
            compact.description
        );
        let model = registry.find("model").expect("/model registered");
        let model_desc = model.description.to_ascii_lowercase();
        assert!(
            model_desc.contains("select") || model_desc.contains("switch"),
            "/model description missing select/switch verb: {}",
            model.description
        );
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

    // ----------------------------------------------------------------------
    // T3.5 — extension-contributed slash command dispatch
    // ----------------------------------------------------------------------

    use crate::core::extensions::api::{
        Extension, ExtensionContext, ExtensionError, ExtensionManifest, SlashCommandSpec,
    };
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn ext_manifest(name: &str) -> ExtensionManifest {
        ExtensionManifest {
            name: name.into(),
            version: "0.1.0".into(),
            description: None,
            capabilities: Default::default(),
            exec: None,
            env: Default::default(),
            slash_commands: Vec::new(),
            custom_tools: Vec::new(),
        }
    }

    fn test_ctx() -> ExtensionContextFactory {
        ExtensionContextFactory::new(PathBuf::from("/tmp"), "s", PathBuf::from("/tmp/extensions"))
    }

    /// A fake extension whose `handle_slash_command` records the call and
    /// returns a caller-configured output string.
    struct FakeSlashExt {
        manifest: ExtensionManifest,
        output: String,
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Extension for FakeSlashExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn handle_slash_command(
            &self,
            _cx: &ExtensionContext,
            name: &str,
            args: &str,
        ) -> Result<String, ExtensionError> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), args.to_string()));
            Ok(self.output.clone())
        }
    }

    /// A built-in command must take precedence over an extension-registered
    /// command of the same name. The extension is recorded but not routed to.
    #[tokio::test]
    async fn builtin_shadows_extension_command_of_same_name() {
        let mut registry = SlashCommandRegistry::new();
        let ext = Arc::new(FakeSlashExt {
            manifest: ext_manifest("collider"),
            output: "extension-ran".into(),
            calls: Mutex::new(Vec::new()),
        });
        // `help` is a built-in.
        registry.register_extension_command(
            SlashCommandSpec {
                name: "help".into(),
                description: "shadow".into(),
                usage: None,
            },
            ext.clone(),
        );

        // Built-in still wins on lookup.
        assert!(registry.find("help").is_some());
        assert!(
            registry.find_extension_command("help").is_none(),
            "built-in must shadow"
        );

        let routed = registry
            .dispatch_extension_command("help", "", &test_ctx())
            .await
            .expect("no error");
        assert!(routed.is_none(), "built-in shadowing must skip extension");
        assert!(
            ext.calls.lock().unwrap().is_empty(),
            "extension must not be invoked when built-in shadows"
        );
    }

    /// An unknown command (no built-in match) routes to the extension
    /// dispatcher. The extension receives the command name and raw args.
    #[tokio::test]
    async fn unknown_command_routes_to_extension() {
        let mut registry = SlashCommandRegistry::new();
        let ext = Arc::new(FakeSlashExt {
            manifest: ext_manifest("auto-commit"),
            output: "committed".into(),
            calls: Mutex::new(Vec::new()),
        });
        registry.register_extension_command(
            SlashCommandSpec {
                name: "foo".into(),
                description: "Run foo".into(),
                usage: None,
            },
            ext.clone(),
        );

        let routed = registry
            .dispatch_extension_command("foo", "bar", &test_ctx())
            .await
            .expect("dispatch ok");
        assert_eq!(routed.as_deref(), Some("committed"));

        let calls = ext.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "foo");
        assert_eq!(calls[0].1, "bar");
    }

    /// Names that the parser could never match (empty, whitespace, slash)
    /// must be rejected at registration time so `/help` never lists a
    /// dead-on-arrival command.
    #[test]
    fn register_extension_command_rejects_invalid_names() {
        let mut registry = SlashCommandRegistry::new();
        let ext = Arc::new(FakeSlashExt {
            manifest: ext_manifest("ext"),
            output: String::new(),
            calls: Mutex::new(Vec::new()),
        });

        for bad in ["", "has space", "with/slash", "tab\there"] {
            registry.register_extension_command(
                SlashCommandSpec {
                    name: bad.into(),
                    description: "should be rejected".into(),
                    usage: None,
                },
                ext.clone(),
            );
        }
        assert!(
            registry.extension_commands().is_empty(),
            "invalid names must be rejected: {:?}",
            registry.extension_commands()
        );

        // Sanity: a valid name still registers.
        registry.register_extension_command(
            SlashCommandSpec {
                name: "valid-name".into(),
                description: "ok".into(),
                usage: None,
            },
            ext.clone(),
        );
        assert_eq!(registry.extension_commands().len(), 1);
    }

    #[test]
    fn extension_commands_listed_for_help() {
        let mut registry = SlashCommandRegistry::new();
        let ext = Arc::new(FakeSlashExt {
            manifest: ext_manifest("ext-a"),
            output: String::new(),
            calls: Mutex::new(Vec::new()),
        });
        registry.register_extension_command(
            SlashCommandSpec {
                name: "alpha".into(),
                description: "Alpha".into(),
                usage: None,
            },
            ext.clone(),
        );
        let listed = registry.extension_commands();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].spec.name, "alpha");
        assert_eq!(listed[0].extension.manifest().name, "ext-a");
    }
}
