#!/usr/bin/env bash
# Keybindings-wiring tmux repro (app-layer, Decision Log 2026-07-24 option A).
#
# Isolation contract: HOME *and* HAND_HOME both point at a throwaway dir, so the
# fixture's .hand/ is the only config on the resolution path and the developer's
# real ~/.hand is never read or written.
#
# It drives, in one interactive session:
#   - VAL-COMPAT-003 startup diagnostic (from an invalid override),
#   - VAL-COMPAT-001 override applying verbatim (/hotkeys shows Alt+C),
#   - VAL-COMPAT-006 /hotkeys with no dead entries,
#   - VAL-COMPAT-020 /reload picking up a live edit,
# capturing the pane after each so the effects are visible.
#
# Usage:
#   1. In another shell: cargo run --example mock_provider -p hand-coding-agent
#      (note the port it prints; default 39217).
#   2. MOCK_PORT=<port> tests/fixtures/tui/keybindings/scenario.sh
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
BIN="$ROOT/target/debug/hand"
FIXTURES="$ROOT/tests/fixtures/tui/keybindings"
MOCK_PORT="${MOCK_PORT:-39217}"

[ -x "$BIN" ] || { echo "build first: cargo build -p hand-coding-agent"; exit 1; }

SOCK="kbrepro$$"
ISO="$(mktemp -d)"
trap 'tmux -L "$SOCK" kill-server 2>/dev/null || true; rm -rf "$ISO"' EXIT

# Isolated config tree. The models.json (ModelRegistry path, read via HOME) points
# the interactive driver at the mock provider; keybindings.yaml (global layer, read
# via HAND_HOME) carries the fixture.
mkdir -p "$ISO/.hand/agent"
sed "s/39217/$MOCK_PORT/" "$ROOT/tests/fixtures/tui/mock-provider/models.json" \
  > "$ISO/.hand/agent/models.json"

# Global keybindings: a valid override (copy -> Alt+C) plus an invalid entry so the
# startup diagnostic is visible. Concatenate two fixtures into the global file.
cat "$FIXTURES/valid-copy-alt-c.yaml" "$FIXTURES/invalid-unknown-action.yaml" \
  > "$ISO/.hand/keybindings.yaml"

capture() { echo "=== $1 ==="; tmux -L "$SOCK" capture-pane -pt p -S -; echo; }

tmux -L "$SOCK" new-session -d -s p -x 120 -y 40 \
  "env HOME=$ISO HAND_HOME=$ISO OPENAI_API_KEY=any \
    $BIN --provider openai --model mock-model \
    --base-url http://127.0.0.1:$MOCK_PORT/v1 --no-context-files"

# Wait for the welcome chrome to paint.
for _ in $(seq 1 40); do
  tmux -L "$SOCK" capture-pane -pt p 2>/dev/null | grep -qi "hand\|>" && break
  perl -e 'select(undef,undef,undef,0.2)'
done
perl -e 'select(undef,undef,undef,0.5)'
capture "startup (expect a yellow 'unknown action' diagnostic; app running)"

# /hotkeys reads the live table: Copy shows Alt+C, listing has no dead entries.
tmux -L "$SOCK" send-keys -t p "/hotkeys" Enter; perl -e 'select(undef,undef,undef,0.5)'
capture "/hotkeys (expect Alt+C for copy, an Input + Selectors section)"

# Live-edit the file, then /reload picks it up (copy -> Ctrl+Y).
printf 'copy-last-message: ctrl+y\n' > "$ISO/.hand/keybindings.yaml"
tmux -L "$SOCK" send-keys -t p "/reload" Enter; perl -e 'select(undef,undef,undef,0.5)'
capture "/reload (expect '[reloaded keybindings]')"

tmux -L "$SOCK" send-keys -t p "/hotkeys" Enter; perl -e 'select(undef,undef,undef,0.5)'
capture "/hotkeys after reload (expect Ctrl+Y for copy)"

tmux -L "$SOCK" send-keys -t p C-d
echo "done"
