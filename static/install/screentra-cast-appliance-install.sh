#!/usr/bin/env bash
# Screentra Cast appliance installer — retired until the appliance image has
# completed independent physical boot validation.
set -euo pipefail
printf '%s\n' \
  'ERROR: the appliance installer is intentionally disabled.' \
  'The appliance ISO/install path is not a production-supported artifact.' \
  'Use the signed Debian 13 server release installer and the signed player bundle instead.' >&2
exit 64
