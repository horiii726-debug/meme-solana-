//! Quick descriptive check (no trading): per tracked pool, price path from on-chain data only.
//!   naik <raw_file>...
//! PumpSwap (constant product): price = SOL vault / token vault AFTER the swap (postTokenBalances of the vault accounts
//! named in the swap instruction, per idl/pump_amm.json). DAMM v2: price from EvtSwap2.swap_result.next_sqrt_price
//! (Q64.64, sqrt of token_b raw per token_a raw, per idl/meteora_damm_v2.json), SOL liquidity = SOL vault after the swap.
//! Only pools quoted in WSOL. Decimals from the tx token balances; a swap with unknown decimals is skipped and counted.
use dekode::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const WSOL: &str = "So11111111111111111111111111111111111111112";

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let recs = ingest_read(&files);
    // pool -> (venue, Vec<(block_time, slot, price SOL per token, sol liquidity)>)
    let mut path: BTreeMap<String, (String, Vec<(f64, u64, f64, f64)>)> = BTreeMap::new();
    let (mut unknown_dec, mut not_full) = (0, 0);
    for r in recs.iter().filter(|r| r["kind"] == "tx" && r["failed"] == false) {
        let venue = r["venue"].as_str().unwrap_or("");
        let pool = r["pool"].as_str().unwrap_or("").to_string();
        let tx = &r["tx"];
        let tb = token_balances(tx);
        let idl = if venue == "pumpswap" { &ps } else { &dm };
        for e in events(tx, idl) {
            if e["kind"] != "event" { continue }
            if e["fully_decoded"] == false { not_full += 1 }
            let f = &e["fields"];
            let acc = &e["ix_accounts"];
            let (bt, slot) = (tx["blockTime"].as_f64().unwrap_or(0.0), tx["slot"].as_u64().unwrap_or(0));
            let pt = match (venue, e["event"].as_str()) {
                ("pumpswap", Some("BuyEvent" | "SellEvent")) if f["pool"] == json!(pool) => {
                    let (bv, qv) = (acc["pool_base_token_account"].as_str(), acc["pool_quote_token_account"].as_str());
                    match (bv.and_then(|a| tb.get(a)), qv.and_then(|a| tb.get(a))) {
                        (Some(b), Some(q)) if q.0 == WSOL => match (b.2, q.2, b.4, q.4) {
                            (Some(bd), Some(qd), Some(bp), Some(qp)) if bp > 0 => {
                                let sol = qp as f64 / 10f64.powi(qd as i32);
                                Some((sol / (bp as f64 / 10f64.powi(bd as i32)), sol))
                            }
                            _ => { unknown_dec += 1; None }
                        },
                        _ => None,
                    }
                }
                ("meteora_damm_v2", Some("EvtSwap2")) if f["pool"] == json!(pool) => {
                    let (am, bm) = (acc["token_a_mint"].as_str().unwrap_or(""), acc["token_b_mint"].as_str().unwrap_or(""));
                    let (ad, bd) = (decimals_of(&tb, am), decimals_of(&tb, bm));
                    let s = n(&f["swap_result"]["next_sqrt_price"]).map(|x| x as f64 / 18446744073709551616.0);
                    let solv = if bm == WSOL { acc["token_b_vault"].as_str() } else if am == WSOL { acc["token_a_vault"].as_str() } else { None };
                    match (ad.as_u64(), bd.as_u64(), s, solv.and_then(|v| tb.get(v)).and_then(|x| x.4)) {
                        (Some(ad), Some(bd), Some(s), Some(sv)) if s > 0.0 => {
                            let b_per_a = s * s * 10f64.powi(ad as i32 - bd as i32); // human units of B per 1 A
                            let px = if bm == WSOL { b_per_a } else { 1.0 / b_per_a };
                            Some((px, sv as f64 / 1e9))
                        }
                        _ => { unknown_dec += 1; None }
                    }
                }
                _ => None,
            };
            if let Some((px, liq)) = pt {
                path.entry(pool.clone()).or_insert((venue.to_string(), vec![])).1.push((bt, slot, px, liq));
            }
        }
    }
    let mut rows = vec![];
    for (pool, (venue, mut v)) in path {
        v.sort_by(|a, b| a.1.cmp(&b.1));
        let (t0, p0, l0) = (v[0].0, v[0].2, v[0].3);
        let (mut pmax, mut tmax, mut pmin) = (p0, t0, p0);
        for x in &v {
            if x.2 > pmax { pmax = x.2; tmax = x.0 }
            pmin = pmin.min(x.2);
        }
        let last = v.last().unwrap();
        rows.push(json!({"venue": venue, "pool": pool, "swaps": v.len(), "minutes_observed": ((last.0 - t0) / 60.0 * 10.0).round() / 10.0,
            "sol_liq_start": (l0 * 100.0).round() / 100.0, "sol_liq_end": (last.3 * 100.0).round() / 100.0,
            "max_rise_pct": ((pmax / p0 - 1.0) * 1000.0).round() / 10.0, "min_drop_pct": ((pmin / p0 - 1.0) * 1000.0).round() / 10.0,
            "end_pct": ((last.2 / p0 - 1.0) * 1000.0).round() / 10.0, "minutes_to_peak": ((tmax - t0) / 60.0 * 10.0).round() / 10.0}));
    }
    for r in &rows {
        println!("{r}");
    }
    let up100 = rows.iter().filter(|r| r["max_rise_pct"].as_f64().unwrap_or(0.0) >= 100.0).count();
    let up100_liq = rows.iter().filter(|r| r["max_rise_pct"].as_f64().unwrap_or(0.0) >= 100.0 && r["sol_liq_end"].as_f64().unwrap_or(0.0) >= 1.0).count();
    println!("{}", json!({"pools_with_price": rows.len(), "max_rise_ge_100pct": up100, "of_which_sol_liq_end_ge_1": up100_liq,
        "swaps_unknown_decimals": unknown_dec, "events_not_fully_decoded": not_full}));
}

fn ingest_read(paths: &[String]) -> Vec<Value> {
    let mut out = vec![];
    for p in paths {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                out.push(v);
            }
        }
    }
    out
}
