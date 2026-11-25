# vike

A Rust trading platform: a live execution core, a backtest and research harness, a desktop
charting shell, and bridges to fourteen venues.

This repository is a **source mirror**. It carries the code and nothing else — no CI workflows, no
operator runbooks, no history. Each publish is one commit. The development repository is private;
what you see here is what the released binaries are built from.

## What is in the box

| | |
|---|---|
| **Live daemon** (`vike-tradehub`) | headless execution: runs a strategy, reconciles against the venue, and stops cleanly on SIGTERM with the resting book handled |
| **Gateway** (`vike-gatewayhub`) | the one daemon that opens venue connections: every other process reaches a venue through it |
| **Backtest + research** (`backtesthub`, `vike-study`, `report`) | a bar/tick harness with parameter sweeps, successive-halving refinement and a TPE optimizer; reports persist as a run manifest |
| **Desktop shell** (`vike-desktop`) | charts, order entry, a data manager and a connections editor, on egui/wgpu — a client of the daemons: it mounts no venue itself |
| **Data** (`vike-datahub`) | a Parquet/DataFusion history store, a live recorder, and a query server, in one daemon |
| **Venues** | binance, bybit, okx, deribit, hyperliquid, aster, alpaca, ctrader, ig, oanda, polymarket, dukascopy, fxcm, ibkr — each a crate under `crates/bridges/` |
| **Agent skills** (`skills/`) | procedures for an agent driving `vike-mcp` — backtesting, authoring a strategy, getting data in, and trading on a node through the preview gate — one `SKILL.md` each, indexed in `skills/README.md` |

## Running it

Container images carry every headless tool as one binary with a symlink per tool:

```sh
docker pull vikeio/vike-tradehub:latest      # Docker Hub
docker pull ghcr.io/vike-io/vike-tradehub    # GHCR
```

⚠ An image is built on request, not with every release, so `latest` can be many versions behind the
newest release (on 2026-11-07: image 0.1.51, release v0.1.101) and a command on this page may not
exist in it. Read its version first: `docker run --rm --entrypoint vike-cli vikeio/vike-tradehub:latest --version`.

The command-line tool and the MCP server install from the release without a toolchain on Linux; any
other target compiles from the clone (`crates/vike-cli/Cargo.toml`'s `[package.metadata.binstall]` says
why):

```sh
cargo binstall --git https://github.com/vike-io/vike-trader vike-cli
cargo binstall --git https://github.com/vike-io/vike-trader vike-mcp
```

⚠ On Windows no release carries a binary any more (since 2026-11-24): the Windows CLI, `vike-cli.exe`,
is optional and built on request together with the desktop app, and those builds are not published
here. `cargo binstall` therefore compiles it from the clone, which needs a Rust toolchain. MCP on
Windows is `vike-desktop.exe --mcp`, landing with the desktop's own change; `vike-mcp.exe` is retired.
Releases published before that day still carry `vike-cli.exe` (and, after v0.1.101, `vike-mcp.exe`),
listed in their `SHA256SUMS`.

The server half is one more asset of the release, `vike-backend`, for Linux x86-64 only. Everything
below that says `vike-backend`, `--local` or `--source` needs it. Put it beside `vike-cli` or in
`<project>/bin`, the two places `vike-cli` looks for it:

```sh
curl -L -o vike-backend https://github.com/vike-io/vike-trader/releases/latest/download/vike-backend
chmod +x vike-backend
```

⚠ Neither path updates itself. `cargo binstall` records the install under the manifest version, so a
re-install needs `--force`; a downloaded executable is replaced by downloading it again.

⚠ On Windows neither the standalone backtest ENGINE nor the store tool is published, so `data history fetch --source starter`
and `backtest run --local` are unavailable until you build them. The remote path needs neither:
`vike-cli backtest run --addr HOST:PORT` runs on a backtesthub server (`vike-backend backtesthub --addr`) instead.

From source:

```sh
cargo build --workspace                      # no native dependencies on default features
cargo run -p vike-desktop                    # the desktop shell (charts stay empty with no backend)
```

For an agent (Claude Code, Cursor, any MCP client), the skills install straight from this repo:

```sh
npx skills add vike-io/vike-trader           # every procedure under skills/, one SKILL.md each
```

## A fresh install has no market data — load real bars

No demo, sample or synthetic data ships: the store is EMPTY until you load real bars, and no run
profile ships either (a profile is your own run recipe).

```sh
mkdir -p my-project/settings && cd my-project   # a settings/ directory is what makes this a project
vike-cli init                              # starter strategies, presets and notebooks
vike-cli secrets init                      # the settings database, empty
vike-cli backend mint-missing              # this box's device key on the project's access list, and the hub keys
vike-backend datahub &                     # serves this project's store: loopback only
vike-backend gatewayhub &                  # the one daemon that reaches a venue: loopback only
vike-cli data catalog refresh --venue VENUE  # the venue's instrument list, a public read
vike-cli data history fetch --source starter  # the published dataset (real bars), over plain HTTPS
vike-cli backtest run --local --strategy trend/sma_cross.rhai \
                  --venue VENUE --symbol SYMBOL --interval INTERVAL \
                  --from FROM --to TO --cash CASH
```

`VENUE`, `SYMBOL` and `INTERVAL` name a series you loaded: `vike-cli data history ls` lists what the
store holds. `FROM` and `TO` are UTC hours (`YYYY-MM-DDTHH`, or epoch-ms) inside that series, and a
run has no default window. `CASH` is the starting balance; leave `--cash` out and a run starts with
10000. A clone of this repository is already a project, so skip the first line there. The
datahub line is not optional: every history READ goes through one, a `--local` run included —
`--local` moves the backtest ENGINE onto this machine, never the history. Started with no
environment it serves the same `<project>/market_data/hist` the loading command wrote into and binds
loopback only, which is where `--local` looks by default. It ALWAYS authenticates: it admits only a
device on this project's access list, and an empty list refuses every read as "no device
registered". `backend mint-missing` is what puts this box's device key on that list (it is safe to
run again: it mints only what is missing), and it mints the gatewayhub's hub keys too. A run and a
fetch name an instrument by the venue's own list, kept in the project: `data catalog refresh` fetches
it through the gatewayhub, once per venue (the starter dataset is binance's), and a run over a venue
with no list is refused naming that command. If the bars went somewhere else, name that directory:

```sh
vike-backend datahub --store DIR   # DIR: where the bars went
```

A run needs no profile: flags build one, and `--write-profile FILE` keeps it for next time. A vendor
archive you downloaded onto the datahub's box is read in with:

```sh
vike-cli data history import FORMAT DATASET                # e.g. Dukascopy tick files, Binance aggTrades zips
```

A venue's public history takes no venue credentials: the datahub fetches it through the gatewayhub
started above, with the half of the hub keys it read when it started (once, at start: a datahub
started before `backend mint-missing` ran needs a restart):

```sh
vike-cli data history fetch VENUE:SYMBOL:INTERVAL --days 180  # asks the datahub
```

## Credentials, and the gate that keeps an account from running

There is exactly one credential store, the settings database `<project>/settings/db/vike.db`, and
**absent credentials are the live gate** — a `demo` or `live` account with no keys does not run. Each
account trades at its own tier (`paper`, `demo` or `live`) only while its `account` row is active, or
it does not run: no row, an inactive row, or a tier its keys cannot bind exactly runs nothing — never
paper in its place. `vike-cli account ls`
lists the rows, and `vike-cli account activate|deactivate|set-tier --id <N>` changes one.
⚠ A credential belongs to its ACCOUNT, and storing one never creates an account or chooses its
tier: `vike-cli account add --venue <VENUE> --tier <paper|demo|live> --no-label` creates
the row, born ACTIVE, so a `live` row that then holds its keys arms that account: a running daemon
follows the row within about a second, and a stopped one at its next start.

On a fresh box `vike-cli secrets init` creates the empty store; `vike-cli secrets set --id <ID>
<FIELD>` adds one of an account's credentials (the value on stdin, never on the command line);
`vike-cli secrets path` prints which store answers; `vike-cli secrets ls` shows which keys it
holds, account by account.
Nothing in this workspace deletes, moves or rewrites that database as a whole.

**Dukascopy needs a Dukascopy account, even for history alone; a free demo account is enough.** Its instrument list, its price
grids and the point scale of its tick files come from the account's own login, and none is built into the program. Until a
Dukascopy account has logged in, every Dukascopy history fetch, archive import and order answers with one line saying what
to do: no Dukascopy instrument list yet: open a free Dukascopy demo account, add it with
`vike-cli account add --venue dukascopy --tier demo --no-label`, file its login with
`vike-cli secrets set --id <ID> LOGIN` and `PASSWORD` (each value on stdin), and start it; the list arrives at its first
login.

**Dukascopy's bulk tick archive is downloaded by the datahub itself**, from the vendor's Requester-Pays S3 bucket, with your
OWN AWS key stored on the datahub's box: `vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_ACCESS_KEY_ID` and
`vike-cli secrets set DUKASCOPY_ARCHIVE_AWS_SECRET_ACCESS_KEY` there, each value on stdin (the key's IAM user needs
`s3:ListBucket` and `s3:GetObject` on the bucket). Then
`vike-cli data history import dukascopy-bi5 EURUSD --from 2024-01-02 --to 2024-01-31 --fetch --dry-run` plans the download
(the files it would fetch and its estimated AWS cost) and stops; without `--dry-run` it downloads the missing days and imports them. Every
request is billed to that key's AWS account.

## Licence

[FSL-1.1-ALv2](LICENSE.md) — source-available, and each version becomes Apache-2.0 on the second
anniversary of its release. You may read, modify, self-host and run it, including commercially, and
you may charge for support and consulting; you may not offer it as a competing product or hosted
service. The two-year clock means every release eventually lands under a permissive licence.

## Where to put a bug, a question, or nothing at all

**A bug or a wrong claim in these pages** belongs in
[issues](https://github.com/vike-io/vike-trader/issues). Issues are open and read. Say what you ran, what happened, what you expected, and the output of `vike-cli --version`.

**A question about using it** belongs in
[discussions](https://github.com/vike-io/vike-trader/discussions).

**What this repository cannot take is a pull request**, and the reason is structural rather than a
policy about contributors: this mirror is a SNAPSHOT — one commit per release, force-pushed, with no
history — because a mirror that kept history would hand back every file an earlier release withheld.
A branch here is replaced by the next publish. So a patch is welcome as an issue carrying the diff,
and it lands upstream by hand.

**What will not be answered**: requests to run your strategy, judge whether an edge is real, or
support a fork. The tests in this tree are the argument for every claim it makes; start there.
