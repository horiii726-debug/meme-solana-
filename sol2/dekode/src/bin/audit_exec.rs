//! Execution audit: are the simulated exits realistic? For every simulated sell of the TP plan (all liquid pools,
//! decision at TAU, 3 failure seeds):
//!   - price we got (SOL / token) vs the realized price of the nearest REAL sell by another trader within +-3 slots of
//!     our landing slot, split by our size relative to that trade's size;
//!   - share of positions fully closed before the LP removal, share closed only by the "last state" fallback,
//!     failed attempts per trade, sells per trade.
//!   audit_exec <raw_file>...   env TAU (default 15)
use dekode::series::*;
use serde_json::json;

fn q(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() { return f64::NAN }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[((v.len() - 1) as f64 * p) as usize] * 10000.0).round() / 10000.0
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(15.0);
    let pools = load_pools(&files);
    let plan = Plan { tiers: vec![(2.0, 0.7), (3.0, 0.2)], trail: 0.3, max_age: 120.0, mom: None, be: None, ratchet: None };
    let (mut ratio_small, mut ratio_big, mut no_ref) = (vec![], vec![], 0u64);
    let (mut trades, mut closed_before_rug, mut fallback, mut rug_hold) = (0u64, 0u64, 0u64, 0u64);
    let (mut fails, mut sells) = (vec![], vec![]);
    for p in pools.values() {
        if p.swaps.len() < 10 { continue }
        let t0 = p.swaps[0].t;
        let Some(i) = p.swaps.iter().position(|w| w.t > t0 + tau) else { continue };
        if p.swaps[i].liq < 1.0 { continue }
        let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        for sd in 0..3 {
            let Some(ex) = trade_amm(p, tau, &plan, sd) else { continue };
            trades += 1;
            fails.push(ex.failed_attempts as f64);
            sells.push(ex.sells as f64);
            if ex.closed_by_last_state { fallback += 1 }
            if ex.rugged_while_holding { rug_hold += 1 }
            if !ex.closed_by_last_state && ex.fills.iter().all(|f| f.3) { closed_before_rug += 1 }
            for (j, amt, got, _) in &ex.fills {
                let sl = p.swaps[*j].slot;
                let ours = got / amt;
                let real = p.swaps.iter().filter(|w| !w.buy && w.tok > 0.0 && w.slot + 3 >= sl && w.slot <= sl + 3 && w.t < rug.max(p.swaps[*j].t + 1.0))
                    .min_by(|a, b| (a.slot as i64 - sl as i64).abs().cmp(&(b.slot as i64 - sl as i64).abs()));
                match real {
                    Some(w) => {
                        let r = ours / (w.sol / w.tok);
                        if *amt <= w.tok { ratio_small.push(r) } else { ratio_big.push(r) }
                    }
                    None => no_ref += 1,
                }
            }
        }
    }
    println!("{}", json!({"tau": tau, "trades": trades,
        "our_price_vs_nearest_real_sell": {
            "when_we_sell_less_than_that_trade": {"n": ratio_small.len(), "median": q(&mut ratio_small.clone(), 0.5), "p10": q(&mut ratio_small.clone(), 0.1), "p90": q(&mut ratio_small.clone(), 0.9)},
            "when_we_sell_more_than_that_trade": {"n": ratio_big.len(), "median": q(&mut ratio_big.clone(), 0.5), "p10": q(&mut ratio_big.clone(), 0.1), "p90": q(&mut ratio_big.clone(), 0.9)},
            "sells_without_any_real_sell_within_3_slots": no_ref},
        "positions_closed_fully_before_lp_removal": closed_before_rug, "closed_only_by_last_state_fallback": fallback, "lp_removed_while_holding": rug_hold,
        "failed_attempts_per_trade": {"median": q(&mut fails.clone(), 0.5), "p90": q(&mut fails.clone(), 0.9)}, "sells_per_trade": {"median": q(&mut sells.clone(), 0.5)}}));
}
