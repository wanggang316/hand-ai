#!/usr/bin/env bash
# Run the same non-interactive prompt against `pi` (TypeScript reference)
# and `hand` (Rust port), strip terminal control sequences, and diff the
# results. Useful for catching behavioral divergence on real LLM calls.
#
# Usage:
#   ./scripts/diff-hand-vs-pi.sh "<prompt>" [provider] [model]
#
# Defaults:
#   provider = openrouter
#   model    = deepseek/deepseek-v4-flash
#
# Requirements:
#   - pi on PATH (npm i -g @mariozechner/pi-coding-agent)
#   - target/debug/hand (cargo build -p hand-coding-agent --bin hand)
#   - ~/.config/secrets.env exports the matching API key

set -euo pipefail

prompt="${1:?missing prompt}"
provider="${2:-openrouter}"
model="${3:-deepseek/deepseek-v4-flash}"

if [[ ! -f "$HOME/.config/secrets.env" ]]; then
  echo "missing ~/.config/secrets.env" >&2
  exit 2
fi
# shellcheck disable=SC1091
source "$HOME/.config/secrets.env"

out_pi="/tmp/diff-pi.out"
out_hand="/tmp/diff-hand.out"

# Strip ANSI SGR, OSC, and CSI sequences down to printable text.
strip_ansi() {
  python3 -c '
import re, sys
data = sys.stdin.read()
# Drop ANSI CSI/OSC/APC sequences and the bell terminator they often use.
data = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", data)  # OSC ... BEL/ST
data = re.sub(r"\x1b\[[\?>=]?[0-9;]*[A-Za-z]", "", data)         # CSI ... <letter>
data = re.sub(r"\x1b[PX^_][^\x1b]*\x1b\\", "", data)             # DCS/SOS/PM/APC
data = re.sub(r"\x1b[0-9A-Za-z]", "", data)                       # short ESC
data = data.replace("\r\n", "\n").replace("\r", "\n")
sys.stdout.write(data)
'
}

echo "[pi] running…"
pi --print --provider "$provider" --model "$model" --no-tools "$prompt" 2>&1 \
  | strip_ansi \
  | sed 's/^[[:space:]]*//' \
  > "$out_pi"
echo "[pi] wrote $(wc -c <"$out_pi") bytes → $out_pi"

echo "[hand] running…"
./target/debug/hand --print --prompt "$prompt" --provider "$provider" -m "$model" --no-tools 2>&1 \
  | strip_ansi \
  | sed 's/^[[:space:]]*//' \
  > "$out_hand"
echo "[hand] wrote $(wc -c <"$out_hand") bytes → $out_hand"

echo
echo "=================== pi ==================="
cat "$out_pi"
echo
echo "================== hand =================="
cat "$out_hand"
echo
echo "=================== diff ==================="
if diff -u "$out_pi" "$out_hand"; then
  echo "(identical after ANSI stripping)"
fi
