# Migration to hand-tui 0.1

This is the first usable release of `hand-tui`. There is no prior version to migrate from.

## API Stability

- The `Component`, `Focusable`, and `Tui` traits / structs are stable.
- The `InputEvent` enum is stable; new variants may be added in minor versions and will be marked `#[non_exhaustive]` if added.
- See `README.md` "Migration / Known Limitations" for behavioral divergences from the upstream `pi-tui` TypeScript port.
