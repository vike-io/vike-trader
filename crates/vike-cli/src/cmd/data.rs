//! `vike-cli data` — get market data into the hist store, and ask a datahub what is in one.
//!
//! ```text
//! vike-cli data hist fetch <SPEC> (--days N | --from LABEL --to LABEL) [--store DIR] [--json]
//! vike-cli data hist fetch --source starter|demo [--store DIR] [--json]
//! vike-cli data hist export <SPEC> --out FILE [--from LABEL] [--to LABEL] [--store DIR] [--json]
//! vike-cli data hist export <SPEC> --out FILE --addr HOST:PORT --format jsonl|csv
//!                        --from LABEL --to LABEL [--kind bar|quote|trade] [--window SPAN] [--json]
//! vike-cli data hist get    <SPEC> (--days N | --from LABEL | --to LABEL) [--limit N]
//!                        [--addr HOST:PORT] [--format table|json|jsonl]
//! vike-cli data hist ls     [--addr HOST:PORT] [--kind K] [--venue V] [--name N] [--class]
//!                        [--json]
//! vike-cli data hist gaps   [--addr HOST:PORT] [--kind K] [--venue V] [--name N] [--json]
//! vike-cli data hist coverage [--addr HOST:PORT] [--venue V] [--name N] [--partial-only] [--json]
//! vike-cli data hist health [--addr HOST:PORT] [--kind K] [--venue V] [--name N] [--json]
//! vike-cli data hist universe [--addr HOST:PORT] [--kind K] [--venue V] [--name N]
//!                        [--from LABEL] [--to LABEL] [--json]
//! vike-cli data hist gate   <SPEC> --require-days N [--max-gap D] [--require-kind K]...
//!                        [--addr HOST:PORT] [--json]
//! vike-cli data hist rm       --kind K --venue V (--symbol S [--interval I] | --group G)
//!                        [--produced-by PREFIX] [--dry-run] [--yes]
//!                        [--addr HOST:PORT | --store DIR] [--engine PATH] [--json]
//! vike-cli data hist repair   --kind K --venue V (--symbol S [--interval I] | --group G)
//!                        [--dry-run] [--yes] [--store DIR] [--engine PATH] [--json]
//! ```
//!
//! # ⚠ TWO HALVES, and they do not touch the same store
//!
//! `fetch` / `fetch-starter` / `seed-demo` / `export` reach a store on THIS machine, by spawning
//! the standalone engine against it.
//! `list` / `coverage` / `tape-health` / `universe` READ, by asking a running `vike-datahub` over
//! RPC about the store THAT process opened. There is no third mode: this crate cannot open a hist
//! store itself, because
//! doing so needs DataFusion and being DataFusion-free is the crate's identity (argued edge by
//! edge in `crates/vike-cli/Cargo.toml`, machine-checked by CI's `light-consumers` lane).
//!
//! ⚠ **Those four are RULING 12** (§0.7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). The engine carried
//! `--fetch`, `--fetch-starter`, `--seed-demo`, `--export` and `--rm-series` as FLAGS on a verb
//! whose job is running backtests, and not one of the five is about backtesting — they fetch from
//! venues, write the store, export from it and DELETE from it. The operator-facing spelling is
//! HERE now, and each old flag is refused by name
//! (`crates/vike-backtest/src/backtest_cli.rs`'s `refuse_a_retired_data_flag`).
//!
//! ⚠ **What moved is the SURFACE. The code did not, and could not** — every one of the five opens
//! a `DataFusionHist`. So each subcommand below SPAWNS `backtest data <sub>`
//! (`crates/vike-backtest/src/backtest_cli.rs`'s `run_data`), which is the same grammar typed at
//! the same words. A reader who "finishes" the ruling by deleting the engine's implementation
//! deletes these verbs with it.
//!
//! ⚠ **`repair` is a THIRD shape and is engine-only**, which is worth saying here rather than only
//! at the verb: it names a series by identity like `rm` does, but it has no `--addr` at all. The
//! argument is [`refuse_the_remote_route_on_repair`]'s and it is not about tidiness — a datahub can
//! only name series it ENUMERATED, and a series whose base manifest is missing is in no
//! enumeration. That is the exact failure `repair` exists for, so a remote route would be reachable
//! for precisely the series that do not need one.
//!
//! ⚠ **`rm` is the one subcommand that reaches BOTH**, and it is the reason the halves are stated as
//! a property of the SUBCOMMAND rather than of the flag: `--addr` sends it to a datahub, and its
//! absence spawns the engine against a store on this machine. That is not a third mode — it is the
//! same verb, reached by whichever of the two mechanisms the operator named, and the plan prints
//! which store answered either way. Both flags at once is a contradiction and is refused.
//!
//! So a `--store DIR` on a read verb is REFUSED rather than ignored, and the refusal names the
//! reason: that flag is a path this process would have to open, and the read verbs reach a store
//! only through a server that already has it open. It is the mistake an operator makes first —
//! `data fetch --store /srv/…` works, so `data list --store /srv/…` looks like it should — and a
//! silently-ignored `--store` would answer about a completely different store with no sign that it
//! had.
//!
//! # The read half: which verbs, and why the split falls there
//!
//! [`vike_datahub_client::DatahubClient`] has carried four store-metadata RPCs since the Phase-2
//! split, and until this module no `vike-cli` verb called any of them — the MCP server's
//! `list_series` tool was the only surface in this crate that did, so an operator at a shell had no
//! way to ask what a datahub holds while an agent did.
//!
//! * **`list`** — the headline. ONE `inventory()` round trip: every stored series with its cheap
//!   coverage. `list_series()` is deliberately not wired as a verb of its own; `inventory()` is its
//!   coverage-carrying superset over the same enumeration, and a second verb that answered the same
//!   question with less in it would only ever be the wrong one to reach for.
//! * **`gaps`, a VERB that SELECTS BY FILTER rather than by identity.** A gap query names ONE
//!   series, and a series is identified by four dimensions with a grouped/per-symbol alternative
//!   inside them (see below). A verb that built that identity out of flags would have to guess
//!   whether the operator meant a symbol or a group, and hand the server an id that matches nothing
//!   when it guessed wrong. This one guesses nothing: the ids come back from `inventory()` and the
//!   ones the FILTER selected are handed straight back to `series_gaps`, so this side never
//!   CONSTRUCTS a series identity. The cost is one extra round trip per matched series — the same
//!   shape `crates/vike-app-core/src/stored_load.rs`'s `load_stored_tree` pays for the Data
//!   Manager, and the reason the filter flags are worth typing before this verb is.
//!
//!   ⚠ It was `ls --gaps` until the surface design's §7 tree ruled a verb per question, and the
//!   flag is refused BY NAME on every verb rather than dropped — see [`parse`]'s flag section.
//!   Nothing about the SELECTION changed; only where the question is spelled.
//! * **`--class`, the READER of the recorded asset class, and a flag on `ls` for `gaps`'
//!   reason.** `vike_model::SymbolProperties::asset_class` is written by the venue producers into
//!   the `kind=properties` tape and — until this flag — was read back by NO production code in the
//!   workspace: `git grep` found `scan_symbol_properties` called only from the bridges'
//!   `filters_rec.rs` TEST modules. A stored field nothing reads is a field that goes wrong in
//!   silence, so this is where an operator sees `okx/BTC-USDT-SWAP` say `CryptoPerp` without
//!   knowing okx's naming, and — the more useful half — sees WHICH instruments carry no class at
//!   all, which is what says a venue producer has not been wired yet. It rides
//!   [`vike_datahub_client::DatahubClient::properties_as_of`], an RPC that has existed and been
//!   served since long before this flag, so nothing about the wire moved to add it.
//! * **Why it hangs off `list` and not off `coverage`, which is the instrument-shaped verb.**
//!   MEASURED: `crates/vike-data/src/datafusion_hist.rs`'s `coverage_report` skips every series
//!   whose kind is not in `crates/vike-data/src/coverage.rs`'s `TICK_KINDS`, and `bar` is not one
//!   — so an instrument a `data hist fetch` put in the store appears in NO coverage row, and a class
//!   column there would be blind to the commonest thing a store holds. `list` enumerates every
//!   series of every kind. The cost of that choice is that one instrument's class renders once per
//!   kind and interval; the ROUND TRIPS do not multiply with it, because [`execute_list`] probes
//!   once per distinct `(venue, symbol)` and reuses the answer.
//! * **`coverage`, a SIBLING subcommand rather than a second flag.** `coverage_report()` answers a
//!   different question over a different shape — per INSTRUMENT, joined ACROSS kinds, in UTC-day
//!   indices — and its whole product is the day one kind has and another lacks. Hanging it off
//!   `list` would make one verb emit two unrelated tables and, worse, put per-series epoch-ms gap
//!   ranges next to per-instrument day-index ones under one heading. Two units of "missing" in one
//!   output is a trap; two verbs is not.
//! * **`tape-health` and `universe`, two more SIBLINGS over the SAME `inventory()` call.** Neither
//!   adds an RPC and neither ships a row; both are pure folds over the answer `list` already gets,
//!   and they are separate verbs for `coverage`'s stated reason — a different question over a
//!   different shape, which hanging off `list` would have put under one heading. `tape-health` asks
//!   whether what is present is POSSIBLE (its findings are contradictions, not absences);
//!   `universe` asks what the store CONTAINED over a window (its rows are instruments, and its
//!   unit is a membership verdict, not a row count). Each module's own doc carries its argument and
//!   — this matters more — its declared bound: [`tape_health`] states the row-level checks it does
//!   NOT run and why they belong beside the data, and [`universe`] states that a first and last
//!   recorded row are EVIDENCE of a listing and a delisting rather than a venue calendar.
//! * **`gate`, the one read verb whose PRODUCT is the exit code.** Same two RPCs `ls` and `gaps`
//!   already drive, folded into a verdict a CI step branches on without parsing: the store either
//!   holds what a run needs or it does not. It is the only verb here that takes a SPEC and still
//!   never constructs a series identity — the spec SELECTS among the ids `inventory()` returned,
//!   for `gaps`' reason above. [`gate`]'s module doc carries what each criterion measures and,
//!   more importantly, the two things it deliberately does not: `--require-days` judges the
//!   recorded SPAN rather than the days actually in it, and a hole in this store is a WHOLE UTC
//!   DAY.
//!
//! # ⚠ A series is FOUR dimensions, and `VENUE:SYMBOL:INTERVAL` is not one of them
//!
//! `vike_data::SeriesId` is `(kind, venue, symbol | group, interval?)`: `kind` is `bar`/`quote`/
//! `trade`/`book`/`depth`, `interval` is `Some` only for bars, and a GROUPED series (one part
//! holding many symbols, told apart by a row-level column) carries an EMPTY `symbol` and a
//! `Some(group)` instead. Exactly one of `symbol`/`group` is meaningful for any given series.
//!
//! The `fetch` spec gets away with `VENUE:SYMBOL:INTERVAL` because a fetch always writes
//! `kind=bar` per symbol — it has no other shape to express. A LISTING has no such luck, so the
//! rendering carries `kind` and a `SCOPE` cell (`symbol` or `group`) as their own columns and never
//! joins the parts into one colon-string. The `--json` document goes further and carries the raw
//! `symbol` and `group` fields BESIDE the resolved `name`, so a machine reader gets the identity
//! itself rather than this side's rendering of it.
//!
//! That is also why the filter flag is `--name` and not `--symbol`: it matches
//! `vike_data::SeriesId`'s `label` — the symbol for a per-symbol series, the group for a grouped
//! one — and calling it `--symbol` would be the same lie in a smaller place, silently matching
//! nothing for every grouped series in the store.
//!
//! # What this verb is, and what it deliberately is not
//!
//! The WRITE half is **not a collector**. Every byte of the work is done by the standalone
//! `backtest` engine, which has carried these five operations since long before this verb existed.
//! What was missing was DISCOVERABILITY: `vike-cli init` used to end by telling a new user to run
//! `backtest --seed-demo`, which was a different binary with a different name, and a person who
//! installed "the vike CLI" has no reason to expect that the tool that runs a backtest is not the
//! tool that fetches the data for one. Ruling 12 finished that argument by retiring the engine's
//! own spelling; this is the route, and [`crate::cmd::engine`] is the ONE place that knows where
//! the engine lives and what its exit codes mean.
//!
//! ⚠ **It could not be anything else.** Fetching writes a hist store, which needs DataFusion, and
//! this crate's whole identity is being DataFusion-free — argued edge by edge in
//! `crates/vike-cli/Cargo.toml` and machine-checked by CI's `light-consumers` lane. `engine`'s
//! module doc carries the full argument for spawning rather than linking; it applies here
//! unchanged.
//!
//! # What is validated HERE, and why only that much
//!
//! The spec's SHAPE (`VENUE:SYMBOL:INTERVAL`, three non-empty parts) and the WINDOW (`--days N`, or
//! `--from`/`--to` together, exactly one of the two forms). Both are things an operator gets wrong
//! by typing, and catching them here makes them a usage error instead of a process spawn whose
//! diagnostic arrives from a binary the user did not name.
//!
//! What is NOT validated here is the VENUE and the INTERVAL, deliberately: a roster copied into
//! this crate would be a second list to keep in step — refusing a venue the far side supports, or
//! accepting one it does not, with equal confidence. The far side's own error names what it can do.
//!
//! ⚠ **WHICH far side changed, and this sentence named the wrong one.** It read "a property of the
//! ENGINE's `venue-fetch` feature and its collectors", which was true while `fetch` spawned the
//! engine. It asks a DATAHUB now, so the reachable set is that SERVER's `BackfillTable` — folded
//! from `vike_backfill::kline_source::KLINE_SOURCES` and advertised in the handshake. The
//! conclusion is unchanged and is in fact stronger: the set is now a property of a REMOTE process
//! this crate cannot see at parse time, so copying it here would be a list that goes stale across a
//! version skew rather than merely across a build.
//!
//! The read verbs' filters are validated even less, and on purpose: [`Filter`] is a
//! case-insensitive SUBSTRING match applied CLIENT-SIDE to what the server already sent, never a
//! roster check and never a wire argument. A `--kind bra` is a filter that matched nothing, not a
//! usage error — these verbs exist to DISCOVER what a store holds, and a filter that demands you
//! already know the exact string answers only questions you did not need to ask. The rendered rows
//! show what matched, and the empty case says how many series the filter was applied to, so a
//! too-wide or too-narrow filter is visible without a second run.
//!
//! # `--json`, and what a FAILURE looks like under it
//!
//! [`report_json`] is the document for the write half; the read half has one per verb
//! ([`list_json`], [`coverage_json`], [`tape_health_json`], [`universe_json`]) and `rm` has three
//! of its own (see [`execute_rm`]). Each is emitted on SUCCESS only. That is the shape every
//! sibling in this crate already has — `crate::cmd::secrets`'s `list`, `crate::cmd::init`,
//! `crate::cmd::backtest` — where a failure is a sentence on stderr plus a rung on the exit ladder
//! (`crate::exit`) and stdout carries nothing at all. It is deliberately NOT a `{"ok": false}`
//! document: matching every sibling beats being cleverer than them in one, and a caller already has
//! to read the exit code, which is the field such a document would be duplicating.
//!
//! ⚠ The rung is the SAME one the human path gives, which is the half worth checking when editing
//! here: a `--json` that folded the child's status differently would tell a wrapper to retry (or
//! not to) on the strength of an output format. `crate::cmd::engine`'s `fold_status` is the one
//! place that decision lives, and both paths call it.
//!
//! # The exit ladder on the read half
//!
//! One rung is classified and the rest deliberately are not. An unreachable datahub is
//! [`crate::exit::Exit::Connect`], classified at [`connect`] where the address is still in scope —
//! the same sentence and the same rung `crate::cmd::backtest` gives for the same socket, so a
//! wrapper backs off for one reason across every verb that dials a datahub. Everything AFTER the
//! connection opens stays on the pre-existing run-failure rung through
//! [`crate::exit::CliError`]'s `From<String>`: a server-side `Response::Error`, a protocol desync
//! and the `coverage_report` capability refusal are all facts about a box that ANSWERED, and
//! retrying any of them forever is what rung 3 exists to prevent (`crate::cmd::trade_status`'s
//! `failure_exit` spells the same rule against a tradehub node).
//!
//! # Why the row types here are this module's own, and not the wire's
//!
//! [`SeriesRow`] / [`Coverage`] / [`InstrumentRow`] are flattened out of the RPC answers at ONE
//! site each, immediately on arrival, so every renderer below is a pure function over plain data
//! and is unit-tested as one. The flattening is not free of cost — it is a second shape, which this
//! workspace is right to be suspicious of — and it earns its place twice: it is what resolves the
//! grouped/per-symbol alternative into an explicit `scope` + `name` ONCE rather than at four render
//! sites, and it is what this crate can do at all. `vike-data` is a DEV-dependency here (the
//! manifest argues every edge, and no library code has ever needed one), so `vike_data::SeriesId`
//! cannot be NAMED outside `#[cfg(test)]` — the values are read through their public fields and
//! `label()`, which needs no edge, and nothing about the wire types is restated.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use vike_datahub_client::{DatahubClient, NodeKeys, Scope};
use vike_model::{epoch_ms_to_utc_date, parse_date_label};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::engine;
use crate::exit::{CliError, CmdResult, Exit};

/// The STRUCTURAL scan: series whose own catalog contradicts itself. A module of its own rather
/// than more functions here, because every check in it is a pure fold over plain numbers and is
/// unit-tested against planted impossibilities — and because this file is long enough that a
/// seventh renderer in it would be read by nobody.
mod catalog;
/// ROWS OUT OF A **REMOTE** STORE — `export`'s second route, which walks the wire in fixed
/// wall-clock windows and writes `jsonl`/`csv` to `--out`. A module of its own for [`get`]'s
/// reason and for one of its own: the walk is pure arithmetic over two bounds and a step, the
/// CSV grammar is three decisions that have to be stated beside the code that makes them, and
/// neither can be tested through a socket.
mod export;
/// DATA READINESS AS AN EXIT CODE: the one verb here whose product is a NUMBER a CI step branches
/// on rather than a table a person reads. A module of its own for [`tape_health`]'s reason and for
/// one of its own — the judging is a pure fold over plain numbers, and a verdict whose rung is
/// decided anywhere but beside the words that render it is how a table comes to say `pass` while
/// the process exits on a breach.
mod gate;
/// THE ROWS THEMSELVES — the one verb of this plane whose product is a PRICE rather than a fact
/// about a store. A module of its own for [`gate`]'s reason and for one of its own: §8.2's two
/// rules (a window is REQUIRED, a row ceiling is REPORTED rather than silently applied) are pure
/// arithmetic over plain numbers, and a cost guard whose thresholds live anywhere but beside the
/// words that disclose them is how an answer comes to be cut without saying so.
mod get;
/// WHEN = NOW — the live wire, where this file's every other verb answers about a past window. A
/// module of its own for [`catalog`]'s reason: a group owns its own grammar, and this one's noun is
/// a live KEY `(venue, symbol, lane)` that lands in no store and has no window to bound.
mod realtime;
/// WHAT THIS BOX PERSISTS — the recorder's subscription ROWS, under `data realtime record`. A
/// SUB-GROUP of [`realtime`] and the first four-token path in this binary, so it is declared here
/// (its file is `src/cmd/data/record.rs`, a sibling of `realtime.rs`) and ROUTED from that module
/// above its parser. A module of its own for [`catalog`]'s reason, which bites harder here than
/// anywhere else on this plane: its noun is a stored SELECTION rather than a live key, and the two
/// read the SAME character oppositely — `@` marks a whole market family on this verb and is an
/// ordinary symbol byte on `watch`, because hyperliquid spells real instruments that way.
mod record;
mod source;
mod tape_health;
/// Point-in-time MEMBERSHIP: what the store CONTAINED over a window. Split out for
/// [`tape_health`]'s reason, and for one of its own — its verdicts are arithmetic over two
/// timestamps and a frame, which is exactly the shape that has to be testable without a store.
mod universe;

/// The default datahub listen address, and the same value `crate::cmd::backtest`,
/// `crate::cmd::walkforward` and `crate::cmd::mcp` each spell for themselves. A private copy per
/// verb rather than a shared `pub(crate)` one is this crate's standing convention for it: each
/// verb owns the default it documents in its own `USAGE`, and the value mirrors
/// `VIKE_DATAHUB_ADDR`'s default in the server bin. (There were five; the `sweep` verb was
/// deleted by ruling 13 — the count is gone with it rather than decremented, which is the shape
/// of claim this repository has watched rot.)
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// The engine SUBCOMMAND every write route spawns — `backtest data <sub> …`.
///
/// Spelled once here because [`engine_argv`] and [`rm_engine_argv`] both build it, and it is the
/// name of a surface in ANOTHER crate
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `DATA_SUBCOMMAND`): two literals would be two
/// places to notice a rename, and the failure mode is a child process refusing an argv this side
/// believes it built correctly.
const ENGINE_DATA_VERB: &str = "data";

/// How many partial DAYS `coverage` renders under one instrument before it stops and says how many
/// it withheld. A half-failed recording can be partial on every day it has, and a report that
/// scrolled a year of dates off the top of a terminal would bury the row it belongs to.
///
/// ⚠ It bounds the HUMAN rendering only. [`coverage_json`] carries every partial day, because a
/// machine reader asked for the report in order to fold it and a truncated array would be a wrong
/// answer rather than a long one.
const MAX_PARTIAL_DAYS_SHOWN: usize = 10;

/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
pub(crate) const USAGE: &str = "\
usage: vike-cli data <group> <verb> [options]

Get market data into the hist store the backtest engine reads, and ask a running
vike-datahub what is already in one. The two halves reach DIFFERENT stores and their
flags do not mix — each half's flags are refused on the other, by name.

GROUPS — every verb below lives in one, and the group is REQUIRED: there is no bare
`vike-cli data <verb>`, and each pre-group spelling is refused by name with its
replacement rather than silently accepted:
  hist         WHEN = a past window. The verbs on THIS page
  realtime     WHEN = now (watch | status | record) — `data realtime --help`. `record`
               is a SUB-GROUP of its own (ls | add | rm): what the BOX persists, as
               rows in this project's settings database. Its --addr half — editing
               ANOTHER box's rows — is designed and refused by name
  catalog      what is addressable (ls | show | refresh | venues) — `data catalog --help`
  source       where rows may come from (ls | show) — `data source --help`, and it
               reaches no server: every answer says what it did NOT check

FETCH — asks a DATAHUB, which is the only thing that reaches a venue:
  fetch SPEC   ask the datahub to pull REAL public bars into its store. SPEC is
               VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h). Needs a window:
               --days, or --from/--to. No credentials — this is public market data.
               ⚠ Needs a reachable datahub (--addr, default 127.0.0.1:7878): history
               is fetched by the backend, once, into the store. It takes no --store
               and no --engine, and refuses them by name rather than ignoring them

WRITE — drives the standalone `backtest` engine on THIS machine (attached beside this
binary in a Linux release; on Windows a binary you supply yourself, and the failure
message says how) — see --engine below:
  fetch --source starter
               download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue
               and no credentials) and load it. For a box a venue cannot be reached from
               — a geoblock, a locked-down network. Verified against its published
               SHA256SUMS; safe to re-run. Takes no SPEC and no window: the published
               span is fixed
  fetch --source demo
               write the SYNTHETIC demo tape into the store. Venue `demo`, a closed-form
               curve, NOT market data — it is the slice the shipped
               user_data/profiles/backtest.toml names, so a fresh install can run that
               profile immediately. Safe to re-run. Takes no SPEC and no window
  export SPEC  write one series OUT of the store to a standalone file (--out FILE). TWO
               ROUTES, chosen by --addr:
                 without --addr — the ENGINE, against a store on this machine: BARS to
                   Parquet, optionally bounded by --from/--to INDEPENDENTLY (an export
                   slices what the store already holds, so one bound alone is meaningful
                   and neither is required). Any venue the store holds, `demo` included
                 with --addr — a running datahub's store, streamed: --format jsonl|csv,
                   --kind bar|quote|trade (SPEC is VENUE:SYMBOL for the two tick lanes,
                   which have no interval), BOTH --from and --to required. The range is
                   walked in --window steps so each request fits one frame; nothing is
                   held in memory but the window being written. `--format parquet` is
                   the LOCAL route's — the remote one has no server-side encoder

READ — asks a running vike-datahub over RPC about the store THAT process opened. There
is no local-store read: opening one needs DataFusion, which this binary does not link:
  get SPEC     the ROWS themselves — the only verb here that shows you a PRICE rather
               than a fact about a store. SPEC is VENUE:SYMBOL:INTERVAL and it reads
               BARS; there is no --kind, because a quote, a print and a book level are
               three other row shapes. A WINDOW IS REQUIRED — --days N, or --from/--to,
               either of which stands alone — and at most --limit rows are printed
               (default 1000). Hitting that ceiling is REPORTED, never silent. --format
               jsonl is the pipeline form; bulk extraction is `export`
  ls           every stored series with its coverage — kind, venue, symbol-or-group,
               interval, rows, days, first/last. Add --class for the asset class each
               instrument's venue actually RECORDED
  gaps         the same enumeration, answering the OTHER question: the HOLES inside
               each matched series' recorded span, in epoch-ms. Takes the same
               --kind/--venue/--name filters — and costs one extra round trip per
               MATCHED series, so filter first. Every matched series is printed, clean
               ones included: an empty listing would otherwise mean either `nothing
               matched` or `nothing is missing`, which are opposite answers
  coverage     the CROSS-KIND report: per instrument, which days have trade but no
               book. A half-failed recording is invisible per series and obvious here
  health       the STRUCTURAL scan: series whose OWN CATALOG contradicts itself — more
               bars than the span's grid can hold (a duplicated timestamp, proven by
               counting), a span that ends before it begins, more date= partitions than
               days. `gaps` finds what is MISSING; a gap shows up in an equity
               curve as a flat stretch, while a duplicated bar shows up as alpha. Only
               the offenders are printed; the summary says how many were scanned.
               ⚠ NOT a row scan — it reads no OHLC, and --help's own note says why
  universe     point-in-time MEMBERSHIP: what the store CONTAINED over a window, with
               each instrument's FIRST and LAST recorded row. The survivorship-bias
               defence — ask `ls` what exists today and a backtest of last year
               samples only the instruments that survived it. Bound it with
               --from/--to (both optional and independent). ⚠ The endpoints are
               EVIDENCE of a listing/delisting, never a venue calendar: a tape that
               stops may be a delisting, a dead recorder, or an unfinished backfill
  gate SPEC    IS THE STORE READY? The verb whose PRODUCT is the exit code, for a CI
               step or an ExecStartPre= that must not run an hour of backtest on a
               store missing three weeks in the middle. SPEC is VENUE:SYMBOL[:INTERVAL]
               (or VENUE:@GROUP) and SELECTS among the series a datahub reported —
               exactly, never a substring. --require-days N is REQUIRED; add --max-gap D
               for the other half of `a whole year, WHOLE`, and --require-kind K for the
               tape a run needs. The verdict is a DOCUMENT naming every criterion that
               passed and failed, on stdout under both formats; the exit code is its
               summary — 0 held, 6 a declared threshold BREACHED (the command WORKED),
               7 nothing was evaluated

DELETE — reaches EITHER store: --addr asks a datahub, otherwise the engine runs
against a store on this machine:
  rm           DELETE stored series, IRREVERSIBLY. Selects on the four series
               dimensions — --kind and --venue are REQUIRED, and an omitted
               --symbol/--group/--interval is a wildcard. The PLAN is printed first,
               always: the store that answered, then every matched series with its
               rows/days and the commit keys that wrote it. --produced-by asserts
               that EVERY key of EVERY matched series carries that prefix, and one
               foreign key refuses the whole run; it is REQUIRED for a sweep. Confirm
               with --yes, or by typing `delete N series` at a terminal. Matching
               nothing is a SUCCESS. There is no --force

REPAIR — drives the engine against a store on THIS machine; there is no --addr:
  repair       REBUILD one series' index from its parts — the repair the store's own
               `manifest … is missing` error names. Selects ONE series EXACTLY
               (--symbol or --group is REQUIRED, and --interval too on `bar`), because
               a series whose base manifest was deleted appears in no listing, so a
               wildcard could not reach the very failure this fixes. REHEARSES BY
               DEFAULT: it prints what the rebuild would recover and what it would
               LOSE, writes nothing, takes no lock, exits 0. --yes performs it;
               --dry-run wins over --yes. A rebuild that recovers the index but not the
               idempotency log exits NON-ZERO and says what to do. It REFUSES while
               another writer holds the series lock, and never waits for one

options:
  --source SRC    fetch: WHERE the rows come from. Omit it for a VENUE (the default, and the
                  only source that takes a SPEC and a window), or name `starter` / `demo`,
                  each a fixed span that takes neither. Refused on every other verb by
                  name: they work on the store that is already there
  --days N        fetch: a window counting back from now. Refused on `export`, by name:
                  a day count back from NOW bounds a fetch, and an export slices what
                  the store already holds
  --from LABEL    fetch/export/universe: window start — epoch-ms, or a date. On `fetch`
                  it needs a matching --to; on a LOCAL `export` and on `universe` it
                  stands alone; on a REMOTE `export` (--addr) BOTH are required, because
                  a windowed walk has to know where the first step begins and where to
                  stop. ⚠ `universe` and a remote `export` parse it HERE (the comparison
                  and the walk happen in this process), so an unreadable label is a
                  usage error rather than the engine's
  --to LABEL      fetch/export/universe: window end, same spellings and the same
                  asymmetries. On `universe` both bounds are INCLUSIVE, and an omitted
                  one takes the store's own endpoint rather than the wall clock
  --out FILE      export: the file to write — Parquet on the local route, the rows in
                  --format on the remote one. REQUIRED there, refused elsewhere
  --window SPAN   export --addr: the wall-clock width of one step of the walk (4h, 1d,
                  7d). Default 30d for bars, 1d for the tick lanes. Each step is ONE
                  request answering in ONE frame, so lower it when a window overruns the
                  64 MiB frame cap and raise it for fewer round trips. Refused
                  everywhere else, including a local export, which walks nothing
  --store DIR     the WRITE half, rm and repair: the hist-store root to act on
  --engine PATH   the WRITE half, rm and repair: the standalone engine to run, instead
                  of searching <project>/bin, this executable's directory, and PATH
  --addr H:P      fetch, every READ verb, rm and export: the datahub to ask (default
                  127.0.0.1:7878).
                  It binds localhost, so reach a remote one over `ssh -L 7878:localhost:7878`.
                  ⚠ On `rm` and `export` it is the REMOTE ROUTE — a different grammar,
                  not a different address — and naming it there excludes --store/--engine.
                  On `rm` it is served only by a datahub that holds node keys: a key-less
                  one serves no delete verb at all. REFUSED
                  on `repair`, by name: a datahub can only reach series it ENUMERATED
  --kind K        ls/gaps/health/universe: keep series whose kind contains K
                  (bar/quote/trade/book/depth). On `universe` it narrows WHICH universe
                  — `--kind bar` is the set a bar-driven profile actually reads. Refused
                  on `coverage`, whose row IS the join across kinds.
                  rm/repair: the EXACT kind (required)
                  export --addr: the EXACT row shape — bar | quote | trade, the three
                  the wire can read. Refused on a LOCAL export, whose engine writes bars
  --venue V       every READ verb: keep rows whose venue contains V. rm/repair: the
                  EXACT venue (required)
  --symbol S      rm: the exact symbol of a PER-SYMBOL series, omit to wildcard.
                  repair: the same, but REQUIRED (--group is its alternative)
  --group G       rm/repair: the exact group of a GROUPED series (which has no symbol at
                  all). An alternative to --symbol, never a pair
  --interval I    rm: the exact bar interval, omit to wildcard. repair: the same, but
                  REQUIRED on `bar`. Refused with --group on both
  --produced-by P rm: the commit-key PREFIX every key of every matched series must
                  carry. REQUIRED whenever the selector can match more than one series.
                  ⚠ A repo-relative PRODUCER PATH resolves to its prefix on the LOCAL
                  route (--store) only — the datahub resolves nothing, so a path is
                  refused by name under --addr rather than asserted literally
  --dry-run       rm/repair: print the plan and stop. Wins over --yes. On `repair` it
                  spells the DEFAULT — that verb rehearses unless told otherwise
  --yes           rm: the non-interactive confirmation. Without it and without a
                  terminal, the run is REFUSED — never read from a pipe. repair: what
                  turns the rehearsal into a write; without it nothing is written and
                  the run says so and exits 0
  --name N        every READ verb: keep rows whose NAME contains N — the symbol of a
                  per-symbol series, or the GROUP of a grouped one (a grouped series
                  has no symbol at all, which is why this is not called --symbol)
  --class         ls: also show the ASSET CLASS each instrument's venue recorded in
                  the store's kind=properties tape — the LATEST one on record. One
                  extra round trip per distinct (venue, symbol), so filter first. The
                  cell says which kind of answer it is: the class word itself, or
                  `unclassified` (a grid was recorded and it named no class — the venue
                  producer is not wired), `no-properties` (nothing recorded for this
                  instrument at all), `(group)` (a grouped series' name is a GROUP, not
                  a symbol, so nothing was asked) or `(error)`, whose reason is printed
                  under the row
  --partial-only  coverage: keep only instruments that have a day some recorded kind
                  covers and another does not
  --require-days N
                  gate: the recorded SPAN each selected series must reach, in whole
                  days. REQUIRED there — a gate with no criterion exits 0 having checked
                  nothing, which is the one answer a CI step must never get. ⚠ It is the
                  SPAN, not the days actually in it: a year-wide series missing three
                  weeks in the middle passes this and fails --max-gap
  --max-gap D     gate: the largest HOLE each selected series may carry — a duration in
                  this workspace's own grammar (4h | 1d | 2w). ⚠ A hole here is a WHOLE
                  UTC DAY (the store derives them from the date= partition set), so a
                  tolerance under 24h means `no missing day at all` and the verdict says
                  so. Omit it and the holes are NOT checked, which the verdict also says
  --require-kind K
                  gate: a kind the store must hold for this spec, repeatable. Defaults
                  to `bar`. It declares what the gate is ABOUT: a kind named here is
                  judged for days and gaps, one that is not is evidence for nothing but
                  the listing — so a one-row properties grid cannot redden an
                  instrument. A required kind the store lacks is a BREACH, never a
                  `nothing was evaluated`: you said it was required
  --limit N       get: how many rows to PRINT, at most 1000 — the default IS that ceiling,
                  and a larger --limit is refused rather than clamped, because a silent
                  clamp would leave you believing you had received what you asked for.
                  Hitting it is reported with the exact number of rows withheld. Refused
                  on every other verb by name: nothing else here emits rows
  --format F      HOW the answer is rendered: `table` (the default) or `json`. The uniform
                  output axis, on every verb. `--json` is its shorthand and the two are
                  refused together only when they DISAGREE. `jsonl` is `get`'s third form
                  — one JSON object per row, and stdout carries nothing else under it; on
                  the catalog verbs it is refused by name, because a catalog is not rows.
                  ⚠ On `export` this flag names a DIFFERENT AXIS — the FILE that --out
                  receives, which is section 7's own grammar: `parquet` on the local
                  route, `jsonl`|`csv` on the remote one (--addr), and a terminal
                  rendering (`table`/`json`) is refused by name there with that
                  correction. The report `export` prints about what it wrote is `--json`,
                  which is unchanged
  --json          shorthand for --format json: one JSON object on stdout describing the run. For the WRITE half
                  (fetch/export): what was asked for, which
                  engine ran, and its own report lines verbatim, with that report moved
                  to stderr so stdout is the document and nothing else. For get: the
                  request, the resolved window, the BARS, and `returned` beside `shown`
                  so a consumer can tell a complete answer from one the ceiling cut. For
                  ls/gaps/coverage: the rows, carrying each series' raw symbol AND group
                  rather than this side's rendering of them, plus — under --class —
                  an asset_class_status naming WHICH answer each row got, so an
                  absent class can never be read as a present-but-empty one. For
                  health/universe: every row INCLUDING the healthy and absent ones,
                  each carrying the numbers its verdict was derived FROM, so a consumer
                  can re-derive it rather than trust it. For gate: the same verdict the
                  table renders — every criterion with its verdict, plus each selected
                  series' own numbers, so a consumer re-derives the judgement instead of
                  trusting it. ⚠ It is emitted on a BREACH too, unlike every other
                  document here: a breach means the command WORKED, so the verdict is
                  the answer rather than a failure. For rm: the plan, the
                  outcome, and the provenance refusal when there is one. For repair:
                  the plan with every RebuildReport count, the LOSSY verdict, and
                  whether anything was written
  -h, --help      this message";

/// Which subcommand ran.
///
/// Adding one is FIVE edits, and they are listed because the last two are the ones a compiler does
/// not ask for: an arm here, a row in [`SUBCOMMANDS`], an arm in [`Sub::as_str`], an arm in
/// [`execute`] — and a row in [`USAGE`], which nothing forces and which
/// `the_usage_names_every_subcommand_and_flag_this_parser_accepts` is the only thing standing
/// between an operator and a verb they cannot discover. A READ subcommand owes three answers as
/// well: [`Sub::is_read`], [`Sub::refuses_a_window`] and [`Sub::takes_a_spec`].
///
/// ⚠ The two halves the module doc opens with are this enum's two halves: [`Sub::is_read`] is the
/// split, and it is what decides which flags a given line may carry. Read it as the answer to
/// "does this subcommand talk to a server or spawn a child", because that is the only question the
/// flag refusals below ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    /// `fetch SPEC` — real public bars, over a window.
    Fetch,
    /// `export SPEC --out FILE` — one series OUT of the store, as a standalone Parquet file.
    Export,
    /// `get SPEC (--days N | --from/--to) [--limit N]` — BOUNDED ROWS TO STDOUT, the surface
    /// design's §8.2. The only verb of this plane whose answer is a PRICE: every sibling here
    /// renders a fact ABOUT a store — its catalog, its holes, its readiness — and until this one
    /// there was no way to get rows out of a REMOTE store at all, in any quantity or format.
    ///
    /// ⚠ It is a READ verb that REQUIRES a window, which makes it the one exception to both halves
    /// of [`Sub::refuses_a_window`]'s rule — see that predicate. [`get`]'s module doc carries §8.2's
    /// two rules, the residual between them (the window bounds the WIRE, the limit bounds STDOUT),
    /// and why this verb reads bars and takes no `--kind`.
    Get,
    /// `list` — every stored series with its coverage, from a datahub's `inventory()`.
    List,
    /// `gaps` — the same enumeration as `ls`, answering the OTHER question: what is MISSING inside
    /// each matched series' recorded span. One `inventory()` round trip, then one `series_gaps`
    /// probe per MATCHED series.
    ///
    /// ⚠ **This was a FLAG on `ls` and the promotion keeps the whole of the argument that made it
    /// one.** That argument was never "gaps is a lesser question" — it was that a gap query names
    /// ONE series, a series is four dimensions with a grouped/per-symbol alternative inside them,
    /// and a verb that built that identity out of flags would have to GUESS whether the operator
    /// meant a symbol or a group and hand the server an id matching nothing when it guessed wrong.
    /// That cost is still refused: this verb takes the same SUBSTRING FILTERS `ls` takes, the ids
    /// come back from `inventory()`, and the ones the filter selected are handed straight back to
    /// `series_gaps` — so this side still never CONSTRUCTS a `SeriesId`. What changed is only where
    /// the question is spelled, which the surface design's §7 tree rules: a verb per question.
    ///
    /// ⚠ **It renders every matched series, clean ones included.** Dropping the clean rows would
    /// make an empty output mean two different things — "nothing matched your filter" and
    /// "everything you matched is whole" — and those are opposite answers. [`empty_note`] exists
    /// one level up for exactly that reason.
    Gaps,
    /// `coverage` — the cross-kind report, from a datahub's `coverage_report()`.
    Coverage,
    /// `tape-health` — the series whose own catalog CONTRADICTS itself, from the same
    /// `inventory()` the listing reads. The inverse question from every sibling: not what is
    /// missing, but whether what is here is even possible. [`tape_health`]'s module doc carries the
    /// argument, and the declared bound — it is not a ROW scan, and why it cannot be one here.
    TapeHealth,
    /// `universe` — point-in-time MEMBERSHIP over a window, the survivorship-bias defence. Also
    /// one `inventory()` round trip: every instrument's first and last RECORDED row, judged against
    /// the store's own span. [`universe`]'s module doc carries what that evidence is and — more
    /// importantly — what it is not.
    Universe,
    /// `gate SPEC --require-days N …` — DATA READINESS AS AN EXIT CODE, the surface design's §8.4.
    /// The only verb of this group whose PRODUCT is the rung rather than the output, and the only
    /// READ verb that takes a spec — a SELECTOR over what `inventory()` returned, never an
    /// identity this side built. [`gate`]'s module doc carries every criterion and the two
    /// measurements that bound them.
    Gate,
    /// `rm` — DELETE series, irreversibly. The one subcommand that reaches EITHER store: `--addr`
    /// is the datahub route, its absence the engine route. See [`Sub::is_read`].
    Rm,
    /// `repair` — rebuild ONE series' manifest from its parts, the repair
    /// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `read_manifest` names in its own error
    /// text. ENGINE-ONLY: `--addr` is refused by name, and [`refuse_the_remote_route_on_repair`]
    /// carries the three-part argument for why the datahub serves no such verb.
    ///
    /// It shares `rm`'s SELECTOR flags (`--kind`/`--venue`/`--symbol`/`--group`/`--interval`) and
    /// its `--dry-run`/`--yes` pair, and it is deliberately not a sibling of `rm` in anything else:
    /// it wildcards NOTHING, it rehearses by DEFAULT, and it has no `--produced-by` because it
    /// asserts no provenance — a rebuild reads what is on disk and touches no row.
    Repair,
}

/// Every subcommand, in the order [`USAGE`] lists them.
///
/// ⚠ It exists so the "a subcommand is required (…)" refusal is DERIVED rather than typed. The
/// hand-written copy it replaced said `(fetch | seed-demo | list | coverage)` and omitted `rm` —
/// a subcommand that had shipped in #1688, months earlier. The one message whose whole job is to
/// name the roster named it short, and it named it short on the verb that DELETES. Two more arms
/// were landing anyway, which is exactly when a list like that goes wrong a second time.
///
/// `all_subcommands_are_reachable_by_the_name_they_advertise` holds it and [`parse`]'s match in
/// agreement, both directions.
/// WHERE a `hist fetch` gets its rows — the surface design's §6 coordinate that today is spelled as
/// three different VERBS.
///
/// ⚠ **The three are not three commands, they are one command and one axis**, and collapsing them is
/// what makes §1's constraint 4 — *"it must work with NO venue"* — expressible as a VALUE rather than
/// as a special case. A box that is geoblocked, corporate-firewalled, credential-less or air-gapped
/// types `--source starter` instead of finding a differently-named verb.
///
/// ⚠ **The axis is not the transport.** `Venue` asks a DATAHUB (`Request::Backfill`); `Starter` and
/// `Demo` spawn the standalone engine. §2 records that this plane has three transport mechanisms and
/// no rule that predicts which verb uses which — this enum is where that stops being true, because
/// the transport becomes a property of the SOURCE rather than of the verb name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The venue's own public API, reached through the datahub's `Backfill` dispatch. The DEFAULT,
    /// and the only source that takes a SPEC and a window.
    Venue,
    /// The published starter bundle on the public mirror — plain HTTPS, no credentials, no venue.
    Starter,
    /// The synthetic generator. No network at all.
    Demo,
}

impl Source {
    /// The ENGINE's own verb for this source, or `None` when the source asks a datahub instead.
    ///
    /// ⚠ **The engine's CLI does NOT collapse with ours, and this function is the seam.** That
    /// binary has its own surface and its own compatibility story, so it still takes `fetch-starter`
    /// and `seed-demo` as VERBS. A sweep that renamed them there would have broken the spawn while
    /// every unit test passed — which is not hypothetical: a test caught exactly that during the
    /// group split, twice.
    ///
    /// Same shape as the venue bridges: one spelling above, the wire's own spelling at the boundary.
    fn engine_verb(self) -> Option<&'static str> {
        match self {
            Source::Venue => None,
            Source::Starter => Some("fetch-starter"),
            Source::Demo => Some("seed-demo"),
        }
    }
}

/// The sources §9 declares, and what each one is REACHABLE by today.
///
/// ⚠ Three work and six do not, and the six are refused BY NAME rather than as "unknown source".
/// An operator who types `--source tardis` has read the design; telling them the value is unknown
/// would be false, and would send them to check their spelling instead of the phase plan. (It read
/// "the same choice the GROUP LAYER makes for `data realtime`", and that stopped being true when
/// that group shipped its verbs; it then read "the same choice [`realtime`]'s own parser makes for
/// `record`", and THAT stopped being true on 2026-09-22 when `record` shipped as a sub-group. So
/// the rule is stated on its own terms here rather than by pointing at a sibling that keeps
/// outgrowing it.)
///
/// Each unbuilt row carries WHY, because the six are not blocked on one thing: `vike` needs a
/// datahub dispatch arm, `tardis`/`databento` are feature-gated AND keyed, and `eod`/`pmxt` are not
/// gated at all but reach no CLI verb.
///
/// ⚠ The `recorder` row said "a TOML profile on the server, not a fetch" and BOTH halves of that
/// are now wrong: the subscription set is settings-DB ROWS, and `data realtime record` edits them.
/// What survives is that it is still not a `--source`, and the row now says which it is — an
/// operator who types `--source recorder` wants the verb, not a phase number.
const UNBUILT_SOURCES: &[(&str, &str)] = &[
    ("vike", "data.vike.io — needs a datahub dispatch arm per lane (P4)"),
    ("tardis", "the paid crypto L2 archive — feature-gated at module AND bin, and keyed (P4)"),
    ("databento", "the paid market-data vendor — feature-gated at module AND bin, and keyed (P4)"),
    ("eod", "the daily-bar vendor — compiled into every vike-backfill build, but no CLI verb (P4)"),
    ("pmxt", "the Polymarket archive lane — same shape as eod (P4)"),
    (
        "recorder",
        "this box's own live tape, which is not a FETCH at all: the recording daemon writes it \
         continuously from what it is subscribed to. `vike-cli data realtime record` is the verb \
         group that edits those subscriptions, and `data hist ls` is what is already on disk",
    ),
];

/// Parse a `--source` value into the axis, or refuse it with the reason that fits.
///
/// ⚠ An unrecognised value is treated as a VENUE rather than refused, and that is deliberate: the
/// reachable venue set is a property of a remote process this crate cannot see at parse time, which
/// is the rule `fetch`'s spec check already follows. Refusing here would put a venue roster in
/// `vike-cli` — the thing §7.1 says this grammar must not do.
fn parse_source(value: &str) -> Result<Source, String> {
    match value {
        "starter" => Ok(Source::Starter),
        "demo" => Ok(Source::Demo),
        "" => Err("--source was given an EMPTY value. Omit the flag to use a venue, or name \
                   `starter` / `demo`."
            .to_string()),
        other => {
            if let Some((_, why)) = UNBUILT_SOURCES.iter().find(|(name, _)| *name == other) {
                return Err(format!(
                    "`--source {other}` is designed but not built yet: {why}. Built today: a \
                     venue (the default), `starter`, `demo`. See \
                     docs/superpowers/specs/2026-09-20-cli-data-surface-design.md §9."
                ));
            }
            Ok(Source::Venue)
        }
    }
}

/// HOW a verb renders its answer — the surface design's uniform-output axis (section 7's tree
/// spells `--format ...` on `get`, `ls` and `export`; section 8.2 calls this "where the uniform
/// output door starts").
///
/// ⚠ **`--json` is NOT retired, and that is a deliberate exception to this branch's own habit.**
/// Every other pre-group spelling is refused by name ([`RETIRED_SPELLINGS`]), because each was a
/// spelling of something this plane alone owns. `--json` is not: it is the WORKSPACE convention,
/// carried by most of this binary's verbs, and retiring it here would make `data` the one command
/// that spells the common thing differently. So it stays, as the SHORTHAND for `--format json`,
/// and [`parse`] refuses the one line where the two can disagree.
///
/// ⚠ No roster is written down here, and no command that would print one. A count would rot — this
/// file has watched that happen — and `crates/vike-ops/tests/unrun_command_gate.rs` requires a
/// documented command's claim to be CHECKED or declared unverifiable, which is a row this fact does
/// not earn.
///
/// ⚠ **The door is the REFUSALS as much as the two values — and they are no longer ONE KIND.**
/// `csv` and `parquet` are named in the design and written by nothing in this workspace, so each
/// is refused BY NAME with what it is waiting on — the same choice [`UNBUILT_SOURCES`] makes, for
/// the same reason: an operator who typed one has read the design, and "unknown format" would send
/// them to check their spelling instead of the phase plan.
///
/// ⚠ **`jsonl` is the OTHER kind, and this paragraph counted it with the first until [`get`]
/// shipped.** It is BUILT — [`ROW_VERB`] serves it — so [`parse_format`] refuses it here with the
/// VERB it works on rather than with a phase to wait for, and it is not in [`UNBUILT_FORMATS`]. A
/// reader who trusted the old sentence would size a `jsonl` request as unbuildable, or add a row
/// back to that roster on the strength of a doc; the roster and that function's `"jsonl"` arm are
/// the authority, and this paragraph is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// The human rendering: aligned columns, a summary line, and the disclosures. The DEFAULT.
    Table,
    /// One JSON document on stdout. What `--json` has always meant.
    Json,
}

/// **THE ROSTER IS EMPTY, and that is the whole entry.**
///
/// ⚠ **It held three rows, then two, and now none — and the shape of each departure is the reason
/// this const survives its own emptying.** `jsonl` left when [`get`] shipped; `csv` left when
/// [`export`]'s remote route did. Both had read *"designed but not built yet"* and both became
/// false the day a verb wrote one, so each is now refused by an arm of its own naming the VERB it
/// works on rather than a phase to wait for. `parquet` left for the OTHER reason and is the
/// instructive one: it was never unbuilt at all — `data hist export --out FILE` has written
/// Parquet since before this table existed, and the row's own text SAID so while filing it under
/// "nothing writes one".
///
/// Kept rather than deleted because the SHAPE is what a future `--format` value needs: a value
/// this plane names and nothing writes goes here, with what it is waiting on, so an operator who
/// typed it meets the phase plan instead of "unknown format". An empty roster is a CLAIM — that
/// today there is no such value — and `the_format_axis_carries_json_and_refuses_the_other_verbs_formats_by_name`
/// asserts it, because a LOOP over an empty roster asserts nothing at all: that test used to run
/// three assertions per row against a table with no rows.
///
/// ⚠ **This once said "all three are ROW formats … none of the verbs THIS axis reaches emits
/// rows", and both halves have now been falsified.** The surviving claim is narrow and is the only
/// one worth carrying: the verbs that reach THIS function emit a CATALOG
/// (`ls`/`gaps`/`coverage`/`health`/`universe`, plus `catalog` and `source`) or a REPORT
/// (`fetch`/`rm`/`repair`), and `export` no longer reaches it at all — it reads
/// [`export::parse_wire`], a different AXIS. None of those emits a row to stdout, which is why the
/// refusals below are still right where they are.
const UNBUILT_FORMATS: &[(&str, &str)] = &[];

/// **The verb that writes ROWS TO A FILE — ONE spelling, for [`ROW_VERB`]'s reason.**
///
/// `csv` and `parquet` are both refused by [`parse_format`] with this verb in the sentence, and
/// `get`'s own `UNSERVED_RENDERS` names it too. Three copies of a verb name is exactly the drift
/// [`ROW_VERB`] exists to have prevented once already.
const FILE_VERB: &str = "`vike-cli data hist export --out FILE`";

/// **The verb that emits ROWS to stdout — ONE spelling, rendered by every refusal that names it.**
///
/// ⚠ **The two copies of this fact had drifted, which is why it is a const.** This module's roster
/// said `(P2)` three times and [`realtime`]'s `unbuilt_renders` said `(P4)`, about the same verb,
/// on the same plane, in the same session: `data hist ls --format csv` answered "same verb, same
/// phase (P2)" and `data realtime watch … --format csv` answered "`data hist get` (P4)". Both
/// refusals render this const, so the two planes cannot answer differently again.
///
/// ⚠ **The PHASE MARKER is gone from the value, because the verb SHIPPED.** It read
/// `` `data hist get` (P4) `` while §11's phase table was the whole of what anyone could say about
/// it. [`get`] exists now, so a refusal calling it a phase would send an operator to a plan
/// instead of to a working command line — the same defect the `(P2)` spelling had, wearing the
/// other direction.
const ROW_VERB: &str = "`vike-cli data hist get`";

/// Parse a `--format` value into the axis, or refuse it with the reason that fits.
///
/// ⚠ Unlike [`parse_source`], an unrecognised value here IS refused. A source names a remote
/// process's capability that this crate cannot see at parse time; a format names THIS side's own
/// renderer, and the roster of those is closed and local.
///
/// ⚠ **This is the CATALOG verbs' parser and no longer the whole `hist` group's.** [`Sub::Get`]
/// reaches [`get::parse_render`] instead, which admits `jsonl` — the same split
/// [`realtime`] already makes for `watch`, and for its reason: widening THIS function would make
/// `data catalog ls --format jsonl` valid for a verb that emits no rows.
fn parse_format(value: &str) -> Result<Format, String> {
    match value {
        "table" => Ok(Format::Table),
        "json" => Ok(Format::Json),
        // ⚠ BUILT, and refused HERE anyway — so the message names where it works rather than
        // calling it unbuilt. That distinction is the whole reason this arm is not a roster row.
        "jsonl" => Err(format!(
            "`--format jsonl` is ONE JSON OBJECT PER ROW, and this verb answers with a CATALOG — \
             one line per series, which is what `table` and `json` are for. {ROW_VERB} is the \
             verb that emits rows, and it serves `jsonl` today."
        )),
        // ⚠ BUILT, and refused HERE for the same reason `jsonl` is — a FILE format asked of a verb
        // that prints. It left [`UNBUILT_FORMATS`] when `export --addr --format csv` shipped; the
        // message names the verb that writes one rather than a phase to wait for.
        "csv" | "parquet" => Err(format!(
            "`--format {value}` is a FILE format, and this verb PRINTS — it renders a catalog to \
             a terminal or a pipe, which is what `table` and `json` are for. {FILE_VERB} writes \
             both: `parquet` from a store on this machine, `csv` from a datahub's over --addr. \
             For rows on stdout, {ROW_VERB} serves `jsonl`."
        )),
        "" => {
            Err("--format was given an EMPTY value. Name `table` (the default) or `json`."
                .to_string())
        }
        other => {
            // ⚠ The roster is EMPTY today — see [`UNBUILT_FORMATS`] for why the lookup stays. A
            // value added there is refused with its own reason before the generic arm below is
            // reached, which is what makes adding one a one-line change.
            if let Some((_, why)) = UNBUILT_FORMATS.iter().find(|(name, _)| *name == other) {
                return Err(format!(
                    "`--format {other}` is designed but not built yet: {why} — and the verb that \
                     emits ROWS at all is {ROW_VERB}, which serves `table`, `json` and `jsonl`. \
                     Built here: `table` (the default), `json`. See \
                     docs/superpowers/specs/2026-09-20-cli-data-surface-design.md section 7."
                ));
            }
            Err(format!(
                "unknown `--format {other}` (table | json). `csv`/`parquet` are FILE formats and \
                 belong to {FILE_VERB}; `jsonl` belongs to {ROW_VERB} — see the surface design's \
                 section 7"
            ))
        }
    }
}

/// The FLAT spellings that shipped before the group split, and what each one is now.
///
/// ⚠ Every verb of this plane used to sit directly under `data`. The surface design's §7 puts them
/// in groups, which is a BREAKING change to a shipped CLI — so each old spelling is refused BY NAME
/// with its replacement rather than removed. A deprecation that keeps working is one nobody
/// migrates off; a deprecation that fails without naming its replacement is one that costs a
/// support round trip.
///
/// ⚠ Two of these are RENAMES as well as moves — `list` -> `ls` and `tape-health` -> `health` — and
/// `data hist list` is refused separately with its own message, because a reader who typed the
/// group correctly and the verb by its old name should not be handed the whole roster to diff.
///
/// ⚠ `fetch-starter` and `seed-demo` point at themselves under `hist` rather than at
/// `fetch --source starter`/`--source demo`. The `--source` axis is the NEXT step of this phase and
/// does not exist yet; a refusal that names a spelling which then errors is worse than the one it
/// replaced.
const RETIRED_SPELLINGS: &[(&str, &str)] = &[
    ("fetch", "data hist fetch"),
    ("fetch-starter", "data hist fetch --source starter"),
    ("seed-demo", "data hist fetch --source demo"),
    ("export", "data hist export"),
    ("list", "data hist ls"),
    ("coverage", "data hist coverage"),
    ("tape-health", "data hist health"),
    ("universe", "data hist universe"),
    ("rm", "data hist rm"),
    ("repair", "data hist repair"),
];

const SUBCOMMANDS: &[Sub] = &[
    Sub::Fetch,
    Sub::Export,
    Sub::Get,
    Sub::List,
    Sub::Gaps,
    Sub::Coverage,
    Sub::TapeHealth,
    Sub::Universe,
    Sub::Gate,
    Sub::Rm,
    Sub::Repair,
];

impl Sub {
    /// The name the operator typed, which is also what every refusal message names it by.
    fn as_str(self) -> &'static str {
        match self {
            Sub::Fetch => "fetch",
            Sub::Export => "export",
            Sub::Get => "get",
            Sub::List => "ls",
            Sub::Gaps => "gaps",
            Sub::Coverage => "coverage",
            Sub::TapeHealth => "health",
            Sub::Universe => "universe",
            Sub::Gate => "gate",
            Sub::Rm => "rm",
            Sub::Repair => "repair",
        }
    }

    /// `true` for the subcommands that ONLY ask a datahub, `false` for the ones that spawn the
    /// engine.
    ///
    /// ⚠ `rm` is `false` here and is NOT purely an engine verb: this predicate answers "may this
    /// subcommand carry a store-side flag", which for `rm` is yes, and the `--addr`-vs-`--store`
    /// contradiction is checked separately in [`parse`]. Widening this to a three-way enum was
    /// tried and abandoned — every existing caller asks the binary question, and a third state
    /// would have made two of them silently wrong for the new arm.
    fn is_read(self) -> bool {
        matches!(
            self,
            Sub::Get
                | Sub::List
                | Sub::Gaps
                | Sub::Coverage
                | Sub::TapeHealth
                | Sub::Universe
                | Sub::Gate
        )
    }

    /// `true` for the read subcommands whose answer is a WHOLE-SERIES fold of a manifest, and which
    /// therefore refuse a time bound.
    ///
    /// ⚠ This predicate exists because [`Sub::Universe`] broke the rule the read half used to hold
    /// unanimously. `ls`, `gaps`, `coverage` and `health` each answer about a series' entire
    /// recorded span — `--from`/`--to` could only narrow the RENDERING, never the question, so a
    /// bound that appeared to have worked would be the worst of both. `universe`'s question IS a
    /// window ("what was in it between these dates"), so the same two flags are load-bearing there.
    /// One predicate rather than an inline `matches!` at the refusal site, so a future read verb
    /// has to answer this question rather than inherit whichever side it was written next to.
    ///
    /// ⚠ [`Sub::Gate`] is on the REFUSING side, which is worth stating because it takes a spec and
    /// therefore looks like the window-shaped verbs: it judges each selected series' whole
    /// recorded span, so a bound could only narrow what was rendered, never what was asserted —
    /// and a gate whose subject a flag had quietly narrowed is a green over less than it claims.
    ///
    /// ⚠ **[`Sub::Get`] is the one read verb that REQUIRES a window rather than merely accepting
    /// one**, which is why this predicate could not have been widened into "does a bound mean
    /// anything here". `universe` takes a window and defaults to the store's own span; `get`
    /// REFUSES the line that names none, because §8.2 makes the bound the cost guard rather than a
    /// narrowing. The requirement therefore lives in [`get::parse_window`], where the message can
    /// say what it is for, and this predicate only has to keep `get` off the refusing side.
    fn refuses_a_window(self) -> bool {
        matches!(self, Sub::List | Sub::Gaps | Sub::Coverage | Sub::TapeHealth | Sub::Gate)
    }

    /// `true` for the subcommands that take the POSITIONAL spec, whichever half they belong to.
    ///
    /// ⚠ This predicate exists because [`Sub::Gate`] broke the rule the read half used to hold
    /// unanimously — *a stored series is four dimensions, which no colon-string can spell, so no
    /// read verb takes one*. That argument is about naming an IDENTITY, and it survives: `gate`'s
    /// spec SELECTS among the ids `inventory()` returned and builds none, which is exactly what
    /// `gaps` does with its substring filters one degree looser. So the refusal sites ask this
    /// question rather than `is_read`, and a future verb has to answer it rather than inherit
    /// whichever side it was written next to. Two grammars sit behind it — [`check_spec`]'s three
    /// mandatory parts for `fetch`/`export`/`get`, [`gate::parse_spec`]'s §7.1 selector — and each
    /// says at its own site why one parser could not serve both. ⚠ `get` is on the THREE-PART side
    /// though it is a read verb, and [`get::parse_spec`] argues why: its spec is an ADDRESS handed
    /// to `load_bars_ms`, not a selector over an enumeration.
    fn takes_a_spec(self) -> bool {
        matches!(self, Sub::Fetch | Sub::Export | Sub::Get | Sub::Gate)
    }
}

/// The client-side row filter the read verbs apply to what the server already sent.
///
/// Case-insensitive SUBSTRING on each dimension, ANDed, and an absent field matches everything —
/// see the module doc for why this is a browse aid rather than a validated roster lookup. Nothing
/// here reaches the wire: both RPCs answer with the whole catalog and the filter is applied to the
/// answer, so a filter can never make the server do less work (which is exactly why `gaps` is the
/// verb worth pairing one with — THAT does).
#[derive(Debug, Default, PartialEq, Eq)]
struct Filter {
    /// `--kind` — matched against `SeriesId::kind`. `coverage` refuses it (see [`parse`]).
    kind: Option<String>,
    /// `--venue` — matched against the venue slug.
    venue: Option<String>,
    /// `--name` — matched against the series LABEL: the symbol of a per-symbol series, the group of
    /// a grouped one. Never against the raw `symbol`, which is EMPTY for every grouped series.
    name: Option<String>,
}

impl Filter {
    /// `true` when every named dimension matches. `kind` is passed as `None` by the `coverage`
    /// path, whose rows are the join ACROSS kinds and so have no single kind to test.
    fn matches(&self, kind: Option<&str>, venue: &str, name: &str) -> bool {
        fn contains(needle: &Option<String>, haystack: &str) -> bool {
            match needle {
                None => true,
                Some(n) => haystack.to_ascii_lowercase().contains(&n.to_ascii_lowercase()),
            }
        }
        // A row with NO kind dimension passes the kind test unconditionally. That is not a
        // silently-dropped filter: `coverage` is the only caller that passes `None`, and `parse`
        // refuses `--kind` there outright — so a discarded needle cannot reach this arm.
        let kind_ok = match kind {
            None => true,
            Some(k) => contains(&self.kind, k),
        };
        kind_ok && contains(&self.venue, venue) && contains(&self.name, name)
    }

    /// `true` when nothing was filtered — used only to decide whether an EMPTY result reads as
    /// "this store holds nothing" or "your filter matched nothing", which are different problems.
    fn is_empty(&self) -> bool {
        self.kind.is_none() && self.venue.is_none() && self.name.is_none()
    }
}

/// The window a `fetch` covers. Exactly one form, chosen by the operator; there is no default,
/// because "fetch everything" is not a thing any venue serves and a silent default would decide how
/// much of somebody's rate limit to spend.
#[derive(Debug, PartialEq, Eq)]
enum Window {
    /// `--days N`, counting back from now.
    Days(String),
    /// `--from LABEL --to LABEL`, an explicit range.
    Range { from: String, to: String },
}

/// `export`'s bounds — BOTH OPTIONAL, BOTH INDEPENDENT, and that is the one place this verb's
/// grammar deliberately diverges from `fetch`'s [`Window`].
///
/// ⚠ **The divergence is about what the flag decides, not about tidiness.** [`window_from`] refuses
/// `--from` without `--to` and refuses the neither-form, because a FETCH with no window would
/// decide how much of somebody's venue rate limit to spend — there is no meaningful default. An
/// EXPORT bounds a slice that is already on disk: every one of the four combinations is a
/// well-formed request, "the whole series" included, and refusing three of them would be a rule
/// with nothing behind it. So `export` does not go through `window_from` at all, and `--days` — a
/// count back from NOW, which bounds a fetch and says nothing about what a store holds — is
/// refused on it BY NAME rather than quietly accepted into a shape it cannot fill.
#[derive(Debug, Default, PartialEq, Eq)]
struct ExportRange {
    /// `--from LABEL` — epoch-ms or `YYYY-MM-DDTHH`, parsed by the ENGINE (one timestamp parser in
    /// the workspace; see this module's doc on what is validated here and what is not).
    from: Option<String>,
    /// `--to LABEL`, same spellings, and it needs no `from`.
    to: Option<String>,
}

/// The parsed `data` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// WHERE `fetch` gets its rows — the §6 coordinate. `Source::Venue` unless `--source` named
    /// otherwise, and meaningless for every other subcommand (the parser refuses the flag there).
    source: Source,
    /// `VENUE:SYMBOL:INTERVAL`, shape-checked. `None` for every subcommand but `fetch`/`export`.
    ///
    /// ⚠ `gate` takes a positional spec too and does NOT leave it here: it is PARSED into
    /// [`GateArgs::spec`], the way `rm` moves `--kind`/`--venue` out of [`Filter`]. One home per
    /// fact, and a raw string beside a parsed one is how the two come to disagree about a trimmed
    /// part or a stripped `@`.
    spec: Option<String>,
    /// `Some` only for `fetch` (the parser refuses one without, and refuses one anywhere else).
    ///
    /// ⚠ `export` carries NO `Window`, and that is deliberate rather than an omission — see
    /// [`ExportRange`], which is the shape a bound-what-is-already-on-disk range needs.
    window: Option<Window>,
    /// `export`'s two INDEPENDENT bounds. `None` for every other subcommand.
    export_range: Option<ExportRange>,
    /// `universe`'s membership window, ALREADY PARSED to epoch-ms. `None` for every other
    /// subcommand.
    ///
    /// ⚠ A SECOND range field rather than a reuse of [`ExportRange`], and the divergence is the
    /// whole reason: that one carries STRINGS because the engine parses them, while nothing here is
    /// forwarded — the comparison runs in this process, so this side must parse and must refuse an
    /// unreadable bound as a usage error. Sharing one field would have meant one of the two
    /// subcommands holding a shape it cannot use.
    universe_window: Option<universe::MembershipWindow>,
    /// `--out FILE` — `export`'s destination. Required there, refused everywhere else.
    out: Option<String>,
    store: Option<String>,
    engine: Option<String>,
    /// `--addr`, resolved to [`DEFAULT_ADDR`] when absent — and resolved for EVERY subcommand, not
    /// just the read ones. The write half never looks at it (the parser has already refused an
    /// explicit `--addr` there), and carrying one unconditional field is cheaper than a second
    /// `Option` whose `None` would mean two different things.
    addr: String,
    /// The read verbs' client-side row filter. Always present; a defaulted [`Filter`] matches
    /// everything.
    filter: Filter,
    /// Ask `series_gaps` for each MATCHED series after the inventory lands. DERIVED from the verb
    /// — `true` for [`Sub::Gaps`] and false for every other — never from a flag.
    gaps: bool,
    /// `--class`: on `ls`, ask `properties_as_of` for each MATCHED series' instrument — once per
    /// distinct `(venue, symbol)`, see [`execute_list`] — and render the recorded asset class.
    class: bool,
    /// `--partial-only`: on `coverage`, keep only instruments with at least one partial day.
    partial_only: bool,
    /// `--json`: emit this subcommand's document instead of the human rendering. Applies to EVERY
    /// subcommand in [`SUBCOMMANDS`] — the write half because "what was written and where" is the
    /// question a caller driving it has, the read half because the answer IS data and a table is
    /// the lossy form of it, and `rm` because its plan carries the one fact a human reads and a
    /// machine must be able to compare: the store that answered. (⚠ This said "ALL FIVE" and was
    /// two short within a release; the roster is the authority, not a number written here.)
    json: bool,
    /// `rm`'s selector. `None` for every other subcommand — the parser builds it only where it
    /// means something, so no other arm can read a half-filled one.
    rm: Option<RmArgs>,
    /// `repair`'s selector. `None` everywhere else, for [`Args::rm`]'s reason. A SECOND struct
    /// rather than a shared one: the two overlap in five fields and differ in the two that matter
    /// — `repair` has no `produced_by` (it asserts nothing) and cannot wildcard (its `symbol`/
    /// `group` alternative is REQUIRED), and a shared type would make both of those representable.
    repair: Option<RepairArgs>,
    /// `gate`'s spec and its criteria. `None` everywhere else, for [`Args::rm`]'s reason: no other
    /// arm can then read a half-filled one, and no other arm can be handed a `require_days` that
    /// defaulted to something.
    gate: Option<GateArgs>,
    /// `get`'s spec, window, ceiling and rendering. `None` everywhere else, for [`Args::gate`]'s
    /// reason — and one more that is this verb's own: [`Args::json`] is a TWO-state axis and `get`
    /// has THREE renderings, so a `jsonl` run would be indistinguishable from a `table` one if the
    /// answer lived there. [`GetArgs::render`] is the authority for this verb and `json` is its
    /// projection, computed once in [`parse`].
    get: Option<GetArgs>,
    /// `export`'s REMOTE route — its kind, spec, file format, bounds and walk step, all resolved.
    ///
    /// ⚠ **`Some` is what SELECTS the route**, which is why this field exists rather than a `bool`
    /// beside [`Args::export_range`]: the two routes take different grammars (`--kind`/`--window`/
    /// `--format` belong to one, `--engine`/`--store` to the other, and only the remote one
    /// requires BOTH bounds), and a single struct holding both would be one a future arm could
    /// read a half-filled version of — [`Args::rm`]'s reason. [`Args::export_range`] stays the
    /// LOCAL route's, carrying strings because the engine parses them.
    /// ⚠ **The resolved request lives in [`export::Plan`] rather than in a sibling of [`GateArgs`]
    /// here**, which is the one place this verb diverges from the file's habit. That module's
    /// renderers each need four or five of its fields at once, and passing them individually put
    /// its `--json` document at nine parameters — past `clippy::too_many_arguments`, a merge gate.
    /// A type the pure half owns is also what lets the walk and both renderings be tested without
    /// building an [`Args`].
    export: Option<export::Plan>,
    /// `--addr` was given EXPLICITLY. `addr` above is always resolved, so it cannot answer "did the
    /// operator ask for the remote route" — which is the question `rm` and now `export` turn on.
    addr_given: bool,
}

/// `rm`'s own arguments — the selector, the assertion, and the two confirmation flags.
///
/// A struct of its own rather than five more `Option`s on [`Args`], because every field here is
/// meaningless for the other four subcommands and an `Args` that could hold a selector for
/// `seed-demo` is an `Args` some future arm will read one from.
#[derive(Debug, PartialEq, Eq)]
struct RmArgs {
    /// The four identity dimensions. Held as the store's own type so this side never re-implements
    /// what a series IS — ⚠ and it is reached through a DEV-dependency (see the module doc's note
    /// on `SeriesRow`), so it may be NAMED only under `#[cfg(test)]`. Hence the plain fields.
    kind: String,
    venue: String,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    /// `--produced-by`, verbatim as typed.
    ///
    /// ⚠ **A producer PATH resolves to its prefix on BOTH far sides since 2026-09-11, and this CLI
    /// still refuses to SEND one.** This doc said "on the far side" unqualified, which was true of
    /// one of the two: `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` called
    /// `vike_data::store_kind::resolve_produced_by` and
    /// `crates/vike-datahub/src/server.rs`'s `delete_series_verb` called nothing of the kind. The
    /// server now calls it too — through the `vike_datahub_client::proto` re-export, so one
    /// definition answers both ends — but a DEPLOYED datahub may predate that and the protocol
    /// carries no capability string to tell the two apart. See
    /// [`refuse_a_producer_path_on_the_remote_route`], which is now a compatibility guard and
    /// carries what the unresolved spelling used to report instead.
    produced_by: Option<String>,
    dry_run: bool,
    yes: bool,
}

/// `repair`'s own arguments — the identity of ONE series, and the two confirmation flags.
///
/// ⚠ **Every dimension but `interval` is REQUIRED, and that is the whole difference from
/// [`RmArgs`].** `rm` wildcards an omitted dimension because a cleanup is a set; `repair` rebuilds
/// exactly one series' index, so an omitted `--symbol`/`--group` names nothing and is refused. The
/// residual `Option`s here are therefore the store's own ALTERNATIVE (`symbol` xor `group`) and
/// bars' `interval=` segment, never a wildcard.
#[derive(Debug, PartialEq, Eq)]
struct RepairArgs {
    kind: String,
    venue: String,
    /// Exactly one of these two is `Some` — [`parse`] refuses both and refuses neither.
    symbol: Option<String>,
    group: Option<String>,
    /// `Some` for a bar series. Its REQUIREMENT on `bar` is the engine's call, not this crate's —
    /// see [`parse`]'s `Sub::Repair` arm.
    interval: Option<String>,
    /// `--dry-run`: the DEFAULT spelled explicitly, and it WINS over `--yes`.
    dry_run: bool,
    /// `--yes`: perform the rebuild. Without it this verb rehearses — see [`Sub::Repair`].
    yes: bool,
}

/// `gate`'s own arguments — what the gate is ABOUT, and what it ASSERTS.
///
/// A struct of its own for [`RmArgs`]'s reason, and one more that matters here: every field below
/// is already RESOLVED. `require_days` is a number rather than the string an operator typed,
/// `max_gap_ms` is milliseconds rather than `"4h"`, and `kinds` is never empty because [`parse`]
/// folds in [`gate::DEFAULT_KIND`] when nobody named one. So the judging in [`gate`] is a pure fold
/// over plain data with no parse left in it, and no site downstream can re-decide a default.
#[derive(Debug, PartialEq, Eq)]
struct GateArgs {
    /// The SELECTOR the positional spec parsed to — never a `vike_data::SeriesId`; see
    /// [`gate::Spec`].
    spec: gate::Spec,
    /// `--require-days N`, a whole positive count. REQUIRED, because a gate with no criterion
    /// exits 0 having checked nothing.
    require_days: i64,
    /// `--max-gap D` in milliseconds, or `None` when the holes were not asked about — which the
    /// verdict DISCLOSES rather than leaving to be noticed.
    max_gap_ms: Option<i64>,
    /// `--require-kind K`, repeatable, defaulted to `[bar]`. Never empty: it is what the gate is
    /// about, and an empty one would judge nothing while reading like a gate.
    kinds: Vec<String>,
}

/// `get`'s own arguments — WHICH series, over WHAT window, at most HOW MANY rows, rendered HOW.
///
/// A struct of its own for [`GateArgs`]'s reason, and RESOLVED for its reason too: the spec is
/// parsed, the bounds are epoch-ms, the ceiling is folded in and the rendering is decided. So
/// [`execute_get`] is a read and a render with no parse left in it, and no site downstream can
/// re-decide a default.
///
/// ⚠ **The one thing deliberately NOT resolved here is the CLOCK.** `--days N` stays a count
/// because [`parse`] is PURE — it reads no environment and no clock — so
/// [`get::Window::bounds`] takes the instant as a parameter and `execute_get` supplies it. That is
/// the same split `fetch` makes: its `Window` is carried raw and `fetch_window_ms` resolves it at
/// execute time.
#[derive(Debug, PartialEq, Eq)]
struct GetArgs {
    /// The ADDRESS of one bar series — `VENUE:SYMBOL:INTERVAL`, all three parts. Not a selector
    /// over an enumeration the way [`GateArgs::spec`] is: it goes straight to `load_bars_ms`.
    spec: get::Spec,
    /// The REQUIRED bound, in whichever of the two forms was typed. §8.2's first rule.
    window: get::Window,
    /// The effective row ceiling — `--limit` when it lowered [`get::ROW_CEILING`], that constant
    /// otherwise. Never above it: [`get::parse_limit`] refuses rather than clamping.
    limit: usize,
    /// `true` when the ceiling was DEFAULTED rather than named, so the disclosure can say which
    /// one cut the answer. A message that said "--limit 1000" to somebody who typed no `--limit`
    /// would be telling them about a flag they did not use.
    limit_defaulted: bool,
    /// `table` | `json` | `jsonl` — this verb's three, and the authority for how it renders. See
    /// [`Args::get`] for why [`Args::json`] cannot be that authority here.
    render: get::Render,
}

/// Parse `data`'s own argv tail (everything after the verb). PURE — no I/O, no spawn.
///
/// ⚠ Every flag is accepted by the ONE loop below and then refused per-subcommand by
/// [`refuse_foreign_flags`], rather than being routed by a per-subcommand match. That ordering is
/// what lets an inapplicable flag be named in a message that says which subcommand it DOES belong
/// to; an unknown-option error would tell an operator the flag does not exist, which is false and
/// sends them looking in the wrong place.
fn parse(
    mut it: impl Iterator<Item = String>,
    configured_addr: Option<&str>,
) -> Result<Args, String> {
    let Some(first) = it.next() else {
        // ⚠ DERIVED from [`SUBCOMMANDS`], never re-typed. The hand-written copy this replaced
        // omitted `rm` — a subcommand that had shipped months earlier — so the one message whose
        // whole job is to name the roster named it short, on the verb that DELETES.
        return Err(format!(
            "a subcommand is required ({})",
            SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
        ));
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return help_requested();
    }
    // ⚠ THE GROUP LAYER. `data` is a PLANE and its verbs live in groups — the surface design's §7.
    // This function is `hist`'s parser and ONLY `hist`'s: [`run`] routes `catalog`, `realtime` and
    // `source` to their own modules above here, so no other group word reaches this match.
    //
    // ⚠ This carried a refusal arm for `realtime` — the last group that was designed and unbuilt —
    // and it went with that group's verbs, along with the `unbuilt_group_message` it called. A
    // function no caller reaches is `-D dead-code`, and papering over that with an `allow` would
    // have left a second spelling of a sentence nothing produces lying in wait. The history is in
    // `crates/vike-cli/tests/data_cli.rs`'s
    // `a_group_that_answers_never_reads_as_designed_but_unbuilt`, which is what survives of it.
    //
    // The sub-verb is always REQUIRED, as it is on `backtest`: there is no bare `vike-cli data …`.
    let verb = match first.as_str() {
        "hist" => it.next().ok_or_else(|| {
            format!(
                "`data hist` needs a verb ({})",
                SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
            )
        })?,
        // A bare verb is the PRE-GROUP spelling. Refused by NAME with its replacement, never
        // silently accepted: a deprecation that keeps working is one nobody migrates off, and a
        // deprecation that fails without naming its replacement is one that costs a support round
        // trip. See [`RETIRED_SPELLINGS`].
        other => {
            if let Some((_, now)) = RETIRED_SPELLINGS.iter().find(|(was, _)| *was == other) {
                return Err(format!(
                    "`data {other}` moved: it is `{now}` now. Every `data` verb lives in a GROUP \
                     (hist | realtime | catalog | source) — the plane got too wide to be flat."
                ));
            }
            return Err(format!(
                "unknown `data` group '{other}' (hist | realtime | catalog | source)"
            ));
        }
    };
    let sub = match verb.as_str() {
        "fetch" => Sub::Fetch,
        "export" => Sub::Export,
        "get" => Sub::Get,
        "ls" => Sub::List,
        "gaps" => Sub::Gaps,
        "coverage" => Sub::Coverage,
        "health" => Sub::TapeHealth,
        "universe" => Sub::Universe,
        "gate" => Sub::Gate,
        "rm" => Sub::Rm,
        "repair" => Sub::Repair,
        "-h" | "--help" | "help" => return help_requested(),
        // The verbs that were RENAMED or ABSORBED, caught here so the message can name the new
        // spelling rather than listing the roster and leaving the reader to spot the difference.
        //
        // ⚠ `fetch-starter` and `seed-demo` are in this list even though they are also in
        // [`RETIRED_SPELLINGS`], and the duplication is the point: that table catches the FLAT
        // pre-group form (`data seed-demo`), while this arm catches the operator who learned the
        // group split and typed `data hist seed-demo`. Both are plausible, and "unknown verb" is
        // the wrong answer to either.
        "list" => return Err("`data hist list` is `data hist ls` now".to_string()),
        "tape-health" => {
            return Err("`data hist tape-health` is `data hist health` now".to_string());
        }
        "fetch-starter" => {
            return Err(
                "`fetch-starter` is not a verb any more — the SOURCE is an axis on `fetch` now: \
                 `data hist fetch --source starter`"
                    .to_string(),
            );
        }
        "seed-demo" => {
            return Err(
                "`seed-demo` is not a verb any more — the SOURCE is an axis on `fetch` now: \
                 `data hist fetch --source demo`"
                    .to_string(),
            );
        }
        other => return Err(format!("unknown `data hist` verb '{other}'")),
    };

    let mut spec: Option<String> = None;
    let mut source: Option<Source> = None;
    let mut days: Option<String> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut store: Option<String> = None;
    let mut engine: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut filter = Filter::default();
    // The RETIRED flag, tracked only so the refusal below can name what replaced it. The `gaps`
    // the rest of this function reasons about is derived from the VERB, one line down.
    let mut gaps_flag = false;
    let mut class = false;
    let mut partial_only = false;
    let mut json_flag = false;
    // ⚠ `--format` is the ONE valued flag kept RAW through the loop, because WHICH parser reads it
    // is a property of the VERB: [`Sub::Get`] emits rows and reaches [`get::parse_render`], every
    // other verb here emits a catalog or a report and reaches [`parse_format`]. Parsing eagerly
    // would have meant one roster answering for both, which is exactly how
    // `data catalog ls --format jsonl` would have become valid for a verb that emits no rows.
    let mut format_raw: Option<String> = None;
    let mut limit: Option<String> = None;
    // `export --addr`'s walk step, kept RAW for the reason its match arm states.
    let mut window_raw: Option<String> = None;
    let mut symbol: Option<String> = None;
    let mut group: Option<String> = None;
    let mut interval: Option<String> = None;
    let mut produced_by: Option<String> = None;
    let mut out: Option<String> = None;
    let mut dry_run = false;
    let mut yes = false;
    // `gate`'s three. Each is kept RAW here and resolved in the `Sub::Gate` arm, so the refusals
    // below can name a flag that was given without this loop having to know which verb it belongs
    // to — the ordering [`parse`]'s own doc argues for.
    let mut require_days: Option<String> = None;
    let mut max_gap: Option<String> = None;
    let mut require_kinds: Vec<String> = Vec::new();

    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--symbol" => symbol = Some(flags.value(&flag, inline)?),
            "--group" => group = Some(flags.value(&flag, inline)?),
            "--interval" => interval = Some(flags.value(&flag, inline)?),
            "--produced-by" => produced_by = Some(flags.value(&flag, inline)?),
            "--dry-run" => {
                no_value(&flag, inline)?;
                dry_run = true;
            }
            "--yes" => {
                no_value(&flag, inline)?;
                yes = true;
            }
            "--source" => source = Some(parse_source(&flags.value(&flag, inline)?)?),
            "--days" => days = Some(flags.value(&flag, inline)?),
            "--from" => from = Some(flags.value(&flag, inline)?),
            "--to" => to = Some(flags.value(&flag, inline)?),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            "--out" => out = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--kind" => filter.kind = Some(flags.value(&flag, inline)?),
            "--venue" => filter.venue = Some(flags.value(&flag, inline)?),
            "--name" => filter.name = Some(flags.value(&flag, inline)?),
            "--require-days" => require_days = Some(flags.value(&flag, inline)?),
            "--max-gap" => max_gap = Some(flags.value(&flag, inline)?),
            // ⚠ The ONE repeatable flag in this verb. It PUSHES rather than replaces, because its
            // whole job is to declare a SET — "this run needs bars and a trade tape" is one gate,
            // not two, and a last-wins flag would silently assert half of what was typed.
            "--require-kind" => require_kinds.push(flags.value(&flag, inline)?),
            // ⚠ STILL RECOGNISED, and refused below by name rather than dropped from the match.
            // Falling through to `unknown option '--gaps'` would tell an operator the flag does
            // not exist — which sends them to check their spelling instead of to the verb that
            // replaced it. Same rule as [`RETIRED_SPELLINGS`], one layer down.
            "--gaps" => {
                no_value(&flag, inline)?;
                gaps_flag = true;
            }
            "--class" => {
                no_value(&flag, inline)?;
                class = true;
            }
            "--partial-only" => {
                no_value(&flag, inline)?;
                partial_only = true;
            }
            // ⚠ The SHORTHAND, kept because it is the workspace convention - see [`Format`]. It
            // sets the same axis the long flag sets, so nothing downstream reads two fields.
            "--json" => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            "--format" => format_raw = Some(flags.value(&flag, inline)?),
            "--limit" => limit = Some(flags.value(&flag, inline)?),
            // ⚠ Kept RAW here for `--format`'s reason inverted: this one has exactly ONE reader
            // ([`export::parse_window_step`]) but its refusals NAME the kind, which is not resolved
            // until the `Sub::Export` arm. Parsing it in the loop would mean a message that could
            // not say which lane's step was being asked for.
            export::WINDOW_FLAG => window_raw = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return help_requested(),
            // ⚠ Judged on the `--` prefix, the same rule `crate::cmd::args`'s `is_flag_token`
            // spells for every valued flag in this crate: a token beginning with `--` is a FLAG,
            // so an unrecognised one is a usage error rather than something to read as a spec.
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            // The one POSITIONAL in this verb: the spec. Anything after the first is a mistake
            // worth naming — a second bare word is nearly always a shell-quoting accident, and
            // silently ignoring it would fetch a series the operator did not ask for.
            // ⚠ The tail NAMES THE VERB rather than saying "one series per fetch", which is what it
            // said while `fetch`/`export` were the only spec-takers. [`Sub::takes_a_spec`] admits
            // [`Sub::Gate`] now, and that verb fetches nothing — an operator who typed `gate` and
            // was refused in the name of a fetch has to work out which of the two sentences is
            // about them.
            positional => match &spec {
                None => spec = Some(positional.to_string()),
                Some(already) => {
                    return Err(format!(
                        "unexpected extra argument '{positional}' (the spec is already \
                         '{already}'); `{}` takes ONE series",
                        sub.as_str()
                    ));
                }
            },
        }
    }

    // ⚠ `--gaps` is a VERB now, and the refusal is UNIVERSAL rather than per-subcommand — including
    // on `ls`, the one verb that used to take it. Five arms below used to refuse it each with a
    // reason of their own (a coverage row is UTC days, a health finding is presence, a universe row
    // is membership); every one of those reasons survives in this message's second half, and none
    // of them was ever the reason it is refused on `ls`. Keeping five sites would have meant one of
    // them going stale silently the day a sixth verb landed.
    if gaps_flag {
        return Err(format!(
            "`--gaps` is not a flag any more — it is a VERB. `vike-cli data hist gaps` takes the \
             same --kind/--venue/--name filters `ls` takes and answers what is MISSING inside each \
             matched series. You typed it on `{}`, which answers a different question — a gap \
             range is epoch-ms, a coverage day is a UTC-day index, and a health finding is about \
             what is PRESENT",
            sub.as_str()
        ));
    }
    // ONE axis, two spellings, and the CONTRADICTION is refused rather than resolved. `--json` is
    // the shorthand for `--format json` ([`Format`] carries why it survives), so a line carrying
    // both can only disagree by naming some OTHER rendering. Picking a winner there would silently
    // discard half of what the operator typed, which is the rule `--days` vs `--from`/`--to`
    // already follows one arm down.
    //
    // ⚠ It is refused on the RAW value, ABOVE the two parsers, so both of them inherit it and the
    // `table` sentence is spelled once for the whole group — `catalog`'s
    // `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words` holds this literal
    // and that group's equal, word for word.
    //
    // ⚠ **`jsonl` is a SECOND way to disagree and it arrived with `get`.** It gets its own sentence
    // rather than a widened `table` one, because the reason differs and the reason is what an
    // operator acts on: `table` and `json` are two renderings of ONE document, while `jsonl` is not
    // a document at all. Every other value — `csv`, `parquet`, a typo — is refused by the format
    // parser below whether or not `--json` was given, so there is nothing for this to say about it.
    //
    // ⚠ **…and that arm is [`Sub::Get`]'s ALONE, unlike the `table` one above it.** On every other
    // verb `jsonl` is in exactly the class the sentence above describes: [`parse_format`] refuses
    // it there with or without `--json`, naming [`ROW_VERB`] as the verb that serves it. Firing
    // this arm on `ls` replaced that with a message describing GET's document and ending "Pass
    // one" — advice that is FALSE there, because dropping `--json` leaves a `--format jsonl` the
    // verb still refuses. It also split the two groups: `data catalog ls --json --format jsonl`
    // answers with the [`ROW_VERB`] message, because `catalog`'s parser reads `--format` eagerly
    // and its contradiction check never sees a `jsonl` at all. One question, one answer.
    //
    // ⚠ **[`Sub::Export`] is EXEMPT from the `table` arm, and it is the one verb that must be.**
    // On `export` `--format` names the FILE `--out` receives (§7's own grammar), so `--json
    // --format table` is not two spellings of one rendering — it is a terminal flag beside a file
    // flag, and "`--json` IS `--format json` — pass one" would be advice to drop a flag that is
    // not the problem. [`export::parse_wire`] and [`export::refuse_a_wire_on_the_local_route`]
    // give the AXIS correction instead, each on its own route.
    match (format_raw.as_deref(), json_flag) {
        (Some("table"), true) if sub != Sub::Export => {
            return Err("--json and --format table ask for two different renderings. `--json` IS \
                 `--format json` — pass one"
                .to_string());
        }
        (Some("jsonl"), true) if sub == Sub::Get => {
            return Err(
                "--json and --format jsonl are not two spellings of one thing. `--json` IS \
                 `--format json`: ONE document, with the rows in a field beside the counts. \
                 `jsonl` is a SEQUENCE — one object per row and nothing else on stdout, for a \
                 pipeline. Pass one."
                    .to_string(),
            );
        }
        _ => {}
    }
    // WHICH parser reads the value is the VERB's property — see [`format_raw`]'s declaration. Only
    // one of these two is ever `Some`, and each arm's `None` means "this verb does not use that
    // axis" rather than "nothing was given".
    let get_render = match sub {
        Sub::Get => Some(get::render_for(format_raw.as_deref(), json_flag)?),
        _ => None,
    };
    // ⚠ **THREE parsers now, not two, and [`Sub::Export`]'s is the one that reads a different
    // AXIS.** `get`'s reads a terminal rendering and this file's own [`parse_format`] reads a
    // catalog rendering; `export`'s reads the FILE FORMAT `--out` receives. That is §7's grammar
    // rather than a widening: `export SPEC --out FILE [--format parquet|csv|jsonl]` names no
    // terminal form at all. The consequence a reader needs is that `--format json` on `export`
    // used to parse HERE and mean the report document — it is now refused by name with the axis
    // correction, and `--json` (which is what it always was) is untouched.
    let format = match sub {
        Sub::Get | Sub::Export => None,
        _ => format_raw.as_deref().map(parse_format).transpose()?,
    };

    // ⚠ ONE downstream field, deliberately. Sixty-odd sites read [`Args::json`]; a second field
    // saying the same thing in different words is how the two come to disagree. For `get` it is a
    // PROJECTION of [`GetArgs::render`] rather than a second decision — that verb has three
    // renderings and this field has two states, so the projection is computed here, once, and
    // `execute_get` reads the render instead.
    let json = match get_render {
        Some(r) => r == get::Render::Json,
        None => match (format, json_flag) {
            (Some(f), _) => f == Format::Json,
            (None, given) => given,
        },
    };

    // DERIVED from the verb, never from a flag: [`Sub::Gaps`] IS the probe, so nothing downstream
    // has to ask both questions.
    let gaps = sub == Sub::Gaps;

    // `--limit` belongs to `get` and to nothing else, for [`refuse_foreign_flags`]'s reason. It
    // bounds ROWS, and `get` is the only verb of this plane that emits any: a `--limit` on `ls`
    // looks like it would shorten a listing and would in fact be a flag with nothing to bound,
    // which is worse than a flag that does not exist.
    //
    // ⚠ [`ROW_VERB`] is RENDERED, not re-typed. This site hand-typed `vike-cli data hist get SPEC`
    // in the same PR that introduced the const to stop exactly that — and the two spellings had
    // already diverged by a trailing `SPEC` on the day they were written. A third copy is how the
    // `(P2)` vs `(P4)` drift the const exists for comes back.
    if sub != Sub::Get {
        refuse_foreign_flags(
            sub,
            &[("--limit", limit.is_some())],
            &format!(
                "that flag bounds how many ROWS are printed, and the only verb here that emits \
                 rows is {ROW_VERB}. Every other verb answers about a store rather than with its \
                 contents — narrow a LISTING with --kind/--venue/--name instead"
            ),
        )?;
    }

    // `--out` belongs to `export` and to nothing else, for [`refuse_foreign_flags`]'s reason: an
    // operator who typed it on a `fetch` is not looking for "unknown option" — that flag exists,
    // and they want the verb that takes it.
    if sub != Sub::Export {
        refuse_foreign_flags(
            sub,
            &[("--out", out.is_some())],
            "that flag names the FILE an `export` writes, and only `export` writes one",
        )?;
    }

    // `--window` belongs to `export`'s REMOTE route alone, and it is refused in TWO places with
    // TWO different sentences — here for every other verb, and in the `Sub::Export` arm for the
    // local route. One message could not serve both: elsewhere the fact is that nothing walks a
    // wire, while on the local route the walk is the thing that does not happen.
    if sub != Sub::Export {
        refuse_foreign_flags(
            sub,
            &[(export::WINDOW_FLAG, window_raw.is_some())],
            "that flag sets the step of a WINDOWED WALK, and the only verb here that walks one is \
             `vike-cli data hist export SPEC --out FILE --addr H:P`, which splits a bulk range \
             into frame-sized requests. Every other verb answers in one request",
        )?;
    }

    // `gate`'s three CRITERION flags belong to `gate` and to nothing else — refused BY NAME
    // everywhere, in one place. An operator who typed one on `ls` is not looking for "unknown
    // option": they want the verb whose answer is a verdict rather than a table, and the message
    // names it.
    if sub != Sub::Gate {
        refuse_foreign_flags(
            sub,
            &[
                ("--require-days", require_days.is_some()),
                ("--max-gap", max_gap.is_some()),
                ("--require-kind", !require_kinds.is_empty()),
            ],
            "that flag declares a CRITERION, and the only verb here that judges one is \
             `vike-cli data hist gate SPEC --require-days N` — whose product is an exit code a CI \
             step branches on. Every other verb RENDERS a store and leaves the decision to you",
        )?;
    }

    // The SELECTOR flags belong to the two subcommands that name a series by identity — `rm` and
    // `repair` — and to nothing else. Refused BY NAME everywhere else, in one place.
    //
    // ⚠ `repair` joined this set rather than getting refusals of its own, and the naming of the
    // set changed with it: these flags are not "rm's flags", they are how a series is ADDRESSED
    // when the store's own enumeration is not the way in. For `rm` that is because a wildcard is
    // wanted; for `repair` it is because the broken series may not be enumerable at all.
    if !matches!(sub, Sub::Rm | Sub::Repair) {
        refuse_foreign_flags(
            sub,
            &[
                ("--symbol", symbol.is_some()),
                ("--group", group.is_some()),
                ("--interval", interval.is_some()),
                ("--dry-run", dry_run),
                ("--yes", yes),
            ],
            "that flag names a series by IDENTITY and belongs to `rm` or `repair`. To narrow a \
             LISTING use --kind/--venue/--name, which are substring filters over what a datahub \
             already sent",
        )?;
    }
    // `--produced-by` belongs to `rm` ALONE, `repair` included — and the reason is worth the extra
    // refusal rather than a shared row above. It is a provenance ASSERTION over the commit keys of
    // everything a selector matched, and it exists because a DELETE by name is not safe enough. A
    // rebuild asserts nothing and deletes nothing: it re-derives an index from parts it reads, so
    // there is no act for a provenance check to stand in front of. Accepting it here would be a
    // flag that looks like a guard and guards nothing.
    if sub != Sub::Rm {
        refuse_foreign_flags(
            sub,
            &[("--produced-by", produced_by.is_some())],
            "that flag ASSERTS the commit-key provenance of everything about to be DELETED, and \
             only `rm` deletes. A `repair` reads parts and rewrites an index — it removes no row, \
             so there is nothing for a provenance assertion to guard",
        )?;
    }

    // The half-crossing refusals, spelled once for both directions. A read verb may not carry a
    // store-side flag and a write verb may not carry a datahub-side one — see [`Sub::is_read`] and
    // the module doc's opening for why "ignore it" was never an option here.
    if sub.is_read() {
        // The STORE-side flags, refused on every read subcommand for the one reason they all share:
        // this process cannot open a hist store at all.
        refuse_foreign_flags(
            sub,
            &[("--store", store.is_some()), ("--engine", engine.is_some())],
            "that flag names a hist store (or the engine that writes one) on THIS machine, and \
             the read verbs read the store a running vike-datahub already has open — reach it \
             with --addr",
        )?;
        // ⚠ The WINDOW is refused on the read subcommands whose answer is a whole-series fold, and
        // ACCEPTED on `universe`, whose question IS a window — [`Sub::refuses_a_window`] is where
        // that split is decided and argued. `--days` is refused on `universe` too, separately, in
        // its own arm below: it counts back from NOW, which makes a membership window answer a
        // different question every time it is run.
        if sub.refuses_a_window() {
            refuse_foreign_flags(
                sub,
                &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
                "that flag bounds a FETCH window, and this verb folds each series' WHOLE recorded \
                 span — a bound could only narrow the rendering, never the question. \
                 `vike-cli data hist universe --from … --to …` is the read verb whose question IS a \
                 window",
            )?;
        }
        // ⚠ Keyed on [`Sub::takes_a_spec`] rather than on `is_read`, because `gate` is a READ verb
        // that takes one — see that predicate's doc for why the identity argument below survives
        // it unchanged.
        if let Some(extra) = &spec
            && !sub.takes_a_spec()
        {
            return Err(format!(
                "'{extra}': `{}` takes no VENUE:SYMBOL:INTERVAL spec — a stored series is \
                 (kind, venue, symbol-or-group, interval?), which no colon-string can spell. \
                 Narrow the listing with --kind/--venue/--name instead",
                sub.as_str()
            ));
        }
    } else if sub == Sub::Rm {
        // `rm` keeps `--addr` (its REMOTE route) and `--kind`/`--venue` (its SELECTOR, not a
        // substring filter). What it refuses is the read half's browse aids and the fetch half's
        // window, each with the reason it does not apply.
        refuse_foreign_flags(
            sub,
            &[
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING. `rm` selects by EXACT identity — --symbol \
             or --group, never a substring — because a substring match is not a thing to delete by",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
            "that flag bounds a FETCH window. `rm` removes whole series, never a time range — a \
             partial delete is a different feature with a different plan",
        )?;
        // ⚠ The two ROUTES are exclusive, and this is where that is decided. `--addr` names a
        // datahub that already has a store open; `--store`/`--engine` name a store and a binary on
        // THIS machine. Both at once has two readable meanings and no obviously right one, and
        // either choice would silently discard half of what the operator typed — the same rule
        // `window_from` applies to `--days` vs `--from`/`--to`.
        if addr.is_some() && (store.is_some() || engine.is_some()) {
            return Err(
                "--addr and --store/--engine are two DIFFERENT stores: --addr asks a running \
                 vike-datahub about the store THAT process opened, while --store names one on this \
                 machine for the engine to open. Pass one."
                    .to_string(),
            );
        }
        if let Some(extra) = &spec {
            return Err(format!(
                "'{extra}': `rm` takes no VENUE:SYMBOL:INTERVAL spec — a stored series is \
                 (kind, venue, symbol-or-group, interval?), and naming it positionally cannot \
                 express a GROUPED series (whose symbol is empty) or a wildcarded dimension. Use \
                 --kind/--venue/--symbol|--group/--interval"
            ));
        }
    } else if sub == Sub::Repair {
        // `repair` keeps `--kind`/`--venue` (its SELECTOR, not a substring filter) and the
        // store-side flags. It refuses the read half's browse aids and the fetch half's window for
        // `rm`'s reasons, and `--addr` for one of its own.
        refuse_foreign_flags(
            sub,
            &[
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING. `repair` names ONE series by EXACT identity \
             — --symbol or --group, never a substring — because the series it repairs may be one \
             no listing can show you",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
            "that flag bounds a FETCH window. `repair` rebuilds a whole series' index from the \
             parts on disk — there is no time range to rebuild",
        )?;
        if addr.is_some() {
            return Err(refuse_the_remote_route_on_repair());
        }
        if let Some(extra) = &spec {
            return Err(format!(
                "'{extra}': `repair` takes no VENUE:SYMBOL:INTERVAL spec — a stored series is \
                 (kind, venue, symbol-or-group, interval?), which no colon-string can spell (it \
                 carries no kind, and a GROUPED series has no symbol at all). Use \
                 --kind/--venue/--symbol|--group/--interval"
            ));
        }
    } else if sub == Sub::Export {
        // ⚠ **`export` LEFT the blanket write-half refusal below for TWO of its six rows, and only
        // `export` did.** `--addr` is this verb's ROUTE SWITCH now and `--kind` is its remote
        // route's row shape, so refusing them here with "that belongs to the READ half" would be
        // false about the verb the operator is actually running — and would make the two flags
        // unreachable, which is how this pair sat until 2026-09-22. The four that remain are
        // LISTING aids and are as foreign to an export as they are to a fetch, so they keep the
        // sentence they had. ⚠ The two departed rows are NOT ignored anywhere: `--addr` decides
        // the route in the `Sub::Export` arm below, and `--kind` is either parsed there (remote) or
        // refused there BY NAME with the flag that reaches the route it means (local).
        refuse_foreign_flags(
            sub,
            &[
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING, and an export names ONE series. To find \
             the series you meant, `vike-cli data hist ls` takes all four",
        )?;
    } else {
        refuse_foreign_flags(
            sub,
            &[
                ("--addr", addr.is_some()),
                ("--kind", filter.kind.is_some()),
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag belongs to the READ half (`ls`/`gaps`/`coverage`), which asks a running \
             vike-datahub about a store — while `fetch --source starter|demo` drives the engine \
             against a store on this machine, named with --store",
        )?;
    }

    // The per-subcommand half of the grammar. Only `fetch` produces a spec and a window; the other
    // arms are refusals (and `rm`'s own construction), so the ONE `Args` below cannot drift between
    // subcommands the way five hand-written literals would.
    let mut rm = None;
    let mut repair = None;
    let mut export_range = None;
    // `Some` ONLY on `export`'s remote route — see [`Args::export`] for why that is the selector
    // rather than a boolean beside `export_range`.
    let mut export_args: Option<export::Plan> = None;
    let mut universe_window = None;
    let mut gate_args = None;
    let mut get_args = None;
    let (spec, window) = match sub {
        Sub::Fetch => {
            // ⚠ ONE verb, and the SOURCE decides its whole grammar. A venue fetch takes a spec and
            // a window; `starter` and `demo` take NEITHER, because each is a fixed span this side
            // does not choose. Collapsing them lost nothing — the refusals that made them separate
            // subcommands are all still here, keyed on the axis instead of on a verb name.
            // The local is an Option so the guard below can tell 'flag absent' from 'flag
            // given'; the axis itself has no third state — an omitted --source IS a venue.
            let resolved = source.unwrap_or(Source::Venue);
            match resolved {
                Source::Venue => {
                    let spec = spec.ok_or(
                        "fetch needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data hist \
                         fetch binance:BTCUSDT:1h --days 180` — or name a source that needs none \
                         (--source starter | demo)",
                    )?;
                    check_spec(&spec)?;
                    // ⚠ The MIRROR of `repair`'s `--addr` refusal, and it arrived with the route:
                    // a VENUE fetch has no engine route, so a flag naming one would be silently
                    // ignored — the shape this module refuses everywhere else rather than tolerating.
                    refuse_foreign_flags(
                        sub,
                        &[("--store", store.is_some()), ("--engine", engine.is_some())],
                        "those flags name a LOCAL store and the engine that opens it, and a VENUE \
                         fetch has no local route: history is fetched by the backend, once, into \
                         the store the datahub has open. Use --addr HOST:PORT (default \
                         127.0.0.1:7878) to say WHICH datahub — or --source starter|demo, which \
                         DO run locally and do take --store",
                    )?;
                    (Some(spec), Some(window_from(days, from, to)?))
                }
                // Every fetch-shaped flag is REFUSED here rather than ignored, for the reason both
                // share: each is a FIXED span. A `--days 30` that quietly did nothing would leave
                // an operator believing they had seeded or downloaded a month.
                Source::Starter | Source::Demo => {
                    let (what, why) = if resolved == Source::Demo {
                        (
                            "--source demo writes the synthetic `demo` tape",
                            "the demo tape is a fixed synthetic span",
                        )
                    } else {
                        (
                            "--source starter downloads the PUBLISHED dataset, whose series are fixed",
                            "the starter dataset is a fixed PUBLISHED span",
                        )
                    };
                    if spec.is_some() {
                        return Err(format!("that source takes no spec — {what}"));
                    }
                    refuse_a_window(sub, &days, &from, &to, why)?;
                    (None, None)
                }
            }
        }
        // ⚠ `--source` is FETCH's axis and nothing else's, refused elsewhere rather than ignored.
        // Every other verb either reads the store that is already there (`ls`, `coverage`,
        // `health`, `universe`, `export`) or edits it (`rm`, `repair`) — none of them CHOOSES where
        // rows come from, so a `--source` on one names a decision that verb does not make.
        _ if source.is_some() => {
            return Err(format!(
                "--source does not apply to `{}` — it is `fetch`'s axis: WHERE rows come from. \
                 This verb works on the store that is already there.",
                sub.as_str()
            ));
        }
        Sub::Export => {
            let raw_spec = spec.ok_or(
                "export needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data hist export \
                 demo:DEMOUSDT:1h --out slice.parquet` — or VENUE:SYMBOL with \
                 `--kind quote|trade` over --addr, whose lanes have no interval",
            )?;
            if out.is_none() {
                return Err(
                    "export needs --out FILE: it writes a standalone file, and there is no \
                     default name a store could supply"
                        .into(),
                );
            }
            // ⚠ `--days` is refused BY NAME rather than folded into a range — see [`ExportRange`].
            // It is refused on BOTH routes and for one reason, so the check sits above the split.
            if days.is_some() {
                return Err(
                    "--days bounds a FETCH: it counts back from NOW, which says nothing about \
                     what a store already holds. An export slices what is on disk — use \
                     --from/--to"
                        .into(),
                );
            }
            // ⚠ **THE ROUTE SPLIT.** `--addr` names a datahub that already has a store open;
            // `--store`/`--engine` name a store and a binary on THIS machine. This is `rm`'s rule
            // arriving on the second verb that has two routes, and it is spelled here rather than
            // shared because the two verbs' OTHER flags differ — what follows below is entirely
            // per-route grammar.
            if addr.is_some() && (store.is_some() || engine.is_some()) {
                return Err(
                    "--addr and --store/--engine are two DIFFERENT stores: --addr streams rows \
                     out of the store a running vike-datahub opened, while --store names one on \
                     this machine for the engine to open and write Parquet from. Pass one."
                        .to_string(),
                );
            }
            // ⚠ `addr.is_some()` rather than the resolved [`Args::addr`], which is always `Some`:
            // the route is a question about what the operator TYPED. This is the same local the
            // `addr_given` field is built from, read before that field exists.
            if addr.is_some() {
                let kind = export::parse_kind(filter.kind.as_deref())?;
                let parsed = export::parse_spec(&raw_spec, kind)?;
                let wire = export::parse_wire(format_raw.as_deref())?;
                // ⚠ BOTH bounds, and the refusal names the verb that PRINTS them — see
                // [`export::Plan::bounds`] for why a walk needs what a DataFusion scan does not.
                let (Some(from_raw), Some(to_raw)) = (from.as_deref(), to.as_deref()) else {
                    return Err(format!(
                        "a remote export needs BOTH --from and --to. It walks the range in \
                         windows, so it has to know where the first step begins and where to \
                         stop, and an unbounded side has neither — unlike the local route, whose \
                         engine scans whatever the store holds. \
                         `vike-cli data hist ls --venue {} --name {}` prints this series' \
                         recorded span, which is the two numbers to pass.",
                        parsed.venue, parsed.symbol
                    ));
                };
                let start = parse_date_label(from_raw).map_err(|e| {
                    format!("--from {from_raw:?} is not a timestamp this side can read ({e})")
                })?;
                let end = parse_date_label(to_raw).map_err(|e| {
                    format!("--to {to_raw:?} is not a timestamp this side can read ({e})")
                })?;
                // Refused rather than swapped, for `membership_window`'s reason: a range whose
                // ends are the wrong way round has two readable meanings, and picking one discards
                // half of what the operator typed. Here it would also produce an EMPTY file, which
                // reads exactly like a store that holds nothing.
                if start > end {
                    return Err(format!(
                        "--from ({}) is AFTER --to ({}) — an inverted range holds nothing, so the \
                         export would write a file with no rows in it, which reads exactly like \
                         an empty store. Pass them the other way round.",
                        epoch_ms_to_utc_date(start),
                        epoch_ms_to_utc_date(end)
                    ));
                }
                let step_defaulted = window_raw.is_none();
                let step_ms = match window_raw.as_deref() {
                    Some(raw) => export::parse_window_step(raw, kind)?,
                    None => kind.default_window_ms(),
                };
                export_args = Some(export::Plan {
                    kind,
                    spec: parsed,
                    wire,
                    bounds: (start, end),
                    step_ms,
                    step_defaulted,
                });
            } else {
                // The LOCAL route keeps the grammar it shipped with: three mandatory spec parts
                // (the engine's export writes bars), two INDEPENDENT optional bounds, Parquet.
                check_spec(&raw_spec)?;
                if let Some(raw) = format_raw.as_deref() {
                    export::refuse_a_wire_on_the_local_route(raw)?;
                }
                // ⚠ Both refused rather than ignored, and each names --addr, because both are
                // meaningful ONLY on the walk. `--kind` USED to be refused one block up with "that
                // flag belongs to the READ half", which is a true no for a false reason — it
                // belongs to a route this verb did not have. The reason moves; the refusal stays.
                if let Some(kind) = filter.kind.as_deref() {
                    refuse_an_account_kind_on_a_read(kind)?;
                    return Err(format!(
                        "--kind does not apply to a LOCAL export: the engine writes BARS, and the \
                         bar step is the spec's third part ({kind:?} would be a different row \
                         shape). The remote route reads three kinds — add --addr HOST:PORT to \
                         reach it"
                    ));
                }
                refuse_foreign_flags(
                    sub,
                    &[(export::WINDOW_FLAG, window_raw.is_some())],
                    "that flag sets the step of a WINDOWED WALK, and this route does not walk: it \
                     spawns the engine, which scans the whole range in one query. The walk is the \
                     remote route's — add --addr HOST:PORT",
                )?;
                export_range = Some(ExportRange { from, to });
            }
            (Some(raw_spec), None)
        }
        Sub::Get => {
            // ⚠ **THE ACCOUNT-KIND REFUSAL COMES FIRST, and the ORDER is the point.** This verb
            // takes no `--kind` at all, so the obvious answer to one is "that flag belongs to a
            // listing". For an ACCOUNT kind that answer would be a fact about this verb's shape
            // standing in front of a fact about the PLANE — §9.3.2's rule is that your fills,
            // orders, funding payments and equity are not `data`'s to serve, and an operator who
            // asked for them must meet that sentence rather than a flag-placement note. It is the
            // same sentence `ls --kind` and `gate --require-kind` give, from the one function that
            // spells it ([`refuse_an_account_kind_on_a_read`]).
            if let Some(kind) = filter.kind.as_deref() {
                refuse_an_account_kind_on_a_read(kind)?;
                return Err(format!(
                    "--kind does not apply to `get`: it reads BARS, and the bar step is the \
                     spec's THIRD part ({kind:?} would be a different row shape — a quote, a \
                     print or a book level — which this verb does not serve). \
                     `vike-cli data hist ls --kind {kind}` lists what the store holds of it"
                ));
            }
            refuse_foreign_flags(
                sub,
                &[
                    ("--venue", filter.venue.is_some()),
                    ("--name", filter.name.is_some()),
                    ("--class", class),
                    ("--partial-only", partial_only),
                ],
                "that flag narrows or annotates a LISTING, and this verb reads ONE series named \
                 EXACTLY by its spec — there is nothing here to filter. `vike-cli data hist ls` \
                 is the verb those flags belong to",
            )?;
            let spec = spec.ok_or(
                "get needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data hist get \
                 binance:BTCUSDT:1h --days 7 --limit 20`. This verb reads ONE bar series; \
                 `vike-cli data hist ls` is how you find out which ones a store holds",
            )?;
            let spec = get::parse_spec(&spec)?;
            // ⚠ §8.2's cost guard, both halves, at PARSE time — before a socket is opened, for
            // `gate`'s stated reason: nothing here is forwarded, so the far side could never
            // refuse it, and a request that can never be honoured must not cost a connection to
            // discover.
            let window = get::parse_window(days.as_deref(), from.as_deref(), to.as_deref())?;
            let limit_defaulted = limit.is_none();
            let limit = get::parse_limit(limit.as_deref())?;
            get_args = Some(GetArgs {
                spec,
                window,
                limit,
                limit_defaulted,
                // Present by construction: the match above builds one for every `Sub::Get`.
                render: get_render.expect("`parse` resolves a Render for every Sub::Get"),
            });
            (None, None)
        }
        Sub::List => {
            // `--partial-only` is a verdict about the CROSS-KIND join, and a per-series listing has
            // no such verdict to filter on — each row here is one kind, which can never disagree
            // with itself.
            refuse_foreign_flags(
                sub,
                &[("--partial-only", partial_only)],
                "that flag filters `coverage`'s cross-kind verdict, and an `ls` row is ONE \
                 series of ONE kind — there is nothing for it to disagree with",
            )?;
            (None, None)
        }
        Sub::Gaps => {
            // `--partial-only` for `ls`'s reason — a gap row is one series of one kind too.
            refuse_foreign_flags(
                sub,
                &[("--partial-only", partial_only)],
                "that flag filters `coverage`'s cross-kind verdict, and a `gaps` row is ONE \
                 series of ONE kind — there is nothing for it to disagree with",
            )?;
            // ⚠ `--class` is refused here and ACCEPTED on `ls`, and the split is the whole reason
            // these are two verbs. A recorded asset class is an annotation on WHAT EXISTS; a gap
            // is a statement about what does not. Putting the two in one table would cost one
            // extra round trip per distinct (venue, symbol) on top of the one per matched SERIES
            // this verb already pays — and would answer a question the operator did not ask, in
            // the output whose whole value is that every line in it is an absence.
            refuse_foreign_flags(
                sub,
                &[("--class", class)],
                "that flag annotates what a store HOLDS, and every line here is about what it \
                 does NOT — and it costs a second round trip per instrument on top of this verb's \
                 own one per series. `vike-cli data hist ls --class` is where the class is",
            )?;
            (None, None)
        }
        Sub::Coverage => {
            // ⚠ The `--gaps` refusal that used to open this arm has MOVED to the universal one in
            // [`parse`]'s flag section, because gaps is a VERB now — the unit argument it carried
            // (a gap range is epoch-ms, a coverage day is a UTC-day index) went with it. What is
            // left is the one refusal peculiar to this verb: `--kind` would filter away exactly
            // the kinds whose disagreement the report exists to show.
            refuse_foreign_flags(
                sub,
                &[("--kind", filter.kind.is_some())],
                "a coverage row IS the join across kinds, so filtering to one would leave a \
                 report that cannot show a day one kind has and another lacks — which is the \
                 whole of what it reports",
            )?;
            // ⚠ The refusal an operator is likeliest to argue with, since a class IS an instrument
            // fact and a coverage row IS an instrument — so the reason is the MEASURED one rather
            // than a taxonomy of verbs. `crates/vike-data/src/datafusion_hist.rs`'s
            // `coverage_report` enumerates only `crates/vike-data/src/coverage.rs`'s `TICK_KINDS`,
            // which excludes `bar`, so an instrument a `data hist fetch` wrote appears in NO row here at
            // all — measured on that function, not inferred. A class column on
            // this verb would be silently blind to the commonest thing a store holds, and an
            // operator reading it as "every instrument is classified" would be reading a subset.
            refuse_foreign_flags(
                sub,
                &[("--class", class)],
                "a `coverage` row is the join across the TICK kinds only — a bar-only instrument \
                 (what `vike-cli data hist fetch` writes) appears in none of them, so a class column \
                 here would answer for a subset of the store while reading like the whole of it. \
                 `vike-cli data hist ls --class` enumerates every stored series",
            )?;
            (None, None)
        }
        Sub::TapeHealth => {
            // The refusal is about the DISTINCTION this verb exists to draw, not about tidiness.
            // `--partial-only` reports ABSENCE — a day one kind lacks — and absence is what the
            // sibling verbs already answer correctly. This one reports what is PRESENT and
            // impossible. Accepting an absence flag here would put the two units in one output
            // under one heading.
            refuse_foreign_flags(
                sub,
                &[("--partial-only", partial_only)],
                "that flag reports what is MISSING, and this verb reports what is PRESENT and \
                 self-contradictory — a gap is absence, which `vike-cli data hist gaps` and \
                 `vike-cli data hist coverage` already answer. Mixing the two puts two different \
                 meanings of `wrong` in one table",
            )?;
            // ⚠ A SEPARATE refusal from the pair above, because the reason is not the absence/
            // presence split — it is this verb's declared property that NO ROW CROSSES THE WIRE
            // (see [`execute_tape_health`]): every finding is arithmetic over the coverage numbers
            // one `inventory()` already carried. A class probe is a second round trip per
            // instrument, and an unclassified instrument is not a contradiction in its own catalog.
            refuse_foreign_flags(
                sub,
                &[("--class", class)],
                "every finding here is folded from the ONE inventory this verb already fetched, \
                 and a recorded class is a second round trip per instrument that no finding is \
                 derived from — an instrument naming no class is a wiring gap, not a catalog that \
                 contradicts itself. `vike-cli data hist ls --class` is where the class is",
            )?;
            (None, None)
        }
        Sub::Universe => {
            refuse_foreign_flags(
                sub,
                &[("--partial-only", partial_only)],
                "that flag annotates one series' recorded span, while a `universe` row is one \
                 INSTRUMENT's membership of a window. Whether a member's tape has holes IN it is \
                 `vike-cli data hist gaps`; this verb answers whether it was there at all",
            )?;
            // ⚠ `--class` is refused for a reason of its OWN, and it is the one this verb exists
            // to defend: every answer here is judged AS OF a window the operator wrote down, and
            // the class probe is deliberately as-of NOW ([`CLASS_AS_OF_TS`]). A class column on a
            // point-in-time membership report would be the one cell in it that changed meaning
            // between two runs over the same window — which is the survivorship defect wearing a
            // different field.
            refuse_foreign_flags(
                sub,
                &[("--class", class)],
                "every cell here is judged as of the window you named, and the recorded class is \
                 read as of NOW — so it would be the one column that answers differently on two \
                 runs over the same window. `vike-cli data hist ls --class` is where the class is",
            )?;
            // ⚠ `--days` is refused BY NAME here for a reason of its own, and not the one
            // `Sub::Export` gives. An export's `--days` is meaningless because a store's contents
            // have nothing to do with now(); a universe's would be WORSE than meaningless — it
            // would resolve, and it would resolve to a different window every day it is run. A
            // membership report is a thing two backtests are compared against, so its window has
            // to be a date somebody wrote down.
            if days.is_some() {
                return Err(
                    "--days counts back from NOW, so a membership window built from it answers a \
                     different question every day it is run — and a point-in-time universe exists \
                     precisely to be re-askable. Name the dates: --from/--to (epoch-ms or \
                     YYYY-MM-DD), either of which stands alone"
                        .into(),
                );
            }
            universe_window = Some(membership_window(from.as_deref(), to.as_deref())?);
            (None, None)
        }
        Sub::Gate => {
            // The browse aids are refused with the reason this verb makes them wrong rather than
            // merely inapplicable: a gate ASSERTS, and every one of these three widens or annotates
            // a listing. `--kind` is the one an operator will reach for first, so its message names
            // the criterion flag that replaced it rather than the shape of the mistake.
            refuse_foreign_flags(
                sub,
                &[("--venue", filter.venue.is_some()), ("--name", filter.name.is_some())],
                "that flag narrows a LISTING by substring, and a gate names the series it asserts \
                 about EXACTLY — in the spec. A substring match would pass the gate on a series \
                 you did not name, which is a green build over the wrong data",
            )?;
            refuse_foreign_flags(
                sub,
                &[("--kind", filter.kind.is_some())],
                "that flag filters a listing; here the kind is a CRITERION, so it is \
                 `--require-kind K` — repeatable, defaulting to `bar`, and a kind named there that \
                 the store lacks is a BREACH rather than a row that quietly vanished",
            )?;
            refuse_foreign_flags(
                sub,
                &[("--class", class), ("--partial-only", partial_only)],
                "that flag annotates a LISTING, and every line here is a criterion and its \
                 verdict. A recorded asset class is not something a store can be READY or not \
                 ready for, and it costs a round trip per instrument to fetch — \
                 `vike-cli data hist ls --class` is where it is",
            )?;
            let spec = spec.ok_or(
                "gate needs a spec: VENUE:SYMBOL[:INTERVAL], or VENUE:@GROUP for a grouped \
                 series — e.g. `vike-cli data hist gate binance:BTCUSDT:1h --require-days 365 \
                 --max-gap 1d`. A gate over `whatever the store holds` is a gate nobody can act on",
            )?;
            let spec = gate::parse_spec(&spec)?;
            // ⚠ REQUIRED, and refused at the door rather than defaulted. The precedent is
            // `crates/vike-cli/src/cmd/runs/gate.rs`'s `refuse_an_ungateable_line`: a gate with no
            // criteria would exit 0 having checked nothing, which is the one answer a CI step must
            // never get — so there is no default that could be safe here.
            // ⚠ The remedy clause says what `--require-days 1` IS, and it used to say something
            // else: "For a PRESENCE-only gate write `--require-days 1`". That label was false —
            // `judge_days` requires `span_ms >= 86_400_000`, so a series that is present and six
            // hours old BREACHES it, and an operator who wrote the advertised line into an
            // `ExecStartPre=` had a unit refusing to start over exactly the tape it asked for. The
            // trailing clause was always accurate; the label in front of it is the part a reader
            // skims, so the label is gone and the absence it papered over is stated outright.
            let require_days = require_days.ok_or(
                "gate needs --require-days N: a gate with no criterion exits 0 having checked \
                 nothing, which is the one answer a CI step must never get. There is no \
                 PRESENCE-only spelling — the narrowest gate is `--require-days 1`, which asserts \
                 the series holds a WHOLE DAY of anything, so a tape fetched an hour ago breaches \
                 it",
            )?;
            let require_days = gate::parse_require_days(&require_days)?;
            let max_gap_ms = max_gap.as_deref().map(gate::parse_max_gap).transpose()?;
            // ⚠ The account-kind REFUSAL is applied HERE, at PARSE time, where `ls` applies its
            // twin at execute time. Both are right for their verb: a listing's `--kind` is a filter
            // over an answer that has already arrived, while a gate's criterion decides whether a
            // socket is worth opening at all — and a criterion that can never be honoured must not
            // cost a connection to discover ([`refuse_a_blank_produced_by`] makes the same trade).
            //
            // ⚠ It is HALF of §9.3.2 and was once mistaken for the whole of it: the other half is
            // the EXCLUSION, which this verb owes exactly as `ls` does and which lives in
            // [`execute_gate`] because it is about what the store ANSWERED, not about what the
            // operator asked. A refused criterion says nothing about evidence.
            let mut kinds = Vec::new();
            for kind in require_kinds {
                let kind = kind.trim().to_string();
                if kind.is_empty() {
                    return Err(
                        "--require-kind was given an EMPTY value. It names a kind the store must \
                         hold (bar/quote/trade/book/depth and more); omit the flag to gate `bar`, \
                         which is the default"
                            .to_string(),
                    );
                }
                refuse_an_account_kind_on_a_read(&kind)?;
                // Repeating a kind is accepted and collapsed rather than refused: the flag
                // declares a SET, and a set that already contains the value is not a mistake worth
                // a message. Collapsing keeps the verdict one row per kind.
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
            }
            if kinds.is_empty() {
                kinds.push(gate::DEFAULT_KIND.to_string());
            }
            // ⚠ AFTER the default is folded in, because the default is the one kind a spec's third
            // part can select — checking before it would refuse nothing and check the wrong set.
            // The refusal itself is the mirror of `gate::parse_spec`'s grouped one; see its doc.
            gate::refuse_a_kind_the_spec_can_never_select(&spec, &kinds)?;
            gate_args = Some(GateArgs { spec, require_days, max_gap_ms, kinds });
            (None, None)
        }
        Sub::Rm => {
            // ⚠ `--kind`/`--venue` arrive in [`Filter`] because ONE flag loop parses every flag
            // (see [`parse`]'s doc). For `rm` they are not a substring filter at all — they are the
            // two required path segments above every leaf — so they are MOVED here, and the
            // `Filter` this `Args` carries is left empty for the subcommand that has no listing.
            let kind = filter.kind.take().ok_or(
                "rm needs --kind: it is the first dimension a cleanup selects on, and the store \
                 holds bar/quote/trade/book/depth and more under one venue",
            )?;
            let venue = filter.venue.take().ok_or(
                "rm needs --venue: with --kind it bounds the blast radius to a subtree an \
                 operator can name and see, rather than to `the store`",
            )?;
            refuse_an_account_kind(&kind)?;
            // The SHAPE rules that need no store. Everything else — an unknown kind, an interval
            // on a kind that does not sub-partition by one, a group on a kind that has no grouped
            // form — is refused on the far side against `STORE_KINDS`, because a roster copied
            // into this crate would be a second list to keep in step. That is `fetch`'s own rule
            // (see the module doc) applied unchanged.
            if symbol.is_some() && group.is_some() {
                return Err(
                    "--symbol and --group are ALTERNATIVES, not a pair: a GROUPED series has an \
                     EMPTY symbol and a per-symbol series has no group. Pass one."
                        .to_string(),
                );
            }
            if group.is_some() && interval.is_some() {
                return Err("--interval does not apply to --group: a grouped series' leaf has no \
                     `interval=` segment at all"
                    .to_string());
            }
            // ⚠ **The SELECTOR dimensions only** — `--produced-by` is deliberately NOT in this
            // list any more, and taking it out is a correction rather than a refactor. It rode
            // here because both flags are strings that must not be blank, and it inherited a
            // sentence written for a DIMENSION: *"An empty `symbol=` is the store's GROUPED-series
            // sentinel … omit the flag to wildcard the dimension instead."* Both halves are false
            // of a provenance assertion. It is not a dimension, and OMITTING it does not wildcard
            // anything — it turns the assertion OFF, which on a sweep is refused outright. So the
            // one guard standing between a blank prefix and the wire told the operator to do the
            // thing that is forbidden. [`refuse_a_blank_produced_by`] carries the real reason.
            for (flag, value) in [
                ("--kind", Some(&kind)),
                ("--venue", Some(&venue)),
                ("--symbol", symbol.as_ref()),
                ("--group", group.as_ref()),
                ("--interval", interval.as_ref()),
            ] {
                let Some(value) = value else { continue };
                if value.trim().is_empty() {
                    return Err(format!(
                        "{flag} was given an EMPTY value. An empty `symbol=` is the store's \
                         GROUPED-series sentinel, so an empty selector names neither layout — omit \
                         the flag to wildcard the dimension instead"
                    ));
                }
                // A glob is a SECOND matcher with its own escaping rules, and omitting a dimension
                // already covers the shape a cleanup needs. Refused at the door rather than
                // half-implemented.
                if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
                    return Err(format!(
                        "{flag} value {value:?} contains the glob character {c:?}. Globs are \
                         refused here: omitting a dimension already wildcards it"
                    ));
                }
            }
            // The assertion's OWN rules, on BOTH routes, before anything is dialled or spawned.
            if let Some(p) = produced_by.as_deref() {
                refuse_a_blank_produced_by(p)?;
                if addr.is_some() {
                    refuse_a_producer_path_on_the_remote_route(p)?;
                }
                if let Some(c) = p.chars().find(|c| "*?[".contains(*c)) {
                    return Err(format!(
                        "--produced-by value {p:?} contains the glob character {c:?}. A prefix is \
                         matched with `starts_with`, never globbed: pass the literal prefix the \
                         rows carry"
                    ));
                }
            }
            rm = Some(RmArgs { kind, venue, symbol, group, interval, produced_by, dry_run, yes });
            (None, None)
        }
        Sub::Repair => {
            // `--kind`/`--venue` arrive in [`Filter`] because ONE flag loop parses every flag.
            // For `repair`, as for `rm`, they are not a substring filter — they are the two path
            // segments above the series leaf — so they are MOVED out of it here.
            let kind = filter.kind.take().ok_or(
                "repair needs --kind: it is the first path segment above every series leaf, and \
                 the store holds bar/quote/trade/book/depth and more under one venue",
            )?;
            let venue = filter.venue.take().ok_or(
                "repair needs --venue: with --kind it names the subtree the series leaf sits under",
            )?;
            // The SHAPE rules that need no store — `rm`'s two, plus one that is this verb's alone.
            if symbol.is_some() && group.is_some() {
                return Err(
                    "--symbol and --group are ALTERNATIVES, not a pair: a GROUPED series has an \
                     EMPTY symbol and a per-symbol series has no group. Pass one."
                        .to_string(),
                );
            }
            if group.is_some() && interval.is_some() {
                return Err("--interval does not apply to --group: a grouped series' leaf has no \
                     `interval=` segment at all"
                    .to_string());
            }
            // ⚠ **THE DIFFERENCE FROM `rm`.** An omitted dimension WILDCARDS there and names
            // nothing here: `repair` rebuilds exactly one series. ⚠ `--interval`'s requirement on
            // `bar` is deliberately NOT checked here and cannot be — which kinds sub-partition by
            // interval is `vike_data::store_kind::STORE_KINDS`, the table this crate does not
            // link — so the ENGINE refuses that half through `SeriesSelector::is_sweep`, which is
            // the same shape-vs-roster split `rm` already draws.
            if symbol.is_none() && group.is_none() {
                return Err(
                    "repair needs --symbol S or --group G: it rebuilds ONE series' index, never a \
                     wildcard set. A rebuild holds that series' lock across every part footer it \
                     reads, and its verdict — what came back and what did not — is per-series, so \
                     a sweep would fold N unbounded critical sections and N verdicts into one exit \
                     code. For several series, run this verb several times."
                        .to_string(),
                );
            }
            for (flag, value) in [
                ("--kind", Some(&kind)),
                ("--venue", Some(&venue)),
                ("--symbol", symbol.as_ref()),
                ("--group", group.as_ref()),
                ("--interval", interval.as_ref()),
            ] {
                let Some(value) = value else { continue };
                if value.trim().is_empty() {
                    return Err(format!(
                        "{flag} was given an EMPTY value. An empty `symbol=` is the store's \
                         GROUPED-series sentinel, so an empty selector names neither layout — and \
                         `repair` has no wildcard to fall back to"
                    ));
                }
                if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
                    return Err(format!(
                        "{flag} value {value:?} contains the glob character {c:?}. `repair` names \
                         ONE series exactly; there is no matcher here for a glob to feed"
                    ));
                }
            }
            repair = Some(RepairArgs { kind, venue, symbol, group, interval, dry_run, yes });
            (None, None)
        }
    };

    Ok(Args {
        sub,
        // Defaulted here rather than at the flag, so every subcommand carries a source and only
        // `fetch` consults it — the axis has one home, not an Option threaded through.
        source: source.unwrap_or(Source::Venue),
        spec,
        window,
        export_range,
        universe_window,
        out,
        store,
        engine,
        // ⚠ THE TWO ARE DELIBERATELY NOT THE SAME QUESTION, and collapsing them would change
        // what `rm` DELETES. `addr_given` is "the operator asked for the remote route ON THIS LINE"
        // — `execute_rm` turns on it — so a configured `config.datahub_addr` must NOT set it: a box
        // that merely names where its datahub lives has not thereby asked for every `data hist rm` to be
        // executed against that datahub instead of the local store. The setting answers WHERE to
        // dial, never WHETHER to.
        addr_given: addr.is_some(),
        addr: addr
            .or_else(|| {
                // A BLANK rung is skipped rather than honoured, the same rule the compute ladder
                // applies: an `Environment=` line that set nothing must not aim this at an empty
                // address.
                configured_addr.filter(|s| !s.trim().is_empty()).map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        filter,
        gaps,
        class,
        partial_only,
        json,
        rm,
        repair,
        gate: gate_args,
        get: get_args,
        export: export_args,
    })
}

/// ⚠ **`repair` has no REMOTE route, and `--addr` is refused rather than ignored.** The sentence,
/// and the argument behind it, live here because three separate facts each say no and a reader who
/// only meets one of them will try to add the route back.
///
/// 1. **The wire's own rule.** `docs/decisions/0057`'s decision 3 admits a write-shaped verb to the
///    Observe side only when it is COST-bounded by server constants, ADDITIVE, IDEMPOTENT and
///    CONTAINED, and operator-armed. A rebuild is none of the first four: its cost is however many
///    part footers the series has, it REPLACES an index rather than adding to one, and it clears a
///    delta log. `vike_datahub_client::proto`'s `FEATURE_DELETE_SERIES` is the precedent for the
///    Control side, and `docs/decisions/0050` is why a key-less datahub would serve no such verb
///    at all — so the remote route would exist for a minority of deployments.
/// 2. **The datahub's vocabulary cannot name the broken series.** Every store-metadata RPC starts
///    from `DataFusionHist::list_series`, which finds leaves by the presence of `_manifest.json` —
///    so the headline failure this verb repairs is absent from `inventory()` and from every answer
///    a datahub can give. A remote repair would be reachable for exactly the series that do not
///    need one.
/// 3. **It would be a SECOND writer on a hot series**, which is the reopening clause
///    `docs/decisions/0060` names by hand — and the process holding the store open over that socket
///    is usually the recorder itself.
///
/// So the route is `--store` (or the resolved default) and the engine, on the box the store is on,
/// which is also where somebody repairing a store already is.
fn refuse_the_remote_route_on_repair() -> String {
    "--addr asks a running vike-datahub about the store THAT process opened, and `repair` has no \
     remote route. Three reasons: a rebuild is neither cost-bounded nor additive nor idempotent, \
     so it does not meet the bar a write-shaped wire verb has to clear; a datahub can only name \
     series it ENUMERATED, and a series whose base manifest is missing is in no enumeration — \
     which is exactly the series this repairs; and the process serving that socket is usually the \
     writer a rebuild must not collide with. Run it on the box, with --store DIR (or none, for the \
     resolved default)."
        .to_string()
}

/// Refuse a fetch WINDOW on a subcommand whose span is fixed — `seed-demo`'s synthetic curve and
/// `fetch-starter`'s published dataset.
///
/// Refused rather than ignored, for one reason both share: a `--days 30` that quietly did nothing
/// would leave an operator believing they had a month of history that is not there.
fn refuse_a_window(
    sub: Sub,
    days: &Option<String>,
    from: &Option<String>,
    to: &Option<String>,
    why: &str,
) -> Result<(), String> {
    for (flag, present) in
        [("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())]
    {
        if present {
            return Err(format!(
                "{flag} bounds a `fetch` window and does not apply to `{}` — {why}",
                sub.as_str()
            ));
        }
    }
    Ok(())
}

/// ⚠ **A BLANK `--produced-by` is refused, and this is the sentence that says why.**
///
/// The words are `vike_data::store_kind::resolve_produced_by`'s own, deliberately: that resolver is
/// the ENGINE's one site for this rule, this crate cannot link it (`vike-data` is a DEV-dependency
/// here — the module doc's note on `SeriesRow` carries the edge), and the two must not answer
/// differently about the same token on the same verb.
///
/// **What a blank prefix actually does is the opposite of what it looks like.** It does not match
/// nothing — `vike_data::store_kind::key_matches_prefix` is `starts_with`, so EVERY commit key
/// satisfies it, `vike_data::removal::RemovalPlan::verdict` finds no foreign key in any series, and
/// the whole provenance assertion passes VACUOUSLY. And because the value is `Some` rather than
/// `None`, the rule that REQUIRES an assertion before a wildcard delete sees a value and stands
/// down. Two guards fall to one blank token, on an IRREVERSIBLE verb.
///
/// ⚠ **It is reachable from a script, not only from a typo**: `--produced-by="$PREFIX"` with
/// `PREFIX` unset collapses to `--produced-by=`, and `--produced-by "$PREFIX"` to `--produced-by
/// ""`. `crate::cmd::args`'s `Flags::value` accepts both — an empty string is not a flag token —
/// so both reach here.
///
/// ⚠ **It was already refused, and that is the interesting part.** The refusal was a row in the
/// SELECTOR loop above, whose message argues about the store's grouped-series `symbol=` sentinel
/// and tells the operator to *omit the flag to wildcard the dimension* — which for this flag means
/// turn the assertion off, i.e. the one thing a sweep refuses. So the only thing standing between
/// a blank prefix and the wire was an untested line that did not know what it was guarding, and
/// the refactor ruling 12 asks for is exactly the edit that drops it. Hence a function, hence its
/// own tests on BOTH routes.
///
/// ⚠ **On the remote route there used to be nothing behind it, and there is now.** Until
/// 2026-09-11 `crates/vike-datahub/src/server.rs`'s `delete_series_verb` tested
/// `produced_by.is_none()` for the sweep gate — which `Some("")` satisfies — and handed the raw
/// spelling to `plan_removal`, so a blank prefix getting past this check was a wildcard delete
/// wearing an assertion. That server now resolves the spelling through
/// `vike_datahub_client::proto`'s `resolve_produced_by` at its own door.
///
/// **This refusal stays, and it is not redundant.** It refuses BEFORE a socket is dialled, which is
/// strictly better than a round trip; it is the only guard on the LOCAL route's argv before the
/// engine is spawned; and a `vike-cli` this new will meet datahubs older than the fix for as long
/// as one is deployed — the protocol is capability-negotiated, and there is no capability string
/// for "this server validates its provenance filter".
/// Refuse a cleanup aimed at ACCOUNT data, by name.
///
/// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 3 requires
/// this, and the reason is narrower and sharper than "wrong plane". `kind=exec_funding` partitions
/// on `(venue, coin)` while its commit key carries `{account}` — so two accounts' funding payments
/// for one coin land in ONE series, and `rm` deletes BY SERIES. An `rm` aimed at a test account's
/// history takes the live account's with it, silently. There is no selector that could scope it,
/// because the dimension the operator would need is not in the partition.
///
/// So this plane does not offer the deletion at all, rather than offering one that cannot be
/// aimed. The same refusal covers the other three account kinds for the plainer reason in the
/// surface design's §9.3.2: they are not market data and belong to a future `vike-cli account`.
///
/// ⚠ **The roster is NOT a copy.** `vike_model::is_account_kind` is the one declaration, and
/// `crates/vike-data/tests/store_kind_gate.rs` holds the store's own rows against it. An earlier
/// version of this function carried a private `ACCOUNT_KINDS` array held equal by a text gate that
/// parsed both source files; the root `CLAUDE.md` names the better cure — *a shared crate BELOW
/// both* — and `vike-model` is already a normal dependency of this crate and of `vike-data`, with
/// no DataFusion in it. The copy and its gate are gone.
fn refuse_an_account_kind(kind: &str) -> Result<(), String> {
    if !vike_model::is_account_kind(kind) {
        return Ok(());
    }
    let extra = if kind == "exec_funding" {
        " — and for this kind a scoped delete is not merely unimplemented but UNEXPRESSIBLE: the \
         series partitions on (venue, coin) while the account rides in the commit key, so one \
         series holds every account's payments for that coin and `rm` deletes by series"
    } else {
        ""
    };
    Err(format!(
        "`{kind}` is ACCOUNT data, not market data, and `data hist rm` refuses it{extra}. Your \
         fills, orders, funding payments and equity belong to `vike-cli account`, which is not \
         built yet. The market funding RATE is unaffected and is not an account kind — it is \
         `--kind bar --interval funding`."
    ))
}

fn refuse_a_blank_produced_by(produced_by: &str) -> Result<(), String> {
    if produced_by.trim().is_empty() {
        return Err(format!(
            "--produced-by {produced_by:?} is BLANK, so there is nothing to assert against — an \
             empty prefix matches every key, which makes the provenance check pass for every \
             series while looking like an assertion, and satisfies the rule that REQUIRES one \
             before a wildcard delete. Pass a literal prefix instead, or omit the flag."
        ));
    }
    Ok(())
}

/// ⚠ **A PRODUCER PATH is refused on the REMOTE route — a COMPATIBILITY guard since 2026-09-11,
/// and a stand-in for a missing check before that.**
///
/// `--produced-by` has two spellings: a literal commit-key prefix (`panel_bars:`), and the
/// repo-relative path of a declared producer, which `vike_data::store_kind::resolve_produced_by`
/// turns INTO its prefix by reading `STORE_KINDS`. That resolver used to have exactly one caller in
/// the tree — `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series`, i.e. the LOCAL route —
/// while `crates/vike-datahub/src/server.rs`'s `delete_series_verb` called neither it nor anything
/// like it: it handed the raw spelling to `plan_removal`.
///
/// So the same command line answered two ways. `--produced-by crates/vike-data/src/demo.rs` with
/// `--store` resolved to `demo-tape:v`, the assertion held and the series were deleted; the same
/// line with `--addr` sent the PATH as a literal prefix, `key_matches_prefix` (`starts_with`)
/// matched no key in any series, `RemovalPlan::verdict` called every key foreign, and the operator
/// was told their store had foreign provenance — when in fact their flag had never been resolved.
/// It fails SAFE (a path no key starts with can only refuse), and that is exactly what made it
/// survive: the wrong answer looks like a serious finding about the data rather than a defect.
///
/// THREE surfaces promised the resolution unconditionally — this module's `USAGE`, `RmArgs`'s
/// `produced_by` doc, and `crate::cmd::mcp`'s `delete_series` tool schema, which is REMOTE-ONLY and
/// so was never true. All three now say where it holds; this refusal is what makes the boundary
/// visible at the moment it is crossed instead of a page later.
///
/// ⚠ **THE ASYMMETRY IS CLOSED ON THE SERVER, and this refusal survives as a compatibility guard
/// rather than as a stand-in.** The follow-up this doc named — a `resolve_produced_by` re-export in
/// `vike_datahub_client::proto`, beside the `SeriesSelector`/`describe_id` re-exports that exist for
/// exactly this reason — landed with the wire's blank-filter fix, and `delete_series_verb` now
/// resolves a path exactly as the local route does. What it does NOT close is the mixed fleet: this
/// protocol is capability-negotiated rather than version-gated and carries no capability string for
/// "this server resolves producer paths", so a new CLI cannot tell a fixed datahub from an
/// unfixed one and must keep refusing to send a spelling the older one would assert literally.
/// Deleting this refusal hands #1754's defect back to every operator pointing a current `vike-cli`
/// at a datahub that has not been redeployed.
fn refuse_a_producer_path_on_the_remote_route(produced_by: &str) -> Result<(), String> {
    if produced_by.contains('/') {
        return Err(format!(
            "--produced-by {produced_by:?} looks like a PRODUCER PATH, and a datahub that has not \
             been redeployed since 2026-09-11 resolves none — it would be asserted as a literal \
             prefix, match no commit key in any series, and report your data as foreign when in \
             fact the flag was never resolved. This protocol carries no capability string for \
             \"this server resolves producer paths\", so a fixed datahub and an unfixed one cannot \
             be told apart from here and a path is refused before anything is dialled. Pass the \
             commit-key PREFIX literally (e.g. `panel_bars:`), or run the delete against a local \
             store (`--store DIR`), which always resolves a path."
        ));
    }
    Ok(())
}

/// Refuse the first flag in `present` that was actually given, naming the subcommand it was given
/// to and WHY it belongs elsewhere.
///
/// ⚠ The `why` sentence is the product, not the refusal. Every flag this function guards is one an
/// operator typed because a SIBLING subcommand accepts it, so "unknown option" would be a lie and
/// a bare "not allowed here" would leave them guessing which sibling. The messages therefore name
/// the store the flag would have reached and the flag that reaches the other one.
fn refuse_foreign_flags(sub: Sub, present: &[(&str, bool)], why: &str) -> Result<(), String> {
    for (flag, given) in present {
        if *given {
            return Err(format!("{flag} does not apply to `{}` — {why}", sub.as_str()));
        }
    }
    Ok(())
}

/// The spec's SHAPE: three non-empty `:`-separated parts. See this module's doc for why the venue
/// and interval themselves are the ENGINE's to judge.
fn check_spec(spec: &str) -> Result<(), String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL:INTERVAL — three non-empty parts, e.g. binance:BTCUSDT:1h"
        ));
    }
    Ok(())
}

/// The window, as exactly one of the two forms.
///
/// ⚠ Mixing them is an error rather than a precedence rule. `--days 30 --from 2026-01-01T00` has
/// two readable meanings and no obviously right one, and whichever a precedence rule picked would
/// silently discard the other half of what the operator typed.
fn window_from(
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<Window, String> {
    match (days, from, to) {
        (Some(d), None, None) => {
            let n: u32 = d
                .trim()
                .parse()
                .map_err(|_| format!("--days takes a whole number of days, got {d:?}"))?;
            if n == 0 {
                return Err("--days 0 covers no time at all".to_string());
            }
            Ok(Window::Days(d))
        }
        (None, Some(f), Some(t)) => Ok(Window::Range { from: f, to: t }),
        (None, Some(_), None) => Err("--from needs a matching --to".to_string()),
        (None, None, Some(_)) => Err("--to needs a matching --from".to_string()),
        (None, None, None) => Err(
            "fetch needs a window: --days N, or --from LABEL --to LABEL (epoch-ms or YYYY-MM-DDTHH)"
                .to_string(),
        ),
        (Some(_), _, _) => {
            Err("--days and --from/--to are two ways to say the same thing — pass one".to_string())
        }
    }
}

/// `universe`'s window: BOTH bounds optional, BOTH independent, and BOTH parsed here.
///
/// ⚠ **The parse is the difference from [`window_from`] and from [`ExportRange`], and it is the
/// reason this verb has a helper of its own.** A fetch's and an export's bounds are forwarded to
/// the engine as TEXT — one timestamp parser in the workspace, and this side deliberately does not
/// own a second. Nothing is forwarded here: `universe` compares timestamps a datahub already sent,
/// in this process, so an unreadable bound has to be refused HERE or it would be silently
/// discarded into a window that means something else. It goes through
/// [`vike_model::parse_date_label`] — the SAME parser the engine's own bounds reach, so the two
/// verbs cannot disagree about what `2026-01-01` means.
///
/// The four combinations are all well-formed (see [`ExportRange`] for that argument), so the only
/// refusals are an unreadable label and an INVERTED pair — and the second is refused rather than
/// swapped, for [`window_from`]'s reason: a range whose ends are the wrong way round has two
/// readable meanings, and picking one discards half of what the operator typed. An inverted window
/// would otherwise report every instrument in the store `absent`, which reads exactly like an
/// empty store.
fn membership_window(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<universe::MembershipWindow, String> {
    fn bound(flag: &str, raw: Option<&str>) -> Result<Option<i64>, String> {
        let Some(raw) = raw else { return Ok(None) };
        match parse_date_label(raw) {
            Ok(ms) => Ok(Some(ms)),
            Err(e) => Err(format!(
                "{flag} {raw:?} is not a timestamp this side can read ({e}). `universe` compares \
                 dates in THIS process rather than forwarding them, so the bound is parsed here: \
                 epoch-ms, or YYYY-MM-DD"
            )),
        }
    }
    let from_ms = bound("--from", from)?;
    let to_ms = bound("--to", to)?;
    match (from_ms, to_ms) {
        (Some(f), Some(t)) if f > t => Err(format!(
            "--from ({}) is AFTER --to ({}) — an inverted window contains nothing, so every \
             instrument in the store would be reported `absent`, which is indistinguishable from \
             an empty store. Pass them the other way round.",
            epoch_ms_to_utc_date(f),
            epoch_ms_to_utc_date(t)
        )),
        _ => Ok(universe::MembershipWindow { from: from_ms, to: to_ms }),
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `data` verb; `project_root`
/// is `<project>`, resolved once by [`crate::run`], and is how the engine under `<project>/bin` is
/// found — a PARAMETER, because a `src/cmd/` file may not read the environment for itself.
///
/// ⚠ **`settings_dir` is a SECOND root and is NOT derivable from the first.** [`crate::run`] builds
/// `project_root` as `booted.settings_dir.parent()`, so on any box whose `$VIKE_SETTINGS_DIR` does
/// not end in `settings` a `project_root.join("settings")` here names a DIFFERENT directory than
/// the one the boot loaded settings and credentials from — silently, and for a verb that WRITES.
/// It arrives for [`record`], the one verb on this plane that touches the settings store; every
/// other arm ignores it.
pub fn run(
    args: impl Iterator<Item = String>,
    project_root: Option<&Path>,
    settings_dir: Option<&Path>,
    keys: Option<&NodeKeys>,
    configured_addr: Option<&str>,
) -> ExitCode {
    // ⚠ THE GROUP LAYER SPLITS HERE, above `parse`, and that is a statement about GRAMMAR rather
    // than about tidiness. [`Args`] is one struct answering for the `hist` group's verbs, and every
    // flag on it is refused by name on the verbs it does not belong to — a discipline that holds
    // because those verbs share a vocabulary (a series, a window, a store). `catalog`'s noun is an
    // INSTRUMENT a venue lists and `source`'s is a PROVENANCE; folding either into that struct
    // would make one type answer for three command languages and every refusal in it ambiguous.
    //
    // ⚠ The collect is deliberate and costs one small Vec: the group word has to be READ before
    // this is routed, and a `Peekable` would hand `parse` an iterator whose first item had already
    // been consumed on one path and not on the other — which is the shape a caller gets wrong.
    let argv: Vec<String> = args.collect();
    match argv.first().map(String::as_str) {
        Some("catalog") => return catalog::run(&argv[1..], keys, configured_addr),
        Some("realtime") => {
            return realtime::run(&argv[1..], settings_dir, keys, configured_addr);
        }
        Some("source") => return source::run(&argv[1..], keys, configured_addr),
        // `hist` and everything else. ⚠ This named `realtime` as the one group still refused inside
        // [`parse`], and that stopped being true when it shipped its verbs: EVERY group but `hist`
        // is routed above this line now, so no group word reaches that parser at all.
        _ => {}
    }
    let args = match parse(argv.into_iter(), configured_addr) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("data", USAGE, &msg),
    };
    // ⚠ The success arm carries a RUNG rather than being assumed to be zero, and `gate` is why:
    // its product IS the exit code, and a verdict that breached is a command that WORKED — so
    // routing it through `Err` would print one stderr line and discard the per-criterion document
    // that is the whole point (`crate::exit::Exit::Breach`'s own doc states that rule). Every other
    // subcommand answers `Exit::Ok` here, which is `ExitCode::SUCCESS` by construction.
    match execute(&args, project_root, keys) {
        Ok(exit) => exit.into(),
        Err(e) => {
            eprintln!("vike-cli data: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Route the parsed line to whichever half it belongs to — see [`Sub::is_read`] and the module
/// doc's opening. Nothing is shared between the two arms but this dispatch and the exit ladder:
/// they reach different stores by different mechanisms, which is exactly the fact the flag
/// refusals in [`parse`] exist to keep visible.
///
/// ⚠ It returns a RUNG rather than `()`. Every arm but one answers [`Exit::Ok`], and the
/// conversion `.map(|()| Exit::Ok)` says so at each site rather than being hidden in a helper;
/// `gate` is the one verb whose answer is the number, and threading it through the shared type is
/// what keeps that number decided in the function that judged it rather than re-derived at this
/// boundary.
fn execute(args: &Args, project_root: Option<&Path>, keys: Option<&NodeKeys>) -> CmdResult<Exit> {
    match args.sub {
        // The engine routes spawn a child against a LOCAL store and open no socket, so a datahub
        // key is not theirs to carry.
        // ⚠ `Fetch` LEFT this arm. It is the one verb here that asks a DATAHUB rather than running
        // an engine against a local store — see [`execute_fetch`] for the whole argument.
        // ⚠ ONE verb, TWO ROUTES, chosen by whether `--addr` was TYPED — the second verb in this
        // plane to have that shape, after `rm`. [`Args::export`] is `Some` for exactly the remote
        // one, so the route is read off a field that could only have been built on that branch
        // rather than re-derived from a flag here.
        Sub::Export => match args.export.as_ref() {
            Some(e) => execute_export_remote(args, e, keys).map(|()| Exit::Ok),
            None => execute_engine(args, project_root).map(|()| Exit::Ok),
        },
        // ⚠ ONE verb, TWO transports, chosen by the SOURCE. §2 records that this plane had three
        // transport mechanisms and no rule predicting which verb used which; this line is where
        // that stops being true — the transport is a property of WHERE the rows come from.
        Sub::Fetch => match args.source {
            Source::Venue => execute_fetch(args, keys).map(|()| Exit::Ok),
            Source::Starter | Source::Demo => execute_engine(args, project_root).map(|()| Exit::Ok),
        },
        // ⚠ ONE function for TWO verbs, and deliberately so: `gaps` IS `ls` with the probe armed,
        // and [`Args::gaps`] is already `true` here because [`parse`] derives it from the verb.
        // A second function would be the same enumeration, the same filter, the same account-kind
        // exclusion and the same disclosure, copied — which is how the two would come to disagree
        // about which series a filter matches.
        // The ROW verb. `Exit::Ok` even when the window held nothing: an empty answer is a fact
        // about the store rather than a failure of the command, the same call `execute_list` makes
        // for an empty listing — and [`get::empty_note`] is what stops it being read as "this
        // series does not exist".
        Sub::Get => execute_get(args, keys).map(|()| Exit::Ok),
        Sub::List | Sub::Gaps => execute_list(args, keys).map(|()| Exit::Ok),
        Sub::Coverage => execute_coverage(args, keys).map(|()| Exit::Ok),
        Sub::TapeHealth => execute_tape_health(args, keys).map(|()| Exit::Ok),
        Sub::Universe => execute_universe(args, keys).map(|()| Exit::Ok),
        // The ONE arm that decides its own rung — see [`execute_gate`].
        Sub::Gate => execute_gate(args, keys),
        Sub::Rm => execute_rm(args, project_root, keys).map(|()| Exit::Ok),
        // ENGINE-ONLY and carrying no datahub key: [`parse`] has already refused `--addr` here —
        // see [`refuse_the_remote_route_on_repair`].
        Sub::Repair => execute_repair(args, project_root).map(|()| Exit::Ok),
    }
}

// ─── `fetch`: the one verb that reaches ONLY a datahub ──────────────────────────────────────────

/// Resolve [`Window`]'s two forms to an epoch-ms pair, HERE rather than by forwarding text.
///
/// ⚠ **This is the difference the whole route turns on.** While `fetch` spawned the engine its
/// bounds were forwarded as TEXT and the engine owned the parse — this module's [`membership_window`]
/// says so in its own doc, and says this side *"deliberately does not own a second"* timestamp
/// parser. Asking a datahub changes that: `Request::Backfill` carries `start`/`end` as `i64`, so
/// somebody has to parse, and it can only be this side.
///
/// It is NOT a second parser. [`vike_model::parse_date_label`] is the same function the engine's own
/// bounds reach, which is what keeps `2026-01-01` meaning one thing across both routes — the
/// property `membership_window` already relies on for `universe`.
///
/// `--days N` counts back from [`vike_model::clock::now_ms`], the workspace's one sanctioned clock
/// read; `crates/vike-cli/src/lib.rs` already reads it five times, so this adds no
/// `crates/vike-ops/tests/clock_pin.rs` row.
///
/// # Errors
///
/// An unreadable label, a non-positive `--days`, or an inverted range — the last REFUSED rather
/// than swapped, for [`membership_window`]'s reason: a range whose ends are the wrong way round has
/// two readable meanings and picking one discards half of what the operator typed.
fn fetch_window_ms(window: Option<&Window>) -> Result<(i64, i64), String> {
    let now = vike_model::clock::now_ms();
    match window {
        Some(Window::Days(d)) => {
            let days: i64 =
                d.parse().map_err(|_| format!("--days {d:?} is not a whole number of days"))?;
            if days <= 0 {
                return Err(format!(
                    "--days {days} asks for an empty window; a fetch needs at least one day"
                ));
            }
            Ok((now - days * 86_400_000, now))
        }
        Some(Window::Range { from, to }) => {
            let f = parse_date_label(from).map_err(|e| {
                format!("--from {from:?} is not a timestamp this side can read ({e})")
            })?;
            let t = parse_date_label(to)
                .map_err(|e| format!("--to {to:?} is not a timestamp this side can read ({e})"))?;
            if f >= t {
                return Err(format!(
                    "--from ({}) is not before --to ({}) — an inverted or empty window fetches \
                     nothing, and swapping the ends would discard half of what was typed",
                    epoch_ms_to_utc_date(f),
                    epoch_ms_to_utc_date(t)
                ));
            }
            Ok((f, t))
        }
        // Unreachable by construction: `parse` gives `fetch` a window or refuses the line. Spelled
        // as a refusal rather than an `unwrap` so a future grammar change surfaces here.
        None => Err("fetch needs a window: --days N, or --from/--to".into()),
    }
}

/// `data hist fetch` — ask a datahub to pull a range of history into ITS store.
///
/// # Why this verb has no engine route, when `rm` has both
///
/// `rm` reaches either store because a deletion is meaningful against a local one. A FETCH is not:
/// history is fetched by the backend, once, into the store — *"clients request, never fetch"*, which
/// is [`vike_datahub_client::DatahubClient::backfill`]'s own sentence. The engine route this
/// replaced went straight to a venue's REST from inside `vike-backtest`, which made the compute
/// plane the only crate outside the data plane holding a venue bridge.
///
/// What that buys, measured: the datahub's collector table folds
/// `vike_backfill::kline_source::KLINE_SOURCES` — **six venues** against the engine route's one —
/// and it carries the still-forming-candle guard the direct path declares it does not have
/// (`crates/vike-ops/tests/kline_ingest_gate.rs` holds that row).
///
/// ⚠ **What it COSTS, stated because it is a real loss**: `data hist fetch` no longer works with no
/// server. It was a direct venue call and needed nothing; it now needs a reachable datahub — the
/// default `--addr` is `127.0.0.1:7878`. That was ruled deliberately rather than fallen into: a
/// fetch is a data-plane act, and a compute-plane binary reaching a venue directly is the thing
/// being removed.
///
/// [`Scope::Write`] because a backfill WRITES. `crates/vike-datahub-client/src/proto.rs`'s
/// `required_scope` puts it there and argues the boundary: the line that survives scrutiny is
/// bounded-by-an-operator-ceiling versus not, and a backfill spends a venue budget.
fn execute_fetch(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // Present by construction: `parse` refuses a `fetch` without a spec, and `check_spec` has
    // already held it to three non-empty parts.
    let spec = args.spec.as_deref().unwrap_or_default();
    let mut parts = spec.splitn(3, ':');
    let (venue, symbol, interval) = match (parts.next(), parts.next(), parts.next()) {
        (Some(v), Some(s), Some(i)) => (v, s, i),
        _ => return Err(CliError::usage(format!("'{spec}' is not VENUE:SYMBOL:INTERVAL"))),
    };
    let (start, end) = fetch_window_ms(args.window.as_ref()).map_err(CliError::usage)?;

    let mut client = connect(&args.addr, keys, Scope::Write)?;
    let done = client.backfill(venue, symbol, interval, start, end).map_err(|e| {
        CliError::failed(format!(
            "{e}\n  the datahub at {} is what fetches now; `vike-cli data hist fetch` no longer reaches \
             a venue itself. A server that does not advertise the verb was built without \
             `--features backfill-serve`.",
            args.addr
        ))
    })?;

    // Same stdout/stderr split as the `rm` route above: under `--json` stdout is the document and
    // the human line goes to stderr, so a pipe stays parseable while a person still sees what
    // happened.
    let span = match (done.first_ts, done.last_ts) {
        (Some(a), Some(b)) => {
            format!(" spanning {} .. {}", epoch_ms_to_utc_date(a), epoch_ms_to_utc_date(b))
        }
        _ => String::new(),
    };
    let line = format!(
        "fetched {venue}:{symbol}:{interval} -> {} rows written{span} (datahub at {})",
        done.rows_written, args.addr
    );
    if args.json {
        eprintln!("{line}");
        println!(
            "{}",
            serde_json::json!({
                "venue": venue,
                "symbol": symbol,
                "interval": interval,
                "rows_written": done.rows_written,
                "first_ts": done.first_ts,
                "last_ts": done.last_ts,
                "addr": args.addr,
            })
        );
    } else {
        println!("{line}");
    }
    Ok(())
}

// ─── `rm`: the one verb that reaches EITHER store ───────────────────────────────────────────────

/// `data hist rm` — route to the datahub or to the engine, having first refused the one shape neither
/// may be asked to handle.
///
/// # ⚠ The TTY refusal happens HERE, before a socket or a process
///
/// No `--yes`, no `--dry-run` and no terminal on stdin is a REFUSAL, on the usage rung. It is not a
/// pre-flight for the far side's own check — both routes refuse it too — it is the rung: nothing
/// was attempted and re-running unchanged cannot succeed, which is exactly what `Exit::Usage`
/// promises and what a wrapper needs to hear before it retries. Reading a confirmation from a PIPE
/// is the failure this exists to prevent (`yes | vike-cli data hist rm …`), and a pipe is
/// indistinguishable from a person once you have decided to read one.
///
/// # ⚠ ONE OPERATION, THREE PROCEDURES — what agrees, what was made to agree, and what still does not
///
/// The deletion CORE is one implementation and always was: `vike_data::removal`'s `plan_removal` /
/// `execute_removal` over `SeriesSelector`, with `DataFusionHist::delete_series_checked` re-asserting
/// under the series lock. The wire uses the SAME TYPES —
/// `vike_datahub_client::proto` re-exports `SeriesSelector`, `RemovalPlan`, `RemovalOutcome` and
/// `describe_id` rather than restating them — so selector semantics and plan RENDERING cannot
/// diverge between routes at all.
///
/// What is written three times is the PROCEDURE around it: here, in
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` (the engine, which the local route
/// spawns), and in `crates/vike-datahub/src/server.rs`'s `delete_series_verb`. Every divergence
/// below lives in that layer.
///
/// **Made to agree by this PR**, both client-side:
///
/// * **The provenance VERDICT.** The engine checks `RemovalPlan::verdict` immediately after
///   printing the plan and before the dry-run branch; [`execute_rm_remote`] checked it NOWHERE, so
///   a refused assertion still prompted a human to type `delete N series` and still spent a second
///   destructive round trip. It is checked at the same point on both routes now.
/// * **A blank `--produced-by`** — refused with the reason that is true of a provenance prefix
///   rather than the grouped-series-sentinel sentence it had inherited, and refused on BOTH routes
///   before anything is dialled or spawned. [`refuse_a_blank_produced_by`].
/// * **A producer PATH under `--addr`** — refused by name, because at the time neither side of that
///   wire resolved one. ⚠ The SERVER half landed on 2026-09-11 and
///   [`refuse_a_producer_path_on_the_remote_route`] became a compatibility guard for a datahub that
///   has not been redeployed; its doc carries why removing it would hand the defect back.
///
/// **Deliberately NOT closed here, because each needs a change outside this crate.** Recorded
/// rather than tolerated silently, and `crates/vike-cli/tests/data_cli.rs` pins the behaviour that
/// survives:
///
/// * **The EXIT LADDER differs by route.** The engine answers `2` for everything — a shape error, a
///   provenance refusal, a partial failure — while `crate::cmd::engine`'s `fold_status`
///   deliberately maps every non-zero child code onto `Exit::Failed` (`1`), because the engine's `2`
///   also means "the venue was geoblocked" and folding that onto the usage rung would tell a
///   wrapper a retry cannot help. So a shape error is `2` typed at the engine and `1` through
///   `data rm --store`, while the same error typed at `data hist rm` is `2` (this side refused it first).
///   Closing it needs the ENGINE to distinguish a pre-flight code from a runtime one; `fold_status`
///   carries that as its own "what would reopen this".
/// * **The no-TTY refusal fires at a different MOMENT.** This function refuses at the door, before
///   a socket or a process, on the usage rung — the engine's `confirm_removal` is reached only
///   after the plan and only when `matched() > 0`, so a no-match cleanup in CI succeeds there and
///   is refused here. THIS ordering is the one that survives, and deliberately: a confirmation must
///   bind to a COUNT, the remote route needs a round trip to learn one, and refusing before the
///   round trip is strictly better than after it. `USAGE`'s "Matching nothing is a SUCCESS" is
///   about the PLAN, not about this pre-flight.
/// * **THREE `--json` documents for one verb** — the engine's (`rm_series_json`, the only one that
///   can carry the resolved store ROOT and the rung that chose it), the local route's
///   ([`rm_json_local`], which nests it), and the remote route's ([`rm_json_remote`], the only one
///   carrying `addr` and `plan`). A machine consumer branches on `route`. Folding them needs a
///   shape neither side can produce alone.
/// * **The SERVER does not resolve `--produced-by` and does not consult `STORE_KINDS` at all.** The
///   clean fix is a `resolve_produced_by` re-export in `vike_datahub_client::proto`, beside the
///   re-exports that exist for exactly this reason — a change to a crate this branch does not own.
///   Until then the refusal above is what stands in for it.
fn execute_rm(args: &Args, project_root: Option<&Path>, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let rm = args.rm.as_ref().expect("`parse` builds an RmArgs for every Sub::Rm");
    if !rm.yes && !rm.dry_run && !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "refusing to delete without --yes: stdin is not a terminal, so there is nobody to \
             confirm. Pass --yes (a deliberate, greppable token in shell history), or --dry-run to \
             see the plan and stop.",
        ));
    }
    if args.addr_given {
        execute_rm_remote(args, rm, keys)
    } else {
        execute_rm_local(args, rm, project_root)
    }
}

/// The LOCAL route: drive the engine's `data hist rm` against a store on this machine.
///
/// ⚠ **The confirmation is the ENGINE's, and the child gets this process's stdin so it can read
/// one.** The alternative — confirming here — needs the matched COUNT the typed line is bound to,
/// which only the plan knows, which means spawning once for a plan and once to act, plus parsing a
/// machine document on the side of the fence that deliberately cannot open a store. Passing the
/// terminal through costs an enum ([`engine::Stdin`]) and keeps one confirmation, in the process
/// that computed the number it names.
fn execute_rm_local(args: &Args, rm: &RmArgs, project_root: Option<&Path>) -> CmdResult<()> {
    let program = engine::locate(args.engine.as_deref(), project_root);
    let argv = rm_engine_argv(args, rm);
    // The child may need to read a typed confirmation — unless `--yes` or `--dry-run` already
    // settled it, in which case it reads nothing and the default applies.
    let stdin = if rm.yes || rm.dry_run { engine::Stdin::Null } else { engine::Stdin::Inherit };
    if !args.json {
        return engine::run_with_stdin(&program, &argv, "data", stdin);
    }
    let report = engine::run_capturing_stdout_with_stdin(&program, &argv, "data", stdin)?;
    println!("{}", rm_json_local(args, rm, &program, &argv, &report));
    Ok(())
}

/// The engine argv `rm`'s arguments become — PURE, so the translation is unit-tested rather than
/// only observed through a spawn. `backtest data rm …` since ruling 12; see [`engine_argv`].
///
/// ⚠ `--produced-by` is forwarded VERBATIM, never resolved here. A producer-path spelling resolves
/// against `STORE_KINDS`, and that table lives in `vike-data` — the tree this crate exists not to
/// link. Resolving it here would be a second copy of the roster; the engine's message names what it
/// could not resolve.
///
/// ⚠ That forwarding used to make the LOCAL route the ONLY one that resolves a producer path. Since
/// 2026-09-11 the datahub resolves one too, through the `vike_datahub_client::proto` re-export of
/// the same function — so the asymmetry is closed in the code and survives only as a MIXED-FLEET
/// question, which [`refuse_a_producer_path_on_the_remote_route`] is what still answers.
fn rm_engine_argv(args: &Args, rm: &RmArgs) -> Vec<String> {
    let mut argv = vec![
        ENGINE_DATA_VERB.to_string(),
        Sub::Rm.as_str().to_string(),
        "--kind".to_string(),
        rm.kind.clone(),
        "--venue".to_string(),
        rm.venue.clone(),
    ];
    for (flag, value) in [
        ("--symbol", &rm.symbol),
        ("--group", &rm.group),
        ("--interval", &rm.interval),
        ("--produced-by", &rm.produced_by),
    ] {
        if let Some(v) = value {
            argv.push(flag.to_string());
            argv.push(v.clone());
        }
    }
    if rm.dry_run {
        argv.push("--dry-run".to_string());
    }
    if rm.yes {
        argv.push("--yes".to_string());
    }
    if args.json {
        // ⚠ FORWARDED, unlike `fetch`'s `--json` (which this module CONSUMES —
        // `json_parses_on_both_subcommands_and_is_not_forwarded` pins that). The divergence is
        // deliberate and it is about ONE fact: the engine's `data hist rm` emits a machine document
        // carrying the RESOLVED store root and the rung that chose it, and this side cannot know
        // either — the engine resolves them in another process. `fetch` has no such document, so
        // its counts stay prose rather than becoming a second implementation of another crate's
        // sentences.
        argv.push("--json".to_string());
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

/// The LOCAL route's `--json` document: what was asked for, what ran, and the ENGINE's own document
/// nested whole.
///
/// ⚠ `engine_report` is the child's document PARSED, not its lines. That is the opposite of
/// [`report_json`]'s rule for `fetch`, and the difference is what is on the other end: `fetch`'s
/// engine prints PROSE, so reading numbers out of it would be a second implementation of another
/// crate's sentences; `data hist rm` prints a JSON document it owns, so nesting it hands a caller
/// the structure rather than a string to re-parse. A document that does not parse degrades to the
/// raw lines under `engine_report_lines`, so a caller is never handed a silently-empty object.
fn rm_json_local(
    args: &Args,
    rm: &RmArgs,
    program: &engine::Engine,
    argv: &[String],
    report: &[String],
) -> String {
    let joined = report.join("\n");
    let parsed = serde_json::from_str::<serde_json::Value>(&joined).ok();
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        "route": "engine",
        // What `--store` NAMED, or null. Never a path this side guessed at — the ENGINE resolves
        // the root, in another process, and its own document carries the answer.
        "store": args.store.clone(),
        "selector": rm_selector_json(rm),
        "produced_by": rm.produced_by.clone(),
        "dry_run": rm.dry_run,
        "engine": program.display(),
        "engine_argv": argv,
        "engine_report": parsed,
        "engine_report_lines": if parsed.is_some() { serde_json::Value::Null } else {
            serde_json::json!(report)
        },
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

/// The selector as this side parsed it — the four dimensions as their own fields, so a caller need
/// not re-split anything and an omitted (wildcarded) dimension is an explicit `null`.
fn rm_selector_json(rm: &RmArgs) -> serde_json::Value {
    serde_json::json!({
        "kind": rm.kind,
        "venue": rm.venue,
        "symbol": rm.symbol,
        "group": rm.group,
        "interval": rm.interval,
    })
}

/// The REMOTE route: ask a running `vike-datahub` to delete from the store IT has open.
///
/// # Two round trips, and the confirmation is bound to the first
///
/// The plan pass (`dry_run: true`) always runs, its lines are printed, and only then is the
/// operator asked. `--dry-run` stops there; `--yes` skips the asking. That is one more round trip
/// than the local route needs — the engine there prints its own plan and reads its own confirmation
/// in one process — and the reason it is worth it is that this side has no other way to know the
/// matched COUNT the typed line names.
///
/// ⚠ **The plan and the delete are two requests, so the store can move between them.** The
/// confirmation binds to what was SHOWN, not to what will be deleted, and nothing on this wire
/// makes those the same set. What DOES hold across the gap is the provenance assertion: the server
/// re-checks it under each series' own lock immediately before the removal, so a key committed in
/// between refuses that series rather than deleting it. Closing the count gap entirely means a
/// server-side token, which is a protocol change rather than a client one.
///
/// # ⚠ This route AUTHENTICATES now, and `keys` is how — it was a declared gap until it did
///
/// The verb is served ONLY by a KEYED server (`vike_datahub_client::proto`'s
/// `FEATURE_DELETE_SERIES` carries why), so until `vike-cli` could resolve
/// `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` this route could not reach a server that
/// serves it: every datahub caller used the plain unauthenticated `DatahubClient::connect`, which a
/// keyed server refuses. The fix could not live inside `src/cmd/` — an `env::var` here would be a
/// new `Layer::Library` row on `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchet,
/// and a credential-store read a new `CREDENTIAL_STORE_PIN` entry, both ratchets that may shrink
/// and never grow — so the keys arrive as a PARAMETER from the dispatcher, exactly as
/// `crate::cmd::mcp`'s `NodeKeyring` already did. `crate::Resolved::datahub_keys` is the field.
///
/// **The two refusals are still there and are still the right ones**, which is why neither was
/// deleted: a KEY-LESS server does not advertise the verb, so `DatahubClient::delete_series`
/// refuses before sending; and a keyed server asked for the wrong SCOPE refuses at the handshake.
/// [`connect`] asks for [`Scope::Write`] here — a delete is a write — so a key that only grants
/// Observe is refused at the socket rather than after a selector has travelled.
/// [`rm_remote_hint`] is the sentence that turns any of these into an instruction.
fn execute_rm_remote(args: &Args, rm: &RmArgs, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // ⚠ NAMED through `vike_datahub_client::proto`'s re-export, never through a `vike_data` edge —
    // this crate takes that dependency for DEV targets only, and the re-export exists so the wire's
    // vocabulary can be CONSTRUCTED here without one. See that re-export's own doc.
    use vike_datahub_client::proto::{SeriesSelector, describe_id};

    // Control: this route DELETES. A keyed server that would grant only Observe refuses here,
    // before a selector is sent — which is the refusal an operator wants to see.
    let mut client = connect(&args.addr, keys, Scope::Write)?;
    let selector = SeriesSelector {
        kind: rm.kind.clone(),
        venue: rm.venue.clone(),
        symbol: rm.symbol.clone(),
        group: rm.group.clone(),
        interval: rm.interval.clone(),
    };
    let planned = client
        .delete_series(&selector, rm.produced_by.as_deref(), true)
        .map_err(|e| CliError::failed(format!("{e}\n{}", rm_remote_hint())))?;
    let plan = planned.plan;

    // ⚠ The plan is SHOWN either way, and under `--json` it goes to STDERR rather than nowhere.
    // Stdout is the document and nothing else, but a run about to ask a human to type
    // `delete N series` must have shown them what N is made of — and the refusal below is only
    // legible beside the lines that carry it. Same split the ENGINE's own arm makes.
    let store_line = format!("store: the one the datahub at {} has open", args.addr);
    if args.json {
        // This side cannot name the store's PATH — that is the server's resolution, in another
        // process on another box — so it names the SERVER rather than guessing at a directory.
        eprintln!("{store_line}");
        for line in plan.lines() {
            eprintln!("{line}");
        }
    } else {
        println!("{store_line}");
        for line in plan.lines() {
            println!("{line}");
        }
    }
    // ⚠ **THE PROVENANCE VERDICT, checked HERE — where the ENGINE checks it, and where this route
    // did not.** `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` calls
    // `plan.verdict()` immediately after printing the plan and BEFORE the dry-run branch, so a
    // refused assertion exits without asking anybody anything. This route called it nowhere: it
    // printed the plan (which does render "provenance: REFUSED", so the text was on screen),
    // fell through to `confirm_typed`, made the operator type `delete N series` for a deletion
    // that could never happen, sent a second `delete_series`, and only then surfaced the server's
    // `execute_removal` error. Under `--yes` that is a wasted destructive round trip; without it,
    // a human is asked to authorise something already decided against. Under `--json` it was
    // worse still — the plan went nowhere and `rm_json_remote` had no `refused` field, so the
    // refusal reached a machine reader as a bare error string with no document at all.
    //
    // ⚠ **It REPORTS what the server already decided; it is not a client-side gate.** The plan
    // arrived over the wire, so this is a second evaluation of the assertion in a different
    // process from the one that will act. It can only ever make a run FAIL that the server had
    // already rendered as REFUSED in the very lines just printed. The authority is unchanged and
    // is on the far side: `vike_data::removal::execute_removal` re-checks, and
    // `DataFusionHist::delete_series_checked` re-checks again under the series lock.
    if let Err(refusals) = plan.verdict() {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, None, Some(&refusals)));
        }
        return Err(CliError::failed(
            "provenance REFUSED — nothing was deleted (the plan above names every series whose \
             commit keys do not carry --produced-by)",
        ));
    }
    if rm.dry_run {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, None, None));
        } else {
            println!("--dry-run: nothing was deleted");
        }
        return Ok(());
    }
    // "Nothing matched" is a SUCCESS, on the same rung as a delete — the server's delete is
    // idempotent and a cleanup that fails on re-run is a cleanup nobody re-runs.
    if plan.matched() == 0 {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, Some(&Default::default()), None));
        }
        return Ok(());
    }
    if !rm.yes {
        confirm_typed(plan.matched())?;
    }
    let done = client
        .delete_series(&selector, rm.produced_by.as_deref(), false)
        .map_err(CliError::failed)?;
    let outcome = done.outcome.unwrap_or_default();
    if args.json {
        println!("{}", rm_json_remote(args, rm, &done.plan, Some(&outcome), None));
    } else {
        for id in &outcome.deleted {
            println!("deleted {}", describe_id(id));
        }
        for (id, why) in &outcome.failed {
            eprintln!("FAILED {}: {why}", describe_id(id));
        }
        println!(
            "{} of {} series deleted",
            outcome.deleted.len(),
            outcome.deleted.len() + outcome.failed.len()
        );
    }
    // One broken series is one SKIPPED series: reported, the rest went, and the rung says look.
    if outcome.is_clean() {
        Ok(())
    } else {
        Err(CliError::failed(format!(
            "{} of {} series could not be deleted (see above); re-running finishes the job",
            outcome.failed.len(),
            outcome.failed.len() + outcome.deleted.len()
        )))
    }
}

/// The sentence appended to every remote-route refusal — what to do about it.
///
/// ⚠ **It named the wrong remedy between #1688 and #1691, which is the worst thing a refusal can
/// do.** It said "this CLI cannot yet authenticate to one" and sent the operator to the box. That
/// was true for three commits; #1691 taught `crates/vike-cli/src/lib.rs`'s `datahub_keyring` to
/// resolve the pair, so the CLI authenticates whenever the box sets the keys — and an operator
/// following the old sentence would have gone to the box rather than setting the two variables that
/// fix it.
///
/// ⚠ And then it named the wrong remedy a SECOND time, for a different reason, which is why this
/// paragraph is worth keeping rather than trimming. That resolver was a field called `datahub_keys`
/// reading the PROCESS ENVIRONMENT alone, while every refusal — this one included — told the
/// operator to put the keys in the CREDENTIAL STORE. Following the advice exactly left them broken.
/// #1701 made it the lazy function named above, env first and store second. Twice in one surface is
/// what a refusal costs when it is written from intent rather than from the resolver.
///
/// The refusal it is appended to has TWO causes and the sentence now separates them, because the
/// remedies differ: this side holding no key (set them here), or the datahub itself holding none
/// (a key-less server advertises no delete verb at all — `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`),
/// in which case no client-side change helps and the local route is the answer.
fn rm_remote_hint() -> String {
    "The remote delete is served only by a datahub that holds node keys. Either set \
     VIKE_DATAHUB_OBSERVE_KEY and VIKE_DATAHUB_CONTROL_KEY here so this CLI authenticates, or — if \
     the datahub itself holds none, in which case it advertises no delete verb at all — run the \
     delete ON that box: `vike-cli data hist rm --store DIR …`, over `ssh` if it is remote."
        .to_string()
}

/// Read one line and require it to equal `delete N series`.
///
/// ⚠ Binding the confirmation to a FACT OF THE PLAN is what makes a line copied from a previous run
/// against a different plan fail to match — the same property the MCP surface's `preview_token`
/// buys, with no token and no state. The caller has already refused the no-terminal case, so this
/// is only ever reached with a person on the other end.
fn confirm_typed(matched: usize) -> CmdResult<()> {
    let want = format!("delete {matched} series");
    eprintln!("type `{want}` to confirm, or anything else to abort:");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| CliError::failed(format!("reading the confirmation: {e}")))?;
    if line.trim() == want {
        Ok(())
    } else {
        Err(CliError::failed(format!("not confirmed (expected `{want}`) — nothing was deleted")))
    }
}

/// The REMOTE route's `--json` document. Unlike the local route's, this side BUILDS it: the answer
/// arrives as typed data over the wire rather than as another process's stdout, so there is nothing
/// to nest and nothing to re-parse.
///
/// ⚠ **`refused` is new and it closes a hole, not a gap in symmetry.** The ENGINE's document
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `rm_series_json`) has always carried the
/// per-series provenance refusals; this one had no field for them because this route never
/// consulted `RemovalPlan::verdict` at all. A machine reader driving `--json --addr` therefore saw
/// a provenance refusal as a bare error string with no document — the one failure whose whole
/// value is the LIST of which series objected.
///
/// ⚠ Three `--json` shapes still exist for this one verb (engine, local-nested, remote-built) and
/// this PR does not fold them: the engine's carries the resolved store ROOT and its rung, which
/// this side cannot know, and the remote's carries `addr` and `plan`, which the engine's has no
/// use for. A caller branches on `route`. Recorded rather than silently tolerated.
fn rm_json_remote(
    args: &Args,
    rm: &RmArgs,
    plan: &vike_datahub_client::proto::RemovalPlan,
    outcome: Option<&vike_datahub_client::proto::RemovalOutcome>,
    refused: Option<&[String]>,
) -> String {
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        "route": "datahub",
        // ⚠ `store` is the ADDRESS, never a path: the resolved root is the SERVER's, and a
        // directory named here would be this side's guess about another box's filesystem — the
        // exact class of confident-wrong answer `report_json`'s `store: null` rule exists for.
        "addr": args.addr,
        "store": serde_json::Value::Null,
        "selector": rm_selector_json(rm),
        "produced_by": rm.produced_by.clone(),
        "dry_run": rm.dry_run,
        "matched": plan.matched(),
        "rows": plan.rows(),
        "bytes": plan.bytes(),
        "series": plan.series,
        "plan": plan.lines(),
        "outcome": outcome,
        // `null` when the assertion held; the per-series refusals otherwise — the same field, with
        // the same meaning, the engine's own document carries.
        "refused": refused,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

// ─── `repair`: the one verb that reaches a series the store cannot ENUMERATE ────────────────────

/// `data hist repair` — rebuild ONE series' manifest from its parts, through the engine.
///
/// # ⚠ It REHEARSES by default, and that is a different rule from `rm`'s
///
/// [`execute_rm`] refuses without `--yes` because somebody typing `rm` means to delete and the
/// danger is doing it unseen. This verb prints the plan and exits 0 without `--yes`, because the
/// danger here is the opposite: a rebuild can SUCCEED and still cost the series its idempotency
/// log — `RebuildReport::parts_without_keys` — after which re-running a backfill re-admits an
/// already-applied append and DUPLICATES rows. The fact an operator needs is therefore the
/// VERDICT, and the verdict is knowable only by running the pass the write runs. The rehearsal IS
/// that pass: lock-free, writing nothing, costing a live writer nothing.
///
/// So there is no TTY refusal here and deliberately none. `rm`'s exists because a confirmation read
/// from a PIPE is not a confirmation; this verb asks for no confirmation at all, and a line without
/// `--yes` is already the safe one. Adding a refusal would make the SAFE spelling the one that
/// fails in CI.
///
/// # ⚠ ONE route, and `--addr` is refused at [`parse`]
///
/// [`refuse_the_remote_route_on_repair`] carries the three-part argument. The short form: a
/// datahub can only name series it ENUMERATED, and the series this repairs is by definition one no
/// enumeration shows.
///
/// # What this side does NOT do
///
/// It resolves no store root, opens nothing and judges no series. The whole of the work — the plan,
/// the lock, the rebuild and the verdict — is `crates/vike-backtest/src/backtest_cli.rs`'s
/// `run_repair_series`, for [`execute_engine`]'s reason: opening a hist store needs DataFusion and
/// this binary links none. The engine's non-zero exit for a LOSSY rebuild folds onto
/// [`crate::exit::Exit::Failed`] through `crate::cmd::engine`'s `fold_status`, exactly as `rm`'s
/// partial-failure rung does.
fn execute_repair(args: &Args, project_root: Option<&Path>) -> CmdResult<()> {
    let repair = args.repair.as_ref().expect("`parse` builds a RepairArgs for every Sub::Repair");
    let program = engine::locate(args.engine.as_deref(), project_root);
    let argv = repair_engine_argv(args, repair);
    // ⚠ NO stdin is passed through, unlike [`execute_rm_local`]: the engine's `data hist repair` arm
    // reads no confirmation, so a child holding this process's terminal would be a door nothing
    // opens.
    if !args.json {
        return engine::run(&program, &argv, "data");
    }
    let report = engine::run_capturing_stdout(&program, &argv, "data")?;
    println!("{}", repair_json_local(args, repair, &program, &argv, &report));
    Ok(())
}

/// The engine argv `repair`'s arguments become — PURE, so the translation is unit-tested rather
/// than only observed through a spawn. `backtest data repair …`; see [`engine_argv`].
///
/// ⚠ `--dry-run` is forwarded when the operator wrote it AND when they wrote neither flag, so the
/// engine is told in one word what this side decided. The alternative — forwarding nothing and
/// letting the engine's own default rehearse — makes the child's behaviour depend on a default
/// this side is also documenting, which is two places for one rule.
fn repair_engine_argv(args: &Args, repair: &RepairArgs) -> Vec<String> {
    let mut argv = vec![
        ENGINE_DATA_VERB.to_string(),
        Sub::Repair.as_str().to_string(),
        "--kind".to_string(),
        repair.kind.clone(),
        "--venue".to_string(),
        repair.venue.clone(),
    ];
    for (flag, value) in
        [("--symbol", &repair.symbol), ("--group", &repair.group), ("--interval", &repair.interval)]
    {
        if let Some(v) = value {
            argv.push(flag.to_string());
            argv.push(v.clone());
        }
    }
    // ⚠ `--dry-run` WINS over `--yes` on BOTH sides, and it is spelled here as well as there so a
    // reader of either argv can see which one was decided. `repair.dry_run || !repair.yes` is the
    // rehearsal condition — the exact complement of the engine's `repair_writes`.
    if repair.dry_run || !repair.yes {
        argv.push("--dry-run".to_string());
    } else {
        argv.push("--yes".to_string());
    }
    if args.json {
        // FORWARDED, for [`rm_engine_argv`]'s reason: the engine's `data hist repair` emits a machine
        // document carrying the RESOLVED store root, the rung that chose it, and the plan — none
        // of which this side can know, because the engine resolves them in another process.
        argv.push("--json".to_string());
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

/// `repair`'s `--json` document: what was asked for, what ran, and the ENGINE's own document
/// nested whole.
///
/// The shape and the reasoning are [`rm_json_local`]'s unchanged — the child prints a JSON document
/// it owns, so nesting it hands a caller the structure rather than a string to re-parse, and a
/// document that does not parse degrades to the raw lines.
fn repair_json_local(
    args: &Args,
    repair: &RepairArgs,
    program: &engine::Engine,
    argv: &[String],
    report: &[String],
) -> String {
    let joined = report.join("\n");
    let parsed = serde_json::from_str::<serde_json::Value>(&joined).ok();
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        // Spelled even though there is only one, because a caller branching on `route` across the
        // `data` verbs should not have to special-case the one that omits the field.
        "route": "engine",
        "store": args.store.clone(),
        "series": {
            "kind": repair.kind,
            "venue": repair.venue,
            "symbol": repair.symbol,
            "group": repair.group,
            "interval": repair.interval,
        },
        // What this side DECIDED, which is what the child was told — not what was typed. A caller
        // reading `false` here knows no write was attempted without having to re-derive the
        // dry-run-wins rule.
        "writes": !(repair.dry_run || !repair.yes),
        "engine": program.display(),
        "engine_argv": argv,
        "engine_report": parsed,
        "engine_report_lines": if parsed.is_some() { serde_json::Value::Null } else {
            serde_json::json!(report)
        },
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

/// Build the child's argv and hand it to [`crate::cmd::engine`]. Nothing else happens on this
/// path: the store is opened, written and reported on by the engine, whose streams this process
/// inherits.
///
/// ⚠ Under `--json` the child's STDOUT is read back instead of inherited, and this process emits
/// [`report_json`] there instead — because stdout under `--json` is the document and nothing else,
/// the same rule `crate::cmd::secrets`'s `list` and `crate::cmd::init` follow. The engine's report
/// is not lost: [`engine::run_capturing_stdout`] echoes every line to stderr as it arrives, and the
/// document carries the same lines verbatim.
fn execute_engine(args: &Args, project_root: Option<&Path>) -> CmdResult<()> {
    let program = engine::locate(args.engine.as_deref(), project_root);
    let argv = engine_argv(args);
    if !args.json {
        return engine::run(&program, &argv, "data");
    }
    let report = engine::run_capturing_stdout(&program, &argv, "data")?;
    println!("{}", report_json(args, &program, &argv, &report));
    Ok(())
}

/// The `--json` document: what was asked for, what ran, and what the engine said about it.
///
/// # What is in it, and what each field is honest about
///
/// * `store` is what `--store` NAMED, or `null` when the flag was absent. It is not a resolved
///   path, and must not be printed as one: with no `--store` the ENGINE resolves the root (its
///   `--store` > `$VIKE_HIST_STORE` > default chain), in a different process, and this side would
///   be guessing. `null` hands a caller the absence rather than a sentence to pattern-match — the
///   same shape `crate::cmd::secrets`'s `list --json` gives an absent store.
/// * `series` and `window` are the parsed request, split into fields so a caller need not re-split
///   `VENUE:SYMBOL:INTERVAL` or guess which window form was used. Both are `null` for `seed-demo`,
///   which takes neither (the parser REFUSES a spec or a window there rather than ignoring one).
/// * `engine` and `engine_argv` are the command this process actually ran. The verb's whole product
///   is that argv — `crates/vike-cli/tests/data_cli.rs`'s
///   `fetch_and_seed_demo_reach_the_engine_as_its_own_flags` says so for the human path — so a
///   machine reader gets it rather than having to infer it.
/// * `report` is the engine's own stdout, line by line, VERBATIM.
///
/// ⚠ **`report` is where the counts are, and they are not parsed into fields.** `data hist fetch`
/// prints `N bars returned … M rows written` and `data seed-demo` prints a line per slice; neither has
/// a `--json` mode of its own, so the only way to field those numbers would be to read them out of
/// the sentences — a second implementation of another crate's output format, in a crate that cannot
/// see it change, which would start reporting a WRONG count rather than failing on the day a word
/// moves. Giving a caller the lines is honest; claiming to have understood them would not be. The
/// day the engine grows a machine report for these two paths, this field becomes structured and
/// the change is one function.
fn report_json(
    args: &Args,
    program: &engine::Engine,
    argv: &[String],
    report: &[String],
) -> String {
    let doc = serde_json::json!({
        // [`Sub::as_str`], not a second table: the refusals in `parse` name a subcommand by that
        // one spelling, and a document that named it differently would be the same verb under two
        // names in one session.
        "subcommand": args.sub.as_str(),
        "store": args.store.clone(),
        "series": args.spec.as_deref().map(|spec| {
            // Three non-empty parts by construction: `check_spec` refused anything else before a
            // process was started, so this split cannot be partial.
            let mut parts = spec.splitn(3, ':');
            serde_json::json!({
                "venue": parts.next().unwrap_or_default(),
                "symbol": parts.next().unwrap_or_default(),
                "interval": parts.next().unwrap_or_default(),
                // The spelling the operator typed, kept beside the split so a caller echoing the
                // request back does not have to reassemble it.
                "spec": spec,
            })
        }),
        // ⚠ TWO shapes and a null, because two subcommands bound a range and they bound
        // DIFFERENT things: `fetch`'s [`Window`] is one form or the other and is REQUIRED;
        // `export`'s [`ExportRange`] is two independent optional bounds over what is already on
        // disk. Rendering them as one shape would make a caller unable to tell an absent bound
        // from an absent window.
        "window": match (&args.window, &args.export_range) {
            (Some(Window::Days(d)), _) => serde_json::json!({ "days": d }),
            (Some(Window::Range { from, to }), _) => serde_json::json!({ "from": from, "to": to }),
            (None, Some(range)) => serde_json::json!({ "from": range.from, "to": range.to }),
            (None, None) => serde_json::Value::Null,
        },
        // `export`'s destination, and `null` everywhere else — the one fact a caller driving an
        // export needs back and cannot re-derive from the report prose.
        "out": args.out.clone(),
        // ⚠ THE AXIS, and it is in the document because the collapse would otherwise LOSE
        // information a caller already had. Before it, `subcommand` alone distinguished
        // `seed-demo` from `fetch-starter`; now both are `fetch` and only this field separates
        // them. `null` for a venue fetch and for every other verb — the flag names a choice only
        // `fetch` makes, so a value elsewhere would assert one nobody took.
        "source": match (args.sub, args.source) {
            (Sub::Fetch, Source::Starter) => serde_json::json!("starter"),
            (Sub::Fetch, Source::Demo) => serde_json::json!("demo"),
            _ => serde_json::Value::Null,
        },
        "engine": program.display(),
        "engine_argv": argv,
        "report": report,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

/// The engine argv this verb's arguments become — PURE, so the translation is unit-tested rather
/// than only observed through a spawn.
///
/// ⚠ **It is `backtest data <sub> …` since ruling 12, and the translation is now a RENAME rather
/// than a re-shape.** The engine used to carry `--fetch`/`--seed-demo`/`--export`/`--fetch-starter`
/// as flags on the backtest verb; it carries a `data` SUBCOMMAND now
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `run_data`), spelled with the same words as this
/// side. So each line below reads as the command the operator typed, which is the property that
/// makes an argv assertion in `crates/vike-cli/tests/data_cli.rs` legible as a contract rather than
/// as a mapping table.
///
/// ⚠ **The engine flags did NOT retire from the SPAWN — only from the engine's own help.** This
/// argv is what makes `vike-cli data` work at all: opening a hist store needs DataFusion and this
/// crate links none. Deleting the engine's `data` subcommand deletes these verbs with it.
fn engine_argv(args: &Args) -> Vec<String> {
    // ⚠ The CHILD's verb, which is NOT always ours. `fetch --source starter` spawns the engine's
    // own `fetch-starter`: that binary has its own surface and its own compatibility story, and
    // collapsing our three verbs into one must not collapse its. [`Source::engine_verb`] is the
    // seam, and a test asserts the child argv rather than trusting this line.
    let child_verb = args.source.engine_verb().unwrap_or_else(|| args.sub.as_str());
    let mut argv = vec![ENGINE_DATA_VERB.to_string(), child_verb.to_string()];
    match args.sub {
        Sub::Export => {
            // Both present by construction: `parse` refuses an `export` without a spec or `--out`.
            argv.push(args.spec.clone().unwrap_or_default());
            argv.push("--out".to_string());
            argv.push(args.out.clone().unwrap_or_default());
            // ⚠ INDEPENDENTLY, unlike `fetch`'s window — see [`ExportRange`] for why an export's
            // two bounds are neither paired nor required.
            if let Some(range) = &args.export_range {
                for (flag, value) in [("--from", &range.from), ("--to", &range.to)] {
                    if let Some(v) = value {
                        argv.push(flag.to_string());
                        argv.push(v.clone());
                    }
                }
            }
        }
        // ⚠ THE ONLY OTHER REACHABLE ARM, and it carries no argument at all. `fetch` reaches this
        // function ONLY under `--source starter|demo` ([`execute`] sends `Source::Venue` to a
        // datahub instead), and those two sources take no spec and no window — the whole of their
        // argv is the child verb plus `--store`. [`Source::engine_verb`] has already chosen that
        // child verb above, which is why one arm serves both.
        //
        // ⚠ This used to push a spec and a window, back when `fetch` drove the engine against a
        // local store. It does not any more, and the flags did not move here — they went to
        // `Request::Backfill`. An `if let Some(spec)` left behind would hand the child a starter
        // pull with a series name on it.
        Sub::Fetch => {}
        // ⚠ UNREACHABLE: [`execute`] routes the read verbs to their own arms, and `rm`/`repair`
        // build their argv in [`rm_engine_argv`] / [`repair_engine_argv`] — each selector has flags
        // of its own and folding them in here would make one function answer for three grammars.
        // Named rather than folded into a `_`, deliberately: a wildcard would silently absorb a
        // future subcommand that DOES need an engine flag and hand the child an argv with the flag
        // missing instead of failing to compile — which is exactly what it would have done to
        // `Sub::Gaps`.
        Sub::Get
        | Sub::List
        | Sub::Gaps
        | Sub::Coverage
        | Sub::TapeHealth
        | Sub::Universe
        | Sub::Gate
        | Sub::Rm
        | Sub::Repair => {}
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

// ─── the read half: two verbs over a running datahub ────────────────────────────────────────────

/// Open the datahub connection both read verbs need, classifying a failure at the SOCKET — where
/// the address is still in scope, so the message can name what was unreachable.
///
/// ⚠ The sentence is deliberately word-for-word `crate::cmd::backtest`'s. Four verbs in this crate
/// now dial a datahub and a wrapper should not have to learn four spellings of "it was not there"
/// to decide whether to back off — that is what [`crate::exit::Exit::Connect`] promises, and a
/// per-verb wording would leave the rung carrying the promise alone.
/// ⚠ `keys` decides WHICH constructor runs, and the absent case is the unchanged one: `None` dials
/// `DatahubClient::connect`, exactly as every caller here did before the dispatcher learned to
/// resolve the datahub pair, so a key-less server behaves identically. `Some` dials
/// `connect_authed` at the `scope` the CALLER names — a read verb asks for [`Scope::Read`] and
/// gets refused by a server that would only grant it control, which is the right way round.
///
/// The keys reach here as a parameter rather than an `env::var`, and `crate::Resolved::datahub_keys`
/// carries the two ratchets that make that mandatory rather than stylistic.
fn connect(addr: &str, keys: Option<&NodeKeys>, scope: Scope) -> CmdResult<DatahubClient> {
    match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, scope),
        None => DatahubClient::connect(addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to datahub at {addr}: {e}")))
}

// ─── `get`: the rows themselves ─────────────────────────────────────────────────────────────────

/// `data hist get SPEC …` — read one bar series' rows over a bounded window and print them.
///
/// ⚠ **The ONE wire call is [`vike_datahub_client::DatahubClient::load_bars_ms`]**, the epoch-ms
/// sibling that exists because `load_bars` takes a `vike_data::TsRange` this crate cannot NAME
/// (that method's doc carries the wall and why closing it needed no protocol arm). [`Scope::Read`]
/// because this reads: `crates/vike-datahub-client/src/proto.rs`'s `required_scope` puts
/// `LoadBars` beside `Inventory` and `SeriesGaps`, which is the same scope every other read verb
/// here asks for.
///
/// ⚠ **Which stream carries the NOTES depends on the rendering, and that is the contract rather
/// than a preference.** Under `jsonl`, stdout is rows and only rows — a `| jq` must not have to
/// strip our prose and a `> rows.jsonl` must be data — so the counts and the disclosures go to
/// stderr. Under `table` they are part of the answer a person is reading, and under `json` they
/// are FIELDS of the one document, which is why that form prints no note to either stream. Same
/// split `realtime`'s `watch` makes, for the same reason.
///
/// ⚠ **"FIELDS" covers BOTH disclosures, and it once covered only one.** The ceiling is
/// `returned` beside `shown`; the EMPTY reading is [`get::json_doc`]'s `note`, which that function
/// grew because nothing in the counts can say which of the two facts an empty answer is. Until it
/// did, a `--json` run over a typo'd symbol printed `returned: 0, bars: []` and exited 0 with
/// NOTHING on either stream, while the equally machine-facing `jsonl` arm below put the same note
/// on stderr — two machine forms disagreeing about whether the ambiguity is worth stating.
fn execute_get(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let g = args.get.as_ref().expect("`parse` builds a GetArgs for every Sub::Get");
    // The clock is read HERE rather than in `parse`, which is pure — see [`GetArgs`].
    // `vike_model::clock::now_ms` is the workspace's one sanctioned read and this crate already
    // makes several, so this adds no `crates/vike-ops/tests/clock_pin.rs` row.
    let bounds = g.window.bounds(vike_model::clock::now_ms());
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    // A server-side `Response::Error` and a protocol desync both arrive as `Err(String)` and both
    // stay on the run-failure rung: once the connection is open, a failure is the request's and not
    // the connection's — the ladder the whole read half follows.
    let bars = client.load_bars_ms(
        &g.spec.venue,
        &g.spec.symbol,
        &g.spec.interval,
        bounds.0,
        bounds.1,
        None,
    )?;
    let returned = bars.len();
    // ⚠ §8.2's ceiling, applied HERE and STILL applied here now that the wire HAS a row cap
    // (`vike_datahub_client::FEATURE_SCAN_LIMIT`). Pushing it down would be the obvious move and
    // it is the wrong one: the cap's whole purpose is to bound a FRAME, so a capped read returns
    // a page and cannot say how many rows the store holds. `returned` is that real total, and it
    // is what lets the disclosure below name an exact number rather than "there may be more".
    // The residual the client-side ceiling leaves is in [`get`]'s module doc, unchanged.
    let shown = &bars[..returned.min(g.limit)];

    match g.render {
        get::Render::Json => {
            let doc = get::json_doc(
                // DERIVED from the verb, never re-typed — the correction `universe_json` carries.
                args.sub.as_str(),
                &args.addr,
                &g.spec,
                bounds,
                g.limit,
                returned,
                shown,
            );
            println!("{doc}");
        }
        get::Render::Jsonl => {
            for bar in shown {
                println!("{}", get::json_row(&g.spec, bar));
            }
            // STDERR, so stdout is the tape. Both notes, because a pipeline reader has exactly the
            // same two questions a person does: was this cut, and does empty mean absent.
            if returned == 0 {
                eprintln!("{}", get::empty_note(&g.spec));
            }
            if let Some(note) = get::ceiling_note(returned, g.limit, g.limit_defaulted) {
                eprintln!("{note}");
            }
        }
        get::Render::Table => {
            if returned == 0 {
                println!("{}", get::empty_note(&g.spec));
            } else {
                for line in get::table_lines(&g.spec, shown) {
                    println!("{line}");
                }
            }
            if let Some(note) = get::ceiling_note(returned, g.limit, g.limit_defaulted) {
                println!("{note}");
            }
        }
    }
    Ok(())
}

/// `export --addr` — THE WALK: open `--out`, ask for one window at a time, append, repeat.
///
/// ⚠ **The file is opened BEFORE the first request and written window by window**, which is what
/// makes the memory bound real: at most one window's rows are alive at a time, and a row that has
/// been written is dropped. Collecting the whole range and writing once would be the same code
/// shape with none of the property — and would reintroduce exactly the unbounded client-side
/// buffer [`export`]'s module doc argues this design avoids.
///
/// ⚠ **A failure mid-walk leaves a PARTIAL file, and that is stated rather than cleaned up.**
/// Deleting it would destroy rows the operator may want (a 12-hour pull that died at hour 11 is
/// mostly good data), and writing to a temp file and renaming would hold the whole export in a
/// second copy on disk. So the error names the window that failed and the bound to resume from —
/// [`export::read_failed_note`] — and the file is left where it is.
///
/// ⚠ **The CSV header is written even when the walk returns nothing**, so an empty export is a
/// one-line file rather than a zero-byte one indistinguishable from a crash. `jsonl` has no header
/// and genuinely is empty; [`export::summary`] says which of the two happened.
fn execute_export_remote(args: &Args, e: &export::Plan, keys: Option<&NodeKeys>) -> CmdResult<()> {
    use std::io::Write as _;

    let out_path = args.out.as_deref().expect("`parse` refuses an export without --out");
    // ⚠ Opened BEFORE the socket, deliberately: an unwritable destination is a fact this side can
    // establish for free, and discovering it after a multi-window pull would throw the pull away.
    let file = std::fs::File::create(out_path).map_err(|err| {
        CliError::usage(format!(
            "could not open --out {out_path:?} for writing ({err}). A remote export streams into \
             that file as it walks, so it is opened before the first request rather than after \
             the last."
        ))
    })?;
    let mut sink = std::io::BufWriter::new(file);

    // The header belongs to the FORMAT, not to the rows — see this function's doc for why it lands
    // before the walk rather than beside the first row that arrives.
    if e.wire == export::Wire::Csv {
        writeln!(sink, "{}", export::csv_header(e.kind)).map_err(write_failed(out_path))?;
    }

    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let walk = export::windows(e.bounds.0, e.bounds.1, e.step_ms);
    let mut rows = 0usize;
    for (lo, hi) in &walk {
        // ⚠ ONE match rather than three loops, so the walk, the write and the counting are written
        // once and the KIND only chooses which RPC and which roster. Three copies is how a fix to
        // the resume hint would land in one lane and not the others.
        let cells: Vec<export::Cells> = match e.kind {
            export::Kind::Bar => {
                let interval =
                    e.spec.interval.as_deref().expect("`parse_spec` gives a bar spec an interval");
                client
                    .load_bars_ms(
                        &e.spec.venue,
                        &e.spec.symbol,
                        interval,
                        Some(*lo),
                        Some(*hi),
                        None,
                    )
                    .map_err(read_failed(e, *lo, *hi))?
                    .iter()
                    .map(|b| export::bar_cells(&e.spec, b))
                    .collect()
            }
            export::Kind::Quote => client
                .scan_quotes_ms(&e.spec.venue, &e.spec.symbol, Some(*lo), Some(*hi), None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|q| export::quote_cells(&e.spec, q))
                .collect(),
            export::Kind::Trade => client
                .scan_trades_ms(&e.spec.venue, &e.spec.symbol, Some(*lo), Some(*hi), None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|t| export::trade_cells(&e.spec, t))
                .collect(),
        };
        for row in &cells {
            let line = match e.wire {
                export::Wire::Jsonl => export::jsonl_line(row),
                export::Wire::Csv => export::csv_line(row),
            };
            writeln!(sink, "{line}").map_err(write_failed(out_path))?;
        }
        rows += cells.len();
    }
    sink.flush().map_err(write_failed(out_path))?;

    let written = export::Written { rows, windows: walk.len() };
    if args.json {
        println!(
            "{}",
            export::json_doc(
                // DERIVED from the verb, never re-typed — `universe_json`'s correction.
                args.sub.as_str(),
                &args.addr,
                out_path,
                e,
                &written,
            )
        );
    } else {
        for line in export::summary(e, out_path, &written) {
            println!("{line}");
        }
        if let Some(note) = export::step_note(e, &written) {
            println!("{note}");
        }
    }
    Ok(())
}

/// The write-failure classifier — a closure so the path is named once and every `writeln!` in the
/// walk reports the same way.
///
/// ⚠ It says the partial file is DATA rather than wreckage, because that is the choice
/// [`execute_export_remote`]'s doc makes and an operator who is not told will delete it.
fn write_failed(out_path: &str) -> impl Fn(std::io::Error) -> CliError + '_ {
    move |err| {
        CliError::failed(format!(
            "writing to --out {out_path:?} failed ({err}). Rows already written are still there — \
             this verb streams, so a partial file is partial DATA rather than a corrupt one."
        ))
    }
}

/// The read-failure classifier, which renders [`export::read_failed_note`] at the run rung.
///
/// The rung is the RUN failure rather than [`Exit::Connect`] for [`connect`]'s stated reason: once
/// the connection is open a failure is the REQUEST's and not the connection's, which is the ladder
/// the whole read half follows.
fn read_failed(e: &export::Plan, lo: i64, hi: i64) -> impl Fn(String) -> CliError + '_ {
    move |why| CliError::failed(export::read_failed_note(&e.spec.text(), lo, hi, e.step_ms, &why))
}

/// One series' cheap coverage, flattened out of the wire type on arrival — see the module doc for
/// why the read half carries its own row types.
///
/// `bytes`/`parts`/`dates` are ON-DISK facts (summed part-file size, part count, `date=` partition
/// count), so a store that has no files answers 0 for all three while still reporting real rows.
/// They are carried into [`list_json`] and left out of the human table, which has room for the
/// three numbers a person browsing actually asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Coverage {
    first_ts: i64,
    last_ts: i64,
    rows: u64,
    bytes: u64,
    parts: usize,
    dates: usize,
}

/// The as-of instant EVERY class probe uses: the latest properties row the store holds.
///
/// ⚠ **Not the series' own `last_ts`, and the difference is not cosmetic.** Two reasons, and the
/// second is the one that would be a BUG rather than a preference:
///
/// 1. The question this column answers is *does this instrument have a class on record* — the
///    operator-facing half of `vike_model::SymbolProperties::asset_class`, whose own doc says a
///    producer that does not know must leave it `None`. A properties row written AFTER an
///    instrument's tape stopped is still on record, and asking as of the tape's end would render it
///    `no-properties`, which reads as "the venue producer is not wired" — the exact false negative
///    this flag exists to make visible.
/// 2. A `list` row is a SERIES, and one instrument's series end at different instants (its `1h`
///    bars, its `trade` tape and its `depth` tape each have their own `last_ts`). A per-row instant
///    would let the SAME instrument render two different classes in one table, and would make
///    [`execute_list`]'s per-instrument probe cache unsound — the cache is only correct because
///    every row of one instrument asks the same question.
///
/// `HistStore::properties_as_of` folds this into `TsRange::of(i64::MIN, ts)`, so the bound is
/// inclusive and nothing here overflows.
const CLASS_AS_OF_TS: i64 = i64::MAX;

/// What the store's `kind=properties` tape says about ONE instrument's asset class — the READER
/// `vike_model::SymbolProperties::asset_class` did not have until this flag.
///
/// ⚠ **Five outcomes, and every one of them is SAID rather than left to an empty cell.** The field
/// is an `Option` in the model precisely so that "the venue told us" and "nobody said" stay
/// distinguishable (that field's doc argues it at length), and a renderer that folded both into a
/// blank would undo the distinction on the way to the terminal — which is the same defect
/// `vike_data::KindDays::absent` names for a kind that was never recorded. Here the absence splits
/// THREE ways, not two, and the three call for different actions: fix the producer, run the
/// recorder, or nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassProbe {
    /// A properties row exists and NAMES a class. Carried as the variant's own
    /// `vike_model::AssetClass::sql_word`, which is also its serde word — never a second spelling
    /// minted here.
    Classified(&'static str),
    /// A properties row exists and its `asset_class` is `None`. **This is the wiring signal**: the
    /// venue producer recorded an instrument grid and named no class for it.
    Unclassified,
    /// No properties row at all for this `(venue, symbol)` — nothing has ever recorded a grid for
    /// this instrument, which is a RECORDER gap rather than a producer one.
    Unrecorded,
    /// A GROUPED series: its name is a group, not a symbol, so no lookup was made. Asking
    /// `properties_as_of(venue, group)` would answer `None` for a question that was malformed, and
    /// that `None` is indistinguishable from [`ClassProbe::Unrecorded`] — a wrong answer dressed as
    /// a real one, plus a round trip spent to get it.
    Grouped,
    /// The probe failed for this instrument. Degrades the ROW, never the run — see [`execute_list`].
    Failed(String),
}

impl ClassProbe {
    /// The probe's verdict as the `--json` document spells it. A machine reader gets WHICH answer
    /// this was, so an absent class can never be read as a present-but-empty one.
    fn status(&self) -> &'static str {
        match self {
            ClassProbe::Classified(_) => "classified",
            ClassProbe::Unclassified => "unclassified",
            ClassProbe::Unrecorded => "unrecorded",
            ClassProbe::Grouped => "grouped",
            ClassProbe::Failed(_) => "error",
        }
    }

    /// The class word itself — `Some` for exactly one variant, which is what makes
    /// `asset_class != null` mean `status == "classified"` and nothing else.
    fn word(&self) -> Option<&'static str> {
        match self {
            ClassProbe::Classified(w) => Some(w),
            _ => None,
        }
    }
}

/// One row of `ls` or `gaps`: a series' identity, its coverage, and — only under `gaps` — its holes.
///
/// ⚠ `name`/`grouped` are DERIVED from `symbol`/`group` at the one conversion site, and all four
/// are kept. The pair is what the renderers use (exactly one of symbol/group is meaningful, and
/// resolving that once beats resolving it at every render site); the raw fields are what
/// [`list_json`] emits, so a machine reader gets the identity rather than this side's reading of
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeriesRow {
    kind: String,
    venue: String,
    /// The symbol for a per-symbol series, the group for a grouped one — `SeriesId::label()`.
    name: String,
    /// `true` when this series is a `group=` directory holding many symbols in one part.
    grouped: bool,
    /// The RAW symbol: EMPTY for every grouped series, which is why it is not what `name` reads.
    symbol: String,
    /// The RAW group: `Some` exactly when `grouped`.
    group: Option<String>,
    /// `Some` for bars (which sub-partition by bar step), `None` for every tick-shaped kind.
    interval: Option<String>,
    coverage: Coverage,
    /// Inclusive epoch-ms ranges MISSING inside the recorded span. `None` means the question was
    /// not asked (this is `ls`) or could not be answered — `gaps_error` tells those apart, and an
    /// EMPTY `Some` is the real "this series has no holes".
    gaps: Option<Vec<(i64, i64)>>,
    /// Why this one series' gap probe failed, when it did. See [`execute_list`] for the degrade.
    gaps_error: Option<String>,
    /// The recorded asset class for this row's INSTRUMENT. `None` means the question was not asked
    /// (no `--class`) — every way of it having been asked and answered, failure included, is a
    /// [`ClassProbe`] variant, so this side needs no sibling `class_error` field the way `gaps`
    /// does.
    class: Option<ClassProbe>,
}

/// `data hist ls` AND `data hist gaps` — one `inventory()` round trip, filtered client-side, plus
/// one `series_gaps` probe per MATCHED series under the latter.
///
/// ⚠ **A failed gap probe degrades the ROW; it does not fail the run.** The listing is what was
/// asked for and it is complete and correct; the gaps are an annotation on it, and one series whose
/// manifest cannot be read should not make `data hist list` unusable against a store of a thousand. The
/// failure is not swallowed either — it lands on the row it belongs to in both renderings, and
/// [`SeriesRow::gaps`] stays `None` so a machine reader can never mistake it for "no holes". Same
/// contract, and the same argument, as `crates/vike-app-core/src/stored_load.rs`'s
/// `load_stored_tree`, which the Data Manager runs over the same two verbs.
///
/// The rejected alternative was exiting on the run-failure rung with the table still emitted: that
/// makes a wrapper treat a complete listing as no listing, and there is no rung for "mostly".
fn execute_list(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    refuse_an_account_kind_filter(args)?;
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    // A server-side `Response::Error` and a protocol desync both arrive as `Err(String)` and both
    // stay on the pre-existing rung: once the connection is open, a failure is the request's and
    // not the connection's.
    let inventory = client.inventory()?;
    let reported = inventory.len();

    // One entry per DISTINCT `(venue, symbol)`, so an instrument recorded as five series costs one
    // round trip rather than five — see [`CLASS_AS_OF_TS`] for why every row of one instrument is
    // genuinely asking the same question. A failed probe is cached like any other answer: a store
    // that cannot answer for an instrument will not answer on the retry either, and re-asking would
    // multiply one failure into one round trip per series of it.
    let mut class_cache: std::collections::BTreeMap<(String, String), ClassProbe> =
        std::collections::BTreeMap::new();

    // ⚠ The ACCOUNT-kind exclusion, and it is ACTIVE rather than a filter default — the surface
    // design's §9.3.2. The store is SHARED: `exec_fill`, `exec_order`, `exec_funding` and `equity`
    // sit in the same tree as market data, and they are YOUR OWN activity rather than the market's.
    // This plane does not serve them; `vike-cli account` will.
    //
    // ⚠ Counted rather than silently dropped, because an operator can see those series exist with
    // their own eyes — `ls` is what they would use to look. A silent filter would make this verb
    // UNDER-REPORT a store rather than scope itself, which is the failure §9.3.2 exists to prevent.
    // The disclosure below is therefore part of the contract, not a nicety.
    let mut excluded: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut excluded_count = 0usize;

    let mut rows: Vec<SeriesRow> = Vec::new();
    for (id, cov) in &inventory {
        if !args.filter.matches(Some(&id.kind), &id.venue, id.label()) {
            continue;
        }
        // AFTER the filter, deliberately: the note then describes what THIS listing withheld from
        // THIS operator, rather than reporting account series in venues they did not ask about.
        if vike_model::is_account_kind(&id.kind) {
            excluded.insert(id.kind.as_str());
            excluded_count += 1;
            continue;
        }
        // The id is handed BACK to the server exactly as it arrived — this side never constructs
        // a `SeriesId`, which is the whole reason `gaps` selects by FILTER rather than taking a
        // four-dimensional identity off the command line.
        let (gaps, gaps_error) = if args.gaps {
            match client.series_gaps(id) {
                Ok(ranges) => (Some(ranges), None),
                Err(e) => (None, Some(e)),
            }
        } else {
            (None, None)
        };
        // ⚠ Keyed on the RAW `symbol`, never on `label()`. A grouped series' label is its GROUP,
        // and two grouped series of different groups would otherwise share the one empty-symbol
        // cache slot — harmless today only because [`ClassProbe::Grouped`] carries no venue-specific
        // answer, which is the kind of accident that stops being harmless when a variant grows.
        let class = args.class.then(|| {
            if id.group.is_some() {
                return ClassProbe::Grouped;
            }
            class_cache
                .entry((id.venue.clone(), id.symbol.clone()))
                .or_insert_with(|| {
                    match client.properties_as_of(&id.venue, &id.symbol, CLASS_AS_OF_TS) {
                        // The READ this whole flag exists for: the field the venue producers write
                        // and, until now, nothing in the workspace read back.
                        Ok(Some(props)) => {
                            props.asset_class.map_or(ClassProbe::Unclassified, |c| {
                                ClassProbe::Classified(c.sql_word())
                            })
                        }
                        Ok(None) => ClassProbe::Unrecorded,
                        Err(e) => ClassProbe::Failed(e),
                    }
                })
                .clone()
        });
        rows.push(SeriesRow {
            kind: id.kind.clone(),
            venue: id.venue.clone(),
            name: id.label().to_string(),
            grouped: id.group.is_some(),
            symbol: id.symbol.clone(),
            group: id.group.clone(),
            interval: id.interval.clone(),
            coverage: Coverage {
                first_ts: cov.first_ts,
                last_ts: cov.last_ts,
                rows: cov.rows,
                bytes: cov.bytes,
                parts: cov.parts,
                dates: cov.dates,
            },
            gaps,
            gaps_error,
            class,
        });
    }

    if args.json {
        println!("{}", list_json(args, &rows, reported));
    } else {
        for line in list_lines(&rows, reported, !args.filter.is_empty(), args.gaps, args.class) {
            println!("{line}");
        }
        // ⚠ The disclosure goes to the HUMAN render only. `--json` is consumed by a program, and a
        // note appended to a document is noise at best and a parse error at worst; the JSON's
        // `reported` already carries the store's own total, so a consumer that wants the difference
        // can compute it. `account_exclusion_note` is pure and unit-tested beside the other
        // renderers.
        if let Some(note) = account_exclusion_note(excluded_count, &excluded) {
            println!("{note}");
        }
    }
    Ok(())
}

/// Refuse a READ whose `--kind` names an account kind outright — the second of §9.3.2's rules.
///
/// ⚠ The exclusion and this refusal answer DIFFERENT questions and both are needed. An unfiltered
/// listing withholds account series and says so (the note below); a listing that ASKED for one by
/// name must not answer "no series match", because that sentence is false — the series exist and
/// this plane declines to serve them. Silence there would send an operator to check the wrong end
/// of the pipe, which is the same failure [`empty_note`] exists to prevent one level up.
///
/// ⚠ EXACT match only. `--kind` on a listing is a SUBSTRING filter (the module doc's rule: an
/// unknown kind is a filter that matched nothing, not a roster error), so `--kind exec` stays a
/// filter and simply matches nothing after the exclusion. Refusing every substring that could
/// reach an account kind would put a roster's worth of guessing into a filter.
fn refuse_an_account_kind_filter(args: &Args) -> Result<(), String> {
    let Some(kind) = args.filter.kind.as_deref() else { return Ok(()) };
    refuse_an_account_kind_on_a_read(kind)
}

/// The SENTENCE the rule above answers with, spelled once for every read-side flag that names a
/// kind.
///
/// ⚠ Two flags reach it and they are refused at different RUNGS, which is deliberate rather than an
/// oversight: `ls --kind` is a filter over an answer that has already arrived, so its refusal sits
/// in [`execute_list`]; `gate --require-kind` is a criterion, so [`parse`] refuses it before a
/// socket is opened. What must NOT differ is the words — an operator who meets the same rule twice
/// and reads two sentences learns that one of them is a different KIND of no, which is false.
/// Distinct from [`refuse_an_account_kind`], which is `rm`'s and carries the extra paragraph
/// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` requires for a
/// DELETE.
fn refuse_an_account_kind_on_a_read(kind: &str) -> Result<(), String> {
    if !vike_model::is_account_kind(kind) {
        return Ok(());
    }
    Err(format!(
        "`{kind}` is ACCOUNT data, not market data, and no verb in this CLI plane reads it.\n\
         Your fills, orders, funding payments and equity belong to `vike-cli account`, which is \
         not built yet. Two of those four — equity and exec_fill — ARE on the datahub wire now, \
         so what is missing for them is the CLI verb rather than the read; orders and funding \
         payments are on neither. The market funding RATE is not an account kind — it is \
         `--kind bar --interval funding`."
    ))
}

/// The `§9.3.2` disclosure: what this ANSWER withheld, and why, in the operator's own terms.
///
/// `None` when nothing was withheld — a note that fires on every run stops being read.
///
/// ⚠ It names the KINDS rather than only a count, because the count alone cannot be acted on: an
/// operator who sees "3 series are hidden" has to guess whether the thing they are looking for is
/// among them. It also names the plane that WILL serve them, so the answer to "where did my fills
/// go" is in the message rather than in a document.
///
/// ⚠ **TWO verbs reach it now, and the sentence moved to stay true of both.** It read "…and
/// {is_are} not LISTED here", which was exact while [`execute_list`] was the only caller.
/// [`execute_gate`] withholds the same series from a VERDICT's evidence and lists nothing at all,
/// so the clause is "not part of this answer" — one rule keeping one sentence, which is the whole
/// reason [`refuse_an_account_kind_on_a_read`] exists as a shared function one rung up.
fn account_exclusion_note(
    count: usize,
    kinds: &std::collections::BTreeSet<&str>,
) -> Option<String> {
    if count == 0 {
        return None;
    }
    // `series` is its own plural, so only the KIND noun and the verb inflect.
    let names: Vec<&str> = kinds.iter().copied().collect();
    let is_are = if count == 1 { "is" } else { "are" };
    Some(format!(
        "note: {count} series of kind{} {} {is_are} in this store and {is_are} not part of this \
         answer — they are account data (`vike-cli account`, not built yet).",
        if names.len() == 1 { "" } else { "s" },
        names.join(", "),
    ))
}

/// The human `list` table — PURE, so every column rule below is unit-tested rather than only seen.
///
/// One row per series, with `kind` and a `SCOPE` cell (`symbol` | `group`) as their own columns:
/// the identity is four dimensions with an alternative inside it, and joining them into a
/// `VENUE:SYMBOL:INTERVAL` string would render every grouped series as a venue and two empties.
///
/// ⚠ `FIRST`/`LAST` are `-` for a ZERO-ROW series rather than a date. A store folds an empty series
/// to an all-zero coverage, and `epoch_ms_to_utc_date(0)` is a perfectly well-formed `1970-01-01`
/// that reads as data. [`list_json`] carries the store's own numbers untouched — a render may
/// decline to show a sentinel, but a document may not edit one.
///
/// ⚠ The CLASS column is appended only under `--class`, and the widths are arranged so that WITHOUT
/// it every line is byte-identical to a build that had never heard of the flag: `last_w` is 0 there,
/// and `{:<0$}` pads to at least zero characters, i.e. not at all. A trailing column that was always
/// present would have had to render an unasked question in every cell.
fn list_lines(
    rows: &[SeriesRow],
    reported: usize,
    filtered: bool,
    gaps_requested: bool,
    class_requested: bool,
) -> Vec<String> {
    if rows.is_empty() {
        return vec![empty_note("series", reported, filtered)];
    }
    let kind_w = col("KIND", rows.iter().map(|r| r.kind.len()));
    let venue_w = col("VENUE", rows.iter().map(|r| r.venue.len()));
    let scope_w = col("SCOPE", rows.iter().map(|r| scope_cell(r.grouped).len()));
    let name_w = col("NAME", rows.iter().map(|r| r.name.len()));
    let ivl_w = col("INTERVAL", rows.iter().map(|r| interval_cell(r).len()));
    let rows_w = col("ROWS", rows.iter().map(|r| r.coverage.rows.to_string().len()));
    let days_w = col("DAYS", rows.iter().map(|r| r.coverage.dates.to_string().len()));
    // Only the LAST column needs a measured width once something follows it — see this function's
    // doc for why zero is the right "no class asked for" value rather than a second format string.
    let last_w = if class_requested {
        col("LAST", rows.iter().map(|r| span_cell(r.coverage.last_ts, r.coverage.rows).len()))
    } else {
        0
    };
    let class_head = if class_requested { "  CLASS" } else { "" };

    let mut lines = vec![format!(
        "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {:>rows_w$}  \
         {:>days_w$}  {:<10}  {:<last_w$}{class_head}",
        "KIND", "VENUE", "SCOPE", "NAME", "INTERVAL", "ROWS", "DAYS", "FIRST", "LAST"
    )];
    for r in rows {
        let class_tail =
            if class_requested { format!("  {}", class_cell(r)) } else { String::new() };
        lines.push(format!(
            "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {:>rows_w$}  \
             {:>days_w$}  {:<10}  {:<last_w$}{class_tail}",
            r.kind,
            r.venue,
            scope_cell(r.grouped),
            r.name,
            interval_cell(r),
            r.coverage.rows,
            r.coverage.dates,
            span_cell(r.coverage.first_ts, r.coverage.rows),
            span_cell(r.coverage.last_ts, r.coverage.rows),
        ));
        if class_requested {
            lines.extend(class_error_line(r));
        }
        if gaps_requested {
            lines.extend(gap_lines(r));
        }
    }
    lines.push(String::new());
    let shown_rows: u64 = rows.iter().map(|r| r.coverage.rows).sum();
    let head = if filtered {
        format!("{} of {reported} series", rows.len())
    } else {
        format!("{reported} series")
    };
    lines.push(format!("{head} · {shown_rows} rows"));
    lines
}

/// One row's CLASS cell, under `--class` only.
///
/// ⚠ **The three absences are three different words, and none of them is blank.** `unclassified`
/// means a grid was recorded and named no class — the producer is not wired, which is the thing
/// this column was added to make visible; `no-properties` means nothing has recorded a grid for
/// this instrument at all; `(group)` means the question was not asked because a grouped series'
/// name is a GROUP and `properties_as_of` takes a symbol. Rendering any of them as an empty cell
/// would put the model's carefully-preserved `Option` back into the state its own doc refuses —
/// a guess and a fetch indistinguishable once stored.
///
/// The parenthesised two are the ones that are NOT facts about the instrument (nothing was asked;
/// something broke), which is why they wear brackets and the two real verdicts do not.
fn class_cell(row: &SeriesRow) -> &'static str {
    match &row.class {
        Some(ClassProbe::Classified(word)) => word,
        Some(ClassProbe::Unclassified) => "unclassified",
        Some(ClassProbe::Unrecorded) => "no-properties",
        Some(ClassProbe::Grouped) => "(group)",
        Some(ClassProbe::Failed(_)) => "(error)",
        // UNREACHABLE from [`list_lines`], which only calls this under `class_requested`. Spelled
        // as a cell rather than a panic, for the reason [`empty_note`]'s last arm gives: a renderer
        // has no business aborting a run that already succeeded.
        None => "-",
    }
}

/// The reason under a row whose class probe FAILED, and nothing at all for any other outcome.
///
/// The asymmetry with [`gap_lines`] is deliberate: a gap probe's three outcomes all need a line
/// because "no gaps" is invisible in the row itself, whereas every class verdict is already IN the
/// row as a word. Only the failure carries text the cell cannot hold.
fn class_error_line(row: &SeriesRow) -> Vec<String> {
    match &row.class {
        Some(ClassProbe::Failed(e)) => vec![format!("      class unavailable: {e}")],
        _ => Vec::new(),
    }
}

/// The gap annotation under one row, under `gaps` only.
///
/// Three outcomes, and all three are SAID rather than implied by an absence: holes, no holes, and
/// a probe this store could not answer. Printing nothing for the middle case would leave the
/// operator who ran `gaps` unable to tell an answered "clean" from an unasked question.
fn gap_lines(row: &SeriesRow) -> Vec<String> {
    const INDENT: &str = "      ";
    match (&row.gaps, &row.gaps_error) {
        (_, Some(e)) => vec![format!("{INDENT}gaps unavailable: {e}")],
        (Some(ranges), None) if ranges.is_empty() => vec![format!("{INDENT}no gaps")],
        (Some(ranges), None) => ranges
            .iter()
            .map(|(from, to)| {
                format!(
                    "{INDENT}gap {} .. {}",
                    epoch_ms_to_utc_date(*from),
                    epoch_ms_to_utc_date(*to)
                )
            })
            .collect(),
        (None, None) => Vec::new(),
    }
}

/// The `list --json` document.
///
/// It carries `symbol` AND `group` beside the derived `name`/`grouped`, because the four dimensions
/// ARE the identity and a caller that wants to ask the same server about the same series needs them
/// rather than this side's rendering. `interval` is `null` for every tick-shaped kind, which is the
/// store's own answer and not an omission.
///
/// Gap ranges are OBJECTS (`from_ts`/`to_ts`), not two-element arrays: the wire shape is a tuple,
/// and a caller reading `g[0]`/`g[1]` has to remember an order that nothing in the document states.
///
/// ⚠ `coverage` carries the store's numbers UNTOUCHED — including a zero-row series' all-zero
/// timestamps, which [`list_lines`] renders as `-`. A document that substituted `null` there would
/// be reporting a judgement, and a caller folding several stores' inventories would have no way to
/// tell that judgement from a field the server never sent.
///
/// # ⚠ The class fields are a WIRE, and an absent class may not read as a present-but-empty one
///
/// `asset_class` alone could not carry this: a `null` there would mean "not asked", "no grid
/// recorded", "a grid that named no class" and "the probe failed" all at once, and the last three
/// are what an operator acts on differently. So the document carries `asset_class_status`
/// ([`ClassProbe::status`]) beside it, and `asset_class` is non-null for EXACTLY the `classified`
/// verdict — a relation pinned by `the_class_fields_are_a_status_and_a_word_that_cannot_disagree`.
/// `asset_class_error` carries the sentence for `error` and is null otherwise.
///
/// Under no `--class`, all three are `null` on every row and the top-level `class_requested` is
/// `false` — the exact shape `gaps`/`gaps_requested` already has, so a caller that learned the one
/// has not been handed a second convention. That pairing is also what keeps this ADDITIVE: a
/// pre-existing consumer sees three new always-null keys and a `false`.
fn list_json(args: &Args, rows: &[SeriesRow], reported: usize) -> String {
    let series: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "kind": r.kind,
                "venue": r.venue,
                "name": r.name,
                "grouped": r.grouped,
                "symbol": r.symbol,
                "group": r.group,
                "interval": r.interval,
                "coverage": {
                    "first_ts": r.coverage.first_ts,
                    "last_ts": r.coverage.last_ts,
                    "rows": r.coverage.rows,
                    "bytes": r.coverage.bytes,
                    "parts": r.coverage.parts,
                    "dates": r.coverage.dates,
                },
                "gaps": r.gaps.as_ref().map(|ranges| {
                    ranges
                        .iter()
                        .map(|(from, to)| serde_json::json!({ "from_ts": from, "to_ts": to }))
                        .collect::<Vec<_>>()
                }),
                "gaps_error": r.gaps_error,
                "asset_class": r.class.as_ref().and_then(ClassProbe::word),
                "asset_class_status": r.class.as_ref().map(ClassProbe::status),
                "asset_class_error": match &r.class {
                    Some(ClassProbe::Failed(e)) => Some(e.as_str()),
                    _ => None,
                },
            })
        })
        .collect();
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": {
            "kind": args.filter.kind,
            "venue": args.filter.venue,
            "name": args.filter.name,
        },
        "gaps_requested": args.gaps,
        "class_requested": args.class,
        // What the SERVER reported, beside what the filter kept — so a caller can tell an empty
        // store from an over-narrow filter without a second call.
        "series_reported": reported,
        "count": series.len(),
        "series": series,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

/// One kind's presence for one instrument in the cross-kind report: the kind, and how many UTC days
/// of it are on disk. Only kinds the instrument ACTUALLY records appear — "never recorded" is a
/// different fact from "recorded with holes", and the wire type says so itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KindRow {
    kind: String,
    days: usize,
}

/// A day some of an instrument's recorded kinds cover and others do not — the report's whole point.
///
/// Both `day` (the UTC-day INDEX the wire carries) and `start_ms` (that day's epoch-ms midnight,
/// via the wire type's own converter) are kept: the index is what the store indexes by, and the ms
/// is what a date renders from. Deriving the second here rather than in the renderer keeps this
/// module free of a day-length constant it would otherwise have to spell for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PartialRow {
    day: i64,
    start_ms: i64,
    missing: Vec<String>,
}

/// One row of `coverage`: an instrument's kinds lined up, and the days on which they disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstrumentRow {
    venue: String,
    /// The symbol for a per-symbol instrument, the group for a grouped one — the same `label`
    /// rule [`SeriesRow::name`] follows, and the same reason.
    name: String,
    grouped: bool,
    kinds: Vec<KindRow>,
    /// The union of days ANY kind has — this instrument's overall recorded span.
    spanned_days: usize,
    partial: Vec<PartialRow>,
}

/// `data hist coverage` — one `coverage_report()` round trip, filtered and optionally narrowed to the
/// instruments that actually have a disagreement.
///
/// ⚠ The verb is CAPABILITY-NEGOTIATED on the client side: against a datahub too old to advertise
/// the coverage feature, `coverage_report()` refuses before sending anything and its own sentence
/// says so. That refusal stays on the run-failure rung, not the connect one — the box answered the
/// handshake, so it is a fact about a REACHABLE server and retrying it forever is the inversion the
/// ladder exists to prevent (`crate::cmd::trade_status`'s `failure_exit` argues the same rule
/// against a tradehub node).
fn execute_coverage(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let report = client.coverage_report()?;
    let reported = report.len();

    let mut rows: Vec<InstrumentRow> = Vec::new();
    for instrument in &report {
        // `None` for the kind dimension: a coverage row IS the join across kinds, which is also why
        // `parse` refuses `--kind` here.
        if !args.filter.matches(None, &instrument.key.venue, &instrument.key.label) {
            continue;
        }
        let partial: Vec<PartialRow> = instrument
            .partial_days()
            .into_iter()
            .map(|p| PartialRow { day: p.day, start_ms: p.start_ms(), missing: p.missing_kinds })
            .collect();
        if args.partial_only && partial.is_empty() {
            continue;
        }
        let kinds = instrument
            .recorded_kinds()
            .into_iter()
            .map(|k| KindRow {
                kind: k.to_string(),
                days: instrument.kinds.get(k).map_or(0, |d| d.present.len()),
            })
            .collect();
        rows.push(InstrumentRow {
            venue: instrument.key.venue.clone(),
            name: instrument.key.label.clone(),
            grouped: instrument.key.grouped,
            kinds,
            spanned_days: instrument.spanned_days().len(),
            partial,
        });
    }

    if args.json {
        println!("{}", coverage_json(args, &rows, reported));
    } else {
        let narrowed = args.partial_only || !args.filter.is_empty();
        for line in coverage_lines(&rows, reported, narrowed) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The human `coverage` table — PURE, unit-tested below.
///
/// One row per instrument, then the partial days indented under it, capped at
/// [`MAX_PARTIAL_DAYS_SHOWN`] with a line saying how many were withheld. A COMPLETE instrument gets
/// its row and no detail lines: there is nothing to explain, and a "no partial days" line under
/// every healthy instrument is how a report teaches an operator to skim past it.
///
/// ⚠ No gap ranges appear here in any form. This table's days are UTC-day INDICES and a series' gap
/// ranges are epoch-ms; both would render as dates and neither would say which it was.
/// `vike-cli data hist gaps` is the per-series view.
fn coverage_lines(rows: &[InstrumentRow], reported: usize, narrowed: bool) -> Vec<String> {
    if rows.is_empty() {
        return vec![empty_note("instruments", reported, narrowed)];
    }
    let venue_w = col("VENUE", rows.iter().map(|r| r.venue.len()));
    let scope_w = col("SCOPE", rows.iter().map(|r| scope_cell(r.grouped).len()));
    let name_w = col("INSTRUMENT", rows.iter().map(|r| r.name.len()));
    let kinds_w = col("KINDS", rows.iter().map(|r| kinds_cell(r).len()));
    let days_w = col("DAYS", rows.iter().map(|r| r.spanned_days.to_string().len()));

    let mut lines = vec![format!(
        "{:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<kinds_w$}  {:>days_w$}  {}",
        "VENUE", "SCOPE", "INSTRUMENT", "KINDS", "DAYS", "PARTIAL"
    )];
    for r in rows {
        let partial_cell =
            if r.partial.is_empty() { "-".to_string() } else { r.partial.len().to_string() };
        lines.push(format!(
            "{:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<kinds_w$}  {:>days_w$}  {}",
            r.venue,
            scope_cell(r.grouped),
            r.name,
            kinds_cell(r),
            r.spanned_days,
            partial_cell,
        ));
        for p in r.partial.iter().take(MAX_PARTIAL_DAYS_SHOWN) {
            lines.push(format!(
                "      {}  missing: {}",
                epoch_ms_to_utc_date(p.start_ms),
                p.missing.join(", ")
            ));
        }
        if let Some(hidden) = r.partial.len().checked_sub(MAX_PARTIAL_DAYS_SHOWN).filter(|n| *n > 0)
        {
            lines.push(format!("      … and {hidden} more partial days (--json carries them all)"));
        }
    }
    lines.push(String::new());
    let partial_rows = rows.iter().filter(|r| !r.partial.is_empty()).count();
    let head = if narrowed {
        format!("{} of {reported} instruments", rows.len())
    } else {
        format!("{reported} instruments")
    };
    lines.push(format!("{head} · {partial_rows} with partial days"));
    lines
}

/// The `coverage --json` document — every partial day, uncapped (see [`MAX_PARTIAL_DAYS_SHOWN`]).
///
/// `complete` is computed from the SAME `partial_days` list the rows carry rather than from a
/// second call to the wire type's own predicate, so the flag and the array cannot disagree about
/// one instrument.
fn coverage_json(args: &Args, rows: &[InstrumentRow], reported: usize) -> String {
    let instruments: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "venue": r.venue,
                "name": r.name,
                "grouped": r.grouped,
                "complete": r.partial.is_empty(),
                "spanned_days": r.spanned_days,
                "kinds": r.kinds.iter().map(|k| serde_json::json!({
                    "kind": k.kind,
                    "days": k.days,
                })).collect::<Vec<_>>(),
                "partial_days": r.partial.iter().map(|p| serde_json::json!({
                    "day": p.day,
                    "start_ms": p.start_ms,
                    "date": epoch_ms_to_utc_date(p.start_ms),
                    "missing_kinds": p.missing,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": { "venue": args.filter.venue, "name": args.filter.name },
        "partial_only": args.partial_only,
        "instruments_reported": reported,
        "count": instruments.len(),
        "instruments": instruments,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

// ─── `tape-health`: the series whose own catalog contradicts itself ─────────────────────────────

/// The ROW-LEVEL checks `tape-health` does NOT run, named in its own `--json` document.
///
/// ⚠ **Declared as data, not as a comment, and that is the whole point.** A health report whose
/// clean verdict could be mistaken for a COMPLETE one is worse than no report: an operator reads
/// "nothing contradicts itself" and concludes the OHLC is sane, which this verb never looked at.
/// So the document carries what was skipped, by name, and `crate::cmd::data::tape_health`'s module
/// doc carries the two walls behind it — the `vike_data::TsRange` parameter on every row-reading
/// RPC (this crate takes `vike-data` as a DEV-dependency, so that type cannot be named in library
/// code), and the compute-to-data rule that says a fold over a year of bars belongs beside the
/// Parquet rather than across a socket.
///
/// The human rendering states the same bound in `--help` rather than under every run, for the
/// reason `tape_health::lines` gives for the survivorship sentence one verb over: a paragraph
/// printed unconditionally is a paragraph that is skipped on the run where it mattered.
const ROW_CHECKS_NOT_RUN: [&str; 3] =
    ["ohlc-bounds", "row-duplicate-timestamp", "row-non-monotonic"];

/// `data hist tape-health` — one `inventory()` round trip, then a pure fold per matched series.
///
/// ⚠ **No row crosses the wire, and that is a property rather than an optimization.** This is the
/// same single RPC [`execute_list`] makes; every finding below is arithmetic over the coverage
/// numbers the server already sent. There is deliberately no per-series second round trip the way
/// `gaps` makes one: a gap probe answers a question the catalog does not already contain, and
/// every check here is answerable from what one `inventory()` carries.
fn execute_tape_health(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let inventory = client.inventory()?;
    let reported = inventory.len();

    let mut scanned: Vec<tape_health::Scanned> = Vec::new();
    for (id, cov) in &inventory {
        if !args.filter.matches(Some(&id.kind), &id.venue, id.label()) {
            continue;
        }
        let facts = tape_health::SeriesFacts {
            kind: id.kind.clone(),
            venue: id.venue.clone(),
            name: id.label().to_string(),
            grouped: id.group.is_some(),
            interval: id.interval.clone(),
            first_ts: cov.first_ts,
            last_ts: cov.last_ts,
            rows: cov.rows,
            parts: cov.parts,
            dates: cov.dates,
        };
        let findings = tape_health::findings_for(&facts);
        scanned.push(tape_health::Scanned { facts, findings });
    }

    if args.json {
        println!("{}", tape_health_json(args, &scanned, reported));
    } else {
        for line in tape_health::lines(&scanned, reported, !args.filter.is_empty()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The `tape-health --json` document.
///
/// Every scanned series appears, the clean ones included — unlike the human rendering, which shows
/// only the offenders. The two are deliberately different: a person is reading a verdict and wants
/// the exceptions, while a machine reader is folding several stores and needs to know a series was
/// LOOKED AT and passed. An absent row would otherwise be indistinguishable from a filter that
/// never selected it.
fn tape_health_json(args: &Args, scanned: &[tape_health::Scanned], reported: usize) -> String {
    let (contradictions, suspects) = tape_health::totals(scanned);
    let series = tape_health::json_series(scanned);
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": {
            "kind": args.filter.kind,
            "venue": args.filter.venue,
            "name": args.filter.name,
        },
        "series_reported": reported,
        "count": series.len(),
        "with_findings": scanned.iter().filter(|s| !s.clean()).count(),
        // The two classes are carried APART and never summed — see `tape_health::Class`.
        "contradictions": contradictions,
        "suspects": suspects,
        // What this verb did NOT check. See [`ROW_CHECKS_NOT_RUN`].
        "row_checks_not_run": ROW_CHECKS_NOT_RUN,
        "series": series,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

// ─── `universe`: point-in-time membership ───────────────────────────────────────────────────────

/// `data hist universe` — one `inventory()` round trip, folded per instrument and judged against a
/// window.
///
/// ⚠ **The FRAME is computed over the WHOLE inventory, before the filter narrows anything**, and
/// the filter then selects which instruments are RENDERED. That ordering is load-bearing: the
/// survivorship verdict is "this tape stopped while the store went on", so the store's newest row
/// has to mean the same thing whether or not `--venue binance` was typed. Computing it from the
/// filtered subset would make a single-venue run place every one of that venue's instruments at
/// the edge, and the one verb whose job is to find the tapes that stopped would find none.
fn execute_universe(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let inventory = client.inventory()?;

    let all: Vec<universe::SeriesSpan> = inventory
        .iter()
        .map(|(id, cov)| universe::SeriesSpan {
            kind: id.kind.clone(),
            venue: id.venue.clone(),
            name: id.label().to_string(),
            grouped: id.group.is_some(),
            first_ts: cov.first_ts,
            last_ts: cov.last_ts,
            rows: cov.rows,
        })
        .collect();
    let store = universe::store_span(&all);
    let asked = args.universe_window.unwrap_or_default();
    let resolved = universe::resolve_window(asked, store);

    // ⚠ The filter is applied to the SERIES, not to the folded instruments, because `--kind` is a
    // per-series dimension: `--kind bar` asks about the BAR universe, and an instrument's kind
    // list is assembled from the series that survived. Filtering after the fold would keep every
    // instrument that has a bar series and then report its trade span beside it.
    let selected: Vec<universe::SeriesSpan> = all
        .iter()
        .filter(|s| args.filter.matches(Some(&s.kind), &s.venue, &s.name))
        .cloned()
        .collect();
    // ⚠ A SECOND fold over the unfiltered spans, purely for the count. `reported` is the number of
    // INSTRUMENTS the store holds, and the inventory's own length is a number of SERIES — reusing
    // it would tell an operator their filter had selected 3 of 400 when the store holds 40
    // instruments. It is a pure fold over data already in memory, which is cheaper than being
    // wrong in `empty_note`.
    let reported = universe::fold_members(&all).len();

    let mut rows: Vec<(universe::Member, universe::Membership)> = Vec::new();
    for member in universe::fold_members(&selected) {
        let status = match resolved {
            Some((from, to)) => universe::classify(&member, from, to, store.last_ts),
            // No frame: the store reported no span at all, so there is nothing to judge
            // membership OF. Every instrument is `absent`, which is honest — inventing `now()`
            // for the missing endpoint is what `universe`'s module doc refuses.
            None => universe::Membership::absent(),
        };
        rows.push((member, status));
    }

    if args.json {
        println!("{}", universe_json(args, &rows, reported, resolved));
    } else {
        for line in universe::lines(&rows, reported, !args.filter.is_empty(), resolved) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The `universe --json` document.
///
/// ⚠ It carries the RESOLVED window rather than what the operator typed, and both when they
/// differ: an omitted `--to` took the store's own newest row, and a consumer comparing two runs
/// against the same "unbounded" universe must be able to see that the two windows were different.
/// The requested bounds are carried beside it for the same reason — so `null` means "not asked
/// for" rather than "not applied".
fn universe_json(
    args: &Args,
    rows: &[(universe::Member, universe::Membership)],
    reported: usize,
    resolved: Option<(i64, i64)>,
) -> String {
    let requested = args.universe_window.unwrap_or_default();
    let members = universe::json_members(rows);
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": {
            "kind": args.filter.kind,
            "venue": args.filter.venue,
            "name": args.filter.name,
        },
        "requested_window": { "from_ts": requested.from, "to_ts": requested.to },
        "resolved_window": resolved.map(|(from, to)| serde_json::json!({
            "from_ts": from,
            "to_ts": to,
            "from_date": epoch_ms_to_utc_date(from),
            "to_date": epoch_ms_to_utc_date(to),
        })),
        "instruments_reported": reported,
        "count": members.len(),
        "spanned_whole_window": rows.iter().filter(|(_, s)| s.covers_window).count(),
        "began_inside_window": rows.iter().filter(|(_, s)| s.listed_inside).count(),
        // The survivorship count — the instruments a listing taken from today's store omits.
        "stopped_inside_window": rows.iter().filter(|(_, s)| s.left_inside).count(),
        "instruments": members,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

// ─── `gate`: data readiness as an exit code ─────────────────────────────────────────────────────

/// `data hist gate` — one `inventory()` round trip, then one `series_gaps` probe per JUDGED series,
/// and only when `--max-gap` asked a question the catalog does not already answer.
///
/// ⚠ **The RUNG is this function's product**, which is why it is the one arm of [`execute`] that
/// returns something other than [`Exit::Ok`]. A breach is not a failure and must not travel as a
/// [`CliError`]: the command evaluated every criterion it was given and the answer was no, and
/// routing that through an error would print one stderr line and throw away the per-criterion
/// document that is the whole point (`crate::exit::Exit::Breach`'s own doc states the rule).
///
/// ⚠ **A probe is made for the series this gate is ABOUT and no others.** `gaps` pays one round
/// trip per matched series because every line of its output is a hole; here a series of an
/// undeclared kind is evidence for the presence criterion alone, and its holes are a question
/// nobody asked. With no `--max-gap` at all, this verb costs exactly one round trip — the same as
/// `ls`.
fn execute_gate(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<Exit> {
    let g = args.gate.as_ref().expect("`parse` builds a GateArgs for every Sub::Gate");
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let inventory = client.inventory()?;
    let reported = inventory.len();

    // ⚠ The §9.3.2 ACCOUNT-KIND exclusion, and this verb owes it exactly as [`execute_list`] does.
    // The first cut argued it did not — "this verb selects by an EXACT spec, and [`parse`] has
    // already refused that spelling of `--require-kind`" — and that argument covers the CRITERION
    // side ONLY. A spec is `VENUE:NAME`, which is two of a series' four dimensions: on a SHARED
    // store, `binance:BTCUSDT` matches this account's `exec_fill` tape as squarely as it matches
    // the bars. The EVIDENCE side was therefore unfiltered, so `gate` named `exec_fill` in the
    // presence criterion's `this spec holds:` cell and emitted its first_ts/last_ts/rows in the
    // `--json` `series[]` — out of a market-data read verb, while `ls` over the same store showed
    // neither and said so in a note. Two sibling read verbs rendering one store two ways, and the
    // one with no disclosure was the one a CI step keeps.
    let mut excluded: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut excluded_count = 0usize;

    let mut matched: Vec<gate::Candidate> = Vec::new();
    for (id, cov) in &inventory {
        if !g.spec.matches(&id.venue, id.label(), id.group.is_some(), id.interval.as_deref()) {
            continue;
        }
        // AFTER the spec, for [`execute_list`]'s reason: the disclosure then describes what THIS
        // gate withheld from THIS verdict, rather than reporting account series the operator's
        // spec never reached.
        if vike_model::is_account_kind(&id.kind) {
            excluded.insert(id.kind.as_str());
            excluded_count += 1;
            continue;
        }
        let judged = g.kinds.iter().any(|k| k == &id.kind);
        // The id is handed BACK to the server exactly as it arrived — this side never constructs a
        // `SeriesId`, which is the rule `gaps` follows and the reason the spec is a SELECTOR.
        let (gaps, gaps_error) = if judged && g.max_gap_ms.is_some() {
            match client.series_gaps(id) {
                Ok(ranges) => (Some(ranges), None),
                Err(e) => (None, Some(e)),
            }
        } else {
            (None, None)
        };
        matched.push(gate::Candidate {
            kind: id.kind.clone(),
            interval: id.interval.clone(),
            first_ts: cov.first_ts,
            last_ts: cov.last_ts,
            rows: cov.rows,
            gaps,
            gaps_error,
        });
    }
    // ⚠ SORTED, so two runs over one store render a byte-identical verdict whatever order the
    // enumeration arrived in — which is what makes this diffable across a backfill. The same
    // property `crate::cmd::data::tape_health`'s `findings_for` fixes its check order for.
    matched.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.interval.cmp(&b.interval)));

    // ⚠ THE NOTHING-WAS-EVALUATED RUNG, and it is reached only when the spec matched no stored
    // series of ANY kind. A required kind the store LACKS is a breach instead — the operator
    // declared it required — and the two are different actions: fix the command against fetch the
    // tape. Collapsing them onto one number would send a CI step to the wrong end of the pipe.
    if matched.is_empty() {
        // ⚠ A spec that matched ONLY account series is a different sentence, and it has to be: the
        // operator can see those series with their own eyes (`data hist ls` says they exist in its
        // note), so "matches none of them" would be a refusal they can disprove in one command.
        let withheld = match account_exclusion_note(excluded_count, &excluded) {
            Some(note) => format!("\n{note}"),
            None => String::new(),
        };
        return Err(CliError::empty(format!(
            "`{}` matches none of the {reported} series this datahub reported, so there was \
             nothing to gate — this is not a pass. Check the spelling, or that {} is the datahub \
             holding the store you meant: `vike-cli data hist ls --venue {}` lists what it has.{}",
            g.spec.text(),
            args.addr,
            g.spec.venue,
            withheld
        )));
    }

    let judgements = gate::judge(g, &matched);
    let exit = gate::rung(&judgements);
    if args.json {
        println!("{}", gate_json(args, g, &judgements, exit, &matched, reported));
    } else {
        for line in gate::lines(g, &judgements, exit, matched.len(), reported) {
            println!("{line}");
        }
        // ⚠ The disclosure goes to the HUMAN render only — [`execute_list`]'s rule, reached
        // through the SAME function so the two verbs cannot meet an operator with two sentences.
        // `--json` is consumed by a program and carries the facts instead: `series_reported` is
        // the store's own total and `series_matched` is what survived, so a consumer computes the
        // difference rather than parsing prose.
        if let Some(note) = account_exclusion_note(excluded_count, &excluded) {
            println!("{note}");
        }
    }
    Ok(exit)
}

/// The `gate --json` document.
///
/// ⚠ **The one document in this module emitted on a NON-ZERO exit**, and deliberately: every
/// sibling here writes nothing to stdout when the run failed, because a failure is a sentence on
/// stderr plus a rung. A breach is not a failure — §7.1 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` says the verdict is a
/// document naming every criterion that passed and failed, and a CI step that got the rung without
/// it would have to re-run the gate to learn which criterion moved.
///
/// It carries the criteria AND the series' own numbers, so a consumer re-derives a judgement
/// rather than trusting it — the shape `health` and `universe` already have. There is deliberately
/// no `exit_code` field: the process's own code is that fact, and a second spelling of it inside
/// the document is a second thing to keep in step.
fn gate_json(
    args: &Args,
    g: &GateArgs,
    judgements: &[gate::Judgement],
    exit: Exit,
    matched: &[gate::Candidate],
    reported: usize,
) -> String {
    let doc = serde_json::json!({
        // DERIVED from the verb, never re-typed — see [`tape_health_json`] for what four
        // hand-written copies of this cost.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "spec": {
            "text": g.spec.text(),
            "venue": g.spec.venue,
            "name": g.spec.name,
            "grouped": g.spec.grouped,
            "interval": g.spec.interval,
        },
        "require_days": g.require_days,
        // `null` when the holes were NOT checked, which is the fact the human rendering's note is
        // derived from — the document states it rather than restating the note.
        "max_gap_ms": g.max_gap_ms,
        "require_kinds": g.kinds,
        "series_reported": reported,
        "series_matched": matched.len(),
        "series_judged": matched.iter().filter(|c| g.kinds.iter().any(|k| k == &c.kind)).count(),
        // The COMPUTE plane's own vocabulary, read from where it is declared rather than from a
        // copy — see `crate::cmd::data::gate`'s module doc for what the copy cost.
        "verdict": crate::cmd::runs::failif::verdict_word(exit),
        "criteria": gate::json_criteria(judgements),
        "series": gate::json_series(matched),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

// ─── rendering helpers, shared by both read verbs ───────────────────────────────────────────────

/// A column's width: the widest cell, never narrower than its own header. Same shape
/// `crate::cmd::trade_status`'s `registry_lines` uses, so a long venue slug widens its column
/// instead of shearing the row.
fn col(header: &str, cells: impl Iterator<Item = usize>) -> usize {
    cells.chain([header.len()]).max().unwrap_or(0)
}

/// `symbol` or `group` — which of the two alternatives this row's NAME actually is. It is a column
/// rather than a decoration on the name, because a reader has to be able to tell them apart at a
/// glance and a grouped series' raw symbol is empty.
fn scope_cell(grouped: bool) -> &'static str {
    if grouped { "group" } else { "symbol" }
}

/// The bar step, or `-` for a tick-shaped kind that genuinely has none.
fn interval_cell(row: &SeriesRow) -> String {
    row.interval.clone().unwrap_or_else(|| "-".to_string())
}

/// A span endpoint as a UTC date, or `-` when the series holds no rows at all — see [`list_lines`]
/// for why a zero-row series may not be rendered as `1970-01-01`.
fn span_cell(ts: i64, rows: u64) -> String {
    if rows == 0 { "-".to_string() } else { epoch_ms_to_utc_date(ts) }
}

/// An instrument's recorded kinds with their day counts, e.g. `trade:180, depth:177`. Absent kinds
/// are absent rather than shown as zero — the wire type keeps "never recorded" distinct from
/// "recorded with holes", and flattening the two here would undo that on the way to the terminal.
fn kinds_cell(row: &InstrumentRow) -> String {
    row.kinds.iter().map(|k| format!("{}:{}", k.kind, k.days)).collect::<Vec<_>>().join(", ")
}

/// The EMPTY answer, whose two causes an operator must be able to tell apart: a server that
/// reported nothing, and a filter that selected nothing out of what it did report.
///
/// This is the same class of distinction `crate::cmd::secrets`' absent-vs-unreadable store draws —
/// one is the ordinary unconfigured state, the other is a thing you typed — and collapsing them
/// into a bare "nothing found" sends somebody to check the wrong end of the pipe.
fn empty_note(noun: &str, reported: usize, narrowed: bool) -> String {
    match (reported, narrowed) {
        (0, _) => format!("the datahub reported no {noun} at all"),
        (n, true) => format!("no {noun} match the filter ({n} reported)"),
        // UNREACHABLE by construction: with nothing narrowed, every reported row is kept, so an
        // empty result implies a zero count — which the arm above already answered. Spelled as a
        // true sentence rather than a panic, because a renderer has no business aborting a run
        // that already succeeded.
        (n, false) => format!("no {noun} to show ({n} reported)"),
    }
}

// ── the DATA flags on `backtest run`, as profile-key sugar ───────────────────────────────────────
//
// `--explain-data`, `--require-coverage`, `--max-gap`, `--on-gap` and `--universe` are flags on the
// COMPUTE plane's `run` sub-verb that configure the DATA a run reads. Their parser arms live in
// `crate::cmd::backtest`; what each one MEANS lives here, beside the verbs an operator is sent to
// when one of them fires (`data hist fetch` fills a span, `data hist coverage` shows the cross-kind join).
//
// # ⚠ Every one of them is SUGAR for a `[data]` profile key, and that is what makes them work
//
// Only the profile TEXT crosses the wire, and the store is on the far side — so a flag this side
// merely remembered could never reach a remote run. As an override on the profile document
// (`crate::cmd::backtest`'s `Override`) each one rides the existing `build_profile_toml` path: it
// reaches the compute daemon, it reaches the `--local` engine, and it shows up in
// `--show-effective` with its origin, so `--explain-data --show-effective > run.toml` writes a
// profile that plans. No wire verb, no protocol bump, no second mechanism.
//
// The authority for what each key does is `crates/vike-backtest/src/harness/profile.rs`'s `DataCfg`
// — `explain`, `require_coverage`, `max_gap`, `on_gap`, `universe` — and the pre-flight that applies
// them is `crates/vike-backtest/src/data_plan.rs`'s `enforce`. Nothing here re-implements either.

/// The dotted `[data]` keys the five flags below land on, spelled ONCE.
///
/// A `const` per key rather than a literal at each constructor, because a mistyped key is the one
/// failure mode this sugar has that nothing catches: `BacktestProfile` is `deny_unknown_fields`, so
/// a wrong key is a far-side parse error naming a key the operator never typed — the flag reads as
/// broken rather than as misspelled here.
const KEY_EXPLAIN: &str = "data.explain";
const KEY_REQUIRE_COVERAGE: &str = "data.require_coverage";
const KEY_MAX_GAP: &str = "data.max_gap";
const KEY_ON_GAP: &str = "data.on_gap";
const KEY_UNIVERSE: &str = "data.universe";

/// The `--on-gap` values, validated HERE so a typo is a local usage error instead of a wasted round
/// trip to a compute daemon that then reads the profile it was handed.
///
/// ⚠ It is a SPELLING check, never a second implementation — the same rule and the same words
/// `crate::cmd::backtest`'s `one_of` states (the `RANK_METRICS` const this sentence used to cite
/// is gone: its roster moved down to the protocol crate's shared flag vocabulary, and that
/// module's tombstone comment carries the argument). What a disposition MEANS is
/// `vike_backtest::data_plan::OnGap`, resolved by `DataCfg::on_gap` on whichever side runs, and its
/// refusal is the authoritative sentence if these two ever disagree.
const ON_GAP_VALUES: [&str; 3] = ["refuse", "warn", "run"];

/// The `--universe` values, on the same terms as [`ON_GAP_VALUES`]. The authority is
/// `vike_backtest::data_plan::UniverseMode` via `DataCfg::universe_mode`.
const UNIVERSE_VALUES: [&str; 3] = ["declared", "covered", "strict"];

/// `--explain-data`: resolve the profile's whole data slice, report what the store holds for it, and
/// EXIT without computing.
///
/// A bare switch, and IDEMPOTENT — writing it twice is the same request, so the parser may push this
/// more than once without the profile changing (the later write of an identical key/value is a
/// no-op through `build_profile_toml`'s last-wins fold).
pub fn explain_data_override() -> crate::cmd::backtest::Override {
    crate::cmd::backtest::Override {
        key: KEY_EXPLAIN.to_string(),
        value: toml::Value::Boolean(true),
        origin: crate::cmd::backtest::Origin::Sugar("--explain-data"),
    }
}

/// `--require-coverage`: arm the coverage gate, so a run whose window the store does not cover is
/// refused instead of loading whatever is there.
///
/// A bare switch, idempotent for [`explain_data_override`]'s reason. It arms the gate and says
/// nothing about the disposition — `--on-gap` does that, and its absence means `refuse`.
pub fn require_coverage_override() -> crate::cmd::backtest::Override {
    crate::cmd::backtest::Override {
        key: KEY_REQUIRE_COVERAGE.to_string(),
        value: toml::Value::Boolean(true),
        origin: crate::cmd::backtest::Origin::Sugar("--require-coverage"),
    }
}

/// `--max-gap SPAN`: how much of the window an armed gate tolerates missing in ONE span.
///
/// The span GRAMMAR is deliberately not checked here. `vike_model::time::parse_span` is what reads
/// it and `DataCfg::max_gap_ms` is what refuses a bar count or a calendar month, each with a
/// sentence arguing why that shape cannot become a wall-clock tolerance — and re-deriving those
/// three refusals on this side would be the second implementation this crate's spelling checks are
/// careful not to be. What IS refused here is a BLANK, because an empty value cannot be a typo of
/// anything and would reach the far side as `max_gap = ""`, whose refusal names a grammar the
/// operator never wrote.
pub fn max_gap_override(value: &str) -> Result<crate::cmd::backtest::Override, String> {
    let v = value.trim();
    if v.is_empty() {
        return Err("--max-gap requires a duration, e.g. --max-gap 1d (or 4h, 900000)".to_string());
    }
    Ok(crate::cmd::backtest::Override {
        key: KEY_MAX_GAP.to_string(),
        value: toml::Value::String(v.to_string()),
        origin: crate::cmd::backtest::Origin::Sugar("--max-gap"),
    })
}

/// `--on-gap refuse|warn|run`: what an armed gate DOES about a finding.
///
/// Case-insensitive, matching the far side's own resolution, so a value copied out of a runbook in
/// any case is the same choice on both routes.
pub fn on_gap_override(value: &str) -> Result<crate::cmd::backtest::Override, String> {
    let v = value.trim().to_ascii_lowercase();
    if !ON_GAP_VALUES.contains(&v.as_str()) {
        return Err(format!(
            "--on-gap {value:?} is not one of {}. It says what --require-coverage DOES about a \
             window the store does not cover: refuse stops the run (the default), warn logs the \
             findings and runs unchanged, run leaves the gate inert",
            ON_GAP_VALUES.join(" | ")
        ));
    }
    Ok(crate::cmd::backtest::Override {
        key: KEY_ON_GAP.to_string(),
        value: toml::Value::String(v),
        origin: crate::cmd::backtest::Origin::Sugar("--on-gap"),
    })
}

/// `--universe declared|covered|strict`: point-in-time membership, the survivorship defence.
///
/// ⚠ **None of the three DROPS a member**, and the message says so — because "point-in-time
/// universe" in most tools means selection, and an operator who expects this to narrow the slice
/// would read a clean run as a narrowed one. `DataCfg::universe` carries the whole argument.
pub fn universe_override(value: &str) -> Result<crate::cmd::backtest::Override, String> {
    let v = value.trim().to_ascii_lowercase();
    if !UNIVERSE_VALUES.contains(&v.as_str()) {
        return Err(format!(
            "--universe {value:?} is not one of {}. declared takes the symbol list verbatim (the \
             default), covered names every member whose tape does not span the window and runs \
             anyway, strict refuses that run. None of the three drops a member — a universe a run \
             narrowed silently would not be the one your profile names",
            UNIVERSE_VALUES.join(" | ")
        ));
    }
    Ok(crate::cmd::backtest::Override {
        key: KEY_UNIVERSE.to_string(),
        value: toml::Value::String(v),
        origin: crate::cmd::backtest::Origin::Sugar("--universe"),
    })
}

#[cfg(test)]
mod data_flag_tests {
    use super::*;

    #[test]
    fn the_two_switches_land_on_their_keys_as_booleans() {
        let e = explain_data_override();
        assert_eq!(e.key, "data.explain");
        assert_eq!(e.value, toml::Value::Boolean(true));
        let r = require_coverage_override();
        assert_eq!(r.key, "data.require_coverage");
        assert_eq!(r.value, toml::Value::Boolean(true));
    }

    /// A mixed-case value from a runbook is the same choice on both routes.
    #[test]
    fn on_gap_and_universe_are_case_insensitive_and_normalise_to_lowercase() {
        assert_eq!(
            on_gap_override("Warn").expect("warn is valid").value,
            toml::Value::String("warn".to_string())
        );
        assert_eq!(
            universe_override("  STRICT ").expect("strict is valid").value,
            toml::Value::String("strict".to_string())
        );
    }

    #[test]
    fn a_misspelled_disposition_is_a_local_usage_error_naming_the_set() {
        let e = on_gap_override("refues").expect_err("a typo must not reach the far side");
        assert!(e.contains("refuse | warn | run"), "the valid set is named: {e}");
        let u = universe_override("survivors").expect_err("a typo must not reach the far side");
        assert!(u.contains("declared | covered | strict"), "the valid set is named: {u}");
        assert!(u.contains("drops a member"), "the message corrects the expectation: {u}");
    }

    /// The span GRAMMAR belongs to the far side; a BLANK belongs here, because it is a typo of
    /// nothing and its far-side refusal would name a grammar nobody wrote.
    #[test]
    fn max_gap_passes_a_span_through_verbatim_and_refuses_only_a_blank() {
        assert_eq!(
            max_gap_override("1d").expect("a span is passed through").value,
            toml::Value::String("1d".to_string())
        );
        assert_eq!(
            max_gap_override(" 4h ").expect("trimmed, not parsed").value,
            toml::Value::String("4h".to_string())
        );
        // A bar count and a calendar month are REFUSED — by `DataCfg::max_gap_ms`, not here, so
        // this side must let them through rather than growing a second grammar.
        assert!(max_gap_override("200bars").is_ok(), "the grammar is the far side's to refuse");
        assert!(max_gap_override("   ").is_err());
    }

    #[test]
    fn every_key_is_under_the_data_table() {
        for key in [KEY_EXPLAIN, KEY_REQUIRE_COVERAGE, KEY_MAX_GAP, KEY_ON_GAP, KEY_UNIVERSE] {
            assert!(key.starts_with("data."), "{key} is not a [data] key");
        }
    }
}

#[cfg(test)]
mod tests {
    // ── §7: the group split, and the refusals that make it survivable ───────────────────────

    /// EVERY pre-group spelling is refused BY NAME and NAMES ITS REPLACEMENT.
    ///
    /// ⚠ Driven from [`RETIRED_SPELLINGS`], never a hand-written list — the same discipline
    /// `a_missing_subcommand_names_every_subcommand_that_exists` exists for. A table row added
    /// without a message, or a message that forgets to name where the verb went, fails here.
    #[test]
    fn every_retired_flat_spelling_is_refused_and_names_its_replacement() {
        for (was, now) in RETIRED_SPELLINGS {
            let err = parse_of(&[was]).unwrap_err();
            assert!(
                err.contains(now),
                "refusing `data {was}` must NAME its replacement `{now}`: {err}"
            );
        }
    }

    /// ...and every replacement it names actually PARSES. A refusal that points at a spelling the
    /// parser rejects is worse than the spelling it replaced — it costs the operator two attempts
    /// instead of one, and there is no third message to correct them.
    #[test]
    fn every_named_replacement_is_a_spelling_that_parses() {
        for (_, now) in RETIRED_SPELLINGS {
            // The replacement is a full command line minus `vike-cli data`; the verbs that need
            // arguments get the minimum that makes a NAME failure the only possible one.
            let mut argv: Vec<&str> = now.split_whitespace().skip(1).collect();
            match *argv.last().unwrap() {
                "fetch" => argv.extend(["d:S:1h", "--days", "1"]),
                "export" => argv.extend(["d:S:1h", "--out", "s.parquet"]),
                "rm" => argv.extend(["--kind", "bar", "--venue", "d", "--symbol", "S"]),
                "repair" => argv.extend(["--kind", "trade", "--venue", "d", "--symbol", "S"]),
                _ => {}
            }
            parse_of(&argv).unwrap_or_else(|e| {
                panic!(
                    "the replacement named for a retired verb, {now:?}, \
                                            does not parse: {e}"
                )
            });
        }
    }

    /// The two verbs the split RENAMED get their own message. A reader who typed the GROUP
    /// correctly and the verb by its old name should not be handed the whole roster to diff.
    #[test]
    fn a_renamed_verb_under_the_right_group_names_its_new_spelling() {
        for (old, new) in [("list", "ls"), ("tape-health", "health")] {
            let err = parse_of(&["hist", old]).unwrap_err();
            assert!(err.contains(new), "`data hist {old}` must name `{new}`: {err}");
        }
    }

    /// A group nobody has heard of names the GROUPS, not the verbs — the roster that is one level
    /// up from where the reader went wrong.
    #[test]
    fn the_groups_are_named_when_one_is_missing() {
        let err = parse_of(&["nonsense"]).unwrap_err();
        for group in ["hist", "realtime", "catalog", "source"] {
            assert!(err.contains(group), "the unknown-group error must name {group}: {err}");
        }
    }

    /// The sub-verb is REQUIRED, as it is on `backtest`. There is no bare `vike-cli data hist`.
    #[test]
    fn the_group_alone_is_not_a_command() {
        assert!(parse_of(&["hist"]).is_err());
    }

    // ── §9.3.2: the account-kind exclusion, its disclosure, and its refusal ──────────────────

    use std::collections::BTreeSet;

    fn note(count: usize, kinds: &[&str]) -> Option<String> {
        let set: BTreeSet<&str> = kinds.iter().copied().collect();
        super::account_exclusion_note(count, &set)
    }

    /// A note that fires on every run stops being read, so nothing withheld means nothing said.
    #[test]
    fn nothing_withheld_says_nothing() {
        assert_eq!(note(0, &[]), None);
    }

    /// The spec's own example sentence, §9.3.2. It names the KINDS rather than only a count,
    /// because a count alone cannot be acted on — an operator who sees "3 series are hidden" has
    /// to guess whether the thing they came for is among them.
    #[test]
    fn the_disclosure_names_the_kinds_and_the_plane_that_will_serve_them() {
        let n = note(3, &["exec_fill", "exec_order", "equity"]).unwrap();
        assert!(n.starts_with("note: 3 series of kinds "), "{n}");
        for kind in ["exec_fill", "exec_order", "equity"] {
            assert!(n.contains(kind), "the note must NAME {kind}: {n}");
        }
        assert!(n.contains("account data"), "{n}");
        assert!(n.contains("vike-cli account"), "the note must say WHERE they will be served: {n}");
    }

    /// One series is "1 series of kind X is", not "1 series of kinds X are".
    #[test]
    fn the_disclosure_inflects_for_one() {
        let n = note(1, &["equity"]).unwrap();
        assert!(n.contains("1 series of kind equity is in this store"), "{n}");
        assert!(!n.contains("kinds"), "{n}");
    }

    /// ⚠ The exclusion and the refusal answer DIFFERENT questions, and this is the refusal: a
    /// listing that ASKED for an account kind by name must not answer "no series match", because
    /// that sentence is false — the series exist and this plane declines to serve them.
    #[test]
    fn a_kind_filter_naming_account_data_is_refused_by_name() {
        for kind in vike_model::ACCOUNT_KINDS {
            let args = parse_of(&["hist", "ls", "--kind", kind]).expect("the FILTER still parses");
            let err = super::refuse_an_account_kind_filter(&args)
                .expect_err("an account kind must be refused, not filtered to nothing");
            assert!(err.contains(kind), "{err}");
            assert!(err.contains("vike-cli account"), "{err}");
        }
    }

    /// ⚠ The MARKET funding RATE is not account data and must never be refused as such. It is a
    /// BAR series under `interval=funding`; `docs/decisions/0080` separated the two names so that
    /// this test can be written at all.
    #[test]
    fn the_market_funding_rate_is_not_refused() {
        for kind in ["bar", "funding"] {
            let args = parse_of(&["hist", "ls", "--kind", kind]).expect("parses");
            assert!(
                super::refuse_an_account_kind_filter(&args).is_ok(),
                "{kind:?} is MARKET data — the funding RATE is a bar series under \
                 `interval=funding`, and refusing it would hide market data behind an account \
                 refusal, which is the collision 0079 exists to have resolved"
            );
        }
    }

    /// EXACT match only. `--kind` on a listing is a substring filter, and refusing every substring
    /// that could reach an account kind would put a roster's worth of guessing into a filter.
    #[test]
    fn a_substring_that_merely_overlaps_an_account_kind_stays_a_filter() {
        for kind in ["exec", "equ", "fill", "exec_f"] {
            let args = parse_of(&["hist", "ls", "--kind", kind]).expect("parses");
            assert!(
                super::refuse_an_account_kind_filter(&args).is_ok(),
                "{kind:?} is a substring, not a name — it must stay a filter"
            );
        }
    }

    use super::*;
    use crate::cmd::args::HELP_SENTINEL;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()), None)
    }

    /// The address ladder, all three rungs — the twin of
    /// `crate::cmd::backtest::resolve_addr`'s `the_address_ladder_is_cli_then_configured_then_default`,
    /// one plane over.
    ///
    /// ⚠ **What would make this fail:** dropping the `configured_addr` rung from `parse`, which is
    /// how this verb behaved until that parameter existed — `config.datahub_addr` was read by
    /// `crates/vike-desktop/src/app_methods.rs` and by nothing here, so a box whose datahub is not
    /// on [`DEFAULT_ADDR`] reached it from the GUI and dialled the compiled-in default from the CLI.
    #[test]
    fn the_address_ladder_is_cli_then_configured_then_default() {
        let cli = parse(
            ["hist", "ls", "--addr", "1.2.3.4:9"].iter().map(|s| s.to_string()),
            Some("5.6.7.8:9"),
        )
        .unwrap();
        assert_eq!(cli.addr, "1.2.3.4:9", "the flag outranks the setting");

        let configured =
            parse(["hist", "ls"].iter().map(|s| s.to_string()), Some("5.6.7.8:9")).unwrap();
        assert_eq!(configured.addr, "5.6.7.8:9", "the setting is the middle rung");

        let neither = parse(["hist", "ls"].iter().map(|s| s.to_string()), None).unwrap();
        assert_eq!(neither.addr, DEFAULT_ADDR, "and the compiled-in default is the floor");

        // A blank rung is SKIPPED, not honoured: an `Environment=` line that set nothing must not
        // aim this client at an empty address.
        let blank = parse(["hist", "ls"].iter().map(|s| s.to_string()), Some("   ")).unwrap();
        assert_eq!(blank.addr, DEFAULT_ADDR, "a blank setting falls through to the default");
    }

    /// ⚠ The setting says WHERE to dial, never WHETHER to — and `rm` is where the difference
    /// deletes something. `execute_rm` routes on `addr_given`, so a configured `config.datahub_addr`
    /// must leave it FALSE: a box that merely names its datahub has not asked for every `data hist rm` to
    /// run against that datahub instead of its local store.
    ///
    /// **What would make this fail:** setting `addr_given` from the resolved address rather than
    /// from the flag — the tidying a reader who saw only the ladder above would reach for.
    #[test]
    fn a_configured_address_does_not_ask_for_the_remote_route() {
        let configured =
            parse(["hist", "ls"].iter().map(|s| s.to_string()), Some("5.6.7.8:9")).unwrap();
        assert_eq!(configured.addr, "5.6.7.8:9", "the address resolved…");
        assert!(!configured.addr_given, "…and the ROUTE was still not asked for");

        let flagged =
            parse(["hist", "ls", "--addr", "5.6.7.8:9"].iter().map(|s| s.to_string()), None)
                .unwrap();
        assert!(flagged.addr_given, "naming it on the line IS asking");
    }

    #[test]
    fn each_subcommand_parses() {
        assert_eq!(
            parse_of(&["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7"]).unwrap().sub,
            Sub::Fetch
        );
        // ⚠ `seed-demo` is no longer a VERB — it is a source. The verb is `fetch` and the
        // axis carries what used to be the name.
        let demo = parse_of(&["hist", "fetch", "--source", "demo"]).unwrap();
        assert_eq!(demo.sub, Sub::Fetch);
        assert_eq!(demo.source, Source::Demo);
        assert_eq!(parse_of(&["hist", "ls"]).unwrap().sub, Sub::List);
        assert_eq!(parse_of(&["hist", "coverage"]).unwrap().sub, Sub::Coverage);
    }

    #[test]
    fn help_short_circuits_at_both_levels() {
        assert_eq!(parse_of(&["--help"]).unwrap_err(), HELP_SENTINEL);
        assert_eq!(parse_of(&["hist", "fetch", "-h"]).unwrap_err(), HELP_SENTINEL);
    }

    /// The verb takes no default action, so a bare `data` must name every subcommand there is —
    /// this message is the only place a user who typed the verb alone learns what it can do.
    #[test]
    fn a_missing_subcommand_names_every_subcommand_that_exists() {
        // ⚠ `data hist` with no verb, not bare `data` — the group split moved the roster one level
        // down, so THIS is now the message whose whole job is to name what the group can do. Bare
        // `data` names the GROUPS instead, which `the_groups_are_named_when_one_is_missing` covers.
        let err = parse_of(&["hist"]).unwrap_err();
        // ⚠ EVERY row of [`SUBCOMMANDS`], not a hand-written list — the hand-written list in this
        // test AND in the message it checked both omitted `rm`, which had shipped months earlier.
        // A test that names its own expectations cannot catch a roster going short.
        for sub in SUBCOMMANDS {
            assert!(
                err.contains(sub.as_str()),
                "the missing-subcommand error must name {}: {err}",
                sub.as_str()
            );
        }
    }

    /// Every advertised subcommand is REACHABLE by the name it advertises, both directions:
    /// [`SUBCOMMANDS`] round-trips through [`parse`]'s match, and nothing is in one and not the
    /// other. A verb in [`USAGE`] that `parse` answers with "unknown `data` subcommand" is the
    /// failure this exists to make impossible.
    #[test]
    fn all_subcommands_are_reachable_by_the_name_they_advertise() {
        for sub in SUBCOMMANDS {
            // Parsed with the flags each one REQUIRES, so a refusal here can only be about the
            // NAME rather than about a missing argument.
            let argv: Vec<&str> = match sub {
                Sub::Fetch => vec!["hist", "fetch", "d:S:1h", "--days", "1"],
                Sub::Export => vec!["hist", "export", "d:S:1h", "--out", "s.parquet"],
                Sub::Rm => vec!["hist", "rm", "--kind", "bar", "--venue", "d", "--symbol", "S"],
                // ⚠ `--symbol` is REQUIRED here where it is optional on `rm` — the one shape
                // difference between the two selectors, and the reason this arm cannot reuse
                // `rm`'s.
                Sub::Repair => {
                    vec!["hist", "repair", "--kind", "trade", "--venue", "d", "--symbol", "S"]
                }
                // A spec AND a criterion: the one read verb that requires both, and the only one
                // for which `hist gate` alone is two separate refusals rather than a name lookup.
                Sub::Gate => vec!["hist", "gate", "d:S:1h", "--require-days", "30"],
                // A spec AND a window: the READ verb that requires a bound rather than merely
                // accepting one — §8.2's first rule, which `hist get d:S:1h` alone would trip.
                Sub::Get => vec!["hist", "get", "d:S:1h", "--days", "1"],
                other => vec!["hist", other.as_str()],
            };
            let parsed = parse_of(&argv).unwrap_or_else(|e| panic!("{}: {e}", sub.as_str()));
            assert_eq!(parsed.sub, *sub, "{} parsed as a different subcommand", sub.as_str());
        }
    }

    /// ⚠ The shape check is a TYPO catcher, not a roster. Three non-empty parts pass whatever they
    /// name; two, four, or an empty part do not.
    #[test]
    fn the_spec_shape_is_checked_and_nothing_else_is() {
        assert!(check_spec("binance:BTCUSDT:1h").is_ok());
        assert!(check_spec("notavenue:WHATEVER:3q").is_ok(), "the engine judges the venue, not us");
        for bad in ["binance:BTCUSDT", "a:b:c:d", "binance::1h", ":BTCUSDT:1h", "binance:BTCUSDT:"]
        {
            let err = check_spec(bad).unwrap_err();
            assert!(err.contains(bad), "the message names what was typed: {err}");
            assert!(err.contains("VENUE:SYMBOL:INTERVAL"), "…and the shape it wanted: {err}");
        }
    }

    /// A fetch with no window is a usage error naming both spellings — the engine would refuse it
    /// too, but only after a process spawn and in a binary the user did not name.
    #[test]
    fn a_fetch_needs_a_window() {
        let err = parse_of(&["hist", "fetch", "binance:BTCUSDT:1h"]).unwrap_err();
        assert!(err.contains("--days"), "{err}");
        assert!(err.contains("--from"), "{err}");
    }

    /// The two window forms are EXCLUSIVE, and a half-range is named for the half that is missing.
    #[test]
    fn the_window_forms_do_not_mix() {
        assert!(window_from(Some("7".into()), Some("a".into()), Some("b".into())).is_err());
        assert!(window_from(None, Some("a".into()), None).unwrap_err().contains("--to"));
        assert!(window_from(None, None, Some("b".into())).unwrap_err().contains("--from"));
        assert_eq!(
            window_from(None, Some("a".into()), Some("b".into())).unwrap(),
            Window::Range { from: "a".into(), to: "b".into() }
        );
    }

    /// `--days` is a whole positive number: a `--days 7.5` or a `--days 0` is caught here rather
    /// than becoming a fetch that covers nothing.
    #[test]
    fn days_must_be_a_positive_whole_number() {
        assert!(window_from(Some("7".into()), None, None).is_ok());
        assert!(window_from(Some("7.5".into()), None, None).is_err());
        assert!(window_from(Some("-3".into()), None, None).is_err());
        assert!(window_from(Some("0".into()), None, None).unwrap_err().contains("no time"));
    }

    /// `seed-demo` refuses every fetch-shaped flag rather than ignoring it — a `--days 30` that
    /// quietly did nothing would leave an operator believing they had seeded a month.
    #[test]
    fn seed_demo_refuses_a_window_and_a_spec() {
        assert!(
            parse_of(&["hist", "fetch", "--source", "demo", "--days", "30"])
                .unwrap_err()
                .contains("--days")
        );
        assert!(
            parse_of(&["hist", "fetch", "--source", "demo", "--from", "0"])
                .unwrap_err()
                .contains("--from")
        );
        assert!(
            parse_of(&["hist", "fetch", "--source", "demo", "binance:BTCUSDT:1h"])
                .unwrap_err()
                .contains("takes no spec")
        );
        // …and the one flag that DOES apply to it still parses.
        assert_eq!(
            parse_of(&["hist", "fetch", "--source", "demo", "--store", "/s"])
                .unwrap()
                .store
                .as_deref(),
            Some("/s")
        );
    }

    /// A second bare word is a shell-quoting accident far more often than an intention, and
    /// ignoring it would fetch a series nobody asked for.
    #[test]
    fn a_second_positional_is_refused_naming_both() {
        let err =
            parse_of(&["hist", "fetch", "binance:BTCUSDT:1h", "okx:BTC-USDT:1h", "--days", "7"])
                .unwrap_err();
        assert!(err.contains("okx:BTC-USDT:1h") && err.contains("binance:BTCUSDT:1h"), "{err}");
    }

    /// THE translation: what the engine is actually asked to do. Pinned as argv because that is
    /// the whole product of this module — everything else is the engine's.
    #[test]
    fn the_engine_argv_is_the_translation() {
        // ⚠ `fetch` was the first two cases here and is GONE from this test, because it is gone
        // from the engine: it asks a datahub now (see [`execute_fetch`]) and `parse` refuses the
        // `--store`/`--engine` these cases passed it. `engine_argv`'s `Sub::Fetch` arm went with
        // them — an arm nothing can reach is not coverage, it is a second answer waiting to drift
        // from the one that runs.
        assert_eq!(
            engine_argv(&parse_of(&["hist", "fetch", "--source", "demo"]).unwrap()),
            ["data", "seed-demo"]
        );
        assert_eq!(
            engine_argv(
                &parse_of(&["hist", "fetch", "--source", "demo", "--store", "/s"]).unwrap()
            ),
            ["data", "seed-demo", "--store", "/s"]
        );
        assert_eq!(
            engine_argv(
                &parse_of(&["hist", "fetch", "--source", "starter", "--store", "/s"]).unwrap()
            ),
            ["data", "fetch-starter", "--store", "/s"]
        );

        // ⚠ `export`'s two bounds are INDEPENDENT — see [`ExportRange`]. Each of the four
        // combinations reaches the engine as itself, which is the whole difference from a
        // [`Window`].
        assert_eq!(
            engine_argv(&parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet"]).unwrap()),
            ["data", "export", "demo:D:1h", "--out", "s.parquet"]
        );
        assert_eq!(
            engine_argv(
                &parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet", "--from", "5"])
                    .unwrap()
            ),
            ["data", "export", "demo:D:1h", "--out", "s.parquet", "--from", "5"],
            "a lone --from is a WELL-FORMED export bound, where on a fetch it is a usage error"
        );
        assert_eq!(
            engine_argv(
                &parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet", "--to", "9"])
                    .unwrap()
            ),
            ["data", "export", "demo:D:1h", "--out", "s.parquet", "--to", "9"],
            "…and so is a lone --to"
        );
    }

    /// The two subcommands ruling 12 moved that had NO home here before: their grammar, and the
    /// one place it deliberately diverges from `fetch`'s.
    #[test]
    fn export_and_fetch_starter_carry_their_own_grammar() {
        // `export` REQUIRES a spec and a destination; neither has a default a store could supply.
        assert!(
            parse_of(&["hist", "export", "--out", "s.parquet"])
                .unwrap_err()
                .contains("needs a spec")
        );
        assert!(parse_of(&["hist", "export", "demo:D:1h"]).unwrap_err().contains("--out"));
        assert!(
            parse_of(&["hist", "export", "not-a-spec", "--out", "s"])
                .unwrap_err()
                .contains("VENUE")
        );

        // ⚠ `--days` is refused BY NAME rather than folded into a range: it counts back from NOW,
        // which bounds a FETCH and says nothing about what a store already holds.
        let err =
            parse_of(&["hist", "export", "demo:D:1h", "--out", "s", "--days", "7"]).unwrap_err();
        assert!(err.contains("--days") && err.contains("FETCH"), "{err}");

        // `fetch-starter` is `seed-demo`'s shape: no spec, no window, `--store` and nothing else.
        assert!(
            parse_of(&["hist", "fetch", "--source", "starter", "d:S:1h"])
                .unwrap_err()
                .contains("takes no spec")
        );
        assert!(
            parse_of(&["hist", "fetch", "--source", "starter", "--days", "7"])
                .unwrap_err()
                .contains("--days")
        );
        assert_eq!(
            parse_of(&["hist", "fetch", "--source", "starter", "--store", "/s"])
                .unwrap()
                .store
                .as_deref(),
            Some("/s")
        );

        // `--out` belongs to `export` alone, and is refused elsewhere by name rather than ignored.
        // ⚠ The two absorbed verbs are now SOURCES, so they are exercised through the axis — a
        // bare `fetch-starter` would be refused for its NAME and prove nothing about `--out`.
        for argv in [
            &["hist", "fetch", "--source", "starter", "--out", "s.parquet"][..],
            &["hist", "fetch", "--source", "demo", "--out", "s.parquet"][..],
            &["hist", "ls", "--out", "s.parquet"][..],
            &["hist", "rm", "--out", "s.parquet"][..],
        ] {
            let err = parse_of(argv).unwrap_err();
            assert!(err.contains("--out"), "{argv:?}: {err}");
        }
    }

    /// **THE ROUTE SWITCH.** `--addr` — TYPED, not merely resolved — is what selects the remote
    /// export, and the two routes then take different grammars.
    ///
    /// ⚠ The load-bearing half is the SECOND assertion: a line with no `--addr` builds NO
    /// [`export::Plan`] even though [`Args::addr`] is always `Some`. Reading the resolved address
    /// would send every local export over a socket.
    #[test]
    fn the_export_route_is_chosen_by_a_typed_addr_and_not_by_the_resolved_one() {
        let local = parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet"])
            .expect("the shipped local grammar is unchanged");
        assert!(local.export.is_none(), "no --addr is the ENGINE route");
        assert!(!local.addr_given, "…and the resolved default is not a request");
        assert!(!local.addr.is_empty(), "…while the address itself is still resolved");

        let remote = parse_of(&[
            "hist",
            "export",
            "demo:D:1h",
            "--out",
            "s.jsonl",
            "--addr",
            "h:1",
            "--format",
            "jsonl",
            "--from",
            "1000",
            "--to",
            "2000",
        ])
        .expect("the remote grammar");
        let e = remote.export.as_ref().expect("--addr selects the walk");
        assert_eq!(e.kind, export::Kind::Bar);
        assert_eq!(e.wire, export::Wire::Jsonl);
        assert_eq!(e.bounds, (1000, 2000));
        assert_eq!(e.step_ms, export::Kind::Bar.default_window_ms());
        assert!(e.step_defaulted);
        // ...and the LOCAL route's range field stays empty, so no arm can read a half-filled one.
        assert!(remote.export_range.is_none());
    }

    /// Each route refuses the OTHER's flags by name, and each refusal names the flag that would
    /// reach the route the operator wanted.
    #[test]
    fn each_export_route_refuses_the_others_flags_and_names_the_way_across() {
        // REMOTE-only flags on the LOCAL route.
        for (argv, needle) in [
            (&["hist", "export", "d:S:1h", "--out", "o", "--kind", "trade"][..], "add --addr"),
            (&["hist", "export", "d:S:1h", "--out", "o", "--window", "1d"][..], "--addr"),
            (&["hist", "export", "d:S:1h", "--out", "o", "--format", "csv"][..], "--addr"),
        ] {
            let err = parse_of(argv).unwrap_err();
            assert!(err.contains(needle), "{argv:?} must name the way across: {err}");
        }

        // ...and the LOCAL route's store flags on the REMOTE one.
        let clash =
            parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--store", "/s"])
                .unwrap_err();
        assert!(clash.contains("two DIFFERENT stores"), "{clash}");

        // ⚠ THE ANTI-VACUITY CONTROL: each of those flags is ACCEPTED on the route it belongs to,
        // so the refusals above are about the route rather than about the flag not existing.
        assert!(
            parse_of(&[
                "hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "csv",
                "--kind", "trade", "--window", "1d", "--from", "1", "--to", "2",
            ])
            .is_err_and(|e| e.contains("no INTERVAL")),
            "a THREE-part spec on a tick kind is the only thing wrong with that line"
        );
        assert!(
            parse_of(&[
                "hist", "export", "d:S", "--out", "o", "--addr", "h:1", "--format", "csv",
                "--kind", "trade", "--window", "1d", "--from", "1", "--to", "2",
            ])
            .is_ok(),
            "…and with the interval dropped, every one of those flags is accepted"
        );
        assert!(
            parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--store", "/s"]).is_ok(),
            "--store is accepted on the route it belongs to"
        );
    }

    /// A remote export needs BOTH bounds, and the refusal names the verb that PRINTS them.
    #[test]
    fn a_remote_export_needs_both_bounds_and_names_where_to_read_them() {
        let base = ["hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "jsonl"];
        for extra in [&[][..], &["--from", "1"][..], &["--to", "9"][..]] {
            let mut argv = base.to_vec();
            argv.extend_from_slice(extra);
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains("BOTH --from and --to"), "{extra:?}: {err}");
            assert!(err.contains("data hist ls --venue d --name S"), "{extra:?}: {err}");
        }
        // ANTI-VACUITY: the pair together is accepted, so the refusal is about absence rather than
        // about the flags being rejected outright.
        let mut both = base.to_vec();
        both.extend_from_slice(&["--from", "1", "--to", "9"]);
        assert!(parse_of(&both).is_ok(), "both bounds is the accepted line");

        // ...and an INVERTED pair is refused rather than swapped, because the file it would write
        // is empty and an empty file reads exactly like an empty store.
        let mut inverted = base.to_vec();
        inverted.extend_from_slice(&["--from", "9", "--to", "1"]);
        let err = parse_of(&inverted).unwrap_err();
        assert!(err.contains("AFTER"), "{err}");
    }

    /// The LOCAL route is BYTE-IDENTICAL to what shipped, which is the property this whole change
    /// had to preserve — a second route may not alter the first.
    ///
    /// ⚠ `--format parquet` is the one ADDITION to it, and it changes nothing: it names what this
    /// route already writes. That is deliberately not the same as being ignored — see
    /// [`export::refuse_a_wire_on_the_local_route`].
    #[test]
    fn the_local_export_route_is_unchanged_and_parquet_merely_names_what_it_writes() {
        let plain = parse_of(&[
            "hist",
            "export",
            "demo:D:1h",
            "--out",
            "s.parquet",
            "--from",
            "5",
            "--store",
            "/s",
        ])
        .expect("the shipped line");
        let named = parse_of(&[
            "hist",
            "export",
            "demo:D:1h",
            "--out",
            "s.parquet",
            "--from",
            "5",
            "--store",
            "/s",
            "--format",
            "parquet",
        ])
        .expect("…and the same line naming the format it already writes");
        assert_eq!(plain, named, "naming `parquet` here changes nothing about the request");
        assert!(plain.export.is_none());
        assert_eq!(
            plain.export_range,
            Some(ExportRange { from: Some("5".into()), to: None }),
            "a LONE --from still stands alone on this route"
        );
        // ANTI-VACUITY: the engine argv is still what it was, so the equality above is over a
        // request that genuinely reaches the child.
        assert!(engine_argv(&plain).contains(&"--out".to_string()));
    }

    /// `--json` parses on EVERY write subcommand, takes no value, and — like `--engine` — is consumed
    /// here rather than forwarded. The engine's own `--json` is a different flag on a different
    /// code path (it renders a `BacktestReport`), and passing this one through would ask `--fetch`
    /// for a document it does not produce.
    #[test]
    fn json_parses_on_every_write_subcommand_and_is_not_forwarded() {
        for argv in [
            vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7", "--json"],
            vec!["hist", "fetch", "--source", "demo", "--json"],
            vec!["hist", "fetch", "--source", "starter", "--json"],
            vec!["hist", "export", "demo:D:1h", "--out", "s.parquet", "--json"],
        ] {
            let a = parse_of(&argv).unwrap();
            assert!(a.json, "{argv:?}");
            assert!(!engine_argv(&a).iter().any(|s| s == "--json"), "{:?}", engine_argv(&a));
        }
        assert!(
            !parse_of(&["hist", "fetch", "--source", "demo"]).unwrap().json,
            "absent means absent"
        );
        // A value is refused rather than swallowed — the same `no_value` rung every other
        // valueless flag in this crate uses.
        assert!(
            parse_of(&["hist", "fetch", "--source", "demo", "--json=1"])
                .unwrap_err()
                .contains("--json")
        );
    }

    /// The document, field by field. It is built from the SAME parsed `Args` and the SAME argv the
    /// engine was handed, so a machine and a person cannot be told different things about one run.
    #[test]
    fn the_json_document_carries_the_request_the_engine_and_its_report() {
        // ⚠ This was a `fetch` until that verb left the engine route. `export` is the surviving
        // engine verb that carries a SPEC, so the series half is pinned exactly as before.
        //
        // ⚠ `report_json`'s `args.window` branch is still EXERCISED — the range block further down
        // builds a `fetch` and renders it directly — but it is no longer REACHABLE in production,
        // because `report_json` is called only from the engine route and `fetch` no longer takes
        // it. The renderer stays: `--from`/`--to` on an engine verb is a grammar this module may
        // grow again, and deleting a renderer is a wider change than retiring a route. Stated so
        // the difference between "tested" and "reachable" is known here rather than found later.
        let a = parse_of(&[
            "hist",
            "export",
            "binance:BTCUSDT:1h",
            "--out",
            "s.parquet",
            "--store",
            "/s",
            "--json",
        ])
        .unwrap();
        let argv = engine_argv(&a);
        let doc: serde_json::Value = serde_json::from_str(&report_json(
            &a,
            &engine::Engine::standalone("/opt/backtest"),
            &argv,
            &["12 bars".to_string()],
        ))
        .expect("report_json writes JSON");

        assert_eq!(doc["subcommand"], "export");
        assert_eq!(doc["store"], "/s");
        assert_eq!(doc["series"]["venue"], "binance");
        assert_eq!(doc["series"]["symbol"], "BTCUSDT");
        assert_eq!(doc["series"]["interval"], "1h");
        assert_eq!(doc["series"]["spec"], "binance:BTCUSDT:1h");
        assert_eq!(doc["engine"], "/opt/backtest");
        assert_eq!(doc["engine_argv"][0], "data");
        assert_eq!(doc["engine_argv"][1], "export");
        assert_eq!(doc["report"][0], "12 bars");

        // The range window is the OTHER form, and a caller must be able to tell which it got
        // without re-parsing the argv.
        let range =
            parse_of(&["hist", "fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100", "--json"])
                .unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&report_json(&range, &engine::Engine::standalone("b"), &[], &[]))
                .unwrap();
        assert_eq!(doc["window"]["from"], "0");
        assert_eq!(doc["window"]["to"], "100");
        assert!(doc["window"]["days"].is_null());

        // `seed-demo` takes neither a spec nor a window, and both are NULL rather than absent: a
        // machine can tell "no series" from "the field is gone" only if the field is there.
        let seed = parse_of(&["hist", "fetch", "--source", "demo", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&report_json(&seed, &engine::Engine::standalone("b"), &[], &[]))
                .unwrap();
        // ⚠ The verb is `fetch` now and the SOURCE carries what the old name did. Both are
        // asserted, because either alone would let the collapse lose a fact.
        assert_eq!(doc["subcommand"], "fetch");
        assert_eq!(doc["source"], "demo");
        assert!(doc["series"].is_null());
        assert!(doc["window"].is_null());
        // ...and an unnamed store is NULL, never a guessed path: the ENGINE resolves the root, in
        // another process, and this side would be inventing one.
        assert!(doc["store"].is_null());
    }

    /// `--engine` never reaches the child: it says WHICH binary to run, not what to tell it.
    #[test]
    fn the_engine_flag_is_consumed_here_and_not_forwarded() {
        let a =
            parse_of(&["hist", "fetch", "--source", "demo", "--engine", "/opt/backtest"]).unwrap();
        assert_eq!(a.engine.as_deref(), Some("/opt/backtest"));
        assert!(!engine_argv(&a).iter().any(|s| s == "--engine"), "{:?}", engine_argv(&a));
    }

    // ---- `--produced-by`: the assertion's own rules, on BOTH routes ----

    /// ⚠ **A BLANK `--produced-by` is refused on BOTH routes, in BOTH spellings, with the reason
    /// that is actually true of it.**
    ///
    /// It was already refused — as a row in the SELECTOR loop, whose message argues about the
    /// store's grouped-series `symbol=` sentinel and tells the operator to *omit the flag to
    /// wildcard the dimension*. Both halves are false here: this is not a dimension, and omitting
    /// it turns the assertion OFF, which on a sweep is refused outright. So the only guard between
    /// a blank prefix and the wire was an untested line that did not know what it was guarding —
    /// and lifting `--produced-by` out of that loop, which is what a `rm` refactor does first, was
    /// enough to drop it silently.
    ///
    /// The needle is the CONSEQUENCE ("matches every key"), not the flag name, because that is
    /// what distinguishes the right refusal from the wrong one that was already passing.
    #[test]
    fn a_blank_produced_by_is_refused_on_both_routes_with_the_real_reason() {
        for route in [vec![], vec!["--addr", "1.2.3.4:9"]] {
            for blank in ["", "   "] {
                let mut argv = vec![
                    "hist",
                    "rm",
                    "--kind",
                    "bar",
                    "--venue",
                    "binance",
                    "--produced-by",
                    blank,
                ];
                argv.extend(route.iter().copied());
                let err = parse_of(&argv).unwrap_err();
                assert!(err.contains("--produced-by"), "{argv:?}: {err}");
                assert!(
                    err.contains("matches every key"),
                    "the refusal must give the reason that is TRUE of a provenance prefix, not \
                     the grouped-series sentinel argument it inherited: {err}"
                );
                assert!(
                    !err.contains("wildcard the dimension"),
                    "…and must not tell the operator to omit the flag, which on a sweep is the \
                     one thing that is refused: {err}"
                );
            }
        }
    }

    /// ⚠ **A PRODUCER PATH is refused on the REMOTE route and accepted on the LOCAL one, because
    /// only one of the two resolves it.**
    ///
    /// `vike_data::store_kind::resolve_produced_by` has one caller in the tree —
    /// `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series`, i.e. the local route.
    /// `crates/vike-datahub/src/server.rs`'s `delete_series_verb` calls nothing of the kind, so the
    /// same line under `--addr` asserted the PATH as a literal prefix, matched no commit key, and
    /// told the operator their store had foreign provenance — a serious-looking finding about the
    /// data that was really an unresolved flag.
    #[test]
    fn a_producer_path_is_refused_on_the_remote_route_and_forwarded_on_the_local_one() {
        let path = "crates/vike-data/src/demo.rs";
        let base = ["hist", "rm", "--kind", "bar", "--venue", "demo", "--produced-by", path];

        let err = parse_of(&[&base[..], &["--addr", "1.2.3.4:9"][..]].concat()).unwrap_err();
        assert!(err.contains("PRODUCER PATH"), "{err}");
        assert!(err.contains("--store"), "…and it names the route that DOES resolve one: {err}");

        // The local route forwards it verbatim for the engine to resolve — the behaviour this
        // whole asymmetry exists to preserve rather than to flatten.
        let local = parse_of(&base).expect("a producer path is legitimate on the local route");
        let rm = local.rm.as_ref().expect("rm args");
        assert_eq!(rm.produced_by.as_deref(), Some(path));
        assert!(
            rm_engine_argv(&local, rm).iter().any(|a| a == path),
            "the path reaches the engine unresolved: {:?}",
            rm_engine_argv(&local, rm)
        );
    }

    /// A LITERAL prefix is untouched on both routes — the refusals above are narrow, and a prefix
    /// carrying no `/` is the ordinary spelling both sides understand.
    #[test]
    fn a_literal_prefix_reaches_both_routes_unchanged() {
        for route in [vec![], vec!["--addr", "1.2.3.4:9"]] {
            let mut argv = vec![
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "demo",
                "--produced-by",
                "panel_bars:",
            ];
            argv.extend(route.iter().copied());
            let a = parse_of(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            assert_eq!(a.rm.as_ref().and_then(|r| r.produced_by.as_deref()), Some("panel_bars:"));
        }
    }

    /// A glob in `--produced-by` is refused with the reason that fits a PREFIX: it is matched with
    /// `starts_with`, so `pmxt:*` is a literal that matches nothing rather than a pattern.
    #[test]
    fn a_glob_in_the_prefix_is_refused_as_a_prefix_rather_than_as_a_dimension() {
        let err =
            parse_of(&["hist", "rm", "--kind", "bar", "--venue", "d", "--produced-by", "pmxt:*"])
                .unwrap_err();
        assert!(err.contains("--produced-by") && err.contains("starts_with"), "{err}");
    }

    // ---- `repair`: the grammar ----

    /// ⚠ **The one shape rule that differs from `rm`'s, and the reason this verb exists.** An
    /// omitted dimension WILDCARDS on `rm`; here it names nothing, because a repair rebuilds
    /// exactly one series' index. The refusal must carry the argument (a per-series lock hold, a
    /// per-series verdict) rather than only the rule, because "why can't I repair a whole venue"
    /// is the first question an operator asks.
    #[test]
    fn repair_refuses_a_wildcard_and_argues_for_one_series() {
        let err = parse_of(&["hist", "repair", "--kind", "bar", "--venue", "binance"]).unwrap_err();
        assert!(err.contains("--symbol S or --group G"), "{err}");
        assert!(err.contains("per-series"), "the refusal must carry the argument: {err}");
        assert!(err.contains("run this verb several times"), "…and the way out: {err}");
    }

    /// Both spellings of a named series parse, and the `symbol`/`group` alternative is refused as a
    /// PAIR exactly as `rm` refuses it — one store, one rule.
    #[test]
    fn repair_takes_either_spelling_of_a_named_series() {
        let a = parse_of(&[
            "hist",
            "repair",
            "--kind",
            "bar",
            "--venue",
            "b",
            "--symbol",
            "S",
            "--interval",
            "1m",
        ])
        .unwrap();
        let r = a.repair.as_ref().expect("a RepairArgs");
        assert_eq!(r.kind, "bar");
        assert_eq!(r.symbol.as_deref(), Some("S"));
        assert_eq!(r.interval.as_deref(), Some("1m"));
        assert!(r.group.is_none());
        // ...and the filter it was PARSED into is left empty, so no listing code can read a
        // selector out of it.
        assert_eq!(a.filter, Filter::default());

        let g = parse_of(&[
            "hist",
            "repair",
            "--kind",
            "book",
            "--venue",
            "polymarket",
            "--group",
            "btc-5m",
        ])
        .unwrap();
        assert_eq!(g.repair.as_ref().unwrap().group.as_deref(), Some("btc-5m"));

        for (argv, needle) in [
            (
                vec![
                    "hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "S", "--group",
                    "G",
                ],
                "ALTERNATIVES",
            ),
            (
                vec![
                    "hist",
                    "repair",
                    "--kind",
                    "b",
                    "--venue",
                    "v",
                    "--group",
                    "G",
                    "--interval",
                    "1m",
                ],
                "no `interval=` segment",
            ),
            (vec!["hist", "repair", "--venue", "v", "--symbol", "S"], "repair needs --kind"),
            (vec!["hist", "repair", "--kind", "b", "--symbol", "S"], "repair needs --venue"),
        ] {
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(needle), "{argv:?} must say {needle:?}: {err}");
        }
    }

    /// ⚠ **`--addr` is REFUSED, not ignored**, and the refusal carries the reason that would
    /// otherwise be re-litigated: a datahub can only name series it ENUMERATED, and the series this
    /// repairs is by definition in no enumeration.
    #[test]
    fn repair_refuses_the_remote_route_and_says_why() {
        let err = parse_of(&[
            "hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "S", "--addr", "h:1",
        ])
        .unwrap_err();
        assert!(err.contains("--addr"), "{err}");
        assert!(err.contains("ENUMERATED"), "the load-bearing half of the argument: {err}");
        assert!(err.contains("--store"), "…and the route that does work: {err}");
    }

    /// `--produced-by` guards a DELETE and guards nothing here, so it is refused by name rather
    /// than accepted as a flag that looks like a safety rail and is not one.
    #[test]
    fn repair_refuses_produced_by_because_it_removes_no_row() {
        let err = parse_of(&[
            "hist",
            "repair",
            "--kind",
            "b",
            "--venue",
            "v",
            "--symbol",
            "S",
            "--produced-by",
            "panel_bars:",
        ])
        .unwrap_err();
        assert!(err.contains("--produced-by"), "{err}");
        assert!(err.contains("only `rm` deletes"), "{err}");
        assert!(err.contains("removes no row"), "{err}");
    }

    /// The read half's browse aids and the fetch half's window are refused here for the reasons
    /// they do not apply, never as "unknown option" — this module's standing rule.
    #[test]
    fn repair_refuses_the_other_halves_flags_with_their_reasons() {
        let base = ["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "S"];
        for (extra, needle) in [
            (vec!["--name", "BTC"], "may be one no listing can show you"),
            (vec!["--days", "7"], "no time range to rebuild"),
            (vec!["--from", "0"], "no time range to rebuild"),
        ] {
            let argv: Vec<&str> = base.iter().chain(extra.iter()).copied().collect();
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(needle), "{argv:?} must say {needle:?}: {err}");
        }
        let err = parse_of(&[
            "hist",
            "repair",
            "--kind",
            "b",
            "--venue",
            "v",
            "--symbol",
            "S",
            "binance:BTCUSDT:1h",
        ])
        .unwrap_err();
        assert!(err.contains("no VENUE:SYMBOL:INTERVAL spec"), "{err}");
    }

    /// An EMPTY or GLOBBED dimension is refused — and with `repair`'s own reason, not `rm`'s: there
    /// is no wildcard here to fall back to.
    #[test]
    fn repair_refuses_a_blank_or_globbed_dimension() {
        let err = parse_of(&["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", ""])
            .unwrap_err();
        assert!(
            err.contains("EMPTY value") && err.contains("no wildcard to fall back to"),
            "{err}"
        );
        let err = parse_of(&["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "BTC*"])
            .unwrap_err();
        assert!(err.contains("glob character") && err.contains("ONE series exactly"), "{err}");
    }

    /// ⚠ **THE REHEARSAL DEFAULT, in the argv the child actually receives.** A line with neither
    /// flag is forwarded as `--dry-run`, so the child is TOLD what this side decided rather than
    /// relying on a default spelled in two places; `--dry-run` WINS over `--yes`; and only
    /// `--yes` alone produces a write.
    #[test]
    fn the_repair_engine_argv_forwards_the_rehearsal_decision() {
        let argv_of = |extra: &[&str]| {
            let base = [
                "hist",
                "repair",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1m",
            ];
            let all: Vec<&str> = base.iter().chain(extra.iter()).copied().collect();
            let a = parse_of(&all).unwrap();
            repair_engine_argv(&a, a.repair.as_ref().unwrap())
        };
        assert_eq!(
            argv_of(&[]),
            vec![
                "data",
                "repair",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1m",
                "--dry-run"
            ],
            "the BARE form rehearses, and says so to the child"
        );
        assert!(argv_of(&["--dry-run"]).contains(&"--dry-run".to_string()));
        assert!(!argv_of(&["--dry-run"]).contains(&"--yes".to_string()));
        assert_eq!(
            argv_of(&["--yes"]).last().map(String::as_str),
            Some("--yes"),
            "--yes alone performs it"
        );
        let both = argv_of(&["--yes", "--dry-run"]);
        assert!(both.contains(&"--dry-run".to_string()), "--dry-run WINS over --yes: {both:?}");
        assert!(!both.contains(&"--yes".to_string()), "{both:?}");
    }

    /// `--json` is FORWARDED (like `rm`'s and unlike `fetch`'s) because the engine owns the only
    /// document that can carry the RESOLVED store root, and `--store`/`--engine` keep their usual
    /// asymmetry: the store is forwarded, the engine path is consumed here.
    #[test]
    fn repair_forwards_json_and_the_store_but_consumes_the_engine_path() {
        let a = parse_of(&[
            "hist",
            "repair",
            "--kind",
            "trade",
            "--venue",
            "d",
            "--symbol",
            "S",
            "--json",
            "--store",
            "/srv/hist",
            "--engine",
            "/opt/backtest",
        ])
        .unwrap();
        let argv = repair_engine_argv(&a, a.repair.as_ref().unwrap());
        assert!(argv.contains(&"--json".to_string()), "{argv:?}");
        assert_eq!(
            argv.iter().position(|s| s == "--store").map(|i| argv[i + 1].clone()),
            Some("/srv/hist".to_string())
        );
        assert!(!argv.iter().any(|s| s == "--engine"), "consumed HERE, never forwarded: {argv:?}");
    }

    /// The selector flags are refused on the verbs that have no series identity — and the message
    /// now names BOTH verbs that take them, because `repair` joined the set.
    #[test]
    fn the_selector_flags_name_both_verbs_that_take_them() {
        // ⚠ The demo source is exercised through the AXIS, not as a verb — a bare `seed-demo`
        // would be refused for its name and prove nothing about `--symbol`.
        for argv in [
            &["hist", "fetch", "--symbol", "S"][..],
            &["hist", "fetch", "--source", "demo", "--symbol", "S"][..],
            &["hist", "ls", "--symbol", "S"][..],
            &["hist", "coverage", "--symbol", "S"][..],
        ] {
            let err = parse_of(argv).unwrap_err();
            assert!(err.contains("`rm` or `repair`"), "{argv:?}: {err}");
        }
    }

    // ---- the read half: the grammar ----

    /// A per-symbol bar row and a grouped tick row — every rendering property below is visible in
    /// one pair: the four dimensions, the `symbol`/`group` alternative, and an absent interval.
    fn bar_row() -> SeriesRow {
        SeriesRow {
            kind: "bar".to_string(),
            venue: "binance".to_string(),
            name: "BTCUSDT".to_string(),
            grouped: false,
            symbol: "BTCUSDT".to_string(),
            group: None,
            interval: Some("1h".to_string()),
            coverage: Coverage {
                first_ts: 0,
                last_ts: 86_400_000,
                rows: 48,
                bytes: 900,
                parts: 2,
                dates: 2,
            },
            gaps: None,
            gaps_error: None,
            class: None,
        }
    }

    fn grouped_row() -> SeriesRow {
        SeriesRow {
            kind: "trade".to_string(),
            venue: "polymarket".to_string(),
            name: "fam".to_string(),
            grouped: true,
            // ⚠ EMPTY, exactly as the store reports it for a grouped series. The whole point of
            // the fixture: anything that renders `symbol` would render nothing here.
            symbol: String::new(),
            group: Some("fam".to_string()),
            interval: None,
            coverage: Coverage {
                first_ts: 172_800_000,
                last_ts: 259_200_000,
                rows: 7,
                bytes: 40,
                parts: 1,
                dates: 2,
            },
            gaps: None,
            gaps_error: None,
            class: None,
        }
    }

    #[test]
    fn the_read_subcommands_default_the_addr_and_take_the_filter_flags() {
        let a = parse_of(&["hist", "ls"]).unwrap();
        assert_eq!(a.addr, DEFAULT_ADDR);
        assert_eq!(a.filter, Filter::default());
        assert!(!a.gaps && !a.class && !a.partial_only && !a.json);

        let a = parse_of(&[
            "hist",
            "ls",
            "--addr",
            "1.2.3.4:9",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--name",
            "BTC",
            "--class",
            "--json",
        ])
        .unwrap();
        assert_eq!(a.addr, "1.2.3.4:9");
        assert_eq!(a.filter.kind.as_deref(), Some("bar"));
        assert_eq!(a.filter.venue.as_deref(), Some("binance"));
        assert_eq!(a.filter.name.as_deref(), Some("BTC"));
        assert!(a.class && a.json && !a.gaps, "`ls` never probes gaps");

        // ...and the same filters on `gaps`, where the probe is on because the VERB is.
        let a = parse_of(&["hist", "gaps", "--kind", "bar", "--venue", "binance", "--name", "BTC"])
            .unwrap();
        assert_eq!(a.filter.kind.as_deref(), Some("bar"));
        assert_eq!(a.filter.venue.as_deref(), Some("binance"));
        assert_eq!(a.filter.name.as_deref(), Some("BTC"));
        assert!(a.gaps && !a.class, "the verb arms the probe, and only the probe");

        let a = parse_of(&["hist", "coverage", "--venue", "binance", "--partial-only"]).unwrap();
        assert!(a.partial_only);
        assert_eq!(a.filter.venue.as_deref(), Some("binance"));
    }

    /// The bare booleans reject an inline value, on the same rung every valueless flag in this
    /// crate uses.
    #[test]
    fn the_read_booleans_take_no_value() {
        assert!(parse_of(&["hist", "ls", "--class=1"]).unwrap_err().contains("--class"));
        assert!(parse_of(&["hist", "ls", "--json=1"]).unwrap_err().contains("--json"));
        assert!(
            parse_of(&["hist", "coverage", "--partial-only=yes"])
                .unwrap_err()
                .contains("--partial-only")
        );
    }

    /// `--class` is accepted on `ls` ALONE, and every other subcommand refuses it BY NAME with
    /// the reason that subcommand has — never silently ignores it.
    ///
    /// ⚠ The three read siblings are the interesting rows and they do NOT share a reason, which is
    /// why each is asserted on its own words rather than on "is an error": `coverage` refuses it
    /// because its roster is the TICK kinds and a bar-only instrument is in none of them;
    /// `tape-health` because every finding it makes is folded from the one inventory it already
    /// fetched; `universe` because its cells are as-of a window the operator wrote down and this
    /// probe is as-of now.
    #[test]
    fn the_class_flag_belongs_to_list_alone_and_every_refusal_says_why() {
        assert!(parse_of(&["hist", "ls", "--class"]).unwrap().class);

        let err = |args: &[&str]| parse_of(args).unwrap_err();

        // ⚠ The SIBLING that is easiest to get wrong: `gaps` takes the same filters as `ls` and
        // deliberately not this annotation — every line it prints is about an ABSENCE.
        let gaps = err(&["hist", "gaps", "--class"]);
        assert!(gaps.contains("--class"), "{gaps}");
        assert!(gaps.contains("does NOT"), "the absence reason: {gaps}");
        assert!(gaps.contains("data hist ls --class"), "…and where to go: {gaps}");

        let coverage = err(&["hist", "coverage", "--class"]);
        assert!(coverage.contains("--class"), "{coverage}");
        assert!(coverage.contains("TICK kinds"), "the MEASURED reason: {coverage}");
        assert!(coverage.contains("data hist ls --class"), "…and where to go: {coverage}");

        let health = err(&["hist", "health", "--class"]);
        assert!(health.contains("inventory"), "{health}");

        let uni = err(&["hist", "universe", "--class"]);
        assert!(uni.contains("as of NOW") || uni.contains("as of the window"), "{uni}");

        for args in [
            &["hist", "fetch", "binance:BTCUSDT:1h", "--days", "2", "--class"][..],
            &["hist", "fetch", "--source", "demo", "--class"][..],
            &["hist", "fetch", "--source", "starter", "--class"][..],
            &["hist", "export", "demo:X:1h", "--out", "o.parquet", "--class"][..],
            &["hist", "rm", "--kind", "bar", "--venue", "demo", "--class"][..],
            &[
                "hist",
                "repair",
                "--kind",
                "bar",
                "--venue",
                "demo",
                "--symbol",
                "X",
                "--interval",
                "1h",
                "--class",
            ][..],
        ] {
            let e = err(args);
            assert!(e.contains("--class"), "{args:?} must refuse it by name: {e}");
        }
    }

    /// ⚠ **THE refusal this module exists to make loud.** `--store` on a read verb is the mistake
    /// an operator makes first, because the sibling subcommand takes one — and a silently-ignored
    /// `--store` would answer about a completely different store with no sign that it had. The
    /// message must name the flag, the verb, and the flag that reaches the other store.
    #[test]
    fn a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store() {
        // ⚠ EVERY read subcommand, not just the two that shipped first. A verb that inherited
        // `is_read()` without inheriting this refusal would accept `--store` and answer about a
        // completely different store, which is the exact failure this test is named for.
        for sub in SUBCOMMANDS.iter().filter(|s| s.is_read()) {
            for (flag, value) in [("--store", "/srv/hist"), ("--engine", "/opt/backtest")] {
                let argv = ["hist", sub.as_str(), flag, value];
                let err = parse_of(&argv).unwrap_err();
                assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
                assert!(err.contains("--addr"), "{argv:?} must point at --addr: {err}");
            }
        }
    }

    /// ⚠ **The WINDOW refusal moved out of the test above, and the move is the point.** `--days`
    /// and `--from`/`--to` used to ride the store-side refusal and so were asserted to name
    /// `--addr` — which was never apt for them: neither flag names a store, and pointing an
    /// operator at `--addr` answers a question they did not ask. They bound a FETCH, and the
    /// verbs that refuse them fold each series' WHOLE recorded span, so the useful thing to name
    /// is the read verb whose question IS a window. See [`Sub::refuses_a_window`].
    #[test]
    fn a_window_on_a_whole_span_read_verb_is_refused_and_names_the_verb_that_takes_one() {
        for sub in SUBCOMMANDS.iter().filter(|s| s.refuses_a_window()) {
            for (flag, value) in [("--days", "7"), ("--from", "0"), ("--to", "0")] {
                let argv = ["hist", sub.as_str(), flag, value];
                let err = parse_of(&argv).unwrap_err();
                assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
                assert!(err.contains("universe"), "{argv:?} must name the verb: {err}");
            }
        }
    }

    /// …and `universe` ACCEPTS the pair, parses both bounds HERE, and keeps each one independent.
    /// The parse is what makes it different from `export`'s forwarded strings — see
    /// [`membership_window`].
    #[test]
    fn universe_takes_an_independent_pair_of_bounds_and_parses_them_here() {
        let a = parse_of(&["hist", "universe"]).unwrap();
        assert_eq!(
            a.universe_window,
            Some(universe::MembershipWindow::default()),
            "a bare `universe` is unbounded, not an error — the store's own span is the window"
        );

        let a = parse_of(&["hist", "universe", "--from", "2026-01-01"]).unwrap();
        let window = a.universe_window.expect("universe always carries a window");
        assert_eq!(window.from, Some(1_767_225_600_000), "YYYY-MM-DD resolves to UTC midnight");
        assert_eq!(window.to, None, "one bound stands alone, as on `export`");

        let a = parse_of(&["hist", "universe", "--to", "0"]).unwrap();
        let window = a.universe_window.expect("universe always carries a window");
        assert_eq!(window.from, None);
        assert_eq!(window.to, Some(0), "a bare epoch-ms stays an epoch-ms, including zero");
    }

    /// An unreadable bound is a USAGE error here rather than a silently-discarded flag, because
    /// nothing is forwarded: the comparison happens in this process.
    #[test]
    fn an_unreadable_universe_bound_is_refused_by_name() {
        let err = parse_of(&["hist", "universe", "--from", "last-tuesday"]).unwrap_err();
        assert!(err.contains("--from"), "{err}");
        assert!(err.contains("last-tuesday"), "the message names what was typed: {err}");
        assert!(err.contains("YYYY-MM-DD"), "…and the spellings it wanted: {err}");
    }

    /// An INVERTED window is refused rather than swapped: it would report every instrument in the
    /// store `absent`, which reads exactly like an empty store.
    #[test]
    fn an_inverted_universe_window_is_refused_rather_than_swapped() {
        let err = parse_of(&["hist", "universe", "--from", "2026-06-01", "--to", "2026-01-01"])
            .unwrap_err();
        assert!(err.contains("--from") && err.contains("--to"), "{err}");
        assert!(err.contains("absent"), "…and what it would have produced: {err}");
    }

    /// `--days` is refused on `universe` for a reason of its OWN, and the message has to carry it:
    /// a window counted back from now answers a different question every day it is run, and a
    /// point-in-time universe exists to be re-askable.
    #[test]
    fn days_is_refused_on_universe_with_the_re_askability_reason() {
        let err = parse_of(&["hist", "universe", "--days", "30"]).unwrap_err();
        assert!(err.contains("--days"), "{err}");
        assert!(err.contains("--from"), "…and the spelling that works: {err}");
        assert!(err.contains("NOW"), "…and why: {err}");
    }

    /// The two new read verbs refuse the ABSENCE flag, and the message says which verb answers
    /// absence — `health`'s whole distinction is present-and-impossible versus missing.
    #[test]
    fn the_new_read_verbs_refuse_the_absence_flags_and_name_the_verbs_that_answer_absence() {
        for sub in ["health", "universe"] {
            let err = parse_of(&["hist", sub, "--partial-only"]).unwrap_err();
            assert!(err.contains("--partial-only"), "{sub}: {err}");
            assert!(err.contains("hist gaps"), "{sub} must name the verb that answers: {err}");
        }
    }

    /// **THE OUTPUT DOOR.** `--format` is the axis, `--json` is its shorthand, the two are refused
    /// only where they DISAGREE, and every value this plane names but this verb does not serve is
    /// refused BY NAME.
    ///
    /// ⚠ **[`UNBUILT_FORMATS`] IS EMPTY NOW, and the loop over it was therefore VACUOUS** — it
    /// asserted three things about every row of a roster with no rows, which passes whatever the
    /// refusals say. That is the shape this suite exists to catch, so the roster's emptiness is
    /// asserted as a CLAIM (with the reason a row would be added back) and each of the three
    /// departed values is exercised through the arm that actually refuses it:
    ///
    /// * `jsonl` — BUILT, on [`ROW_VERB`]. Refused here because a catalog is not rows.
    /// * `csv`/`parquet` — BUILT, on [`FILE_VERB`]. Refused here because this verb PRINTS.
    ///
    /// None of the three may read as "waiting on a phase", which is what each of them said once.
    #[test]
    fn the_format_axis_carries_json_and_refuses_the_other_verbs_formats_by_name() {
        assert!(!parse_of(&["hist", "ls"]).unwrap().json, "table is the default");
        assert!(parse_of(&["hist", "ls", "--format", "json"]).unwrap().json);
        assert!(!parse_of(&["hist", "ls", "--format", "table"]).unwrap().json);
        assert!(parse_of(&["hist", "ls", "--json"]).unwrap().json, "the shorthand still works");
        assert!(
            parse_of(&["hist", "ls", "--json", "--format", "json"]).unwrap().json,
            "agreeing spellings are not a contradiction"
        );

        // ...and the ONE way they can disagree is refused rather than resolved.
        let err = parse_of(&["hist", "ls", "--json", "--format", "table"]).unwrap_err();
        assert!(err.contains("--json") && err.contains("--format table"), "{err}");
        assert!(err.contains("pass one"), "…and what to do: {err}");

        // THE ROSTER'S EMPTINESS AS A CLAIM. Every value it once held is now written by some verb
        // of this plane, so there is nothing left that is "designed but not built". A row added
        // back is a value nothing writes, and it gets the arm that names what it is waiting on.
        assert!(
            UNBUILT_FORMATS.is_empty(),
            "every value this roster held is now WRITTEN by a verb; a row here must name what it \
             is waiting on: {UNBUILT_FORMATS:?}"
        );
        // ...but the loop over it is vacuous, so each departed value is exercised through its own
        // arm — and none of the three may read as waiting on a phase.
        for (name, needle) in [("jsonl", ROW_VERB), ("csv", FILE_VERB), ("parquet", FILE_VERB)] {
            let err = parse_of(&["hist", "ls", "--format", name]).unwrap_err();
            assert!(err.contains(name), "{name}: {err}");
            assert!(err.contains(needle), "{name} must name the verb that serves it: {err}");
            assert!(!err.contains("not built"), "{name} SHIPS: {err}");
        }
        // ⚠ THE ANTI-VACUITY CONTROL for all three at once: each is genuinely ACCEPTED somewhere,
        // so the refusals above are about THIS verb rather than about a value nothing serves.
        parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--format", "jsonl"])
            .expect("`get` is the verb that serves jsonl on stdout");
        parse_of(&[
            "hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "csv", "--from",
            "1", "--to", "2",
        ])
        .expect("`export --addr` is the verb that writes csv");
        parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--format", "parquet"])
            .expect("`export` is the verb that writes parquet");

        // An unrecognised one IS a spelling mistake, and says so — unlike `--source`, whose roster
        // lives in a process this crate cannot see.
        let err = parse_of(&["hist", "ls", "--format", "yaml"]).unwrap_err();
        assert!(err.contains("unknown") && err.contains("table | json"), "{err}");
        assert!(parse_of(&["hist", "ls", "--format", ""]).unwrap_err().contains("EMPTY"));

        // The axis is on EVERY verb, not just the read half — a `fetch` report is a document too.
        assert!(parse_of(&["hist", "fetch", "--source", "demo", "--format", "json"]).unwrap().json);
    }

    /// **THE PROMOTION.** `gaps` is a verb, `--gaps` is refused on EVERY verb by name, and the
    /// refusal names the verb that replaced it.
    ///
    /// ⚠ The `ls` row is the one that matters. A refusal that fired only on the siblings would
    /// leave the ONE verb that used to accept the flag answering `unknown option '--gaps'` — which
    /// tells an operator the flag never existed and sends them to check their spelling. Every verb
    /// is asserted rather than a sample, for [`RETIRED_SPELLINGS`]'s reason one layer up.
    #[test]
    fn gaps_is_a_verb_and_the_retired_flag_is_refused_everywhere_by_name() {
        let a = parse_of(&["hist", "gaps"]).unwrap();
        assert_eq!(a.sub, Sub::Gaps);
        assert!(a.gaps, "the verb IS the probe");
        assert_eq!(a.addr, DEFAULT_ADDR, "…and it is a READ verb, so it resolves a datahub");

        for sub in SUBCOMMANDS {
            // Each verb needs its own minimum argv before the flag can be judged, so the refusal
            // is reached with the line that verb would otherwise accept.
            let mut argv: Vec<&str> = match sub {
                Sub::Fetch => vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "1"],
                Sub::Export => vec!["hist", "export", "demo:X:1h", "--out", "o.parquet"],
                Sub::Rm => vec!["hist", "rm", "--kind", "bar", "--venue", "demo"],
                Sub::Repair => {
                    vec!["hist", "repair", "--kind", "bar", "--venue", "demo", "--symbol", "X"]
                }
                other => vec!["hist", other.as_str()],
            };
            argv.push("--gaps");
            let err = parse_of(&argv).unwrap_err();
            assert!(!err.contains("unknown option"), "{argv:?} must not read as a typo: {err}");
            assert!(err.contains("--gaps"), "{argv:?} must name the flag: {err}");
            assert!(
                err.contains("data hist gaps"),
                "{argv:?} must name the verb that replaced it: {err}"
            );
        }
    }

    /// `--kind` is ACCEPTED on `tape-health` and on `universe` while `coverage` refuses it, and the
    /// asymmetry is deliberate: a coverage row IS the join across kinds, while a universe narrowed
    /// to `--kind bar` is exactly the set a bar-driven profile reads.
    #[test]
    fn kind_narrows_the_new_read_verbs_while_coverage_still_refuses_it() {
        for sub in ["health", "universe"] {
            let a = parse_of(&["hist", sub, "--kind", "bar"]).unwrap();
            assert_eq!(a.filter.kind.as_deref(), Some("bar"), "{sub}");
        }
        assert!(parse_of(&["hist", "coverage", "--kind", "bar"]).is_err());
    }

    /// …and the mirror image: a datahub flag on the write half is refused rather than ignored,
    /// naming the half it belongs to. Ignoring one would let `data fetch --addr the CI box:7878` read
    /// as "fetch into the remote store", which is not a thing this verb can do.
    ///
    /// ⚠ **`export` used to be in this loop and has been split out, deliberately.** `--addr` and
    /// `--kind` are that verb's own now (the route switch and the row shape), so the blanket
    /// sentence would be a true no for a false reason. The LISTING aids still are foreign there and
    /// still refuse — with a sentence of their own, which is what the second half asserts.
    #[test]
    fn a_datahub_flag_on_a_write_verb_is_refused_and_names_the_read_half() {
        for (argv, flag) in [
            (vec!["hist", "fetch", "--source", "demo", "--class"], "--class"),
            (vec!["hist", "fetch", "--source", "demo", "--venue", "binance"], "--venue"),
            (vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7", "--name", "BTC"], "--name"),
            // ⚠ `fetch` KEEPS both of the rows `export` took back, and this is the control for
            // that: the split is per-verb, not a widening of the grammar.
            (vec!["hist", "fetch", "--source", "demo", "--addr", "h:1"], "--addr"),
            (vec!["hist", "fetch", "--source", "demo", "--kind", "bar"], "--kind"),
        ] {
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("READ half"), "{argv:?} must name the half it belongs to: {err}");
        }

        // `export` refuses the four LISTING aids too, with the sentence that is true of IT.
        for flag in ["--venue", "--name"] {
            let err = parse_of(&["hist", "export", "d:S:1h", "--out", "o", flag, "x"]).unwrap_err();
            assert!(err.contains(flag), "{flag}: {err}");
            assert!(err.contains("names ONE series"), "{flag}: {err}");
            // ANTI-VACUITY: it is a DIFFERENT sentence, which is the whole point of the split.
            assert!(!err.contains("READ half"), "{flag} must not inherit fetch's reason: {err}");
        }
    }

    /// The intra-half refusals, each about a UNIT rather than about tidiness — see the arms in
    /// [`parse`]. `--kind` on `coverage` would filter away the disagreement the report exists to
    /// show; `--partial-only` on a per-series listing has no cross-kind verdict to filter.
    #[test]
    fn each_read_verb_refuses_the_others_flag_with_the_reason() {
        for sub in ["ls", "gaps"] {
            let err = parse_of(&["hist", sub, "--partial-only"]).unwrap_err();
            assert!(err.contains("--partial-only") && err.contains("coverage"), "{sub}: {err}");
        }

        let err = parse_of(&["hist", "coverage", "--kind", "trade"]).unwrap_err();
        assert!(err.contains("--kind") && err.contains("across kinds"), "{err}");
    }

    /// A colon-string on a read verb is refused with the REASON: a stored series is four
    /// dimensions with an alternative inside them, and no `VENUE:SYMBOL:INTERVAL` can spell one.
    /// It is the single likeliest thing to type after using `fetch`.
    #[test]
    fn a_read_verb_refuses_a_fetch_shaped_spec_and_says_why() {
        let err = parse_of(&["hist", "ls", "binance:BTCUSDT:1h"]).unwrap_err();
        assert!(err.contains("binance:BTCUSDT:1h"), "the message names what was typed: {err}");
        assert!(err.contains("--kind"), "…and the flags that do narrow a listing: {err}");
    }

    // ---- the filter ----

    /// Case-insensitive SUBSTRING, ANDed, and an absent dimension matches everything. The grouped
    /// case is the load-bearing one: `--name` matches a group, which is why it is not `--symbol`.
    #[test]
    fn the_filter_is_case_insensitive_substring_and_reaches_a_group_by_name() {
        let f = |args: &[&str]| parse_of(args).unwrap().filter;

        let all = Filter::default();
        assert!(all.matches(Some("bar"), "binance", "BTCUSDT"));
        assert!(all.is_empty());

        let by_name = f(&["hist", "ls", "--name", "btc"]);
        assert!(by_name.matches(Some("bar"), "binance", "BTCUSDT"), "case-insensitive substring");
        assert!(!by_name.matches(Some("bar"), "binance", "ETHUSDT"));
        assert!(!by_name.is_empty());

        // A GROUPED series' label is its group; `--name fam` must reach it.
        let grouped = f(&["hist", "ls", "--name", "FAM"]);
        assert!(grouped.matches(Some("trade"), "polymarket", "fam"));

        // ANDed: every named dimension has to agree.
        let both = f(&["hist", "ls", "--kind", "bar", "--venue", "okx"]);
        assert!(both.matches(Some("bar"), "okx", "BTC-USDT"));
        assert!(!both.matches(Some("trade"), "okx", "BTC-USDT"));
        assert!(!both.matches(Some("bar"), "binance", "BTCUSDT"));

        // A row with NO kind dimension (the `coverage` caller) passes the kind test — and `--kind`
        // cannot reach that path at all, because `parse` refuses it there.
        assert!(f(&["hist", "ls", "--kind", "bar"]).matches(None, "binance", "BTCUSDT"));
    }

    // ---- the `list` rendering ----

    /// ⚠ The identity is rendered as COLUMNS, never as a colon-string: `kind` and a `SCOPE` cell
    /// are their own cells, so a grouped series reads as a group rather than as a venue with two
    /// empties after it.
    #[test]
    fn list_lines_render_four_dimensions_and_never_a_colon_string() {
        let rows = [bar_row(), grouped_row()];
        let lines = list_lines(&rows, 2, false, false, false);

        let cells =
            |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_string).collect() };
        assert_eq!(cells(&lines[0])[..5], ["KIND", "VENUE", "SCOPE", "NAME", "INTERVAL"]);
        assert_eq!(cells(&lines[1])[..7], ["bar", "binance", "symbol", "BTCUSDT", "1h", "48", "2"]);
        // The grouped row: its NAME comes from the group, and its interval is genuinely absent
        // rather than an empty cell — the two things a colon-string identity gets wrong.
        assert_eq!(cells(&lines[2])[..7], ["trade", "polymarket", "group", "fam", "-", "7", "2"]);
        // …and the columns actually line up, which is what makes the table skimmable at all.
        let scope_col = lines[0].find("SCOPE").expect("the header names the column");
        assert_eq!(lines[1].find("symbol"), Some(scope_col), "{}", lines[1]);
        assert_eq!(lines[2].find("group"), Some(scope_col), "{}", lines[2]);
        for line in &lines {
            assert!(!line.contains("binance:BTCUSDT"), "no colon-string identity: {line}");
        }
        assert_eq!(lines.last().unwrap(), "2 series · 55 rows");
    }

    /// A filtered listing says how many the SERVER reported beside how many survived — otherwise
    /// "3 series" is unreadable without knowing whether it was 3 of 3 or 3 of 3000.
    #[test]
    fn a_filtered_listing_counts_both_sides() {
        let lines = list_lines(&[bar_row()], 9, true, false, false);
        assert_eq!(lines.last().unwrap(), "1 of 9 series · 48 rows");
    }

    /// A zero-row series renders `-` for its span rather than a well-formed `1970-01-01` folded
    /// from an all-zero coverage — while [`list_json`] keeps the store's own numbers untouched.
    #[test]
    fn a_zero_row_series_shows_a_dash_span_and_the_document_still_carries_the_zeroes() {
        let mut row = bar_row();
        row.coverage = Coverage::default();
        let lines = list_lines(std::slice::from_ref(&row), 1, false, false, false);
        assert!(!lines[1].contains("1970"), "a sentinel span is not a date: {}", lines[1]);
        let cells: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(&cells[cells.len() - 2..], ["-", "-"], "both span cells: {}", lines[1]);

        let args = parse_of(&["hist", "ls", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, std::slice::from_ref(&row), 1)).unwrap();
        assert_eq!(doc["series"][0]["coverage"]["first_ts"], 0, "the document is not edited");
        assert_eq!(doc["series"][0]["coverage"]["rows"], 0);
    }

    /// All THREE gap outcomes are said out loud. An unasked question, a clean series and a probe
    /// that failed must not share a rendering — the middle one is what an operator ran `gaps`
    /// to learn.
    #[test]
    fn gap_lines_distinguish_holes_from_none_from_unanswerable() {
        let mut row = bar_row();
        assert!(gap_lines(&row).is_empty(), "an `ls` row carries no annotation at all");

        row.gaps = Some(Vec::new());
        assert_eq!(gap_lines(&row), vec!["      no gaps".to_string()]);

        row.gaps = Some(vec![(86_400_000, 172_800_000)]);
        assert_eq!(gap_lines(&row), vec!["      gap 1970-01-02 .. 1970-01-03".to_string()]);

        row.gaps = None;
        row.gaps_error = Some("manifest unreadable".to_string());
        let lines = gap_lines(&row);
        assert!(lines[0].contains("gaps unavailable"), "{lines:?}");
        assert!(lines[0].contains("manifest unreadable"), "the server's own words: {lines:?}");
    }

    /// The `--gaps` degrade, seen through the renderer that carries it: the LISTING still renders
    /// in full and the failure lands on the row it belongs to.
    #[test]
    fn a_failed_gap_probe_annotates_its_row_and_leaves_the_listing_whole() {
        let mut broken = bar_row();
        broken.gaps_error = Some("series_gaps: manifest unreadable".to_string());
        let mut clean = grouped_row();
        clean.gaps = Some(Vec::new());

        let lines = list_lines(&[broken, clean], 2, false, true, false);
        assert!(lines.iter().any(|l| l.contains("gaps unavailable")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("no gaps")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row itself survives: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("fam")), "{lines:?}");
    }

    // ---- `--class`: the reader `SymbolProperties::asset_class` did not have ----

    /// Every one of the five probe outcomes renders as its own WORD, and none of them as a blank.
    ///
    /// ⚠ **What would make this fail, and why it matters more than it looks:** folding
    /// `unclassified` and `no-properties` into one cell. They are the two halves of "there is no
    /// class here" and they send an operator to different places — the venue's producer, or the
    /// recorder that never ran for this instrument. `vike_model::SymbolProperties::asset_class`
    /// keeps them apart in the store by being an `Option` at all; a renderer that collapsed them
    /// would undo that on the last hop.
    #[test]
    fn every_class_outcome_has_its_own_word_and_none_of_them_is_blank() {
        let cell = |probe: Option<ClassProbe>| {
            let mut row = bar_row();
            row.class = probe;
            class_cell(&row)
        };

        assert_eq!(cell(Some(ClassProbe::Classified("CryptoPerp"))), "CryptoPerp");
        assert_eq!(cell(Some(ClassProbe::Unclassified)), "unclassified");
        assert_eq!(cell(Some(ClassProbe::Unrecorded)), "no-properties");
        assert_eq!(cell(Some(ClassProbe::Grouped)), "(group)");
        assert_eq!(cell(Some(ClassProbe::Failed("boom".into()))), "(error)");
        assert_eq!(cell(None), "-", "the unreachable arm is still a cell, never a panic");

        // The two that mean "no class" are DIFFERENT words — the assertion the fold above would
        // break, stated on its own so the failure names the defect rather than a string.
        assert_ne!(
            cell(Some(ClassProbe::Unclassified)),
            cell(Some(ClassProbe::Unrecorded)),
            "a producer that named no class and a recorder that never ran are different findings"
        );
    }

    /// The class word is the MODEL's own spelling, taken from `vike_model::AssetClass::sql_word`
    /// (which is also its serde word) — never a lowercase or prettified second form minted here.
    /// A third spelling of one vocabulary is the exact defect `crates/vike-model/src/asset_class.rs`
    /// opens its module doc with.
    #[test]
    fn the_rendered_class_word_is_the_models_own_spelling() {
        for class in vike_model::AssetClass::ALL {
            let mut row = bar_row();
            row.class = Some(ClassProbe::Classified(class.sql_word()));
            assert_eq!(class_cell(&row), class.sql_word());
        }
    }

    /// The CLASS column appears ONLY under `--class`, and without it every line is byte-identical
    /// to the listing this verb rendered before the flag existed.
    ///
    /// ⚠ That equality is the point of the test, not tidiness: `list_lines` grew a width that is
    /// zero in the unasked case, and a regression there would silently add trailing whitespace to
    /// every row of every `data hist list` anybody has ever piped into a diff.
    #[test]
    fn the_class_column_is_absent_unless_asked_and_changes_nothing_when_it_is() {
        let mut classified = bar_row();
        classified.class = Some(ClassProbe::Classified("CryptoPerp"));
        let mut grouped = grouped_row();
        grouped.class = Some(ClassProbe::Grouped);
        let rows = [classified, grouped];

        let without = list_lines(&rows, 2, false, false, false);
        assert!(!without[0].contains("CLASS"), "no header without the flag: {}", without[0]);
        assert!(!without[1].contains("CryptoPerp"), "…and no cell either: {}", without[1]);
        // The rows carry a probe, so this proves the RENDERER is gated and not merely the caller.
        assert_eq!(
            without,
            list_lines(&[bar_row(), grouped_row()], 2, false, false, false),
            "an unasked class may not change one byte of the listing"
        );

        let with = list_lines(&rows, 2, false, false, true);
        assert!(with[0].trim_end().ends_with("CLASS"), "the column is last: {}", with[0]);
        assert!(with[1].ends_with("CryptoPerp"), "{}", with[1]);
        assert!(with[2].ends_with("(group)"), "a grouped row was never asked: {}", with[2]);
        // The cells sit UNDER the header — the whole reason `last_w` is measured rather than
        // assumed, since a `LAST` column of varying width would otherwise ragged the one after it.
        let class_col = with[0].find("CLASS").expect("the header names the column");
        assert!(with[1][class_col..].starts_with("CryptoPerp"), "aligned: {}", with[1]);
        assert!(with[2][class_col..].starts_with("(group)"), "…both rows: {}", with[2]);
    }

    /// A failed probe degrades the ROW and leaves the listing whole — the same contract the gap
    /// probe has, and the reason both are annotations rather than run failures.
    #[test]
    fn a_failed_class_probe_annotates_its_row_and_leaves_the_listing_whole() {
        let mut broken = bar_row();
        broken.class = Some(ClassProbe::Failed("properties_as_of: store unreadable".to_string()));
        let mut fine = grouped_row();
        fine.class = Some(ClassProbe::Unclassified);

        let lines = list_lines(&[broken, fine], 2, false, false, true);
        assert!(lines.iter().any(|l| l.contains("class unavailable")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("store unreadable")),
            "the server's words: {lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row survives: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("unclassified")), "…and so does its sibling");

        // Only the FAILURE gets a line — every other verdict is already a word in the row, which is
        // the asymmetry with `gap_lines` and the reason it is written down there.
        for probe in [ClassProbe::Classified("Fx"), ClassProbe::Unclassified, ClassProbe::Grouped] {
            let mut row = bar_row();
            row.class = Some(probe);
            assert!(class_error_line(&row).is_empty(), "{:?} needs no line", row.class);
        }
    }

    /// The `--json` wire: `asset_class_status` names WHICH answer each row got, and `asset_class`
    /// is non-null for EXACTLY the `classified` verdict.
    ///
    /// ⚠ This is the compatibility assertion the module doc argues for. Without the status field a
    /// `null` class would mean four different things at once — not asked, no grid, a grid naming no
    /// class, and a failed probe — and the last three are what an operator acts on differently.
    #[test]
    fn the_class_fields_are_a_status_and_a_word_that_cannot_disagree() {
        let args = parse_of(&["hist", "ls", "--class", "--json"]).unwrap();
        let mut rows = Vec::new();
        for probe in [
            ClassProbe::Classified("CryptoPerp"),
            ClassProbe::Unclassified,
            ClassProbe::Unrecorded,
            ClassProbe::Grouped,
            ClassProbe::Failed("store unreadable".to_string()),
        ] {
            let mut row = bar_row();
            row.class = Some(probe);
            rows.push(row);
        }
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, &rows, rows.len())).unwrap();

        assert_eq!(doc["class_requested"], true);
        let series = doc["series"].as_array().expect("an array of rows");
        let statuses: Vec<&str> =
            series.iter().map(|s| s["asset_class_status"].as_str().unwrap()).collect();
        assert_eq!(
            statuses,
            ["classified", "unclassified", "unrecorded", "grouped", "error"],
            "one status per outcome, and all five distinct"
        );
        for row in series {
            let classified = row["asset_class_status"] == "classified";
            assert_eq!(
                !row["asset_class"].is_null(),
                classified,
                "asset_class is non-null for exactly the classified verdict: {row}"
            );
            assert_eq!(
                !row["asset_class_error"].is_null(),
                row["asset_class_status"] == "error",
                "…and asset_class_error for exactly the error one: {row}"
            );
        }
        assert_eq!(series[0]["asset_class"], "CryptoPerp");
        assert_eq!(series[4]["asset_class_error"], "store unreadable");
    }

    /// WITHOUT `--class` the three keys are present and null on every row and `class_requested` is
    /// false — the shape `gaps`/`gaps_requested` already has, so the addition is ADDITIVE for a
    /// consumer that predates it and needs no new convention from one that does not.
    #[test]
    fn an_unasked_class_is_null_on_the_wire_and_says_it_was_not_asked() {
        let args = parse_of(&["hist", "ls", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
        assert_eq!(doc["class_requested"], false);
        let row = &doc["series"][0];
        for key in ["asset_class", "asset_class_status", "asset_class_error"] {
            assert!(row.get(key).is_some(), "{key} is a KEY, so a consumer can read it uniformly");
            assert!(row[key].is_null(), "…and null, because nothing was asked: {row}");
        }
    }

    /// The document carries the RAW `symbol` and `group` beside the resolved `name`/`grouped`, so
    /// a machine reader gets the identity rather than this side's reading of it — and the gap
    /// ranges are objects rather than positional pairs.
    #[test]
    fn list_json_carries_the_raw_identity_beside_the_resolved_name() {
        let mut per_symbol = bar_row();
        per_symbol.gaps = Some(Vec::new());
        let mut grouped = grouped_row();
        grouped.gaps = Some(vec![(0, 86_400_000)]);
        // ⚠ `gaps`, not `ls --gaps`: the probe is DERIVED from the verb now, so this is also the
        // assertion that `gaps_requested` still follows it.
        let args = parse_of(&["hist", "gaps", "--venue", "poly", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, &[per_symbol, grouped], 5)).unwrap();

        assert_eq!(doc["subcommand"], "gaps");
        assert_eq!(doc["addr"], DEFAULT_ADDR);
        assert_eq!(doc["filter"]["venue"], "poly");
        assert!(doc["filter"]["kind"].is_null(), "an unset filter dimension is null, not absent");
        assert_eq!(doc["gaps_requested"], true);
        assert_eq!(doc["series_reported"], 5);
        assert_eq!(doc["count"], 2);

        let per_symbol = &doc["series"][0];
        assert_eq!(per_symbol["kind"], "bar");
        assert_eq!(per_symbol["name"], "BTCUSDT");
        assert_eq!(per_symbol["symbol"], "BTCUSDT");
        assert!(per_symbol["group"].is_null());
        assert_eq!(per_symbol["grouped"], false);
        assert_eq!(per_symbol["interval"], "1h");
        assert_eq!(per_symbol["coverage"]["rows"], 48);
        assert!(per_symbol["gaps"].as_array().expect("the verb asked for them").is_empty());

        // ⚠ The grouped row is where a `VENUE:SYMBOL:INTERVAL` document would fall apart: its
        // symbol is EMPTY and its name comes from the group.
        let grouped = &doc["series"][1];
        assert_eq!(grouped["name"], "fam");
        assert_eq!(grouped["symbol"], "");
        assert_eq!(grouped["group"], "fam");
        assert_eq!(grouped["grouped"], true);
        assert!(grouped["interval"].is_null());
        assert_eq!(grouped["gaps"][0]["from_ts"], 0);
        assert_eq!(grouped["gaps"][0]["to_ts"], 86_400_000);
    }

    /// A series whose gaps were NOT asked for carries `null`, never an empty array — "nobody
    /// asked" and "this series has no holes" are different facts and a fold over them differs.
    #[test]
    fn an_unasked_gap_field_is_null_rather_than_an_empty_array() {
        let args = parse_of(&["hist", "ls", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
        assert!(doc["series"][0]["gaps"].is_null());
        assert!(doc["series"][0]["gaps_error"].is_null());
        assert_eq!(doc["gaps_requested"], false);
    }

    /// The EMPTY answer's two causes are told apart, in both verbs' nouns.
    #[test]
    fn the_empty_answer_separates_an_empty_store_from_an_over_narrow_filter() {
        assert_eq!(
            list_lines(&[], 0, false, false, false),
            vec!["the datahub reported no series at all"]
        );
        assert_eq!(
            list_lines(&[], 12, true, false, false),
            vec!["no series match the filter (12 reported)"]
        );
        assert_eq!(
            coverage_lines(&[], 0, false),
            vec!["the datahub reported no instruments at all"]
        );
        assert_eq!(
            coverage_lines(&[], 4, true),
            vec!["no instruments match the filter (4 reported)"]
        );
    }

    // ---- the `coverage` rendering ----

    /// One instrument with a real disagreement (`trade` on every day, `depth` missing one) and one
    /// that is complete — the two dispositions the report separates.
    fn coverage_rows() -> Vec<InstrumentRow> {
        vec![
            InstrumentRow {
                venue: "binance".to_string(),
                name: "BTCUSDT".to_string(),
                grouped: false,
                kinds: vec![
                    KindRow { kind: "trade".to_string(), days: 3 },
                    KindRow { kind: "depth".to_string(), days: 2 },
                ],
                spanned_days: 3,
                partial: vec![PartialRow {
                    day: 2,
                    start_ms: 172_800_000,
                    missing: vec!["depth".to_string()],
                }],
            },
            InstrumentRow {
                venue: "polymarket".to_string(),
                name: "fam".to_string(),
                grouped: true,
                kinds: vec![KindRow { kind: "trade".to_string(), days: 2 }],
                spanned_days: 2,
                partial: Vec::new(),
            },
        ]
    }

    #[test]
    fn coverage_lines_line_the_kinds_up_and_detail_only_the_partial_days() {
        let lines = coverage_lines(&coverage_rows(), 2, false);
        assert!(lines[0].contains("INSTRUMENT") && lines[0].contains("PARTIAL"), "{}", lines[0]);
        assert!(lines[1].contains("trade:3, depth:2"), "{}", lines[1]);
        assert!(lines[1].ends_with("  1"), "the partial COUNT is on the row: {}", lines[1]);
        assert_eq!(lines[2], "      1970-01-03  missing: depth");
        // The complete instrument gets its row and NO detail lines — a "nothing missing" line
        // under every healthy instrument is how a report teaches an operator to skim past it.
        assert!(lines[3].contains("fam") && lines[3].ends_with("  -"), "{}", lines[3]);
        assert_eq!(lines.last().unwrap(), "2 instruments · 1 with partial days");
    }

    /// The human table CAPS the day detail and says how many it withheld; the document does not
    /// cap at all (see [`MAX_PARTIAL_DAYS_SHOWN`]).
    #[test]
    fn the_human_table_caps_the_partial_days_while_the_document_carries_them_all() {
        let mut rows = coverage_rows();
        rows[0].partial = (0..MAX_PARTIAL_DAYS_SHOWN as i64 + 2)
            .map(|d| PartialRow {
                day: d,
                start_ms: d * 86_400_000,
                missing: vec!["depth".to_string()],
            })
            .collect();

        let lines = coverage_lines(&rows, 2, false);
        let detail = lines.iter().filter(|l| l.contains("missing:")).count();
        assert_eq!(detail, MAX_PARTIAL_DAYS_SHOWN, "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("… and 2 more partial days")), "{lines:?}");

        let args = parse_of(&["hist", "coverage", "--json"]).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&coverage_json(&args, &rows, 2)).unwrap();
        assert_eq!(
            doc["instruments"][0]["partial_days"].as_array().unwrap().len(),
            MAX_PARTIAL_DAYS_SHOWN + 2,
            "a truncated array would be a wrong answer, not a long one"
        );
    }

    /// The document's `complete` flag is computed from the same list it ships, and a partial day
    /// carries its day INDEX, its epoch-ms midnight and the rendered date — the index is what the
    /// store indexes by, the ms is what everything else derives from.
    #[test]
    fn coverage_json_carries_the_verdict_and_both_spellings_of_a_day() {
        let args = parse_of(&["hist", "coverage", "--partial-only", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&coverage_json(&args, &coverage_rows(), 7)).unwrap();

        assert_eq!(doc["subcommand"], "coverage");
        assert_eq!(doc["partial_only"], true);
        assert_eq!(doc["instruments_reported"], 7);
        assert_eq!(doc["count"], 2);

        let partial = &doc["instruments"][0];
        assert_eq!(partial["complete"], false);
        assert_eq!(partial["venue"], "binance");
        assert_eq!(partial["spanned_days"], 3);
        assert_eq!(partial["kinds"][0], serde_json::json!({ "kind": "trade", "days": 3 }));
        assert_eq!(partial["partial_days"][0]["day"], 2);
        assert_eq!(partial["partial_days"][0]["start_ms"], 172_800_000);
        assert_eq!(partial["partial_days"][0]["date"], "1970-01-03");
        assert_eq!(partial["partial_days"][0]["missing_kinds"][0], "depth");

        let complete = &doc["instruments"][1];
        assert_eq!(complete["complete"], true);
        assert_eq!(complete["grouped"], true, "a grouped instrument says so");
        assert!(complete["partial_days"].as_array().unwrap().is_empty());
    }

    /// `gate`'s whole line, resolved: the spec becomes a SELECTOR, the duration becomes
    /// milliseconds, and the kind roster is never empty. Everything downstream is then a pure fold
    /// with no parse left in it — see [`GateArgs`].
    #[test]
    fn a_gate_line_resolves_its_spec_its_duration_and_its_kind_roster() {
        let a = parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "365",
            "--max-gap",
            "4h",
        ])
        .expect("a well-formed gate");
        let g = a.gate.as_ref().expect("a GateArgs");
        assert_eq!(g.spec.text(), "binance:BTCUSDT:1h");
        assert_eq!(g.require_days, 365);
        assert_eq!(g.max_gap_ms, Some(4 * 3_600_000));
        assert_eq!(g.kinds, vec!["bar".to_string()], "defaulted, never empty");
        assert_eq!(a.addr, DEFAULT_ADDR, "…and it is a READ verb, so the addr is resolved");
        assert!(a.spec.is_none(), "the spec is MOVED into GateArgs, never left in both");

        // Repeatable, deduped, and the default is replaced rather than added to.
        let a = parse_of(&[
            "hist",
            "gate",
            "polymarket:@election-2026",
            "--require-days",
            "7",
            "--require-kind",
            "book",
            "--require-kind",
            "trade",
            "--require-kind",
            "book",
        ])
        .expect("a grouped gate");
        let g = a.gate.as_ref().expect("a GateArgs");
        assert_eq!(g.kinds, vec!["book".to_string(), "trade".to_string()]);
        assert_eq!(g.max_gap_ms, None, "the holes were not asked about");
        assert!(g.spec.grouped, "the @ reached the selector: {:?}", g.spec);
    }

    /// The two refusals that keep a gate from asserting nothing: no spec, and no criterion. Each
    /// names what to type instead, so the refusal is never a dead end.
    ///
    /// ⚠ **The criterion refusal used to advertise `--require-days 1` as "the PRESENCE-only
    /// spelling", and that label was FALSE.** `judge_days` compares `span_ms >= 86_400_000`, so a
    /// series that exists and is six hours old breaches it — an operator who wrote the advertised
    /// line into an `ExecStartPre=` had a unit refusing to start over exactly the tape it had just
    /// fetched. There is no presence-only spelling, and the message says so rather than implying
    /// one.
    #[test]
    fn a_gate_with_no_subject_or_no_criterion_is_refused_at_the_door() {
        let e = parse_of(&["hist", "gate", "--require-days", "30"]).expect_err("no spec");
        assert!(e.contains("gate needs a spec"), "{e}");
        assert!(e.contains("VENUE:@GROUP"), "…and names the grouped spelling too: {e}");

        let e = parse_of(&["hist", "gate", "binance:BTCUSDT:1h"]).expect_err("no criterion");
        assert!(e.contains("--require-days"), "{e}");
        assert!(e.contains("checked nothing"), "{e}");
        assert!(e.contains("no PRESENCE-only spelling"), "the label that was false is gone: {e}");
        assert!(e.contains("`--require-days 1`"), "…and the narrowest gate is still named: {e}");
        assert!(e.contains("WHOLE DAY"), "…with what it actually asserts: {e}");
    }

    /// `--require-kind` naming ACCOUNT data is refused in the SAME words `ls --kind` is refused in
    /// — one rule, one sentence. The control is the second half: the market funding RATE is not an
    /// account kind and still parses.
    ///
    /// ⚠ **The control USED TO BE UNABLE TO FAIL.** It spelled `binance:BTCUSDT:funding` and
    /// `--require-kind bar` — putting the word in the spec's INTERVAL slot, which this crate
    /// deliberately does not validate, and passing the flag the DEFAULT value that the first half
    /// of this file already proves parses. Flip `vike_model::store_plane::plane_of` so `funding`
    /// classifies as `StorePlane::Account` and that spelling stays green while the boundary it
    /// claims to guard has moved. The word now reaches `--require-kind` itself, which is the only
    /// place the refusal reads.
    #[test]
    fn a_required_kind_naming_account_data_is_refused_in_the_readers_own_words() {
        let e = parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "1",
            "--require-kind",
            "exec_fill",
        ])
        .expect_err("account data");
        assert!(e.contains("ACCOUNT data"), "{e}");
        assert!(e.contains("vike-cli account"), "…and the plane that will serve it: {e}");
        assert_eq!(
            e,
            refuse_an_account_kind_on_a_read("exec_fill").expect_err("the shared sentence"),
            "the two flags must meet ONE sentence"
        );

        // The CONTROL, through the same flag the refusal reads. `funding` is the MARKET rate and
        // must pass `refuse_an_account_kind_on_a_read`; the spec carries NO interval, because the
        // market rate lives in the `bar` kind under `interval=funding` and a third part here would
        // meet `refuse_a_kind_the_spec_can_never_select` for a different reason entirely.
        assert!(
            refuse_an_account_kind_on_a_read("funding").is_ok(),
            "the MARKET funding rate is not account data — `docs/decisions/0080` separated the \
             two names so this can be asserted at all"
        );
        let ok = parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "funding",
        ]);
        let g = ok.as_ref().expect("the market funding RATE parses as a criterion").gate.as_ref();
        assert_eq!(
            g.expect("a GateArgs").kinds,
            vec!["funding".to_string()],
            "…and it reaches the criterion roster rather than being swallowed: {ok:?}"
        );
    }

    /// A `--require-kind` the SPEC can never select is refused HERE, before a socket opens — the
    /// wiring of [`gate::refuse_a_kind_the_spec_can_never_select`], whose own unit tests carry the
    /// argument and the anti-vacuity control.
    ///
    /// ⚠ The order matters and this is where it is asserted: the refusal runs AFTER the default
    /// kind is folded in, so a bare `gate binance:BTCUSDT:1h` — whose roster is `[bar]` — is not
    /// refused by the check meant for the kind an operator NAMED.
    #[test]
    fn a_required_kind_the_spec_can_never_select_is_refused_before_a_socket_opens() {
        let e = parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "1",
            "--require-kind",
            "bar",
            "--require-kind",
            "trade",
        ])
        .expect_err("an interval-bearing spec cannot select a trade series");
        assert!(e.contains("--require-kind trade"), "{e}");
        assert!(e.contains("`binance:BTCUSDT` gates"), "…and the spec that would work: {e}");

        // The two controls: the default roster under the same spec, and the same roster under a
        // spec that named no step.
        assert!(
            parse_of(&["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"]).is_ok(),
            "the DEFAULT roster is the one kind a third part can select"
        );
        assert!(
            parse_of(&[
                "hist",
                "gate",
                "binance:BTCUSDT",
                "--require-days",
                "1",
                "--require-kind",
                "trade",
            ])
            .is_ok(),
            "…and with no step named, a tick kind is exactly what the spec reaches"
        );
    }

    /// An EMPTY `--require-kind` is refused rather than collapsing into the default. It is
    /// reachable from a script (`--require-kind="$K"` with `K` unset), and silently gating `bar`
    /// where the operator meant a variable's value is a green over the wrong tape.
    #[test]
    fn a_blank_required_kind_is_refused_rather_than_defaulted() {
        let e = parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "1",
            "--require-kind",
            "",
        ])
        .expect_err("a blank kind");
        assert!(e.contains("EMPTY value"), "{e}");
        assert!(e.contains("omit the flag"), "…and what to do instead: {e}");
    }

    /// Every flag that does not apply to `gate` is refused BY NAME, and `--kind` — the one an
    /// operator reaches for first — names the criterion flag that replaced it.
    #[test]
    fn gate_refuses_the_listing_flags_and_names_what_replaced_the_kind_filter() {
        let line = |extra: &[&str]| {
            let mut v = vec!["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"];
            v.extend_from_slice(extra);
            parse_of(&v)
        };
        assert!(line(&[]).is_ok(), "the negative control: the bare line parses");
        for (extra, needle) in [
            (vec!["--kind", "bar"], "--require-kind"),
            (vec!["--venue", "binance"], "EXACTLY"),
            (vec!["--name", "BTC"], "EXACTLY"),
            (vec!["--class"], "data hist ls --class"),
            (vec!["--partial-only"], "verdict"),
            (vec!["--store", "/srv/hist"], "--addr"),
            (vec!["--engine", "/bin/backtest"], "--addr"),
            (vec!["--days", "30"], "bounds a FETCH window"),
            (vec!["--from", "0"], "bounds a FETCH window"),
            (vec!["--out", "x.parquet"], "only `export` writes one"),
            (vec!["--symbol", "S"], "belongs to `rm` or `repair`"),
            (vec!["--produced-by", "p:"], "only `rm` deletes"),
            (vec!["--source", "demo"], "`fetch`'s axis"),
            (vec!["--gaps"], "data hist gaps"),
        ] {
            let e = line(&extra).expect_err(&format!("{extra:?} must be refused"));
            assert!(e.contains(needle), "{extra:?} must say {needle:?}: {e}");
        }
    }

    /// ...and the mirror: every CRITERION flag is refused by name on the verbs that judge nothing,
    /// with the verb that does judge in the message.
    #[test]
    fn a_criterion_flag_on_a_rendering_verb_is_refused_and_names_the_gate() {
        for sub in ["ls", "gaps", "coverage", "health", "universe", "fetch", "export"] {
            for extra in [
                vec!["--require-days", "30"],
                vec!["--max-gap", "1d"],
                vec!["--require-kind", "bar"],
            ] {
                let mut v = vec!["hist", sub];
                v.extend_from_slice(&extra);
                let e = parse_of(&v).expect_err(&format!("{sub} {extra:?}"));
                assert!(e.contains(extra[0]), "{sub} must name the flag: {e}");
                assert!(e.contains("data hist gate"), "{sub} must name the gate: {e}");
            }
        }
    }

    /// A malformed `--max-gap` is refused HERE, before a socket is opened, because nothing is
    /// forwarded: this flag is consumed in this process, so the far side would never see it.
    #[test]
    fn a_max_gap_that_is_not_fixed_time_is_refused_before_anything_is_dialled() {
        let line = |v: &str| {
            parse_of(&["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1", "--max-gap", v])
        };
        assert!(line("1d").is_ok(), "the negative control");
        assert!(line("500bars").expect_err("bars").contains("BAR COUNT"));
        assert!(line("3mo").expect_err("months").contains("CALENDAR"));
        assert!(line("").expect_err("blank").contains("--max-gap"));
    }

    // ── §8.2: `get`, the ROW verb ───────────────────────────────────────────────────────────

    /// **§8.2 RULE 1 REACHES THE GRAMMAR.** A `get` with no window is refused BY NAME, and the
    /// three flag spellings that ARE a window all parse.
    ///
    /// ⚠ The refusal lives in [`get::parse_window`] and is unit-tested there; what this case adds
    /// is that the flags REACH it — `--days`/`--from`/`--to` survive the read half's
    /// [`Sub::refuses_a_window`] refusal, which every other read verb but `universe` trips.
    #[test]
    fn get_requires_a_window_and_the_window_flags_reach_it() {
        let err = parse_of(&["hist", "get", "binance:BTCUSDT:1h"]).expect_err("no window");
        assert!(err.contains("get needs a window"), "{err}");
        for window in
            [vec!["--days", "7"], vec!["--from", "2026-01-01"], vec!["--to", "2026-02-01"]]
        {
            let mut v = vec!["hist", "get", "binance:BTCUSDT:1h"];
            v.extend_from_slice(&window);
            let a = parse_of(&v).unwrap_or_else(|e| panic!("{window:?}: {e}"));
            assert_eq!(a.sub, Sub::Get);
            assert!(a.get.is_some(), "{window:?} must build a GetArgs");
            assert!(a.spec.is_none(), "the spec is MOVED into GetArgs, never left in both");
        }
    }

    /// The RESOLVED defaults `GetArgs` promises: the ceiling folded in, the fold recorded, and the
    /// rendering decided — so nothing downstream re-decides one.
    #[test]
    fn get_args_arrive_resolved_with_the_ceiling_folded_in() {
        let a = parse_of(&["hist", "get", "binance:BTCUSDT:1h", "--days", "7"]).expect("a line");
        let g = a.get.as_ref().expect("a GetArgs");
        assert_eq!(g.limit, get::ROW_CEILING);
        assert!(g.limit_defaulted, "nobody typed --limit, and the disclosure has to know");
        assert_eq!(g.render, get::Render::Table, "the plane's default");
        assert_eq!(g.window, get::Window::Days(7));
        assert_eq!(g.spec.text(), "binance:BTCUSDT:1h");

        let a = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--limit", "5", "--json"])
            .expect("a line");
        let g = a.get.as_ref().expect("a GetArgs");
        assert_eq!(g.limit, 5);
        assert!(!g.limit_defaulted, "a named limit says so rather than reading as the default");
        assert_eq!(g.render, get::Render::Json);
        assert!(a.json, "…and `Args::json` is its PROJECTION, not a second decision");
    }

    /// **`jsonl` is a THIRD state and `Args::json` has two**, so the render is the authority for
    /// this verb — the reason [`GetArgs::render`] exists at all. A `jsonl` run must not be
    /// indistinguishable from a `table` one downstream.
    #[test]
    fn a_jsonl_get_is_not_a_json_get_and_not_a_table_one_either() {
        let line = |f: &str| {
            parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--format", f])
                .unwrap_or_else(|e| panic!("{f}: {e}"))
        };
        let jsonl = line("jsonl");
        assert_eq!(jsonl.get.as_ref().expect("a GetArgs").render, get::Render::Jsonl);
        assert!(!jsonl.json, "jsonl is NOT the one-document form");
        // THE CONTROLS: the two states `Args::json` CAN express, so the assertion above is about
        // `jsonl` being a third rather than about the field always being false.
        assert!(!line("table").json);
        assert!(line("json").json);

        // ...and because it is a third state, `--json --format jsonl` is a SECOND way the two
        // spellings can disagree — refused with a sentence of its own rather than resolved, for
        // the same reason `--json --format table` is.
        let err =
            parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"])
                .expect_err("a sequence is not one document");
        assert!(err.contains("SEQUENCE"), "{err}");
        assert!(err.contains("Pass one"), "…and what to do: {err}");
        // THE CONTROL: the AGREEING pair is not a contradiction, so the refusal is about the
        // values rather than about naming both spellings at all.
        assert!(
            parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "json"])
                .expect("agreeing spellings")
                .json
        );
    }

    /// **The `jsonl` contradiction is [`Sub::Get`]'s ALONE, and on every other verb the answer is
    /// the one the sibling group already gives.**
    ///
    /// ⚠ It shipped UNCONDITIONAL, above the verb dispatch, so `data hist ls --json --format
    /// jsonl` answered with a sentence describing GET's document and ending "Pass one" — advice
    /// that is false there, since dropping `--json` leaves a `--format jsonl` `ls` also refuses.
    /// Meanwhile `data catalog ls --json --format jsonl` answered with [`ROW_VERB`], because that
    /// parser reads `--format` eagerly and its contradiction check never sees a `jsonl`. Two
    /// groups, one plane, one question, two answers — which is exactly what
    /// `catalog`'s `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words` holds
    /// for the `table` spelling and nothing held for this one.
    #[test]
    fn a_jsonl_contradiction_on_a_catalog_verb_names_the_row_verb_in_both_groups() {
        let hist = parse_of(&["hist", "ls", "--json", "--format", "jsonl"])
            .expect_err("a catalog verb emits no rows");
        assert!(hist.contains(ROW_VERB), "it must name where `jsonl` works: {hist}");
        assert!(
            !hist.contains("SEQUENCE"),
            "…and must NOT hand over `get`'s document sentence, whose advice is false here: {hist}"
        );
        // ⚠ The CROSS-GROUP half of this pairing lives in `catalog`'s tests
        // (`the_two_groups_refuse_the_jsonl_format_contradiction_in_the_same_words`) and not here,
        // for a visibility reason rather than a taste one: `catalog::parse` is private to its own
        // module, so only a descendant can reach BOTH parsers.
        //
        // ...and it must not merely be refused: dropping `--json` has to leave the SAME answer,
        // because the arm above is gone on this verb and the format parser is what refuses it.
        let without = parse_of(&["hist", "ls", "--format", "jsonl"]).expect_err("still refused");
        assert_eq!(hist, without, "`--json` must not change what `--format jsonl` means on `ls`");
        // THE CONTROL: the same pair on the ROW verb DOES get the document sentence, so the
        // assertions above are about the VERB rather than about the arm being dead.
        let get =
            parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"])
                .expect_err("a sequence is not one document");
        assert!(get.contains("SEQUENCE"), "{get}");
        assert_ne!(get, hist, "the two verbs answer with different sentences");
    }

    /// **THE ACCOUNT-KIND REFUSAL, and the ORDER it is applied in.** `--kind` does not apply to
    /// `get` at all — but for an ACCOUNT kind the §9.3.2 sentence comes FIRST, because "this plane
    /// does not serve your fills" outranks "that flag belongs elsewhere".
    #[test]
    fn an_account_kind_on_get_is_the_plane_s_refusal_and_not_a_flag_note() {
        for kind in vike_model::ACCOUNT_KINDS {
            let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--kind", kind])
                .expect_err("account data is not this plane's");
            assert!(err.contains("ACCOUNT data"), "{kind}: {err}");
            assert!(err.contains("vike-cli account"), "…and where it will live: {kind}: {err}");
        }
        // THE CONTROL: a MARKET kind gets the flag-placement answer instead, so the refusal above
        // is about the kind rather than about `--kind` being refused with one sentence for all.
        let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--kind", "trade"])
            .expect_err("`get` reads bars");
        assert!(!err.contains("ACCOUNT data"), "{err}");
        assert!(err.contains("reads BARS"), "{err}");
        assert!(err.contains("data hist ls"), "…and names where a kind IS a filter: {err}");
    }

    /// The LISTING flags are refused on `get` by name, and `--limit` is refused everywhere else by
    /// name — both directions of the one rule that this verb's noun is a ROW and its siblings' is a
    /// series.
    ///
    /// ⚠ **The `--limit` half DERIVES its roster**, as its two neighbours upfile already do
    /// ([`a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store`],
    /// [`a_window_on_a_whole_span_read_verb_is_refused_and_names_the_verb_that_takes_one`]). It
    /// shipped hand-typed and three verbs short — `gate`, `rm` and `repair` were never exercised.
    /// The production guard is `sub != Sub::Get`, so it was right for them anyway; what was wrong
    /// was the PROOF, which would have stayed green through a new [`SUBCOMMANDS`] row or a guard
    /// refined into an enumeration.
    ///
    /// ⚠ …and it asserts the RENDERED [`ROW_VERB`], not the words inside it. `contains("data hist
    /// get")` is satisfied by a stale hand copy, which is the drift the const exists to stop.
    #[test]
    fn the_row_flag_and_the_listing_flags_are_refused_on_each_other() {
        for (flag, value) in
            [("--venue", Some("binance")), ("--name", Some("BTC")), ("--class", None)]
        {
            let mut v = vec!["hist", "get", "d:S:1h", "--days", "1", flag];
            v.extend(value);
            let err = parse_of(&v).expect_err(flag);
            assert!(err.contains(flag), "{flag}: {err}");
            assert!(err.contains("data hist ls"), "{flag} must name the listing verb: {err}");
        }
        let mut refused = 0usize;
        for sub in SUBCOMMANDS.iter().filter(|s| **s != Sub::Get) {
            let err = parse_of(&["hist", sub.as_str(), "--limit", "10"]).expect_err(sub.as_str());
            assert!(err.contains("--limit"), "{}: {err}", sub.as_str());
            assert!(err.contains(ROW_VERB), "{} must RENDER the row verb: {err}", sub.as_str());
            refused += 1;
        }
        // Anti-vacuity: a `SUBCOMMANDS` that had lost its rows, or a filter that kept none, would
        // leave the loop above asserting nothing at all.
        assert_eq!(refused, SUBCOMMANDS.len() - 1, "every verb but `get` was exercised");
        assert!(refused > 5, "and there are enough of them for that to mean something");
        // THE CONTROL: `get` itself TAKES the flag, so the refusals above are about the verb
        // rather than about `--limit` being rejected wherever it appears.
        let a = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--limit", "10"])
            .expect("`get` is the verb that bounds rows");
        assert_eq!(a.get.as_ref().expect("a GetArgs").limit, 10);
    }

    /// The usage roster names every subcommand and every flag the parser accepts — `crate::cmd::
    /// mcp`'s `the_instructions_name_only_real_commands` reads this text to check the MCP
    /// surface's `vike-cli data …` mentions, so a subcommand missing from it makes that check
    /// unable to confirm a command that does exist.
    #[test]
    fn the_usage_names_every_subcommand_and_flag_this_parser_accepts() {
        for sub in SUBCOMMANDS {
            assert!(USAGE.contains(sub.as_str()), "USAGE must name {}", sub.as_str());
        }
        for token in [
            "--out",
            "--produced-by",
            "--dry-run",
            "--yes",
            "--symbol",
            "--group",
            "--interval",
            "--days",
            "--from",
            "--to",
            "--store",
            "--engine",
            "--addr",
            "--kind",
            "--venue",
            "--name",
            "--class",
            "--partial-only",
            "--require-days",
            "--max-gap",
            "--require-kind",
            "--limit",
            "--format",
            "--json",
        ] {
            assert!(USAGE.contains(token), "USAGE must name {token}");
        }
        assert!(USAGE.contains(DEFAULT_ADDR), "…and the datahub default it resolves to");
        // ⚠ …and `get`'s ROW CEILING, which this page states as a bare NUMBER twice while
        // [`get::ROW_CEILING`] declares it. A `const &'static str` cannot interpolate, so the
        // duplication is unavoidable and this assertion is what stops it rotting — the same shape
        // the rung above gives `DEFAULT_ADDR`, and the same defect this file has watched a
        // hand-copied count produce more than once.
        assert!(
            USAGE.contains(&get::ROW_CEILING.to_string()),
            "USAGE states the row ceiling as a number and it no longer matches get::ROW_CEILING"
        );
    }
}
