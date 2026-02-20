#!/usr/bin/env bash
set -euo pipefail

if [[ "${OSTYPE:-}" != linux* ]] && [[ "$(uname -s)" != "Linux" ]]; then
  echo "This installer is for Linux only." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ENV_DST="/etc/default/ptree-driver"
PROFILE_DST="/etc/profile.d/ptree-path.sh"
UPDATE_SCRIPT="${SCRIPT_DIR}/update-driver.sh"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd git
require_cmd sudo
require_cmd systemctl

if ! command -v inotifywait >/dev/null 2>&1; then
  echo "Missing inotifywait (inotify-tools)." >&2
  echo "Install it first, e.g. Debian/Ubuntu: sudo apt-get install -y inotify-tools" >&2
  exit 1
fi

if [[ ! -x "${UPDATE_SCRIPT}" ]]; then
  echo "Missing updater script: ${UPDATE_SCRIPT}" >&2
  exit 1
fi

echo "Running updater/install pipeline..."
bash "${UPDATE_SCRIPT}"

if [[ ! -f "${ENV_DST}" ]]; then
  sudo tee "${ENV_DST}" >/dev/null <<'EOF'
# XDG cache base for the systemd service user.
# ptree will use: $XDG_CACHE_HOME/ptree/ptree.dat
XDG_CACHE_HOME="/var/cache"

# Space-separated paths to monitor recursively.
# Keep "/" for broad coverage; tune this if your system hits inotify limits.
PTREE_WATCH_PATHS="/"

# Debounce window in seconds before cache refresh after events.
PTREE_DEBOUNCE_SECONDS="5"

# CLI args passed to ptree on each refresh.
# cache-ttl=0 means event-triggered runs always rescan and persist updates.
PTREE_ARGS="--quiet --cache-ttl 0"

# Optional thread override (empty = ptree default heuristic).
# PTREE_THREADS="1"
EOF
fi

sudo tee "${PROFILE_DST}" >/dev/null <<'EOF'
# Ensure ptree commands are available in Bash sessions.
case ":$PATH:" in
  *:/usr/local/bin:*) ;;
  *) export PATH="/usr/local/bin:$PATH" ;;
esac
EOF

echo
echo "Installed successfully."
echo "Binary:"
echo "  /usr/local/bin/ptree"
echo "  /usr/local/bin/Ptree"
echo "Service:"
echo "  sudo systemctl status ptree-driver.service"
echo "  sudo journalctl -u ptree-driver.service -f"
echo "Auto-update:"
echo "  sudo systemctl status ptree-auto-update.timer"
echo "  sudo systemctl list-timers | grep ptree-auto-update"
echo "Config:"
echo "  ${ENV_DST}"
