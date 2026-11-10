//! Find accounts that appear in EVERY stored pool-creation tx of a venue but in (almost) no swap tx of that venue.
//! Such an account lets getSignaturesForAddress list pool creations directly (few pages per day) instead of paging the
//! whole program. Offline, reads raw files.
//!   akun_lahir <raw_file>...
use dekode::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    // venue -> (creation tx count, account -> count in creation txs), (swap tx count, account -> count in swap txs)
    let mut cre: HashMap<String, (u64, HashMap<String, u64>)> = HashMap::new();
    let mut swp: HashMap<String, (u64, HashMap<String, u64>)> = HashMap::new();
    for p in &files {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
            if r["kind"] != "tx" || r["failed"] == true { continue }
            let venue = r["venue"].as_str().unwrap_or("").to_string();
            let idl = if venue == "pumpswap" { &ps } else { &dm };
            let evs = events(&r["tx"], idl);
            let is_create = evs.iter().any(|e| matches!(e["event"].as_str(), Some("CreatePoolEvent" | "EvtInitializePool")));
            let is_swap = evs.iter().any(|e| matches!(e["event"].as_str(), Some("BuyEvent" | "SellEvent" | "EvtSwap2")));
            let keys: HashSet<String> = all_keys(&r["tx"]).into_iter().collect();
            let (flat, _) = flatten(&r["tx"]);
            // creation route = program of the OUTER instruction that contains the create instruction
            let route = flat.iter().find(|ix| ix.prog == idl.program && idl.ix(&ix.data).map_or(false, |(n, _)| n.starts_with("create_pool") || n.starts_with("initialize")))
                .and_then(|ix| flat.iter().find(|o| o.outer == ix.outer && o.inner.is_none()).map(|o| o.prog.clone())).unwrap_or_default();
            let key = if is_create { format!("{venue} via {route}") } else { venue.clone() };
            let target = if is_create { &mut cre } else if is_swap { &mut swp } else { continue };
            let e = target.entry(key).or_default();
            e.0 += 1;
            for k in keys { *e.1.entry(k).or_insert(0) += 1 }
        }
    }
    let mut out = BTreeMap::new();
    for (venue, (nc, cm)) in &cre {
        let (ns, sm) = swp.get(venue.split(" via ").next().unwrap_or("")).cloned().unwrap_or_default();
        let mut cands: Vec<Value> = cm.iter().filter(|(_, c)| **c == *nc).map(|(k, _)| {
            let s = sm.get(k).copied().unwrap_or(0);
            json!({"account": k, "in_creations": format!("{nc}/{nc}"), "in_swaps": format!("{s}/{ns}")})
        }).collect();
        cands.sort_by_key(|v| v["in_swaps"].as_str().unwrap_or("").split('/').next().unwrap_or("0").parse::<u64>().unwrap_or(0));
        out.insert(venue.clone(), cands);
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
