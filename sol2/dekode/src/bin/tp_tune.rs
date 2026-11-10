//! Walk-forward tuning of the take-profit plan on the execution engine (verified pool math, 2-slot latency, real
//! fees, real failure rates with retries). Pools sorted by t0 into 5 time blocks; for each test block k >= 1 the plan is
//! chosen on blocks < k only (objective: mean return with the 3 best trades removed = robust to lucky outliers) and
//! then evaluated on block k. Reports the out-of-sample result of the chosen plans and the plans' stability.
//!   tp_tune <raw_file>...    env TAU (default 10), LIQ_MIN (default 1), SEEDS (default 3)
use dekode::series::*;
use serde_json::json;

fn trimmed_mean(v: &[f64], k: usize) -> f64 {
    if v.len() <= k + 2 { return f64::NEG_INFINITY }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[..s.len() - k].iter().sum::<f64>() / (s.len() - k) as f64
}

fn stats(r: &[f64]) -> serde_json::Value {
    if r.len() < 3 { return json!({"n": r.len()}) }
    let n = r.len() as f64;
    let mut v = r.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / n;
    let mut seed = 5u64;
    let mut bm: Vec<f64> = (0..10000).map(|_| (0..r.len()).map(|_| { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; r[(seed % r.len() as u64) as usize] }).sum::<f64>() / n).collect();
    bm.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({"n": r.len(), "mean": r3(mean), "median": r3(v[v.len() / 2]), "mean_without_top3": r3(trimmed_mean(r, 3)), "win_rate": r3(r.iter().filter(|x| **x > 0.0).count() as f64 / n),
        "ge_100pct": r.iter().filter(|x| **x >= 1.0).count(), "ge_500pct": r.iter().filter(|x| **x >= 5.0).count(), "worst": r3(v[0]), "best": r3(v[v.len() - 1]), "ci95_mean": [r3(bm[250]), r3(bm[9749])]})
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    let liq_min: f64 = std::env::var("LIQ_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let seeds: u64 = std::env::var("SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let pools = load_pools(&files);
    let mut ids: Vec<(f64, &Pool)> = pools.values().filter(|p| p.swaps.len() >= 10).filter_map(|p| {
        let t0 = p.swaps[0].t;
        let i = p.swaps.iter().position(|w| w.t > t0 + tau)?;
        (p.swaps[i].liq >= liq_min).then_some((t0, p))
    }).collect();
    ids.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    // plan grid
    let mut plans: Vec<Plan> = vec![];
    for (m1, f1) in [(1.5, 0.3), (1.5, 0.5), (2.0, 0.3), (2.0, 0.5), (2.0, 0.7), (3.0, 0.3), (3.0, 0.5)] {
        for t2 in [None, Some((3.0, 0.2)), Some((5.0, 0.2)), Some((5.0, 0.3))] {
            for t3 in [None, Some((10.0, 0.1)), Some((10.0, 0.2))] {
                for trail in [0.3, 0.5, 0.7] {
                    for max_age in [120.0, 180.0, 300.0, 600.0] {
                        let mut tiers = vec![(m1, f1)];
                        if let Some(t) = t2 { if t.0 > m1 { tiers.push(t) } }
                        if let Some(t) = t3 { tiers.push(t) }
                        if tiers.iter().map(|t| t.1).sum::<f64>() > 0.95 { continue }
                        plans.push(Plan { tiers, trail, max_age, mom: None, be: None, ratchet: None });
                    }
                }
            }
        }
    }
    eprintln!("pools {}, plans {}, seeds {seeds}", ids.len(), plans.len());
    // results[plan][pool] = mean return over seeds (None = no entry)
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let chunk = (plans.len() / cores).max(1);
    let res: Vec<Vec<Option<f64>>> = std::thread::scope(|sc| {
        plans.chunks(chunk).map(|pc| { let ids = &ids; sc.spawn(move || pc.iter().map(|pl| ids.iter().map(|(_, p)| {
            let v: Vec<f64> = (0..seeds).filter_map(|sd| trade_amm(p, tau, pl, sd).map(|e| e.ret)).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        }).collect::<Vec<_>>()).collect::<Vec<_>>()) }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let n = ids.len();
    let block = |i: usize| (i * 5 / n).min(4);
    let mut oos: Vec<f64> = vec![];
    let mut chosen = vec![];
    for k in 1..5 {
        let tr: Vec<usize> = (0..n).filter(|i| block(*i) < k).collect();
        let te: Vec<usize> = (0..n).filter(|i| block(*i) == k).collect();
        let (best, score) = (0..plans.len()).map(|pi| {
            let v: Vec<f64> = tr.iter().filter_map(|i| res[pi][*i]).collect();
            (pi, trimmed_mean(&v, 3))
        }).max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap();
        let v: Vec<f64> = te.iter().filter_map(|i| res[best][*i]).collect();
        oos.extend(v.iter().cloned());
        chosen.push(json!({"fold": k, "plan": format!("{:?}", plans[best]), "train_trimmed_mean": (score * 1000.0).round() / 1000.0, "test": stats(&v)}));
    }
    println!("{}", json!({"tau": tau, "pools": n, "plans_tried": plans.len(), "folds": chosen, "out_of_sample_all_folds": stats(&oos)}));
    // baseline plans for reference (whole sample)
    for (name, pl) in [("sell_all_at_age_150s", Plan { tiers: vec![], trail: 1.0, max_age: 150.0, mom: None, be: None, ratchet: None }), ("half_2x_trail50_age300", Plan { tiers: vec![(2.0, 0.5)], trail: 0.5, max_age: 300.0, mom: None, be: None, ratchet: None })] {
        let v: Vec<f64> = ids.iter().filter_map(|(_, p)| trade_amm(p, tau, &pl, 0).map(|e| e.ret)).collect();
        println!("{}", json!({"baseline": name, "all_pools": stats(&v)}));
    }
}
