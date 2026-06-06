// <app-header> — the top bar above the chat panel.
//
// Buttons: Sessions (opens the session list), New session (resets), and Settings
// (opens the settings dialog) — all delegated to callbacks the bootstrap wires.
// Between them sits an inline-editable session title and a light/dark theme
// toggle. The title is a plain text span that becomes an input on click; commit
// (Enter / blur) calls `onRenameTitle`. The theme toggle flips a `data-theme`
// attribute on <html> and persists the choice via SettingsStore (basic toggle;
// full theming is M11).

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { MessageSquare, Moon, Plus, Settings, Sun } from "lucide";
import { icon } from "../ui/icons";
import { i18n } from "../utils/i18n";

export type Theme = "light" | "dark";

@customElement("app-header")
export class AppHeader extends LitElement {
  /** Current session title shown (and edited) in the bar. */
  @property() title = i18n("New Session");
  /** Current theme; reflected onto <html data-theme>. */
  @property() theme: Theme = "dark";

  /** Open the session-list dialog. */
  @property({ attribute: false }) onOpenSessions?: () => void;
  /** Start a new session (reset). */
  @property({ attribute: false }) onNewSession?: () => void;
  /** Open the settings dialog. */
  @property({ attribute: false }) onOpenSettings?: () => void;
  /** Persist a renamed session title. */
  @property({ attribute: false }) onRenameTitle?: (title: string) => void;
  /** Persist a theme change. */
  @property({ attribute: false }) onThemeChange?: (theme: Theme) => void;

  @state() private editing = false;
  @state() private draftTitle = "";

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  private startEdit(): void {
    this.draftTitle = this.title;
    this.editing = true;
    this.updateComplete.then(() => {
      const input = this.querySelector<HTMLInputElement>("input[data-title-edit]");
      input?.focus();
      input?.select();
    });
  }

  private commitEdit(): void {
    if (!this.editing) return;
    this.editing = false;
    const next = this.draftTitle.trim();
    if (next && next !== this.title) {
      this.title = next;
      this.onRenameTitle?.(next);
    }
  }

  private cancelEdit(): void {
    this.editing = false;
  }

  private toggleTheme(): void {
    const next: Theme = this.theme === "dark" ? "light" : "dark";
    this.theme = next;
    document.documentElement.setAttribute("data-theme", next);
    this.onThemeChange?.(next);
  }

  override render(): TemplateResult {
    return html`
      <header
        class="flex items-center gap-2 px-3 h-12 border-b border-border bg-background flex-shrink-0"
      >
        <button
          class="inline-flex items-center justify-center h-8 w-8 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          title=${i18n("Sessions")}
          @click=${() => this.onOpenSessions?.()}
        >
          ${icon(MessageSquare, "sm")}
        </button>
        <button
          class="inline-flex items-center justify-center h-8 w-8 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          title=${i18n("New Session")}
          @click=${() => this.onNewSession?.()}
        >
          ${icon(Plus, "sm")}
        </button>

        <div class="flex-1 min-w-0 flex justify-center">
          ${this.editing
            ? html`<input
                data-title-edit
                class="w-full max-w-md rounded-md border border-border bg-background px-2 h-8 text-sm text-foreground text-center outline-none focus:ring-1 focus:ring-ring"
                .value=${this.draftTitle}
                @input=${(e: Event) => {
                  this.draftTitle = (e.target as HTMLInputElement).value;
                }}
                @keydown=${(e: KeyboardEvent) => {
                  if (e.isComposing) return;
                  if (e.key === "Enter") {
                    e.preventDefault();
                    this.commitEdit();
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    this.cancelEdit();
                  }
                }}
                @blur=${() => this.commitEdit()}
              />`
            : html`<button
                class="max-w-md truncate text-sm font-medium text-foreground px-2 h-8 rounded-md hover:bg-muted transition-colors"
                title=${i18n("Rename session")}
                @click=${() => this.startEdit()}
              >
                ${this.title}
              </button>`}
        </div>

        <button
          class="inline-flex items-center justify-center h-8 w-8 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          title=${this.theme === "dark" ? i18n("Switch to light theme") : i18n("Switch to dark theme")}
          @click=${() => this.toggleTheme()}
        >
          ${this.theme === "dark" ? icon(Sun, "sm") : icon(Moon, "sm")}
        </button>
        <button
          class="inline-flex items-center justify-center h-8 w-8 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          title=${i18n("Settings")}
          @click=${() => this.onOpenSettings?.()}
        >
          ${icon(Settings, "sm")}
        </button>
      </header>
    `;
  }
}
