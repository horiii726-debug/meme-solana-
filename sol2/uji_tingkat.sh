#!/bin/bash
# tiered targets: wait for the running y2x/rug5 tests, rebuild the dump with all labels, then test each tier
cd /root/sol
while pgrep -x permutasi >/dev/null; do sleep 30; done
D=reports/rebuild/explore/dump1000t.jsonl
LIQ_MIN=1 ./sol2/dekode/target/release/tambang1000 $D raw/*/*.zst raw_hist_test2/*/*.zst raw_gtfa_check/*/*.zst raw_1000/*/*.zst raw_more/*/*.zst
for L in y3x y6x y11x y31x; do
  ./sol2/dekode/target/release/permutasi $D 1000000 10000 none $L > reports/rebuild/explore/perm1000_$L.jsonl 2>&1 &
done
wait
jq -r "[.tau, .y2x, .y3x, .y6x, .y11x, .y31x] | @tsv" $D | awk "{n[\$1]++; for(i=2;i<=6;i++) if(\$i==\"true\") c[\$1\" \"i]++} END {for (t in n) print \"tau\", t, \"n\", n[t], \"2x\", c[t\" 2\"]+0, \"3x\", c[t\" 3\"]+0, \"6x\", c[t\" 4\"]+0, \"11x\", c[t\" 5\"]+0, \"31x\", c[t\" 6\"]+0}" > reports/rebuild/explore/target_counts.txt
echo TINGKAT_DONE
