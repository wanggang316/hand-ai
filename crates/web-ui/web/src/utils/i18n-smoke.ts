// Smoke check for the i18n translation table. Switches the active language to
// `de`, reads back a few representative lookups (including one with a `{param}`
// substitution), reports the size of the de table, then restores `en`. Used by
// the milestone controller to confirm the table is populated and lookups work.

import { getLanguage, i18n, setLanguage, translations } from "./i18n";

export interface I18nSmokeResult {
  /** Number of entries in the de translation table. */
  deKeyCount: number;
  /** A handful of `[key, translated]` pairs resolved under `de`. */
  sample: [string, string][];
}

export function runI18nSmoke(): I18nSmokeResult {
  const previous = getLanguage();
  try {
    setLanguage("de");
    const sample: [string, string][] = [
      ["Settings", i18n("Settings")],
      ["Cancel", i18n("Cancel")],
      ["Send", i18n("Send")],
      ["Free", i18n("Free")],
      // exercises {param} substitution
      ["{days} days ago", i18n("{days} days ago", { days: 3 })],
    ];
    return {
      deKeyCount: Object.keys(translations.de).length,
      sample,
    };
  } finally {
    setLanguage(previous);
  }
}
