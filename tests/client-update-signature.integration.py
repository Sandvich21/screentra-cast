import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTALLER = (ROOT / "static/install/player-client.sh").read_text(encoding="utf-8")
BUNDLE_BUILDER = (ROOT / "tools/build-player-client-bundle.py").read_text(encoding="utf-8")

assert "PUBLIC_KEY_FILE" in INSTALLER
assert "player-client.sh.sig" in INSTALLER
assert "pkeyutl" in INSTALLER
assert "-verify" in INSTALLER
assert "player-client.sh.sig" in BUNDLE_BUILDER
assert "NOPASSWD: ALL" not in INSTALLER
assert "gpasswd -d" in INSTALLER

BOOTSTRAP = (ROOT / "static/install/player-client-bundle-install.sh").read_text(encoding="utf-8")
assert "ssl._create_unverified_context" not in BOOTSTRAP
assert "pinned client-update signing key is missing" in BOOTSTRAP
assert "pkeyutl" in BOOTSTRAP
assert "--no-check-certificate" not in (ROOT / "static/js/app.js").read_text(encoding="utf-8")
assert "ssl._create_unverified_context" not in INSTALLER
actor_start = INSTALLER.index("def update_client(")
actor_end = INSTALLER.index("\ndef execute(", actor_start)
compile(INSTALLER[actor_start:actor_end], "player-client-actor.update_client", "exec")
assert "signature" in BUNDLE_BUILDER

signature = ROOT / "static/install/player-client.sh.sig"
assert signature.is_file(), "signed player installer is missing"
assert signature.stat().st_size > 0

manifest = json.loads((ROOT / "static/install/player-client-bundle.tar.gz.json").read_text())
assert manifest["ok"] is True
assert manifest["size_bytes"] > 0
assert len(manifest["sha256"]) == 64

print("client update signature integration: signed installer contract present")
