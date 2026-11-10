//! FROZEN test on coins never seen before. Nothing is tuned here:
//!   - models P(2x) and P(rug5) are trained once on the TRAINING dump (all rows at TAU), CatBoost settings fixed;
//!   - the selection threshold = the FRAC quantile of the model's scores on the training rows (FRAC fixed, default 0.3);
//!   - the TP plan is fixed (default = the plan chosen by the nested tuning): 70 % at 2x, 20 % at 3x, trail 30 %, 120 s;
//!   - test coins = rows of the TEST dump whose pool is NOT in the training dump.
//! Execution = trade_amm (fixed engine, 3 failure seeds). Reports the selected coins, the rejected ones and buy-all.
//!   uji_beku <train_dump.jsonl> <test_dump.jsonl> <work_dir> <raw_file>...    env TAU (15), FRAC (0.3)
use dekode::series::*;
use serde_json::{json, Value};
use std::collections::HashSet;
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
    let mut seed = 17u64;
    let mut bm: Vec<f64> = (0..10000).map(|_| (0..r.len()).map(|_| { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; r[(seed % r.len() as u64) as usize] }).sum::<f64>() / n).collect();
    bm.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({"n": r.len(), "mean": r3(mean), "median": r3(v[v.len() / 2]), "mean_without_top3": r3(wo3), "win_rate": r3(r.iter().filter(|x| **x > 0.0).count() as f64 / n),
        "ge_100pct": r.iter().filter(|x| **x >= 1.0).count(), "worst": r3(v[0]), "best": r3(v[v.len() - 1]), "ci95_mean": [r3(bm[250]), r3(bm[9749])]})
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (trd, ted, work) = (&a[1], &a[2], &a[3]);
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(15.0);
    let frac: f64 = std::env::var("FRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.3);
    std::fs::create_dir_all(work).unwrap();
    let load = |p: &str| -> Vec<Value> { std::fs::read_to_string(p).unwrap().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|r| r["tau"].as_f64() == Some(tau)).collect() };
    let train = load(trd);
    let seen: HashSet<String> = std::fs::read_to_string(trd).unwrap().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|r| r["pool"].as_str().map(String::from)).collect();
    let test: Vec<Value> = load(ted).into_iter().filter(|r| !seen.contains(r["pool"].as_str().unwrap_or(""))).collect();
    let feats: Vec<String> = train[0]["feat"].as_object().unwrap().keys().cloned().filter(|k| !(k.starts_with("mc_") || k == "log_mc")).collect();
    eprintln!("train rows {}, NEW test coins {}", train.len(), test.len());
    let cd = format!("{work}/cd");
    std::fs::write(&cd, "0\tLabel\n").unwrap();
    let write = |path: &str, set: &[Value], label: &str| {
        let mut f = std::fs::File::create(path).unwrap();
        for r in set {
            let mut line = format!("{}", r[label].as_bool().unwrap_or(false) as u8);
            for k in &feats { line += &format!("\t{}", r["feat"][k].as_f64().map_or("nan".to_string(), |v| v.to_string())) }
            writeln!(f, "{line}").unwrap();
        }
    };
    let fit_pred = |label: &str| -> (Vec<f64>, Vec<f64>) {
        let (ftr, fte, m) = (format!("{work}/{label}_tr.tsv"), format!("{work}/{label}_te.tsv"), format!("{work}/{label}.bin"));
        write(&ftr, &train, label);
        write(&fte, &test, label);
        let _ = Command::new(CB).args(["fit", "--learn-set", &ftr, "--column-description", &cd, "--loss-function", "Logloss", "-i", "500", "--depth", "4",
            "--learning-rate", "0.03", "--l2-leaf-reg", "8", "--random-seed", "7", "--train-dir", &format!("{work}/cb_{label}"), "-m", &m, "--logging-level", "Silent"]).output();
        let pred = |inp: &str, out: &str| -> Vec<f64> {
            let _ = Command::new(CB).args(["calc", "-m", &m, "--input-path", inp, "--column-description", &cd, "--output-path", out, "--prediction-type", "Probability"]).output();
            std::fs::read_to_string(out).unwrap_or_default().lines().skip(1).filter_map(|l| l.split('\t').last().and_then(|x| x.parse().ok())).collect()
        };
        (pred(&ftr, &format!("{work}/{label}_ptr")), pred(&fte, &format!("{work}/{label}_pte")))
    };
    let ((p2tr, p2te), (prtr, prte)) = (fit_pred("y2x"), fit_pred("rug5"));
    let mut s_tr: Vec<f64> = p2tr.iter().zip(&prtr).map(|(a, b)| a * (1.0 - b)).collect();
    s_tr.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let thr = s_tr[((s_tr.len() as f64 * frac).ceil() as usize).saturating_sub(1)];
    let plan = Plan { tiers: vec![(2.0, 0.7), (3.0, 0.2)], trail: 0.3, max_age: 120.0, mom: None, be: None, ratchet: None };
    let pools = load_pools(&a[4..]);
    let (mut sel, mut rej, mut all) = (vec![], vec![], vec![]);
    let (mut sel_n, mut sel_2x, mut all_2x, mut sel_rug) = (0, 0, 0, 0);
    for (i, r) in test.iter().enumerate() {
        let k = r["pool"].as_str().unwrap();
        let Some(p) = pools.get(k) else { continue };
        let v: Vec<f64> = (0..3).filter_map(|sd| trade_amm(p, tau, &plan, sd).map(|e| e.ret)).collect();
        if v.is_empty() || i >= p2te.len() || i >= prte.len() { continue }
        let x = v.iter().sum::<f64>() / v.len() as f64;
        let y2 = r["y2x"].as_bool().unwrap_or(false);
        all.push(x);
        all_2x += y2 as usize;
        if p2te[i] * (1.0 - prte[i]) >= thr { sel.push(x); sel_n += 1; sel_2x += y2 as usize; sel_rug += r["rug5"].as_bool().unwrap_or(false) as usize } else { rej.push(x) }
    }
    println!("{}", json!({"tau": tau, "frac": frac, "plan": format!("{plan:?}"), "train_rows": train.len(), "new_test_coins": all.len(),
        "radar": {"selected": sel_n, "precision_2x": sel_2x as f64 / sel_n.max(1) as f64, "base_rate_2x": all_2x as f64 / all.len().max(1) as f64, "recall_2x": sel_2x as f64 / all_2x.max(1) as f64, "rug5_in_selected": sel_rug as f64 / sel_n.max(1) as f64},
        "SELECTED": stats(&sel), "REJECTED": stats(&rej), "BUY_ALL": stats(&all)}));
}
