#!/usr/bin/env bash
# The historical server bundle was unsigned and used raw tar extraction.
# Refuse it instead of preserving an executable trust bypass.
set -euo pipefail
printf '%s\n' \
  'ERROR: the unsigned server bundle installer is retired.' \
  'Use screentra-cast-server-install.sh with SCREENTRA_CAST_RELEASE_BASE_URL' \
  'set to an HTTPS GitHub Release and verify SHA256SUMS.' >&2
exit 64
