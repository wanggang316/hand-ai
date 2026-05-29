# Web UI — User Acceptance Test Suite

User-perspective acceptance test cases for the browser chat UI (`crates/web-ui`).
Every case is written from a real persona's point of view and is verifiable from
observable behaviour alone — no knowledge of the implementation is required to
run it. Each case carries explicit, individually checkable assertions.

**Totals: 15 areas · 483 cases · 1505 assertions.**

## How to read a case

Each case follows a fixed template:

```
### WUI-<AREA>-NN: <one-line title>
- **Persona:**       who is doing this and why
- **Preconditions:** the state the system must be in before the steps
- **Steps:**         numbered, observable actions a user performs
- **Assertions:**    A1..Ak — each an independently checkable expectation
- **Traces:**        capability-matrix row(s) / milestone / spec section
```

- **Case IDs** are stable: `WUI-<AREA>-NN`. Reference them in bug reports and
  review notes; renumbering is avoided.
- **Assertions** are the unit of pass/fail. A case passes only when *all* of its
  assertions hold. Cite the failing `A#` when reporting.
- **Traces** link each case back to the capability parity matrix and milestones
  in `docs/exec-plans/web-ui.md`, and to sections of
  `docs/web-ui-architecture.md`.

## Areas

| # | File | Area | Case prefix | Cases | Assertions |
|---|------|------|-------------|------:|-----------:|
| 01 | [01-chat-shell.md](01-chat-shell.md) | Chat shell & conversation flow | `WUI-CHAT` | 38 | 94 |
| 02 | [02-message-tool-rendering.md](02-message-tool-rendering.md) | Message & tool rendering | `WUI-MSG` | 35 | 118 |
| 03 | [03-sandbox-runtime.md](03-sandbox-runtime.md) | Sandbox runtime | `WUI-SBX` | 31 | 91 |
| 04 | [04-artifacts.md](04-artifacts.md) | Artifacts | `WUI-ART` | 54 | 166 |
| 05 | [05-browser-tools.md](05-browser-tools.md) | Browser tools (JS REPL & extract document) | `WUI-TOOL` | 26 | 99 |
| 06 | [06-attachments.md](06-attachments.md) | Attachments | `WUI-ATT` | 44 | 129 |
| 07 | [07-storage-sessions.md](07-storage-sessions.md) | Storage & sessions | `WUI-STO` | 28 | 89 |
| 08 | [08-providers-models.md](08-providers-models.md) | Providers & models | `WUI-MDL` | 36 | 109 |
| 09 | [09-dialogs-settings.md](09-dialogs-settings.md) | Dialogs, settings & app header | `WUI-DLG` | 32 | 94 |
| 10 | [10-networking.md](10-networking.md) | Networking (WebSocket, upload/download, dispatch) | `WUI-NET` | 32 | 109 |
| 11 | [11-image-and-session-replay.md](11-image-and-session-replay.md) | Image delivery & session replay | `WUI-CTX` | 20 | 59 |
| 12 | [12-i18n-theming.md](12-i18n-theming.md) | i18n, theming & design system | `WUI-UIX` | 32 | 97 |
| 13 | [13-packaging-deploy.md](13-packaging-deploy.md) | Packaging & deployment | `WUI-PKG` | 23 | 73 |
| 14 | [14-extension-ui.md](14-extension-ui.md) | Extension UI protocol | `WUI-EXT` | 25 | 86 |
| 15 | [15-robustness-security.md](15-robustness-security.md) | Robustness & security | `WUI-SEC` | 27 | 92 |
| | | **Total** | | **483** | **1505** |

## Scope & coverage

The suite covers the full surface of the web UI as built across milestones
M0–M12: the WebSocket-backed chat shell and streaming; message/tool/markdown
rendering; the sandboxed code runtime and artifacts panel; browser-executed
tools (JS REPL, document extraction); attachment upload/download and image
delivery; IndexedDB persistence and session save/load/replay; provider and model
management; dialogs, settings and the app header; the networking layer
(reconnect, correlation, dispatch); internationalisation, theming and the design
system; single-binary packaging and deployment; the extension UI request/response
protocol; and cross-cutting robustness and security behaviour.

Cases are behaviour-level and tool-agnostic: they can be executed manually, or
driven by a browser-automation harness, against a running `hand-web-ui` server.
