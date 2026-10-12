#!/usr/bin/env bash
# Download and install a state-free Screentra Cast server bundle.
# HTTPS requires RIPLEY_CA_CERT (or SCREENTRA_CAST_CA_CERT) unless the source URL
# is explicitly plain HTTP on a trusted local network.
set -euo pipefail

SERVER_URL="${RIPLEY_SERVER_URL:-}"
BUNDLE_URL="${RIPLEY_SERVER_BUNDLE_URL:-}"
if [ -z "$SERVER_URL" ] && [ -n "${SERVER_HOST:-}" ]; then
    SERVER_URL="https://${SERVER_HOST}:3000"
fi
if [ -z "$SERVER_URL" ]; then
    printf '%s\n' 'ERROR: set RIPLEY_SERVER_URL, for example https://screentra-cast.home.arpa:3000' >&2
    exit 1
fi
BUNDLE_URL="${BUNDLE_URL:-$SERVER_URL/install/ripley-signage-server-bundle.tar.gz}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/screentra-cast-server-bundle.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

CA_FILE="${RIPLEY_CA_CERT:-${SCREENTRA_CAST_CA_CERT:-}}"
python3 - "$BUNDLE_URL" "$WORK_DIR/server-bundle.tar.gz" "$CA_FILE" <<'PY'
import os
import ssl
import sys
import urllib.request

url, destination, ca_file = sys.argv[1:]
if url.startswith("https://"):
    if not ca_file or not os.path.isfile(ca_file):
        raise SystemExit("ERROR: HTTPS requires RIPLEY_CA_CERT or SCREENTRA_CAST_CA_CERT pointing to the server CA")
    context = ssl.create_default_context(cafile=ca_file)
else:
    context = None
with urllib.request.urlopen(url, context=context, timeout=120) as response:
    data = response.read()
with open(destination, "wb") as stream:
    stream.write(data)
print(f"Downloaded {len(data)} bytes")
PY

tar -xzf "$WORK_DIR/server-bundle.tar.gz" -C "$WORK_DIR"
INSTALLER="$WORK_DIR/ripley-signage-server-bundle/install-server.sh"
[ -x "$INSTALLER" ] || chmod +x "$INSTALLER"
exec "$INSTALLER" "$@"
