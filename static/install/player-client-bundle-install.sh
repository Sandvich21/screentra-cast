#!/usr/bin/env bash
set -euo pipefail
# If the user didn't override, derive the server URL from where the bundle
# was downloaded (e.g. https://10.1.1.76:3000). The embedded installer gets
# the same value via env so its unit files, the heartbeat, the actor, and
# the installer's persisted `server` file all use the correct URL.
if [ -n "${RIPLEY_SERVER_URL:-}" ]; then
    SERVER_URL="$RIPLEY_SERVER_URL"
elif [ -n "${RIPLEY_PLAYER_BUNDLE_URL:-}" ]; then
    # Strip the trailing /install/...path to get the origin.
    SERVER_URL="$(echo "$RIPLEY_PLAYER_BUNDLE_URL" | sed -E 's|/install/.*$||')"
else
    echo "ERROR: set RIPLEY_SERVER_URL or RIPLEY_PLAYER_BUNDLE_URL before running this bootstrap." >&2
    echo "Example: RIPLEY_SERVER_URL=https://10.1.1.76:3000 $0" >&2
    exit 1
fi
BUNDLE_URL="${RIPLEY_PLAYER_BUNDLE_URL:-$SERVER_URL/install/player-client-bundle.tar.gz}"
WORK_DIR="${TMPDIR:-/tmp}/ripley-player-client-bundle.$$"
CA_FILE="${RIPLEY_CA_CERT:-$HOME/.local/share/ripley-player-client/screentra-cast-signage-ca.crt}"
UPDATE_PUBLIC_KEY="${SCREENTRA_CAST_UPDATE_PUBLIC_KEY_PATH:-/etc/screentra-cast/client-update-signing.pub}"
cleanup(){ rm -rf "$WORK_DIR"; }
trap cleanup EXIT
mkdir -p "$WORK_DIR"
echo "Downloading Ripley player/client bundle: $BUNDLE_URL"
python3 - "$BUNDLE_URL" "$WORK_DIR/player-client-bundle.tar.gz" "$CA_FILE" <<'PY_FETCH'
import os, ssl, sys, urllib.request
url,out,cafile=sys.argv[1:4]
ctx=None
if cafile and os.path.exists(cafile):
    try: ctx=ssl.create_default_context(cafile=cafile)
    except Exception: ctx=None
if ctx is None:
    raise SystemExit(f"TLS verification CA is missing or invalid: {cafile}")
with urllib.request.urlopen(url, context=ctx, timeout=60) as r:
    data=r.read()
open(out,'wb').write(data)
print(f"Downloaded {len(data)} bytes to {out}")
PY_FETCH

tar -xzf "$WORK_DIR/player-client-bundle.tar.gz" -C "$WORK_DIR"
INSTALL_SH="$WORK_DIR/ripley-player-client-bundle/payload/install/player-client.sh"
INSTALL_SIG="$WORK_DIR/ripley-player-client-bundle/payload/install/player-client.sh.sig"
if [ ! -x "$INSTALL_SH" ]; then chmod +x "$INSTALL_SH"; fi
if [ ! -f "$UPDATE_PUBLIC_KEY" ]; then
    echo "ERROR: pinned client-update signing key is missing at $UPDATE_PUBLIC_KEY" >&2
    echo "Provision the operator-approved key before running a client bootstrap." >&2
    exit 1
fi
openssl pkeyutl -verify -rawin -pubin -inkey "$UPDATE_PUBLIC_KEY" -sigfile "$INSTALL_SIG" -in "$INSTALL_SH" >/dev/null
export RIPLEY_SERVER_URL="$SERVER_URL"
export RIPLEY_CA_CERT="$CA_FILE"
export SCREENTRA_CAST_UPDATE_PUBLIC_KEY_SOURCE="$WORK_DIR/ripley-player-client-bundle/payload/install/screentra-cast-client-update.pub"
exec "$INSTALL_SH" "$@"
