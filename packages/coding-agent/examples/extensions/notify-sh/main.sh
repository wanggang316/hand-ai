#!/usr/bin/env bash
# Tier 2 (subprocess) extension fixture: log every tool call to a file.
#
# This script is intentionally simple — it greps fields out of the JSONL
# event line rather than parsing JSON properly. Real polyglot extensions
# should use `jq` or a language with a JSON library.
#
# The host injects HAND_DATA_DIR pointing at the extension's per-session data
# directory; we write notifications.log there.
set -euo pipefail
LOG="${HAND_DATA_DIR:-./data}/notifications.log"
mkdir -p "$(dirname "$LOG")"

while IFS= read -r line; do
  type=$(printf '%s' "$line" | grep -o '"type":"[^"]*"' | head -1 | cut -d'"' -f4)
  case "$type" in
    on_after_tool_call)
      tool=$(printf '%s' "$line" | grep -o '"toolName":"[^"]*"' | cut -d'"' -f4)
      success=$(printf '%s' "$line" | grep -o '"success":[a-z]*' | cut -d':' -f2)
      printf '%s tool=%s success=%s\n' "$(date -u +%FT%TZ)" "$tool" "$success" >> "$LOG"
      printf '{"type":"ok"}\n'
      ;;
    on_load|on_shutdown|on_before_tool_call)
      printf '{"type":"ok"}\n'
      ;;
    *)
      printf '{"type":"error","message":"unknown event"}\n'
      ;;
  esac
done
