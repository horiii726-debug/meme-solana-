//! Lapisan ML/DL adaptif (walk-forward per hari): CatBoost (program C++ resmi) + MLP Rust, skor = rata-rata keduanya.
//! Setiap hari uji, kedua model dilatih ulang HANYA dengan baris dari hari-hari sebelumnya yang hasilnya sudah
//! selesai sebelum hari uji dimulai (purge). Ambang skor ditentukan dari data latih, bukan dari hari uji.
use crate::{Ev, Koin, JENDELA};
use std::collections::{HashMap, HashSet};
use std::io::Write;

pub const NAMA_FITUR: [&str; 22] = [
    "umur", "pembeli60", "pembeli_sebelum60", "tumbuh_pembeli", "baru", "beli_sol60", "jual_sol60", "rasio_bs",
    "n_beli60", "n_jual60", "penjual60", "top", "riil", "naik_sejak_lahir", "naik60", "dev_jual", "bundle", "n_trade",
    "n_dompet", "dev_pct", "top10_pct", "lulus",
];

/// Semua fitur pada waktu t, HANYA dari event dengan waktu <= t (tes `fitur_ml_kausal`).
pub fn fitur_ml(k: &Koin, t: f64) -> Vec<f64> {
    let ev: Vec<&Ev> = k.ev.iter().filter(|e| e.t <= t).collect();
    let w = |a: f64, b: f64| ev.iter().filter(move |e| e.t > a && e.t <= b);
    let pembeli: HashSet<&str> = w(t - JENDELA, t).filter(|e| e.beli).map(|e| e.user.as_str()).collect();
    let sebelum: HashSet<&str> = w(t - 2.0 * JENDELA, t - JENDELA).filter(|e| e.beli).map(|e| e.user.as_str()).collect();
    let penjual: HashSet<&str> = w(t - JENDELA, t).filter(|e| !e.beli).map(|e| e.user.as_str()).collect();
    let lama: HashSet<&str> = ev.iter().filter(|e| e.t <= t - JENDELA).map(|e| e.user.as_str()).collect();
    let baru = if pembeli.is_empty() { 0.0 } else { pembeli.iter().filter(|u| !lama.contains(*u)).count() as f64 / pembeli.len() as f64 };
    let (mut bs, mut js, mut nb, mut nj) = (0.0, 0.0, 0.0, 0.0);
    let mut per: HashMap<&str, f64> = HashMap::new();
    for e in w(t - JENDELA, t) {
        if e.beli {
            bs += e.sol;
            nb += 1.0;
            *per.entry(e.user.as_str()).or_insert(0.0) += e.sol;
        } else {
            js += e.sol;
            nj += 1.0;
        }
    }
    let top = if bs > 0.0 { per.values().cloned().fold(0.0, f64::max) / bs } else { 1.0 };
    let px_now = ev.last().map_or(0.0, |e| e.px);
    let px_awal = ev.first().map_or(0.0, |e| e.px);
    let px_60 = ev.iter().rev().find(|e| e.t <= t - JENDELA).map_or(px_awal, |e| e.px);
    // kepemilikan dari aliran trade (token beli - jual per dompet); suplai Pump.fun 1 miliar
    let mut pegang: HashMap<&str, f64> = HashMap::new();
    for e in &ev {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        *pegang.entry(e.user.as_str()).or_insert(0.0) += if e.beli { tok } else { -tok };
    }
    let dev_pct = pegang.get(k.creator.as_str()).cloned().unwrap_or(0.0).max(0.0) / 1e9;
    let mut v: Vec<f64> = pegang.values().cloned().filter(|x| *x > 0.0).collect();
    v.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let top10 = v.iter().take(10).sum::<f64>() / 1e9;
    let dompet: HashSet<&str> = ev.iter().map(|e| e.user.as_str()).collect();
    let bundle = ev.iter().filter(|e| e.beli && e.slot == k.slot_lahir && e.user != k.creator).map(|e| e.user.as_str()).collect::<HashSet<_>>().len();
    vec![
        t - k.lahir,
        pembeli.len() as f64,
        sebelum.len() as f64,
        (pembeli.len() as f64 + 1.0) / (sebelum.len() as f64 + 1.0),
        baru,
        bs,
        js,
        (bs + 1e-3) / (js + 1e-3),
        nb,
        nj,
        penjual.len() as f64,
        top,
        ev.last().map_or(0.0, |e| e.riil),
        if px_awal > 0.0 { px_now / px_awal } else { 0.0 },
        if px_60 > 0.0 { px_now / px_60 } else { 0.0 },
        if ev.iter().any(|e| !e.beli && e.user == k.creator) { 1.0 } else { 0.0 },
        bundle as f64,
        ev.len() as f64,
        dompet.len() as f64,
        dev_pct,
        top10,
        if ev.iter().any(|e| e.amm) { 1.0 } else { 0.0 },
    ]
}

pub struct Baris {
    pub hari: i64,
    pub mint: String,
    pub t: f64,
    pub keluar: f64,
    pub x: Vec<f64>,
    /// candidate features (research only, NOT used by V2/V3 models)
    pub xk: Vec<f64>,
    pub ret: f64,
    /// ML label: ret > 1 by default; with LABEL_ROKET=x: the price peaks at >= x times the fill price within 30 min
    pub y: bool,
}

/// Candidate quantitative features (research). Each one is tested alone before it may join the model.
pub const NAMA_KANDIDAT: [&str; 66] = [
    "ofi30", "ofi60", "ofi300", "ofi_accel", "buyvol_accel", "intensity_ratio", "logret30", "logret300", "vol60",
    "efficiency60", "hhi_buy60", "unique_buy_ratio", "flip300", "median_buy60", "small_buy_share60", "dist_peak",
    "vwap300_dev", "kyle_lambda60", "holders", "holder_growth60", "since_big_sell",
    // from the literature (arXiv 2602.14860 Pump.fun graduation; arXiv 2602.13480 MemeTrans; arXiv 2308.14235 momentum change)
    "sol_per_trade", "early10_hold_pct", "mom_change", "direct_buy_share60",
    // trader-flow / bot map (measured on real trades): speed, same-slot clusters, whales, size tail, repeat buyers, snipers
    "trade_gap_med60", "same_slot_share60", "whale_buy_share60", "buy_p90_60", "repeat_buyer_share60", "sniper_hold_pct",
    "time_to_10_buyers",
    // manipulation detectors (arXiv 2609.10246 Meme Coin Factories; arXiv 2601.08641 bump bots)
    "bump_share300", "wash_pairs300",
    // raw-data features (price path, holder cost basis, money flow, context)
    "ath_mult", "since_ath", "trend_slope300", "trend_r2_300", "local_peaks300", "supply_in_profit", "holders_in_profit",
    "profit_taking60", "top10_flow60", "max_buys_slot60", "sell_size_trend", "fresh_sol_share60", "hour_utc",
    // sniper / dev / early-holder behaviour and flow microstructure
    "dev_init_buy", "dev_sold_frac", "early_exit_share", "retention_m1", "seller_hold_med60", "impact_asym60",
    "ofi_autocorr120", "buy_streak60",
    // moonshot features: SOL inflow speed into the curve, progress to graduation, holder spreading, buyer growth
    "curve_velocity120", "curve_progress", "top10_change300", "buyer_growth300",
    // cross-coin, filled in main from coins that FINISHED before t (causal): smart wallets, creator history
    "smart_buyers", "smart_buy_share", "creator_prev_coins",
    // cross-coin: wallets with t-stat > 1.645 on finished coins (arXiv 2601.08641); copycat name/symbol (arXiv 2609.10246)
    "smart_t_buyers", "copycat", "global_launch10m", "global_trades60",
];

/// Candidate features at time t, using ONLY events with time <= t (test `fitur_kandidat_kausal`).
pub fn fitur_kandidat(k: &Koin, t: f64) -> Vec<f64> {
    let ev: Vec<&Ev> = k.ev.iter().filter(|e| e.t <= t).collect();
    let win = |a: f64, b: f64| ev.iter().filter(move |e| e.t > a && e.t <= b);
    // order-flow imbalance (SOL): (buy - sell) / (buy + sell), 0 when no flow
    let ofi = |a: f64, b: f64| {
        let (mut bs, mut ss) = (0.0, 0.0);
        for e in win(a, b) {
            if e.beli { bs += e.sol } else { ss += e.sol }
        }
        if bs + ss > 0.0 { (bs - ss) / (bs + ss) } else { 0.0 }
    };
    let buy_sol = |a: f64, b: f64| win(a, b).filter(|e| e.beli).map(|e| e.sol).sum::<f64>();
    let px_at = |s: f64| ev.iter().rev().find(|e| e.t <= s).or(ev.first()).map_or(0.0, |e| e.px);
    let px_now = ev.last().map_or(0.0, |e| e.px);
    let lr = |a: f64, b: f64| if a > 0.0 && b > 0.0 { (b / a).ln() } else { 0.0 };
    // per-trade log returns inside the last 60 s
    let w60: Vec<&&Ev> = win(t - 60.0, t).collect();
    let mut rets = Vec::new();
    let mut prev = px_at(t - 60.0);
    for e in &w60 {
        rets.push(lr(prev, e.px));
        prev = e.px;
    }
    let mean_r = if rets.is_empty() { 0.0 } else { rets.iter().sum::<f64>() / rets.len() as f64 };
    let vol60 = if rets.len() < 2 { 0.0 } else { (rets.iter().map(|r| (r - mean_r).powi(2)).sum::<f64>() / (rets.len() - 1) as f64).sqrt() };
    let path = rets.iter().map(|r| r.abs()).sum::<f64>();
    let eff = if path > 0.0 { lr(px_at(t - 60.0), px_now).abs() / path } else { 0.0 };
    // buy concentration and sizes in the last 60 s
    let mut per: HashMap<&str, f64> = HashMap::new();
    let mut buys: Vec<f64> = Vec::new();
    for e in w60.iter().filter(|e| e.beli) {
        *per.entry(e.user.as_str()).or_insert(0.0) += e.sol;
        buys.push(e.sol);
    }
    let tot: f64 = buys.iter().sum();
    let hhi = if tot > 0.0 { per.values().map(|v| (v / tot).powi(2)).sum() } else { 1.0 };
    let uniq = if buys.is_empty() { 0.0 } else { per.len() as f64 / buys.len() as f64 };
    buys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if buys.is_empty() { 0.0 } else { buys[buys.len() / 2] };
    let small = if buys.is_empty() { 0.0 } else { buys.iter().filter(|s| **s < 0.1).count() as f64 / buys.len() as f64 };
    // wallets that both bought and sold in the last 5 min (flipping / wash)
    let (mut b5, mut s5): (HashSet<&str>, HashSet<&str>) = (HashSet::new(), HashSet::new());
    for e in win(t - 300.0, t) {
        if e.beli { b5.insert(e.user.as_str()); } else { s5.insert(e.user.as_str()); }
    }
    let all5 = b5.union(&s5).count();
    let flip = if all5 > 0 { b5.intersection(&s5).count() as f64 / all5 as f64 } else { 0.0 };
    let peak = ev.iter().map(|e| e.px).fold(0.0, f64::max);
    let (mut pv, mut v) = (0.0, 0.0);
    for e in win(t - 300.0, t) {
        pv += e.px * e.sol;
        v += e.sol;
    }
    let vwap_dev = if v > 0.0 && pv > 0.0 { px_now / (pv / v) - 1.0 } else { 0.0 };
    // Kyle's lambda: OLS slope of per-trade log return on signed SOL flow (price impact per SOL)
    let flows: Vec<f64> = w60.iter().map(|e| if e.beli { e.sol } else { -e.sol }).collect();
    let kyle = if flows.len() < 3 {
        0.0
    } else {
        let mf = flows.iter().sum::<f64>() / flows.len() as f64;
        let cov: f64 = flows.iter().zip(&rets).map(|(f, r)| (f - mf) * (r - mean_r)).sum();
        let var: f64 = flows.iter().map(|f| (f - mf).powi(2)).sum();
        if var > 0.0 { cov / var } else { 0.0 }
    };
    // holders = wallets with positive net token balance (from trade flow)
    let holders_at = |s: f64| {
        let mut bal: HashMap<&str, f64> = HashMap::new();
        for e in ev.iter().filter(|e| e.t <= s) {
            let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
            *bal.entry(e.user.as_str()).or_insert(0.0) += if e.beli { tok } else { -tok };
        }
        bal.values().filter(|b| **b > 1.0).count() as f64
    };
    let h_now = holders_at(t);
    let since_big = ev.iter().rev().find(|e| !e.beli && e.riil > 0.0 && e.sol >= 0.02 * e.riil).map_or(600.0, |e| (t - e.t).min(600.0));
    // Marino et al.: fewer trades needed to fill the curve = stronger (real SOL in curve per trade so far)
    let sol_per_trade = ev.last().map_or(0.0, |e| e.riil) / (ev.len().max(1) as f64);
    // MemeTrans: current holdings of the first 10 distinct buyers, share of 1e9 supply
    let mut first10: Vec<&str> = Vec::new();
    for e in ev.iter().filter(|e| e.beli && e.user != k.creator) {
        if first10.len() >= 10 {
            break;
        }
        if !first10.contains(&e.user.as_str()) {
            first10.push(e.user.as_str());
        }
    }
    let mut bal10 = 0.0;
    for e in ev.iter().filter(|e| first10.contains(&e.user.as_str())) {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        bal10 += if e.beli { tok } else { -tok };
    }
    let early10 = bal10.max(0.0) / 1e9;
    // momentum change: last-30 s log return minus the previous 30 s log return
    let mom_change = lr(px_at(t - 30.0), px_now) - lr(px_at(t - 60.0), px_at(t - 30.0));
    // share of buys in 60 s sent as plain "buy" (exact token amount; typical of bots/direct contract calls) [U: proxy]
    let nb60 = w60.iter().filter(|e| e.beli).count();
    let direct = if nb60 > 0 { w60.iter().filter(|e| e.beli && e.ix == 0).count() as f64 / nb60 as f64 } else { 0.0 };
    // --- trader-flow / bot measures (last 60 s unless stated) ---
    // speed: median seconds between consecutive trades (60 when fewer than 2 trades)
    let mut gaps: Vec<f64> = w60.windows(2).map(|p| p[1].t - p[0].t).collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let gap_med = if gaps.is_empty() { 60.0 } else { gaps[gaps.len() / 2] };
    // bot clusters: share of trades sitting in a slot that holds trades from >= 2 different wallets
    let mut slot_w: HashMap<u64, HashSet<&str>> = HashMap::new();
    for e in &w60 {
        slot_w.entry(e.slot).or_default().insert(e.user.as_str());
    }
    let same_slot = if w60.is_empty() { 0.0 } else { w60.iter().filter(|e| slot_w[&e.slot].len() >= 2).count() as f64 / w60.len() as f64 };
    // size: share of buy SOL from buys >= 1 SOL, and 90th percentile buy size
    let whale = if tot > 0.0 { w60.iter().filter(|e| e.beli && e.sol >= 1.0).map(|e| e.sol).sum::<f64>() / tot } else { 0.0 };
    let p90 = if buys.is_empty() { 0.0 } else { buys[((buys.len() as f64 * 0.9) as usize).min(buys.len() - 1)] };
    // repeat buyers: share of 60 s buyers with >= 3 buys in the window (bots / wash)
    let mut cnt: HashMap<&str, usize> = HashMap::new();
    for e in w60.iter().filter(|e| e.beli) {
        *cnt.entry(e.user.as_str()).or_insert(0) += 1;
    }
    let repeat = if cnt.is_empty() { 0.0 } else { cnt.values().filter(|c| **c >= 3).count() as f64 / cnt.len() as f64 };
    // snipers: wallets whose first buy came within 3 s of birth; their current holdings as share of supply
    let snipers: HashSet<&str> = ev.iter().filter(|e| e.beli && e.t <= k.lahir + 3.0 && e.user != k.creator).map(|e| e.user.as_str()).collect();
    let mut sb = 0.0;
    for e in ev.iter().filter(|e| snipers.contains(e.user.as_str())) {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        sb += if e.beli { tok } else { -tok };
    }
    let sniper = sb.max(0.0) / 1e9;
    // wave speed: seconds from birth until 10 distinct buyers (age so far if not reached)
    let mut seen10: HashSet<&str> = HashSet::new();
    let mut t10 = t - k.lahir;
    for e in ev.iter().filter(|e| e.beli) {
        seen10.insert(e.user.as_str());
        if seen10.len() >= 10 {
            t10 = e.t - k.lahir;
            break;
        }
    }
    // --- manipulation detectors over the last 300 s ---
    // bump bot (arXiv 2601.08641 Alg. 3, simplified): a wallet whose consecutive opposite-direction trades have equal size
    // (+-1%) at least 3 times while its net position stays under 5% of its gross volume; feature = share of trades by them
    // wash pair (arXiv 2609.10246 WT2): same wallet buys and sells the same token amount (+-1%) within 60 s
    let w300: Vec<&&Ev> = win(t - 300.0, t).collect();
    let mut per_w: HashMap<&str, Vec<(f64, bool, f64)>> = HashMap::new();
    for e in &w300 {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        per_w.entry(e.user.as_str()).or_default().push((e.t, e.beli, tok));
    }
    let (mut bump_trades, mut wash_pairs) = (0usize, 0usize);
    for v in per_w.values() {
        let same = |a: f64, b: f64| a > 0.0 && (a - b).abs() <= 0.01 * a;
        let f = v.windows(2).filter(|p| p[0].1 != p[1].1 && same(p[0].2, p[1].2)).count();
        wash_pairs += v.windows(2).filter(|p| p[0].1 != p[1].1 && same(p[0].2, p[1].2) && p[1].0 - p[0].0 <= 60.0).count();
        let gross: f64 = v.iter().map(|x| x.2).sum();
        let net: f64 = v.iter().map(|x| if x.1 { x.2 } else { -x.2 }).sum();
        if f >= 3 && net.abs() <= 0.05 * gross {
            bump_trades += v.len();
        }
    }
    let bump = if w300.is_empty() { 0.0 } else { bump_trades as f64 / w300.len() as f64 };
    let wash = if w300.is_empty() { 0.0 } else { wash_pairs as f64 / w300.len() as f64 };
    // --- raw-data features ---
    // price path: ATH multiple from first price, seconds since ATH (cap 600)
    let first_px = ev.first().map_or(0.0, |e| e.px);
    let (mut ath, mut ath_t) = (0.0f64, t);
    for e in &ev {
        if e.px >= ath {
            ath = e.px;
            ath_t = e.t;
        }
    }
    let ath_mult = if first_px > 0.0 { ath / first_px } else { 0.0 };
    let since_ath = (t - ath_t).min(600.0);
    // trend over the last 300 s: OLS of ln(price) on minutes, slope and R^2; local peaks = choppiness
    let pts: Vec<(f64, f64)> = win(t - 300.0, t).filter(|e| e.px > 0.0).map(|e| ((e.t - t) / 60.0, e.px.ln())).collect();
    let (slope, r2) = if pts.len() >= 3 {
        let n = pts.len() as f64;
        let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
        let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
        let syy: f64 = pts.iter().map(|p| (p.1 - my).powi(2)).sum();
        if sxx > 0.0 && syy > 0.0 { (sxy / sxx, sxy * sxy / (sxx * syy)) } else { (0.0, 0.0) }
    } else {
        (0.0, 0.0)
    };
    let peaks = pts.windows(3).filter(|w| w[1].1 > w[0].1 && w[1].1 > w[2].1).count() as f64;
    // holder cost basis from trade flow: tokens and SOL spent per wallet (average cost); in profit = avg cost < price now
    let mut cost: HashMap<&str, (f64, f64)> = HashMap::new(); // (tokens, SOL cost of those tokens)
    let mut ptake_n = (0usize, 0usize);
    for e in &ev {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        let c = cost.entry(e.user.as_str()).or_insert((0.0, 0.0));
        if e.beli {
            c.0 += tok;
            c.1 += e.sol;
        } else {
            let avg = if c.0 > 0.0 { c.1 / c.0 } else { 0.0 };
            if e.t > t - 60.0 {
                ptake_n.1 += 1;
                if avg > 0.0 && e.px > avg {
                    ptake_n.0 += 1;
                }
            }
            let take = tok.min(c.0.max(0.0));
            c.1 -= avg * take;
            c.0 -= take;
        }
    }
    let holders_v: Vec<(f64, f64)> = cost.values().filter(|c| c.0 > 1.0).cloned().collect();
    let tot_tok: f64 = holders_v.iter().map(|c| c.0).sum();
    let in_profit = |c: &(f64, f64)| c.0 > 0.0 && c.1 / c.0 < px_now;
    let sup_profit = if tot_tok > 0.0 { holders_v.iter().filter(|c| in_profit(c)).map(|c| c.0).sum::<f64>() / tot_tok } else { 0.0 };
    let hold_profit = if holders_v.is_empty() { 0.0 } else { holders_v.iter().filter(|c| in_profit(c)).count() as f64 / holders_v.len() as f64 };
    let ptake = if ptake_n.1 > 0 { ptake_n.0 as f64 / ptake_n.1 as f64 } else { 0.0 };
    // top-10 holders (by tokens now) net token flow in the last 60 s, as share of 1e9 supply (accumulation > 0)
    let mut top: Vec<(&str, f64)> = cost.iter().map(|(w, c)| (*w, c.0)).collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top10: HashSet<&str> = top.iter().take(10).map(|x| x.0).collect();
    let top10_flow = w60.iter().filter(|e| top10.contains(e.user.as_str())).map(|e| {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        if e.beli { tok } else { -tok }
    }).sum::<f64>() / 1e9;
    // burst: most buys landing in one slot in the last 60 s
    let mut per_slot: HashMap<u64, usize> = HashMap::new();
    for e in w60.iter().filter(|e| e.beli) {
        *per_slot.entry(e.slot).or_insert(0) += 1;
    }
    let max_slot = per_slot.values().cloned().max().unwrap_or(0) as f64;
    // sell size trend: median sell 0-30 s ago / median sell 30-90 s ago (sell exhaustion < 1)
    let med_sell = |a: f64, b: f64| {
        let mut v: Vec<f64> = win(a, b).filter(|e| !e.beli).map(|e| e.sol).collect();
        v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        v.get(v.len() / 2).cloned()
    };
    let sell_trend = match (med_sell(t - 30.0, t), med_sell(t - 90.0, t - 30.0)) {
        (Some(a), Some(b)) if b > 0.0 => a / b,
        _ => 1.0,
    };
    // fresh money: buy SOL in the last 60 s from wallets never seen before t - 60
    let seen_before: HashSet<&str> = ev.iter().filter(|e| e.t <= t - 60.0).map(|e| e.user.as_str()).collect();
    let fresh = if tot > 0.0 { w60.iter().filter(|e| e.beli && !seen_before.contains(e.user.as_str())).map(|e| e.sol).sum::<f64>() / tot } else { 0.0 };
    // --- sniper / dev / early-holder behaviour ---
    let dev_buys: Vec<&&Ev> = ev.iter().filter(|e| e.user == k.creator && e.beli).collect();
    let dev_init = dev_buys.first().map_or(0.0, |e| e.sol);
    let (dev_b, dev_s) = ev.iter().filter(|e| e.user == k.creator).fold((0.0, 0.0), |a, e| {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        if e.beli { (a.0 + tok, a.1) } else { (a.0, a.1 + tok) }
    });
    let dev_sold = if dev_b > 0.0 { (dev_s / dev_b).min(1.0) } else { 0.0 };
    // early buyers (first 30 s, not creator): share that already sold >= 95% of what they bought
    let mut bal_all: HashMap<&str, (f64, f64, f64)> = HashMap::new(); // (bought tok, sold tok, first buy time)
    for e in &ev {
        let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
        let b = bal_all.entry(e.user.as_str()).or_insert((0.0, 0.0, f64::INFINITY));
        if e.beli {
            b.0 += tok;
            b.2 = b.2.min(e.t);
        } else {
            b.1 += tok;
        }
    }
    let early: Vec<&str> = ev.iter().filter(|e| e.beli && e.t <= k.lahir + 30.0 && e.user != k.creator).map(|e| e.user.as_str()).collect::<HashSet<_>>().into_iter().collect();
    let early_exit = if early.is_empty() { 0.0 } else { early.iter().filter(|w| bal_all.get(*w).is_some_and(|b| b.0 > 0.0 && b.1 >= 0.95 * b.0)).count() as f64 / early.len() as f64 };
    let m1: Vec<&str> = ev.iter().filter(|e| e.beli && e.t <= k.lahir + 60.0).map(|e| e.user.as_str()).collect::<HashSet<_>>().into_iter().collect();
    let retention = if m1.is_empty() { 0.0 } else { m1.iter().filter(|w| bal_all.get(*w).is_some_and(|b| b.1 < 0.5 * b.0)).count() as f64 / m1.len() as f64 };
    // median seconds between a seller's first buy and its sell, sells in the last 60 s (cap 3600)
    let mut hold: Vec<f64> = w60.iter().filter(|e| !e.beli).filter_map(|e| bal_all.get(e.user.as_str()).filter(|b| b.2.is_finite()).map(|b| (e.t - b.2).clamp(0.0, 3600.0))).collect();
    hold.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let seller_hold = hold.get(hold.len() / 2).cloned().unwrap_or(3600.0);
    // price impact asymmetry: mean |log return| per SOL on buys vs on sells in the last 60 s (> 1 = buys move price more)
    let (mut ib, mut nb_, mut is_, mut ns_) = (0.0, 0.0, 0.0, 0.0);
    for (e, r) in w60.iter().zip(&rets) {
        if e.sol > 0.0 {
            if e.beli { ib += r.abs() / e.sol; nb_ += 1.0 } else { is_ += r.abs() / e.sol; ns_ += 1.0 }
        }
    }
    let asym = if nb_ > 0.0 && ns_ > 0.0 && is_ > 0.0 { (ib / nb_) / (is_ / ns_) } else { 1.0 };
    // persistence of order flow: lag-1 autocorrelation of 10 s OFI buckets over the last 120 s
    let buckets: Vec<f64> = (0..12).map(|i| ofi(t - 120.0 + 10.0 * i as f64, t - 110.0 + 10.0 * i as f64)).collect();
    let mb = buckets.iter().sum::<f64>() / 12.0;
    let (num, den) = buckets.windows(2).fold((0.0, 0.0), |a, w| (a.0 + (w[0] - mb) * (w[1] - mb), a.1));
    let den = buckets.iter().map(|x| (x - mb).powi(2)).sum::<f64>() + den;
    let autocorr = if den > 0.0 { num / den } else { 0.0 };
    // longest run of consecutive buys in the last 60 s
    let (mut streak, mut cur) = (0.0f64, 0.0f64);
    for e in &w60 {
        if e.beli { cur += 1.0; streak = streak.max(cur) } else { cur = 0.0 }
    }
    // --- moonshot features ---
    let riil_at = |s: f64| ev.iter().rev().find(|e| e.t <= s).map_or(0.0, |e| e.riil);
    let riil_now = ev.last().map_or(0.0, |e| e.riil);
    let curve_vel = (riil_now - riil_at(t - 120.0)) / 2.0; // SOL per minute over the last 2 min
    let progress = riil_now / 85.0; // Pump.fun curve graduates at about 85 real SOL
    let top10_share = |s: f64| {
        let mut bal: HashMap<&str, f64> = HashMap::new();
        for e in ev.iter().filter(|e| e.t <= s) {
            let tok = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
            *bal.entry(e.user.as_str()).or_insert(0.0) += if e.beli { tok } else { -tok };
        }
        let mut v: Vec<f64> = bal.values().cloned().filter(|x| *x > 0.0).collect();
        v.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let tot: f64 = v.iter().sum();
        if tot > 0.0 { v.iter().take(10).sum::<f64>() / tot } else { 1.0 }
    };
    let top10_chg = top10_share(t) - top10_share(t - 300.0); // < 0 = holders spreading out
    let ub = |a: f64, b: f64| win(a, b).filter(|e| e.beli).map(|e| e.user.as_str()).collect::<HashSet<_>>().len() as f64;
    let buyer_growth = (ub(t - 300.0, t) + 1.0) / (ub(t - 600.0, t - 300.0) + 1.0);
    let n15 = win(t - 15.0, t).count() as f64;
    let n60 = w60.len() as f64;
    vec![
        ofi(t - 30.0, t),
        ofi(t - JENDELA, t),
        ofi(t - 300.0, t),
        ofi(t - 30.0, t) - ofi(t - 60.0, t - 30.0),
        (buy_sol(t - 30.0, t) + 1e-3) / (buy_sol(t - 60.0, t - 30.0) + 1e-3),
        (4.0 * n15 + 1.0) / (n60 + 1.0),
        lr(px_at(t - 30.0), px_now),
        lr(px_at(t - 300.0), px_now),
        vol60,
        eff,
        hhi,
        uniq,
        flip,
        median,
        small,
        if peak > 0.0 { px_now / peak } else { 0.0 },
        vwap_dev,
        kyle,
        h_now,
        h_now - holders_at(t - 60.0),
        since_big,
        sol_per_trade,
        early10,
        mom_change,
        direct,
        gap_med,
        same_slot,
        whale,
        p90,
        repeat,
        sniper,
        t10,
        bump,
        wash,
        ath_mult,
        since_ath,
        slope,
        r2,
        peaks,
        sup_profit,
        hold_profit,
        ptake,
        top10_flow,
        max_slot,
        sell_trend,
        fresh,
        (t / 3600.0).rem_euclid(24.0),
        dev_init,
        dev_sold,
        early_exit,
        retention,
        seller_hold,
        asym,
        autocorr,
        streak,
        curve_vel,
        progress,
        top10_chg,
        buyer_growth,
    ]
}

/// CatBoost lewat program C++ resmi: latih (Logloss, label ret > 1) lalu skor. Gagal = None (tidak ada trade ML).
pub fn catboost(bin: &str, kerja: &str, latih: &[&Baris], uji: &[Vec<f64>]) -> Option<(Vec<f64>, Vec<f64>)> {
    // unique scratch dir per process: parallel runs used the same /tmp/sol_* names and overwrote/deleted each other's
    // CatBoost files (found 7 Nov 2026: "CatBoost gagal" and mixed predictions in concurrent runs)
    let kerja_unik = format!("{kerja}_{}", std::process::id());
    let kerja = kerja_unik.as_str();
    std::fs::create_dir_all(kerja).ok()?;
    let tulis = |p: &str, rows: &mut dyn Iterator<Item = (f64, &Vec<f64>)>| -> Option<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(p).ok()?);
        for (y, x) in rows {
            let s: Vec<String> = std::iter::once(format!("{y}")).chain(x.iter().enumerate().filter(|(i, _)| !dibuang(*i)).map(|(_, v)| if v.is_finite() { format!("{v}") } else { "0".to_string() })).collect();
            writeln!(f, "{}", s.join("\t")).ok()?;
        }
        Some(())
    };
    tulis(&format!("{kerja}/latih.tsv"), &mut latih.iter().map(|b| (if b.y { 1.0 } else { 0.0 }, &b.x)))?;
    tulis(&format!("{kerja}/uji.tsv"), &mut uji.iter().map(|x| (0.0, x)))?;
    tulis(&format!("{kerja}/latih_x.tsv"), &mut latih.iter().map(|b| (0.0, &b.x)))?;
    std::fs::write(format!("{kerja}/cd"), "0\tLabel\n").ok()?;
    let ok = std::process::Command::new(bin)
        .args(["fit", "--learn-set", &format!("{kerja}/latih.tsv"), "--column-description", &format!("{kerja}/cd"), "--loss-function", "Logloss",
            "--iterations", "300", "--depth", "6", "--learning-rate", "0.05", "--random-seed", &(1 + crate::benih()).to_string(), "--model-file", &format!("{kerja}/m.cbm"),
            "--train-dir", kerja, "--logging-level", "Silent", "--allow-writing-files", "false"])
        .stdout(std::process::Stdio::null()).status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_dir_all(kerja);
        return None;
    }
    let skor = |input: &str| -> Option<Vec<f64>> {
        let out = format!("{kerja}/pred_{}.tsv", input.replace('.', "_"));
        let ok = std::process::Command::new(bin)
            .args(["calc", "--model-file", &format!("{kerja}/m.cbm"), "--input-path", &format!("{kerja}/{input}"), "--column-description", &format!("{kerja}/cd"),
                "--output-path", &out, "--prediction-type", "Probability"])
            .stdout(std::process::Stdio::null()).status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let t = std::fs::read_to_string(&out).ok()?;
        // kolom terakhir = peluang kelas 1; baris pertama = judul
        Some(t.lines().skip(1).filter_map(|l| l.split('\t').last()?.parse().ok()).collect())
    };
    let hasil = (skor("latih_x.tsv"), skor("uji.tsv"));
    // remove this fold's CatBoost scratch files (they filled the disk: 2.3 GB over many runs)
    let _ = std::fs::remove_dir_all(kerja);
    Some((hasil.0?, hasil.1?))
}

/// splitmix64: RNG deterministik (benih tetap = hasil bisa diulang)
struct Acak(u64);
impl Acak {
    fn u(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// salinan baris latih dengan hasil (label) diacak antar baris (Fisher-Yates, benih tetap). Hanya untuk uji serangan.
pub fn acak_label(latih: &[&Baris], benih: u64) -> Vec<Baris> {
    let mut r = Acak(benih);
    let mut ret: Vec<(f64, bool)> = latih.iter().map(|b| (b.ret, b.y)).collect();
    for i in (1..ret.len()).rev() {
        let j = ((r.u() * (i + 1) as f64) as usize).min(i);
        ret.swap(i, j);
    }
    latih.iter().zip(ret).map(|(b, (rv, yv))| Baris { hari: b.hari, mint: b.mint.clone(), t: b.t, keluar: b.keluar, x: b.x.clone(), xk: b.xk.clone(), ret: rv, y: yv }).collect()
}

/// MLP n-32-16-1 (ReLU, sigmoid), Adam, standarisasi dari data latih saja. Kembali (skor latih, skor uji).
pub fn mlp(latih: &[&Baris], uji: &[Vec<f64>], benih: u64) -> (Vec<f64>, Vec<f64>) {
    let n = latih.first().map_or(0, |b| b.x.len());
    let m = latih.len().max(1) as f64;
    let mean: Vec<f64> = (0..n).map(|j| latih.iter().map(|b| b.x[j]).sum::<f64>() / m).collect();
    let sd: Vec<f64> = (0..n).map(|j| (latih.iter().map(|b| (b.x[j] - mean[j]).powi(2)).sum::<f64>() / m).sqrt().max(1e-9)).collect();
    let z = |x: &[f64]| -> Vec<f64> { x.iter().enumerate().map(|(j, v)| if dibuang(j) { 0.0 } else { ((v - mean[j]) / sd[j]).clamp(-5.0, 5.0) }).collect() };
    let lapis = [n, 32, 16, 1];
    let mut r = Acak(benih);
    let mut w: Vec<Vec<f64>> = (0..3).map(|l| (0..lapis[l] * lapis[l + 1]).map(|_| (r.u() * 2.0 - 1.0) * (6.0 / (lapis[l] + lapis[l + 1]) as f64).sqrt()).collect()).collect();
    let mut b: Vec<Vec<f64>> = (0..3).map(|l| vec![0.0; lapis[l + 1]]).collect();
    let (mut mw, mut vw) = (w.iter().map(|x| vec![0.0; x.len()]).collect::<Vec<_>>(), w.iter().map(|x| vec![0.0; x.len()]).collect::<Vec<_>>());
    let (mut mb, mut vb) = (b.iter().map(|x| vec![0.0; x.len()]).collect::<Vec<_>>(), b.iter().map(|x| vec![0.0; x.len()]).collect::<Vec<_>>());
    let maju = |w: &Vec<Vec<f64>>, b: &Vec<Vec<f64>>, x: &[f64]| -> Vec<Vec<f64>> {
        let mut a = vec![x.to_vec()];
        for l in 0..3 {
            let prev = &a[l];
            let mut o = vec![0.0; lapis[l + 1]];
            for (i, oi) in o.iter_mut().enumerate() {
                let mut s = b[l][i];
                for (j, pj) in prev.iter().enumerate() {
                    s += w[l][i * lapis[l] + j] * pj;
                }
                *oi = if l < 2 { s.max(0.0) } else { 1.0 / (1.0 + (-s).exp()) };
            }
            a.push(o);
        }
        a
    };
    let xs: Vec<Vec<f64>> = latih.iter().map(|b| z(&b.x)).collect();
    let ys: Vec<f64> = latih.iter().map(|b| if b.y { 1.0 } else { 0.0 }).collect();
    let mut idx: Vec<usize> = (0..xs.len()).collect();
    let (lr, l2, mut langkah) = (1e-3, 1e-4, 0i32);
    for _ in 0..30 {
        for i in (1..idx.len()).rev() {
            let j = (r.u() * (i + 1) as f64) as usize;
            idx.swap(i, j.min(i));
        }
        for batch in idx.chunks(64) {
            let mut gw: Vec<Vec<f64>> = w.iter().map(|x| vec![0.0; x.len()]).collect();
            let mut gb: Vec<Vec<f64>> = b.iter().map(|x| vec![0.0; x.len()]).collect();
            for &i in batch {
                let a = maju(&w, &b, &xs[i]);
                let mut d = vec![a[3][0] - ys[i]];
                for l in (0..3).rev() {
                    let mut dp = vec![0.0; lapis[l]];
                    for (o, dv) in d.iter().enumerate() {
                        gb[l][o] += dv;
                        for j in 0..lapis[l] {
                            gw[l][o * lapis[l] + j] += dv * a[l][j];
                            dp[j] += dv * w[l][o * lapis[l] + j];
                        }
                    }
                    if l > 0 {
                        d = dp.iter().zip(&a[l]).map(|(g, av)| if *av > 0.0 { *g } else { 0.0 }).collect();
                    }
                }
            }
            langkah += 1;
            let bs = batch.len() as f64;
            let (b1, b2) = (0.9f64, 0.999f64);
            let (c1, c2) = (1.0 - b1.powi(langkah), 1.0 - b2.powi(langkah));
            for l in 0..3 {
                for k in 0..w[l].len() {
                    let g = gw[l][k] / bs + l2 * w[l][k];
                    mw[l][k] = b1 * mw[l][k] + (1.0 - b1) * g;
                    vw[l][k] = b2 * vw[l][k] + (1.0 - b2) * g * g;
                    w[l][k] -= lr * (mw[l][k] / c1) / ((vw[l][k] / c2).sqrt() + 1e-8);
                }
                for k in 0..b[l].len() {
                    let g = gb[l][k] / bs;
                    mb[l][k] = b1 * mb[l][k] + (1.0 - b1) * g;
                    vb[l][k] = b2 * vb[l][k] + (1.0 - b2) * g * g;
                    b[l][k] -= lr * (mb[l][k] / c1) / ((vb[l][k] / c2).sqrt() + 1e-8);
                }
            }
        }
    }
    let p = |x: &Vec<f64>| maju(&w, &b, &z(x))[3][0];
    (latih.iter().map(|bb| p(&bb.x)).collect(), uji.iter().map(p).collect())
}

/// AUC (Mann-Whitney) untuk melaporkan akurasi di hari uji
pub fn auc(skor: &[f64], y: &[bool]) -> f64 {
    let mut v: Vec<(f64, bool)> = skor.iter().cloned().zip(y.iter().cloned()).collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (mut rank_pos, mut n_pos) = (0.0, 0.0);
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1].0 == v[i].0 {
            j += 1;
        }
        let r = (i + j) as f64 / 2.0 + 1.0;
        for item in &v[i..=j] {
            if item.1 {
                rank_pos += r;
                n_pos += 1.0;
            }
        }
        i = j + 1;
    }
    let n_neg = v.len() as f64 - n_pos;
    if n_pos == 0.0 || n_neg == 0.0 {
        return f64::NAN;
    }
    (rank_pos - n_pos * (n_pos + 1.0) / 2.0) / (n_pos * n_neg)
}

/// model input columns dropped by the active feature set (FITUR_V2); set once in main, empty = use all columns
pub static KOLOM_BUANG: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();
fn dibuang(i: usize) -> bool {
    KOLOM_BUANG.get().is_some_and(|v| v.contains(&i))
}
