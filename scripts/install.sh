#!/usr/bin/env bash
#
# Install the `oven` binary from a GitHub release.
#
# Usage:
#   ./install.sh [TAG]        # e.g. ./install.sh v0.1.0 (defaults to latest release)
#
# When no TAG is given and the requested version is already installed, the
# download is skipped.
#
# You can also pin the version with OVEN_VERSION:
#   OVEN_VERSION=v0.1.0 ./install.sh
#
# One-liner (latest release):
#   curl -fsSL https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.sh | bash

set -euo pipefail

REPO="guuzaa/oven"
BIN_NAME="oven"
INSTALL_DIR="$HOME/.oven"
BIN_DIR="$INSTALL_DIR/bin"

# Reads from /dev/tty so the one-liner (`curl ... | bash`) still works.
prompt_yes_no() {
  local reply=""
  if [ -r /dev/tty ]; then
    printf '%s' "$1" > /dev/tty
    read -r reply < /dev/tty || reply=""
  fi
  case "$reply" in
    y | Y | yes | Yes | YES) return 0 ;;
    *) return 1 ;;
  esac
}

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

TARGETS=()
case "$OS-$ARCH" in
  linux-x86_64)
    TARGETS=("x86_64-unknown-linux-gnu" "x86_64-unknown-linux-musl")
    ;;
  linux-aarch64)
    TARGETS=("aarch64-unknown-linux-gnu" "aarch64-unknown-linux-musl")
    ;;
  darwin-x86_64)
    TARGETS=("x86_64-apple-darwin")
    ;;
  darwin-aarch64)
    TARGETS=("aarch64-apple-darwin")
    ;;
  *)
    echo "error: no prebuilt binary for $OS-$ARCH" >&2
    exit 1
    ;;
esac

download() {
  local url="$1"
  local output="$2"
  local curl_status=""
  local wget_status=""

  if command -v curl >/dev/null 2>&1; then
    if curl -fsSL "$url" -o "$output"; then
      return 0
    else
      curl_status=$?
    fi
  fi

  if command -v wget >/dev/null 2>&1; then
    if wget -q "$url" -O "$output"; then
      return 0
    else
      wget_status=$?
    fi
  fi

  rm -f "$output"
  if [ -n "$curl_status" ] && [ -n "$wget_status" ]; then
    echo "error: failed to download $url (curl exit $curl_status; wget exit $wget_status)" >&2
  elif [ -n "$curl_status" ]; then
    echo "error: failed to download $url (curl exit $curl_status; wget is unavailable)" >&2
  elif [ -n "$wget_status" ]; then
    echo "error: failed to download $url (curl is unavailable; wget exit $wget_status)" >&2
  else
    echo "error: failed to download $url (neither curl nor wget is available)" >&2
  fi
  return 1
}

# --- Resolve the release tag ----------------------------------------------
PINNED_TAG="${1:-${OVEN_VERSION:-}}"
TAG="$PINNED_TAG"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ -z "$TAG" ]; then
  echo "Resolving the latest release tag..."
  RELEASE_JSON="$TMP_DIR/latest.json"
  download "https://api.github.com/repos/$REPO/releases/latest" "$RELEASE_JSON"
  TAG="$(grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' "$RELEASE_JSON" \
    | head -1 \
    | cut -d '"' -f 4 || true)"
fi
if [ -z "$TAG" ]; then
  echo "error: could not determine the release tag; pass it explicitly, e.g. ./install.sh v0.1.0" >&2
  exit 1
fi

# Release tags are v-prefixed; the installed binary reports a bare version.
VERSION="${TAG#v}"

if [ -z "$PINNED_TAG" ]; then
  INSTALLED_BIN=""
  if [ -x "$BIN_DIR/$BIN_NAME" ]; then
    INSTALLED_BIN="$BIN_DIR/$BIN_NAME"
  elif command -v "$BIN_NAME" >/dev/null 2>&1; then
    INSTALLED_BIN="$(command -v "$BIN_NAME")"
  fi

  if [ -n "$INSTALLED_BIN" ]; then
    INSTALLED_VERSION="$("$INSTALLED_BIN" -V 2>/dev/null | awk '{print $2; exit}' || true)"
    if [ -n "$INSTALLED_VERSION" ] && [ "$INSTALLED_VERSION" = "$VERSION" ]; then
      echo "oven $VERSION is already installed at $INSTALLED_BIN"
      if prompt_yes_no "Reinstall anyway? [y/N] "; then
        echo "Reinstalling oven $VERSION ..."
      else
        exit 0
      fi
    elif [ -n "$INSTALLED_VERSION" ]; then
      echo "Found oven $INSTALLED_VERSION at $INSTALLED_BIN, upgrading to $VERSION ..."
    fi
  fi
fi

# Tags are v-prefixed; accept either form.
case "$TAG" in
  v*) ;;
  *) TAG="v$TAG" ;;
esac

# --- Download and extract -------------------------------------------------
TARGET=""
for candidate in "${TARGETS[@]}"; do
  # The GNU/Linux release is built against glibc 2.28. Do not select it on
  # systems with an older glibc, where the musl release is the compatible one.
  if [[ "$candidate" == *-linux-gnu && "$OS" == "Linux" ]]; then
    glibc_version="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $NF}' || true)"
    if [ -z "$glibc_version" ] || ! awk -v version="$glibc_version" '
      BEGIN {
        split(version, parts, ".")
        exit !(parts[1] > 2 || (parts[1] == 2 && parts[2] >= 28))
      }'; then
      continue
    fi
  fi

  ASSET="oven-$TAG-$candidate.tar.gz"
  URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
  echo "Trying $URL ..."
  if download "$URL" "$TMP_DIR/$ASSET"; then
    TARGET="$candidate"
    break
  fi
done

if [ -z "$TARGET" ]; then
  echo "error: no compatible prebuilt binary found for $OS-$ARCH" >&2
  exit 1
fi

tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
mkdir -p "$BIN_DIR"
install -m 755 "$TMP_DIR/oven-$TARGET/$BIN_NAME" "$BIN_DIR/$BIN_NAME"

# --- Add to PATH ----------------------------------------------------------
case "${SHELL:-}" in
  */bash) RC_FILE="$HOME/.bashrc" ;;
  */zsh) RC_FILE="$HOME/.zshrc" ;;
  *) RC_FILE="$HOME/.profile" ;;
esac

if ! grep -Fq "$BIN_DIR" "$RC_FILE" 2>/dev/null; then
  printf '\n# Add %s to PATH (added by oven installer)\nexport PATH="%s:$PATH"\n' "$BIN_NAME" "$BIN_DIR" >> "$RC_FILE"
  echo "Added $BIN_DIR to PATH in $RC_FILE"
fi

echo
echo "oven $TAG installed to $BIN_DIR/$BIN_NAME"
echo "Restart your shell or run 'source ~/.bashrc' (or the matching rc file) to use it."
echo "Verify with: oven --help"
