#!/bin/bash
# Keep pulling NEW coins backwards in time, 11 h windows, every tx of the first 6 h of each pool (Helius gTFA, verified
# identical to live data). Only COMPLETED pulls (and the live-recorded pools) count as already collected (fix 2026-11-09:
# pools with only a creation record were wrongly excluded). Stops when disk free < 3 GB.
cd /root/sol
while pgrep -x riwayat >/dev/null; do sleep 30; done
set -a; . ./.rpc.env; set +a
END=$(( $(date +%s) - 17*3600 ))
for i in $(seq 1 30); do
  FREE=$(df --output=avail -B1G / | tail -1 | tr -d " ")
  [ "$FREE" -lt 3 ] && { echo "disk low ($FREE GB): stop"; break; }
  {
    zstdcat raw/*/*.zst 2>/dev/null | jq -r 'select(.kind=="tx")|.pool'
    for d in raw_1000 raw_more; do zstdcat $d/*/*.zst 2>/dev/null | jq -r 'select(.kind=="pull" and .listing_complete==true)|.pool'; done
  } | sort -u > exclude_pools.txt
  START=$((END - 11*3600))
  echo "== window $i: $(date -u -d @$START +%F_%H:%M) .. $(date -u -d @$END +%F_%H:%M) exclude $(wc -l < exclude_pools.txt)"
  RPC_URLS="https://solana-rpc.publicnode.com|40|40,${HELIUS_HTTP}|300|100000" ALLOW_PAID=1 MAX_CALLS=3000000 GTFA=1 GTFA_MAX_CALLS=60000 GTFA_PACE_MS=120 HOURS=6 MAX_POOLS=0 WORKERS=10 EXCLUDE=/root/sol/exclude_pools.txt \
    ./sol2/ingest/target/release/riwayat /root/sol/raw_more $START $END 2>&1 | grep -vE "^(meteora_damm_v2|pumpswap) " | tail -4
  END=$START
done
echo LANJUT_DONE
