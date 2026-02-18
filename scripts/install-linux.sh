#!/usr/bin/env bash
set -euo pipefail

if [[ "${OSTYPE:-}" != linux* ]] && [[ "$(uname -s)" != "Linux" ]]; then
  echo "This installer is for Linux only." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

PTREE_BIN_SRC="${REPO_ROOT}/target/release/ptree"
PTREE_BIN_DST="/usr/local/bin/ptree"
PTREE_BIN_ALT="/usr/local/bin/Ptree"
LOOP_SCRIPT_SRC="${SCRIPT_DIR}/ptree-driver-loop.sh"
LOOP_SCRIPT_DST="/usr/local/lib/ptree/ptree-driver-loop.sh"
UNIT_SRC="${SCRIPT_DIR}/systemd/ptree-driver.service"
UNIT_DST="/etc/systemd/system/ptree-driver.service"
ENV_DST="/etc/default/ptree-driver"
PROFILE_DST="/etc/profile.d/ptree-path.sh"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd sudo
require_cmd systemctl

if ! command -v inotifywait >/dev/null 2>&1; then
  echo "Missing inotifywait (inotify-tools)." >&2
  echo "Install it first, e.g. Debian/Ubuntu: sudo apt-get install -y inotify-tools" >&2
  exit 1
fi

echo "Building ptree (release)..."
(cd "${REPO_ROOT}" && cargo build --release)

if [[ ! -x "${PTREE_BIN_SRC}" ]]; then
  echo "Build did not produce ${PTREE_BIN_SRC}" >&2
  exit 1
fi

echo "Installing binaries and service assets..."
sudo install -d /usr/local/bin /usr/local/lib/ptree /etc/systemd/system /etc/default /etc/profile.d
sudo install -m 0755 "${PTREE_BIN_SRC}" "${PTREE_BIN_DST}"
sudo ln -sfn "${PTREE_BIN_DST}" "${PTREE_BIN_ALT}"
sudo install -m 0755 "${LOOP_SCRIPT_SRC}" "${LOOP_SCRIPT_DST}"
sudo install -m 0644 "${UNIT_SRC}" "${UNIT_DST}"

if [[ ! -f "${ENV_DST}" ]]; then
  sudo tee "${ENV_DST}" >/dev/null <<'EOF'
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

echo "Enabling and starting ptree-driver.service..."
sudo systemctl daemon-reload
sudo systemctl enable --now ptree-driver.service

echo
echo "Installed successfully."
echo "Binary:"
echo "  /usr/local/bin/ptree"
echo "  /usr/local/bin/Ptree"
echo "Service:"
echo "  sudo systemctl status ptree-driver.service"
echo "  sudo journalctl -u ptree-driver.service -f"
echo "Config:"
echo "  ${ENV_DST}"
