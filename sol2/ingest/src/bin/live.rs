//! C2 live collector (read-only). Detects new pools and records EVERY transaction of each new pool (incl. failed) as raw
//! getTransaction JSON for its first TRACK_HOURS.
//!   live <raw_dir>
//! Env: ANKR_HTTP, ANKR_WSS; TRACK_HOURS (default 6); MAX_POOLS (default 50, concurrent tracked pools);
//!      SAMPLE_P (default 1.0, probability to track a new pool; the weight 1/SAMPLE_P is stored); MAX_TX_DAY (default
//!      150000, getTransaction budget per UTC day; when reached: no new pools, a "budget_stop" record); RUN_SECONDS (0 = forever).
//! Flow:
//!  1. WSS logsSubscribe(mentions program) per venue. A notification whose logs contain the venue's Anchor
//!     "Instruction: <create>" line is a candidate; getTransaction confirms it by the IDL discriminator -> pool address.
//!  2. A tracked pool gets its own logsSubscribe(mentions pool). Every notification (success or failed) -> getTransaction
//!     -> raw record. Right after subscribing, and after every reconnect, the pool is re-listed with
//!     getSignaturesForAddress so transactions between creation and subscription (or during a drop) are filled.
//!  3. Every WSS drop writes a {"kind":"gap"} record (recv times + last slot). Records keep block_time and recv_time apart.
//! The venue-wide streams are only used for detection and are not stored (disk: PumpSwap alone is ~800 tx/s).
use ingest::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::Message;

enum Job {
    Candidate { venue: usize, sig: String, recv: f64 },
    PoolTx { venue: usize, pool: String, sig: String, recv: f64, source: &'static str },
    Backfill { venue: usize, pool: String, since: f64 },
}

struct Pool {
    venue: usize,
    created: f64,
    sub_id: Option<u64>,
    seen: HashSet<String>,
}

fn env_f(k: &str, d: f64) -> f64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn rand01() -> f64 {
    // xorshift on time + counter; sampling only needs to be independent of pool properties
    use std::sync::atomic::{AtomicU64, Ordering};
    static S: AtomicU64 = AtomicU64::new(0);
    let mut x = S.fetch_add(0x9E3779B97F4A7C15, Ordering::Relaxed) ^ (now() * 1e6) as u64;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "/root/sol/raw".into());
    let wss = std::env::var("ANKR_WSS").unwrap_or_else(|_| { eprintln!("ANKR_WSS missing"); std::process::exit(2) });
    let track_s = env_f("TRACK_HOURS", 6.0) * 3600.0;
    let max_pools = env_f("MAX_POOLS", 50.0) as usize;
    let sample_p = env_f("SAMPLE_P", 1.0).clamp(0.0, 1.0);
    let max_tx_day = env_f("MAX_TX_DAY", 150000.0) as u64;
    let run_s = env_f("RUN_SECONDS", 0.0);
    let vs = venues();
    let rpc = Arc::new(Rpc::from_env());
    let w = Arc::new(Mutex::new(RawWriter::new(&dir)));
    let pools: Arc<Mutex<HashMap<String, Pool>>> = Arc::new(Mutex::new(HashMap::new()));
    // getTransaction count per UTC day (budget)
    let budget: Arc<Mutex<(String, u64, bool)>> = Arc::new(Mutex::new((date(now()), 0, false)));
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    // pools to subscribe (sent from workers to the WSS thread)
    let (sub_tx, sub_rx) = mpsc::channel::<String>();
    let t_begin = Instant::now();
    let stats: Arc<Mutex<HashMap<&'static str, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    let bump = |s: &Arc<Mutex<HashMap<&'static str, u64>>>, k: &'static str| *s.lock().unwrap().entry(k).or_insert(0) += 1;

    // ---------- workers ----------
    for _ in 0..12 {
        let (rpc, w, pools, budget, job_rx, sub_tx, job_tx2, stats) = (rpc.clone(), w.clone(), pools.clone(), budget.clone(), job_rx.clone(), sub_tx.clone(), job_tx.clone(), stats.clone());
        let vnames: Vec<&'static str> = vs.iter().map(|v| v.name).collect();
        std::thread::spawn(move || {
            let vs = venues();
            let spend = |n: u64| -> bool {
                let mut b = budget.lock().unwrap();
                let d = date(now());
                if b.0 != d { *b = (d, 0, false) }
                b.1 += n;
                b.1 <= max_tx_day
            };
            loop {
                let job = { let r = job_rx.lock().unwrap(); r.recv() };
                let Ok(job) = job else { break };
                match job {
                    Job::Candidate { venue, sig, recv } => {
                        if !spend(1) { continue }
                        bump(&stats, "candidate_fetch");
                        let Ok(tx) = rpc.get_tx(&sig, 10) else { bump(&stats, "candidate_fetch_failed"); continue };
                        for (ix_name, pool) in pool_creations(&tx, &vs[venue]) {
                            let mut ps = pools.lock().unwrap();
                            if ps.contains_key(&pool) { continue }
                            let over_budget = { let b = budget.lock().unwrap(); b.1 >= max_tx_day };
                            let tracked = ps.len() < max_pools && !over_budget && rand01() < sample_p;
                            let rec = json!({"kind": "pool_new", "venue": vnames[venue], "pool": pool, "create_ix": ix_name, "sig": sig,
                                "slot": tx["slot"], "block_time": tx["blockTime"], "recv_time": recv, "tracked": tracked,
                                "skip_reason": if tracked { Value::Null } else if over_budget { json!("budget") } else if ps.len() >= max_pools { json!("max_pools") } else { json!("sampling") },
                                "sample_p": sample_p, "weight": if sample_p > 0.0 { 1.0 / sample_p } else { 0.0 }});
                            w.lock().unwrap().write(vnames[venue], &rec);
                            bump(&stats, "pool_new");
                            if tracked {
                                let mut seen = HashSet::new();
                                seen.insert(sig.clone());
                                ps.insert(pool.clone(), Pool { venue, created: recv, sub_id: None, seen });
                                drop(ps);
                                w.lock().unwrap().write(vnames[venue], &tx_record(vnames[venue], &pool, "live_create", &sig, recv, &tx));
                                let _ = sub_tx.send(pool.clone());
                                let _ = job_tx2.send(Job::Backfill { venue, pool, since: tx["blockTime"].as_f64().unwrap_or(recv) - 5.0 });
                                bump(&stats, "pool_tracked");
                            }
                        }
                    }
                    Job::PoolTx { venue, pool, sig, recv, source } => {
                        {
                            let mut ps = pools.lock().unwrap();
                            match ps.get_mut(&pool) {
                                Some(p) => if !p.seen.insert(sig.clone()) { continue },
                                None => continue,
                            }
                        }
                        // budget is enforced on EVERY getTransaction (bug in the first test run: pool tx ignored the cap)
                        if !spend(1) {
                            bump(&stats, "pool_tx_skipped_budget");
                            w.lock().unwrap().write(vnames[venue], &json!({"kind": "skipped_budget", "venue": vnames[venue], "pool": pool, "sig": sig, "recv_time": recv}));
                            continue;
                        }
                        let rec = match rpc.get_tx(&sig, 10) {
                            Ok(tx) => { bump(&stats, "pool_tx"); tx_record(vnames[venue], &pool, source, &sig, recv, &tx) }
                            Err(e) => { bump(&stats, "pool_tx_fetch_failed"); json!({"kind": "fetch_failed", "venue": vnames[venue], "pool": pool, "source": source, "sig": sig, "recv_time": recv, "err": e}) }
                        };
                        w.lock().unwrap().write(vnames[venue], &rec);
                    }
                    Job::Backfill { venue, pool, since } => {
                        let (sigs, complete) = rpc.list_sigs(&pool, since, None, 50);
                        let n = sigs.len();
                        for s in sigs {
                            if let Some(sig) = s["signature"].as_str() {
                                let _ = job_tx2.send(Job::PoolTx { venue, pool: pool.clone(), sig: sig.into(), recv: now(), source: "backfill" });
                            }
                        }
                        w.lock().unwrap().write(vnames[venue], &json!({"kind": "backfill", "venue": vnames[venue], "pool": pool, "since": since,
                            "listed": n, "complete": complete, "recv_time": now()}));
                    }
                }
            }
        });
    }

    // ---------- WSS loop ----------
    let mut last_slot = 0u64;
    let mut t_report = Instant::now();
    loop {
        if run_s > 0.0 && t_begin.elapsed().as_secs_f64() > run_s { break }
        let (mut ws, _) = match tungstenite::connect(wss.as_str()) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("wss connect failed: {}", e.to_string().replace(&wss, "<wss>").chars().take(120).collect::<String>());
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        if let tungstenite::stream::MaybeTlsStream::NativeTls(s) = ws.get_mut() {
            let _ = s.get_mut().set_read_timeout(Some(Duration::from_millis(200)));
        }
        let connected = now();
        // request id -> what: venue index (detection) or pool address
        let mut req: HashMap<u64, Result<usize, String>> = HashMap::new();
        let mut sub_of: HashMap<u64, Result<usize, String>> = HashMap::new();
        let mut next_id = 1u64;
        let mut send_sub = |ws: &mut tungstenite::WebSocket<_>, addr: &str, what: Result<usize, String>, req: &mut HashMap<u64, Result<usize, String>>| {
            let id = next_id;
            next_id += 1;
            req.insert(id, what);
            let m = json!({"jsonrpc": "2.0", "id": id, "method": "logsSubscribe", "params": [{"mentions": [addr]}, {"commitment": "confirmed"}]});
            let _ = ws.send(Message::text(m.to_string()));
        };
        for (i, v) in vs.iter().enumerate() {
            send_sub(&mut ws, v.program, Ok(i), &mut req);
        }
        // (re)subscribe tracked pools and backfill what happened while disconnected
        let tracked: Vec<(String, usize)> = pools.lock().unwrap().iter().map(|(k, p)| (k.clone(), p.venue)).collect();
        for (p, vi) in tracked {
            send_sub(&mut ws, &p, Err(p.clone()), &mut req);
            let _ = job_tx.send(Job::Backfill { venue: vi, pool: p, since: connected - 600.0 });
        }
        eprintln!("wss connected; tracked pools {}", pools.lock().unwrap().len());
        let drop_reason: String = loop {
            if run_s > 0.0 && t_begin.elapsed().as_secs_f64() > run_s { break "run_seconds".into() }
            // new pools to subscribe
            while let Ok(p) = sub_rx.try_recv() {
                send_sub(&mut ws, &p, Err(p.clone()), &mut req);
            }
            // expire pools after TRACK_HOURS
            let expired: Vec<(String, Pool)> = {
                let mut ps = pools.lock().unwrap();
                let ks: Vec<String> = ps.iter().filter(|(_, p)| now() - p.created > track_s).map(|(k, _)| k.clone()).collect();
                ks.into_iter().filter_map(|k| ps.remove(&k).map(|p| (k, p))).collect()
            };
            for (k, p) in expired {
                if let Some(id) = p.sub_id {
                    let _ = ws.send(Message::text(json!({"jsonrpc": "2.0", "id": 0, "method": "logsUnsubscribe", "params": [id]}).to_string()));
                }
                w.lock().unwrap().write(vs[p.venue].name, &json!({"kind": "track_end", "venue": vs[p.venue].name, "pool": k, "tx_seen": p.seen.len(), "recv_time": now()}));
            }
            match ws.read() {
                Ok(Message::Text(t)) => {
                    let recv = now();
                    let Ok(m) = serde_json::from_str::<Value>(&t) else { continue };
                    if let Some(id) = m["id"].as_u64() {
                        if let Some(what) = req.remove(&id) {
                            match m["result"].as_u64() {
                                Some(sid) => {
                                    if let Err(p) = &what {
                                        if let Some(pp) = pools.lock().unwrap().get_mut(p) { pp.sub_id = Some(sid) }
                                    }
                                    sub_of.insert(sid, what);
                                }
                                None => eprintln!("subscribe error: {}", m["error"]),
                            }
                        }
                        continue;
                    }
                    let r = &m["params"]["result"];
                    let slot = r["context"]["slot"].as_u64().unwrap_or(0);
                    last_slot = last_slot.max(slot);
                    let sig = r["value"]["signature"].as_str().unwrap_or("").to_string();
                    match m["params"]["subscription"].as_u64().and_then(|s| sub_of.get(&s)) {
                        Some(Ok(vi)) => {
                            // venue stream: only look for pool-creation instruction log lines (successful tx only)
                            if !r["value"]["err"].is_null() { continue }
                            let logs = r["value"]["logs"].as_array();
                            let hit = logs.map_or(false, |l| l.iter().any(|x| x.as_str().map_or(false, |s| vs[*vi].create_logs.contains(&s))));
                            if hit {
                                let _ = job_tx.send(Job::Candidate { venue: *vi, sig, recv });
                            }
                        }
                        Some(Err(pool)) => {
                            let vi = pools.lock().unwrap().get(pool).map(|p| p.venue);
                            if let Some(vi) = vi {
                                let _ = job_tx.send(Job::PoolTx { venue: vi, pool: pool.clone(), sig, recv, source: "live" });
                            }
                        }
                        None => {}
                    }
                }
                Ok(Message::Close(_)) => break "closed by server".into(),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => break format!("read error: {}", e.to_string().chars().take(80).collect::<String>()),
            }
            if t_report.elapsed() > Duration::from_secs(60) {
                let b = budget.lock().unwrap().clone();
                eprintln!("min: {:?} | tracked {} | tx today {} / {} | {}", stats.lock().unwrap(), pools.lock().unwrap().len(), b.1, max_tx_day, calls_summary());
                if b.1 >= max_tx_day && !b.2 {
                    budget.lock().unwrap().2 = true;
                    w.lock().unwrap().write("_system", &json!({"kind": "budget_stop", "recv_time": now(), "tx_today": b.1}));
                }
                w.lock().unwrap().flush();
                t_report = Instant::now();
            }
        };
        let _ = ws.close(None);
        w.lock().unwrap().write("_system", &json!({"kind": "gap", "recv_time": now(), "connected_at": connected, "last_slot": last_slot, "reason": drop_reason}));
        w.lock().unwrap().flush();
        eprintln!("wss dropped ({drop_reason}); reconnecting");
        if drop_reason == "run_seconds" { break }
        std::thread::sleep(Duration::from_secs(1));
    }
    // let workers drain briefly, then final flush
    std::thread::sleep(Duration::from_secs(20));
    w.lock().unwrap().write("_system", &json!({"kind": "run_end", "recv_time": now(), "calls": calls_summary(), "stats": *stats.lock().unwrap()}));
    w.lock().unwrap().flush();
    eprintln!("end: {} | {:?}", calls_summary(), stats.lock().unwrap());
}
