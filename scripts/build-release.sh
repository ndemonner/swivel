#!/usr/bin/env bash
#
# Builds the binary a friend can run.
#
# The product is one executable, not an application bundle. Two things make that
# work, and both happen here. See ARCHITECTURE.md §9.1.
#
#   1. build.rs links Info.plist into the binary, so macOS finds the microphone
#      usage description and the LSUIElement flag.
#   2. codesign gives the binary a stable identity, so the permission a user
#      grants is remembered.

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET_DIR=${CARGO_TARGET_DIR:-target}
BINARY="$TARGET_DIR/release/walkie"

echo "==> building"
cargo build --release

echo "==> checking the embedded Info.plist"
if ! otool -s __TEXT __info_plist "$BINARY" | grep -q "Contents of"; then
  echo "the Info.plist is not in the binary. The microphone will be refused." >&2
  exit 1
fi

echo "==> signing"
# Ad-hoc, because a friend is not going to install a certificate. The signature
# is what gives TCC something stable to remember the microphone grant against.
#
# The cdhash changes on every build, so each new build asks for the microphone
# once more. B-002 tracks using a stable self-signed certificate instead.
codesign --force --sign - --timestamp=none "$BINARY"
codesign --verify --verbose=1 "$BINARY"

SIZE=$(du -h "$BINARY" | cut -f1)
echo
echo "==> $BINARY ($SIZE)"
echo
echo "Send that one file to a friend. They run:"
echo "    chmod +x walkie"
echo "    xattr -d com.apple.quarantine walkie   # only if it came through a browser"
echo "    ./walkie doctor"
echo
