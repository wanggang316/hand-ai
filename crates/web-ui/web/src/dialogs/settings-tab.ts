// SettingsTab — abstract base for a tab hosted inside <settings-dialog>.
//
// Each tab is a LitElement that renders into the light DOM (so Tailwind utility
// classes apply) and exposes a stable `id` and a human-readable `label` for the
// dialog's navigation. The dialog mounts every tab once and toggles visibility
// via `display:none`, so a tab's `connectedCallback` runs exactly once.
//
// Subclasses implement `renderContent()`; the base's `render()` simply delegates
// to it so a tab can be both used standalone and hosted in the dialog.

import { LitElement, type TemplateResult } from "lit";

export abstract class SettingsTab extends LitElement {
  /** Stable identifier (used for the nav `data-tab` and selection key). */
  abstract readonly id: string;
  /** Human-readable nav label. */
  abstract readonly label: string;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  /** Subclasses render the tab body here. */
  protected abstract renderContent(): TemplateResult;

  override render(): TemplateResult {
    return this.renderContent();
  }
}
