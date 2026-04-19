#!/bin/bash
# Run profile_go_c3_memory.py as a child and sample RSS from outside.
# Outputs: /tmp/rss_profile_ext_<pid>.csv

set -e

DATASET="${DATASET:-event4_617}"
DIVISION="${DIVISION:-D1}"
SCENARIO="${SCENARIO:-2}"
AC_RECONCILE="${AC_RECONCILE:-none}"
EXTRA_FLAGS="${EXTRA_FLAGS:-}"

cd "$(dirname "$0")/.."

# Launch the child
uv run python scripts/profile_go_c3_memory.py \
    --dataset "$DATASET" \
    --division "$DIVISION" \
    --scenario "$SCENARIO" \
    --ac-reconcile "$AC_RECONCILE" \
    $EXTRA_FLAGS > /tmp/child_stdout_$$.log 2>&1 &

UV_PID=$!
# The python child writes its pid to a file early in startup.
rm -f /tmp/profile_go_c3_memory.pid
CHILD_PID=""
for _ in $(seq 1 100); do
    if [ -f /tmp/profile_go_c3_memory.pid ]; then
        CHILD_PID=$(cat /tmp/profile_go_c3_memory.pid)
        break
    fi
    sleep 0.1
done
if [ -z "$CHILD_PID" ]; then
    echo "child never wrote pidfile /tmp/profile_go_c3_memory.pid"
    exit 1
fi
CSV=/tmp/rss_profile_ext_${CHILD_PID}.csv
echo "uv pid=$UV_PID  child python pid=$CHILD_PID  csv=$CSV"
echo "t_s,rss_mb" > "$CSV"

# Sample every 250 ms for a long time or until child exits
START=$(date +%s.%N)
while kill -0 "$CHILD_PID" 2>/dev/null; do
    NOW=$(date +%s.%N)
    T=$(awk -v n="$NOW" -v s="$START" 'BEGIN{printf "%.3f", n-s}')
    RSS_KB=$(ps -o rss= -p "$CHILD_PID" 2>/dev/null | tr -d ' ')
    if [ -n "$RSS_KB" ]; then
        RSS_MB=$(awk -v k="$RSS_KB" 'BEGIN{printf "%.1f", k/1024}')
        echo "$T,$RSS_MB" >> "$CSV"
    fi
    sleep 0.25
done

echo "child exited"
wait $UV_PID || true

cat /tmp/child_stdout_$$.log
rm -f /tmp/child_stdout_$$.log

echo "---"
echo "CSV samples:"
wc -l "$CSV"
echo "Peak RSS (MB):"
python3 -c "
import csv
rows = list(csv.reader(open('$CSV')))[1:]
vals = [(float(t), float(rss)) for t, rss in rows]
print(f'  samples: {len(vals)}')
if vals:
    peak = max(vals, key=lambda v: v[1])
    print(f'  peak: {peak[1]:,.1f} MB at t={peak[0]:.2f}s')
    # Show growth steps (>200MB jumps)
    prev = 0.0
    prev_t = 0.0
    print('  >200MB jumps:')
    for t, rss in vals:
        if rss - prev > 200:
            print(f'    t={t:7.2f}s  rss={rss:11,.1f} MB  (+{rss-prev:,.1f})')
        prev = rss
        prev_t = t
    # Sample every 10%
    print('  timeline (every 10%):')
    n = len(vals)
    for i in range(0, n, max(1, n//10)):
        t, rss = vals[i]
        print(f'    t={t:7.2f}s  rss={rss:11,.1f} MB')
    print(f'    t={vals[-1][0]:7.2f}s  rss={vals[-1][1]:11,.1f} MB (last)')
"
