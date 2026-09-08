//! `vike-cli data` — get market data into the hist store, and ask a datahub what is in one.
//!
//! ```text
//! vike-cli data fetch <SPEC> (--days N | --from LABEL --to LABEL) [--store DIR] [--json]
//! vike-cli data seed-demo [--store DIR] [--json]
//! vike-cli data list     [--addr HOST:PORT] [--kind K] [--venue V] [--name N] [--gaps] [--json]
//! vike-cli data coverage [--addr HOST:PORT] [--venue V] [--name N] [--partial-only] [--json]
//! vike-cli data rm       --kind K --venue V (--symbol S [--interval I] | --group G)
//!                        [--produced-by PREFIX] [--dry-run] [--yes]
//!                        [--addr HOST:PORT | --store DIR] [--engine PATH] [--json]
//! ```
//!
//! # ⚠ TWO HALVES, and they do not touch the same store
//!
//! `fetch` / `seed-demo` WRITE, by spawning the standalone engine against a store on THIS machine.
//! `list` / `coverage` READ, by asking a running `vike-datahub` over RPC about the store THAT
//! process opened. There is no third mode: this crate cannot open a hist store itself, because
//! doing so needs DataFusion and being DataFusion-free is the crate's identity (argued edge by
//! edge in `crates/vike-cli/Cargo.toml`, machine-checked by CI's `light-consumers` lane).
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
//! * **`--gaps`, a FLAG on `list` rather than a `data gaps` sibling.** A gap query names ONE series,
//!   and a series is identified by four dimensions with a grouped/per-symbol alternative inside them
//!   (see below). A standalone verb would have to build that identity out of flags — i.e. guess
//!   whether the operator meant a symbol or a group, and hand the server an id that matches nothing
//!   when it guessed wrong. As a flag it costs no guessing at all: the ids come back from
//!   `inventory()` and the ones the filter selected are handed straight back to `series_gaps`, so
//!   this side never CONSTRUCTS a series identity. The cost is one extra round trip per matched
//!   series — the same shape `crates/vike-app-core/src/stored_load.rs`'s `load_stored_tree` pays for
//!   the Data Manager, and the reason the filter flags are worth typing before the `--gaps` is.
//! * **`coverage`, a SIBLING subcommand rather than a second flag.** `coverage_report()` answers a
//!   different question over a different shape — per INSTRUMENT, joined ACROSS kinds, in UTC-day
//!   indices — and its whole product is the day one kind has and another lacks. Hanging it off
//!   `list` would make one verb emit two unrelated tables and, worse, put per-series epoch-ms gap
//!   ranges next to per-instrument day-index ones under one heading. Two units of "missing" in one
//!   output is a trap; two verbs is not.
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
//! `backtest` engine,
//! which has carried `--fetch`, `--seed-demo` and their siblings since long before this verb
//! existed. What was missing was DISCOVERABILITY: `vike-cli init` ends by telling a new user to run
//! `backtest --seed-demo`, which is a different binary with a different name, and a person who
//! installed "the vike CLI" has no reason to expect that the tool that runs a backtest is not the
//! tool that fetches the data for one. So this is a route, and [`crate::cmd::engine`] is the ONE
//! place that knows where the engine lives and what its exit codes mean.
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
//! What is NOT validated here is the VENUE and the INTERVAL, deliberately: which venues a build can
//! reach is a property of the ENGINE's `venue-fetch` feature and its collectors, and a roster copied
//! into this crate would be a second list to keep in step — refusing a venue the engine supports, or
//! accepting one it does not, with equal confidence. The engine's own error names what it can do.
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
//! [`report_json`] is the document for the write half; [`list_json`] and [`coverage_json`] are the
//! read half's. Each is emitted on SUCCESS only. That is the shape every
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
//! retrying any of them forever is what rung 3 exists to prevent (`crate::cmd::strategy_status`'s
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
use vike_model::epoch_ms_to_utc_date;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::engine;
use crate::exit::{CliError, CmdResult};

/// The default datahub listen address, and the same value `crate::cmd::backtest`,
/// `crate::cmd::sweep`, `crate::cmd::walkforward` and `crate::cmd::mcp` each spell for themselves.
/// A fifth private copy rather than a shared `pub(crate)` one is this crate's standing convention
/// for it: each verb owns the default it documents in its own `USAGE`, and the value mirrors
/// `VIKE_DATAHUB_ADDR`'s default in the server bin.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

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
usage: vike-cli data <subcommand> [options]

Get market data into the hist store the backtest engine reads, and ask a running
vike-datahub what is already in one. The two halves reach DIFFERENT stores and their
flags do not mix — each half's flags are refused on the other, by name.

WRITE — drives the standalone `backtest` engine on THIS machine (attached beside this
binary in a Linux release; on Windows a binary you supply yourself, and the failure
message says how) — see --engine below:
  fetch SPEC   pull REAL public bars into the store. SPEC is VENUE:SYMBOL:INTERVAL
               (e.g. binance:BTCUSDT:1h). Needs a window: --days, or --from/--to.
               No credentials — this is public market data
  seed-demo    write the SYNTHETIC demo tape into the store. Venue `demo`, a closed-form
               curve, NOT market data — it is the slice the shipped
               user_data/profiles/backtest.toml names, so a fresh install can run that
               profile immediately. Safe to re-run

READ — asks a running vike-datahub over RPC about the store THAT process opened. There
is no local-store read: opening one needs DataFusion, which this binary does not link:
  list         every stored series with its coverage — kind, venue, symbol-or-group,
               interval, rows, days, first/last. Add --gaps for the holes inside each
               matched series' recorded span (one extra round trip per series, so
               filter first)
  coverage     the CROSS-KIND report: per instrument, which days have trade but no
               book. A half-failed recording is invisible per series and obvious here

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

options:
  --days N        fetch: a window counting back from now
  --from LABEL    fetch: window start — epoch-ms, or YYYY-MM-DDTHH
  --to LABEL      fetch: window end, same spellings
  --store DIR     fetch/seed-demo/rm: the hist-store root to act on
  --engine PATH   fetch/seed-demo/rm: the standalone engine to run, instead of searching
                  <project>/bin, this executable's directory, and PATH
  --addr H:P      list/coverage/rm: the datahub to ask (default 127.0.0.1:7878). It binds
                  localhost, so reach a remote one over `ssh -L 7878:localhost:7878`.
                  ⚠ On `rm` it is the REMOTE route and is served only by a datahub that
                  holds node keys — a key-less one serves no delete verb at all
  --kind K        list: keep series whose kind contains K (bar/quote/trade/book/depth).
                  rm: the EXACT kind to delete from (required)
  --venue V       list/coverage: keep rows whose venue contains V. rm: the EXACT venue
                  (required)
  --symbol S      rm: the exact symbol of a PER-SYMBOL series. Omit to wildcard
  --group G       rm: the exact group of a GROUPED series (which has no symbol at all).
                  Alternatives, never a pair
  --interval I    rm: the exact bar interval. Omit to wildcard; refused with --group
  --produced-by P rm: the commit-key PREFIX every key of every matched series must
                  carry, or the repo-relative path of a declared producer, which
                  resolves to its prefix. REQUIRED whenever the selector can match more
                  than one series
  --dry-run       rm: print the plan and stop. Wins over --yes
  --yes           rm: the non-interactive confirmation. Without it and without a
                  terminal, the run is REFUSED — never read from a pipe
  --name N        list/coverage: keep rows whose NAME contains N — the symbol of a
                  per-symbol series, or the GROUP of a grouped one (a grouped series
                  has no symbol at all, which is why this is not called --symbol)
  --gaps          list: also fetch each matched series' gap ranges
  --partial-only  coverage: keep only instruments that have a day some recorded kind
                  covers and another does not
  --json          one JSON object on stdout describing the run. For fetch/seed-demo:
                  what was asked for, which engine ran, and its own report lines
                  verbatim, with that report moved to stderr so stdout is the document
                  and nothing else. For list/coverage: the rows, carrying each series'
                  raw symbol AND group rather than this side's rendering of them
  -h, --help      this message";

/// Which subcommand ran. Adding one is an arm here, an arm in [`execute`], and a row in [`USAGE`].
///
/// ⚠ The two halves the module doc opens with are this enum's two halves: [`Sub::is_read`] is the
/// split, and it is what decides which flags a given line may carry. Read it as the answer to
/// "does this subcommand talk to a server or spawn a child", because that is the only question the
/// flag refusals below ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    /// `fetch SPEC` — real public bars, over a window.
    Fetch,
    /// `seed-demo` — the synthetic tape, no window and no network.
    SeedDemo,
    /// `list` — every stored series with its coverage, from a datahub's `inventory()`.
    List,
    /// `coverage` — the cross-kind report, from a datahub's `coverage_report()`.
    Coverage,
    /// `rm` — DELETE series, irreversibly. The one subcommand that reaches EITHER store: `--addr`
    /// is the datahub route, its absence the engine route. See [`Sub::is_read`].
    Rm,
}

impl Sub {
    /// The name the operator typed, which is also what every refusal message names it by.
    fn as_str(self) -> &'static str {
        match self {
            Sub::Fetch => "fetch",
            Sub::SeedDemo => "seed-demo",
            Sub::List => "list",
            Sub::Coverage => "coverage",
            Sub::Rm => "rm",
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
        matches!(self, Sub::List | Sub::Coverage)
    }
}

/// The client-side row filter the read verbs apply to what the server already sent.
///
/// Case-insensitive SUBSTRING on each dimension, ANDed, and an absent field matches everything —
/// see the module doc for why this is a browse aid rather than a validated roster lookup. Nothing
/// here reaches the wire: both RPCs answer with the whole catalog and the filter is applied to the
/// answer, so a filter can never make the server do less work (which is exactly why `--gaps` is the
/// flag worth pairing one with — THAT does).
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

/// The parsed `data` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    /// `VENUE:SYMBOL:INTERVAL`, shape-checked. `None` for every subcommand but `fetch`.
    spec: Option<String>,
    /// `Some` only for `fetch` (the parser refuses one without, and refuses one anywhere else).
    window: Option<Window>,
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
    /// `--gaps`: on `list`, ask `series_gaps` for each MATCHED series after the inventory lands.
    gaps: bool,
    /// `--partial-only`: on `coverage`, keep only instruments with at least one partial day.
    partial_only: bool,
    /// `--json`: emit this subcommand's document instead of the human rendering. Applies to ALL
    /// FIVE — the write half because "what was written and where" is the question a caller driving
    /// it has, the read half because the answer IS data and a table is the lossy form of it, and
    /// `rm` because its plan carries the one fact a human reads and a machine must be able to
    /// compare: the store that answered.
    json: bool,
    /// `rm`'s selector. `None` for every other subcommand — the parser builds it only where it
    /// means something, so no other arm can read a half-filled one.
    rm: Option<RmArgs>,
    /// `--addr` was given EXPLICITLY. `addr` above is always resolved, so it cannot answer "did the
    /// operator ask for the remote route" — which is the question `rm` turns on.
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
    /// `--produced-by`, verbatim as typed. It is RESOLVED (a producer path → its prefix) on the far
    /// side, against `STORE_KINDS` — a roster this crate deliberately cannot see.
    produced_by: Option<String>,
    dry_run: bool,
    yes: bool,
}

/// Parse `data`'s own argv tail (everything after the verb). PURE — no I/O, no spawn.
///
/// ⚠ Every flag is accepted by the ONE loop below and then refused per-subcommand by
/// [`refuse_foreign_flags`], rather than being routed by a per-subcommand match. That ordering is
/// what lets an inapplicable flag be named in a message that says which subcommand it DOES belong
/// to; an unknown-option error would tell an operator the flag does not exist, which is false and
/// sends them looking in the wrong place.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err("a subcommand is required (fetch | seed-demo | list | coverage)".to_string());
    };
    let sub = match first.as_str() {
        "fetch" => Sub::Fetch,
        "seed-demo" => Sub::SeedDemo,
        "list" => Sub::List,
        "coverage" => Sub::Coverage,
        "rm" => Sub::Rm,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `data` subcommand '{other}'")),
    };

    let mut spec: Option<String> = None;
    let mut days: Option<String> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut store: Option<String> = None;
    let mut engine: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut filter = Filter::default();
    let mut gaps = false;
    let mut partial_only = false;
    let mut json = false;
    let mut symbol: Option<String> = None;
    let mut group: Option<String> = None;
    let mut interval: Option<String> = None;
    let mut produced_by: Option<String> = None;
    let mut dry_run = false;
    let mut yes = false;

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
            "--days" => days = Some(flags.value(&flag, inline)?),
            "--from" => from = Some(flags.value(&flag, inline)?),
            "--to" => to = Some(flags.value(&flag, inline)?),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--kind" => filter.kind = Some(flags.value(&flag, inline)?),
            "--venue" => filter.venue = Some(flags.value(&flag, inline)?),
            "--name" => filter.name = Some(flags.value(&flag, inline)?),
            "--gaps" => {
                no_value(&flag, inline)?;
                gaps = true;
            }
            "--partial-only" => {
                no_value(&flag, inline)?;
                partial_only = true;
            }
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            // ⚠ Judged on the `--` prefix, the same rule `crate::cmd::args`'s `is_flag_token`
            // spells for every valued flag in this crate: a token beginning with `--` is a FLAG,
            // so an unrecognised one is a usage error rather than something to read as a spec.
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            // The one POSITIONAL in this verb: the spec. Anything after the first is a mistake
            // worth naming — a second bare word is nearly always a shell-quoting accident, and
            // silently ignoring it would fetch a series the operator did not ask for.
            positional => match &spec {
                None => spec = Some(positional.to_string()),
                Some(already) => {
                    return Err(format!(
                        "unexpected extra argument '{positional}' (the spec is already \
                         '{already}'); one series per fetch"
                    ));
                }
            },
        }
    }

    // `rm`'s own flags belong to `rm` and to nothing else — refused BY NAME on every other
    // subcommand, in one place, for the reason [`refuse_foreign_flags`]'s doc gives: an operator
    // who typed `--produced-by` on a `list` is not looking for "unknown option", they are looking
    // for the verb that takes it.
    if sub != Sub::Rm {
        refuse_foreign_flags(
            sub,
            &[
                ("--symbol", symbol.is_some()),
                ("--group", group.is_some()),
                ("--interval", interval.is_some()),
                ("--produced-by", produced_by.is_some()),
                ("--dry-run", dry_run),
                ("--yes", yes),
            ],
            "that flag selects series for DELETION and belongs to `rm`. To narrow a LISTING use \
             --kind/--venue/--name, which are substring filters over what a datahub already sent",
        )?;
    }

    // The half-crossing refusals, spelled once for both directions. A read verb may not carry a
    // store-side flag and a write verb may not carry a datahub-side one — see [`Sub::is_read`] and
    // the module doc's opening for why "ignore it" was never an option here.
    if sub.is_read() {
        refuse_foreign_flags(
            sub,
            &[
                ("--store", store.is_some()),
                ("--engine", engine.is_some()),
                ("--days", days.is_some()),
                ("--from", from.is_some()),
                ("--to", to.is_some()),
            ],
            "that flag names a hist store (or the engine that writes one) on THIS machine, and \
             `list`/`coverage` read the store a running vike-datahub already has open — reach it \
             with --addr",
        )?;
        if let Some(extra) = &spec {
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
                ("--gaps", gaps),
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
    } else {
        refuse_foreign_flags(
            sub,
            &[
                ("--addr", addr.is_some()),
                ("--kind", filter.kind.is_some()),
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--gaps", gaps),
                ("--partial-only", partial_only),
            ],
            "that flag belongs to the READ half (`list`/`coverage`), which asks a running \
             vike-datahub about a store — while `fetch`/`seed-demo` drive the engine against a \
             store on this machine, named with --store",
        )?;
    }

    // The per-subcommand half of the grammar. Only `fetch` produces a spec and a window; the other
    // arms are refusals (and `rm`'s own construction), so the ONE `Args` below cannot drift between
    // subcommands the way five hand-written literals would.
    let mut rm = None;
    let (spec, window) = match sub {
        Sub::Fetch => {
            let spec = spec.ok_or(
                "fetch needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data fetch \
                 binance:BTCUSDT:1h --days 180`",
            )?;
            check_spec(&spec)?;
            (Some(spec), Some(window_from(days, from, to)?))
        }
        Sub::SeedDemo => {
            // Every fetch-shaped flag is REFUSED here rather than ignored. The demo tape is a
            // closed-form curve computed over a fixed span: a `--days 30` that quietly did nothing
            // would leave an operator believing they had seeded a month.
            if spec.is_some() {
                return Err("seed-demo takes no spec — it writes the synthetic `demo` tape".into());
            }
            for (flag, present) in
                [("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())]
            {
                if present {
                    return Err(format!(
                        "{flag} applies to `fetch` only — the demo tape is a fixed synthetic span"
                    ));
                }
            }
            (None, None)
        }
        Sub::List => {
            // `--partial-only` is a verdict about the CROSS-KIND join, and a per-series listing has
            // no such verdict to filter on — each row here is one kind, which can never disagree
            // with itself.
            refuse_foreign_flags(
                sub,
                &[("--partial-only", partial_only)],
                "that flag filters `coverage`'s cross-kind verdict, and a `list` row is ONE \
                 series of ONE kind — there is nothing for it to disagree with",
            )?;
            (None, None)
        }
        Sub::Coverage => {
            // Two refusals, and each is about a UNIT rather than about tidiness. `--gaps` fetches
            // per-series holes in epoch-ms; a coverage row's days are UTC-day indices. `--kind`
            // would filter away exactly the kinds whose disagreement the report exists to show.
            refuse_foreign_flags(
                sub,
                &[("--gaps", gaps)],
                "that flag fetches one SERIES' gap ranges in epoch-ms, while a `coverage` row is \
                 one INSTRUMENT's kinds lined up in UTC-day indices — `vike-cli data list --gaps` \
                 is where the per-series holes are",
            )?;
            refuse_foreign_flags(
                sub,
                &[("--kind", filter.kind.is_some())],
                "a coverage row IS the join across kinds, so filtering to one would leave a \
                 report that cannot show a day one kind has and another lacks — which is the \
                 whole of what it reports",
            )?;
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
            for (flag, value) in [
                ("--kind", Some(&kind)),
                ("--venue", Some(&venue)),
                ("--symbol", symbol.as_ref()),
                ("--group", group.as_ref()),
                ("--interval", interval.as_ref()),
                ("--produced-by", produced_by.as_ref()),
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
            rm = Some(RmArgs { kind, venue, symbol, group, interval, produced_by, dry_run, yes });
            (None, None)
        }
    };

    Ok(Args {
        sub,
        spec,
        window,
        store,
        engine,
        addr_given: addr.is_some(),
        addr: addr.unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        filter,
        gaps,
        partial_only,
        json,
        rm,
    })
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

/// Entry point the dispatcher routes to. `args` is everything AFTER the `data` verb; `project_root`
/// is `<project>`, resolved once by [`crate::run`], and is how the engine under `<project>/bin` is
/// found — a PARAMETER, because a `src/cmd/` file may not read the environment for itself.
pub fn run(
    args: impl Iterator<Item = String>,
    project_root: Option<&Path>,
    keys: Option<&NodeKeys>,
) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("data", USAGE, &msg),
    };
    match execute(&args, project_root, keys) {
        Ok(()) => ExitCode::SUCCESS,
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
fn execute(args: &Args, project_root: Option<&Path>, keys: Option<&NodeKeys>) -> CmdResult<()> {
    match args.sub {
        // The engine routes spawn a child against a LOCAL store and open no socket, so a datahub
        // key is not theirs to carry.
        Sub::Fetch | Sub::SeedDemo => execute_engine(args, project_root),
        Sub::List => execute_list(args, keys),
        Sub::Coverage => execute_coverage(args, keys),
        Sub::Rm => execute_rm(args, project_root, keys),
    }
}

// ─── `rm`: the one verb that reaches EITHER store ───────────────────────────────────────────────

/// `data rm` — route to the datahub or to the engine, having first refused the one shape neither
/// may be asked to handle.
///
/// # ⚠ The TTY refusal happens HERE, before a socket or a process
///
/// No `--yes`, no `--dry-run` and no terminal on stdin is a REFUSAL, on the usage rung. It is not a
/// pre-flight for the far side's own check — both routes refuse it too — it is the rung: nothing
/// was attempted and re-running unchanged cannot succeed, which is exactly what `Exit::Usage`
/// promises and what a wrapper needs to hear before it retries. Reading a confirmation from a PIPE
/// is the failure this exists to prevent (`yes | vike-cli data rm …`), and a pipe is
/// indistinguishable from a person once you have decided to read one.
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

/// The LOCAL route: drive the engine's `--rm-series` against a store on this machine.
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

/// The engine flags `rm`'s arguments become — PURE, so the translation is unit-tested rather than
/// only observed through a spawn.
///
/// ⚠ `--produced-by` is forwarded VERBATIM, never resolved here. A producer-path spelling resolves
/// against `STORE_KINDS`, and that table lives in `vike-data` — the tree this crate exists not to
/// link. Resolving it here would be a second copy of the roster; the engine's message names what it
/// could not resolve.
fn rm_engine_argv(args: &Args, rm: &RmArgs) -> Vec<String> {
    let mut argv = vec![
        "--rm-series".to_string(),
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
        // deliberate and it is about ONE fact: the engine's `--rm-series` emits a machine document
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
/// crate's sentences; `--rm-series` prints a JSON document it owns, so nesting it hands a caller
/// the structure rather than a string to re-parse. A document that does not parse degrades to the
/// raw lines under `engine_report_lines`, so a caller is never handed a silently-empty object.
fn rm_json_local(
    args: &Args,
    rm: &RmArgs,
    program: &Path,
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
        "engine": program.display().to_string(),
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
/// [`connect`] asks for [`Scope::Control`] here — a delete is a write — so a key that only grants
/// Observe is refused at the socket rather than after a selector has travelled.
/// [`rm_remote_hint`] is the sentence that turns any of these into an instruction.
fn execute_rm_remote(args: &Args, rm: &RmArgs, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // ⚠ NAMED through `vike_datahub_client::proto`'s re-export, never through a `vike_data` edge —
    // this crate takes that dependency for DEV targets only, and the re-export exists so the wire's
    // vocabulary can be CONSTRUCTED here without one. See that re-export's own doc.
    use vike_datahub_client::proto::{SeriesSelector, describe_id};

    // Control: this route DELETES. A keyed server that would grant only Observe refuses here,
    // before a selector is sent — which is the refusal an operator wants to see.
    let mut client = connect(&args.addr, keys, Scope::Control)?;
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

    if !args.json {
        // The remote route's answer to "WHICH store": the one that process opened. This side cannot
        // name its path — that is the server's resolution, in another process on another box — so
        // it names the SERVER rather than guessing at a directory.
        println!("store: the one the datahub at {} has open", args.addr);
        for line in plan.lines() {
            println!("{line}");
        }
    }
    if rm.dry_run {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, None));
        } else {
            println!("--dry-run: nothing was deleted");
        }
        return Ok(());
    }
    // "Nothing matched" is a SUCCESS, on the same rung as a delete — the server's delete is
    // idempotent and a cleanup that fails on re-run is a cleanup nobody re-runs.
    if plan.matched() == 0 {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, Some(&Default::default())));
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
        println!("{}", rm_json_remote(args, rm, &done.plan, Some(&outcome)));
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
     delete ON that box: `vike-cli data rm --store DIR …`, over `ssh` if it is remote."
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
fn rm_json_remote(
    args: &Args,
    rm: &RmArgs,
    plan: &vike_datahub_client::proto::RemovalPlan,
    outcome: Option<&vike_datahub_client::proto::RemovalOutcome>,
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
/// ⚠ **`report` is where the counts are, and they are not parsed into fields.** `backtest --fetch`
/// prints `N bars returned … M rows written` and `--seed-demo` prints a line per slice; neither has
/// a `--json` mode of its own, so the only way to field those numbers would be to read them out of
/// the sentences — a second implementation of another crate's output format, in a crate that cannot
/// see it change, which would start reporting a WRONG count rather than failing on the day a word
/// moves. Giving a caller the lines is honest; claiming to have understood them would not be. The
/// day the engine grows a machine report for these two paths, this field becomes structured and
/// the change is one function.
fn report_json(args: &Args, program: &Path, argv: &[String], report: &[String]) -> String {
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
        "window": match &args.window {
            Some(Window::Days(d)) => serde_json::json!({ "days": d }),
            Some(Window::Range { from, to }) => serde_json::json!({ "from": from, "to": to }),
            None => serde_json::Value::Null,
        },
        "engine": program.display().to_string(),
        "engine_argv": argv,
        "report": report,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

/// The engine flags this verb's arguments become — PURE, so the translation is unit-tested rather
/// than only observed through a spawn.
fn engine_argv(args: &Args) -> Vec<String> {
    let mut argv = Vec::new();
    match args.sub {
        Sub::Fetch => {
            argv.push("--fetch".to_string());
            // Present by construction: `parse` refuses a `fetch` without one.
            argv.push(args.spec.clone().unwrap_or_default());
            match &args.window {
                Some(Window::Days(d)) => {
                    argv.push("--days".to_string());
                    argv.push(d.clone());
                }
                Some(Window::Range { from, to }) => {
                    argv.push("--from".to_string());
                    argv.push(from.clone());
                    argv.push("--to".to_string());
                    argv.push(to.clone());
                }
                None => {}
            }
        }
        Sub::SeedDemo => argv.push("--seed-demo".to_string()),
        // ⚠ UNREACHABLE: [`execute`] routes the read verbs to their own arms, and `rm` builds its
        // argv in [`rm_engine_argv`] — its selector has six flags of its own and folding them in
        // here would make one function answer for two grammars. Named rather than folded into a
        // `_`, deliberately: a wildcard would silently absorb a future subcommand that DOES need an
        // engine flag and hand the child an argv with the flag missing instead of failing to
        // compile.
        Sub::List | Sub::Coverage | Sub::Rm => {}
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
/// `connect_authed` at the `scope` the CALLER names — a read verb asks for [`Scope::Observe`] and
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

/// One row of `list`: a series' identity, its coverage, and — only under `--gaps` — its holes.
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
    /// not asked (no `--gaps`) or could not be answered — `gaps_error` tells those apart, and an
    /// EMPTY `Some` is the real "this series has no holes".
    gaps: Option<Vec<(i64, i64)>>,
    /// Why this one series' gap probe failed, when it did. See [`execute_list`] for the degrade.
    gaps_error: Option<String>,
}

/// `data list` — one `inventory()` round trip, filtered client-side, plus one `series_gaps` probe
/// per MATCHED series under `--gaps`.
///
/// ⚠ **A failed gap probe degrades the ROW; it does not fail the run.** The listing is what was
/// asked for and it is complete and correct; the gaps are an annotation on it, and one series whose
/// manifest cannot be read should not make `data list` unusable against a store of a thousand. The
/// failure is not swallowed either — it lands on the row it belongs to in both renderings, and
/// [`SeriesRow::gaps`] stays `None` so a machine reader can never mistake it for "no holes". Same
/// contract, and the same argument, as `crates/vike-app-core/src/stored_load.rs`'s
/// `load_stored_tree`, which the Data Manager runs over the same two verbs.
///
/// The rejected alternative was exiting on the run-failure rung with the table still emitted: that
/// makes a wrapper treat a complete listing as no listing, and there is no rung for "mostly".
fn execute_list(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Observe)?;
    // A server-side `Response::Error` and a protocol desync both arrive as `Err(String)` and both
    // stay on the pre-existing rung: once the connection is open, a failure is the request's and
    // not the connection's.
    let inventory = client.inventory()?;
    let reported = inventory.len();

    let mut rows: Vec<SeriesRow> = Vec::new();
    for (id, cov) in &inventory {
        if !args.filter.matches(Some(&id.kind), &id.venue, id.label()) {
            continue;
        }
        // The id is handed BACK to the server exactly as it arrived — this side never constructs
        // a `SeriesId`, which is the whole reason `--gaps` is a flag on a listing rather than a
        // verb taking a four-dimensional identity off the command line.
        let (gaps, gaps_error) = if args.gaps {
            match client.series_gaps(id) {
                Ok(ranges) => (Some(ranges), None),
                Err(e) => (None, Some(e)),
            }
        } else {
            (None, None)
        };
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
        });
    }

    if args.json {
        println!("{}", list_json(args, &rows, reported));
    } else {
        for line in list_lines(&rows, reported, !args.filter.is_empty(), args.gaps) {
            println!("{line}");
        }
    }
    Ok(())
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
fn list_lines(
    rows: &[SeriesRow],
    reported: usize,
    filtered: bool,
    gaps_requested: bool,
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

    let mut lines = vec![format!(
        "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {:>rows_w$}  \
         {:>days_w$}  {:<10}  {}",
        "KIND", "VENUE", "SCOPE", "NAME", "INTERVAL", "ROWS", "DAYS", "FIRST", "LAST"
    )];
    for r in rows {
        lines.push(format!(
            "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {:>rows_w$}  \
             {:>days_w$}  {:<10}  {}",
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

/// The gap annotation under one `list` row, under `--gaps` only.
///
/// Three outcomes, and all three are SAID rather than implied by an absence: holes, no holes, and
/// a probe this store could not answer. Printing nothing for the middle case would leave the
/// operator who typed `--gaps` unable to tell an answered "clean" from an unasked question.
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
            })
        })
        .collect();
    let doc = serde_json::json!({
        "subcommand": "list",
        "addr": args.addr,
        "filter": {
            "kind": args.filter.kind,
            "venue": args.filter.venue,
            "name": args.filter.name,
        },
        "gaps_requested": args.gaps,
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

/// `data coverage` — one `coverage_report()` round trip, filtered and optionally narrowed to the
/// instruments that actually have a disagreement.
///
/// ⚠ The verb is CAPABILITY-NEGOTIATED on the client side: against a datahub too old to advertise
/// the coverage feature, `coverage_report()` refuses before sending anything and its own sentence
/// says so. That refusal stays on the run-failure rung, not the connect one — the box answered the
/// handshake, so it is a fact about a REACHABLE server and retrying it forever is the inversion the
/// ladder exists to prevent (`crate::cmd::strategy_status`'s `failure_exit` argues the same rule
/// against a tradehub node).
fn execute_coverage(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Observe)?;
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
/// `vike-cli data list --gaps` is the per-series view.
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
        "subcommand": "coverage",
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

// ─── rendering helpers, shared by both read verbs ───────────────────────────────────────────────

/// A column's width: the widest cell, never narrower than its own header. Same shape
/// `crate::cmd::strategy_status`'s `human_lines` uses, so a long venue slug widens its column
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn each_subcommand_parses() {
        assert_eq!(
            parse_of(&["fetch", "binance:BTCUSDT:1h", "--days", "7"]).unwrap().sub,
            Sub::Fetch
        );
        assert_eq!(parse_of(&["seed-demo"]).unwrap().sub, Sub::SeedDemo);
        assert_eq!(parse_of(&["list"]).unwrap().sub, Sub::List);
        assert_eq!(parse_of(&["coverage"]).unwrap().sub, Sub::Coverage);
    }

    #[test]
    fn help_short_circuits_at_both_levels() {
        assert_eq!(parse_of(&["--help"]).unwrap_err(), HELP_SENTINEL);
        assert_eq!(parse_of(&["fetch", "-h"]).unwrap_err(), HELP_SENTINEL);
    }

    /// The verb takes no default action, so a bare `data` must name every subcommand there is —
    /// this message is the only place a user who typed the verb alone learns what it can do.
    #[test]
    fn a_missing_subcommand_names_every_subcommand_that_exists() {
        let err = parse_of(&[]).unwrap_err();
        for sub in ["fetch", "seed-demo", "list", "coverage"] {
            assert!(err.contains(sub), "the missing-subcommand error must name {sub}: {err}");
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
        let err = parse_of(&["fetch", "binance:BTCUSDT:1h"]).unwrap_err();
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
        assert!(parse_of(&["seed-demo", "--days", "30"]).unwrap_err().contains("--days"));
        assert!(parse_of(&["seed-demo", "--from", "0"]).unwrap_err().contains("--from"));
        assert!(parse_of(&["seed-demo", "binance:BTCUSDT:1h"]).unwrap_err().contains("no spec"));
        // …and the one flag that DOES apply to it still parses.
        assert_eq!(parse_of(&["seed-demo", "--store", "/s"]).unwrap().store.as_deref(), Some("/s"));
    }

    /// A second bare word is a shell-quoting accident far more often than an intention, and
    /// ignoring it would fetch a series nobody asked for.
    #[test]
    fn a_second_positional_is_refused_naming_both() {
        let err = parse_of(&["fetch", "binance:BTCUSDT:1h", "okx:BTC-USDT:1h", "--days", "7"])
            .unwrap_err();
        assert!(err.contains("okx:BTC-USDT:1h") && err.contains("binance:BTCUSDT:1h"), "{err}");
    }

    /// THE translation: what the engine is actually asked to do. Pinned as argv because that is
    /// the whole product of this module — everything else is the engine's.
    #[test]
    fn the_engine_argv_is_the_translation() {
        let days =
            parse_of(&["fetch", "binance:BTCUSDT:1h", "--days", "180", "--store", "/s"]).unwrap();
        assert_eq!(
            engine_argv(&days),
            ["--fetch", "binance:BTCUSDT:1h", "--days", "180", "--store", "/s"]
        );

        let range = parse_of(&["fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100"]).unwrap();
        assert_eq!(
            engine_argv(&range),
            ["--fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100"]
        );

        assert_eq!(engine_argv(&parse_of(&["seed-demo"]).unwrap()), ["--seed-demo"]);
        assert_eq!(
            engine_argv(&parse_of(&["seed-demo", "--store", "/s"]).unwrap()),
            ["--seed-demo", "--store", "/s"]
        );
    }

    /// `--json` parses on BOTH subcommands, takes no value, and — like `--engine` — is consumed
    /// here rather than forwarded. The engine's own `--json` is a different flag on a different
    /// code path (it renders a `BacktestReport`), and passing this one through would ask `--fetch`
    /// for a document it does not produce.
    #[test]
    fn json_parses_on_both_subcommands_and_is_not_forwarded() {
        for argv in [
            vec!["fetch", "binance:BTCUSDT:1h", "--days", "7", "--json"],
            vec!["seed-demo", "--json"],
        ] {
            let a = parse_of(&argv).unwrap();
            assert!(a.json, "{argv:?}");
            assert!(!engine_argv(&a).iter().any(|s| s == "--json"), "{:?}", engine_argv(&a));
        }
        assert!(!parse_of(&["seed-demo"]).unwrap().json, "absent means absent");
        // A value is refused rather than swallowed — the same `no_value` rung every other
        // valueless flag in this crate uses.
        assert!(parse_of(&["seed-demo", "--json=1"]).unwrap_err().contains("--json"));
    }

    /// The document, field by field. It is built from the SAME parsed `Args` and the SAME argv the
    /// engine was handed, so a machine and a person cannot be told different things about one run.
    #[test]
    fn the_json_document_carries_the_request_the_engine_and_its_report() {
        let a =
            parse_of(&["fetch", "binance:BTCUSDT:1h", "--days", "180", "--store", "/s", "--json"])
                .unwrap();
        let argv = engine_argv(&a);
        let doc: serde_json::Value = serde_json::from_str(&report_json(
            &a,
            Path::new("/opt/backtest"),
            &argv,
            &["12 bars".to_string()],
        ))
        .expect("report_json writes JSON");

        assert_eq!(doc["subcommand"], "fetch");
        assert_eq!(doc["store"], "/s");
        assert_eq!(doc["series"]["venue"], "binance");
        assert_eq!(doc["series"]["symbol"], "BTCUSDT");
        assert_eq!(doc["series"]["interval"], "1h");
        assert_eq!(doc["series"]["spec"], "binance:BTCUSDT:1h");
        assert_eq!(doc["window"]["days"], "180");
        assert_eq!(doc["engine"], "/opt/backtest");
        assert_eq!(doc["engine_argv"][0], "--fetch");
        assert_eq!(doc["report"][0], "12 bars");

        // The range window is the OTHER form, and a caller must be able to tell which it got
        // without re-parsing the argv.
        let range = parse_of(&["fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100", "--json"])
            .unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&report_json(&range, Path::new("b"), &[], &[])).unwrap();
        assert_eq!(doc["window"]["from"], "0");
        assert_eq!(doc["window"]["to"], "100");
        assert!(doc["window"]["days"].is_null());

        // `seed-demo` takes neither a spec nor a window, and both are NULL rather than absent: a
        // machine can tell "no series" from "the field is gone" only if the field is there.
        let seed = parse_of(&["seed-demo", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&report_json(&seed, Path::new("b"), &[], &[])).unwrap();
        assert_eq!(doc["subcommand"], "seed-demo");
        assert!(doc["series"].is_null());
        assert!(doc["window"].is_null());
        // ...and an unnamed store is NULL, never a guessed path: the ENGINE resolves the root, in
        // another process, and this side would be inventing one.
        assert!(doc["store"].is_null());
    }

    /// `--engine` never reaches the child: it says WHICH binary to run, not what to tell it.
    #[test]
    fn the_engine_flag_is_consumed_here_and_not_forwarded() {
        let a = parse_of(&["seed-demo", "--engine", "/opt/backtest"]).unwrap();
        assert_eq!(a.engine.as_deref(), Some("/opt/backtest"));
        assert!(!engine_argv(&a).iter().any(|s| s == "--engine"), "{:?}", engine_argv(&a));
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
        }
    }

    #[test]
    fn the_read_subcommands_default_the_addr_and_take_the_filter_flags() {
        let a = parse_of(&["list"]).unwrap();
        assert_eq!(a.addr, DEFAULT_ADDR);
        assert_eq!(a.filter, Filter::default());
        assert!(!a.gaps && !a.partial_only && !a.json);

        let a = parse_of(&[
            "list",
            "--addr",
            "1.2.3.4:9",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--name",
            "BTC",
            "--gaps",
            "--json",
        ])
        .unwrap();
        assert_eq!(a.addr, "1.2.3.4:9");
        assert_eq!(a.filter.kind.as_deref(), Some("bar"));
        assert_eq!(a.filter.venue.as_deref(), Some("binance"));
        assert_eq!(a.filter.name.as_deref(), Some("BTC"));
        assert!(a.gaps && a.json);

        let a = parse_of(&["coverage", "--venue", "binance", "--partial-only"]).unwrap();
        assert!(a.partial_only);
        assert_eq!(a.filter.venue.as_deref(), Some("binance"));
    }

    /// The bare booleans reject an inline value, on the same rung every valueless flag in this
    /// crate uses.
    #[test]
    fn the_read_booleans_take_no_value() {
        assert!(parse_of(&["list", "--gaps=1"]).unwrap_err().contains("--gaps"));
        assert!(
            parse_of(&["coverage", "--partial-only=yes"]).unwrap_err().contains("--partial-only")
        );
    }

    /// ⚠ **THE refusal this module exists to make loud.** `--store` on a read verb is the mistake
    /// an operator makes first, because the sibling subcommand takes one — and a silently-ignored
    /// `--store` would answer about a completely different store with no sign that it had. The
    /// message must name the flag, the verb, and the flag that reaches the other store.
    #[test]
    fn a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store() {
        for (argv, flag) in [
            (vec!["list", "--store", "/srv/hist"], "--store"),
            (vec!["list", "--engine", "/opt/backtest"], "--engine"),
            (vec!["coverage", "--store", "/srv/hist"], "--store"),
            (vec!["list", "--days", "7"], "--days"),
            (vec!["coverage", "--from", "0"], "--from"),
        ] {
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("--addr"), "{argv:?} must point at --addr: {err}");
        }
    }

    /// …and the mirror image: a datahub flag on the write half is refused rather than ignored,
    /// naming the half it belongs to. Ignoring one would let `data fetch --addr the CI box:7878` read
    /// as "fetch into the remote store", which is not a thing this verb can do.
    #[test]
    fn a_datahub_flag_on_a_write_verb_is_refused_and_names_the_read_half() {
        for (argv, flag) in [
            (vec!["fetch", "binance:BTCUSDT:1h", "--days", "7", "--addr", "p:1"], "--addr"),
            (vec!["seed-demo", "--gaps"], "--gaps"),
            (vec!["seed-demo", "--venue", "binance"], "--venue"),
            (vec!["fetch", "binance:BTCUSDT:1h", "--days", "7", "--name", "BTC"], "--name"),
        ] {
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("list"), "{argv:?} must name the half it belongs to: {err}");
        }
    }

    /// The two intra-half refusals, each about a UNIT rather than about tidiness — see the arms in
    /// [`parse`]. `--kind` on `coverage` would filter away the disagreement the report exists to
    /// show; `--gaps` there would put epoch-ms ranges beside UTC-day indices under one heading.
    #[test]
    fn each_read_verb_refuses_the_others_flag_with_the_reason() {
        let err = parse_of(&["list", "--partial-only"]).unwrap_err();
        assert!(err.contains("--partial-only") && err.contains("coverage"), "{err}");

        let err = parse_of(&["coverage", "--gaps"]).unwrap_err();
        assert!(err.contains("--gaps") && err.contains("epoch-ms"), "{err}");

        let err = parse_of(&["coverage", "--kind", "trade"]).unwrap_err();
        assert!(err.contains("--kind") && err.contains("across kinds"), "{err}");
    }

    /// A colon-string on a read verb is refused with the REASON: a stored series is four
    /// dimensions with an alternative inside them, and no `VENUE:SYMBOL:INTERVAL` can spell one.
    /// It is the single likeliest thing to type after using `fetch`.
    #[test]
    fn a_read_verb_refuses_a_fetch_shaped_spec_and_says_why() {
        let err = parse_of(&["list", "binance:BTCUSDT:1h"]).unwrap_err();
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

        let by_name = f(&["list", "--name", "btc"]);
        assert!(by_name.matches(Some("bar"), "binance", "BTCUSDT"), "case-insensitive substring");
        assert!(!by_name.matches(Some("bar"), "binance", "ETHUSDT"));
        assert!(!by_name.is_empty());

        // A GROUPED series' label is its group; `--name fam` must reach it.
        let grouped = f(&["list", "--name", "FAM"]);
        assert!(grouped.matches(Some("trade"), "polymarket", "fam"));

        // ANDed: every named dimension has to agree.
        let both = f(&["list", "--kind", "bar", "--venue", "okx"]);
        assert!(both.matches(Some("bar"), "okx", "BTC-USDT"));
        assert!(!both.matches(Some("trade"), "okx", "BTC-USDT"));
        assert!(!both.matches(Some("bar"), "binance", "BTCUSDT"));

        // A row with NO kind dimension (the `coverage` caller) passes the kind test — and `--kind`
        // cannot reach that path at all, because `parse` refuses it there.
        assert!(f(&["list", "--kind", "bar"]).matches(None, "binance", "BTCUSDT"));
    }

    // ---- the `list` rendering ----

    /// ⚠ The identity is rendered as COLUMNS, never as a colon-string: `kind` and a `SCOPE` cell
    /// are their own cells, so a grouped series reads as a group rather than as a venue with two
    /// empties after it.
    #[test]
    fn list_lines_render_four_dimensions_and_never_a_colon_string() {
        let rows = [bar_row(), grouped_row()];
        let lines = list_lines(&rows, 2, false, false);

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
        let lines = list_lines(&[bar_row()], 9, true, false);
        assert_eq!(lines.last().unwrap(), "1 of 9 series · 48 rows");
    }

    /// A zero-row series renders `-` for its span rather than a well-formed `1970-01-01` folded
    /// from an all-zero coverage — while [`list_json`] keeps the store's own numbers untouched.
    #[test]
    fn a_zero_row_series_shows_a_dash_span_and_the_document_still_carries_the_zeroes() {
        let mut row = bar_row();
        row.coverage = Coverage::default();
        let lines = list_lines(std::slice::from_ref(&row), 1, false, false);
        assert!(!lines[1].contains("1970"), "a sentinel span is not a date: {}", lines[1]);
        let cells: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(&cells[cells.len() - 2..], ["-", "-"], "both span cells: {}", lines[1]);

        let args = parse_of(&["list", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, std::slice::from_ref(&row), 1)).unwrap();
        assert_eq!(doc["series"][0]["coverage"]["first_ts"], 0, "the document is not edited");
        assert_eq!(doc["series"][0]["coverage"]["rows"], 0);
    }

    /// All THREE gap outcomes are said out loud. An unasked question, a clean series and a probe
    /// that failed must not share a rendering — the middle one is what an operator typed `--gaps`
    /// to learn.
    #[test]
    fn gap_lines_distinguish_holes_from_none_from_unanswerable() {
        let mut row = bar_row();
        assert!(gap_lines(&row).is_empty(), "no --gaps means no annotation at all");

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

        let lines = list_lines(&[broken, clean], 2, false, true);
        assert!(lines.iter().any(|l| l.contains("gaps unavailable")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("no gaps")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row itself survives: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("fam")), "{lines:?}");
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
        let args = parse_of(&["list", "--venue", "poly", "--gaps", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, &[per_symbol, grouped], 5)).unwrap();

        assert_eq!(doc["subcommand"], "list");
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
        assert!(per_symbol["gaps"].as_array().expect("--gaps was asked for").is_empty());

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
        let args = parse_of(&["list", "--json"]).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
        assert!(doc["series"][0]["gaps"].is_null());
        assert!(doc["series"][0]["gaps_error"].is_null());
        assert_eq!(doc["gaps_requested"], false);
    }

    /// The EMPTY answer's two causes are told apart, in both verbs' nouns.
    #[test]
    fn the_empty_answer_separates_an_empty_store_from_an_over_narrow_filter() {
        assert_eq!(list_lines(&[], 0, false, false), vec!["the datahub reported no series at all"]);
        assert_eq!(
            list_lines(&[], 12, true, false),
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

        let args = parse_of(&["coverage", "--json"]).unwrap();
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
        let args = parse_of(&["coverage", "--partial-only", "--json"]).unwrap();
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

    /// The usage roster names every subcommand and every flag the parser accepts — `crate::cmd::
    /// mcp`'s `the_instructions_name_only_real_commands` reads this text to check the MCP
    /// surface's `vike-cli data …` mentions, so a subcommand missing from it makes that check
    /// unable to confirm a command that does exist.
    #[test]
    fn the_usage_names_every_subcommand_and_flag_this_parser_accepts() {
        for token in [
            "fetch",
            "seed-demo",
            "list",
            "coverage",
            "--days",
            "--from",
            "--to",
            "--store",
            "--engine",
            "--addr",
            "--kind",
            "--venue",
            "--name",
            "--gaps",
            "--partial-only",
            "--json",
        ] {
            assert!(USAGE.contains(token), "USAGE must name {token}");
        }
        assert!(USAGE.contains(DEFAULT_ADDR), "…and the datahub default it resolves to");
    }
}
