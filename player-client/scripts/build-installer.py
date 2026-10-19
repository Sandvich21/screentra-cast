#!/usr/bin/env python3
"""Build screentra-player-client.sh installer from source files."""

from pathlib import Path
import sys

# Map: heredoc marker -> source file path (relative to the player-client tree)
PLAYER_ROOT = Path(__file__).resolve().parents[1]
SECTIONS = {
    "PY_PREFLIGHT":           PLAYER_ROOT / "scripts/screentra-player-preflight.py",
    "PY_DISCOVERY":           PLAYER_ROOT / "scripts/screentra-discover.py",
    "PY":                     PLAYER_ROOT / "scripts/screentra-player-heartbeat.py",
    "PY_RENDERER":            PLAYER_ROOT / "scripts/screentra-player-renderer.py",
    "PY_GPU_HEALTH":          PLAYER_ROOT / "scripts/screentra-gpu-health.py",
    "PY_DETECT_FILE":         PLAYER_ROOT / "scripts/screentra-display-detect.py",
    "PY_DISPLAY_GOVERNANCE":  PLAYER_ROOT / "scripts/screentra-display-governance.py",
    "PY_ACTOR":               PLAYER_ROOT / "scripts/screentra-player-actor.py",
    "PY_TRANSCRIPTION_ADMISSION": PLAYER_ROOT / "scripts/screentra-stt-transcription-request-admission.py",
    "PY_ROLLBACK_ADMISSION": PLAYER_ROOT / "scripts/screentra-client-service-rollback-request-admission.py",
    "PY_STATUS_ADMISSION": PLAYER_ROOT / "scripts/screentra-client-service-status-request-admission.py",
    "SH_STATUS":              PLAYER_ROOT / "scripts/screentra-player-status.sh",
    "SH_KIOSK":               PLAYER_ROOT / "install/screentra-kiosk.sh",
    "PY_NOLAYOUT":            PLAYER_ROOT / "static/no-layout.html",
    "PY_CALIB":               PLAYER_ROOT / "static/touch-calibration.html",
    "EOF_UNIT":               PLAYER_ROOT / "static/screentra-cast-kiosk@.service",
}

TEMPLATE = PLAYER_ROOT / "install/player-client.sh"
OUTPUT  = PLAYER_ROOT / "static/install/player-client.sh"


def replace_section(text: str, marker: str, src: Path) -> str:
    """Replace heredoc section for marker with source file content."""
    search = f"<<'{marker}'"
    start = text.index(search) + len(search)  # after the <<'MARKER'
    end_marker = f"\n{marker}\n"
    end = text.index(end_marker, start)
    content = src.read_text()
    # Preserve the \n that precedes the heredoc body (it's the end of the
    # opening line). Source file provides the body; end marker follows.
    return text[:start] + "\n" + content + text[end:]


def main():
    deploy_server = None
    if len(sys.argv) > 1 and sys.argv[1] == "--deploy-server":
        deploy_server = Path(sys.argv[2])

    template = TEMPLATE.read_text()
    for marker, src_path in SECTIONS.items():
        src = Path(src_path)
        if not src.exists():
            print(f"WARNING: {src} not found, skipping {marker}")
            continue
        try:
            template = replace_section(template, marker, src)
            print(f"  {marker} <- {src_path}")
        except ValueError as e:
            print(f"ERROR: marker '{marker}' not found in template: {e}")
            sys.exit(1)

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(template)
    print(f"\nWrote {OUTPUT} ({len(template)} bytes)")

    if deploy_server:
        dest = deploy_server / "static/install/player-client.sh"
        dest.write_text(template)
        print(f"Deployed to {dest}")


if __name__ == "__main__":
    main()
