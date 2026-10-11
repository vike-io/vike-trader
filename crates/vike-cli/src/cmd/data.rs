//! `vike-cli data` — get market data into the hist store, and ask a datahub what is in one.
//!
//! ```text
//! vike-cli data hist fetch <SPEC> (--days N | --from LABEL --to LABEL) [--store DIR] [--json]
//! vike-cli data hist fetch --source starter|demo [--store DIR] [--json]
//! vike-cli data hist running  [--addr HOST:PORT] [--json]
//! vike-cli data hist cancel <SPEC> [--addr HOST:PORT] [--json]
//! vike-cli data hist import FORMAT DATASET [--from DAY] [--to DAY] [--bars IV[,IV...] | --bars none]
//!                        [--dry-run [--verify]] [--yes] [--addr HOST:PORT] [--json]
//! vike-cli data hist export <SPEC> --out FILE [--from LABEL] [--to LABEL] [--engine PATH] [--json]
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
//! `fetch --source starter|demo`, `rm` and `repair` WRITE a store on THIS machine, by spawning the
//! standalone engine against it.
//! `list` / `coverage` / `tape-health` / `universe` READ, by asking a running `vike-datahub` over
//! RPC about the store THAT process opened. There is no third mode: this crate cannot open a hist
//! store itself, because
//! doing so needs DataFusion and being DataFusion-free is the crate's identity (argued edge by
//! edge in `crates/vike-cli/Cargo.toml`, machine-checked by CI's `light-consumers` lane).
//!
//! ⚠ **`export` moved from the first half to the second on 2026-09-26**, and it is the one verb
//! whose route did not change shape while its SOURCE did. Decision 0084's amendment closed the
//! local READ door on every history reader and named this verb as the reader it had left open: the
//! engine it spawns used to open the store at `--store`. The engine still does the ENCODING — this
//! crate links no Parquet writer — but it reads the bars through a datahub like everything else,
//! and `--store` on `export` is refused BY NAME with
//! `vike_datahub_client::flag_vocab::store_flag_removed`, the one sentence every reader prints.
//!
//! ⚠ **Those four are RULING 12** (§0.7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). The engine carried
//! `--fetch`, `--fetch-starter`, `--seed-demo`, `--export` and `--rm-series` as FLAGS on a verb
//! whose job is running backtests, and not one of the five is about backtesting — they fetch from
//! venues, write the store, export from it and DELETE from it. The operator-facing spelling is
//! HERE now, and each old flag is refused by name
//! (`crates/vike-backtest/src/backtest_cli/data_cmd.rs`'s `refuse_a_retired_data_flag`).
//!
//! ⚠ **What moved is the SURFACE. The code did not, and could not** — every one of the five opens
//! a `DataFusionHist`. So each subcommand below SPAWNS `backtest data <sub>`
//! (`crates/vike-backtest/src/backtest_cli/data_cmd.rs`'s `run_data`), which is the same grammar
//! typed at the same words. A reader who "finishes" the ruling by deleting the engine's
//! implementation deletes these verbs with it.
//!
//! ⚠ **`repair` is a THIRD shape and is engine-only**, which is worth saying here rather than only
//! at the verb: it names a series by identity like `rm` does, but it has no `--addr` at all. The
//! argument is [`refuse_the_remote_route_on_repair`]'s and it is not about tidiness — a datahub can
//! only name series it ENUMERATED, and a series whose base manifest is missing is in no
//! enumeration. That is the exact failure `repair` exists for, so a remote route would be reachable
//! for precisely the series that do not need one.
//!
//! ⚠ **`running` and `cancel` are a FOURTH shape, and neither half's.** They touch no store at
//! all: they ask a datahub about the `fetch` requests it is RUNNING — its own registry of them, kept
//! beside its collector table — and `cancel` raises the stop flag of every one on a series. So they
//! take `--addr` and nothing that names a store, a window or a listing filter, and each of those is
//! refused by name, in [`parse`]'s running-door arm. [`backfills`]' module doc carries what a cancel
//! does and does not do.
//!
//! ⚠ **`import` is a FIFTH shape, and it writes through a datahub without fetching anything.** It
//! asks a datahub to read ONE dataset of a vendor archive from a folder on THE DATAHUB'S OWN BOX
//! (`<project>/market_data/imports/FORMAT/DATASET/`) into the store that process opened — files the
//! operator synced there under their own vendor account, so no credential of theirs ever reaches
//! vike. Like `rm` it prints a PLAN first and asks for a typed confirmation; unlike every other verb
//! here it then sends one request per calendar month. It takes `--addr` and nothing that names a
//! local store, and its two positionals are a FORMAT and a DATASET rather than a spec. [`import`]'s
//! module doc carries the flow and the exit rung; `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`
//! carries why the files must be on the datahub's box.
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
//! `data hist rm --store /srv/…` works, so `data hist ls --store /srv/…` looks like it should — and
//! a silently-ignored `--store` would answer about a completely different store with no sign that
//! it had. The refusal names the way to read files on this machine too: serve them with a key-less
//! datahub first (`vike-backend datahub --store DIR`), which is what the owner's
//! 2026-09-25 ruling made the ONLY local read. It is the ONE sentence every history reader in the
//! workspace prints, `export` included — [`store_refusal`] says why a read verb adds one clause to
//! it.
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
//!   shape `crates/vike-app-core/src/data/stored_load.rs`'s `load_stored_tree` pays for the Data
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
//!   MEASURED: `crates/vike-data/src/store/datafusion_hist/inventory.rs`'s `coverage_report` skips every series
//!   whose kind is not in `crates/vike-data/src/store/coverage.rs`'s `TICK_KINDS`, and `bar` is not one
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
//! from `vike_datahub::backfill::KLINE_SOURCES` and advertised in the handshake. The
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
//! retrying any of them forever is what rung 3 exists to prevent (`crate::cmd::trade::status`'s
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

use std::path::Path;
use std::process::ExitCode;

use vike_node_proto::auth::NodeKeys;

#[cfg(test)]
use self::hist::get;
#[cfg(doc)]
use self::hist::{
    Coverage, Filter, InstrumentRow, SeriesRow, backfills,
    coverage::coverage_json,
    engine::report_json,
    gate, import,
    list::{execute_list, list_json},
    refuse::{refuse_the_remote_route_on_repair, store_refusal},
    rm::execute_rm,
    tape_health::{self, tape_health_json},
    universe::{self, universe_json},
};
use self::hist::{execute, parse::parse};
use self::shared::{
    CLASS_AS_OF_TS, DEFAULT_ADDR, FILE_VERB, Format, ROW_VERB, Source, UNBUILT_SOURCES, col,
    connect, empty_note, parse_format, parse_source,
};
use self::usage::USAGE;
use crate::cmd::args::exit_for_parse_error;

/// The STRUCTURAL scan: series whose own catalog contradicts itself. A module of its own rather
/// than more functions here, because every check in it is a pure fold over plain numbers and is
/// unit-tested against planted impossibilities — and because this file is long enough that a
/// seventh renderer in it would be read by nobody.
mod catalog;
mod hist;
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
mod shared;
mod source;
pub(super) mod usage;

/// PARSE ONLY: does `data`'s real grammar accept this argv tail — everything after `data`? For a
/// sibling module that PRINTS a `vike-cli data …` line and must never print one this binary
/// refuses; `crate::cmd::accepts` is the entry those tests call.
///
/// ⚠ It follows [`run`]'s routing rather than calling [`parse`] alone: `catalog`, `realtime` and
/// `source` are routed away before `parse` ever sees them, so handing one of those to `parse` would
/// REFUSE a line the binary accepts. They are refused here BY NAME instead — no printed line uses
/// one yet, and a checker that said yes to a grammar it never read would be worse than one that
/// says it does not know.
#[cfg(test)]
pub(super) fn accepts(argv: &[&str]) -> Result<(), String> {
    if let Some(group @ ("catalog" | "realtime" | "source")) = argv.first().copied() {
        return Err(format!(
            "`data {group}` has its own parser, which this check does not drive yet — wire it in \
             before printing a `data {group}` line"
        ));
    }
    parse(argv.iter().map(|s| s.to_string()), None).map(|_| ())
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
