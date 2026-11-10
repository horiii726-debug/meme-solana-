//! Moonshot take-profit search on REAL transactions. Coins = only those the entry model selected (out-of-sample scores
//! from sistem2, per block threshold). For ~2,700 exit plans (tiers at 2/3/5/10/20x, trailing width, ML exit threshold
//! from exit_ml out-of-sample triggers, age cap) every trade is replayed with the execution engine (latency, real fees,
//! failure retries, 3 seeds).
//! Objective = Kelly growth g = max_f mean ln(1 + f r) (the right criterion for positive-skew payoffs); the plan is chosen
//! on EARLIER out-of-sample blocks only and evaluated on the next block (nested, no look-ahead). Also reported: how much
//! of the moonshots (peak >= 10x after entry, before the LP removal) the plans captured.
//!   tp_moon <sistem2.json> <exit_ml.json> <raw_file>...   env TAU (10)
use dekode::series::*;
use serde_json::{json, Value};
use std::collections::HashMap;

fn kelly_g(r: &[f64]) -> (f64, f64) {
    if r.len() < 10 { return (0.0, f64::NEG_INFINITY) }
    let mut best = (0.0, 0.0);
    let mut f = 0.005;
    while f <= 1.0 { let g = r.iter().map(|x| (1.0 + f * x).max(1e-12).ln()).sum::<f64>() / r.len() as f64; if g > best.1 { best = (f, g) } f += 0.005 }
    best
}

fn stats(r: &[f64]) -> Value {
    if r.len() < 3 { return json!({"n": r.len()}) }
    let n = r.len() as f64;
    let mut v = r.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / n;
    let wo3 = if v.len() > 5 { v[..v.len() - 3].iter().sum::<f64>() / (n - 3.0) } else { f64::NAN };
    let (kf, kg) = kelly_g(r);
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({"n": r.len(), "mean": r3(mean), "median": r3(v[v.len() / 2]), "mean_without_top3": r3(wo3), "win_rate": r3(r.iter().filter(|x| **x > 0.0).count() as f64 / n),
        "ge_100pct": r.iter().filter(|x| **x >= 1.0).count(), "ge_500pct": r.iter().filter(|x| **x >= 5.0).count(), "best": r3(v[v.len() - 1]), "kelly_f": r3(kf), "kelly_growth_per_trade": (kg * 10000.0).round() / 10000.0})
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    let s2: Value = serde_json::from_str(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let ex: Value = serde_json::from_str(&std::fs::read_to_string(&a[2]).unwrap()).unwrap();
    let thetas: Vec<f64> = ex["THETAS"].as_array().unwrap().iter().filter_map(|x| x.as_f64()).collect();
    let mut trig: HashMap<(String, usize), f64> = HashMap::new();
    for t in ex["TRIGGERS"].as_array().into_iter().flatten() { trig.insert((t[0].as_str().unwrap().into(), t[1].as_u64().unwrap() as usize), t[2].as_f64().unwrap()); }
    // selected coins per block (entry model, out-of-sample)
    let mut sel: Vec<(String, usize)> = s2["ENTRY_SCORES_OOS"].as_array().unwrap().iter()
        .filter(|r| r["score"].as_f64().unwrap() >= r["thr"].as_f64().unwrap()).map(|r| (r["pool"].as_str().unwrap().into(), r["block"].as_u64().unwrap() as usize)).collect();
    sel.sort();
    sel.dedup();
    let pools = load_pools(&a[3..]);
    // plan grid
    let mut plans: Vec<(Plan, Option<usize>)> = vec![];
    for (m1, f1) in [(2.0, 0.0), (2.0, 0.2), (2.0, 0.4), (3.0, 0.2), (3.0, 0.4), (5.0, 0.2), (5.0, 0.4)] {
        for t2 in [None, Some((10.0, 0.2)), Some((10.0, 0.3)), Some((20.0, 0.2)), Some((20.0, 0.3))] {
            for trail in [0.3, 0.5, 0.7, 1.0] {
                for ml in [None, Some(0.5), Some(0.7), Some(0.8), Some(0.9)] {
                    for max_age in [300.0, 900.0, 1e9] {
                        let mut tiers = vec![];
                        if f1 > 0.0 { tiers.push((m1, f1)) }
                        if let Some(t) = t2 { if t.0 > m1 { tiers.push(t) } }
                        if f1 == 0.0 && m1 != 2.0 { continue }
                        let ti = ml.and_then(|th: f64| thetas.iter().position(|x| (x - th).abs() < 1e-9));
                        if ml.is_some() && ti.is_none() { continue }
                        // break-even stop variants: off, or after +5 % / +10 % / +20 % move the stop to the entry price
                        for be in [None, Some((0.05, 1.0)), Some((0.10, 1.0)), Some((0.20, 1.0))] {
                            plans.push((Plan { tiers: tiers.clone(), trail, max_age, mom: None, be, ratchet: None }, ti));
                        }
                    }
                }
            }
        }
    }
    // MODE=ratchet: no partial sells; break-even after +5 % (optional) and a ratchet stop at r x the high once the high
    // reached `start` x entry; optional ML exit; age cap 15 min or none
    if std::env::var("MODE").map(|m| m == "ratchet").unwrap_or(false) {
        plans.clear();
        for be in [None, Some((0.05, 1.0))] { for start in [2.0, 3.0, 5.0, 11.0] { for r in [0.15, 0.2, 0.3, 0.4, 0.5] { for ml in [None, Some(0.5), Some(0.6), Some(0.7), Some(0.8), Some(0.9)] { for max_age in [900.0, 1e9] {
            let ti = ml.and_then(|th: f64| thetas.iter().position(|x| (x - th).abs() < 1e-9));
            plans.push((Plan { tiers: vec![], trail: 1.0, max_age, mom: None, be, ratchet: Some((start, r)) }, ti));
        } } } } }
    }
    eprintln!("selected coins {}, plans {}", sel.len(), plans.len());
    // returns: plan x selected coin (mean of 3 seeds)
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let chunk = (plans.len() / cores).max(1);
    let res: Vec<Vec<Option<f64>>> = std::thread::scope(|sc| plans.chunks(chunk).map(|pc| { let (sel, pools, trig) = (&sel, &pools, &trig); sc.spawn(move || pc.iter().map(|(pl, ti)| sel.iter().map(|(k, _)| {
        let p = pools.get(k)?;
        let ex_at = ti.and_then(|t| trig.get(&(k.clone(), t)).cloned()).unwrap_or(f64::INFINITY);
        let v: Vec<f64> = (0..3).filter_map(|sd| trade_amm_ext(p, tau, pl, sd, ex_at).map(|e| e.ret)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    }).collect::<Vec<_>>()).collect::<Vec<_>>()) }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect());
    // moonshots among the selected coins
    let is_moon: Vec<bool> = sel.iter().map(|(k, _)| pools.get(k).map_or(false, |p| {
        let t0 = p.swaps[0].t; let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        p.swaps.iter().position(|w| w.t > t0 + tau).map_or(false, |e| p.swaps[e..].iter().filter(|w| w.t < rug).map(|w| w.px).fold(0.0, f64::max) >= 10.0 * p.swaps[e].px)
    })).collect();
    let pick = |pi: usize, f: &dyn Fn(usize) -> bool| -> Vec<f64> { sel.iter().enumerate().filter(|(_, (_, b))| f(*b)).filter_map(|(i, _)| res[pi][i]).collect() };
    let name = |pi: usize| -> String { let (pl, ti) = &plans[pi]; format!("tiers {:?} | BE {:?} | ratchet {:?} | trail {} | ML {} | age {}", pl.tiers, pl.be, pl.ratchet, pl.trail, ti.map_or("off".to_string(), |t| thetas[t].to_string()), if pl.max_age > 1e8 { "none".into() } else { pl.max_age.to_string() }) };
    // nested: choose on earlier OOS blocks by Kelly growth, evaluate on the next block
    let (mut oos, mut folds) = (vec![], vec![]);
    for k in 2..5 {
        let best = (0..plans.len()).map(|pi| (pi, kelly_g(&pick(pi, &|b| b >= 1 && b < k)).1)).max_by(|x, y| x.1.partial_cmp(&y.1).unwrap()).unwrap();
        let v = pick(best.0, &|b| b == k);
        oos.extend(v.iter().cloned());
        folds.push(json!({"fold": k, "chosen": name(best.0), "past_kelly_growth": (best.1 * 10000.0).round() / 10000.0, "result": stats(&v)}));
    }
    // descriptive: top 10 plans by Kelly growth over all OOS blocks + their moonshot capture
    let mut ranked: Vec<(usize, f64)> = (0..plans.len()).map(|pi| (pi, kelly_g(&pick(pi, &|b| b >= 1)).1)).collect();
    ranked.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());
    let moon_cap = |pi: usize| -> f64 { let v: Vec<f64> = sel.iter().enumerate().filter(|(i, _)| is_moon[*i]).filter_map(|(i, _)| res[pi][i]).collect(); if v.is_empty() { f64::NAN } else { (v.iter().sum::<f64>() / v.len() as f64 * 1000.0).round() / 1000.0 } };
    let top: Vec<Value> = ranked.iter().take(10).map(|(pi, _)| json!({"plan": name(*pi), "all_oos": stats(&pick(*pi, &|b| b >= 1)), "moonshot_mean_ret": moon_cap(*pi)})).collect();
    // every plan (descriptive), for side-by-side comparison of hybrids
    if let Ok(path) = std::env::var("ALL_OUT") {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        for pi in 0..plans.len() { let _ = writeln!(f, "{}", json!({"plan": name(pi), "tiers": format!("{:?}", plans[pi].0.tiers), "trail": plans[pi].0.trail, "ml": plans[pi].1.map(|t| thetas[t]), "age": plans[pi].0.max_age, "all_oos": stats(&pick(pi, &|b| b >= 1)), "moonshot_mean_ret": moon_cap(pi)})); }
    }
    let default_pi = plans.iter().position(|(p, ti)| p.tiers == vec![(2.0, 0.4)] && p.trail == 0.3 && ti.is_none() && p.max_age == 300.0);
    println!("{}", json!({"tau": tau, "selected_coins": sel.len(), "moonshots_in_selected": is_moon.iter().filter(|x| **x).count(), "plans": plans.len(),
        "NESTED_folds": folds, "NESTED_oos_blocks_2_4": stats(&oos), "TOP10_descriptive": top, "reference_plan": default_pi.map(|pi| json!({"plan": name(pi), "all_oos": stats(&pick(pi, &|b| b >= 1)), "moonshot_mean_ret": moon_cap(pi)}))}));
}
