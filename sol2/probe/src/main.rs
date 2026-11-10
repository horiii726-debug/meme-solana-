//! C1 — Ankr capability probe (read-only). Never sends transactions, never prints the endpoint (it holds the API key).
//! Env: ANKR_HTTP, ANKR_WSS (from /root/sol/.rpc.env). Output: one JSON object per line on stdout (results), progress on stderr.
//! Program IDs and instruction discriminators come from idl/*.json (`address`, `instructions[].discriminator`).
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// idl/pump_amm.json, idl/raydium_cp_swap.json, idl/meteora_damm_v2.json (`address`)
const PUMPSWAP: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
const CPMM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
const DAMM2: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
const WSOL: &str = "So11111111111111111111111111111111111111112";

/// (venue, program, swap discriminators, create discriminators, pool account index in swap ix) — from the IDLs
fn venues() -> Vec<(&'static str, &'static str, Vec<[u8; 8]>, Vec<[u8; 8]>, usize)> {
    vec![
        ("pumpswap", PUMPSWAP,
         vec![[102, 6, 61, 18, 1, 218, 235, 234], [198, 46, 21, 82, 180, 217, 232, 112], [51, 230, 133, 164, 1, 127, 131, 173]],
         vec![[233, 146, 209, 142, 207, 104, 64, 188]], 0),
        ("raydium_cpmm", CPMM,
         vec![[143, 190, 90, 218, 196, 30, 51, 222], [55, 217, 98, 86, 163, 74, 180, 173]],
         vec![[175, 175, 109, 31, 13, 152, 155, 237]], 3),
        ("meteora_damm_v2", DAMM2,
         vec![[248, 198, 158, 145, 225, 117, 135, 200], [65, 75, 63, 76, 235, 91, 91, 136]],
         vec![[20, 161, 241, 24, 189, 221, 180, 2], [95, 180, 10, 172, 84, 174, 232, 40], [149, 82, 72, 197, 253, 252, 68, 15]], 1),
    ]
}

static CALLS: AtomicU64 = AtomicU64::new(0);
static PER_METHOD: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn out(v: Value) {
    println!("{v}");
}

struct Rpc {
    url: String,
    agent: ureq::Agent,
}

impl Rpc {
    /// one JSON-RPC call; Err = "http <code>" / "rpc <code> <message>" / "net" (URL never included)
    fn call(&self, method: &str, params: Value) -> (Result<Value, String>, f64) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        *PER_METHOD.lock().unwrap().entry(method.to_string()).or_insert(0) += 1;
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let t = Instant::now();
        let r = self.agent.post(&self.url).timeout(Duration::from_secs(60)).send_json(body);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let r = match r {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(v) if v.get("error").is_some() => Err(format!("rpc {} {}", v["error"]["code"], v["error"]["message"].as_str().unwrap_or("").chars().take(120).collect::<String>())),
                Ok(v) => Ok(v["result"].clone()),
                Err(_) => Err("bad json".into()),
            },
            Err(ureq::Error::Status(c, resp)) => {
                let txt = resp.into_string().unwrap_or_default().chars().take(120).collect::<String>();
                Err(format!("http {c} {txt}"))
            }
            Err(_) => Err("net".into()),
        };
        (r, ms)
    }
    fn ok(&self, method: &str, params: Value) -> Option<Value> {
        match self.call(method, params).0 {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("{method}: {e}");
                None
            }
        }
    }
}

/// outer + inner instructions in execution order; account keys incl. address-lookup-table keys
struct Ix {
    prog: String,
    accounts: Vec<String>,
    data: Vec<u8>,
}
fn flatten(tx: &Value) -> Vec<Ix> {
    let msg = &tx["transaction"]["message"];
    let mut keys: Vec<String> = msg["accountKeys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)).collect();
    for part in ["writable", "readonly"] {
        keys.extend(tx["meta"]["loadedAddresses"][part].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)));
    }
    let conv = |ix: &Value| -> Option<Ix> {
        Some(Ix {
            prog: keys.get(ix["programIdIndex"].as_u64()? as usize)?.clone(),
            accounts: ix["accounts"].as_array()?.iter().filter_map(|a| keys.get(a.as_u64()? as usize).cloned()).collect(),
            data: bs58::decode(ix["data"].as_str()?).into_vec().ok()?,
        })
    };
    let inner = tx["meta"]["innerInstructions"].as_array().cloned().unwrap_or_default();
    let mut v = Vec::new();
    for (i, ix) in msg["instructions"].as_array().into_iter().flatten().enumerate() {
        v.extend(conv(ix));
        for g in inner.iter().filter(|g| g["index"].as_u64() == Some(i as u64)) {
            for ix in g["instructions"].as_array().into_iter().flatten() {
                v.extend(conv(ix));
            }
        }
    }
    v
}

fn all_keys(tx: &Value) -> Vec<String> {
    let mut keys: Vec<String> = tx["transaction"]["message"]["accountKeys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)).collect();
    for part in ["writable", "readonly"] {
        keys.extend(tx["meta"]["loadedAddresses"][part].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)));
    }
    keys
}

fn tx_opts() -> Value {
    json!({"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "confirmed"})
}

fn quantiles(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    json!({"n": v.len(), "p50": q(0.5), "p90": q(0.9), "max": q(1.0)})
}

/// Block-order check: walking the block's transactions in the returned order, every token account's preTokenBalance must
/// equal the postTokenBalance it had in the previous transaction that touched it. Any break = the order is not execution order.
fn order_check(block: &Value) -> Value {
    let mut last: HashMap<String, String> = HashMap::new();
    let (mut chained, mut broken, mut ntx) = (0u64, 0u64, 0u64);
    for tx in block["transactions"].as_array().into_iter().flatten() {
        ntx += 1;
        let keys = all_keys(tx);
        let amt = |side: &str| -> Vec<(String, String)> {
            tx["meta"][side].as_array().into_iter().flatten().filter_map(|b| {
                let k = keys.get(b["accountIndex"].as_u64()? as usize)?.clone();
                Some((k, b["uiTokenAmount"]["amount"].as_str()?.to_string()))
            }).collect()
        };
        for (k, a) in amt("preTokenBalances") {
            if let Some(p) = last.get(&k) {
                if *p == a { chained += 1 } else { broken += 1 }
            }
        }
        for (k, a) in amt("postTokenBalances") {
            last.insert(k, a);
        }
    }
    json!({"txs": ntx, "transitions_checked": chained + broken, "consistent": chained, "broken": broken})
}

fn main() {
    let (Ok(http), Ok(wss)) = (std::env::var("ANKR_HTTP"), std::env::var("ANKR_WSS")) else {
        eprintln!("ANKR_HTTP / ANKR_WSS missing");
        std::process::exit(2);
    };
    let only: Option<String> = std::env::args().nth(1);
    let run = |s: &str| only.as_deref().map_or(true, |o| o == s);
    let rpc = Rpc { url: http.clone(), agent: ureq::AgentBuilder::new().max_idle_connections_per_host(64).build() };

    // ---------- 1. basic methods ----------
    let mut cur_slot = 0u64;
    if run("basic") {
        let (r, ms) = rpc.call("getSlot", json!([{"commitment": "confirmed"}]));
        cur_slot = r.as_ref().ok().and_then(|v| v.as_u64()).unwrap_or(0);
        out(json!({"test": "getSlot", "ok": r.is_ok(), "ms": ms, "err": r.err()}));
        let (r, ms) = rpc.call("getVersion", json!([]));
        out(json!({"test": "getVersion", "ok": r.is_ok(), "ms": ms, "version": r.ok()}));
        // getBlock full + block-order check on 3 recent blocks
        let mut done = 0;
        let mut s = cur_slot.saturating_sub(100);
        while done < 3 && s < cur_slot {
            let (r, ms) = rpc.call("getBlock", json!([s, {"encoding": "json", "maxSupportedTransactionVersion": 1, "transactionDetails": "full", "rewards": false, "commitment": "confirmed"}]));
            match r {
                Ok(b) if !b.is_null() => {
                    let failed = b["transactions"].as_array().map_or(0, |a| a.iter().filter(|t| !t["meta"]["err"].is_null()).count());
                    let mut vers: BTreeMap<String, u64> = BTreeMap::new();
                    let mut v1_keys = (0u64, 0u64);
                    for t in b["transactions"].as_array().into_iter().flatten() {
                        *vers.entry(t["version"].to_string()).or_insert(0) += 1;
                        if t["version"] == json!(1) {
                            v1_keys.0 += 1;
                            v1_keys.1 += t["transaction"]["message"]["accountKeys"].is_array() as u64;
                        }
                    }
                    if v1_keys.0 > 0 && done == 0 {
                        let t = b["transactions"].as_array().unwrap().iter().find(|t| t["version"] == json!(1)).unwrap();
                        let m = &t["transaction"]["message"];
                        out(json!({"test": "v1_tx_shape", "message_fields": m.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
                            "meta_fields": t["meta"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())}));
                    }
                    out(json!({"test": "getBlock_full", "slot": s, "ok": true, "ms": ms, "blockTime": b["blockTime"], "txs": b["transactions"].as_array().map(|a| a.len()),
                        "versions": vers, "v1_with_accountKeys": format!("{}/{}", v1_keys.1, v1_keys.0), "failed_tx_in_block": failed, "order": order_check(&b)}));
                    done += 1;
                }
                Ok(_) => {}
                Err(e) => out(json!({"test": "getBlock_full", "slot": s, "ok": false, "err": e})),
            }
            s += 7;
        }
    }

    // ---------- 2. signatures paging + getTransaction (success and failed) ----------
    let mut sample_sigs: Vec<Value> = vec![];
    if run("basic") {
        let (r, ms) = rpc.call("getSignaturesForAddress", json!([PUMPSWAP, {"limit": 1000, "commitment": "confirmed"}]));
        let p1 = r.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        out(json!({"test": "getSignaturesForAddress_page1", "ok": !p1.is_empty(), "n": p1.len(), "ms": ms}));
        if let Some(last) = p1.last().and_then(|x| x["signature"].as_str()) {
            let (r, ms) = rpc.call("getSignaturesForAddress", json!([PUMPSWAP, {"limit": 1000, "before": last, "commitment": "confirmed"}]));
            let p2 = r.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
            let s1: HashSet<&str> = p1.iter().filter_map(|x| x["signature"].as_str()).collect();
            let overlap = p2.iter().filter(|x| x["signature"].as_str().map_or(false, |s| s1.contains(s))).count();
            let slots_desc = p1.iter().chain(p2.iter()).filter_map(|x| x["slot"].as_u64()).collect::<Vec<_>>().windows(2).all(|w| w[0] >= w[1]);
            out(json!({"test": "getSignaturesForAddress_page2_before", "ok": !p2.is_empty(), "n": p2.len(), "ms": ms, "overlap_with_page1": overlap, "slots_non_increasing": slots_desc}));
        }
        let failed: Vec<&Value> = p1.iter().filter(|x| !x["err"].is_null()).collect();
        out(json!({"test": "failed_share_in_sig_list", "failed": failed.len(), "of": p1.len()}));
        sample_sigs = p1.clone();
        for (kind, sig) in [("success", p1.iter().find(|x| x["err"].is_null())), ("failed", failed.first().copied())] {
            let Some(sig) = sig.and_then(|x| x["signature"].as_str()) else { continue };
            let (r, ms) = rpc.call("getTransaction", json!([sig, tx_opts()]));
            let t = r.as_ref().ok().cloned().unwrap_or(Value::Null);
            out(json!({"test": format!("getTransaction_{kind}"), "ok": !t.is_null(), "ms": ms, "version": t["version"], "has_meta": t["meta"].is_object(),
                "meta_err": t["meta"]["err"], "has_inner": t["meta"]["innerInstructions"].is_array(), "has_logs": t["meta"]["logMessages"].is_array(),
                "has_loadedAddresses": t["meta"]["loadedAddresses"].is_object(), "has_pre_post_token_balances": t["meta"]["preTokenBalances"].is_array(),
                "fee": t["meta"]["fee"], "computeUnitsConsumed": t["meta"]["computeUnitsConsumed"], "blockTime": t["blockTime"], "err": r.err()}));
            // mint checks on the meme token of the successful tx
            if kind == "success" {
                let mint = t["meta"]["postTokenBalances"].as_array().into_iter().flatten().filter_map(|b| b["mint"].as_str()).find(|m| *m != WSOL).map(String::from);
                if let Some(m) = mint {
                    let (r, ms) = rpc.call("getAccountInfo", json!([m, {"encoding": "jsonParsed", "commitment": "confirmed"}]));
                    let v = r.as_ref().ok().cloned().unwrap_or(Value::Null);
                    out(json!({"test": "getAccountInfo_mint_jsonParsed", "ok": !v["value"].is_null(), "ms": ms, "owner": v["value"]["owner"],
                        "decimals": v["value"]["data"]["parsed"]["info"]["decimals"], "has_mintAuthority_field": v["value"]["data"]["parsed"]["info"].get("mintAuthority").is_some(), "err": r.err()}));
                    let (r, ms) = rpc.call("getTokenLargestAccounts", json!([m, {"commitment": "confirmed"}]));
                    out(json!({"test": "getTokenLargestAccounts", "ok": r.is_ok(), "ms": ms, "n": r.as_ref().ok().and_then(|v| v["value"].as_array().map(|a| a.len())), "err": r.err()}));
                }
            }
        }
    }

    // ---------- 3. history depth ----------
    if run("depth") {
        if cur_slot == 0 {
            cur_slot = rpc.ok("getSlot", json!([{"commitment": "confirmed"}])).and_then(|v| v.as_u64()).unwrap_or(0);
        }
        let t_now = now();
        for days in [1.0f64, 7.0, 30.0, 90.0] {
            let target = t_now - days * 86400.0;
            let mut slot = cur_slot.saturating_sub((days * 86400.0 / 0.4) as u64);
            let mut bt: Option<i64> = None;
            // correct the slot estimate with getBlockTime (skipped slots -> try the next few)
            for _ in 0..3 {
                let mut got = None;
                for k in 0..5 {
                    if let Some(b) = rpc.ok("getBlockTime", json!([slot + k])).and_then(|v| v.as_i64()) {
                        got = Some((slot + k, b));
                        break;
                    }
                }
                let Some((s, b)) = got else { break };
                slot = s;
                bt = Some(b);
                let diff = target - b as f64;
                if diff.abs() < 3600.0 { break }
                slot = (slot as f64 + diff / 0.4) as u64;
            }
            // first signature of a block there, then getTransaction and an address-index query anchored on it
            let mut res = json!({"test": "history_depth", "days": days, "slot": slot, "blockTime": bt});
            let mut sig = None;
            for k in 0..5 {
                let (r, _) = rpc.call("getBlock", json!([slot + k, {"encoding": "json", "maxSupportedTransactionVersion": 1, "transactionDetails": "signatures", "rewards": false}]));
                match r {
                    Ok(b) if !b.is_null() => {
                        sig = b["signatures"].as_array().and_then(|a| a.get(a.len() / 2)).and_then(|s| s.as_str()).map(String::from);
                        res["getBlock"] = json!(true);
                        break;
                    }
                    Err(e) => res["getBlock_err"] = json!(e),
                    _ => {}
                }
            }
            if let Some(s) = sig {
                let (r, ms) = rpc.call("getTransaction", json!([s, tx_opts()]));
                res["getTransaction"] = json!(r.as_ref().map_or(false, |v| !v.is_null()));
                res["getTransaction_ms"] = json!(ms);
                if let Err(e) = r { res["getTransaction_err"] = json!(e) }
                let (r, _) = rpc.call("getSignaturesForAddress", json!([PUMPSWAP, {"limit": 5, "before": s}]));
                res["getSignaturesForAddress_before"] = json!(r.as_ref().ok().and_then(|v| v.as_array().map(|a| a.len())));
                if let Err(e) = r { res["sigs_err"] = json!(e) }
            }
            out(res);
        }
    }

    // ---------- 4. websocket ----------
    if run("wss") {
        for (name, sub) in [
            ("logsSubscribe", json!({"jsonrpc": "2.0", "id": 1, "method": "logsSubscribe", "params": [{"mentions": [PUMPSWAP]}, {"commitment": "confirmed"}]})),
            ("programSubscribe", json!({"jsonrpc": "2.0", "id": 1, "method": "programSubscribe", "params": [PUMPSWAP, {"encoding": "base64", "commitment": "confirmed"}]})),
        ] {
            let mut res = json!({"test": format!("wss_{name}")});
            match tungstenite::connect(wss.as_str()) {
                Err(e) => res["err"] = json!(e.to_string().replace(&wss, "<wss>").chars().take(150).collect::<String>()),
                Ok((mut ws, _)) => {
                    if let tungstenite::stream::MaybeTlsStream::NativeTls(s) = ws.get_mut() {
                        let _ = s.get_mut().set_read_timeout(Some(Duration::from_secs(5)));
                    }
                    let _ = ws.send(tungstenite::Message::text(sub.to_string()));
                    let t0 = Instant::now();
                    let (mut n, mut errs, mut first_ms, mut confirm) = (0u64, 0u64, None::<f64>, Value::Null);
                    let mut slots = vec![];
                    while t0.elapsed() < Duration::from_secs(15) {
                        match ws.read() {
                            Ok(tungstenite::Message::Text(t)) => {
                                let Ok(m) = serde_json::from_str::<Value>(&t) else { continue };
                                if m.get("id").is_some() {
                                    confirm = if m.get("error").is_some() { m["error"].clone() } else { json!("subscribed") };
                                    continue;
                                }
                                if m["params"]["result"]["value"]["err"].is_null() == false { errs += 1 }
                                n += 1;
                                first_ms.get_or_insert(t0.elapsed().as_secs_f64() * 1000.0);
                                if let Some(s) = m["params"]["result"]["context"]["slot"].as_u64() { slots.push(s) }
                            }
                            Ok(_) => {}
                            Err(tungstenite::Error::Io(_)) => {}
                            Err(_) => break,
                        }
                    }
                    let tip = rpc.ok("getSlot", json!([{"commitment": "confirmed"}])).and_then(|v| v.as_u64()).unwrap_or(0);
                    res["subscribe_reply"] = confirm;
                    res["notifications_15s"] = json!(n);
                    res["notifications_with_err"] = json!(errs);
                    res["first_ms"] = json!(first_ms);
                    res["last_notif_slot_behind_tip"] = json!(slots.last().map(|s| tip as i64 - *s as i64));
                    let _ = ws.close(None);
                }
            }
            out(res);
        }
    }

    // ---------- 5. venue throughput + pool sample + per-pool 6 h tx counts ----------
    if run("venues") {
        let t_now = now();
        for (venue, prog, swaps, creates, pool_ix) in venues() {
            // all signatures of the program in the last 300 s
            let (mut n, mut nfail, mut before, mut pages, mut oldest) = (0u64, 0u64, None::<String>, 0, t_now);
            let mut recent: Vec<String> = vec![];
            'p: while pages < 40 {
                let mut o = json!({"limit": 1000, "commitment": "confirmed"});
                if let Some(b) = &before { o["before"] = json!(b) }
                let Some(v) = rpc.ok("getSignaturesForAddress", json!([prog, o])) else { break };
                pages += 1;
                let a = v.as_array().cloned().unwrap_or_default();
                if a.is_empty() { break }
                for x in &a {
                    let bt = x["blockTime"].as_f64().unwrap_or(0.0);
                    if bt < t_now - 300.0 { break 'p }
                    oldest = bt;
                    n += 1;
                    if !x["err"].is_null() { nfail += 1 } else if recent.len() < 40 && n % 5 == 0 {
                        recent.push(x["signature"].as_str().unwrap_or("").into());
                    }
                }
                before = a.last().and_then(|x| x["signature"].as_str()).map(String::from);
            }
            let span = (t_now - oldest).max(1.0);
            out(json!({"test": "venue_rate", "venue": venue, "tx_in_window": n, "failed": nfail, "window_s": span, "pages": pages,
                "tx_per_s": n as f64 / span, "est_tx_per_day": (n as f64 / span * 86400.0).round()}));
            // decode a sample: pools touched and pool-creation instructions
            let mut pools: Vec<String> = vec![];
            let (mut n_swap_tx, mut n_create) = (0, 0);
            for s in recent.iter().take(25) {
                let Some(tx) = rpc.ok("getTransaction", json!([s, tx_opts()])) else { continue };
                let mut sw = false;
                for ix in flatten(&tx).iter().filter(|ix| ix.prog == prog && ix.data.len() >= 8) {
                    let d: [u8; 8] = ix.data[..8].try_into().unwrap();
                    if swaps.contains(&d) {
                        sw = true;
                        if let Some(p) = ix.accounts.get(pool_ix) { if !pools.contains(p) { pools.push(p.clone()) } }
                    }
                    if creates.contains(&d) { n_create += 1 }
                }
                n_swap_tx += sw as usize;
            }
            out(json!({"test": "venue_sample", "venue": venue, "tx_decoded": recent.len().min(25), "with_swap_ix": n_swap_tx, "create_ix": n_create, "distinct_pools": pools.len()}));
            // per-pool tx count over the last 6 h (cap 20 pages = 20,000 sigs), 10 pools
            let mut per_pool = vec![];
            let mut capped = 0;
            for p in pools.iter().take(10) {
                let (mut c, mut before, mut pg, mut cap) = (0u64, None::<String>, 0, true);
                'q: while pg < 20 {
                    let mut o = json!({"limit": 1000, "commitment": "confirmed"});
                    if let Some(b) = &before { o["before"] = json!(b) }
                    let Some(v) = rpc.ok("getSignaturesForAddress", json!([p, o])) else { break };
                    pg += 1;
                    let a = v.as_array().cloned().unwrap_or_default();
                    for x in &a {
                        if x["blockTime"].as_f64().unwrap_or(0.0) < t_now - 21600.0 { cap = false; break 'q }
                        c += 1;
                    }
                    if a.len() < 1000 { cap = false; break }
                    before = a.last().and_then(|x| x["signature"].as_str()).map(String::from);
                }
                capped += cap as usize;
                per_pool.push(c as f64);
            }
            out(json!({"test": "pool_tx_6h", "venue": venue, "pools": per_pool.len(), "capped_at_20k": capped, "tx_6h": quantiles(per_pool.clone()), "values": per_pool}));
        }
    }

    // ---------- 6. rate limit ----------
    if run("rate") {
        let rpc = Arc::new(rpc);
        for target in [10u64, 25, 50, 100, 200, 400] {
            let (ok, e429, eother) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
            let lat = Arc::new(Mutex::new(vec![]));
            let workers = target.min(64);
            let interval = Duration::from_secs_f64(workers as f64 / target as f64);
            let t0 = Instant::now();
            let hs: Vec<_> = (0..workers).map(|w| {
                let (rpc, ok, e429, eother, lat) = (rpc.clone(), ok.clone(), e429.clone(), eother.clone(), lat.clone());
                std::thread::spawn(move || {
                    let start = Instant::now() + interval.mul_f64(w as f64 / workers as f64);
                    let mut k = 0u32;
                    loop {
                        let due = start + interval * k;
                        if due.duration_since(t0) >= Duration::from_secs(4) { break }
                        if let Some(d) = due.checked_duration_since(Instant::now()) { std::thread::sleep(d) }
                        let (r, ms) = rpc.call("getSlot", json!([]));
                        match r {
                            Ok(_) => { ok.fetch_add(1, Ordering::Relaxed); lat.lock().unwrap().push(ms); }
                            Err(e) if e.starts_with("http 429") || e.contains("rate") || e.contains("limit") => { e429.fetch_add(1, Ordering::Relaxed); }
                            Err(_) => { eother.fetch_add(1, Ordering::Relaxed); }
                        }
                        k += 1;
                    }
                })
            }).collect();
            for h in hs { let _ = h.join(); }
            let el = t0.elapsed().as_secs_f64();
            let (o, r4, eo) = (ok.load(Ordering::Relaxed), e429.load(Ordering::Relaxed), eother.load(Ordering::Relaxed));
            out(json!({"test": "rate_getSlot", "target_rps": target, "achieved_ok_rps": o as f64 / el, "ok": o, "rate_limited": r4, "other_err": eo, "latency_ms": quantiles(lat.lock().unwrap().clone())}));
            if r4 as f64 > 0.05 * (o + r4) as f64 { break }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    out(json!({"test": "call_count", "total_calls": CALLS.load(Ordering::Relaxed), "per_method": *PER_METHOD.lock().unwrap()}));
}
