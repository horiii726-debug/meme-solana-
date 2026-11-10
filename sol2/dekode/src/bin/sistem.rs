//! Full system, walk-forward, out-of-sample only:
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
    let feats: Vec<String> = rows[0]["feat"].as_object().unwrap().keys().cloned().collect();
    let pools = load_pools(&a[3..]);
    let plan = Plan { tiers: vec![(2.0, 0.7), (3.0, 0.2)], trail: 0.3, max_age: 120.0, mom: None, be: None, ratchet: None };
    // execution result per pool (mean over 3 failure seeds)
    let ret: HashMap<String, Option<f64>> = rows.iter().map(|r| {
        let k = r["pool"].as_str().unwrap().to_string();
        let v = pools.get(&k).map(|p| (0..3).filter_map(|sd| trade_amm(p, tau, &plan, sd).map(|e| e.ret)).collect::<Vec<_>>()).unwrap_or_default();
        (k, (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64))
    }).collect();
    let cd = format!("{work}/cd");
    std::fs::write(&cd, "0\tLabel\n").unwrap();
    let write = |path: &str, set: &[&Value], label: &str| {
        let mut f = std::fs::File::create(path).unwrap();
        for r in set {
            let mut line = format!("{}", r[label].as_bool().unwrap_or(false) as u8);
            for k in &feats { line += &format!("\t{}", r["feat"][k].as_f64().map_or("nan".to_string(), |v| v.to_string())) }
            writeln!(f, "{line}").unwrap();
        }
    };
    let fit_pred = |label: &str, k: usize, tr: &[&Value], te: &[&Value]| -> Option<(Vec<f64>, Vec<f64>)> {
        let (ftr, fte, m) = (format!("{work}/{label}_tr{k}.tsv"), format!("{work}/{label}_te{k}.tsv"), format!("{work}/{label}_m{k}.bin"));
        write(&ftr, tr, label);
        write(&fte, te, label);
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
    let (mut sel, mut all, mut sc2, mut scr) = (vec![], vec![], vec![], vec![]);
    let mut folds = vec![];
    for k in 1..5 {
        let tr: Vec<&Value> = rows.iter().enumerate().filter(|(i, _)| block(*i) < k).map(|x| x.1).collect();
        let te: Vec<&Value> = rows.iter().enumerate().filter(|(i, _)| block(*i) == k).map(|x| x.1).collect();
        let (Some((p2tr, p2te)), Some((prtr, prte))) = (fit_pred("y2x", k, &tr, &te), fit_pred("rug5", k, &tr, &te)) else { eprintln!("fold {k}: catboost failed"); continue };
        if p2te.len() != te.len() || prte.len() != te.len() { continue }
        let s_tr: Vec<f64> = p2tr.iter().zip(&prtr).map(|(a, b)| a * (1.0 - b)).collect();
        let mut st = s_tr.clone();
        st.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let thr = st[(st.len() as f64 * 0.7) as usize];
        let mut fsel = vec![];
        for (i, r) in te.iter().enumerate() {
            let s = p2te[i] * (1.0 - prte[i]);
            sc2.push((p2te[i], r["y2x"].as_bool().unwrap_or(false)));
            scr.push((prte[i], r["rug5"].as_bool().unwrap_or(false)));
            if let Some(Some(x)) = ret.get(r["pool"].as_str().unwrap()) {
                all.push(*x);
                if s >= thr { sel.push(*x); fsel.push(*x) }
            }
        }
        folds.push(json!({"fold": k, "train": tr.len(), "test": te.len(), "selected": fsel.len(), "selected_result": stats(&fsel)}));
    }
    println!("{}", json!({"tau": tau, "plan": format!("{plan:?}"), "folds": folds,
        "auc_oos_p2x": (auc(&sc2) * 1000.0).round() / 1000.0, "auc_oos_prug5": (auc(&scr) * 1000.0).round() / 1000.0,
        "SYSTEM_selected_oos": stats(&sel), "BUY_ALL_same_plan_oos": stats(&all)}));
}
