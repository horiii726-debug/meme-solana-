//! Liquidity events per pool (DAMM v2 EvtLiquidityChange, PumpSwap Deposit/WithdrawEvent) with time since the first
//! stored tx of the pool and the raw event fields. Offline.
//!   likuid <raw_file>...
use dekode::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let mut first: BTreeMap<String, f64> = BTreeMap::new();
    let mut evs: Vec<(String, f64, Value)> = vec![];
    for p in &files {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
            if r["kind"] != "tx" || r["failed"] == true { continue }
            let pool = r["pool"].as_str().unwrap_or("").to_string();
            let t = r["tx"]["blockTime"].as_f64().unwrap_or(0.0);
            let e = first.entry(pool.clone()).or_insert(t);
            *e = e.min(t);
            let idl = if r["venue"] == "pumpswap" { &ps } else { &dm };
            for e in events(&r["tx"], idl) {
                if matches!(e["event"].as_str(), Some("EvtLiquidityChange" | "DepositEvent" | "WithdrawEvent")) && e["fields"]["pool"] == json!(pool) {
                    evs.push((pool.clone(), t, json!({"event": e["event"], "ix": e["ix"], "fields": e["fields"]})));
                }
            }
        }
    }
    evs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap()));
    for (pool, t, e) in evs {
        println!("{}", json!({"pool": pool, "sec_after_first_tx": t - first[&pool], "ev": e}));
    }
}
