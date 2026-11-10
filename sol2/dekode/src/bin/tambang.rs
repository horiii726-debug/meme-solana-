//! Data-driven feature mining: generates every combination of window x side x statistic from the decoded swaps
//! (plus wallet-behaviour, slot-clustering, timing, price-path, liquidity and failed-tx features), all causal
//! (only events with t <= t0 + tau). Writes a DUMP for `permutasi`, which screens features on TRAIN and validates
//! survivors on TEST. No hypothesis-picked shortlist: the data selects.
//!   tambang <dump_out.jsonl> <raw_file>...        env LIQ_MIN (SOL at tau, default 1)
use dekode::series::*;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;

const TAUS: [f64; 5] = [5.0, 10.0, 15.0, 30.0, 60.0];

fn q(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() { return 0.0 }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p) as usize]
}

fn mine(p: &Pool, tau: f64) -> Map<String, Value> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let x: Vec<&Sw> = s.iter().filter(|w| w.t <= t0 + tau).collect();
    let mut m = Map::new();
    let mut put = |k: String, v: f64| { m.insert(k, if v.is_finite() { json!(v) } else { Value::Null }); };
    // windows: whole [t0, t0+tau], last 5 s, last third
    let wins: Vec<(&str, f64)> = vec![("all", t0), ("last5", t0 + tau - 5.0), ("last3rd", t0 + tau * 2.0 / 3.0)];
    for (wn, from) in &wins {
        let w: Vec<&&Sw> = x.iter().filter(|e| e.t >= *from).collect();
        let len = (t0 + tau - from).max(1.0);
        for (sn, side) in [("buy", Some(true)), ("sell", Some(false)), ("any", None)] {
            let mut v: Vec<f64> = w.iter().filter(|e| side.map_or(true, |b| e.buy == b)).map(|e| e.sol).collect();
            let n = v.len() as f64;
            let sum: f64 = v.iter().sum();
            let wallets: HashSet<&str> = w.iter().filter(|e| side.map_or(true, |b| e.buy == b)).map(|e| e.trader.as_str()).collect();
            put(format!("{wn}_{sn}_n"), n);
            put(format!("{wn}_{sn}_n_per_s"), n / len);
            put(format!("{wn}_{sn}_sol"), sum);
            put(format!("{wn}_{sn}_wallets"), wallets.len() as f64);
            put(format!("{wn}_{sn}_mean"), if n > 0.0 { sum / n } else { 0.0 });
            put(format!("{wn}_{sn}_med"), q(&mut v, 0.5));
            put(format!("{wn}_{sn}_p90"), q(&mut v, 0.9));
            put(format!("{wn}_{sn}_max"), v.iter().cloned().fold(0.0, f64::max));
        }
        let (bs, ss): (f64, f64) = (w.iter().filter(|e| e.buy).map(|e| e.sol).sum(), w.iter().filter(|e| !e.buy).map(|e| e.sol).sum());
        let (bn, sn) = (w.iter().filter(|e| e.buy).count() as f64, w.iter().filter(|e| !e.buy).count() as f64);
        put(format!("{wn}_buy_share_sol"), bs / (bs + ss + 1e-12));
        put(format!("{wn}_buy_share_n"), bn / (bn + sn + 1e-12));
        put(format!("{wn}_net_sol"), bs - ss);
        if let (Some(a), Some(b)) = (w.first(), w.last()) { put(format!("{wn}_ret"), (b.px / a.px).ln()) } else { put(format!("{wn}_ret"), 0.0) }
    }
    // wallet behaviour over the whole window
    let mut buys_by: HashMap<&str, (u32, f64)> = HashMap::new();
    let mut sold: HashSet<&str> = HashSet::new();
    for e in &x {
        if e.buy { let b = buys_by.entry(&e.trader).or_insert((0, 0.0)); b.0 += 1; b.1 += e.sol } else { sold.insert(&e.trader); }
    }
    let tb: f64 = buys_by.values().map(|b| b.1).sum();
    let nb = buys_by.len().max(1) as f64;
    put("wal_flipper_share".into(), buys_by.keys().filter(|k| sold.contains(*k)).count() as f64 / nb);
    put("wal_repeat_buyer_share".into(), buys_by.values().filter(|b| b.0 >= 2).count() as f64 / nb);
    put("wal_repeat_sol_share".into(), buys_by.values().filter(|b| b.0 >= 2).map(|b| b.1).sum::<f64>() / tb.max(1e-12));
    let mut shares: Vec<f64> = buys_by.values().map(|b| b.1 / tb.max(1e-12)).collect();
    shares.sort_by(|a, b| b.partial_cmp(a).unwrap());
    put("wal_top1".into(), shares.first().cloned().unwrap_or(0.0));
    put("wal_top5".into(), shares.iter().take(5).sum());
    put("wal_hhi".into(), shares.iter().map(|s| s * s).sum());
    put("wal_creator_bought".into(), (!p.creator.is_empty() && buys_by.contains_key(p.creator.as_str())) as u8 as f64);
    put("wal_creator_sold".into(), (!p.creator.is_empty() && sold.contains(p.creator.as_str())) as u8 as f64);
    // slot clustering: bundles / snipers
    let mut by_slot: HashMap<u64, HashSet<&str>> = HashMap::new();
    let mut sol_slot: HashMap<u64, f64> = HashMap::new();
    for e in x.iter().filter(|e| e.buy) { by_slot.entry(e.slot).or_default().insert(&e.trader); *sol_slot.entry(e.slot).or_insert(0.0) += e.sol; }
    put("slot_max_buyers".into(), by_slot.values().map(|v| v.len()).max().unwrap_or(0) as f64);
    put("slot_bundle_sol_share".into(), by_slot.iter().filter(|(_, v)| v.len() >= 3).map(|(k, _)| sol_slot[k]).sum::<f64>() / tb.max(1e-12));
    let s0 = s[0].slot;
    put("slot_first_buyers".into(), by_slot.get(&s0).map_or(0, |v| v.len()) as f64);
    put("slot_first2_sol".into(), x.iter().filter(|e| e.buy && e.slot <= s0 + 2).map(|e| e.sol).sum());
    put("slot_distinct".into(), x.iter().map(|e| e.slot).collect::<HashSet<_>>().len() as f64);
    // timing
    let gaps: Vec<f64> = x.windows(2).map(|w| w[1].t - w[0].t).collect();
    let gm = gaps.iter().sum::<f64>() / gaps.len().max(1) as f64;
    let gsd = (gaps.iter().map(|g| (g - gm).powi(2)).sum::<f64>() / gaps.len().max(1) as f64).sqrt();
    put("time_gap_mean".into(), gm);
    put("time_gap_cv".into(), gsd / gm.max(1e-9));
    let half = x.iter().filter(|e| e.t > t0 + tau / 2.0).count() as f64;
    put("time_accel".into(), half / (x.len() as f64 - half).max(1.0));
    put("time_to_10th".into(), x.get(9).map_or(tau * 2.0, |e| e.t - t0));
    put("time_to_25th".into(), x.get(24).map_or(tau * 2.0, |e| e.t - t0));
    // price path
    let p0 = s[0].px;
    let pmax = x.iter().map(|e| e.px).fold(p0, f64::max);
    let pmin = x.iter().map(|e| e.px).fold(p0, f64::min);
    let plast = x.last().map_or(p0, |e| e.px);
    put("px_ret".into(), (plast / p0).ln());
    put("px_max_ret".into(), (pmax / p0).ln());
    put("px_min_ret".into(), (pmin / p0).ln());
    put("px_dd_from_max".into(), (plast / pmax).ln());
    let (mut up, mut dn) = (0.0f64, 0.0f64);
    for w in x.windows(2) { if w[1].px > w[0].px { up += 1.0 } else if w[1].px < w[0].px { dn += 1.0 } }
    put("px_up_share".into(), up / (up + dn).max(1.0));
    // liquidity
    let l0 = s[0].liq;
    let ll = x.last().map_or(l0, |e| e.liq);
    put("liq_sol".into(), ll);
    put("liq_start".into(), l0);
    put("liq_dlog".into(), (ll.max(1e-9) / l0.max(1e-9)).ln());
    put("liq_lock_events".into(), p.lp_lock.iter().filter(|t| **t <= t0 + tau).count() as f64);
    // failed transactions (bots competing)
    let nf = p.failed.iter().filter(|t| **t >= t0 && **t <= t0 + tau).count() as f64;
    put("fail_n".into(), nf);
    put("fail_share".into(), nf / (nf + x.len() as f64).max(1.0));
    put("venue_pumpswap".into(), (p.venue == "pumpswap") as u8 as f64);
    m
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pools = load_pools(&a[2..]);
    let liq_min: f64 = std::env::var("LIQ_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let mut ids: Vec<(f64, String)> = pools.iter().filter(|(_, p)| p.swaps.len() >= 5 && p.swaps[0].liq > 0.0).map(|(k, p)| (p.swaps[0].t, k.clone())).collect();
    ids.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let cut = (ids.len() as f64 * 0.6) as usize;
    let train: HashSet<String> = ids[..cut].iter().map(|x| x.1.clone()).collect();
    let exits = [Exit::Hold, Exit::Trail(0.3), Exit::Trail(0.5), Exit::Ladder(2.0, 0.5), Exit::Ladder(3.0, 0.5), Exit::Ladder(5.0, 0.5), Exit::Time(60.0), Exit::Time(120.0), Exit::Time(300.0), Exit::Time(1800.0), Exit::Launch(90.0), Exit::Launch(150.0), Exit::Launch(240.0), Exit::Flow(10.0, 1.5), Exit::Flow(10.0, 2.0), Exit::Flow(20.0, 1.5), Exit::Flow(20.0, 3.0)];
    let reach = |p: &Pool, mult: f64, tau: f64| -> bool {
        let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        let i0 = p.swaps.iter().position(|w| w.t > p.swaps[0].t + tau);
        i0.map_or(false, |i| p.swaps[i..].iter().filter(|w| w.t < rug).any(|w| w.px >= mult * p.swaps[i].px))
    };
    let mut f = std::fs::File::create(&a[1]).expect("out");
    let (mut n, mut nf) = (0, 0);
    for (_, k) in &ids {
        let p = &pools[k];
        for tau in TAUS {
            if p.swaps.last().unwrap().t < p.swaps[0].t + tau { continue }
            if p.lp_out.first().map_or(false, |r| r.0 <= p.swaps[0].t + tau) { continue }
            let feat = mine(p, tau);
            if feat["liq_sol"].as_f64().unwrap_or(0.0) < liq_min { continue }
            nf = feat.len();
            let rets: Map<String, Value> = exits.iter().map(|e| (exit_name(e), json!(trade(p, tau, *e)))).collect();
            // the same exits replayed against REAL fills (real buyer/seller prices, real meta.fee, exits fail when no real seller)
            let real: Map<String, Value> = exits.iter().map(|e| (exit_name(e), trade_real(p, tau, *e).map_or(Value::Null, |r| json!({"ret": r.ret, "exits": r.exits, "failed_exits": r.failed_exits, "fees_sol": r.fees_sol, "held_into_rug": r.held_into_rug})))).collect();
            let _ = writeln!(f, "{}", json!({"tau": tau, "set": if train.contains(k) { "train" } else { "test" }, "pool": k, "t0": p.swaps[0].t,
                "y2x": reach(p, 2.0, tau), "rug": !p.lp_out.is_empty(), "feat": feat, "ret": rets, "real": real}));
            n += 1;
        }
    }
    eprintln!("rows {n}, features per row {nf}, pools {} (train {}, test {})", ids.len(), cut, ids.len() - cut);
}
