//! Exit model, walk-forward and out-of-sample only. Pools sorted by t0 into 5 blocks; for block k the CatBoost
//! "dump in the next 30 s" model is trained on snapshots of earlier blocks only (purge 30 min) and scores block k.
//! Trading (entry = every liquid pool at TAU, i.e. buy-all, to isolate the exit): sell 100 % at the first snapshot whose
//! P(dump) >= theta (+ optional first tier). theta (and the tier option) are chosen on EARLIER out-of-sample blocks only;
//! block 1 uses theta 0.5. Execution = trade_amm_ext (latency, real fees, failure retries). Baselines on the same coins.
//!   exit_ml <snapshots.jsonl> <work_dir> <raw_file>...    env TAU (10)
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
    let mut seed = 23u64;
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
    let (snapf, work) = (&a[1], &a[2]);
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    std::fs::create_dir_all(work).unwrap();
    let snaps: Vec<Value> = std::fs::read_to_string(snapf).unwrap().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let feats: Vec<String> = snaps[0]["feat"].as_object().unwrap().keys().cloned().collect();
    // pools by t0 -> blocks
    let mut pt: Vec<(f64, String)> = { let mut m: HashMap<String, f64> = HashMap::new(); for s in &snaps { m.insert(s["pool"].as_str().unwrap().into(), s["t0"].as_f64().unwrap()); } m.into_iter().map(|(k, v)| (v, k)).collect() };
    pt.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let np = pt.len();
    let blk: HashMap<String, usize> = pt.iter().enumerate().map(|(i, (_, k))| (k.clone(), (i * 5 / np).min(4))).collect();
    let t0of: HashMap<String, f64> = pt.iter().map(|(t, k)| (k.clone(), *t)).collect();
    let cd = format!("{work}/cd");
    std::fs::write(&cd, "0\tLabel\n").unwrap();
    let write = |path: &str, set: &[&Value]| {
        let mut f = std::fs::File::create(path).unwrap();
        for r in set {
            let mut line = format!("{}", r["dump"].as_bool().unwrap_or(false) as u8);
            for k in &feats { line += &format!("\t{}", r["feat"][k].as_f64().map_or("nan".to_string(), |v| v.to_string())) }
            writeln!(f, "{line}").unwrap();
        }
    };
    // OOS P(dump) per snapshot
    let mut pd: Vec<Option<f64>> = vec![None; snaps.len()];
    let mut aucv = vec![];
    for k in 1..5 {
        let te_i: Vec<usize> = (0..snaps.len()).filter(|i| blk[snaps[*i]["pool"].as_str().unwrap()] == k).collect();
        let t_test = pt.iter().filter(|(_, p)| blk[p] == k).map(|x| x.0).fold(f64::INFINITY, f64::min);
        let tr: Vec<&Value> = snaps.iter().filter(|s| { let p = s["pool"].as_str().unwrap(); blk[p] < k && t0of[p] < t_test - 1800.0 }).collect();
        let te: Vec<&Value> = te_i.iter().map(|i| &snaps[*i]).collect();
        if tr.len() < 200 { continue }
        let (ftr, fte, m, out) = (format!("{work}/tr{k}.tsv"), format!("{work}/te{k}.tsv"), format!("{work}/m{k}.bin"), format!("{work}/p{k}.tsv"));
        write(&ftr, &tr);
        write(&fte, &te);
        let _ = Command::new(CB).args(["fit", "--learn-set", &ftr, "--column-description", &cd, "--loss-function", "Logloss", "-i", "500", "--depth", "5",
            "--learning-rate", "0.05", "--l2-leaf-reg", "6", "--random-seed", "7", "--train-dir", &format!("{work}/cb{k}"), "-m", &m, "--logging-level", "Silent"]).output();
        let _ = Command::new(CB).args(["calc", "-m", &m, "--input-path", &fte, "--column-description", &cd, "--output-path", &out, "--prediction-type", "Probability"]).output();
        let pr: Vec<f64> = std::fs::read_to_string(&out).unwrap_or_default().lines().skip(1).filter_map(|l| l.split('\t').last().and_then(|x| x.parse().ok())).collect();
        if pr.len() != te.len() { eprintln!("fold {k}: prediction mismatch"); continue }
        for (j, i) in te_i.iter().enumerate() { pd[*i] = Some(pr[j]); aucv.push((pr[j], snaps[*i]["dump"].as_bool().unwrap_or(false))) }
    }
    // first trigger time per (pool, theta)
    let thetas = [0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
    let mut trig: HashMap<(String, usize), f64> = HashMap::new();
    for (i, s) in snaps.iter().enumerate() {
        let Some(p) = pd[i] else { continue };
        let pool = s["pool"].as_str().unwrap().to_string();
        let t = s["t"].as_f64().unwrap();
        for (ti, th) in thetas.iter().enumerate() {
            if p >= *th { let e = trig.entry((pool.clone(), ti)).or_insert(t); if t < *e { *e = t } }
        }
    }
    let pools = load_pools(&a[3..]);
    let plans = [("ml_only", Plan { tiers: vec![], trail: 1.0, max_age: 900.0, mom: None, be: None, ratchet: None }), ("ml+30%@2x", Plan { tiers: vec![(2.0, 0.3)], trail: 1.0, max_age: 900.0, mom: None, be: None, ratchet: None })];
    let base = Plan { tiers: vec![(2.0, 0.7), (3.0, 0.2)], trail: 0.3, max_age: 120.0, mom: None, be: None, ratchet: None };
    let ret = |pool: &str, pl: &Plan, ex: f64| -> Option<f64> {
        let p = pools.get(pool)?;
        let v: Vec<f64> = (0..3).filter_map(|sd| trade_amm_ext(p, tau, pl, sd, ex).map(|e| e.ret)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    let trim = |v: &[f64]| -> f64 { if v.len() < 15 { return f64::NEG_INFINITY } let mut s = v.to_vec(); s.sort_by(|a, b| a.partial_cmp(b).unwrap()); s[..s.len() - 3].iter().sum::<f64>() / (s.len() - 3) as f64 };
    let pools_in = |f: &dyn Fn(usize) -> bool| -> Vec<String> { pt.iter().filter(|(_, p)| f(blk[p])).map(|x| x.1.clone()).collect() };
    let (mut ml, mut b_def, mut b_hold, mut folds) = (vec![], vec![], vec![], vec![]);
    for k in 1..5 {
        let cur = pools_in(&|b| b == k);
        let past = pools_in(&|b| b >= 1 && b < k);
        let (mut bt, mut bp, mut bs) = (2usize, 0usize, f64::NEG_INFINITY);
        if !past.is_empty() {
            for ti in 0..thetas.len() { for pi in 0..plans.len() {
                let v: Vec<f64> = past.iter().filter_map(|p| ret(p, &plans[pi].1, *trig.get(&(p.clone(), ti)).unwrap_or(&f64::INFINITY))).collect();
                let s = trim(&v);
                if s > bs { bs = s; bt = ti; bp = pi }
            } }
        }
        let v: Vec<f64> = cur.iter().filter_map(|p| ret(p, &plans[bp].1, *trig.get(&(p.clone(), bt)).unwrap_or(&f64::INFINITY))).collect();
        ml.extend(v.iter().cloned());
        b_def.extend(cur.iter().filter_map(|p| ret(p, &base, f64::INFINITY)));
        b_hold.extend(cur.iter().filter_map(|p| ret(p, &Plan { tiers: vec![], trail: 1.0, max_age: 300.0, mom: None, be: None, ratchet: None }, f64::INFINITY)));
        folds.push(json!({"fold": k, "theta": thetas[bt], "plan": plans[bp].0, "result": stats(&v)}));
    }
    // ALL variants (no selection, descriptive, blocks 1-4 out-of-sample): fraction sold at 2x x ML threshold; plus how much
    // of the moonshots (peak >= 10x before the LP removal) each variant actually captured
    let oos_pools = pools_in(&|b| b >= 1);
    let moon: Vec<String> = oos_pools.iter().filter(|k| pools.get(*k).map_or(false, |p| {
        let t0 = p.swaps[0].t; let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        p.swaps.iter().position(|w| w.t > t0 + tau).map_or(false, |e| p.swaps[e..].iter().filter(|w| w.t < rug).map(|w| w.px).fold(0.0, f64::max) >= 10.0 * p.swaps[e].px)
    })).cloned().collect();
    let mut variants = vec![];
    for f in [0.0, 0.3, 0.5, 0.7] { for (ti, th) in thetas.iter().enumerate() {
        let pl = Plan { tiers: if f > 0.0 { vec![(2.0, f)] } else { vec![] }, trail: 1.0, max_age: 900.0, mom: None, be: None, ratchet: None };
        let v: Vec<f64> = oos_pools.iter().filter_map(|p| ret(p, &pl, *trig.get(&(p.clone(), ti)).unwrap_or(&f64::INFINITY))).collect();
        let mv: Vec<f64> = moon.iter().filter_map(|p| ret(p, &pl, *trig.get(&(p.clone(), ti)).unwrap_or(&f64::INFINITY))).collect();
        variants.push(json!({"sell_at_2x": f, "ml_theta": th, "all": stats(&v), "moonshots_n": mv.len(), "moonshot_mean_ret": if mv.is_empty() { f64::NAN } else { (mv.iter().sum::<f64>() / mv.len() as f64 * 1000.0).round() / 1000.0 }}));
    } }
    let mvb: Vec<f64> = moon.iter().filter_map(|p| ret(p, &base, f64::INFINITY)).collect();
    variants.push(json!({"OLD_TP_70_20_120s": true, "moonshots_n": mvb.len(), "moonshot_mean_ret": if mvb.is_empty() { f64::NAN } else { (mvb.iter().sum::<f64>() / mvb.len() as f64 * 1000.0).round() / 1000.0 }}));
    println!("{}", json!({"tau": tau, "snapshots": snaps.len(), "auc_dump_oos": (auc(&aucv) * 1000.0).round() / 1000.0, "folds": folds,
        "EXIT_ML_buy_all": stats(&ml), "OLD_TP_70_20_120s_buy_all": stats(&b_def), "HOLD_300s_buy_all": stats(&b_hold), "VARIANTS": variants, "THETAS": thetas, "TRIGGERS": trig.iter().map(|((p, ti), t)| json!([p, ti, t])).collect::<Vec<_>>()}));
}
