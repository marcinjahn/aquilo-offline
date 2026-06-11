#!/usr/bin/env bash
# Generates a self-signed cert used by capture.py to impersonate the AWS endpoint.
# SANs cover the known device endpoints + wildcards so that, if the device only
# checks hostname (not the CA chain), the handshake still succeeds. If it ALSO
# fails with this cert, the device is validating the CA chain (cloud-locked).
set -euo pipefail
cd "$(dirname "$0")"

cat >san.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = *.iot.us-east-1.amazonaws.com
[ext]
subjectAltName = @alt
[alt]
DNS.1 = *.credentials.iot.us-east-1.amazonaws.com
DNS.2 = c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com
DNS.3 = *.iot.us-east-1.amazonaws.com
DNS.4 = *.elb.us-east-1.amazonaws.com
DNS.5 = public-ethos501-prod-va6-d20a6746fffd84dd.elb.us-east-1.amazonaws.com
DNS.6 = *.amazonaws.com
EOF

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout key.pem -out cert.pem -days 825 \
  -config san.cnf

echo "Wrote $(pwd)/cert.pem and key.pem"
