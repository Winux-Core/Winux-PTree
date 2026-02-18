#!/usr/bin/env bash
set -euo pipefail

PTREE_BIN="${PTREE_BIN:-/usr/local/bin/ptree}"
PTREE_WATCH_PATHS="${PTREE_WATCH_PATHS:-/}"
PTREE_DEBOUNCE_SECONDS="${PTREE_DEBOUNCE_SECONDS:-5}"
PTREE_ARGS="${PTREE_ARGS:---quiet --cache-ttl 0}"
PTREE_THREADS="${PTREE_THREADS:-}"

EXCLUDE_REGEX='^/(proc|sys|dev|run|tmp)($|/)'

if ! command -v inotifywait >/dev/null 2>&1; then
  echo "inotifywait is required (install inotify-tools)." >&2
  exit 1
fi

if [[ ! -x "${PTREE_BIN}" ]]; then
  echo "ptree binary not found or not executable: ${PTREE_BIN}" >&2
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
echo "Watching: ${PTREE_WATCH_PATHS}"
echo "Debounce: ${PTREE_DEBOUNCE_SECONDS}s"
echo "Ptree args: ${PTREE_ARGS}${PTREE_THREADS:+ --threads ${PTREE_THREADS}}"

# Warm cache at startup.
run_refresh || true

last_run=0

while true; do
  # Keep one long-lived inotify process; restart loop if it exits.
  inotifywait -m -r -q \
    -e create -e modify -e delete -e move -e attrib \
    --exclude "${EXCLUDE_REGEX}" \
    ${PTREE_WATCH_PATHS} | while read -r _; do
      now="$(date +%s)"
      if (( now - last_run < PTREE_DEBOUNCE_SECONDS )); then
        continue
      fi

      last_run="${now}"
      run_refresh || true
    done

  sleep 1
done
