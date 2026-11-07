# Status proyek memecoin Solana (dibaca Claude Code setiap sesi)

Pemilik: non-teknis, bahasa Indonesia, minta jawaban SINGKAT. Semua kerja dijalankan DI VPS INI, bukan di PC pemilik.
Aturan mengikat: `CLAUDE-CODE-INSTRUCTIONS.md` (folder ini). Bagian Solana = Part 14.

## Keputusan pemilik (6 Nov 2026)
- Fokus **Solana**, Robinhood dihentikan. Logika trading tetap sama (¼ Kelly + compounding, gerbang, rem, kill switch, ML+DL).
- Pakai **Rust + C++ (CatBoost)** saja. **Jangan Python** (juga untuk analisa).
- Tanpa TP1 dan tanpa trailing: keluar saat momentum melemah, tekanan beli turun, jual besar-besaran, atau dev jual.
- Honeypot: JANGAN mewajibkan "sudah ada penjual". Filter koin: dev belum jual, tanpa bundle, likuiditas kurva >= 2 SOL, quote SOL.
- Tidak sniping: keputusan paling cepat menit ke-2 umur koin.
- Semua fitur diuji satu per satu (AUC sendirian + uji buang-satu) sebelum digabung.
- Anti overfitting / look-ahead / bug: aturan ditetapkan di muka, walk-forward per hari, tes kausal wajib lulus.

## Komponen di VPS (`/root/sol`)
| Komponen | Lokasi | Keterangan |
|---|---|---|
| Pengumpul live | `kumpul/` -> `target/release/sol-kumpul`, layanan systemd `sol-kumpul` | WebSocket `logsSubscribe` Pump.fun + PumpSwap, dekoder IDL resmi (`src/dekode.rs`), isi celah lewat Helius, data `/root/sol/data/pump_<jam>.jsonl` (gzip otomatis via `/etc/cron.d/sol-gzip`). JANGAN restart sembarangan (tiap restart = celah). |
| Penarik riwayat | `kumpul/src/bin/tarik.rs` -> `target/release/tarik` | Helius `getTransactionsForAddress`: daftar koin dari mint authority `TSLvdd1...`, saring >= 20 tx di 2 menit pertama (env `MIN_TX_2M`), lalu riwayat kurva + PumpSwap 8 jam. Skrip `jalan2.sh`, data `/root/sol/hist2/hist_<tanggal>.jsonl`, log `hist2/jalan.log`. |
| Backtest | `uji/` -> `target/release/sol-uji <file...>` | V0 dasar, V1 aturan gelombang, V2 ML+DL walk-forward (CatBoost C++ `/opt/rhc/bin/catboost` + MLP Rust), biaya x2, uji syarat buang-satu, AUC per fitur, uji buang-satu fitur ML. `cargo test --release` = 4 tes kausal. |
| Kunci | `/root/sol/.rpc.env` (chmod 600) | `SOL_WS` (publik, Helius cadangan), `SOL_RPC` (Helius), `SOL_RPC_GATEKEEPER`. Token Bitquery `/root/sol/.bitquery_token` (akun belum aktif: "No active billing period"). Jangan pernah tampilkan isi kunci. |

Biaya Helius: `getTransactionsForAddress` 10 kredit per 100 tx (full), 10 kredit flat (signatures). Paket gratis 1 juta kredit/bulan.

## Temuan penting
- IDL: Pump.fun sekarang punya koin quote non-SOL (vsol = 0, harga di field quote) dan koin "mayhem" (vsol-rsol != 30). Backtest memakai koin quote SOL saja.
- Solana punya transaksi versi 1: `getTransaction` wajib `maxSupportedTransactionVersion: 1`.
- Uji awal (201 koin, 2 Nov + sebagian 3 Nov): terlalu kecil untuk kesimpulan (ML diuji 47 baris). Bug ukuran minimum (0,005 SOL > 2% x $30) sudah diperbaiki (0,002 SOL).

## Berikutnya
1. Tunggu `hist2/jalan.log` -> `SEMUA_SELESAI`, lalu `uji/target/release/sol-uji /root/sol/hist2/hist_*.jsonl`.
2. Laporkan ke pemilik: win rate, $30 -> ?, AUC ML/DL per hari, fitur jelek (uji buang-satu), syarat jelek. Semua varian dilaporkan.
3. Tambah data live (`/root/sol/data`) untuk hari-hari berikutnya.

- Tugas riset fitur: CLAUDE-CODE-FEATURE-RESEARCH.md (dipasang 7 Nov 2026, belum dikerjakan; tunggu hist2 selesai)
