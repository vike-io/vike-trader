---
name: get-market-data
description: Get market data into the local history store so a backtest has something to run over. Use when a user says they have no data, asks how to download or fetch bars, asks for BTCUSDT (or any symbol) history, says list_series came back empty, asks what a fresh install can run, or asks for demo or sample data to try the tools on (none ships; say so, and load real data). Covers `vike-cli data hist fetch VENUE:SYMBOL:INTERVAL` for REAL public bars (most venues need no credentials and no venue account; `vike-cli data source show VENUE` says what one needs), `vike-cli data hist fetch --source starter` for the PUBLISHED dataset where no venue can be reached, `vike-cli data hist export` for getting a slice back out through a datahub, `vike-cli data hist import` for a vendor archive the user synced onto the datahub's box (Dukascopy's tick files, Binance's aggTrades zips), choosing a window, where the store lives, and reading the result back with list_series before believing the fetch landed.
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
| `vike-cli backtest` | compute a strategy over history and judge what came back: `backtest run` runs one on a remote backtesthub — `vike-backend backtesthub --addr`, config.backtest_addr — or --local, and the PROFILE says what — a [paramscan] grid searches the parameter space, a [walkforward] table walks it forward, and declaring both ships the profile whole to the walk-forward runner. Then `backtest ls\|show\|path` over the run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn that into a CI exit code, and `params` and `strategies` for what can be tuned and what can be run. Before a strategy exists: `templates` ships starters, `script-api` prints everything a Rhai script may call, and `script-check` compiles one and answers in the exit code |
| `vike-cli data` | the hist store: fetch, import or load the published real bars into it (no demo or synthetic tape ships), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | this box's settings: provenance (`show`), a validating pre-flight (`check`), and a journalled one-key write (`set`) |
| `vike-cli trade` | observe + control a running vike-tradehub: every verb but three lives under a REQUIRED group (`account`, `order`, `position`, `strategy` built; `watch` named in the roster and refused as designed-not-built) — `trade status\|halt\|resume` are the three NODE-WIDE, risk-REDUCING words that take no book and so take no group, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub for a tearsheet over its live journal, or re-render a FINISHED run from this machine's own run directory (read-only; a node built from this tree serves the live verb, an older one refuses and names the command that does) |
| `vike-cli research` | investigate a signal and FIT a model — the plane BEFORE a strategy exists: `research study` asks the backend to run a compiled study over the hist store it holds (served by `vike-backend backtesthub --addr` when it mounts the study runner; a peer that does not refuses by name and points at `vike-backend study`) |
| `vike-cli secrets` | inspect the credential store THIS box reads — the settings database `<project>`/settings/db/vike.db, the only store, which `secrets path` reports — set ONE key in it, and create it EMPTY (list \| path \| set \| init) |
| `vike-cli backend` | stand a vike-tradehub — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the hub key pair a vike-datahub server authenticates with (setup), or hand its CONTROL key to a pipe for `just studio` (control-key), on that server's box |
| `vike-cli gatewayhub` | MINT the hub key pair a vike-gatewayhub server authenticates with (setup), on that server's box |
| `vike-cli init` | create `<project>`/user_data — starter strategies, presets, notebooks; no run profile and no data ship — and print how to load real bars and run a strategy file |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |
| `vike-cli surface` | write this binary's own command surface as JSON, for the documentation the docs site generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled in, so the answer is a property of THIS build |

## Step 1 — find out what is already there

Call `list_series` (no arguments). It returns one row per stored series —
`{kind, venue, symbol, interval, first_timestamp, last_timestamp, rows}` — and answers the question before
anybody downloads anything twice.

- An EMPTY `series` list means the store is empty. That is the case this skill is for.
- `cannot connect to datahub at …` means there is no server to ask, which is a different problem
  (start one, or point `--addr` at it). The store may well be full.

## Step 2 — pick which of the two you want

**Real public bars.** Most venues need nothing from you — no credentials and no venue account,
because this is public market data — and a venue that needs something is marked:
`vike-cli data source show VENUE` says what it needs. OANDA's lane is credentialed: the daemon that
makes the datahub's venue calls, `vike-gatewayhub`, reads one practice-tier token from its own box when
the request arrives, the CLI and the datahub never hold it, and a fetch for OANDA is refused until that
token is stored there. **Dukascopy needs a Dukascopy account, even for history alone; a free demo account
is enough.** Its tick files are public, but vike reads each instrument's price scale from the instrument list
the account's own login states, and no list is built into the program. Until a Dukascopy account has logged
in, every Dukascopy fetch and import is refused with one line that starts `no Dukascopy instrument list yet`.
The steps, as that line gives them: open a free Dukascopy demo account, add it with
`vike-cli secrets account add --venue dukascopy --tier demo --no-label`, file its login with
`vike-cli secrets set --account <ID> LOGIN` and `PASSWORD` (each value on stdin, never on the command line),
and start it; the list arrives at its first login. (`vike-cli data source show dukascopy` calls the hour-file
feed keyless, and the FEED is; vike's lane over it is not.) ⚠ It does need a **reachable datahub, with
`vike-gatewayhub` running beside it**: this verb asks a server to fetch the window into the store, and that server asks the gateway to
call the exchange, rather than calling it itself. Default `127.0.0.1:7878`, else `--addr HOST:PORT`.
With no server, use `vike-cli data hist fetch --source starter` (below), which needs nothing but HTTPS.

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

A fetch error that names `gatewayhub <address>` — then `cannot connect`, or
`no … hub key is configured for this link` — means the datahub answered and the daemon that calls
the venue for it did not: it is not running on the datahub's box, or that box holds no key pair for
it. The store still reads; a
venue fetch waits for the operator to start `vike-gatewayhub`, or to mint its keys on that box with
`vike-cli gatewayhub setup`. Retrying the fetch fixes neither.

**Stopping a fetch.** Interrupting the command (Ctrl-C, a closed tunnel) stops the datahub's side
too, at its next chunk boundary: the chunk in flight finishes and is stored, nothing after it is
fetched. A fetch nobody can interrupt — left running in another session or on another machine —
is stopped from any shell that reaches the datahub:

```sh
vike-cli data hist running
vike-cli data hist cancel oanda:EUR_USD:5s
```

`running` lists every fetch the datahub is serving, each with the spec it was asked for; `cancel`
takes that spec and stops every running fetch on it at its next chunk boundary. It removes nothing
and asks no confirmation: the chunks already stored stay, and running the same fetch again resumes.
It does not wait — the stopped fetch's own command exits with an error naming the cancel. A
ONE-BATCH lane (a keyless kline venue, a funding series) has no boundary to stop at: the cancel
names it and it runs to its end. Against a datahub older than these two verbs both are refused by
name with nothing sent, and such a datahub keeps fetching after the command is interrupted:
restarting it is the only stop there.

**There is no demo or synthetic data.** Vike ships none and generates none: a fresh store holds
NOTHING until real bars are loaded, and no shipped profile names a slice. If a user asks for a
"demo tape" or sample data, say exactly that, and load real data with one of the routes here.
(`vike-cli data hist fetch --source demo` is refused by name and says the same.)

**The PUBLISHED starter dataset** — real bars over plain HTTPS, for a box no venue can be reached
from (a geoblock, a locked-down network):

```sh
vike-cli data hist fetch --source starter
```

It takes no spec and no window: the dataset is a fixed published span, and each file is verified
against the release's `SHA256SUMS` before it is loaded. Safe to re-run.

**A vendor ARCHIVE** — today Dukascopy's daily tick files and Binance's daily aggTrades zips
(below). The DATAHUB downloads Dukascopy's itself, from the vendor's Requester-Pays S3 bucket, with
the user's OWN AWS key stored on the datahub's box and billed to that key's account. Store the key
once, on that box, each value on stdin and never on the command line:
`vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_ACCESS_KEY_ID` and
`vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_SECRET_ACCESS_KEY` (its IAM user needs `s3:ListBucket`
and `s3:GetObject` on the bucket). The import needs the Dukascopy account above too:
the datahub reads each dataset's price scale from the instrument list it keeps from that account's login, and
refuses every dataset with the same `no Dukascopy instrument list yet` line until it has one. The files land in,
and are read from, the DATAHUB's
`<project>/market_data/imports/dukascopy-bi5/SYMBOL/` folder:

```sh
vike-cli data hist import dukascopy-bi5 EURUSD --from 2024-01-01 --to 2024-01-31 --fetch --dry-run
vike-cli data hist import dukascopy-bi5 EURUSD --from 2024-01-01 --to 2024-01-31 --fetch --yes
vike-cli data hist import dukascopy-bi5 EURUSD --dry-run
vike-cli data hist import dukascopy-bi5 EURUSD --dry-run --verify
```

`--fetch` needs `--from` and `--to`, and its plan comes first: what the bucket holds in the window,
what the folder already has, and what the download would fetch, with its ESTIMATED AWS cost. Beside
`--dry-run` nothing is downloaded. Show the user that plan and its cost before anything is
downloaded: the download asks for `fetch N files` to be typed (`--yes` replaces that), fetches one
calendar month per request, and then the import runs. A file already in the folder is never fetched
again, so the same command resumes. Without `--fetch` the import reads what the folder holds.

Run `--dry-run` FIRST and show the user the plan: it names the folder on the datahub's box, which
days are already held, which are refused and why, and how many it would import. `--verify` also
decodes every importable file and writes nothing. The import itself asks for `import N days` to be
typed at a terminal; `--yes` replaces that, so pass it only once the user has seen the plan and
agreed. `--from`/`--to` are inclusive UTC days (default the whole dataset), and the bars derived
per imported day default to `1m` (`--bars`). It sends one request per calendar month, and running
the same command again resumes. If the folder is missing, the plan names the folder's path on the
datahub's box and the two ways to fill it. A datahub up to v0.1.41 has no import lane and refuses
the verb by name; one without the archive download refuses `--fetch` by name.

**Binance's public aggTrades archive** is the second format, `binance-aggtrades`: the DAILY zips
under `data/futures/um/daily/aggTrades/SYMBOL/` on data.binance.vision, public, so no account and
no key. They go flat into `<project>/market_data/imports/binance-aggtrades/SOLUSDT/` on the
datahub's box; the dataset is Binance's own symbol (`SOLUSDT`), and the trades land as venue
`binance`, symbol `SOLUSDT.P`. A monthly zip is never read. The same flow, the same flags:

```sh
vike-cli data hist import binance-aggtrades SOLUSDT --dry-run --verify
vike-cli data hist import binance-aggtrades SOLUSDT --from 2026-10-01 --to 2026-10-31 --yes
```

What differs, and what to tell the user before they run it:

- **No bars unless `--bars` names a step, and never among the candles.** They land in the
  `source=aggtrades` lane: an aggregated trade carries its first fill's time, so these bars differ
  from Binance's klines, and a chart or a backtest keeps reading the klines `data hist fetch`
  stores. `data hist get` and `data hist export` reach the lane with `--source aggtrades`.
- **A day the live recorder already wrote is `overlapped`**: never imported and never completed,
  however little the recorder caught. Import a symbol's history BEFORE recording it.
- **`--verify` lists ids that do not run on under `reported`.** Those days are verified, and an
  import stores them as published; the verify still exits 0.
- A datahub that predates this format answers that it registers no archive format by that name,
  and names the ones it does.

**Getting a slice back OUT** as a standalone Parquet file:

```sh
vike-cli data hist export binance:BTCUSDT:1h --out btc-1h.parquet
vike-cli data hist export binance:BTCUSDT:1h --out btc-1h.parquet --from 2024-01-01T00
```

⚠ An export is a READ, and every history read goes through a **datahub** — decision 0084. The bars
come from the datahub this box is configured for (the `config.datahub_addr` row in the settings;
loopback by default) and only the Parquet encoding happens locally, so
`--store` is REFUSED on `export` by name. For files that are only on this machine, start a key-less
datahub on them first and export from that: `vike-backend datahub --store DIR` — it
binds loopback only and needs no keys.

⚠ `--from` and `--to` are INDEPENDENT here and neither is required — an export bounds a slice that
is already in the store, so all four combinations are meaningful — where `fetch` requires a window
and `--from` needs a matching `--to`. `--days` is refused on `export` by name: it counts back from
NOW, which says nothing about what a store holds.

⚠ The bounds are SPELT differently too. This export (no `--addr`) hands them to the engine, which
reads epoch milliseconds or a UTC hour `YYYY-MM-DDTHH` (`2024-01-01T00`, as above) and refuses a
bare date — the opposite of `fetch`, which takes a bare date and refuses an hour.

The local WRITER (`fetch --source starter`) writes into the store root, which
`--store DIR` names outright when the user wants a particular one; a venue `fetch` asks the datahub,
which writes into the store IT serves. `export` reads, so it takes no `--store` at all — and neither
do the read verbs (`ls`, `get`, `coverage`, …), which refuse it with the same sentence.

## Step 3 — read the result back

Call `list_series` again. A fetch that worked shows up as a row with a `first_timestamp`/`last_timestamp` span
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

The `data hist` verbs that write a store on this machine (`fetch --source starter`, a local
`rm`, `repair`) and `export`'s Parquet route drive a standalone `backtesthub` engine rather than doing
the work in process. On Linux a release attaches that engine beside `vike-cli`, so it is simply
there. On
**Windows there is no published engine binary**, and `data` and `backtest run --local`
are unavailable until the user supplies one — `--engine PATH` names it outright. The failure
message says this in those words; if a user reports it, the answer is the engine, not the command
line.

## Reporting back

Say which route you had them run. Then name the series `list_series` now reports — venue, symbol, interval
and the covered span — because that is what a profile's `[data]` table has to match, and it is the
next thing they will need.
