---
name: get-market-data
description: Get market data into the local history store so a backtest has something to run over. Use when a user says they have no data, asks how to download or fetch bars, asks for BTCUSDT (or any symbol) history, says list_series came back empty, asks what a fresh install can run, or asks for a demo tape to try the tools on. Covers `vike-cli data hist fetch VENUE:SYMBOL:INTERVAL` for REAL public bars (most venues need no credentials and no venue account; `vike-cli data source show VENUE` says what one needs), `vike-cli data hist fetch --source demo` for the SYNTHETIC demo tape, `vike-cli data hist fetch --source starter` for the PUBLISHED dataset where no venue can be reached, `vike-cli data hist export` for getting a slice back out through a datahub, choosing a window, where the store lives, and reading the result back with list_series before believing the fetch landed.
metadata:
  tools: "list_series"
  source: "ai/trader/backtesting ai/trader/tools/list_series trader/tutorials/rsi-mean-reversion-backtest"
---

# Get market data

A backtest reads a **history store** — a Parquet tree the `vike-datahub` server opens and serves.
The backtest engine, like every other history reader, reads it THROUGH that server rather than
opening the files itself (decision 0084). A fresh install has an empty one, and an empty store is
the single commonest reason a backtest cannot be run at all.

Filling it is a CLI job, not a tool call. There is no MCP tool that fetches data, deliberately:
fetching writes the store and needs the whole DataFusion/Parquet stack, which the light CLI that
serves these tools does not link. What you CAN do from here is read the result back.

| verb | what it does |
| --- | --- |
| `vike-cli backtest` | compute a strategy over history and judge what came back: `backtest run` runs one on a remote COMPUTE daemon — `vike-backend backtest --addr`, config.backtest_addr — or --local, and the PROFILE says what — a [paramscan] grid searches the parameter space, a [walkforward] table walks it forward, and declaring both ships the profile whole to the walk-forward runner. Then `backtest ls\|show\|path` over the run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn that into a CI exit code, and `params` and `strategies` for what can be tuned and what can be run. Before a strategy exists: `templates` ships starters, `script-api` prints everything a Rhai script may call, and `script-check` compiles one and answers in the exit code |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | this box's settings: provenance (`show`), a validating pre-flight (`check`), and a journalled one-key write (`set`) |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | observe + control a running vike-tradehub node: every verb but three lives under a REQUIRED group (`order`, `position`, `strategy` built; `account`, `watch` named in the roster and refused as designed-not-built) — `trade status\|halt\|resume` are the three NODE-WIDE, risk-REDUCING words that take no book and so take no group, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub node for a tearsheet over its live journal, or re-render a FINISHED run from this machine's own run directory (read-only; a node built from this tree serves the live verb, an older one refuses and names the command that does) |
| `vike-cli research` | investigate a signal and FIT a model — the plane BEFORE a strategy exists: `research study` asks the backend to run a compiled study over the hist store it holds (served by `vike-backend backtest --addr` when it mounts the study runner; a peer that does not refuses by name and points at `vike-backend study`) |
| `vike-cli secrets` | inspect the credential store THIS box reads — `<project>`/settings/secrets.env, or the settings database `<project>`/settings/db/vike.db once migrated, which `secrets path` reports — set ONE key in it, and perform that migration (list \| path \| template \| set \| migrate) |
| `vike-cli backend` | stand a vike-tradehub node — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the node key pair a vike-datahub server authenticates with (setup), or hand its CONTROL key to a pipe for `just studio` (control-key), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |
| `vike-cli surface` | write this binary's own command surface as JSON, for the documentation the docs site generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled in, so the answer is a property of THIS build |

## Step 1 — find out what is already there

Call `list_series` (no arguments). It returns one row per stored series —
`{kind, venue, symbol, interval, first_ts, last_ts, rows}` — and answers the question before
anybody downloads anything twice.

- An EMPTY `series` list means the store is empty. That is the case this skill is for.
- `cannot connect to datahub at …` means there is no server to ask, which is a different problem
  (start one, or point `--addr` at it). The store may well be full.

## Step 2 — pick which of the two you want

**Real public bars.** Most venues need nothing from you — no credentials and no venue account,
because this is public market data — and a venue that needs something is marked:
`vike-cli data source show VENUE` says what it needs. OANDA's lane is credentialed: the DATAHUB reads
one practice-tier token from its own box when the request arrives, the CLI never holds it, and a
fetch for OANDA is refused until that token is stored there. ⚠ It does need a **reachable datahub**:
this verb asks a server to fetch the window into the store rather than calling the exchange itself.
Default `127.0.0.1:7878`, else `--addr HOST:PORT`. With no server, use
`vike-cli data hist fetch --source starter` (below), which needs nothing but HTTPS.

```sh
vike-cli data hist fetch binance:BTCUSDT:1h --days 180
vike-cli data hist fetch okx:BTC-USDT:1h --from 2024-01-01 --to 2024-06-01 --addr <host>:7878
```

The spec is `VENUE:SYMBOL:INTERVAL`, three non-empty parts. A window is REQUIRED and there is no
default — "fetch everything" is not a thing any venue serves, and a silent default would decide
how much of somebody's rate limit to spend. Use `--days N` counting back from now, OR `--from` and
`--to` together (epoch milliseconds, or a UTC date `YYYY-MM-DD`, which means that day's midnight
UTC — an hour label such as `2024-01-01T00` is refused), never a mixture.

A fetch is synchronous: one request, and the command prints its answer when the datahub has
written the window. A window LONGER than a year at a venue whose datahub lane stores whole UTC days
(the lane `vike-cli data source show VENUE` calls `CredentialedKlines` — OANDA's) is sent as one
request per calendar year instead, with a line on stderr as each year finishes. If one year fails
the run stops there, the years before it stay stored, and running the same command again resumes
— days already stored are skipped. Every other venue's window is one request, however long.

**The synthetic demo tape.** A closed-form curve, written under its own venue id `demo`:

```sh
vike-cli data hist fetch --source demo
```

⚠ **It is NOT market data**, and you must say so whenever you suggest it. It exists so the tools
have something to draw and run against, and it is written under a venue id that cannot be mistaken
for a real one. Never report a backtest result on the `demo` venue as evidence about a strategy.
It is safe to re-run, and it is the slice the shipped example backtest profile names, so a fresh
install can run that profile immediately.

**The PUBLISHED starter dataset** — real bars over plain HTTPS, for a box no venue can be reached
from (a geoblock, a locked-down network):

```sh
vike-cli data hist fetch --source starter
```

It takes no spec and no window: the dataset is a fixed published span, and each file is verified
against the release's `SHA256SUMS` before it is loaded. Safe to re-run.

**Getting a slice back OUT** as a standalone Parquet file:

```sh
vike-cli data hist export binance:BTCUSDT:1h --out btc-1h.parquet
vike-cli data hist export binance:BTCUSDT:1h --out btc-1h.parquet --from 2024-01-01T00
```

⚠ An export is a READ, and every history read goes through a **datahub** — decision 0084. The bars
come from the datahub this box is configured for (`config.datahub_addr` in the settings, or
`VIKE_DATAHUB_ADDR` above it; loopback by default) and only the Parquet encoding happens locally, so
`--store` is REFUSED on `export` by name. For files that are only on this machine, start a key-less
datahub on them first and export from that: `VIKE_DATAHUB_STORE=DIR vike-backend datahub` — it
binds loopback only and needs no keys.

⚠ `--from` and `--to` are INDEPENDENT here and neither is required — an export bounds a slice that
is already in the store, so all four combinations are meaningful — where `fetch` requires a window
and `--from` needs a matching `--to`. `--days` is refused on `export` by name: it counts back from
NOW, which says nothing about what a store holds.

⚠ The bounds are SPELT differently too. This export (no `--addr`) hands them to the engine, which
reads epoch milliseconds or a UTC hour `YYYY-MM-DDTHH` (`2024-01-01T00`, as above) and refuses a
bare date — the opposite of `fetch`, which takes a bare date and refuses an hour.

The WRITERS (`fetch --source demo`, `fetch --source starter`) write into the store root, which
`--store DIR` names outright when the user wants a particular one; a venue `fetch` asks the datahub,
which writes into the store IT serves. `export` reads, so it takes no `--store` at all — and neither
do the read verbs (`ls`, `get`, `coverage`, …), which refuse it with the same sentence.

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

The `data hist` verbs that write a store on this machine (`fetch --source demo|starter`, a local
`rm`, `repair`) and `export`'s Parquet route drive a standalone `backtest` engine rather than doing
the work in process. On Linux a release attaches that engine beside `vike-cli`, so it is simply
there. On
**Windows there is no published engine binary**, and `data` and `backtest --local`
are unavailable until the user supplies one — `--engine PATH` names it outright. The failure
message says this in those words; if a user reports it, the answer is the engine, not the command
line.

## Reporting back

Say which of the two you had them run, and if it was `fetch --source demo`, say in the same
sentence that the tape is synthetic. Then name the series `list_series` now reports — venue, symbol, interval
and the covered span — because that is what a profile's `[data]` table has to match, and it is the
next thing they will need.
