#!/bin/bash
# deskd-autoupdate: check for new GitHub release and upgrade if needed.
# Designed to run via systemd timer (e.g. every 5 minutes).
#
# Usage: deskd-autoupdate.sh [--install-dir /usr/local/bin]

set -euo pipefail

REPO="kgatilin/deskd"
INSTALL_DIR="${1:-/usr/local/bin}"
STATE_FILE="/var/lib/deskd/current-version"
BINARY="$INSTALL_DIR/deskd"

mkdir -p "$(dirname "$STATE_FILE")"

# Get latest release tag from GitHub API (no auth needed for public repos).
LATEST=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)

if [ -z "$LATEST" ]; then
    echo "Failed to fetch latest release"
    exit 1
fi

# Read current deployed version.
CURRENT=""
if [ -f "$STATE_FILE" ]; then
    CURRENT=$(cat "$STATE_FILE")
fi

if [ "$LATEST" = "$CURRENT" ]; then
    exit 0
fi

echo "New release: $LATEST (current: ${CURRENT:-none})"

# Determine artifact name.
OS="linux"
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  ARCH="amd64" ;;
    aarch64) ARCH="arm64" ;;
    *)       echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

ARTIFACT="deskd-${OS}-${ARCH}"
URL="https://github.com/$REPO/releases/download/$LATEST/$ARTIFACT"

echo "Downloading $URL ..."
TMP=$(mktemp "$INSTALL_DIR/.deskd-upgrade.XXXXXX")
trap 'rm -f "$TMP"' EXIT

if ! curl -fsSL "$URL" -o "$TMP"; then
    echo "Download failed"
    exit 1
fi

chmod 755 "$TMP"
mv "$TMP" "$BINARY"
trap - EXIT

echo "$LATEST" > "$STATE_FILE"
echo "Installed $LATEST, restarting deskd ..."

systemctl restart deskd
echo "Done."
