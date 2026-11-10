//! Full system with NESTED tuning (selection fraction + TP plan chosen only on earlier out-of-sample blocks), walk-forward:
//!   entry score = P(reaches 2x) x (1 - P(LP removed within 5 min)), two CatBoost models (official C++ CLI) trained on
//!   earlier time blocks only (874 data-mined causal features from `tambang1000`, decision at TAU);
//!   trade if score >= the 70th percentile of the TRAIN scores (top 30 %);
//!   execution = `trade_amm` (verified pool math, 2-slot latency, real fees, real failure rates with retries) with the
//!   take-profit plan chosen by `tp_tune` (stable in all folds): 70 % at 2x, 20 % at 3x, moon bag trail 30 %, age 120 s.
//! Also reports "buy all" with the same plan on the same folds.
//!   sistem <dump1000t.jsonl> <work_dir> <raw_file>...    env TAU (default 10), CB_DEV (cpu|gpu, default cpu)
use dekode::series::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;

const CB: &str = "/root/tools/catboost/catboost";

fn stats(r: &[f64]) -> Value {
    if r.len() < 3 { return json!({"n": r.len()}) }
    let n = r.len() as f64;
    let mut v = r.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / n;
    let wo3 = if v.len() > 5 { v[..v.len() - 3].iter().sum::<f64>() / (n - 3.0) } else { f64::NAN };
    let mut seed = 11u64;
    let mut bm: Vec<f64> = (0..10000).map(|_| (0..r.len()).map(|_| { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; r[(seed % r.len() as u64) as usize] }).sum::<f64>() / n).collect();
    bm.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({"n": r.len(), "mean": r3(mean), "median": r3(v[v.len() / 2]), "mean_without_top3": r3(wo3), "win_rate": r3(r.iter().filter(|x| **x > 0.0).count() as f64 / n),
        "ge_100pct": r.iter().filter(|x| **x >= 1.0).count(), "worst": r3(v[0]), "best": r3(v[v.len() - 1]), "ci95_mean": [r3(bm[250]), r3(bm[9749])]})
}

fn auc(s: &[(f64, bool)]) -> f64 {
    let mut v = s.to_vec();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let np = v.iter().filter(|x| x.1).count() as f64;
    let nn = v.len() as f64 - np;
    if np == 0.0 || nn == 0.0 { return f64::NAN }
    let rs: f64 = v.iter().enumerate().filter(|(_, x)| x.1).map(|(i, _)| i as f64 + 1.0).sum();
    (rs - np * (np + 1.0) / 2.0) / (np * nn)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dump, work) = (&a[1], &a[2]);
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    let dev = std::env::var("CB_DEV").unwrap_or_else(|_| "cpu".into());
    std::fs::create_dir_all(work).unwrap();
    let mut rows: Vec<Value> = std::fs::read_to_string(dump).unwrap().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|r| r["tau"].as_f64() == Some(tau)).collect();
    rows.sort_by(|x, y| x["t0"].as_f64().partial_cmp(&y["t0"].as_f64()).unwrap());
    // market-cap features use the CURRENT token supply (fetched later) -> possible look-ahead -> excluded from the model
    let feats: Vec<String> = rows[0]["feat"].as_object().unwrap().keys().cloned().filter(|k| !(k.starts_with("mc_") || k == "log_mc")).filter(|k| std::env::var("EXCLUDE_PREFIX").map_or(true, |p| p.is_empty() || !k.starts_with(&p))).collect();
    let pools = load_pools(&a[3..]);
    // plan grid (same family as tp_tune) and the execution result of every (plan, pool), mean over 3 failure seeds
    let mut plans: Vec<Plan> = vec![];
    for (m1, f1) in [(1.5, 0.5), (2.0, 0.5), (2.0, 0.7), (3.0, 0.5), (3.0, 0.7)] {
        for t2 in [None, Some((3.0, 0.2)), Some((5.0, 0.2))] {
            for t3 in [None, Some((10.0, 0.1))] {
                for trail in [0.3, 0.5, 0.7] {
                    for max_age in [90.0, 120.0, 180.0, 300.0] {
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
    // momentum exits (hold while the flow is strong, exit 100 % when price falls AND flow fades) + rug emergency exit
    for win in [5.0, 10.0, 20.0] { for dd in [0.15, 0.25, 0.35, 0.5] { for imb_min in [-0.2, 0.0, 0.2] { for rate_frac in [0.3, 0.5] { for fail_max in [0.5, 0.7, 1.1] { for max_age in [180.0, 300.0, 600.0, 1e9] {
        plans.push(Plan { tiers: vec![], trail: 1.0, max_age, mom: Some(Mom { win, dd, imb_min, rate_frac, fail_max }), be: None, ratchet: None });
    } } } } } }
    for f1 in [0.3, 0.5] { for dd in [0.25, 0.35] { for max_age in [300.0, 600.0, 1e9] {
        plans.push(Plan { tiers: vec![(2.0, f1)], trail: 1.0, max_age, mom: Some(Mom { win: 10.0, dd, imb_min: 0.0, rate_frac: 0.5, fail_max: 0.7 }), be: None, ratchet: None });
    } } }
    let keys: Vec<String> = rows.iter().map(|r| r["pool"].as_str().unwrap().to_string()).collect();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let chunk = (plans.len() / cores).max(1);
    let retm: Vec<Vec<Option<f64>>> = std::thread::scope(|sc| {
        plans.chunks(chunk).map(|pc| { let (keys, pools) = (&keys, &pools); sc.spawn(move || pc.iter().map(|pl| keys.iter().map(|k| {
            let v: Vec<f64> = pools.get(k).map(|p| (0..3).filter_map(|sd| trade_amm(p, tau, pl, sd).map(|e| e.ret)).collect()).unwrap_or_default();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        }).collect::<Vec<_>>()).collect::<Vec<_>>()) }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    eprintln!("plans {} x pools {} simulated", plans.len(), keys.len());
    let cd = format!("{work}/cd");
    std::fs::write(&cd, "0\tLabel\n").unwrap();
    // TOPK > 0: per component (label) and per fold, keep the TOPK features with the largest |AUC - 0.5| on the TRAIN
    // rows only, dropping any feature whose rank correlation with an already kept one exceeds 0.9 (redundant twins).
    let topk: usize = std::env::var("TOPK").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let select = |tr: &[&Value], label: &str| -> Vec<String> {
        if topk == 0 { return feats.clone() }
        let y: Vec<bool> = tr.iter().map(|r| r[label].as_bool().unwrap_or(false)).collect();
        let rank = |v: &[f64]| -> Vec<f64> { let mut ix: Vec<usize> = (0..v.len()).collect(); ix.sort_by(|a, b| v[*a].partial_cmp(&v[*b]).unwrap()); let mut r = vec![0.0; v.len()]; for (k, i) in ix.iter().enumerate() { r[*i] = k as f64 } r };
        let mut cand: Vec<(f64, &String, Vec<f64>)> = feats.iter().filter_map(|f| {
            let v: Vec<f64> = tr.iter().map(|r| r["feat"][f].as_f64().unwrap_or(f64::NAN)).collect();
            if v.iter().filter(|x| x.is_finite()).count() < v.len() * 9 / 10 { return None }
            let v: Vec<f64> = v.iter().map(|x| if x.is_finite() { *x } else { 0.0 }).collect();
            let rk = rank(&v);
            let a = auc(&rk.iter().cloned().zip(y.iter().cloned()).collect::<Vec<_>>());
            a.is_finite().then(|| ((a - 0.5).abs(), f, rk))
        }).collect();
        cand.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let corr = |a: &[f64], b: &[f64]| { let n = a.len() as f64; let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n); let (mut c, mut va, mut vb) = (0.0, 0.0, 0.0); for i in 0..a.len() { c += (a[i] - ma) * (b[i] - mb); va += (a[i] - ma).powi(2); vb += (b[i] - mb).powi(2) } c / (va * vb).sqrt().max(1e-12) };
        let mut keep: Vec<(&String, &Vec<f64>)> = vec![];
        for (_, f, rk) in &cand { if keep.len() >= topk { break } if keep.iter().all(|(_, r2)| corr(rk, r2).abs() <= 0.9) { keep.push((f, rk)) } }
        keep.into_iter().map(|(f, _)| f.clone()).collect()
    };
    let write = |path: &str, set: &[&Value], label: &str, fl: &[String]| {
        let mut f = std::fs::File::create(path).unwrap();
        for r in set {
            let mut line = format!("{}", r[label].as_bool().unwrap_or(false) as u8);
            for k in fl { line += &format!("\t{}", r["feat"][k].as_f64().map_or("nan".to_string(), |v| v.to_string())) }
            writeln!(f, "{line}").unwrap();
        }
    };
    let fit_pred = |label: &str, k: usize, tr: &[&Value], te: &[&Value]| -> Option<(Vec<f64>, Vec<f64>)> {
        let (ftr, fte, m) = (format!("{work}/{label}_tr{k}.tsv"), format!("{work}/{label}_te{k}.tsv"), format!("{work}/{label}_m{k}.bin"));
        let fl = select(tr, label);
        let _ = std::fs::write(format!("{work}/{label}_features{k}.txt"), fl.join("
"));
        write(&ftr, tr, label, &fl);
        write(&fte, te, label, &fl);
        let ok = Command::new(CB).args(["fit", "--learn-set", &ftr, "--column-description", &cd, "--loss-function", "Logloss", "-i", "500", "--depth", "4",
            "--learning-rate", "0.03", "--l2-leaf-reg", "8", "--random-seed", "7", "--task-type", if dev == "gpu" { "GPU" } else { "CPU" },
            "--train-dir", &format!("{work}/cb_{label}{k}"), "-m", &m, "--logging-level", "Silent"]).output().map(|o| o.status.success()).unwrap_or(false);
        if !ok { return None }
        let pred = |inp: &str, out: &str| -> Vec<f64> {
            let _ = Command::new(CB).args(["calc", "-m", &m, "--input-path", inp, "--column-description", &cd, "--output-path", out, "--prediction-type", "Probability"]).output();
            std::fs::read_to_string(out).unwrap_or_default().lines().skip(1).filter_map(|l| l.split('\t').last().and_then(|x| x.parse().ok())).collect()
        };
        Some((pred(&ftr, &format!("{work}/{label}_ptr{k}")), pred(&fte, &format!("{work}/{label}_pte{k}"))))
    };
    let n = rows.len();
    let block = |i: usize| (i * 5 / n).min(4);
    // pass 1: out-of-sample scores for blocks 1..4 (each block scored by models trained on earlier blocks only)
    let mut score: Vec<Option<f64>> = vec![None; n];
    // in-sample scores of the block-1 model on its own TRAIN rows: the only allowed reference for the block-1 threshold
    let mut train_scores1: Vec<f64> = vec![];
    let (mut sc2, mut scr) = (vec![], vec![]);
    for k in 1..5 {
        let te_idx: Vec<usize> = (0..n).filter(|i| block(*i) == k).collect();
        // purge: training pools created within 30 min before the test block are dropped (their outcomes overlap it)
        let t_test = te_idx.first().map_or(f64::INFINITY, |i| rows[*i]["t0"].as_f64().unwrap_or(0.0));
        let tr: Vec<&Value> = rows.iter().enumerate().filter(|(i, r)| block(*i) < k && r["t0"].as_f64().unwrap_or(0.0) < t_test - 1800.0).map(|x| x.1).collect();
        let te: Vec<&Value> = te_idx.iter().map(|i| &rows[*i]).collect();
        let (Some((p2tr, p2te)), Some((prtr, prte))) = (fit_pred("y2x", k, &tr, &te), fit_pred("rug5", k, &tr, &te)) else { eprintln!("fold {k}: catboost failed"); continue };
        if p2te.len() != te.len() || prte.len() != te.len() { continue }
        if k == 1 { train_scores1 = p2tr.iter().zip(&prtr).map(|(a, b)| a * (1.0 - b)).collect() }
        for (j, i) in te_idx.iter().enumerate() {
            score[*i] = Some(p2te[j] * (1.0 - prte[j]));
            sc2.push((p2te[j], rows[*i]["y2x"].as_bool().unwrap_or(false)));
            scr.push((prte[j], rows[*i]["rug5"].as_bool().unwrap_or(false)));
        }
    }
    // pass 2: nested choice. For test block k, (selection fraction, plan) is chosen on blocks 1..k-1 using their
    // OUT-OF-SAMPLE scores only (objective: mean return without the 3 best trades, >= 15 trades); block 1 uses the default.
    let fracs = [0.1, 0.2, 0.3, 0.4];
    let trim = |v: &[f64]| -> f64 { if v.len() < 15 { return f64::NEG_INFINITY } let mut s = v.to_vec(); s.sort_by(|a, b| a.partial_cmp(b).unwrap()); s[..s.len() - 3].iter().sum::<f64>() / (s.len() - 3) as f64 };
    let pick = |idx: &[usize], frac: f64, pi: usize| -> Vec<f64> {
        let mut sc: Vec<(f64, usize)> = idx.iter().filter_map(|i| score[*i].map(|s| (s, *i))).collect();
        sc.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        sc.iter().take((sc.len() as f64 * frac).ceil() as usize).filter_map(|(_, i)| retm[pi][*i]).collect()
    };
    let default_pi = plans.iter().position(|p| p.tiers == vec![(2.0, 0.7), (3.0, 0.2)] && p.trail == 0.3 && p.max_age == 120.0).unwrap_or(0);
    let (mut sys, mut all, mut folds) = (vec![], vec![], vec![]);
    let mut radar: Vec<(bool, bool, bool, bool)> = vec![];
    let mut rejected: Vec<f64> = vec![];
    let mut stage1_sel: Vec<bool> = vec![false; n];
    let mut entry_scores: Vec<Value> = vec![];
    for k in 1..5 {
        let te: Vec<usize> = (0..n).filter(|i| block(*i) == k).collect();
        let past: Vec<usize> = (0..n).filter(|i| block(*i) >= 1 && block(*i) < k).collect();
        let (mut bf, mut bp, mut bs) = (0.3, default_pi, f64::NEG_INFINITY);
        if !past.is_empty() {
            for f in fracs { for pi in 0..plans.len() { let v = trim(&pick(&past, f, pi)); if v > bs { bs = v; bf = f; bp = pi } } }
        }
        // threshold for block k = the frac-quantile of the past OOS scores (block 1: its own score distribution)
        // block 1: threshold from the block-1 model's TRAIN scores (no look at the test block) - fix 2026-11-09
        let mut rs: Vec<f64> = if past.is_empty() { train_scores1.clone() } else { past.iter().filter_map(|i| score[*i]).collect() };
        rs.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let thr = rs.get(((rs.len() as f64 * bf).ceil() as usize).saturating_sub(1)).cloned().unwrap_or(f64::INFINITY);
        let v: Vec<f64> = te.iter().filter(|i| score[**i].map_or(false, |s| s >= thr)).filter_map(|i| retm[bp][*i]).collect();
        for i in &te { if score[*i].is_some() { radar.push((score[*i].unwrap() >= thr, rows[*i]["y2x"].as_bool().unwrap_or(false), rows[*i]["y6x"].as_bool().unwrap_or(false), rows[*i]["rug5"].as_bool().unwrap_or(false))) } }
        all.extend(te.iter().filter_map(|i| retm[default_pi][*i]));
        rejected.extend(te.iter().filter(|i| score[**i].map_or(false, |s| s < thr)).filter_map(|i| retm[bp][*i]));
        for i in &te { stage1_sel[*i] = score[*i].map_or(false, |s| s >= thr) }
        for i in &te { if let Some(sc) = score[*i] { entry_scores.push(json!({"pool": keys[*i], "block": k, "score": sc, "thr": thr})) } }
        sys.extend(v.iter().cloned());
        // how long the chosen plan actually held the selected positions (seconds, seed 0)
        let mut holds: Vec<f64> = te.iter().filter(|i| score[**i].map_or(false, |s| s >= thr)).filter_map(|i| pools.get(&keys[*i]).and_then(|p| trade_amm(p, tau, &plans[bp], 0)).map(|e| e.hold_s)).collect();
        holds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let hq = |q: f64| holds.get(((holds.len().max(1) - 1) as f64 * q) as usize).cloned().unwrap_or(f64::NAN).round();
        folds.push(json!({"fold": k, "chosen_frac": bf, "chosen_plan": format!("{:?}", plans[bp]), "hold_seconds_p25_median_p75": [hq(0.25), hq(0.5), hq(0.75)], "past_trimmed_mean": if bs.is_finite() { (bs * 1000.0).round() / 1000.0 } else { f64::NAN }, "result": stats(&v)}));
    }
    // Monte Carlo: 10,000,000 equity paths x 100 trades (1e9 simulated trades) resampled from the system's OOS trades,
    // 1/4 Kelly (estimated on the same OOS trades - descriptive), compounding; also with the 3 best trades removed.
    let mc = |r: &[f64]| -> Value {
        if r.len() < 10 { return json!(null) }
        let mut best = (0.0, f64::NEG_INFINITY);
        let mut f = 0.005;
        while f <= 0.6 { let g: f64 = r.iter().map(|x| (1.0 + f * x).max(1e-12).ln()).sum::<f64>() / r.len() as f64; if g > best.1 { best = (f, g) } f += 0.005 }
        let k = (best.0 / 4.0).min(0.15);
        let paths = 10_000_000usize;
        let per = paths / cores;
        let res: Vec<(f64, f64)> = std::thread::scope(|sc| (0..cores).map(|t| { let r = r.to_vec(); sc.spawn(move || {
            let mut g = 0x2545F4914F6CDD1Du64 ^ (t as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15);
            (0..per).map(|_| { let (mut eq, mut pk, mut dd) = (1.0f64, 1.0f64, 0.0f64);
                for _ in 0..100 { g ^= g << 13; g ^= g >> 7; g ^= g << 17; eq *= 1.0 + k * r[(g % r.len() as u64) as usize]; pk = pk.max(eq); dd = dd.max(1.0 - eq / pk) }
                (eq, dd) }).collect::<Vec<_>>() }) }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect());
        let mut e: Vec<f64> = res.iter().map(|x| x.0).collect();
        e.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| (e[((e.len() - 1) as f64 * p) as usize] * 1000.0).round() / 1000.0;
        json!({"quarter_kelly": (k * 1000.0).round() / 1000.0, "simulated_trades": res.len() * 100, "equity_after_100_trades": {"p01": q(0.01), "p05": q(0.05), "median": q(0.5), "p95": q(0.95)},
            "p_loss": res.iter().filter(|x| x.0 < 1.0).count() as f64 / res.len() as f64, "p_drawdown_gt_30pct": res.iter().filter(|x| x.1 > 0.3).count() as f64 / res.len() as f64,
            "p_drawdown_gt_50pct": res.iter().filter(|x| x.1 > 0.5).count() as f64 / res.len() as f64})
    };
    // ---------------- STAGE 2: re-check the coins stage 1 rejected, at 60 s ----------------
    // Rows at tau = 60 (same pools, features up to 60 s). Models for stage 2 are trained per block on earlier blocks
    // only; (selection fraction, plan) for stage 2 are chosen on earlier OUT-OF-SAMPLE blocks only (nested, no look-ahead).
    let blk_of: HashMap<String, usize> = rows.iter().enumerate().map(|(i, r)| (r["pool"].as_str().unwrap().to_string(), block(i))).collect();
    let rows60: Vec<Value> = std::fs::read_to_string(dump).unwrap().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| r["tau"].as_f64() == Some(60.0) && blk_of.contains_key(r["pool"].as_str().unwrap_or(""))).collect();
    let b60 = |r: &Value| blk_of[r["pool"].as_str().unwrap()];
    let keys60: Vec<String> = rows60.iter().map(|r| r["pool"].as_str().unwrap().to_string()).collect();
    let retm60: Vec<Vec<Option<f64>>> = std::thread::scope(|sc| {
        plans.chunks(chunk).map(|pc| { let (keys60, pools) = (&keys60, &pools); sc.spawn(move || pc.iter().map(|pl| keys60.iter().map(|k| {
            let v: Vec<f64> = pools.get(k).map(|p| (0..3).filter_map(|sd| trade_amm(p, 60.0, pl, sd).map(|e| e.ret)).collect()).unwrap_or_default();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        }).collect::<Vec<_>>()).collect::<Vec<_>>()) }).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let mut score60: Vec<Option<f64>> = vec![None; rows60.len()];
    for k in 1..5 {
        let tr: Vec<&Value> = rows60.iter().filter(|r| b60(r) < k).collect();
        let te_idx: Vec<usize> = (0..rows60.len()).filter(|i| b60(&rows60[*i]) == k).collect();
        let te: Vec<&Value> = te_idx.iter().map(|i| &rows60[*i]).collect();
        if tr.len() < 30 || te.is_empty() { continue }
        let (Some((_, p2)), Some((_, pr))) = (fit_pred("y2x", k + 10, &tr, &te), fit_pred("rug5", k + 10, &tr, &te)) else { continue };
        if p2.len() != te.len() || pr.len() != te.len() { continue }
        for (j, i) in te_idx.iter().enumerate() { score60[*i] = Some(p2[j] * (1.0 - pr[j])) }
    }
    // stage-1 decision per pool (selected at 15 s?) for the test blocks, using the per-block thresholds of stage 1
    let idx15: HashMap<String, usize> = rows.iter().enumerate().map(|(i, r)| (r["pool"].as_str().unwrap().to_string(), i)).collect();
    let rejected60 = |blocks: &dyn Fn(usize) -> bool| -> Vec<usize> {
        (0..rows60.len()).filter(|i| blocks(b60(&rows60[*i])) && score60[*i].is_some())
            .filter(|i| { let k = &keys60[*i]; idx15.get(k).map_or(false, |j| !stage1_sel[*j]) }).collect()
    };
    let pick2 = |idx: &[usize], frac: f64, pi: usize| -> Vec<f64> {
        let mut sc: Vec<(f64, usize)> = idx.iter().filter_map(|i| score60[*i].map(|s| (s, *i))).collect();
        sc.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        sc.iter().take((sc.len() as f64 * frac).ceil() as usize).filter_map(|(_, i)| retm60[pi][*i]).collect()
    };
    let (mut st2, mut st2_rest, mut folds2) = (vec![], vec![], vec![]);
    for k in 2..5 {
        let past = rejected60(&|b| b >= 1 && b < k);
        let cur = rejected60(&|b| b == k);
        if past.len() < 15 || cur.is_empty() { continue }
        let (mut bf, mut bp, mut bs) = (0.2, default_pi, f64::NEG_INFINITY);
        for f in [0.1, 0.2, 0.3] { for pi in 0..plans.len() { let v = trim(&pick2(&past, f, pi)); if v > bs { bs = v; bf = f; bp = pi } } }
        let mut rs: Vec<f64> = past.iter().filter_map(|i| score60[*i]).collect();
        rs.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let thr = rs.get(((rs.len() as f64 * bf).ceil() as usize).saturating_sub(1)).cloned().unwrap_or(f64::INFINITY);
        let v: Vec<f64> = cur.iter().filter(|i| score60[**i].map_or(false, |s| s >= thr)).filter_map(|i| retm60[bp][*i]).collect();
        st2_rest.extend(cur.iter().filter(|i| score60[**i].map_or(false, |s| s < thr)).filter_map(|i| retm60[bp][*i]));
        st2.extend(v.iter().cloned());
        folds2.push(json!({"fold": k, "stage2_frac": bf, "stage2_plan": format!("{:?}", plans[bp]), "past_trimmed": if bs.is_finite() { (bs * 1000.0).round() / 1000.0 } else { f64::NAN }, "result": stats(&v)}));
    }
    // ---------------- analysis: rejected coins that went up vs rejected coins that did not ----------------
    // descriptive (test blocks only): AUC of each feature at 15 s and at 60 s for "rejected and reached 2x before LP
    // removal" vs "rejected and did not"; top 12 per decision time.
    let mut analysis = serde_json::Map::new();
    for (name, rset, labkey) in [("tau15", &rows, "y2x"), ("tau60", &rows60, "y2x")] {
        let ids: Vec<usize> = (0..rset.len()).filter(|i| {
            let k = rset[*i]["pool"].as_str().unwrap();
            idx15.get(k).map_or(false, |j| block(*j) >= 1 && score[*j].is_some() && !stage1_sel[*j])
        }).collect();
        let y: Vec<bool> = ids.iter().map(|i| rset[*i][labkey].as_bool().unwrap_or(false)).collect();
        let mut fa: Vec<(f64, String, f64)> = feats.iter().filter_map(|f| {
            let v: Vec<(f64, bool)> = ids.iter().zip(&y).filter_map(|(i, yy)| rset[*i]["feat"][f].as_f64().map(|x| (x, *yy))).collect();
            if v.len() < ids.len() * 9 / 10 { return None }
            let a = auc(&v);
            a.is_finite().then(|| ((a - 0.5).abs(), f.clone(), a))
        }).collect();
        fa.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        analysis.insert(name.into(), json!({"rejected": ids.len(), "rejected_reached_2x": y.iter().filter(|b| **b).count(),
            "top_features": fa.iter().take(12).map(|x| json!([x.1, (x.2 * 1000.0).round() / 1000.0, if x.2 > 0.5 { "higher=up" } else { "lower=up" }])).collect::<Vec<_>>()}));
    }
    let mut both = sys.clone();
    both.extend(st2.iter().cloned());
    let mut sys_sorted = sys.clone();
    sys_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let without_top3: Vec<f64> = if sys_sorted.len() > 5 { sys_sorted[..sys_sorted.len() - 3].to_vec() } else { vec![] };
    let cnt = |f: &dyn Fn(&(bool, bool, bool, bool)) -> bool| radar.iter().filter(|x| f(x)).count();
    let (sel_n, all_n) = (cnt(&|x| x.0), radar.len());
    let radar_json = json!({"test_coins": all_n, "selected": sel_n,
        "precision_2x_selected": cnt(&|x| x.0 && x.1) as f64 / sel_n.max(1) as f64, "base_rate_2x_all": cnt(&|x| x.1) as f64 / all_n.max(1) as f64, "recall_2x": cnt(&|x| x.0 && x.1) as f64 / cnt(&|x| x.1).max(1) as f64,
        "precision_6x_selected": cnt(&|x| x.0 && x.2) as f64 / sel_n.max(1) as f64, "base_rate_6x_all": cnt(&|x| x.2) as f64 / all_n.max(1) as f64, "recall_6x": cnt(&|x| x.0 && x.2) as f64 / cnt(&|x| x.2).max(1) as f64,
        "rug5_rate_selected": cnt(&|x| x.0 && x.3) as f64 / sel_n.max(1) as f64, "rug5_rate_rejected": cnt(&|x| !x.0 && x.3) as f64 / (all_n - sel_n).max(1) as f64});
    println!("{}", json!({"tau": tau, "plans_tried": plans.len(), "folds": folds, "RADAR": radar_json,
        "auc_oos_p2x": (auc(&sc2) * 1000.0).round() / 1000.0, "auc_oos_prug5": (auc(&scr) * 1000.0).round() / 1000.0,
        "SYSTEM_oos": stats(&sys), "REJECTED_same_plan_oos": stats(&rejected), "STAGE2_folds": folds2, "ENTRY_SCORES_OOS": entry_scores, "STAGE2_entries_at_60s_oos": stats(&st2), "STAGE2_still_rejected_oos": stats(&st2_rest), "STAGE1_plus_STAGE2_oos": stats(&both), "ANALYSIS_rejected_up_vs_not": analysis, "BUY_ALL_default_plan_oos": stats(&all),
        "SYSTEM_trades_in_time_order": sys, "BLOCK0_buyall_trades_before_test": (0..n).filter(|i| block(*i) == 0).filter_map(|i| retm[default_pi][i]).collect::<Vec<f64>>(), "MONTECARLO_system": mc(&sys), "MONTECARLO_system_without_top3": mc(&without_top3)}));
}
