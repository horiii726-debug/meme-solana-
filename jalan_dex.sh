#!/bin/bash
# history pull for Meteora DBC + Raydium LaunchLab (full transactions), 2 Nov 2026 00:00 UTC .. 3 Nov 02:00 (2 h tail)
set -a; . /root/sol/.rpc.env; set +a
cd /root/sol/hist_dex
T0=$(date -u -d "2026-11-02T00:00:00Z" +%s); T1=$((T0 + 26*3600))
/root/sol/kumpul/target/release/tarik_dex dbc_2026-11-02.jsonl dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN $T0 $T1 4000 2>&1 | tail -3
/root/sol/kumpul/target/release/tarik_dex launchlab_2026-11-02.jsonl LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj $T0 $T1 2000 2>&1 | tail -3
echo DEX_SELESAI
