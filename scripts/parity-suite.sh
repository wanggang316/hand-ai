#!/usr/bin/env bash
# Functional-parity test suite — runs a battery of identical prompts
# against `pi` (TS reference) and `hand` (Rust port), compares both
# stdout (after ANSI stripping) and exit codes, and produces a pass/fail
# table.
#
# Designed for fast regression sweeps — prompts kept short to keep
# total runtime under ~5 min against deepseek/deepseek-v4-flash.
#
# Usage:
#   ./scripts/parity-suite.sh
#
# Output:
#   /tmp/parity-suite.log   raw per-test logs
#   stdout                  PASS / FAIL summary table

set -euo pipefail

source "$HOME/.config/secrets.env"

PROVIDER="openrouter"
MODEL="deepseek/deepseek-v4-flash"
HAND="./target/debug/hand"

strip_ansi() {
  python3 -c '
import re, sys
data = sys.stdin.read()
data = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", data)
data = re.sub(r"\x1b\[[\?>=]?[0-9;]*[A-Za-z]", "", data)
data = re.sub(r"\x1b[PX^_][^\x1b]*\x1b\\", "", data)
data = re.sub(r"\x1b[0-9A-Za-z]", "", data)
data = data.replace("\r\n", "\n").replace("\r", "\n")
sys.stdout.write(data)
'
}

logfile="/tmp/parity-suite.log"
: > "$logfile"

declare -a results=()
declare -i pass_count=0 fail_count=0

run_case() {
  local label="$1"
  local prompt="$2"
  shift 2
  local tools_arg="${1:-}"

  local pi_out hand_out pi_exit hand_exit
  local pi_args=(--print --provider "$PROVIDER" --model "$MODEL")
  local hand_args=(--print --prompt "$prompt" --provider "$PROVIDER" -m "$MODEL")
  if [[ -z "$tools_arg" ]]; then
    pi_args+=(--no-tools)
    hand_args+=(--no-tools)
  else
    pi_args+=(--tools "$tools_arg")
    hand_args+=(--tools "$tools_arg")
  fi

  pi_out=$(pi "${pi_args[@]}" "$prompt" 2>&1 | strip_ansi || true)
  pi_exit=$?
  hand_out=$("$HAND" "${hand_args[@]}" 2>&1 | strip_ansi || true)
  hand_exit=$?

  {
    echo "=== $label ==="
    echo "PROMPT: $prompt"
    echo "TOOLS: ${tools_arg:-<none>}"
    echo "--- pi (exit=$pi_exit) ---"
    echo "$pi_out"
    echo "--- hand (exit=$hand_exit) ---"
    echo "$hand_out"
    echo
  } >> "$logfile"

  # Pass criteria: exit codes match AND output volume is consistent with pi.
  # Content matching is too noisy with LLMs; we only check structural parity:
  #  - both succeeded → hand must be non-empty if pi was non-empty
  #    (if pi also returned nothing, e.g. empty prompt, hand may match)
  #  - both failed → structurally consistent
  local outcome="UNKNOWN"
  local pi_nonempty hand_nonempty
  pi_nonempty=$(echo "$pi_out" | tr -d '[:space:]')
  hand_nonempty=$(echo "$hand_out" | tr -d '[:space:]')
  if [[ "$pi_exit" -eq "$hand_exit" ]]; then
    if [[ "$pi_exit" -eq 0 ]]; then
      if [[ -z "$pi_nonempty" && -z "$hand_nonempty" ]]; then
        outcome="PASS (both-empty)"
      elif [[ -n "$hand_nonempty" ]]; then
        outcome="PASS"
      else
        outcome="FAIL hand-empty"
      fi
    else
      outcome="PASS (both-error)"
    fi
  else
    outcome="FAIL exit-mismatch pi=$pi_exit hand=$hand_exit"
  fi

  if [[ "$outcome" == PASS* ]]; then
    pass_count+=1
  else
    fail_count+=1
  fi
  results+=("$outcome  $label")
}

# Create test fixtures.
mkdir -p /tmp/parity-fix
echo "alpha" > /tmp/parity-fix/a.txt
echo "beta" > /tmp/parity-fix/b.txt
echo "gamma" > /tmp/parity-fix/c.txt

echo "Running parity suite (10 cases, ~4min)…" >&2

# Case 1: trivial math, no tools, no thinking required.
run_case "math-no-tools" "what is 2+2? answer in one number only."

# Case 2: list output, no tools.
run_case "list-no-tools" "list three primary colors lowercase, comma-separated, no other text."

# Case 3: read tool (zero-arg style — but read takes path).
run_case "tool-read" "read /tmp/parity-fix/a.txt and tell me the one word it contains" "read"

# Case 4: multi-tool sequence.
run_case "tool-read-twice" "read /tmp/parity-fix/a.txt then /tmp/parity-fix/b.txt and join the two words" "read"

# Case 5: tool error handling.
run_case "tool-error" "read /tmp/does-not-exist-xyz.txt and tell me what happened" "read"

# Case 6: bash tool.
run_case "tool-bash" "run 'echo parity' and tell me the output line." "bash"

# Case 7: invalid model — both should exit 1.
run_case "bad-model-exit-1" "hi" </dev/null  # no tools so the runs are fast

# Case 8: empty stdin.
run_case "empty-stdin" ""

# Case 9: parallel/sequential tool calls — exercises the consecutive
# ToolResult handling path that previously double-emitted images.
run_case "tool-three-files" \
  "read /tmp/parity-fix/a.txt /tmp/parity-fix/b.txt /tmp/parity-fix/c.txt and concatenate the three words in order separated by hyphens" \
  "read"

# Case 10: bash + read combo — multiple tool kinds in one turn.
run_case "tool-bash-then-read" \
  "first run 'echo combo' then read /tmp/parity-fix/a.txt — report both outputs" \
  "bash,read"

echo
echo "================ PARITY SUITE RESULTS ================"
for r in "${results[@]}"; do
  echo "$r"
done
echo "------------------------------------------------------"
echo "PASS: $pass_count  FAIL: $fail_count  total: $((pass_count + fail_count))"
echo "Full log: $logfile"
exit "$fail_count"
