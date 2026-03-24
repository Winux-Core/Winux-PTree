#!/usr/bin/env bash
set -euo pipefail

PTREE_BIN="${PTREE_BIN:-/usr/local/bin/ptree}"
DEFAULT_WATCH_PATHS=(/home /etc /usr/local /opt /srv /var/lib /var/www)
PTREE_WATCH_PATHS="${PTREE_WATCH_PATHS:-}"
PTREE_DEBOUNCE_SECONDS="${PTREE_DEBOUNCE_SECONDS:-15}"
PTREE_ARGS="${PTREE_ARGS:---quiet}"
PTREE_THREADS="${PTREE_THREADS:-}"

# Ensure cache TTL stays at 30 seconds for watcher-triggered runs.
if [[ "${PTREE_ARGS}" =~ --cache-ttl[=[:space:]]*[0-9]+ ]]; then
  PTREE_ARGS="$(printf '%s\n' "${PTREE_ARGS}" | sed -E 's/--cache-ttl[=[:space:]]*[0-9]+/--cache-ttl 30/g')"
else
  PTREE_ARGS+=" --cache-ttl 30"
fi

# Ensure a cache home for systemd environments that lack HOME.
XDG_CACHE_HOME="${XDG_CACHE_HOME:-/var/cache}"
export XDG_CACHE_HOME

EXCLUDE_REGEX='^/(proc|sys|dev|run|tmp)($|/)'

if ! command -v inotifywait >/dev/null 2>&1; then
  echo "inotifywait is required (install inotify-tools)." >&2
  exit 1
fi

if [[ ! -x "${PTREE_BIN}" ]]; then
  echo "ptree binary not found or not executable: ${PTREE_BIN}" >&2
  exit 1
fi

if [[ -n "${PTREE_WATCH_PATHS}" ]]; then
  read -r -a WATCH_PATHS <<< "${PTREE_WATCH_PATHS}"
else
  WATCH_PATHS=()
  for path in "${DEFAULT_WATCH_PATHS[@]}"; do
    if [[ -d "${path}" ]]; then
      WATCH_PATHS+=("${path}")
    fi
  done
fi

FILTERED_WATCH_PATHS=()
for path in "${WATCH_PATHS[@]}"; do
  if [[ -d "${path}" ]]; then
    FILTERED_WATCH_PATHS+=("${path}")
  fi
done
WATCH_PATHS=("${FILTERED_WATCH_PATHS[@]}")

if [[ -z "${PTREE_WATCH_PATHS}" && ${#WATCH_PATHS[@]} -eq 0 ]]; then
  WATCH_PATHS=(/)
fi

if [[ ${#WATCH_PATHS[@]} -eq 0 ]]; then
  echo "No valid watch paths configured. Set PTREE_WATCH_PATHS to one or more existing directories." >&2
  exit 1
fi

read -r -a PTREE_ARGV <<< "${PTREE_ARGS}"
if [[ -n "${PTREE_THREADS}" ]]; then
  PTREE_ARGV+=(--threads "${PTREE_THREADS}")
fi

run_refresh() {
  if ! "${PTREE_BIN}" "${PTREE_ARGV[@]}"; then
    echo "ptree refresh failed at $(date -Is)" >&2
    return 1
  fi
  return 0
}

echo "ptree-driver loop starting"
echo "Watching: ${WATCH_PATHS[*]}"
echo "Debounce: ${PTREE_DEBOUNCE_SECONDS}s"
echo "Ptree args: ${PTREE_ARGS}${PTREE_THREADS:+ --threads ${PTREE_THREADS}}"

# Warm cache at startup.
run_refresh || true

wait_for_quiet_period() {
  local deadline now timeout

  deadline=$(( $(date +%s) + PTREE_DEBOUNCE_SECONDS ))

  while true; do
    now="$(date +%s)"
    timeout=$(( deadline - now ))
    if (( timeout <= 0 )); then
      return 0
    fi

    if ! IFS= read -r -t "${timeout}" _; then
      return 0
    fi

    deadline=$(( $(date +%s) + PTREE_DEBOUNCE_SECONDS ))
  done
}

while true; do
  # Keep one long-lived inotify process; after the first event, wait for a quiet period
  # so bursts of related filesystem activity collapse into a single refresh.
  while IFS= read -r _; do
      wait_for_quiet_period
      run_refresh || true
  done < <(
    inotifywait -m -r -q \
      -e create -e modify -e delete -e move -e attrib \
      --exclude "${EXCLUDE_REGEX}" \
      "${WATCH_PATHS[@]}"
  )

  sleep 1
done
