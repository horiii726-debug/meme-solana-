#!/bin/bash
# penarikan v2: 4 hari x 4 jendela x 1000 koin diperiksa, hanya koin aktif (>=20 tx di 2 menit pertama) ditarik penuh
set -a; . /root/sol/.rpc.env; set +a
cd /root/sol/hist2
for d in 2026-11-02 2026-11-03 2026-11-04 2026-11-05; do
  for h in 03 09 15 21; do
    T=$(date -u -d "${d}T${h}:00:00Z" +%s)
    echo "== $d $h:00 ($T)"
    /root/sol/kumpul/target/release/tarik hist_${d}.jsonl $T 1000 8 6000 2>&1 | tail -2
  done
done
echo SEMUA_SELESAI
