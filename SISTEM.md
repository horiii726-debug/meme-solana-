# Solana new-pool system (rebuild) — what exists and what it showed

State 2026-11-10. Research/backtest only: no live trading, no keys, nothing sends transactions.
All numbers below are out-of-sample (walk-forward by time, purge 30 min, no look-ahead — enforced by tests).

## Components (`sol2/`)

| Component | Where | Function |
|---|---|---|
| Ingestion: live | `ingest/src/bin/live.rs` | detect new pools (PumpSwap, Raydium CPMM, Meteora DAMM v2) and record every tx raw (zstd JSONL) |
| Ingestion: history | `ingest/src/bin/riwayat.rs` | pool creations from migration accounts (Pump.fun `MigrateV2`, DBC→DAMM v2), every tx of the first 6 h per pool; Helius `getTransactionsForAddress` path verified identical to live data (32,905/32,905 tx) |
| RPC client | `ingest/src/lib.rs` | multi-endpoint, pacing, hard call caps, metered endpoints refused without `ALLOW_PAID` + `MAX_CALLS` |
| Wallet history (C7) | `ingest/src/bin/dompet.rs` | early buyers' history before their buy: tx count, age, first funder |
| Decoder | `dekode/src/lib.rs` | IDL-driven borsh decoder (no hand-written offsets), exact integers, decimals only from chain |
| Pool math check (C5) | `dekode/src/bin/amm_cek.rs` | predicted vs real swap output: PumpSwap median error 1e-7, DAMM v2 5e-6 |
| Series + execution engine | `dekode/src/series.rs` | per-pool swap series; `trade_amm_ext`: verified pool math, 2-slot latency, real `meta.fee`, real failure rates (sell 30 %, buy 16 %) with retries, post-rug drained state, TP tiers, trailing, momentum, break-even, ratchet, external ML exit |
| Features (888) | `dekode/src/bin/tambang1000.rs` | 7 windows x 116 flow/size/wallet/slot/timing/price/liquidity/failed-tx features + ratios + first-N + pool-level + 14 ported from sniper_v4 (holders, wash, dust, EWMA accel, VWAP); causal universe |
| Statistics | `dekode/src/bin/permutasi.rs` | per-feature AUC, train screening, permutation p-values, bootstrap CI, Holm, Monte Carlo |
| Entry system | `dekode/src/bin/sistem2.rs` | CatBoost P(2x) x (1 - P(rug in 5 min)), nested walk-forward tuning of selection + exit plan, radar precision/recall |
| Exit model | `dekode/src/bin/exit_data.rs`, `exit_ml.rs` | snapshots every 3 s while holding; CatBoost P(dump >= 30 % or LP removal within 30 s) |
| Exit plan search | `dekode/src/bin/tp_moon.rs` | thousands of exit plans on the selected coins, Kelly-growth objective, moonshot capture |
| Frozen test | `dekode/src/bin/uji_beku.rs` | train once, apply unchanged to never-seen coins |
| Tests | `cargo test` | anti-look-ahead on every feature, post-rug valuation, drained-state sells, quiet coins, momentum and break-even exits |

## Results (2,016 pools, entry 15 s, liquid >= 1 SOL)

| Item | Result |
|---|---|
| Rug model P(rug 5 min) | AUC 0.76 (out of sample) |
| Rise model P(2x) | AUC 0.68-0.70 |
| Radar precision 2x | 37-38 % vs 31-32 % base rate; recall 58-60 % |
| Rejected coins | lose 3-13 % per trade (filter works) |
| Selected coins, simple exit | +5 % to +10 % per trade, not significant (CI includes 0) |
| Moonshot exit (20 % at 5x, trail 30 %, ML exit 0.8) | captures about +1,500 % per moonshot coin vs +80-380 % for quick TP; win rate ~38 %; descriptive, needs frozen confirmation |
| Honeypots | 0 found in 580 pools |

No configuration is proven profitable yet. Next: frozen test on newly pulled coins, more data.

## Removed (no edge after costs)

Old crates `kumpul/`, `uji/`, `rekonstruksi/`, old scripts and old reports/data — results were negative after costs
(Pump.fun −10.9 % to −14.2 % per trade, DBC −15.6 %). History stays in git.
