# Packaging & Deployment — Web UI Acceptance Test Cases

Scope: how the web UI is built, packaged, and run by an operator or developer — the `scripts/build-web-ui.sh` build wrapper (Vite then `cargo build --release`), the self-contained `rust-embed` release binary, serving-mode selection (`--web-dir` disk vs. embedded), the empty-bundle fallback probe page, loopback-only binding on `--port`, the `/healthz` probe, the `--model` / `--provider` / `--cwd` flags, the two-terminal dev workflow with the Vite proxy, the `build.rs` release-bundle warning, server-side-only provider keys, and brand neutrality of the shipped artifacts.

### WUI-PKG-01: build-web-ui.sh builds the frontend then the release binary and prints the binary path
- **Persona:** Operator building the shippable artifact from a clean checkout
- **Preconditions:** Workspace checked out; Node/npm and the Rust toolchain installed; run from any directory (the script cd's to the repo root itself)
- **Steps:**
  1. `scripts/build-web-ui.sh`
  2. Observe stdout and the exit code (`echo $?`).
- **Assertions:**
  - A1: The script prints `==> Installing frontend dependencies`, `==> Building frontend bundle (Vite)`, and `==> Building release binary (cargo, embeds web/dist)` in that order, proving the Vite build runs before `cargo build --release`.
  - A2: On success the script exits `0`.
  - A3: The final two stdout lines are `==> Done. Self-contained binary:` followed by the absolute path `<repo_root>/target/release/hand-web-ui`.
  - A4: The file at the printed path exists and is executable.
  - A5: `crates/web-ui/web/dist/index.html` exists after the run (the Vite bundle that was embedded).
- **Traces:** rows 196, 197; M12; §6.2

### WUI-PKG-02: build-web-ui.sh aborts the whole pipeline if the Vite build fails
- **Persona:** Operator on a checkout with a broken frontend
- **Preconditions:** A type error or build error is present such that `npm --prefix crates/web-ui/web run build` exits non-zero
- **Steps:**
  1. `scripts/build-web-ui.sh`
  2. Observe stdout and exit code.
- **Assertions:**
  - A1: The script stops at the failing Vite step; the line `==> Building release binary (cargo, embeds web/dist)` is NOT printed (`set -e` halts before `cargo build`).
  - A2: The script exits non-zero.
  - A3: No new `target/release/hand-web-ui` is produced by this run (the cargo step never executed).
- **Traces:** rows 196, 197; M12; §6.2

### WUI-PKG-03: The release binary serves the real embedded bundle from a directory with no web/dist
- **Persona:** Operator running the shipped binary on a fresh host
- **Preconditions:** A release binary built via `scripts/build-web-ui.sh`; copy ONLY the binary to an empty directory (e.g. `/tmp/deploy`) that contains no `web/dist` and no frontend files
- **Steps:**
  1. From `/tmp/deploy`, run `./hand-web-ui --port 4137` (no `--web-dir`).
  2. `curl -s http://127.0.0.1:4137/` and capture the body.
- **Assertions:**
  - A1: The HTTP response for `/` is `200`.
  - A2: The served HTML references at least one hashed asset under `/assets/` (e.g. `/assets/index-*.js`) and a CSS asset, i.e. it is the real Vite bundle.
  - A3: The HTML does NOT contain the connectivity-probe text `This is the built-in connectivity probe.` nor `Frontend bundle not embedded`.
  - A4: The binary runs with no external file dependency — serving works despite the empty working directory.
- **Traces:** row 196; M12; §6.2

### WUI-PKG-04: Embedded assets are reachable and return the correct content type
- **Persona:** Operator verifying static asset delivery from the embedded bundle
- **Preconditions:** Release binary running from an empty directory (no `--web-dir`), as in WUI-PKG-03
- **Steps:**
  1. `curl -s http://127.0.0.1:4137/` and extract the first `/assets/index-*.js` path referenced in the HTML.
  2. `curl -s -o /dev/null -w '%{http_code} %{content_type}\n' http://127.0.0.1:4137/<that-asset>`
  3. Do the same for the referenced `.css` asset.
- **Assertions:**
  - A1: The JS asset request returns `200`.
  - A2: Its `Content-Type` is a JavaScript MIME type (e.g. `text/javascript` / `application/javascript`), from `mime_guess`.
  - A3: The CSS asset request returns `200` with a `text/css` content type.
- **Traces:** row 196; M12; §6.2

### WUI-PKG-05: A genuine missing static asset returns 404, an extensionless SPA route returns 200 index
- **Persona:** Developer probing routing semantics of the embedded fallback
- **Preconditions:** Release binary running from an empty directory (no `--web-dir`)
- **Steps:**
  1. `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4137/assets/does-not-exist.js`
  2. `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4137/some/spa/route`
  3. `curl -s http://127.0.0.1:4137/some/spa/route` and inspect the body.
- **Assertions:**
  - A1: A missing path that contains a file extension (`does-not-exist.js`) returns `404`.
  - A2: An extensionless unknown path (`/some/spa/route`) returns `200`.
  - A3: The body for the extensionless route is the `index.html` shell (the SPA owns client-side routing), not a 404 page.
- **Traces:** row 196; M12; §6.2

### WUI-PKG-06: With --web-dir pointing at an existing directory, assets are served from disk
- **Persona:** Developer iterating on a pre-built frontend without recompiling the server
- **Preconditions:** A built `crates/web-ui/web/dist` exists on disk; the binary (debug or release) is available
- **Steps:**
  1. `cargo run -p hand-web-ui -- --web-dir crates/web-ui/web/dist --port 4137` (or run the binary with the same flag).
  2. `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4137/`
  3. Modify a file in `crates/web-ui/web/dist` (e.g. append a marker to `index.html`), then `curl -s http://127.0.0.1:4137/` again.
- **Assertions:**
  - A1: `/` returns `200` and serves the on-disk `index.html`.
  - A2: After editing the on-disk `index.html`, the new content is served without rebuilding or restarting the server (disk/`ServeDir` mode reads from disk).
  - A3: Static assets resolve under the served directory (e.g. `/assets/*` returns `200`).
- **Traces:** rows 196, 198; M12; §6.2

### WUI-PKG-07: A non-existent --web-dir falls back to the embedded bundle, not 404s
- **Persona:** Operator who passed a wrong path by mistake
- **Preconditions:** A release binary with an embedded bundle; a path that does not exist on disk (e.g. `/no/such/dist`)
- **Steps:**
  1. `./hand-web-ui --web-dir /no/such/dist --port 4137`
  2. `curl -s http://127.0.0.1:4137/` and `curl` a referenced `/assets/*` path.
- **Assertions:**
  - A1: The server starts successfully (the bad `--web-dir` is filtered because it is not a directory).
  - A2: `/` returns `200` and serves the embedded real bundle (references `/assets/*`), not a 404.
  - A3: A referenced `/assets/*.js` asset returns `200`.
- **Traces:** row 196; M12; §6.2

### WUI-PKG-08: An empty/missing embedded bundle falls back to the inline connectivity probe page
- **Persona:** Developer running a debug binary built without ever building the frontend
- **Preconditions:** A binary whose embedded bundle is empty (e.g. a debug build where `web/dist` has no `index.html`); no `--web-dir` passed
- **Steps:**
  1. Run the binary with no `--web-dir`.
  2. `curl -s http://127.0.0.1:4137/` and inspect the body.
- **Assertions:**
  - A1: `/` returns `200`.
  - A2: The body is the minimal inline page containing `Frontend bundle not embedded` and a reference to `scripts/build-web-ui.sh`.
  - A3: The body does NOT reference any `/assets/*` hashed bundle (there is no real bundle to reference).
- **Traces:** row 196; M12; §6.2

### WUI-PKG-09: With --web-dir set but its index.html missing, / falls back to the dev probe page
- **Persona:** Developer who pointed at a directory that exists but lacks a built index
- **Preconditions:** A directory exists on disk but contains no `index.html` (e.g. an empty `dist`); pass it via `--web-dir`
- **Steps:**
  1. `mkdir -p /tmp/empty-dist`
  2. `./hand-web-ui --web-dir /tmp/empty-dist --port 4137`
  3. `curl -s http://127.0.0.1:4137/` and inspect the body.
- **Assertions:**
  - A1: `/` returns `200`.
  - A2: The body is the inline connectivity-probe page containing `This is the built-in connectivity probe.` and a `/ws`-connecting `<script>`.
  - A3: The probe page lets a user send a single prompt over `/ws` (it contains the prompt form and WebSocket bootstrap), demonstrating the streaming seam even with no real frontend.
- **Traces:** row 196; M12; §6.2

### WUI-PKG-10: The server binds loopback only on the chosen --port
- **Persona:** Operator concerned about network exposure
- **Preconditions:** Host with a routable non-loopback interface; release binary available
- **Steps:**
  1. `./hand-web-ui --port 4137`
  2. `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4137/healthz`
  3. From another machine (or via the host's LAN IP), attempt `curl --max-time 3 http://<host-lan-ip>:4137/healthz`.
- **Assertions:**
  - A1: The server logs/prints `http://127.0.0.1:4137` (the bind address is `127.0.0.1`, not `0.0.0.0`).
  - A2: Requests to `127.0.0.1:4137` succeed (`/healthz` returns `200`).
  - A3: A request to the host's non-loopback IP on the same port fails to connect / times out (the socket is bound to loopback only).
- **Traces:** row 4; M0/M12; §6.2

### WUI-PKG-11: --port selects the listening port and the printed URL matches
- **Persona:** Operator running multiple instances or avoiding a busy port
- **Preconditions:** Release binary available; chosen port (e.g. 5599) is free
- **Steps:**
  1. `./hand-web-ui --port 5599`
  2. Read the stdout line.
  3. `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:5599/healthz`
  4. `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4137/healthz` (the default port).
- **Assertions:**
  - A1: stdout contains `hand web ui: open http://127.0.0.1:5599`.
  - A2: `/healthz` on `5599` returns `200`.
  - A3: The default port `4137` is not listening when `--port 5599` was given (connection refused).
- **Traces:** row 4; M0/M12; §6.2

### WUI-PKG-12: --port on an already-bound port exits with an error
- **Persona:** Operator who started a second instance on the same port by accident
- **Preconditions:** One instance already listening on port 4137
- **Steps:**
  1. With the first instance running, start a second: `./hand-web-ui --port 4137`
  2. Observe stderr and the exit code.
- **Assertions:**
  - A1: The second process fails to bind (address already in use).
  - A2: The second process exits non-zero (the `TcpListener::bind` error propagates from `main`).
  - A3: The first instance continues serving (`/healthz` on 4137 still returns `200`).
- **Traces:** row 4; M0/M12; §6.2

### WUI-PKG-13: /healthz returns ok and is independent of the asset bundle
- **Persona:** Operator wiring a liveness probe / load balancer health check
- **Preconditions:** Server running in any serving mode (embedded, disk, or empty-bundle fallback)
- **Steps:**
  1. `curl -s -w '\n%{http_code}\n' http://127.0.0.1:4137/healthz`
- **Assertions:**
  - A1: The response status is `200`.
  - A2: The response body is exactly `ok`.
  - A3: `/healthz` returns `200 ok` even when the bundle is empty / the probe page is being served at `/` (the health route does not depend on assets).
- **Traces:** row 4; M0/M12; §6.2

### WUI-PKG-14: --model overrides the default model for new sessions
- **Persona:** Operator pinning a specific model at launch
- **Preconditions:** Release binary; a valid provider configured in the server environment for the chosen model
- **Steps:**
  1. Start with the default: `./hand-web-ui` and note the default model is `deepseek/deepseek-v4-flash`.
  2. Restart with an override: `./hand-web-ui --model openrouter/some-model`.
  3. Open `/ws`, start a session, and observe which model the session reports/uses.
- **Assertions:**
  - A1: With no `--model`, new sessions use the documented default `deepseek/deepseek-v4-flash`.
  - A2: With `--model openrouter/some-model`, new sessions are created with that model id (the value flows into `AppState.model`).
  - A3: The chosen model is visible in the session's reported model id, not the default.
- **Traces:** row 195; M8/M12; §6.2

### WUI-PKG-15: --provider overrides the provider for the configured model
- **Persona:** Operator routing the same model through a specific provider
- **Preconditions:** Release binary; the named provider is configured in the server environment
- **Steps:**
  1. `./hand-web-ui --model some/model --provider my-provider`
  2. Start a session over `/ws` and trigger one assistant turn.
- **Assertions:**
  - A1: The server accepts `--provider` and starts normally (the value flows into `AppState.provider`).
  - A2: When omitted, `provider` is `None` and the default provider resolution applies.
  - A3: When set, the session uses the named provider override for model calls.
- **Traces:** row 195; M8/M12; §6.2

### WUI-PKG-16: --cwd sets the agent working directory; default is the launch directory
- **Persona:** Operator scoping agent file/tool execution to a project directory
- **Preconditions:** Release binary; a target project directory exists (e.g. `/tmp/project`)
- **Steps:**
  1. `./hand-web-ui --cwd /tmp/project`
  2. Over `/ws`, ask the agent to run a tool that reports its working directory (e.g. list files / print cwd).
  3. Separately, start the binary from inside a different directory with no `--cwd` and repeat.
- **Assertions:**
  - A1: With `--cwd /tmp/project`, tool execution resolves relative paths against `/tmp/project`.
  - A2: With no `--cwd`, the agent working directory defaults to the directory the server was launched from (`std::env::current_dir()`).
  - A3: The server starts successfully in both cases.
- **Traces:** row 4; M0/M12; §6.2

### WUI-PKG-17: Two-terminal dev workflow — Vite dev server proxies /ws, /upload, /download to the Rust server
- **Persona:** Developer doing frontend work with HMR against the live backend
- **Preconditions:** `crates/web-ui/web` deps installed; Rust server runnable
- **Steps:**
  1. Terminal 1: `cargo run -p hand-web-ui -- --web-dir crates/web-ui/web/dist` (server on 4137).
  2. Terminal 2: `npm --prefix crates/web-ui/web run dev` (Vite dev server).
  3. In the browser open the Vite dev URL; confirm the WebSocket connects and a prompt streams a reply.
  4. Trigger an upload and a download through the dev server.
- **Assertions:**
  - A1: `vite.config.ts` proxies `/ws` to `ws://127.0.0.1:4137` with `ws: true`, and `/upload` and `/download` to `http://127.0.0.1:4137`.
  - A2: From the Vite dev origin, the WebSocket to `/ws` connects (proxied to the Rust server) and an assistant reply streams token-by-token.
  - A3: An upload via `/upload` and a download via `/download/:id` succeed through the proxy (handled by the Rust server, not Vite).
  - A4: Editing a frontend source file hot-reloads in the browser without restarting the Rust server.
- **Traces:** row 198; M10/M12; §6.3

### WUI-PKG-18: Single-binary smoke test — npm build then cargo run (no --web-dir) serves like a release build
- **Persona:** Developer validating the embedded path before shipping
- **Preconditions:** Workspace checkout with toolchains installed
- **Steps:**
  1. `npm --prefix crates/web-ui/web run build`
  2. `cargo run -p hand-web-ui` (no `--web-dir`).
  3. `curl -s http://127.0.0.1:4137/` and a referenced `/assets/*` asset.
- **Assertions:**
  - A1: `/` returns `200` and serves the real embedded bundle (references `/assets/*`), exactly as a release binary would.
  - A2: A referenced `/assets/*.js` asset returns `200`.
  - A3: The probe page text `This is the built-in connectivity probe.` is NOT present.
- **Traces:** rows 196, 198; M12; §6.3

### WUI-PKG-19: build.rs warns (does not fail) when web/dist is missing for a release build
- **Persona:** Operator who ran `cargo build --release` without first building the frontend
- **Preconditions:** No `crates/web-ui/web/dist/index.html` present
- **Steps:**
  1. `rm -rf crates/web-ui/web/dist`
  2. `cargo build -p hand-web-ui --release` and capture stderr.
- **Assertions:**
  - A1: The build emits a `cargo:warning=` containing `web/dist/index.html not found` and pointing at `scripts/build-web-ui.sh`.
  - A2: The release build still completes successfully (exit `0`) — the missing bundle is a warning, not a hard error.
  - A3: The resulting binary embeds an empty bundle, so running it serves the empty-bundle fallback page (`Frontend bundle not embedded`), consistent with WUI-PKG-08.
- **Traces:** rows 196, 197; M12; §6.2

### WUI-PKG-20: A debug build with no web/dist succeeds without the release warning
- **Persona:** CI / developer running `cargo check` and tests without a frontend build
- **Preconditions:** No `crates/web-ui/web/dist` present
- **Steps:**
  1. `rm -rf crates/web-ui/web/dist`
  2. `cargo check -p hand-web-ui` and `cargo test -p hand-web-ui`, capturing stderr.
- **Assertions:**
  - A1: The debug check/test completes successfully (exit `0`).
  - A2: No `web/dist/index.html not found` warning is emitted (the `build.rs` check only fires when `PROFILE == release`).
  - A3: Backend tests run without requiring any frontend build (debug `rust-embed` reads from disk at runtime).
- **Traces:** rows 196, 197; M12; §6.2

### WUI-PKG-21: Provider API keys are read from the server environment and never embedded in the shipped bundle
- **Persona:** Security-conscious operator auditing the artifact
- **Preconditions:** A release binary built with real provider keys present in the build environment; the built `crates/web-ui/web/dist`
- **Steps:**
  1. Inspect the shipped frontend bundle: `grep -rIl "sk-\|api[_-]\?key\|secret" crates/web-ui/web/dist` (or scan for any concrete key value used in the environment).
  2. Inspect the binary's embedded assets for a known key value.
  3. Start the server with the provider key set ONLY in its process environment and run one assistant turn.
- **Assertions:**
  - A1: No real provider key value appears anywhere under `crates/web-ui/web/dist` (the frontend never contains keys).
  - A2: The known key value does not appear in the embedded asset bytes of the binary.
  - A3: The server resolves the key from its own process environment and an assistant turn succeeds; the browser never receives the key.
- **Traces:** row 195; M0/M8/M12; §6.2

### WUI-PKG-22: Brand-neutrality grep over the shipped frontend and server source returns zero matches
- **Persona:** Maintainer enforcing de-branding before release
- **Preconditions:** Workspace checkout; release artifacts built
- **Steps:**
  1. Run a case-insensitive recursive grep for the project's forbidden brand substrings (the reference project's package prefixes, the author handle, the reference-project name, and the issue marker — as defined by the team branding policy) over: `crates/web-ui/src`, `crates/web-ui/web/src`, `crates/web-ui/web/dist`, `scripts/build-web-ui.sh`, `crates/web-ui/build.rs`, and `crates/web-ui/README.md`.
  2. Capture the output and exit code.
- **Assertions:**
  - A1: The grep prints no matches across server source (`crates/web-ui/src`), frontend source (`crates/web-ui/web/src`), and the shipped bundle (`crates/web-ui/web/dist`).
  - A2: The build wrapper, `build.rs`, and `README.md` contain none of the forbidden substrings.
  - A3: The grep exits non-zero (no matches found), i.e. the brand-neutrality gate passes.
- **Traces:** row 199; M11/M12; §6.2

### WUI-PKG-23: The release binary runs offline of the Vite dev server and the npm toolchain
- **Persona:** Operator deploying to a host without Node installed
- **Preconditions:** A release binary built elsewhere via `scripts/build-web-ui.sh`; target host has no Node/npm and no Vite dev server running
- **Steps:**
  1. Copy only `hand-web-ui` to the host.
  2. Run `./hand-web-ui --port 4137` (no `--web-dir`).
  3. Open `http://127.0.0.1:4137/` in a browser and send a prompt.
- **Assertions:**
  - A1: The server starts and serves the full app from the embedded bundle with no Node/npm present.
  - A2: `/` returns `200` with the real bundle (references `/assets/*`); the WebSocket at `/ws` connects and an assistant reply streams.
  - A3: No connection to any Vite dev server is required (the app is self-contained).
- **Traces:** rows 196, 198; M12; §6.2/§6.3
