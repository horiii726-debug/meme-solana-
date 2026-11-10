//! Per-pool swap series from raw files (decoded on-chain swaps only; no candles).
//! Price: PumpSwap = SOL vault / token vault after the swap (either side may be WSOL); DAMM v2 = next_sqrt_price.
//! Duplicate (pool, signature) records across datasets are counted once.
use crate::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub const WSOL: &str = "So11111111111111111111111111111111111111112";

#[derive(Clone, Debug)]
pub struct Sw {
    pub t: f64,
    pub slot: u64,
    pub buy: bool,
    /// SOL moved by the trader (lamports / 1e9)
    pub sol: f64,
    pub trader: String,
    /// SOL per token after the swap
    pub px: f64,
    /// SOL reserve of the pool after the swap
    pub liq: f64,
    /// total fee rate charged on this swap (from the event), fraction
    pub fee: f64,
    /// tokens moved by the trader (human units, from the event: what the trader really paid / received)
    pub tok: f64,
    /// network fee actually paid by this transaction (meta.fee, base + priority), SOL
    pub txfee: f64,
    /// DAMM v2 only: sqrt price after the swap (sqrt(b_raw / a_raw)), SOL is token a?, token decimals; 0 / false otherwise
    pub sqrt: f64,
    pub sol_is_a: bool,
    pub dec_tok: i32,
    /// DAMM v2 only: liquidity L estimated from this swap and the previous one (|d b_raw| / |d sqrt|), 0 if unknown
    pub l: f64,
    /// transaction signature of this swap
    pub sig: String,
}

#[derive(Default, Clone)]
pub struct Pool {
    pub venue: String,
    pub route: String,
    pub creator: String,
    pub created: f64,
    pub swaps: Vec<Sw>,
    /// (time, instruction) of liquidity removals
    pub lp_out: Vec<(f64, String)>,
    /// times of LP lock events
    pub lp_lock: Vec<f64>,
    pub failed: Vec<f64>,
    /// the non-WSOL mint of the pool
    pub mint: String,
}

/// one decoded raw line (produced in parallel, merged in order afterwards)
enum Rec {
    PoolNew { pool: String, route: String, created: f64 },
    Tx { pool: String, sig: String, venue: String, t: f64, failed: bool, creator: Option<String>, swaps: Vec<Sw>, lp_out: Vec<(f64, String)>, lp_lock: Vec<f64>, mint: Option<String> },
}

fn decode_line(l: &str, ps: &Idl, dm: &Idl) -> Option<Rec> {
    let r: Value = serde_json::from_str(l).ok()?;
    let pool = r["pool"].as_str().unwrap_or("").to_string();
    if r["kind"] == "pool_new" {
        return Some(Rec::PoolNew { pool, route: r["route"].as_str().unwrap_or(r["create_ix"].as_str().unwrap_or("")).to_string(), created: r["block_time"].as_f64().unwrap_or(0.0) });
    }
    if r["kind"] != "tx" { return None }
    let venue = r["venue"].as_str().unwrap_or("").to_string();
    let tx = &r["tx"];
    let t = tx["blockTime"].as_f64().unwrap_or(0.0);
    let sig = r["sig"].as_str().unwrap_or("").to_string();
    if r["failed"] == true {
        return Some(Rec::Tx { pool, sig, venue, t, failed: true, creator: None, swaps: vec![], lp_out: vec![], lp_lock: vec![], mint: None });
    }
    let creator = if matches!(r["source"].as_str(), Some("live_create" | "history_create")) { tx["transaction"]["message"]["accountKeys"][0].as_str().map(String::from) } else { None };
    let tb = token_balances(tx);
    let idl = if venue == "pumpswap" { ps } else { dm };
    let (mut swaps, mut lp_out, mut lp_lock, mut mint) = (vec![], vec![], vec![], None);
    for ev in events(tx, idl) {
        let (f, acc) = (&ev["fields"], &ev["ix_accounts"]);
        if f["pool"] != json!(pool) { continue }
        let name = ev["event"].as_str().unwrap_or("");
        let ix = ev["ix"].as_str().unwrap_or("");
        if ix.starts_with("remove") || ix == "withdraw" { lp_out.push((t, ix.to_string())) }
        if name.contains("Lock") { lp_lock.push(t) }
        if mint.is_none() {
            for k in ["base_mint", "quote_mint", "token_a_mint", "token_b_mint"] {
                if let Some(m) = acc[k].as_str() { if m != WSOL { mint = Some(m.to_string()) } }
            }
        }
        if let Some(mut s) = swap_of(name, f, acc, &tb, t, tx["slot"].as_u64().unwrap_or(0), tx["meta"]["fee"].as_f64().unwrap_or(0.0) / 1e9) { s.sig = sig.clone(); swaps.push(s) }
    }
    Some(Rec::Tx { pool, sig, venue, t, failed: false, creator, swaps, lp_out, lp_lock, mint })
}

/// Load and decode raw files using all CPU cores (one decompress thread per file, then parallel decoding of lines).
pub fn load_pools(files: &[String]) -> HashMap<String, Pool> {
    let ps = Idl::load("/root/sol/idl/pump_amm.json");
    let dm = Idl::load("/root/sol/idl/meteora_damm_v2.json");
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let lines: Vec<String> = std::thread::scope(|s| {
        files.iter().map(|p| s.spawn(move || {
            let Ok(f) = std::fs::File::open(p) else { return vec![] };
            let Ok(d) = zstd::Decoder::new(f) else { return vec![] };
            use std::io::BufRead;
            std::io::BufReader::new(d).lines().map_while(Result::ok).collect::<Vec<String>>()
        })).collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let chunk = (lines.len() / cores).max(1);
    let recs: Vec<Rec> = std::thread::scope(|s| {
        lines.chunks(chunk).map(|c| { let (ps, dm) = (&ps, &dm); s.spawn(move || c.iter().filter_map(|l| decode_line(l, ps, dm)).collect::<Vec<Rec>>()) })
            .collect::<Vec<_>>().into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut pools: HashMap<String, Pool> = HashMap::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for r in recs {
        match r {
            Rec::PoolNew { pool, route, created } => { let e = pools.entry(pool).or_default(); e.route = route; e.created = created; }
            Rec::Tx { pool, sig, venue, t, failed, creator, swaps, lp_out, lp_lock, mint } => {
                if !seen.insert((pool.clone(), sig)) { continue }
                let e = pools.entry(pool).or_default();
                e.venue = venue;
                if failed { e.failed.push(t); continue }
                if let Some(c) = creator { e.creator = c; if e.created == 0.0 { e.created = t } }
                if e.mint.is_empty() { if let Some(m) = mint { e.mint = m } }
                e.swaps.extend(swaps);
                e.lp_out.extend(lp_out);
                e.lp_lock.extend(lp_lock);
            }
        }
    }
    for p in pools.values_mut() { p.failed.sort_by(|a, b| a.partial_cmp(b).unwrap()) }
    for p in pools.values_mut() {
        p.swaps.sort_by(|a, b| a.slot.cmp(&b.slot).then(a.t.partial_cmp(&b.t).unwrap()));
        // DAMM v2: liquidity estimate L_i = |d b_raw| / |sqrt_i - sqrt_(i-1)| (b side raw amount moved by the trader)
        for i in 1..p.swaps.len() {
            let (s0, s1) = (p.swaps[i - 1].sqrt, p.swaps[i].sqrt);
            if s0 > 0.0 && s1 > 0.0 && (s1 - s0).abs() > 0.0 {
                let w = &p.swaps[i];
                let db = if w.sol_is_a { w.tok * 10f64.powi(w.dec_tok) } else { w.sol * 1e9 };
                p.swaps[i].l = db / (s1 - s0).abs();
            }
        }
        p.lp_out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    }
    pools
}

fn swap_of(ev: &str, f: &Value, acc: &Value, tb: &HashMap<String, (String, String, Option<u64>, Option<u128>, Option<u128>)>, t: f64, slot: u64, txfee: f64) -> Option<Sw> {
    match ev {
        "BuyEvent" | "SellEvent" => {
            let (bm, qm) = (acc["base_mint"].as_str()?, acc["quote_mint"].as_str()?);
            let bv = tb.get(acc["pool_base_token_account"].as_str()?)?;
            let qv = tb.get(acc["pool_quote_token_account"].as_str()?)?;
            let (bd, qd, bp, qp) = (bv.2?, qv.2?, bv.4?, qv.4?);
            let buy_base = ev == "BuyEvent";
            let fee_bps = ["lp_fee_basis_points", "protocol_fee_basis_points", "coin_creator_fee_basis_points"].iter().filter_map(|k| n(&f[*k])).sum::<u128>() as f64;
            // trader-side amounts (fees included): user_quote_amount_in / user_quote_amount_out per idl/pump_amm.json
            let (sol_raw, tok_amt, sol_res, tok_res, buy) = if qm == WSOL {
                (if buy_base { n(&f["user_quote_amount_in"]) } else { n(&f["user_quote_amount_out"]) }?,
                 if buy_base { n(&f["base_amount_out"]) } else { n(&f["base_amount_in"]) }? as f64 / 10f64.powi(bd as i32),
                 qp as f64 / 10f64.powi(qd as i32), bp as f64 / 10f64.powi(bd as i32), buy_base)
            } else if bm == WSOL {
                (if buy_base { n(&f["base_amount_out"]) } else { n(&f["base_amount_in"]) }?,
                 if buy_base { n(&f["user_quote_amount_in"]) } else { n(&f["user_quote_amount_out"]) }? as f64 / 10f64.powi(qd as i32),
                 bp as f64 / 10f64.powi(bd as i32), qp as f64 / 10f64.powi(qd as i32), !buy_base)
            } else { return None };
            if tok_res <= 0.0 { return None }
            Some(Sw { t, slot, buy, sol: sol_raw as f64 / 1e9, trader: f["user"].as_str().unwrap_or("").into(), px: sol_res / tok_res, liq: sol_res, fee: fee_bps / 1e4, tok: tok_amt, txfee, sqrt: 0.0, sol_is_a: false, dec_tok: if qm == WSOL { bd as i32 } else { qd as i32 }, l: 0.0, sig: String::new() })
        }
        "EvtSwap2" => {
            let (am, bm) = (acc["token_a_mint"].as_str()?, acc["token_b_mint"].as_str()?);
            let (ad, bd) = (decimals_of(tb, am).as_u64()?, decimals_of(tb, bm).as_u64()?);
            let s = n(&f["swap_result"]["next_sqrt_price"])? as f64 / 18446744073709551616.0;
            let b_per_a = s * s * 10f64.powi(ad as i32 - bd as i32);
            let sol_is_a = am == WSOL;
            if !sol_is_a && bm != WSOL { return None }
            let a_in = f["trade_direction"].as_u64() == Some(0);
            let buy = a_in == sol_is_a;
            let r = &f["swap_result"];
            let sol_raw = if buy { n(&r["included_fee_input_amount"]) } else { n(&r["output_amount"]) }?;
            let tdec = if sol_is_a { bd } else { ad };
            let tok_amt = if buy { n(&r["output_amount"]) } else { n(&r["included_fee_input_amount"]) }? as f64 / 10f64.powi(tdec as i32);
            // fee fraction in the token it was charged in: on input when included > excluded input, else on output
            let fees: u128 = ["claiming_fee", "protocol_fee", "compounding_fee", "referral_fee"].iter().filter_map(|k| n(&r[*k])).sum();
            let (inc, exc, outa) = (n(&r["included_fee_input_amount"]).unwrap_or(0), n(&r["excluded_fee_input_amount"]).unwrap_or(0), n(&r["output_amount"]).unwrap_or(0));
            let fee = if inc > exc { (inc - exc) as f64 / inc.max(1) as f64 } else { fees as f64 / (fees + outa).max(1) as f64 };
            let fee = fee.clamp(0.0, 0.5);
            let solv = if sol_is_a { acc["token_a_vault"].as_str() } else { acc["token_b_vault"].as_str() };
            let liq = solv.and_then(|v| tb.get(v)).and_then(|x| x.4).unwrap_or(0) as f64 / 1e9;
            let px = if sol_is_a { 1.0 / b_per_a } else { b_per_a };
            if !(px > 0.0) || !px.is_finite() { return None }
            Some(Sw { t, slot, buy, sol: sol_raw as f64 / 1e9, trader: acc["payer"].as_str().unwrap_or("").into(), px, liq, fee, tok: tok_amt, txfee, sqrt: s, sol_is_a, dec_tok: tdec as i32, l: 0.0, sig: String::new() })
        }
        _ => None,
    }
}

// ---------------- trade simulation (shared by quant and tambang) ----------------
pub const Q: f64 = 0.1;
/// priority + base fee per transaction, SOL — ASSUMPTION [U]: not yet measured (C15)
pub const TX_COST: f64 = 0.0005;
#[derive(Clone, Copy, Debug)]
/// Launch(t): sell at pool age t s. Flow(w, r): sell at the NEXT swap after the last w seconds show sell SOL >= r x buy SOL
/// (at least 3 swaps in the window) — a causal sell-pressure exit.
pub enum Exit { Hold, Trail(f64), Ladder(f64, f64), Time(f64), Launch(f64), Flow(f64, f64) }

pub fn exit_name(e: &Exit) -> String {
    match e { Exit::Hold => "hold6h".into(), Exit::Trail(d) => format!("trail{:.0}%", d * 100.0), Exit::Ladder(m, d) => format!("half@{m}x+trail{:.0}%", d * 100.0), Exit::Time(t) => format!("time{t}s"), Exit::Launch(t) => format!("age{t}s"), Exit::Flow(w, r) => format!("flow{w}s_sell{r}x") }
}

/// constant product: tokens out for `q` SOL in at post-swap reserves (L SOL, L/px tokens), fee f
pub fn buy_tokens(liq: f64, px: f64, q: f64, f: f64) -> f64 {
    let (l, t) = (liq.max(1e-9), liq.max(1e-9) / px);
    t - l * t / (l + q * (1.0 - f))
}
pub fn sell_sol(liq: f64, px: f64, tok: f64, f: f64) -> f64 {
    let (l, t) = (liq.max(1e-9), liq.max(1e-9) / px);
    ((l - l * t / (t + tok)) * (1.0 - f.clamp(0.0, 0.5))).max(0.0)
}

/// return R (SOL out / SOL in - 1) of one trade; None if no entry possible
pub fn trade(p: &Pool, tau: f64, ex: Exit) -> Option<f64> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let rug = p.lp_out.first().map(|x| x.0);
    if rug.map_or(false, |r| r <= t0 + tau) { return None }
    let i0 = s.iter().position(|w| w.t > t0 + tau)?;
    let e = &s[i0];
    let mut tok = buy_tokens(e.liq, e.px, Q, e.fee);
    if !(tok > 0.0) { return None }
    let entry_px = Q / tok;
    let mut out = -TX_COST;
    let mut peak = e.px;
    let mut half_done = false;
    let (mut lo, mut wb, mut ws, mut wn, mut armed) = (i0 + 1, 0.0f64, 0.0f64, 0usize, false);
    for (j, w) in s.iter().enumerate().skip(i0 + 1) {
        if armed { out += sell_sol(w.liq, w.px, tok, w.fee) - TX_COST; return Some(out / Q - 1.0) }
        if let Exit::Flow(win, _) = ex {
            if w.buy { wb += w.sol } else { ws += w.sol }
            wn += 1;
            while lo < j && s[lo].t <= w.t - win { if s[lo].buy { wb -= s[lo].sol } else { ws -= s[lo].sol } wn -= 1; lo += 1 }
        }
        if rug.map_or(false, |r| w.t >= r) {
            // after an LP removal the remainder is sold into what is left
            out += sell_sol(w.liq, w.px, tok, w.fee) - TX_COST;
            return Some(out / Q - 1.0);
        }
        peak = peak.max(w.px);
        let mult = w.px / entry_px;
        let sell = |tk: f64| sell_sol(w.liq, w.px, tk, w.fee) - TX_COST;
        match ex {
            Exit::Hold => {}
            Exit::Time(tt) => if w.t - e.t >= tt { out += sell(tok); return Some(out / Q - 1.0) },
            Exit::Launch(tt) => if w.t - t0 >= tt { out += sell(tok); return Some(out / Q - 1.0) },
            Exit::Flow(_, r) => if wn >= 3 && ws > 0.0 && ws >= r * wb { armed = true },
            Exit::Trail(d) => if w.px <= peak * (1.0 - d) { out += sell(tok); return Some(out / Q - 1.0) },
            Exit::Ladder(m, d) => {
                if !half_done && mult >= m { out += sell(tok / 2.0); tok /= 2.0; half_done = true }
                if half_done && w.px <= peak * (1.0 - d) { out += sell(tok); return Some(out / Q - 1.0) }
            }
        }
    }
    if rug.is_some() { return Some(out / Q - 1.0) } // removed after the last observed swap: remainder lost
    let l = s.last().unwrap();
    out += sell_sol(l.liq, l.px, tok, l.fee) - TX_COST;
    Some(out / Q - 1.0)
}


// ---------------- trade replay against REAL market fills (no curve model, no assumed fee) ----------------
/// Execution latency: our transaction lands at the earliest LAT_SLOTS slots after the decision.
pub const LAT_SLOTS: u64 = 2;
/// How long we keep trying to get filled before the attempt counts as failed (seconds).
pub const FILL_WINDOW: f64 = 30.0;

/// realized trader price of a swap, SOL per token (fees included, trader side)
fn rpx(w: &Sw) -> f64 { if w.tok > 0.0 { w.sol / w.tok } else { f64::NAN } }

/// network fee we would pay = median meta.fee of the pool's swaps in the 30 s before `t` (real market fee level)
fn fee_at(s: &[Sw], t: f64) -> f64 {
    let mut v: Vec<f64> = s.iter().filter(|w| w.t <= t && w.t > t - 30.0 && w.txfee > 0.0).map(|w| w.txfee).collect();
    if v.is_empty() { v = s.iter().filter(|w| w.t <= t && w.txfee > 0.0).map(|w| w.txfee).collect() }
    if v.is_empty() { return f64::NAN }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[derive(Debug, Clone)]
pub struct RealTrade { pub ret: f64, pub entry_px: f64, pub exits: usize, pub failed_exits: usize, pub fees_sol: f64, pub held_into_rug: bool }

/// Replay one trade using only fills that really happened:
///  - entry = the first real BUY of >= Q SOL landing >= LAT_SLOTS after the decision (within FILL_WINDOW); we get its price;
///  - a sell of k tokens = the first real SELL of >= k tokens landing >= LAT_SLOTS after the exit decision; we get its price;
///    if none exists within FILL_WINDOW the exit FAILED and the position stays open (decided again at the next swap);
///  - fees = the pool's median real meta.fee at that time, per transaction.
/// Exit rules use realized trade prices only. Returns None when entry could not be filled.
pub fn trade_real(p: &Pool, tau: f64, ex: Exit) -> Option<RealTrade> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
    if rug <= t0 + tau { return None }
    let dec_slot = s.iter().filter(|w| w.t <= t0 + tau).map(|w| w.slot).max()?;
    let ie = s.iter().position(|w| w.buy && w.slot >= dec_slot + LAT_SLOTS && w.sol >= Q && w.t < rug && w.t <= t0 + tau + FILL_WINDOW)?;
    let e = &s[ie];
    let entry_px = rpx(e);
    if !entry_px.is_finite() || entry_px <= 0.0 { return None }
    let fee_in = fee_at(s, e.t);
    if !fee_in.is_finite() { return None }
    let mut tok = Q / entry_px;
    let (mut proceeds, mut fees, mut exits, mut failed) = (0.0, fee_in, 0usize, 0usize);
    let mut peak = entry_px;
    let mut half_done = false;
    // try to sell `amt` tokens after a decision at swap index j; returns (index of fill, SOL received)
    let sell = |j: usize, amt: f64| -> Option<(usize, f64)> {
        let (sl, tj) = (s[j].slot, s[j].t);
        s.iter().enumerate().skip(j + 1).find(|(_, w)| !w.buy && w.slot >= sl + LAT_SLOTS && w.tok >= amt && w.t <= tj + FILL_WINDOW)
            .map(|(k, w)| (k, amt * rpx(w)))
    };
    let mut j = ie;
    while j + 1 < s.len() {
        j += 1;
        let w = &s[j];
        let px = rpx(w);
        if !px.is_finite() { continue }
        peak = peak.max(px);
        let mult = px / entry_px;
        let want: Option<f64> = match ex {
            Exit::Hold => None,
            Exit::Time(tt) => (w.t - e.t >= tt).then_some(tok),
            Exit::Launch(tt) => (w.t - t0 >= tt).then_some(tok),
            Exit::Trail(d) => (px <= peak * (1.0 - d)).then_some(tok),
            Exit::Ladder(m, d) => if !half_done && mult >= m { Some(tok / 2.0) } else if half_done && px <= peak * (1.0 - d) { Some(tok) } else { None },
            Exit::Flow(_, _) => None,
        };
        let Some(amt) = want else { continue };
        match sell(j, amt) {
            Some((k, sol)) => {
                proceeds += sol;
                fees += fee_at(s, s[k].t).max(0.0);
                exits += 1;
                tok -= amt;
                if let Exit::Ladder(_, _) = ex { if !half_done && tok > 0.0 { half_done = true; continue } }
                if tok <= 1e-12 { return Some(RealTrade { ret: (proceeds - fees) / Q - 1.0, entry_px, exits, failed_exits: failed, fees_sol: fees, held_into_rug: false }) }
            }
            None => failed += 1,
        }
    }
    // never got out: anything still held is sold at the last real sell large enough, else worth 0
    let last = s.iter().rev().find(|w| !w.buy && w.tok >= tok);
    if let Some(w) = last { proceeds += tok * rpx(w); fees += fee_at(s, w.t).max(0.0) }
    Some(RealTrade { ret: (proceeds - fees) / Q - 1.0, entry_px, exits, failed_exits: failed, fees_sol: fees, held_into_rug: rug.is_finite() })
}

// ---------------- execution against verified pool math (C5) with real fees and real failure rates ----------------
/// SOL received for selling `tok` tokens into the pool state AFTER swap `w` (= state our tx meets if it lands next).
/// PumpSwap: constant product on the vault reserves with the swap's fee rate (C5: median error 1e-7).
/// DAMM v2: concentrated-liquidity step with L estimated from the latest swap (C5: median error 5e-6); 0 if L unknown.
pub fn sell_into(w: &Sw, l_est: f64, tok: f64) -> f64 {
    if w.sqrt > 0.0 {
        if !(l_est > 0.0) { return 0.0 }
        let inp = tok * 10f64.powi(w.dec_tok) * (1.0 - w.fee);
        let s0 = w.sqrt;
        let out_raw = if w.sol_is_a {
            // token is b: s1 = s0 + in/L, SOL (a) out = L (1/s0 - 1/s1)
            let s1 = s0 + inp / l_est;
            l_est * (1.0 / s0 - 1.0 / s1)
        } else {
            // token is a: 1/s1 = 1/s0 + in/L, SOL (b) out = L (s0 - s1)
            let s1 = 1.0 / (1.0 / s0 + inp / l_est);
            l_est * (s0 - s1)
        };
        (out_raw / 1e9).max(0.0)
    } else {
        let (l, t) = (w.liq.max(1e-12), w.liq.max(1e-12) / w.px);
        ((l - l * t / (t + tok)) * (1.0 - w.fee)).max(0.0)
    }
}
/// tokens received for `sol` SOL bought into the pool state after swap `w`
pub fn buy_into(w: &Sw, l_est: f64, sol: f64) -> f64 {
    let x = sol * (1.0 - w.fee);
    if w.sqrt > 0.0 {
        if !(l_est > 0.0) { return 0.0 }
        let inp = x * 1e9;
        let s0 = w.sqrt;
        let out_raw = if w.sol_is_a { let s1 = 1.0 / (1.0 / s0 + inp / l_est); l_est * (s0 - s1) } else { let s1 = s0 + inp / l_est; l_est * (1.0 / s0 - 1.0 / s1) };
        (out_raw / 10f64.powi(w.dec_tok)).max(0.0)
    } else {
        let (l, t) = (w.liq.max(1e-12), w.liq.max(1e-12) / w.px);
        (t - l * t / (l + x)).max(0.0)
    }
}

/// Take-profit plan: sell `frac` of the INITIAL position when the price reaches `mult` x entry; the remainder ("moon
/// bag") uses a trailing stop `trail` from its high and is closed at pool age `max_age` s; `rug_exit` closes everything
/// at the first LP removal we can react to (only helps if the removal is not the same transaction).
#[derive(Clone, Debug)]
pub struct Plan { pub tiers: Vec<(f64, f64)>, pub trail: f64, pub max_age: f64, pub mom: Option<Mom>,
    /// break-even stop: once the price reached (1 + be.0) x entry, sell everything if it falls back to be.1 x entry
    pub be: Option<(f64, f64)>,
    /// ratchet stop: once the high reached `start` x entry, sell everything when the price falls to `r` x that high
    /// (e.g. start 2, r 0.18: a coin that ran to 11x is sold if it falls back to ~2x)
    pub ratchet: Option<(f64, f64)> }

/// Momentum exit read from the live transaction flow (window `win` s, causal: swaps and failed txs up to now):
/// sell 100 % when the price is `dd` below its high since entry AND the flow fades (buy-SOL imbalance <= `imb_min` OR the
/// buy rate fell below `rate_frac` x its best rate since entry); emergency rug exit when the failed-tx share in the window
/// reaches `fail_max` (strongest rug feature in the data). Tiers are optional on top.
#[derive(Clone, Debug, PartialEq)]
pub struct Mom { pub win: f64, pub dd: f64, pub imb_min: f64, pub rate_frac: f64, pub fail_max: f64 }

#[derive(Clone, Debug, Default)]
pub struct Exec { pub ret: f64, pub sells: u32, pub failed_attempts: u32, pub fees: f64, pub peak_mult: f64, pub rugged_while_holding: bool,
    /// every simulated sell: (index of the landing swap, tokens sold, SOL received, closed before the LP removal?)
    pub fills: Vec<(usize, f64, f64, bool)>, pub entry_px: f64, pub closed_by_last_state: bool,
    /// seconds from our entry to our last sell (how long the position was held)
    pub hold_s: f64 }

/// deterministic per-attempt failure draw (real failure rates measured on chain: sells 0.298, buys 0.159 in the first
/// 5 min — reports/rebuild/explore/honeypot.json)
fn fails(pool_salt: u64, attempt: u64, rate: f64) -> bool {
    let mut x = pool_salt ^ attempt.wrapping_mul(0x9E3779B97F4A7C15);
    x ^= x >> 33; x = x.wrapping_mul(0xff51afd7ed558ccd); x ^= x >> 33; x = x.wrapping_mul(0xc4ceb9fe1a85ec53); x ^= x >> 33;
    (x >> 11) as f64 / (1u64 << 53) as f64 <= rate
}
pub const FAIL_SELL: f64 = 0.298;
pub const FAIL_BUY: f64 = 0.159;

/// Replay: decide at t0 + tau; every tx lands >= LAT_SLOTS later and executes against the pool state just before the
/// landing swap; each attempt may fail with the real on-chain failure rate (then retried at the next landing slot);
/// fee per tx = pool median meta.fee at that time.
pub fn trade_amm(p: &Pool, tau: f64, plan: &Plan, seed: u64) -> Option<Exec> { trade_amm_ext(p, tau, plan, seed, f64::INFINITY) }

/// Same engine with an external exit signal: from time `exit_at` on (e.g. the first snapshot where the exit model fired)
/// the whole remaining position is sold (still with latency, real fees and failure retries).
pub fn trade_amm_ext(p: &Pool, tau: f64, plan: &Plan, seed: u64, exit_at: f64) -> Option<Exec> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
    if rug <= t0 + tau { return None }
    let salt = seed ^ p.mint.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
    let mut attempt = 0u64;
    // pool state our tx meets when it lands at swap j: the state after swap j-1, EXCEPT when an LP removal happened in
    // between (a removal is not a swap): then the first post-removal swap state is used (drained reserves) - bug fix 2026-11-09
    let pre_state = |j: usize| -> usize { if s[j].t >= rug && (j == 0 || s[j - 1].t < rug) { j } else { j - 1 } };
    let land = |from_idx: usize| -> Option<usize> { let sl = s[from_idx].slot; s.iter().enumerate().skip(from_idx + 1).find(|(_, w)| w.slot >= sl + LAT_SLOTS).map(|(k, _)| k) };
    // DAMM L estimate: latest swap with a known L, never crossing the LP removal (post-removal states use only
    // post-removal L) - fix 2026-11-09
    let l_at = |k: usize| -> f64 { let after = s[k].t >= rug; s[..=k].iter().rev().take_while(|w| !after || w.t >= rug).find(|w| w.l > 0.0).map_or(0.0, |w| w.l) };
    // entry
    let dec = s.iter().rposition(|w| w.t <= t0 + tau)?;
    // no later swap to land on: our buy is the only trade; we can only sell back into the same state -> exact
    // round-trip cost (causal: the coin is not dropped just because it went quiet) - fix 2026-11-09
    let Some(mut k) = land(dec) else {
        let st = &s[dec];
        let tk = buy_into(st, l_at(dec), Q);
        let back = if tk > 0.0 { sell_into(st, l_at(dec), tk) } else { 0.0 };
        let fee = fee_at(s, st.t).max(0.0) * 2.0;
        return Some(Exec { ret: (back - fee) / Q - 1.0, entry_px: if tk > 0.0 { Q / tk } else { 0.0 }, ..Default::default() });
    };
    let mut ex = Exec::default();
    loop {
        if s[k].t >= rug || s[k].t > t0 + tau + FILL_WINDOW { return None }
        attempt += 1;
        let fee = fee_at(s, s[k].t);
        if !fails(salt, attempt, FAIL_BUY) { ex.fees += fee.max(0.0); break }
        ex.failed_attempts += 1;
        ex.fees += fee.max(0.0);
        k = land(k)?;
    }
    let pre = pre_state(k);
    let tok0 = buy_into(&s[pre], l_at(pre), Q);
    if !(tok0 > 0.0) { return None }
    let entry_px = Q / tok0;
    let mut tok = tok0;
    let mut proceeds = 0.0;
    let mut done_tiers = vec![false; plan.tiers.len()];
    let mut moon_hi = 0.0f64;
    let mut be_hit = false;
    let mut j = k;
    let mut pending: Option<f64> = None; // tokens we are trying to sell
    let mut peak = s[k].px;
    // momentum window state (from the entry on)
    let (mut lo, mut wb, mut ws, mut wbn) = (k + 1, 0.0f64, 0.0f64, 0usize);
    let mut best_rate = 0.0f64;
    while j + 1 < s.len() && tok > 1e-12 {
        j += 1;
        let w = &s[j];
        peak = peak.max(w.px);
        if let Some(amt) = pending {
            // our sell lands at swap j (if j is far enough from the decision); executes against state after j-1
            attempt += 1;
            ex.fees += fee_at(s, w.t).max(0.0);
            if fails(salt, attempt, FAIL_SELL) {
                // failed: the retry also needs LAT_SLOTS more slots - fix 2026-11-09
                ex.failed_attempts += 1;
                let sl = w.slot;
                while j + 1 < s.len() && s[j + 1].slot < sl + LAT_SLOTS { j += 1 }
                continue;
            }
            let st = pre_state(j);
            let got = sell_into(&s[st], l_at(st), amt);
            proceeds += got;
            tok -= amt;
            ex.sells += 1;
            ex.fills.push((j, amt, got, w.t < rug));
            ex.hold_s = w.t - s[k].t;
            pending = None;
            continue;
        }
        if w.t >= rug { ex.rugged_while_holding = true }
        let mult = w.px / entry_px;
        if let Some((trig, lvl)) = plan.be { if peak / entry_px >= 1.0 + trig && mult <= lvl { be_hit = true } }
        if let Some((start, r)) = plan.ratchet { if peak / entry_px >= start && w.px <= peak * r { be_hit = true } }
        // tiers (fraction of the INITIAL position)
        let mut want = 0.0;
        for (i, (m, fr)) in plan.tiers.iter().enumerate() {
            if !done_tiers[i] && mult >= *m { done_tiers[i] = true; want += fr * tok0 }
        }
        let tiers_left = done_tiers.iter().any(|d| !d);
        if !tiers_left || plan.tiers.is_empty() {
            moon_hi = moon_hi.max(w.px);
            if w.px <= moon_hi * (1.0 - plan.trail) { want = tok }
        }
        if let Some(m) = &plan.mom {
            if w.buy { wb += w.sol; wbn += 1 } else { ws += w.sol }
            while lo < j && s[lo].t <= w.t - m.win { if s[lo].buy { wb -= s[lo].sol; wbn -= 1 } else { ws -= s[lo].sol } lo += 1 }
            let rate = wbn as f64 / m.win;
            if w.t - s[k].t >= m.win { best_rate = best_rate.max(rate) }
            let imb = (wb - ws) / (wb + ws + 1e-12);
            let dd = 1.0 - w.px / peak;
            let fade = imb <= m.imb_min || (best_rate > 0.0 && rate < m.rate_frac * best_rate);
            let nf = p.failed.partition_point(|t| *t <= w.t) - p.failed.partition_point(|t| *t <= w.t - m.win);
            let fshare = nf as f64 / (nf + (j + 1 - lo)) as f64;
            if (dd >= m.dd && fade) || fshare >= m.fail_max { want = tok }
        }
        if w.t - t0 >= plan.max_age { want = tok }
        if w.t >= exit_at { want = tok }
        if be_hit { want = tok }
        if want > 0.0 {
            let amt = want.min(tok);
            // wait until the landing slot: skip swaps in the same/next slot
            let sl = w.slot;
            while j + 1 < s.len() && s[j + 1].slot < sl + LAT_SLOTS { j += 1 }
            pending = Some(amt);
        }
    }
    // never sold everything: the rest is sold into the last observed state
    if tok > 1e-12 {
        ex.closed_by_last_state = true;
        let lastk = s.len() - 1;
        // still holding when the LP was removed and no swap happened afterwards: the pool is drained -> worth 0 (bug fix)
        if !(rug.is_finite() && s[lastk].t < rug) { proceeds += sell_into(&s[lastk], l_at(lastk), tok) }
        ex.fees += fee_at(s, s[lastk].t).max(0.0);
        if rug.is_finite() { ex.rugged_while_holding = true }
    }
    ex.ret = (proceeds - ex.fees) / Q - 1.0;
    ex.peak_mult = peak / entry_px;
    ex.entry_px = entry_px;
    Some(ex)
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    fn sw(t: f64, slot: u64, buy: bool, px: f64, liq: f64) -> Sw {
        Sw { t, slot, buy, sol: 0.2, trader: "x".into(), px, liq, fee: 0.01, tok: 0.2 / px, txfee: 0.00002, sqrt: 0.0, sol_is_a: false, dec_tok: 6, l: 0.0, sig: String::new() }
    }
    fn base(n: usize) -> Pool {
        let mut p = Pool::default();
        p.mint = "M".into();
        for i in 0..n { p.swaps.push(sw(1000.0 + i as f64, 100 + i as u64 * 3, true, 1e-6, 10.0)) }
        p
    }
    /// holding through an LP removal with no swap afterwards must be worth 0 (no pre-rug price)
    #[test]
    fn held_into_rug_without_later_swaps_is_worth_zero() {
        let mut p = base(40);
        p.lp_out.push((1039.5, "remove_all_liquidity".into()));
        let plan = Plan { tiers: vec![(100.0, 0.5)], trail: 0.99, max_age: 1e9, mom: None, be: None, ratchet: None };
        let e = trade_amm(&p, 5.0, &plan, 0).unwrap();
        assert!(e.ret <= -0.99, "ret {} should be a total loss", e.ret);
    }
    /// a sell landing after an LP removal uses the drained post-removal state, not the pre-removal reserves
    #[test]
    fn sell_after_rug_uses_drained_state() {
        let mut p = base(30);
        for i in 30..40 { p.swaps.push(sw(1000.0 + i as f64, 100 + i as u64 * 3, false, 1e-6, 0.01)) }
        p.lp_out.push((1029.5, "remove_all_liquidity".into()));
        let plan = Plan { tiers: vec![], trail: 1.0, max_age: 31.0, mom: None, be: None, ratchet: None };
        let e = trade_amm(&p, 5.0, &plan, 0).unwrap();
        assert!(e.ret < -0.9, "ret {} should reflect the drained pool", e.ret);
    }
    /// a coin with no swap after the decision is not dropped: it gives a small round-trip loss
    #[test]
    fn quiet_coin_gives_round_trip_cost() {
        let p = base(5);
        let plan = Plan { tiers: vec![(2.0, 0.5)], trail: 0.3, max_age: 120.0, mom: None, be: None, ratchet: None };
        let e = trade_amm(&p, 60.0, &plan, 0).unwrap();
        assert!(e.ret < 0.0 && e.ret > -0.1, "ret {}", e.ret);
    }
}

#[cfg(test)]
mod momentum_tests {
    use super::*;
    /// price runs up with buyers, then sellers take over and the price falls: the momentum exit must sell before the
    /// end of the data and keep a profit; holding to the end (no momentum exit) must do worse
    /// break-even stop: +10 % then a slow fall to -60 % -> the BE plan exits near the entry, holding loses much more
    #[test]
    fn break_even_stop_limits_the_loss() {
        let mut p = Pool::default();
        p.mint = "M".into();
        let mut px = 1e-6;
        for i in 0..120u64 {
            px *= if i < 20 { 1.006 } else { 0.99 };
            p.swaps.push(Sw { t: 1000.0 + i as f64, slot: 100 + i * 3, buy: i < 20, sol: 0.3, trader: format!("w{i}"), px, liq: 50.0, fee: 0.01, tok: 0.3 / px, txfee: 0.00002, sqrt: 0.0, sol_is_a: false, dec_tok: 6, l: 0.0, sig: String::new() });
        }
        let be = Plan { tiers: vec![], trail: 1.0, max_age: 1e9, mom: None, be: Some((0.05, 1.0)), ratchet: None };
        let hold = Plan { tiers: vec![], trail: 1.0, max_age: 1e9, mom: None, be: None, ratchet: None };
        let (a, b) = (trade_amm(&p, 5.0, &be, 0).unwrap(), trade_amm(&p, 5.0, &hold, 0).unwrap());
        assert!(a.ret > -0.1, "BE ret {}", a.ret);
        assert!(a.ret > b.ret + 0.3, "BE {} vs hold {}", a.ret, b.ret);
    }
    #[test]
    fn momentum_exit_sells_when_flow_fades() {
        let mut p = Pool::default();
        p.mint = "M".into();
        let mut px = 1e-6;
        for i in 0..200u64 {
            let up = i < 100;
            px *= if up { 1.01 } else { 0.985 };
            p.swaps.push(Sw { t: 1000.0 + i as f64, slot: 100 + i * 3, buy: up, sol: 0.3, trader: format!("w{i}"), px, liq: 20.0, fee: 0.01, tok: 0.3 / px, txfee: 0.00002, sqrt: 0.0, sol_is_a: false, dec_tok: 6, l: 0.0, sig: String::new() });
        }
        let mom = Plan { tiers: vec![], trail: 1.0, max_age: 1e9, mom: Some(Mom { win: 10.0, dd: 0.1, imb_min: 0.0, rate_frac: 0.5, fail_max: 1.1 }), be: None, ratchet: None };
        let hold = Plan { tiers: vec![], trail: 1.0, max_age: 1e9, mom: None, be: None, ratchet: None };
        let (a, b) = (trade_amm(&p, 5.0, &mom, 0).unwrap(), trade_amm(&p, 5.0, &hold, 0).unwrap());
        assert!(a.ret > 0.5, "momentum exit ret {}", a.ret);
        assert!(a.ret > b.ret, "momentum {} should beat hold {}", a.ret, b.ret);
    }
}
