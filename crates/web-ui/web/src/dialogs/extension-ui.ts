// Extension UI handler — renders server-relayed extension UI requests and
// replies over the WebSocket.
//
// The server relays a loaded extension's host-UI calls as `extension_ui_request`
// frames (see `client/wire.ts`). This module renders the matching M9 dialog
// primitive for the interactive methods and applies the non-modal methods
// directly, then sends one `extension_ui_response` keyed by the request `id`.
//
// Interactive (modal) methods → reply with a value/confirmed/cancelled:
//   - select  → a single-choice list dialog (reply: chosen option as `value`)
//   - confirm → a yes/no dialog (reply: `confirmed`)
//   - input   → a single-line text dialog (reply: entered text as `value`)
//   - editor  → a multiline editor dialog (reply: edited text as `value`)
//
// Non-modal methods → applied immediately, no reply expected by the protocol:
//   - notify        → transient toast (info/warning/error)
//   - setStatus     → transient toast labeling the status
//   - setTitle      → updates the app header title
//   - setWidget     → transient toast summarizing the widget lines
//   - set_editor_text → toast hint (the chat editor is not script-driven here)
//
// NB: the server does not emit these frames during normal agent turns — they
// originate only when an extension calls the host UI. This handler is wired
// unconditionally so the capability lights up as soon as such an extension is
// present; until then it is dormant.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import type {
  ExtensionUiRequestFrame,
  ExtensionUiResponseCommand,
} from "../client/wire";
import type { WsConnection } from "../client/ws-connection";
import { Button } from "../ui/button";
import { DialogBase } from "../ui/dialog-base";
import { i18n } from "../utils/i18n";

/** Update the app-header title (used by the `setTitle` method). */
type SetTitleFn = (title: string) => void;

/**
 * Wire a WebSocket connection to the extension UI protocol. Returns an
 * unsubscribe function. `onSetTitle` (optional) lets the host update the app
 * header on a `setTitle` request; everything else is self-contained.
 */
export function installExtensionUiHandler(
  conn: WsConnection,
  onSetTitle?: SetTitleFn,
): () => void {
  return conn.onFrame((frame) => {
    if (frame.type !== "extension_ui_request") return;
    void handleRequest(conn, frame, onSetTitle);
  });
}

async function handleRequest(
  conn: WsConnection,
  req: ExtensionUiRequestFrame,
  onSetTitle?: SetTitleFn,
): Promise<void> {
  const reply = (body: Omit<ExtensionUiResponseCommand, "type" | "id">): void => {
    conn.send({ type: "extension_ui_response", id: req.id, ...body } as ExtensionUiResponseCommand);
  };

  switch (req.method) {
    case "select": {
      const choice = await ExtensionUiDialog.select(req.title, req.options, req.timeout);
      if (choice === null) reply({ cancelled: true });
      else reply({ value: choice });
      break;
    }
    case "confirm": {
      const confirmed = await ExtensionUiDialog.confirm(req.title, req.message, req.timeout);
      if (confirmed === null) reply({ cancelled: true });
      else reply({ confirmed });
      break;
    }
    case "input": {
      const value = await ExtensionUiDialog.input(req.title, req.placeholder, req.timeout);
      if (value === null) reply({ cancelled: true });
      else reply({ value });
      break;
    }
    case "editor": {
      const value = await ExtensionUiDialog.editor(req.title, req.prefill);
      if (value === null) reply({ cancelled: true });
      else reply({ value });
      break;
    }
    case "notify":
      showToast(req.message, req.notifyType ?? "info");
      break;
    case "setStatus":
      // A cleared status (no text) is a no-op toast.
      if (req.statusText) showToast(`${req.statusKey}: ${req.statusText}`, "info");
      break;
    case "setTitle":
      onSetTitle?.(req.title);
      break;
    case "setWidget": {
      const lines = req.widgetLines;
      if (lines && lines.length > 0) showToast(lines.join("\n"), "info");
      break;
    }
    case "set_editor_text":
      showToast(i18n("Suggested input: {text}", { text: req.text }), "info");
      break;
    default:
      break;
  }
}

// ---- modal dialog -----------------------------------------------------------

type DialogKind = "select" | "confirm" | "input" | "editor";

/**
 * A single dialog element backing all four interactive extension UI methods.
 * Each static helper resolves to the user's answer, or `null` when the dialog
 * is dismissed (Escape / backdrop / Cancel) or its `timeout` elapses.
 */
@customElement("extension-ui-dialog")
export class ExtensionUiDialog extends DialogBase {
  @state() private kind: DialogKind = "confirm";
  @state() private dialogTitle = "";
  @state() private message = "";
  @state() private options: string[] = [];
  @state() private draft = "";

  private resolveFn?: (value: string | boolean | null) => void;
  private settled = false;
  private timeoutHandle?: ReturnType<typeof setTimeout>;

  protected override modalWidth = "min(560px, 92vw)";

  static select(title: string, options: string[], timeout?: number): Promise<string | null> {
    const dialog = ExtensionUiDialog.create("select", title, timeout);
    dialog.options = options;
    return dialog.start() as Promise<string | null>;
  }

  static confirm(title: string, message: string, timeout?: number): Promise<boolean | null> {
    const dialog = ExtensionUiDialog.create("confirm", title, timeout);
    dialog.message = message;
    return dialog.start() as Promise<boolean | null>;
  }

  static input(title: string, placeholder?: string, timeout?: number): Promise<string | null> {
    const dialog = ExtensionUiDialog.create("input", title, timeout);
    dialog.message = placeholder ?? "";
    return dialog.start() as Promise<string | null>;
  }

  static editor(title: string, prefill?: string): Promise<string | null> {
    const dialog = ExtensionUiDialog.create("editor", title);
    dialog.draft = prefill ?? "";
    return dialog.start() as Promise<string | null>;
  }

  private static create(kind: DialogKind, title: string, timeout?: number): ExtensionUiDialog {
    const dialog = new ExtensionUiDialog();
    dialog.kind = kind;
    dialog.dialogTitle = title;
    if (timeout && timeout > 0) {
      dialog.timeoutHandle = setTimeout(() => dialog.settle(null), timeout);
    }
    return dialog;
  }

  private start(): Promise<string | boolean | null> {
    this.open();
    return new Promise((resolve) => {
      this.resolveFn = resolve;
    });
  }

  /** Resolve once, clear the timeout, and remove the dialog from the DOM. */
  private settle(value: string | boolean | null): void {
    if (this.settled) return;
    this.settled = true;
    if (this.timeoutHandle !== undefined) clearTimeout(this.timeoutHandle);
    this.resolveFn?.(value);
    this.resolveFn = undefined;
    super.close();
  }

  // Escape / backdrop close → treat as a cancellation.
  override close(): void {
    this.settle(null);
  }

  private commitText(): void {
    this.settle(this.draft);
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col gap-4 p-6">
        <h2 class="text-lg font-semibold text-foreground">${this.dialogTitle}</h2>
        ${this.renderBody()}
      </div>
    `;
  }

  private renderBody(): TemplateResult {
    switch (this.kind) {
      case "select":
        return this.renderSelect();
      case "confirm":
        return this.renderConfirm();
      case "input":
        return this.renderInput();
      case "editor":
        return this.renderEditor();
      default:
        return html``;
    }
  }

  private renderSelect(): TemplateResult {
    return html`
      <div class="flex flex-col gap-1 max-h-[50vh] overflow-y-auto">
        ${this.options.length === 0
          ? html`<p class="text-sm text-muted-foreground">${i18n("No options available")}</p>`
          : this.options.map(
              (option) => html`<button
                class="text-left text-sm px-3 py-2 rounded-md border border-border hover:bg-muted text-foreground transition-colors"
                @click=${() => this.settle(option)}
              >
                ${option}
              </button>`,
            )}
      </div>
      <div class="flex justify-end">
        ${Button({ variant: "ghost", size: "sm", onClick: () => this.settle(null), children: i18n("Cancel") })}
      </div>
    `;
  }

  private renderConfirm(): TemplateResult {
    return html`
      <p class="text-sm text-muted-foreground">${this.message}</p>
      <div class="flex gap-3 justify-end">
        ${Button({ variant: "outline", onClick: () => this.settle(false), children: i18n("No") })}
        ${Button({ variant: "default", onClick: () => this.settle(true), children: i18n("Yes") })}
      </div>
    `;
  }

  private renderInput(): TemplateResult {
    return html`
      <input
        class="rounded-md border border-border bg-background px-2 h-9 text-sm text-foreground outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring"
        .value=${this.draft}
        placeholder=${this.message}
        @input=${(e: Event) => {
          this.draft = (e.target as HTMLInputElement).value;
        }}
        @keydown=${(e: KeyboardEvent) => {
          if (e.key === "Enter" && !e.isComposing) {
            e.preventDefault();
            this.commitText();
          }
        }}
        ${autofocus()}
      />
      <div class="flex gap-3 justify-end">
        ${Button({ variant: "outline", onClick: () => this.settle(null), children: i18n("Cancel") })}
        ${Button({ variant: "default", onClick: () => this.commitText(), children: i18n("Submit") })}
      </div>
    `;
  }

  private renderEditor(): TemplateResult {
    return html`
      <textarea
        class="rounded-md border border-border bg-background px-2 py-2 text-sm text-foreground outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring min-h-[200px] resize-y font-mono"
        .value=${this.draft}
        @input=${(e: Event) => {
          this.draft = (e.target as HTMLTextAreaElement).value;
        }}
      ></textarea>
      <div class="flex gap-3 justify-end">
        ${Button({ variant: "outline", onClick: () => this.settle(null), children: i18n("Cancel") })}
        ${Button({ variant: "default", onClick: () => this.commitText(), children: i18n("Submit") })}
      </div>
    `;
  }
}

/** Lit directive-free autofocus: focus the rendered element on connect. */
function autofocus() {
  return (part: { element?: Element }) => {
    queueMicrotask(() => (part.element as HTMLElement | undefined)?.focus());
  };
}

// ---- non-modal toast --------------------------------------------------------

const TOAST_DURATION_MS = 4000;

const TOAST_VARIANT: Record<ExtensionUiToastKind, string> = {
  info: "bg-secondary text-foreground border-border",
  warning: "bg-secondary text-foreground border-border",
  error: "bg-destructive text-destructive-foreground border-destructive",
};

type ExtensionUiToastKind = "info" | "warning" | "error";

/**
 * Append a transient, dismissible toast to a bottom-right stack. Brand-neutral
 * and dependency-free; used for the non-modal extension UI methods.
 */
export function showToast(message: string, kind: ExtensionUiToastKind = "info"): void {
  const host = getToastHost();
  const toast = new ExtensionUiToast();
  toast.message = message;
  toast.kind = kind;
  toast.className = `pointer-events-auto`;
  host.appendChild(toast);
  setTimeout(() => toast.remove(), TOAST_DURATION_MS);
}

function getToastHost(): HTMLElement {
  let host = document.getElementById("extension-ui-toasts");
  if (!host) {
    host = document.createElement("div");
    host.id = "extension-ui-toasts";
    host.className =
      "fixed bottom-4 right-4 z-[60] flex flex-col gap-2 pointer-events-none max-w-sm";
    document.body.appendChild(host);
  }
  return host;
}

@customElement("extension-ui-toast")
export class ExtensionUiToast extends LitElement {
  message = "";
  kind: ExtensionUiToastKind = "info";

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override render(): TemplateResult {
    return html`<div
      class=${`rounded-md border px-3 py-2 text-sm shadow-lg whitespace-pre-wrap break-words ${TOAST_VARIANT[this.kind]}`}
      @click=${() => this.remove()}
      role="status"
    >
      ${this.message}
    </div>`;
  }
}
