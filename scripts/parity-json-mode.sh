#!/usr/bin/env bash
# Validates --print --mode json structural parity with pi-mono.
#
# Pi-mono and hand both serialize the same underlying Message/Event types
# via serde rename_all = "camelCase", so the JSONL outputs should parse
# identically and produce overlapping `type` event counts.
#
# Usage:
#   ./scripts/parity-json-mode.sh

set -euo pipefail

source "$HOME/.config/secrets.env"

PROVIDER="openrouter"
MODEL="deepseek/deepseek-v4-flash"
HAND="./target/debug/hand"
PROMPT="say one word"

echo "[hand] --mode json …" >&2
hand_out=$("$HAND" --print --mode json --prompt "$PROMPT" \
  --provider "$PROVIDER" -m "$MODEL" --no-tools 2>&1)

# 1. Every line must parse as JSON.
echo "$hand_out" | python3 -c '
import sys, json
n = 0
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        json.loads(line)
        n += 1
    except Exception as e:
        print(f"FAIL line {n+1}: {e}; preview={line[:120]!r}", file=sys.stderr)
        sys.exit(1)
print(f"PASS: {n} valid JSONL events")
'

# 2. Must include the pi-mono-mandatory event types.
required=(session agent_start turn_start message_start message_end turn_end agent_end)
for t in "${required[@]}"; do
  count=$(echo "$hand_out" | python3 -c "
import sys, json
n = 0
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        if json.loads(line).get('type') == '$t':
            n += 1
    except Exception: pass
print(n)
")
  if [[ "$count" -lt 1 ]]; then
    echo "FAIL: missing event type '$t' (count=$count)" >&2
    exit 1
  fi
  echo "PASS: event '$t' fired $count time(s)"
done

# 3. Session header must arrive FIRST.
first_line=$(echo "$hand_out" | head -1)
first_type=$(echo "$first_line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('type',''))")
if [[ "$first_type" != "session" ]]; then
  echo "FAIL: first event must be 'session', got '$first_type'" >&2
  exit 1
fi
echo "PASS: session header arrives first"

echo
echo "================ JSON-MODE PARITY: ALL CHECKS PASSED ================"
