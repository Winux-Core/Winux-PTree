#!/usr/bin/env bash
set -euo pipefail

if [[ "${OSTYPE:-}" != linux* ]] && [[ "$(uname -s)" != "Linux" ]]; then
  echo "This updater is for Linux only." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DEFAULT_WATCH_PATHS="/home /etc /usr/local /opt /srv /var/lib /var/www"
DEFAULT_DEBOUNCE_SECONDS="15"
CURRENT_BRANCH="$(git -C "${REPO_ROOT}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
# Build artifacts should not land in the repo (avoids root-owned target/).
# Default to a user-writable cache path for the build phase; we reuse it after sudo re-exec.
PTREE_TARGET_DIR="${PTREE_TARGET_DIR:-${HOME}/.cache/ptree-target}"
# Prompt binary manifest (used in both user and root phases)
PROMPT_MANIFEST="${REPO_ROOT}/tools/ptree-update-prompt/Cargo.toml"
PROMPT_BIN_SRC="${PTREE_TARGET_DIR}/release/ptree-update-prompt"

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

ensure_rust_toolchain() {
  if cargo --version >/dev/null 2>&1; then
    return
  fi

  if command -v rustup >/dev/null 2>&1; then
    echo "Configuring Rust toolchain via rustup (stable)..." >&2
    rustup toolchain install stable --component rustfmt --component cargo >/dev/null 2>&1 || rustup toolchain install stable >/dev/null 2>&1
    rustup default stable >/dev/null 2>&1
    if ! cargo --version >/dev/null 2>&1; then
      echo "Rust toolchain still unavailable after rustup install." >&2
      exit 1
    fi
  else
    echo "cargo is unavailable and rustup is not installed. Install Rust from https://rustup.rs then re-run." >&2
    exit 1
  fi
}

if [[ "${PTREE_SUDO_REEXEC:-0}" != "1" && "$(id -u)" -ne 0 ]]; then
  echo "Building ptree as $(id -un) with target dir: ${PTREE_TARGET_DIR}" >&2
  require_cmd cargo
  require_cmd git

  ensure_rust_toolchain
  mkdir -p "${PTREE_TARGET_DIR}"

  echo "Building ptree (release)..."
  (cd "${REPO_ROOT}" && CARGO_TARGET_DIR="${PTREE_TARGET_DIR}" cargo build --release)

  if [[ -f "${PROMPT_MANIFEST}" ]]; then
    echo "Building update-permission prompt (egui)..." >&2
    (cd "${REPO_ROOT}" && CARGO_TARGET_DIR="${PTREE_TARGET_DIR}" cargo build --release --manifest-path "${PROMPT_MANIFEST}") || \
      echo "Warning: failed to build ptree-update-prompt; continuing without GUI prompt binary." >&2
  fi

  echo "Escalating with sudo to install system-wide..." >&2
  exec sudo PTREE_SUDO_REEXEC=1 PTREE_TARGET_DIR="${PTREE_TARGET_DIR}" PTREE_ORIG_USER="$(id -un)" PTREE_ORIG_GROUP="$(id -gn)" "$0" "$@"
fi

PTREE_BIN_SRC="${PTREE_TARGET_DIR}/release/ptree"
PTREE_BIN_DST="/usr/local/bin/ptree"
PTREE_BIN_ALT="/usr/local/bin/Ptree"
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

require_cmd cargo
require_cmd git
require_cmd systemctl
if [[ "$(id -u)" -ne 0 ]]; then
  require_cmd sudo
fi

ensure_rust_toolchain

# At this point we are root (post re-exec). Ensure the target dir exists (user-built path).
install -d -m 0755 "$(dirname "${PTREE_TARGET_DIR}")"
install -d -m 0755 "${PTREE_TARGET_DIR}"

if [[ ! -x "${PTREE_BIN_SRC}" ]]; then
  echo "Build artifacts not found at ${PTREE_BIN_SRC}; run without sudo first so it can build as your user." >&2
  exit 1
fi

# Clean up stale previous installs before enabling new units/binaries
if as_root systemctl list-unit-files | grep -q "^${SERVICE_NAME}"; then
  as_root systemctl stop "${SERVICE_NAME}" || true
  as_root systemctl disable "${SERVICE_NAME}" || true
fi
if as_root systemctl list-unit-files | grep -q "^${AUTO_UPDATE_TIMER_NAME}"; then
  as_root systemctl stop "${AUTO_UPDATE_TIMER_NAME}" || true
  as_root systemctl disable "${AUTO_UPDATE_TIMER_NAME}" || true
fi
as_root rm -f /usr/local/bin/ptree /usr/local/bin/Ptree
as_root rm -rf /usr/local/lib/ptree
as_root rm -f "${UNIT_DST}" "${AUTO_UPDATE_SERVICE_DST}" "${AUTO_UPDATE_WAKE_SERVICE_DST}" "${AUTO_UPDATE_TIMER_DST}"

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

# Ensure executability after install (defensive for environments that strip modes)
as_root chmod 0755 "${PTREE_BIN_DST}" "${PTREE_BIN_ALT}" 2>/dev/null || true
as_root find /usr/local/lib/ptree -type f -maxdepth 1 -print0 | as_root xargs -0 chmod 0755 2>/dev/null || true

# Fix cache ownership for the invoking user (needed when service created these as nobody/root)
if [[ -n "${PTREE_ORIG_USER:-}" ]]; then
  as_root chown -R "${PTREE_ORIG_USER}":"${PTREE_ORIG_GROUP:-${PTREE_ORIG_USER}}" /var/cache/ptree 2>/dev/null || true
  as_root chown -R "${PTREE_ORIG_USER}":"${PTREE_ORIG_GROUP:-${PTREE_ORIG_USER}}" "/home/${PTREE_ORIG_USER}/.cache/ptree" 2>/dev/null || true
fi

if [[ ! -f "${AUTO_UPDATE_ENV_DST}" ]]; then
  as_root tee "${AUTO_UPDATE_ENV_DST}" >/dev/null <<EOF
PTREE_REPO_ROOT="${REPO_ROOT}"
PTREE_UPDATE_REMOTE="origin"
PTREE_UPDATE_BRANCH="${CURRENT_BRANCH}"
PTREE_PROMPT_ON_WAKE_FAILURE="1"
PTREE_UPDATE_SCRIPT="scripts/linux/update-driver.sh"
PTREE_STATE_DIR="/var/lib/ptree"
PTREE_TARGET_DIR="${PTREE_TARGET_DIR}"
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

  if as_root grep -q '^PTREE_WATCH_PATHS="/"$' "${DRIVER_ENV_DST}"; then
    as_root sed -i "s|^PTREE_WATCH_PATHS=\"/\"\$|PTREE_WATCH_PATHS=\"${DEFAULT_WATCH_PATHS}\"|" "${DRIVER_ENV_DST}"
  elif ! as_root grep -q '^PTREE_WATCH_PATHS=' "${DRIVER_ENV_DST}"; then
    as_root tee -a "${DRIVER_ENV_DST}" >/dev/null <<EOF

# Space-separated paths to monitor recursively by default.
# Set PTREE_WATCH_PATHS="/" only if you explicitly want full-root watching.
PTREE_WATCH_PATHS="${DEFAULT_WATCH_PATHS}"
EOF
  fi

  if as_root grep -q '^PTREE_DEBOUNCE_SECONDS="5"$' "${DRIVER_ENV_DST}"; then
    as_root sed -i "s/^PTREE_DEBOUNCE_SECONDS=\"5\"\$/PTREE_DEBOUNCE_SECONDS=\"${DEFAULT_DEBOUNCE_SECONDS}\"/" "${DRIVER_ENV_DST}"
  elif ! as_root grep -q '^PTREE_DEBOUNCE_SECONDS=' "${DRIVER_ENV_DST}"; then
    as_root tee -a "${DRIVER_ENV_DST}" >/dev/null <<EOF

# Debounce window in seconds before cache refresh after events.
PTREE_DEBOUNCE_SECONDS="${DEFAULT_DEBOUNCE_SECONDS}"
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
if [[ -d "${REPO_ROOT}/target" && -n "${PTREE_ORIG_USER:-}" ]]; then
  echo "Fixing ownership of repo-local target/ to ${PTREE_ORIG_USER} to avoid future permission errors..."
  as_root chown -R "${PTREE_ORIG_USER}":"${PTREE_ORIG_GROUP:-${PTREE_ORIG_USER}}" "${REPO_ROOT}/target" || true
fi
echo "Check status:"
echo "  sudo systemctl status ${SERVICE_NAME}"
echo "  sudo journalctl -u ${SERVICE_NAME} -f"
echo "  sudo systemctl status ${AUTO_UPDATE_TIMER_NAME}"
