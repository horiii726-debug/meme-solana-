//! Honeypot evidence from transactions (read-only, offline). Per liquid pool (SOL reserve >= 1 at the 20th swap):
//!   - successful buys / successful sells by NON-creator wallets in the first 5 minutes;
//!   - failed transactions that touched the pool, split by the venue instruction they called (buy/sell side from the
//!     instruction name for PumpSwap; for DAMM v2 swap the side comes from the mint of the input token account);
//!   - token program of the meme mint (spl-token vs Token-2022) from the tx token balances.
//! A "sell-blocked" pool = >= 10 successful buys, 0 successful non-creator sells, and >= 3 failed sell attempts.
//!   honeypot <raw_file>...
use dekode::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const WSOL: &str = "So11111111111111111111111111111111111111112";

#[derive(Default)]
struct St { buys: u32, sells_nc: u32, fail_buy: u32, fail_sell: u32, fail_other: u32, t0: f64, creator: String, prog: String, liq_ok: bool, swaps: u32 }

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let mut st: HashMap<String, St> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut recs: Vec<Value> = vec![];
    for p in &files {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            if let Ok(r) = serde_json::from_str::<Value>(&l) { if r["kind"] == "tx" { recs.push(r) } }
        }
    }
    recs.sort_by(|a, b| a["slot"].as_u64().cmp(&b["slot"].as_u64()));
    for r in &recs {
        let sig = r["sig"].as_str().unwrap_or("").to_string();
        if !seen.insert(sig) { continue }
        let pool = r["pool"].as_str().unwrap_or("").to_string();
        let venue = r["venue"].as_str().unwrap_or("");
        let tx = &r["tx"];
        let t = tx["blockTime"].as_f64().unwrap_or(0.0);
        let e = st.entry(pool.clone()).or_default();
        if e.t0 == 0.0 { e.t0 = t }
        if matches!(r["source"].as_str(), Some("live_create" | "history_create")) {
            e.creator = tx["transaction"]["message"]["accountKeys"][0].as_str().unwrap_or("").to_string();
        }
        if e.prog.is_empty() {
            for b in tx["meta"]["postTokenBalances"].as_array().into_iter().flatten() {
                if b["mint"].as_str().map_or(false, |m| m != WSOL) { e.prog = b["programId"].as_str().unwrap_or("").to_string(); break }
            }
        }
        if t > e.t0 + 300.0 { continue }
        let idl = if venue == "pumpswap" { &ps } else { &dm };
        let signer = tx["transaction"]["message"]["accountKeys"][0].as_str().unwrap_or("").to_string();
        if r["failed"] == true {
            // side of the failed attempt from the venue instruction
            let (flat, _) = flatten(tx);
            let tb = token_balances(tx);
            let mut side = 0; // 1 buy, 2 sell
            for ix in flat.iter().filter(|ix| ix.prog == idl.program) {
                let Some((name, accs)) = idl.ix(&ix.data) else { continue };
                if venue == "pumpswap" {
                    // buy* = receive base; in base=WSOL pools receiving base means selling the meme
                    let base_is_sol = accs.iter().position(|a| a == "base_mint").and_then(|i| ix.accounts.get(i)).map_or(false, |m| m == WSOL);
                    if name.starts_with("buy") { side = if base_is_sol { 2 } else { 1 } }
                    if name.starts_with("sell") { side = if base_is_sol { 1 } else { 2 } }
                } else if name.starts_with("swap") {
                    let inp = accs.iter().position(|a| a == "input_token_account").and_then(|i| ix.accounts.get(i));
                    if let Some(m) = inp.and_then(|a| tb.get(a)).map(|x| x.0.clone()) { side = if m == WSOL { 1 } else { 2 } }
                }
                if side != 0 { break }
            }
            match side { 1 => e.fail_buy += 1, 2 => e.fail_sell += 1, _ => e.fail_other += 1 }
            continue;
        }
        for ev in events(tx, idl) {
            if ev["fields"]["pool"] != json!(pool) { continue }
            let f = &ev["fields"];
            let acc = &ev["ix_accounts"];
            let buy = match ev["event"].as_str() {
                Some("BuyEvent") => acc["quote_mint"] == json!(WSOL),
                Some("SellEvent") => acc["base_mint"] == json!(WSOL),
                Some("EvtSwap2") => {
                    let sol_is_a = acc["token_a_mint"] == json!(WSOL);
                    (f["trade_direction"].as_u64() == Some(0)) == sol_is_a
                }
                _ => continue,
            };
            e.swaps += 1;
            if e.swaps == 20 {
                // SOL reserve of the pool after the 20th swap
                let tb = token_balances(tx);
                let liq = tb.values().filter(|v| v.0 == WSOL && (v.1 == pool || acc.as_object().map_or(false, |o| o.values().any(|x| x == &json!(v.1))))).filter_map(|v| v.4).max().unwrap_or(0) as f64 / 1e9;
                e.liq_ok = liq >= 1.0 || venue == "meteora_damm_v2";
            }
            if buy { e.buys += 1 } else if signer != e.creator { e.sells_nc += 1 }
        }
    }
    let liquid: Vec<(&String, &St)> = st.iter().filter(|(_, s)| s.swaps >= 20).collect();
    let blocked: Vec<&String> = liquid.iter().filter(|(_, s)| s.buys >= 10 && s.sells_nc == 0 && s.fail_sell >= 3).map(|x| x.0).collect();
    let no_sells: usize = liquid.iter().filter(|(_, s)| s.buys >= 10 && s.sells_nc == 0).count();
    let t22 = liquid.iter().filter(|(_, s)| s.prog.starts_with("TokenzQd")).count();
    let (fb, fs): (u32, u32) = liquid.iter().fold((0, 0), |a, (_, s)| (a.0 + s.fail_buy, a.1 + s.fail_sell));
    let (sb, ss): (u32, u32) = liquid.iter().fold((0, 0), |a, (_, s)| (a.0 + s.buys, a.1 + s.sells_nc));
    println!("{}", json!({"pools_with_20_swaps": liquid.len(), "pools_ge10_buys_and_zero_noncreator_sells_5min": no_sells, "sell_blocked_pools": blocked.len(), "sell_blocked_examples": blocked.iter().take(5).collect::<Vec<_>>(),
        "token2022_mints": t22, "first5min_success_buys": sb, "first5min_success_sells_noncreator": ss, "first5min_failed_buys": fb, "first5min_failed_sells": fs,
        "fail_rate_buy": fb as f64 / (fb + sb).max(1) as f64, "fail_rate_sell": fs as f64 / (fs + ss).max(1) as f64}));
}
