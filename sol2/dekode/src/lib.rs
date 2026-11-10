//! C3 decoders. Layouts are NOT hand-written: events and instruction accounts are decoded from the official Anchor IDLs
//! in /root/sol/idl (borsh, little endian). Integers are kept exact (u64/u128/i128 as decimal strings).
//! Decimals come only from the chain (token balance entries of the transaction, which carry the mint decimals);
//! when absent the value is "UNKNOWN" — there is no 6/9 fallback.
pub mod series;
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const EVENT_CPI_TAG: [u8; 8] = [228, 69, 165, 46, 81, 203, 154, 29];

pub struct Idl {
    pub program: String,
    types: HashMap<String, Value>,
    /// event discriminator -> event name
    events: HashMap<[u8; 8], String>,
    /// instruction discriminator -> (name, account names)
    ixs: HashMap<[u8; 8], (String, Vec<String>)>,
}

fn disc(v: &Value) -> Option<[u8; 8]> {
    let a: Vec<u8> = v.as_array()?.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect();
    a.try_into().ok()
}

impl Idl {
    pub fn load(path: &str) -> Idl {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("idl {path}: {e}"))).expect("idl json");
        let types = v["types"].as_array().into_iter().flatten().filter_map(|t| Some((t["name"].as_str()?.to_string(), t["type"].clone()))).collect();
        let events = v["events"].as_array().into_iter().flatten().filter_map(|e| Some((disc(&e["discriminator"])?, e["name"].as_str()?.to_string()))).collect();
        let ixs = v["instructions"].as_array().into_iter().flatten().filter_map(|i| {
            let names = i["accounts"].as_array()?.iter().filter_map(|a| a["name"].as_str().map(String::from)).collect();
            Some((disc(&i["discriminator"])?, (i["name"].as_str()?.to_string(), names)))
        }).collect();
        Idl { program: v["address"].as_str().unwrap_or("").to_string(), types, events, ixs }
    }
    pub fn ix(&self, d: &[u8]) -> Option<&(String, Vec<String>)> {
        self.ixs.get(<&[u8; 8]>::try_from(d.get(..8)?).ok()?)
    }
    /// decode an event payload (8-byte discriminator + borsh). Returns (name, fields, fully_consumed)
    pub fn event(&self, b: &[u8]) -> Option<(String, Value, usize)> {
        let name = self.events.get(<&[u8; 8]>::try_from(b.get(..8)?).ok()?)?.clone();
        let ty = self.types.get(&name)?;
        let mut r = Reader { b, o: 8 };
        let v = self.decode_def(ty, &mut r)?;
        Some((name, v, b.len() - r.o))
    }
    fn decode_def(&self, ty: &Value, r: &mut Reader) -> Option<Value> {
        match ty["kind"].as_str()? {
            "struct" => {
                let mut m = Map::new();
                for (i, f) in ty["fields"].as_array().into_iter().flatten().enumerate() {
                    // named fields, or tuple-struct fields (plain types)
                    let (name, t) = if f.get("type").is_some() { (f["name"].as_str().unwrap_or("").to_string(), &f["type"]) } else { (i.to_string(), f) };
                    m.insert(name, self.decode(t, r)?);
                }
                Some(Value::Object(m))
            }
            "enum" => {
                let tag = r.u8()? as usize;
                let var = ty["variants"].as_array()?.get(tag)?;
                let name = var["name"].as_str()?.to_string();
                match var.get("fields") {
                    None => Some(json!(name)),
                    Some(_) => {
                        let inner = self.decode_def(&json!({"kind": "struct", "fields": var["fields"]}), r)?;
                        Some(json!({name: inner}))
                    }
                }
            }
            _ => None,
        }
    }
    fn decode(&self, t: &Value, r: &mut Reader) -> Option<Value> {
        if let Some(s) = t.as_str() {
            return Some(match s {
                "u8" => json!(r.u8()?),
                "i8" => json!(r.u8()? as i8),
                "u16" => json!(u16::from_le_bytes(r.take(2)?.try_into().ok()?)),
                "i16" => json!(i16::from_le_bytes(r.take(2)?.try_into().ok()?)),
                "u32" => json!(u32::from_le_bytes(r.take(4)?.try_into().ok()?)),
                "i32" => json!(i32::from_le_bytes(r.take(4)?.try_into().ok()?)),
                "u64" => json!(u64::from_le_bytes(r.take(8)?.try_into().ok()?).to_string()),
                "i64" => json!(i64::from_le_bytes(r.take(8)?.try_into().ok()?).to_string()),
                "u128" => json!(u128::from_le_bytes(r.take(16)?.try_into().ok()?).to_string()),
                "i128" => json!(i128::from_le_bytes(r.take(16)?.try_into().ok()?).to_string()),
                "bool" => json!(r.u8()? != 0),
                "pubkey" | "publicKey" => json!(bs58::encode(r.take(32)?).into_string()),
                "string" => {
                    let n = u32::from_le_bytes(r.take(4)?.try_into().ok()?) as usize;
                    json!(String::from_utf8_lossy(r.take(n)?).to_string())
                }
                "bytes" => {
                    let n = u32::from_le_bytes(r.take(4)?.try_into().ok()?) as usize;
                    json!(base64::engine::general_purpose::STANDARD.encode(r.take(n)?))
                }
                _ => return None,
            });
        }
        if let Some(d) = t.get("defined") {
            let name = d["name"].as_str().or(d.as_str())?;
            return self.decode_def(self.types.get(name)?, r);
        }
        if let Some(inner) = t.get("vec") {
            let n = u32::from_le_bytes(r.take(4)?.try_into().ok()?) as usize;
            return (0..n).map(|_| self.decode(inner, r)).collect::<Option<Vec<_>>>().map(Value::Array);
        }
        if let Some(inner) = t.get("option") {
            return if r.u8()? == 0 { Some(Value::Null) } else { self.decode(inner, r) };
        }
        if let Some(a) = t.get("array") {
            let n = a[1].as_u64()? as usize;
            return (0..n).map(|_| self.decode(&a[0], r)).collect::<Option<Vec<_>>>().map(Value::Array);
        }
        None
    }
}

struct Reader<'a> {
    b: &'a [u8],
    o: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let x = self.b.get(self.o..self.o + n)?;
        self.o += n;
        Some(x)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
}

pub struct Ix {
    pub prog: String,
    pub accounts: Vec<String>,
    pub data: Vec<u8>,
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

pub fn flatten(tx: &Value) -> (Vec<Ix>, usize) {
    let keys = all_keys(tx);
    let mut skipped = 0;
    let mut conv = |ix: &Value, outer: usize, inner: Option<usize>| -> Option<Ix> {
        let r = (|| Some(Ix {
            prog: keys.get(ix["programIdIndex"].as_u64()? as usize)?.clone(),
            accounts: ix["accounts"].as_array()?.iter().map(|a| keys.get(a.as_u64()? as usize).cloned()).collect::<Option<Vec<_>>>()?,
            data: bs58::decode(ix["data"].as_str()?).into_vec().ok()?,
            outer,
            inner,
        }))();
        if r.is_none() { skipped += 1 }
        r
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
    (v, skipped)
}

/// token balances of the tx: account -> (mint, owner, decimals, pre raw, post raw)
pub fn token_balances(tx: &Value) -> HashMap<String, (String, String, Option<u64>, Option<u128>, Option<u128>)> {
    let keys = all_keys(tx);
    let mut m: HashMap<String, (String, String, Option<u64>, Option<u128>, Option<u128>)> = HashMap::new();
    for (side, post) in [("preTokenBalances", false), ("postTokenBalances", true)] {
        for b in tx["meta"][side].as_array().into_iter().flatten() {
            let Some(k) = b["accountIndex"].as_u64().and_then(|i| keys.get(i as usize)) else { continue };
            let e = m.entry(k.clone()).or_insert((b["mint"].as_str().unwrap_or("").into(), b["owner"].as_str().unwrap_or("").into(), None, None, None));
            e.2 = b["uiTokenAmount"]["decimals"].as_u64().or(e.2);
            let amt = b["uiTokenAmount"]["amount"].as_str().and_then(|s| s.parse::<u128>().ok());
            if post { e.4 = amt } else { e.3 = amt }
        }
    }
    m
}

/// One decoded event with its position and the instruction it belongs to.
/// Pairing: an event emitted by program P (emit_cpi inner ix, or a "Program data:" log) belongs to the nearest
/// preceding non-event instruction of P in execution order (same outer instruction).
pub fn events(tx: &Value, idl: &Idl) -> Vec<Value> {
    let (flat, _) = flatten(tx);
    let mut out = vec![];
    let mut owner: Option<usize> = None;
    for (i, ix) in flat.iter().enumerate() {
        if ix.prog != idl.program || ix.data.len() < 8 {
            continue;
        }
        if ix.data[..8] == EVENT_CPI_TAG {
            let Some((name, fields, full)) = idl.event(&ix.data[8..]) else {
                out.push(json!({"kind": "undecoded_event", "outer": ix.outer, "inner": ix.inner, "disc": ix.data.get(8..16)}));
                continue;
            };
            let (ix_name, accounts) = match owner.filter(|o| flat[*o].outer == ix.outer) {
                Some(o) => match idl.ix(&flat[o].data) {
                    Some((n, names)) => (json!(n), json!(names.iter().zip(flat[o].accounts.iter()).map(|(n, a)| (n.clone(), json!(a))).collect::<Map<String, Value>>())),
                    None => (Value::Null, Value::Null),
                },
                None => (Value::Null, Value::Null),
            };
            out.push(json!({"kind": "event", "event": name, "fields": fields, "fully_decoded": full == 0, "extra_bytes": full, "outer": ix.outer, "inner": ix.inner,
                "ix": ix_name, "ix_accounts": accounts}));
        } else {
            owner = Some(i);
        }
    }
    out
}

/// decimals of a mint as seen in this tx's token balances; "UNKNOWN" if the tx has no balance entry for it
pub fn decimals_of(tb: &HashMap<String, (String, String, Option<u64>, Option<u128>, Option<u128>)>, mint: &str) -> Value {
    tb.values().find(|v| v.0 == mint).and_then(|v| v.2).map_or(json!("UNKNOWN"), |d| json!(d))
}

pub fn n(v: &Value) -> Option<u128> {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64().map(|x| x as u128))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn idl() -> Idl {
        // synthetic IDL (test only)
        let v = json!({"address": "P", "instructions": [], "events": [{"name": "E", "discriminator": [1,2,3,4,5,6,7,8]}],
            "types": [{"name": "E", "type": {"kind": "struct", "fields": [
                {"name": "a", "type": "u64"}, {"name": "k", "type": "pubkey"}, {"name": "s", "type": "string"},
                {"name": "o", "type": {"option": "u8"}}, {"name": "v", "type": {"vec": "u16"}}, {"name": "x", "type": "u128"},
                {"name": "e", "type": {"defined": {"name": "D"}}}]}},
                {"name": "D", "type": {"kind": "enum", "variants": [{"name": "Buy"}, {"name": "Sell"}]}}]});
        static K: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = format!("/tmp/idl_test_{}_{}.json", std::process::id(), K.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        std::fs::write(&p, v.to_string()).unwrap();
        let i = Idl::load(&p);
        let _ = std::fs::remove_file(p);
        i
    }
    #[test]
    fn borsh_from_idl_exact() {
        let mut b = vec![1, 2, 3, 4, 5, 6, 7, 8];
        b.extend(u64::MAX.to_le_bytes());
        b.extend([7u8; 32]);
        b.extend(2u32.to_le_bytes());
        b.extend(b"hi");
        b.extend([1, 9]);
        b.extend(2u32.to_le_bytes());
        b.extend(3u16.to_le_bytes());
        b.extend(4u16.to_le_bytes());
        b.extend((1u128 << 100).to_le_bytes());
        b.push(1);
        let (name, v, full) = idl().event(&b).unwrap();
        assert_eq!(name, "E");
        assert_eq!(full, 0);
        assert_eq!(v["a"], json!(u64::MAX.to_string()));
        assert_eq!(v["k"], json!(bs58::encode([7u8; 32]).into_string()));
        assert_eq!((v["s"].as_str(), v["o"].as_u64()), (Some("hi"), Some(9)));
        assert_eq!(v["v"], json!([3, 4]));
        assert_eq!(v["x"], json!((1u128 << 100).to_string()));
        assert_eq!(v["e"], json!("Sell"));
    }
    #[test]
    fn short_payload_is_none_not_garbage() {
        let b = vec![1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3];
        assert!(idl().event(&b).is_none());
    }
    #[test]
    fn extra_bytes_flagged_not_fully_decoded() {
        let mut b = vec![1, 2, 3, 4, 5, 6, 7, 8];
        b.extend(1u64.to_le_bytes());
        b.extend([0u8; 32]);
        b.extend(0u32.to_le_bytes());
        b.push(0);
        b.extend(0u32.to_le_bytes());
        b.extend(0u128.to_le_bytes());
        b.push(0);
        b.push(99); // trailing byte (newer on-chain layout than the IDL)
        let (_, _, full) = idl().event(&b).unwrap();
        assert_eq!(full, 1);
    }
}
