//! Inspect one stored tx: decoded events of its venue and every token-balance change (owner, mint, pre, post, delta).
//!   lihat <signature> <raw_file>...
use dekode::*;
use serde_json::{json, Value};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let sig = &a[1];
    for p in &a[2..] {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            if !l.contains(sig.as_str()) { continue }
            let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
            if r["sig"] != json!(sig) || r["kind"] != "tx" { continue }
            let idl = Idl::load(if r["venue"] == "pumpswap" { "/root/sol/idl/pump_amm.json" } else { "/root/sol/idl/meteora_damm_v2.json" });
            for e in events(&r["tx"], &idl) {
                println!("EVENT {} ix={} extra={} fields={}", e["event"], e["ix"], e["extra_bytes"], e["fields"]);
                println!("  accounts={}", e["ix_accounts"]);
            }
            for (k, v) in token_balances(&r["tx"]) {
                let d = v.4.unwrap_or(0) as i128 - v.3.unwrap_or(0) as i128;
                if d != 0 { println!("TOKEN {k} owner={} mint={} pre={:?} post={:?} delta={d}", v.1, v.0, v.3, v.4) }
            }
            for l in r["tx"]["meta"]["logMessages"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                if l.contains("Instruction:") || l.contains("invoke [") { println!("LOG {l}") }
            }
            return;
        }
    }
}
