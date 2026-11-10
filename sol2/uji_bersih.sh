#!/bin/bash
# Clean re-run after the 8 fixes (no look-ahead, no post-rug bug, causal universe, purge, MC excluded):
# features -> wallet features -> system without / with wallet features. Results in reports/rebuild/explore/bersih_*.json
cd /root/sol
O=reports/rebuild/explore
RAW=$(ls raw/*/*.zst raw_hist_test2/*/*.zst raw_gtfa_check/*/*.zst raw_1000/*/*.zst raw_more/*/*.zst 2>/dev/null)
LIQ_MIN=1 ./sol2/dekode/target/release/tambang1000 $O/dump_bersih.jsonl $RAW 2> $O/bersih_tambang.txt
./sol2/dekode/target/release/fitur_dompet $O/dump_bersih.jsonl early_buyers.tsv dompet.jsonl $O/dump_bersih_w.jsonl 2> $O/bersih_dompet.txt
TAU=15 ./sol2/dekode/target/release/sistem2 $O/dump_bersih.jsonl /tmp/bersih_nw $RAW > $O/bersih_tanpa_wallet.json 2>/dev/null &
TAU=15 ./sol2/dekode/target/release/sistem2 $O/dump_bersih_w.jsonl /tmp/bersih_w $RAW > $O/bersih_dengan_wallet.json 2>/dev/null &
wait
echo BERSIH_DONE
