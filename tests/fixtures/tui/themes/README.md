# Theme test fixtures

Theme JSON fixtures for the app-layer theme-compat wiring: the user theme
format (`~/.hand/themes/*.json`) mapped to `ratatui::style::Style`, plus the
tolerance paths for unknown / corrupt / partial themes and corrupt settings.

They exercise the user-facing behaviours the rt driver wires up:

- **VAL-COMPAT-004** a valid custom theme colours the UI (its palette shows up
  in the rendered SGR stream);
- **VAL-COMPAT-005** an unknown theme name falls back to the default palette
  with a visible notice, and the session stays usable;
- **VAL-COMPAT-016** a malformed / partial theme JSON falls back to the default
  palette rather than crashing;
- **VAL-COMPAT-017** (pinned) a corrupt `settings.yaml` exits with a readable
  error and the terminal in cooked mode (raw mode is never entered);
  unknown-keys-only settings start normally.

## Files

| File | Role |
|------|------|
| `custom-neon.json` | A complete, valid custom theme with a deliberately loud neon palette (accent `#ff00ff`). Copied to `~/.hand/themes/` so `theme: custom-neon` resolves. Its truecolor SGR (`38;2;255;0;255`) is absent from the default dark theme, making the "custom applied vs default" diff unambiguous. |
| `malformed.json` | Syntactically broken JSON (truncated, contains a comment). Loading it must fail to parse and fall back to the default palette. |
| `partial.json` | Syntactically valid JSON missing most required colour slots. Deserialising into the exhaustive `ThemeColors` fails, so it must fall back to the default palette. |
| `scenario.sh` | tmux + `script` repro driving the three isolated launches (custom / unknown / corrupt-settings). |

## Pinned decision — corrupt `settings.yaml`

The feature required choosing one of two behaviours for a *syntactically
corrupt* `settings.yaml`. **Pinned: option (b)** — surface a **readable error
naming the offending file and exit with the terminal in cooked mode.** This is
the pre-existing behaviour: `Settings::load` returns `SettingsError::Yaml`,
which propagates out of `SettingsManager::from_cwd` and session construction
*before* `SessionGuard::enter` toggles raw mode, so the shell is never left in
a broken state. Unknown-keys-only settings are not corrupt — they parse, warn,
and start normally.

## The unit tests are the source of truth

The tmux scenario is a human-visible smoke check. The load-bearing assertions
live in Rust unit tests (`cargo test -p hand-coding-agent --lib`):

- `modes::interactive::theme::ratatui_style::tests::*` — theme → `ratatui::Style`
  mapping (truecolor RGB, 256-colour quantisation, `""` → `Color::Reset`);
- `modes::interactive::theme::loader::tests::resolve_*` — unknown / corrupt /
  partial / system / high-contrast fallback to the default palette;
- `core::settings::tests::{malformed_yaml_returns_yaml_error_with_path,
  corrupt_settings_error_is_readable_and_names_the_file,
  unknown_top_level_key_ignored_without_error}` — the pinned corrupt-settings
  behaviour and the unknown-keys tolerance.

## Isolation contract (IRON RULE)

`scenario.sh` sets **both** `HOME` and `HAND_HOME` to a throwaway dir. The
custom-theme directory resolves through `dirs::home_dir()` (i.e. `HOME`), and
the settings / models config through `HAND_HOME`, so the developer's real
`~/.hand` is never read or written.
