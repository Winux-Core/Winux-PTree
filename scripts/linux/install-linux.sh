#!/usr/bin/env bash
set -euo pipefail

if [[ "${OSTYPE:-}" != linux* ]] && [[ "$(uname -s)" != "Linux" ]]; then
  echo "This installer is for Linux only." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DEFAULT_WATCH_PATHS="/home /etc /usr/local /opt /srv /var/lib /var/www"
DEFAULT_DEBOUNCE_SECONDS="15"

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

ensure_rust_toolchain

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
  sudo tee "${ENV_DST}" >/dev/null <<EOF
# XDG cache base for the systemd service user.
# ptree will use: \$XDG_CACHE_HOME/ptree/ptree.dat
XDG_CACHE_HOME="/var/cache"

# Space-separated paths to monitor recursively by default.
# Set PTREE_WATCH_PATHS="/" only if you explicitly want full-root watching.
PTREE_WATCH_PATHS="${DEFAULT_WATCH_PATHS}"

# Debounce window in seconds before cache refresh after events.
PTREE_DEBOUNCE_SECONDS="${DEFAULT_DEBOUNCE_SECONDS}"

# CLI args passed to ptree on each refresh.
# cache-ttl=30 keeps the cache fresh but avoids unnecessary full rewrites.
PTREE_ARGS="--quiet --cache-ttl 30"

# Optional thread override (empty = ptree default heuristic).
# PTREE_THREADS="1"
EOF
else
  if sudo grep -q '^PTREE_WATCH_PATHS="/"$' "${ENV_DST}"; then
    sudo sed -i "s|^PTREE_WATCH_PATHS=\"/\"\$|PTREE_WATCH_PATHS=\"${DEFAULT_WATCH_PATHS}\"|" "${ENV_DST}"
  elif ! sudo grep -q '^PTREE_WATCH_PATHS=' "${ENV_DST}"; then
    sudo tee -a "${ENV_DST}" >/dev/null <<EOF

# Space-separated paths to monitor recursively by default.
# Set PTREE_WATCH_PATHS="/" only if you explicitly want full-root watching.
PTREE_WATCH_PATHS="${DEFAULT_WATCH_PATHS}"
EOF
  fi

  if sudo grep -q '^PTREE_DEBOUNCE_SECONDS="5"$' "${ENV_DST}"; then
    sudo sed -i "s/^PTREE_DEBOUNCE_SECONDS=\"5\"\$/PTREE_DEBOUNCE_SECONDS=\"${DEFAULT_DEBOUNCE_SECONDS}\"/" "${ENV_DST}"
  elif ! sudo grep -q '^PTREE_DEBOUNCE_SECONDS=' "${ENV_DST}"; then
    sudo tee -a "${ENV_DST}" >/dev/null <<EOF

# Debounce window in seconds before cache refresh after events.
PTREE_DEBOUNCE_SECONDS="${DEFAULT_DEBOUNCE_SECONDS}"
EOF
  fi

  if sudo grep -q -- "--cache-ttl" "${ENV_DST}"; then
    sudo sed -i 's/--cache-ttl[=[:space:]]*[0-9]*/--cache-ttl 30/g' "${ENV_DST}"
  else
    sudo sed -i 's/^PTREE_ARGS="\(.*\)"/PTREE_ARGS="\1 --cache-ttl 30"/' "${ENV_DST}"
  fi
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
