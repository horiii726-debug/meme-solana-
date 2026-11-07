//! Full-transaction decoder for launchpads whose swap events do NOT carry the trader or the mint
//! (Raydium LaunchLab, Meteora DBC). The trader and mints come from the swap instruction's accounts (official IDLs in
//! /root/sol/idl). Events are taken from Anchor `emit_cpi` self-invocations (inner instruction data =
//! EVENT_CPI_TAG + event discriminator + payload), paired with the nearest preceding swap instruction of the SAME
//! program in execution order. Program-aware on purpose: LaunchLab's TradeEvent has the same discriminator as
//! Pump.fun's TradeEvent (both are sha256("event:TradeEvent")).
use crate::dekode::Baca;

pub const LAUNCHLAB: &str = "LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj";
pub const DBC: &str = "dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN";
const EVENT_CPI_TAG: [u8; 8] = [228, 69, 165, 46, 81, 203, 154, 29];
pub const D_LL_TRADE: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];
const LL_SWAP_IX: [[u8; 8]; 4] = [
    [250, 234, 13, 123, 213, 156, 19, 236], // buy_exact_in
    [24, 211, 116, 40, 105, 3, 153, 56],    // buy_exact_out
    [149, 39, 222, 155, 211, 124, 152, 26], // sell_exact_in
    [95, 200, 71, 34, 8, 9, 11, 166],       // sell_exact_out
];
pub const D_DBC_SWAP2: [u8; 8] = [189, 66, 51, 168, 38, 80, 117, 153];
pub const D_DBC_SWAP3: [u8; 8] = [187, 119, 248, 13, 175, 255, 64, 63];
const DBC_SWAP_IX: [[u8; 8]; 2] = [
    [248, 198, 158, 145, 225, 117, 135, 200], // swap
    [65, 75, 63, 76, 235, 91, 91, 136],       // swap2
];

/// one executed instruction (outer or inner), in execution order
pub struct Ix {
    pub prog: String,
    pub accounts: Vec<String>,
    pub data: Vec<u8>,
}

/// Flatten outer + inner instructions in execution order (outer i, then its inner instructions).
pub fn flatten(tx: &serde_json::Value) -> Vec<Ix> {
    let msg = &tx["transaction"]["message"];
    let mut keys: Vec<String> = msg["accountKeys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)).collect();
    for part in ["writable", "readonly"] {
        keys.extend(tx["meta"]["loadedAddresses"][part].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)));
    }
    let conv = |ix: &serde_json::Value| -> Option<Ix> {
        let prog = keys.get(ix["programIdIndex"].as_u64()? as usize)?.clone();
        let accounts = ix["accounts"].as_array()?.iter().filter_map(|a| keys.get(a.as_u64()? as usize).cloned()).collect();
        let data = bs58::decode(ix["data"].as_str()?).into_vec().ok()?;
        Some(Ix { prog, accounts, data })
    };
    let mut out = Vec::new();
    let inner = tx["meta"]["innerInstructions"].as_array().cloned().unwrap_or_default();
    for (i, ix) in msg["instructions"].as_array().into_iter().flatten().enumerate() {
        if let Some(x) = conv(ix) {
            out.push(x);
        }
        for grp in inner.iter().filter(|g| g["index"].as_u64() == Some(i as u64)) {
            for ix in grp["instructions"].as_array().into_iter().flatten() {
                if let Some(x) = conv(ix) {
                    out.push(x);
                }
            }
        }
    }
    out
}

fn decimals(tx: &serde_json::Value, mint: &str) -> Option<u32> {
    for side in ["postTokenBalances", "preTokenBalances"] {
        for b in tx["meta"][side].as_array().into_iter().flatten() {
            if b["mint"].as_str() == Some(mint) {
                return b["uiTokenAmount"]["decimals"].as_u64().map(|d| d as u32);
            }
        }
    }
    None
}

const WSOL: &str = "So11111111111111111111111111111111111111112";

/// LaunchLab TradeEvent payload (after the 8-byte discriminator), official raydium_launchpad IDL field order
fn ll_trade(b: &[u8], ix: &Ix, tx: &serde_json::Value) -> Option<serde_json::Value> {
    let mut r = Baca { b, o: 8 };
    let pool = r.kunci()?;
    let _total_base_sell = r.u64()?;
    let (vb, vq) = (r.u64()?, r.u64()?);
    let (_rb0, _rq0, rb1, rq1) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let (amt_in, amt_out) = (r.u64()?, r.u64()?);
    let (pf, plf, cf, sf) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let dir = r.u8()?; // 0 = Buy, 1 = Sell
    let _status = r.u8()?;
    let _exact_in = r.u8()?;
    let (user, base_mint, quote_mint) = (ix.accounts.first()?.clone(), ix.accounts.get(9)?.clone(), ix.accounts.get(10)?.clone());
    let bd = 10f64.powi(decimals(tx, &base_mint).unwrap_or(6) as i32);
    let qd = 10f64.powi(decimals(tx, &quote_mint).unwrap_or(9) as i32);
    let buy = dir == 0;
    let fee = (pf + plf + cf + sf) as f64;
    // quote amount the trader paid (buy) or received (sell); token amount moved
    let (q, tok) = if buy { (amt_in as f64, amt_out as f64) } else { (amt_out as f64, amt_in as f64) };
    let gross = if buy { q } else { q + fee };
    Some(serde_json::json!({"k": "trade", "dex": "launchlab", "pool": pool, "mint": base_mint, "user": user, "buy": buy,
        "sol": q / qd, "tok": tok / bd, "quote": if quote_mint == WSOL { "11111111111111111111111111111111".to_string() } else { quote_mint },
        // constant-product curve state after the trade: (virtual + real quote) * (virtual - real base) = k
        "vsol": (vq + rq1) as f64 / qd, "vtok": (vb.saturating_sub(rb1)) as f64 / bd, "rsol": rq1 as f64 / qd,
        "fee_bps": if gross > 0.0 { (1e4 * fee / gross).round() } else { 0.0 }, "cfee_bps": 0}))
}

/// Meteora DBC EvtSwap2 payload (official dynamic_bonding_curve IDL field order)
fn dbc_swap2(b: &[u8], ix: &Ix, tx: &serde_json::Value) -> Option<serde_json::Value> {
    let mut r = Baca { b, o: 8 };
    let pool = r.kunci()?;
    let _config = r.kunci()?;
    let dir = r.u8()?; // TradeDirection: 0 = BaseToQuote (sell), 1 = QuoteToBase (buy)
    let _ref = r.u8()?;
    let (_a0, _a1, _mode) = (r.u64()?, r.u64()?, r.u8()?);
    let (inc_in, exc_in, _left, out) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let lo = r.u64()? as u128;
    let hi = r.u64()? as u128;
    let sqrt = (hi << 64) | lo;
    let (tf, pf, rf) = (r.u64()?, r.u64()?, r.u64()?);
    let quote_reserve = r.u64()?;
    let _threshold = r.u64()?;
    // swap accounts: [0] pool_authority [1] config [2] pool ... [7] base_mint [8] quote_mint [9] payer
    let (user, base_mint, quote_mint) = (ix.accounts.get(9)?.clone(), ix.accounts.get(7)?.clone(), ix.accounts.get(8)?.clone());
    let bdec = decimals(tx, &base_mint).unwrap_or(6) as i32;
    let qdec = decimals(tx, &quote_mint).unwrap_or(9) as i32;
    let buy = dir == 1;
    let fee = (tf + pf + rf) as f64;
    // Fee side depends on the pool config: if included == excluded input, no fee was taken from the input, so the fee
    // was taken from the OUTPUT token (verified on real swaps: buy fees in base units on such pools).
    let fee_on_input = inc_in > exc_in;
    let (q, tok) = if buy { (inc_in as f64, out as f64) } else { (out as f64, inc_in as f64) };
    let fee_bps = if fee_on_input { 1e4 * fee / (inc_in as f64).max(1.0) } else { 1e4 * fee / (out as f64 + fee).max(1.0) };
    // price from sqrt price Q64.64 (quote raw per base raw), converted to SOL per token
    let s = sqrt as f64 / 18446744073709551616.0;
    let px = s * s * 10f64.powi(bdec - qdec);
    // Local constant-liquidity reserves (the DBC curve is piecewise concentrated liquidity): from one swap,
    // dy/dx = s0 * s1 gives the start sqrt price s0, and L = dy / |s1 - s0|. Virtual reserves at the new price:
    // quote = L * s1, base = L / s1 (raw units). Lets the backtest fill orders with x*y=k on the current segment.
    // dx, dy = base and quote that actually moved through the curve (fees removed on whichever side they were taken).
    let out_gross = if fee_on_input { out as f64 } else { out as f64 + fee };
    let (dx, dy) = if buy { (out_gross, exc_in as f64) } else { (exc_in as f64, out_gross) };
    let s0 = if dx > 0.0 && s > 0.0 { (dy / dx) / s } else { 0.0 };
    let l = if (s - s0).abs() > 1e-12 * s { dy / (s - s0).abs() } else { 0.0 };
    let (vq, vb) = if l > 0.0 { (l * s / 10f64.powi(qdec), l / s / 10f64.powi(bdec)) } else { (0.0, 0.0) };
    Some(serde_json::json!({"k": "trade", "dex": "dbc", "pool": pool, "mint": base_mint, "user": user, "buy": buy,
        "sol": q / 10f64.powi(qdec), "tok": tok / 10f64.powi(bdec),
        "quote": if quote_mint == WSOL { "11111111111111111111111111111111".to_string() } else { quote_mint },
        "vsol": vq, "vtok": vb,
        "px": px, "sqrt": sqrt.to_string(), "rsol": quote_reserve as f64 / 10f64.powi(qdec),
        "fee_bps": fee_bps.round(), "cfee_bps": 0}))
}

/// All LaunchLab / DBC trades in one transaction (failed transactions give nothing).
pub fn trades(tx: &serde_json::Value) -> Vec<serde_json::Value> {
    if !tx["meta"]["err"].is_null() {
        return vec![];
    }
    let flat = flatten(tx);
    let mut out = Vec::new();
    let mut last_swap: Option<usize> = None;
    for (i, ix) in flat.iter().enumerate() {
        let d8: Option<[u8; 8]> = ix.data.get(..8).and_then(|s| s.try_into().ok());
        let Some(d8) = d8 else { continue };
        if (ix.prog == LAUNCHLAB && LL_SWAP_IX.contains(&d8)) || ((ix.prog == DBC || ix.prog == DAMM2) && DBC_SWAP_IX.contains(&d8)) {
            last_swap = Some(i);
            continue;
        }
        // emit_cpi event: self-invocation of the same program with the event-CPI tag
        if d8 != EVENT_CPI_TAG {
            continue;
        }
        let Some(s) = last_swap.filter(|s| flat[*s].prog == ix.prog) else { continue };
        let ev = &ix.data[8..];
        let e8: Option<[u8; 8]> = ev.get(..8).and_then(|s| s.try_into().ok());
        let e = match (ix.prog.as_str(), e8) {
            (LAUNCHLAB, Some(D_LL_TRADE)) => ll_trade(ev, &flat[s], tx),
            (DBC, Some(D_DBC_SWAP2)) => dbc_swap2(ev, &flat[s], tx),
            (DAMM2, Some(D_DBC_SWAP2)) => damm2_swap(ev, &flat[s], tx),
            _ => None,
        };
        if let Some(e) = e {
            out.push(e);
            last_swap = None;
        }
    }
    out
}

const LL_INIT_IX: [[u8; 8]; 3] = [
    [175, 175, 109, 31, 13, 152, 155, 237], // initialize
    [67, 153, 175, 39, 218, 16, 38, 32],    // initialize_v2
    [37, 190, 126, 222, 44, 154, 171, 17],  // initialize_with_token_2022
];
const DBC_INIT_IX: [[u8; 8]; 3] = [
    [140, 85, 215, 176, 102, 54, 104, 79],  // initialize_virtual_pool_with_spl_token
    [169, 118, 51, 78, 145, 110, 220, 155], // initialize_virtual_pool_with_token2022
    [182, 13, 233, 177, 42, 145, 135, 2],   // initialize_virtual_pool_with_token2022_transfer_hook
];

/// Pool (coin) creations in one transaction, from the initialize instruction accounts (official IDLs):
/// LaunchLab: [1] creator [5] pool_state [6] base_mint [7] quote_mint; DBC: [2] creator [3] base_mint [4] quote_mint [5] pool.
pub fn creates(tx: &serde_json::Value) -> Vec<serde_json::Value> {
    if !tx["meta"]["err"].is_null() {
        return vec![];
    }
    let mut out = Vec::new();
    for ix in flatten(tx) {
        let Some(d8) = ix.data.get(..8).and_then(|s| <[u8; 8]>::try_from(s).ok()) else { continue };
        let pick = |c: usize, p: usize, b: usize, q: usize, dex: &str| -> Option<serde_json::Value> {
            let quote = ix.accounts.get(q)?.clone();
            Some(serde_json::json!({"k": "create", "dex": dex, "creator": ix.accounts.get(c)?, "pool": ix.accounts.get(p)?,
                "mint": ix.accounts.get(b)?, "quote": if quote == WSOL { "11111111111111111111111111111111".to_string() } else { quote }}))
        };
        let e = if ix.prog == LAUNCHLAB && LL_INIT_IX.contains(&d8) {
            pick(1, 5, 6, 7, "launchlab")
        } else if ix.prog == DBC && DBC_INIT_IX.contains(&d8) {
            pick(2, 5, 3, 4, "dbc")
        } else {
            None
        };
        out.extend(e);
    }
    out
}

pub const DAMM2: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";

/// Meteora DAMM v2 EvtSwap2 (official cp_amm IDL): pool, trade_direction u8 (0 = A->B, 1 = B->A), collect_fee_mode u8,
/// has_referral bool, SwapParameters2 {u64, u64, u8}, SwapResult2 {inc_in, exc_in, left, out, next_sqrt u128,
/// claiming, protocol, compounding, referral fees}, ... Swap accounts: [6] token_a_mint [7] token_b_mint [8] payer.
/// The meme coin is whichever side is NOT wrapped SOL; pools without SOL are skipped.
fn damm2_swap(b: &[u8], ix: &Ix, tx: &serde_json::Value) -> Option<serde_json::Value> {
    let mut r = Baca { b, o: 8 };
    let pool = r.kunci()?;
    let dir = r.u8()?;
    let (_mode, _ref) = (r.u8()?, r.u8()?);
    let (_a0, _a1, _sm) = (r.u64()?, r.u64()?, r.u8()?);
    let (inc_in, exc_in, _left, out) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let lo = r.u64()? as u128;
    let hi = r.u64()? as u128;
    let sqrt = (hi << 64) | lo;
    let (f1, f2, f3, f4) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let (a_mint, b_mint, user) = (ix.accounts.get(6)?.clone(), ix.accounts.get(7)?.clone(), ix.accounts.get(8)?.clone());
    let (meme, sol_is_b) = if b_mint == WSOL { (a_mint.clone(), true) } else if a_mint == WSOL { (b_mint.clone(), false) } else { return None };
    let (adec, bdec) = (decimals(tx, &a_mint).unwrap_or(9) as i32, decimals(tx, &b_mint).unwrap_or(9) as i32);
    let (mdec, sdec) = if sol_is_b { (adec, bdec) } else { (bdec, adec) };
    // input token: A when dir == 0. Buy = SOL goes in.
    let input_is_a = dir == 0;
    let buy = input_is_a != sol_is_b;
    let fee = (f1 + f2 + f3 + f4) as f64;
    let fee_on_input = inc_in > exc_in;
    let out_gross = if fee_on_input { out as f64 } else { out as f64 + fee };
    // amounts in meme / SOL raw units
    let (q, tok) = if buy { (inc_in as f64, out as f64) } else { (out as f64, inc_in as f64) };
    let (dx, dy) = if buy { (out_gross, exc_in as f64) } else { (exc_in as f64, out_gross) };
    // sqrt price Q64.64 = sqrt(B raw per A raw); convert to SOL per meme raw
    let s_ab = sqrt as f64 / 18446744073709551616.0;
    let s = if sol_is_b { s_ab } else if s_ab > 0.0 { 1.0 / s_ab } else { 0.0 };
    let px = s * s * 10f64.powi(mdec - sdec);
    let s0 = if dx > 0.0 && s > 0.0 { (dy / dx) / s } else { 0.0 };
    let l = if (s - s0).abs() > 1e-12 * s { dy / (s - s0).abs() } else { 0.0 };
    let (vq, vb) = if l > 0.0 { (l * s / 10f64.powi(sdec), l / s / 10f64.powi(mdec)) } else { (0.0, 0.0) };
    let fee_bps = if fee_on_input { 1e4 * fee / (inc_in as f64).max(1.0) } else { 1e4 * fee / out_gross.max(1.0) };
    Some(serde_json::json!({"k": "trade", "dex": "damm2", "pool": pool, "mint": meme, "user": user, "buy": buy,
        "sol": q / 10f64.powi(sdec), "tok": tok / 10f64.powi(mdec), "quote": "11111111111111111111111111111111",
        "vsol": vq, "vtok": vb, "px": px, "rsol": vq, "fee_bps": fee_bps.round(), "cfee_bps": 0, "amm": true}))
}
