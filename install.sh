#!/bin/bash

set -e

# Get the latest version from GitHub
LATEST_VERSION=$(curl -s "https://api.github.com/repos/deathbyknowledge/sos/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')

if [ -z "$LATEST_VERSION" ]; then
    echo "Could not fetch the latest version."
    exit 1
fi

# Detect the OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    linux)
        case "$ARCH" in
            x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
            aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
            *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
        esac
        ;;
    darwin)
        case "$ARCH" in
            x86_64) TARGET="x86_64-apple-darwin" ;;
            arm64) TARGET="aarch64-apple-darwin" ;;
            *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
        esac
        ;;
    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac

# Construct the download URL
DOWNLOAD_URL="https://github.com/deathbyknowledge/sos/releases/download/${LATEST_VERSION}/sos-${TARGET}.tar.gz"

# Download and extract the binary
echo "Downloading sos for ${TARGET}..."
curl -Lsf "${DOWNLOAD_URL}" | tar -xz -C "/usr/local/bin"

echo "sos has been installed successfully."
