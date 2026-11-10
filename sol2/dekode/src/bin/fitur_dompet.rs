//! Adds C7 wallet features to a feature dump. For each (pool, tau) only the early buyers whose first buy happened at or
//! before t0 + tau are used (causal); their history is only what existed BEFORE their buy (dompet.rs).
//!   fitur_dompet <dump_in.jsonl> <early_buyers.tsv> <dompet.jsonl> <dump_out.jsonl>
//! Features (prefix w_): n_known, fresh_share (<= 5 earlier txs), zero_hist_share (0 earlier txs), med_prior,
//! age_med_h (hours since the oldest earlier tx; >= 100 earlier txs counts as 1000 h), funded_10m_share (first funding
//! within 10 min before the buy), funded_1h_share, same_funder_max_share (largest group of buyers sharing one funder),
//! funder_groups_ratio (distinct funders / buyers with a known funder), creator_funded_share, med_funded_sol.
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;

fn med(v: &mut Vec<f64>) -> f64 { if v.is_empty() { return f64::NAN } v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] }

fn main() {
    let a: Vec<String> = std::env::args().collect();
    // pool -> [(wallet, buy_time)], t0, creator
    let mut eb: HashMap<String, (Vec<(String, f64)>, f64, String)> = HashMap::new();
    for l in std::fs::read_to_string(&a[2]).unwrap_or_default().lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 6 { continue }
        let e = eb.entry(f[0].to_string()).or_insert((vec![], f[4].parse().unwrap_or(0.0), f[5].to_string()));
        e.0.push((f[1].to_string(), f[3].parse().unwrap_or(0.0)));
    }
    let wal: HashMap<(String, String), Value> = std::fs::read_to_string(&a[3]).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| ((v["pool"].as_str().unwrap_or("").to_string(), v["wallet"].as_str().unwrap_or("").to_string()), v)).collect();
    let mut out = std::fs::File::create(&a[4]).unwrap();
    let (mut rows, mut with) = (0, 0);
    for l in std::fs::read_to_string(&a[1]).unwrap().lines() {
        let Ok(mut r) = serde_json::from_str::<Value>(l) else { continue };
        rows += 1;
        let pool = r["pool"].as_str().unwrap_or("").to_string();
        let tau = r["tau"].as_f64().unwrap_or(0.0);
        let mut m = Map::new();
        if let Some((buyers, t0, creator)) = eb.get(&pool) {
            let known: Vec<(&Value, f64)> = buyers.iter().filter(|(_, t)| *t <= t0 + tau).filter_map(|(w, t)| wal.get(&(pool.clone(), w.clone())).filter(|v| v.get("err").is_none()).map(|v| (v, *t))).collect();
            let n = known.len() as f64;
            if n > 0.0 {
                with += 1;
                let np: Vec<f64> = known.iter().map(|(v, _)| v["n_prior"].as_f64().unwrap_or(0.0)).collect();
                let mut ages: Vec<f64> = known.iter().map(|(v, t)| if v["n_prior"].as_f64().unwrap_or(0.0) >= 100.0 { 1000.0 } else { v["oldest_prior"].as_f64().map_or(0.0, |o| (t - o) / 3600.0) }).collect();
                let funded: Vec<(&str, f64, f64)> = known.iter().filter_map(|(v, t)| Some((v["funder"].as_str()?, *t - v["funded_at"].as_f64().unwrap_or(0.0), v["funded_sol"].as_f64().unwrap_or(0.0)))).collect();
                let mut groups: HashMap<&str, usize> = HashMap::new();
                for f in &funded { *groups.entry(f.0).or_insert(0) += 1 }
                let nf = funded.len().max(1) as f64;
                m.insert("w_n_known".into(), json!(n));
                m.insert("w_fresh_share".into(), json!(np.iter().filter(|x| **x <= 5.0).count() as f64 / n));
                m.insert("w_zero_hist_share".into(), json!(np.iter().filter(|x| **x == 0.0).count() as f64 / n));
                m.insert("w_med_prior".into(), json!(med(&mut np.clone())));
                m.insert("w_age_med_h".into(), json!(med(&mut ages)));
                m.insert("w_funded_10m_share".into(), json!(funded.iter().filter(|f| f.1 <= 600.0).count() as f64 / n));
                m.insert("w_funded_1h_share".into(), json!(funded.iter().filter(|f| f.1 <= 3600.0).count() as f64 / n));
                m.insert("w_same_funder_max_share".into(), json!(groups.values().cloned().max().unwrap_or(0) as f64 / n));
                m.insert("w_funder_groups_ratio".into(), json!(groups.len() as f64 / nf));
                m.insert("w_creator_funded_share".into(), json!(funded.iter().filter(|f| !creator.is_empty() && f.0 == creator).count() as f64 / n));
                m.insert("w_med_funded_sol".into(), json!(med(&mut funded.iter().map(|f| f.2).collect())));
            }
        }
        for k in ["w_n_known", "w_fresh_share", "w_zero_hist_share", "w_med_prior", "w_age_med_h", "w_funded_10m_share", "w_funded_1h_share", "w_same_funder_max_share", "w_funder_groups_ratio", "w_creator_funded_share", "w_med_funded_sol"] {
            let v = m.get(k).cloned().unwrap_or(Value::Null);
            r["feat"][k] = if v.as_f64().map_or(false, |x| x.is_finite()) { v } else { Value::Null };
        }
        let _ = writeln!(out, "{r}");
    }
    eprintln!("rows {rows}, rows with wallet info {with}");
}
