// Abstract base for all artifact viewer elements. Renders into the light DOM so
// shared Tailwind utility classes apply. Each concrete viewer exposes a
// `content` getter/setter (the artifact's raw string content) and a
// `getHeaderButtons()` that returns the per-type header actions (copy, download,
// preview/code toggle, reload, ...) the panel places in the tab bar.

import { LitElement, type TemplateResult } from "lit";

export abstract class ArtifactElement extends LitElement {
  public filename = "";

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this; // light DOM for shared styles
  }

  public abstract get content(): string;
  public abstract set content(value: string);

  abstract getHeaderButtons(): TemplateResult | HTMLElement;
}
