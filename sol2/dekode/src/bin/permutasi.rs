//! Statistical validation of features and of the combined system (input: `quant` DUMP file).
//!   permutasi <dump.jsonl> [n_perm=1000000] [n_boot=10000] [exit=time300s]
//! Per decision time tau and per feature, on the TEST pools only (later in time than train):
//!   - AUC for "reaches 2x from entry before LP removal", direction fixed on TRAIN;
//!   - permutation p-value: labels shuffled n_perm times (exact rank-sum resampling), one-sided;
//!   - bootstrap 95 % CI of the AUC (n_boot resamples);
//!   - Holm-Bonferroni adjustment over all (tau, feature) tests.
//! Combined system: z-scored sum of the features whose TRAIN permutation p < 0.05 (signs from train), evaluated on test
//! with the same permutation test. Then a Monte Carlo of 1,000,000 equity paths: test trades of the top 30 % scored pools,
//! resampled with replacement, 1/4 Kelly fraction estimated on TRAIN trades (owner rule), compounding.
use serde_json::{json, Value};
use std::collections::BTreeMap;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
}

fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|a, b| v[*a].partial_cmp(&v[*b]).unwrap());
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] { j += 1 }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j { r[idx[k]] = avg }
        i = j + 1;
    }
    r
}

fn auc(x: &[f64], y: &[bool]) -> f64 {
    let r = ranks(x);
    let np = y.iter().filter(|b| **b).count() as f64;
    let nn = y.len() as f64 - np;
    if np == 0.0 || nn == 0.0 { return f64::NAN }
    (r.iter().zip(y).filter(|(_, b)| **b).map(|(r, _)| r).sum::<f64>() - np * (np + 1.0) / 2.0) / (np * nn)
}

/// one-sided permutation p-value of AUC >= observed, by resampling which ranks are "positive"
fn perm_p(x: &[f64], y: &[bool], n_perm: usize, seed: u64) -> f64 {
    let r = ranks(x);
    let np = y.iter().filter(|b| **b).count();
    let nn = y.len() - np;
    if np == 0 || nn == 0 { return f64::NAN }
    let obs: f64 = r.iter().zip(y).filter(|(_, b)| **b).map(|(r, _)| r).sum();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(16);
    let per = n_perm / threads + 1;
    let hits: usize = std::thread::scope(|s| {
        (0..threads).map(|t| {
            let r = r.clone();
            s.spawn(move || {
                let mut g = Rng(seed ^ (0x9E3779B97F4A7C15u64.wrapping_mul(t as u64 + 1)));
                let mut idx: Vec<usize> = (0..r.len()).collect();
                let mut h = 0usize;
                for _ in 0..per {
                    let mut sum = 0.0;
                    for i in 0..np {
                        let j = i + g.below(idx.len() - i);
                        idx.swap(i, j);
                        sum += r[idx[i]];
                    }
                    if sum >= obs - 1e-9 { h += 1 }
                }
                h
            })
        }).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).sum()
    });
    (hits as f64 + 1.0) / ((per * threads) as f64 + 1.0)
}

fn boot_ci(x: &[f64], y: &[bool], n: usize) -> [f64; 2] {
    let mut g = Rng(42);
    let mut v: Vec<f64> = (0..n).filter_map(|_| {
        let ix: Vec<usize> = (0..x.len()).map(|_| g.below(x.len())).collect();
        let a = auc(&ix.iter().map(|i| x[*i]).collect::<Vec<_>>(), &ix.iter().map(|i| y[*i]).collect::<Vec<_>>());
        a.is_finite().then_some(a)
    }).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() { return [f64::NAN, f64::NAN] }
    [v[(v.len() as f64 * 0.025) as usize], v[((v.len() as f64 * 0.975) as usize).min(v.len() - 1)]]
}

fn kelly(r: &[f64]) -> f64 {
    // f* = argmax mean ln(1 + f r), f in 0.005..0.6 (same rule as uji main.rs:528)
    let mut best = (0.0, f64::NEG_INFINITY);
    let mut f = 0.005;
    while f <= 0.6 {
        let g: f64 = r.iter().map(|x| (1.0 + f * x).max(1e-12).ln()).sum::<f64>() / r.len().max(1) as f64;
        if g > best.1 { best = (f, g) }
        f += 0.005;
    }
    best.0
}

fn r3(x: f64) -> f64 { (x * 1000.0).round() / 1000.0 }

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n_perm: usize = a.get(2).and_then(|x| x.parse().ok()).unwrap_or(1_000_000);
    let n_boot: usize = a.get(3).and_then(|x| x.parse().ok()).unwrap_or(10_000);
    let exit = a.get(4).cloned().unwrap_or_else(|| "time300s".into());
    // target field in the dump: y2x (reaches 2x) or rug5 (LP removed within 5 min) ...
    let label = a.get(5).cloned().unwrap_or_else(|| "y2x".into());
    let label = label.as_str();
    let rows: Vec<Value> = std::fs::read_to_string(&a[1]).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut taus: Vec<f64> = rows.iter().filter_map(|r| r["tau"].as_f64()).collect();
    taus.sort_by(|a, b| a.partial_cmp(b).unwrap());
    taus.dedup();
    let feats: Vec<String> = rows.first().map(|r| r["feat"].as_object().unwrap().keys().cloned().collect()).unwrap_or_default();
    let mut tests: Vec<(f64, String, f64, f64, [f64; 2], f64, bool, usize, usize)> = vec![]; // tau, feat, auc_tr, auc_te, ci, p, higher_better, n_te, pos_te
    for tau in &taus {
        let sel = |set: &str| -> Vec<&Value> { rows.iter().filter(|r| r["tau"].as_f64() == Some(*tau) && r["set"] == set).collect() };
        let (tr, te) = (sel("train"), sel("test"));
        for f in &feats {
            let col = |v: &[&Value]| -> (Vec<f64>, Vec<bool>) {
                v.iter().filter_map(|r| r["feat"][f].as_f64().map(|x| (x, r[label].as_bool().unwrap_or(false)))).unzip()
            };
            let ((xtr, ytr), (xte, yte)) = (col(&tr), col(&te));
            if xtr.len() < 20 || xte.len() < 15 { continue }
            let atr = auc(&xtr, &ytr);
            if !atr.is_finite() { continue }
            let sgn = if atr < 0.5 { -1.0 } else { 1.0 };
            // screening on TRAIN only (100,000 permutations); only survivors are tested on TEST
            let p_tr = perm_p(&xtr.iter().map(|x| sgn * x).collect::<Vec<_>>(), &ytr, 100_000, 3 + tests.len() as u64);
            if !(p_tr < 0.05) { continue }
            let xte_s: Vec<f64> = xte.iter().map(|x| sgn * x).collect();
            let ate = auc(&xte_s, &yte);
            if !ate.is_finite() { continue }
            let p = perm_p(&xte_s, &yte, n_perm, 7 + tests.len() as u64);
            let ci = boot_ci(&xte_s, &yte, n_boot);
            tests.push((*tau, f.clone(), if sgn < 0.0 { 1.0 - atr } else { atr }, ate, ci, p, sgn > 0.0, xte.len(), yte.iter().filter(|b| **b).count()));
        }
    }
    // Holm-Bonferroni
    let m = tests.len();
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|a, b| tests[*a].5.partial_cmp(&tests[*b].5).unwrap());
    let mut holm = vec![1.0; m];
    let mut running: f64 = 0.0;
    for (rank, i) in order.iter().enumerate() {
        running = running.max(((m - rank) as f64 * tests[*i].5).min(1.0));
        holm[*i] = running;
    }
    let mut out: Vec<(f64, Value)> = tests.iter().enumerate().map(|(i, t)| (t.5, json!({"tau": t.0, "feature": t.1, "higher_is_better": t.6, "auc_train": r3(t.2), "auc_test": r3(t.3),
        "auc_test_ci95": [r3(t.4[0]), r3(t.4[1])], "perm_p": t.5, "holm_p": holm[i], "n_test": t.7, "pos_test": t.8}))).collect();
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    println!("{}", json!({"tests": m, "permutations_per_test": n_perm, "bootstrap": n_boot}));
    for (_, v) in &out { println!("{v}") }
    if rows.first().map_or(true, |r| r.get("ret").is_none()) { return }
    // combined system per tau: features with TRAIN permutation p < 0.05, z-scored, signed, summed
    for tau in &taus {
        let sel = |set: &str| -> Vec<&Value> { rows.iter().filter(|r| r["tau"].as_f64() == Some(*tau) && r["set"] == set).collect() };
        let (tr, te) = (sel("train"), sel("test"));
        if tr.len() < 20 || te.len() < 15 { continue }
        let mut chosen = vec![];
        for f in &feats {
            let (x, y): (Vec<f64>, Vec<bool>) = tr.iter().filter_map(|r| r["feat"][f].as_f64().map(|v| (v, r[label].as_bool().unwrap_or(false)))).unzip();
            if x.len() < 20 { continue }
            let at = auc(&x, &y);
            if !at.is_finite() { continue }
            let sgn = if at < 0.5 { -1.0 } else { 1.0 };
            let p = perm_p(&x.iter().map(|v| sgn * v).collect::<Vec<_>>(), &y, 100_000, 99);
            if p < 0.05 {
                let mu = x.iter().sum::<f64>() / x.len() as f64;
                let sd = (x.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / x.len() as f64).sqrt().max(1e-12);
                chosen.push((f.clone(), sgn, mu, sd));
            }
        }
        if chosen.is_empty() {
            println!("{}", json!({"system_tau": tau, "chosen_features": [], "note": "no feature passed p<0.05 on train"}));
            continue;
        }
        let score = |r: &Value| -> f64 { chosen.iter().map(|(f, s, mu, sd)| s * (r["feat"][f].as_f64().unwrap_or(*mu) - mu) / sd).sum() };
        let (xs, ys): (Vec<f64>, Vec<bool>) = te.iter().map(|r| (score(r), r[label].as_bool().unwrap_or(false))).unzip();
        let ate = auc(&xs, &ys);
        let p = perm_p(&xs, &ys, n_perm, 1234);
        let ci = boot_ci(&xs, &ys, n_boot);
        // trades: top 30 % scored test pools under the chosen exit; Kelly from train top 30 %
        let top = |v: &Vec<&Value>| -> Vec<f64> {
            let mut s: Vec<(f64, &Value)> = v.iter().map(|r| (score(r), *r)).collect();
            s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            s.iter().take((s.len() as f64 * 0.3).ceil() as usize).filter_map(|(_, r)| r["ret"][&exit].as_f64()).collect()
        };
        let (rtr, rte) = (top(&tr), top(&te));
        let all_te: Vec<f64> = te.iter().filter_map(|r| r["ret"][&exit].as_f64()).collect();
        let k = kelly(&rtr) / 4.0;
        let k = k.min(0.15);
        // Monte Carlo: 1,000,000 paths of 100 trades resampled from the TEST top-30 % trades
        let paths = 1_000_000usize;
        let res: Vec<(f64, f64)> = if rte.is_empty() || k <= 0.0 { vec![] } else {
            std::thread::scope(|s| {
                (0..std::thread::available_parallelism().map(|n| n.get()).unwrap_or(16)).map(|t| {
                    let rte = rte.clone();
                    s.spawn(move || {
                        let mut g = Rng(777 + t as u64);
                        (0..paths / std::thread::available_parallelism().map(|n| n.get()).unwrap_or(16)).map(|_| {
                            let (mut eq, mut peak, mut dd) = (1.0f64, 1.0f64, 0.0f64);
                            for _ in 0..100 {
                                eq *= 1.0 + k * rte[g.below(rte.len())];
                                peak = peak.max(eq);
                                dd = dd.max(1.0 - eq / peak);
                            }
                            (eq, dd)
                        }).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect()
            })
        };
        let mut eqs: Vec<f64> = res.iter().map(|x| x.0).collect();
        eqs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| if eqs.is_empty() { f64::NAN } else { r3(eqs[((eqs.len() - 1) as f64 * p) as usize]) };
        let mean = |v: &[f64]| if v.is_empty() { f64::NAN } else { r3(v.iter().sum::<f64>() / v.len() as f64) };
        println!("{}", json!({"system_tau": tau, "chosen_features": chosen.iter().map(|c| json!([c.0, if c.1 > 0.0 { "higher" } else { "lower" }])).collect::<Vec<_>>(),
            "auc_test": r3(ate), "auc_test_ci95": [r3(ci[0]), r3(ci[1])], "perm_p": p, "exit": exit,
            "mean_ret_top30_test": mean(&rte), "n_top30_test": rte.len(), "mean_ret_all_test": mean(&all_te), "quarter_kelly": r3(k),
            "montecarlo_1e6_paths_100_trades": {"equity_p05": q(0.05), "equity_median": q(0.5), "equity_p95": q(0.95),
                "p_loss": if res.is_empty() { f64::NAN } else { r3(res.iter().filter(|x| x.0 < 1.0).count() as f64 / res.len() as f64) },
                "p_drawdown_gt_50pct": if res.is_empty() { f64::NAN } else { r3(res.iter().filter(|x| x.1 > 0.5).count() as f64 / res.len() as f64) }}}));
    }
}
