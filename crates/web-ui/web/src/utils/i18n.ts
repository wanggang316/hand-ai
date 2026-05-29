// Minimal i18n shim. The full translation table (en + de, ~200 keys) lands in
// the theming/i18n milestone. For now `i18n(key)` returns the active-language
// string if present, otherwise the key itself (the keys are written as English
// strings). `setLanguage` and `translations` are exported so the full table can
// be populated later without changing call sites.

export type Language = "en" | "de";

export type Translations = Record<Language, Record<string, string>>;

export const translations: Translations = {
  en: {},
  de: {},
};

let currentLanguage: Language = "en";

export function setLanguage(lang: Language): void {
  currentLanguage = lang;
}

export function getLanguage(): Language {
  return currentLanguage;
}

/**
 * Resolve a UI string. Returns the translated value if the active language has
 * an entry for `key`, otherwise the key (which is itself the English text).
 */
export function i18n(key: string): string {
  return translations[currentLanguage]?.[key] ?? key;
}
