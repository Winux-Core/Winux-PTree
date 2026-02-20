#!/usr/bin/env bash
set -euo pipefail

if [[ "${OSTYPE:-}" != linux* ]] && [[ "$(uname -s)" != "Linux" ]]; then
  echo "This updater is for Linux only." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CURRENT_BRANCH="$(git -C "${REPO_ROOT}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"

PTREE_BIN_SRC="${REPO_ROOT}/target/release/ptree"
PTREE_BIN_DST="/usr/local/bin/ptree"
PTREE_BIN_ALT="/usr/local/bin/Ptree"
PROMPT_MANIFEST="${REPO_ROOT}/tools/ptree-update-prompt/Cargo.toml"
PROMPT_BIN_SRC="${REPO_ROOT}/target/release/ptree-update-prompt"
PROMPT_BIN_DST="/usr/local/lib/ptree/ptree-update-prompt"
LOOP_SCRIPT_SRC="${SCRIPT_DIR}/ptree-driver-loop.sh"
LOOP_SCRIPT_DST="/usr/local/lib/ptree/ptree-driver-loop.sh"
UNIT_SRC="${SCRIPT_DIR}/systemd/ptree-driver.service"
UNIT_DST="/etc/systemd/system/ptree-driver.service"
AUTO_UPDATE_SCRIPT_SRC="${SCRIPT_DIR}/ptree-auto-update.sh"
AUTO_UPDATE_SCRIPT_DST="/usr/local/lib/ptree/ptree-auto-update.sh"
PROMPT_LAUNCHER_SRC="${SCRIPT_DIR}/ptree-update-prompt-launch.sh"
PROMPT_LAUNCHER_DST="/usr/local/lib/ptree/ptree-update-prompt-launch.sh"
AUTO_UPDATE_SERVICE_SRC="${SCRIPT_DIR}/systemd/ptree-auto-update.service"
AUTO_UPDATE_SERVICE_DST="/etc/systemd/system/ptree-auto-update.service"
AUTO_UPDATE_WAKE_SERVICE_SRC="${SCRIPT_DIR}/systemd/ptree-auto-update-wake.service"
AUTO_UPDATE_WAKE_SERVICE_DST="/etc/systemd/system/ptree-auto-update-wake.service"
AUTO_UPDATE_TIMER_SRC="${SCRIPT_DIR}/systemd/ptree-auto-update.timer"
AUTO_UPDATE_TIMER_DST="/etc/systemd/system/ptree-auto-update.timer"
SLEEP_HOOK_SRC="${SCRIPT_DIR}/systemd/ptree-auto-update-sleep-hook.sh"
SLEEP_HOOK_DST="/usr/lib/systemd/system-sleep/ptree-auto-update"
AUTO_UPDATE_ENV_DST="/etc/default/ptree-auto-update"
DRIVER_ENV_DST="/etc/default/ptree-driver"
SERVICE_NAME="ptree-driver.service"
AUTO_UPDATE_TIMER_NAME="ptree-auto-update.timer"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

as_root() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  else
    sudo "$@"
  fi
}

require_cmd cargo
require_cmd git
require_cmd systemctl
if [[ "$(id -u)" -ne 0 ]]; then
  require_cmd sudo
fi

echo "Building ptree (release)..."
(cd "${REPO_ROOT}" && cargo build --release)

if [[ -f "${PROMPT_MANIFEST}" ]]; then
  echo "Building update-permission prompt (egui)..."
  if ! (cd "${REPO_ROOT}" && cargo build --release --manifest-path "${PROMPT_MANIFEST}" --target-dir "${REPO_ROOT}/target"); then
    echo "Warning: failed to build ptree-update-prompt; continuing without GUI prompt binary." >&2
  fi
fi

if [[ ! -x "${PTREE_BIN_SRC}" ]]; then
  echo "Build did not produce ${PTREE_BIN_SRC}" >&2
  exit 1
fi

echo "Updating binaries and service assets..."
as_root install -d /usr/local/bin /usr/local/lib/ptree /etc/systemd/system /etc/default /usr/lib/systemd/system-sleep /var/lib/ptree /var/cache/ptree
as_root install -m 0755 "${PTREE_BIN_SRC}" "${PTREE_BIN_DST}"
as_root ln -sfn "${PTREE_BIN_DST}" "${PTREE_BIN_ALT}"
as_root install -m 0755 "${LOOP_SCRIPT_SRC}" "${LOOP_SCRIPT_DST}"
as_root install -m 0644 "${UNIT_SRC}" "${UNIT_DST}"
as_root install -m 0755 "${AUTO_UPDATE_SCRIPT_SRC}" "${AUTO_UPDATE_SCRIPT_DST}"
as_root install -m 0755 "${PROMPT_LAUNCHER_SRC}" "${PROMPT_LAUNCHER_DST}"
as_root install -m 0644 "${AUTO_UPDATE_SERVICE_SRC}" "${AUTO_UPDATE_SERVICE_DST}"
as_root install -m 0644 "${AUTO_UPDATE_WAKE_SERVICE_SRC}" "${AUTO_UPDATE_WAKE_SERVICE_DST}"
as_root install -m 0644 "${AUTO_UPDATE_TIMER_SRC}" "${AUTO_UPDATE_TIMER_DST}"
as_root install -m 0755 "${SLEEP_HOOK_SRC}" "${SLEEP_HOOK_DST}"

if [[ -x "${PROMPT_BIN_SRC}" ]]; then
  as_root install -m 0755 "${PROMPT_BIN_SRC}" "${PROMPT_BIN_DST}"
fi

if [[ ! -f "${AUTO_UPDATE_ENV_DST}" ]]; then
  as_root tee "${AUTO_UPDATE_ENV_DST}" >/dev/null <<EOF
PTREE_REPO_ROOT="${REPO_ROOT}"
PTREE_UPDATE_REMOTE="origin"
PTREE_UPDATE_BRANCH="${CURRENT_BRANCH}"
PTREE_PROMPT_ON_WAKE_FAILURE="1"
PTREE_UPDATE_SCRIPT="scripts/update-driver.sh"
PTREE_STATE_DIR="/var/lib/ptree"
EOF
fi

if [[ -f "${DRIVER_ENV_DST}" ]]; then
  if ! as_root grep -q '^XDG_CACHE_HOME=' "${DRIVER_ENV_DST}"; then
    as_root tee -a "${DRIVER_ENV_DST}" >/dev/null <<'EOF'

# XDG cache base for the systemd service user.
# ptree will use: $XDG_CACHE_HOME/ptree/ptree.dat
XDG_CACHE_HOME="/var/cache"
EOF
  fi
fi

echo "Reloading systemd and restarting ${SERVICE_NAME}..."
as_root systemctl daemon-reload

if ! as_root systemctl is-enabled --quiet "${SERVICE_NAME}"; then
  as_root systemctl enable "${SERVICE_NAME}"
fi

if as_root systemctl is-active --quiet "${SERVICE_NAME}"; then
  as_root systemctl restart "${SERVICE_NAME}"
else
  as_root systemctl start "${SERVICE_NAME}"
fi

if ! as_root systemctl is-enabled --quiet "${AUTO_UPDATE_TIMER_NAME}"; then
  as_root systemctl enable "${AUTO_UPDATE_TIMER_NAME}"
fi

if as_root systemctl is-active --quiet "${AUTO_UPDATE_TIMER_NAME}"; then
  as_root systemctl restart "${AUTO_UPDATE_TIMER_NAME}"
else
  as_root systemctl start "${AUTO_UPDATE_TIMER_NAME}"
fi

echo
echo "Update complete."
echo "Check status:"
echo "  sudo systemctl status ${SERVICE_NAME}"
echo "  sudo journalctl -u ${SERVICE_NAME} -f"
echo "  sudo systemctl status ${AUTO_UPDATE_TIMER_NAME}"
