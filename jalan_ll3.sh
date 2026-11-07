#!/bin/bash
set -a; . /root/sol/.rpc.env; set +a
cd /root/sol/hist_dex
T0=$(date -u -d "2026-11-03T00:00:00Z" +%s); T1=$((T0 + 26*3600))
/root/sol/kumpul/target/release/tarik_dex launchlab_2026-11-03.jsonl LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj $T0 $T1 2000 2>&1 | tail -2
echo LL3_SELESAI
