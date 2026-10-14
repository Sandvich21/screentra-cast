#!/usr/bin/env bash
# Deprecated unsigned client wrapper. Retained only to fail closed.
set -euo pipefail
printf '%s\n' \
  'ERROR: ripley-client-install.sh is retired because it fetched and executed' \
  'unverified server bytes.' \
  'Use /install/player-client-bundle-install.sh with the pinned client key.' >&2
exit 64
