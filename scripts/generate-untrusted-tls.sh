#!/bin/sh
# Create a test-only self-signed certificate for untrusted.opmux.test.
# This is not a production CA and is never added to the runtime trust store.
set -eu

OUT_DIR="${1:-}"
if [ -z "$OUT_DIR" ]; then
  echo "usage: generate-untrusted-tls.sh OUT_DIR" >&2
  exit 1
fi
mkdir -p "$OUT_DIR"
chmod 700 "$OUT_DIR"

cat >"$OUT_DIR/ext.cnf" <<'EOF'
[req]
default_bits = 2048
prompt = no
default_md = sha256
distinguished_name = dn
x509_extensions = v3_req

[dn]
CN = untrusted.opmux.test

[v3_req]
subjectAltName = DNS:untrusted.opmux.test
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
EOF

openssl req -x509 -newkey rsa:2048 -sha256 -nodes \
  -keyout "$OUT_DIR/key.pem" -out "$OUT_DIR/cert.pem" -days 1 \
  -config "$OUT_DIR/ext.cnf" -extensions v3_req >/dev/null 2>&1
chmod 600 "$OUT_DIR/key.pem" "$OUT_DIR/cert.pem"
