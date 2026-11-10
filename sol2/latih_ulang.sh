#!/bin/bash
# Automatic retraining loop (read-only research; no trading). Every 3 hours on ALL data collected so far:
#   1. token supplies for new mints (free official RPC)          -> supply.tsv (market cap feature)
#   2. tambang1000: ~874 causal data-mined features + targets     -> dump1000t.jsonl
#   3. permutasi: 1,000,000 permutations per feature (y2x, rug5)  -> perm1000_*.jsonl
#   4. sistem2 (entry 15 s): CatBoost P(2x) x (1-P(rug)), nested walk-forward tuning of selection + TP plan,
#      execution engine (verified pool math, latency, real fees, real failure rates), 1e9-trade Monte Carlo
#   5. one summary line per run appended to reports/rebuild/explore/latih_ulang.log
cd /root/sol
OUT=reports/rebuild/explore
while true; do
  RAW=$(ls raw/*/*.zst raw_hist_test2/*/*.zst raw_gtfa_check/*/*.zst raw_1000/*/*.zst raw_more/*/*.zst 2>/dev/null)
  zstdcat $RAW 2>/dev/null | jq -r 'select(.kind=="tx" and .failed==false)|.tx.meta.postTokenBalances[]?.mint' | grep -v So11111111111111111111111111111111111111112 | sort -u > /tmp/mints_all.txt
  RPC_URLS="https://api.mainnet-beta.solana.com|150|100000" nice -n 10 ./sol2/ingest/target/release/suplai /tmp/mints_all.txt /root/sol/supply.tsv 2>&1 | tail -1
  LIQ_MIN=1 nice -n 5 ./sol2/dekode/target/release/tambang1000 $OUT/dump1000t.jsonl $RAW 2> /tmp/tb.err
  ./sol2/dekode/target/release/permutasi $OUT/dump1000t.jsonl 1000000 10000 none y2x > $OUT/perm1000_y2x.jsonl 2>/dev/null
  ./sol2/dekode/target/release/permutasi $OUT/dump1000t.jsonl 1000000 10000 none rug5 > $OUT/perm1000_rug5.jsonl 2>/dev/null
  TAU=15 ./sol2/dekode/target/release/sistem2 $OUT/dump1000t.jsonl /tmp/sys2_auto $RAW > $OUT/sistem2_auto.json 2>/dev/null
  {
    echo "== $(date -u '+%F %H:%M UTC') | $(tail -1 /tmp/tb.err)"
    echo "features passing Holm: y2x $(jq -r 'select(.feature and .holm_p<0.05)|.feature' $OUT/perm1000_y2x.jsonl | wc -l), rug5 $(jq -r 'select(.feature and .holm_p<0.05)|.feature' $OUT/perm1000_rug5.jsonl | wc -l)"
    jq -c '{auc2x:.auc_oos_p2x, aucrug:.auc_oos_prug5, radar:.RADAR, system:.SYSTEM_oos, buy_all:.BUY_ALL_default_plan_oos, mc_wo3:.MONTECARLO_system_without_top3.equity_after_100_trades}' $OUT/sistem2_auto.json
  } >> $OUT/latih_ulang.log
  sleep 60
done
