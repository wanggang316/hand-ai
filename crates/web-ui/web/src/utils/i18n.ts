// i18n: a small, dependency-free translation layer. `i18n(key, params?)` looks
// the key up in the active language's table and falls back to the key itself
// (the keys are written as English source strings, so `en` is identity and only
// the non-identity `de` overrides are stored). `{param}` tokens in the resolved
// string are substituted from `params`.
//
// `setLanguage(lang)` switches the active language and notifies subscribers so
// components can re-render. Two notification paths are exposed: `subscribe(fn)`
// (returns an unsubscribe) and a `language-change` CustomEvent dispatched on
// `window`, so both functional helpers and custom elements can react.
//
// Brand-neutral: every value below is vendored locally. No reference-brand
// strings appear in keys or values.

export type Language = "en" | "de";

export type TranslationTable = Record<string, string>;

export type Translations = Record<Language, TranslationTable>;

/**
 * The German overrides. `en` is the identity of the keys, so it is left empty
 * (the lookup falls back to the key). `de` provides a translation for every
 * call-site key found across `src/`, preserving `{placeholder}` tokens exactly.
 */
export const translations: Translations = {
  en: {},
  de: {
    Free: "Kostenlos",
    Cancel: "Abbrechen",
    Close: "Schließen",
    "Select Model": "Modell auswählen",
    "Search models...": "Modelle suchen...",
    "No models found": "Keine Modelle gefunden",
    Thinking: "Thinking",
    Vision: "Vision",
    "Type a message...": "Nachricht eingeben...",
    "Drop files here": "Dateien hier ablegen",
    "Attach files": "Dateien anhängen",
    Send: "Senden",
    Stop: "Stopp",
    "Maximum {n} files allowed": "Maximal {n} Dateien erlaubt",
    "{name} exceeds the maximum size of {mb}MB":
      "{name} überschreitet die maximale Größe von {mb}MB",
    "Failed to process {name}: {error}": "Verarbeitung von {name} fehlgeschlagen: {error}",
    // Tool / artifact rendering
    Call: "Aufruf",
    "Tool Call": "Tool-Aufruf",
    Result: "Ergebnis",
    "(no result)": "(kein Ergebnis)",
    "(no output)": "(keine Ausgabe)",
    Input: "Eingabe",
    Output: "Ausgabe",
    "Request aborted": "Anfrage abgebrochen",
    "An error occurred": "Ein Fehler ist aufgetreten",
    "Error:": "Fehler:",
    console: "Konsole",
    Code: "Code",
    Preview: "Vorschau",
    "Copy output": "Ausgabe kopieren",
    "Copy logs": "Logs kopieren",
    Copy: "Kopieren",
    "Copied!": "Kopiert!",
    Download: "Herunterladen",
    "Copy HTML": "HTML kopieren",
    "Download HTML": "HTML herunterladen",
    "Reload HTML": "HTML neu laden",
    "Copy SVG": "SVG kopieren",
    "Download SVG": "SVG herunterladen",
    "Copy Markdown": "Markdown kopieren",
    "Download Markdown": "Markdown herunterladen",
    "Show artifacts": "Artefakte anzeigen",
    "Close artifacts": "Artefakte schließen",
    Artifacts: "Artefakte",
    "Autoscroll enabled": "Automatisches Scrollen aktiviert",
    "Autoscroll disabled": "Automatisches Scrollen deaktiviert",
    "Getting logs": "Hole Logs",
    "Got logs": "Logs geholt",
    "No logs for {filename}": "Keine Logs für {filename}",
    "Creating artifact": "Erstelle Artefakt",
    "Created artifact": "Artefakt erstellt",
    "Updating artifact": "Aktualisiere Artefakt",
    "Updated artifact": "Artefakt aktualisiert",
    "Rewriting artifact": "Überschreibe Artefakt",
    "Rewrote artifact": "Artefakt überschrieben",
    "Deleting artifact": "Lösche Artefakt",
    "Deleted artifact": "Artefakt gelöscht",
    "Getting artifact": "Hole Artefakt",
    "Got artifact": "Artefakt geholt",
    "Processing artifact": "Verarbeite Artefakt",
    "Processed artifact": "Artefakt verarbeitet",
    "Preparing artifact...": "Bereite Artefakt vor...",
    // Tool execution states
    "Preparing tool...": "Bereite Tool vor...",
    "Preparing tool parameters...": "Bereite Tool-Parameter vor...",
    Calculating: "Berechne",
    "Writing expression...": "Schreibe Ausdruck...",
    "Waiting for expression...": "Warte auf Ausdruck...",
    "Getting current time in": "Hole aktuelle Zeit in",
    "Getting current date and time": "Hole aktuelles Datum und Uhrzeit",
    "Getting time...": "Hole Zeit...",
    "Waiting for command...": "Warte auf Befehl...",
    "Running command...": "Führe Befehl aus...",
    "Executing JavaScript": "Führe JavaScript aus",
    "Preparing JavaScript...": "Bereite JavaScript vor...",
    // Document / attachment handling
    PDF: "PDF",
    Document: "Dokument",
    Presentation: "Präsentation",
    Spreadsheet: "Tabelle",
    Text: "Text",
    "Failed to fetch file": "Datei konnte nicht abgerufen werden",
    "Invalid source type": "Ungültiger Quellentyp",
    "Error loading file": "Fehler beim Laden der Datei",
    "Error loading PDF": "Fehler beim Laden des PDFs",
    "Error loading document": "Fehler beim Laden des Dokuments",
    "Error loading spreadsheet": "Fehler beim Laden der Tabelle",
    "Failed to load PDF": "PDF konnte nicht geladen werden",
    "Failed to load document": "Dokument konnte nicht geladen werden",
    "Failed to load spreadsheet": "Tabelle konnte nicht geladen werden",
    "Failed to extract document": "Dokument konnte nicht extrahiert werden",
    "Extracted text from document": "Text aus Dokument extrahiert",
    "No text content available": "Kein Textinhalt verfügbar",
    "No content available": "Kein Inhalt verfügbar",
    "Failed to display text content": "Textinhalt konnte nicht angezeigt werden",
    "Preview not available for this file type.": "Vorschau für diesen Dateityp nicht verfügbar.",
    "Click the download button above to view it on your computer.":
      "Klicken Sie oben auf die Download-Schaltfläche, um die Datei auf Ihrem Computer anzuzeigen.",
    Loading: "Lädt",
    "Loading...": "Lädt...",
    // Settings & API keys
    Settings: "Einstellungen",
    "API Keys": "API-Schlüssel",
    "API Key (Optional)": "API-Schlüssel (Optional)",
    "API Key Required": "API-Schlüssel erforderlich",
    "Enter API key": "API-Schlüssel eingeben",
    "Enter an API key for {provider} to continue.":
      "Geben Sie einen API-Schlüssel für {provider} ein, um fortzufahren.",
    "Configure API keys for LLM providers. Keys are stored locally in your browser.":
      "Konfigurieren Sie API-Schlüssel für LLM-Anbieter. Schlüssel werden lokal in Ihrem Browser gespeichert.",
    "Key stored": "Schlüssel gespeichert",
    Save: "Speichern",
    "Saving...": "Speichere...",
    "Failed to save": "Speichern fehlgeschlagen",
    Remove: "Entfernen",
    Edit: "Bearbeiten",
    Refresh: "Aktualisieren",
    "Checking...": "Überprüfe...",
    "Testing...": "Teste...",
    Disconnected: "Getrennt",
    "Connecting…": "Verbinde…",
    "Reconnecting…": "Verbinde erneut…",
    // Proxy (document-fetch)
    Proxy: "Proxy",
    "Proxy URL": "Proxy-URL",
    "Use document-fetch proxy": "Dokument-Abruf-Proxy verwenden",
    "Format: the proxy must accept requests as <proxy-url>/?url=<target-url>":
      "Format: Der Proxy muss Anfragen als <proxy-url>/?url=<ziel-url> akzeptieren",
    "Lets the in-browser document fetcher bypass CORS restrictions when extracting remote documents. This does not affect LLM calls, which are made server-side.":
      "Ermöglicht dem In-Browser-Dokumentabruf, CORS-Einschränkungen beim Extrahieren entfernter Dokumente zu umgehen. Dies betrifft keine LLM-Aufrufe, die serverseitig erfolgen.",
    // Providers & models
    "Providers & Models": "Anbieter & Modelle",
    "Cloud Providers": "Cloud-Anbieter",
    "Cloud LLM providers with predefined models. API keys are stored locally in your browser.":
      "Cloud-LLM-Anbieter mit vordefinierten Modellen. API-Schlüssel werden lokal in Ihrem Browser gespeichert.",
    "Custom Providers": "Benutzerdefinierte Anbieter",
    "User-configured servers with auto-discovered or manually defined models.":
      "Benutzerkonfigurierte Server mit automatisch erkannten oder manuell definierten Modellen.",
    "Add Provider": "Anbieter hinzufügen",
    "Edit Provider": "Anbieter bearbeiten",
    "No custom providers configured. Click 'Add Provider' to get started.":
      "Keine benutzerdefinierten Anbieter konfiguriert. Klicken Sie auf 'Anbieter hinzufügen', um zu beginnen.",
    Models: "Modelle",
    models: "Modelle",
    Discovered: "Erkannt",
    "Provider Name": "Anbietername",
    "Provider Type": "Anbietertyp",
    "Base URL": "Basis-URL",
    "e.g., My Ollama Server": "z.B. Mein Ollama Server",
    "e.g., http://localhost:11434": "z.B. http://localhost:11434",
    "Leave empty if not required": "Leer lassen, falls nicht erforderlich",
    "Test Connection": "Verbindung testen",
    "For manual provider types, add models after saving the provider.":
      "Für manuelle Anbietertypen fügen Sie Modelle nach dem Speichern des Anbieters hinzu.",
    "Please fill in all required fields": "Bitte füllen Sie alle erforderlichen Felder aus",
    "Failed to save provider": "Anbieter konnte nicht gespeichert werden",
    and: "und",
    more: "mehr",
    // Sessions
    Sessions: "Sitzungen",
    "New Session": "Neue Sitzung",
    "Load a previous conversation": "Frühere Konversation laden",
    "No sessions yet": "Noch keine Sitzungen",
    "Rename session": "Sitzung umbenennen",
    "Confirm delete": "Löschen bestätigen",
    Delete: "Löschen",
    "No session available": "Keine Sitzung verfügbar",
    "No session set": "Keine Sitzung gesetzt",
    "No agent set": "Kein Agent gesetzt",
    Today: "Heute",
    Yesterday: "Gestern",
    "{days} days ago": "vor {days} Tagen",
    messages: "Nachrichten",
    error: "Fehler",
    errors: "Fehler",
    // Persistent storage
    "Storage Permission": "Speicherberechtigung",
    "Allow persistent storage so your conversations are not cleared when the browser needs disk space.":
      "Erlauben Sie dauerhaften Speicher, damit Ihre Konversationen nicht gelöscht werden, wenn der Browser Speicherplatz benötigt.",
    "Your conversations are saved locally in your browser.":
      "Ihre Konversationen werden lokal in Ihrem Browser gespeichert.",
    "Data will not be deleted automatically to free up space.":
      "Daten werden nicht automatisch gelöscht, um Speicherplatz freizugeben.",
    "No data is sent to external servers.": "Keine Daten werden an externe Server gesendet.",
    "Grant Permission": "Berechtigung erteilen",
    "Requesting...": "Anfrage läuft...",
    "Continue Anyway": "Trotzdem fortfahren",
    "Could not request persistent storage. Your data is still saved locally.":
      "Dauerhafter Speicher konnte nicht angefordert werden. Ihre Daten werden weiterhin lokal gespeichert.",
    "Persistent storage is not supported in this browser. Your data is still saved locally but may be cleared under storage pressure.":
      "Dauerhafter Speicher wird in diesem Browser nicht unterstützt. Ihre Daten werden weiterhin lokal gespeichert, können aber bei Speicherknappheit gelöscht werden.",
    // Thinking levels
    Off: "Aus",
    Minimal: "Minimal",
    Low: "Niedrig",
    Medium: "Mittel",
    High: "Hoch",
    // Theme
    "Switch to dark theme": "Zum dunklen Design wechseln",
    "Switch to light theme": "Zum hellen Design wechseln",
  },
};

let currentLanguage: Language = "en";

/** Subscribers notified whenever the active language changes. */
type LanguageListener = (lang: Language) => void;
const listeners = new Set<LanguageListener>();

/** Custom event dispatched on `window` when the active language changes. */
export const LANGUAGE_CHANGE_EVENT = "language-change";

/**
 * Subscribe to language changes. Returns an unsubscribe function. Components
 * that cannot easily attach a `window` listener (e.g. functional helpers) can
 * use this; long-lived custom elements may prefer the `language-change` event.
 */
export function subscribe(listener: LanguageListener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function setLanguage(lang: Language): void {
  if (lang === currentLanguage) return;
  currentLanguage = lang;
  for (const listener of listeners) {
    try {
      listener(lang);
    } catch (e) {
      console.error("language-change listener failed", e);
    }
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(LANGUAGE_CHANGE_EVENT, { detail: { language: lang } }));
  }
}

export function getLanguage(): Language {
  return currentLanguage;
}

/**
 * Resolve a UI string. Returns the translated value for the active language if
 * present, otherwise the key (which is itself the English source text). Any
 * `{name}` tokens are substituted from `params`.
 */
export function i18n(key: string, params?: Record<string, string | number>): string {
  let value = translations[currentLanguage]?.[key] ?? key;
  if (params) {
    for (const [name, replacement] of Object.entries(params)) {
      value = value.replaceAll(`{${name}}`, String(replacement));
    }
  }
  return value;
}
