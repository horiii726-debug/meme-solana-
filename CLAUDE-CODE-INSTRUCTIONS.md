# CLAUDE CODE INSTRUCTIONS: rhc-trader (memecoin bot, Robinhood Chain 4663)

**This file is binding.** Read all of it before doing anything in this repo. It sits on top of the owner's
`CLAUDE.md`; if the two conflict, the stricter rule wins. Written 6 Nov 2026 from a forensic audit of commit
`aed3cf3` plus the raw live data of 2 Nov 2026.

Two targets: **(1) fix the Robinhood bot (Parts 1–13, mandatory, first)**, **(2) port the system to Solana memecoins
(Part 14, after (1), data first)**.

Tags used in this file: **[V] Verified** (command run or exact code read) · **[I] Inferred** · **[U] Unknown**.

---

## PART 1: SCOPE LOCK (stay on the system's track)

1. **Work only on this repo** (`C:\Users\horii\rhc-kerja\repo`, Rust bot `rhc-trader`) and only on the task the owner
   approved. No side projects, no other chains, no other DEXes, no new strategies, no "improvements" that were not
   asked for.
2. **One task = one approved plan = one commit.** Before any edit, write a short plan (goal, files you touch, files
   you won't touch, assumptions marked verified/unverified, risks and how you'll detect them). Then **wait for the
   owner's OK**.
3. **Only the fixes listed in Part 6, in that order.** Anything else you notice: report it, don't fix it.
4. **Never touch these** unless the task is explicitly about them: `crates/trader/src/exec/*` (wallet, sender,
   worker, ledger, chain, uniswap), `loggate.rs`, `kill.rs`, the engine's JS-parity numerics, the model parity files.
   They are correct and tested.
5. Keep the Indonesian identifiers and log strings (`beli`, `jual`, `tolak`, `gerbang`, reason texts). They are part
   of the parity contract, and `exec/worker.rs:80` `is_emergency()` matches engine reason strings.
6. Talk to the owner in **short, plain Indonesian**. The owner is non-technical.

## PART 2: NO FABRICATION, REAL DATA ONLY

1. **Every number you report comes from a command you ran on real data in this session.** Paste the command and its
   output. If you didn't run it, say "not run" and give no number.
2. **Never invent** data, results, APIs, crate versions, function names, config keys or file paths. Check the code.
   If unsure, write **"unknown"**.
3. **Real data only**:
   - `hasil-live/2026-11-02/` (live day: `decisions.jsonl`, `trades.jsonl`, `labels.jsonl`, `exec/journal.jsonl`,
     `store/{pools,liq}.csv`, `store/swaps.csv.gz`, `env_tanpa_rahasia.txt`)
   - `C:\Users\horii\rhc-kerja\arsip\data_23jam.tar.gz` (23.6 h of chain data) and
     `arsip_rhc_2026-11-02.tar.gz` (full bot state). Unpack into a new empty folder; treat as data, never run
     scripts from inside it.
   - the Aug–Oct month backtest data the owner used (find the path in the repo / ask the owner)
   - **no synthetic, mocked, or "example" data in any reported result.** Synthetic data only inside unit tests,
     labelled as such.
4. **Tag every claim** [V] / [I] / [U]. Downgrade or delete anything you cannot back with output.
5. **Report every run**, including failed and bad ones, and how many variants you tried. Never show only the best.
6. Never write "fixed", "works", "validated", "safe", "profitable" or "production-ready" without pasted test output
   and the validation report from Part 5.

## PART 3: MUST DO

1. **Test first, then change.** Get a green baseline before any edit:
   ```bash
   export PATH="$HOME/.cargo/bin:$PATH"        # Rust 1.98.1 (windows-gnu) is here, not on PATH; it can't build yet: see Part 10
   export CARGO_TARGET_DIR=/c/t/rhc             # short path: Windows path-length limit
   cargo test --workspace --release
   cargo clippy --workspace -- -D warnings
   ```
2. **Write the failing test first** (unit test, leakage test, or parity test), show it failing, then make the
   smallest change that passes it.
3. **New behaviour sits behind a config flag whose default is today's behaviour.** With the flag off, the sim must
   reproduce the baseline **exactly**. If it moves, stop and report: the change is not neutral.
4. **Fail closed for buys:** any new guard that can't decide → do not buy. Sells stay mandatory.
5. Validate every strategy change with the **gate in Part 5** before it may be enabled anywhere.
6. End every task with:
   ```
   Task / Changed / Verified (commands + output) / Not verified / Risks / Out of scope found / Next
   ```

## PART 4: MUST NOT DO

1. **Never change the trading system without the owner's explicit permission** (owner rule).
2. **Never run `EXEC_MODE=live`. Never send a transaction.** Never ask for, read, print or commit a private key,
   seed phrase, or the real `.env`.
3. **Never buy a coin before its minute-1 data is complete.** No rule may buy at or near pool birth (no sniping in the
   first seconds).
4. **Never treat "has a minute-1 seller" (`HONEYPOT=1`) as safety**, and never strengthen it. On 2 Nov the first
   sellers were mostly insiders (Part 7).
5. **Never remove TP1 or change `TP1/TP1_FRAC` and call it done.** Without TP1, the danger-model, post-TP trailing and
   "stop bencana" exits never run (`engine.rs:1199-1296`). Backtest every exit variant first.
6. **Never edit tests, thresholds or the cost model to make something pass.** Never tune on the holdout.
7. **Never promote a retrained ML model** without an out-of-time comparison against the current one.
8. **Never trust** one day, a win rate > 70%, a smooth equity curve, or a total without the per-day breakdown. Treat
   it as leakage until disproven.
9. **Never use the backup RPC for trading** (owner rule: if Goldsky is down, stop trading).
10. **Never repeat failed ideas:** a 10-minute wait before buying (win 19%), scanning old coins with radar criteria
    (win 0%), radar + ML threshold (win 0%), a raw score floor of 0.50 (blocked every winner), loosening the coin
    age limit (no effect).
11. Never run destructive commands (`git reset --hard`, `git checkout -- .`, force-push, `rm -rf`). Never discard
    the owner's uncommitted work.
12. Never add crates, dependencies or new files without asking. No `unwrap()`/`expect()` on new live-path code; no
    silenced errors; no `unsafe`.
13. Never hard-code a rug operator's exact amounts or addresses as a rule (overfitting to one actor).
14. **Never remove or replace quarter-Kelly sizing with compounding** (owner rule; `bot.rs` `kelly()`). Never change
    `KELLY_SEED`, `KELLY_SEED_FROM`, `KELLY_MAKS` or `KELLY_MIN` without the owner's explicit OK. Only report their
    effect, measured on real data.

## PART 5: VALIDATION GATE (before anything is enabled or deployed)

A change counts as **"good"** only if ALL of these pass, measured on real data:

| # | Check | Pass condition |
|---|---|---|
| 1 | `cargo test --workspace` + `cargo clippy -- -D warnings` | all green, output pasted |
| 2 | Flag OFF | 2 Nov sim and month backtest **identical** to baseline |
| 3 | Leakage | a feature at block t uses only data with block ≤ t; a test that shifts data 1 block into the future must fail |
| 4 | Several days | better than baseline on **most separate days** (2 Nov, 23.6 h archive, the month data split by day), not only in the total |
| 5 | Payoff | report trades, win %, avg win, avg loss, RR, **breakeven win %**, PF, PnL, **PnL without the top 5 trades**, max drawdown, per day. Realised win % must be above breakeven win % |
| 6 | Costs | run with `SELL_IMPACT=1` and real gas/fail rates. Also run with costs doubled: must still not lose |
| 7 | Robustness | change the seed; shuffle labels for any ML change (AUC must drop to ~0.5) |
| 8 | Trial count | report every variant tried |
| 9 | Shadow | only then shadow mode: **≥ 50 trades, win ≥ 35%, equity rising, 2 consecutive days** |
| 10 | Live | only on the owner's explicit word, **$5 per order max**, daily loss limit on, quarter-Kelly sizing kept |

If any check fails or is skipped: say so plainly, and the change stays OFF. "No edge found" is a valid, honest result.

---

## PART 6: WHAT TO FIX AND TEST, IN ORDER

### State of the repo right now [V]

`C:\Users\horii\rhc-kerja\repo` is on HEAD `aed3cf3` **with uncommitted work in 6 files** (`features.rs`,
`features/lib.rs`, `bot.rs`, `cfg.rs`, `live.rs`, `sim.rs`). That work already contains:
- **LP cap fails closed** (`bot.rs`: `lp_m1` NaN/0 → reject "LP tidak diketahui").
- **Root cause of the `lp_m1 = 0` bug**: datasets without a per-coin `ethUsd` (e.g. the 23.6 h set) made every USD
  feature and `lp_m1` NaN. `sim.rs` now uses the ETH/USDG price at pool birth (`eth_at(init)`), as live does.
- **`DOMPET_RUG` (default off)**: `dompet_menit1()` in `features.rs` lists the wallets that bought and/or sold in
  minute 1 (from token Transfer logs, following router hops). Wallets that **bought then sold** in minute 1 of a pool
  that **later rugged** (> 80% LP removed, only once it has actually happened: causal) go into a blacklist; a new
  coin whose minute-1 wallets include one is rejected ("dompet perug"). Saved in the account state.
- `live.rs` keeps watching a pool's liquidity after its PoolSim is gone, so late rugs still feed the blacklist.

**Do not discard this work.** Test it first (Step 0).

### Step 0: Baseline and test the existing work (no new edits)
- Run `cargo test` and `clippy` (Part 3.1). Paste the output.
- Run the 2 Nov sim with the owner's live `.env` and every new flag OFF. Expected per the owner's notes: **$28.95,
  4 buys, 0 wins**. Because the LP cap fix and `eth_at` change what the 1-day sim sees, compare with `git stash`
  (baseline) vs the working tree **only after asking the owner** (stash is reversible; still ask). Report both.
- **Done when:** tests green, clippy clean, both sim results reported side by side.

### Step 1: `TP1` / `TP1_FRAC` in `.env` are silently ignored [V]
- `main.rs:39` `PROCESS_ENV_KEYS` = `["MAX_FEE","DS","RADAR_BOOT_HARI","START_BLOCK"]`; the engine reads `TP1` and
  `TP1_FRAC` only from the process environment (`engine.rs:164-165`).
- Fix: add `"TP1", "TP1_FRAC"` (array size 6). Log the active exit config at startup.
- Test: a sim with `TP1=2.0`, `TP1_FRAC=0.5` in `.env` → every `tp1V` in trades ≥ 2.0; without them the output is
  byte-identical to Step 0.

### Step 2: Config numbers that don't parse fall back silently [V]
- `cfg.rs:118` `num()`: `QTHR=0,8` runs as 0.90 with no warning.
- Fix: `fn angka(k, s) -> Result<f64, String>`; an unparseable value stops the bot with `ERROR: KEY=value: bukan
  angka` (same idiom as `kelly_seed`).
- Test: `"0.8"` → Ok; `"0,8"`, `"abc"` → Err; `config/trader.env.example` still loads.
- Report but don't change: `on()` treats anything other than `1`/`true` as false.

### Step 3: Test the rug filters on real data (config only, then decide)
For each dataset **and each day separately**, with `SELL_IMPACT=1`, full Part 5 report:

| Exp | Setting | Question |
|---|---|---|
| E1 | `DOMPET_RUG` 0 / 1 | Does the wallet blacklist block factory rugs without killing winners? |
| E2 | `HONEYPOT` 1 / 0 | What does dropping "require a seller" do? |
| E3 | `UMUR_MIN_BLOK` 0 / 650 / 700 / 800 / 900 | Does waiting just past the factory's pull window (620–628 blocks) help? Stay ≤ 900: above ~930 the live gate rejects as `data basi` (`bot.rs:200`), so sim and live differ |
| E4 | `QTHR` 0.80/0.90 × `AMBANG_MIN` 0/0.3/0.4/0.5 | Does an absolute score floor help? |

### Step 4: Optional second rug filter `beli_awal` (only if E1 isn't enough)
- One helper in `features.rs`: count the buys in the first 10 blocks after pool birth (swaps only, `blk ≤ init+600`).
- Flag `SARING_ORANG_DALAM` (default 0): 1 = reject `beli_awal ≥ BELI_AWAL_MAKS` (default 3); 2 = also reject any
  minute-1 seller. `HONEYPOT=1` with 2 = config error.
- Add `beli_awal` to `Keputusan` (all construction sites; replay uses `unwrap_or(0)`) and to `decisions.jsonl`.
- Tests: count 3 on the real `0x62be34f6` swaps (buys at +3, +3, +4); +11 not counted; a sell not counted; both
  native orientations; a swap at init+601 changes nothing; flag 0 is byte-identical.

### Step 5: Exit / TP sweep (config only)
`TP1/TP1_FRAC` ∈ {1.2/0.6 (baseline), 1.5/0.4, 2.0/0.5, 3.0/0.33, x/0 (no TP1)}, `SELL_IMPACT=1`, per day. Pick only
a variant that wins on **most days** and beats its own breakeven win rate. Remember Part 4.5.

### Step 6: ML retrain safety
- **Champion/challenger in `belajar.rs` `selesai_latih`**: hold out the newest labels (e.g. the last 2 days) from
  training, score them with old and new models, and swap only if the new one is not worse on AUC **and** gate PnL.
  Log both. Test: a challenger trained on shuffled labels must be rejected.
- Make the retrain triggers (win < 0.65, rug > 0.20) config values; default = today's.
- Measure and record the entry filter's walk-forward AUC per day (undocumented today).
- Walk-forward by day only, purge ≥ 324,000 blocks (`HZ_LIVE`), newest ≥ 3 days as a holdout opened once.
- Don't change the label `ret > 1.0` on the live path until a research comparison shows a better target.

### Step 7: Relaunch settings (the owner decides; config only, after Part 5 passes)
```
EXEC_MODE=shadow
# sizing stays QUARTER KELLY + compounding (owner rule, bot.rs kelly(): best f / 4). Do not replace it.
# KELLY_SEED / KELLY_SEED_FROM / KELLY_MAKS: the owner's choice. Claude Code may only REPORT, on real data, how each
# setting changes PnL and drawdown (e.g. seed on vs off, KELLY_MAKS 0.10 vs 0.15), never change it on its own.
EXEC_MAX_ORDER_USD=5
EXEC_DAILY_LOSS_LIMIT_USD=<owner picks>
SELL_IMPACT=1
```

---

## PART 7: EVIDENCE (why these fixes)

### 7.1 What went wrong on 2 Nov [V]
- Live config was more aggressive than the code defaults: `QTHR=0.80` (top 20%), `KELLY_MAKS=0.15`, Kelly seeded
  from the backtest (~13% per trade from trade #1), `EXEC_DAILY_LOSS_LIMIT_USD=0`, `EXEC_MAX_ORDER_USD=0`,
  `HONEYPOT=1`.
- Money: $41.38 → $29.34 (−28.7%). 3 real buys, all rugged. 531 `sell_presim_reverted`: sells reverted because
  the **LP was fully removed**, not because the token blocked transfers. The pre-buy sell simulation passed,
  correctly.
- Backtest RR 0.87:1 → breakeven win rate 53.5%; live win rate 40–48%. Cause: TP1 sells 60% at 1.2× (caps wins);
  a rug costs −100%.

### 7.2 The rug factory [V]
| Pool | Dev | Score | Minute-1 sellers | LP at init | Buys in the first 10 blocks | LP removed |
|---|---|---|---|---|---|---|
| `0x62be34f6` | `0x31a253…` | 0.491 | 16 | 25725498245903809899643 | +3, +3, +4 | init + 626 |
| `0xbbd0e7e0` | `0x8a9d4a…` | 0.486 | 20 | 25725498245903809899643 | +3, +4, +4 | init + 628 |
| `0x701675ba` | `0x6d755d…` | 0.463 | 19 | 25725498245903809899643 | +3, +4, +4 | init + 620 |

Same LP to the wei, same scripted buy sizes (12.8e12 / ~1.998e12 / 10.496e12 wei), fee 1%, LP ≈ $20k, fresh dev
wallet each time. The bot bought at init + 603; the LP was pulled 1.7–2.5 s later. The insider sold just before the
pull. [V] Model of the operator (fake early demand → sell in minute 1 → pull just after the 60 s decision) [I].

### 7.3 Rug signals on 2 Nov, pools still alive at 60 s (n = 385, 9 later rugs) [V]
| Signal | Later rug with | Without | Catches the 3 real buys |
|---|---|---|---|
| any minute-1 seller | 9/213 | 0/172 | 3/3 |
| ≥ 3 buys in the first 10 blocks | 8/158 | 1/227 | 3/3 |
| an early sniper sells its exact token amount | 0/7 | 9/378 | 0/3 (useless) |

Whole day (n = 460 pools with LP; the store window is limited, so rug counts are a lower bound): minute-1 seller
69/273 = 25% vs 15/187 = 8%. Rug timing after init: 0–300 blocks: 55, 300–600: 20, **600–700: 5**, later: 4.
An identical LP amount **alone** is not a flag (launchpad defaults, e.g. 32156000099526845215062 ×39, 0 rugs).
**Caveat: 9 events from one day. A lead, not proof** [I]. That's why Part 5 demands several days.

### 7.4 All code findings [V]
| # | Finding | Where | Status |
|---|---|---|---|
| F1 | Rug factory beats the fixed 60 s decision | design | Step 3/4 |
| F2 | `HONEYPOT=1` selects insider-active pools | `bot.rs:245` | Step 3 (E2) |
| F3 | Relative-only gate: buys the top (1−QTHR) of the last 100 scores whatever the market; scores 0.205–0.797 passed | `bot.rs:186` | Step 3 (E4) |
| F4 | LP cap failed open on missing `lp_m1` (live path too) | `bot.rs:283` | fixed in uncommitted work, test in Step 0 |
| F5 | `TP1/TP1_FRAC` in `.env` ignored | `main.rs:39` | Step 1 |
| F6 | No TP1 → no danger/trailing/bencana exits | `engine.rs:1199-1296` | Step 5 caution |
| F7 | Payoff asymmetry (RR 0.87) | engine config | Step 5 |
| F8 | ML label `ret > 1.0` ignores magnitude and depends on the exit policy | `belajar.rs:454` | Step 6 (research) |
| F9 | Retrain swaps the model on a parity check only | `belajar.rs:528-541` | Step 6 |
| F10 | Retrain triggers tuned to an 82%-win regime | `belajar.rs:501` | Step 6 |
| F11 | DL AUC guard dormant for the first 500 outcomes; scaler fit on Aug–Sep only | `belajar.rs:460` | Step 6 |
| F12 | Paper sells ignore own price impact by default (`SELL_IMPACT=0`) | `cfg.rs` | use `SELL_IMPACT=1` in tests |
| F13 | Dev = first-mint `tx.from`; fresh wallets beat it; `Ok(None)` passes | `live.rs:345` | `DOMPET_RUG` addresses part of it |
| F14 | No LP-owner / dev-balance / insider features | `features.rs` | later research |
| F15 | Bad numbers in config fall back silently | `cfg.rs:118` | Step 2 |
| F16 | Kelly seed sized live at ~13% from trade #1 (quarter Kelly itself is the owner's rule and stays) | live `.env` | report only; owner decides |
| F17 | `UMUR_MIN_BLOK` > ~900 behaves differently live vs sim | `bot.rs:200,237` | Step 3 note |
| F18 | `unwrap()` on locks/sorts in live path (panic → kill switch; safe, noisy) | `bot.rs:94` etc. | report only |

### 7.5 Correct and verified, don't "fix" [V]
Exactly-once log gate · write-before-broadcast ledger · kill switch inside `sender.rs` · mode-600 key handling ·
golden calldata test · pre-buy sell simulation · bit-exact JS parity · splitmix64 RNG · fail-closed contract
verification at startup.

### 7.6 Reproduce the key numbers
Run from `hasil-live/2026-11-02/` with `python -I`:
```python
import csv, collections
pools = {r['pool_id']: r for r in csv.DictReader(open('store/pools.csv'))}
liq = collections.defaultdict(list)
for r in csv.DictReader(open('store/liq.csv')):
    liq[r['pool_id']].append((int(r['block']), int(r['liq_delta'])))
for p in ('0x62be34f6', '0xbbd0e7e0', '0x701675ba'):
    pid = next(k for k in pools if k.startswith(p)); init = int(pools[pid]['init_block'])
    print(p, [(b - init, d) for b, d in liq[pid]])
# expected: (0, +25725498245903809899643) then (626 / 628 / 620, -25725498245903809899643)
```

---

## PART 8: SYSTEM MAP (one screen)

```
chain 4663 (0.1 s blocks) --300 ms poll--> loggate (exactly-once) --> event store
pool born --> dev lookup (first mint tx.from)
init+600 blocks (60 s): minute-1 swaps + Transfer logs --> 34 features --> score = (CatBoost + MLP)/2
bot.rs gerbang: excluded? -> ml_ok (top 1-QTHR of last 100, floor AMBANG_MIN) -> dev rule -> UMUR_MIN_BLOK
   -> DOMPET_RUG -> HONEYPOT -> kill / slots / exposure / daily brake -> quarter-Kelly -> LP cap -> pending
engine PoolSim, entry at decide + LAT_BLOCKS: exits = rug (>80% LP removed) | LP>30% | dump >=5% LP | tanda pucuk
   | stop -40% (before TP) | TP1 1.2x sell 60% | after TP1: model bahaya | trailing | stop bencana
ModelOrder --mpsc--> exec worker (age <= 1.5 s, sell simulation, pool-key check) --> sign --> broadcast
```

## PART 9: OWNER'S RELAUNCH CONDITIONS (unchanged, binding)
1. Fix `lp_m1` and re-run the simulations.
2. Test exits (e.g. TP1 2.0/0.5) on several different days.
3. Re-check the honeypot rule.
4. Shadow ≥ 50 trades, win rate ≥ 35%, equity rising, 2 consecutive days.
5. Only then live, with **$5**, a dedicated bot-only wallet.

---

## PART 10: TEST STATUS OF THE `perbaikan-rug` WORK

Repo `C:\Users\horii\rhc-kerja\repo`, branch **`perbaikan-rug`**, HEAD `aed3cf3` + uncommitted changes in 6 files
(Part 6). Command run on 6 Nov 2026:

```bash
export PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR=/c/t/rhc
cargo test --workspace --release
```

Result [V]: **build failed before any test ran** (13 min):
```
error: error calling dlltool 'dlltool.exe': program not found
error: could not compile `windows-sys` (lib) due to 1 previous error
```
Cause: the PC has Rust 1.98.1 **`stable-x86_64-pc-windows-gnu`**, but no MinGW tools (`gcc`, `dlltool`). There is no
Visual Studio (MSVC) and no WSL distribution either. This is a toolchain problem, not a problem in the bot code.

Fix (ask the owner first; each one installs software):
1. **Recommended: WSL Ubuntu**, the same OS family as the VPS the bot runs on:
   `wsl --install -d Ubuntu` → inside Ubuntu: install Rust with rustup, `sudo apt install build-essential pkg-config`,
   then build the repo from `/mnt/c/Users/horii/rhc-kerja/repo` (or a copy in the Linux home folder for speed).
2. Alternative: MSYS2 with `mingw-w64-x86_64-toolchain` (provides `gcc` and `dlltool`) on PATH.
3. Alternative: Visual Studio Build Tools (C++) + `rustup default stable-x86_64-pc-windows-msvc` (large download).

Until `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` are green, the `perbaikan-rug` work is
**unverified** [U]. Paste their output here.

Not yet covered by tests in that work (write these first, Part 3.2):
- the `lp_m1` NaN/0 → `"LP tidak diketahui"` rejection (unit test like `dev_rule_blocks_known_ruggers...`);
- `dompet_menit1()`: a router hop is followed to the real wallet; transfers in the pool's own block are skipped;
  a wallet that only buys is not in `flip`;
- `DOMPET_RUG`: a wallet is blacklisted **only after** its pool's rug has happened (causal), and a blacklisted wallet
  in a new coin's minute-1 list → reject; `DOMPET_RUG=0` behaves exactly as before;
- the blacklist survives a restart (`ke_json` / `dari_json`) and the replay of stored decisions gives the same
  result as live;
- `sim.rs` `eth_at(init)`: a dataset without per-coin `ethUsd` now gives finite `lp_m1` (the root cause of the
  1-day-sim LP-cap bug).

---

## PART 11: REA (Reverse Engineer Anything): install and purpose

- What: `morluto/rea`, npm package `rea-agents` 6.8.0, MIT. One CLI/MCP server bridging AI agents to Ghidra, IDA,
  Hopper, Cutter, pwndbg, pwntools, mitmproxy, JADX, and JS/website analysis.
- Purpose here: **prove that a compiled `rhc-trader` binary (e.g. on a VPS) is built from this audited source.**
  Compare the contract addresses (`exec/chain.rs`: PoolManager `0x8366a39c…`, Universal Router `0x204faca1…`,
  Permit2 `0x000000000022D473…`, V4Quoter `0x8dc178ef…`, StateView `0xf3334192…`), RPC hosts, log strings and
  version strings. Extra addresses or hosts in the binary = stop and report.
- Status on this PC [V]: cloned into the session folder; **`npm ci` fails with `EBADENGINE`**. REA needs Node
  `^22.23.4 || ^24.22.1 || >=26.12.1`; the PC has `v24.14.1`.
- Install (ask the owner before upgrading Node or installing anything):
  ```bash
  # 1. upgrade Node to >= 24.22.1 (official installer from nodejs.org)
  git -c core.longpaths=true clone https://github.com/morluto/rea.git
  cd rea
  npm ci --no-audit --no-fund          # full install; --omit=dev is NOT enough (the build needs devDependencies)
  npm run build:cached
  node scripts/rea.mjs --version       # expect 6.8.0
  node scripts/rea.mjs capabilities    # provider list
  ```
- Real binary analysis needs Ghidra (or IDA/Hopper) installed separately. Never upload the binary or the key
  anywhere. REA runs locally.

---

## PART 12: ML / DL PROTOCOL (full detail)

### What exists [V]
| Part | Detail |
|---|---|
| Entry score | `(CatBoost + MLP) / 2`, or CatBoost alone when the DL guard trips (`belajar.rs:443`) |
| CatBoost | `filter_cat.json`: 200 trees, depth 6, 34 features, trained 19 Sep 2026, n=2,797, `thr_start` 0.747. Retrain: official C++ binary, `--iterations 200 --depth 6 --learning-rate 0.08 --random-seed 1` |
| MLP | 34-128-64-32-1, ReLU, sigmoid, Adam lr 1e-3, L2 1e-3; quantile scaler fitted on base data only |
| Online learning | Each outcome: one Adam step on that coin + 32 replayed from the last 500 |
| Label | `y = ret > 1.0` (`belajar.rs:454`) |
| Retrain triggers | Last 20 picks: win < 65% or rug (ret < 0.1) > 20%, with ≥ 30 scores since the last fit; or 300 new labels |
| Weights | Base 0.3; new labels `0.5^(age_days/3)`; labels older than 45 days dropped |
| DL guard | Out-of-sample AUC over the last 500 outcomes: off < 0.52, on > 0.55 (only after 500 outcomes exist) |
| Exit heads | `p_bahaya` AUC 0.823, `p_naik` AUC 0.795 (205,243 snapshots). **The entry filter's AUC is not documented.** For old coins the owner measured 0.465 (below random) |

### Rules for any training or evaluation
1. **Data:** Aug–Sep base, the Sep–Oct month, 2 Nov live, the 23.6 h archive. Keep rugs, dead and flat coins.
   Never survivors only.
2. **Splits:** time-ordered walk-forward folds **by day**. Purge ≥ the label horizon (`HZ_LIVE` 324,000 blocks
   ≈ 9 h) plus an embargo after each test fold. **Never random K-fold.**
3. **Holdout:** the newest ≥ 3 days, opened **once**, at the end. Never tune on it.
4. **Targets (research, compare on the same folds before any switch):** `p_rug` (LP removed > 80% within the
   horizon), `p_besar` (ret ≥ 1.5), a clipped `log(ret)` regression, versus today's `ret > 1.0`. A new target also
   needs the exit policy fixed first, because labels depend on it.
5. **Metrics per fold:** ROC-AUC, PR-AUC (for rug), Brier score, calibration curve, top-20% precision, and the
   **gate's trading PnL** under the real cost model (`SELL_IMPACT=1`). Report mean ± spread across days.
6. **Calibration:** fit isotonic or Platt on a time-ordered validation fold **before** using any absolute threshold
   (`AMBANG_MIN`) or probability-based sizing. Show the calibration curve.
7. **Champion/challenger:** a new model replaces the old one only if it is not worse on the most recent out-of-time
   fold in both AUC and gate PnL. Log the comparison.
8. **Attacks:** shuffle labels (AUC must fall to ~0.5); shift features 1 block into the future (the leakage test must
   fail); drop the top 5 trades; change the seed; double the costs.
9. **Parity:** a shipped model must pass the Rust-vs-CatBoost check (≤ 1e-5) and the stored `models/*_check.json`.
10. **Log** config hash, seed, data range and number of trials for every experiment.

---

## PART 13: NEW ANTI-RUG FEATURES (research, after Steps 0–6)

Each one is a separate task: Rust-only implementation in `crates/features` (one implementation, used by live and
sim), a leakage test (uses only data with block ≤ init + 600), behind a flag (default off), then the Part 5 gate.

| Feature | Data source | Why | Cost |
|---|---|---|---|
| `beli_awal`: buys in the first 10 blocks | minute-1 swaps | factory fingerprint; caught 3/3 on 2 Nov (Step 4) | none (swaps already loaded) |
| insider-seller share: minute-1 sellers whose tokens came from the creator or a non-pool wallet, not from the PoolManager | minute-1 Transfer logs (already fetched; `dompet_menit1` builds buyers/sellers) | "the first seller is the rugger" | none |
| creator token balance % at 60 s | Transfer logs from the mint | a dev holding supply can dump | none |
| LP provider = creator | `tx.from` of the `ModifyLiquidity` (add) transaction | a creator-owned, unlocked LP can be pulled at any time | +1 RPC call per coin |
| dev funding source | the dev's first incoming ETH transfer | links fresh dev wallets to known ruggers (the factory used a new dev each time) | +1–2 RPC calls; must finish inside the 60 s budget or the coin is skipped (fail closed) |
| LP removal rights / lock | position owner of the LP (PositionManager NFT or direct) | locked LP cannot be rugged that way | research [U]: how LP is held on chain 4663 is not verified |

Rules for these: a feature that can't be computed in time → **do not buy** (fail closed). Never hard-code one
operator's amounts or addresses. Any new feature fed to the ML needs a retrain, so it goes through Part 12.

---

## PART 14: SOLANA PORT (second target; Robinhood fixes stay mandatory)

### 14.0 Order of work (binding)
1. **Robinhood first.** Part 6 Steps 0–3 (baseline, `TP1` env keys, strict config, rug-filter experiments) and Step 6
   (ML champion/challenger) are fixed and tested **before** any Solana code. They live in the shared core (`bot.rs`
   gate, `cfg.rs`, `belajar.rs`) that the Solana bot will reuse, so bugs fixed there are fixed for both.
2. **Solana data before Solana trading.** Collect several days of real data with our own pipeline (14.2), and prove an
   edge with the Part 5 gate on that data, **before** writing any Solana execution code.
3. Structure (a new crate or a separate repo) is the **owner's decision**: ask first (Part 4.12).

### 14.1 Why this is a port, not a network switch [V]
Robinhood Chain is EVM + Uniswap v4. Solana is not EVM. Different transactions, keys (ed25519), accounts, programs,
math per DEX, and rug types. Everything that touches the chain must be rewritten. The risk/strategy/ML framework can
be reused.

### 14.2 Data pipeline: own pipeline, no gRPC, no Jito, no third-party APIs [V tested 6 Nov 2026]
- Source: Solana public RPC websocket `wss://api.mainnet-beta.solana.com`, method `logsSubscribe` with
  `{"mentions": [<program id>]}`, commitment `confirmed`. Events are decoded **ourselves** from `Program data:` log
  lines (Anchor events: first 8 bytes = `sha256("event:<Name>")[:8]`).
- Proven for pump.fun (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`): a 30 s run gave 2,616 transactions,
  **1,508 TradeEvents, 8 CreateEvents**, 0 reconnects. TradeEvent layout used (after the 8-byte tag): mint (32),
  sol_amount u64 (lamports), token_amount u64 (6 decimals), is_buy u8, user (32), timestamp i64,
  virtual_sol_reserves u64, virtual_token_reserves u64, real_sol_reserves u64, real_token_reserves u64, then more
  fields (not used yet). CreateEvent: name, symbol, uri (borsh strings), mint (32), bonding_curve (32), user (32).
  Decoded values are plausible (initial virtual SOL ≈ 30, timestamps current) [V]. **Re-verify the layout against the
  program's published IDL before relying on it** [U].
- Prototype scripts (Python, analysis only): `C:\Users\horii\rhc-kerja\solana\skrip\{rpc.py,ws.py,pump.py,kumpul.py,
  analisa.py}`. Run with `python -I kumpul.py <out.jsonl> <seconds>` and `python -I analisa.py <data.jsonl> [decision_s]
  [horizon_s]`. Data: `C:\Users\horii\rhc-kerja\solana\data\pump_2026-11-06.jsonl` (60-min run). The production
  collector must be Rust (owner rule: no Python in the bot).
- `getBlock` is **too heavy** for real time: 2.5–3.9 s and ~2 MB per slot, vs 0.4 s slots [V]. Use it only to backfill.
- The old collector `rhc-kerja/solana/kumpul` used PumpPortal (third party). Trades need a paid key, so it recorded
  0 trades [V]. Do not use it.
- Must add before production: (a) a gap backfill after reconnect (`getSignaturesForAddress` back to the last seen
  signature + `getTransaction`); (b) dedupe by signature and order by slot (the Solana equivalent of `loggate.rs`);
  (c) one decoder per program: pump.fun bonding curve, PumpSwap, Raydium (AMM v4, CPMM, CLMM, LaunchLab), Meteora
  (DLMM, DAMM), Moonshot. Each decoder gets golden tests built from **real transactions** (signature + expected
  decoded values), like `exec/uniswap.rs`'s golden calldata test.
- Public RPC rate limits are tight. For multi-day collection, a normal paid RPC (not gRPC) may be needed: owner's
  decision.
- Coins that graduate from pump.fun move to PumpSwap. Until that decoder exists, post-graduation prices are
  **missing**, so peaks of graduated coins are undercounted [V by design].

### 14.3 Module mapping

| Robinhood module | Solana | What changes |
|---|---|---|
| `live.rs` polling `eth_getLogs` | **rewrite** | websocket `logsSubscribe` per program + backfill + reconnect |
| `loggate.rs` | **rewrite (same idea)** | exactly-once by signature, slot order, gap detection |
| `engine.rs` PoolSim (Uniswap v4 math, JS parity) | **rewrite** | per-DEX math: bonding curve (virtual reserves, constant product), CPMM, CLMM, DLMM bins. The JS-parity constraint does **not** apply (no JS training data on Solana); the rule becomes: one Rust implementation used by both sim and live |
| `features.rs` (34 minute-1 EVM features) | **rewrite** | Solana feature set (14.5), computed at the decision age (minutes, not 60 s) |
| `bot.rs` account, ¼ Kelly + compounding, brakes, gates | **keep** | `Keputusan` gets Solana fields; gate order stays fail-closed |
| `belajar.rs` CatBoost + DL, AUC guard | **keep framework** | retrain from zero on Solana data; Robinhood models are invalid on Solana |
| `kill.rs`, `exec/ledger.rs` journal | **keep** | same kill switch and write-before-broadcast |
| `exec/wallet.rs` (secp256k1, mode 600) | **rewrite** | ed25519 keypair, same mode-600 discipline, same no-echo tests |
| `exec/uniswap.rs`, `sender.rs`, `worker.rs` | **rewrite** | build program instructions directly (no aggregator API), compute-budget priority fee, `simulateTransaction` before every send, send to several RPCs, confirm by signature status, create/close associated token accounts |
| `exec/chain.rs` contract checks | **rewrite** | verify each program id is executable and owned by the expected loader at startup; fail closed |
| `radar.rs` (DEX Screener) | **replace** | own pipeline only; no GMGN/DexScreener dependency |
| `kelas.rs` classification | **adapt** | on-chain metadata (Metaplex) instead of an external API |

### 14.4 Solana rug / honeypot checks (fail closed: any check that can't be done → don't buy)
| Check | Why |
|---|---|
| mint authority revoked | otherwise the dev can mint unlimited supply |
| **freeze authority revoked** | otherwise the dev can freeze your token account: the Solana honeypot |
| Token-2022 extensions: transfer fee, transfer hook, permanent delegate, non-transferable, default-frozen → reject | hidden taxes, blocked sells, tokens taken back |
| sell simulation (`simulateTransaction`) of buy + sell before buying | same idea as `EXEC_CEK_JUAL` |
| creator holdings % and creator sells | on a bonding curve the rug is the **dev dumping** (no LP to pull before graduation) |
| bundled launch: many buys in the creation slot / from wallets funded by one source | fake demand, the same pattern as the Robinhood rug factory (Part 7.2) |
| wallet clusters by funding source; wallet age | a "wave" made of linked fresh wallets is fake |
| wash trading: the same wallets buy and sell repeatedly | fake volume |
| creator history: the creator's previous coins and their outcome | serial launchers |
| after graduation: LP burned or locked | an unburned LP can be pulled |
| the first sellers are insiders | apply the Robinhood lesson: never require a seller as a safety check |

### 14.5 Strategy: "buy waves" on coins a few minutes old
- The owner's idea: buy coins that are already a few minutes old when a wave of real traders enters (high buy
  pressure, many active wallets). No sniping at birth (Part 4.3 holds on Solana too).
- **Warning from Robinhood data:** "scan older coins with radar criteria" gave win 0% / −82% and "wait 10 min" gave
  win 19% on Robinhood. A wave strategy must be proven on Solana data first, not assumed.
- **A wave is exactly what ruggers fake.** Only count waves of distinct, unlinked, non-bundled wallets (14.4).
- Candidate features at decision age (each with a leakage test): unique buyers in the last 60 s / 5 min, growth of
  unique buyers, net SOL inflow, buy/sell count ratio, median buy size, share of volume from the top 3 wallets,
  creator holdings %, bundle share, cluster share, bonding-curve progress (real SOL), market cap, time since birth.
- Exits: design fresh and test on data. Do **not** copy TP1 1.2×/60% (Part 7.1 payoff problem). Add exits for
  creator sells and a large-holder dump.
- Sizing: **¼ Kelly + compounding (owner rule)**, plus a per-trade cap as a share of the pool / curve liquidity
  (like `LP_CAP`).

### 14.6 Solana cost model (must be in every backtest; Part 5.6)
Base fee (5,000 lamports per signature), priority fee (measured, not assumed), DEX/curve fees (read from on-chain
config per program: values [U]), associated token account rent (refundable on close), slippage from the curve math
at our size, failed-transaction rate (measured), and **sandwich risk without Jito**: measure it on real data (how
often a buy of our size gets front-run), use tight slippage. Never assume zero.

### 14.7 Validation for Solana (same gate, Part 5)
Several days of collected data, walk-forward by day, the full report, then shadow ≥ 50 trades with win ≥ 35% and
rising equity on 2 consecutive days, then live only on the owner's word with a small amount. Same MUST / MUST NOT
rules, including: never live, never touch keys, never invent data.

### 14.8 First Solana data result (6 Nov 2026)
**PENDING:** the 60-minute pump.fun collection is running. Results are added here when it finishes (coins per hour,
share reaching 2×/5× after age 3 min, wave vs no wave, dev-sold vs not). One hour is a first look, not proof.

---

## WHEN UNSURE
Stop. Say in one line what is unclear and what you need. A question is cheaper than a wrong live trade.
