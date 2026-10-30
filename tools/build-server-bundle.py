#!/usr/bin/env python3
"""Build a state-free Screentra Cast server source/install bundle."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import tarfile
import tempfile
from datetime import datetime, timezone
from pathlib import Path

EXCLUDED_DIRS = {".git", "target", "certs", "uploads", "backups", "__pycache__", "download"}
EXCLUDED_SUFFIXES = {".db", ".sqlite", ".sqlite3", ".key", ".csr", ".srl", ".pid", ".log", ".tmp"}
EXCLUDED_NAMES = {
    "signage.db",
    "server.env",
    "screentra-cast-signage-ca.crt",
    "ripley-signage-server-bundle.tar.gz",
    "player-client-bundle.tar.gz",
    "screentra-cast-installers.tar.gz",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def excluded(relative: Path) -> bool:
    if any(part in EXCLUDED_DIRS for part in relative.parts):
        return True
    if relative.name in EXCLUDED_NAMES:
        return True
    return relative.suffix.lower() in EXCLUDED_SUFFIXES or relative.name.endswith(".tar.gz.json")


def copy_tree(source: Path, destination: Path, files: list[dict[str, object]]) -> None:
    if not source.exists():
        return
    for item in source.rglob("*"):
        if not item.is_file():
            continue
        relative = item.relative_to(source.parent)
        if excluded(relative):
            continue
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(item, target)
        files.append({"path": str(relative), "size": target.stat().st_size, "sha256": sha256(target)})


def installer_script() -> str:
    return r'''#!/usr/bin/env bash
set -euo pipefail
DEST="${SCREENTRA_CAST_SERVER_DIR:-/home/board/server}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD="$ROOT/payload"
if [ "$(id -u)" -ne 0 ]; then
    echo "Run as root or with sudo." >&2
    exit 1
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
    command -v apt-get >/dev/null 2>&1 || { echo "FFmpeg is required, but apt-get is unavailable." >&2; exit 1; }
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ffmpeg
fi
[ -d "$PAYLOAD" ] || { echo "Bundle payload missing: $PAYLOAD" >&2; exit 1; }
install -d -m 0755 "$DEST"
if [ -f "$PAYLOAD/screentra-cast-server" ]; then
    install -m 0755 "$PAYLOAD/screentra-cast-server" "$DEST/screentra-cast-server"
fi
if [ -d "$PAYLOAD/static" ]; then
    install -d -m 0755 "$DEST/static"
    cp -a "$PAYLOAD/static/." "$DEST/static/"
fi
printf '%s\n' "Installed server bundle files under $DEST. Existing database, certificates, uploads, backups, and environment files were preserved."
printf '%s\n' "Restart the configured screentra-cast-server.service after reviewing the installed files."
'''


def build(source: Path, install_dir: Path, output: Path) -> dict[str, object]:
    source_root = source if source.is_dir() else install_dir
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="screentra-cast-server-bundle-") as temp:
        root = Path(temp) / "ripley-signage-server-bundle"
        payload = root / "payload"
        files: list[dict[str, object]] = []

        # Prefer a source tree, but support installed deployments that only
        # have static assets and the release binary available.
        for name in ("Cargo.toml", "Cargo.lock", "README.md", "CHANGELOG.md", ".gitignore"):
            item = source_root / name
            if item.is_file() and not excluded(Path(name)):
                target = payload / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(item, target)
                files.append({"path": name, "size": target.stat().st_size, "sha256": sha256(target)})
        for directory in ("src", "scripts", "tools", "static"):
            copy_tree(source_root / directory, payload, files)

        binary_candidates = [
            source_root / "screentra-cast-server",
            source_root / "target/release/screentra-cast-server",
            source_root / "target/release/screentra-cast",
            source_root / "static/download/screentra-cast-server",
            source_root / "static/download/screentra-cast",
        ]
        for binary in binary_candidates:
            if binary.is_file():
                target = payload / "screentra-cast-server"
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(binary, target)
                target.chmod(0o755)
                files.append({"path": "screentra-cast-server", "size": target.stat().st_size, "sha256": sha256(target)})
                break

        installer = root / "install-server.sh"
        installer.write_text(installer_script(), encoding="utf-8")
        installer.chmod(0o755)
        (root / "README.txt").write_text(
            "State-free Screentra Cast server bundle. Run ./install-server.sh as root. "
            "Database, certificates, uploads, backups, and environment files are excluded and preserved.\n",
            encoding="utf-8",
        )
        manifest = {
            "bundle_type": "ripley-signage-server-bundle",
            "format_version": 2,
            "built_at": datetime.now(timezone.utc).isoformat(),
            "source": str(source_root),
            "install_entrypoint": "install-server.sh",
            "state_included": False,
            "files": files,
            "excluded": sorted(EXCLUDED_DIRS | EXCLUDED_NAMES),
        }
        (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        temporary = output.with_suffix(output.suffix + ".tmp")
        if temporary.exists():
            temporary.unlink()
        with tarfile.open(temporary, "w:gz") as archive:
            archive.add(root, arcname=root.name)
        temporary.replace(output)

    result = {
        "ok": True,
        "path": str(output),
        "url": "/install/ripley-signage-server-bundle.tar.gz",
        "size_bytes": output.stat().st_size,
        "sha256": sha256(output),
        "bootstrap_url": "/install/ripley-signage-server-bundle-install.sh",
    }
    output.with_suffix(output.suffix + ".json").write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(result))
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path(os.environ.get("SCREENTRA_CAST_SOURCE_REPO", "/nonexistent")))
    parser.add_argument("--install-dir", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    install_dir = args.install_dir.expanduser().resolve()
    source = args.repo.expanduser().resolve()
    if not source.is_dir():
        source = install_dir
    output = args.out.expanduser().resolve() if args.out else install_dir / "static/install/ripley-signage-server-bundle.tar.gz"
    build(source, install_dir, output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
