#!/usr/bin/env bash
# Regenerate the self-signed test certificates for the mTLS mock server.
# Localhost-only, throwaway — do NOT use anywhere real.
#
#   bash generate.sh
#
set -e
cd "$(dirname "$0")"
export MSYS_NO_PATHCONV=1   # Git Bash on Windows: keep the /CN=... subject intact

# CA
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -days 3650 -subj "/CN=Maelstrom Test CA"

# Server cert (SAN 127.0.0.1 / localhost), signed by the CA
openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj "/CN=127.0.0.1"
printf "subjectAltName=IP:127.0.0.1,DNS:localhost" > san.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out server.pem -days 3650 -extfile san.ext

# Client cert (for mTLS), signed by the CA
openssl req -newkey rsa:2048 -nodes -keyout client.key -out client.csr -subj "/CN=maelstrom-client"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out client.pem -days 3650

rm -f ./*.csr ./*.ext ./*.srl
echo "Certificates regenerated: ca.pem, server.pem/key, client.pem/key"
