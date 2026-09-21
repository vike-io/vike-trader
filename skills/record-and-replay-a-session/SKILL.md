---
name: record-and-replay-a-session
description: Record a live market feed to a tape with `vike-backend datahub --record`, prove the recorder is really recording, then replay the result — a recorded market TAPE through the backtest engine via list_series + run_backtest, or a live session's COMMAND JOURNAL through the real core via replay_journal. Use when asked to record a feed, capture a tape, build your own history store, backtest a market you watched, replay a session, replay a recorded tape, replay a journal, verify a live run was deterministic, turn a recording into a backtest series, or when "replay" is ambiguous and you must decide which of the two it means.
metadata:
  tools: "list_series run_backtest"
  source: "trader/guides/record-a-feed trader/guides/replay-a-session trader/tutorials/record-and-replay ai/trader/tools/list_series"
---

# Record a session, then replay it

"Replay" means two different things and they answer different questions
(`https://vike.io/docs/trader/guides/replay-a-session`):

| replay | input | engine | what it proves |
|---|---|---|---|
| **Tick replay** | a recorded market tape in the history store | the backtest engine | how a *strategy* behaves on the quotes, trades and book events you actually recorded |
| **Journal replay** | a live session's command journal | the real single-writer core, offline | the *run* was deterministic — the re-folded final state hash equals the journaled one |

The first tests a strategy; the second tests the run. Decide which one the user means before doing
anything. Only tick replay is reachable through MCP tools (`list_series`, `run_backtest`); recording
and journal replay are operator-run binaries, so those steps are instructions you give, not calls
you make. This skill calls no write tool and places no order.

## Part A — record a live feed to a tape

Recording is `vike-backend datahub --record <profile>` — the SAME process that serves the store,
since ruling 10 merged `vike-recorder` into the data daemon so one process owns venue connections,
the store and serving. It is deliberately not the desktop app (a tape that exists only while a
window is open has a hole wherever you closed it) and not the trading daemon (a trading restart must
not punch a hole in the tape) (`https://vike.io/docs/trader/guides/record-a-feed`).

1. **Write the profile.** One TOML names the store and what to record
   (`https://vike.io/docs/trader/tutorials/record-and-replay`):

   ```toml
   store = "market_data/hist"

   [[subscribe]]
   venue = "polymarket"
   family = "btc-updown-5m"
   backfill = "off"

   [maintenance]
   interval_secs = 300
   min_parts = 4
   ```

   A subscription names a `family` **or** `symbols`, never both. A family is recorded as one grouped
   series (one commit per family rather than one per token). Every struct is `deny_unknown_fields`:
   a misspelled key is refused by name with the line number and the accepted keys — `webhook` for
   `webhooks` once left an operator believing a pager was armed.
2. **Get the store line right — two traps in one line.** The directory name must be `hist`: it is
   the name every binary's default resolves to (`crates/vike-model/src/state_path.rs`'s
   `HIST_SUBDIR`), so a store called anything else is one a reader run without `--store` cannot
   find — it reports zero rows rather than failing. And a **relative** `store` resolves against the
   process working directory: right for the daemon (its unit sets `WorkingDirectory=<project>`),
   wrong for a hand-run command, which was measured writing its tape into a source checkout while
   the project's own `market_data/` stayed empty. The store you record into must be the one the
   `vike-datahub` server holds — and since the merge it IS: the daemon REFUSES to start when the
   profile's `store` and `VIKE_DATAHUB_STORE` disagree, so `list_series` in Part B can no longer be
   pointed at a different store than the one being written.
3. **Build with the venue feature.** Each recordable venue is a Cargo feature
   (`crates/vike-recorder/src/venues/mod.rs`); build `vike-datahub` with `--features
   record-polymarket` (or `record-binance`).
   A feature-absent build is a startup error naming the missing feature, never a silent no-record.
   Know what a Binance tape contains before planning a study: its caps are bars, trades and depth
   with `quotes: false` and `book: false`, so the tape is the trade tape plus a conflated `depth`
   series — not a lossless L2 book.
4. **Dry-run before daemonizing:** run with `--profile <file> --once`. It runs a single tick and
   exits, proving the profile parses, the store opens and each family resolves to live symbols.
   **Read the exit code, not the log** — `0` only if every feed ended the tick with a live
   subscription, `4` (`EXIT_DRY_RUN`) otherwise, naming each feed that would record nothing.
5. **Run it, and trust the two watchdogs, not the clean startup log.** A feed can be subscribed and
   connected yet receiving nothing, invisible to every other vantage point — a production recorder
   ran 95 minutes in that state writing nothing (`crates/vike-recorder/src/liveness.rs`).
   `SilenceWatch` watches *series* (`--silent-secs`, default 300 s — a Polymarket up/down family
   rotates every five minutes, so shorter cries wolf); `ResolveWatch` watches *feeds* — a venue that
   never produced a symbol to subscribe, which the series watch is structurally blind to. Both are
   always on; a silent series is always logged and alerted. `--exit-on-silence` (`EXIT_SILENT` = 3)
   is opt-in, because a quiet market must not kill a daemon recording five healthy ones. The
   `[alerting]` table's absence means defaults, not off; `webhooks` names targets and **no
   credential goes in this file**.
6. **Size `[maintenance]`.** Its absence means defaults, not off. A busy series was measured at
   23 parts in 150 s (about 13,000 files a day unmerged). `max_merge_rows` is the memory knob, in
   rows because a merge is materialized whole — a 64 MB budget peaked at 8.95 GB on a live
   Polymarket book. `retention_days` omitted keeps forever; `0` is rejected.
7. **Stop without losing the tape.** A control word on stdin (`quit` / `shutdown` / `stop` / `exit`
   — the code matches four, its doc lists three; follow the code), or SIGTERM/SIGINT through
   `vike_ops::stop`'s `install_handlers` (Ctrl-C, Ctrl-Break and window close on Windows). A
   **detached** recorder has neither a console nor a stdin and watches no stop file, so `taskkill`
   is a kill and loses the buffered tape. Teardown is feeds first, then sink — the sequence is
   budgeted so a hard cap abandons a compaction pass and never the rows.

## Part B — replay the tape through the backtest engine (tick replay)

1. **Confirm the tape is in the store: call `list_series`** with no arguments (`{}`). It returns
   `{ "series": [ {kind, venue, symbol, interval, first_ts, last_ts, rows} ] }` — one row per stored
   series with the span and row count it covers, read from the manifest with no DataFusion scan
   (`https://vike.io/docs/ai/trader/tools/list_series`; `crates/vike-cli/src/cmd/mcp.rs`'s `tool_list_series`).
   `interval` is `null` for tick series. Pick `from`/`to` inside `first_ts..last_ts`.
   - A **blank `symbol` means a grouped series** (a recorded family), never "no symbol" — the tool
     does not project `group`, so treat it as "grouped, ask the store".
   - An error whose text begins `cannot connect to datahub at` means nothing is listening at the
     `--addr` the MCP server was started with (default `127.0.0.1:7878`); start or point at the
     `vike-datahub` that holds the store. If the datahub answers but the tape is absent, the recorder
     wrote to a store the datahub does not serve — Part A step 2.
2. **Write a tick run profile.** `kind = "tick"` on `[data]` makes the harness load from the
   history store instead of the bar lane (`crates/vike-backtest/src/harness/profile.rs`'s
   `DataKind`) (`https://vike.io/docs/trader/guides/replay-a-session`):

   ```toml
   [data]
   kind = "tick"
   venue = "polymarket"
   symbols = ["<token id from list_series>"]
   from = "2026-08-01T00"
   to   = "2026-08-02T00"

   [strategy]
   name = "<a compiled strategy name>"
   ```

   A cross-venue slice uses `[[data.series]]` instead, one entry per series with its own venue,
   symbol and lane kind; order is meaningful — the k-way merge breaks an equal-timestamp tie by
   stream order, so list a reference series first.
3. **Decide the delivery clock.** The default orders each stream by the venue `ts` alone, so the
   strategy sees every event the instant the venue stamped it — earlier than any live consumer,
   which on a proxied feed systematically flatters the backtest. `[engine] feed_latency = true`
   orders each symbol's stream by the arrival clock (`local_ts`) instead; it retags nothing and
   matching stays on venue time. A `local_ts` of zero falls back to venue `ts`; one earlier than
   venue `ts` is clamped up. It is tick-mode only — a bar-mode profile setting it is rejected at
   load, as is the queue-position fill model.
4. **Call `run_backtest`** with `profile` (the TOML above, as a string — required) and optionally
   `script` (Rhai source the server injects as `[strategy.params].src`). It runs on the remote
   `vike-datahub` server and returns `{ "report": <BacktestReport JSON> }`; `readOnlyHint: true` —
   a backtest mutates nothing (`crates/vike-cli/src/cmd/mcp.rs`'s `tools_spec` and
   `tool_run_backtest`). Profile-shape errors surface from the server, the one profile parser.
5. **Read the result knowing the merge rules.** `crates/vike-backtest/src/hist_replay.rs`'s
   `replay_ticks` merges each symbol's `scan_quotes`, `scan_trades` and `scan_book_updates` into one
   timestamp-ordered stream; the tie-break at equal timestamps is **Book, then Quote, then Trade**.
   A symbol with zero rows of all three kinds in range is a warning and a skip, not an error, so an
   all-sparse window is an empty-but-valid report — check `rows` from step 1 before reading an
   empty report as a strategy that never traded.

## Part C — replay a command journal (proves the run, not the strategy)

No MCP tool does this; give the operator the procedure (`https://vike.io/docs/trader/guides/replay-a-session`).

1. **Have a journal.** It is off by default. `crates/vike-core/src/run_profile.rs`'s
   `journal_config_from_env` resolves it from exactly two places: a loaded `VIKE_RUN_PROFILE` is
   authoritative (its `[sinks.journal]` decides, even when that means no journal) and the
   `VIKE_JOURNAL_DIR` quick knob is consulted only when no profile is present. A set-but-malformed
   profile disables journaling with a warning rather than falling through.
2. **Run `replay_journal <journal-dir>`** (`crates/vike-core/src/bin/replay_journal.rs`, one
   argument). It prints the total source record count (commands and snapshots), the number of
   snapshots verified, the final state hash, and one line per engine with open orders, positions and
   balance. `REPLAY FAILED` plus a non-zero exit is a failed fence.
3. **What the fence is.** `crates/vike-core/src/replay.rs`'s `replay_offline` restores from the
   journal's **first** `Snap`, re-folds the whole tail through a real core with a no-op
   `ReplayClient` (venue events are already journaled and re-pumped), a `QueueClock` replaying the
   recorded `now_ms`, and **no strategy mounted** (every intent was journaled write-ahead, so it is
   re-applied, never re-evaluated), then compares the final hash to the **last** `Snap`. Final-hash
   equality is the guarantee — sequence numbers differ, so per-record comparison is impossible.
4. **Two tails it refuses rather than approximates:** a multi-engine base and a watchdog-tick tail
   both return `ReplayError::Unsupported`. A refusal is the honest answer. Its sibling
   `restore_from_journal` restores from the *latest* `Snap` with no fence — a restore, not a proof.
   A journal written by a later build (version above `VERSION`) is a hard error; older ones from
   `MIN_READABLE_VERSION` up read fine.

## Reporting back

State which replay ran and what it proves: a tick replay report speaks about the strategy on that
tape (name the `from`/`to`, the `feed_latency` setting, and any skipped-sparse symbols); a journal
replay speaks only about determinism of that run. Never present one as evidence for the other.
