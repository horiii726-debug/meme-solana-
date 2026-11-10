//! C2 accuracy test (read-only): stored signature set of one pool vs a fresh re-listing of the same pool address.
//!   cek_sig <venue> <pool> <raw_file>...
//! Window = from the oldest stored tx slot to the newest stored tx slot (re-listing is limited to that slot range, so
//! transactions that happened after the pull are not counted as missing). Reports missing, extra, duplicates.
use ingest::*;
use serde_json::json;
use std::collections::{HashMap, HashSet};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: cek_sig <venue> <pool> <raw_file>...");
        std::process::exit(2);
    }
    let (vname, pool) = (&a[1], &a[2]);
    let recs = read_raw(&a[3..]);
    let mut count: HashMap<String, usize> = HashMap::new();
    let (mut smin, mut smax, mut tmin) = (u64::MAX, 0u64, f64::MAX);
    let mut fetch_failed = 0;
    for r in recs.iter().filter(|r| r["pool"] == json!(pool) && r["venue"] == json!(vname)) {
        match r["kind"].as_str() {
            Some("tx") => {
                *count.entry(r["sig"].as_str().unwrap_or("").to_string()).or_insert(0) += 1;
                let s = r["slot"].as_u64().unwrap_or(0);
                smin = smin.min(s);
                smax = smax.max(s);
                tmin = tmin.min(r["block_time"].as_f64().unwrap_or(f64::MAX));
            }
            Some("fetch_failed") => fetch_failed += 1,
            _ => {}
        }
    }
    if count.is_empty() {
        println!("{}", json!({"venue": vname, "pool": pool, "error": "no stored tx"}));
        return;
    }
    let dups: usize = count.values().filter(|c| **c > 1).map(|c| c - 1).sum();
    let rpc = Rpc::from_env();
    let (relist, complete) = rpc.list_sigs(pool, tmin - 1.0, None, 200);
    let fresh: HashSet<String> = relist.iter().filter(|x| {
        let s = x["slot"].as_u64().unwrap_or(0);
        s >= smin && s <= smax
    }).filter_map(|x| x["signature"].as_str().map(String::from)).collect();
    let stored: HashSet<String> = count.keys().cloned().collect();
    let missing: Vec<&String> = fresh.difference(&stored).collect();
    let extra: Vec<&String> = stored.difference(&fresh).collect();
    println!("{}", json!({"venue": vname, "pool": pool, "stored_unique": stored.len(), "relisted_in_window": fresh.len(),
        "relisting_complete": complete, "missing": missing.len(), "extra": extra.len(), "duplicates": dups,
        "fetch_failed_records": fetch_failed, "slot_window": [smin, smax], "missing_sample": missing.iter().take(3).collect::<Vec<_>>(),
        "calls": calls_summary()}));
}
