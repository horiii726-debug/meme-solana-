# Solana Early DEX Pool System — Full Rebuild Plan

Owner: non-technical, speaks Indonesian, wants short answers. This document is in English for the coding agent.
Repository: `meme-solana-` (work runs on the VPS, `/root/sol`). Data provider: **Ankr Premium** (HTTPS + WSS
endpoints; gRPC availability unknown until verified in C1).
Strategy reference: `solana_early_dex_pool_trading_plan.md` (owner, 2026-11-09).
Prior audit: `docs/AI_REVIEW_BRIEF.md`, `reports/moonshot_reconstruction/20261109_local_f34775c/`.

---

## 0. Why a rebuild

The current system has **no proven edge after costs** (Pump.fun −10.9 % to −14.2 % per trade, DBC −15.6 %,
LaunchLab ≈ 0 and unstable — `uji/LOG-EKSPERIMEN.md`). The data behind it is < 2 days, has broken DAMM v2 prices,
no PumpSwap rows, no failed transactions, float amounts with assumed decimals, and a survivor-biased prefilter
(`AI_REVIEW_BRIEF.md` §5, items D1–D12, S1–S8, T1–T2).

Therefore **every component is rebuilt and re-tested from zero, one at a time**. An existing component is kept only
if it passes the same accuracy gate as its replacement. Otherwise it is replaced.

The goal is to find out, from real on-chain data, **what separates coins that later moonshot from coins that go
flat or rug**, and to build the radar, entry, exit, sizing and execution from that evidence. The goal is not to
guess.

---

## 1. Binding rules for the coding agent (read every session)

1. **One component at a time, in the order of §4.** Do not start component N+1 until component N has passed its
   gate **and** the owner has replied "lanjut" (continue) in chat.
2. **Gate failed → stay on that component.** Work only on the blocking problem. Do not jump ahead, and do not
   "temporarily" use a failed component downstream. After 3 unsuccessful repair attempts, stop and report the
   options to the owner.
3. **Do not invent anything.** No made-up program IDs, account layouts, event fields, API methods, credit costs,
   thresholds or results. Every number in a report must come from a command run on real data, and the command must
   be written next to the number. Program IDs come from the `address` field in `idl/*.json` and are confirmed
   against real transactions.
4. Evidence tags on every claim: **[V]** verified by a command or exact code line · **[I]** inferred · **[U]** unknown.
5. **Unknown is never safe and never zero.** Missing or undecodable fields are stored as `UNKNOWN`.
6. **No candle data for decisions.** Volume, buy pressure, trader activity and price are computed **only from
   decoded on-chain swaps and pool reserves** (§3). Candle or aggregator numbers (DexScreener, GeckoTerminal,
   Birdeye) are used **only as an external cross-check** of our own numbers. They are never used as an input.
7. **Language:** Rust for all system code. C++ only for CatBoost. **No Python**, including for analysis.
8. **Safety:** never run live mode, never call `sendTransaction`, never read or print private keys. The Ankr
   endpoint contains the API key: store it in `/root/sol/.rpc.env` as `ANKR_HTTP` / `ANKR_WSS` (chmod 600), read it
   from the environment, and never print it. Logs say `ankr`, not the URL.
9. Do not delete, rename or modify the old crates (`kumpul`, `uji`, `rekonstruksi`). New code goes into a new
   workspace `sol2/` on a new branch `rebuild` cut from `main`. Rust collectors from `spec-features` may be read for
   reference. Python files from `spec-features` are not carried over.
10. Do not restart the running `sol-kumpul` service. Do not push to GitHub without the owner's permission.
11. Report to the owner in **short Indonesian**. Write the technical reports in English.

### Session protocol
- Start: read this file, then `sol2/PROGRESS.md` (the component status table), then the last component report.
- End: update `sol2/PROGRESS.md` (component, status `in_progress | passed | failed | blocked`, gate numbers, report
  path, open problems). Summarize in 3–5 Indonesian lines for the owner.
- Each component writes its report to `reports/rebuild/<component-id>/REPORT.md`, with the commands it ran, the
  results, the gate verdict, and its known limitations.

---

## 2. Target lifecycle and venues

Time zero (**t0**) = the first **successful swap** in a DEX pool, i.e. the pool is actually tradable.
Two routes lead there: launchpad/bonding-curve migration (e.g. Pump.fun → PumpSwap, Meteora DBC → DAMM v2,
Raydium LaunchLab → Raydium pool) and direct DEX pool creation.

- **Initial venues:** PumpSwap, Raydium CPMM, Meteora DAMM v2. IDLs for these already exist in `idl/`.
- **Later venues:** Raydium AMM v4, Meteora DLMM / DAMM v1, and others. Each is added only after it has its own
  decoder that passes gate C3.
- **Quote asset:** SOL/WSOL only for strategy work. Pools in other quotes are recorded but kept separate.
- **Bonding-curve trading before migration** is out of scope for the first strategy. It is still decoded where it
  explains the pool's history (dev wallet, early holders).

---

## 3. Metric definitions (computed by our own algorithm from swaps)

All metrics are computed **per pool** over a window `[t − W, t]` that uses only events with
`block_time ≤ t` (causal). Candidate windows are W ∈ {5 s, 10 s, 30 s, 60 s, 120 s, 300 s}. These windows are
experiment variables, not proven settings.

| Metric | Definition |
|---|---|
| buy_sol, sell_sol | Sum of quote (SOL) amount in successful buy / sell swaps, from raw integer amounts ÷ decoded decimals |
| tx_volume_sol | buy_sol + sell_sol (on-chain volume, not candle volume) |
| net_flow_sol | buy_sol − sell_sol |
| buy_pressure | buy_sol / (buy_sol + sell_sol + ε) |
| n_buys, n_sells | Count of successful buy / sell swaps |
| active_traders | Distinct signer wallets with ≥ 1 swap in the window |
| new_buyers | Buyers whose first swap in this pool falls inside the window |
| buyer_growth, buyer_accel | Change in new_buyers between consecutive windows, and the change of that change |
| top1_share, top5_share | Share of buy_sol from the largest 1 / 5 buyers (by wallet, then by cluster after C7) |
| cluster_adj_flow | net_flow_sol after merging wallets that C7 links to one actor |
| failed_tx_rate | Failed swap tx / all swap tx touching the pool |
| realized_slippage | (execution price − pre-trade pool price) / pre-trade pool price, per swap |
| price | Pool price from reserves after each swap (not from candles) |
| liquidity_sol | Quote-side reserve in SOL, plus the depth needed for a given order size (price impact curve) |
| creator_sold | Creator / dev-linked wallets' sell_sol and share of holdings sold |

The units, signs and formulas of every metric must have unit tests on hand-built examples (C6).

---

## 4. Component ladder (strict order)

Each component lists: **Purpose · Existing code to audit · Accuracy test · Gate · If the gate fails**.
Gate thresholds marked *(proposed)* are starting values set in this plan. The owner may change them before the
component starts. They are not results.

### C0 — Audit of existing components (no new code)
- **Purpose:** decide which old pieces can be reused.
- **Audit:** for every module in `kumpul/`, `uji/`, `rekonstruksi/` (and the Rust bins on `spec-features`), record
  its role, inputs and outputs, tests, the known problems from `AI_REVIEW_BRIEF.md`, and a verdict
  `reuse | fix | replace`. The evidence must be a command or a file:line reference.
- **Gate:** a table that covers every source file, with no verdict left without evidence.
- **Deliverable:** `reports/rebuild/C0/REPORT.md`.

### C1 — Data access (Ankr capability probe)
- **Purpose:** know exactly what Ankr can and cannot give us before designing the collector.
- **Test** (small binary `sol2/probe`):
  - `getSlot`, `getBlock`, and `getTransaction` with `maxSupportedTransactionVersion: 0`.
  - `getSignaturesForAddress` with paging (`before`).
  - `getTokenLargestAccounts`, `getAccountInfo`.
  - WSS `logsSubscribe` (mentions a program ID) and `programSubscribe`.
  - Whether a gRPC (Yellowstone) endpoint exists.
  - History depth: fetch transactions aged 1, 7, 30 and 90 days.
  - Rate limit: measure the requests per second at which 429 errors begin.
  - Credits per method: measure the dashboard credit change for 100 calls of each method; ask the owner to read it
    if needed.
  - Whether `getBlock` returns transactions in block order, which is needed for the same-slot order.
- **Gate:** every row of the capability table is filled with [V] or with an explicit "not available". There is a
  credit-budget estimate per pool (create + all swaps for the first 6 h) and per day.
- **If it fails:** if full `getTransaction` for swaps is not available, stop and report. Nothing downstream is
  possible without it.

### C2 — Raw ingestion (history + live)
- **Purpose:** store raw transactions as the single source of truth, so they can be decoded again at any time.
- **Build:**
  - `sol2/tarik`: history per pool and per program, via `getSignaturesForAddress` then `getTransaction`.
  - `sol2/live`: a WSS listener, then `getTransaction`.
  - Both write `raw/<venue>/<date>.jsonl.zst` (the full RPC JSON), including failed transactions.
  - Both write gap markers when the stream drops, and keep `block_time` and `recv_time` separately.
- **Accuracy test:** for 3 pools × 3 venues, compare our signature set against an independent re-listing of the
  same pool address. Also count duplicates.
- **Gate *(proposed)*:** missing signatures = 0 on history pulls, duplicates = 0, every gap is marked.

### C3 — Decoders (per venue, per event type)
- **Purpose:** turn raw transactions into events: `pool_new, swap, liq_add, liq_remove, authority_change,
  migration`.
- **Rules:**
  - Keep raw integer amounts.
  - Read decimals from the mint account. There is no 6/9 fallback; a failed read is `UNKNOWN`.
  - Record tx index, outer instruction index and inner instruction index.
  - Decode inner instructions as well, because swaps routed through aggregators appear as CPI.
- **Accuracy test:**
  - Golden fixtures: ≥ 5 real transactions per venue per event type.
  - Plus a random 1,000 swaps per venue reconciled against `preTokenBalances` / `postTokenBalances` and the SOL
    balance deltas of the pool vaults.
- **Gate *(proposed)*:** golden fixtures 100 % exact. Random reconciliation ≥ 99.5 %, and every mismatch is
  explained.
- **If it fails:** fix the decoder of that venue only. A venue that fails stays out of every later component.

### C4 — New-pool detector and pool registry
- **Purpose:** catch every new tradable pool, with a timestamp.
- **Accuracy test:**
  - Recall: compare the pools found live over 24 h with an independent history scan of each program for the same
    24 h.
  - Latency: our detection time vs the slot time of the pool creation.
  - Reference: also compare with the time DexScreener first lists the pair, measured and not assumed.
- **Gate *(proposed)*:** recall ≥ 99 % per venue. The median detection latency is reported (no gate on it yet).

### C5 — Price, liquidity and slippage engine
- **Purpose:** pool price from reserves, depth or price impact for any order size, and the realized slippage per
  swap.
- **Accuracy test:**
  - Predicted output (from the pre-trade reserves and the fee) vs the actual output for 1,000 real swaps per venue.
  - Price continuity across a migration (e.g. DBC → DAMM v2), which tests problem D3.
- **Gate *(proposed)*:** median absolute error ≤ 0.5 %, p99 ≤ 2 %. Migration price jumps are explained.

### C6 — Flow metrics engine (buy pressure, traders, on-chain volume)
- **Purpose:** compute every metric in §3, causally.
- **Accuracy test:**
  - Unit tests on hand-built swap sequences.
  - A causality test: any future event changes no past value.
  - An external cross-check vs DexScreener on 30 pools (txns, buys/sells, volume for 5 min and 1 h). This is used
    only to detect bugs.
- **Gate *(proposed)*:** all unit and causality tests pass. Gaps against DexScreener > 5 % are explained.

### C7 — Wallet intelligence (creator, insiders, bundles, clusters)
- **Purpose:** know who is buying.
- **Covers:** the creator / dev wallet, the funding source of early buyers (who sent them SOL, and how long before),
  same-slot bundles, wallets that move tokens between each other, and wallet age and past behaviour.
- **Accuracy test:** manually verify a sample of 50 detected clusters and 50 "independent" wallets against raw
  transfers.
- **Gate *(proposed)*:** cluster precision ≥ 90 % on the sample. Every link rule is documented.
- **If it fails:** use only the rules that pass. Weak link rules are dropped, not guessed.

### C8 — Token and pool risk checks
- **Purpose:** check the token and pool for rug risk.
- **Covers:** mint and freeze authority, Token-2022 extensions (transfer fee, transfer hook, permanent delegate, and
  others), supply, top-holder concentration, LP burned or locked versus withdrawable, and authority changes after t0.
- **Accuracy test:** compare against on-chain account state for 100 tokens.
- **Gate *(proposed)*:** 100 % agreement on authorities. An undecodable extension is `UNKNOWN` and rejects the
  token.

### C9 — Dataset and outcome labels
- **Purpose:** build the history sample and label each pool.
- **Sample** (same calendar period, as many days as the credit budget from C1 allows; target ≥ 14 days):
  - **All** new pools on the covered venues, without any activity prefilter (fixes survivor bias D4).
  - When the budget is too small for all pools, take a uniform random sample **plus** every pool that reached ≥ 2x,
    and keep the sampling weights.
- **Labels:** written down in `research/PREREG_REBUILD.md` **before** looking at the results, and measured from t0
  using the C5 price with a minimum-liquidity floor (fixes D11):
  - `moon` (≥ 10x peak)
  - `rise` (2x–10x)
  - `flat`
  - `rug` (liquidity removed or a fast collapse; the exact rule is preregistered)
  - `unknown`
- **Gate:** the label distribution is reported per venue and per day, the `unknown` rate is reported, and each
  label has ≥ 30 independent pools. If a label has fewer, collect more days before continuing.

### C10 — Reverse-engineering atlas (descriptive, no trading)
- **Purpose:** for every pool, a second-by-second timeline of all §3 metrics, C7 wallet facts and C8 risk facts.
- **Analysis:** compare `moon`, `rise`, `flat` and `rug` at t0 + 10 s, 30 s, 60 s, 120 s and 300 s. Questions to
  answer with numbers:
  - What differs early?
  - When does the big move start relative to t0?
  - How deep is the drawdown of winners before the run (this informs the stop)?
  - How long does the peak last and how does it end (this informs the TP)?
  - What did rugs look like just before the rug?
- **Gate:** every claimed difference shows its effect size, its confidence interval and its counterexamples. Claims
  without evidence are removed.

### C11 — Radar (screening stage 1)
- **Purpose:** a cheap, fast filter that drops obviously bad pools and keeps candidates. It is built only from
  differences proven in C10 plus the C8 hard rejects.
- **Metric:** on held-out days, the share of `moon` / `rise` pools kept (recall) vs the share of `rug` / `flat`
  pools removed.
- **Gate *(proposed)*:** keeps ≥ 80 % of `moon` pools while removing ≥ 50 % of `rug` pools, out of time. It must
  beat a simple baseline (e.g. a liquidity filter alone).
- **If it fails:** stay on C11. Do not build C12 on a radar that does not pass.

### C12 — Moonshot discriminator (rules first, ML only if rules are not enough)
- **Step 1 — Rules:** a score from the C10 features.
- **Step 2 — ML (only if it beats the rules out of time):**
  - CatBoost (C++), trained on radar survivors.
  - Separate targets:
    - `P(≥ 5x before −50 %)` at snapshot t
    - `P(rug within 30 min)`
  - The split is by time (walk-forward by day) and by pool, never random rows. Features come from C6–C8 at the
    snapshot time only.
  - Probabilities are calibrated.
- **Metric:** precision at top-k, lift vs the base rate, calibration, and the net PnL of the top-k after costs, by
  venue and by day.
- **Gate *(proposed)*:** on the untouched holdout, top-decile lift ≥ 3× the base rate, and positive net expectancy
  after costs when combined with the C14 exit. A high AUC alone is **not** a pass.
- ML never overrides a C8 hard reject.

### C13 — Entry timing
- **Compare:**
  - entering at the radar pass time
  - after confirmed positive flow across ≥ 2 windows
  - after a first healthy pullback while the flow stays positive
- **Metric:** the upside still remaining at entry, the adverse move after entry, and the false-signal rate.
- **Gate:** the chosen rule beats immediate entry on net expectancy out of time.

### C14 — Exit / take-profit
- **Compare** using the C10 peak and drawdown statistics:
  - tiered TP (sell parts at fixed multiples)
  - flow-deterioration exit (sell pressure, buyer loss, creator selling)
  - trailing stop
  - hybrids of the above
  - a max-hold time
  - an emergency exit on liquidity removal
- The old "no TP" rule is re-tested here, not assumed. In earlier tests it averaged 0.80–0.84x exits on winners.
- **Gate:** the best exit is chosen on dev days only, then confirmed on the holdout. All variants are reported, not
  only the best one.

### C15 — Position sizing and cost model
- **Purpose:** size from the C5 price-impact curve (a maximum impact per order).
- **Costs:** the real network fee and priority fee from `meta.fee` of comparable transactions, the failed-tx rate
  from C6, and latency of 1–3 slots.
- **Gate:** the backtest cost per trade matches the costs measured on real transactions within the stated error.

### C16 — Full replay backtest
- **Purpose:** an event-by-event replay of C4 → C15 on the holdout days.
- **Report:** everything in strategy-plan §11 — trades, win rate, average win and loss, expectancy, profit factor,
  max drawdown, worst losses, rug exposure, slippage, failures, and latency — by venue and by day. Include failed
  variants.
- **Gate:** positive net expectancy on the holdout with enough trades for a confidence interval that excludes zero.
  If not, report honestly that no edge has been found yet, and return to the weakest component.

### C17 — Execution engine (simulation only)
- **Purpose:** build swap transactions per venue and run `simulateTransaction` only. Measure success, compute units
  and priority fees. Add kill switches and per-token and per-day limits.
- **Gate *(proposed)*:** ≥ 99 % simulation success on valid pools.

### C18 — Shadow live → C19 paper → C20 tiny live
- **C18 Shadow:** live detection and decisions with no transactions, for ≥ 7 days, compared against the C16
  expectations.
- **C19 Paper:** the same, with the simulated fills included.
- **C20 Tiny live:** **only after the owner's explicit written approval in chat.** Size is very small, with at most
  1 position, and it stops automatically on decoder errors, stale data, unexpected slippage, a failed sell, or RPC
  instability.

---

## 5. What the owner must provide

- The Ankr endpoints saved on the VPS as `ANKR_HTTP` / `ANKR_WSS` in `/root/sol/.rpc.env`. Never paste them in chat.
- Ask Ankr support whether Solana Yellowstone gRPC is included in the Premium plan.
- Approve or change the *(proposed)* gate values before each component starts.

## 6. First task

**Start with C0 only.** Deliver `reports/rebuild/C0/REPORT.md` and `sol2/PROGRESS.md`, summarize in short
Indonesian for the owner, then stop and wait for "lanjut".
