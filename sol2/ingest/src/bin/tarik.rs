//! C2 history puller (read-only): every transaction of one pool address, incl. failed ones, stored raw.
//!   tarik <raw_dir> <venue> <pool> [hours_back=13]
//! getSignaturesForAddress(pool) pages back to `hours_back` (or the start of the pool), then getTransaction for each in
//! chain order with 8 workers. A tx that cannot be fetched is written as {"kind":"fetch_failed"} (never dropped silently).
//! Writes a {"kind":"pull"} summary record with the signature count, so cek_sig can test completeness.
use ingest::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: tarik <raw_dir> <venue> <pool> [hours_back=13]");
        std::process::exit(2);
    }
    let (dir, vname, pool) = (a[1].clone(), a[2].clone(), a[3].clone());
    let hours: f64 = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(13.0);
    if venue(&vname).is_none() {
        eprintln!("unknown venue {vname}");
        std::process::exit(2);
    }
    let rpc = Arc::new(Rpc::from_env());
    let t_start = now();
    let (sigs, complete) = rpc.list_sigs(&pool, t_start - hours * 3600.0, None, 200);
    // dedupe (a signature can only appear once in a correct listing; count it if not)
    let mut seen = std::collections::HashSet::new();
    let mut dup = 0;
    let list: Vec<String> = sigs.iter().filter_map(|x| x["signature"].as_str().map(String::from)).filter(|s| if seen.insert(s.clone()) { true } else { dup += 1; false }).collect();
    eprintln!("{vname} {pool}: {} signatures (complete listing: {complete}, duplicates in listing: {dup})", list.len());
    let w = Arc::new(Mutex::new(RawWriter::new(&dir)));
    // full signature list (with slots) is always stored, so completeness can be checked even when only a sample is fetched
    w.lock().unwrap().write(&vname, &json!({"kind": "sig_list", "venue": vname, "pool": pool, "recv_time": now(), "sigs": sigs.iter().map(|x| json!([x["signature"], x["slot"], x["err"].is_null()])).collect::<Vec<_>>()}));
    let every: usize = std::env::var("SAMPLE_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let queue = Arc::new(Mutex::new(list.clone().into_iter().enumerate().filter(|(i, _)| i % every == 0).collect::<Vec<_>>()));
    let failed = Arc::new(Mutex::new(0usize));
    let hs: Vec<_> = (0..8).map(|_| {
        let (rpc, w, q, failed, vname, pool) = (rpc.clone(), w.clone(), queue.clone(), failed.clone(), vname.clone(), pool.clone());
        std::thread::spawn(move || loop {
            let Some((_, sig)) = q.lock().unwrap().pop() else { break };
            let rec = match rpc.get_tx(&sig, 3) {
                Ok(tx) => tx_record(&vname, &pool, "tarik", &sig, now(), &tx),
                Err(e) => {
                    *failed.lock().unwrap() += 1;
                    json!({"kind": "fetch_failed", "venue": vname, "pool": pool, "source": "tarik", "sig": sig, "recv_time": now(), "err": e})
                }
            };
            w.lock().unwrap().write(&vname, &rec);
        })
    }).collect();
    for h in hs {
        let _ = h.join();
    }
    let nf = *failed.lock().unwrap();
    let mut w = w.lock().unwrap();
    w.write(&vname, &json!({"kind": "pull", "venue": vname, "pool": pool, "recv_time": now(), "t_start": t_start, "hours_back": hours,
        "signatures": list.len(), "listing_complete": complete, "listing_duplicates": dup, "fetch_failed": nf,
        "first_sig": list.first(), "last_sig": list.last(), "calls": calls_summary()}));
    w.flush();
    eprintln!("done: {} tx, fetch_failed {nf}, {}", list.len(), calls_summary());
}
