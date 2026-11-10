//! Data for the take-profit design. For liquid pools (SOL reserve >= LIQ_MIN at entry), entry = first swap after
//! t0 + TAU, and only swaps BEFORE any LP removal:
//!   - peak multiple and time to peak;
//!   - deepest pullback on the way up (max drop from the running high BEFORE the final peak) -> how wide a trailing stop
//!     must be to not sell a runner too early;
//!   - for pools whose LP was removed: seconds from the peak to the removal and price at removal vs peak.
//!   tp_stat <raw_file>...   env TAU (default 10), LIQ_MIN (default 1)
use dekode::series::*;
use serde_json::json;

fn q(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() { return f64::NAN }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[((v.len() - 1) as f64 * p) as usize] * 1000.0).round() / 1000.0
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let tau: f64 = std::env::var("TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    let liq_min: f64 = std::env::var("LIQ_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let pools = load_pools(&files);
    for (lo, hi) in [(2.0, 3.0), (3.0, 6.0), (6.0, 11.0), (11.0, f64::INFINITY)] {
        let (mut tpk, mut pull, mut pull_after2x, mut rug_after, mut rug_px) = (vec![], vec![], vec![], vec![], vec![]);
        let mut n = 0;
        for p in pools.values() {
            if p.swaps.len() < 5 { continue }
            let t0 = p.swaps[0].t;
            let rug = p.lp_out.first().map(|x| x.0).unwrap_or(f64::INFINITY);
            if rug <= t0 + tau { continue }
            let Some(i0) = p.swaps.iter().position(|w| w.t > t0 + tau) else { continue };
            let e = &p.swaps[i0];
            if e.liq < liq_min { continue }
            let path: Vec<&Sw> = p.swaps[i0..].iter().filter(|w| w.t < rug).collect();
            let (mut pk, mut tp) = (e.px, e.t);
            for w in &path { if w.px > pk { pk = w.px; tp = w.t } }
            let m = pk / e.px;
            if !(m >= lo && m < hi) { continue }
            n += 1;
            tpk.push(tp - e.t);
            // deepest pullback before the final peak, overall and after the price first reached 2x
            let (mut hiw, mut dd, mut dd2, mut seen2) = (e.px, 0.0f64, 0.0f64, false);
            for w in path.iter().filter(|w| w.t <= tp) {
                hiw = hiw.max(w.px);
                let d = 1.0 - w.px / hiw;
                dd = dd.max(d);
                if hiw >= 2.0 * e.px { seen2 = true }
                if seen2 { dd2 = dd2.max(d) }
            }
            pull.push(dd);
            if seen2 { pull_after2x.push(dd2) }
            if rug.is_finite() {
                rug_after.push(rug - tp);
                if let Some(w) = p.swaps.iter().find(|w| w.t >= rug) { rug_px.push(w.px / pk) }
            }
        }
        println!("{}", json!({"peak_band": format!("{lo}x-{hi}x"), "pools": n, "seconds_entry_to_peak": {"p25": q(&mut tpk.clone(), 0.25), "median": q(&mut tpk.clone(), 0.5), "p75": q(&mut tpk.clone(), 0.75)},
            "deepest_pullback_before_peak": {"median": q(&mut pull.clone(), 0.5), "p75": q(&mut pull.clone(), 0.75), "p90": q(&mut pull.clone(), 0.9)},
            "deepest_pullback_after_2x": {"n": pull_after2x.len(), "median": q(&mut pull_after2x.clone(), 0.5), "p75": q(&mut pull_after2x.clone(), 0.75), "p90": q(&mut pull_after2x.clone(), 0.9)},
            "rugged_after_peak": rug_after.len(), "seconds_peak_to_rug": {"p10": q(&mut rug_after.clone(), 0.1), "median": q(&mut rug_after.clone(), 0.5), "p90": q(&mut rug_after.clone(), 0.9)},
            "price_after_rug_vs_peak": {"median": q(&mut rug_px.clone(), 0.5)}}));
    }
}
