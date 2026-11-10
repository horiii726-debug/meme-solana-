#!/bin/bash
cd /root/sol
D=reports/rebuild/explore/dump1000.jsonl
LIQ_MIN=1 ./sol2/dekode/target/release/tambang1000 $D raw/*/*.zst raw_hist_test2/*/*.zst raw_gtfa_check/*/*.zst raw_1000/*/*.zst raw_more/*/*.zst
./sol2/dekode/target/release/permutasi $D 1000000 10000 none y2x > reports/rebuild/explore/perm1000_y2x.jsonl 2>&1 &
./sol2/dekode/target/release/permutasi $D 1000000 10000 none rug5 > reports/rebuild/explore/perm1000_rug5.jsonl 2>&1 &
wait
echo UJI1000_DONE
