# User-Cases: cli/initial_message

**Upstream source:** `pi-mono/packages/coding-agent/test/initial-message.test.ts` (3 cases)
**hand-ai source:**   `crates/coding-agent/src/cli/initial_message.rs`
**Surface:**          `build_initial_message(InitialMessageInput { messages, file_text, file_images, stdin_content }) -> InitialMessageResult` — composes the seed prompt for non-interactive runs from three sources (stdin / `@file` text / first positional message). Shifts the consumed message off `messages` so the caller can iterate the remainder as follow-up turns.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-im-001 | ✅ pass | `concatenates_stdin_file_text_and_first_message_in_order` — stdin + first CLI message land in one prompt; the first message is shifted off the queue |
| UC-im-002 | ✅ pass | `stdin_only_emits_stdin_text` — stdin alone becomes the prompt when no CLI message exists |
| UC-im-003 | ✅ pass | `concatenates_stdin_file_text_and_first_message_in_order` — stdin + file_text + first message compose in that order; subsequent CLI messages stay in the queue |

## Bonus coverage hand carries beyond pi

- `returns_none_when_all_inputs_empty` — `initial_message: None` when no source contributed.
- `preserves_remaining_messages_when_no_stdin_or_files` — message-only input still consumes the first message.
- `empty_file_text_does_not_contribute` — empty `file_text` is skipped so concatenation doesn't carry a phantom empty string.
- `passes_through_non_empty_images` / `empty_image_vec_collapses_to_none` — image attachments flow through; an empty vec collapses to `None` so callers see "no images" not "empty list".

## Cases (load-bearing)

### UC-im-001 — stdin + first CLI message merge into one prompt; message queue shrinks

**Given** `messages = ["Summarize the text given"]`, `stdin_content = Some("README contents\n")`.
**When** `build_initial_message` runs.
**Then** `initial_message == Some("README contents\nSummarize the text given")` and `messages` is empty (the first message was shifted off).

### UC-im-003 — stdin + file_text + first message compose in order; remainder stays

**Given** `messages = ["Explain it", "Second message"]`, `stdin = Some("stdin\n")`, `file_text = Some("file\n")`.
**Then** `initial_message == Some("stdin\nfile\nExplain it")`, and `messages == ["Second message"]`.

- Probe: `cargo test -p hand-coding-agent --lib cli::initial_message -- --exact`.
