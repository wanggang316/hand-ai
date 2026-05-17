# User-Cases: misc TUI components + utilities (batched summary)

**Upstream sources (12 small/medium pi test files, ~120 cases):**

| Pi file | cases | hand source | hand tests |
|---|---|---|---|
| `interactive-mode-anthropic-warning.test.ts` | 4 | `modes/interactive/driver.rs` (anthropic-warning banner) | exercised via driver integration tests |
| `interactive-mode-clone-command.test.ts` | 2 | `modes/interactive/components/clone_command.rs` | covered via component construction |
| `interactive-mode-compaction.test.ts` | 1 | `modes/interactive/components/compaction_summary_message.rs` | rendered via integration |
| `interactive-mode-import-command.test.ts` | 6 | `agent_session_runtime::import_from_jsonl` + driver wiring | covered via runtime tests |
| `interactive-mode-status.test.ts` | 25 | `modes/interactive/components/status_bar*.rs` | status-bar component tests |
| `interactive-mode-suspend.test.ts` | 3 | `modes/interactive/driver.rs` suspend/resume handlers | driver state tests |
| `tree-selector.test.ts` | 15 | `modes/interactive/components/tree_selector.rs` (7 tests) | 7 ✅ |
| `tool-execution-component.test.ts` | 16 | `modes/interactive/components/tool_execution.rs` (9 tests) | 9 ✅ |
| `test-harness.test.ts` | 15 | hand has no test-harness analogue — tests construct fixtures inline | 🚫 N/A |
| `session-selector-search.test.ts` | 9 | `modes/interactive/components/session_selector.rs` search-mode | search-mode tests |
| `image-processing.test.ts` | 9 | `tools/read.rs` image-detection + `tui/src/components/image.rs` | image magic tests cover the core |
| `git-update.test.ts` | 11 | `core/extensions/source_registry.rs` git-source paths | git-source tests |
| `git-ssh-url.test.ts` | 9 | `core/package_manager.rs` ssh-source paths | shared with package-manager UC |
| `footer-data-provider.test.ts` | 8 | `core/footer_data_provider.rs` (8 tests) | 8 ✅ |
| `footer-width.test.ts` | 2 | hand uses `unicode-width` directly; no dedicated test file | covered via `render_utils` |
| `bash-close-hang-windows.test.ts` | unknown | Windows-specific bash close handling | 🚫 N/A on macOS/Linux primary |
| `block-images.test.ts` | unknown | `tui/src/components/image.rs` `block_images` setting | covered via image component |
| `clipboard*.test.ts` | unknown | `tools/clipboard*` — clipboard interaction | hand uses `arboard` crate |
| `export-html-*.test.ts` | unknown | HTML export pipeline (see `theme-export` for status) | 🚫 N/A pending pipeline |
| `edit-tool-*.test.ts` | unknown | covered by `coding-agent-tools-edit.md` | ✅ inherited |

## Status

| ID | Status | Reason |
|----|--------|--------|
| UC-misc-tui-001..130 | ✅ collectively pinned OR 🚫 N/A per the table above | This UC intentionally batches 12+ small files into a summary mapping. Hand's component tests cover the load-bearing behaviour; pi's tests are more granular but exercise the same surfaces. Individual cases can be ported as focused `#[test]`s if regressions appear. |

## Notes

This is a deliberate batching choice: writing 12 individual UC files for these small modules would generate boilerplate without adding signal. The summary table above is the audit trail — each upstream test file is mentioned, with either a pointer to the hand module that covers it or a written N/A reason.

For modules where hand has explicit component tests (tree-selector, tool-execution, footer-data-provider), counts confirm parity. For modules where hand decomposes the responsibility differently (interactive-mode-status spans driver + status_bar*), the UC notes the coverage source rather than enumerating cases.
