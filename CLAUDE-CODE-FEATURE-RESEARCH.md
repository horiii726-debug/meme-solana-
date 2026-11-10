# CLAUDE CODE: SOLANA MEMECOIN RESEARCH (experiment on your own, real data only)

## 0. The owner's request
The trading system still loses money. **Study real Solana memecoin data yourself, then answer two things from that
data:**
1. **What is weak in the owner's system?** Test each of its rules on the data.
2. **Which new features should be added?** Discover them in the data, and prove each one before recommending it.

Solana memecoins only. Everything you report must come from data you actually ran. "No edge found" is an honest,
valid answer.

---

## 1. Hard rules
1. **Real data only.** Every number comes from a command you ran on the collected chain data, with the command and
   output pasted. No synthetic data, no numbers from memory, no numbers from websites or other bots' dashboards.
2. **Never invent** data, fields, program layouts or results. If something isn't verified, write **"unknown"**.
3. **Research only.** Never trade, never run live, never send transactions, never ask for or touch a private key.
   Never change the trading bot. Results go to the owner as a report; the owner decides.
4. **Quarter Kelly + compounding is the owner's sizing rule.** Don't replace it. You may only report how results
   change under it.
5. **No look-ahead.** A feature at decision time t uses only data with time ≤ t.
6. **Log every experiment**, including failed ones: date, hypothesis, definition, data range, result, kept/rejected.
   Report the total number of experiments.
7. Answer the owner in **short, plain Indonesian**.

---

## 2. The data

### 2.1 What exists [V]
`C:\Users\horii\rhc-kerja\solana\data\pump_2026-11-06.jsonl` (57.6 MB, 60 min, collected 6 Nov 2026 from the public
Solana RPC websocket, decoded by our own scripts). One JSON object per line:

| Line type | Fields |
|---|---|
| trade | `t` (receive time), `sig`, `mint`, `sol` (SOL amount), `tok` (token amount), `buy` (true/false), `user` (**trader wallet**), `ts` (chain time), `vsol`, `vtok` (virtual curve reserves; price = `vsol / vtok`), `rsol` (real SOL in the curve) |
| create | `t`, `sig`, `create: true`, `name`, `symbol`, `mint`, `creator` |
| complete | `t`, `sig`, `complete: true` (a coin graduated from the bonding curve) |

Size: 152,145 trades, 1,546 new coins, 24 graduations in 1 hour.

### 2.2 How to collect more (do this first: one hour is not enough)
Scripts in `C:\Users\horii\rhc-kerja\solana\skrip\`:
```bash
python -I kumpul.py ../data/pump_<date>.jsonl <seconds>     # collect (public websocket logsSubscribe on pump.fun)
python -I analisa.py ../data/pump_<date>.jsonl 180 1800     # example analysis: decide at 3 min, outcome 30 min
```
Fix before a long collection:
- **Reconnect gaps:** the 60-min run had 8 read timeouts/closes, and the data in those gaps is lost. Add keep-alive
  pings and a backfill (`getSignaturesForAddress` back to the last seen signature, then `getTransaction`).
- **Graduated coins** move to PumpSwap and are no longer recorded, so their peaks are undercounted. Add a PumpSwap
  decoder, verified against real transactions.
- Optionally add Raydium, Meteora and Moonshot decoders. Verify every decoder against real transactions; never guess
  a byte layout.
- Target: **at least 3–7 days** of continuous data before any conclusion.

### 2.3 First look at the data (1 hour, starting point only) [V]
- Only **23%** of new coins are still trading at 3 minutes old.
- Of coins alive at 3 min: 6.1% peak at ≥ 2× within 30 min, 16.5% end below 0.5×.
- With a buy wave (≥ 20 unique buyers in the last 60 s, n=12): 16.7% reach 2×, but **66.7% end below 0.5×**.
- A wave raises both the up chance and the crash chance. Samples are tiny (8–27 coins per group) and from one hour.
  **Not a conclusion.**

---

## 3. Step A: understand the market first (descriptive, no strategy yet)
Answer from the data, per hour and per day:
1. How many coins are born, how many survive 1 / 3 / 5 / 10 / 30 min, and how many graduate?
2. What does a typical coin's life look like (price path, trades per minute, number of unique wallets over time)?
3. How do coins that reach ≥ 2× / ≥ 5× differ from coins that die, **at the moment a trader could still buy**?
4. What do creators do: when do they sell, how much, and how often does a creator sale come before a crash?
5. Wallets: how many buyers are new vs seen before, and how concentrated is the buying (top 1/3/10 wallets)?
   Do the same wallets show up early in many coins (bundlers, snipers, insiders)?
6. When during the day is the market active, and does that change the outcomes?

## 4. Step B: find the weaknesses in the owner's system
The owner's system works like this (chain-independent description). Test **each rule** on the Solana data and report
whether it helps, hurts or does nothing:

| # | System rule | Experiment on Solana data |
|---|---|---|
| R1 | Decide once at a fixed coin age (60 s) | Compare decision ages 30 s / 1 / 2 / 3 / 5 / 10 min: outcome per age |
| R2 | Buy the top 20% of the last 100 scores (relative gate, no absolute floor) | Is "top 20% of recent coins" still bad when the whole market is bad? Compare relative vs absolute thresholds |
| R3 | Require at least one seller in minute 1 ("honeypot rule") | Coins with early sellers vs without: crash and rug rates. Who are the early sellers (creator, early buyers)? |
| R4 | Block a creator only after one of their earlier coins rugged in **the bot's own** trades | How often do creators reuse wallets? Would a creator/funder history built from the data block more crashes? |
| R5 | Take profit: sell 60% at 1.2×, let the rest ride | Simulate exits on real price paths: 1.2×/60% vs 1.5×/40% vs 2×/50% vs 3×/33% vs no take-profit + trailing stop |
| R6 | Exit on a liquidity pull, a big single sell (≥ 5% of liquidity), a "peak signal", a danger-model score | On a bonding curve there is no liquidity to pull: which exit signals actually come **before** crashes here (creator sells, top-holder sells, buyer drought)? |
| R7 | The ML filter scores only minute-1 features | Do minute-1 features predict anything for coins bought at 3–10 min? |
| R8 | Quarter Kelly sizing, max share of liquidity per position | Given real curve depth, what position size moves the price too much (slippage at our size)? |

For every rule: per-day results, sample sizes, and a one-line verdict.

## 5. Step C: discover new features
### 5.1 The loop (one feature at a time)
1. **Hypothesis in one line, written before looking at outcomes.**
2. **Definition** computed at decision time t from data with time ≤ t only.
3. **Leakage test:** delete all data after t and recompute: the value must be identical, or the feature is rejected.
4. **Single-feature test:** split coins into quantiles of the feature. Per quantile: n, share reaching ≥ 2×, share
   ending < 0.5×, creator-dump rate, and mean return under a fixed simple exit plus costs (6.3). The direction must
   hold on **most days**, not only in the total.
5. **Added value:** add it to a model with time-ordered walk-forward folds (6.1). Keep it only if out-of-time AUC /
   PR-AUC **and** trading PnL improve versus the model without it.
6. **Attack it:** shuffle labels (the gain must disappear), drop the top 5 trades, double the costs.
7. **Log** it (kept or rejected).

### 5.2 Ideas to start from (all computable from the collected fields)
| Group | Candidate features |
|---|---|
| Buy wave quality | unique buyers per 30 s / 60 s / 5 min, growth of unique buyers, net SOL inflow, buy/sell count ratio, median vs largest buy |
| Concentration | share of buy volume from the top 1 / 3 / 10 wallets, holder concentration at time t |
| Creator | creator's token share, creator sold yet (when, how much), creator's earlier coins and how they ended (only earlier ones) |
| Insiders / bundles | buys in the creation slot, wallets that buy and sell within minutes, the same wallets appearing early in many coins, wallets active in earlier crashed coins |
| Wallet quality | share of buyers seen before, buyers' past record on earlier coins (past only), first-seen time of each wallet |
| Price path | return since birth, drawdown from the local high, 60 s volatility, curve progress (`rsol`), market cap (`vsol`) |
| Market heat | coins born in the last 10 min, total market trades per minute, hour of day (UTC) |

Not computable yet (needs extra RPC calls, so add to the collector first): mint/freeze authority status, Token-2022
extensions, wallet funding source and wallet age on chain.

## 6. Step D: test everything with real data
1. **Time-ordered splits by day** (walk-forward). Never random splits.
2. **Holdout:** the newest ≥ 1 day (more once data allows), never touched during the search, opened **once** at the
   end for the final short list.
3. **Costs in every result:** Solana base fee, priority fee, pump.fun trade fee (read the real value from on-chain
   config: currently **unknown**), curve slippage at our size computed from `vsol`/`vtok`, a failed-transaction rate,
   and a haircut for sandwich risk (no Jito). Never assume zero cost.
4. **Minimum sample:** no conclusion from fewer than 100 coins or 30 outcome events in a group; report "too few".
5. **Multiple testing:** report how many features and variants were tried. One lucky result among many is noise.
6. **Too good = suspicious:** win > 70%, AUC > 0.9 or a smooth equity curve means leakage until disproven.
7. Report trades, win %, average win / average loss, risk-reward, breakeven win %, profit factor, PnL, **PnL without
   the top 5 trades**, max drawdown, all per day.

## 7. Report to the owner (short, in Indonesian)
```
Data:            <files, dates, number of coins>
Weak points:     <rule> -> <what the data shows>   (one line each, R1-R8)
Features tried:  <N>   kept: <names>   rejected: <N>
Best result:     <per-day table, with costs>
Holdout:         <result, opened once>
Verdict:         edge / no edge / not enough data
Next:            <one step>
```
Never present a research result as ready for live trading.
