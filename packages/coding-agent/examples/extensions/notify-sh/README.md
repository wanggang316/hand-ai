# notify-sh

Tier 2 (subprocess) extension fixture. Listens on stdin for JSONL extension
events and appends a one-line summary of every `on_after_tool_call` event
to `${HAND_DATA_DIR}/notifications.log`.

This is the simplest possible polyglot extension. The script uses
shell-grep to pluck fields out of the wire frame rather than a real JSON
parser; that is acceptable for a demo but a production extension should
use `jq` or a language with proper JSON support.

## Wire protocol

- Host writes one JSON event per line on stdin.
- Script must respond with one JSON line per event:
  - `{"type":"ok"}` for `on_load`, `on_shutdown`, `on_before_tool_call`,
    `on_after_tool_call`.
  - `{"type":"error","message":"..."}` for unknown events.

The host injects `HAND_DATA_DIR` (set to the session's
`extension_context().data_dir`) so the script can persist state.

## Layout

```
notify-sh/
├── extension.toml   # discovered by `discover_subprocess_extensions`
├── main.sh          # the subprocess host
└── README.md
```

Drop the directory into a `subprocess_extensions/` root passed to
`discover_subprocess_extensions` to load it.
