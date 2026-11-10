//! Walk-forward CatBoost (official C++ CLI, GPU) on the `tambang` dump. No look-ahead:
//!   - pools sorted by t0, split into K = 5 consecutive time blocks; model k trains ONLY on blocks < k and is tested on
//!     block k (expanding window); train pools whose t0 is within PURGE seconds before the test block start are dropped;
//!   - features are the causal ones from `tambang` (events with t <= t0 + tau only);
//!   - the trading threshold is the 70th percentile of the model's scores on its OWN training data (top 30 %).
//! Reports out-of-sample AUC, and out-of-sample trade results of the selected pools vs buying all, per exit.
//!   latih <dump.jsonl> <work_dir> <tau> <label: y2x | win:<exit>> [gpu|cpu]
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::Command;

const CB: &str = "/root/tools/catboost/catboost";
const K: usize = 5;
const PURGE: f64 = 1800.0;

fn auc(s: &[(f64, bool)]) -> f64 {
    let mut v = s.to_vec();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let np = v.iter().filter(|x| x.1).count() as f64;
    let nn = v.len() as f64 - np;
    if np == 0.0 || nn == 0.0 { return f64::NAN }
    let mut rs = 0.0;
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1].0 == v[i].0 { j += 1 }
        let r = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j { if v[k].1 { rs += r } }
        i = j + 1;
    }
    (rs - np * (np + 1.0) / 2.0) / (np * nn)
}

fn stats(r: &[f64]) -> Value {
    if r.len() < 2 { return json!({"n": r.len()}) }
    let n = r.len() as f64;
    let mut v = r.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / n;
    let wo3 = if v.len() > 4 { v[..v.len() - 3].iter().sum::<f64>() / (n - 3.0) } else { f64::NAN };
    let mut seed = 99u64;
    let mut bm: Vec<f64> = (0..10000).map(|_| (0..r.len()).map(|_| { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; r[(seed % r.len() as u64) as usize] }).sum::<f64>() / n).collect();
    bm.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({"n": r.len(), "mean": r3(mean), "median": r3(v[v.len() / 2]), "mean_without_top3": r3(wo3), "win_rate": r3(r.iter().filter(|x| **x > 0.0).count() as f64 / n),
        "ci95_mean": [r3(bm[250]), r3(bm[9749])]})
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dump, work, tau, label) = (&a[1], &a[2], a[3].parse::<f64>().unwrap(), a[4].clone());
    let dev = a.get(5).cloned().unwrap_or_else(|| "gpu".into());
    std::fs::create_dir_all(work).unwrap();
    let mut rows: Vec<Value> = std::fs::read_to_string(dump).unwrap().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|r| r["tau"].as_f64() == Some(tau)).collect();
    rows.sort_by(|a, b| a["t0"].as_f64().partial_cmp(&b["t0"].as_f64()).unwrap());
    let feats: Vec<String> = rows[0]["feat"].as_object().unwrap().keys().cloned().collect();
    let exits: Vec<String> = rows[0]["ret"].as_object().unwrap().keys().cloned().collect();
    let y = |r: &Value| -> bool {
        if let Some(e) = label.strip_prefix("win:") { r["ret"][e].as_f64().map_or(false, |x| x > 0.0) } else { r[label.as_str()].as_bool().unwrap_or(false) }
    };
    let write_tsv = |path: &str, set: &[&Value]| {
        let mut f = std::fs::File::create(path).unwrap();
        for r in set {
            let mut line = format!("{}", y(r) as u8);
            for k in &feats { line += &format!("\t{}", r["feat"][k].as_f64().map_or("nan".to_string(), |v| v.to_string())) }
            writeln!(f, "{line}").unwrap();
        }
    };
    let cd = format!("{work}/cd");
    std::fs::write(&cd, "0\tLabel\n").unwrap();
    let n = rows.len();
    let block = |i: usize| (i * K / n).min(K - 1);
    let mut oos: Vec<(f64, bool)> = vec![];
    let mut picked: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut all: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut folds = vec![];
    for k in 1..K {
        let test: Vec<&Value> = rows.iter().enumerate().filter(|(i, _)| block(*i) == k).map(|x| x.1).collect();
        let t_start = test[0]["t0"].as_f64().unwrap();
        let train: Vec<&Value> = rows.iter().enumerate().filter(|(i, r)| block(*i) < k && r["t0"].as_f64().unwrap() < t_start - PURGE).map(|x| x.1).collect();
        if train.len() < 40 || train.iter().filter(|r| y(r)).count() < 8 { continue }
        let (ftr, fte) = (format!("{work}/train{k}.tsv"), format!("{work}/test{k}.tsv"));
        write_tsv(&ftr, &train);
        write_tsv(&fte, &test);
        let model = format!("{work}/model{k}.bin");
        let st = Command::new(CB).args(["fit", "--learn-set", &ftr, "--column-description", &cd, "--loss-function", "Logloss", "-i", "600", "--depth", "5",
            "--learning-rate", "0.03", "--l2-leaf-reg", "6", "--random-seed", "7", "--task-type", if dev == "gpu" { "GPU" } else { "CPU" },
            "--train-dir", &format!("{work}/cb{k}"), "-m", &model, "--logging-level", "Silent"]).status();
        if !st.map(|s| s.success()).unwrap_or(false) { eprintln!("catboost fit failed fold {k}"); continue }
        let pred = |inp: &str, out: &str| -> Vec<f64> {
            let _ = Command::new(CB).args(["calc", "-m", &model, "--input-path", inp, "--column-description", &cd, "--output-path", out, "--prediction-type", "Probability"]).status();
            std::fs::read_to_string(out).unwrap_or_default().lines().skip(1).filter_map(|l| l.split('\t').last().and_then(|x| x.parse().ok())).collect()
        };
        let (ptr, pte) = (pred(&ftr, &format!("{work}/ptr{k}.tsv")), pred(&fte, &format!("{work}/pte{k}.tsv")));
        if pte.len() != test.len() { eprintln!("prediction size mismatch fold {k}"); continue }
        let mut s = ptr.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let thr = s[(s.len() as f64 * 0.7) as usize];
        let fold_auc = auc(&pte.iter().zip(test.iter()).map(|(p, r)| (*p, y(r))).collect::<Vec<_>>());
        for (p, r) in pte.iter().zip(test.iter()) {
            oos.push((*p, y(r)));
            for e in &exits {
                if let Some(v) = r["ret"][e].as_f64() {
                    all.entry(e.clone()).or_default().push(v);
                    if *p >= thr { picked.entry(e.clone()).or_default().push(v) }
                }
            }
        }
        folds.push(json!({"fold": k, "n_train": train.len(), "n_test": test.len(), "pos_train": train.iter().filter(|r| y(r)).count(), "pos_test": test.iter().filter(|r| y(r)).count(),
            "auc_test": (fold_auc * 1000.0).round() / 1000.0, "picked": pte.iter().filter(|p| **p >= thr).count()}));
    }
    println!("{}", json!({"tau": tau, "label": label, "device": dev, "folds": folds, "auc_oos_all": (auc(&oos) * 1000.0).round() / 1000.0, "n_oos": oos.len()}));
    for e in &exits {
        println!("{}", json!({"tau": tau, "label": label, "exit": e, "model_picked": stats(picked.get(e).map_or(&vec![], |v| v)), "buy_all": stats(all.get(e).map_or(&vec![], |v| v))}));
    }
}
