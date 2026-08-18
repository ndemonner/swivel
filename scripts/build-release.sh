#!/usr/bin/env bash
#
# Builds the binary you send to a friend.
#
# The result is one universal executable in the repository root, signed, with
# nothing for the recipient to install. See ARCHITECTURE.md §9.1 for why a bare
# executable can still ask for the microphone.
#
#   ./scripts/build-release.sh            both architectures, for sending out
#   ./scripts/build-release.sh --fast     this machine only, for testing

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET_DIR=${CARGO_TARGET_DIR:-target}
OUT="swivel"
FAST=${1:-}

ARM="aarch64-apple-darwin"
INTEL="x86_64-apple-darwin"

if [[ "$FAST" == "--fast" ]]; then
  echo "==> building for this machine only"
  cargo build --release
  cp "$TARGET_DIR/release/swivel" "$OUT"
else
  echo "==> building for Apple Silicon"
  rustup target add "$ARM" >/dev/null 2>&1 || true
  cargo build --release --target "$ARM"

  echo "==> building for Intel"
  rustup target add "$INTEL" >/dev/null 2>&1 || true
  cargo build --release --target "$INTEL"

  echo "==> joining them into one binary"
  # A friend should not have to know which Mac they own.
  lipo -create -output "$OUT" \
    "$TARGET_DIR/$ARM/release/swivel" \
    "$TARGET_DIR/$INTEL/release/swivel"
fi

echo "==> checking the embedded Info.plist"
# Without it macOS refuses the microphone, and the error it gives is useless.
if ! otool -s __TEXT __info_plist "$OUT" | grep -q "Contents of"; then
  echo "the Info.plist is not in the binary. The microphone will be refused." >&2
  exit 1
fi

echo "==> signing"
# `lipo` discards signatures, so this has to happen last.
#
# The stable identity from make-signing-cert.sh is preferred. macOS remembers
# the microphone grant against the signing identity, so with the certificate an
# update keeps the grant. Ad-hoc is the fallback: it still works, but its
# identity is the hash of the binary, so each new version asks once more.
IDENTITY="${SWIVEL_SIGN_IDENTITY:-swivel release}"
if security find-identity -v -p codesigning 2>/dev/null | grep -Fq "$IDENTITY"; then
  # The explicit identifier keeps the identity stable even if the file is
  # renamed. It matches CFBundleIdentifier in Info.plist.
  codesign --force --sign "$IDENTITY" --identifier dev.motor.swivel \
    --timestamp=none "$OUT"
else
  echo "    no \"$IDENTITY\" certificate. Signing ad-hoc, so this update will"
  echo "    re-ask for the microphone. Run ./scripts/make-signing-cert.sh once."
  codesign --force --sign - --identifier dev.motor.swivel --timestamp=none "$OUT"
fi
codesign --verify --verbose=1 "$OUT" 2>&1 | sed 's/^/    /'

SIZE=$(du -h "$OUT" | cut -f1)
ARCHES=$(lipo -archs "$OUT")
SUM=$(shasum -a 256 "$OUT" | cut -d' ' -f1)

cat <<SUMMARY

==> ./$OUT  ($SIZE, $ARCHES)
    sha256 $SUM

Sending it to a friend
----------------------

The easy way, which avoids Gatekeeper entirely. Put the file somewhere with a
URL, such as a GitHub release, then send this one line:

    curl -fsSL <url>/swivel -o swivel && chmod +x swivel && ./swivel doctor

curl does not mark the file as quarantined, so macOS does not block it.

If you send the file through a browser, AirDrop, Messages, or Slack, macOS does
mark it, and your friend has to clear the mark before it will run:

    xattr -d com.apple.quarantine swivel
    chmod +x swivel
    ./swivel doctor

Either way 'swivel doctor' is the right first command. It checks the audio
devices and asks for the microphone, so anything wrong shows up before they try
to talk to somebody.

SUMMARY
