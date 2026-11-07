# Log eksperimen riset fitur (CLAUDE-CODE-FEATURE-RESEARCH.md)

Semua angka dari `uji/target/release/sol-uji` di data nyata (lihat perintah di bawah). Biaya: fee dari event on-chain
per trade + 0,0002 SOL per transaksi (≈ p90 dari 60 transaksi Pump.fun nyata; median 0,000026 SOL), dampak harga
order sendiri lewat rumus kurva. Keputusan hanya memakai data ≤ t (6 tes kausal lulus).

## Data
| Pasar | File | Koin | Trade |
|---|---|---|---|
| Pump.fun | hist2/hist_2026-11-02.jsonl (sebagian), hist/hist_2026-11-0{2,3}.jsonl | 3.190 (1.414 quote SOL lengkap aman) | ~1,4 juta event |
| Meteora DBC + DAMM v2 | hist_dex/dbc_2026-11-02.jsonl, hist_dex/lulus_w0*.jsonl | 1.341 (600 diikuti setelah lulus) | 375.339 + 516.657 |
| Raydium LaunchLab | hist_dex/launchlab_2026-11-02.jsonl | 1.291 | 172.598 |
| Keamanan mint | data_aman.jsonl, data_aman_ll.jsonl | 4.332 dicek | 16 hook + 229 fee transfer dibuang |

Perintah utama:
`LP_CAP=0 LIPAT_JAM=6 ML_MIN_RIIL=2 uji/target/release/sol-uji <file...> data_aman*.jsonl`

## Jumlah eksperimen (6 Nov 2026)
- Fitur diuji satu per satu: 22 lama + 54 kandidat = **76** (uji sendiri + tambah-ke-model, per lipatan 6 jam).
- Aturan keluar: K0–K8 (9) + pegang tetap 30–3600 dtk (10 nilai).
- Penyetelan jujur (nested walk-forward): 975–4.608 kombinasi per pasar per run.
- Penambangan pola: ±15.000 pola (1–2 fitur, top/bottom 20%) × 15 cara keluar per lipatan.
- Varian sistem: V0, V1, V2, V2b, V3, V3b, V3c, V3d, V4 (+ biaya ×2).
- Set fitur: FITUR_PLUS, FITUR_V2, FITUR_V3 (dikunci sebelum diuji ulang).

## Hasil kunci (luar sampel = dipilih di masa lalu, diuji di periode berikutnya)
| Pasar | Terbaik luar sampel | Catatan |
|---|---|---|
| Pump.fun | −10,9% s/d −12,0% per trade | semua varian rugi |
| LaunchLab | +0,7% per trade (n=41) | impas; fee 1,75%/sisi |
| DBC+DAMM | −19,1% per trade (n=51) | hasil positif sebelumnya = bias data berhenti saat lulus; setelah lulus sering rug |
| Pola tambang | Pump −42,8% (n=20); LaunchLab/DBC 0 trade | pola masa lalu tidak berulang (overfit) |

## Aturan sistem (R1–R8, Pump.fun 2 Nov, n per baris di output)
- R1 umur keputusan: 30 dtk ≥2x 18,6% crash 35,5%; 120 dtk 11,4%/19,1%; 300 dtk 7,7%/16,4%.
- R3 syarat penjual menit-1: tanpa penjual hanya 17 koin (terlalu sedikit); 445 dari 11.223 penjual menit-1 = creator.
- R4 creator: 164 dari 671 creator membuat ≥2 koin; setelah koin sebelumnya crash → crash 80% (n=10, terlalu sedikit).
- R6 sinyal sebelum turun >30% dalam 5 menit: top-3 holder jual 32% (n=447), creator jual 26%, jual tunggal ≥5% 26%,
  jual>beli 18%, tanpa pembeli 1%, dasar 3%.
- R7 fitur menit-1 → beli di menit 3: rasio_bs 0,64, top 0,64, bundle 0,35 (AUC); n=565.
- R8 dampak harga+fee di menit 3: 0,01 SOL 1,3%; 0,1 SOL 1,6%; 0,5 SOL 2,9%; 1 SOL 4,6%.
- Keluar baru K7 (top-3 holder jual): Pump −11,0%/trade, paling stabil antar lipatan (−10,3% s/d −11,3%), tetap rugi.

## Fitur yang lolos SOP (≥ 2/3 lipatan, uji sendiri + tambah-ke-model) — belum memberi hasil positif
Pump: buyvol_accel, logret30, holders, since_big_sell, trade_gap_med60, same_slot_share60, whale_buy_share60, buy_p90_60,
trend_r2_300, top10_flow60, max_buys_slot60, copycat, global_launch10m.
LaunchLab: holders_in_profit (3/3), supply_in_profit, local_peaks300 (3/3), flip300 (3/3), trade_gap_med60 (3/3).
Ditolak/ragu: OFI 30/60, dist_peak, sol_per_trade, direct_buy_share60, repeat_buyer_share60, sniper_hold_pct, smart wallets
(sederhana & t-stat), creator_prev_coins, bump_share300, wash_pairs300.

## Holdout
Belum dibuka. Holdout terkunci: Pump.fun 3 Nov jendela 09/15/21 + 4–5 Nov (masih ditarik).

## Vonis sementara
Belum ada edge setelah biaya di 3 pasar, data 1–2 hari. Bukan siap live.

## Tinjauan independen (skill doubt-driven-development) dan perbaikan, 6 Nov 2026 malam
Peninjau baru (tanpa konteks) menemukan 10 masalah. Diperbaiki: K7/K8 punya batas 60 dtk tersembunyi; fill di belakang
trade agen mayhem dapat fee 0 (kini 1,25%); trade tanpa event lanjutan dibuang (kini rugi pulang-pergi); purge penyetelan &
penambangan memakai waktu masuk (kini waktu keluar); pengaman likuiditas DBC/DAMM bisa terkunci (kini dibanding nilai mentah).
Dicatat (belum diperbaiki, sebagian besar membuat hasil TERLALU BAGUS): koin Pump.fun lulus dinilai di puncak kurva (tidak ada
data PumpSwap di hist2), cadangan PumpSwap adalah sebelum trade, biaya/latensi optimis, koin terpotong dibuang memakai info
masa depan, keluar sinyal diisi di trade berikutnya (terlalu jelek), baris 30 dtk tumpang tindih membesarkan n.

## Hasil setelah perbaikan
- Pump.fun: penyetelan jujur −14,2%/trade (n=46); pola tambang −39,6% (n=22); semua keluar K0–K8 rugi (terbaik K5 −11,2%).
- DBC+DAMM: penyetelan jujur −15,6%/trade (n=69); semua keluar rugi.
- LaunchLab: K7 (keluar hanya bila dev / top-3 holder jual) +5,8%/trade, PF 1,47, median −8,6%; per lipatan +2,4% / −16,5%
  / −9,4% / +27,0% → positif hanya 2 dari 4 lipatan, 82 keluar "data habis" (bisa bias lulus). Penyetelan jujur memilih K7
  di semua lipatan: +32,6% (n=77 baris tumpang tindih; 67 dari satu lipatan). STATUS: PETUNJUK, BELUM TERBUKTI.

## PRA-REGISTRASI uji terkunci (ditulis sebelum membuka LaunchLab 3 Nov)
Entry: SOL kurva >= 5, pembeli 60 dtk >= 5, rasio SOL beli:jual >= 2, dev belum jual, tanpa bundle, ML mati.
Keluar: K7 (dev / top-3 holder jual; batas 6 jam). Data: hist_dex/launchlab_2026-11-03.jsonl + cek keamanan. Dibuka SEKALI.
Lulus bila: rata per trade > 0 setelah biaya di sebagian besar lipatan 6 jam, PF > 1, dan tetap tidak rugi dengan biaya x2.

## 6 Nov malam: data lama + hari baru gratis (live 6 Nov 14:00–19:30, sol-kumpul, termasuk PumpSwap setelah lulus)
Kuota Helius habis ("max usage reached") → uji terkunci K7 LaunchLab 3 Nov TERTUNDA (pra-registrasi tetap berlaku).
8 fitur baru: dev_init_buy, dev_sold_frac, early_exit_share, retention_m1, seller_hold_med60, impact_asym60,
ofi_autocorr120, buy_streak60 (total kandidat 62 + 22 lama = 84).
Fitur lolos SOP DAN arah sama di dua hari Pump (2 Nov tanpa mayhem, 6 Nov live): vol60 (0,53 / 0,74), buy_p90_60
(0,52 / 0,73), top10_flow60 (0,50 / 0,61). Arah berbalik antar hari (tidak stabil): logret300, hhi_buy60, time_to_10_buyers.
Kuat di 6 Nov saja (1 lipatan, perlu hari lain): sniper_hold_pct 0,215, early10_hold_pct 0,211, since_big_sell 0,244,
fresh_sol_share60 0,714, median_buy60 0,748, early_exit_share 0,642, ofi_autocorr120 0,610.
Hari 6 Nov (dengan harga PumpSwap setelah lulus): K0 −39,5%/trade, K7 −52,5%, K3 −8,0%, K5 −8,1% → keluar lama
sangat rugi karena koin lulus dibuang di PumpSwap; ini membenarkan temuan tinjauan #1 (data riwayat 2 Nov terlalu optimis).

## 6–7 Nov: perbaikan dari tinjauan ahli + strategi dari referensi (jeda 3 dtk, tanpa mayhem, aman, LP_CAP=0)
Perbaikan: jeda isi order bisa diatur (JEDA_DTK; jeda websocket terukur median 1,3 dtk, p90 1,7 dtk); isi order = keadaan
kurva saat order mendarat (tidak menunggu trade berikut, event detik yang sama tidak terlewat); keluar "data habis
(tertutup)" dilaporkan terpisah; NOL_BIAYA, UKURAN_SOL, MODAL_SOL; interval keyakinan 95% bootstrap + hari untung.
Sistem ML+DL + 15 fitur (V2b/V3d), $30: Pump 2 Nov $29,07–29,12; Pump 6 Nov 1–2 trade; LaunchLab $28,81–29,23;
DBC+DAMM $28,41–29,33. Semua rugi atau terlalu sedikit trade.
Strategi referensi (aturan apa adanya): Marino (kurva >= 40 SOL & <= 400 trade), Luo (dompet pintar t-stat ikut beli),
Factories (koin asli + kurva >= 20), TSMOM (return 5 mnt & 30 dtk > 0); keluar 60 dtk dan pegang 10 mnt; 4 pasar.
Hasil: hampir semua rugi. Satu-satunya positif: Luo di DBC+DAMM, pegang 10 mnt: 74 trade, win 33,8%, PF 2,43,
$30 -> $34,74, CI 95% per trade [+0,1%, +8,2%], TETAPI seluruh untung dari 38 keluar "data habis (tertutup)" (rata 1,13x,
koin lulus yang data setelah lulusnya tidak ditarik; dinilai di harga terakhir DBC = terlalu optimis); keluar normal
rugi (0,89–0,94x). Vonis: bias, bukan edge. Perlu data DAMM untuk semua koin lulus sebelum dinilai.

## 6 Nov malam: model "pemburu roket" (LABEL_ROKET=5, UMUR_MULAI 30/60, CELAH_TERISI=1, live 6 Nov, 2.934 koin)
Label: puncak >= 5x harga isi dalam 30 mnt. Basis 0,8–1,2% titik. Top 5% skor model: 2–18% roket (lift 8–15x),
AUC 0,73–0,98 (>0,9 = curiga; kelas sangat jarang). Portofolio $30: K7 $28,46–28,88, K5 $28,70–29,06 (semua rugi);
keluar rata 0,80–0,84x: lonjakan 5x sesaat tidak tertangkap oleh keluar tanpa TP. Menunggu izin pemilik untuk uji TP penuh.
Juga: disk sempat penuh oleh file sementara CatBoost (/tmp/sol_*, 2,26 GB) -> dihapus; ml::catboost kini membersihkan diri.
Loop verifikasi (uji/loop_verifikasi.sh, kandidat di KANDIDAT-BEKU.md) siap; cron perlu dipasang pemilik.

## Keluar momentum (permintaan pemilik): K9 = SOL jual >= R x beli, K10 = jumlah jual >= R x beli (live 6 Nov, model roket, umur 30 dtk)
Jendela 30 dtk: terbaik K9 4:1 screening $27,66 (win 14%, PF 0,37). Jendela pendek (JENDELA_KELUAR):
10 dtk 4:1 screening $28,94 (16 trade, win 31,2%, PF 0,51, impas 46,9%); 10 dtk 2:1 $28,24; 15 dtk 4:1 $28,17; 15 dtk 2:1 $28,36.
Semua rugi; jendela 10 dtk menggandakan win rate (14% -> 31%) tapi masih di bawah impas.
