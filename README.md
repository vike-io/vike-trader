# vike

A Rust trading platform: a live execution core, a backtest and research harness, a desktop
charting shell, and bridges to fourteen venues.

This repository is a **source mirror**. It carries the code and nothing else — no CI workflows, no
operator runbooks, no history. Each publish is one commit. The development repository is private;
what you see here is what the released binaries are built from.

## What is in the box

| | |
|---|---|
| **Live daemon** (`vike-tradehub`) | headless execution: mounts a venue, runs a strategy, reconciles against the venue, and stops cleanly on SIGTERM with the resting book handled |
| **Backtest + research** (`backtest`, `vike-study`, `tearsheet`) | a bar/tick harness with parameter sweeps, successive-halving refinement and a TPE optimizer; reports persist as a run manifest |
| **Desktop shell** (`vike-app`) | charts, order entry, a data manager and a connections editor, on egui/wgpu |
| **Data** (`vike-datahub`, `vike-recorder`) | a Parquet/DataFusion history store, a live recorder, and a query server |
| **Venues** | binance, bybit, okx, deribit, hyperliquid, aster, alpaca, ctrader, ig, oanda, polymarket, dukascopy, fxcm, ibkr — each a crate under `crates/bridges/` |
| **Agent skills** (`skills/`) | ten procedures for an agent driving `vike-cli mcp` — backtesting, authoring a strategy, and trading on a node through the preview gate — one `SKILL.md` each |

## Running it

Container images carry every headless tool as one binary with a symlink per tool:

```sh
docker pull vikeio/vike-tradehub:latest      # Docker Hub
docker pull ghcr.io/vike-io/vike-tradehub    # GHCR
```

The command-line tool installs from the release without a toolchain (Linux; any other target
compiles from the clone — `crates/vike-cli/Cargo.toml`'s `[package.metadata.binstall]` says why):

```sh
cargo binstall --git https://github.com/vike-io/vike-trader vike-cli
```

From source:

```sh
cargo build --workspace                      # no native dependencies on default features
cargo run -p vike-app                        # the desktop shell (live feed; needs internet)
```

For an agent (Claude Code, Cursor, any MCP client), the skills install straight from this repo:

```sh
npx skills add vike-io/vike-trader           # the ten procedures under skills/, one SKILL.md each
```

## A fresh install has no market data — two commands fix that

```sh
vike-cli init                # example strategies, indicators and run profiles under user_data/
backtest --seed-demo         # a SYNTHETIC demo tape, written under the venue id `demo`
backtest --profile user_data/profiles/backtest.toml \
         --script user_data/strategies/rhai/sma_cross/sma_cross.rhai
```

The demo tape is a closed-form curve, **not market data**; it exists so the tools have something to
draw and run, and it is written under its own venue id so it can never be mistaken for, or
aggregated with, real rows. For real bars:

```sh
backtest --fetch binance:BTCUSDT:1h --days 180    # public history, no credentials
```

## Credentials, and the gate that keeps you on paper

There is exactly one credential store, `<project>/settings/secrets.env`, and **absent credentials
are the live gate** — a venue with no keys mounts as paper. A second ceiling sits above it:
`policy.venues.<venue>` in `<project>/settings/policy.toml` defaults every venue to `paper`, and it
can only ever refuse, never arm. A box with no `[venues]` table trades paper whatever its store
holds.

`vike-cli secrets template` prints the whole key grid with empty values;
`vike-cli secrets path` prints where the store belongs. Nothing in this workspace deletes, moves or
rewrites that file as a whole.

## Licence

[FSL-1.1-ALv2](LICENSE.md) — source-available, and each version becomes Apache-2.0 on the second
anniversary of its release. You may read, modify, self-host and run it, including commercially, and
you may charge for support and consulting; you may not offer it as a competing product or hosted
service. The two-year clock means every release eventually lands under a permissive licence.
