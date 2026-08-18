#!/usr/bin/env bash
#
# Creates the self-signed code signing certificate that release builds use.
# Run it once on the release machine. See TODO T-138 and B-002.
#
#   ./scripts/make-signing-cert.sh             create, or say it exists
#   ./scripts/make-signing-cert.sh --rotate    replace an existing identity
#
# Why it exists: macOS ties the microphone grant to the code signing identity.
# An ad-hoc signature is a hash of the binary, so every release used to be a
# new identity and every update re-asked for the microphone. One stable
# certificate ends that: updates keep the grant.
#
# The certificate and key are also saved as a .p12 next to the database, so
# they can be uploaded once to the repository's secret store for CI releases
# (T-139). Rotating creates a new identity, and the release after a rotation
# re-asks for the microphone once.
#
# macOS may show one or two password dialogs while this runs. That is the
# keychain asking for permission, once, and it is expected.

set -euo pipefail

NAME="${SWIVEL_SIGN_IDENTITY:-swivel release}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
OUT_DIR="$HOME/Library/Application Support/dev.motor.swivel"
P12="$OUT_DIR/release-cert.p12"
P12_PASS_FILE="$OUT_DIR/release-cert.p12.pass"

if security find-identity -v -p codesigning 2>/dev/null | grep -Fq "$NAME"; then
  if [[ "${1:-}" == "--rotate" ]]; then
    echo "==> removing the existing \"$NAME\" identity"
    security delete-identity -c "$NAME" "$KEYCHAIN"
  else
    echo "the identity \"$NAME\" already exists. Nothing to do."
    echo "Use --rotate to replace it. The next release then re-asks for the"
    echo "microphone once, because the identity changes."
    exit 0
  fi
fi

mkdir -p "$OUT_DIR"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
basicConstraints = critical,CA:FALSE
EOF

echo "==> creating the certificate"
openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/cert.cnf" 2>/dev/null

echo "==> saving the .p12 for the CI secret store"
P12_PASS=$(openssl rand -hex 16)
# OpenSSL 3 wraps the p12 in ciphers macOS cannot read. `-legacy` uses the old
# ones. LibreSSL, which macOS ships, has no such flag and needs none.
LEGACY=""
if openssl pkcs12 -help 2>&1 | grep -q -- -legacy; then
  LEGACY="-legacy"
fi
openssl pkcs12 -export $LEGACY -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -name "$NAME" -out "$P12" -passout "pass:$P12_PASS"
printf '%s' "$P12_PASS" > "$P12_PASS_FILE"
chmod 600 "$P12" "$P12_PASS_FILE"

echo "==> importing it into the login keychain"
security import "$P12" -k "$KEYCHAIN" -P "$P12_PASS" -T /usr/bin/codesign

echo "==> trusting it for code signing (a password dialog may appear)"
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$TMP/cert.pem"

echo "==> checking that codesign can see it"
if security find-identity -v -p codesigning | grep -Fq "$NAME"; then
  echo "done. build-release.sh will sign as \"$NAME\" from now on."
  echo "The first signing may show one more dialog. Choose Always Allow."
  echo
  echo "For CI releases, an admin uploads the secrets once:"
  echo "  gh secret set CODESIGN_P12 -R <owner/repo> < <(base64 -i \"$P12\")"
  echo "  gh secret set CODESIGN_P12_PASSWORD -R <owner/repo> < \"$P12_PASS_FILE\""
else
  echo "the identity did not appear. Open Keychain Access and look for \"$NAME\"." >&2
  exit 1
fi
