//! Exit-model dataset. For every liquid pool (SOL >= LIQ_MIN at entry), entry = first swap after t0 + TAU; while the
//! position would be held, a snapshot is taken at a swap every >= STEP seconds up to MAXH seconds after entry (and never
//! after the LP removal). Features use only swaps / failed txs up to the snapshot (causal). Label "dump": within the next
//! HORIZON seconds the price falls >= 30 % below the snapshot price, or the LP is removed.
//!   exit_data <out.jsonl> <raw_file>...   env TAU (10), LIQ_MIN (1), STEP (3), MAXH (600), HORIZON (30)
use dekode::series::*;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::io::Write;

fn envf(k: &str, d: f64) -> f64 { std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d) }

fn snap(p: &Pool, e: usize, j: usize) -> Map<String, Value> {
    let s = &p.swaps;
    let t = s[j].t;
    let mut m = Map::new();
    let mut put = |k: String, v: f64| { m.insert(k, if v.is_finite() { json!(v) } else { Value::Null }); };
    let hi_idx = (e..=j).max_by(|a, b| s[*a].px.partial_cmp(&s[*b].px).unwrap()).unwrap();
    put("ret_since_entry".into(), (s[j].px / s[e].px).ln());
    put("dd_from_high".into(), 1.0 - s[j].px / s[hi_idx].px);
    put("high_mult".into(), s[hi_idx].px / s[e].px);
    put("sec_since_entry".into(), t - s[e].t);
    put("sec_since_high".into(), t - s[hi_idx].t);
    put("sec_since_launch".into(), t - s[0].t);
    put("liq".into(), s[j].liq);
    put("liq_vs_entry".into(), (s[j].liq.max(1e-9) / s[e].liq.max(1e-9)).ln());
    put("liq_vs_high".into(), (s[j].liq.max(1e-9) / s[hi_idx].liq.max(1e-9)).ln());
    let early: HashSet<&str> = s[..=e].iter().filter(|w| w.buy).map(|w| w.trader.as_str()).collect();
    let seen_before: HashSet<&str> = s[..e].iter().map(|w| w.trader.as_str()).collect();
    for win in [5.0, 10.0, 30.0] {
        let w: Vec<&Sw> = s[..=j].iter().filter(|x| x.t > t - win).collect();
        let (bs, ss): (f64, f64) = (w.iter().filter(|x| x.buy).map(|x| x.sol).sum(), w.iter().filter(|x| !x.buy).map(|x| x.sol).sum());
        let (bn, sn) = (w.iter().filter(|x| x.buy).count() as f64, w.iter().filter(|x| !x.buy).count() as f64);
        let buyers: HashSet<&str> = w.iter().filter(|x| x.buy).map(|x| x.trader.as_str()).collect();
        let sellers: HashSet<&str> = w.iter().filter(|x| !x.buy).map(|x| x.trader.as_str()).collect();
        let k = format!("w{}", win as u32);
        put(format!("{k}_imb"), (bs - ss) / (bs + ss + 1e-12));
        put(format!("{k}_buy_rate"), bn / win);
        put(format!("{k}_sell_rate"), sn / win);
        put(format!("{k}_buy_sol"), bs);
        put(format!("{k}_sell_sol"), ss);
        put(format!("{k}_sell_per_liq"), ss / s[j].liq.max(1e-9));
        put(format!("{k}_buyers"), buyers.len() as f64);
        put(format!("{k}_sellers"), sellers.len() as f64);
        put(format!("{k}_new_buyers"), buyers.iter().filter(|b| !seen_before.contains(*b)).count() as f64);
        put(format!("{k}_early_sell_share"), w.iter().filter(|x| !x.buy && early.contains(x.trader.as_str())).map(|x| x.sol).sum::<f64>() / ss.max(1e-12));
        put(format!("{k}_max_sell"), w.iter().filter(|x| !x.buy).map(|x| x.sol).fold(0.0, f64::max));
        put(format!("{k}_ret"), w.first().map_or(0.0, |f| (s[j].px / f.px).ln()));
        let nf = p.failed.partition_point(|x| *x <= t) - p.failed.partition_point(|x| *x <= t - win);
        put(format!("{k}_fail_share"), nf as f64 / (nf as f64 + w.len() as f64).max(1.0));
        put(format!("{k}_creator_sold"), w.iter().any(|x| !x.buy && !p.creator.is_empty() && x.trader == p.creator) as u8 as f64);
    }
    // flow now vs the best flow since entry (buy rate over 10 s windows)
    let rate_now = s[..=j].iter().filter(|x| x.buy && x.t > t - 10.0).count() as f64;
    let mut best = 0.0f64;
    let mut a = e;
    for b in e..=j { while s[a].t <= s[b].t - 10.0 { a += 1 } best = best.max(s[a..=b].iter().filter(|x| x.buy).count() as f64) }
    put("buy_rate_vs_best".into(), rate_now / best.max(1.0));
    m
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (tau, liq_min, step, maxh, hor) = (envf("TAU", 10.0), envf("LIQ_MIN", 1.0), envf("STEP", 3.0), envf("MAXH", 600.0), envf("HORIZON", 30.0));
    let pools = load_pools(&a[2..]);
    let mut out = std::fs::File::create(&a[1]).unwrap();
    let (mut n, mut pos, mut np) = (0usize, 0usize, 0usize);
    for (k, p) in &pools {
        let s = &p.swaps;
        if s.iter().filter(|w| w.t <= s.first().map_or(0.0, |f| f.t) + tau).count() < 5 { continue }
        let t0 = s[0].t;
        let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        if rug <= t0 + tau { continue }
        let Some(e) = s.iter().position(|w| w.t > t0 + tau) else { continue };
        if s[e].liq < liq_min { continue }
        np += 1;
        let mut last = f64::NEG_INFINITY;
        for j in e..s.len() {
            let t = s[j].t;
            if t >= rug || t > s[e].t + maxh { break }
            if t - last < step { continue }
            last = t;
            let fut_min = s[j + 1..].iter().take_while(|w| w.t <= t + hor && w.t < rug).map(|w| w.px).fold(f64::INFINITY, f64::min);
            let dump = rug <= t + hor || fut_min <= 0.7 * s[j].px;
            let row = json!({"pool": k, "t0": t0, "t": t, "sec_since_entry": t - s[e].t, "dump": dump, "feat": snap(p, e, j)});
            let _ = writeln!(out, "{row}");
            n += 1;
            pos += dump as usize;
        }
    }
    eprintln!("pools {np}, snapshots {n}, dump-labelled {pos}");
}
