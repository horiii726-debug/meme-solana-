//! C2 history backfill (read-only, free endpoints by default): pool creations from migration accounts, then every
//! transaction (incl. failed) of each new pool for its first HOURS, stored raw like `live`.
//!   riwayat <raw_dir> <t_from_unix> <t_to_unix>
//! Env: RPC_URLS (see ingest::Rpc), HOURS (default 6), MAX_POOLS (default 0 = all; else a uniform random sample whose
//!      weight is stored), MAX_PAGES_POOL (default 300 pages of 1000 signatures per pool), WORKERS (default 4),
//!      ROUTES (default "pump,dbc").
//! Creation sources (found from the stored creation txs, reports/rebuild/C3/akun_lahir.json, and checked on chain):
//!   pump: 9C4nRvhhVquCKATjDCx5FKvNS9PNgNqgyWy9AcoDjYv5 — every sampled tx is Pump.fun `MigrateV2` (-> PumpSwap pool)
//!   dbc:  A8gMrEPJkacWkcb3DGwtJwTe16HktSEfvwtuDh2MCtck — sampled txs are DAMM v2 `InitializePoolWithDynamicConfig`
//! Each creation tx is confirmed by the IDL discriminator (ingest::pool_creations); unconfirmed ones are counted.
//! Per pool the signature list is paged back from the newest; pools whose listing hits MAX_PAGES_POOL are marked
//! {"kind":"pull","listing_complete":false} (never silently partial).
use ingest::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn env_f(k: &str, d: f64) -> f64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: riwayat <raw_dir> <t_from_unix> <t_to_unix>");
        std::process::exit(2);
    }
    let dir = a[1].clone();
    let (t_from, t_to): (f64, f64) = (a[2].parse().unwrap_or(0.0), a[3].parse().unwrap_or(0.0));
    let hours = env_f("HOURS", 6.0);
    let max_pools = env_f("MAX_POOLS", 0.0) as usize;
    let max_pages = env_f("MAX_PAGES_POOL", 300.0) as usize;
    let workers = env_f("WORKERS", 4.0) as usize;
    let routes = std::env::var("ROUTES").unwrap_or_else(|_| "pump,dbc".into());
    let rpc = Arc::new(Rpc::from_env());
    let w = Arc::new(Mutex::new(RawWriter::new(&dir)));
    let age = |t: f64| (now() - t) / 3600.0 + 1.0;

    // ---------- 1. creations ----------
    let mut pools: Vec<(String, String, f64, String)> = vec![]; // (venue, pool, creation block time, creation sig)
    for (route, acct, venue) in [("pump", "9C4nRvhhVquCKATjDCx5FKvNS9PNgNqgyWy9AcoDjYv5", "pumpswap"), ("dbc", "A8gMrEPJkacWkcb3DGwtJwTe16HktSEfvwtuDh2MCtck", "meteora_damm_v2")] {
        if !routes.split(',').any(|r| r == route) { continue }
        let v = ingest::venue(venue).unwrap();
        // newest-first paging from t_to back to t_from
        let mut sigs = vec![];
        let mut before: Option<String> = None;
        'pg: loop {
            let mut o = json!({"limit": 1000});
            if let Some(b) = &before { o["before"] = json!(b) }
            let Ok(r) = rpc.call_age("getSignaturesForAddress", json!([acct, o]), age(t_from)) else { eprintln!("listing {route} failed"); break };
            let arr = r.as_array().cloned().unwrap_or_default();
            for x in &arr {
                let t = x["blockTime"].as_f64().unwrap_or(0.0);
                if t < t_from { break 'pg }
                if t <= t_to && x["err"].is_null() { sigs.push((x["signature"].as_str().unwrap_or("").to_string(), t)) }
            }
            if arr.len() < 1000 { break }
            before = arr.last().and_then(|x| x["signature"].as_str()).map(String::from);
        }
        eprintln!("{route}: {} successful txs on the creation account in window", sigs.len());
        let (mut conf, mut unconf) = (0, 0);
        for (s, t) in sigs {
            match rpc.get_tx_age(&s, 3, age(t)) {
                Ok(tx) => {
                    let c = pool_creations(&tx, &v);
                    if c.is_empty() { unconf += 1; continue }
                    for (ixn, p) in c {
                        conf += 1;
                        w.lock().unwrap().write(venue, &json!({"kind": "pool_new", "venue": venue, "pool": p, "create_ix": ixn, "route": route,
                            "sig": s, "slot": tx["slot"], "block_time": tx["blockTime"], "recv_time": now(), "source": "history"}));
                        w.lock().unwrap().write(venue, &tx_record(venue, &p, "history_create", &s, now(), &tx));
                        pools.push((venue.to_string(), p, tx["blockTime"].as_f64().unwrap_or(t), s.clone()));
                    }
                }
                Err(e) => eprintln!("creation tx {s}: {e}"),
            }
        }
        eprintln!("{route}: confirmed pool creations {conf}, txs without a creation instruction {unconf}");
        w.lock().unwrap().write(venue, &json!({"kind": "creations_scan", "route": route, "venue": venue, "t_from": t_from, "t_to": t_to,
            "confirmed": conf, "unconfirmed": unconf, "recv_time": now()}));
    }
    // ---------- 2. sampling ----------
    // POOL_LIST=<file "venue pool t0 [creation_sig]" per line>: pull exactly these pools (creation scan still limited by ROUTES)
    if let Ok(p) = std::env::var("POOL_LIST") {
        for l in std::fs::read_to_string(&p).unwrap_or_default().lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() >= 3 {
                pools.push((f[0].to_string(), f[1].to_string(), f[2].parse().unwrap_or(0.0), f.get(3).unwrap_or(&"").to_string()));
            }
        }
    }
    // EXCLUDE=<file with one pool address per line>: pools already collected elsewhere are not pulled again
    if let Ok(p) = std::env::var("EXCLUDE") {
        let ex: std::collections::HashSet<String> = std::fs::read_to_string(&p).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        let before = pools.len();
        pools.retain(|x| !ex.contains(&x.1));
        eprintln!("excluded {} already-collected pools", before - pools.len());
    }
    let total = pools.len();
    if max_pools > 0 && pools.len() > max_pools {
        // deterministic uniform sample: order by a hash of the pool address (independent of pool behaviour)
        pools.sort_by_key(|p| p.1.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211)));
        pools.truncate(max_pools);
    }
    let weight = total as f64 / pools.len().max(1) as f64;
    eprintln!("pools to pull: {} of {} (weight {weight:.2})", pools.len(), total);
    // ---------- 3. every tx of each pool for its first HOURS ----------
    let q = Arc::new(Mutex::new(pools));
    // Optional Helius getTransactionsForAddress path (GTFA=1): 100 full transactions per call, incl. failed, ascending,
    // filtered to [t0 - 60, t0 + HOURS]. Hard cap GTFA_MAX_CALLS (default 3000). Verified 2026-11-09 on one pool:
    // page 1 starts at the pool's creation blockTime and contains failed txs.
    let gtfa: Option<(String, u64)> = if std::env::var("GTFA").map(|v| v == "1").unwrap_or(false) {
        std::env::var("HELIUS_HTTP").ok().map(|u| (u, std::env::var("GTFA_MAX_CALLS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000)))
    } else { None };
    let gtfa_calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let gtfa_pace = Arc::new(Mutex::new(std::time::Instant::now()));
    let hs: Vec<_> = (0..workers).map(|_| {
        let (rpc, w, q, gtfa, gtfa_calls, gtfa_pace) = (rpc.clone(), w.clone(), q.clone(), gtfa.clone(), gtfa_calls.clone(), gtfa_pace.clone());
        std::thread::spawn(move || loop {
            let Some((venue, pool, t0, csig)) = q.lock().unwrap().pop() else { break };
            let a_h = (now() - t0) / 3600.0 + 1.0;
            if let Some((url, cap)) = &gtfa {
                let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(90)).build();
                let (mut token, mut n, mut pages, mut ok_all) = (None::<String>, 0usize, 0usize, true);
                loop {
                    if gtfa_calls.load(std::sync::atomic::Ordering::Relaxed) >= *cap { ok_all = false; break }
                    {   // global pacing: one gTFA call every 250 ms (Helius limit measured ~2-5 req/s)
                        let mut nx = gtfa_pace.lock().unwrap();
                        let now_i = std::time::Instant::now();
                        if *nx > now_i { std::thread::sleep(*nx - now_i) }
                        *nx = std::time::Instant::now() + std::time::Duration::from_millis(std::env::var("GTFA_PACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(250));
                    }
                    let mut o = json!({"transactionDetails": "full", "limit": 100, "sortOrder": "asc", "encoding": "json", "maxSupportedTransactionVersion": 1,
                        "commitment": "confirmed", "filters": {"blockTime": {"gte": (t0 - 60.0) as i64, "lte": (t0 + hours * 3600.0) as i64}}});
                    if let Some(t) = &token { o["paginationToken"] = json!(t) }
                    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "getTransactionsForAddress", "params": [pool, o]});
                    let mut res = None;
                    for k in 0..6u32 {
                        gtfa_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        match agent.post(url).send_json(body.clone()).ok().and_then(|r| r.into_json::<serde_json::Value>().ok()) {
                            Some(v) if v.get("result").is_some() => { res = Some(v["result"].clone()); break }
                            _ => std::thread::sleep(std::time::Duration::from_millis(500 << k)),
                        }
                    }
                    let Some(r) = res else { ok_all = false; break };
                    pages += 1;
                    for tx in r["data"].as_array().into_iter().flatten() {
                        let s = tx["transaction"]["signatures"][0].as_str().unwrap_or("").to_string();
                        if s == csig { continue }
                        n += 1;
                        w.lock().unwrap().write(&venue, &tx_record(&venue, &pool, "history_gtfa", &s, now(), tx));
                    }
                    token = r["paginationToken"].as_str().map(String::from);
                    if token.is_none() { break }
                }
                w.lock().unwrap().write(&venue, &json!({"kind": "pull", "method": "gtfa", "venue": venue, "pool": pool, "t0": t0, "hours": hours, "fetched": n,
                    "fetch_failed": 0, "listing_complete": ok_all, "listing_pages": pages, "weight": weight, "recv_time": now()}));
                eprintln!("{venue} {pool}: {n} tx (gtfa, complete {ok_all}, pages {pages})");
                if ok_all { continue }
                // cap reached or failure: fall through to the standard path for this pool
            }
            // signatures newest-first; keep those within [t0, t0 + HOURS]
            let mut keep = vec![];
            // Jump to the end of the window: `before` accepts ANY confirmed signature (verified 2026-11-09: anchor from
            // another address -> all returned pool signatures have blockTime <= anchor time). Anchor = a signature of
            // the block at the estimated slot of t0 + HOURS (0.2146 s/slot measured in C1), so busy pools are not
            // paged back from "now".
            let te = t0 + hours * 3600.0;
            let mut anchor: Option<String> = None;
            if te < now() - 120.0 {
                if let Some(tip) = rpc.call("getSlot", json!([])).ok().and_then(|v| v.as_u64()) {
                    let est = tip.saturating_sub(((now() - te) / 0.2146) as u64);
                    for k in 0..8u64 {
                        let r = rpc.call_age("getBlock", json!([est + k, {"encoding": "json", "transactionDetails": "signatures", "maxSupportedTransactionVersion": 1, "rewards": false}]), a_h);
                        if let Ok(b) = r {
                            if b["blockTime"].as_f64().map_or(false, |bt| bt >= te - 120.0) {
                                anchor = b["signatures"].as_array().and_then(|a| a.last()).and_then(|s| s.as_str()).map(String::from);
                            }
                            if anchor.is_some() { break }
                        }
                    }
                }
            }
            let (mut before, mut complete, mut pages) = (anchor, false, 0);
            while pages < max_pages {
                let mut o = json!({"limit": 1000});
                if let Some(b) = &before { o["before"] = json!(b) }
                let Ok(r) = rpc.call_age("getSignaturesForAddress", json!([pool, o]), a_h) else { break };
                pages += 1;
                let arr = r.as_array().cloned().unwrap_or_default();
                let mut done = false;
                for x in &arr {
                    let t = x["blockTime"].as_f64().unwrap_or(0.0);
                    if t < t0 - 60.0 { done = true; break }
                    if t <= t0 + hours * 3600.0 { keep.push(x["signature"].as_str().unwrap_or("").to_string()) }
                }
                if done || arr.len() < 1000 { complete = true; break }
                before = arr.last().and_then(|x| x["signature"].as_str()).map(String::from);
            }
            keep.reverse();
            let (mut ok, mut failed) = (0, 0);
            for s in &keep {
                if *s == csig { continue }
                match rpc.get_tx_age(s, 3, a_h) {
                    Ok(tx) => { ok += 1; w.lock().unwrap().write(&venue, &tx_record(&venue, &pool, "history", s, now(), &tx)) }
                    Err(e) => { failed += 1; w.lock().unwrap().write(&venue, &json!({"kind": "fetch_failed", "venue": venue, "pool": pool, "source": "history", "sig": s, "recv_time": now(), "err": e})) }
                }
            }
            w.lock().unwrap().write(&venue, &json!({"kind": "pull", "venue": venue, "pool": pool, "t0": t0, "hours": hours, "signatures": keep.len(),
                "fetched": ok, "fetch_failed": failed, "listing_complete": complete, "listing_pages": pages, "weight": weight, "recv_time": now()}));
            eprintln!("{venue} {pool}: {ok} tx (failed {failed}, listing complete {complete}, pages {pages})");
        })
    }).collect();
    for h in hs { let _ = h.join(); }
    let mut w = w.lock().unwrap();
    w.write("_system", &json!({"kind": "run_end", "tool": "riwayat", "recv_time": now(), "calls": calls_summary()}));
    w.flush();
    eprintln!("end {}", calls_summary());
}

#[allow(dead_code)]
fn _unused(_: Value) {}
