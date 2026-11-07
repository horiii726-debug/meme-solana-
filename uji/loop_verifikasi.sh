#!/bin/bash
# Autonomous verification loop: frozen candidates on UNSEEN live data only. Read-only on market data; writes logs only.
cd /root/sol
B=/root/sol/uji/beku/sol-uji-beku-1793999747
LOG=/root/sol/uji/VERIFIKASI.log
TMP=$(mktemp -d)
E="LAPOR_DARI=1793999747 LP_CAP=0 TANPA_MAYHEM=1 JEDA_DTK=3 ML_MIN_RIIL=2 FITUR_V2=1 FITUR_V4=1 LIPAT_JAM=6"
env $E KELUAR=3 nice -n 10 $B data/pump_*.jsonl* > $TMP/k3.txt 2>&1
env $E KELUAR=5 nice -n 10 $B data/pump_*.jsonl* > $TMP/k5.txt 2>&1
{
  echo "===== $(date -u '+%Y-%m-%d %H:%M UTC') | data: $(ls data/pump_*.jsonl* | wc -l) file jam"
  for pat in "C1|k3|### V3d RADAR DATA" "C2|k5|### V2b ML" "C3|k5|### REF Marino.*keluar 60" "C4|k5|### REF Factories.*kelu" "C5|k5|### REF TSMOM.*keluar 60"; do
    id=${pat%%|*}; rest=${pat#*|}; f=${rest%%|*}; re=${rest#*|}
    awk -v id="$id" -v re="$re" '$0 ~ re {p=1; n=0; next} p {n++; if ($0 ~ /^trade|^modal|^per hari|^rata per trade/) print id": "$0; if (n>=7) p=0}' $TMP/$f.txt | grep -v "^$id: per hari" | cut -c1-260
  done
  awk '/BIAYA x2/ && /V2b/ {getline; getline; print "C2 biaya x2: "$0}' $TMP/k5.txt | cut -c1-200
} >> $LOG
rm -rf "$TMP"
# the frozen binary leaves CatBoost scratch dirs in /tmp: remove them (they filled the disk once)
find /tmp -maxdepth 1 -type d -name "sol_*" -mmin +1 -exec rm -rf {} +
