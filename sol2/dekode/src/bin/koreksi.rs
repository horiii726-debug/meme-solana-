//! Pullback study: when a coin we hold falls DEPTH below its running high (after entry), does it make a new high
//! ("naik_lagi") or die (LP removal / never recovers)? One event per running high per depth. Features use only swaps up
//! to the event time (causal). Output = dump for `permutasi` / `latih` (field "tau" = pullback depth in %).
//!   koreksi <dump_out.jsonl> <raw_file>...     env ENTRY_TAU (default 10), LIQ_MIN (default 1)
use dekode::series::*;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;

const DEPTHS: [f64; 3] = [0.3, 0.5, 0.6];

fn feats(p: &Pool, ev: usize, entry: usize, hi_idx: usize) -> Map<String, Value> {
    let s = &p.swaps;
    let te = s[ev].t;
    let mut m = Map::new();
    let mut put = |k: &str, v: f64| { m.insert(k.to_string(), if v.is_finite() { json!(v) } else { Value::Null }); };
    let hi = s[hi_idx].px;
    put("depth", 1.0 - s[ev].px / hi);
    put("peak_mult_so_far", hi / s[entry].px);
    put("sec_since_entry", te - s[entry].t);
    put("sec_since_high", te - s[hi_idx].t);
    put("sec_since_launch", te - s[0].t);
    put("liq", s[ev].liq);
    put("liq_vs_high", (s[ev].liq.max(1e-9) / s[hi_idx].liq.max(1e-9)).ln());
    put("liq_vs_entry", (s[ev].liq.max(1e-9) / s[entry].liq.max(1e-9)).ln());
    let earlier: HashSet<&str> = s[..entry].iter().map(|w| w.trader.as_str()).collect();
    let first20: HashSet<&str> = {
        let mut v: Vec<&str> = vec![];
        for w in s.iter().filter(|w| w.buy) { if !v.contains(&w.trader.as_str()) { v.push(&w.trader) } if v.len() >= 20 { break } }
        v.into_iter().collect()
    };
    let seen_before_high: HashSet<&str> = s[..=hi_idx].iter().map(|w| w.trader.as_str()).collect();
    for (wn, from) in [("since_high", s[hi_idx].t), ("l10", te - 10.0), ("l30", te - 30.0)] {
        let w: Vec<&Sw> = s[..=ev].iter().filter(|x| x.t >= from).collect();
        let len = (te - from).max(1.0);
        let (bs, ss): (f64, f64) = (w.iter().filter(|x| x.buy).map(|x| x.sol).sum(), w.iter().filter(|x| !x.buy).map(|x| x.sol).sum());
        let (bn, sn) = (w.iter().filter(|x| x.buy).count() as f64, w.iter().filter(|x| !x.buy).count() as f64);
        let buyers: HashSet<&str> = w.iter().filter(|x| x.buy).map(|x| x.trader.as_str()).collect();
        let sellers: HashSet<&str> = w.iter().filter(|x| !x.buy).map(|x| x.trader.as_str()).collect();
        put(&format!("{wn}_buy_sol"), bs);
        put(&format!("{wn}_sell_sol"), ss);
        put(&format!("{wn}_imb"), (bs - ss) / (bs + ss + 1e-12));
        put(&format!("{wn}_buy_rate"), bn / len);
        put(&format!("{wn}_sell_rate"), sn / len);
        put(&format!("{wn}_buyers"), buyers.len() as f64);
        put(&format!("{wn}_sellers"), sellers.len() as f64);
        put(&format!("{wn}_new_buyers"), buyers.iter().filter(|b| !seen_before_high.contains(*b)).count() as f64);
        put(&format!("{wn}_new_buyer_share"), buyers.iter().filter(|b| !seen_before_high.contains(*b)).count() as f64 / buyers.len().max(1) as f64);
        put(&format!("{wn}_early_holder_sell_sol"), w.iter().filter(|x| !x.buy && (first20.contains(x.trader.as_str()) || earlier.contains(x.trader.as_str()))).map(|x| x.sol).sum());
        put(&format!("{wn}_early_holder_sell_share"), w.iter().filter(|x| !x.buy && (first20.contains(x.trader.as_str()) || earlier.contains(x.trader.as_str()))).map(|x| x.sol).sum::<f64>() / ss.max(1e-12));
        let mut ps: HashMap<&str, f64> = HashMap::new();
        for x in w.iter().filter(|x| !x.buy) { *ps.entry(&x.trader).or_insert(0.0) += x.sol }
        put(&format!("{wn}_top_seller_share"), ps.values().cloned().fold(0.0, f64::max) / ss.max(1e-12));
        let mut sz: Vec<f64> = w.iter().filter(|x| !x.buy).map(|x| x.sol).collect();
        sz.sort_by(|a, b| a.partial_cmp(b).unwrap());
        put(&format!("{wn}_sell_max"), sz.last().cloned().unwrap_or(0.0));
        put(&format!("{wn}_sell_per_liq"), ss / s[ev].liq.max(1e-9));
        put(&format!("{wn}_buy_per_liq"), bs / s[ev].liq.max(1e-9));
        let nf = p.failed.iter().filter(|t| **t >= from && **t <= te).count() as f64;
        put(&format!("{wn}_fail_n"), nf);
        put(&format!("{wn}_fail_share"), nf / (nf + w.len() as f64).max(1.0));
        let flips = sellers.iter().filter(|x| buyers.contains(*x)).count() as f64;
        put(&format!("{wn}_flipper_share"), flips / sellers.len().max(1) as f64);
        put(&format!("{wn}_creator_sold"), w.iter().any(|x| !x.buy && !p.creator.is_empty() && x.trader == p.creator) as u8 as f64);
    }
    // buy-side recovery in the last 10 s vs the decline since the high
    let lows = s[hi_idx..=ev].iter().map(|w| w.px).fold(f64::INFINITY, f64::min);
    put("bounce_from_low", (s[ev].px / lows).ln());
    m
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pools = load_pools(&a[2..]);
    let tau: f64 = std::env::var("ENTRY_TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    let liq_min: f64 = std::env::var("LIQ_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let mut ids: Vec<(f64, &String)> = pools.iter().filter(|(_, p)| p.swaps.len() >= 10).map(|(k, p)| (p.swaps[0].t, k)).collect();
    ids.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let cut = (ids.len() as f64 * 0.6) as usize;
    let train: HashSet<&String> = ids[..cut].iter().map(|x| x.1).collect();
    let mut f = std::fs::File::create(&a[1]).expect("out");
    let (mut n, mut pos) = (0, 0);
    for (_, k) in &ids {
        let p = &pools[*k];
        let s = &p.swaps;
        let t0 = s[0].t;
        let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        if rug <= t0 + tau { continue }
        let Some(entry) = s.iter().position(|w| w.t > t0 + tau) else { continue };
        if s[entry].liq < liq_min { continue }
        for d in DEPTHS {
            let mut hi_idx = entry;
            let mut fired_for_high = false;
            for i in entry + 1..s.len() {
                if s[i].t >= rug { break }
                if s[i].px > s[hi_idx].px { hi_idx = i; fired_for_high = false; continue }
                // only pullbacks from a high that is at least +20 % above entry (a run to protect)
                if fired_for_high || s[hi_idx].px < 1.2 * s[entry].px { continue }
                if s[i].px <= s[hi_idx].px * (1.0 - d) {
                    fired_for_high = true;
                    let hi = s[hi_idx].px;
                    let later: Vec<&Sw> = s[i + 1..].iter().filter(|w| w.t < rug).collect();
                    let naik_lagi = later.iter().any(|w| w.px > hi);
                    let naik_50 = later.iter().any(|w| w.px > 1.5 * hi);
                    let best_after = later.iter().map(|w| w.px).fold(s[i].px, f64::max) / s[i].px;
                    let row = json!({"tau": (d * 100.0).round(), "set": if train.contains(*k) { "train" } else { "test" }, "pool": k, "t0": s[i].t,
                        "naik_lagi": naik_lagi, "naik_50": naik_50, "best_after_mult": best_after, "rug_after": rug.is_finite(), "feat": feats(p, i, entry, hi_idx), "ret": {}});
                    let _ = writeln!(f, "{row}");
                    n += 1;
                    pos += naik_lagi as usize;
                }
            }
        }
    }
    eprintln!("pullback events {n}, recovered to a new high {pos}");
}
