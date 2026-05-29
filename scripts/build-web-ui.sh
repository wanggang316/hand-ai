#!/usr/bin/env bash
# Build the self-contained web UI binary.
#
# Order matters: the Vite build must produce `crates/web-ui/web/dist` before
# `cargo build --release`, because `rust-embed` embeds that directory into the
# release binary. Run from the workspace root (the paths below are relative to
# it); the script is location-independent and cd's there itself.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

web_dir="crates/web-ui/web"

echo "==> Installing frontend dependencies"
npm --prefix "${web_dir}" install

echo "==> Building frontend bundle (Vite)"
npm --prefix "${web_dir}" run build

echo "==> Building release binary (cargo, embeds web/dist)"
cargo build -p hand-web-ui --release

bin_path="${repo_root}/target/release/hand-web-ui"
echo "==> Done. Self-contained binary:"
echo "${bin_path}"
