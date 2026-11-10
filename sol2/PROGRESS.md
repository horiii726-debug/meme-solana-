# sol2 — component progress

Plan: `REBUILD_PLAN.md`. One component at a time; next starts only after the gate passes **and** the owner says "lanjut".

| Component | Status | Gate numbers | Report | Open problems |
|---|---|---|---|---|
| C0 Audit of existing components | passed | all source files covered; 0 verdicts without evidence | `reports/rebuild/C0/REPORT.md` | PumpSwap reserves pre/post [U]; Pump.fun curve program has no IDL in repo [U] |
| C1 Data access (Ankr probe) | blocked | history 13.4 h only; gRPC denied (status 7); 0 rate-limit errors at 400 rps; getBlock order 5186/5186 | `reports/rebuild/C1/REPORT.md` | credits/method (dashboard); owner choice of data source |
| C2–C20 | not started | — | — | — |

## Environment (2026-11-10)
- VPS vast.ai instance 59926231, repo `/root/sol` (branch `rebuild`), spec worktree `/root/sol_spec` (read-only reference).
- Rust 1.99 (rustup), gcc, libssl-dev, jq present. No CatBoost binary, no old `data/`, no `.rpc.env`.
- Claude Code skills on the VPS: `graphify` (CLI via uv), `karpathy-guidelines`.
