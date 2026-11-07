//! Dekoder event Pump.fun & PumpSwap (IDL resmi pump-public-docs 639b87fb). Dipakai `sol-kumpul` dan `sol-tarik`.
use base64::Engine;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PUMP: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const AMM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const D_TRADE: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];
pub const D_CREATE: [u8; 8] = [27, 114, 169, 77, 222, 235, 99, 118];
pub const D_COMPLETE: [u8; 8] = [95, 114, 97, 156, 212, 46, 152, 8];
pub const D_MIGRASI: [u8; 8] = [189, 233, 93, 185, 92, 148, 234, 148];
pub const D_AMM_BELI: [u8; 8] = [103, 244, 82, 31, 44, 245, 119, 119];
pub const D_AMM_JUAL: [u8; 8] = [62, 47, 55, 10, 165, 3, 220, 42];
pub const D_AMM_POOL: [u8; 8] = [177, 49, 12, 210, 160, 118, 167, 116];
/// batas pengisian celah per program per putus (RPC publik: ±10 permintaan/detik)
pub const ISI_MAKS: usize = 400;

pub fn detik() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Pembaca byte berurutan (borsh, little endian). Semua baca gagal = None, tidak pernah panik.
pub struct Baca<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) o: usize,
}
impl<'a> Baca<'a> {
    pub(crate) fn ambil(&mut self, n: usize) -> Option<&'a [u8]> {
        let x = self.b.get(self.o..self.o + n)?;
        self.o += n;
        Some(x)
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.ambil(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.ambil(2)?.try_into().ok()?))
    }
    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.ambil(4)?.try_into().ok()?))
    }
    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.ambil(8)?.try_into().ok()?))
    }
    pub(crate) fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.ambil(8)?.try_into().ok()?))
    }
    pub(crate) fn kunci(&mut self) -> Option<String> {
        Some(bs58::encode(self.ambil(32)?).into_string())
    }
    pub(crate) fn teks(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.ambil(n)?).to_string())
    }
}

pub fn trade(b: &[u8]) -> Option<serde_json::Value> {
    let mut r = Baca { b, o: 8 };
    let mint = r.kunci()?;
    let (sol, tok) = (r.u64()?, r.u64()?);
    let buy = r.u8()? == 1;
    let user = r.kunci()?;
    let ts = r.i64()?;
    let (vs, vt, rs, rt) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let mut v = serde_json::json!({"k": "trade", "mint": mint, "sol": sol as f64 / 1e9, "tok": tok as f64 / 1e6,
        "buy": buy, "user": user, "ts": ts, "vsol": vs as f64 / 1e9, "vtok": vt as f64 / 1e6,
        "rsol": rs as f64 / 1e9, "rtok": rt as f64 / 1e6});
    // ekor event (versi IDL 13 Okt 2026); versi lama yang lebih pendek tetap tercatat tanpa ekor
    let ekor = (|| -> Option<serde_json::Value> {
        let _fee_recipient = r.kunci()?;
        let (fee_bps, fee) = (r.u64()?, r.u64()?);
        let creator = r.kunci()?;
        let (cfee_bps, cfee) = (r.u64()?, r.u64()?);
        let _track = r.u8()?;
        let (_unclaimed, _claimed, _vol, _last) = (r.u64()?, r.u64()?, r.u64()?, r.i64()?);
        let ix = r.teks()?;
        let mayhem = r.u8()? == 1;
        let (_cb_bps, _cb, _bb_bps, _bb) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
        let n_sh = r.u32()? as usize;
        r.ambil(n_sh.checked_mul(34)?)?;
        let quote_mint = r.kunci()?;
        let (qa, vq, rq) = (r.u64()?, r.u64()?, r.u64()?);
        Some(serde_json::json!({"fee_bps": fee_bps, "fee": fee, "creator": creator, "cfee_bps": cfee_bps, "cfee": cfee,
            "ix": ix, "mayhem": mayhem, "quote": quote_mint, "qamt": qa, "vq": vq, "rq": rq}))
    })();
    match (ekor, v.as_object_mut()) {
        (Some(serde_json::Value::Object(e)), Some(o)) => o.extend(e),
        // ekor tidak terbaca: simpan panjang + byte mentah (base64) supaya tata letaknya bisa diperiksa
        (_, Some(o)) => {
            o.insert("len".into(), serde_json::json!(b.len()));
            o.insert("raw".into(), serde_json::json!(base64::engine::general_purpose::STANDARD.encode(b)));
        }
        _ => {}
    }
    Some(v)
}

pub fn create(b: &[u8]) -> Option<serde_json::Value> {
    let mut r = Baca { b, o: 8 };
    let (name, symbol, _uri) = (r.teks()?, r.teks()?, r.teks()?);
    let (mint, curve, user, creator) = (r.kunci()?, r.kunci()?, r.kunci()?, r.kunci()?);
    let ts = r.i64()?;
    let (vt, vs, rt, supply) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let token_program = r.kunci()?;
    let mayhem = r.u8()? == 1;
    let _cashback = r.u8()?;
    let quote = r.kunci()?;
    Some(serde_json::json!({"k": "create", "mint": mint, "curve": curve, "user": user, "creator": creator, "ts": ts,
        "name": name, "symbol": symbol, "vtok": vt as f64 / 1e6, "vsol": vs as f64 / 1e9, "rtok": rt as f64 / 1e6,
        "supply": supply as f64 / 1e6, "token_program": token_program, "mayhem": mayhem, "quote": quote}))
}

/// PumpSwap Buy/SellEvent: 14 angka pertama sama urutannya (ts, jumlah base, batas, cadangan user & pool, jumlah quote, fee)
pub fn amm_trade(b: &[u8], beli: bool) -> Option<serde_json::Value> {
    let mut r = Baca { b, o: 8 };
    let ts = r.i64()?;
    let base = r.u64()?;
    let _batas = r.u64()?;
    let (_ub, _uq, pb, pq) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let quote = r.u64()?;
    let (lp_bps, _lp, pr_bps, _pr) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    let (_q2, _q3) = (r.u64()?, r.u64()?);
    let (pool, user) = (r.kunci()?, r.kunci()?);
    let _ = (r.kunci()?, r.kunci()?, r.kunci()?, r.kunci()?);
    let coin_creator = r.kunci()?;
    let mut v = serde_json::json!({"k": if beli { "beli_amm" } else { "jual_amm" }, "pool": pool, "user": user, "ts": ts,
        "base": base, "quote_amt": quote, "pool_base": pb, "pool_quote": pq, "lp_bps": lp_bps, "pr_bps": pr_bps,
        "creator": coin_creator});
    // Optional tail (official pump_amm IDL; older events may stop earlier): creator / cashback / buyback fee bps.
    // Buy and Sell differ: BuyEvent has volume-tracking fields and ix_name (string) before the cashback fields.
    let mut ekor = || -> Option<()> {
        let cr = r.u64()?;
        let _ = r.u64()?;
        v["cr_bps"] = cr.into();
        if beli {
            let _ = r.u8()?; // track_volume
            let _ = (r.u64()?, r.u64()?, r.u64()?, r.i64()?, r.u64()?);
            let _ = r.teks()?; // ix_name
        }
        let cb = r.u64()?;
        let _ = r.u64()?;
        let bb = r.u64()?;
        v["cb_bps"] = cb.into();
        v["bb_bps"] = bb.into();
        Some(())
    };
    let _ = ekor();
    Some(v)
}

pub fn dekode(b: &[u8]) -> Option<serde_json::Value> {
    let tag: [u8; 8] = b.get(..8)?.try_into().ok()?;
    let mut r = Baca { b, o: 8 };
    match tag {
        D_TRADE => trade(b),
        D_CREATE => create(b),
        D_COMPLETE => Some(serde_json::json!({"k": "complete", "user": r.kunci()?, "mint": r.kunci()?})),
        D_MIGRASI => {
            let (user, mint) = (r.kunci()?, r.kunci()?);
            let (mint_amt, sol_amt, _fee) = (r.u64()?, r.u64()?, r.u64()?);
            let _curve = r.kunci()?;
            let ts = r.i64()?;
            let (pool, quote) = (r.kunci()?, r.kunci()?);
            Some(serde_json::json!({"k": "migrasi", "user": user, "mint": mint, "mint_amt": mint_amt, "sol_amt": sol_amt,
                "ts": ts, "pool": pool, "quote": quote}))
        }
        D_AMM_BELI => amm_trade(b, true),
        D_AMM_JUAL => amm_trade(b, false),
        D_AMM_POOL => {
            let ts = r.i64()?;
            let _index = r.u16()?;
            let creator = r.kunci()?;
            let (base_mint, quote_mint) = (r.kunci()?, r.kunci()?);
            let (_bd, _qd) = (r.u8()?, r.u8()?);
            let _ = (r.u64()?, r.u64()?);
            let (pb, pq) = (r.u64()?, r.u64()?);
            let _ = (r.u64()?, r.u64()?, r.u64()?, r.u8()?);
            let pool = r.kunci()?;
            Some(serde_json::json!({"k": "pool", "ts": ts, "creator": creator, "mint": base_mint, "quote": quote_mint,
                "pool": pool, "pool_base": pb, "pool_quote": pq}))
        }
        _ => None,
    }
}

