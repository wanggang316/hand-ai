# User-Cases: modes/interactive/theme — export

**Upstream source:** `pi-mono/packages/coding-agent/test/theme-export.test.ts` (2 cases)
**hand-ai source:**   `crates/coding-agent/src/modes/interactive/theme/` (color.rs / core.rs)
**Surface:**          `getThemeExportColors(themeName)` resolves the `export:` block on a theme file (variable references + 256-color → hex conversion) for HTML export.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-th-001 | 🚫 N/A | "resolves export variable references using the same syntax as colors" — the HTML export pipeline is not ported. `color.rs:10` documents this explicitly: `ansi256ToHex`, `getResolvedThemeColors`, `getThemeExportColors`, `isLightTheme` are intentionally absent in hand pending the export pipeline. Theme files themselves load and serialise correctly (`ThemeExport` struct is defined and populated), so the data shape is in place — only the resolver function is missing. |
| UC-th-002 | 🚫 N/A | "resolves recursive vars and converts 256-color export values to hex" — same reason. Track via `docs/exec-plans/parity-completion.md` §A1 when the export pipeline lands. |

## Notes

The two pi tests exercise a behaviour that depends on a multi-step transform: variable interpolation → 256-color quantisation → hex serialisation. Reproducing them in hand requires porting the entire `getThemeExportColors` helper plus its dependencies. Marking N/A is honest until the export pipeline lands; the `ThemeExport` carrier struct already exists so the data side is ready when the resolver gets ported.
