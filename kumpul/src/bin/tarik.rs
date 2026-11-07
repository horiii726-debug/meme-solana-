//! Penarik riwayat Pump.fun (hanya baca) lewat Helius `getTransactionsForAddress`, format keluaran = `sol-kumpul`.
//!   sol-tarik <keluar.jsonl> <waktu_akhir_unix> <jumlah_koin> [jam_horizon=8] [maks_panggilan=4000]
//! 1. koin baru: transaksi alamat mint authority Pump.fun (hanya muncul di pembuatan koin), mundur dari waktu_akhir;
//! 2. riwayat tiap koin: transaksi alamat bonding curve-nya, lahir .. lahir + horizon (maks 20 halaman = 2000 tx);
//! 3. koin yang lulus: transaksi pool PumpSwap-nya sampai lahir + horizon (maks 20 halaman).
//! Baris diberi "n" (urutan dalam riwayat) supaya urutan dalam satu slot tetap; "t" = blockTime (detik).
//! RPC dari SOL_RPC (berkas .rpc.env). Batas panggilan melindungi kuota kredit.
#[path = "../dekode.rs"]
#[allow(dead_code, unused_imports)]
mod dekode;
use base64::Engine;
use dekode::*;
use std::io::Write;
use std::time::Duration;

const MINT_AUTH: &str = "TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM";
const HALAMAN_MAKS: usize = 50;

struct Tarik {
    rpc: String,
    panggil: usize,
    maks: usize,
    n: u64,
    out: std::fs::File,
}

impl Tarik {
    fn gtfa(&mut self, addr: &str, asc: bool, gte: Option<i64>, lte: Option<i64>, token: Option<&str>) -> Option<serde_json::Value> {
        self.gtfa_x(addr, asc, gte, lte, token, "full")
    }
    /// detail "full" (10 kredit per 100 tx) atau "signatures" (10 kredit flat); maks 1000 per permintaan
    fn gtfa_x(&mut self, addr: &str, asc: bool, gte: Option<i64>, lte: Option<i64>, token: Option<&str>, detail: &str) -> Option<serde_json::Value> {
        if self.panggil >= self.maks {
            return None;
        }
        let mut f = serde_json::Map::new();
        let mut bt = serde_json::Map::new();
        if let Some(x) = gte {
            bt.insert("gte".into(), x.into());
        }
        if let Some(x) = lte {
            bt.insert("lte".into(), x.into());
        }
        f.insert("blockTime".into(), bt.into());
        let mut o = serde_json::json!({"transactionDetails": detail, "limit": if detail == "full" { 100 } else { 1000 }, "sortOrder": if asc { "asc" } else { "desc" },
            "encoding": "json", "maxSupportedTransactionVersion": 1, "filters": f, "commitment": "confirmed"});
        if let Some(t) = token {
            o["paginationToken"] = t.into();
        }
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getTransactionsForAddress", "params": [addr, o]});
        for k in 0..5u32 {
            self.panggil += 1;
            let r = ureq::post(&self.rpc).timeout(Duration::from_secs(60)).send_json(body.clone()).ok().and_then(|x| x.into_json::<serde_json::Value>().ok());
            std::thread::sleep(Duration::from_millis(130));
            match r {
                Some(r) if r.get("result").is_some() => return Some(r["result"].clone()),
                Some(r) => eprintln!("rpc error: {}", r["error"].to_string().chars().take(160).collect::<String>()),
                None => eprintln!("rpc gagal (percobaan {k})"),
            }
            std::thread::sleep(Duration::from_secs(1 << k));
        }
        None
    }
    /// event dari satu transaksi; `saring` memilih event yang ditulis
    fn event(&mut self, tx: &serde_json::Value, saring: &dyn Fn(&serde_json::Value) -> bool) -> Vec<serde_json::Value> {
        if !tx["meta"]["err"].is_null() {
            return vec![];
        }
        let (slot, bt) = (tx["slot"].as_u64().unwrap_or(0), tx["blockTime"].as_i64().unwrap_or(0));
        let sig = tx["transaction"]["signatures"][0].as_str().unwrap_or("").to_string();
        let mut hasil = vec![];
        for l in tx["meta"]["logMessages"].as_array().into_iter().flatten().filter_map(|l| l.as_str()) {
            let Some(d) = l.strip_prefix("Program data: ") else { continue };
            let Ok(b) = base64::engine::general_purpose::STANDARD.decode(d) else { continue };
            let Some(mut e) = dekode(&b) else { continue };
            if !saring(&e) {
                continue;
            }
            self.n += 1;
            e["t"] = (bt as f64).into();
            e["slot"] = slot.into();
            e["sig"] = sig.clone().into();
            e["n"] = self.n.into();
            hasil.push(e);
        }
        hasil
    }
    fn tulis(&mut self, v: &[serde_json::Value]) {
        for e in v {
            let _ = self.out.write_all(format!("{e}\n").as_bytes());
        }
    }
    /// semua halaman riwayat alamat (urut naik) antara gte..lte; kembali (transaksi, terpotong?)
    fn riwayat(&mut self, addr: &str, gte: i64, lte: i64) -> (Vec<serde_json::Value>, bool) {
        let (mut semua, mut token) = (Vec::new(), None::<String>);
        for _ in 0..HALAMAN_MAKS {
            let Some(r) = self.gtfa(addr, true, Some(gte), Some(lte), token.as_deref()) else { return (semua, true) };
            semua.extend(r["data"].as_array().cloned().unwrap_or_default());
            token = r["paginationToken"].as_str().map(|s| s.to_string());
            if token.is_none() {
                return (semua, false);
            }
        }
        (semua, true)
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("pakai: sol-tarik <keluar.jsonl> <waktu_akhir_unix> <jumlah_koin> [jam_horizon=8] [maks_panggilan=4000]");
        std::process::exit(2);
    }
    let akhir: i64 = a[2].parse().unwrap_or(0);
    let jumlah: usize = a[3].parse().unwrap_or(0);
    let horizon: i64 = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(8) * 3600;
    let maks: usize = a.get(5).and_then(|x| x.parse().ok()).unwrap_or(4000);
    let Ok(rpc) = std::env::var("SOL_RPC") else {
        eprintln!("ERROR: SOL_RPC belum diisi (berkas .rpc.env)");
        std::process::exit(2);
    };
    let out = match std::fs::OpenOptions::new().create(true).append(true).open(&a[1]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ERROR {}: {e}", a[1]);
            std::process::exit(2);
        }
    };
    let mut t = Tarik { rpc, panggil: 0, maks, n: 0, out };
    // 1. daftar koin (mundur dari waktu akhir)
    let mut koin: Vec<(String, String, i64)> = Vec::new(); // (mint, curve, lahir)
    let mut token: Option<String> = None;
    while koin.len() < jumlah {
        let Some(r) = t.gtfa(MINT_AUTH, false, None, Some(akhir), token.as_deref()) else { break };
        for tx in r["data"].as_array().cloned().unwrap_or_default() {
            let bt = tx["blockTime"].as_i64().unwrap_or(0);
            for e in t.event(&tx, &|e| e["k"] == "create") {
                if koin.len() < jumlah {
                    koin.push((e["mint"].as_str().unwrap_or("").into(), e["curve"].as_str().unwrap_or("").into(), bt));
                }
            }
        }
        token = r["paginationToken"].as_str().map(|s| s.to_string());
        if token.is_none() {
            break;
        }
    }
    eprintln!("koin: {} (panggilan {})", koin.len(), t.panggil);
    koin.reverse(); // urut lahir naik
    let (mut potong, mut lulus, mut aktif) = (0, 0, 0);
    let min_tx: usize = std::env::var("MIN_TX_2M").ok().and_then(|x| x.parse().ok()).unwrap_or(20);
    for (i, (mint, curve, lahir)) in koin.iter().enumerate() {
        // saring murah (10 kredit): jumlah transaksi kurva dalam 2 menit pertama. Informasi ini sudah ada di menit ke-2,
        // jadi strategi yang hanya memutuskan mulai menit ke-2 tidak melihat masa depan. Koin sepi tidak ditarik penuh.
        let n2 = t
            .gtfa_x(curve, true, Some(*lahir), Some(lahir + 120), None, "signatures")
            .and_then(|r| r["data"].as_array().map(|a| a.iter().filter(|x| x["err"].is_null()).count()))
            .unwrap_or(0);
        if n2 < min_tx {
            continue;
        }
        aktif += 1;
        // 2. riwayat kurva (termasuk transaksi pembuatan: event create + beli dev)
        let (txs, terpotong) = t.riwayat(curve, *lahir, lahir + horizon);
        let mut pool: Option<(String, i64)> = None;
        let m = mint.clone();
        for tx in &txs {
            let ev = t.event(tx, &|e| e["mint"].as_str() == Some(m.as_str()) && ["create", "trade", "complete", "migrasi"].contains(&e["k"].as_str().unwrap_or("")));
            for e in &ev {
                if e["k"] == "migrasi" {
                    pool = Some((e["pool"].as_str().unwrap_or("").into(), e["t"].as_f64().unwrap_or(0.0) as i64));
                }
            }
            t.tulis(&ev);
        }
        // 3. trade PumpSwap setelah lulus
        let mut terpotong2 = false;
        if let Some((p, tm)) = &pool {
            lulus += 1;
            let (txs, tp) = t.riwayat(p, *tm, lahir + horizon);
            terpotong2 = tp;
            let pp = p.clone();
            for tx in &txs {
                let mut ev = t.event(tx, &|e| e["pool"].as_str() == Some(pp.as_str()) && (e["k"] == "beli_amm" || e["k"] == "jual_amm"));
                for e in ev.iter_mut() {
                    e["mint"] = m.clone().into();
                }
                t.tulis(&ev);
            }
        }
        if terpotong || terpotong2 {
            potong += 1;
            t.tulis(&[serde_json::json!({"k": "terpotong", "mint": mint})]);
        }
        if (i + 1) % 50 == 0 {
            eprintln!("{} / {} koin | aktif {} | panggilan {} | lulus {} | terpotong {}", i + 1, koin.len(), aktif, t.panggil, lulus, potong);
        }
        if t.panggil >= t.maks {
            eprintln!("BATAS PANGGILAN tercapai di koin {}; berhenti", i + 1);
            t.tulis(&[serde_json::json!({"k": "berhenti", "koin_selesai": i + 1})]);
            break;
        }
    }
    t.tulis(&[serde_json::json!({"k": "saring", "diperiksa": koin.len(), "aktif": aktif, "min_tx_2m": min_tx})]);
    eprintln!("SELESAI: {} koin diperiksa, {} aktif, panggilan {}, lulus {}, terpotong {}", koin.len(), aktif, t.panggil, lulus, potong);
}
