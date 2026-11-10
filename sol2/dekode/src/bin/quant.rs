//! Quantitative strategy research on decoded on-chain swaps (no candles, no chart indicators).
//!   quant <raw_file>...
//! Features at decision time tau after t0 (first swap), causal (only swaps with t <= t0 + tau):
//!   ret      ln(p_tau / p_0)
//!   ofi      order-flow imbalance (B - S) / (B + S) in SOL            — Cont, Kukanov & Stoikov (2014), J. Fin. Econometrics
//!   kyle     OLS slope of d ln p on signed SOL flow (price impact)     — Kyle (1985), Econometrica; Hasbrouck (2007)
//!   amihud   mean |d ln p| / SOL volume                                — Amihud (2002), J. Financial Markets
//!   vpin     sum |B_k - S_k| / sum V_k over 20 equal-volume buckets   — Easley, Lopez de Prado & O'Hara (2012), RFS
//!   hawkes   arrival-rate ratio last quarter of tau vs whole tau (self-excitation proxy) — Bacry, Mastromatteo & Muzy (2015)
//!   hhi      Herfindahl of buy SOL by wallet; buyers = distinct buying wallets
//!   rv       realized volatility sqrt(sum d ln p^2)
//!   liq, dliq  SOL reserve and ln change since t0; lock = LP lock events so far; devsold = creator sold
//!   hmm      filtered P(state with highest mean return) from a 3-state Gaussian HMM on 5 s bins of (return, OFI),
//!            fitted by Baum-Welch on TRAIN pools only, forward filter only (causal) — Hamilton (1989); Rabiner (1989)
//! Trade simulation: buy Q SOL at the first swap after t0 + tau, constant-product fill on the post-swap reserves with the
//! swap's own fee; exits walked swap by swap; an LP removal values the rest at the next observed price (0 if none).
//! Train = earliest 60 % of pools by t0, test = latest 40 % (walk-forward in time). Rules chosen on train only.
use dekode::series::*;
use serde_json::json;
use std::collections::{HashMap, HashSet};

const TAUS: [f64; 6] = [5.0, 10.0, 15.0, 30.0, 60.0, 120.0];
const FEATS: [&str; 19] = ["ret", "ofi", "kyle", "amihud", "vpin", "hawkes", "hhi", "buyers", "rv", "liq", "dliq", "lock", "devsold", "hmm", "rate", "buy_sol", "sell_sol", "top1", "mc"];

// ---------------- HMM (diagonal Gaussian, 2-D observations) ----------------
#[derive(Clone)]
struct Hmm { k: usize, pi: Vec<f64>, a: Vec<Vec<f64>>, mu: Vec<[f64; 2]>, var: Vec<[f64; 2]> }

fn gauss(x: &[f64; 2], m: &[f64; 2], v: &[f64; 2]) -> f64 {
    let mut p = 1.0;
    for d in 0..2 { p *= (-(x[d] - m[d]).powi(2) / (2.0 * v[d])).exp() / (2.0 * std::f64::consts::PI * v[d]).sqrt() }
    p.max(1e-300)
}

impl Hmm {
    fn init(seqs: &[Vec<[f64; 2]>], k: usize) -> Hmm {
        let mut r: Vec<f64> = seqs.iter().flatten().map(|x| x[0]).collect();
        r.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let all: Vec<&[f64; 2]> = seqs.iter().flatten().collect();
        let (m1, v1) = mv(all.iter().map(|x| x[1]));
        let (_, v0) = mv(all.iter().map(|x| x[0]));
        // distinct starting means: return quantiles 10/50/90 % of NON-ZERO bins, OFI spread around its mean
        let nz: Vec<f64> = r.iter().cloned().filter(|x| x.abs() > 1e-12).collect();
        let q = |p: f64| if nz.is_empty() { 0.0 } else { nz[((nz.len() - 1) as f64 * p) as usize] };
        let mu = (0..k).map(|i| { let p = 0.1 + 0.8 * i as f64 / (k - 1).max(1) as f64; [q(p), m1 + (p - 0.5) * 2.0 * v1.sqrt()] }).collect();
        Hmm { k, pi: vec![1.0 / k as f64; k], a: (0..k).map(|i| (0..k).map(|j| if i == j { 0.8 } else { 0.2 / (k - 1) as f64 }).collect()).collect(),
            mu, var: vec![[v0.max(1e-8), v1.max(1e-8)]; k] }
    }
    /// scaled forward pass; returns filtered posteriors (causal) and log-likelihood
    fn forward(&self, x: &[[f64; 2]]) -> (Vec<Vec<f64>>, f64) {
        let mut out = vec![];
        let mut ll = 0.0;
        let mut prev: Vec<f64> = vec![];
        for (t, o) in x.iter().enumerate() {
            let mut cur: Vec<f64> = (0..self.k).map(|j| {
                let prior = if t == 0 { self.pi[j] } else { (0..self.k).map(|i| prev[i] * self.a[i][j]).sum() };
                prior * gauss(o, &self.mu[j], &self.var[j])
            }).collect();
            let c: f64 = cur.iter().sum::<f64>().max(1e-300);
            ll += c.ln();
            for v in cur.iter_mut() { *v /= c }
            out.push(cur.clone());
            prev = cur;
        }
        (out, ll)
    }
    fn baum_welch(&mut self, seqs: &[Vec<[f64; 2]>], iters: usize) -> f64 {
        let mut ll_tot = 0.0;
        for _ in 0..iters {
            let k = self.k;
            let (mut a_num, mut a_den) = (vec![vec![0.0; k]; k], vec![0.0; k]);
            let (mut g_sum, mut gx, mut gxx) = (vec![0.0; k], vec![[0.0; 2]; k], vec![[0.0; 2]; k]);
            let mut pi_acc = vec![0.0; k];
            ll_tot = 0.0;
            for x in seqs.iter().filter(|s| s.len() > 1) {
                let n = x.len();
                // scaled forward
                let mut alpha = vec![vec![0.0; k]; n];
                let mut c = vec![0.0; n];
                for t in 0..n {
                    for j in 0..k {
                        let prior = if t == 0 { self.pi[j] } else { (0..k).map(|i| alpha[t - 1][i] * self.a[i][j]).sum() };
                        alpha[t][j] = prior * gauss(&x[t], &self.mu[j], &self.var[j]);
                    }
                    c[t] = alpha[t].iter().sum::<f64>().max(1e-300);
                    for j in 0..k { alpha[t][j] /= c[t] }
                    ll_tot += c[t].ln();
                }
                let mut beta = vec![vec![1.0; k]; n];
                for t in (0..n - 1).rev() {
                    for i in 0..k {
                        beta[t][i] = (0..k).map(|j| self.a[i][j] * gauss(&x[t + 1], &self.mu[j], &self.var[j]) * beta[t + 1][j]).sum::<f64>() / c[t + 1];
                    }
                }
                for t in 0..n {
                    let g: Vec<f64> = (0..k).map(|j| alpha[t][j] * beta[t][j]).collect();
                    let s: f64 = g.iter().sum::<f64>().max(1e-300);
                    for j in 0..k {
                        let gj = g[j] / s;
                        if t == 0 { pi_acc[j] += gj }
                        g_sum[j] += gj;
                        for d in 0..2 { gx[j][d] += gj * x[t][d]; gxx[j][d] += gj * x[t][d] * x[t][d] }
                        if t < n - 1 { a_den[j] += gj }
                    }
                    if t < n - 1 {
                        let mut xi = vec![vec![0.0; k]; k];
                        let mut z = 0.0;
                        for i in 0..k { for j in 0..k {
                            xi[i][j] = alpha[t][i] * self.a[i][j] * gauss(&x[t + 1], &self.mu[j], &self.var[j]) * beta[t + 1][j];
                            z += xi[i][j];
                        } }
                        for i in 0..k { for j in 0..k { a_num[i][j] += xi[i][j] / z.max(1e-300) } }
                    }
                }
            }
            let ps: f64 = pi_acc.iter().sum::<f64>().max(1e-300);
            for j in 0..k {
                self.pi[j] = pi_acc[j] / ps;
                for i in 0..k { self.a[j][i] = a_num[j][i] / a_den[j].max(1e-300) }
                let rs: f64 = self.a[j].iter().sum::<f64>().max(1e-300);
                for i in 0..k { self.a[j][i] /= rs }
                for d in 0..2 {
                    let m = gx[j][d] / g_sum[j].max(1e-300);
                    self.mu[j][d] = m;
                    self.var[j][d] = (gxx[j][d] / g_sum[j].max(1e-300) - m * m).max(1e-8);
                }
            }
        }
        ll_tot
    }
}

fn mv(it: impl Iterator<Item = f64>) -> (f64, f64) {
    let v: Vec<f64> = it.collect();
    let m = v.iter().sum::<f64>() / v.len().max(1) as f64;
    (m, v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len().max(1) as f64)
}

/// 5 s bins from t0 up to `until` seconds: (log return over bin, OFI of bin)
fn bins(p: &Pool, until: f64) -> Vec<[f64; 2]> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let nb = (until / 5.0).ceil() as usize;
    let mut out = vec![];
    let (mut last_px, mut i) = (s[0].px, 0usize);
    for b in 0..nb {
        let end = t0 + (b as f64 + 1.0) * 5.0;
        let (mut bs, mut ss) = (0.0, 0.0);
        let start_px = last_px;
        while i < s.len() && s[i].t <= end {
            if s[i].buy { bs += s[i].sol } else { ss += s[i].sol }
            last_px = s[i].px;
            i += 1;
        }
        let r = (last_px / start_px).ln();
        out.push([if r.is_finite() { r } else { 0.0 }, if bs + ss > 0.0 { (bs - ss) / (bs + ss) } else { 0.0 }]);
    }
    out
}

fn features(p: &Pool, tau: f64, hmm: &Hmm, bull: usize, supply: &HashMap<String, f64>) -> HashMap<&'static str, f64> {
    let s = &p.swaps;
    let t0 = s[0].t;
    let x: Vec<&Sw> = s.iter().filter(|w| w.t <= t0 + tau).collect();
    let (b, sl): (f64, f64) = (x.iter().filter(|w| w.buy).map(|w| w.sol).sum(), x.iter().filter(|w| !w.buy).map(|w| w.sol).sum());
    let mut m = HashMap::new();
    let last = x.last().unwrap();
    m.insert("ret", (last.px / s[0].px).ln());
    m.insert("ofi", (b - sl) / (b + sl + 1e-12));
    // Kyle lambda: OLS through origin of dlnp on signed flow
    let (mut sxy, mut sxx, mut ami, mut na, mut rv2) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for w in x.windows(2) {
        let dl = (w[1].px / w[0].px).ln();
        if !dl.is_finite() { continue }
        let q = if w[1].buy { w[1].sol } else { -w[1].sol };
        sxy += dl * q;
        sxx += q * q;
        rv2 += dl * dl;
        if w[1].sol > 0.0 { ami += dl.abs() / w[1].sol; na += 1.0 }
    }
    m.insert("kyle", if sxx > 0.0 { sxy / sxx } else { 0.0 });
    m.insert("amihud", if na > 0.0 { ami / na } else { 0.0 });
    m.insert("rv", rv2.sqrt());
    // VPIN over 20 equal-volume buckets
    let vb = (b + sl) / 20.0;
    let (mut acc_b, mut acc_s, mut num, mut den) = (0.0, 0.0, 0.0, 0.0);
    if vb > 0.0 {
        for w in &x {
            let mut rem = w.sol;
            while rem > 0.0 {
                let room = vb - acc_b - acc_s;
                let take = rem.min(room);
                if w.buy { acc_b += take } else { acc_s += take }
                rem -= take;
                if acc_b + acc_s >= vb - 1e-12 { num += (acc_b - acc_s).abs(); den += acc_b + acc_s; acc_b = 0.0; acc_s = 0.0 }
            }
        }
    }
    m.insert("vpin", if den > 0.0 { num / den } else { 0.0 });
    let late = x.iter().filter(|w| w.t > t0 + 0.75 * tau).count() as f64 / (0.25 * tau);
    m.insert("hawkes", late / (x.len() as f64 / tau).max(1e-9));
    m.insert("rate", x.len() as f64 / tau);
    m.insert("buy_sol", b);
    m.insert("sell_sol", sl);
    // market cap in SOL = price x total supply (getTokenSupply, supply.tsv); NaN when unknown
    m.insert("mc", supply.get(&p.mint).map_or(f64::NAN, |s| last.px * s));
    let mut per: HashMap<&str, f64> = HashMap::new();
    for w in x.iter().filter(|w| w.buy) { *per.entry(&w.trader).or_insert(0.0) += w.sol }
    m.insert("top1", per.values().cloned().fold(0.0, f64::max) / b.max(1e-12));
    m.insert("hhi", per.values().map(|v| (v / b.max(1e-12)).powi(2)).sum());
    m.insert("buyers", per.len() as f64);
    m.insert("liq", last.liq);
    m.insert("dliq", (last.liq.max(1e-9) / s[0].liq.max(1e-9)).ln());
    m.insert("lock", p.lp_lock.iter().filter(|t| **t <= t0 + tau).count() as f64);
    m.insert("devsold", x.iter().any(|w| !w.buy && !p.creator.is_empty() && w.trader == p.creator) as u8 as f64);
    let (post, _) = hmm.forward(&bins(p, tau));
    m.insert("hmm", post.last().map_or(0.0, |v| v[bull]));
    m
}

fn stats(r: &[f64]) -> serde_json::Value {
    if r.is_empty() { return json!({"n": 0}) }
    let n = r.len() as f64;
    let mean = r.iter().sum::<f64>() / n;
    let mut v = r.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // bootstrap 95 % CI of the mean (deterministic LCG)
    let mut seed = 12345u64;
    let mut means: Vec<f64> = (0..1000).map(|_| (0..r.len()).map(|_| { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); r[(seed >> 33) as usize % r.len()] }).sum::<f64>() / n).collect();
    means.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({"n": r.len(), "mean_ret": (mean * 1000.0).round() / 1000.0, "median": (v[v.len() / 2] * 1000.0).round() / 1000.0,
        "win_rate": ((r.iter().filter(|x| **x > 0.0).count() as f64 / n) * 100.0).round() / 100.0,
        "ge_2x": r.iter().filter(|x| **x >= 1.0).count(), "ge_5x": r.iter().filter(|x| **x >= 4.0).count(),
        "best": (v[v.len() - 1] * 100.0).round() / 100.0, "ci95_mean": [(means[25] * 1000.0).round() / 1000.0, (means[974] * 1000.0).round() / 1000.0]})
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let pools = load_pools(&files);
    let supply: HashMap<String, f64> = std::fs::read_to_string("/root/sol/supply.tsv").unwrap_or_default().lines()
        .filter_map(|l| { let f: Vec<&str> = l.split('\t').collect(); Some((f.first()?.to_string(), f.get(1)?.parse().ok()?)) }).collect();
    let mut ids: Vec<(f64, String)> = pools.iter().filter(|(_, p)| p.swaps.len() >= 5 && p.swaps[0].liq > 0.0).map(|(k, p)| (p.swaps[0].t, k.clone())).collect();
    ids.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let cut = (ids.len() as f64 * 0.6) as usize;
    let (train, test): (Vec<String>, Vec<String>) = (ids[..cut].iter().map(|x| x.1.clone()).collect(), ids[cut..].iter().map(|x| x.1.clone()).collect());
    eprintln!("pools usable {} (train {}, test {})", ids.len(), train.len(), test.len());
    // HMM on train
    let seqs: Vec<Vec<[f64; 2]>> = train.iter().map(|k| bins(&pools[k], 600.0)).collect();
    let mut hmm = Hmm::init(&seqs, 3);
    let ll = hmm.baum_welch(&seqs, 30);
    let bull = (0..3).max_by(|a, b| hmm.mu[*a][0].partial_cmp(&hmm.mu[*b][0]).unwrap()).unwrap();
    println!("{}", json!({"hmm": {"loglik_train": ll, "state_mean_ret_5s": hmm.mu.iter().map(|m| m[0]).collect::<Vec<_>>(), "state_mean_ofi": hmm.mu.iter().map(|m| m[1]).collect::<Vec<_>>(),
        "transition": hmm.a, "bull_state": bull}}));
    let exits = [Exit::Hold, Exit::Trail(0.3), Exit::Trail(0.5), Exit::Ladder(2.0, 0.5), Exit::Ladder(3.0, 0.5), Exit::Ladder(5.0, 0.5), Exit::Time(300.0), Exit::Time(1800.0)];
    // features and returns for every (pool, tau)
    struct Row { feat: HashMap<&'static str, f64>, ret: Vec<Option<f64>> }
    let mut rows: HashMap<(String, usize), Row> = HashMap::new();
    for (k, p) in &pools {
        if p.swaps.len() < 5 || p.swaps[0].liq <= 0.0 { continue }
        for (ti, tau) in TAUS.iter().enumerate() {
            if p.swaps.last().unwrap().t < p.swaps[0].t + tau { continue }
            let feat = features(p, *tau, &hmm, bull, &supply);
            // liquidity floor at decision time (plan D11): LIQ_MIN SOL, default 1
            let liq_min: f64 = std::env::var("LIQ_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
            if feat["liq"] < liq_min { continue }
            let ret = exits.iter().map(|e| trade(p, *tau, *e)).collect();
            rows.insert((k.clone(), ti), Row { feat, ret });
        }
    }
    let rets = |set: &[String], ti: usize, ei: usize, filt: &dyn Fn(&HashMap<&'static str, f64>) -> bool| -> Vec<f64> {
        set.iter().filter_map(|k| rows.get(&(k.clone(), ti))).filter(|r| filt(&r.feat)).filter_map(|r| r.ret[ei]).collect()
    };
    // 1. baselines: buy every pool at tau
    for (ti, tau) in TAUS.iter().enumerate() {
        for (ei, e) in exits.iter().enumerate() {
            println!("{}", json!({"rule": "buy_all", "tau": tau, "exit": exit_name(e), "train": stats(&rets(&train, ti, ei, &|_| true)), "test": stats(&rets(&test, ti, ei, &|_| true))}));
        }
    }
    // 2. single-feature tercile filters chosen on TRAIN, evaluated on TEST
    let mut tried = 0usize;
    let mut best: Vec<(f64, String, serde_json::Value)> = vec![];
    for (ti, tau) in TAUS.iter().enumerate() {
        for f in FEATS {
            let mut vals: Vec<f64> = train.iter().filter_map(|k| rows.get(&(k.clone(), ti))).map(|r| r.feat[f]).filter(|x| x.is_finite()).collect();
            if vals.len() < 30 { continue }
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (lo, hi) = (vals[vals.len() / 3], vals[2 * vals.len() / 3]);
            for (side, name) in [(0, "low"), (1, "high")] {
                for (ei, e) in exits.iter().enumerate() {
                    tried += 1;
                    let filt = move |m: &HashMap<&'static str, f64>| if side == 0 { m[f] <= lo } else { m[f] >= hi };
                    let tr = rets(&train, ti, ei, &filt);
                    if tr.len() < 20 { continue }
                    let mtr = tr.iter().sum::<f64>() / tr.len() as f64;
                    let te = rets(&test, ti, ei, &filt);
                    best.push((mtr, format!("tau{tau} {f} {name} ({}) exit {}", if side == 0 { lo } else { hi }, exit_name(e)), json!({"train": stats(&tr), "test": stats(&te)})));
                }
            }
        }
    }
    best.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("{}", json!({"single_feature_configs_tried": tried}));
    for (_, name, v) in best.iter().take(15) {
        println!("{}", json!({"rule": name, "result": v}));
    }
    // 3. reverse engineering per decision time: median features of coins that later reach >= 2x from the entry price
    //    (before any LP removal) vs coins whose LP is removed vs others
    let reach = |p: &Pool, mult: f64, tau: f64| -> bool {
        let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
        let i0 = p.swaps.iter().position(|w| w.t > p.swaps[0].t + tau);
        i0.map_or(false, |i| p.swaps[i..].iter().filter(|w| w.t < rug).any(|w| w.px >= mult * p.swaps[i].px))
    };
    let all: Vec<String> = ids.iter().map(|x| x.1.clone()).collect();
    for (ti, tau) in TAUS.iter().enumerate() {
        let mut out = serde_json::Map::new();
        for f in FEATS {
            let (mut a, mut b, mut c) = (vec![], vec![], vec![]);
            for k in &all {
                let Some(r) = rows.get(&(k.clone(), ti)) else { continue };
                let v = r.feat[f];
                if !v.is_finite() { continue }
                if reach(&pools[k], 2.0, *tau) { a.push(v) } else if !pools[k].lp_out.is_empty() { c.push(v) } else { b.push(v) }
            }
            let med = |v: &mut Vec<f64>| { v.sort_by(|x, y| x.partial_cmp(y).unwrap()); if v.is_empty() { f64::NAN } else { (v[v.len() / 2] * 1e4).round() / 1e4 } };
            out.insert(f.into(), json!({"2x": med(&mut a), "rug": med(&mut c), "other": med(&mut b), "n": [a.len(), c.len(), b.len()]}));
        }
        println!("{}", json!({"contrast_tau": tau, "medians": out}));
    }
    // DUMP=<file>: one JSON line per (pool, tau) with set, label, features and per-exit returns (input of `permutasi`)
    if let Ok(path) = std::env::var("DUMP") {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).expect("dump");
        let tset: HashSet<&String> = train.iter().collect();
        for ((k, ti), r) in &rows {
            let feats: serde_json::Map<String, serde_json::Value> = r.feat.iter().map(|(n, v)| (n.to_string(), if v.is_finite() { json!(v) } else { serde_json::Value::Null })).collect();
            let rets: serde_json::Map<String, serde_json::Value> = exits.iter().zip(r.ret.iter()).map(|(e, v)| (exit_name(e), json!(v))).collect();
            let _ = writeln!(f, "{}", json!({"tau": TAUS[*ti], "set": if tset.contains(k) { "train" } else { "test" }, "pool": k, "t0": pools[k].swaps[0].t,
                "y2x": reach(&pools[k], 2.0, TAUS[*ti]), "rug": !pools[k].lp_out.is_empty(), "feat": feats, "ret": rets}));
        }
    }
    // 4. machine learning: L2 logistic regression (standardized, train only) predicting "reaches 2x from entry before LP
    //    removal"; test AUC (Mann-Whitney) and trading result of the top 30 % predicted test coins vs buying all
    let mlf: Vec<&str> = FEATS.iter().cloned().filter(|f| !matches!(*f, "lock" | "devsold" | "mc")).collect();
    for (ti, tau) in TAUS.iter().enumerate() {
        let xy = |set: &[String]| -> Vec<(Vec<f64>, bool, String)> {
            set.iter().filter_map(|k| rows.get(&(k.clone(), ti)).map(|r| (mlf.iter().map(|f| { let v = r.feat[*f]; if v.is_finite() { v.signum() * (1.0 + v.abs()).ln() } else { 0.0 } }).collect(), reach(&pools[k], 2.0, *tau), k.clone()))).collect()
        };
        let (tr, te) = (xy(&train), xy(&test));
        if tr.len() < 30 || te.len() < 15 { continue }
        let d = mlf.len();
        let mean: Vec<f64> = (0..d).map(|j| tr.iter().map(|r| r.0[j]).sum::<f64>() / tr.len() as f64).collect();
        let sd: Vec<f64> = (0..d).map(|j| (tr.iter().map(|r| (r.0[j] - mean[j]).powi(2)).sum::<f64>() / tr.len() as f64).sqrt().max(1e-9)).collect();
        let z = |x: &Vec<f64>| -> Vec<f64> { (0..d).map(|j| (x[j] - mean[j]) / sd[j]).collect() };
        let (mut w, mut b0) = (vec![0.0; d], 0.0);
        for _ in 0..3000 {
            let (mut gw, mut gb) = (vec![0.0; d], 0.0);
            for (x, y, _) in &tr {
                let zx = z(x);
                let p = 1.0 / (1.0 + (-(b0 + (0..d).map(|j| w[j] * zx[j]).sum::<f64>())).exp());
                let e = p - *y as u8 as f64;
                for j in 0..d { gw[j] += e * zx[j] }
                gb += e;
            }
            let n = tr.len() as f64;
            for j in 0..d { w[j] -= 0.1 * (gw[j] / n + 0.1 * w[j]) }
            b0 -= 0.1 * gb / n;
        }
        let score = |x: &Vec<f64>| { let zx = z(x); b0 + (0..d).map(|j| w[j] * zx[j]).sum::<f64>() };
        let auc = |s: &[(f64, bool)]| -> f64 {
            let mut v = s.to_vec();
            v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let (np, nn) = (v.iter().filter(|x| x.1).count() as f64, v.iter().filter(|x| !x.1).count() as f64);
            let rs: f64 = v.iter().enumerate().filter(|(_, x)| x.1).map(|(i, _)| i as f64 + 1.0).sum();
            if np == 0.0 || nn == 0.0 { f64::NAN } else { (rs - np * (np + 1.0) / 2.0) / (np * nn) }
        };
        let st: Vec<(f64, bool)> = te.iter().map(|r| (score(&r.0), r.1)).collect();
        let str_: Vec<(f64, bool)> = tr.iter().map(|r| (score(&r.0), r.1)).collect();
        let mut ord: Vec<(f64, &String)> = te.iter().map(|r| (score(&r.0), &r.2)).collect();
        ord.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let top: Vec<&String> = ord.iter().take((ord.len() as f64 * 0.3).ceil() as usize).map(|x| x.1).collect();
        let mut by_exit = serde_json::Map::new();
        for (ei, e) in exits.iter().enumerate() {
            let rt: Vec<f64> = top.iter().filter_map(|k| rows.get(&((*k).clone(), ti)).and_then(|r| r.ret[ei])).collect();
            let ra: Vec<f64> = te.iter().filter_map(|r| rows.get(&(r.2.clone(), ti)).and_then(|x| x.ret[ei])).collect();
            by_exit.insert(exit_name(e), json!({"top30": stats(&rt), "all_test": stats(&ra)}));
        }
        // single-feature accuracy: AUC of each feature alone, direction fixed on train, measured on test
        let mut single = serde_json::Map::new();
        for j in 0..d {
            let a_tr = auc(&tr.iter().map(|r| (r.0[j], r.1)).collect::<Vec<_>>());
            let sgn = if a_tr.is_finite() && a_tr < 0.5 { -1.0 } else { 1.0 };
            let a_te = auc(&te.iter().map(|r| (sgn * r.0[j], r.1)).collect::<Vec<_>>());
            single.insert(mlf[j].into(), json!({"auc_train": ((if sgn < 0.0 { 1.0 - a_tr } else { a_tr }) * 1000.0).round() / 1000.0, "auc_test": (a_te * 1000.0).round() / 1000.0, "higher_is_better": sgn > 0.0}));
        }
        let mut wts: Vec<(f64, &str)> = (0..d).map(|j| (w[j], mlf[j])).collect();
        wts.sort_by(|a, b| b.0.abs().partial_cmp(&a.0.abs()).unwrap());
        println!("{}", json!({"ml_tau": tau, "n_train": tr.len(), "n_test": te.len(), "pos_rate_train": tr.iter().filter(|r| r.1).count() as f64 / tr.len() as f64,
            "pos_rate_test": te.iter().filter(|r| r.1).count() as f64 / te.len() as f64, "auc_train": auc(&str_), "auc_test": auc(&st),
            "single_feature_auc": single, "top_weights": wts.iter().take(6).map(|(v, n)| json!([n, (v * 100.0).round() / 100.0])).collect::<Vec<_>>(), "trading": by_exit}));
    }
}
