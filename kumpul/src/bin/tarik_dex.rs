//! History puller for launchpads decoded from full transactions (Raydium LaunchLab, Meteora DBC). Read-only.
//!   tarik_dex <out.jsonl> <program_id> <t_start_unix> <t_end_unix> [max_calls=3000]
//! Pages Helius `getTransactionsForAddress` (full, ascending) over the program address between the two times and writes
//! coin creations + trades in the same JSONL format as `sol-kumpul` / `tarik` ("t" = blockTime, "n" = order).
#[path = "../dekode.rs"]
#[allow(dead_code, unused_imports)]
mod dekode;
#[path = "../dekode_tx.rs"]
#[allow(dead_code)]
mod dekode_tx;
use std::io::Write;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("pakai: tarik_dex <keluar.jsonl> <program_id> <t_mulai> <t_akhir> [maks_panggilan=3000]");
        std::process::exit(2);
    }
    let (prog, t0, t1) = (a[2].clone(), a[3].parse::<i64>().unwrap_or(0), a[4].parse::<i64>().unwrap_or(0));
    let maks: usize = a.get(5).and_then(|x| x.parse().ok()).unwrap_or(3000);
    let Ok(rpc) = std::env::var("SOL_RPC") else {
        eprintln!("ERROR: SOL_RPC belum diisi (berkas .rpc.env)");
        std::process::exit(2);
    };
    let mut out = match std::fs::OpenOptions::new().create(true).append(true).open(&a[1]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ERROR {}: {e}", a[1]);
            std::process::exit(2);
        }
    };
    let (mut token, mut calls, mut n, mut n_tx, mut n_create, mut n_trade) = (None::<String>, 0usize, 0u64, 0usize, 0usize, 0usize);
    loop {
        if calls >= maks {
            eprintln!("BATAS PANGGILAN {maks} tercapai; berhenti (data sampai sini lengkap berurutan)");
            let _ = writeln!(out, "{}", serde_json::json!({"k": "berhenti", "program": prog}));
            break;
        }
        let mut o = serde_json::json!({"transactionDetails": "full", "limit": 100, "sortOrder": "asc", "encoding": "json",
            "maxSupportedTransactionVersion": 1, "commitment": "confirmed",
            "filters": {"blockTime": {"gte": t0, "lte": t1}, "status": "succeeded"}});
        if let Some(t) = &token {
            o["paginationToken"] = t.clone().into();
        }
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getTransactionsForAddress", "params": [prog, o]});
        let mut r = None;
        for k in 0..5u32 {
            calls += 1;
            let x = ureq::post(&rpc).timeout(Duration::from_secs(60)).send_json(body.clone()).ok().and_then(|x| x.into_json::<serde_json::Value>().ok());
            std::thread::sleep(Duration::from_millis(130));
            if let Some(x) = x.filter(|x| x.get("result").is_some()) {
                r = Some(x["result"].clone());
                break;
            }
            std::thread::sleep(Duration::from_secs(1 << k));
        }
        let Some(r) = r else {
            eprintln!("rpc gagal 5x; berhenti");
            let _ = writeln!(out, "{}", serde_json::json!({"k": "berhenti", "program": prog}));
            break;
        };
        for tx in r["data"].as_array().into_iter().flatten() {
            n_tx += 1;
            let (slot, bt) = (tx["slot"].as_u64().unwrap_or(0), tx["blockTime"].as_i64().unwrap_or(0));
            let sig = tx["transaction"]["signatures"][0].as_str().unwrap_or("").to_string();
            let mut evs = dekode_tx::creates(tx);
            n_create += evs.len();
            let tr = dekode_tx::trades(tx);
            n_trade += tr.len();
            evs.extend(tr);
            for mut e in evs {
                n += 1;
                e["t"] = (bt as f64).into();
                e["slot"] = slot.into();
                e["sig"] = sig.clone().into();
                e["n"] = n.into();
                let _ = writeln!(out, "{e}");
            }
        }
        if calls % 50 == 0 {
            eprintln!("panggilan {calls} | tx {n_tx} | koin baru {n_create} | trade {n_trade}");
        }
        token = r["paginationToken"].as_str().map(|s| s.to_string());
        if token.is_none() {
            break;
        }
    }
    eprintln!("SELESAI {prog}: panggilan {calls} | tx {n_tx} | koin baru {n_create} | trade {n_trade}");
}
