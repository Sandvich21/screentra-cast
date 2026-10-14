#!/usr/bin/env python3
"""Build the self-contained player-client bundle served by Screentra Cast.

The generated bundle contains the canonical generated installer and no client
identity, cached layouts, credentials, database, uploads, or runtime state.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tarfile
import os
import tempfile
from datetime import datetime, timezone
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_result(output: Path) -> dict[str, object]:
    result = {
        "ok": True,
        "path": str(output),
        "url": "/install/player-client-bundle.tar.gz",
        "size_bytes": output.stat().st_size,
        "sha256": sha256(output),
    }
    output.with_suffix(output.suffix + ".json").write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(result))
    return result


def ensure_generated_installer(repo: Path) -> Path:
    player_root = repo / "player-client"
    generated = player_root / "static/install/player-client.sh"
    template = player_root / "install/player-client.sh"
    builder = player_root / "scripts/build-installer.py"
    if not template.exists():
        # Preserve the installed-tree mode used when building from a staged
        # server directory rather than the source repository.
        generated = repo / "static/install/player-client.sh"
        template = repo / "install/player-client.sh"
        builder = repo / "scripts/build-installer.py"
    if template.exists() and builder.exists():
        # Always regenerate the embedded installer. A stale generated file can
        # otherwise be selected over newer installer source changes.
        subprocess.run(["python3", str(builder)], cwd=repo, check=True)
    if not generated.is_file():
        raise SystemExit(f"generated player installer missing: {generated}")
    return generated


def sign_installer(installer: Path, signature: Path, public_key: Path) -> None:
    """Create the detached client-update signature and publish only its key."""
    signing_key = Path(os.environ.get(
        "SCREENTRA_CAST_CLIENT_UPDATE_SIGNING_KEY",
        "/home/tech/.config/screentra-cast/client-update-signing.key",
    )).expanduser()
    if not signing_key.is_file():
        raise SystemExit(
            f"client-update signing key missing: {signing_key}; "
            "provision it before building a player bundle"
        )
    signature.parent.mkdir(parents=True, exist_ok=True)
    public_key.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", str(signing_key),
         "-in", str(installer), "-out", str(signature)],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if not public_key.exists():
        temporary = public_key.with_suffix(public_key.suffix + ".tmp")
        subprocess.run(
            ["openssl", "pkey", "-in", str(signing_key), "-pubout", "-out", str(temporary)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        temporary.replace(public_key)


def build(repo: Path, install_dir: Path, output: Path) -> dict[str, object]:
    source_repo = repo if repo.is_dir() else None
    if source_repo is not None:
        installer = ensure_generated_installer(source_repo)
        source_label = str(source_repo)
        version_file = source_repo / "VERSION"
        version = version_file.read_text(encoding="utf-8").strip() if version_file.exists() else "0.1.0"
        extra_files = []
        uninstall = source_repo / "install/player-uninstall.sh"
        if uninstall.is_file():
            extra_files.append((uninstall, "payload/install/player-uninstall.sh"))
        no_layout = source_repo / "static/no-layout.html"
        if no_layout.is_file():
            extra_files.append((no_layout, "payload/static/no-layout.html"))
    else:
        installer = install_dir / "static/install/player-client.sh"
        if not installer.is_file():
            raise SystemExit(f"served player installer missing: {installer}")
        source_label = str(install_dir)
        version = "unknown"
        extra_files = []

    signature = installer.with_name("player-client.sh.sig")
    public_key = installer.with_name("screentra-cast-client-update.pub")
    sign_installer(installer, signature, public_key)

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="screentra-cast-player-bundle-") as temp:
        root = Path(temp) / "ripley-player-client-bundle"
        payload_installer = root / "payload/install/player-client.sh"
        payload_installer.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(installer, payload_installer)
        payload_installer.chmod(0o755)
        files = [{
            "path": "payload/install/player-client.sh",
            "size": payload_installer.stat().st_size,
            "sha256": sha256(payload_installer),
        }]
        payload_signature = root / "payload/install/player-client.sh.sig"
        shutil.copy2(signature, payload_signature)
        files.append({
            "path": "payload/install/player-client.sh.sig",
            "size": payload_signature.stat().st_size,
            "sha256": sha256(payload_signature),
        })
        payload_public_key = root / "payload/install/screentra-cast-client-update.pub"
        shutil.copy2(public_key, payload_public_key)
        files.append({
            "path": "payload/install/screentra-cast-client-update.pub",
            "size": payload_public_key.stat().st_size,
            "sha256": sha256(payload_public_key),
        })
        for source, relative in extra_files:
            destination = root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
            files.append({"path": relative, "size": destination.stat().st_size, "sha256": sha256(destination)})

        manifest = {
            "bundle_type": "ripley-player-client-bundle",
            "format_version": 2,
            "built_at": datetime.now(timezone.utc).isoformat(),
            "source": source_label,
            "version": version,
            "install_entrypoint": "payload/install/player-client.sh",
            "state_included": False,
            "files": files,
            "excluded_state": ["client identity", "cached layouts", "credentials", "database", "uploads", "runtime state"],
        }
        (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        (root / "README.txt").write_text(
            "Screentra Cast player-client bundle. Run payload/install/player-client.sh.\n"
            "This bundle contains no client identity, credentials, layouts, uploads, or runtime state.\n",
            encoding="utf-8",
        )
        temporary = output.with_suffix(output.suffix + ".tmp")
        if temporary.exists():
            temporary.unlink()
        with tarfile.open(temporary, "w:gz") as archive:
            archive.add(root, arcname=root.name)
        temporary.replace(output)
    return write_result(output)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path(os.environ.get("SCREENTRA_CAST_PLAYER_SOURCE", "/nonexistent")))
    parser.add_argument("--install-dir", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    install_dir = args.install_dir.expanduser().resolve()
    output = args.out.expanduser().resolve() if args.out else install_dir / "static/install/player-client-bundle.tar.gz"
    repo = args.repo.expanduser().resolve()
    if not repo.is_dir():
        repo = Path("/nonexistent")
    build(repo, install_dir, output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
