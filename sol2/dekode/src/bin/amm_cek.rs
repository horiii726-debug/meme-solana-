//! C5 check: can we compute what a swap pays out from the pool state BEFORE the swap? (needed to simulate our own
//! sells exactly). Compares prediction vs the amounts the chain actually moved.
//!   amm_cek <raw_file>...
//! PumpSwap (idl/pump_amm.json): event reserves = vault balances BEFORE the swap (verified 100 % in C3).
//!   sell: quote_out = floor(base_in * Q / (B + base_in)) vs event quote_amount_out;
//!   buy:  quote_in  = ceil(base_out * Q / (B - base_out)) vs event quote_amount_in.
//!   user side: user_quote_amount_out = quote_out - lp_fee - protocol_fee - coin_creator_fee (checked exactly).
//! DAMM v2 (idl/meteora_damm_v2.json): liquidity L estimated from the PREVIOUS swap of the same pool
//!   (L = |dB| / |d sqrtP|, B = token_b amount that moved through the curve), then the NEXT swap's output is predicted
//!   from its own input and start sqrt price, and compared with output_amount (relative error reported).
use dekode::*;
use serde_json::{json, Value};
use std::collections::HashMap;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let (mut ps_n, mut ps_exact, mut ps_user_exact) = (0u64, 0u64, 0u64);
    let mut ps_err: Vec<f64> = vec![];
    // DAMM: per pool last swap (sqrt_after, L)
    let mut last: HashMap<String, (f64, f64, u64)> = HashMap::new();
    let mut dm_err: Vec<f64> = vec![];
    let mut recs: Vec<(u64, String, Value, String)> = vec![];
    for p in &files {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
            if r["kind"] != "tx" || r["failed"] == true { continue }
            recs.push((r["slot"].as_u64().unwrap_or(0), r["venue"].as_str().unwrap_or("").to_string(), r["tx"].clone(), r["pool"].as_str().unwrap_or("").to_string()));
        }
    }
    recs.sort_by_key(|x| x.0);
    let mut seen = std::collections::HashSet::new();
    for (_, venue, tx, pool) in &recs {
        let sig = tx["transaction"]["signatures"][0].as_str().unwrap_or("").to_string();
        if !seen.insert(sig) { continue }
        let idl = if venue == "pumpswap" { &ps } else { &dm };
        let evs: Vec<Value> = events(tx, idl).into_iter().filter(|e| e["fields"]["pool"] == json!(pool)).collect();
        if evs.iter().filter(|e| matches!(e["event"].as_str(), Some("BuyEvent" | "SellEvent" | "EvtSwap2"))).count() != 1 { continue }
        for e in &evs {
            let f = &e["fields"];
            match e["event"].as_str() {
                Some(ev @ ("BuyEvent" | "SellEvent")) => {
                    let (b, q) = (n(&f["pool_base_token_reserves"]).unwrap_or(0), n(&f["pool_quote_token_reserves"]).unwrap_or(0));
                    if b == 0 || q == 0 { continue }
                    ps_n += 1;
                    let fees = ["lp_fee", "protocol_fee", "coin_creator_fee"].iter().filter_map(|k| n(&f[*k])).sum::<u128>();
                    if ev == "SellEvent" {
                        let bi = n(&f["base_amount_in"]).unwrap_or(0);
                        let pred = bi * q / (b + bi);
                        let act = n(&f["quote_amount_out"]).unwrap_or(0);
                        if pred == act { ps_exact += 1 }
                        ps_err.push((pred as f64 - act as f64).abs() / (act as f64).max(1.0));
                        if n(&f["user_quote_amount_out"]) == Some(act.saturating_sub(fees)) { ps_user_exact += 1 }
                    } else {
                        let bo = n(&f["base_amount_out"]).unwrap_or(0);
                        if bo >= b { continue }
                        let pred = (bo * q + (b - bo) - 1) / (b - bo);
                        let act = n(&f["quote_amount_in"]).unwrap_or(0);
                        if pred == act { ps_exact += 1 }
                        ps_err.push((pred as f64 - act as f64).abs() / (act as f64).max(1.0));
                        if n(&f["user_quote_amount_in"]) == Some(act + fees) { ps_user_exact += 1 }
                    }
                }
                Some("EvtSwap2") => {
                    let r = &f["swap_result"];
                    let s1 = n(&r["next_sqrt_price"]).unwrap_or(0) as f64 / 18446744073709551616.0;
                    let a_to_b = f["trade_direction"].as_u64() == Some(0);
                    let (inc, exc, out) = (n(&r["included_fee_input_amount"]).unwrap_or(0) as f64, n(&r["excluded_fee_input_amount"]).unwrap_or(0) as f64, n(&r["output_amount"]).unwrap_or(0) as f64);
                    let fees: f64 = ["claiming_fee", "protocol_fee", "compounding_fee", "referral_fee"].iter().filter_map(|k| n(&r[*k])).sum::<u128>() as f64;
                    let fee_on_out = !(inc > exc);
                    let out_gross = if fee_on_out { out + fees } else { out };
                    // amount of token_b that moved through the curve
                    let db = if a_to_b { out_gross } else { exc };
                    if let Some((s0, l_prev, _)) = last.get(pool).cloned() {
                        // predict this swap's output with the previous L and the start price s0
                        if l_prev > 0.0 && s0 > 0.0 {
                            let pred_gross = if a_to_b {
                                // a in: 1/s1 = 1/s0 + exc/L ; b out = L (s0 - s1)
                                let s1p = 1.0 / (1.0 / s0 + exc / l_prev);
                                l_prev * (s0 - s1p)
                            } else {
                                // b in: s1 = s0 + exc/L ; a out = L (1/s0 - 1/s1)
                                let s1p = s0 + exc / l_prev;
                                l_prev * (1.0 / s0 - 1.0 / s1p)
                            };
                            let pred = if fee_on_out { pred_gross - fees } else { pred_gross };
                            if out > 0.0 && pred.is_finite() { dm_err.push((pred - out).abs() / out) }
                        }
                        let l_now = if (s1 - s0).abs() > 0.0 { db / (s1 - s0).abs() } else { 0.0 };
                        last.insert(pool.clone(), (s1, l_now, 0));
                    } else {
                        last.insert(pool.clone(), (s1, 0.0, 0));
                    }
                }
                Some(x) if x.contains("Liquidity") => { last.remove(pool); }
                _ => {}
            }
        }
    }
    let q = |v: &mut Vec<f64>, p: f64| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); if v.is_empty() { f64::NAN } else { v[((v.len() - 1) as f64 * p) as usize] } };
    println!("{}", json!({"pumpswap": {"swaps": ps_n, "exact_curve_match": ps_exact, "exact_user_amount_match": ps_user_exact,
        "rel_err_median": q(&mut ps_err.clone(), 0.5), "rel_err_p99": q(&mut ps_err.clone(), 0.99)},
        "damm_v2": {"predicted": dm_err.len(), "rel_err_median": q(&mut dm_err.clone(), 0.5), "rel_err_p90": q(&mut dm_err.clone(), 0.9), "rel_err_p99": q(&mut dm_err.clone(), 0.99),
            "share_within_0.5pct": dm_err.iter().filter(|e| **e <= 0.005).count() as f64 / dm_err.len().max(1) as f64}}));
}
