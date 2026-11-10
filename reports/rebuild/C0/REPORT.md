# C0 — Audit of existing components

Date: 2026-11-10 · Branch `rebuild` @ `7e87c2c` · reference branch `origin/spec-features` @ `16c033d`
Host: new vast.ai VPS (Ubuntu 22.04, 23 vCPU, 98 GB RAM, RTX 5060 Ti), repo at `/root/sol`, spec worktree at `/root/sol_spec`.
Tags: **[V]** verified by command or file:line · **[I]** inferred · **[U]** unknown.
No new system code was written. Nothing was pushed.

## 1. Commands run

| # | Command | Result |
|---|---|---|
| 1 | `cargo build --release` in `kumpul`, `uji`, `rekonstruksi`, `/root/sol_spec/kumpul` (cargo 1.99.1 via rustup) | all exit 0 [V] (`/root/c0log/build_*.txt`) |
| 2 | `cargo test --release` in `kumpul` | 0 tests (4 bins, 0 passed) [V] |
| 3 | `cargo test --release` in `uji` | 6 passed [V] |
| 4 | `cargo test --release` in `rekonstruksi` | 8 passed [V] |
| 5 | `cargo test --release` in `/root/sol_spec/kumpul` | 2 passed (`tarik.rs` tests only) [V] |
| 6 | `cargo test --release` in `/root/sol_spec/uji` | 14 passed [V] |
| 7 | `jq .address idl/*.json` | pump_amm `pAMMBay6…XEA`, raydium_cp_swap `CPMMoo8L…KP1C`, meteora_damm_v2 `cpamdpZC…1sGG`, meteora_dbc `dbcij3LW…MaqN`, raydium_launchpad `LanMV9sA…J3uj` [V] |
| 8 | `jq` field lists of `EvtSwap2`, `SwapResult2`, `SwapParameters2` (DAMM v2, DBC), `BuyEvent` / `SellEvent` / `CreatePoolEvent` (pump_amm), init and swap discriminators | compared with decoder code in §2 [V] |
| 9 | `grep -rlE "CPMMoo8L\|raydium_cp" kumpul uji rekonstruksi /root/sol_spec/{kumpul,uji} --include=*.rs` | no match: **no Raydium CPMM decoder exists** [V] |
| 10 | `git diff --stat rebuild origin/spec-features -- kumpul/src` | +rekon.rs, +tarik_koin.rs, +tarik_publik.rs, tarik.rs +40/−3 [V] |
| 11 | `ls -d /root/sol/data /root/sol/.rpc.env`; `systemctl list-units --all \| grep sol` | none exist on this VPS; no `sol-kumpul` service here [V] |

## 2. Program-ID and layout checks [V]

| Constant in code | Value | IDL `address` | Match |
|---|---|---|---|
| `kumpul/src/dekode.rs:6` `AMM` | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` | pump_amm.json | yes |
| `kumpul/src/dekode_tx.rs:222` `DAMM2` | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` | meteora_damm_v2.json | yes |
| `kumpul/src/dekode_tx.rs:10` `DBC` | `dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN` | meteora_dbc.json | yes |
| `kumpul/src/dekode_tx.rs:9` `LAUNCHLAB` | `LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj` | raydium_launchpad.json | yes |
| `kumpul/src/dekode.rs:5` `PUMP` (bonding curve) | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | **no IDL in `idl/`** | [U] — confirm on a real tx in C3 (curve is out of strategy scope) |

- PumpSwap `BuyEvent` / `SellEvent` / `CreatePoolEvent` discriminators in `dekode.rs:11-13` equal the IDL [V]. The field order read by `amm_trade` (`dekode.rs:113-147`) matches the IDL up to `buyback_fee`. The newer IDL fields after it (`virtual_quote_reserves, can_boost, base_supply, holder_rewards_bps, holder_rewards, creator_fee_unclaimed`) are not read [V].
- DAMM v2 and DBC `EvtSwap2` field order in `damm2_swap` / `dbc_swap2` matches the IDL [V].
- **DAMM v2 `EvtSwap2` ends with `reserve_a_amount, reserve_b_amount` (real pool reserves). The decoder never reads them and writes `rsol = vq`, a virtual reserve rebuilt from a single swap** (`dekode_tx.rs:238-262`) [V]. This is a concrete candidate cause of D3 (DAMM v2 price jumps) [I].
- DBC / LaunchLab init discriminators (`dekode_tx.rs:185-194`) and the LaunchLab `TradeEvent` discriminator equal the IDL [V].

Note: line numbers below are per file (`dekode_tx.rs` starts at line 1 with its module doc).

## 3. Component table (every source file)

Verdicts: **reuse** = take as is into `sol2` after its component gate · **fix** = take the logic, change the listed items, re-test at the gate · **replace** = write new, old file only as reference · **retire** = not used in the rebuild.

### 3.1 `kumpul/` (branch `rebuild`, same as `main`)

| File / module | Role | In → Out | Tests | Known problems (evidence) | Verdict | Target |
|---|---|---|---|---|---|---|
| `src/dekode.rs:21-54` `Baca` | borsh little-endian reader, returns `None`, never panics | bytes → ints / keys / strings | 0 | none found; bounds via `get()` (`:28`) [V] | **reuse** (add tests) | C3 |
| `src/dekode.rs:56-110` `trade`, `create` (Pump.fun curve) | curve events from `Program data:` logs | log bytes → JSON | 0 | fixed 1e9 / 1e6 division (`:64-66`, `:108`) = D9 [V]; no IDL for this program in repo [V]; curve trading out of scope (plan §2) | **fix** (raw ints, decimals from mint); low priority, only for dev / early-holder history | C3 (later) |
| `src/dekode.rs:113-148` `amm_trade` (PumpSwap buy / sell) | PumpSwap swap event | log bytes → raw-int JSON | 0 | keeps raw ints [V]; ignores 6 newer IDL tail fields [V]; reserves pre- or post-trade not verified [U]; uji treats them as post-trade price (`uji/src/main.rs:510-516`) [V] | **fix** (golden fixtures, pre/post check, also decode from inner ix) | C3, C5 |
| `src/dekode.rs:150-183` `dekode` dispatcher incl. `migrasi`, `pool` (CreatePoolEvent) | event routing | | 0 | CreatePoolEvent reads base / quote decimals, then discards them (`:173`) [V] | **fix** | C3, C4 |
| `src/dekode_tx.rs:26-61` `flatten` | outer + inner ix in execution order, incl. lookup-table keys (writable then readonly, `:37-39`) | tx JSON → `Vec<Ix>` | 0 | no outer / inner / tx index kept (D7) [V]; undecodable ix silently skipped (`:49-54`) [V] | **fix** (keep indices, count skips) | C3 |
| `src/dekode_tx.rs:63-72` `decimals` | decimals from the tx token balances | | 0 | callers fall back to 6 / 9 (`:89-90`, `:120-121`, `:241`) = D9 [V] | **replace** (decimals from the mint account; failure = `UNKNOWN`) | C3 |
| `src/dekode_tx.rs:76-101` `ll_trade` (LaunchLab) | LaunchLab curve swap | | 0 | fallback decimals [V]; venue not in initial scope | **fix** later | later venue |
| `src/dekode_tx.rs:104-147` `dbc_swap2` (Meteora DBC) | DBC curve swap, local L rebuilt from one swap | | 0 | fallback decimals (`:120-121`) [V]; fee-side heuristic `inc_in > exc_in` (`:126`) [I]; curve out of initial scope | **fix** later (history only) | later / C7 |
| `src/dekode_tx.rs:150-183` `trades` | pairs each emit_cpi event with the nearest preceding swap ix of the same program | tx → trades | 0 | failed tx give nothing (`:151-153`) = D6 [V]; pairing rule untested | **fix** (record failed tx as `swap_failed`, golden tests) | C3 |
| `src/dekode_tx.rs:198-220` `creates` (LaunchLab, DBC init) | coin / pool creation from init accounts | | 0 | no DAMM v2 / PumpSwap / CPMM pool-creation decode here [V] | **fix** + extend | C4 |
| `src/dekode_tx.rs:228-263` `damm2_swap` | DAMM v2 swap | | 0 | ignores real reserves `reserve_a/b_amount` [V]; `rsol = vq` virtual (`:262`) [V]; decimals fallback 9 / 9 (`:241`) [V]; D3: 181 of 596 graduates jump > 300x (AI_REVIEW_BRIEF §5) | **replace** | C3, C5 |
| Raydium CPMM | — | — | — | no decoder anywhere (cmd 9) [V] | **new** | C3 |
| `src/main.rs` `sol-kumpul` (live WSS) | `logsSubscribe` Pump.fun + PumpSwap, gap fill, failover | WSS logs → `pump_<hour>.jsonl` | 0 | `t` = receive time (`:68`) = D8 [V]; failed tx skipped (`:237`) [V]; no raw tx stored [V]; PumpSwap only for coins seen born live (`:96-106`) [V]; gap fill capped at `ISI_MAKS = 400` (`dekode.rs:15`, `main.rs:132`) [V]; dedupe ring 300k (`:63`) [V]. Reusable patterns: WSS failover + backoff (`:185-197`), key masking `tanpa_kunci` (`:159`) [V] | **replace** (keep failover / masking pattern) | C2 |
| `src/bin/tarik.rs` | Pump.fun history via Helius `getTransactionsForAddress` | → `hist*/` | 0 (main) / 2 (spec) | provider-specific method (`:50`) [V], availability on Ankr [U]; prefilter `MIN_TX_2M = 20` (`:151-160`) = D4 [V]; failed tx dropped (`:66-68`) [V] | **replace** | C2 |
| `src/bin/tarik_dex.rs` | program history via Helius gTFA with `status: succeeded` | → `hist_dex/` | 0 | provider-specific (`:46`) [V]; failed tx excluded (`:42`) = D6 [V]; decoded rows only, no raw tx [V] | **replace** | C2 |
| `src/bin/cek.rs` | prints decoded events for real signatures; `--dbcpool` account reader; `--program` via gTFA | sig → stdout | 0 | `--program` uses Helius gTFA (`:56`) [V]; DBC account offsets hard-coded (`:27-48`) [V], no test | **fix** → golden-fixture capture tool | C3 |
| `Cargo.toml` | deps tungstenite, native-tls, ureq, serde_json, bs58, base64 | | | builds on VPS (cmd 1) [V] | **reuse** dependency set | C1 / C2 |

### 3.2 `uji/` (branch `rebuild`)

| File / module | Role | Tests | Known problems (evidence) | Verdict | Target |
|---|---|---|---|---|---|
| `src/main.rs:20-112` constants / env knobs (`BIAYA_TX`, `JEDA`, `UMUR_MAKS`, …) | configuration | — | `BIAYA_TX = 0.0002` is an assumption (`:30-31`) = S7 [V] | **replace** (costs from C15) | C15 |
| `src/main.rs:114-155` `Ev`, `Koin` | in-memory event model | — | f64 amounts, no tx / ix index, `t` mixes chain and receive time (D7, D8) [V] | **replace** | C3 / C6 |
| `src/main.rs:424-524` `baca` (loader) | read JSONL, dedupe, fix-ups | — | PumpSwap units hard-coded `/1e9`, `/1e6` (`:510-516`) [V]; DBC / DAMM liquidity jumps > 3x replaced by the previous value — this hides D3 (`PETA_SISTEM_PER_KOMPONEN.md` §2) | **replace** | C3 |
| `src/main.rs:723` ordering | sort events | — | `sort_by_key(slot)` only (D7) [V] | **replace** (slot, tx index, ix index) | C3 |
| `src/main.rs:179-236` `fitur`, `sinyal_masuk`, `sinyal_tanpa` | 60 s wave features, entry rule V1 | causal test `:2053` [V] | fixed W = 60 s; `dev_jual` = creator wallet only (`:205`) = S3 [V]; rule has no proven edge (brief §3) | **replace** by C6 metrics; keep the causal-test pattern | C6 |
| `src/main.rs:239-300` `beli_tok`, `jual_sol`, `isi`, `trade` | x·y=k fill on the reserves after the first event ≥ signal + delay | `:2096` [V] | PumpSwap reserves pre/post [U]; never checked against real swaps | **fix** (validate on 1,000 real swaps) | C5 |
| `src/main.rs:301-420` exits K0–K11, return formula | exit rules | `:2083` [V] | no TP (S1); avg exit 0.80–0.84x on rockets (`uji/LOG-EKSPERIMEN.md`) | **fix** → variants for the C14 comparison only | C14 |
| `src/main.rs:528-567` `kelly`, `kelly_kuart` | ¼ Kelly sizing (owner-locked) | 0 [V] | formula fine; needs a unit test | **reuse** (add tests) | C15 / C16 |
| `src/main.rs:572-693` `portofolio` | equity simulation, caps, report (PF, DD, CI) | — | `LP_CAP` 1 % of real SOL [V]; costs assumed | **fix** (inputs from C5 / C15) | C16 |
| `src/main.rs:753-2036` variants V0–V4, R1–R8, reference strategies, mining, tuning | experiments | — | results only; no edge after costs (brief §3); in-sample cut S6 (`:1016-1018`) [V]; label `ret > 1` S5 (`:824`) [V] | **retire** (history only) | — |
| `src/main.rs:2038-2108` 6 tests | causality, fill delay, K5, honeypot | 6 pass (cmd 3) [V] | — | **reuse pattern** | C5, C6 |
| `src/ml.rs:15-75` `fitur_ml` (22 features) | ML base features | causal test | holdings from flow `sol/px`, supply assumed 1e9 (`:40-48`) = S4 [V] | **replace** | C6–C8 |
| `src/ml.rs:117-485` `fitur_kandidat` (66) | candidate features | causal test | same 1e9 supply (`:209`) [V]; wallet facts without funding source (S2) | **replace** (re-derive in C10 from evidence) | C10 |
| `src/ml.rs:488-536` `catboost` | calls the official CatBoost C++ binary | — | scratch dirs in /tmp once filled the disk (`uji/loop_verifikasi.sh`, last line) [V]; `CATBOOST_BIN` not installed on this VPS [V: `/root/sol` has no binary] | **fix** (scratch cleanup, time-split interface) | C12 |
| `src/ml.rs:551-641` `acak_label`, `mlp` | label shuffle, small MLP | — | plan uses rules → CatBoost; MLP not required | **retire** (unless C12 needs it) | — |
| `src/ml.rs:644-668` `auc` | Mann-Whitney AUC | 0 | none found | **reuse** (add test) | C11 / C12 |
| `loop_verifikasi.sh` | cron check of frozen candidates | — | frozen binary `uji/beku/…` and `data/` absent on this VPS (cmd 11) [V] | **retire** | — |

### 3.3 `rekonstruksi/`

| File / module | Role | Tests | Known problems | Verdict | Target |
|---|---|---|---|---|---|
| `src/main.rs:1-154` types, constants, inventory | inventory of the old JSONL files | — | bound to the old float row schema [V] | **retire** | — |
| `src/main.rs:160-186` `reconcile` | constant-product check of a swap vs post-trade reserves | `reconciliation_constant_product` [V] | float, 2 % tolerance (`REC_TOL`, `:28`) [V] | **fix** (raw ints) — method reused | C5 |
| `src/main.rs:241-341` `label`, `no_ref`, `cv` | outcome labels (2 / 5 / 10x, sustain, crash, censoring) | 6 label tests pass [V] | no liquidity floor (D11, brief §5); reference = curve price, not pool t0 | **fix** (t0 = first pool swap, liquidity floor, preregistered) | C9 |
| `src/main.rs:343-351` `venue_continuity` | price continuity across migration | — | float | **fix** | C5 |
| `src/main.rs:352-864` `main` reporting | CSV reports | — | old schema | **retire** | — |

### 3.4 Root files

| File | Role | Problems | Verdict | Target |
|---|---|---|---|---|
| `cek_aman.sh` | mint / freeze authority + dangerous Token-2022 extensions via `getMultipleAccounts jsonParsed`; unknown = unsafe | current state, not state at t0 (script header) [V]; shell + jq, not Rust (rule 7) | **fix** → port the rules to Rust; add LP burn / lock, supply, holders | C8 |
| `jalan2.sh`, `jalan_dex.sh`, `jalan_damm.sh`, `jalan_ll3.sh` | drive the Helius pullers | depend on replaced binaries | **retire** | — |
| `idl/*.json` (5) | official IDLs | addresses verified (§2) | **reuse** | C3 |

### 3.5 Branch `origin/spec-features` (reference only, rule 9)

| File | Role | Tests | Problems (evidence) | Verdict | Target |
|---|---|---|---|---|---|
| `kumpul/src/bin/tarik_publik.rs` | program history with **standard** `getSignaturesForAddress` + `getTransaction`, workers, retry, `gagal` rows | 0 | keeps successful signatures only (`:81`) = D6 [V]; writes decoded rows, not raw tx [V] | **fix** → base of `sol2/tarik` (store raw, keep failed) | C2 |
| `kumpul/src/bin/tarik_koin.rs` | per-mint history, standard methods, `terpotong` / `gagal` / `koin_selesai` ledger | 0 | successful only (`:83`) [V]; per mint, not per pool | **fix** (per pool address) | C2 |
| `kumpul/src/bin/rekon.rs` | reconciles decoded rows vs `pre/postTokenBalances` and SOL deltas; same-slot order check | 0 | float deltas (`:72-101`) [V] | **fix** → C3 random-1,000 reconciliation tool | C3 |
| `kumpul/src/bin/tarik.rs` (spec) | adds `kandidat` ledger; RPC failure = unknown, not 0 | 2 pass [V] | still Helius gTFA | **replace** (keep the ledger and the None ≠ 0 rule) | C2 / C9 |
| `uji/src/main.rs` (3268 lines), `uji/src/ml.rs` (864) | research rounds 2–3 (K14–K16, TRADE_LOG, atlas, model A) | 14 pass [V] | same base problems as §3.2; tests `waktu_chain_memilih_ts`, `token_pasti_memakai_jumlah_asli`, `dedupe_sisi_memisahkan_beli_jual` are useful patterns [V] | **replace** (reuse test patterns) | C6 |
| `research/*.py` (9), `docs/audit_trades.py` | Python analysis | — | violates the no-Python rule [V] | **retire** (not carried, rule 9) | — |
| `research/*.sh` (5) | experiment drivers | — | drive the old uji | **retire** | — |

## 4. Gate check
- Every source file in `kumpul/`, `uji/`, `rekonstruksi/`, the root scripts and the `spec-features` Rust bins has a row with role, I/O, tests, problems and a verdict [V: `find */src -type f`, `git ls-tree -r origin/spec-features`].
- Every verdict cites a file:line or a command from §1.
- **Gate C0: PASSED** — waiting for the owner's "lanjut" before C1.

## 5. Conclusions for the rebuild
1. Nothing in the data path is reusable as is: the collectors use Helius-only methods or logs, drop failed tx, keep no raw tx, and mix receive and chain time.
2. Reusable: `Baca` reader, `flatten` (after adding indices), the IDLs, the `rekon.rs` reconciliation method, the `tarik_publik.rs` paging pattern, label / censoring logic, ¼-Kelly code, AUC, CatBoost wrapper, causal-test patterns.
3. Missing entirely: Raydium CPMM decoder, DAMM v2 / CPMM pool-creation decode, raw storage, failed-tx capture, tx / ix ordering, mint-decimals lookup.
4. D3 has a concrete lead: DAMM v2 real reserves are in the event but unused (`dekode_tx.rs:262`).

## 6. Limitations
- Old live data (`/root/sol/data`) and `.rpc.env` do not exist on this VPS. No decoder was run on real chain data in C0 (that is C1 / C3 work).
- The `uji` research blocks (`main.rs:753-2036`) were audited at block level from `docs/PETA_SISTEM_PER_KOMPONEN.md` plus spot checks (`:723`, `:824`, `:1016-1018`), not line by line. They are retired, so no reuse depends on them.
- PumpSwap event reserves pre- vs post-trade: [U] until C3 / C5.
