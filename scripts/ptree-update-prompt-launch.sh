#!/usr/bin/env bash
set -uo pipefail

ENV_FILE="/etc/default/ptree-auto-update"
PROMPT_BIN="/usr/local/lib/ptree/ptree-update-prompt"

REPO_ROOT="${1:-}"

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

if [[ -z "${REPO_ROOT}" ]]; then
  REPO_ROOT="${PTREE_REPO_ROOT:-}"
fi

if [[ -z "${REPO_ROOT}" ]]; then
  echo "No repository path available for update prompt." >&2
  exit 1
fi

if [[ -f "${PROMPT_SHOWN_FILE}" ]]; then
  exit 0
fi

if [[ ! -x "${PROMPT_BIN}" ]]; then
  echo "Prompt binary missing: ${PROMPT_BIN}" >&2
  exit 1
fi

if ! command -v loginctl >/dev/null 2>&1 || ! command -v runuser >/dev/null 2>&1; then
  echo "loginctl/runuser unavailable; cannot launch GUI prompt." >&2
  exit 1
fi

target_session=""
target_user=""
target_uid=""
target_display=""

while read -r sid _rest; do
  [[ -n "${sid}" ]] || continue

  active="$(loginctl show-session "${sid}" -p Active --value 2>/dev/null || true)"
  stype="$(loginctl show-session "${sid}" -p Type --value 2>/dev/null || true)"
  user="$(loginctl show-session "${sid}" -p Name --value 2>/dev/null || true)"
  display="$(loginctl show-session "${sid}" -p Display --value 2>/dev/null || true)"

  if [[ "${active}" == "yes" ]] && ([[ "${stype}" == "x11" ]] || [[ "${stype}" == "wayland" ]]); then
    uid="$(id -u "${user}" 2>/dev/null || true)"
    if [[ -n "${uid}" ]]; then
      target_session="${sid}"
      target_user="${user}"
      target_uid="${uid}"
      target_display="${display}"
      break
    fi
  fi
done < <(loginctl list-sessions --no-legend 2>/dev/null || true)

if [[ -z "${target_session}" ]] || [[ -z "${target_user}" ]] || [[ -z "${target_uid}" ]]; then
  echo "No active graphical session found; cannot launch prompt." >&2
  exit 1
fi

user_state_base="$(
  runuser -u "${target_user}" -- sh -lc 'printf %s "${XDG_STATE_HOME:-$HOME/.local/state}"' 2>/dev/null || true
)"

if [[ -z "${user_state_base}" ]] || [[ "${user_state_base}" != /* ]]; then
  if command -v getent >/dev/null 2>&1; then
    user_home="$(getent passwd "${target_user}" | cut -d: -f6)"
    if [[ -n "${user_home}" ]] && [[ "${user_home}" == /* ]]; then
      user_state_base="${user_home}/.local/state"
    fi
  fi
fi

if [[ -z "${user_state_base}" ]] || [[ "${user_state_base}" != /* ]]; then
  echo "Could not determine XDG state directory for ${target_user}." >&2
  exit 1
fi

STATE_DIR="${user_state_base%/}/ptree"
PROMPT_SHOWN_FILE="${STATE_DIR}/update-prompt-shown"

if runuser -u "${target_user}" -- test -f "${PROMPT_SHOWN_FILE}" 2>/dev/null; then
  exit 0
fi

if ! runuser -u "${target_user}" -- mkdir -p "${STATE_DIR}"; then
  echo "Failed to create user state directory: ${STATE_DIR}" >&2
  exit 1
fi

if ! runuser -u "${target_user}" -- touch "${PROMPT_SHOWN_FILE}"; then
  echo "Failed to mark prompt-shown flag: ${PROMPT_SHOWN_FILE}" >&2
  exit 1
fi

RUNTIME_DIR="/run/user/${target_uid}"
DBUS_ADDR="unix:path=${RUNTIME_DIR}/bus"
DISPLAY_VALUE="${target_display:-:0}"

runuser -u "${target_user}" -- env \
  XDG_RUNTIME_DIR="${RUNTIME_DIR}" \
  DBUS_SESSION_BUS_ADDRESS="${DBUS_ADDR}" \
  DISPLAY="${DISPLAY_VALUE}" \
  WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-0}" \
  "${PROMPT_BIN}" --repo "${REPO_ROOT}" >/dev/null 2>&1 &

exit 0
