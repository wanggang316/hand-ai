//! Build script: warn (do not fail) when the embedded frontend bundle is
//! missing for a release build.
//!
//! `rust-embed` embeds `web/dist/**` into the release binary. In debug builds
//! it reads those files from disk at runtime, so a missing `web/dist` is fine
//! there — `cargo check` / tests must not be blocked on a frontend build.
//!
//! For a release build we emit a `cargo:warning=` (not a hard error) when
//! `web/dist/index.html` is absent, so a self-contained binary is not shipped
//! with an empty bundle by accident. The build wrapper
//! (`scripts/build-web-ui.sh`) runs the Vite build first to satisfy this.

use std::path::Path;

fn main() {
    // Rebuild bookkeeping: re-run if the built index changes.
    println!("cargo:rerun-if-changed=web/dist/index.html");

    // Only meaningful for release; debug reads from disk at runtime.
    let profile = std::env::var("PROFILE").unwrap_or_default();
    if profile != "release" {
        return;
    }

    if !Path::new("web/dist/index.html").exists() {
        println!(
            "cargo:warning=web/dist/index.html not found; the release binary will embed an empty \
             frontend bundle. Run scripts/build-web-ui.sh (or `npm --prefix crates/web-ui/web run \
             build`) before `cargo build --release`."
        );
    }
}
