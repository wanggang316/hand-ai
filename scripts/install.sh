#!/bin/sh
# Install hand — AI coding agent CLI.
#
# Usage:
#   curl -sSL https://github.com/wanggang316/hand-ai/releases/latest/download/install.sh | sh
#   curl -sSL https://github.com/wanggang316/hand-ai/releases/latest/download/install.sh | sh -s -- --version v0.1.0
#
# Environment overrides:
#   VERSION       Release tag to install (default: latest). Examples: v0.1.0, v0.2.0-rc1.
#   INSTALL_DIR   Directory to drop the binary in (default: $HOME/.local/bin if writable
#                 and on PATH, else /usr/local/bin via sudo).
#   FORCE         "1" to overwrite an existing binary without prompt (default: overwrite
#                 silently — this matches install.sh conventions of rustup/uv/starship).

set -eu

REPO="wanggang316/hand-ai"
BINARY_NAME="hand"

# ---- Pretty output -----------------------------------------------------------

# Use ANSI colours only when stdout is a TTY. Piped install.sh output (e.g.
# from `curl | sh`) often goes to a TTY anyway because shells forward fds —
# but if the user pipes to a logger, plain text is friendlier.
if [ -t 1 ]; then
  BOLD=$(printf '\033[1m')
  DIM=$(printf '\033[2m')
  RED=$(printf '\033[31m')
  GREEN=$(printf '\033[32m')
  YELLOW=$(printf '\033[33m')
  RESET=$(printf '\033[0m')
else
  BOLD=""
  DIM=""
  RED=""
  GREEN=""
  YELLOW=""
  RESET=""
fi

info()  { printf "%s%s%s\n" "$DIM" "$1" "$RESET" >&2; }
note()  { printf "%s%s%s\n" "$BOLD" "$1" "$RESET" >&2; }
ok()    { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$1" >&2; }
warn()  { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$1" >&2; }
fail()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$1" >&2; exit 1; }

# ---- Argument parsing --------------------------------------------------------
# A single repeated `--version <tag>` form is supported via `sh -s --`. Anything
# else gets the usage hint — keeps the script tiny and the documented surface
# tight. Env vars stay the canonical override path.

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || fail "--version needs a tag argument (e.g. --version v0.1.0)"
      VERSION="$2"
      shift 2
      ;;
    --install-dir)
      [ $# -ge 2 ] || fail "--install-dir needs a path argument"
      INSTALL_DIR="$2"
      shift 2
      ;;
    -h|--help)
      cat <<'EOF'
Install hand — AI coding agent.

Usage:
  install.sh [--version <tag>] [--install-dir <path>]

Environment:
  VERSION       Release tag (default: latest)
  INSTALL_DIR   Target directory (default: ~/.local/bin or /usr/local/bin)
  FORCE         Set to 1 to overwrite without prompt
EOF
      exit 0
      ;;
    *)
      fail "Unknown argument: $1 (use --help)"
      ;;
  esac
done

# ---- Detect target platform --------------------------------------------------

uname_os="$(uname -s)"
uname_arch="$(uname -m)"

case "$uname_os" in
  Darwin)  os="darwin" ;;
  Linux)   os="linux" ;;
  *)       fail "Unsupported OS: $uname_os (this installer covers macOS and Linux only — see README for source builds)" ;;
esac

case "$uname_arch" in
  arm64|aarch64)
    arch="arm64"
    ;;
  x86_64|amd64)
    arch="x86_64"
    ;;
  *)
    fail "Unsupported architecture: $uname_arch"
    ;;
esac

# Match against the matrix actually shipped in the release workflow.
# When new targets land (e.g. linux-arm64), add them here.
case "${os}-${arch}" in
  darwin-arm64)
    : # supported
    ;;
  linux-x86_64)
    : # supported
    ;;
  darwin-x86_64)
    fail "macOS Intel pre-built binary isn't published. Build from source:
      cargo install --git https://github.com/${REPO} hand-coding-agent --tag <version>"
    ;;
  linux-arm64)
    fail "Linux arm64 pre-built binary isn't published yet. Build from source:
      cargo install --git https://github.com/${REPO} hand-coding-agent --tag <version>"
    ;;
  *)
    fail "No pre-built binary for ${os}-${arch}"
    ;;
esac

# ---- Resolve version ---------------------------------------------------------

if [ -z "${VERSION:-}" ]; then
  info "Resolving latest release..."
  # `releases/latest` redirects to the latest tag. Following the redirect with
  # `-L` and printing the effective URL is the cheapest way to discover the
  # tag without an API token — works against the GitHub Releases page.
  if command -v curl >/dev/null 2>&1; then
    VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
                "https://github.com/${REPO}/releases/latest" 2>/dev/null \
              | sed -n 's@.*/tag/\([^/]*\)$@\1@p')
  elif command -v wget >/dev/null 2>&1; then
    VERSION=$(wget --max-redirect=5 --server-response --spider \
                "https://github.com/${REPO}/releases/latest" 2>&1 \
              | sed -n 's@.*Location: .*/tag/\([^ ]*\).*@\1@p' | tail -n1)
  else
    fail "Neither curl nor wget is available. Please install one of them and rerun."
  fi
  [ -n "$VERSION" ] || fail "Could not resolve the latest release tag. Pass --version explicitly."
fi

case "$VERSION" in
  v*) : ;;
  *)  warn "Version tag '${VERSION}' doesn't start with 'v' — that's unusual; continuing anyway." ;;
esac

# ---- Resolve install dir -----------------------------------------------------

# Honour explicit INSTALL_DIR / --install-dir. Otherwise prefer ~/.local/bin
# when it's already on $PATH (no sudo needed, no PATH-add hint required); fall
# back to /usr/local/bin (the system-wide convention, needs sudo to write).
needs_sudo=0
if [ -z "${INSTALL_DIR:-}" ]; then
  case ":${PATH}:" in
    *":${HOME}/.local/bin:"*)
      INSTALL_DIR="${HOME}/.local/bin"
      ;;
    *)
      INSTALL_DIR="/usr/local/bin"
      ;;
  esac
fi

# Even an explicit INSTALL_DIR may not be writable as the current user.
if [ ! -w "$INSTALL_DIR" ] && [ ! -w "$(dirname "$INSTALL_DIR")" ]; then
  if command -v sudo >/dev/null 2>&1; then
    needs_sudo=1
    info "Installing to ${INSTALL_DIR} will require sudo."
  else
    fail "Install dir ${INSTALL_DIR} is not writable and sudo isn't available. Try --install-dir <writable-path>."
  fi
fi

# ---- Existing-binary check ---------------------------------------------------

dest="${INSTALL_DIR}/${BINARY_NAME}"
if [ -e "$dest" ] && [ -z "${FORCE:-}" ]; then
  warn "Overwriting existing $dest (set FORCE=0 to abort, FORCE=1 to silence)."
fi
if [ -e "$dest" ] && [ "${FORCE:-}" = "0" ]; then
  fail "$dest exists and FORCE=0 — aborting."
fi

# ---- Download + verify -------------------------------------------------------

asset="${BINARY_NAME}-${VERSION}-${os}-${arch}.tar.gz"
asset_url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
checksum_url="${asset_url}.sha256"

staging=$(mktemp -d 2>/dev/null || mktemp -d -t 'hand-install')
# shellcheck disable=SC2064
trap "rm -rf '$staging'" EXIT INT TERM HUP

note "Downloading hand ${VERSION} for ${os}-${arch}..."
info "  $asset_url"

if command -v curl >/dev/null 2>&1; then
  curl -fsSL -o "$staging/$asset" "$asset_url" \
    || fail "Download failed: $asset_url"
  curl -fsSL -o "$staging/$asset.sha256" "$checksum_url" \
    || fail "Checksum download failed: $checksum_url"
else
  wget -q -O "$staging/$asset" "$asset_url" \
    || fail "Download failed: $asset_url"
  wget -q -O "$staging/$asset.sha256" "$checksum_url" \
    || fail "Checksum download failed: $checksum_url"
fi

# ---- Verify checksum ---------------------------------------------------------

# The .sha256 sidecar is `<hex>  <filename>` produced by `shasum -a 256` /
# `sha256sum`. Both formats accept that same input, so we just chdir to the
# staging dir and run whichever tool is available.
note "Verifying checksum..."
(
  cd "$staging"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$asset.sha256" >/dev/null
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$asset.sha256" >/dev/null
  else
    warn "Neither shasum nor sha256sum available — skipping checksum verification."
  fi
) || fail "Checksum mismatch — refusing to install. The download may be corrupted or tampered with."
ok "Checksum OK"

# ---- Extract + install -------------------------------------------------------

note "Extracting..."
tar -C "$staging" -xzf "$staging/$asset"

# The tarball layout is `hand-<tag>/{hand,README.md,...}` (see release.yml).
extracted_bin="$staging/hand-${VERSION}/${BINARY_NAME}"
[ -f "$extracted_bin" ] || fail "Unexpected tarball layout — binary not found at $extracted_bin"

note "Installing to $dest..."
if [ "$needs_sudo" = "1" ]; then
  sudo install -m 0755 "$extracted_bin" "$dest"
else
  # Create the dir if it didn't exist (~/.local/bin on fresh systems).
  mkdir -p "$(dirname "$dest")"
  install -m 0755 "$extracted_bin" "$dest"
fi
ok "Installed $dest"

# ---- Post-install verification + PATH hint -----------------------------------

if "$dest" --version >/dev/null 2>&1; then
  version_line=$("$dest" --version 2>/dev/null || true)
  ok "Verified: $version_line"
else
  warn "$dest installed but '--version' didn't run cleanly. The binary may be incompatible with this system."
fi

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    : # already on PATH
    ;;
  *)
    warn "$INSTALL_DIR is not on \$PATH."
    note "Add it to your shell rc:"
    printf '  %sexport PATH="%s:\$PATH"%s\n' "$DIM" "$INSTALL_DIR" "$RESET" >&2
    ;;
esac

note "Done. Run '${BINARY_NAME}' to start, or '${BINARY_NAME} --help' for options."
