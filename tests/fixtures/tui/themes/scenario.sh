#!/usr/bin/env bash
# Theme-compat tmux repro (app-layer theme -> ratatui Style mapping + tolerance).
#
# Isolation contract (IRON RULE): HOME *and* HAND_HOME both point at a throwaway
# dir, so the fixture's .hand/ is the only config on the resolution path and the
# developer's real ~/.hand is never read or written. The custom-theme directory
# `~/.hand/themes` resolves via HOME (dirs::home_dir), so HOME=$ISO puts the
# fixture theme on the load path.
#
# It exercises, across three isolated launches:
#   - VAL-COMPAT-004  custom theme JSON colours the UI (SGR diff vs default),
#   - VAL-COMPAT-005  unknown theme setting -> default palette + usable session,
#   - VAL-COMPAT-017  corrupt settings.yaml -> readable error, terminal stays
#                     cooked (stty check; PINNED behaviour: readable error exit,
#                     not default-plus-warning).
#
# SGR capture: `tmux capture-pane -e` is unreliable for escape passthrough on
# some builds (Knowledge Persistence), so the palette comparison uses
# `script -q` to record the raw SGR stream instead.
#
# Usage:
#   1. In another shell: cargo run --example mock_provider -p hand-coding-agent
#      (note the port it prints; default 39217).
#   2. MOCK_PORT=<port> tests/fixtures/tui/themes/scenario.sh
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
BIN="$ROOT/target/debug/hand"
FIXTURES="$ROOT/tests/fixtures/tui/themes"
MOCK_PORT="${MOCK_PORT:-39217}"

[ -x "$BIN" ] || { echo "build first: cargo build -p hand-coding-agent"; exit 1; }

SOCK="themerepro$$"
ISO="$(mktemp -d)"
trap 'tmux -L "$SOCK" kill-server 2>/dev/null || true; rm -rf "$ISO"' EXIT

# Isolated config tree. models.json points the driver at the mock provider; the
# custom theme lands in ~/.hand/themes; the global settings.yaml selects it.
mkdir -p "$ISO/.hand/agent" "$ISO/.hand/themes"
sed "s/39217/$MOCK_PORT/" "$ROOT/tests/fixtures/tui/mock-provider/models.json" \
  > "$ISO/.hand/agent/models.json"
cp "$FIXTURES/custom-neon.json" "$ISO/.hand/themes/custom-neon.json"

launch() {
  # launch <label> — start hand in a fresh pane under the isolated env.
  tmux -L "$SOCK" new-session -d -s p -x 120 -y 40 \
    "env HOME=$ISO HAND_HOME=$ISO OPENAI_API_KEY=any \
      $BIN --provider openai --model mock-model \
      --base-url http://127.0.0.1:$MOCK_PORT/v1 --no-context-files"
  for _ in $(seq 1 40); do
    tmux -L "$SOCK" capture-pane -pt p 2>/dev/null | grep -qi "hand\|>" && break
    perl -e 'select(undef,undef,undef,0.2)'
  done
  perl -e 'select(undef,undef,undef,0.5)'
}

capture() { echo "=== $1 ==="; tmux -L "$SOCK" capture-pane -pt p -S -; echo; }
teardown() { tmux -L "$SOCK" send-keys -t p C-d; perl -e 'select(undef,undef,undef,0.4)'; \
  tmux -L "$SOCK" kill-server 2>/dev/null || true; }

# ---------------------------------------------------------------------------
# 1. Custom theme applied — settings select the neon palette. Compare the raw
#    SGR stream against a default-theme launch: the accent (#ff00ff) must show
#    up as a truecolor SGR (38;2;255;0;255) the default dark theme never emits.
# ---------------------------------------------------------------------------
printf 'theme: custom-neon\n' > "$ISO/.hand/agent/settings.yaml"
launch
capture "custom-neon applied (expect neon palette; no theme-fallback notice)"
# Raw SGR capture: record the interactive session's escape stream with
# `script`, feeding a Ctrl+D on stdin so the session starts, paints the themed
# chrome, then exits. The neon accent (#ff00ff) emits 38;2;255;0;255 which the
# default dark theme never produces.
NEON_LOG="$ISO/neon.script"
printf '\004' | script -q "$NEON_LOG" \
  env HOME="$ISO" HAND_HOME="$ISO" OPENAI_API_KEY=any \
    "$BIN" --provider openai --model mock-model \
    --base-url "http://127.0.0.1:$MOCK_PORT/v1" --no-context-files >/dev/null 2>&1 || true
grep -a "38;2;255;0;255" "$NEON_LOG" >/dev/null \
  && echo "SGR: neon accent present (custom theme applied)" \
  || echo "SGR: neon accent MISSING (custom theme NOT applied)"
teardown

# ---------------------------------------------------------------------------
# 2. Unknown theme — settings name a theme with no matching file. The session
#    must still start on the default palette with a yellow fallback notice.
# ---------------------------------------------------------------------------
printf 'theme: bogus-does-not-exist\n' > "$ISO/.hand/agent/settings.yaml"
launch
capture "unknown theme (expect default palette, usable session, 'theme: unknown theme' notice)"
teardown

# ---------------------------------------------------------------------------
# 3. Corrupt settings.yaml — PINNED behaviour is a readable error, exiting with
#    the terminal in cooked mode (raw mode is never entered). Verify stty is
#    sane after the process exits.
# ---------------------------------------------------------------------------
printf 'theme: light\n  : broken indent\n' > "$ISO/.hand/agent/settings.yaml"
tmux -L "$SOCK" new-session -d -s p -x 120 -y 40 \
  "env HOME=$ISO HAND_HOME=$ISO OPENAI_API_KEY=any \
    $BIN --provider openai --model mock-model \
    --base-url http://127.0.0.1:$MOCK_PORT/v1 --no-context-files; \
   echo EXIT=\$?; stty -a </dev/tty | head -1; echo COOKED-CHECK-DONE"
for _ in $(seq 1 30); do
  tmux -L "$SOCK" capture-pane -pt p 2>/dev/null | grep -q "COOKED-CHECK-DONE" && break
  perl -e 'select(undef,undef,undef,0.2)'
done
capture "corrupt settings (expect a readable YAML error, EXIT!=0, 'icanon' present => cooked)"

echo "done"
