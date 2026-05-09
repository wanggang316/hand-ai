//! Integration test: deserialize a pi-mono-flavoured `settings.json` via
//! [`Settings::from_json_str`] and assert representative fields land in the
//! expected places.
//!
//! Source of truth for the camelCase shape:
//! `pi-mono/packages/coding-agent/src/core/settings-manager.ts`. The fixture
//! at `tests/fixtures/pi-mono-settings.json` exercises one entry per
//! Settings field that M2 added so the camelCase aliases stay honest.

use hand_coding_agent::core::settings::{
    DoubleEscapeAction, PackageSource, Settings, SteeringMode, ThinkingLevelSetting,
    TransportSetting, TreeFilterMode,
};

const FIXTURE: &str = include_str!("fixtures/pi-mono-settings.json");

#[test]
fn pi_mono_settings_json_deserializes_into_settings() {
    let s = Settings::from_json_str(FIXTURE).expect("fixture parses");

    // Top-level scalars (camelCase → snake_case via serde alias).
    assert_eq!(s.last_changelog_version.as_deref(), Some("0.42.1"));
    assert_eq!(s.default_provider.as_deref(), Some("anthropic"));
    assert_eq!(
        s.default_thinking_level,
        Some(ThinkingLevelSetting::Medium),
    );
    assert_eq!(s.transport, Some(TransportSetting::Websocket));
    assert_eq!(s.steering_mode, Some(SteeringMode::All));
    assert_eq!(s.follow_up_mode, Some(SteeringMode::OneAtATime));
    assert_eq!(s.hide_thinking_block, Some(false));
    assert_eq!(s.quiet_startup, Some(true));
    assert_eq!(s.collapse_changelog, Some(true));
    assert_eq!(s.enable_install_telemetry, Some(false));
    assert_eq!(s.enable_skill_commands, Some(true));
    assert_eq!(s.editor_padding_x, Some(2));
    assert_eq!(s.autocomplete_max_visible, Some(10));
    assert_eq!(s.show_hardware_cursor, Some(false));

    // Nested sub-structs.
    assert_eq!(s.branch_summary.skip_prompt, Some(true));
    assert_eq!(s.branch_summary.reserve_tokens(), 16384);

    assert_eq!(s.retry.max_retries(), 5);
    // pi-mono uses `baseDelayMs`; we alias it onto `initial_delay_ms`.
    assert_eq!(s.retry.initial_delay_ms(), 2500);
    assert_eq!(s.retry.provider.max_retry_delay_ms(), 60_000);
    assert_eq!(s.retry.provider.timeout_ms, Some(30_000));
    assert_eq!(s.retry.provider.max_retries, Some(3));

    assert!(s.terminal.show_images());
    assert_eq!(s.terminal.image_width_cells(), 72);
    assert!(s.terminal.show_terminal_progress());

    assert!(s.images.auto_resize());
    assert!(!s.images.block_images());

    assert_eq!(s.thinking_budgets.minimal, Some(1024));
    assert_eq!(s.thinking_budgets.low, Some(4096));
    assert_eq!(s.thinking_budgets.medium, Some(16384));
    assert_eq!(s.thinking_budgets.high, Some(32768));

    assert_eq!(s.markdown.code_block_indent(), "    ");
    assert!(!s.warnings.anthropic_extra_usage());

    // Enums.
    assert_eq!(s.double_escape_action, Some(DoubleEscapeAction::Fork));
    assert_eq!(s.tree_filter_mode, Some(TreeFilterMode::NoTools));

    // Lists round-trip.
    assert_eq!(
        s.npm_command.as_deref(),
        Some(&[
            "mise".to_string(),
            "exec".to_string(),
            "node@20".to_string(),
            "--".to_string(),
            "npm".to_string(),
        ][..]),
    );
    assert_eq!(
        s.enabled_models.as_deref(),
        Some(&["claude-*".to_string(), "gpt-4o*".to_string()][..]),
    );

    // Packages: bare + filtered both deserialize.
    let pkgs = s.packages();
    assert_eq!(pkgs.len(), 2);
    assert!(matches!(&pkgs[0], PackageSource::Bare(spec) if spec == "npm:@scope/pkg-a"));
    match &pkgs[1] {
        PackageSource::Filtered {
            source,
            extensions,
            skills,
            ..
        } => {
            assert_eq!(source, "github:owner/repo");
            assert_eq!(extensions.as_deref(), Some(&["ext-a".to_string()][..]));
            assert_eq!(skills.as_deref(), Some(&["skill-x".to_string()][..]));
        }
        other => panic!("expected Filtered, got {other:?}"),
    }

    // Path types — `sessionDir` lives in JSON as a string, decoded into
    // `PathBuf`.
    assert_eq!(
        s.session_dir.as_deref(),
        Some(std::path::Path::new("~/.local/share/pi-mono/sessions")),
    );
    assert_eq!(s.shell_path.as_deref(), Some(std::path::Path::new("/bin/zsh")));
}
