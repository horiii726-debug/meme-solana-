//! Pengumpul data Solana untuk riset (hanya baca: tanpa kunci, tanpa transaksi, tanpa API pihak ketiga).
//! Sumber: RPC Solana. Websocket `logsSubscribe` untuk Pump.fun dan PumpSwap (commitment `confirmed`).
//! Event Anchor didekode sendiri dari baris `Program data:`; tata letak dari IDL resmi
//! github.com/pump-fun/pump-public-docs (commit 639b87fb, 13 Okt 2026).
//! Keluaran <dir>/pump_<jam-sejak-epoch>.jsonl, satu baris per event:
//!   create / trade / complete / migrasi (Pump.fun), pool / beli_amm / jual_amm (PumpSwap),
//!   sambung (sambungan dibuka, dengan slot terakhir sebelum putus = celah), isi (hasil pengisian celah).
//! PumpSwap hanya dicatat untuk koin yang lahir selama pengumpulan (koin.txt); pemetaan pool -> koin (pool.txt)
//! dari event migrasi Pump.fun dan CreatePoolEvent PumpSwap.
use base64::Engine;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};
use tungstenite::{connect, Message};

mod dekode;
use dekode::*;

struct Keadaan {
    dir: String,
    rpc: String,
    koin: HashSet<String>,
    pool: HashMap<String, String>,
    lihat: HashSet<String>,
    urut: VecDeque<String>,
    n: HashMap<&'static str, u64>,
}

impl Keadaan {
    fn tulis(&self, v: &serde_json::Value) {
        let jam = (detik() / 3600.0) as u64;
        // satu write per baris (O_APPEND): thread pengisi celah boleh menulis ke berkas yang sama tanpa baris tercampur
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{}/pump_{jam}.jsonl", self.dir)) {
            let _ = f.write_all(format!("{v}
").as_bytes());
        }
    }
    fn simpan(&self, berkas: &str, baris: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{}/{berkas}", self.dir)) {
            let _ = writeln!(f, "{baris}");
        }
    }
    /// panggilan RPC dengan ulang (batas laju RPC publik: 429 / error): jeda 1, 2, 4, 8 detik
    fn rpc(&self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        for k in 0..5u32 {
            let r = ureq::post(&self.rpc).timeout(Duration::from_secs(20)).send_json(body.clone()).ok().and_then(|x| x.into_json::<serde_json::Value>().ok());
            if let Some(r) = r {
                if r.get("result").is_some() && !r["result"].is_null() {
                    return Some(r["result"].clone());
                }
            }
            std::thread::sleep(Duration::from_secs(1 << k));
        }
        None
    }
    /// satu transaksi (dari websocket atau pengisian celah): dekode semua event, tulis yang relevan
    fn proses(&mut self, sig: &str, slot: u64, logs: &[serde_json::Value], isi: bool) {
        if sig.is_empty() || !self.lihat.insert(sig.to_string()) {
            return;
        }
        self.urut.push_back(sig.to_string());
        if self.urut.len() > 300_000 {
            if let Some(x) = self.urut.pop_front() {
                self.lihat.remove(&x);
            }
        }
        let t = detik();
        for l in logs.iter().filter_map(|l| l.as_str()) {
            let Some(d) = l.strip_prefix("Program data: ") else { continue };
            let Ok(b) = base64::engine::general_purpose::STANDARD.decode(d) else { continue };
            let Some(mut e) = dekode(&b) else { continue };
            let k = match e["k"].as_str() {
                Some("trade") => "trade",
                Some("create") => "create",
                Some("complete") => "complete",
                Some("migrasi") => "migrasi",
                Some("pool") => "pool",
                Some("beli_amm") => "amm",
                Some("jual_amm") => "amm",
                _ => continue,
            };
            match k {
                "create" => {
                    let m = e["mint"].as_str().unwrap_or("").to_string();
                    if self.koin.insert(m.clone()) {
                        self.simpan("koin.txt", &m);
                    }
                }
                "migrasi" | "pool" => {
                    let (p, m) = (e["pool"].as_str().unwrap_or("").to_string(), e["mint"].as_str().unwrap_or("").to_string());
                    if !p.is_empty() && !self.pool.contains_key(&p) {
                        self.pool.insert(p.clone(), m.clone());
                        self.simpan("pool.txt", &format!("{p} {m}"));
                    }
                    if k == "pool" && !self.koin.contains(&m) {
                        continue;
                    }
                }
                "amm" => {
                    let p = e["pool"].as_str().unwrap_or("").to_string();
                    // pool koin kita selalu dikenal dari event migrasi / pembuatan pool; pool lain dilewati
                    match self.pool.get(&p) {
                        Some(m) if self.koin.contains(m) => e["mint"] = serde_json::json!(m.clone()),
                        _ => continue,
                    }
                }
                _ => {}
            }
            *self.n.entry(k).or_insert(0) += 1;
            e["t"] = serde_json::json!(t);
            e["slot"] = serde_json::json!(slot);
            e["sig"] = serde_json::json!(sig);
            if isi {
                e["isi"] = serde_json::json!(true);
            }
            self.tulis(&e);
        }
    }
    /// isi celah setelah putus: tanda tangan program sejak `sampai` (terbaru dulu), lalu getTransaction satu per satu
    fn isi_celah(&mut self, program: &str, sampai: &str) {
        let r = self.rpc("getSignaturesForAddress", serde_json::json!([program, {"until": sampai, "limit": 1000, "commitment": "confirmed"}]));
        let sigs: Vec<(String, u64)> = r
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter(|x| x["err"].is_null())
            .filter_map(|x| Some((x["signature"].as_str()?.to_string(), x["slot"].as_u64().unwrap_or(0))))
            .collect();
        let total = sigs.len();
        let mut terisi = 0;
        for (sig, slot) in sigs.into_iter().rev().take(ISI_MAKS) {
            if self.lihat.contains(&sig) {
                continue;
            }
            let tx = self.rpc("getTransaction", serde_json::json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "confirmed"}]));
            if let Some(logs) = tx.as_ref().and_then(|t| t["meta"]["logMessages"].as_array()).cloned() {
                self.proses(&sig, slot, &logs, true);
                terisi += 1;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        self.tulis(&serde_json::json!({"k": "isi", "program": program, "t": detik(), "celah_tx": total, "terisi": terisi,
            "lengkap": total < 1000 && terisi == total}));
        eprintln!("isi celah {program}: {terisi} dari {total} tx");
    }
}

fn baca_baris(p: &str) -> Vec<String> {
    std::fs::File::open(p).map(|f| std::io::BufReader::new(f).lines().map_while(Result::ok).collect()).unwrap_or_default()
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "data".into());
    // SOL_WS: daftar websocket dipisah koma (utama dulu, cadangan sesudahnya); SOL_RPC: RPC untuk mengisi celah.
    // Dibaca dari lingkungan (berkas .rpc.env, chmod 600) supaya kunci API tidak ada di kode atau log.
    let ws_list: Vec<String> = std::env::var("SOL_WS").unwrap_or_else(|_| "wss://api.mainnet-beta.solana.com".into()).split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
    let rpc = std::env::var("SOL_RPC").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let tanpa_kunci = |u: &str| u.split('?').next().unwrap_or("").to_string();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("ERROR folder {dir}: {e}");
        std::process::exit(2);
    }
    let mut s = Keadaan {
        koin: baca_baris(&format!("{dir}/koin.txt")).into_iter().collect(),
        pool: baca_baris(&format!("{dir}/pool.txt")).iter().filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string()))).collect(),
        lihat: HashSet::new(),
        urut: VecDeque::new(),
        n: HashMap::new(),
        dir,
        rpc,
    };
    eprintln!("mulai: {} koin dikenal, {} pool dikenal", s.koin.len(), s.pool.len());
    // signature terakhir per program (untuk mengisi celah setelah putus)
    // disimpan di <dir>/akhir.json tiap menit, jadi celah karena restart layanan juga diisi
    let p_akhir = format!("{}/akhir.json", s.dir);
    let mut akhir: HashMap<&'static str, String> = std::fs::read_to_string(&p_akhir)
        .ok()
        .and_then(|t| serde_json::from_str::<HashMap<String, String>>(&t).ok())
        .map(|m| [PUMP, AMM].into_iter().filter_map(|p| m.get(p).map(|v| (p, v.clone()))).collect())
        .unwrap_or_default();
    let mut slot_akhir = 0u64;
    let (mut jeda, mut t_lapor) = (2u64, Instant::now());
    let mut iw = 0usize;
    loop {
        let ws_url = ws_list[iw % ws_list.len()].clone();
        let mut ws = match connect(ws_url.as_str()) {
            Ok((ws, _)) => ws,
            Err(e) => {
                // gagal sambung: pindah ke websocket berikutnya di daftar
                eprintln!("sambung gagal ke {}: {}; coba berikutnya {jeda} dtk", tanpa_kunci(&ws_url), e.to_string().replace(&ws_url, ""));
                iw += 1;
                std::thread::sleep(Duration::from_secs(jeda));
                jeda = (jeda * 2).min(60);
                continue;
            }
        };
        let mut ok = true;
        for (id, p) in [(1, PUMP), (2, AMM)] {
            let sub = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "logsSubscribe", "params": [{"mentions": [p]}, {"commitment": "confirmed"}]});
            ok &= ws.send(Message::text(sub.to_string())).is_ok();
        }
        if !ok {
            continue;
        }
        s.tulis(&serde_json::json!({"k": "sambung", "t": detik(), "slot_sebelum_putus": slot_akhir, "sumber": tanpa_kunci(&ws_url)}));
        eprintln!("tersambung ke {}", tanpa_kunci(&ws_url));
        let t_sambung = Instant::now();
        jeda = 2;
        // isi celah sejak signature terakhir sebelum putus, di thread terpisah supaya websocket tetap dibaca.
        // Thread ini punya salinan daftar koin/pool sendiri; baris isian ditandai "isi" (analisa dedupe per sig).
        let sampai: Vec<(&'static str, String)> = [PUMP, AMM].into_iter().filter_map(|p| akhir.get(p).map(|x| (p, x.clone()))).collect();
        if !sampai.is_empty() {
            let mut bantu = Keadaan { dir: s.dir.clone(), rpc: s.rpc.clone(), koin: s.koin.clone(), pool: s.pool.clone(),
                lihat: HashSet::new(), urut: VecDeque::new(), n: HashMap::new() };
            std::thread::spawn(move || {
                for (p, sig) in sampai {
                    bantu.isi_celah(p, &sig);
                }
            });
        }
        let mut sub_prog: HashMap<u64, &'static str> = HashMap::new();
        loop {
            let txt = match ws.read() {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            };
            let Ok(m) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
            // balasan langganan: id 1 = Pump.fun, id 2 = PumpSwap -> nomor langganan
            if let (Some(id), Some(no)) = (m["id"].as_u64(), m["result"].as_u64()) {
                sub_prog.insert(no, if id == 1 { PUMP } else { AMM });
                continue;
            }
            let r = &m["params"]["result"];
            let v = &r["value"];
            if !v["err"].is_null() {
                continue;
            }
            let Some(logs) = v["logs"].as_array() else { continue };
            let sig = v["signature"].as_str().unwrap_or("").to_string();
            let slot = r["context"]["slot"].as_u64().unwrap_or(0);
            if let Some(p) = m["params"]["subscription"].as_u64().and_then(|n| sub_prog.get(&n)) {
                akhir.insert(p, sig.clone());
            }
            slot_akhir = slot_akhir.max(slot);
            let logs = logs.clone();
            s.proses(&sig, slot, &logs, false);
            if t_lapor.elapsed() >= Duration::from_secs(60) {
                if let Ok(t) = serde_json::to_string(&akhir) {
                    let _ = std::fs::write(&p_akhir, t);
                }
                eprintln!("menit: {:?} | koin dipantau {} | pool dikenal {}", s.n, s.koin.len(), s.pool.len());
                s.n.clear();
                t_lapor = Instant::now();
            }
        }
        // putus cepat (< 60 dtk) berarti sumber ini bermasalah: pindah; selain itu kembali ke sumber utama
        iw = if t_sambung.elapsed() < Duration::from_secs(60) { iw + 1 } else { 0 };
        eprintln!("sambungan putus di slot {slot_akhir}; sambung ulang");
        std::thread::sleep(Duration::from_secs(2));
    }
}
