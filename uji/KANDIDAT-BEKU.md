# Kandidat beku untuk loop verifikasi (dibekukan 6 Nov 2026 21:15 UTC, unix 1793999747)

Biner beku: `uji/beku/sol-uji-beku-1793999747` (kode tidak boleh diubah untuk kandidat ini).
Data uji: HANYA koin Pump.fun/PumpSwap yang lahir SETELAH waktu beku (`LAPOR_DARI=1793999747`), dari pengumpul live
`sol-kumpul` (`/root/sol/data/pump_*.jsonl*`). Data ini belum pernah dilihat saat kandidat dipilih.
Setelan umum: LP_CAP=0, TANPA_MAYHEM=1, JEDA_DTK=3, ML_MIN_RIIL=2, FITUR_V2=1, FITUR_V4=1, LIPAT_JAM=6, modal $30,
1/4 Kelly + compounding (aturan pemilik), tanpa TP.

| ID | Kandidat | Baris laporan |
|---|---|---|
| C1 | ML+DL + 15 fitur, radar data (V3d), keluar K3 (tekanan beli < 1:1) | `### V3d` di run KELUAR=3 |
| C2 | ML+DL + 15 fitur, top 10% (V2b), keluar K5 (60 dtk) | `### V2b ML` di run KELUAR=5 |
| C3 | Marino: kurva >= 40 SOL & <= 400 trade, keluar 60 dtk | `### REF Marino ... keluar 60 dtk` |
| C4 | Factories: koin asli + kurva >= 20 SOL, keluar 60 dtk | `### REF Factories ... kelu` |
| C5 | TSMOM: return 5 mnt & 30 dtk > 0, keluar 60 dtk | `### REF TSMOM ... keluar 60 dtk` |

## Aturan LULUS (semua wajib, ditetapkan di muka)
1. >= 200 trade (1 per koin) setelah waktu beku.
2. Interval keyakinan 95% rata per trade seluruhnya > 0.
3. Untung di >= 8 dari 10 hari terakhir (per hari UTC).
4. Tetap tidak rugi dengan biaya x2.
5. Tetap untung tanpa 5 trade terbaik.
6. Win rate di atas win rate impas.
Aturan BERHENTI: jika setelah 20 hari tidak ada kandidat lulus -> vonis "tidak ada edge" untuk kelas strategi ini.
Lulus loop ini BUKAN izin live: tetap perlu mode bayangan >= 50 trade dan keputusan pemilik.
