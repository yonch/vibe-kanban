#!/usr/bin/env bash
set -euo pipefail

pid="${1:-1}"
if ! [[ "$pid" =~ ^[0-9]+$ ]]; then
  printf 'pid must be numeric: %s\n' "$pid" >&2
  exit 2
fi

out_root="${VK_MEM_CAPTURE_DIR:-/tmp/vk-mem-captures}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out_dir="${out_root}/${stamp}-pid${pid}"

mkdir -p "$out_dir"

run() {
  local name="$1"
  shift
  {
    printf '$'
    printf ' %q' "$@"
    printf '\n\n'
    "$@"
  } >"${out_dir}/${name}.txt" 2>&1 || true
}

run "date" date -Ins
run "uname" uname -a
run "free" free -h
run "cmdline" bash -lc "tr '\\0' ' ' </proc/${pid}/cmdline; printf '\\n'"
run "status" cat "/proc/${pid}/status"
run "smaps_rollup" cat "/proc/${pid}/smaps_rollup"
run "pmap_top_rss" bash -lc "pmap -x ${pid} | sort -k3 -nr | head -100"
run "maps" cat "/proc/${pid}/maps"
run "thread_summary" bash -lc "ps -T -p ${pid} -o pid,tid,comm,stat,pcpu,pmem,rss,vsz,wchan:32 | head -1000"
run "thread_names" bash -lc "ps -T -p ${pid} -o comm= | sort | uniq -c | sort -nr"
run "fd_count" bash -lc "ls -la /proc/${pid}/fd | wc -l"
run "fd_list" bash -lc "ls -la /proc/${pid}/fd | head -1000"
run "lsof" lsof -nP -p "$pid"
run "sockets" ss -tanp
run "process_tree" ps -efH
run "top_rss" bash -lc "ps -eo pid,ppid,comm,args,rss,vsz,etime --sort=-rss | head -100"

tar -C "$out_root" -czf "${out_dir}.tar.gz" "$(basename "$out_dir")"
printf '%s\n' "${out_dir}.tar.gz"
