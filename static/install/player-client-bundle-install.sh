#!/usr/bin/env bash
set -euo pipefail
SERVER_URL="${RIPLEY_SERVER_URL:-${RIPLEY_PLAYER_BUNDLE_URL:-}}"
if [ -z "$SERVER_URL" ]; then
  echo "ERROR: set RIPLEY_SERVER_URL or RIPLEY_PLAYER_BUNDLE_URL before running this bootstrap." >&2
  exit 1
fi
if [[ "$SERVER_URL" == */install/* ]]; then SERVER_URL="${SERVER_URL%%/install/*}"; fi
BUNDLE_URL="${RIPLEY_PLAYER_BUNDLE_URL:-$SERVER_URL/install/player-client-bundle.tar.gz}"
WORK_DIR="${TMPDIR:-/tmp}/ripley-player-client-bundle.$$"
CA_FILE="${RIPLEY_CA_CERT:-${SCREENTRA_CA_CERT_SOURCE:-$HOME/.local/share/screentra-client/screentra-cast-signage-ca.crt}}"
UPDATE_PUBLIC_KEY="${SCREENTRA_CAST_UPDATE_PUBLIC_KEY_PATH:-/etc/screentra-cast/client-update-signing.pub}"
MAX_BUNDLE_BYTES=$((256 * 1024 * 1024))
trap 'rm -rf "$WORK_DIR"' EXIT
mkdir -p "$WORK_DIR"
python3 - "$BUNDLE_URL" "$WORK_DIR/player-client-bundle.tar.gz" "$CA_FILE" "$MAX_BUNDLE_BYTES" <<'PY_FETCH'
import os, ssl, sys, urllib.request
url, out, cafile, limit_text = sys.argv[1:]
limit = int(limit_text)
if not os.path.isfile(cafile): raise SystemExit(f"TLS verification CA is missing: {cafile}")
ctx = ssl.create_default_context(cafile=cafile)
with urllib.request.urlopen(url, context=ctx, timeout=60) as r:
    declared = r.headers.get('Content-Length')
    if declared and int(declared) > limit: raise SystemExit('bundle exceeds download limit')
    total = 0
    with open(out, 'wb') as f:
        while True:
            chunk = r.read(min(1024 * 1024, limit - total + 1))
            if not chunk: break
            total += len(chunk)
            if total > limit: raise SystemExit('bundle exceeds download limit')
            f.write(chunk)
PY_FETCH
python3 - "$WORK_DIR/player-client-bundle.tar.gz" "$WORK_DIR" "$MAX_BUNDLE_BYTES" <<'PY_EXTRACT'
import os, posixpath, shutil, stat, sys, tarfile
from pathlib import PurePosixPath
archive, destination, limit_text = sys.argv[1:]
limit = int(limit_text)
with tarfile.open(archive, 'r:*') as tf:
    members = tf.getmembers()
    if len(members) > 4096: raise SystemExit('archive has too many members')
    total = 0; checked = []
    for m in members:
        name = m.name
        if not name or '\x00' in name or '\\' in name or name.startswith('/'):
            raise SystemExit(f'unsafe archive member: {name!r}')
        parts = PurePosixPath(name).parts
        norm = posixpath.normpath(name)
        if any(p in ('', '.', '..') for p in parts) or norm == '..' or norm.startswith('../'):
            raise SystemExit(f'unsafe archive member: {name!r}')
        if not (norm == 'ripley-player-client-bundle' or norm.startswith('ripley-player-client-bundle/')):
            raise SystemExit(f'unexpected archive root: {name!r}')
        if m.issym() or m.islnk() or m.ischr() or m.isblk() or m.isfifo() or not (m.isdir() or m.isfile()):
            raise SystemExit(f'unsupported archive member: {name!r}')
        if m.size > 64 * 1024 * 1024: raise SystemExit(f'archive member too large: {name!r}')
        total += m.size
        if total > limit: raise SystemExit('archive expands beyond size limit')
        target = os.path.realpath(os.path.join(destination, *PurePosixPath(norm).parts))
        if os.path.commonpath((os.path.realpath(destination), target)) != os.path.realpath(destination):
            raise SystemExit(f'archive member escapes extraction directory: {name!r}')
        checked.append((m, target))
    for m, target in checked:
        if m.isdir(): os.makedirs(target, exist_ok=True); continue
        os.makedirs(os.path.dirname(target), exist_ok=True)
        src = tf.extractfile(m)
        if src is None: raise SystemExit(f'cannot read archive member: {m.name!r}')
        with src, open(target, 'xb') as dst: shutil.copyfileobj(src, dst, 1024 * 1024)
        os.chmod(target, stat.S_IMODE(m.mode) & 0o777)
PY_EXTRACT
INSTALL_SH="$WORK_DIR/ripley-player-client-bundle/payload/install/player-client.sh"
INSTALL_SIG="$WORK_DIR/ripley-player-client-bundle/payload/install/player-client.sh.sig"
[ -f "$INSTALL_SH" ] && [ -f "$INSTALL_SIG" ] || { echo 'ERROR: signed installer or signature missing' >&2; exit 1; }
[ -f "$UPDATE_PUBLIC_KEY" ] || { echo "ERROR: pinned client-update signing key is missing at $UPDATE_PUBLIC_KEY" >&2; exit 1; }
openssl pkeyutl -verify -rawin -pubin -inkey "$UPDATE_PUBLIC_KEY" -sigfile "$INSTALL_SIG" -in "$INSTALL_SH" >/dev/null
chmod 0755 "$INSTALL_SH"
export RIPLEY_SERVER_URL="$SERVER_URL" RIPLEY_CA_CERT="$CA_FILE"
exec "$INSTALL_SH" "$@"
