#!/bin/bash
# post-graduation pull: every tx touching each graduated DBC mint, birth .. birth + 2 h (DBC + DAMM v2 trades)
set -a; . /root/sol/.rpc.env; set +a
cd /root/sol/hist_dex
while IFS=$'\t' read m b; do
  /root/sol/kumpul/target/release/tarik_dex lulus_2026-11-02.jsonl $m $b $((b + 7200)) 30 2>&1 | tail -1
done < lulus_600.tsv
echo DAMM_SELESAI
