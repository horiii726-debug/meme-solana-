//! sol2 ingestion library (C2): RPC client with retry, venue table from the IDLs, raw zstd writer, signature listing.
//! Read-only. Never sends transactions. The endpoint URL holds the API key and is never printed or written.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// UTC date "YYYY-MM-DD" of a unix time
pub fn date(t: f64) -> String {
    let days = (t / 86400.0).floor() as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------- venues (all constants from idl/*.json) ----------------
pub struct Venue {
    pub name: &'static str,
    pub program: &'static str,
    /// (instruction name, discriminator, index of the pool account)
    pub creates: Vec<(&'static str, [u8; 8], usize)>,
    /// Anchor instruction-name log lines that mark a candidate pool creation (confirmed afterwards by discriminator)
    pub create_logs: Vec<&'static str>,
}

pub fn venues() -> Vec<Venue> {
    vec![
        // idl/pump_amm.json
        Venue { name: "pumpswap", program: "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
            creates: vec![("create_pool", [233, 146, 209, 142, 207, 104, 64, 188], 0)],
            create_logs: vec!["Program log: Instruction: CreatePool"] },
        // idl/raydium_cp_swap.json
        Venue { name: "raydium_cpmm", program: "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
            creates: vec![("initialize", [175, 175, 109, 31, 13, 152, 155, 237], 3)],
            create_logs: vec!["Program log: Instruction: Initialize"] },
        // idl/meteora_damm_v2.json
        Venue { name: "meteora_damm_v2", program: "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
            creates: vec![
                ("initialize_customizable_pool", [20, 161, 241, 24, 189, 221, 180, 2], 5),
                ("initialize_pool", [95, 180, 10, 172, 84, 174, 232, 40], 6),
                ("initialize_pool_with_dynamic_config", [149, 82, 72, 197, 253, 252, 68, 15], 7),
            ],
            create_logs: vec!["Program log: Instruction: InitializeCustomizablePool", "Program log: Instruction: InitializePool",
                "Program log: Instruction: InitializePoolWithDynamicConfig"] },
    ]
}

pub fn venue(name: &str) -> Option<Venue> {
    venues().into_iter().find(|v| v.name == name)
}

// ---------------- RPC ----------------
pub static CALLS: AtomicU64 = AtomicU64::new(0);
pub static PER_METHOD: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// One endpoint: URL (never printed), minimum spacing between calls, and how far back its history reaches.
struct Endpoint {
    url: String,
    pace: Duration,
    max_age_h: f64,
    next: Mutex<Instant>,
    /// per-endpoint hard cap on calls (4th field of RPC_URLS); u64::MAX = none
    cap: u64,
    used: AtomicU64,
}

pub struct Rpc {
    eps: Vec<Endpoint>,
    rr: AtomicU64,
    agent: ureq::Agent,
}

pub fn tx_opts() -> Value {
    json!({"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "confirmed"})
}

impl Rpc {
    /// RPC_URLS = "url|pace_ms|max_age_hours,url|pace_ms|max_age_hours,..." (free endpoints).
    /// Paid endpoints (ANKR_HTTP) are used ONLY when ALLOW_PAID=1 — owner rule after the 9 Nov 2026 overspend.
    pub fn from_env() -> Rpc {
        let mut eps = vec![];
        if let Ok(list) = std::env::var("RPC_URLS") {
            for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let p: Vec<&str> = item.split('|').collect();
                eps.push(Endpoint { url: p[0].to_string(), pace: Duration::from_millis(p.get(1).and_then(|x| x.parse().ok()).unwrap_or(500)),
                    max_age_h: p.get(2).and_then(|x| x.parse().ok()).unwrap_or(f64::INFINITY), next: Mutex::new(Instant::now()),
                    cap: p.get(3).and_then(|x| x.parse().ok()).unwrap_or(u64::MAX), used: AtomicU64::new(0) });
            }
        }
        if std::env::var("ALLOW_PAID").map(|v| v == "1").unwrap_or(false) {
            if let Ok(u) = std::env::var("ANKR_HTTP") {
                eps.push(Endpoint { url: u, pace: Duration::from_millis(0), max_age_h: 13.0, next: Mutex::new(Instant::now()), cap: u64::MAX, used: AtomicU64::new(0) });
            }
        }
        // metered endpoints (credits): only with ALLOW_PAID=1 AND a hard MAX_CALLS cap
        let metered = eps.iter().any(|e| e.url.contains("ankr") || e.url.contains("helius"));
        if metered && std::env::var("ALLOW_PAID").map(|v| v != "1").unwrap_or(true) {
            eprintln!("refusing a metered (ankr/helius) endpoint without ALLOW_PAID=1");
            std::process::exit(2);
        }
        if metered && std::env::var("MAX_CALLS").ok().and_then(|v| v.parse::<u64>().ok()).is_none() {
            eprintln!("metered endpoint requires MAX_CALLS=<hard cap>");
            std::process::exit(2);
        }
        if eps.is_empty() {
            eprintln!("no RPC endpoint: set RPC_URLS (free) or ALLOW_PAID=1 with ANKR_HTTP");
            std::process::exit(2);
        }
        Rpc { eps, rr: AtomicU64::new(0), agent: ureq::AgentBuilder::new().max_idle_connections_per_host(64).timeout(Duration::from_secs(60)).build() }
    }
    /// pick an endpoint that can serve data `age_h` hours old (round robin), waiting for its pacing slot
    fn pick(&self, age_h: f64) -> &Endpoint {
        let ok: Vec<&Endpoint> = self.eps.iter().filter(|e| e.max_age_h >= age_h && e.used.load(Ordering::Relaxed) < e.cap).collect();
        let ok = if ok.is_empty() { self.eps.iter().collect::<Vec<_>>() } else { ok };
        let e = ok[(self.rr.fetch_add(1, Ordering::Relaxed) as usize) % ok.len()];
        let wait = {
            let mut n = e.next.lock().unwrap();
            let now = Instant::now();
            let start = (*n).max(now);
            *n = start + e.pace;
            start - now
        };
        if !wait.is_zero() { std::thread::sleep(wait) }
        e
    }
    /// one call; Err text never contains the URL
    pub fn once(&self, method: &str, params: &Value) -> Result<Value, String> {
        self.once_age(method, params, f64::INFINITY)
    }
    pub fn once_age(&self, method: &str, params: &Value, age_h: f64) -> Result<Value, String> {
        // hard cap on total calls (process-wide); reaching it stops all further calls
        if let Some(cap) = std::env::var("MAX_CALLS").ok().and_then(|v| v.parse::<u64>().ok()) {
            if CALLS.load(Ordering::Relaxed) >= cap {
                return Err("rpc cap MAX_CALLS reached".into());
            }
        }
        CALLS.fetch_add(1, Ordering::Relaxed);
        *PER_METHOD.lock().unwrap().entry(method.to_string()).or_insert(0) += 1;
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let ep = self.pick(age_h);
        ep.used.fetch_add(1, Ordering::Relaxed);
        match self.agent.post(&ep.url).send_json(body) {
            Ok(r) => match r.into_json::<Value>() {
                Ok(v) if v.get("error").is_some() => Err(format!("rpc {} {}", v["error"]["code"], v["error"]["message"].as_str().unwrap_or("").chars().take(100).collect::<String>())),
                Ok(v) => Ok(v["result"].clone()),
                Err(_) => Err("bad json".into()),
            },
            Err(ureq::Error::Status(c, _)) => Err(format!("http {c}")),
            Err(_) => Err("net".into()),
        }
    }
    /// retry on network / 429 / 5xx with backoff 0.5,1,2,4 s; RPC-level errors are returned at once
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_age(method, params, f64::INFINITY)
    }
    /// like `call`, but only endpoints whose history reaches `age_h` hours back are used
    pub fn call_age(&self, method: &str, params: Value, age_h: f64) -> Result<Value, String> {
        let mut last = String::new();
        for k in 0..6u32 {
            match self.once_age(method, &params, age_h) {
                Ok(v) => return Ok(v),
                Err(e) if e.starts_with("rpc") && !e.contains("429") && !e.contains("-32429") && !e.to_lowercase().contains("rate") => return Err(e),
                Err(e) => last = e,
            }
            std::thread::sleep(Duration::from_millis(500 << k));
        }
        Err(last)
    }
    /// getTransaction; a confirmed tx can be briefly unavailable right after the notification: retry `tries` x 400 ms on null
    pub fn get_tx(&self, sig: &str, tries: u32) -> Result<Value, String> {
        self.get_tx_age(sig, tries, f64::INFINITY)
    }
    pub fn get_tx_age(&self, sig: &str, tries: u32, age_h: f64) -> Result<Value, String> {
        for _ in 0..tries.max(1) {
            let v = self.call_age("getTransaction", json!([sig, tx_opts()]), age_h)?;
            if !v.is_null() {
                return Ok(v);
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        Err("null (not found)".into())
    }
    /// all signatures of `addr` (incl. failed) newest-first, back until blockTime < t_min, or `until` sig reached,
    /// or the history ends. Returns (sigs oldest-first, complete flag: false if stopped by max_pages or an RPC failure).
    pub fn list_sigs(&self, addr: &str, t_min: f64, until: Option<&str>, max_pages: usize) -> (Vec<Value>, bool) {
        let mut out = vec![];
        let mut before: Option<String> = None;
        for _ in 0..max_pages {
            let mut o = json!({"limit": 1000, "commitment": "confirmed"});
            if let Some(b) = &before { o["before"] = json!(b) }
            if let Some(u) = until { o["until"] = json!(u) }
            let Ok(v) = self.call("getSignaturesForAddress", json!([addr, o])) else { return (rev(out), false) };
            let a = v.as_array().cloned().unwrap_or_default();
            for x in &a {
                if x["blockTime"].as_f64().map_or(false, |t| t < t_min) {
                    return (rev(out), true);
                }
                out.push(x.clone());
            }
            if a.len() < 1000 {
                return (rev(out), true);
            }
            before = a.last().and_then(|x| x["signature"].as_str()).map(String::from);
        }
        (rev(out), false)
    }
}

fn rev(mut v: Vec<Value>) -> Vec<Value> {
    v.reverse();
    v
}

pub fn calls_summary() -> Value {
    json!({"total_calls": CALLS.load(Ordering::Relaxed), "per_method": *PER_METHOD.lock().unwrap()})
}

// ---------------- instructions ----------------
pub struct Ix {
    pub prog: String,
    pub accounts: Vec<String>,
    pub data: Vec<u8>,
    /// outer instruction index, inner index (None = outer)
    pub outer: usize,
    pub inner: Option<usize>,
}

pub fn all_keys(tx: &Value) -> Vec<String> {
    let mut keys: Vec<String> = tx["transaction"]["message"]["accountKeys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)).collect();
    for part in ["writable", "readonly"] {
        keys.extend(tx["meta"]["loadedAddresses"][part].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)));
    }
    keys
}

/// outer + inner instructions in execution order with their indices
pub fn flatten(tx: &Value) -> Vec<Ix> {
    let keys = all_keys(tx);
    let conv = |ix: &Value, outer: usize, inner: Option<usize>| -> Option<Ix> {
        Some(Ix {
            prog: keys.get(ix["programIdIndex"].as_u64()? as usize)?.clone(),
            accounts: ix["accounts"].as_array()?.iter().filter_map(|a| keys.get(a.as_u64()? as usize).cloned()).collect(),
            data: bs58::decode(ix["data"].as_str()?).into_vec().ok()?,
            outer,
            inner,
        })
    };
    let inner = tx["meta"]["innerInstructions"].as_array().cloned().unwrap_or_default();
    let mut v = Vec::new();
    for (i, ix) in tx["transaction"]["message"]["instructions"].as_array().into_iter().flatten().enumerate() {
        v.extend(conv(ix, i, None));
        for g in inner.iter().filter(|g| g["index"].as_u64() == Some(i as u64)) {
            for (j, ix) in g["instructions"].as_array().into_iter().flatten().enumerate() {
                v.extend(conv(ix, i, Some(j)));
            }
        }
    }
    v
}

/// pool creations in a tx for this venue, confirmed by discriminator: (instruction name, pool address)
pub fn pool_creations(tx: &Value, v: &Venue) -> Vec<(&'static str, String)> {
    if !tx["meta"]["err"].is_null() {
        return vec![];
    }
    let mut out = vec![];
    for ix in flatten(tx).iter().filter(|ix| ix.prog == v.program && ix.data.len() >= 8) {
        let d: [u8; 8] = ix.data[..8].try_into().unwrap();
        for (name, disc, pi) in &v.creates {
            if d == *disc {
                if let Some(p) = ix.accounts.get(*pi) {
                    out.push((*name, p.clone()));
                }
            }
        }
    }
    out
}

// ---------------- raw writer ----------------
/// Appends records to raw/<venue>/<UTC date of recv_time>.jsonl.zst as independent zstd frames (one frame per flush),
/// so a crash loses at most the unflushed buffer and the file stays decodable with `zstdcat`.
pub struct RawWriter {
    dir: String,
    buf: BTreeMap<String, Vec<u8>>,
    last_flush: Instant,
}

impl RawWriter {
    pub fn new(dir: &str) -> RawWriter {
        RawWriter { dir: dir.to_string(), buf: BTreeMap::new(), last_flush: Instant::now() }
    }
    pub fn write(&mut self, venue: &str, rec: &Value) {
        let t = rec["recv_time"].as_f64().unwrap_or_else(now);
        let path = format!("{}/{}/{}.jsonl.zst", self.dir, venue, date(t));
        let b = self.buf.entry(path).or_default();
        b.extend_from_slice(rec.to_string().as_bytes());
        b.push(b'\n');
        if self.last_flush.elapsed() > Duration::from_secs(2) || b.len() > 8 << 20 {
            self.flush();
        }
    }
    pub fn flush(&mut self) {
        for (path, b) in std::mem::take(&mut self.buf) {
            if b.is_empty() { continue }
            if let Some(d) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(d);
            }
            match (zstd::encode_all(&b[..], 6), std::fs::OpenOptions::new().create(true).append(true).open(&path)) {
                (Ok(z), Ok(mut f)) => {
                    if let Err(e) = f.write_all(&z) { eprintln!("write {path}: {e}") }
                }
                (a, b) => eprintln!("raw flush failed {path}: {:?} {:?}", a.err(), b.err()),
            }
        }
        self.last_flush = Instant::now();
    }
}

/// read every record of the given raw files
pub fn read_raw(paths: &[String]) -> Vec<Value> {
    let mut out = vec![];
    for p in paths {
        let Ok(f) = std::fs::File::open(p) else { eprintln!("cannot open {p}"); continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                out.push(v);
            }
        }
    }
    out
}

/// one raw transaction record
pub fn tx_record(venue: &str, pool: &str, source: &str, sig: &str, recv_time: f64, tx: &Value) -> Value {
    json!({"kind": "tx", "venue": venue, "pool": pool, "source": source, "sig": sig, "slot": tx["slot"],
        "block_time": tx["blockTime"], "recv_time": recv_time, "failed": !tx["meta"]["err"].is_null(), "tx": tx})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn date_conversion() {
        assert_eq!(date(0.0), "1970-01-01");
        assert_eq!(date(1794248947.0), "2026-11-09"); // date -u -d @1794248947 = Mon Nov 9 18:29:07 UTC 2026
        assert_eq!(date(951782400.0), "2000-02-29");
    }
    #[test]
    fn raw_roundtrip_multi_frame() {
        let dir = format!("/tmp/sol2_test_{}", std::process::id());
        let mut w = RawWriter::new(&dir);
        w.write("v", &json!({"recv_time": 1794248947.0, "a": 1}));
        w.flush();
        w.write("v", &json!({"recv_time": 1794248948.0, "a": 2}));
        w.flush();
        let r = read_raw(&[format!("{dir}/v/2026-11-09.jsonl.zst")]);
        assert_eq!(r.iter().map(|x| x["a"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn flatten_keeps_order_and_indices() {
        let tx = json!({"transaction": {"message": {"accountKeys": ["P", "A", "Q"], "instructions": [
            {"programIdIndex": 0, "accounts": [1], "data": bs58::encode([1u8, 2]).into_string()},
            {"programIdIndex": 2, "accounts": [], "data": ""}]}},
            "meta": {"loadedAddresses": {"writable": ["W"], "readonly": []}, "innerInstructions": [
                {"index": 0, "instructions": [{"programIdIndex": 3, "accounts": [0], "data": ""}]}]}});
        let f = flatten(&tx);
        let v: Vec<(String, usize, Option<usize>)> = f.iter().map(|i| (i.prog.clone(), i.outer, i.inner)).collect();
        assert_eq!(v, vec![("P".into(), 0, None), ("W".into(), 0, Some(0)), ("Q".into(), 1, None)]);
    }
}
