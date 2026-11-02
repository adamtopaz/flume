#!/usr/bin/env bash
# Runs a command inside the current cgroup (the harness starts this script in a
# transient systemd scope) and, after it exits, records the cgroup's peak memory
# and CPU usage to the file named by FLUME_BENCH_STATS so the harness can account
# for the whole process tree, not just reaped children. The harness's variables are
# removed from the command's environment, so the measured tool cannot see them.
stats=${FLUME_BENCH_STATS:-}
scoped=${FLUME_BENCH_SCOPED:-0}
unset FLUME_BENCH_STATS FLUME_BENCH_SCOPED
"$@"
rc=$?
if [ -n "$stats" ] && [ "$scoped" = "1" ]; then
  cg=$(cut -d: -f3 /proc/self/cgroup 2>/dev/null | head -n1)
  {
    echo "cgroup=$cg"
    cat "/sys/fs/cgroup$cg/memory.peak" 2>/dev/null | sed 's/^/memory_peak=/'
    grep -E '^(usage_usec|user_usec|system_usec) ' "/sys/fs/cgroup$cg/cpu.stat" 2>/dev/null
  } > "$stats"
fi
exit $rc
