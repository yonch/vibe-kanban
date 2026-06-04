#!/usr/bin/env bash
set -euo pipefail

pid="${1:-1}"
if ! [[ "$pid" =~ ^[0-9]+$ ]]; then
  printf 'pid must be numeric: %s\n' "$pid" >&2
  exit 2
fi

threshold_kb="${2:-10485760}"
interval_seconds="${3:-2}"
captures="${4:-3}"

capture_script="$(dirname "$0")/capture-vk-memory.sh"
made=0

while [ "$made" -lt "$captures" ]; do
  rss_kb="$(awk '/^VmRSS:/ {print $2}' "/proc/${pid}/status" 2>/dev/null || echo 0)"
  threads="$(awk '/^Threads:/ {print $2}' "/proc/${pid}/status" 2>/dev/null || echo 0)"
  printf '%s pid=%s rss_kb=%s threads=%s\n' "$(date -Ins)" "$pid" "$rss_kb" "$threads"

  if [ "${rss_kb:-0}" -ge "$threshold_kb" ]; then
    "$capture_script" "$pid"
    made=$((made + 1))
    sleep 10
  else
    sleep "$interval_seconds"
  fi
done
