//! Early buyers per liquid pool (input for the wallet lookups of C7): the first 20 distinct buying wallets within the
//! first 60 s, with the signature and time of their first buy.  Output TSV: pool, wallet, buy_sig, buy_time, t0, creator
//!   pembeli_awal <out.tsv> <raw_file>...
use dekode::series::*;
use std::collections::HashSet;
use std::io::Write;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pools = load_pools(&a[2..]);
    let mut f = std::fs::File::create(&a[1]).unwrap();
    let mut n = 0;
    for (k, p) in &pools {
        if p.swaps.len() < 10 { continue }
        let t0 = p.swaps[0].t;
        let Some(i) = p.swaps.iter().position(|w| w.t > t0 + 15.0) else { continue };
        if p.swaps[i].liq < 1.0 { continue }
        let mut seen = HashSet::new();
        for w in p.swaps.iter().filter(|w| w.buy && w.t <= t0 + 60.0 && !w.sig.is_empty()) {
            if seen.len() >= 20 { break }
            if seen.insert(w.trader.clone()) {
                let _ = writeln!(f, "{k}\t{}\t{}\t{}\t{t0}\t{}", w.trader, w.sig, w.t, p.creator);
                n += 1;
            }
        }
    }
    eprintln!("early buyers {n}");
}
