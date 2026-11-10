//! C7 wallet lookups (read-only). For every early buyer (pool, wallet, buy_sig, buy_time):
//!   - getSignaturesForAddress(wallet, before = buy_sig, limit 100): how many transactions the wallet made BEFORE this
//!     buy (capped at 100), time of its oldest and newest earlier transaction (causal: only history before the buy);
//!   - if the wallet has < 100 earlier transactions: getTransaction(oldest) -> the funder = the account whose SOL balance
//!     dropped the most in that transaction (the wallet's first funding), and when.
//! Output JSONL, one line per input row. Already-done rows (same pool+wallet) are skipped on restart.
//!   dompet <early_buyers.tsv> <out.jsonl>        RPC from RPC_URLS (+ ALLOW_PAID/MAX_CALLS for metered), WORKERS (default 12)
use ingest::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let done: HashSet<String> = std::fs::read_to_string(&a[2]).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| format!("{}|{}", v["pool"].as_str().unwrap_or(""), v["wallet"].as_str().unwrap_or(""))).collect();
    let rows: Vec<Vec<String>> = std::fs::read_to_string(&a[1]).unwrap_or_default().lines().map(|l| l.split('\t').map(String::from).collect::<Vec<_>>())
        .filter(|r| r.len() >= 4 && !done.contains(&format!("{}|{}", r[0], r[1]))).collect();
    eprintln!("to do {}", rows.len());
    let rpc = Arc::new(Rpc::from_env());
    let out = Arc::new(Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(&a[2]).unwrap()));
    let q = Arc::new(Mutex::new(rows));
    let workers: usize = std::env::var("WORKERS").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let hs: Vec<_> = (0..workers).map(|_| {
        let (rpc, out, q) = (rpc.clone(), out.clone(), q.clone());
        std::thread::spawn(move || loop {
            let Some(r) = q.lock().unwrap().pop() else { break };
            let (pool, wallet, sig, t) = (&r[0], &r[1], &r[2], r[3].parse::<f64>().unwrap_or(0.0));
            let age_h = (now() - t) / 3600.0 + 1.0;
            let res = rpc.call_age("getSignaturesForAddress", json!([wallet, {"before": sig, "limit": 100}]), age_h);
            let mut rec = json!({"pool": pool, "wallet": wallet, "buy_time": t});
            match res {
                Ok(v) => {
                    let arr = v.as_array().cloned().unwrap_or_default();
                    rec["n_prior"] = json!(arr.len());
                    rec["newest_prior"] = arr.first().map_or(Value::Null, |x| x["blockTime"].clone());
                    rec["oldest_prior"] = arr.last().map_or(Value::Null, |x| x["blockTime"].clone());
                    if !arr.is_empty() && arr.len() < 100 {
                        let os = arr.last().and_then(|x| x["signature"].as_str()).unwrap_or("").to_string();
                        if let Ok(tx) = rpc.get_tx_age(&os, 2, age_h) {
                            let keys = all_keys(&tx);
                            let (pre, post) = (tx["meta"]["preBalances"].as_array().cloned().unwrap_or_default(), tx["meta"]["postBalances"].as_array().cloned().unwrap_or_default());
                            let mut best: Option<(i64, String)> = None;
                            for (i, k) in keys.iter().enumerate() {
                                if k == wallet { continue }
                                let d = post.get(i).and_then(|x| x.as_i64()).unwrap_or(0) - pre.get(i).and_then(|x| x.as_i64()).unwrap_or(0);
                                if d < 0 && best.as_ref().map_or(true, |b| d < b.0) { best = Some((d, k.clone())) }
                            }
                            if let Some((d, k)) = best { rec["funder"] = json!(k); rec["funded_sol"] = json!(-d as f64 / 1e9) }
                            rec["funded_at"] = tx["blockTime"].clone();
                        }
                    }
                }
                Err(e) => rec["err"] = json!(e),
            }
            let _ = writeln!(out.lock().unwrap(), "{rec}");
        })
    }).collect();
    for h in hs { let _ = h.join(); }
    eprintln!("done {}", calls_summary());
}
