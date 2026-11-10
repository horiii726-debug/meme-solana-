//! Large-scale data-driven feature mining (~800-1000 features): every combination of time window x side x statistic
//! over the decoded swaps, plus wallet, slot, timing, early-buyer, price, liquidity, market-cap, failed-tx and creator
//! features. All causal (events with t <= t0 + tau). Targets written for `permutasi`:
//!   y2x  = price reaches 2x the entry trade price before any LP removal (entry = first swap after t0 + tau)
//!   rug5 = LP removed within 300 s after the decision time
//!   tambang1000 <dump_out.jsonl> <raw_file>...       env LIQ_MIN (SOL at tau, default 1)
use dekode::series::*;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;

const TAUS: [f64; 5] = [5.0, 10.0, 15.0, 30.0, 60.0];

fn quant(v: &[f64], p: f64) -> f64 {
    if v.is_empty() { return 0.0 }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() - 1) as f64 * p) as usize]
}
fn gini(v: &[f64]) -> f64 {
    if v.len() < 2 { return 0.0 }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len() as f64;
    let sum: f64 = s.iter().sum();
    if sum <= 0.0 { return 0.0 }
    s.iter().enumerate().map(|(i, x)| (2.0 * (i as f64 + 1.0) - n - 1.0) * x).sum::<f64>() / (n * sum)
}

fn mine(p: &Pool, tau: f64, supply: &HashMap<String, f64>) -> Map<String, Value> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let te = t0 + tau;
    let x: Vec<&Sw> = s.iter().filter(|w| w.t <= te).collect();
    let mut m = Map::new();
    let mut put = |k: String, v: f64| { m.insert(k, if v.is_finite() { json!((v * 1e6).round() / 1e6) } else { Value::Null }); };
    let wins: Vec<(&str, f64, f64)> = vec![("all", t0, te), ("h1", t0, t0 + tau / 2.0), ("h2", t0 + tau / 2.0, te), ("l2", te - 2.0, te), ("l5", te - 5.0, te), ("l10", te - 10.0, te), ("l3rd", te - tau / 3.0, te)];
    let seen_before = |from: f64| -> HashSet<&str> { x.iter().filter(|e| e.t < from).map(|e| e.trader.as_str()).collect() };
    let first10: HashSet<&str> = {
        let mut v = vec![];
        for e in x.iter().filter(|e| e.buy) { if !v.contains(&e.trader.as_str()) { v.push(e.trader.as_str()) } if v.len() >= 10 { break } }
        v.into_iter().collect()
    };
    let liq_at = |t: f64| x.iter().filter(|e| e.t <= t).last().map_or(s[0].liq, |e| e.liq);
    let mut rate_sum: HashMap<(String, String), (f64, f64)> = HashMap::new();
    for (wn, a, b) in &wins {
        let w: Vec<&&Sw> = x.iter().filter(|e| e.t >= *a && e.t <= *b).collect();
        let len = (b - a).max(1.0);
        let liq = liq_at(*b).max(1e-9);
        for (sn, side) in [("buy", Some(true)), ("sell", Some(false)), ("any", None)] {
            let ws: Vec<&&&Sw> = w.iter().filter(|e| side.map_or(true, |bb| e.buy == bb)).collect();
            let v: Vec<f64> = ws.iter().map(|e| e.sol).collect();
            let n = v.len() as f64;
            let sum: f64 = v.iter().sum();
            let mean = if n > 0.0 { sum / n } else { 0.0 };
            let sd = if n > 1.0 { (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt() } else { 0.0 };
            let wal: HashSet<&str> = ws.iter().map(|e| e.trader.as_str()).collect();
            let k = format!("{wn}_{sn}");
            put(format!("{k}_n"), n);
            put(format!("{k}_rate"), n / len);
            put(format!("{k}_sol"), sum);
            put(format!("{k}_sol_rate"), sum / len);
            put(format!("{k}_wallets"), wal.len() as f64);
            put(format!("{k}_mean"), mean);
            put(format!("{k}_sd"), sd);
            put(format!("{k}_cv"), sd / mean.max(1e-12));
            put(format!("{k}_min"), v.iter().cloned().fold(f64::INFINITY, f64::min).min(1e9) * (n > 0.0) as u8 as f64);
            for (qn, q) in [("p10", 0.1), ("p25", 0.25), ("med", 0.5), ("p75", 0.75), ("p90", 0.9)] { put(format!("{k}_{qn}"), quant(&v, q)) }
            put(format!("{k}_max"), v.iter().cloned().fold(0.0, f64::max));
            put(format!("{k}_gini"), gini(&v));
            for th in [0.1, 0.5, 1.0, 2.0] { put(format!("{k}_share_ge{th}"), v.iter().filter(|x| **x >= th).count() as f64 / n.max(1.0)) }
            put(format!("{k}_sol_per_liq"), sum / liq);
            put(format!("{k}_max_per_liq"), v.iter().cloned().fold(0.0, f64::max) / liq);
            // per-wallet totals
            let mut pw: HashMap<&str, f64> = HashMap::new();
            for e in &ws { *pw.entry(&e.trader).or_insert(0.0) += e.sol }
            let pv: Vec<f64> = pw.values().cloned().collect();
            put(format!("{k}_wal_med"), quant(&pv, 0.5));
            put(format!("{k}_wal_p90"), quant(&pv, 0.9));
            put(format!("{k}_wal_max"), pv.iter().cloned().fold(0.0, f64::max));
            rate_sum.insert((wn.to_string(), sn.to_string()), (n / len, sum / len));
        }
        // imbalance and price path in the window
        let (bs, ss): (f64, f64) = (w.iter().filter(|e| e.buy).map(|e| e.sol).sum(), w.iter().filter(|e| !e.buy).map(|e| e.sol).sum());
        let (bn, sn) = (w.iter().filter(|e| e.buy).count() as f64, w.iter().filter(|e| !e.buy).count() as f64);
        let bw: HashSet<&str> = w.iter().filter(|e| e.buy).map(|e| e.trader.as_str()).collect();
        let aw: HashSet<&str> = w.iter().map(|e| e.trader.as_str()).collect();
        put(format!("{wn}_imb_sol"), (bs - ss) / (bs + ss + 1e-12));
        put(format!("{wn}_imb_n"), (bn - sn) / (bn + sn).max(1.0));
        put(format!("{wn}_net_sol"), bs - ss);
        put(format!("{wn}_buyer_share"), bw.len() as f64 / aw.len().max(1) as f64);
        let pxs: Vec<f64> = w.iter().map(|e| e.px).collect();
        let pstart = x.iter().filter(|e| e.t < *a).last().map_or(s[0].px, |e| e.px);
        let plast = pxs.last().cloned().unwrap_or(pstart);
        let pmax = pxs.iter().cloned().fold(pstart, f64::max);
        let pmin = pxs.iter().cloned().fold(pstart, f64::min);
        put(format!("{wn}_ret"), (plast / pstart).ln());
        put(format!("{wn}_max_ret"), (pmax / pstart).ln());
        put(format!("{wn}_min_ret"), (pmin / pstart).ln());
        put(format!("{wn}_dd"), (plast / pmax).ln());
        let mut prev = pstart;
        let (mut rv, mut up, mut dn, mut ami, mut na, mut sxy, mut sxx) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for e in &w {
            let d = (e.px / prev).ln();
            if d.is_finite() {
                rv += d * d;
                if d > 0.0 { up += 1.0 } else if d < 0.0 { dn += 1.0 }
                if e.sol > 0.0 { ami += d.abs() / e.sol; na += 1.0 }
                let q = if e.buy { e.sol } else { -e.sol };
                sxy += d * q;
                sxx += q * q;
            }
            prev = e.px;
        }
        put(format!("{wn}_rv"), rv.sqrt());
        put(format!("{wn}_up_share"), up / (up + dn).max(1.0));
        put(format!("{wn}_amihud"), if na > 0.0 { ami / na } else { 0.0 });
        put(format!("{wn}_kyle"), if sxx > 0.0 { sxy / sxx } else { 0.0 });
        // wallets
        let before = seen_before(*a);
        let new_b = bw.iter().filter(|k| !before.contains(*k)).count() as f64;
        put(format!("{wn}_new_buyer_share"), new_b / bw.len().max(1) as f64);
        put(format!("{wn}_new_buyers_rate"), new_b / len);
        let mut bc: HashMap<&str, (u32, f64)> = HashMap::new();
        let sellers: HashSet<&str> = w.iter().filter(|e| !e.buy).map(|e| e.trader.as_str()).collect();
        for e in w.iter().filter(|e| e.buy) { let c = bc.entry(&e.trader).or_insert((0, 0.0)); c.0 += 1; c.1 += e.sol }
        let nb = bc.len().max(1) as f64;
        put(format!("{wn}_repeat_buyers"), bc.values().filter(|c| c.0 >= 2).count() as f64 / nb);
        put(format!("{wn}_flippers"), bc.keys().filter(|k| sellers.contains(*k)).count() as f64 / nb);
        put(format!("{wn}_buys_per_wallet"), bn / nb);
        put(format!("{wn}_sol_from_flippers"), bc.iter().filter(|(k, _)| sellers.contains(*k)).map(|(_, c)| c.1).sum::<f64>() / bs.max(1e-12));
        let mut sh: Vec<f64> = bc.values().map(|c| c.1 / bs.max(1e-12)).collect();
        sh.sort_by(|a, b| b.partial_cmp(a).unwrap());
        for k in [1usize, 3, 5, 10] { put(format!("{wn}_top{k}"), sh.iter().take(k).sum()) }
        put(format!("{wn}_hhi"), sh.iter().map(|v| v * v).sum());
        let early_sold: f64 = w.iter().filter(|e| !e.buy && first10.contains(e.trader.as_str())).map(|e| e.sol).sum();
        put(format!("{wn}_first10_sell_sol"), early_sold);
        put(format!("{wn}_first10_sell_share"), early_sold / ss.max(1e-12));
        // slots
        let mut by_slot: HashMap<u64, (HashSet<&str>, f64, u32)> = HashMap::new();
        for e in &w { let c = by_slot.entry(e.slot).or_insert((HashSet::new(), 0.0, 0)); if e.buy { c.0.insert(&e.trader); c.1 += e.sol } c.2 += 1 }
        let ns = by_slot.len().max(1) as f64;
        put(format!("{wn}_slot_max_buyers"), by_slot.values().map(|c| c.0.len()).max().unwrap_or(0) as f64);
        put(format!("{wn}_slot_mean_buyers"), by_slot.values().map(|c| c.0.len()).sum::<usize>() as f64 / ns);
        for th in [2usize, 3, 5] { put(format!("{wn}_bundle{th}_share"), by_slot.values().filter(|c| c.0.len() >= th).map(|c| c.1).sum::<f64>() / bs.max(1e-12)) }
        put(format!("{wn}_slots"), by_slot.len() as f64);
        put(format!("{wn}_swaps_per_slot"), w.len() as f64 / ns);
        put(format!("{wn}_max_swaps_slot"), by_slot.values().map(|c| c.2).max().unwrap_or(0) as f64);
        // timing
        let gaps: Vec<f64> = w.windows(2).map(|g| g[1].t - g[0].t).collect();
        let gm = gaps.iter().sum::<f64>() / gaps.len().max(1) as f64;
        put(format!("{wn}_gap_mean"), gm);
        put(format!("{wn}_gap_med"), quant(&gaps, 0.5));
        put(format!("{wn}_gap_p90"), quant(&gaps, 0.9));
        put(format!("{wn}_gap_cv"), if gm > 0.0 { (gaps.iter().map(|g| (g - gm).powi(2)).sum::<f64>() / gaps.len().max(1) as f64).sqrt() / gm } else { 0.0 });
        put(format!("{wn}_gap_max"), gaps.iter().cloned().fold(0.0, f64::max));
        // liquidity & failed txs in the window
        put(format!("{wn}_liq_dlog"), (liq_at(*b).max(1e-9) / liq_at(*a).max(1e-9)).ln());
        let nf = p.failed.iter().filter(|t| **t >= *a && **t <= *b).count() as f64;
        put(format!("{wn}_fail_n"), nf);
        put(format!("{wn}_fail_share"), nf / (nf + w.len() as f64).max(1.0));
    }
    // cross-window ratios
    for sn in ["buy", "sell", "any"] {
        for (num, den) in [("l5", "all"), ("l10", "all"), ("h2", "h1"), ("l2", "l10"), ("l3rd", "all")] {
            let (a, b) = (rate_sum[&(num.to_string(), sn.to_string())], rate_sum[&(den.to_string(), sn.to_string())]);
            put(format!("ratio_{sn}_rate_{num}_{den}"), a.0 / b.0.max(1e-9));
            put(format!("ratio_{sn}_sol_{num}_{den}"), a.1 / b.1.max(1e-9));
        }
    }
    // first k swaps
    for k in [5usize, 10, 20, 50] {
        let f: Vec<&&Sw> = x.iter().take(k).collect();
        put(format!("first{k}_buy_sol"), f.iter().filter(|e| e.buy).map(|e| e.sol).sum());
        put(format!("first{k}_wallets"), f.iter().map(|e| e.trader.as_str()).collect::<HashSet<_>>().len() as f64);
        put(format!("first{k}_buy_share"), f.iter().filter(|e| e.buy).count() as f64 / f.len().max(1) as f64);
        put(format!("first{k}_ret"), f.last().map_or(0.0, |e| (e.px / s[0].px).ln()));
        put(format!("first{k}_time"), x.get(k - 1).map_or(tau * 2.0, |e| e.t - t0));
    }
    // pool-level
    let last = x.last().unwrap();
    put("liq_sol".into(), last.liq);
    put("liq_start".into(), s[0].liq);
    put("liq_dlog".into(), (last.liq.max(1e-9) / s[0].liq.max(1e-9)).ln());
    put("lock_events".into(), p.lp_lock.iter().filter(|t| **t <= te).count() as f64);
    let mc = supply.get(&p.mint).map_or(f64::NAN, |sp| last.px * sp);
    put("mc_sol".into(), mc);
    put("mc_per_liq".into(), mc / last.liq.max(1e-9));
    put("log_mc".into(), mc.max(1e-9).ln());
    put("venue_pumpswap".into(), (p.venue == "pumpswap") as u8 as f64);
    let cb: f64 = x.iter().filter(|e| e.buy && !p.creator.is_empty() && e.trader == p.creator).map(|e| e.sol).sum();
    let cs: f64 = x.iter().filter(|e| !e.buy && !p.creator.is_empty() && e.trader == p.creator).map(|e| e.sol).sum();
    put("creator_buy_sol".into(), cb);
    put("creator_sell_sol".into(), cs);
    put("creator_net_sol".into(), cb - cs);
    put("creator_known".into(), (!p.creator.is_empty()) as u8 as f64);
    // ---- features ported from sniper_v4 (causal, from swaps up to the decision) ----
    // holders: net tokens per trader from their own buys / sells (transfers outside the pool are not visible)
    let mut net: HashMap<&str, f64> = HashMap::new();
    let mut order: Vec<&str> = vec![];
    for e in &x { if e.buy && !net.contains_key(e.trader.as_str()) { order.push(&e.trader) } *net.entry(&e.trader).or_insert(0.0) += if e.buy { e.tok } else { -e.tok } }
    let held: Vec<f64> = net.values().cloned().filter(|v| *v > 0.0).collect();
    let tot_held: f64 = held.iter().sum();
    let mut hs = held.clone();
    hs.sort_by(|a, b| b.partial_cmp(a).unwrap());
    put("v4_holders".into(), held.len() as f64);
    put("v4_holders_vs_buyers".into(), held.len() as f64 / order.len().max(1) as f64);
    for k in [1usize, 5, 10] { put(format!("v4_top{k}_holder_share"), hs.iter().take(k).sum::<f64>() / tot_held.max(1e-12)) }
    put("v4_early20_held_share".into(), order.iter().take(20).map(|w| net.get(w).cloned().unwrap_or(0.0).max(0.0)).sum::<f64>() / tot_held.max(1e-12));
    // wash: SOL of wallets that both bought and sold within 10 s; multi-sell transactions
    let mut wash = 0.0;
    for (i, e) in x.iter().enumerate() {
        if x.iter().skip(i + 1).take_while(|f| f.t <= e.t + 10.0).any(|f| f.trader == e.trader && f.buy != e.buy) { wash += e.sol }
    }
    let vol: f64 = x.iter().map(|e| e.sol).sum();
    put("v4_wash_share".into(), wash / vol.max(1e-12));
    let mut sells_per_sig: HashMap<&str, u32> = HashMap::new();
    for e in x.iter().filter(|e| !e.buy && !e.sig.is_empty()) { *sells_per_sig.entry(&e.sig).or_insert(0) += 1 }
    put("v4_multi_sell_txs".into(), sells_per_sig.values().filter(|c| **c >= 2).count() as f64);
    // dust buys (< 0.01 SOL)
    let nb = x.iter().filter(|e| e.buy).count() as f64;
    put("v4_dust_share".into(), x.iter().filter(|e| e.buy && e.sol < 0.01).count() as f64 / nb.max(1.0));
    // EWMA of real-buy arrivals (fast 5 s, slow 30 s) at the decision time, and their ratio
    let (mut ef, mut es) = (0.0f64, 0.0f64);
    let mut lt = t0;
    for e in x.iter().filter(|e| e.buy && e.sol >= 0.01) { let dt = e.t - lt; ef = ef * (-dt / 5.0).exp() + 1.0; es = es * (-dt / 30.0).exp() + 1.0; lt = e.t }
    let dt = te - lt;
    ef *= (-dt / 5.0).exp();
    es *= (-dt / 30.0).exp();
    put("v4_ewma_fast".into(), ef / 5.0);
    put("v4_ewma_slow".into(), es / 30.0);
    put("v4_accel".into(), (ef / 5.0) / (es / 30.0).max(1e-9));
    // price vs VWAP (last 60 s) and lift from the window low
    let w60: Vec<&&Sw> = x.iter().filter(|e| e.t > te - 60.0 && e.tok > 0.0).collect();
    let (vs, vt): (f64, f64) = (w60.iter().map(|e| e.sol).sum(), w60.iter().map(|e| e.tok).sum());
    let lastp = x.last().map_or(s[0].px, |e| e.px);
    put("v4_price_vs_vwap60".into(), if vt > 0.0 { lastp / (vs / vt) } else { f64::NAN });
    put("v4_lift_from_low60".into(), lastp / w60.iter().map(|e| e.px).fold(f64::INFINITY, f64::min).max(1e-30));
    m
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pools = load_pools(&a[2..]);
    let supply: HashMap<String, f64> = std::fs::read_to_string("/root/sol/supply.tsv").unwrap_or_default().lines()
        .filter_map(|l| { let f: Vec<&str> = l.split('\t').collect(); Some((f.first()?.to_string(), f.get(1)?.parse().ok()?)) }).collect();
    let liq_min: f64 = std::env::var("LIQ_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let mut ids: Vec<(f64, String)> = pools.iter().filter(|(_, p)| !p.swaps.is_empty() && p.swaps[0].liq > 0.0).map(|(k, p)| (p.swaps[0].t, k.clone())).collect();
    ids.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let cut = (ids.len() as f64 * 0.6) as usize;
    let train: HashSet<String> = ids[..cut].iter().map(|x| x.1.clone()).collect();
    // features of all (pool, tau) in parallel
    let jobs: Vec<(&String, f64)> = ids.iter().flat_map(|(_, k)| TAUS.iter().map(move |t| (k, *t))).collect();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let chunk = (jobs.len() / cores).max(1);
    let lines: Vec<String> = std::thread::scope(|sc| {
        jobs.chunks(chunk).map(|c| {
            let (pools, supply, train) = (&pools, &supply, &train);
            sc.spawn(move || c.iter().filter_map(|(k, tau)| {
                let p = &pools[*k];
                let t0 = p.swaps[0].t;
                // causal universe: at least 5 swaps BEFORE the decision (no condition on what happens later) - fix 2026-11-09
                if p.swaps.iter().filter(|w| w.t <= t0 + tau).count() < 5 { return None }
                let rug = p.lp_out.first().map(|x| x.0);
                if rug.map_or(false, |r| r <= t0 + tau) { return None }
                let feat = mine(p, *tau, supply);
                if feat["liq_sol"].as_f64().unwrap_or(0.0) < liq_min { return None }
                let rr = rug.unwrap_or(f64::INFINITY);
                // no swap after the decision: label "not up" (the coin went quiet), never dropped
                let i0 = p.swaps.iter().position(|w| w.t > t0 + tau).unwrap_or(p.swaps.len() - 1);
                let y2x = p.swaps[i0..].iter().filter(|w| w.t < rr && w.t > t0 + tau).any(|w| w.px >= 2.0 * p.swaps[i0].px);
                // peak multiple from the entry price before any LP removal -> tiered targets +100 % .. +3000 %
                let peak = p.swaps[i0..].iter().filter(|w| w.t < rr && w.t > t0 + tau).map(|w| w.px).fold(p.swaps[i0].px, f64::max) / p.swaps[i0].px;
                let rug5 = rug.map_or(false, |r| r <= t0 + tau + 300.0);
                Some(json!({"tau": tau, "set": if train.contains(*k) { "train" } else { "test" }, "pool": k, "t0": t0, "y2x": y2x, "y3x": peak >= 3.0, "y6x": peak >= 6.0, "y11x": peak >= 11.0, "y31x": peak >= 31.0, "peak_mult": peak, "rug5": rug5, "feat": feat}).to_string())
            }).collect::<Vec<_>>())
        }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let mut f = std::fs::File::create(&a[1]).expect("out");
    for l in &lines { let _ = writeln!(f, "{l}"); }
    let nf = lines.first().and_then(|l| serde_json::from_str::<Value>(l).ok()).map_or(0, |v| v["feat"].as_object().map_or(0, |o| o.len()));
    eprintln!("rows {}, features per row {nf}, pools {} (train {}, test {})", lines.len(), ids.len(), cut, ids.len() - cut);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sw(t: f64, slot: u64, buy: bool, sol: f64, trader: &str, px: f64, liq: f64) -> Sw {
        Sw { t, slot, buy, sol, trader: trader.into(), px, liq, fee: 0.01, tok: sol / px, txfee: 0.00002, sqrt: 0.0, sol_is_a: false, dec_tok: 6, l: 0.0, sig: format!("s{slot}") }
    }
    /// synthetic pool (test only): swaps every second for 100 s, then an LP removal at 80 s and failed txs everywhere
    fn pool() -> Pool {
        let mut p = Pool::default();
        p.creator = "C".into();
        for i in 0..100u64 {
            let t = 1000.0 + i as f64;
            p.swaps.push(sw(t, 10 + i * 3, i % 3 != 0, 0.1 + (i % 7) as f64 * 0.05, &format!("w{}", i % 11), 1e-6 * (1.0 + i as f64 * 0.02), 5.0 + i as f64 * 0.1));
            p.failed.push(t + 0.5);
        }
        p.lp_out.push((1080.0, "remove_all_liquidity".into()));
        p.lp_lock.push(1003.0);
        p.lp_lock.push(1050.0);
        p
    }
    /// ANTI-LOOK-AHEAD: features at tau must be identical whether or not the future exists
    #[test]
    fn features_do_not_see_the_future() {
        let full = pool();
        let sup = HashMap::new();
        for tau in TAUS {
            let mut cut = full.clone();
            let te = cut.swaps[0].t + tau;
            cut.swaps.retain(|w| w.t <= te);
            cut.failed.retain(|t| *t <= te);
            cut.lp_lock.retain(|t| *t <= te);
            cut.lp_out.retain(|x| x.0 <= te);
            let (a, b) = (mine(&full, tau, &sup), mine(&cut, tau, &sup));
            for (k, v) in &a {
                assert_eq!(Some(v), b.get(k), "feature {k} at tau {tau} changes when the future is removed");
            }
        }
    }
}
