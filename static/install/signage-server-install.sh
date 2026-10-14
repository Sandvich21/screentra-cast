#!/usr/bin/env bash
# Deprecated compatibility wrapper. Fail closed rather than fetching an
# unsigned installer from a configurable HTTP source.
set -euo pipefail
printf '%s\n' \
  'ERROR: signage-server-install.sh is retired.' \
  'Use screentra-cast-server-install.sh with an explicit HTTPS GitHub Release URL.' >&2
exit 64
