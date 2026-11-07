//! Decoder check (read-only): fetch real transactions and print every decoded event.
//!   cek <signature>...             log-based decoders (Pump.fun, PumpSwap)
//!   cek --program <id> [n]         last n successful txs of a program, full-transaction decoder (LaunchLab, DBC)
//! RPC from SOL_RPC (file .rpc.env). Used to verify decoders on real chain data and to build golden tests.
#[path = "../dekode.rs"]
#[allow(dead_code, unused_imports)]
mod dekode;
#[path = "../dekode_tx.rs"]
#[allow(dead_code)]
mod dekode_tx;
use base64::Engine;
use std::collections::BTreeMap;
use std::time::Duration;

fn rpc(url: &str, body: serde_json::Value) -> Option<serde_json::Value> {
    let r = ureq::post(url).timeout(Duration::from_secs(60)).send_json(body).ok()?.into_json::<serde_json::Value>().ok()?;
    std::thread::sleep(Duration::from_millis(150));
    Some(r["result"].clone())
}

fn main() {
    let Ok(url) = std::env::var("SOL_RPC") else {
        eprintln!("ERROR: SOL_RPC belum diisi (berkas .rpc.env)");
        std::process::exit(2);
    };
    let a: Vec<String> = std::env::args().skip(1).collect();
    // --dbcpool <file with pool addresses>: read Meteora DBC VirtualPool accounts (bytemuck repr(C), official IDL):
    // 8 disc + VolatilityTracker 64 | config 72 | creator 104 | base_mint 136 | ... | quote_reserve 240 | sqrt 280 |
    // activation 296 | pool_type 304 | is_migrated 305 | ... | migration_progress 308 | metrics 312..344 |
    // finish_curve_timestamp 344. Output: pool, base_mint (to verify the offsets), is_migrated, progress, finish time.
    if a.first().map(|s| s.as_str()) == Some("--dbcpool") {
        let pools: Vec<String> = std::fs::read_to_string(a.get(1).cloned().unwrap_or_default()).unwrap_or_default()
            .lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        for chunk in pools.chunks(100) {
            let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getMultipleAccounts", "params": [chunk, {"encoding": "base64"}]});
            let Some(r) = rpc(&url, body) else { continue };
            for (p, acc) in chunk.iter().zip(r["value"].as_array().cloned().unwrap_or_default()) {
                let Some(d) = acc["data"][0].as_str().and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()) else {
                    println!("{{\"k\":\"dbcpool\",\"pool\":\"{p}\",\"ada\":false}}");
                    continue;
                };
                if d.len() < 352 || d[..8] != [213, 224, 5, 209, 98, 69, 119, 92] {
                    println!("{{\"k\":\"dbcpool\",\"pool\":\"{p}\",\"ada\":false}}");
                    continue;
                }
                let u64at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap_or([0; 8]));
                println!("{}", serde_json::json!({"k": "dbcpool", "pool": p, "ada": true, "base_mint": bs58::encode(&d[136..168]).into_string(),
                    "quote_reserve": u64at(240), "is_migrated": d[305], "migration_progress": d[308], "finish_curve_timestamp": u64at(344)}));
            }
        }
        return;
    }
    if a.first().map(|s| s.as_str()) == Some("--program") {
        let prog = a.get(1).cloned().unwrap_or_default();
        let n: u64 = a.get(2).and_then(|x| x.parse().ok()).unwrap_or(20);
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getTransactionsForAddress", "params": [prog, {
            "transactionDetails": "full", "limit": n.min(100), "sortOrder": "desc", "encoding": "json",
            "maxSupportedTransactionVersion": 1, "commitment": "confirmed", "filters": {"status": "succeeded"}}]});
        let Some(r) = rpc(&url, body) else {
            eprintln!("rpc gagal");
            std::process::exit(1);
        };
        let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
        for tx in r["data"].as_array().into_iter().flatten() {
            let sig = tx["transaction"]["signatures"][0].as_str().unwrap_or("");
            for e in dekode_tx::trades(tx) {
                println!("{sig}\t{e}");
            }
            // event-CPI payloads of this program that the decoder does not handle (for coverage reports)
            for ix in dekode_tx::flatten(tx).iter().filter(|ix| ix.prog == prog && ix.data.len() >= 16 && ix.data[..8] == [228, 69, 165, 46, 81, 203, 154, 29]) {
                let d: Vec<u8> = ix.data[8..16].to_vec();
                if d != dekode_tx::D_LL_TRADE.to_vec() && d != dekode_tx::D_DBC_SWAP2.to_vec() {
                    *unknown.entry(format!("{d:?}")).or_insert(0) += 1;
                }
            }
        }
        for (d, c) in unknown {
            println!("EVENT LAIN disc={d} x{c}");
        }
        return;
    }
    for sig in a {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getTransaction",
            "params": [sig, {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "confirmed"}]});
        let Some(r) = rpc(&url, body) else {
            eprintln!("{sig}: rpc gagal");
            continue;
        };
        for l in r["meta"]["logMessages"].as_array().into_iter().flatten().filter_map(|l| l.as_str()) {
            let Some(d) = l.strip_prefix("Program data: ") else { continue };
            let Ok(b) = base64::engine::general_purpose::STANDARD.decode(d) else { continue };
            match dekode::dekode(&b) {
                Some(e) => println!("{sig}\t{e}"),
                None => println!("{sig}\ttidak dikenal disc={:?} len={}", b.get(..8), b.len()),
            }
        }
        for e in dekode_tx::trades(&r) {
            println!("{sig}\t{e}");
        }
    }
}
