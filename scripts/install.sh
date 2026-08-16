#!/usr/bin/env bash
#
# Install the `oven` binary from a GitHub release.
#
# Usage:
#   ./install.sh [TAG]        # e.g. ./install.sh v0.1.0 (defaults to latest release)
#
# You can also pin the version with OVEN_VERSION:
#   OVEN_VERSION=v0.1.0 ./install.sh
#
# One-liner (latest release):
#   curl -fsSL https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.sh | bash

set -euo pipefail

REPO="guuzaa/oven"
BIN_NAME="oven"
INSTALL_DIR="${OVEN_INSTALL_DIR:-$HOME/.oven}"
BIN_DIR="$INSTALL_DIR/bin"

# --- Detect OS and architecture -------------------------------------------
case "$(uname -s)" in
  Linux) OS="linux" ;;
  Darwin) OS="darwin" ;;
  *)
    echo "error: unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) ARCH="x86_64" ;;
  aarch64 | arm64) ARCH="aarch64" ;;
  *)
    echo "error: unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

case "$OS-$ARCH" in
  linux-x86_64)  TARGET="x86_64-unknown-linux-musl" ;;
  linux-aarch64) TARGET="aarch64-unknown-linux-musl" ;;
  darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
  darwin-aarch64) TARGET="aarch64-apple-darwin" ;;
  *)
    echo "error: no prebuilt binary for $OS-$ARCH" >&2
    exit 1
    ;;
esac

# --- Resolve the release tag ----------------------------------------------
TAG="${1:-${OVEN_VERSION:-}}"
if [ -z "$TAG" ]; then
  echo "Resolving the latest release tag..."
  TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' \
    | head -1 \
    | cut -d '"' -f 4 || true)"
fi
if [ -z "$TAG" ]; then
  echo "error: could not determine the release tag; pass it explicitly, e.g. ./install.sh v0.1.0" >&2
  exit 1
fi

# Release tags are v-prefixed; accept either form.
case "$TAG" in
  v*) ;;
  *) TAG="v$TAG" ;;
esac

ASSET="oven-$TAG-$TARGET.tar.gz"
URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"

# --- Download and extract -------------------------------------------------
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading $URL ..."
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$URL" -o "$TMP_DIR/$ASSET"
elif command -v wget >/dev/null 2>&1; then
  wget -q "$URL" -O "$TMP_DIR/$ASSET"
else
  echo "error: need either curl or wget to download" >&2
  exit 1
fi

tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
mkdir -p "$BIN_DIR"
install -m 755 "$TMP_DIR/oven-$TARGET/$BIN_NAME" "$BIN_DIR/$BIN_NAME"

# --- Add to PATH ----------------------------------------------------------
RC_FILES=()
[ -f "$HOME/.bashrc" ] && RC_FILES+=("$HOME/.bashrc")
[ -f "$HOME/.zshrc" ] && RC_FILES+=("$HOME/.zshrc")
[ -f "$HOME/.profile" ] && RC_FILES+=("$HOME/.profile")
if [ "${#RC_FILES[@]}" -eq 0 ]; then
  RC_FILES+=("$HOME/.profile")
fi

for rc in "${RC_FILES[@]}"; do
  if ! grep -q "\.oven/bin" "$rc" 2>/dev/null; then
    printf '\n# Add %s to PATH (added by oven installer)\nexport PATH="%s:$PATH"\n' "$BIN_NAME" "$BIN_DIR" >> "$rc"
    echo "Added $BIN_DIR to PATH in $rc"
  fi
done

echo
echo "oven $TAG installed to $BIN_DIR/$BIN_NAME"
echo "Restart your shell or run 'source ~/.bashrc' (or the matching rc file) to use it."
echo "Verify with: oven --help"
