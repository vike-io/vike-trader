---
name: get-market-data
description: Get market data into the local history store so a backtest has something to run over. Use when a user says they have no data, asks how to download or fetch bars, asks for BTCUSDT (or any symbol) history, says list_series came back empty, asks what a fresh install can run, or asks for a demo tape to try the tools on. Covers `vike-cli data fetch VENUE:SYMBOL:INTERVAL` for REAL public bars (no credentials, no venue account), `vike-cli data seed-demo` for the SYNTHETIC demo tape, choosing a window, where the store lives, and reading the result back with list_series before believing the fetch landed.
metadata:
  tools: "list_series"
  source: "ai/trader/backtesting ai/trader/tools/list_series trader/tutorials/rsi-mean-reversion-backtest"
---

# Get market data

A backtest reads a **history store** — a Parquet tree the `vike-datahub` server serves and the
backtest engine reads directly. A fresh install has an empty one, and an empty store is the single
commonest reason a backtest cannot be run at all.

Filling it is a CLI job, not a tool call. There is no MCP tool that fetches data, deliberately:
fetching writes the store and needs the whole DataFusion/Parquet stack, which the light CLI that
serves these tools does not link. What you CAN do from here is read the result back.

| verb | what it does |
| --- | --- |
| `vike-cli backtest` | run a backtest on a remote vike-datahub server and print the report |
| `vike-cli sweep` | run a parameter-grid search on a remote vike-datahub server and print the ranked grid |
| `vike-cli walkforward` | run an anchored walk-forward validation on a remote vike-datahub server |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | settings provenance (`show`) and a validating pre-flight (`check`) for this box |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | interactive REPL to observe + control a running vike-tradehub node (for a human) |
| `vike-cli strategy-status` | ask a running vike-tradehub node what it is running (read-only; --json for machines) |
| `vike-cli secrets` | inspect the credential store `<project>`/settings/secrets.env, and set ONE key in it (list \| path \| template \| set) |
| `vike-cli backend` | stand a vike-tradehub node — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the node key pair a vike-datahub server authenticates with (setup), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |

## Step 1 — find out what is already there

Call `list_series` (no arguments). It returns one row per stored series —
`{kind, venue, symbol, interval, first_ts, last_ts, rows}` — and answers the question before
anybody downloads anything twice.

- An EMPTY `series` list means the store is empty. That is the case this skill is for.
- `cannot connect to datahub at …` means there is no server to ask, which is a different problem
  (start one, or point `--addr` at it). The store may well be full.

## Step 2 — pick which of the two you want

**Real public bars.** No credentials and no venue account — this is public market data:

```sh
vike-cli data fetch binance:BTCUSDT:1h --days 180
vike-cli data fetch okx:BTC-USDT:1h --from 2024-01-01T00 --to 2024-06-01T00
```

The spec is `VENUE:SYMBOL:INTERVAL`, three non-empty parts. A window is REQUIRED and there is no
default — "fetch everything" is not a thing any venue serves, and a silent default would decide
how much of somebody's rate limit to spend. Use `--days N` counting back from now, OR `--from` and
`--to` together (epoch milliseconds, or `YYYY-MM-DDTHH`), never a mixture.

**The synthetic demo tape.** A closed-form curve, written under its own venue id `demo`:

```sh
vike-cli data seed-demo
```

⚠ **It is NOT market data**, and you must say so whenever you suggest it. It exists so the tools
have something to draw and run against, and it is written under a venue id that cannot be mistaken
for a real one. Never report a backtest result on the `demo` venue as evidence about a strategy.
It is safe to re-run, and it is the slice the shipped example backtest profile names, so a fresh
install can run that profile immediately.

Both write into the store root, which `--store DIR` names outright when the user wants a
particular one.

## Step 3 — read the result back

Call `list_series` again. A fetch that worked shows up as a row with a `first_ts`/`last_ts` span
covering the window that was asked for. Two things to check rather than assume:

- The row's `interval` is the one that was fetched (present for bar series, `null` for tick
  series). A profile naming a different interval will not resolve.
- The span actually covers the range a profile will ask for. A venue serving less history than the
  window requested is normal and silent; pick the profile's `from`/`to` INSIDE the reported span
  instead of guessing.

A blank `symbol` in a row means a GROUPED series (many symbols in one part file), not "no symbol".

## What is validated where

The command line's SHAPE is checked locally — the three-part spec, and exactly one of the two
window forms — so a typo is a usage error rather than a diagnostic from a process the user never
named. The VENUE and the INTERVAL are not: which venues a build can reach is a property of the
engine that does the work, and its own error names what it can do. So an unknown venue is a clean
failure from the fetch, not a refusal at the door.

## ⚠ The engine has to exist, and on Windows it may not

Every `data` subcommand drives a standalone `backtest` engine rather than doing the work in
process. On Linux a release attaches that engine beside `vike-cli`, so it is simply there. On
**Windows there is no published engine binary**, and `data`, `backtest --local` and `sweep --local`
are unavailable until the user supplies one — `--engine PATH` names it outright. The failure
message says this in those words; if a user reports it, the answer is the engine, not the command
line.

## Reporting back

Say which of the two you had them run, and if it was `seed-demo`, say in the same sentence that
the tape is synthetic. Then name the series `list_series` now reports — venue, symbol, interval
and the covered span — because that is what a profile's `[data]` table has to match, and it is the
next thing they will need.
