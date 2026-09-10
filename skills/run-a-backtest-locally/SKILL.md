---
name: run-a-backtest-locally
description: Run a backtest or a parameter sweep on THIS machine with `vike-cli backtest --local` and `vike-cli sweep --local`, against a local history store, with no vike-datahub server anywhere. Use when a user asks to backtest without a server, says the datahub is unreachable or they do not want to run one, asks to run a profile offline or on their laptop, asks what `--local` does, or asks what a `vike-cli` exit code means. Covers the local versus remote choice, the profile and script flags, where the standalone engine is looked for, why it is a Linux and container asset, and the exit ladder a wrapper script should branch on.
metadata:
  tools: ""
  source: "ai/trader/backtesting trader/tutorials/rsi-mean-reversion-backtest"
---

# Run a backtest locally

`vike-cli backtest` has two modes, and the whole skill is choosing between them honestly.

| mode | what it needs | when |
| --- | --- | --- |
| REMOTE (the default) | a reachable `vike-datahub` at `--addr`, holding the data | somebody else runs the store; you have a thin client |
| `--local` | a history store on THIS box, plus the standalone engine | no server, or the data is here |

The MCP tools this repository serves are the REMOTE half only. If a user wants a run with no
server at all, that is a command line, not a tool call — hand them the command.

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

## The local commands

```sh
vike-cli backtest --local --profile user_data/profiles/backtest.toml
vike-cli backtest --local --profile run.toml --script user_data/strategies/rhai/sma_cross/sma_cross.rhai
vike-cli sweep    --local --profile sweep.toml --rank-by sharpe
```

Flags `backtest` takes in both modes, meaning the same thing in each:

- `--profile <run.toml>` — the profile. Required for a run.
- `--script <s.rhai>` — a Rhai script, injected into the profile's `[strategy.params].src`.
  ⚠ It only RUNS if `strategy.name` is `"rhai"`; under any other name it rides along as an unread
  param while the built-in strategy runs and the report describes something else.
- `--preset <p.toml>` — a preset whose keys are merged into `[strategy.params]`.
- `--json` — machine-readable output.

`sweep` takes `--rank-by sharpe|return|max_dd|equity` and `--json`, and the same `--local` pair
below. The value is spell-checked locally so a typo is a usage error rather than a wasted round
trip; the metric itself is computed where the run happens.

Flags that exist for `--local` only, because they name things a server would own:

- `--store DIR` — the history-store root to read. Refused in remote mode; the store is the
  server's there.
- `--engine PATH` — name the standalone engine outright instead of searching for it.

`vike-cli backtest --list-params --script s.rhai` is offline in BOTH modes and needs neither a
server nor a store — it prints a script's tunable `param(name, default)` knobs and exits.

## ⚠ Where the engine comes from, and the Windows asymmetry

`--local` does not do the work in process: it SPAWNS a standalone `backtest` engine and inherits
its stdout and stderr, so the report reaches the terminal exactly as if the engine had been run
directly. That is not a shortcut — the engine opens a concrete DataFusion store, and this CLI's
whole identity is being DataFusion-free and small enough to be the thing an agent installs.

It is looked for in this order:

1. `--engine PATH`, if given.
2. `<project>/bin/backtest` — the runtime home project tools are installed into.
3. Beside the `vike-cli` executable — the rung that answers on an ordinary Linux install, because a
   release attaches both to the same place.
4. `PATH`, by bare name — the developer case.

**On Windows, rungs 2, 3 and 4 find nothing that came from a release**, because no `backtest.exe`
is published. So `--local`, `sweep --local` and the whole `data` verb are unavailable on Windows
until the user supplies an engine themselves. A miss on all four rungs is a clean failure naming
what is missing and how to get it, per platform — read that message rather than re-deriving it.

## Reading the exit code

A wrapper script, a CI step or an agent loop branches on the process exit code, so it is a public
interface and each rung licenses a different response — FIX a `2`, WAIT on a `3`, treat a `1` as
the ordinary "it ran and failed".

| code | meaning |
| :---: | --- |
| `0` | The command did what was asked. |
| `1` | The command ran and failed — the pre-existing catch-all, and still where every failure that has not been deliberately classified lands (see `From<String> for CliError`). |
| `2` | The command line was wrong: an unknown verb, an unknown flag, a missing required flag, a bad value. |
| `3` | A service could not be reached: no datahub, no node, a refused or timed-out connection. |
| `4` | ⚠ **RESERVED — nothing produces this yet.** Refused LOCALLY by a ceiling — a policy limit or a client-side guardrail — before anything was sent. |
| `5` | ⚠ **RESERVED — nothing produces this yet.** The venue or the node accepted the request and rejected the order: the far side spoke, and it said no. |

⚠ **The spawned engine has its OWN two-rung ladder and the numbers do not mean the same things.**
Its `2` covers a bad command line AND a failed venue fetch, an unopenable store and a failed demo
seed — so `vike-cli` deliberately does not re-publish that `2` as its own. Do not read a local
run's exit code as if you had run the engine directly.

## Procedure

1. **Check there is data.** A local run needs a store with the profile's series in it. If the user
   has none, that is the `get-market-data` skill first — `vike-cli data fetch` or
   `vike-cli data seed-demo`.
2. **Check the profile names what the store holds** — venue, symbol, kind, interval, and a
   `from`/`to` inside the covered span. A mismatch is a server-side parse or validate failure with
   the reason in the message.
3. **Run it**, with `--local`.
4. **Read the report the same way as any other** — `zero_trade` first on a disappointing run, then
   drawdown and equity beside the Sharpe, never one number as the verdict. That is the
   `read-a-backtest-report` skill, and nothing about it changes because the run was local.
5. **Do not stop at one sample.** One symbol over one range is one sample whether the engine ran
   here or on a server; the next step is a walk-forward, not a conclusion.

## When NOT to use this

- The user already has a datahub. Remote is the default for a reason — the data is where the
  server is, and shipping a profile is cheaper than shipping a tape.
- The user is on Windows and has no engine. Say so plainly instead of suggesting a flag that
  cannot work there.
- The question is about interpreting a report rather than producing one. That is a different
  skill, and it needs no run at all.
