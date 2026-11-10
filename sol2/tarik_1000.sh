#!/bin/bash
# 1000 new pools (not already collected), every tx of the first 6 h, free publicnode + Helius (hard cap 150k calls on Helius)
cd /root/sol
set -a; . ./.rpc.env; set +a
NOW=$(date +%s)
export RPC_URLS="https://solana-rpc.publicnode.com|30|40"
export GTFA=1 GTFA_MAX_CALLS=8000 ALLOW_PAID=1 MAX_CALLS=3000000 HOURS=6 MAX_POOLS=1000 WORKERS=6 EXCLUDE=/root/sol/exclude_pools.txt
./sol2/ingest/target/release/riwayat /root/sol/raw_1000 $((NOW-17*3600)) $((NOW-6*3600))
echo TARIK_1000_DONE
