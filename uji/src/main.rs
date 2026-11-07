//! Backtest strategi "gelombang" memecoin Solana (Pump.fun + PumpSwap) di atas data pengumpul `sol-kumpul`.
//!
//! Aturan ditetapkan DI MUKA (permintaan pemilik, tidak disetel ke data):
//! - masuk hanya pada koin ber-quote SOL, umur >= 60 dtk (tanpa sniping), dicek tiap 30 dtk sampai umur 2 jam;
//! - gelombang: >= 10 dompet pembeli unik dalam 60 dtk terakhir, >= 1,5x dari 60 dtk sebelumnya, >= 50% dompet baru,
//!   SOL beli >= 2x SOL jual, dompet terbesar <= 30% SOL beli, SOL riil di kurva >= 2;
//! - dev belum pernah menjual; tidak ada bundle (>= 3 dompet selain dev membeli di slot kelahiran);
//! - TIDAK ada syarat "sudah ada penjual" (aturan honeypot lama memilih koin rug di Robinhood);
//! - keluar (tanpa TP1, tanpa trailing): momentum melemah (60 dtk terakhir SOL jual > SOL beli DAN pembeli unik 60 dtk
//!   < 50% puncaknya sejak masuk), jual besar (satu jual >= 3% SOL riil kurva, atau jual 30 dtk >= 10%), dev menjual,
//!   batas pegang 6 jam;
//! - isi order: transaksi pertama koin itu dengan waktu >= sinyal + JEDA, harga dari cadangan setelah transaksi itu,
//!   rumus kurva / AMM produk konstan, fee dari event (kurva) atau bps pool (AMM), biaya transaksi Solana per tx;
//! - ukuran: 1/4 Kelly + compounding dari hasil trade sendiri (2% untuk 30 trade pertama, 9,4% sampai 100), maks 15%,
//!   maks 1% SOL riil kurva, maks 6 posisi, eksposur maks 50%. Modal hanya berubah saat posisi DITUTUP.
mod ml;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering as AO};
use std::io::{BufRead, BufReader};

/// earliest decision age in seconds (env UMUR_MULAI, default 120 = owner rule "decide at minute 2 at the earliest")
fn umur_min() -> f64 {
    std::env::var("UMUR_MULAI").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x >= 10.0).unwrap_or(120.0)
}
const UMUR_MAKS: f64 = 7200.0;
const LANGKAH: f64 = 30.0;
pub const JENDELA: f64 = 60.0;
const JEDA: f64 = 1.0;
const PEGANG_MAKS: f64 = 6.0 * 3600.0;
/// biaya per transaksi kita (base fee + priority fee), SOL. Asumsi [U]: belum diukur di chain.
const BIAYA_TX: f64 = 0.0002;
/// pengali biaya (uji ketahanan: biaya x2)
static BIAYA_KALI: AtomicU64 = AtomicU64::new(0x3FF0_0000_0000_0000);
/// exit mode for `trade()` (see comment there); 0 = original rule. Set via env KELUAR or the exit test.
pub static KELUAR: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// K6 parameters (f64 bits): take profit (+x), stop loss (-x), max hold seconds
pub static TP_X: AtomicU64 = AtomicU64::new(0x7FF0_0000_0000_0000);
pub static SL_X: AtomicU64 = AtomicU64::new(0x4000_0000_0000_0000);
pub static HOLD_S: AtomicU64 = AtomicU64::new(0x404E_0000_0000_0000);
fn ld(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(AO::Relaxed))
}
/// walk-forward fold length in hours (env LIPAT_JAM, default 24 = per day)
fn lipat_jam() -> i64 {
    std::env::var("LIPAT_JAM").ok().and_then(|v| v.parse().ok()).filter(|h: &i64| *h > 0 && 24 % *h == 0).unwrap_or(24)
}
/// fold index of a unix time
pub fn lipat(t: f64) -> i64 {
    (t / (3600.0 * lipat_jam() as f64)).floor() as i64
}
/// fold label: date (and start hour when folds are shorter than a day)
fn nama_lipat(d: i64) -> String {
    let h = lipat_jam();
    if h == 24 { tanggal(d) } else { format!("{} {:02}h", tanggal((d * h).div_euclid(24)), (d * h).rem_euclid(24)) }
}
/// per-trade liquidity cap (env LP_CAP, default 0.01 = 1% of real SOL; 0 = off)
fn lp_cap() -> f64 {
    std::env::var("LP_CAP").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x >= 0.0).unwrap_or(0.01)
}
/// NOL_BIAYA=1: results BEFORE all costs (no DEX fee, no network fee) to separate signal from cost (expert review 4)
fn nol_biaya() -> bool {
    bendera("NOL_BIAYA") || NOL_X.load(AO::Relaxed)
}
fn biaya() -> f64 {
    if nol_biaya() { 0.0 } else { BIAYA_TX * f64::from_bits(BIAYA_KALI.load(AO::Relaxed)) }
}
fn fee_of(e: &Ev) -> f64 {
    if nol_biaya() { 0.0 } else { e.fee }
}
/// latency from signal to our transaction landing, seconds (env JEDA_DTK, default 1)
fn jeda() -> f64 {
    if JEDA_NOL.load(AO::Relaxed) {
        return 0.0;
    }
    std::env::var("JEDA_DTK").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x >= 0.0).unwrap_or(JEDA)
}
/// order size in SOL for the ML labels and the exit / hold tests (env UKURAN_SOL, default 0.01)
fn ukuran() -> f64 {
    std::env::var("UKURAN_SOL").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x > 0.0).unwrap_or(0.01)
}
/// per-run overrides used by the tuning grid and the loss decomposition (0 / false = use env)
pub static RASIO_X: AtomicU64 = AtomicU64::new(0);
pub static JENDELA_X: AtomicU64 = AtomicU64::new(0);
pub static AMBANG_X: AtomicU64 = AtomicU64::new(0);
pub static NOL_X: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static JEDA_NOL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// K9/K10 sell:buy ratio that ends the rally (env RASIO_JUAL, default 2; owner suggested 2:1 and 4:1)
fn rasio_jual() -> f64 {
    let o = f64::from_bits(RASIO_X.load(AO::Relaxed));
    if o > 0.0 { return o; }
    std::env::var("RASIO_JUAL").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x > 0.0).unwrap_or(2.0)
}
/// length of the short sell/buy window used by exits K3/K9/K10 and the big-sell rule (env JENDELA_KELUAR, default 30 s)
fn jendela_keluar() -> f64 {
    let o = f64::from_bits(JENDELA_X.load(AO::Relaxed));
    if o > 0.0 { return o; }
    std::env::var("JENDELA_KELUAR").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x >= 3.0).unwrap_or(30.0)
}
/// deliberate wait after the entry signal, seconds (env TUNDA_MASUK, default 0); separate from network latency
fn tunda_masuk() -> f64 {
    std::env::var("TUNDA_MASUK").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x >= 0.0).unwrap_or(0.0)
}
/// K11: minimum gain at 60 s to keep riding a position (env AMBANG_LANJUT, default 0.10 = +10%)
fn ambang_lanjut() -> f64 {
    let o = f64::from_bits(AMBANG_X.load(AO::Relaxed));
    if o > 0.0 {
        return o;
    }
    std::env::var("AMBANG_LANJUT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.10)
}
/// account size in SOL for the portfolio (env MODAL_SOL, default 0.2 = about $30)
fn modal_sol() -> f64 {
    std::env::var("MODAL_SOL").ok().and_then(|v| v.parse().ok()).filter(|x: &f64| *x > 0.0).unwrap_or(0.2)
}

#[derive(Clone, Debug)]
pub struct Ev {
    pub t: f64,
    pub slot: u64,
    pub beli: bool,
    pub user: String,
    pub sol: f64,
    /// harga SOL per token setelah transaksi ini
    pub px: f64,
    /// kurva: cadangan virtual (SOL, token); AMM: cadangan pool (SOL, token)
    pub cad_sol: f64,
    pub cad_tok: f64,
    /// SOL riil (kurva) atau cadangan SOL pool (AMM): ukuran likuiditas
    pub riil: f64,
    pub fee: f64,
    pub amm: bool,
    /// instruction kind: 0 buy, 1 buy_exact_quote_in, 2 buy_exact_sol_in, 3 multi_hop_swap, 4 sell, 5 AMM, 9 other
    pub ix: u8,
}

#[derive(Default)]
pub struct Koin {
    pub lahir: f64,
    pub slot_lahir: u64,
    pub creator: String,
    /// coin name and symbol (Pump.fun create event) for the copycat feature
    pub nama: String,
    pub simbol: String,
    pub sol_quote: bool,
    /// riwayat tidak lengkap (penarik berhenti di batas halaman): dikeluarkan dari uji
    pub terpotong: bool,
    /// koin mode "mayhem": cadangan virtual SOL berubah di luar trade (x*y=k tidak berlaku)
    pub mayhem: bool,
    /// coin graduated (Pump.fun complete event seen in the data)
    pub lulus: bool,
    /// mint safety from cek_aman.sh: Some(true) safe, Some(false) unsafe (hook / fee / authority), None = not checked
    pub aman: Option<bool>,
    pub ev: Vec<Ev>,
}

/// saklar uji (bawaan 0 = perilaku lama): TANPA_MAYHEM, PAKAI_TERPOTONG, ACAK_LABEL
pub fn bendera(k: &str) -> bool {
    std::env::var(k).map(|v| v == "1").unwrap_or(false)
}
/// geser benih acak (uji ketahanan "ganti benih"); bawaan 0
pub fn benih() -> u64 {
    std::env::var("BENIH").ok().and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Fitur pada waktu `t`: HANYA memakai event dengan waktu <= t (tes `fitur_tidak_melihat_masa_depan`).
#[derive(Debug, PartialEq)]
pub struct Fitur {
    pub pembeli: usize,
    pub pembeli_sebelum: usize,
    pub baru: f64,
    pub beli_sol: f64,
    pub jual_sol: f64,
    pub top: f64,
    pub riil: f64,
    pub dev_jual: bool,
    pub bundle: usize,
}

pub fn fitur(k: &Koin, t: f64) -> Fitur {
    let ev: Vec<&Ev> = k.ev.iter().filter(|e| e.t <= t).collect();
    let jendela = |a: f64, b: f64| ev.iter().filter(move |e| e.t > a && e.t <= b);
    let pembeli: HashSet<&str> = jendela(t - JENDELA, t).filter(|e| e.beli).map(|e| e.user.as_str()).collect();
    let sebelum: HashSet<&str> = jendela(t - 2.0 * JENDELA, t - JENDELA).filter(|e| e.beli).map(|e| e.user.as_str()).collect();
    let lama: HashSet<&str> = ev.iter().filter(|e| e.t <= t - JENDELA).map(|e| e.user.as_str()).collect();
    let baru = if pembeli.is_empty() { 0.0 } else { pembeli.iter().filter(|u| !lama.contains(*u)).count() as f64 / pembeli.len() as f64 };
    let (mut beli_sol, mut jual_sol) = (0.0, 0.0);
    let mut per: HashMap<&str, f64> = HashMap::new();
    for e in jendela(t - JENDELA, t) {
        if e.beli {
            beli_sol += e.sol;
            *per.entry(e.user.as_str()).or_insert(0.0) += e.sol;
        } else {
            jual_sol += e.sol;
        }
    }
    let top = if beli_sol > 0.0 { per.values().cloned().fold(0.0, f64::max) / beli_sol } else { 1.0 };
    Fitur {
        pembeli: pembeli.len(),
        pembeli_sebelum: sebelum.len(),
        baru,
        beli_sol,
        jual_sol,
        top,
        riil: ev.last().map_or(0.0, |e| e.riil),
        dev_jual: ev.iter().any(|e| !e.beli && e.user == k.creator),
        bundle: ev.iter().filter(|e| e.beli && e.slot == k.slot_lahir && e.user != k.creator).map(|e| e.user.as_str()).collect::<HashSet<_>>().len(),
    }
}

pub const SYARAT: [&str; 8] = ["pembeli>=10", "tumbuh>=1,5x", "baru>=50%", "beli:jual>=2", "top<=30%", "riil>=2", "dev belum jual", "tanpa bundle"];

/// aturan gelombang tanpa syarat ke-`lewat` (uji buang-satu); lewat >= 8 = semua syarat berlaku
pub fn sinyal_tanpa(f: &Fitur, lewat: usize) -> bool {
    let c = [
        f.pembeli >= 10,
        f.pembeli as f64 >= 1.5 * f.pembeli_sebelum as f64,
        f.baru >= 0.5,
        f.beli_sol >= 2.0 * f.jual_sol,
        f.top <= 0.30,
        f.riil >= 2.0,
        !f.dev_jual,
        f.bundle < 3,
    ];
    c.iter().enumerate().all(|(i, ok)| i == lewat || *ok)
}

pub fn sinyal_masuk(f: &Fitur) -> bool {
    f.pembeli >= 10
        && f.pembeli as f64 >= 1.5 * f.pembeli_sebelum as f64
        && f.baru >= 0.5
        && f.beli_sol >= 2.0 * f.jual_sol
        && f.top <= 0.30
        && f.riil >= 2.0
        && !f.dev_jual
        && f.bundle < 3
}

/// token yang didapat untuk `sol` masuk (produk konstan pada cadangan setelah event pengisi, fee dipotong di depan)
fn beli_tok(e: &Ev, sol: f64) -> f64 {
    let x = sol * (1.0 - fee_of(e));
    e.cad_tok - e.cad_sol * e.cad_tok / (e.cad_sol + x)
}
fn jual_sol(e: &Ev, tok: f64) -> f64 {
    (e.cad_sol - e.cad_sol * e.cad_tok / (e.cad_tok + tok)) * (1.0 - fee_of(e))
}

/// event pengisi: event pertama dengan waktu >= t (None = data habis)
#[allow(dead_code)]
fn isi(k: &Koin, t: f64) -> Option<&Ev> {
    k.ev.iter().find(|e| e.t >= t)
}

pub struct Hasil {
    pub mint: String,
    pub masuk: f64,
    pub keluar: f64,
    pub ret: f64,
    pub alasan: &'static str,
    pub puncak: f64,
}

/// Simulasi satu trade dengan ukuran `sol`. Sinyal keluar memakai data <= waktu event yang sedang diperiksa.
/// Jendela 60/30 dtk dihitung bergulir (dua penunjuk), hasil sama dengan menghitung ulang dari semua event.
pub fn trade(mint: &str, k: &Koin, t_sinyal: f64, sol: f64) -> Option<Hasil> {
    // entry (expert review fixes): our order lands at t_sinyal + latency (env JEDA_DTK, measured websocket receive delay
    // p90 1.7 s + send/landing); the fill is the curve/pool state at that moment, i.e. AFTER every trade that landed
    // before us (no waiting for a later trade, so no stale signals). Events after the fill state are checked by index,
    // so trades in the same second are not skipped.
    // TUNDA_MASUK: wait this many seconds after the signal before sending the buy (signals fire at micro-tops)
    let t0 = t_sinyal + tunda_masuk() + jeda();
    let i0 = k.ev.iter().rposition(|e| e.t <= t0)?;
    let e0 = &k.ev[i0];
    let tok = beli_tok(e0, sol);
    if !(tok > 0.0) {
        return None;
    }
    let px0 = e0.px;
    let mulai = i0 + 1;
    if mulai >= k.ev.len() {
        let kali = f64::from_bits(BIAYA_KALI.load(AO::Relaxed));
        let balik = jual_sol(e0, tok) * (1.0 - (kali - 1.0) * fee_of(e0)) * (1.0 - (kali - 1.0) * fee_of(e0)) - 2.0 * biaya();
        return Some(Hasil { mint: mint.to_string(), masuk: t0, keluar: t0, ret: balik / sol, alasan: "tanpa trade lagi", puncak: 1.0 });
    }
    // jendela bergulir atas k.ev[lo..=i]
    let (mut lo60, mut lo30) = (k.ev.iter().position(|e| e.t > k.ev[mulai].t - JENDELA).unwrap_or(0), 0usize);
    lo30 = lo30.max(k.ev.iter().position(|e| e.t > k.ev[mulai].t - jendela_keluar()).unwrap_or(0));
    let (mut b60, mut j60, mut j30, mut b30) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    // transaction counts in the 30 s window (K10: sell count vs buy count)
    let (mut nb30, mut nj30) = (0usize, 0usize);
    let mode = KELUAR.load(AO::Relaxed);
    // K5 = fixed 60 s hold; K6 = take-profit / stop-loss / max hold from TP_X, SL_X, HOLD_S
    let (tp, sl, hold) = if mode == 6 { (ld(&TP_X), ld(&SL_X), ld(&HOLD_S)) } else { (f64::INFINITY, 2.0, 60.0) };
    let mut pembeli: HashMap<&str, usize> = HashMap::new();
    for x in &k.ev[lo60..mulai] {
        if x.beli { b60 += x.sol; *pembeli.entry(x.user.as_str()).or_insert(0) += 1 } else { j60 += x.sol }
    }
    for x in &k.ev[lo30..mulai] {
        if x.beli { b30 += x.sol; nb30 += 1 } else { j30 += x.sol; nj30 += 1 }
    }
    let mut puncak_pembeli = 0usize;
    // K11: true once the position survived the 60 s check (it was up enough) and is now a "winner" being ridden
    let mut lanjut = false;
    // K7/K8: token balances per wallet (from trade flow) to know the top-3 holders BEFORE each event (research R6)
    let mut saldo: HashMap<&str, f64> = HashMap::new();
    if (mode == 7 || mode == 8 || mode == 11) {
        for x in &k.ev[..mulai] {
            let tk = if x.px > 0.0 { x.sol / x.px } else { 0.0 };
            *saldo.entry(x.user.as_str()).or_insert(0.0) += if x.beli { tk } else { -tk };
        }
    }
    let mut puncak = 1.0f64;
    let mut alasan = "data habis";
    let mut t_keluar = None;
    for i in mulai..k.ev.len() {
        let e = &k.ev[i];
        if e.beli { b60 += e.sol; b30 += e.sol; nb30 += 1; *pembeli.entry(e.user.as_str()).or_insert(0) += 1 } else { j60 += e.sol; j30 += e.sol; nj30 += 1 }
        while k.ev[lo60].t <= e.t - JENDELA {
            let x = &k.ev[lo60];
            if x.beli {
                b60 -= x.sol;
                if let Some(c) = pembeli.get_mut(x.user.as_str()) { *c -= 1; if *c == 0 { pembeli.remove(x.user.as_str()); } }
            } else { j60 -= x.sol }
            lo60 += 1;
        }
        while k.ev[lo30].t <= e.t - jendela_keluar() {
            if k.ev[lo30].beli { b30 -= k.ev[lo30].sol; nb30 = nb30.saturating_sub(1) } else { j30 -= k.ev[lo30].sol; nj30 = nj30.saturating_sub(1) }
            lo30 += 1;
        }
        puncak = puncak.max(e.px / px0);
        puncak_pembeli = puncak_pembeli.max(pembeli.len());
        // exit modes (pre-registered before looking at results; 0 = original rule):
        // 0 dev | big sell 3% / 30s 10% | momentum | timeout
        // 1 dev | big sell 10% / 30s 25% | momentum | timeout
        // 2 dev | momentum | timeout (no big-sell rule)
        // 3 dev | held >= 30 s and 30 s buy SOL < sell SOL (buy pressure below 1:1) | timeout
        // 4 dev | held >= 30 s and 30 s buy SOL < sell SOL and unique buyers 60 s < 70% of peak | timeout
        // 5 dev | fixed hold 60 s (data: 30-60 s holds lose least; Markov: buy waves revert within 30 s)
        let (big1, big30) = if mode == 1 { (0.10, 0.25) } else { (0.03, 0.10) };
        let big = !e.beli && e.riil > 0.0 && (e.sol >= big1 * e.riil || j30 >= big30 * e.riil);
        let momentum = e.t >= t0 + JENDELA && j60 > b60 && (pembeli.len() as f64) < 0.5 * puncak_pembeli as f64;
        let pressure = e.t >= t0 + 30.0 && b30 < j30;
        // top-3 holder sells (K7 alone, K8 together with momentum): balances BEFORE this event
        let top3_jual = (mode == 7 || mode == 8 || mode == 11) && !e.beli && {
            let mut v: Vec<f64> = saldo.values().cloned().collect();
            v.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            let batas = v.get(2).cloned().unwrap_or(0.0);
            saldo.get(e.user.as_str()).is_some_and(|b| *b > 0.0 && *b >= batas)
        };
        if (mode == 7 || mode == 8 || mode == 11) {
            let tk = if e.px > 0.0 { e.sol / e.px } else { 0.0 };
            *saldo.entry(e.user.as_str()).or_insert(0.0) += if e.beli { tk } else { -tk };
        }
        let why = if !e.beli && e.user == k.creator {
            Some("dev menjual")
        } else if mode <= 1 && big {
            Some("jual besar")
        } else if mode <= 2 && momentum {
            Some("momentum melemah")
        } else if mode == 3 && pressure {
            Some("tekanan beli < 1:1")
        } else if mode == 4 && pressure && (pembeli.len() as f64) < 0.7 * puncak_pembeli as f64 {
            Some("tekanan + pembeli turun")
        } else if mode == 11 && !lanjut && e.t >= t0 + 60.0 {
            // K11 step 1 (cut losers fast): at 60 s, a position not up by AMBANG_LANJUT is sold
            if e.px < px0 * (1.0 + ambang_lanjut()) {
                Some("rugi dipotong 60 dtk")
            } else {
                lanjut = true;
                None
            }
        } else if mode == 11 && lanjut && j30 > 0.0 && j30 >= rasio_jual() * b30 {
            // K11 step 2 (let winners run): sell only when sellers beat buyers R:1 in the short window
            Some("pemenang: jual:beli >= R:1")
        } else if mode == 9 && j30 > 0.0 && j30 >= rasio_jual() * b30 {
            // owner's momentum take-profit: the rally is over when sell SOL >= R x buy SOL in the last 30 s
            Some("jual:beli SOL >= R:1")
        } else if mode == 10 && nj30 >= 2 && nj30 as f64 >= rasio_jual() * nb30 as f64 {
            // same idea by number of transactions: sellers outnumber buyers R:1 in the last 30 s
            Some("jumlah jual:beli >= R:1")
        } else if (mode == 7 || mode == 8 || mode == 11) && top3_jual {
            Some("top-3 holder jual")
        } else if mode == 8 && momentum {
            Some("momentum melemah")
        } else if mode == 6 && e.px >= px0 * (1.0 + tp) {
            Some("TP")
        } else if mode == 6 && e.px <= px0 * (1.0 - sl) {
            Some("SL")
        } else if (mode == 5 || mode == 6) && e.t >= t0 + hold {
            Some("pegang waktu")
        } else if e.t >= t0 + PEGANG_MAKS {
            Some("batas waktu")
        } else {
            None
        };
        if let Some(w) = why {
            alasan = w;
            t_keluar = Some(e.t);
            break;
        }
    }
    // K5/K6 time exit. A coin whose data STOPPED (graduated to an undecoded pool, or dead) is labelled separately so it is
    // never reported as a normal hold (expert review bug 2). All exits fill at the state when our sell lands (+ latency).
    let data_habis = alasan == "data habis";
    if (mode == 5 || mode == 6) && (alasan == "pegang waktu" || data_habis) {
        t_keluar = Some(t0 + hold);
        if data_habis {
            alasan = "data habis (tertutup)";
        }
    }
    let (ex, t1) = match t_keluar.map(|t| t + jeda()).and_then(|tl| k.ev.iter().rev().find(|e| e.t <= tl).map(|e| (e, tl))) {
        Some(x) => x,
        None => {
            let e = k.ev.last()?;
            (e, e.t)
        }
    };
    // biaya x2 juga menggandakan fee kurva/AMM (uji ketahanan)
    let kali = f64::from_bits(BIAYA_KALI.load(AO::Relaxed));
    let balik = jual_sol(ex, tok) * (1.0 - (kali - 1.0) * fee_of(ex)) * (1.0 - (kali - 1.0) * fee_of(e0)) - 2.0 * biaya();
    Some(Hasil { mint: mint.to_string(), masuk: t0, keluar: t1, ret: balik / sol, alasan, puncak })
}

/// `lihat` = trades already seen (dedupe ACROSS files: the post-graduation pull overlaps the DBC pull)
fn baca(path: &str, koin: &mut HashMap<String, Koin>, sambung: &mut Vec<f64>, lihat: &mut HashSet<(String, String, String)>, isi_ok: &mut Vec<f64>) {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lewati {path}: {e}");
            return;
        }
    };
    let r: Box<dyn BufRead> = if path.ends_with(".gz") { Box::new(BufReader::new(flate2::read::GzDecoder::new(f))) } else { Box::new(BufReader::new(f)) };
    // last local reserves per mint (Meteora DBC events without computable liquidity)
    let mut cad_akhir: HashMap<String, (f64, f64)> = HashMap::new();
    let mut l_mentah: HashMap<String, f64> = HashMap::new();
    for l in r.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else { continue };
        let g = |k: &str| v[k].as_f64().unwrap_or(0.0);
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match v["k"].as_str() {
            Some("sambung") => sambung.push(g("t")),
            // gap backfill record of sol-kumpul: a COMPLETE Pump.fun backfill means the reconnect gap left no hole
            Some("isi") if v["lengkap"].as_bool() == Some(true) && s("program").starts_with("6EF8") => isi_ok.push(g("t")),
            Some("terpotong") => koin.entry(s("mint")).or_default().terpotong = true,
            // graduation (Pump.fun CompleteEvent): used only to DESCRIBE the market (research step A), never as a feature
            Some("complete") => koin.entry(s("mint")).or_default().lulus = true,
            // tarik_dex hit its call cap while pulling one coin by mint address: that coin's history is incomplete
            Some("berhenti") if v["program"].as_str().is_some_and(|p| koin.contains_key(p)) => koin.entry(s("program")).or_default().terpotong = true,
            // on-chain mint safety (cek_aman.sh): unsafe or unknown = never buy (fail closed)
            Some("aman") => koin.entry(s("mint")).or_default().aman = Some(v["ok"].as_bool().unwrap_or(false)),
            Some("create") => {
                let k = koin.entry(s("mint")).or_default();
                k.lahir = g("t");
                k.slot_lahir = v["slot"].as_u64().unwrap_or(0);
                k.creator = s("creator");
                k.nama = s("name").trim().to_lowercase();
                k.simbol = s("symbol").trim().to_lowercase();
                k.sol_quote = s("quote").starts_with("1111");
                k.mayhem |= v["mayhem"].as_bool().unwrap_or(false);
            }
            Some("trade") => {
                // dedupe: baris isian celah bisa mengulang transaksi yang juga datang lewat websocket
                if !lihat.insert((s("sig"), s("mint"), s("user") + &v["tok"].to_string())) {
                    continue;
                }
                let (mut vs, mut vt) = (g("vsol"), g("vtok"));
                // Meteora DBC: local reserves can be missing on a tiny swap; keep the event (trader flow matters) and
                // rescale the coin's last known local liquidity to the event's own price (x*y = L^2 kept, price = px)
                if (vs <= 0.0 || vt <= 0.0) && g("px") > 0.0 {
                    if let Some((ls, lt)) = cad_akhir.get(&s("mint")) {
                        let l = (ls * lt).sqrt();
                        vs = l * g("px").sqrt();
                        vt = l / g("px").sqrt();
                    }
                }
                if vs <= 0.0 || vt <= 0.0 {
                    continue;
                }
                // DBC / DAMM v2 local liquidity can be an outlier on one swap (price range edge, rounding): if it jumps
                // more than 3x from the last value of the same coin, keep the last liquidity re-priced at this swap's price
                if (v["dex"] == "dbc" || v["dex"] == "damm2") && g("px") > 0.0 {
                    // compare with the previous RAW liquidity (not the accepted one), so a real lasting change is followed
                    let l1 = (vs * vt).sqrt();
                    if let Some(l0) = l_mentah.insert(s("mint"), l1) {
                        if l1 > 3.0 * l0 || l1 < l0 / 3.0 {
                            if let Some((ls, lt)) = cad_akhir.get(&s("mint")) {
                                let la = (ls * lt).sqrt();
                                vs = la * g("px").sqrt();
                                vt = la / g("px").sqrt();
                            }
                        }
                    }
                }
                cad_akhir.insert(s("mint"), (vs, vt));
                // Pump.fun curve rows with zero fee are the mayhem agent's own trades; OUR order would pay the normal fee
                // (95 + 30 bps), so a zero-fee fill event must not make our fill free (review finding 2)
                let fee = if v["dex"].is_null() && g("fee_bps") == 0.0 { 0.0125 } else { (g("fee_bps") + g("cfee_bps")) / 1e4 };
                let kk = koin.entry(s("mint")).or_default();
                kk.mayhem |= v["mayhem"].as_bool().unwrap_or(false);
                kk.ev.push(Ev {
                    t: g("t"), slot: v["slot"].as_u64().unwrap_or(0), beli: v["buy"].as_bool().unwrap_or(false), user: s("user"),
                    sol: g("sol"), px: vs / vt, cad_sol: vs, cad_tok: vt, riil: g("rsol"), fee, amm: false,
                    ix: match v["ix"].as_str() { Some("buy") => 0, Some("buy_exact_quote_in") => 1, Some("buy_exact_sol_in") => 2, Some("multi_hop_swap") => 3, Some("sell") => 4, _ => 9 },
                });
            }
            Some("beli_amm") | Some("jual_amm") => {
                if !lihat.insert((s("sig"), s("pool"), s("user") + &v["base"].to_string())) {
                    continue;
                }
                let (pq, pb) = (g("pool_quote") / 1e9, g("pool_base") / 1e6);
                if pq <= 0.0 || pb <= 0.0 {
                    continue;
                }
                koin.entry(s("mint")).or_default().ev.push(Ev {
                    t: g("t"), slot: v["slot"].as_u64().unwrap_or(0), beli: v["k"] == "beli_amm", user: s("user"),
                    sol: g("quote_amt") / 1e9, px: pq / pb, cad_sol: pq, cad_tok: pb, riil: pq,
                    // PumpSwap total fee = LP + protocol + coin creator (cr_bps logged since 6 Nov 17:13 UTC; older rows lack it)
                    fee: (g("lp_bps") + g("pr_bps") + g("cr_bps")) / 1e4, amm: true, ix: 5,
                });
            }
            _ => {}
        }
    }
}

/// Prior trade returns (gross multiples) from the TRAINING period, used by KELLY_DATA=1 instead of the fixed warm-up
pub static KELLY_PRIOR: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
fn kelly(r: &[f64]) -> f64 {
    // KELLY_DATA=1 (owner 7 Nov 2026: no fixed 2% / 9.4% warm-up): quarter Kelly estimated from data = training-period
    // outcomes of screened coins + our own closed trades, re-estimated after every trade; cap 15% (KELLY_MAKS rule)
    if bendera("KELLY_DATA") {
        // once the system has >= 20 own outcomes (shadow or real), Kelly uses ONLY its own picks (the relevant distribution)
        if r.len() >= 20 {
            return kelly_kuart(r);
        }
        let mut all: Vec<f64> = KELLY_PRIOR.get().cloned().unwrap_or_default();
        all.extend_from_slice(r);
        if all.len() < 20 {
            return 0.0; // not enough data to estimate Kelly: no bet (fail closed)
        }
        return kelly_kuart(&all);
    }
    if r.len() < 30 {
        return 0.02;
    }
    if r.len() < 100 {
        return 0.094;
    }
    kelly_kuart(r)
}
fn kelly_kuart(r: &[f64]) -> f64 {
    let (mut best, mut bv, mut f) = (0.0f64, f64::NEG_INFINITY, 0.005f64);
    while f <= 0.6 {
        if r.iter().all(|x| 1.0 + f * (x - 1.0) > 0.0) {
            let s = r.iter().map(|x| (1.0 + f * (x - 1.0)).ln()).sum::<f64>() / r.len() as f64;
            if s > bv {
                bv = s;
                best = f;
            }
        }
        f += 0.005;
    }
    if bv <= 0.0 {
        return 0.0; // no positive expected log growth: Kelly says do not bet
    }
    (best / 4.0).min(0.15)
}

type Sinyal<'a> = (f64, &'a String, &'a Koin);

/// Akun 1/4 Kelly + compounding; modal berubah hanya saat posisi ditutup. Mencetak laporan lengkap + per hari.
fn portofolio(nama: &str, sinyal: &[Sinyal]) -> (usize, f64, f64) {
    portofolio_x(nama, sinyal, true)
}

fn portofolio_x(nama: &str, sinyal: &[Sinyal], cetak: bool) -> (usize, f64, f64) {
    // LAPOR_DARI=<unix>: verification loop reports only trades on coins born after the freeze time (unseen data)
    let dari: f64 = std::env::var("LAPOR_DARI").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let mut sinyal: Vec<&Sinyal> = sinyal.iter().filter(|s| s.2.lahir >= dari).collect();
    sinyal.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (mut kas, mut puncak_eq, mut dd) = (1.0f64, 1.0f64, 0.0f64);
    let mut buka: BTreeMap<u64, (f64, f64, f64)> = BTreeMap::new();
    let mut rets: Vec<f64> = Vec::new();
    let mut hasil: Vec<(Hasil, f64)> = Vec::new();
    let (mut n_slot, mut n_kecil, mut n_bayangan) = (0, 0, 0usize);
    let modal_sol = modal_sol(); // default 0,2 SOL ~ $30 (env MODAL_SOL)
    let tutup = |buka: &mut BTreeMap<u64, (f64, f64, f64)>, sampai: f64, kas: &mut f64, rets: &mut Vec<f64>, puncak: &mut f64, dd: &mut f64| {
        let ids: Vec<u64> = buka.iter().filter(|(_, v)| v.0 <= sampai).map(|(id, _)| *id).collect();
        for id in ids {
            if let Some((_, uk, r)) = buka.remove(&id) {
                *kas += uk * r;
                rets.push(r);
                let eq = *kas + buka.values().map(|v| v.1).sum::<f64>();
                *puncak = puncak.max(eq);
                *dd = dd.max(1.0 - eq / *puncak);
            }
        }
    };
    for (i, (t, m, k)) in sinyal.iter().enumerate() {
        tutup(&mut buka, *t, &mut kas, &mut rets, &mut puncak_eq, &mut dd);
        if buka.len() >= 6 {
            n_slot += 1;
            continue;
        }
        let eq = kas + buka.values().map(|v| v.1).sum::<f64>();
        let mut uk = (kelly(&rets) * eq).min(kas).min(0.5 * eq - (eq - kas));
        // liquidity cap per trade (env LP_CAP, share of real SOL in the curve/pool; 0 = no cap, owner choice 6 Nov 2026).
        // Our own price impact is always simulated by the x*y=k fill, so large orders still pay for their size.
        if lp_cap() > 0.0 {
            uk = uk.min(lp_cap() * fitur(k, *t).riil / modal_sol);
        }
        // KELLY_DATA: when quarter Kelly says 0 (no proven edge yet) the trade is taken in SHADOW (stake 0, minimum
        // size 0.002 SOL for the fill model) so Kelly can learn from the system's own picks; real money only once Kelly > 0
        if bendera("KELLY_DATA") && (!(uk > 0.0) || uk * modal_sol < 0.002) {
            if let Some(h) = trade(m, k, *t, 0.002) {
                buka.insert(i as u64, (h.keluar, 0.0, h.ret));
                n_bayangan += 1;
            }
            continue;
        }
        if !(uk > 0.0) || uk * modal_sol < 0.002 {
            n_kecil += 1;
            continue;
        }
        if let Some(h) = trade(m, k, *t, uk * modal_sol) {
            kas -= uk;
            buka.insert(i as u64, (h.keluar, uk, h.ret));
            hasil.push((h, uk));
        }
    }
    tutup(&mut buka, f64::INFINITY, &mut kas, &mut rets, &mut puncak_eq, &mut dd);
    let n = hasil.len();
    let menang: Vec<f64> = hasil.iter().filter(|(h, _)| h.ret > 1.0).map(|(h, _)| h.ret - 1.0).collect();
    let kalah: Vec<f64> = hasil.iter().filter(|(h, _)| h.ret <= 1.0).map(|(h, _)| 1.0 - h.ret).collect();
    let rata = |v: &[f64]| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    let (aw, al) = (rata(&menang), rata(&kalah));
    let (gp, gl) = hasil.iter().fold((0.0, 0.0), |a, (h, uk)| if h.ret > 1.0 { (a.0 + uk * (h.ret - 1.0), a.1) } else { (a.0, a.1 + uk * (1.0 - h.ret)) });
    let mut untung: Vec<f64> = hasil.iter().map(|(h, uk)| uk * (h.ret - 1.0)).collect();
    untung.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    if !cetak {
        return (n, menang.len() as f64 / n.max(1) as f64, 30.0 * kas);
    }
    println!("\n### {nama}");
    println!("trade {n} | menang {:.1}% | rata menang +{:.1}% | rata kalah -{:.1}% | RR {:.2} | win impas {:.1}% | PF {:.2}",
        100.0 * menang.len() as f64 / n.max(1) as f64, 100.0 * aw, 100.0 * al, if al > 0.0 { aw / al } else { f64::INFINITY },
        100.0 * al / (aw + al).max(1e-12), if gl > 0.0 { gp / gl } else { f64::INFINITY });
    println!("modal $30 -> ${:.2} | DD maks {:.1}% | PnL tanpa 5 trade terbaik ${:.2} | dilewati: slot penuh {n_slot}, ukuran kecil {n_kecil}, bayangan {n_bayangan}",
        30.0 * kas, 100.0 * dd, 30.0 * untung.iter().skip(5).sum::<f64>());
    // accounting check: equity change must equal the sum over trades of stake x (return - 1)
    let stake_avg = hasil.iter().map(|(_, uk)| *uk).sum::<f64>() / n.max(1) as f64;
    println!("cek akuntansi: rata taruhan {:.2}% modal awal | jumlah taruhan x hasil {:+.2}% | perubahan modal {:+.2}%", 100.0 * stake_avg,
        100.0 * hasil.iter().map(|(h, uk)| uk * (h.ret - 1.0)).sum::<f64>(), 100.0 * (kas - 1.0));
    let mut alasan: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
    for (h, _) in &hasil {
        let a = alasan.entry(h.alasan).or_insert((0, 0.0));
        a.0 += 1;
        a.1 += h.ret;
    }
    let al_s: Vec<String> = alasan.iter().map(|(a, (c, s))| format!("{a} {c} ({:.2}x)", s / *c as f64)).collect();
    println!("keluar: {}", al_s.join(" | "));
    println!("puncak setelah masuk >=2x {} >=5x {} >=10x {} | hasil trade >=2x {} >=5x {}",
        hasil.iter().filter(|(h, _)| h.puncak >= 2.0).count(), hasil.iter().filter(|(h, _)| h.puncak >= 5.0).count(),
        hasil.iter().filter(|(h, _)| h.puncak >= 10.0).count(), hasil.iter().filter(|(h, _)| h.ret >= 2.0).count(), hasil.iter().filter(|(h, _)| h.ret >= 5.0).count());
    let mut hari: BTreeMap<i64, (usize, usize, f64)> = BTreeMap::new();
    for (h, uk) in &hasil {
        let j = hari.entry(lipat(h.masuk)).or_insert((0, 0, 0.0));
        j.0 += 1;
        j.1 += (h.ret > 1.0) as usize;
        j.2 += 30.0 * uk * (h.ret - 1.0);
    }
    let hs: Vec<String> = hari.iter().map(|(d, (c, w, p))| format!("{} : {c} trade, menang {w}, PnL ${p:.2}", nama_lipat(*d))).collect();
    println!("per hari: {}", hs.join(" || "));
    // 95% bootstrap interval of the mean return per trade (one trade per coin here), 2000 resamples, fixed seed;
    // and a cluster bootstrap by day. If the interval contains 0, the result is not distinguishable from luck.
    if n >= 5 {
        let r: Vec<f64> = hasil.iter().map(|(h, _)| h.ret - 1.0).collect();
        let mut s = 0x2545F4914F6CDD1Du64;
        let mut acak = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut means: Vec<f64> = (0..2000).map(|_| (0..r.len()).map(|_| r[(acak() % r.len() as u64) as usize]).sum::<f64>() / r.len() as f64).collect();
        means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let hari_pos = hari.values().filter(|(_, _, p)| *p > 0.0).count();
        println!("rata per trade {:+.1}% | 95% CI [{:+.1}%, {:+.1}%] {} | hari untung {}/{}", 100.0 * r.iter().sum::<f64>() / r.len() as f64,
            100.0 * means[50], 100.0 * means[1950], if means[50] > 0.0 { "(di atas 0)" } else { "(mencakup/di bawah 0 = belum terbukti)" }, hari_pos, hari.len());
    }
    (n, menang.len() as f64 / n.max(1) as f64, 30.0 * kas)
}

fn tanggal(hari: i64) -> String {
    // hari sejak 1970 -> YYYY-MM-DD (kalender Gregorian)
    let z = hari + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{}-{:02}-{:02}", yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    // exit mode for the whole report (env KELUAR=0..5, default 0 = original rule)
    KELUAR.store(std::env::var("KELUAR").ok().and_then(|v| v.parse().ok()).filter(|m: &u8| *m <= 11).unwrap_or(0), AO::Relaxed);
    if files.is_empty() {
        eprintln!("pakai: sol-uji <pump_*.jsonl[.gz]>...");
        std::process::exit(2);
    }
    let (mut koin, mut sambung) = (HashMap::new(), Vec::new());
    let mut lihat: HashSet<(String, String, String)> = HashSet::new();
    let mut isi_ok: Vec<f64> = Vec::new();
    for f in &files {
        baca(f, &mut koin, &mut sambung, &mut lihat, &mut isi_ok);
    }
    for k in koin.values_mut() {
        k.ev.sort_by_key(|e| e.slot);
    }
    let t_awal = koin.values().filter_map(|k| k.ev.first().map(|e| e.t)).fold(f64::INFINITY, f64::min);
    let t_akhir = koin.values().filter_map(|k| k.ev.last().map(|e| e.t)).fold(0.0, f64::max);
    sambung.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // CELAH_TERISI=1: a reconnect followed within 15 min by a COMPLETE Pump.fun backfill is not a data hole
    let celah: Vec<f64> = sambung.iter().skip(1).cloned().filter(|s| !(bendera("CELAH_TERISI") && isi_ok.iter().any(|i| *i > *s && *i <= *s + 900.0))).collect();
    let n_terpotong = koin.values().filter(|k| k.terpotong).count();
    let ada_aman = koin.values().any(|k| k.aman.is_some());
    println!("cek keamanan mint: {} | aman {} | TIDAK aman {} | belum dicek {}", if ada_aman { "aktif" } else { "tidak ada data" },
        koin.values().filter(|k| k.aman == Some(true)).count(), koin.values().filter(|k| k.aman == Some(false)).count(), koin.values().filter(|k| k.lahir > 0.0 && k.aman.is_none()).count());
    let (tanpa_mayhem, pakai_terpotong, acak_label) = (bendera("TANPA_MAYHEM"), bendera("PAKAI_TERPOTONG"), bendera("ACAK_LABEL"));
    println!("saklar: TANPA_MAYHEM={} PAKAI_TERPOTONG={} ACAK_LABEL={} BENIH={} | koin mayhem {}",
        tanpa_mayhem as u8, pakai_terpotong as u8, acak_label as u8, benih(), koin.values().filter(|k| k.mayhem).count());
    let mut lahir_ok: Vec<(&String, &Koin)> = koin
        .iter()
        .filter(|(_, k)| (pakai_terpotong || !k.terpotong) && !(tanpa_mayhem && k.mayhem))
        // safety (Part 14.4): when safety records are loaded, only coins checked SAFE are tradable (unknown = excluded)
        .filter(|(_, k)| !ada_aman || k.aman == Some(true))
        // DIAG_TANPA_LULUS=1: DIAGNOSIS ONLY (uses the future): drop coins whose data ends at their peak liquidity (>= 10 SOL
        // and last real SOL >= 90% of max), i.e. coins that most likely graduated to a pool we do not decode yet.
        .filter(|(_, k)| !bendera("DIAG_TANPA_LULUS") || { let mx = k.ev.iter().map(|e| e.riil).fold(0.0, f64::max); !(mx >= 10.0 && k.ev.last().is_some_and(|e| e.riil >= 0.9 * mx)) })
        .filter(|(_, k)| k.lahir > 0.0 && k.sol_quote && k.lahir >= t_awal && k.lahir + UMUR_MAKS <= t_akhir)
        .filter(|(_, k)| !celah.iter().any(|c| *c > k.lahir && *c < k.lahir + UMUR_MAKS))
        .collect();
    // deterministic order: ties on birth time broken by mint (HashMap order is random per run)
    lahir_ok.sort_by(|a, b| a.1.lahir.partial_cmp(&b.1.lahir).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(b.0)));
    println!("data {:.1} jam | koin {} | terpotong (dikeluarkan) {} | dipakai (quote SOL, lengkap) {}",
        (t_akhir - t_awal) / 3600.0, koin.len(), n_terpotong, lahir_ok.len());

    // V0 pembanding: beli semua koin hidup di umur 2 menit (tanpa seleksi)
    let v0: Vec<Sinyal> = lahir_ok
        .iter()
        .filter_map(|(m, k)| {
            let t = k.lahir + 120.0;
            let f = fitur(k, t);
            (f.riil >= 2.0 && !f.dev_jual && f.bundle < 3).then_some((t, *m, *k))
        })
        .collect();
    // V1 aturan gelombang
    let mut v1: Vec<Sinyal> = Vec::new();
    for (m, k) in &lahir_ok {
        let mut t = k.lahir + umur_min();
        while t <= k.lahir + UMUR_MAKS {
            if sinyal_masuk(&fitur(k, t)) {
                v1.push((t, *m, *k));
                break;
            }
            t += LANGKAH;
        }
    }

    // diagnosa radar: di titik cek yang ramai (>= 10 pembeli unik/60 dtk), berapa koin lolos tiap syarat
    {
        let mut lolos = [0usize; 8];
        let mut n_ramai = 0usize;
        for (_, k) in &lahir_ok {
            let mut pernah = [false; 8];
            let mut ramai = false;
            let mut t = k.lahir + umur_min();
            while t <= k.lahir + UMUR_MAKS {
                let f = fitur(k, t);
                if f.pembeli >= 10 {
                    ramai = true;
                    let c = [true, f.pembeli as f64 >= 1.5 * f.pembeli_sebelum as f64, f.baru >= 0.5, f.beli_sol >= 2.0 * f.jual_sol,
                        f.top <= 0.30, f.riil >= 2.0, !f.dev_jual, f.bundle < 3];
                    for (p, ok) in pernah.iter_mut().zip(c) {
                        *p |= ok;
                    }
                }
                t += LANGKAH;
            }
            if ramai {
                n_ramai += 1;
                for j in 0..8 {
                    lolos[j] += pernah[j] as usize;
                }
            }
        }
        let s: Vec<String> = SYARAT.iter().zip(lolos).skip(1).map(|(n, c)| format!("{n} {c}")).collect();
        println!("diagnosa screening: koin pernah ramai (>=10 pembeli/60 dtk) {n_ramai} | koin yang pernah lolos syarat (saat ramai): {} | lolos SEMUA {}",
            s.join(", "), v1.len());
    }

    // dataset ML: tiap 30 dtk selama ada aktivitas (>= 3 trade dalam 60 dtk); label = hasil trade bila masuk di titik itu
    let mut baris: Vec<ml::Baris> = Vec::new();
    for (m, k) in &lahir_ok {
        let mut t = k.lahir + umur_min();
        while t <= k.lahir + UMUR_MAKS {
            let aktif = k.ev.iter().filter(|e| e.t > t - JENDELA && e.t <= t).count();
            if aktif >= 3 {
                if let Some(h) = trade(m, k, t, ukuran()) {
                    // label: default "trade ends above cost" (ret > 1); LABEL_ROKET=x: rocket = the price peaks at >= x times
                    // the fill price (state when our order lands) within 30 minutes (positive-skew target, outcome only)
                    let y = match std::env::var("LABEL_ROKET").ok().and_then(|v| v.parse::<f64>().ok()) {
                        Some(x) => {
                            let tf = t + jeda();
                            let p0 = k.ev.iter().rev().find(|e| e.t <= tf).map_or(0.0, |e| e.px);
                            let peak = k.ev.iter().filter(|e| e.t > tf && e.t <= tf + 1800.0).map(|e| e.px).fold(0.0, f64::max);
                            p0 > 0.0 && peak >= x * p0
                        }
                        None => h.ret > 1.0,
                    };
                    baris.push(ml::Baris { hari: lipat(k.lahir), mint: (*m).clone(), t, keluar: h.keluar, x: ml::fitur_ml(k, t), xk: ml::fitur_kandidat(k, t), ret: h.ret, y });
                }
            }
            t += LANGKAH;
        }
    }
    // Cross-coin candidate features (causal): only coins whose 8-hour data window ENDED before t are used.
    // smart wallet = realized PnL (sell SOL - buy SOL) > 0.05 SOL summed over >= 2 finished coins (Marino et al. "successful traders").
    {
        const SELESAI: f64 = 8.0 * 3600.0;
        // copycat (arXiv 2609.10246): an EARLIER-born coin in the data has the same name or symbol (causal: birth order)
        let mut lahir_urut: Vec<(&String, &Koin)> = koin.iter().filter(|(_, k)| k.lahir > 0.0).collect();
        lahir_urut.sort_by(|a, b| a.1.lahir.partial_cmp(&b.1.lahir).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(b.0)));
        let mut seen_n: HashSet<&str> = HashSet::new();
        let mut seen_s: HashSet<&str> = HashSet::new();
        let mut copycat: HashMap<&str, f64> = HashMap::new();
        let mut lahir_all: Vec<f64> = koin.values().filter(|k| k.lahir > 0.0).map(|k| k.lahir).collect();
        lahir_all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut trade_all: Vec<f64> = koin.values().flat_map(|k| k.ev.iter().map(|e| e.t)).collect();
        trade_all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for (m, k) in &lahir_urut {
            let hit = (!k.nama.is_empty() && seen_n.contains(k.nama.as_str())) || (!k.simbol.is_empty() && seen_s.contains(k.simbol.as_str()));
            copycat.insert(m.as_str(), if hit { 1.0 } else { 0.0 });
            if !k.nama.is_empty() {
                seen_n.insert(k.nama.as_str());
            }
            if !k.simbol.is_empty() {
                seen_s.insert(k.simbol.as_str());
            }
        }
        // per finished coin: wallet -> (pnl SOL, return ratio sell/buy - 1 when it bought)
        let mut ringkas: Vec<(f64, &str, HashMap<&str, (f64, Option<f64>)>)> = koin
            .values()
            .filter(|k| k.lahir > 0.0)
            .map(|k| {
                let mut bs: HashMap<&str, (f64, f64)> = HashMap::new();
                for e in &k.ev {
                    let x = bs.entry(e.user.as_str()).or_insert((0.0, 0.0));
                    if e.beli { x.0 += e.sol } else { x.1 += e.sol }
                }
                let pnl = bs.into_iter().map(|(w, (b, s))| (w, (s - b, if b > 0.0 { Some(s / b - 1.0) } else { None }))).collect();
                (k.lahir + SELESAI, k.creator.as_str(), pnl)
            })
            .collect();
        ringkas.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut urut: Vec<usize> = (0..baris.len()).collect();
        urut.sort_by(|a, b| baris[*a].t.partial_cmp(&baris[*b].t).unwrap_or(std::cmp::Ordering::Equal));
        // wallet -> (sum pnl, coins, sum r, sum r^2, coins with r)
        let mut dompet: HashMap<&str, (f64, u32, f64, f64, u32)> = HashMap::new();
        let mut creator_n: HashMap<&str, u32> = HashMap::new();
        let mut p = 0;
        for i in urut {
            let t = baris[i].t;
            while p < ringkas.len() && ringkas[p].0 <= t {
                for (w, v) in &ringkas[p].2 {
                    let d = dompet.entry(w).or_insert((0.0, 0, 0.0, 0.0, 0));
                    d.0 += v.0;
                    if let Some(r) = v.1 {
                        d.2 += r;
                        d.3 += r * r;
                        d.4 += 1;
                    }
                    d.1 += 1;
                }
                *creator_n.entry(ringkas[p].1).or_insert(0) += 1;
                p += 1;
            }
            let Some(k) = koin.get(&baris[i].mint) else { continue };
            let buyers: HashSet<&str> = k.ev.iter().filter(|e| e.t <= t && e.beli).map(|e| e.user.as_str()).collect();
            let smart = buyers.iter().filter(|w| dompet.get(*w).is_some_and(|d| d.0 > 0.05 && d.1 >= 2)).count() as f64;
            // t-stat of the wallet's per-coin returns > 1.645 over >= 3 finished coins
            let smart_t = buyers.iter().filter(|w| dompet.get(*w).is_some_and(|d| {
                let n = d.4 as f64;
                if d.4 < 3 { return false; }
                let m = d.2 / n;
                let sd = ((d.3 / n - m * m).max(0.0) * n / (n - 1.0)).sqrt();
                sd > 0.0 && m / (sd / n.sqrt()) > 1.645
            })).count() as f64;
            let share = if buyers.is_empty() { 0.0 } else { smart / buyers.len() as f64 };
            let prev = *creator_n.get(k.creator.as_str()).unwrap_or(&0) as f64;
            let cc = *copycat.get(baris[i].mint.as_str()).unwrap_or(&0.0);
            let gl = lahir_all.partition_point(|x| *x <= t) - lahir_all.partition_point(|x| *x <= t - 600.0);
            let gt = trade_all.partition_point(|x| *x <= t) - trade_all.partition_point(|x| *x <= t - 60.0);
            baris[i].xk.extend([smart, share, prev, smart_t, cc, gl as f64, gt as f64]);
        }
    }
    // FITUR_PLUS=1 (pre-registered 6 Nov 2026 from the candidate test on 2-3 Nov data): append the 7 candidates that passed
    // both the edge test and the add-one test to the MODEL features. Default 0 = original 22 features.
    // ML_MIN_RIIL=x (pre-registered): train and test the ML only on rows we could actually trade (SOL in curve >= x).
    // Default 0 = all rows. Rows on micro-liquidity coins move 1.5x on one small buy and are not tradable at our size.
    if let Some(m) = std::env::var("ML_MIN_RIIL").ok().and_then(|v| v.parse::<f64>().ok()).filter(|m| *m > 0.0) {
        let before = baris.len();
        baris.retain(|b| b.x[12] >= m);
        println!("ML_MIN_RIIL={m}: baris ML {} -> {}", before, baris.len());
    }
    // FITUR_V2=1 (decided 6 Nov 2026 from the tradable-coin tests, ML_MIN_RIIL=2): drop `top` (removing it raised AUC) and
    // `lulus` (no effect) from the MODEL input only (ml::KOLOM_BUANG; rules still see them),
    // and add `logret300` (the only candidate that passed both the edge and the add-one test). Column indices stay fixed.
    if bendera("FITUR_V2") {
        let j = ml::NAMA_KANDIDAT.iter().position(|n| *n == "logret300").unwrap_or(0);
        let _ = ml::KOLOM_BUANG.set(vec![11, 21]);
        for b in baris.iter_mut() {
            let v = b.xk[j];
            b.x.push(v);
        }
        println!("FITUR_V2 aktif: buang top, lulus; tambah logret300");
    }
    // FITUR_V3=1 (pre-registered 6 Nov 2026 ~19:00 UTC from the Pump.fun tradable-coin candidate test, 3 folds): append the
    // 8 candidates that passed the edge test AND raised AUC on 3/3 folds. To be confirmed on unseen data (DBC/DAMM, holdout).
    // FITUR_V4=1 (6 Nov 2026 night): every candidate that passed the SOP somewhere — stable on 2 Pump days (vol60,
    // buy_p90_60, top10_flow60), strong on Pump 6 Nov (sniper_hold_pct, early10_hold_pct, fresh_sol_share60,
    // early_exit_share, ofi_autocorr120, since_big_sell, median_buy60) and strong on LaunchLab (holders_in_profit,
    // supply_in_profit, local_peaks300, flip300, trade_gap_med60). Added to the MODEL input (CatBoost + MLP).
    if bendera("FITUR_V4") {
        const V4: [&str; 15] = ["vol60", "buy_p90_60", "top10_flow60", "sniper_hold_pct", "early10_hold_pct", "fresh_sol_share60",
            "early_exit_share", "ofi_autocorr120", "since_big_sell", "median_buy60", "holders_in_profit", "supply_in_profit",
            "local_peaks300", "flip300", "trade_gap_med60"];
        let idx: Vec<usize> = V4.iter().filter_map(|n| ml::NAMA_KANDIDAT.iter().position(|m| m == n)).collect();
        for b in baris.iter_mut() {
            let add: Vec<f64> = idx.iter().map(|i| b.xk[*i]).collect();
            b.x.extend(add);
        }
        println!("FITUR_V4 aktif: model memakai {} fitur", baris.first().map_or(0, |b| b.x.len()));
    }
    if bendera("FITUR_V3") {
        const V3: [&str; 8] = ["buyvol_accel", "logret30", "holders", "since_big_sell", "trade_gap_med60", "same_slot_share60",
            "whale_buy_share60", "buy_p90_60"];
        let idx: Vec<usize> = V3.iter().filter_map(|n| ml::NAMA_KANDIDAT.iter().position(|m| m == n)).collect();
        for b in baris.iter_mut() {
            let add: Vec<f64> = idx.iter().map(|i| b.xk[*i]).collect();
            b.x.extend(add);
        }
        println!("FITUR_V3 aktif: + {:?}", V3);
    }
    if bendera("FITUR_PLUS") {
        const PLUS: [&str; 7] = ["efficiency60", "unique_buy_ratio", "flip300", "vwap300_dev", "holders", "sol_per_trade", "early10_hold_pct"];
        let idx: Vec<usize> = PLUS.iter().filter_map(|n| ml::NAMA_KANDIDAT.iter().position(|m| m == n)).collect();
        for b in baris.iter_mut() {
            let add: Vec<f64> = idx.iter().map(|i| b.xk[*i]).collect();
            b.x.extend(add);
        }
        println!("FITUR_PLUS aktif: model memakai {} fitur (22 + {:?})", baris.first().map_or(0, |b| b.x.len()), PLUS);
    }
    let mut hari: Vec<i64> = baris.iter().map(|b| b.hari).collect::<HashSet<_>>().into_iter().collect();
    hari.sort();
    println!("baris dataset ML {} | hari {:?}", baris.len(), hari.iter().map(|d| nama_lipat(*d)).collect::<Vec<_>>());
    // Kelly prior (KELLY_DATA=1): outcomes of screened coins in the FIRST fold only (never a test fold for the ML trades)
    if let Some(d0) = hari.first() {
        let prior: Vec<f64> = baris.iter().filter(|b| b.hari == *d0 && b.x[15] == 0.0 && b.x[16] < 3.0 && b.x[12] >= 2.0 && b.x[1] >= 10.0 && b.x[11] <= 0.3).map(|b| b.ret).collect();
        let f0 = if prior.len() >= 20 { kelly_kuart(&prior) } else { 0.0 };
        if bendera("KELLY_DATA") {
            println!("Kelly dari data latih: {} trade awal, rata {:+.1}%, 1/4 Kelly awal {:.1}%", prior.len(), 100.0 * prior.iter().map(|x| x - 1.0).sum::<f64>() / prior.len().max(1) as f64, 100.0 * f0);
        }
        let _ = KELLY_PRIOR.set(prior);
    }

    // V2 ML+DL walk-forward: latih ulang tiap hari dengan hari-hari sebelumnya saja (purge: hasil harus selesai sebelum hari uji)
    let peta: HashMap<&String, &Koin> = lahir_ok.iter().map(|(m, k)| (*m, *k)).collect();
    let mut v2: Vec<Sinyal> = Vec::new();
    let mut hari_uji: Vec<i64> = Vec::new();
    // skor ML+DL tiap titik uji (mint, waktu) dan ambang per hari: dipakai V3 (radar + ML/DL)
    let mut skor_peta: HashMap<(String, u64), f64> = HashMap::new();
    let mut ambang_hari: HashMap<i64, f64> = HashMap::new();
    let mut ambang_b: HashMap<i64, f64> = HashMap::new();
    let mut v2b: Vec<Sinyal> = Vec::new();
    let bin =std::env::var("CATBOOST_BIN").unwrap_or_else(|_| "/opt/rhc/bin/catboost".into());
    for d in hari.iter().skip(1) {
        let mulai = baris.iter().filter(|b| b.hari == *d).map(|b| b.t).fold(f64::INFINITY, f64::min);
        let latih0: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari < *d && b.keluar < mulai).collect();
        // ACAK_LABEL=1: label latih diacak (uji serangan): AUC hari uji harus turun ke ~0,5
        let acak = if acak_label { ml::acak_label(&latih0, 99 + *d as u64 + benih()) } else { Vec::new() };
        let latih: Vec<&ml::Baris> = if acak_label { acak.iter().collect() } else { latih0 };
        let uji: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari == *d).collect();
        let pos = latih.iter().filter(|b| b.y).count();
        if latih.len() < 200 || pos < 20 {
            println!("hari {}: data latih kurang ({} baris, {} positif) -> tanpa trade ML", nama_lipat(*d), latih.len(), pos);
            continue;
        }
        let ux: Vec<Vec<f64>> = uji.iter().map(|b| b.x.clone()).collect();
        let Some((cl, cu)) = ml::catboost(&bin, &format!("/tmp/sol_cb_{d}"), &latih, &ux) else {
            println!("hari {}: CatBoost gagal -> tanpa trade ML", nama_lipat(*d));
            continue;
        };
        if cu.len() != uji.len() || cl.len() != latih.len() {
            println!("hari {}: prediksi CatBoost tidak lengkap ({} dari {}) -> lipatan dilewati", nama_lipat(*d), cu.len(), uji.len());
            continue;
        }
        let (dl, du) = ml::mlp(&latih, &ux, 7 + *d as u64 + benih());
        let skor_l: Vec<f64> = cl.iter().zip(&dl).map(|(a, b)| (a + b) / 2.0).collect();
        let skor_u: Vec<f64> = cu.iter().zip(&du).map(|(a, b)| (a + b) / 2.0).collect();
        let mut q = skor_l.clone();
        q.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let thr = q[((q.len() as f64 * 0.9) as usize).min(q.len() - 1)].max(0.5);
        let y: Vec<bool> = uji.iter().map(|b| b.y).collect();
        println!("hari {}: latih {} baris ({} positif), uji {} baris | AUC uji: CatBoost {:.3}, DL {:.3}, gabungan {:.3} | ambang {:.3}",
            nama_lipat(*d), latih.len(), pos, uji.len(), ml::auc(&cu, &y), ml::auc(&du, &y), ml::auc(&skor_u, &y), thr);
        hari_uji.push(*d);
        ambang_hari.insert(*d, thr);
        for (b, s) in uji.iter().zip(&skor_u) {
            skor_peta.insert((b.mint.clone(), b.t.to_bits()), *s);
        }
        println!("   baris uji dengan skor >= ambang: {}", skor_u.iter().filter(|s| **s >= thr).count());
        // V2b (pre-registered): same model, cut = top 10% of TRAINING scores, without the 0.5 floor
        // V2b cut (calibrated): train on the oldest 80% of the training rows (purged), score the newest 20%
        // (out-of-sample, time-ordered validation), cut = top 10% of THOSE scores. No 0.5 floor.
        let thr_b = {
            let mut tr: Vec<&ml::Baris> = latih.clone();
            tr.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
            let s = (tr.len() as f64 * 0.8) as usize;
            let v_start = tr[s].t;
            let part: Vec<&ml::Baris> = tr[..s].iter().filter(|b| b.keluar < v_start).cloned().collect();
            let val: Vec<Vec<f64>> = tr[s..].iter().map(|b| b.x.clone()).collect();
            let cb = ml::catboost(&bin, &format!("/tmp/sol_cv_{d}"), &part, &val).map(|(_, v)| v);
            let (_, dv) = ml::mlp(&part, &val, 11 + *d as u64 + benih());
            match cb {
                Some(cv) if part.len() >= 100 => {
                    let mut qv: Vec<f64> = cv.iter().zip(&dv).map(|(a, b)| (a + b) / 2.0).collect();
                    qv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    qv[((qv.len() as f64 * 0.9) as usize).min(qv.len() - 1)]
                }
                _ => f64::INFINITY, // cannot calibrate -> no ML trades (fail closed)
            }
        };
        println!("   ambang V2b (validasi waktu, top 10%): {thr_b:.3}");
        ambang_b.insert(*d, thr_b);
        // edge check: mean return of test rows in score buckets (cuts from training scores only)
        // ranking quality on the test fold (cut = quantile of the TEST scores; diagnosis only, not a trading rule):
        // share of positive labels (rockets when LABEL_ROKET is set) in the top 5/10/20% vs all rows
        let mut qt = skor_u.clone();
        qt.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for (lbl, p) in [("top5%", 0.95), ("top10%", 0.9), ("top20%", 0.8), ("semua", 0.0)] {
            let c = if p > 0.0 { qt[((qt.len() as f64 * p) as usize).min(qt.len() - 1)] } else { f64::NEG_INFINITY };
            let sel: Vec<&&ml::Baris> = uji.iter().zip(&skor_u).filter(|(_, s)| **s >= c).map(|(b, _)| b).collect();
            let r: Vec<f64> = sel.iter().map(|b| b.ret - 1.0).collect();
            println!("   skor {lbl}: baris {} | label positif {:.1}% | rata hasil {:+.1}% | menang {:.1}%", r.len(),
                100.0 * sel.iter().filter(|b| b.y).count() as f64 / sel.len().max(1) as f64,
                100.0 * r.iter().sum::<f64>() / r.len().max(1) as f64, 100.0 * r.iter().filter(|x| **x > 0.0).count() as f64 / r.len().max(1) as f64);
        }
        let mut sudah_b: HashSet<&String> = HashSet::new();
        let mut urut_b: Vec<usize> = (0..uji.len()).collect();
        urut_b.sort_by(|a, b| uji[*a].t.partial_cmp(&uji[*b].t).unwrap_or(std::cmp::Ordering::Equal));
        for i in urut_b {
            let b = uji[i];
            if skor_u[i] >= thr_b && b.x[15] == 0.0 && b.x[16] < 3.0 && b.x[12] >= 2.0 {
                if let Some((m, k)) = peta.get_key_value(&b.mint) {
                    if sudah_b.insert(*m) {
                        v2b.push((b.t, *m, *k));
                    }
                }
            }
        }
        let mut sudah: HashSet<&String> = HashSet::new();
        let mut urut: Vec<usize> = (0..uji.len()).collect();
        urut.sort_by(|a, b| uji[*a].t.partial_cmp(&uji[*b].t).unwrap_or(std::cmp::Ordering::Equal));
        for i in urut {
            let b = uji[i];
            // saringan keamanan tetap berlaku: dev belum jual, bukan bundle, likuiditas cukup
            if skor_u[i] >= thr && b.x[15] == 0.0 && b.x[16] < 3.0 && b.x[12] >= 2.0 {
                if let Some((m, k)) = peta.get_key_value(&b.mint) {
                    if sudah.insert(*m) {
                        v2.push((b.t, *m, *k));
                    }
                }
            }
        }
    }
    fn di_hari<'a>(v: &[Sinyal<'a>], hari: &[i64]) -> Vec<Sinyal<'a>> {
        v.iter().filter(|s| hari.contains(&lipat(s.2.lahir))).cloned().collect()
    }
    // V3 RADAR + ML/DL (seperti gerbang sistem: radar dulu, lalu skor model): titik pertama per koin yang lolos SEMUA syarat
    // gelombang DAN skor ML+DL >= ambang hari itu. Model hanya dilatih dari hari sebelumnya (walk-forward).
    let (mut n_radar, mut n_tanpa_skor) = (0usize, 0usize);
    let mut v3: Vec<Sinyal> = Vec::new();
    for (m, k) in &lahir_ok {
        let Some(thr) = ambang_hari.get(&lipat(k.lahir)) else { continue };
        let mut t = k.lahir + umur_min();
        while t <= k.lahir + UMUR_MAKS {
            if sinyal_masuk(&fitur(k, t)) {
                n_radar += 1;
                match skor_peta.get(&((*m).clone(), t.to_bits())) {
                    Some(s) if s >= thr => {
                        v3.push((t, *m, *k));
                        break;
                    }
                    Some(_) => {}
                    None => n_tanpa_skor += 1,
                }
            }
            t += LANGKAH;
        }
    }
    println!("V3: titik lolos screening di hari uji {n_radar}, tanpa skor ML {n_tanpa_skor}, lolos screening+ML {}", v3.len());
    // Pre-registered ML variants without the 0.5 floor (cut = top 10% of training scores):
    // V3b = full wave radar + ML/DL; V3c = loose radar (active >= 10 buyers/60 s, buy:sell >= 2:1, safety) + ML/DL decides
    let radar_plus_ml = |radar: &dyn Fn(&Fitur) -> bool| -> Vec<Sinyal> {
        let mut out: Vec<Sinyal> = Vec::new();
        for (m, k) in &lahir_ok {
            let Some(thr) = ambang_b.get(&lipat(k.lahir)) else { continue };
            let mut t = k.lahir + umur_min();
            while t <= k.lahir + UMUR_MAKS {
                if radar(&fitur(k, t)) && skor_peta.get(&((*m).clone(), t.to_bits())).is_some_and(|s| s >= thr) {
                    out.push((t, *m, *k));
                    break;
                }
                t += LANGKAH;
            }
        }
        out
    };
    let v3b = radar_plus_ml(&sinyal_masuk);
    let v3c = radar_plus_ml(&|f: &Fitur| f.pembeli >= 10 && f.beli_sol >= 2.0 * f.jual_sol && f.riil >= 2.0 && !f.dev_jual && f.bundle < 3);
    // V3d (pre-registered from the tradable winners map): safety + >= 10 buyers + dispersed buying (top <= 30%), NO 2:1 rule
    let v3d = radar_plus_ml(&|f: &Fitur| f.pembeli >= 10 && f.top <= 0.30 && f.riil >= 2.0 && !f.dev_jual && f.bundle < 3);
    println!("V2b sinyal {} | V3b sinyal {} | V3c sinyal {} | V3d sinyal {}", v2b.len(), v3b.len(), v3c.len(), v3d.len());
    println!("\nVARIAN DIUJI: 15 (V0, V1, V2, V2b, V3, V3b, V3c, V3d + biaya x2 untuk V1/V2/V2b/V3/V3b/V3c/V3d). Semua dilaporkan.");
    portofolio("V0 DASAR: beli semua koin hidup di umur 2 menit (semua hari)", &v0);
    portofolio("V1 SCREENING GELOMBANG (semua hari)", &v1);
    portofolio("V0 DASAR (hari uji ML saja)", &di_hari(&v0, &hari_uji));
    portofolio("V1 SCREENING GELOMBANG (hari uji ML saja)", &di_hari(&v1, &hari_uji));
    portofolio("V2 ML+DL ADAPTIF walk-forward (hari uji ML)", &v2);
    portofolio("V3 SCREENING GELOMBANG + ML/DL (hari uji ML)", &v3);
    portofolio("V2b ML+DL top 10% tanpa lantai 0,5 (hari uji ML)", &v2b);
    portofolio("V3b SCREENING GELOMBANG + ML/DL top 10% (hari uji ML)", &v3b);
    portofolio("V3c SCREENING LONGGAR (aktif, beli:jual>=2, aman) + ML/DL top 10% (hari uji ML)", &v3c);
    portofolio("V3d SCREENING DATA (aman, >=10 pembeli, top<=30%, TANPA 2:1) + ML/DL top 10% (hari uji ML)", &v3d);
    BIAYA_KALI.store(2.0f64.to_bits(), AO::Relaxed);
    portofolio("V1 SCREENING GELOMBANG, BIAYA x2", &v1);
    portofolio("V2 ML+DL, BIAYA x2", &v2);
    portofolio("V3 SCREENING + ML/DL, BIAYA x2", &v3);
    portofolio("V2b, BIAYA x2", &v2b);
    portofolio("V3b, BIAYA x2", &v3b);
    portofolio("V3c, BIAYA x2", &v3c);
    portofolio("V3d, BIAYA x2", &v3d);
    BIAYA_KALI.store(1.0f64.to_bits(), AO::Relaxed);

    // ===== UJI FITUR =====
    println!("\n===== UJI SYARAT ATURAN GELOMBANG (buang satu syarat; biaya normal) =====");
    let (n_dasar, w_dasar, e_dasar) = portofolio_x("", &v1, false);
    println!("{:<28} trade {:>4} | menang {:>5.1}% | $30 -> ${:>9.2}", "SEMUA SYARAT (V1)", n_dasar, 100.0 * w_dasar, e_dasar);
    for (j, nama) in SYARAT.iter().enumerate() {
        let mut v: Vec<Sinyal> = Vec::new();
        for (m, k) in &lahir_ok {
            let mut t = k.lahir + umur_min();
            while t <= k.lahir + UMUR_MAKS {
                if sinyal_tanpa(&fitur(k, t), j) {
                    v.push((t, *m, *k));
                    break;
                }
                t += LANGKAH;
            }
        }
        let (n, w, e) = portofolio_x("", &v, false);
        let vonis = if (e - e_dasar).abs() < 0.005 { "tidak berpengaruh di data ini" } else if e > e_dasar { "syarat ini MERUGIKAN (tanpa dia lebih baik)" } else { "syarat ini membantu" };
        println!("tanpa {:<22} trade {:>4} | menang {:>5.1}% | $30 -> ${:>9.2} | {vonis}", nama, n, 100.0 * w, e);
    }

    println!("\n===== DAYA PISAH TIAP FITUR (AUC sendirian di semua hari uji; 0,5 = tidak berguna) =====");
    let uji_semua: Vec<&ml::Baris> = baris.iter().filter(|b| hari_uji.contains(&b.hari)).collect();
    let y_semua: Vec<bool> = uji_semua.iter().map(|b| b.y).collect();
    for (j, nama) in ml::NAMA_FITUR.iter().enumerate() {
        let x: Vec<f64> = uji_semua.iter().map(|b| b.x[j]).collect();
        let a = ml::auc(&x, &y_semua);
        println!("{:<20} AUC {:.3} ({})", nama, a, if a.is_nan() { "tidak bisa dihitung: tidak ada hari uji / satu kelas saja" } else if (a - 0.5).abs() < 0.03 { "LEMAH" } else if a > 0.5 { "tinggi = lebih sering menang" } else { "tinggi = lebih sering kalah" });
    }

    // Single-feature AUC on ALL rows and per day. No model is trained here, so no train/test leakage;
    // a feature is only "stable" if its direction (above/below 0.5) is the same on every day.
    println!("\n===== AUC TIAP FITUR, SEMUA BARIS + PER HARI (tanpa model; 0,5 = tidak berguna) =====");
    let y_all: Vec<bool> = baris.iter().map(|b| b.y).collect();
    println!("baris {} | menang (ret>1) {} | hari {}", baris.len(), y_all.iter().filter(|v| **v).count(),
        hari.iter().map(|d| format!("{}:{}", nama_lipat(*d), baris.iter().filter(|b| b.hari == *d).count())).collect::<Vec<_>>().join(" "));
    for (j, nama) in ml::NAMA_FITUR.iter().enumerate() {
        let x: Vec<f64> = baris.iter().map(|b| b.x[j]).collect();
        let a = ml::auc(&x, &y_all);
        let per_day: Vec<f64> = hari.iter().map(|d| {
            let r: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari == *d).collect();
            ml::auc(&r.iter().map(|b| b.x[j]).collect::<Vec<_>>(), &r.iter().map(|b| b.y).collect::<Vec<_>>())
        }).collect();
        let valid: Vec<f64> = per_day.iter().cloned().filter(|v| !v.is_nan()).collect();
        let stable = valid.len() >= 2 && (valid.iter().all(|v| *v > 0.5) || valid.iter().all(|v| *v < 0.5));
        let verdict = if (a - 0.5).abs() < 0.03 { "LEMAH" } else if valid.len() < 2 { "kuat? (baru 1 hari)" } else if stable { "STABIL" } else { "TIDAK STABIL (arah berubah antar hari)" };
        println!("{:<20} AUC {:.3} | per hari {} | {}", nama, a,
            per_day.iter().map(|v| if v.is_nan() { "-".to_string() } else { format!("{v:.3}") }).collect::<Vec<_>>().join(" "), verdict);
    }

    // ===== LOCKED TEST (pre-registered 6 Nov 2026 before opening LaunchLab 3 Nov): UJI_TERKUNCI=1 =====
    // entry: SOL in curve >= 5, buyers 60 s >= 5, buy:sell SOL ratio >= 2, dev has not sold, no bundle, ML off
    // exit: K7 (dev sells or a top-3 holder sells; 6 h limit). Opened ONCE.
    if bendera("UJI_TERKUNCI") {
        println!("\n===== UJI TERKUNCI: entry [kurva>=5, pembeli>=5, beli:jual>=2, aman] + keluar K7 =====");
        let simpan = KELUAR.load(AO::Relaxed);
        KELUAR.store(7, AO::Relaxed);
        let lolos = |b: &ml::Baris| b.x[15] == 0.0 && b.x[16] < 3.0 && b.x[12] >= 5.0 && b.x[1] >= 5.0 && b.x[7] >= 2.0;
        let mut rs: Vec<(i64, f64, String)> = Vec::new();
        for b in baris.iter().filter(|b| lolos(b)) {
            if let Some(k) = peta.get(&b.mint) {
                if let Some(h) = trade(&b.mint, k, b.t, ukuran()) {
                    rs.push((b.hari, h.ret - 1.0, b.mint.clone()));
                }
            }
        }
        let n = rs.len().max(1) as f64;
        let (gp, gl) = rs.iter().fold((0.0, 0.0), |a, r| if r.1 > 0.0 { (a.0 + r.1, a.1) } else { (a.0, a.1 - r.1) });
        let koin_unik = rs.iter().map(|r| r.2.as_str()).collect::<HashSet<_>>().len();
        println!("semua titik: n={} (koin unik {koin_unik}) | rata {:+.1}% | menang {:.0}% | PF {:.2}", rs.len(), 100.0 * rs.iter().map(|r| r.1).sum::<f64>() / n,
            100.0 * rs.iter().filter(|r| r.1 > 0.0).count() as f64 / n, if gl > 0.0 { gp / gl } else { f64::INFINITY });
        let mut per: BTreeMap<i64, (usize, f64)> = BTreeMap::new();
        for r in &rs {
            let p = per.entry(r.0).or_insert((0, 0.0));
            p.0 += 1;
            p.1 += r.1;
        }
        println!("per lipatan: {}", per.iter().map(|(d, (c, s))| format!("{} {:+.1}% (n={c})", nama_lipat(*d), 100.0 * s / *c as f64)).collect::<Vec<_>>().join(" | "));
        // portfolio: first passing point per coin, $30, 1/4 Kelly + compounding
        let mut seen: HashSet<&String> = HashSet::new();
        let mut sig: Vec<Sinyal> = Vec::new();
        let mut idx: Vec<&ml::Baris> = baris.iter().filter(|b| lolos(b)).collect();
        idx.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        for b in idx {
            if let Some((m, k)) = peta.get_key_value(&b.mint) {
                if seen.insert(*m) {
                    sig.push((b.t, *m, *k));
                }
            }
        }
        portofolio("UJI TERKUNCI K7 (1 trade per koin)", &sig);
        BIAYA_KALI.store(2.0f64.to_bits(), AO::Relaxed);
        portofolio("UJI TERKUNCI K7, BIAYA x2", &sig);
        BIAYA_KALI.store(1.0f64.to_bits(), AO::Relaxed);
        KELUAR.store(simpan, AO::Relaxed);
    }
    // ===== REFERENCE STRATEGIES (rules taken from the papers as-is, fixed before testing, no tuning) =====
    // Each: first time per coin the rule holds (owner safety rules always on), $30, 1/4 Kelly + compounding,
    // exits K5 (60 s) and a 10-minute hold (no take-profit).
    {
        println!("\n===== STRATEGI DARI REFERENSI (aturan dari paper, tanpa disetel) =====");
        let ix = |n: &str| ml::NAMA_KANDIDAT.iter().position(|m| *m == n).unwrap_or(0);
        let (i_smart, i_copy, i_lr300, i_lr30) = (ix("smart_t_buyers"), ix("copycat"), ix("logret300"), ix("logret30"));
        let aman_b = |b: &ml::Baris| b.x[15] == 0.0 && b.x[16] < 3.0 && b.x[12] >= 1.0;
        let aturan: Vec<(&str, Box<dyn Fn(&ml::Baris) -> bool>)> = vec![
            // Marino et al.: high curve SOL reached with few trades (fast accumulation) = higher graduation odds
            ("Marino: SOL kurva >= 40 dan <= 400 trade", Box::new(move |b: &ml::Baris| b.x[12] >= 40.0 && b.x[17] <= 400.0)),
            // Luo et al.: copy a statistically smart wallet (t-stat > 1.645 on finished coins) that bought this coin
            ("Luo: ada dompet pintar t-stat ikut beli", Box::new(move |b: &ml::Baris| b.xk.get(i_smart).is_some_and(|v| *v >= 1.0))),
            // Meme Coin Factories: originals graduate 9.2% vs copycats 0.86%: buy originals with real liquidity
            ("Factories: koin asli (bukan peniru) + SOL kurva >= 20", Box::new(move |b: &ml::Baris| b.xk.get(i_copy).is_some_and(|v| *v == 0.0) && b.x[12] >= 20.0)),
            // time-series momentum: 5-minute and 30-second returns both positive
            ("TSMOM: return 5 mnt > 0 dan 30 dtk > 0", Box::new(move |b: &ml::Baris| b.xk.get(i_lr300).is_some_and(|v| *v > 0.0) && b.xk.get(i_lr30).is_some_and(|v| *v > 0.0))),
        ];
        let simpan = (KELUAR.load(AO::Relaxed), TP_X.load(AO::Relaxed), SL_X.load(AO::Relaxed), HOLD_S.load(AO::Relaxed));
        for (nm, f) in &aturan {
            let mut idx: Vec<&ml::Baris> = baris.iter().filter(|b| aman_b(b) && f(b)).collect();
            idx.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
            let mut seen: HashSet<&String> = HashSet::new();
            let mut sig: Vec<Sinyal> = Vec::new();
            for b in idx {
                if let Some((m, k)) = peta.get_key_value(&b.mint) {
                    if seen.insert(*m) {
                        sig.push((b.t, *m, *k));
                    }
                }
            }
            for (lbl, mode, hold) in [("keluar 60 dtk", 5u8, 60.0f64), ("pegang 10 mnt", 6u8, 600.0f64)] {
                KELUAR.store(mode, AO::Relaxed);
                TP_X.store(f64::INFINITY.to_bits(), AO::Relaxed);
                SL_X.store(2.0f64.to_bits(), AO::Relaxed);
                HOLD_S.store(hold.to_bits(), AO::Relaxed);
                portofolio(&format!("REF {nm} | {lbl}"), &sig);
            }
        }
        KELUAR.store(simpan.0, AO::Relaxed);
        TP_X.store(simpan.1, AO::Relaxed);
        SL_X.store(simpan.2, AO::Relaxed);
        HOLD_S.store(simpan.3, AO::Relaxed);
    }
    // ===== EXIT TEST: every ML entry point re-simulated with each pre-registered exit mode =====
    println!("\n===== UJI CARA KELUAR (semua titik masuk dataset ML; ukuran 0,01 SOL; biaya normal) =====");
    let mode_awal = KELUAR.load(AO::Relaxed);
    let nama_mode = ["K0 asli: dev | jual besar 3%/10% | momentum", "K1: dev | jual besar 10%/25% | momentum", "K2: dev | momentum saja",
        "K3: dev | tekanan beli 30 dtk < 1:1", "K4: dev | tekanan < 1:1 + pembeli < 70% puncak", "K5: dev | pegang 60 dtk", "K6: (grid)", "K7: dev | top-3 holder jual", "K8: dev | top-3 holder jual | momentum", "K9: dev | SOL jual:beli 30 dtk >= R:1", "K10: dev | jumlah jual:beli 30 dtk >= R:1", "K11: potong rugi 60 dtk, pemenang ditahan sampai jual:beli >= R:1"];
    for (mode, nm) in nama_mode.iter().enumerate() {
        KELUAR.store(mode as u8, AO::Relaxed);
        let mut rs: Vec<f64> = Vec::new();
        let mut per: BTreeMap<i64, (usize, f64)> = BTreeMap::new();
        let mut why: BTreeMap<&str, usize> = BTreeMap::new();
        let mut hold = 0.0;
        for b in &baris {
            let Some(k) = peta.get(&b.mint) else { continue };
            if let Some(h) = trade(&b.mint, k, b.t, ukuran()) {
                rs.push(h.ret);
                hold += h.keluar - h.masuk;
                let p = per.entry(b.hari).or_insert((0, 0.0));
                p.0 += 1;
                p.1 += h.ret - 1.0;
                *why.entry(h.alasan).or_insert(0) += 1;
            }
        }
        let n = rs.len().max(1) as f64;
        let mean = rs.iter().map(|r| r - 1.0).sum::<f64>() / n;
        let mut srt = rs.clone();
        srt.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = srt.get(srt.len() / 2).map_or(0.0, |r| r - 1.0);
        let (gp, gl) = rs.iter().fold((0.0, 0.0), |a, r| if *r > 1.0 { (a.0 + r - 1.0, a.1) } else { (a.0, a.1 + 1.0 - r) });
        let (n0, w0, e0) = portofolio_x("", &v0, false);
        let (n1, w1, e1) = portofolio_x("", &v1, false);
        println!("{nm}\n   titik {} | rata hasil/trade {:+.1}% | median {:+.1}% | menang {:.1}% | PF {:.2} | pegang rata {:.0} dtk",
            rs.len(), 100.0 * mean, 100.0 * med, 100.0 * rs.iter().filter(|r| **r > 1.0).count() as f64 / n, if gl > 0.0 { gp / gl } else { f64::INFINITY }, hold / n);
        println!("   per lipatan (rata hasil): {}", per.iter().map(|(d, (c, s))| format!("{} {:+.1}% (n={c})", nama_lipat(*d), 100.0 * s / *c as f64)).collect::<Vec<_>>().join(" | "));
        println!("   alasan keluar: {}", why.iter().map(|(a, c)| format!("{a} {c}")).collect::<Vec<_>>().join(", "));
        println!("   portofolio $30: V0 {n0} trade menang {:.0}% -> ${e0:.2} | V1 radar {n1} trade menang {:.0}% -> ${e1:.2}", 100.0 * w0, 100.0 * w1);
    }
    KELUAR.store(mode_awal, AO::Relaxed);

    // ===== HONEST TUNING: nested walk-forward grid over entry (radar) x ML x exit (TP/SL/hold) =====
    // For each test fold the best config is chosen ONLY on earlier folds (mean return per entry, >= 30 entries,
    // 1 h purge), then applied once to the test fold. The out-of-sample result is the only number that counts.
    println!("\n===== PENYETELAN JUJUR (nested walk-forward: pilih di masa lalu, uji di periode berikutnya) =====");
    // Owner rule (6 Nov 2026): no take-profit and no trailing. Exits = dev sells, big sell, momentum weakens, buy pressure
    // drops, or a time limit (fixed hold, K6 with TP = infinity and no stop).
    let mut exits: Vec<(String, u8, f64, f64, f64)> = vec![("K0".into(), 0, 0.0, 0.0, 0.0), ("K1".into(), 1, 0.0, 0.0, 0.0),
        ("K2".into(), 2, 0.0, 0.0, 0.0), ("K3".into(), 3, 0.0, 0.0, 0.0), ("K4".into(), 4, 0.0, 0.0, 0.0),
        ("K7".into(), 7, 0.0, 0.0, 0.0), ("K8".into(), 8, 0.0, 0.0, 0.0)];
    // owner's momentum exits: sellers beat buyers R:1 within a W-second window (R and W are searched, not fixed)
    for m in [9u8, 10u8] {
        for r in [2.0, 3.0, 4.0, 5.0, 6.0, 8.0] {
            for w in [5.0, 10.0, 15.0, 30.0] {
                exits.push((format!("K{m} {r:.0}:1 {w:.0}s"), m, r, w, 0.0));
            }
        }
    }
    // K11 (cut losers at 60 s unless up >= a, ride winners until sellers beat buyers R:1 in W s): a in the hold slot
    for a in [0.05, 0.10, 0.20] {
        for r in [2.0, 4.0, 6.0] {
            for w in [10.0, 30.0] {
                exits.push((format!("K11 +{:.0}% {r:.0}:1 {w:.0}s", a * 100.0), 11, r, w, a));
            }
        }
    }
    for hold in [30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 300.0, 600.0] {
        exits.push((format!("pegang {hold:.0}s"), 6, f64::INFINITY, 2.0, hold));
    }
    let saved = (KELUAR.load(AO::Relaxed), TP_X.load(AO::Relaxed), SL_X.load(AO::Relaxed), HOLD_S.load(AO::Relaxed));
    // ret[exit][row]
    // per exit and row: (return, exit time); exit time is used to purge training rows that overlap the test fold
    let pair: Vec<Vec<(f64, f64)>> = exits.iter().map(|(_, m, tp, sl, hold)| {
        KELUAR.store(*m, AO::Relaxed);
        TP_X.store(tp.to_bits(), AO::Relaxed);
        SL_X.store(sl.to_bits(), AO::Relaxed);
        HOLD_S.store(hold.to_bits(), AO::Relaxed);
        let k910 = *m == 9 || *m == 10 || *m == 11;
        AMBANG_X.store(if *m == 11 { hold.to_bits() } else { 0 }, AO::Relaxed);
        RASIO_X.store(if k910 { tp.to_bits() } else { 0 }, AO::Relaxed);
        JENDELA_X.store(if k910 { sl.to_bits() } else { 0 }, AO::Relaxed);
        baris.iter().map(|b| peta.get(&b.mint).and_then(|k| trade(&b.mint, k, b.t, ukuran())).map_or((f64::NAN, f64::INFINITY), |h| (h.ret - 1.0, h.keluar))).collect()
    }).collect();
    RASIO_X.store(0, AO::Relaxed);
    JENDELA_X.store(0, AO::Relaxed);
    AMBANG_X.store(0, AO::Relaxed);
    let ret: Vec<Vec<f64>> = pair.iter().map(|v| v.iter().map(|x| x.0).collect()).collect();
    let kel: Vec<Vec<f64>> = pair.iter().map(|v| v.iter().map(|x| x.1).collect()).collect();
    KELUAR.store(saved.0, AO::Relaxed);
    TP_X.store(saved.1, AO::Relaxed);
    SL_X.store(saved.2, AO::Relaxed);
    HOLD_S.store(saved.3, AO::Relaxed);
    let ml_ok: Vec<Option<bool>> = baris.iter().map(|b| {
        let thr = ambang_b.get(&b.hari)?;
        skor_peta.get(&(b.mint.clone(), b.t.to_bits())).map(|s| s >= thr)
    }).collect();
    // entry configs: (min SOL in curve, min buyers 60 s, max top-buyer share, min buy:sell ratio, ML on)
    let mut entries: Vec<(f64, f64, f64, f64, bool)> = Vec::new();
    for r in [1.0, 2.0, 5.0] {
        for p in [0.0, 5.0, 10.0, 20.0] {
            for top in [1.0, 0.3] {
                for bs in [0.0, 1.0, 2.0] {
                    for ml in [false, true] {
                        entries.push((r, p, top, bs, ml));
                    }
                }
            }
        }
    }
    let pass = |i: usize, e: &(f64, f64, f64, f64, bool)| -> bool {
        let x = &baris[i].x;
        // owner safety rules always on: dev has not sold, no bundle
        x[15] == 0.0 && x[16] < 3.0 && x[12] >= e.0 && x[1] >= e.1 && x[11] <= e.2 && x[7] >= e.3 && (!e.4 || ml_ok[i] == Some(true))
    };
    let mean_of = |rows: &[usize], xi: usize, cut: f64| -> (usize, f64, f64) {
        let v: Vec<f64> = rows.iter().filter(|i| kel[xi][**i] < cut).map(|i| ret[xi][*i]).filter(|r| !r.is_nan()).collect();
        (v.len(), v.iter().sum::<f64>() / v.len().max(1) as f64, v.iter().filter(|r| **r > 0.0).count() as f64 / v.len().max(1) as f64)
    };
    let (mut oos_n, mut oos_sum, mut oos_win) = (0usize, 0.0, 0usize);
    for d in hari.iter().skip(1) {
        let mulai = baris.iter().filter(|b| b.hari == *d).map(|b| b.t).fold(f64::INFINITY, f64::min);
        let tr_rows: Vec<usize> = (0..baris.len()).filter(|i| baris[*i].hari < *d && baris[*i].t < mulai - 3600.0).collect();
        let te_rows: Vec<usize> = (0..baris.len()).filter(|i| baris[*i].hari == *d).collect();
        let mut best: Option<(f64, usize, usize, usize)> = None; // (mean, entry, exit, n)
        for (ei, e) in entries.iter().enumerate() {
            if e.4 && !tr_rows.iter().any(|i| ml_ok[*i].is_some()) {
                continue; // ML option needs earlier scored rows
            }
            let rows: Vec<usize> = tr_rows.iter().cloned().filter(|i| pass(*i, e)).collect();
            if rows.len() < 30 {
                continue;
            }
            for xi in 0..exits.len() {
                let (n, m, _) = mean_of(&rows, xi, mulai);
                if n >= 30 && best.is_none_or(|b| m > b.0) {
                    best = Some((m, ei, xi, n));
                }
            }
        }
        let Some((m_tr, ei, xi, n_tr)) = best else {
            println!("{}: tidak ada setelan dengan >= 30 titik latih", nama_lipat(*d));
            continue;
        };
        let e = &entries[ei];
        let rows: Vec<usize> = te_rows.iter().cloned().filter(|i| pass(*i, e)).collect();
        let (n, m, w) = mean_of(&rows, xi, f64::INFINITY);
        oos_n += n;
        oos_sum += m * n as f64;
        oos_win += (w * n as f64).round() as usize;
        println!("{}: dipilih [kurva>={} SOL, pembeli>={}, top<={}, beli:jual>={}, ML {}] + [{}] | latih n={n_tr} rata {:+.1}% | UJI n={n} rata {:+.1}% menang {:.0}%",
            nama_lipat(*d), e.0, e.1, e.2, e.3, if e.4 { "on" } else { "off" }, exits[xi].0, 100.0 * m_tr, 100.0 * m, 100.0 * w);
    }
    println!("HASIL DI LUAR SAMPEL (gabungan lipatan uji): titik {oos_n} | rata {:+.1}% per trade | menang {:.0}%",
        100.0 * oos_sum / oos_n.max(1) as f64, 100.0 * oos_win as f64 / oos_n.max(1) as f64);
    // for information only: the single best config on ALL data (in-sample, overfit by construction)
    let all_rows: Vec<usize> = (0..baris.len()).collect();
    let mut top_cfg: Vec<(f64, usize, usize, usize)> = Vec::new();
    for (ei, e) in entries.iter().enumerate() {
        let rows: Vec<usize> = all_rows.iter().cloned().filter(|i| pass(*i, e)).collect();
        if rows.len() < 30 {
            continue;
        }
        for xi in 0..exits.len() {
            let (n, m, _) = mean_of(&rows, xi, f64::INFINITY);
            if n >= 30 {
                top_cfg.push((m, ei, xi, n));
            }
        }
    }
    top_cfg.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    println!("(info, DALAM SAMPEL = terlalu optimis) 5 setelan terbaik dari {} kombinasi yang diuji:", top_cfg.len());
    for (m, ei, xi, n) in top_cfg.iter().take(5) {
        let e = &entries[*ei];
        println!("   [kurva>={} SOL, pembeli>={}, top<={}, beli:jual>={}, ML {}] + [{}] n={n} rata {:+.1}%", e.0, e.1, e.2, e.3, if e.4 { "on" } else { "off" }, exits[*xi].0, 100.0 * m);
    }

    // ===== LOSS DECOMPOSITION ("where does the money go"), on the screened entries V3d (one per coin, test folds) =====
    // peak  = best price reachable within 30 min after our fill / fill price (upper bound of the entry's potential)
    // gross0 = return of the actual exit rule with NO costs and NO latency; gross = no costs, real latency; net = real
    println!("\n===== BEDAH KERUGIAN (screening V3d, keluar mode {}, 1 per koin) =====", KELUAR.load(AO::Relaxed));
    {
        let mut rows: Vec<(f64, f64, f64, f64)> = Vec::new(); // (peak, gross0, gross, net)
        for (t, m, k) in &v3d {
            let Some(net) = trade(m, k, *t, ukuran()).map(|h| h.ret - 1.0) else { continue };
            NOL_X.store(true, AO::Relaxed);
            let gross = trade(m, k, *t, ukuran()).map_or(f64::NAN, |h| h.ret - 1.0);
            JEDA_NOL.store(true, AO::Relaxed);
            let gross0 = trade(m, k, *t, ukuran()).map_or(f64::NAN, |h| h.ret - 1.0);
            JEDA_NOL.store(false, AO::Relaxed);
            NOL_X.store(false, AO::Relaxed);
            let tf = t + jeda();
            let Some(p0) = k.ev.iter().rev().find(|e| e.t <= tf).map(|e| e.px) else { continue };
            let peak = k.ev.iter().filter(|e| e.t > tf && e.t <= tf + 1800.0).map(|e| e.px).fold(p0, f64::max) / p0;
            rows.push((peak, gross0, gross, net));
        }
        let n = rows.len().max(1) as f64;
        let avg = |f: &dyn Fn(&(f64, f64, f64, f64)) -> f64| rows.iter().map(f).filter(|x| x.is_finite()).sum::<f64>() / n;
        let mut pk: Vec<f64> = rows.iter().map(|r| r.0).collect();
        pk.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let capt: Vec<f64> = rows.iter().filter(|r| r.0 > 1.1 && r.1.is_finite()).map(|r| r.1 / (r.0 - 1.0)).collect();
        println!("trade {} | puncak setelah beli: median {:.2}x, >=1,5x {:.0}%, >=2x {:.0}%, >=5x {:.0}%",
            rows.len(), pk.get(pk.len() / 2).cloned().unwrap_or(0.0), 100.0 * pk.iter().filter(|p| **p >= 1.5).count() as f64 / n,
            100.0 * pk.iter().filter(|p| **p >= 2.0).count() as f64 / n, 100.0 * pk.iter().filter(|p| **p >= 5.0).count() as f64 / n);
        println!("rata per trade: tanpa biaya & tanpa jeda {:+.1}% | tanpa biaya (jeda nyata) {:+.1}% | bersih {:+.1}%",
            100.0 * avg(&|r| r.1), 100.0 * avg(&|r| r.2), 100.0 * avg(&|r| r.3));
        println!("-> hilang karena JEDA {:+.1}% | hilang karena BIAYA {:+.1}% | bagian puncak yang tertangkap (median, puncak>1,1x) {:.0}%",
            100.0 * (avg(&|r| r.2) - avg(&|r| r.1)), 100.0 * (avg(&|r| r.3) - avg(&|r| r.2)),
            100.0 * { let mut c = capt.clone(); c.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)); c.get(c.len() / 2).cloned().unwrap_or(f64::NAN) });
    }
    // CEPAT=1: stop here (portfolio PnL + exit test + honest tuning + loss decomposition); skips mining and research
    if bendera("CEPAT") {
        println!("\n(CEPAT=1: bagian riset berat dilewati)");
        return;
    }
    // ===== PATTERN MINING: entry = 1 or 2 feature conditions (top / bottom 20%), exit = any no-TP exit =====
    // Thresholds and the winning (pattern, exit) are chosen on EARLIER folds only (min 30 entries), then applied once
    // to the next fold. Owner safety rules always on (dev has not sold, no bundle, >= 1 SOL real liquidity).
    println!("\n===== PENAMBANGAN POLA (entry 1-2 fitur x keluar; pilih di masa lalu, uji di periode berikutnya) =====");
    let fnames: Vec<String> = ml::NAMA_FITUR.iter().map(|s| s.to_string()).chain(ml::NAMA_KANDIDAT.iter().map(|s| s.to_string())).collect();
    let fval = |b: &ml::Baris, f: usize| -> f64 { if f < 22 { b.x[f] } else { b.xk.get(f - 22).cloned().unwrap_or(f64::NAN) } };
    let nf = fnames.len();
    let safe = |b: &ml::Baris| b.x[15] == 0.0 && b.x[16] < 3.0 && b.x[12] >= 1.0;
    // pattern = list of (feature, side): side 0 = value <= q20, side 1 = value >= q80
    type Pola = Vec<(usize, u8)>;
    let mut v4_signals: Vec<(i64, usize, Vec<Sinyal>)> = Vec::new();
    let (mut on, mut osum, mut owin) = (0usize, 0.0, 0usize);
    for d in hari.iter().skip(1) {
        let mulai = baris.iter().filter(|b| b.hari == *d).map(|b| b.t).fold(f64::INFINITY, f64::min);
        let tr: Vec<usize> = (0..baris.len()).filter(|i| baris[*i].hari < *d && baris[*i].t < mulai - 3600.0 && safe(&baris[*i])).collect();
        let te: Vec<usize> = (0..baris.len()).filter(|i| baris[*i].hari == *d && safe(&baris[*i])).collect();
        if tr.len() < 60 {
            println!("{}: data latih aman terlalu sedikit ({})", nama_lipat(*d), tr.len());
            continue;
        }
        // thresholds from training rows only
        let q: Vec<(f64, f64)> = (0..nf).map(|f| {
            let mut v: Vec<f64> = tr.iter().map(|i| fval(&baris[*i], f)).filter(|x| x.is_finite()).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if v.is_empty() { (f64::NAN, f64::NAN) } else { (v[v.len() / 5], v[(v.len() * 4 / 5).min(v.len() - 1)]) }
        }).collect();
        let cond = |i: usize, f: usize, s: u8| -> bool {
            let x = fval(&baris[i], f);
            if s == 0 { x <= q[f].0 } else { x >= q[f].1 }
        };
        // condition masks on training rows
        let masks: Vec<Vec<bool>> = (0..nf * 2).map(|c| tr.iter().map(|i| cond(*i, c / 2, (c % 2) as u8)).collect()).collect();
        let mut best: Option<(f64, Pola, usize, usize)> = None;
        let mut eval = |m: &dyn Fn(usize) -> bool, p: Pola| {
            let rows: Vec<usize> = (0..tr.len()).filter(|k| m(*k)).collect();
            if rows.len() < 30 {
                return;
            }
            for xi in 0..exits.len() {
                let (mut s, mut n) = (0.0, 0usize);
                for k in &rows {
                    if kel[xi][tr[*k]] >= mulai {
                        continue; // purge: training outcome must end before the test fold starts
                    }
                    let r = ret[xi][tr[*k]];
                    if r.is_finite() {
                        s += r;
                        n += 1;
                    }
                }
                if n >= 30 && best.as_ref().is_none_or(|b| s / n as f64 > b.0) {
                    best = Some((s / n as f64, p.clone(), xi, n));
                }
            }
        };
        for c1 in 0..nf * 2 {
            let m1 = &masks[c1];
            eval(&|k| m1[k], vec![(c1 / 2, (c1 % 2) as u8)]);
            for c2 in (c1 / 2 + 1) * 2..nf * 2 {
                let m2 = &masks[c2];
                eval(&|k| m1[k] && m2[k], vec![(c1 / 2, (c1 % 2) as u8), (c2 / 2, (c2 % 2) as u8)]);
            }
        }
        let Some((m_tr, pola, xi, n_tr)) = best else { continue };
        let pass: Vec<usize> = te.iter().cloned().filter(|i| pola.iter().all(|(f, s)| cond(*i, *f, *s))).collect();
        let r: Vec<f64> = pass.iter().map(|i| ret[xi][*i]).filter(|x| x.is_finite()).collect();
        let (n, m) = (r.len(), r.iter().sum::<f64>() / r.len().max(1) as f64);
        let w = r.iter().filter(|x| **x > 0.0).count();
        on += n;
        osum += m * n as f64;
        owin += w;
        let desk: Vec<String> = pola.iter().map(|(f, s)| format!("{} {} {:.4}", fnames[*f], if *s == 0 { "<=" } else { ">=" }, if *s == 0 { q[*f].0 } else { q[*f].1 })).collect();
        println!("{}: POLA [{}] + [{}] | latih n={n_tr} rata {:+.1}% | UJI n={n} rata {:+.1}% menang {:.0}%",
            nama_lipat(*d), desk.join(" DAN "), exits[xi].0, 100.0 * m_tr, 100.0 * m, 100.0 * w as f64 / n.max(1) as f64);
        // portfolio signals for this fold: first passing point per coin
        let mut seen: HashSet<&String> = HashSet::new();
        let mut sig: Vec<Sinyal> = Vec::new();
        let mut order = pass.clone();
        order.sort_by(|a, b| baris[*a].t.partial_cmp(&baris[*b].t).unwrap_or(std::cmp::Ordering::Equal));
        for i in order {
            if let Some((mm, k)) = peta.get_key_value(&baris[i].mint) {
                if seen.insert(*mm) {
                    sig.push((baris[i].t, *mm, *k));
                }
            }
        }
        v4_signals.push((*d, xi, sig));
    }
    println!("HASIL POLA DI LUAR SAMPEL: titik {on} | rata {:+.1}% per trade | menang {:.0}%", 100.0 * osum / on.max(1) as f64, 100.0 * owin as f64 / on.max(1) as f64);
    // V4 = mined pattern installed in the trading system: $30, 1/4 Kelly + compounding, per test fold with its chosen exit
    for (d, xi, sig) in &v4_signals {
        let (_, m, tp, sl, hold) = &exits[*xi];
        let saved = (KELUAR.load(AO::Relaxed), TP_X.load(AO::Relaxed), SL_X.load(AO::Relaxed), HOLD_S.load(AO::Relaxed));
        KELUAR.store(*m, AO::Relaxed);
        TP_X.store(tp.to_bits(), AO::Relaxed);
        SL_X.store(sl.to_bits(), AO::Relaxed);
        HOLD_S.store(hold.to_bits(), AO::Relaxed);
        portofolio(&format!("V4 POLA TAMBANG, lipatan {} (keluar {})", nama_lipat(*d), exits[*xi].0), sig);
        BIAYA_KALI.store(2.0f64.to_bits(), AO::Relaxed);
        portofolio(&format!("V4 POLA TAMBANG, lipatan {}, BIAYA x2", nama_lipat(*d)), sig);
        BIAYA_KALI.store(1.0f64.to_bits(), AO::Relaxed);
        KELUAR.store(saved.0, AO::Relaxed);
        TP_X.store(saved.1, AO::Relaxed);
        SL_X.store(saved.2, AO::Relaxed);
        HOLD_S.store(saved.3, AO::Relaxed);
    }
    // ===== RESEARCH STEP A (market) + STEP B (owner's rules R1-R8), CLAUDE-CODE-FEATURE-RESEARCH.md =====
    // Outcome after decision time T (descriptive, uses the future ON PURPOSE because it IS the outcome):
    // peak = max price in (T, T+30 min] / price(T); end = price(T+30 min) / price(T). Crash = end < 0.5x.
    println!("\n===== RISET A: PASAR (per hari lahir, koin quote SOL, aman) =====");
    let px_at = |k: &Koin, s: f64| k.ev.iter().rev().find(|e| e.t <= s).map(|e| e.px);
    let alive = |k: &Koin, a: f64| k.ev.iter().any(|e| e.t > k.lahir + a - 60.0 && e.t <= k.lahir + a);
    let hasil = |k: &Koin, tt: f64| -> Option<(f64, f64)> {
        let p0 = px_at(k, tt)?;
        let peak = k.ev.iter().filter(|e| e.t > tt && e.t <= tt + 1800.0).map(|e| e.px).fold(p0, f64::max);
        Some((peak / p0, px_at(k, tt + 1800.0)? / p0))
    };
    let semua: Vec<(&String, &Koin)> = lahir_ok.clone();
    let mut per_day: BTreeMap<i64, Vec<&Koin>> = BTreeMap::new();
    for (_, k) in &semua {
        per_day.entry((k.lahir / 86400.0) as i64).or_default().push(*k);
    }
    for (d, ks) in &per_day {
        let n = ks.len();
        let surv: Vec<String> = [60.0, 180.0, 300.0, 600.0, 1800.0].iter().map(|a| format!("{:.0}m {:.1}%", a / 60.0, 100.0 * ks.iter().filter(|k| alive(k, *a)).count() as f64 / n as f64)).collect();
        let grad = ks.iter().filter(|k| k.lulus).count();
        let dev1 = ks.iter().filter(|k| k.ev.iter().any(|e| !e.beli && e.user == k.creator && e.t <= k.lahir + 60.0)).count();
        let dev3 = ks.iter().filter(|k| k.ev.iter().any(|e| !e.beli && e.user == k.creator && e.t <= k.lahir + 180.0)).count();
        println!("{} lahir {n} | hidup {} | lulus {grad} | dev jual <=1m {:.0}% <=3m {:.0}%", tanggal(*d), surv.join(" "), 100.0 * dev1 as f64 / n as f64, 100.0 * dev3 as f64 / n as f64);
    }
    // typical life: median trades/min and cumulative unique wallets at ages
    let ages = [60.0, 120.0, 180.0, 300.0, 600.0, 1800.0];
    let med = |mut v: Vec<f64>| -> f64 { v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)); v.get(v.len() / 2).cloned().unwrap_or(0.0) };
    let tpm: Vec<String> = ages.iter().map(|a| format!("{:.0}m {:.0}", a / 60.0, med(semua.iter().map(|(_, k)| k.ev.iter().filter(|e| e.t > k.lahir + a - 60.0 && e.t <= k.lahir + a).count() as f64).collect()))).collect();
    let uw: Vec<String> = ages.iter().map(|a| format!("{:.0}m {:.0}", a / 60.0, med(semua.iter().map(|(_, k)| k.ev.iter().filter(|e| e.t <= k.lahir + a).map(|e| e.user.as_str()).collect::<HashSet<_>>().len() as f64).collect()))).collect();
    println!("median trade/menit: {} | median dompet unik kumulatif: {}", tpm.join(" "), uw.join(" "));
    // creator sale before crash: coins alive at 3 min; P(crash in 30 min) if creator sold in (3 min, 10 min] vs not
    let at3: Vec<&Koin> = semua.iter().map(|(_, k)| *k).filter(|k| alive(k, 180.0)).collect();
    let (mut a1, mut c1, mut a0, mut c0) = (0, 0, 0, 0);
    for k in &at3 {
        let Some((_, end)) = hasil(k, k.lahir + 180.0) else { continue };
        let sold = k.ev.iter().any(|e| !e.beli && e.user == k.creator && e.t > k.lahir + 180.0 && e.t <= k.lahir + 600.0);
        if sold { a1 += 1; c1 += (end < 0.5) as usize } else { a0 += 1; c0 += (end < 0.5) as usize }
    }
    println!("koin hidup di 3m: {} | crash 30m bila dev jual di 3-10m: {:.0}% (n={a1}) vs tidak: {:.0}% (n={a0})", at3.len(), 100.0 * c1 as f64 / a1.max(1) as f64, 100.0 * c0 as f64 / a0.max(1) as f64);
    // wallets: early buyers (first 30 s) seen early in >= 5 coins = repeat early birds; outcome with vs without them
    let mut early_n: HashMap<&str, usize> = HashMap::new();
    for (_, k) in &semua {
        for w in k.ev.iter().filter(|e| e.beli && e.t <= k.lahir + 30.0 && e.user != k.creator).map(|e| e.user.as_str()).collect::<HashSet<_>>() {
            *early_n.entry(w).or_insert(0) += 1;
        }
    }
    let rep: HashSet<&str> = early_n.iter().filter(|(_, c)| **c >= 5).map(|(w, _)| *w).collect();
    let (mut r1, mut rc1, mut r0, mut rc0) = (0, 0, 0, 0);
    for k in &at3 {
        let Some((_, end)) = hasil(k, k.lahir + 180.0) else { continue };
        let has = k.ev.iter().any(|e| e.beli && e.t <= k.lahir + 30.0 && rep.contains(e.user.as_str()));
        if has { r1 += 1; rc1 += (end < 0.5) as usize } else { r0 += 1; rc0 += (end < 0.5) as usize }
    }
    println!("dompet 'pembeli awal berulang' (>=5 koin, 30 dtk pertama): {} | crash bila ada: {:.0}% (n={r1}) vs tidak: {:.0}% (n={r0}) [catatan: daftar dibuat dari seluruh data = deskriptif]", rep.len(), 100.0 * rc1 as f64 / r1.max(1) as f64, 100.0 * rc0 as f64 / r0.max(1) as f64);
    let mut jam: BTreeMap<i64, (usize, f64)> = BTreeMap::new();
    for b in &baris {
        let j = jam.entry(((b.t / 3600.0) as i64).rem_euclid(24)).or_insert((0, 0.0));
        j.0 += 1;
        j.1 += b.ret - 1.0;
    }
    println!("rata hasil per jam UTC (titik dataset ML): {}", jam.iter().map(|(h, (n, s))| format!("{h:02}h {:+.0}%(n={n})", 100.0 * s / *n as f64)).collect::<Vec<_>>().join(" "));

    println!("\n===== RISET B: ATURAN SISTEM R1-R8 =====");
    // R1 decision age
    for a in [30.0, 60.0, 120.0, 180.0, 300.0, 600.0] {
        let mut per: BTreeMap<i64, (usize, usize, usize)> = BTreeMap::new();
        for (_, k) in &semua {
            if !alive(k, a) {
                continue;
            }
            if let Some((peak, end)) = hasil(k, k.lahir + a) {
                let p = per.entry((k.lahir / 86400.0) as i64).or_insert((0, 0, 0));
                p.0 += 1;
                p.1 += (peak >= 2.0) as usize;
                p.2 += (end < 0.5) as usize;
            }
        }
        println!("R1 umur {:>3.0}s: {}", a, per.iter().map(|(d, (n, u, c))| format!("{} n={n} >=2x {:.1}% crash {:.1}%", tanggal(*d), 100.0 * *u as f64 / *n as f64, 100.0 * *c as f64 / *n as f64)).collect::<Vec<_>>().join(" | "));
    }
    // R3 early seller (minute 1) vs none; share of early sellers that are the creator
    let (mut s1, mut sc1, mut s0, mut sc0, mut cre, mut tot_s) = (0, 0, 0, 0, 0, 0);
    for (_, k) in &semua {
        if !alive(k, 120.0) {
            continue;
        }
        let Some((_, end)) = hasil(k, k.lahir + 120.0) else { continue };
        let sellers: HashSet<&str> = k.ev.iter().filter(|e| !e.beli && e.t <= k.lahir + 60.0).map(|e| e.user.as_str()).collect();
        tot_s += sellers.len();
        cre += sellers.contains(k.creator.as_str()) as usize;
        if sellers.is_empty() { s0 += 1; sc0 += (end < 0.5) as usize } else { s1 += 1; sc1 += (end < 0.5) as usize }
    }
    println!("R3 ada penjual menit-1: crash {:.0}% (n={s1}) vs tanpa: {:.0}% (n={s0}) | penjual menit-1 yang = creator: {cre} dari {tot_s}", 100.0 * sc1 as f64 / s1.max(1) as f64, 100.0 * sc0 as f64 / s0.max(1) as f64);
    // R4 creator reuse; crash rate when the creator's previous coin (born earlier, finished) crashed
    let mut by_creator: HashMap<&str, Vec<&Koin>> = HashMap::new();
    for (_, k) in &semua {
        by_creator.entry(k.creator.as_str()).or_default().push(*k);
    }
    let multi = by_creator.values().filter(|v| v.len() >= 2).count();
    let (mut p1, mut pc1) = (0, 0);
    for v in by_creator.values_mut() {
        v.sort_by(|a, b| a.lahir.partial_cmp(&b.lahir).unwrap_or(std::cmp::Ordering::Equal));
        for w in v.windows(2) {
            let prev_crash = hasil(w[0], w[0].lahir + 120.0).is_some_and(|x| x.1 < 0.5) && w[0].lahir + 1920.0 <= w[1].lahir;
            if prev_crash {
                if let Some((_, end)) = hasil(w[1], w[1].lahir + 120.0) {
                    p1 += 1;
                    pc1 += (end < 0.5) as usize;
                }
            }
        }
    }
    println!("R4 creator dengan >=2 koin: {multi} dari {} | koin berikutnya dari creator yang koin sebelumnya crash: crash {:.0}% (n={p1})", by_creator.len(), 100.0 * pc1 as f64 / p1.max(1) as f64);
    // R6 exit signals that come BEFORE a crash: at each 30 s step after 3 min, signal in the last 30 s -> crash in next 5 min
    let mut sig: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for k in &at3 {
        let mut tt = k.lahir + 180.0;
        while tt <= k.lahir + 1800.0 {
            let (Some(p0), Some(p5)) = (px_at(k, tt), px_at(k, tt + 300.0)) else { break };
            let crash = p5 < 0.7 * p0;
            let w: Vec<&Ev> = k.ev.iter().filter(|e| e.t > tt - 30.0 && e.t <= tt).collect();
            let mut bal: HashMap<&str, f64> = HashMap::new();
            for e in k.ev.iter().filter(|e| e.t <= tt) {
                *bal.entry(e.user.as_str()).or_insert(0.0) += if e.px > 0.0 { if e.beli { e.sol / e.px } else { -e.sol / e.px } } else { 0.0 };
            }
            let mut tops: Vec<(&str, f64)> = bal.into_iter().collect();
            tops.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let top3: HashSet<&str> = tops.iter().take(3).map(|x| x.0).collect();
            let flags = [
                ("dasar (semua titik)", true),
                ("creator menjual", w.iter().any(|e| !e.beli && e.user == k.creator)),
                ("top-3 holder menjual", w.iter().any(|e| !e.beli && top3.contains(e.user.as_str()))),
                ("tanpa pembeli 30 dtk", !w.iter().any(|e| e.beli)),
                ("jual tunggal >=5% SOL riil", w.iter().any(|e| !e.beli && e.riil > 0.0 && e.sol >= 0.05 * e.riil)),
                ("jual > beli 30 dtk", w.iter().filter(|e| !e.beli).map(|e| e.sol).sum::<f64>() > w.iter().filter(|e| e.beli).map(|e| e.sol).sum::<f64>()),
            ];
            for (nm, on) in flags {
                if on {
                    let x = sig.entry(nm).or_insert((0, 0));
                    x.0 += 1;
                    x.1 += crash as usize;
                }
            }
            tt += 30.0;
        }
    }
    println!("R6 sinyal di 30 dtk terakhir -> harga turun >30% dalam 5 menit: {}", sig.iter().map(|(nm, (n, c))| format!("{nm}: {:.0}% (n={n})", 100.0 * *c as f64 / (*n).max(1) as f64)).collect::<Vec<_>>().join(" | "));
    // R7 minute-1 features vs outcome of a buy at 3 min (>= 2x within 30 min)
    let mut xs: Vec<Vec<f64>> = vec![Vec::new(); 22];
    let mut ys: Vec<bool> = Vec::new();
    for k in &at3 {
        let Some((peak, _)) = hasil(k, k.lahir + 180.0) else { continue };
        let f = ml::fitur_ml(k, k.lahir + 60.0);
        for (j, v) in f.iter().enumerate().take(22) {
            xs[j].push(*v);
        }
        ys.push(peak >= 2.0);
    }
    let r7: Vec<String> = ml::NAMA_FITUR.iter().enumerate().map(|(j, n)| (n, ml::auc(&xs[j], &ys))).filter(|(_, a)| (a - 0.5).abs() >= 0.05).map(|(n, a)| format!("{n} {a:.2}")).collect();
    println!("R7 fitur menit-1 -> beli di menit 3 capai >=2x (n={}, positif {}): AUC jauh dari 0,5: {}", ys.len(), ys.iter().filter(|y| **y).count(), if r7.is_empty() { "tidak ada".into() } else { r7.join(", ") });
    // R8 slippage at our size on real curve depth at 3 min
    for sz in [0.01, 0.05, 0.1, 0.5, 1.0] {
        let imp: Vec<f64> = at3.iter().filter_map(|k| {
            let e = k.ev.iter().rev().find(|e| e.t <= k.lahir + 180.0)?;
            let tok = beli_tok(e, sz);
            if tok <= 0.0 { return None; }
            Some((sz / tok) / e.px - 1.0)
        }).collect();
        println!("R8 ukuran {sz} SOL di menit 3: median dampak harga + fee {:.1}% (n={})", 100.0 * med(imp.clone()), imp.len());
    }
    // ===== BATTLEFIELD MAP: real outcome (active exit mode, after costs) of every entry point, split by coin characteristic =====
    // Quintile edges over all rows (descriptive discovery only). A bucket is interesting only if it is positive on most folds.
    println!("\n===== PETA MEDAN PERANG (hasil nyata per karakter koin; keluar mode {}; setelah biaya) =====", KELUAR.load(AO::Relaxed));
    let kidx = |n: &str| ml::NAMA_KANDIDAT.iter().position(|m| *m == n).unwrap_or(0);
    let (i_hold, i_peak, i_ofi30, i_e10, i_flip) = (kidx("holders"), kidx("dist_peak"), kidx("ofi30"), kidx("early10_hold_pct"), kidx("flip300"));
    let karakter: Vec<(&str, Box<dyn Fn(&ml::Baris) -> f64>)> = vec![
        ("umur (dtk)", Box::new(|b: &ml::Baris| b.x[0])),
        ("SOL di kurva", Box::new(|b: &ml::Baris| b.x[12])),
        ("holder", Box::new(move |b: &ml::Baris| b.xk[i_hold])),
        ("harga/puncak", Box::new(move |b: &ml::Baris| b.xk[i_peak])),
        ("OFI 30 dtk", Box::new(move |b: &ml::Baris| b.xk[i_ofi30])),
        ("naik 60 dtk (x)", Box::new(|b: &ml::Baris| b.x[14])),
        ("pembeli 60 dtk", Box::new(|b: &ml::Baris| b.x[1])),
        ("porsi 10 pembeli awal", Box::new(move |b: &ml::Baris| b.xk[i_e10])),
        ("dompet flip 5 mnt", Box::new(move |b: &ml::Baris| b.xk[i_flip])),
        ("dev sudah jual (0/1)", Box::new(|b: &ml::Baris| b.x[15])),
        ("porsi dev", Box::new(|b: &ml::Baris| b.x[19])),
        ("mayhem (0/1)", Box::new(|b: &ml::Baris| koin.get(&b.mint).map_or(0.0, |k| k.mayhem as u8 as f64))),
    ];
    for (nm, f) in &karakter {
        let mut v: Vec<f64> = baris.iter().map(|b| f(b)).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut edges: Vec<f64> = (1..5).map(|q| v[(v.len() * q / 5).min(v.len() - 1)]).collect();
        edges.dedup();
        let bucket = |x: f64| edges.iter().filter(|e| x >= **e).count();
        let nb = edges.len() + 1;
        let mut s: Vec<(usize, f64, usize, BTreeMap<i64, (usize, f64)>)> = (0..nb).map(|_| (0, 0.0, 0, BTreeMap::new())).collect();
        for b in &baris {
            let i = bucket(f(b));
            s[i].0 += 1;
            s[i].1 += b.ret - 1.0;
            s[i].2 += (b.y) as usize;
            let p = s[i].3.entry(b.hari).or_insert((0, 0.0));
            p.0 += 1;
            p.1 += b.ret - 1.0;
        }
        let parts: Vec<String> = (0..nb).map(|i| {
            let lo = if i == 0 { v[0] } else { edges[i - 1] };
            let (n, sum, w, per) = &s[i];
            let f_ok = per.values().filter(|(c, _)| *c >= 10).count();
            let f_pos = per.values().filter(|(c, x)| *c >= 10 && *x > 0.0).count();
            format!("[>={lo:.3}] n={n} {:+.1}% menang {:.0}% lipatan+ {f_pos}/{f_ok}", 100.0 * sum / (*n).max(1) as f64, 100.0 * *w as f64 / (*n).max(1) as f64)
        }).collect();
        println!("{nm:<22} {}", parts.join(" | "));
    }
    // ===== WINNERS vs LOSERS MAP: what did the trader flow look like at time t for coins that then ROSE vs FELL? =====
    // outcome over the next 10 min (path after t, descriptive): naik = peak >= 1.5x price at t; turun = peak < 1.1x.
    println!("\n===== PETA PEMENANG vs PECUNDANG (10 menit setelah titik; median karakter saat titik itu) =====");
    let mut grp: Vec<i8> = Vec::with_capacity(baris.len());
    for b in &baris {
        let g = koin.get(&b.mint).and_then(|k| {
            let p0 = k.ev.iter().rev().find(|e| e.t <= b.t)?.px;
            let peak = k.ev.iter().filter(|e| e.t > b.t && e.t <= b.t + 600.0).map(|e| e.px).fold(p0, f64::max);
            Some(if peak >= 1.5 * p0 { 1 } else if peak < 1.1 * p0 { -1 } else { 0 })
        });
        grp.push(g.unwrap_or(0));
    }
    println!("titik naik (>=1,5x dalam 10 mnt) {} | turun (tak pernah >=1,1x) {} | tengah {}", grp.iter().filter(|g| **g == 1).count(),
        grp.iter().filter(|g| **g == -1).count(), grp.iter().filter(|g| **g == 0).count());
    let med = |mut v: Vec<f64>| -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v.get(v.len() / 2).cloned().unwrap_or(f64::NAN)
    };
    for (judul, min_riil) in [("SEMUA titik", 0.0), ("BISA DITRADINGKAN (SOL kurva >= 2)", 2.0)] {
    let ok = |b: &ml::Baris| b.x[12] >= min_riil;
    println!("--- {judul}: naik {} | turun {} ---", baris.iter().zip(&grp).filter(|(b, g)| ok(b) && **g == 1).count(), baris.iter().zip(&grp).filter(|(b, g)| ok(b) && **g == -1).count());
    let mut rows: Vec<(f64, String)> = Vec::new();
    let names: Vec<(&str, bool, usize)> = ml::NAMA_FITUR.iter().enumerate().map(|(i, n)| (*n, false, i))
        .chain(ml::NAMA_KANDIDAT.iter().enumerate().map(|(i, n)| (*n, true, i))).collect();
    for (n, cand, i) in names {
        let val = |b: &ml::Baris| if cand { b.xk.get(i).cloned().unwrap_or(f64::NAN) } else { b.x[i] };
        let up: Vec<f64> = baris.iter().zip(&grp).filter(|(b, g)| ok(b) && **g == 1).map(|(b, _)| val(b)).collect();
        let dn: Vec<f64> = baris.iter().zip(&grp).filter(|(b, g)| ok(b) && **g == -1).map(|(b, _)| val(b)).collect();
        let xs: Vec<f64> = up.iter().chain(dn.iter()).cloned().collect();
        let ys: Vec<bool> = up.iter().map(|_| true).chain(dn.iter().map(|_| false)).collect();
        let a = ml::auc(&xs, &ys);
        rows.push(((a - 0.5).abs(), format!("{n:<22} naik {:>10.4} | turun {:>10.4} | AUC {:.3} {}", med(up), med(dn), a,
            if a > 0.55 { "(tinggi -> naik)" } else if a < 0.45 { "(tinggi -> turun)" } else { "(lemah)" })));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, r) in rows {
        println!("{r}");
    }
    }
    // ===== HOLD-TIME TEST: return if every entry point is held a FIXED time (no exit rule), 0.01 SOL, normal costs =====
    println!("\n===== UJI LAMA PEGANG TETAP (semua titik masuk; tanpa aturan keluar; biaya normal) =====");
    for h in [30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 300.0, 600.0, 1800.0, 3600.0] {
        let mut rs: Vec<f64> = Vec::new();
        let mut per: BTreeMap<i64, (usize, f64)> = BTreeMap::new();
        for b in &baris {
            let Some(k) = peta.get(&b.mint) else { continue };
            let tm = b.t + jeda();
            let Some(e0) = k.ev.iter().rev().find(|e| e.t <= tm) else { continue };
            let tok = beli_tok(e0, ukuran());
            if !(tok > 0.0) {
                continue;
            }
            // exit fill: curve/pool state at entry + h (price is static between trades, a sell needs no counterparty)
            let Some(ex) = k.ev.iter().rev().find(|e| e.t <= tm + h + jeda()) else { continue };
            let r = (jual_sol(ex, tok) - 2.0 * biaya()) / ukuran();
            rs.push(r);
            let p = per.entry(b.hari).or_insert((0, 0.0));
            p.0 += 1;
            p.1 += r - 1.0;
        }
        let n = rs.len().max(1) as f64;
        let mut s = rs.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!("pegang {:>5.0} dtk: rata {:+.1}% | median {:+.1}% | menang {:.1}% | >=2x {} | per lipatan {}", h,
            100.0 * rs.iter().map(|r| r - 1.0).sum::<f64>() / n, 100.0 * (s.get(s.len() / 2).unwrap_or(&1.0) - 1.0),
            100.0 * rs.iter().filter(|r| **r > 1.0).count() as f64 / n, rs.iter().filter(|r| **r >= 2.0).count(),
            per.iter().map(|(d, (c, x))| format!("{} {:+.1}%", nama_lipat(*d), 100.0 * x / *c as f64)).collect::<Vec<_>>().join(" | "));
    }

    // ===== MARKOV CHAIN of 30 s order-flow states (observable states; an HMM only if these show persistence) =====
    // state per 30 s bucket: 0 no trades, 1 sell > buy, 2 buy 1-2x sell, 3 buy >= 2x sell (SOL)
    println!("\n===== RANTAI MARKOV TEKANAN BELI/JUAL (per 30 dtk, umur 2 menit - 2 jam, semua koin dipakai) =====");
    let nama_s = ["sepi", "jual>beli", "beli 1-2x", "beli>=2x"];
    let mut trans = [[0usize; 4]; 4];
    let mut fwd: [(usize, f64); 4] = [(0, 0.0); 4];
    for (_, k) in &lahir_ok {
        let state = |a: f64, b: f64| {
            let (mut bs, mut ss, mut n) = (0.0, 0.0, 0);
            for e in k.ev.iter().filter(|e| e.t > a && e.t <= b) {
                n += 1;
                if e.beli { bs += e.sol } else { ss += e.sol }
            }
            if n == 0 { 0 } else if ss > bs { 1 } else if bs >= 2.0 * ss { 3 } else { 2 }
        };
        let px = |s: f64| k.ev.iter().rev().find(|e| e.t <= s).map(|e| e.px);
        let mut t = k.lahir + umur_min();
        let mut prev = state(t - 30.0, t);
        while t + 30.0 <= k.lahir + UMUR_MAKS {
            let next = state(t, t + 30.0);
            trans[prev][next] += 1;
            if let (Some(a), Some(b)) = (px(t), px(t + 30.0)) {
                if a > 0.0 && b > 0.0 {
                    fwd[prev].0 += 1;
                    fwd[prev].1 += (b / a).ln();
                }
            }
            prev = next;
            t += 30.0;
        }
    }
    for (i, row) in trans.iter().enumerate() {
        let tot: usize = row.iter().sum();
        println!("dari {:<10} (n={:>6}) -> {} | harga 30 dtk berikut rata {:+.2}%", nama_s[i], tot,
            row.iter().enumerate().map(|(j, c)| format!("{} {:.1}%", nama_s[j], 100.0 * *c as f64 / tot.max(1) as f64)).collect::<Vec<_>>().join(", "),
            100.0 * (fwd[i].1 / fwd[i].0.max(1) as f64).exp_m1());
    }

    // ===== CANDIDATE FEATURE TEST: each candidate is tested ALONE on real data before it may join the model =====
    // (a) AUC alone, all rows and per fold; (b) edge: direction + top-20% cut chosen on TRAINING folds only, then mean
    //     return of test-fold rows that pass vs all test rows; (c) add-one: CatBoost(base) vs CatBoost(base + candidate)
    //     on each walk-forward test fold. Verdict needs (b) and (c) to improve on MOST test folds.
    println!("\n===== UJI FITUR KANDIDAT (kuantitatif; diuji sendiri dulu, belum masuk model) =====");
    let folds: Vec<(i64, Vec<&ml::Baris>, Vec<&ml::Baris>)> = hari.iter().skip(1).filter_map(|d| {
        let mulai = baris.iter().filter(|b| b.hari == *d).map(|b| b.t).fold(f64::INFINITY, f64::min);
        let latih: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari < *d && b.keluar < mulai).collect();
        let uji: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari == *d).collect();
        let pos = latih.iter().filter(|b| b.y).count();
        let ok = latih.len() >= 200 && pos >= 20 && uji.iter().any(|b| b.y) && uji.iter().any(|b| b.ret <= 1.0);
        ok.then_some((*d, latih, uji))
    }).collect();
    println!("lipatan uji yang sah (latih >= 200 baris, uji punya menang & kalah): {}", folds.iter().map(|f| nama_lipat(f.0)).collect::<Vec<_>>().join(", "));
    let base_auc: Vec<Option<f64>> = folds.iter().map(|(d, latih, uji)| {
        let ux: Vec<Vec<f64>> = uji.iter().map(|b| b.x.clone()).collect();
        let y: Vec<bool> = uji.iter().map(|b| b.y).collect();
        ml::catboost(&bin, &format!("/tmp/sol_kb_{d}"), latih, &ux).map(|(_, cu)| ml::auc(&cu, &y))
    }).collect();
    println!("model dasar (22 fitur) AUC per lipatan: {}", base_auc.iter().map(|a| a.map_or("-".into(), |v| format!("{v:.3}"))).collect::<Vec<_>>().join(" "));
    let y_all: Vec<bool> = baris.iter().map(|b| b.y).collect();
    for (j, nm) in ml::NAMA_KANDIDAT.iter().enumerate() {
        let a_all = ml::auc(&baris.iter().map(|b| b.xk[j]).collect::<Vec<_>>(), &y_all);
        let (mut edge_win, mut add_win, mut n_f) = (0usize, 0usize, 0usize);
        let (mut pass_sum, mut pass_n, mut all_sum, mut all_n) = (0.0, 0usize, 0.0, 0usize);
        let mut d_auc = Vec::new();
        for ((d, latih, uji), b_auc) in folds.iter().zip(&base_auc) {
            n_f += 1;
            // (b) edge with direction and cut from training rows only
            let ytr: Vec<bool> = latih.iter().map(|b| b.y).collect();
            let dir = if ml::auc(&latih.iter().map(|b| b.xk[j]).collect::<Vec<_>>(), &ytr) >= 0.5 { 1.0 } else { -1.0 };
            let mut q: Vec<f64> = latih.iter().map(|b| dir * b.xk[j]).collect();
            q.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let cut = q[((q.len() as f64 * 0.8) as usize).min(q.len() - 1)];
            let pass: Vec<f64> = uji.iter().filter(|b| dir * b.xk[j] >= cut).map(|b| b.ret - 1.0).collect();
            let all: Vec<f64> = uji.iter().map(|b| b.ret - 1.0).collect();
            let (pm, am) = (pass.iter().sum::<f64>() / pass.len().max(1) as f64, all.iter().sum::<f64>() / all.len() as f64);
            if !pass.is_empty() && pm > am {
                edge_win += 1;
            }
            pass_sum += pass.iter().sum::<f64>();
            pass_n += pass.len();
            all_sum += all.iter().sum::<f64>();
            all_n += all.len();
            // (c) add-one to the base model
            let tr: Vec<ml::Baris> = latih.iter().map(|b| ml::Baris { hari: b.hari, mint: String::new(), t: b.t, keluar: b.keluar,
                x: b.x.iter().cloned().chain(std::iter::once(b.xk[j])).collect(), xk: Vec::new(), ret: b.ret, y: b.y }).collect();
            let trr: Vec<&ml::Baris> = tr.iter().collect();
            let ux: Vec<Vec<f64>> = uji.iter().map(|b| b.x.iter().cloned().chain(std::iter::once(b.xk[j])).collect()).collect();
            let y: Vec<bool> = uji.iter().map(|b| b.y).collect();
            if let (Some(b0), Some((_, cu))) = (b_auc, ml::catboost(&bin, &format!("/tmp/sol_ka_{d}_{j}"), &trr, &ux)) {
                let a = ml::auc(&cu, &y);
                d_auc.push(a - b0);
                if a > b0 + 0.005 {
                    add_win += 1;
                }
            }
        }
        let mayor = |w: usize| n_f > 0 && 2 * w > n_f;
        let verdict = if n_f == 0 { "BELUM BISA DIUJI" } else if mayor(edge_win) && mayor(add_win) { "LAYAK DIMASUKKAN (lolos b & c)" } else if mayor(edge_win) || mayor(add_win) { "RAGU (lolos satu uji)" } else { "TOLAK" };
        println!("{:<18} AUC sendiri {:.3} | top20%: {:+.1}% vs semua {:+.1}% (n={pass_n}), menang di {edge_win}/{n_f} lipatan | tambah ke model: dAUC rata {:+.3}, naik di {add_win}/{n_f} | {verdict}",
            nm, a_all, 100.0 * pass_sum / pass_n.max(1) as f64, 100.0 * all_sum / all_n.max(1) as f64,
            d_auc.iter().sum::<f64>() / d_auc.len().max(1) as f64);
    }

    println!("\n===== UJI BUANG-SATU FITUR ML (CatBoost dilatih ulang tanpa fitur itu; rata-rata AUC hari uji) =====");
    let mut dasar_auc = Vec::new();
    let mut per_fitur: Vec<Vec<f64>> = vec![Vec::new(); ml::NAMA_FITUR.len()];
    for d in &hari_uji {
        let mulai = baris.iter().filter(|b| b.hari == *d).map(|b| b.t).fold(f64::INFINITY, f64::min);
        let latih: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari < *d && b.keluar < mulai).collect();
        let uji: Vec<&ml::Baris> = baris.iter().filter(|b| b.hari == *d).collect();
        let y: Vec<bool> = uji.iter().map(|b| b.y).collect();
        let ux: Vec<Vec<f64>> = uji.iter().map(|b| b.x.clone()).collect();
        if let Some((_, cu)) = ml::catboost(&bin, &format!("/tmp/sol_ab_{d}"), &latih, &ux) {
            dasar_auc.push(ml::auc(&cu, &y));
        }
        for (j, slot) in per_fitur.iter_mut().enumerate() {
            let latih_j: Vec<ml::Baris> = latih.iter().map(|b| ml::Baris { hari: b.hari, mint: String::new(), t: b.t, keluar: b.keluar, x: b.x.iter().enumerate().map(|(i, v)| if i == j { 0.0 } else { *v }).collect(), xk: Vec::new(), ret: b.ret, y: b.y }).collect();
            let latih_jr: Vec<&ml::Baris> = latih_j.iter().collect();
            let ux_j: Vec<Vec<f64>> = ux.iter().map(|x| x.iter().enumerate().map(|(i, v)| if i == j { 0.0 } else { *v }).collect()).collect();
            if let Some((_, cu)) = ml::catboost(&bin, &format!("/tmp/sol_ab_{d}_{j}"), &latih_jr, &ux_j) {
                slot.push(ml::auc(&cu, &y));
            }
        }
    }
    let rata = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    if dasar_auc.is_empty() {
        println!("TIDAK DIJALANKAN: tidak ada hari uji ML (data latih kurang)");
        return;
    }
    let a0 = rata(&dasar_auc);
    println!("semua fitur: AUC {:.3} (hari uji {})", a0, dasar_auc.len());
    for (j, nama) in ml::NAMA_FITUR.iter().enumerate() {
        let a = rata(&per_fitur[j]);
        println!("tanpa {:<20} AUC {:.3} ({:+.3}) {}", nama, a, a - a0, if a > a0 + 0.005 { "-> fitur ini MERUSAK" } else if a < a0 - 0.005 { "-> fitur ini penting" } else { "-> pengaruh kecil" });
    }
}

#[cfg(test)]
mod tes {
    use super::*;
    fn ev(t: f64, beli: bool, user: &str, sol: f64) -> Ev {
        Ev { t, slot: t as u64, beli, user: user.into(), sol, px: 1e-8, cad_sol: 30.0, cad_tok: 1e9, riil: 5.0, fee: 0.0125, amm: false, ix: 0 }
    }
    fn koin() -> Koin {
        let mut k = Koin { lahir: 0.0, slot_lahir: 0, creator: "dev".into(), nama: String::new(), simbol: String::new(), sol_quote: true, terpotong: false, mayhem: false, lulus: false, aman: None, ev: vec![] };
        for i in 0..40 {
            k.ev.push(ev(70.0 + i as f64, true, &format!("w{i}"), 0.1));
        }
        k
    }
    /// data sintetis (hanya untuk tes): event setelah t tidak boleh mengubah fitur pada t
    #[test]
    fn fitur_tidak_melihat_masa_depan() {
        let k = koin();
        let a = fitur(&k, 100.0);
        let mut k2 = koin();
        k2.ev.push(ev(100.5, false, "dev", 9.0));
        k2.ev.push(ev(101.0, true, "zz", 50.0));
        assert_eq!(a, fitur(&k2, 100.0));
        assert!(fitur(&k2, 101.0).dev_jual, "setelah waktunya, dev jual terlihat");
    }
    #[test]
    fn fitur_ml_kausal() {
        let k = koin();
        let a = crate::ml::fitur_ml(&k, 100.0);
        let mut k2 = koin();
        k2.ev.push(ev(100.5, false, "dev", 9.0));
        assert_eq!(a, crate::ml::fitur_ml(&k2, 100.0));
    }
    /// synthetic data (test only): events after t must not change candidate features at t
    #[test]
    fn fitur_kandidat_kausal() {
        let k = koin();
        let a = crate::ml::fitur_kandidat(&k, 100.0);
        let mut k2 = koin();
        k2.ev.push(ev(100.5, false, "dev", 9.0));
        k2.ev.push(ev(101.0, true, "zz", 50.0));
        assert_eq!(a, crate::ml::fitur_kandidat(&k2, 100.0));
        assert_ne!(a, crate::ml::fitur_kandidat(&k2, 101.0), "after its time the new events are visible");
    }
    /// synthetic data (test only): K5 sells at the curve state at entry + 60 s, not at a much later trade
    #[test]
    fn k5_jual_di_harga_kurva_saat_60_dtk() {
        let mut k = koin();
        let mut crash = ev(5000.0, false, "zz", 1.0);
        crash.cad_sol = 1.0; // a trade hours later at a crashed price must not be used
        k.ev.push(crash);
        KELUAR.store(5, AO::Relaxed);
        let h = trade("m", &k, 100.0, 0.01).unwrap();
        KELUAR.store(0, AO::Relaxed);
        assert_eq!(h.alasan, "pegang waktu");
        assert!(h.keluar <= 100.0 + JEDA + 60.0 + JEDA, "exit time {}", h.keluar);
        assert!(h.ret > 0.5, "used the crashed later price: {}", h.ret);
    }
    #[test]
    fn isi_order_tidak_sebelum_jeda() {
        let k = koin();
        let h = trade("m", &k, 100.0, 0.01).unwrap();
        assert!(h.masuk >= 100.0 + JEDA);
    }
    #[test]
    fn honeypot_tidak_mewajibkan_penjual() {
        let k = koin();
        let f = fitur(&k, 109.0);
        assert_eq!(f.jual_sol, 0.0);
        assert!(sinyal_masuk(&f), "koin tanpa penjual tetap boleh dibeli: {f:?}");
    }
}
