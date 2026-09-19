---
name: run-a-backtest-locally
description: Run a backtest or a parameter search on THIS machine with `vike-cli backtest run --local`, against a local history store, with no vike-datahub and no backtest server anywhere. Use when a user asks to backtest without a server, says the datahub is unreachable or they do not want to run one, asks to run a profile offline or on their laptop, asks what `--local` does, or asks what a `vike-cli` exit code means. Covers the local versus remote choice, the profile and script flags, where the standalone engine is looked for, why it is a Linux and container asset, and the exit ladder a wrapper script should branch on.
metadata:
  tools: ""
  source: "ai/trader/backtesting trader/tutorials/rsi-mean-reversion-backtest"
---

# Run a backtest locally

`vike-cli backtest run` has two modes, and the whole skill is choosing between them honestly.

| mode | what it needs | when |
| --- | --- | --- |
| REMOTE (the default) | a reachable backtest daemon at `--addr`, holding the data | somebody else runs the store; you have a thin client |
| `--local` | a history store on THIS box, plus the standalone engine | no server, or the data is here |

The MCP tools this repository serves are the REMOTE half only. If a user wants a run with no
server at all, that is a command line, not a tool call — hand them the command.

| verb | what it does |
| --- | --- |
| `vike-cli backtest` | compute a strategy over history and judge what came back: `backtest run` runs one on a remote vike-datahub server (or --local), and the PROFILE says what — a [paramscan] grid searches the parameter space, a [walkforward] table walks it forward, and declaring both ships the profile whole to the walk-forward runner. Then `backtest ls\|show\|path` over the run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn that into a CI exit code, and `params` and `strategies` for what can be tuned and what can be run. Before a strategy exists: `templates` ships starters, `script-api` prints everything a Rhai script may call, and `script-check` compiles one and answers in the exit code |
| `vike-cli data` | the hist store: fetch real bars or seed the demo tape into it (write, local), list what it holds and report coverage (read, over --addr) |
| `vike-cli config` | this box's settings: provenance (`show`), a validating pre-flight (`check`), and a journalled one-key write (`set`) |
| `vike-cli mcp` | serve the create+backtest tools over stdio MCP (for Claude / an agent) |
| `vike-cli trade` | observe + control a running vike-tradehub node: `trade status\|halt\|resume` run one operation and exit, and a bare `trade` opens the interactive REPL (for a human) |
| `vike-cli report` | ask a vike-tradehub node for a tearsheet over its live journal, or re-render a FINISHED run from this machine's own run directory (read-only; a node built from this tree serves the live verb, an older one refuses and names the command that does) |
| `vike-cli research` | investigate a signal and FIT a model — the plane BEFORE a strategy exists: `research study` asks the backend to run a compiled study over the hist store it holds (served by `vike-backend backtest --addr` when it mounts the study runner; a peer that does not refuses by name and points at `vike-backend study`) |
| `vike-cli secrets` | inspect the credential store THIS box reads — `<project>`/settings/secrets.env, or the settings database `<project>`/settings/db/vike.db once migrated, which `secrets path` reports — set ONE key in it, and perform that migration (list \| path \| template \| set \| migrate) |
| `vike-cli backend` | stand a vike-tradehub node — the running daemon — up, and attach this box to one (setup on the daemon \| connect \| status \| disconnect on the client) |
| `vike-cli datahub` | MINT the node key pair a vike-datahub server authenticates with (setup), on that server's box |
| `vike-cli init` | create `<project>`/user_data — strategies, profiles, results — with examples |
| `vike-cli indicators` | print the indicators a Rhai strategy can call, with their parameters |
| `vike-cli surface` | write this binary's own command surface as JSON, for the documentation the docs site generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled in, so the answer is a property of THIS build |

## The local commands

```sh
vike-cli backtest run --local --profile user_data/profiles/backtest.toml
vike-cli backtest run --local --profile run.toml --explain-data   # what it WILL read, then stop
vike-cli backtest run --local --profile run.toml --script user_data/strategies/rhai/sma_cross/sma_cross.rhai
vike-cli backtest run --local --profile sweep.toml --rank-by sharpe   # a [paramscan] table SEARCHES
vike-cli backtest run --local --profile sweep.toml --optimizer tpe --trials 128
vike-cli backtest run --profile sweep.toml --optimizer tpe --trials 128 --addr 127.0.0.1:7880
```

⚠ **There is no `sweep` verb.** A profile carrying a non-empty `[paramscan]` table IS a parameter
search — the same predicate the engine and the server both branch on — and `--optimizer` names the
method. `vike-cli sweep` fails naming this command. (The section was `[sweep]` before; that
spelling still loads, permanently, so profiles already on disk keep working.)

Flags `backtest run` takes in both modes, meaning the same thing in each:

- `--profile <run.toml>` — the profile. Required for a run.
- `--script <s.rhai>` — a Rhai script, injected into the profile's `[strategy.params].src`.
  ⚠ It only RUNS if `strategy.name` is `"rhai"`; under any other name it rides along as an unread
  param while the built-in strategy runs and the report describes something else.
- `--preset <p.toml>` — a preset whose keys are merged into `[strategy.params]`.
- `--json` — machine-readable output.

A parameter search takes five more flags. The two SELECTORS (`--rank-by`, `--optimizer`) are
spelling-checked locally, so a typo is a usage error rather than a wasted round trip; the three
KNOBS are forwarded as typed and judged where the run happens, which is what keeps ONE refusal
sentence for both modes:

- `--rank-by sharpe|return|max_dd|equity|multi` — how to ORDER the rows. It is IGNORED, not an
  error, on a profile with no `[paramscan]` table: it says how to order results, not what work to do.
- `--optimizer grid|euler|tpe|genetic` — the search METHOD (default `grid`, the exhaustive
  product). ⚠ `genetic` REQUIRES `--seed`: a genetic search reports one sample of a distribution,
  and a seed nobody typed is a constant the result silently depends on.
- `--euler-depth N` / `--trials N` / `--seed S` — the per-method knobs, each refused under a method
  that does not own it.

⚠ The method and its knobs work in BOTH modes. `Request::RunSweepProfile` carries a `search`
selector, and one module resolves it on whichever side runs — so a knob handed to a method that
does not own it (`--trials` under `--optimizer euler`) is refused with the same sentence either
way. Against a backtest daemon OLDER than that capability the command is refused by name before
anything is sent, rather than quietly running the grid.

Flags that exist for `--local` only, because they name things a server would own:

- `--store DIR` — the history-store root to read. Refused in remote mode; the store is the
  server's there.
- `--engine PATH` — name the standalone engine outright instead of searching for it.

`vike-cli backtest params --script s.rhai` is its own SUB-VERB and is offline — it needs neither a
server nor a store, and there is no local/remote choice to make. It prints a script's tunable
`param(name, default)` knobs and exits. (It was the `--list-params` flag on `backtest` until the
sub-verbs landed; the old spelling answers with this one.)

## Plan the data before you spend the compute

A local run reads a store on THIS box, so "is the data actually here" is answerable before anything
computes. `--explain-data` resolves the whole slice, prints it, and stops:

```sh
vike-cli backtest run --local --profile run.toml --explain-data
```

What comes back is a PLAN, not a report — the store root with the RUNG that chose it, the resolved
window, one line per series the run will open with its row count and recorded span, and a `MISSING`
line under any series that falls short of the window. Where the short series is a per-symbol bar
series, the plan prints the `vike-cli data fetch` line that would fill it, so the next command is
one you copy rather than compose (`crates/vike-backtest/src/data_plan.rs`'s `fetch_hint`). It exits
`0` — a plan that found holes ANSWERED the question — and mints no run directory. With `--json`,
the same document comes back as JSON.

⚠ It proves what the STORE holds and nothing else: the strategy is not compiled and the engine
params are not built, so a profile that plans cleanly can still fail on either.

Four more flags turn that finding into the run's own verdict, so a wrapper never has to parse a
plan:

- `--require-coverage` — refuse a run whose window the store does not cover, naming the missing
  spans per series. The failure it ends: a window with a complete trade tape and no book runs to
  completion and REPORTS FILLS.
- `--max-gap 1d` and `--on-gap warn` — the tolerance and the disposition. Both are read only once
  `--require-coverage` has armed the gate, and neither arms it. Unset, the tolerance is zero and
  the disposition is `refuse`.
- `--universe strict` — what to do about a member whose tape does not span the window: `declared`
  takes the list verbatim, `covered` names the short members and runs anyway, `strict` refuses.
  None of the three DROPS a member.
- `--decide simultaneous` — WHEN a multi-symbol step is decided. It makes the answer a property of
  the instrument SET rather than of the order `data.symbols` happens to list them in. Bar mode
  only, and refused beside an armed `[risk]` table or `engine.leverage`.

So the sequence a local run is actually worth, once a plan has named a hole:

```sh
vike-cli backtest run --local --profile run.toml --explain-data
vike-cli data fetch binance:BTCUSDT:1h --from 2026-01-01 --to 2026-02-01
vike-cli backtest run --local --profile run.toml --require-coverage
vike-cli backtest show @last --metrics
```

⚠ **None of those six reaches the engine as a flag — that is what keeps `--local` a rehearsal.**
Each one writes a profile key — `data.explain`, `data.require_coverage`, `data.max_gap`,
`data.on_gap`, `data.universe`, `engine.decide` — while `crates/vike-cli/src/cmd/backtest.rs`'s
`execute_local` hands the child only `--profile`, `--store`, the five search flags and `--json`. A
flag that changed the profile makes its text differ from your file, so the rewritten TOML is staged
under `<project>/tmp` and THAT is the path the child is given: the bytes the local engine parses
are the bytes a remote server would have been shipped. It also means the side that RUNS owns the
refusals — a `--max-gap` whose span grammar does not parse is refused by
`crates/vike-backtest/src/harness/profile.rs`'s `DataCfg`, in the same sentence on either route.

⚠ Staging needs a project above the working directory. Run from outside one with any flag that
rewrote the profile and `--local` refuses rather than guessing a location — hand `--profile` an
already-merged file, or run inside the project.

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
is published. So `--local` and the whole `data` verb are unavailable on Windows
until the user supplies an engine themselves. A miss on all four rungs is a clean failure naming
what is missing and how to get it, per platform — read that message rather than re-deriving it.

⚠ **Two search flags belong to that engine binary and cannot be typed on `vike-cli backtest run`
at all** — `--min-trades N`, the significance FLOOR below which a trial is UNRANKABLE rather than
merely penalised, and `--progress auto|none|json`, the stream a search writes to stderr. On the
client both are unknown arguments, so they exit `2`. Reach them by driving the engine directly; it
takes the profile as its first positional argument:

```sh
backtest sweep.toml --optimizer tpe --trials 128 --seed 7 --min-trades 30 --progress json
```

`crates/vike-datahub-client/src/flag_vocab.rs`'s `BACKTEST_FLAGS` is where that split is recorded,
one reason per row. `--min-trades` is engine-only because the WIRE cannot carry it: a client that
accepted it would have to forward it on `--local` and DROP it on `--addr`, which is the silent
downgrade the capability handshake exists to refuse. `--progress` is engine-only by NATURE and will
not change — `crates/vike-backtest/src/harness/optimize.rs`'s `StderrProgress` writes to the
process's own stderr, and over a socket that stream is the daemon's terminal rather than yours.

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
| `6` | A DECLARED THRESHOLD WAS BREACHED — the product of `vike-cli backtest gate`, and the one rung on this ladder that is a RESULT rather than a failure. |
| `7` | NOTHING WAS EVALUATED — an empty slice, no coverage, zero trials, a run directory with no report in it, or a set of criteria that named no key the document carries. |

⚠ **The spawned engine has its OWN two-rung ladder and the numbers do not mean the same things.**
Its `2` covers a bad command line AND a failed venue fetch, an unopenable store and a failed demo
seed — so `vike-cli` deliberately does not re-publish that `2` as its own. Do not read a local
run's exit code as if you had run the engine directly.

## Procedure

1. **Check there is data.** A local run needs a store with the profile's series in it. If the user
   has none, that is the `get-market-data` skill first — `vike-cli data fetch` or
   `vike-cli data seed-demo`. Ask with `--explain-data` before you run: an empty store and a
   strategy that found no trades produce the same disappointing report.
2. **Check the profile names what the store holds** — venue, symbol, kind, interval, and a
   `from`/`to` inside the covered span. A mismatch is a server-side parse or validate failure with
   the reason in the message. `--require-coverage` turns that check into the run's own refusal,
   which is what a wrapper script arms rather than re-deriving from a plan.
3. **Run it**, with `--local`.
4. **Read the report the same way as any other** — `zero_trade` first on a disappointing run, then
   drawdown and equity beside the Sharpe, never one number as the verdict. That is the
   `read-a-backtest-report` skill, and nothing about it changes because the run was local.
5. **Do not stop at one sample.** One symbol over one range is one sample whether the engine ran
   here or on a server; the next step is a walk-forward, not a conclusion.

## When NOT to use this

- The user already has a server. Remote is the default for a reason — the data is where the
  server is, and shipping a profile is cheaper than shipping a tape.
- The user is on Windows and has no engine. Say so plainly instead of suggesting a flag that
  cannot work there.
- The question is about interpreting a report rather than producing one. That is a different
  skill, and it needs no run at all.
