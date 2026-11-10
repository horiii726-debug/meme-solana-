//! Total token supply per mint (getTokenSupply, read-only) -> TSV "mint<TAB>ui_supply". Used for market cap = price x supply.
//!   suplai <mints.txt> <out.tsv>      (RPC from RPC_URLS; mints already in out.tsv are skipped)
use ingest::*;
use serde_json::json;
use std::io::Write;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let done: std::collections::HashSet<String> = std::fs::read_to_string(&a[2]).unwrap_or_default().lines().filter_map(|l| l.split('\t').next().map(String::from)).collect();
    let rpc = Rpc::from_env();
    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&a[2]).expect("out");
    let (mut ok, mut bad) = (0, 0);
    for m in std::fs::read_to_string(&a[1]).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty() && !done.contains(*l)) {
        match rpc.call("getTokenSupply", json!([m])) {
            Ok(v) => match v["value"]["uiAmountString"].as_str().and_then(|s| s.parse::<f64>().ok()) {
                Some(s) => { let _ = writeln!(out, "{m}\t{s}"); ok += 1 }
                None => bad += 1,
            },
            Err(_) => bad += 1,
        }
    }
    eprintln!("supply ok {ok}, failed {bad}, {}", calls_summary());
}
