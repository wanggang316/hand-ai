// <session-list-dialog> — lists persisted sessions and loads one on click.
//
// Cards show title, a relative date, message count, and a usage summary
// (`utils/format`). Clicking a card calls `onLoad(id)` and closes the dialog.
// Each card has a delete button that uses an IN-UI two-step confirmation (the
// delete icon turns into a "Confirm / Cancel" pair) — no window.confirm/alert.

import { html, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import { Trash2 } from "lucide";
import { getAppStorage } from "../storage/app-storage";
import type { SessionMetadata } from "../storage/backend";
import { icon } from "../ui/icons";
import { formatUsage } from "../utils/format";
import { i18n } from "../utils/i18n";
import { DialogBase } from "../ui/dialog-base";

@customElement("session-list-dialog")
export class SessionListDialog extends DialogBase {
  @state() private sessions: SessionMetadata[] = [];
  @state() private loading = true;
  // Id currently awaiting a second click to confirm deletion (two-step button).
  @state() private confirmingId: string | null = null;

  private onLoadCallback?: (sessionId: string) => void;
  private onDeleteCallback?: (sessionId: string) => void;

  protected override modalWidth = "min(600px, 90vw)";
  protected override modalHeight = "min(700px, 90vh)";

  /** Mount, open, and populate the session list. */
  static open(
    onLoad: (sessionId: string) => void,
    onDelete?: (sessionId: string) => void,
  ): SessionListDialog {
    const dialog = new SessionListDialog();
    dialog.onLoadCallback = onLoad;
    dialog.onDeleteCallback = onDelete;
    dialog.open();
    void dialog.loadSessions();
    return dialog;
  }

  private async loadSessions(): Promise<void> {
    this.loading = true;
    try {
      this.sessions = await getAppStorage().sessions.getAllMetadata();
    } catch (err) {
      console.error("Failed to load sessions:", err);
      this.sessions = [];
    } finally {
      this.loading = false;
    }
  }

  private handleSelect(sessionId: string): void {
    this.onLoadCallback?.(sessionId);
    this.close();
  }

  private async confirmDelete(sessionId: string): Promise<void> {
    this.confirmingId = null;
    try {
      await getAppStorage().sessions.delete(sessionId);
      this.onDeleteCallback?.(sessionId);
      await this.loadSessions();
    } catch (err) {
      console.error("Failed to delete session:", err);
    }
  }

  private formatDate(isoString: string): string {
    const date = new Date(isoString);
    const days = Math.floor((Date.now() - date.getTime()) / (1000 * 60 * 60 * 24));
    if (days <= 0) return i18n("Today");
    if (days === 1) return i18n("Yesterday");
    if (days < 7) return i18n("{days} days ago").replace("{days}", String(days));
    return date.toLocaleDateString();
  }

  private renderDeleteControl(session: SessionMetadata): TemplateResult {
    if (this.confirmingId === session.id) {
      return html`<div class="flex items-center gap-1 flex-shrink-0" @click=${(e: Event) => e.stopPropagation()}>
        <button
          class="px-2 py-1 text-xs rounded bg-destructive text-destructive-foreground hover:bg-destructive/90 transition-colors"
          @click=${() => void this.confirmDelete(session.id)}
          title=${i18n("Confirm delete")}
        >
          ${i18n("Delete")}
        </button>
        <button
          class="px-2 py-1 text-xs rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          @click=${() => {
            this.confirmingId = null;
          }}
          title=${i18n("Cancel")}
        >
          ${i18n("Cancel")}
        </button>
      </div>`;
    }
    return html`<button
      class="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-destructive/10 text-destructive transition-opacity flex-shrink-0"
      @click=${(e: Event) => {
        e.stopPropagation();
        this.confirmingId = session.id;
      }}
      title=${i18n("Delete")}
    >
      ${icon(Trash2, "sm")}
    </button>`;
  }

  private renderCard(session: SessionMetadata): TemplateResult {
    return html`<div
      class="group flex items-start gap-3 p-3 rounded-lg border border-border hover:bg-secondary/50 cursor-pointer transition-colors"
      @click=${() => this.handleSelect(session.id)}
    >
      <div class="flex-1 min-w-0">
        <div class="font-medium text-sm text-foreground truncate">${session.title}</div>
        <div class="text-xs text-muted-foreground mt-1">${this.formatDate(session.lastModified)}</div>
        <div class="text-xs text-muted-foreground mt-1">
          ${session.messageCount} ${i18n("messages")}${formatUsage(session.usage)
            ? html` · ${formatUsage(session.usage)}`
            : ""}
        </div>
      </div>
      ${this.renderDeleteControl(session)}
    </div>`;
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col h-full overflow-hidden p-6">
        <div class="pb-4 flex-shrink-0">
          <h2 class="text-lg font-semibold text-foreground">${i18n("Sessions")}</h2>
          <p class="text-sm text-muted-foreground mt-1">${i18n("Load a previous conversation")}</p>
        </div>
        <div class="flex-1 overflow-y-auto space-y-2">
          ${this.loading
            ? html`<div class="text-center py-8 text-muted-foreground">${i18n("Loading...")}</div>`
            : this.sessions.length === 0
              ? html`<div class="text-center py-8 text-muted-foreground">${i18n("No sessions yet")}</div>`
              : this.sessions.map((session) => this.renderCard(session))}
        </div>
      </div>
    `;
  }
}
