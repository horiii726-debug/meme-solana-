# C1 — Data access (Ankr capability probe)

Date: 2026-11-10 · Branch `rebuild` · Binary `sol2/probe` (Rust, read-only) · raw results `reports/rebuild/C1/*.jsonl`, `depth2.txt`.
Endpoint read from `/root/sol/.rpc.env` (`ANKR_HTTP`, `ANKR_WSS`); never printed. Tags: [V] measured · [I] inferred · [U] unknown.

## 1. Commands
```
cd /root/sol && set -a && . ./.rpc.env && set +a
./sol2/probe/target/release/probe basic   > reports/rebuild/C1/basic.jsonl
./sol2/probe/target/release/probe depth   > reports/rebuild/C1/depth.jsonl
./sol2/probe/target/release/probe wss     > reports/rebuild/C1/wss.jsonl
./sol2/probe/target/release/probe venues  > reports/rebuild/C1/venues.jsonl
./sol2/probe/target/release/probe rate    > reports/rebuild/C1/rate.jsonl
# + curl checks (getBlockTime, getFirstAvailableBlock, getTransaction on repo signatures of 2 Nov, gRPC trailers): depth2.txt
```

## 2. Capability table

| Capability | Result | Evidence |
|---|---|---|
| `getSlot`, `getVersion` | OK, ~100 ms; solana-core 4.4.1 | basic.jsonl [V] |
| **Transaction version 1 on mainnet** | blocks contain `legacy`, `0` **and `1`** tx (e.g. 120 / 180 / 194 of 494). `maxSupportedTransactionVersion: 0` fails with `-32015`. **Must use 1.** v1 message has `accountKeys, header, instructions, recentBlockhash, transactionConfig` | basic.jsonl [V] |
| `getBlock` full (json, v1) | OK, 115–240 ms, ~400–500 tx per block incl. failed tx (151–213 per block) | basic.jsonl [V] |
| `getBlock` order = execution order | **Yes**: token-balance chaining (pre of tx n = post of the previous tx touching that account) consistent in 5,186 / 5,186 transitions over 3 blocks, 0 broken | basic.jsonl `order` [V] |
| `getTransaction` (success, failed) | OK; meta has `err`, inner ix, logs, `loadedAddresses`, pre/post token balances, `fee`, `computeUnitsConsumed`; failed tx available (`Custom 6004`) | basic.jsonl [V] |
| `getSignaturesForAddress` + `before` paging | OK, 1,000 / page, 0 overlap, slots non-increasing; list includes failed tx (410 / 1,000 on PumpSwap) | basic.jsonl [V] |
| `getAccountInfo` jsonParsed (mint) | OK, decimals + authorities | basic.jsonl [V] |
| `getTokenLargestAccounts` | OK (20 accounts) | basic.jsonl [V] |
| WSS `logsSubscribe` (mentions PumpSwap) | OK: 12,148 notifications / 15 s (4,830 with `err`), 1 slot behind tip | wss.jsonl [V] |
| WSS `programSubscribe` (PumpSwap) | OK: 4,459 / 15 s, 22 slots behind tip | wss.jsonl [V] |
| **Yellowstone gRPC** | Server exists at `grpc.ankr.com:443`. With our key: `grpc-status 7 PERMISSION_DENIED "API key is not allowed to access blockchain"`; no key: `16 "API Key required"`; wrong key: `16 "API key not found"`. → **not included in the current plan** | curl trailers [V] |
| **History depth** | **Not an archive node.** `getFirstAvailableBlock` = 467,193,971 = **~13.4 h** before tip. `getBlock` older → `-32001 cleaned up`. `getTransaction` on repo signatures from 2 Nov → `null`. 1 / 7 / 30 / 90 days: **not available** | depth.jsonl, depth2.txt [V] |
| Slot rate | 100,000 slots in 21,464 s = **0.215 s / slot (4.66 slots/s)** | depth2.txt [V] |
| Rate limit | `getSlot` up to **400 rps: 0 errors**, p50 105 ms. Limit is above 400 rps (not reached) | rate.jsonl [V] |
| Credits per method | Ankr docs list Solana "all methods 500 credits" ($0.00005 / request; $0.10 = 1M) [I, docs]; **dashboard check pending** — probe made **~3,555 HTTP calls** + 16,607 WSS notifications | call_count lines [V] / [U] |

## 3. Market load per venue (last 300 s, `getSignaturesForAddress` on the program) [V]

| Venue | tx / s | est. tx / day | failed share | Active pools: tx in last 6 h (sample) |
|---|---|---|---|---|
| PumpSwap | 873 | 75.4 M | 39 % | n = 10, p50 10,566, 3 pools ≥ 20,000 (cap) |
| Raydium CPMM | 45 | 3.9 M | 30 % | n = 2: 71 and ≥ 20,000 |
| Meteora DAMM v2 | 90 | 7.8 M | 20 % | n = 9, p50 700, p90 2,449 |

Pool creations in a 25-tx sample per venue: 0 (creations are rare vs swaps; count per day = C4) [V/U].

## 4. Credit-budget estimate (at the documented 500 credits / request — to be confirmed by the dashboard) [I]

| Method of collection | Requests | Cost |
|---|---|---|
| Every PumpSwap swap via `getTransaction` | 75.4 M / day | ≈ $3,770 / day — impossible |
| One PumpSwap pool, first 6 h, every tx via `getTransaction` | p50 ≈ 10,600 | ≈ $0.53 / pool |
| One DAMM v2 pool, first 6 h | p50 ≈ 700 | ≈ $0.035 / pool |
| **Whole chain via `getBlock`** (all venues, failed tx, block order) | 402,600 blocks / day | ≈ **$20 / day** |
| WSS `logsSubscribe` | per notification billing [U] | PumpSwap events are in logs → swaps without `getTransaction`; DAMM v2 events are `emit_cpi` (not in logs) |

With $10 (100 M credits): ≈ 12 h of `getBlock` firehose, or ≈ 19 PumpSwap pools fully, or ≈ 280 DAMM v2 pools.

## 5. Gate
- Every capability row filled [V] or "not available", except **credits per method** → [U] until the owner reads the dashboard.
- **Gate C1: blocked** on (a) dashboard credit reading, (b) owner decision on data source (§6), because history beyond 13 h does not exist on this plan and the C9 target (≥ 14 days) can only be collected live.

## 6. Options (owner decision)
1. **Ask Ankr to enable Solana gRPC on this key and quote the price** (recommended first). gRPC streams every tx of the chosen programs with full meta and inner ix; it fixes D6, D7, D8 at once.
2. **`getBlock` firehose** (~$20 / day at 500 credits): complete and verified in-order data, but 14 days ≈ $280.
3. **Cheap hybrid**: WSS `logsSubscribe` for PumpSwap (+ CPMM if its events are in logs, C3 to verify) + `getTransaction` only for pool creations, DAMM v2 pools and a reconciliation sample. Cost depends on WSS billing [U].
All options need 14 days of live collection; past days cannot be bought back from this endpoint.
