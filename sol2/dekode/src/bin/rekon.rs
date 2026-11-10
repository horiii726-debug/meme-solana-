//! C3 accuracy test (offline): every decoded swap event vs the token-balance changes of the pool vaults in the SAME tx.
//!   rekon <raw_file>...
//! Vault accounts come from the swap instruction accounts (IDL names). The chain's pre/postTokenBalances are the
//! independent truth. For each candidate identity (event field == vault delta) the hit count is reported, so the
//! correct field mapping is chosen from data, not assumed. Only (tx, pool) pairs with exactly one swap event of that pool
//! are reconciled (several swaps on one pool in one tx sum up in the deltas); the others are counted.
//! Also reports: event decode coverage, trailing bytes per event, pairing failures, quote mints per pool,
//! liquidity events (PumpSwap Deposit/Withdraw, DAMM v2 EvtLiquidityChange) per pool.
use dekode::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

fn read(paths: &[String]) -> Vec<Value> {
    let mut out = vec![];
    for p in paths {
        let Ok(f) = std::fs::File::open(p) else { continue };
        let Ok(d) = zstd::Decoder::new(f) else { continue };
        use std::io::BufRead;
        for l in std::io::BufReader::new(d).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l) { out.push(v) }
        }
    }
    out
}

#[derive(Default)]
struct Tally {
    n: u64,
    hits: BTreeMap<String, u64>,
    miss_sigs: BTreeMap<String, Vec<String>>,
}
impl Tally {
    fn check(&mut self, name: &str, ok: Option<bool>, sig: &str) {
        match ok {
            Some(true) => *self.hits.entry(name.into()).or_insert(0) += 1,
            Some(false) => {
                self.hits.entry(name.into()).or_insert(0);
                let v = self.miss_sigs.entry(name.into()).or_default();
                if v.len() < 3 { v.push(sig.into()) }
            }
            None => *self.hits.entry(format!("{name}__no_data")).or_insert(0) += 1,
        }
    }
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
    let mut ev_count: BTreeMap<String, u64> = BTreeMap::new();
    let mut extra: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
    let mut quote_of: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    let mut liq_ev: BTreeMap<String, u64> = BTreeMap::new();
    let (mut multi, mut unpaired, mut tx_ok, mut tx_fail) = (0u64, 0u64, 0u64, 0u64);
    for r in read(&files).iter().filter(|r| r["kind"] == "tx") {
        if r["failed"] == true { tx_fail += 1; continue }
        tx_ok += 1;
        let venue = r["venue"].as_str().unwrap_or("").to_string();
        let pool = r["pool"].as_str().unwrap_or("").to_string();
        let sig = r["sig"].as_str().unwrap_or("");
        let tx = &r["tx"];
        let tb = token_balances(tx);
        let idl = if venue == "pumpswap" { &ps } else { &dm };
        let evs: Vec<Value> = events(tx, idl);
        for e in &evs {
            let name = format!("{venue}:{}", e["event"].as_str().unwrap_or(e["kind"].as_str().unwrap_or("?")));
            *ev_count.entry(name.clone()).or_insert(0) += 1;
            *extra.entry(name).or_default().entry(e["extra_bytes"].as_u64().unwrap_or(0)).or_insert(0) += 1;
            if e["ix"].is_null() && e["kind"] == "event" { unpaired += 1 }
            if matches!(e["event"].as_str(), Some("DepositEvent" | "WithdrawEvent" | "EvtLiquidityChange")) && e["fields"]["pool"] == json!(pool) {
                *liq_ev.entry(format!("{venue} {pool} {}", e["event"].as_str().unwrap())).or_insert(0) += 1;
            }
        }
        let swaps: Vec<&Value> = evs.iter().filter(|e| matches!(e["event"].as_str(), Some("BuyEvent" | "SellEvent" | "EvtSwap2")) && e["fields"]["pool"] == json!(pool)).collect();
        if swaps.len() != 1 {
            if swaps.len() > 1 { multi += 1 }
            continue;
        }
        let e = swaps[0];
        let (f, acc) = (&e["fields"], &e["ix_accounts"]);
        let delta = |a: Option<&str>| -> Option<i128> {
            let t = tb.get(a?)?;
            Some(t.4.unwrap_or(0) as i128 - t.3.unwrap_or(0) as i128)
        };
        let pre = |a: Option<&str>| tb.get(a?).and_then(|t| t.3);
        let post = |a: Option<&str>| tb.get(a?).and_then(|t| t.4);
        let g = |k: &str| n(&f[k]).map(|x| x as i128);
        if venue == "pumpswap" {
            let (bv, qv) = (acc["pool_base_token_account"].as_str(), acc["pool_quote_token_account"].as_str());
            if let (Some(bm), Some(qm)) = (acc["base_mint"].as_str(), acc["quote_mint"].as_str()) {
                quote_of.entry(pool.clone()).or_insert((venue.clone(), bm.into(), qm.into()));
            }
            let (db, dq) = (delta(bv), delta(qv));
            let buy = e["event"] == "BuyEvent";
            let t = tally.entry(format!("pumpswap_{}", if buy { "buy" } else { "sell" })).or_default();
            t.n += 1;
            let eq = |a: Option<i128>, b: Option<i128>| a.zip(b).map(|(a, b)| a == b);
            let sum = |a: Option<i128>, b: Option<i128>| a.zip(b).map(|(a, b)| a + b);
            if buy {
                t.check("base: -d_base_vault == base_amount_out", eq(db.map(|x| -x), g("base_amount_out")), sig);
                t.check("quote: d_quote_vault == quote_amount_in", eq(dq, g("quote_amount_in")), sig);
                t.check("quote: d_quote_vault == quote_amount_in + lp_fee", eq(dq, sum(g("quote_amount_in"), g("lp_fee"))), sig);
                t.check("quote: d_quote_vault == quote_amount_in_with_lp_fee", eq(dq, g("quote_amount_in_with_lp_fee")), sig);
            } else {
                t.check("base: d_base_vault == base_amount_in", eq(db, g("base_amount_in")), sig);
                t.check("quote: -d_quote_vault == quote_amount_out", eq(dq.map(|x| -x), g("quote_amount_out")), sig);
                t.check("quote: -d_quote_vault == quote_amount_out_without_lp_fee", eq(dq.map(|x| -x), g("quote_amount_out_without_lp_fee")), sig);
                t.check("quote: -d_quote_vault == user_quote_amount_out", eq(dq.map(|x| -x), g("user_quote_amount_out")), sig);
            }
            // residual of the main quote identity, and which single event field (if any) equals it
            let main = if buy { dq.zip(g("quote_amount_in_with_lp_fee")).map(|(d, x)| d - x) } else { dq.zip(g("quote_amount_out_without_lp_fee")).map(|(d, x)| -d - x) };
            if let Some(res) = main.filter(|r| *r != 0) {
                let orient = if acc["base_mint"] == json!("So11111111111111111111111111111111111111112") { "base=WSOL" } else { "quote=WSOL" };
                let mut by: Vec<String> = f.as_object().into_iter().flatten()
                    .filter(|(_, v)| n(v).map_or(false, |x| x as i128 == res.abs()))
                    .map(|(k, _)| format!("{}{k}", if res < 0 { "-" } else { "+" })).collect();
                if by.is_empty() { by.push("unexplained".into()) }
                for b in by {
                    *t.hits.entry(format!("residual[{orient}] {b}")).or_insert(0) += 1;
                }
            }
            t.check("reserves: event pool_base == vault PRE", eq(pre(bv).map(|x| x as i128), g("pool_base_token_reserves")), sig);
            t.check("reserves: event pool_base == vault POST", eq(post(bv).map(|x| x as i128), g("pool_base_token_reserves")), sig);
            t.check("reserves: event pool_quote == vault PRE", eq(pre(qv).map(|x| x as i128), g("pool_quote_token_reserves")), sig);
            t.check("reserves: event pool_quote == vault POST", eq(post(qv).map(|x| x as i128), g("pool_quote_token_reserves")), sig);
        } else {
            let (av, bv) = (acc["token_a_vault"].as_str(), acc["token_b_vault"].as_str());
            if let (Some(am), Some(bm)) = (acc["token_a_mint"].as_str(), acc["token_b_mint"].as_str()) {
                quote_of.entry(pool.clone()).or_insert((venue.clone(), am.into(), bm.into()));
            }
            let dir = f["trade_direction"].as_u64();
            // IDL: trade_direction u8; 0/1 meaning checked here from data: input vault must increase
            let (din_a, din_b) = (delta(av), delta(bv));
            let t = tally.entry("damm_v2_swap".into()).or_default();
            t.n += 1;
            let r = &f["swap_result"];
            let rg = |k: &str| n(&r[k]).map(|x| x as i128);
            let (d_in, d_out) = match dir { Some(0) => (din_a, din_b), Some(1) => (din_b, din_a), _ => (None, None) };
            let eq = |a: Option<i128>, b: Option<i128>| a.zip(b).map(|(a, b)| a == b);
            t.check("dir: input vault increased (0 = A->B)", d_in.map(|x| x > 0), sig);
            t.check("in: d_in == included_fee_input_amount", eq(d_in, rg("included_fee_input_amount")), sig);
            t.check("in: d_in == excluded_fee_input_amount", eq(d_in, rg("excluded_fee_input_amount")), sig);
            t.check("in: d_in == included_transfer_fee_amount_in", eq(d_in, g("included_transfer_fee_amount_in")), sig);
            t.check("out: -d_out == output_amount", eq(d_out.map(|x| -x), rg("output_amount")), sig);
            t.check("out: -d_out == included_transfer_fee_amount_out", eq(d_out.map(|x| -x), g("included_transfer_fee_amount_out")), sig);
            t.check("reserve_a == vault_a POST", eq(post(av).map(|x| x as i128), g("reserve_a_amount")), sig);
            t.check("reserve_b == vault_b POST", eq(post(bv).map(|x| x as i128), g("reserve_b_amount")), sig);
        }
    }
    let rep = |t: &Tally| -> Value {
        json!({"swaps": t.n, "identity_hits": t.hits.iter().map(|(k, v)| (k.clone(), json!(format!("{v}/{} = {:.2}%", t.n, 100.0 * *v as f64 / t.n.max(1) as f64)))).collect::<serde_json::Map<_, _>>(),
            "miss_examples": t.miss_sigs})
    };
    println!("{}", serde_json::to_string_pretty(&json!({
        "tx_success": tx_ok, "tx_failed": tx_fail, "tx_pool_pairs_with_multiple_swaps": multi, "events_unpaired": unpaired,
        "events": ev_count, "trailing_bytes_by_event": extra,
        "reconciliation": tally.iter().map(|(k, t)| (k.clone(), rep(t))).collect::<serde_json::Map<_, _>>(),
        "pool_mints (venue, base/A, quote/B)": quote_of, "liquidity_events_per_pool": liq_ev})).unwrap());
}
