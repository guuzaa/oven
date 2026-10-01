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
# Downloads are verified against the digest GitHub records for the asset in the
# release JSON, so a stale or corrupted mirror copy is refused.
#
# One-liner (latest release):
#   curl -fsSL https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.sh | bash

set -euo pipefail

REPO="guuzaa/oven"
BIN_NAME="oven"
INSTALL_DIR="$HOME/.oven"
BIN_DIR="$INSTALL_DIR/bin"

# Base URL of the distribution mirror, tried before github.com and shaped as
# $MIRROR/latest, $MIRROR/tags/<tag> and $MIRROR/dl/<tag>/oven-<tag>-<target>.tar.gz.
# Point OVEN_MIRROR somewhere else, or set it to an empty string to skip the mirror.
DEFAULT_MIRROR="https://oven.paulden.site"

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

tag_from_release_json() {
  grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' "$1" \
    | head -1 \
    | cut -d '"' -f 4 || true
}

# Prints the digest GitHub recorded for asset $2 in the release JSON $1. Fields
# are split one per line first: the digest of an asset follows its own name and
# nothing else in between.
digest_for_asset() {
  tr '{,}' '\n\n\n' < "$1" | awk -v asset="$2" '
    /"name"/ {
      name = $0
      sub(/.*"name"[[:space:]]*:[[:space:]]*"/, "", name)
      sub(/".*/, "", name)
      wanted = name == asset
    }
    wanted && /"digest"/ {
      digest = $0
      sub(/.*"digest"[[:space:]]*:[[:space:]]*"/, "", digest)
      sub(/".*/, "", digest)
      sub(/^sha256:/, "", digest)
      if (digest != "" && digest !~ /[^0-9a-f]/) {
        print digest
        exit
      }
    }'
}

# --- Detect OS and architecture -------------------------------------------
case "$(uname -s)" in
  Linux) OS="linux" ;;
  Darwin) OS="darwin" ;;
  MINGW* | MSYS* | CYGWIN*)
    echo "error: $(uname -s) has no prebuilt binary; run the PowerShell installer instead:" >&2
    echo "  irm $DEFAULT_MIRROR/install | iex" >&2
    exit 1
    ;;
  *)
    echo "error: unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) ARCH="x86_64" ;;
  aarch64 | arm64) ARCH="aarch64" ;;
  loongarch64) ARCH="loongarch64" ;;
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
  linux-loongarch64)
    # musl only: pinned glibc 2.28 is too old for loongarch64
    TARGETS=("loongarch64-unknown-linux-musl")
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

# Prints the SHA256 of $1. Fails on systems carrying neither coreutils nor perl.
actual_checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "error: sha256sum or shasum is required to verify the download" >&2
    return 1
  fi
}

# GitHub records a sha256 for every uploaded asset, so an install is only as
# trustworthy as its digest. Refusing without one keeps a stale or tampered
# archive from reaching the disk.
verify_download() {
  local asset="$1" file="$2"
  local expected actual

  expected="$(digest_for_asset "$RELEASE_JSON" "$asset")"
  if [ -z "$expected" ]; then
    echo "error: $TAG publishes no digest for $asset; refusing to install it" >&2
    exit 1
  fi

  actual="$(actual_checksum "$file")" || exit 1
  if [ "$actual" != "$expected" ]; then
    echo "error: $asset does not match the digest published with $TAG" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    echo "  The mirror may still serve an earlier build of this tag." >&2
    return 1
  fi
  echo "Verified $asset"
}

# --- Resolve the release tag ------------------------------------------------
PINNED_TAG="${1:-${OVEN_VERSION:-}}"
# `${OVEN_MIRROR-DEFAULT}` rather than `${OVEN_MIRROR:-DEFAULT}`: an explicitly
# empty value keeps the mirror disabled instead of falling back to the default.
MIRROR="${OVEN_MIRROR-$DEFAULT_MIRROR}"
while [ "${MIRROR%/}" != "$MIRROR" ]; do
  MIRROR="${MIRROR%/}"
done

# The release JSON is the source of the tag and of the digest recorded for every
# asset, so it is fetched once and read throughout.
JSON_BASES=()
if [ -n "$MIRROR" ]; then
  JSON_BASES+=("$MIRROR")
fi
JSON_BASES+=("https://api.github.com/repos/$REPO/releases")

# Tags are v-prefixed; accept either form.
case "$PINNED_TAG" in
  "") RELEASE_PATH="latest" ;;
  v*) RELEASE_PATH="tags/$PINNED_TAG" ;;
  *) RELEASE_PATH="tags/v$PINNED_TAG" ;;
esac

TMP_DIR="$(mktemp -d)"
TMP_BIN=""
trap 'rm -rf "$TMP_DIR" "$TMP_BIN"' EXIT

RELEASE_JSON="$TMP_DIR/release.json"
for base in "${JSON_BASES[@]}"; do
  if download "$base/$RELEASE_PATH" "$RELEASE_JSON"; then
    break
  fi
done

TAG="$(tag_from_release_json "$RELEASE_JSON")"
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

# --- Download and extract -------------------------------------------------
DOWNLOAD_BASES=()
if [ -n "$MIRROR" ]; then
  DOWNLOAD_BASES+=("$MIRROR/dl/$TAG")
fi
DOWNLOAD_BASES+=("https://github.com/$REPO/releases/download/$TAG")

TARGET=""
for candidate in "${TARGETS[@]}"; do
  # The GNU/Linux release is built against glibc 2.28. Do not select it on
  # systems with an older glibc, where the musl release is the compatible one.
  if [[ "$candidate" == *-linux-gnu && "$OS" == "linux" ]]; then
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
  for base in "${DOWNLOAD_BASES[@]}"; do
    URL="$base/$ASSET"
    echo "Trying $URL ..."
    if download "$URL" "$TMP_DIR/$ASSET" \
      && verify_download "$ASSET" "$TMP_DIR/$ASSET" \
      && tar -tzf "$TMP_DIR/$ASSET" >/dev/null 2>&1; then
      TARGET="$candidate"
      break 2
    fi
  done
done

if [ -z "$TARGET" ]; then
  echo "error: no compatible prebuilt binary found for $OS-$ARCH" >&2
  exit 1
fi

tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
mkdir -p "$BIN_DIR"
# Stage next to the binary and rename, so an interrupted install cannot leave a
# truncated executable behind.
TMP_BIN="$BIN_DIR/.$BIN_NAME.$$"
install -m 755 "$TMP_DIR/oven-$TARGET/$BIN_NAME" "$TMP_BIN"
mv -f "$TMP_BIN" "$BIN_DIR/$BIN_NAME"

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
