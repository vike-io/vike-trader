//! What every `data` group shares: the datahub dial, the `--source`/`--format` axes, the cells.
use vike_datahub_client::DatahubClient;
use vike_model::time::epoch_ms_to_utc_date;
use vike_node_proto::auth::{NodeKeys, Scope};

use super::hist::{InstrumentRow, SeriesRow};
use crate::exit::{CliError, CmdResult};

/// The default datahub listen address, and the same value `crate::cmd::backtest`,
/// `crate::cmd::walkforward` and `crate::cmd::mcp` each spell for themselves. A private copy per
/// verb rather than a shared `pub(crate)` one is this crate's standing convention for it: each
/// verb owns the default it documents in its own `USAGE`, and the value mirrors
/// `VIKE_DATAHUB_ADDR`'s default in the server bin. (There were five; the `sweep` verb was
/// deleted by ruling 13 — the count is gone with it rather than decremented, which is the shape
/// of claim this repository has watched rot.)
pub(super) const DEFAULT_ADDR: &str = "127.0.0.1:7878";

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
pub(super) enum Source {
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
    pub(super) fn engine_verb(self) -> Option<&'static str> {
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
pub(super) const UNBUILT_SOURCES: &[(&str, &str)] = &[
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
pub(super) fn parse_source(value: &str) -> Result<Source, String> {
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
/// file has watched that happen — and `crates/vike-ops/tests/docs/unrun_command_gate.rs` requires a
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
pub(super) enum Format {
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
pub(super) const UNBUILT_FORMATS: &[(&str, &str)] = &[];

/// **The verb that writes ROWS TO A FILE — ONE spelling, for [`ROW_VERB`]'s reason.**
///
/// `csv` and `parquet` are both refused by [`parse_format`] with this verb in the sentence, and
/// `get`'s own `UNSERVED_RENDERS` names it too. Three copies of a verb name is exactly the drift
/// [`ROW_VERB`] exists to have prevented once already.
pub(super) const FILE_VERB: &str = "`vike-cli data hist export --out FILE`";

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
pub(super) const ROW_VERB: &str = "`vike-cli data hist get`";

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
pub(super) fn parse_format(value: &str) -> Result<Format, String> {
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
        //
        // ⚠ It said `parquet` came "from a store on this machine" until 2026-09-26, when `export`
        // stopped reading one (decision 0084's amendment): BOTH of its routes read through a
        // datahub now and differ only in who writes the file. An operator told otherwise here
        // would type `--store` on `export` and be refused — the second copy of the sentence
        // [`export::parquet_refusal`] had already corrected.
        "csv" | "parquet" => Err(format!(
            "`--format {value}` is a FILE format, and this verb PRINTS — it renders a catalog to \
             a terminal or a pipe, which is what `table` and `json` are for. {FILE_VERB} writes \
             both, and both read through a datahub: `parquet` through the ENGINE route (no \
             --addr; the bars come from the datahub the engine's settings name), `csv` from the \
             datahub at --addr. For rows on stdout, {ROW_VERB} serves `jsonl`."
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
pub(crate) fn connect(
    addr: &str,
    keys: Option<&NodeKeys>,
    scope: Scope,
) -> CmdResult<DatahubClient> {
    match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, scope),
        None => DatahubClient::connect(addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to datahub at {addr}: {e}")))
}

// ─── `get`: the rows themselves ─────────────────────────────────────────────────────────────────

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
pub(super) const CLASS_AS_OF_TS: i64 = i64::MAX;

/// A column's width: the widest cell, never narrower than its own header. Same shape
/// `crate::cmd::trade::status`'s `registry_lines` uses, so a long venue slug widens its column
/// instead of shearing the row.
pub(super) fn col(header: &str, cells: impl Iterator<Item = usize>) -> usize {
    cells.chain([header.len()]).max().unwrap_or(0)
}

/// `symbol` or `group` — which of the two alternatives this row's NAME actually is. It is a column
/// rather than a decoration on the name, because a reader has to be able to tell them apart at a
/// glance and a grouped series' raw symbol is empty.
pub(super) fn scope_cell(grouped: bool) -> &'static str {
    if grouped { "group" } else { "symbol" }
}

/// The bar step, or `-` for a tick-shaped kind that genuinely has none.
pub(super) fn interval_cell(row: &SeriesRow) -> String {
    row.interval.clone().unwrap_or_else(|| "-".to_string())
}

/// A span endpoint as a UTC date, or `-` when the series holds no rows at all — see [`list_lines`]
/// for why a zero-row series may not be rendered as `1970-01-01`.
pub(super) fn span_cell(ts: i64, rows: u64) -> String {
    if rows == 0 { "-".to_string() } else { epoch_ms_to_utc_date(ts) }
}

/// An instrument's recorded kinds with their day counts, e.g. `trade:180, depth:177`. Absent kinds
/// are absent rather than shown as zero — the wire type keeps "never recorded" distinct from
/// "recorded with holes", and flattening the two here would undo that on the way to the terminal.
pub(super) fn kinds_cell(row: &InstrumentRow) -> String {
    row.kinds.iter().map(|k| format!("{}:{}", k.kind, k.days)).collect::<Vec<_>>().join(", ")
}

/// The EMPTY answer, whose two causes an operator must be able to tell apart: a server that
/// reported nothing, and a filter that selected nothing out of what it did report.
///
/// This is the same class of distinction `crate::cmd::secrets`' absent-vs-unreadable store draws —
/// one is the ordinary unconfigured state, the other is a thing you typed — and collapsing them
/// into a bare "nothing found" sends somebody to check the wrong end of the pipe.
pub(super) fn empty_note(noun: &str, reported: usize, narrowed: bool) -> String {
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
// The authority for what each key does is `crates/vike-backtest/src/harness/profile/data_cfg.rs`'s
// `DataCfg` — `explain`, `require_coverage`, `max_gap`, `on_gap`, `universe` — and the
// pre-flight that applies them is `crates/vike-backtest/src/data_plan/verdict.rs`'s `enforce`.
// Nothing here re-implements either.
