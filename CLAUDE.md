# Aturan proyek (WAJIB dibaca setiap sesi)

Baca @STATUS-SOLANA.md dan @CLAUDE-CODE-INSTRUCTIONS.md sebelum melakukan apa pun. File itu mengikat.

Ringkas: pemilik non-teknis, bahasa Indonesia, jawaban singkat. Fokus Solana. Rust + C++ saja, jangan Python. Jangan ubah sistem trading tanpa izin. Jangan pernah EXEC_MODE=live, jangan sentuh private key, jangan tampilkan kunci API. Semua angka harus dari perintah yang dijalankan di data nyata. Jangan push ke GitHub tanpa izin.

Baca juga @CLAUDE-CODE-FEATURE-RESEARCH.md (tugas riset fitur & kelemahan sistem).
Catatan penyesuaian di VPS: data ada di /root/sol/data (bukan C:\...); pengumpul = Rust (sol-kumpul), BUKAN skrip Python di file itu;
analisa pakai Rust/C++ saja (aturan pemilik: tanpa Python); backtest = uji/target/release/sol-uji.
