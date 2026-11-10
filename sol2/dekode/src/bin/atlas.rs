//! Exploratory reverse-engineering atlas (descriptive, no trading). Per pool, from decoded on-chain swaps only:
//! early flow features at t0 + {30, 60, 120} s and outcomes (peak multiple, LP removal, LP lock, end state).
//!   atlas <raw_file>...   -> one JSON line per pool on stdout
//! t0 = first successful swap. Prices: PumpSwap = SOL vault / token vault after the swap (either side may be WSOL);
//! DAMM v2 = EvtSwap2 next_sqrt_price. SOL amounts in lamports from the event, side = direction of SOL.
//! Features use only swaps with block_time <= t0 + W (causal). Small-sample exploration: NOT evidence of an edge.
use dekode::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const WSOL: &str = "So11111111111111111111111111111111111111112";

#[derive(Clone)]
struct Sw { t: f64, slot: u64, buy: bool, sol: f64, trader: String, px: f64, liq: f64 }

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let mut sw: BTreeMap<String, Vec<Sw>> = BTreeMap::new();
    let mut venue_of: HashMap<String, String> = HashMap::new();
    let mut lp_out: HashMap<String, Vec<(f64, String)>> = HashMap::new();
    let mut lp_lock: HashMap<String, u64> = HashMap::new();
    let mut failed: HashMap<String, Vec<f64>> = HashMap::new();
    let mut creator: HashMap<String, String> = HashMap::new();
    for p in &files {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
            if r["kind"] != "tx" { continue }
            let pool = r["pool"].as_str().unwrap_or("").to_string();
            let venue = r["venue"].as_str().unwrap_or("").to_string();
            venue_of.insert(pool.clone(), venue.clone());
            let tx = &r["tx"];
            let t = tx["blockTime"].as_f64().unwrap_or(0.0);
            if r["failed"] == true { failed.entry(pool).or_default().push(t); continue }
            if matches!(r["source"].as_str(), Some("live_create" | "history_create")) {
                if let Some(c) = tx["transaction"]["message"]["accountKeys"][0].as_str() { creator.insert(pool.clone(), c.into()); }
            }
            let tb = token_balances(tx);
            let idl = if venue == "pumpswap" { &ps } else { &dm };
            for e in events(tx, idl) {
                let (f, acc) = (&e["fields"], &e["ix_accounts"]);
                if f["pool"] != json!(pool) { continue }
                let ev = e["event"].as_str().unwrap_or("");
                let ix = e["ix"].as_str().unwrap_or("");
                if ix.starts_with("remove") || ix == "withdraw" { lp_out.entry(pool.clone()).or_default().push((t, ix.into())) }
                if ev.contains("Lock") { *lp_lock.entry(pool.clone()).or_insert(0) += 1 }
                let s = match ev {
                    "BuyEvent" | "SellEvent" => {
                        let (bm, qm) = (acc["base_mint"].as_str().unwrap_or(""), acc["quote_mint"].as_str().unwrap_or(""));
                        let (bv, qv) = (acc["pool_base_token_account"].as_str().and_then(|a| tb.get(a)), acc["pool_quote_token_account"].as_str().and_then(|a| tb.get(a)));
                        let (Some(bv), Some(qv)) = (bv, qv) else { continue };
                        let (Some(bd), Some(qd), Some(bp), Some(qp)) = (bv.2, qv.2, bv.4, qv.4) else { continue };
                        let buy_base = ev == "BuyEvent";
                        let (sol_raw, tok_raw, sol_dec, tok_dec, buy) = if qm == WSOL {
                            (if buy_base { n(&f["quote_amount_in"]) } else { n(&f["quote_amount_out"]) }, bp, qd, bd, buy_base)
                        } else if bm == WSOL {
                            (if buy_base { n(&f["base_amount_out"]) } else { n(&f["base_amount_in"]) }, qp, bd, qd, !buy_base)
                        } else { continue };
                        let sol_res = if qm == WSOL { qp } else { bp } as f64 / 10f64.powi(sol_dec as i32);
                        let tok_res = tok_raw as f64 / 10f64.powi(tok_dec as i32);
                        if tok_res <= 0.0 { continue }
                        Sw { t, slot: tx["slot"].as_u64().unwrap_or(0), buy, sol: sol_raw.unwrap_or(0) as f64 / 1e9, trader: f["user"].as_str().unwrap_or("").into(), px: sol_res / tok_res, liq: sol_res }
                    }
                    "EvtSwap2" => {
                        let (am, bm) = (acc["token_a_mint"].as_str().unwrap_or(""), acc["token_b_mint"].as_str().unwrap_or(""));
                        let (Some(ad), Some(bd)) = (decimals_of(&tb, am).as_u64(), decimals_of(&tb, bm).as_u64()) else { continue };
                        let Some(sq) = n(&f["swap_result"]["next_sqrt_price"]) else { continue };
                        let s = sq as f64 / 18446744073709551616.0;
                        let b_per_a = s * s * 10f64.powi(ad as i32 - bd as i32);
                        let a_in = f["trade_direction"].as_u64() == Some(0);
                        let sol_is_a = am == WSOL;
                        if !sol_is_a && bm != WSOL { continue }
                        let buy = a_in == sol_is_a; // SOL goes in
                        let sol_raw = if buy { n(&f["swap_result"]["included_fee_input_amount"]) } else { n(&f["swap_result"]["output_amount"]) };
                        let solv = if sol_is_a { acc["token_a_vault"].as_str() } else { acc["token_b_vault"].as_str() };
                        let liq = solv.and_then(|v| tb.get(v)).and_then(|x| x.4).unwrap_or(0) as f64 / 1e9;
                        let px = if sol_is_a { 1.0 / b_per_a } else { b_per_a };
                        Sw { t, slot: tx["slot"].as_u64().unwrap_or(0), buy, sol: sol_raw.unwrap_or(0) as f64 / 1e9, trader: acc["payer"].as_str().unwrap_or("").into(), px, liq }
                    }
                    _ => continue,
                };
                sw.entry(pool.clone()).or_default().push(s);
            }
        }
    }
    for (pool, mut v) in sw {
        if v.len() < 3 { continue }
        v.sort_by(|a, b| a.slot.cmp(&b.slot));
        let (t0, p0) = (v[0].t, v[0].px);
        let feat = |w: f64| -> Value {
            let x: Vec<&Sw> = v.iter().filter(|s| s.t <= t0 + w).collect();
            let (bs, ss): (f64, f64) = (x.iter().filter(|s| s.buy).map(|s| s.sol).sum(), x.iter().filter(|s| !s.buy).map(|s| s.sol).sum());
            let mut per: HashMap<&str, f64> = HashMap::new();
            for s in x.iter().filter(|s| s.buy) { *per.entry(&s.trader).or_insert(0.0) += s.sol }
            let top1 = per.values().cloned().fold(0.0, f64::max) / bs.max(1e-9);
            let buyers: HashSet<&str> = x.iter().filter(|s| s.buy).map(|s| s.trader.as_str()).collect();
            let sellers: HashSet<&str> = x.iter().filter(|s| !s.buy).map(|s| s.trader.as_str()).collect();
            let last = x.last().unwrap();
            let fl = failed.get(&pool).map_or(0, |f| f.iter().filter(|t| **t <= t0 + w).count());
            let cr_sold = creator.get(&pool).map_or(false, |c| x.iter().any(|s| !s.buy && &s.trader == c));
            json!({"n_buys": x.iter().filter(|s| s.buy).count(), "n_sells": x.iter().filter(|s| !s.buy).count(),
                "buy_sol": (bs * 100.0).round() / 100.0, "sell_sol": (ss * 100.0).round() / 100.0, "buy_pressure": ((bs / (bs + ss + 1e-9)) * 100.0).round() / 100.0,
                "buyers": buyers.len(), "sellers": sellers.len(), "top1_buy_share": (top1 * 100.0).round() / 100.0,
                "ret_pct": ((last.px / p0 - 1.0) * 1000.0).round() / 10.0, "liq_sol": (last.liq * 10.0).round() / 10.0, "failed_tx": fl, "creator_sold": cr_sold})
        };
        let rug_t = lp_out.get(&pool).and_then(|v| v.iter().map(|x| x.0).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.min(b)))));
        let pre: Vec<&Sw> = v.iter().filter(|s| rug_t.map_or(true, |r| s.t < r)).collect();
        let (peak, tpeak) = pre.iter().fold((p0, t0), |a, s| if s.px > a.0 { (s.px, s.t) } else { a });
        let last = v.last().unwrap();
        println!("{}", json!({"pool": pool, "venue": venue_of[&pool], "swaps": v.len(), "observed_s": (last.t - t0).round(),
            "lp_removed": rug_t.is_some(), "lp_removed_after_s": rug_t.map(|r| (r - t0).round()), "lp_lock_events": lp_lock.get(&pool).copied().unwrap_or(0),
            "peak_mult_before_rug": ((peak / p0) * 100.0).round() / 100.0, "peak_after_s": (tpeak - t0).round(),
            "end_mult": ((last.px / p0) * 100.0).round() / 100.0, "liq_start": (v[0].liq * 10.0).round() / 10.0,
            "f30": feat(30.0), "f60": feat(60.0), "f120": feat(120.0)}));
    }
}
