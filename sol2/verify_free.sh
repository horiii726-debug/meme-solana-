#!/bin/bash
# Re-pull the 21 live-recorded pools from the FREE endpoint and compare with the paid live data. No paid calls.
cd /root/sol

export RPC_URLS="https://solana-rpc.publicnode.com|60|40"
zstdcat raw/*/*.zst | jq -r "select(.kind==\"pool_new\" and .tracked==true)|.venue+\" \"+.pool" | sort -u > /root/sol/raw_verify_pools.txt
while read v p; do SAMPLE_EVERY=20 ./sol2/ingest/target/release/tarik /root/sol/raw_verify $v $p 12 2>&1 | tail -1; done < /root/sol/raw_verify_pools.txt
mkdir -p raw_live_only && for d in pumpswap meteora_damm_v2; do mkdir -p raw_live_only/$d; ln -sf /root/sol/raw/$d/2026-11-09.jsonl.zst raw_live_only/$d/; done
./sol2/dekode/target/release/banding raw_live_only raw_verify > reports/rebuild/C2/live_vs_free.jsonl
echo VERIFY_DONE
