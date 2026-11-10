//! Compare two raw datasets of the same pools (e.g. paid live vs free history), per pool, inside the live window
//! (first..last slot recorded live). Reports missing/extra signatures and, for common signatures, whether the
//! transaction content is identical: slot, blockTime, meta.err, fee, pre/post token balances, pre/post SOL balances,
//! log messages, inner instructions.
//!   banding <dir_A> <dir_B>      (reads <dir>/*/*.jsonl.zst)
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

fn load(dir: &str) -> (HashMap<String, HashMap<String, Value>>, HashMap<String, HashMap<String, u64>>) {
    let mut m: HashMap<String, HashMap<String, Value>> = HashMap::new();
    let mut lists: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if !p.to_string_lossy().ends_with(".jsonl.zst") { continue }
            let Ok(fh) = std::fs::File::open(&p) else { continue };
            let Ok(d) = zstd::Decoder::new(fh) else { continue };
            use std::io::BufRead;
            for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
                let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
                if r["kind"] == "sig_list" {
                    let e = lists.entry(r["pool"].as_str().unwrap_or("").to_string()).or_default();
                    for x in r["sigs"].as_array().into_iter().flatten() { e.insert(x[0].as_str().unwrap_or("").to_string(), x[1].as_u64().unwrap_or(0)); }
                    continue;
                }
                if r["kind"] != "tx" { continue }
                let pool = r["pool"].as_str().unwrap_or("").to_string();
                m.entry(pool).or_default().insert(r["sig"].as_str().unwrap_or("").to_string(), r["tx"].clone());
            }
        }
    }
    (m, lists)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let ((da, _), (db, lb)) = (load(&a[1]), load(&a[2]));
    let fields = ["slot", "blockTime", "meta.err", "meta.fee", "meta.preTokenBalances", "meta.postTokenBalances", "meta.preBalances", "meta.postBalances", "meta.logMessages", "meta.innerInstructions", "transaction"];
    let get = |tx: &Value, f: &str| -> Value { f.split('.').fold(tx.clone(), |v, k| v[k].clone()) };
    let mut tot: BTreeMap<String, u64> = BTreeMap::new();
    for (pool, sa) in &da {
        let Some(sb) = db.get(pool) else {
            println!("{}", json!({"pool": pool, "in_B": false}));
            *tot.entry("pools_missing_in_B".into()).or_insert(0) += 1;
            continue;
        };
        let (smin, smax) = sa.values().filter_map(|t| t["slot"].as_u64()).fold((u64::MAX, 0), |a, s| (a.0.min(s), a.1.max(s)));
        let bw: HashMap<&String, &Value> = sb.iter().filter(|(_, t)| t["slot"].as_u64().map_or(false, |s| s >= smin && s <= smax)).collect();
        // completeness from the full signature list of B when present (B may fetch only a sample)
        let empty = HashMap::new();
        let lw: HashMap<&String, u64> = lb.get(pool).unwrap_or(&empty).iter().filter(|(_, s)| **s >= smin && **s <= smax).map(|(k, s)| (k, *s)).collect();
        let (missing_in_b, missing_in_a, b_listed) = if lw.is_empty() {
            (sa.keys().filter(|s| !bw.contains_key(s)).count(), bw.keys().filter(|s| !sa.contains_key(**s)).count(), bw.len())
        } else {
            (sa.keys().filter(|s| !lw.contains_key(s)).count(), lw.keys().filter(|s| !sa.contains_key(**s)).count(), lw.len())
        };
        let mut diff: BTreeMap<&str, u64> = BTreeMap::new();
        let mut same = 0u64;
        for (s, ta) in sa {
            let Some(tb) = bw.get(s) else { continue };
            let mut all = true;
            for f in fields {
                if get(ta, f) != get(tb, f) { *diff.entry(f).or_insert(0) += 1; all = false }
            }
            same += all as u64;
        }
        *tot.entry("tx_in_A".into()).or_insert(0) += sa.len() as u64;
        *tot.entry("B_listed_in_window".into()).or_insert(0) += b_listed as u64;
        *tot.entry("B_fetched_compared".into()).or_insert(0) += bw.len() as u64;
        *tot.entry("missing_in_B".into()).or_insert(0) += missing_in_b as u64;
        *tot.entry("missing_in_A".into()).or_insert(0) += missing_in_a as u64;
        *tot.entry("common_identical".into()).or_insert(0) += same;
        for (k, v) in &diff { *tot.entry(format!("diff_{k}")).or_insert(0) += v }
        println!("{}", json!({"pool": pool, "A": sa.len(), "B_listed": b_listed, "B_fetched": bw.len(), "missing_in_B": missing_in_b, "missing_in_A": missing_in_a, "identical": same, "field_diffs": diff}));
    }
    println!("{}", json!({"TOTAL": tot}));
}
