#!/usr/bin/env bash
#
# Creates the self-signed code signing certificate that release builds use.
# Run it once on the release machine. See TODO T-138 and B-002.
#
# Why it exists: macOS ties the microphone grant to the code signing identity.
# An ad-hoc signature is a hash of the binary, so every release used to be a
# new identity and every update re-asked for the microphone. One stable
# certificate ends that: updates keep the grant.
#
# macOS may show one or two password dialogs while this runs. That is the
# keychain asking for permission, once, and it is expected.

set -euo pipefail

NAME="${SWIVEL_SIGN_IDENTITY:-swivel release}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning 2>/dev/null | grep -Fq "$NAME"; then
  echo "the identity \"$NAME\" already exists. Nothing to do."
  exit 0
fi

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

echo "==> importing it into the login keychain"
# OpenSSL 3 wraps the p12 in ciphers macOS cannot read. `-legacy` uses the old
# ones. LibreSSL, which macOS ships, has no such flag and needs none.
LEGACY=""
if openssl pkcs12 -help 2>&1 | grep -q -- -legacy; then
  LEGACY="-legacy"
fi
openssl pkcs12 -export $LEGACY -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -name "$NAME" -out "$TMP/cert.p12" -passout pass:swivel
security import "$TMP/cert.p12" -k "$KEYCHAIN" -P swivel -T /usr/bin/codesign

echo "==> trusting it for code signing (a password dialog may appear)"
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$TMP/cert.pem"

echo "==> checking that codesign can see it"
if security find-identity -v -p codesigning | grep -Fq "$NAME"; then
  echo "done. build-release.sh will sign as \"$NAME\" from now on."
  echo "The first signing may show one more dialog. Choose Always Allow."
else
  echo "the identity did not appear. Open Keychain Access and look for \"$NAME\"." >&2
  exit 1
fi
