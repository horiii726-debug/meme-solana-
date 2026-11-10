#!/bin/bash
# 9 walk-forward CatBoost runs in parallel: y2x on GPU, profit labels on CPU (all cores)
cd /root/sol
D=reports/rebuild/explore/tambang_dump2.jsonl
O=/root/sol/reports/rebuild/explore/catboost
mkdir -p "$O"
for T in 5 10 15; do
  ./sol2/dekode/target/release/latih $D /tmp/cbw_${T}_y2x $T y2x gpu > "$O/t${T}_y2x.jsonl" 2>&1 &
  ./sol2/dekode/target/release/latih $D /tmp/cbw_${T}_w120 $T win:time120s cpu > "$O/t${T}_w120.jsonl" 2>&1 &
  ./sol2/dekode/target/release/latih $D /tmp/cbw_${T}_a150 $T win:age150s cpu > "$O/t${T}_a150.jsonl" 2>&1 &
done
wait
echo LATIH_DONE
