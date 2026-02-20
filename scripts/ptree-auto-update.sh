#!/usr/bin/env bash
set -uo pipefail

ON_WAKE=0
if [[ "${1:-}" == "--on-wake" ]]; then
  ON_WAKE=1
fi

ENV_FILE="/etc/default/ptree-auto-update"
PROMPT_LAUNCHER="/usr/local/lib/ptree/ptree-update-prompt-launch.sh"

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

PTREE_REPO_ROOT="${PTREE_REPO_ROOT:-}"
PTREE_UPDATE_REMOTE="${PTREE_UPDATE_REMOTE:-origin}"
PTREE_UPDATE_BRANCH="${PTREE_UPDATE_BRANCH:-main}"
PTREE_PROMPT_ON_WAKE_FAILURE="${PTREE_PROMPT_ON_WAKE_FAILURE:-1}"
PTREE_UPDATE_SCRIPT="${PTREE_UPDATE_SCRIPT:-scripts/update-driver.sh}"
PTREE_STATE_DIR="${PTREE_STATE_DIR:-}"

if [[ -z "${PTREE_STATE_DIR}" ]]; then
  if [[ "$(id -u)" -eq 0 ]]; then
    PTREE_STATE_DIR="/var/lib/ptree"
  else
    PTREE_STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/ptree"
  fi
fi

if [[ "${PTREE_STATE_DIR}" != /* ]]; then
  echo "PTREE_STATE_DIR must be an absolute path: ${PTREE_STATE_DIR}" >&2
  exit 1
fi

STATE_DIR="${PTREE_STATE_DIR}"
LOCK_FILE="${STATE_DIR}/auto-update.lock"
FAIL_FLAG="${STATE_DIR}/auto-update-failed"

if ! command -v git >/dev/null 2>&1; then
  echo "git is required for auto-update." >&2
  exit 1
fi

if [[ -z "${PTREE_REPO_ROOT}" ]]; then
  echo "PTREE_REPO_ROOT is not set; skipping auto-update." >&2
  exit 1
fi

if [[ ! -d "${PTREE_REPO_ROOT}/.git" ]]; then
  echo "Not a git repo: ${PTREE_REPO_ROOT}" >&2
  exit 1
fi

mkdir -p "${STATE_DIR}"

if command -v flock >/dev/null 2>&1; then
  exec 9>"${LOCK_FILE}"
  if ! flock -n 9; then
    echo "Another ptree auto-update is already running; exiting."
    exit 0
  fi
fi

update_ok=0

(
  set -euo pipefail
  cd "${PTREE_REPO_ROOT}"
  current_commit="$(git rev-parse HEAD)"
  git fetch "${PTREE_UPDATE_REMOTE}" --prune
  remote_ref="${PTREE_UPDATE_REMOTE}/${PTREE_UPDATE_BRANCH}"
  remote_commit="$(git rev-parse "${remote_ref}")"
  if [[ "${current_commit}" == "${remote_commit}" ]]; then
    echo "PTree already up to date (${current_commit})."
    exit 0
  fi
  git pull --ff-only "${PTREE_UPDATE_REMOTE}" "${PTREE_UPDATE_BRANCH}"
  bash "${PTREE_REPO_ROOT}/${PTREE_UPDATE_SCRIPT}"
) && update_ok=1

if [[ "${update_ok}" -eq 1 ]]; then
  rm -f "${FAIL_FLAG}"
  echo "PTree auto-update succeeded."
  exit 0
fi

echo "PTree auto-update failed." >&2
touch "${FAIL_FLAG}"

if [[ "${ON_WAKE}" -eq 1 ]] && [[ "${PTREE_PROMPT_ON_WAKE_FAILURE}" == "1" ]]; then
  if [[ -x "${PROMPT_LAUNCHER}" ]]; then
    "${PROMPT_LAUNCHER}" "${PTREE_REPO_ROOT}" || true
  fi
fi

exit 1
