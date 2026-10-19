#!/bin/bash
# Creates a self-signed code-signing identity in the login keychain so that
# macOS permission grants (keyed on the code signature) survive rebuilds.
# One-time setup; install.sh picks the identity up automatically.
# Approach from RRY/lazy-mouse. The certificate does not need to be trusted.
set -euo pipefail

NAME="${SIGN_IDENTITY:-MXMaster Local Signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -p codesigning | grep -q "\"$NAME\""; then
    echo "Identity '$NAME' already exists."
    exit 0
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
PASS=$(openssl rand -hex 16)

openssl req -x509 -newkey rsa:2048 -nodes -days 7300 \
    -keyout "$TMP/key.pem" -out "$TMP/cert.pem" \
    -subj "/CN=$NAME" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning"

# security(1) cannot read the default PKCS#12 encryption of OpenSSL 3+.
P12_FLAGS=()
if ! openssl version | grep -q LibreSSL; then
    P12_FLAGS=(-legacy -macalg sha1)
fi
openssl pkcs12 -export "${P12_FLAGS[@]}" -out "$TMP/identity.p12" \
    -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -name "$NAME" -passout "pass:$PASS"

security import "$TMP/identity.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign
echo "Created code-signing identity '$NAME' in the login keychain."
