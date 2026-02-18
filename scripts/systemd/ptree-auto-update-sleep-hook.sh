#!/usr/bin/env bash
set -euo pipefail

# systemd-sleep hook: called with "pre|post" and sleep action.
if [[ "${1:-}" == "post" ]]; then
  /usr/bin/systemctl start ptree-auto-update-wake.service || true
fi
