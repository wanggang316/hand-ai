# Keybindings test fixtures

`keybindings.yaml` fixtures for the app-layer keybindings wiring (Decision Log
2026-07-24, option A). They exercise the user-facing behaviours the interactive
driver wires up: overrides applying verbatim, project-shadows-global precedence,
non-fatal load diagnostics, `/hotkeys` (no dead entries), `/reload` re-binding,
and custom keys driving the registry-backed selectors.

## File format

A flat YAML map of `action: chord`. Action names are kebab-case (see
`crates/coding-agent/src/core/keybindings.rs` `Action::as_str`); chords are
lowercase `ctrl+`/`alt+`/`shift+`/`cmd+` prefixes over a single key
(`enter`, `escape`, `up`, `f5`, or a printable char). Example:

```yaml
copy-last-message: alt+c
select-down: j
```

## Where they load from

Resolution order is **project > global > defaults**:

- **global**: `$HAND_HOME/.hand/keybindings.yaml` (falls back to
  `~/.hand/keybindings.yaml` when `HAND_HOME` is unset).
- **project**: `<cwd>/.hand/keybindings.yaml`.

`HAND_HOME` is honoured for the global layer so a test can point it at a temp dir
and never touch a developer's real config.

## Isolation contract (mandatory)

Every scenario runs the interactive binary under a throwaway `HOME` **and**
`HAND_HOME`, both set to a `mktemp -d` dir, so the fixture's `.hand/` is the only
config on the resolution path and the developer's real `~/.hand` is never read or
written:

```sh
ISO="$(mktemp -d)"
mkdir -p "$ISO/.hand"
cp valid-copy-alt-c.yaml "$ISO/.hand/keybindings.yaml"        # global layer
# ...run under: env HOME="$ISO" HAND_HOME="$ISO" <hand interactive>
```

For a project-layer fixture, write it to `<cwd>/.hand/keybindings.yaml` where
`<cwd>` is the directory the binary is launched in (also a temp dir).

## The fixtures

| file                          | drives                                                            |
|-------------------------------|------------------------------------------------------------------|
| `valid-copy-alt-c.yaml`       | VAL-COMPAT-001 — override applies verbatim (Ctrl+X → Alt+C)       |
| `valid-select-down-j.yaml`    | VAL-OVERLAY-021 — custom nav key drives registry-backed selectors |
| `global-submit-ctrl-s.yaml`   | VAL-COMPAT-002 (global layer)                                     |
| `project-submit-alt-enter.yaml`| VAL-COMPAT-002 (project layer, wins)                            |
| `invalid-unknown-action.yaml` | VAL-COMPAT-003 — unknown action → yellow diagnostic, app runs     |
| `invalid-bad-chord.yaml`      | VAL-COMPAT-003 — malformed chord → diagnostic, default kept       |
| `invalid-conflict.yaml`       | VAL-COMPAT-003 — one chord, two actions → both disabled           |

## Scenarios

`scenario.sh` (this dir) is a self-contained tmux repro that copies a fixture
into an isolated `$HAND_HOME/.hand/`, launches the interactive binary against the
mock provider, and captures the pane so the diagnostic line, `/hotkeys` listing,
and `/reload` status are visible. It is a manual aid — the validator drives the
same behaviours from outside; the unit tests in `core::keybindings`, the driver's
`keys` module, and `slash_commands` pin the logic.
