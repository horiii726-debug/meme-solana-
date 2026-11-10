#!/bin/bash
# Do the sniper_v4 features help? 1) per-feature permutation test (y2x, rug5); 2) full system with vs without them.
cd /root/sol
O=reports/rebuild/explore
RAW=$(ls raw/*/*.zst raw_hist_test2/*/*.zst raw_gtfa_check/*/*.zst raw_1000/*/*.zst raw_more/*/*.zst 2>/dev/null)
LIQ_MIN=1 ./sol2/dekode/target/release/tambang1000 $O/dump_v4.jsonl $RAW 2> $O/v4_tambang.txt
./sol2/dekode/target/release/permutasi $O/dump_v4.jsonl 1000000 10000 none y2x > $O/perm_v4_y2x.jsonl 2>/dev/null &
./sol2/dekode/target/release/permutasi $O/dump_v4.jsonl 1000000 10000 none rug5 > $O/perm_v4_rug5.jsonl 2>/dev/null &
TAU=15 ./sol2/dekode/target/release/sistem2 $O/dump_v4.jsonl /tmp/v4_with $RAW > $O/v4_dengan.json 2>/dev/null &
TAU=15 EXCLUDE_PREFIX=v4_ ./sol2/dekode/target/release/sistem2 $O/dump_v4.jsonl /tmp/v4_without $RAW > $O/v4_tanpa.json 2>/dev/null &
wait
echo UJI_V4_DONE
