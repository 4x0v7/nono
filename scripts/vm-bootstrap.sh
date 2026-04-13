#!/usr/bin/env bash
# Install a pinned nushell, then exec into the real setup script.
set -euo pipefail

NU_VERSION="0.111.0"
NU_SHA256="aa5376efaa5f2da98ebae884b901af6504dc8291acf5f4147ac994e9d03cd1ba"
NU_TARBALL="nu-${NU_VERSION}-x86_64-unknown-linux-gnu.tar.gz"
NU_URL="https://github.com/nushell/nushell/releases/download/${NU_VERSION}/${NU_TARBALL}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v nu &>/dev/null; then
    echo -e "\033[36m==>\033[0m Installing nushell ${NU_VERSION}"
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT

    curl --proto '=https' --tlsv1.3 -sSfL -o "$TMP/nu.tar.gz" "$NU_URL"
    ACTUAL_SHA256=$(sha256sum "$TMP/nu.tar.gz" | cut -d' ' -f1)
    if [ "$ACTUAL_SHA256" != "$NU_SHA256" ]; then
        echo -e "\033[31msha256 mismatch:\033[0m"
        echo "  expected: $NU_SHA256"
        echo "  got:      $ACTUAL_SHA256"
        exit 1
    fi
    tar -xzf "$TMP/nu.tar.gz" -C "$TMP"
    sudo install -m 0755 "$TMP"/nu-*/nu /usr/local/bin/nu
else
    echo -e "\033[36m==>\033[0m nushell already installed: $(nu --version)"
fi

exec nu "$SCRIPT_DIR/vm-setup.nu" "$@" --verbose
