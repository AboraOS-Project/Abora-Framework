#!/usr/bin/env bash
# End-to-end check of aborad's TLS remote listener with the real binaries:
# `abora tls generate` makes the certificate, a token is generated, aborad is
# started with [remote] enabled, and curl and the CLI talk to it over TLS.
# Run with `make test-remote` (builds first). Needs curl.
set -u
cargo build --workspace --quiet
FAIL=0
BIN=${ABORA_BIN:-$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/debug}
D=$(mktemp -d)
cd "$D"
"$BIN/abora" tls generate --hostname localhost --out-dir "$D" >/dev/null
SECRET=e2e-remote-secret-0123456789abcdef
"$BIN/abora" auth generate-token --name e2e --secret "$SECRET" --permission read:health --permission read:system --permission read:version   | sed -n '/\[\[tokens\]\]/,/secret_hash/p' | sed 's/^  //' > auth.toml
chmod 600 auth.toml key.pem
cat > abora.toml <<EOF
[remote]
enabled = true
listen_addr = "127.0.0.1:17360"
tls_listen_addr = "127.0.0.1:17361"
tls_cert_file = "$D/cert.pem"
tls_key_file = "$D/key.pem"
[security]
require_authentication = true
token_file = "$D/auth.toml"
[updates]
state_file = "$D/updates.json"
[logging]
format = "text"
EOF
"$BIN/aborad" --config abora.toml > daemon.log 2>&1 &
PID=$!
sleep 2
expect() { # expect <what> <wanted> <got>
  if [ "$2" = "$3" ]; then echo "ok:   $1 ($3)"; else echo "FAIL: $1: wanted $2, got $3"; FAIL=1; fi
}
URL=https://localhost:17361/api/v1/health
expect "TLS without a token is refused" 401 "$(curl -s -o /dev/null -w '%{http_code}' --cacert cert.pem $URL)"
expect "TLS with a token is admitted" 200 "$(curl -s -o /dev/null -w '%{http_code}' --cacert cert.pem -H "Authorization: Bearer $SECRET" $URL)"
expect "plain HTTP on the TLS port gets no API answer" 000 "$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $SECRET" http://localhost:17361/api/v1/health)"
expect "an untrusted certificate is refused by the client" 000 "$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $SECRET" $URL)"
if ABORA_CA_FILE=$D/cert.pem "$BIN/abora" status --url https://localhost:17361 --token "$SECRET" | grep -q "status: *ok"; then
  echo "ok:   abora status over TLS with ABORA_CA_FILE"
else
  echo "FAIL: abora status over TLS"; FAIL=1
fi
kill $PID; wait $PID 2>/dev/null
[ $FAIL = 0 ] || { echo "--- daemon log"; cat daemon.log; }
rm -rf "$D"
exit $FAIL
