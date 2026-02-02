#!/usr/bin/env bash
# Quality check script for all Rust packages

set -e

PACKAGES=(
  "packages/model"
  # Add more packages here as they are created
)

echo "==> Running quality checks for all packages..."
echo ""

for pkg in "${PACKAGES[@]}"; do
  if [ -f "$pkg/Cargo.toml" ]; then
    echo "==> Checking $pkg"
    (cd "$pkg" && cargo check)
    echo ""
    echo "==> Testing $pkg"
    (cd "$pkg" && cargo test)
    echo ""
  else
    echo "==> Skipping $pkg (no Cargo.toml)"
    echo ""
  fi
done

echo "==> All checks passed!"
