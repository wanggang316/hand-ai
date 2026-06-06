// <theme-toggle> — a standalone light/dark theme switch. Flips the `data-theme`
// attribute on <html> (which the app.css token sets follow) and emits a
// `theme-change` CustomEvent so a host can persist the choice. Self-contained:
// it reads the current theme from <html data-theme> (falling back to the OS
// preference) so it works whether or not a host wires it up.
//
// The app header has its own inline toggle for layout reasons; this element is
// the reusable primitive for any other surface that needs a theme switch.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import { Moon, Sun } from "lucide";
import { i18n } from "../utils/i18n";
import { icon } from "./icons";

export type Theme = "light" | "dark";

function resolveCurrentTheme(): Theme {
  const attr = document.documentElement.getAttribute("data-theme");
  if (attr === "light" || attr === "dark") return attr;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

@customElement("theme-toggle")
export class ThemeToggle extends LitElement {
  /** Current theme; reflected onto <html data-theme> on toggle. */
  @property() theme: Theme = resolveCurrentTheme();
  /** Optional class applied to the button (sizing/spacing overrides). */
  @property() override className = "";

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  private toggle(): void {
    const next: Theme = this.theme === "dark" ? "light" : "dark";
    this.theme = next;
    document.documentElement.setAttribute("data-theme", next);
    this.dispatchEvent(
      new CustomEvent<Theme>("theme-change", { detail: next, bubbles: true, composed: true }),
    );
  }

  override render(): TemplateResult {
    const cls = [
      "inline-flex items-center justify-center h-8 w-8 rounded-md",
      "hover:bg-muted text-muted-foreground hover:text-foreground transition-colors",
      this.className,
    ]
      .filter(Boolean)
      .join(" ");
    return html`<button
      class=${cls}
      title=${this.theme === "dark" ? i18n("Switch to light theme") : i18n("Switch to dark theme")}
      @click=${() => this.toggle()}
    >
      ${this.theme === "dark" ? icon(Sun, "sm") : icon(Moon, "sm")}
    </button>`;
  }
}
