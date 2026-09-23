//! `vike-cli data hist export SPEC --out FILE --addr H:P --format jsonl|csv [--kind K]
//! [--window SPAN] [--from L] [--to L]` — ROWS OUT OF A **REMOTE** STORE, the surface design's §7
//! and §11's P4 (`docs/superpowers/specs/2026-09-20-cli-data-surface-design.md`).
//!
//! # The hole this fills, which is not the one [`super::get`] filled
//!
//! `get` shows you a PRICE: a bounded peek, a thousand rows at most, refused above its ceiling by
//! name. It is deliberately not an extractor — §8.2 says so and [`super::get::ROW_CEILING`]'s doc
//! repeats it, naming `export` as the verb bulk extraction belongs to. And `export` could not do
//! it: it spawns the ENGINE against a store on THIS machine, so a store that lives on a datahub —
//! which is every store this CLI otherwise talks to — had **no bulk extraction path at all**.
//!
//! ⚠ **`--addr` on `export` was REFUSED, not silently ignored, and the distinction is worth
//! carrying because the starting point was better than it looks.** It fell under
//! [`crate::cmd::data`]'s blanket write-half rule — *"that flag belongs to the READ half
//! (`ls`/`gaps`/`coverage`) … while `fetch --source starter|demo` drives the engine against a
//! store on this machine"* — so an operator who typed it got a clear no. What the no was WRONG
//! about is the reason: the flag does not belong to the read half, it belongs to a route this verb
//! did not have. `--kind` sat in the same list for the same reason. Both left it when this module
//! shipped, and only for `export`.
//!
//! # THE TWO ROUTES, and the one flag that chooses
//!
//! ```text
//! export SPEC --out F                      LOCAL: spawn the engine, write Parquet. Unchanged.
//! export SPEC --out F --addr H:P --format  REMOTE: walk the wire, write rows. This module.
//! ```
//!
//! ⚠ **`--addr` is the switch, and it is the EXPLICIT one** — `crate::cmd::data::Args::addr` is
//! always resolved to a default, so `addr_given` is the field that answers "did the operator ask
//! for the remote route". `rm` already turns on exactly that distinction, for exactly that reason.
//!
//! # ⚠ THE WALK — why a bulk pull over this wire is WINDOWED and not one request
//!
//! `Request::LoadBars` / `ScanQuotes` / `ScanTrades` each answer in **ONE frame**, and
//! `vike_datahub_client::proto::write_frame` refuses a body above `MAX_FRAME_LEN` (64 MiB) on the
//! SERVER side — so an unbounded scan of a busy trade tape does not stream, it fails. This module
//! therefore splits the range into fixed wall-clock windows, asks for one, appends it to `--out`,
//! and repeats. Memory is bounded by one window on both ends, no row is held that has been
//! written, and **nothing on the wire changed**: no variant, no `PROTO_VERSION` bump, no capability
//! string, so this works against a datahub that predates it.
//!
//! ⚠ **The window is WALL-CLOCK, so it cannot bound ROWS**, and pretending otherwise would be the
//! dishonest half. A day of `1d` bars is one row; a day of BTCUSDT prints can be millions. So
//! [`DEFAULT_WINDOW`] is per-KIND, [`WINDOW_FLAG`] lowers it, and a window that still overruns the
//! frame comes back as the transport's own error — which [`read_failed_note`] wraps with the flag
//! that fixes it rather than leaving an operator to read a byte count.
//!
//! # ⚠ §8.2 SAID A LARGE PULL NEVER CROSSES THIS SOCKET, AND THAT SENTENCE IS AMENDED BY THIS WORK
//!
//! It reads: *"Bulk extraction is `export`'s job, and `export` is where a server-side arm belongs
//! (§11, P4) precisely so that a large pull never crosses this socket."* The clause is the TAIL of
//! a paragraph whose own premise is **"printing is not compute"**, and that premise is what
//! survives: what the compute-to-data rule forbids is the CLIENT folding a raw upstream slice, and
//! a byte copy folds nothing. A windowed walk is bounded memory on both ends — strictly more
//! bounded than the single unbounded frame the same socket already serves `get` — so the sentence's
//! REASON does not reach it. What a server-side arm still buys is *encoding* (Parquet, which the
//! client must not link) and *one round trip instead of N*, neither of which is a cost guard. §8.2
//! is amended in the spec rather than contradicted in silence.
//!
//! # ⚠ `--format parquet` over `--addr` IS NOT BUILT, and the measurement is the reason
//!
//! `DataFusionHist::export_bars_parquet` exists and would be the body of a streaming arm. The
//! server cannot reach it: `crates/vike-datahub/src/server.rs` holds an
//! `Arc<dyn HistStore + Send + Sync>` and says of itself that it is BACKEND-AGNOSTIC, while that
//! method is an INHERENT method of the concrete `DataFusionHist` and is not on the `HistStore`
//! trait. Closing that is a layering decision in `vike-data`/`vike-datahub` — put a Parquet encoder
//! on the trait every implementer must answer (including the DataFusion-free doubles), or thread a
//! second `serve-datafusion`-gated store handle through `serve`/`handle_connection`/
//! `handle_request`, which today have no such split at all — plus this wire's FIRST chunked
//! response. That is not this verb's change. So `--format parquet --addr` is refused BY NAME, with
//! the route that does write Parquet ([`parquet_refusal`]).
//!
//! # ⚠ IT SERVES THREE KINDS OF THIRTEEN, and the three WERE not a subset anyone chose
//!
//! [`Kind`] is `bar` | `quote` | `trade`. ⚠ **That used to be exactly the wire's row-reading
//! surface, and since `docs/decisions/0084-only-the-datahub-touches-the-store.md` it is not.**
//! The datahub now answers six more reads with rows — `scan_book_updates`, `scan_depth`,
//! `scan_cohort`, `scan_perp_metrics`, `scan_equity`, `scan_exec_fills` — and this enum did not
//! grow with them. So the bound this verb reports MOVED, from the protocol to this route: a
//! `book` or a `depth` lane is refused here because no arm reads it, not because no verb exists.
//! Which bound it is decides what the reader does next, so [`unserved_kind_refusal`] now says so
//! outright. An ACCOUNT kind is refused
//! FIRST and with §9.3.2's shared sentence, for [`super::get`]'s reason: "this plane does not serve
//! your fills" is a fact about the PLANE and outranks a fact about this verb's shape.
//!
//! # The two file formats, and the three CSV decisions this verb had to make
//!
//! `crate::cmd::data`'s `UNBUILT_FORMATS` refused `csv` everywhere with *"nothing in this workspace
//! writes one"*, and `get`'s own `UNSERVED_RENDERS` named the three decisions missing: **a header
//! row, a quoting rule and a null spelling**. [`Wire::Csv`] makes all three — see [`csv_line`] and
//! [`csv_field`], each of which argues its own — so that refusal one module up now names this verb
//! instead of a phase.

use serde_json::{Value, json};
use vike_model::time::epoch_ms_to_utc_timestamp;
use vike_model::time::{Span, parse_span};
use vike_model::{Bar, MS_PER_DAY, QuoteTick, TradeTick};

/// The flag that lowers [`DEFAULT_WINDOW`] — ONE spelling, rendered by every message that names it.
///
/// `get`'s `ROW_VERB` const exists because two copies of a verb name drifted within one session;
/// this flag is named by four refusals and one note, so it gets the same treatment before rather
/// than after.
pub(super) const WINDOW_FLAG: &str = "--window";

// ─── the KIND ────────────────────────────────────────────────────────────────────────────────────

/// WHICH row shape to pull. Each variant maps to exactly one
/// `vike_datahub_client::proto::Request`.
///
/// ⚠ **This roster WAS the wire's whole row-reading surface and is now a SUBSET of it.** It held
/// three variants because three were all the wire answered with rows; since
/// `docs/decisions/0084-only-the-datahub-touches-the-store.md` the wire answers six more, and
/// this enum did not follow. Widening it is therefore a match arm HERE now — plus a [`Wire`] row
/// shape per kind — where it used to be a wire change, and [`unserved_kind_refusal`] reports the
/// bound accordingly. Nothing about the three that are here changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Derived OHLCV — `Request::LoadBars`. The DEFAULT, because it is what the LOCAL route's
    /// Parquet export writes and a route switch should not silently change WHAT is exported.
    Bar,
    /// L1 quotes — `Request::ScanQuotes`.
    Quote,
    /// Executed prints — `Request::ScanTrades`.
    Trade,
}

impl Kind {
    /// Every kind, in the order a refusal lists them. The ROSTER — nothing below re-types one.
    pub(super) const ALL: [Kind; 3] = [Kind::Bar, Kind::Quote, Kind::Trade];

    /// The `--kind` word, which is also the store's own `kind=` partition spelling.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Kind::Bar => "bar",
            Kind::Quote => "quote",
            Kind::Trade => "trade",
        }
    }

    /// `true` when the spec's THIRD part is a dimension this kind has.
    ///
    /// ⚠ **Only bars have one, and that is a fact about the STORE rather than a simplification
    /// here.** A bar series partitions on `interval=`; a quote tape and a trade tape do not, and
    /// `Request::ScanQuotes`/`ScanTrades` carry no interval field to send one in. So a
    /// `VENUE:SYMBOL:1h` on `--kind trade` names a dimension that would be silently dropped — the
    /// shape this plane refuses everywhere else — and [`parse_spec`] refuses it by name instead.
    pub(super) fn takes_an_interval(self) -> bool {
        matches!(self, Kind::Bar)
    }

    /// The spec shape this kind needs, as an operator would be told to type it.
    fn spec_shape(self) -> &'static str {
        if self.takes_an_interval() { "VENUE:SYMBOL:INTERVAL" } else { "VENUE:SYMBOL" }
    }

    /// The default wall-clock step of the walk, in epoch-ms — see [`DEFAULT_WINDOW`].
    pub(super) fn default_window_ms(self) -> i64 {
        match self {
            Kind::Bar => DEFAULT_WINDOW.bar,
            Kind::Quote | Kind::Trade => DEFAULT_WINDOW.tick,
        }
    }

    /// The column roster's names, in order — the CSV header, and the only place it comes from.
    pub(super) fn columns(self) -> Vec<&'static str> {
        match self {
            Kind::Bar => BAR_COLUMNS.iter().map(|(n, _)| *n).collect(),
            Kind::Quote => QUOTE_COLUMNS.iter().map(|(n, _)| *n).collect(),
            Kind::Trade => TRADE_COLUMNS.iter().map(|(n, _)| *n).collect(),
        }
    }
}

/// The per-kind default step of the walk.
///
/// ⚠ **Two numbers rather than one, because the row DENSITY of the two lanes differs by orders of
/// magnitude and a single default would be wrong for one of them in the expensive direction.** A
/// 30-day window of `1h` bars is 720 rows; 30 days of a busy trade tape would not fit in a frame at
/// all. Neither number is a guarantee — the step is wall-clock and the frame cap is bytes, which is
/// the residual this module's doc states — they are the sizes at which the common case does not
/// need [`WINDOW_FLAG`].
pub(super) struct WindowDefaults {
    /// `bar`: 30 days. A minute tape over 30 days is ~43k rows, comfortably inside one frame, and
    /// a daily tape over 30 days is 30 — so this is one request for most bar ranges anyone asks
    /// for.
    pub(super) bar: i64,
    /// `quote`/`trade`: ONE day. The tick lanes are where a wide window overruns a frame soonest,
    /// and a day is the partition the store itself is laid out in (`date=`), so a window is a
    /// partition-shaped read rather than one that straddles.
    pub(super) tick: i64,
}

/// The two defaults, as a const so both are stated in one place with their arguments.
pub(super) const DEFAULT_WINDOW: WindowDefaults =
    WindowDefaults { bar: 30 * MS_PER_DAY, tick: MS_PER_DAY };

/// Resolve `--kind`, or refuse it with the sentence that fits which of the three refusals it is.
///
/// ⚠ **The ORDER of the three is the design.** An ACCOUNT kind meets §9.3.2's plane sentence
/// first — `super::refuse_an_account_kind_on_a_read`, shared so an operator who meets the
/// rule twice reads it once. Everything else meets a refusal that names the wire's bound. A BLANK
/// value is its own arm, as it is for `--format` and `--source`, because "unknown kind ''" tells an
/// operator their spelling is wrong when what happened is that a shell expanded nothing.
///
/// # Errors
///
/// An account kind (§9.3.2), a blank value, or a market kind this wire has no read verb for.
pub(super) fn parse_kind(raw: Option<&str>) -> Result<Kind, String> {
    let Some(raw) = raw else { return Ok(Kind::Bar) };
    let trimmed = raw.trim();
    super::refuse_an_account_kind_on_a_read(trimmed)?;
    if trimmed.is_empty() {
        return Err(format!(
            "--kind was given an EMPTY value. Name one of {}, or omit the flag for `{}`.",
            served_kinds(),
            Kind::Bar.as_str()
        ));
    }
    Kind::ALL
        .into_iter()
        .find(|k| k.as_str() == trimmed)
        .ok_or_else(|| unserved_kind_refusal(trimmed))
}

/// The three served kinds, rendered from [`Kind::ALL`] — never typed out in a message.
fn served_kinds() -> String {
    Kind::ALL.map(Kind::as_str).join(" | ")
}

/// What a store kind this ROUTE cannot read is told, and what it is pointed at instead.
///
/// ⚠ **It stated the bound as the PROTOCOL's, and that stopped being true.** The reasoning was
/// sound and is kept: the two phrasings imply different next steps, so the message says which
/// bound it is rather than leaving the reader to guess. What changed is the answer. Since
/// `docs/decisions/0084-only-the-datahub-touches-the-store.md` the wire answers six more reads
/// with rows, so the honest sentence points at THIS route — and the old direction is the
/// expensive way to be wrong: an operator told the wire has no verb for books would file work
/// that has already shipped, which is precisely the "go around the server" behaviour 0084 exists
/// to stop. The store genuinely HOLDS the kind — `data hist ls --kind K` will show it — so the
/// message still names the verb that proves the rows are there.
/// The MARKET store kinds the datahub now reads over the wire that this route has no `--kind` arm
/// for — the set that makes [`unserved_kind_refusal`] able to tell a missing ARM from a missing
/// VERB.
///
/// ⚠ **A hand copy, declared as one.** There is nothing to derive it from: [`Kind`] is this
/// route's roster and `crates/vike-datahub/src/server.rs`'s `served_features` is the server's, and
/// no shared table maps a STORE kind string to a wire verb. The four account kinds
/// (`vike_model::ACCOUNT_KINDS`) never reach here — `super::refuse_an_account_kind_on_a_read` takes
/// them first — so this list plus [`Kind::ALL`] plus `chain`/`properties` is the whole market side
/// of `vike_data::store_kind::STORE_KINDS`, which is what
/// `the_refusal_distinguishes_a_missing_arm_from_a_missing_verb` checks. Being wrong here costs a
/// misleading sentence rather than a wrong answer; the day a shared map exists, read from it.
const WIRE_READS_THIS_ROUTE_LACKS: &[&str] = &["book", "depth", "cohort", "perp_metrics"];

fn unserved_kind_refusal(kind: &str) -> String {
    // ⚠ TWO refusals, not one, because the two imply different work. Collapsing them was the defect
    // this function acquired the day the wire grew: one sentence told a `book` and a `properties`
    // reader the same thing, and exactly one of them was true.
    let where_the_gap_is = if WIRE_READS_THIS_ROUTE_LACKS.contains(&kind) {
        "The bound is THIS ROUTE's, not the wire's: the datahub DOES answer this kind with rows, \
         so what is missing is a `--kind` arm here rather than a protocol verb."
    } else {
        "Neither this route nor the datahub's wire reads this kind: there is no read verb for it \
         at all, so the wire is what has to change first."
    };
    format!(
        "`--kind {kind}` is not a row shape this route can read. A remote export walks three of \
         the datahub's row verbs — {} — and has an arm for no others. {where_the_gap_is} \
         `vike-cli data hist ls --kind {kind}` shows what the store holds of it.",
        served_kinds()
    )
}

// ─── the SPEC ────────────────────────────────────────────────────────────────────────────────────

/// The series to pull: one venue, one symbol, and an interval IF the kind has one.
///
/// ⚠ **A THIRD spec type in this plane, and the arity is the reason.** `super::get::Spec` is three
/// mandatory parts because it addresses a BAR series and nothing else; `super::gate::Spec` is a
/// SELECTOR over what a store already holds. This one is an ADDRESS like `get`'s, but its arity is
/// a function of `--kind` — which is precisely the thing neither sibling has to express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Spec {
    pub(super) venue: String,
    pub(super) symbol: String,
    /// `Some` for a bar series, `None` for the tick lanes. See [`Kind::takes_an_interval`].
    pub(super) interval: Option<String>,
}

impl Spec {
    /// The spec as the operator would have typed it, rebuilt from the parsed parts — `get::Spec`'s
    /// reason: a header line and a `--json` document must not disagree about a trimmed part.
    pub(super) fn text(&self) -> String {
        match &self.interval {
            Some(i) => format!("{}:{}:{}", self.venue, self.symbol, i),
            None => format!("{}:{}", self.venue, self.symbol),
        }
    }
}

/// Parse the positional spec FOR THIS KIND, or refuse it by naming the shape that kind takes.
///
/// ⚠ **A wrong-arity spec is refused with the KIND in the sentence**, never with a generic "want
/// VENUE:SYMBOL:INTERVAL". The operator typed an interval because every other spec in this plane
/// has one, and the fact they need is that their kind does not — `super::check_spec`'s message
/// cannot say that, which is why this verb parses its own.
///
/// # Errors
///
/// An empty part, the wrong number of parts for `kind`, or an interval on a kind that has none.
pub(super) fn parse_spec(raw: &str, kind: Kind) -> Result<Spec, String> {
    let parts: Vec<&str> = raw.split(':').map(str::trim).collect();
    let want = kind.spec_shape();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!(
            "'{raw}': every part of a spec must be non-empty — `--kind {}` wants {want}",
            kind.as_str()
        ));
    }
    match (parts.len(), kind.takes_an_interval()) {
        (3, true) => Ok(Spec {
            venue: parts[0].to_string(),
            symbol: parts[1].to_string(),
            interval: Some(parts[2].to_string()),
        }),
        (2, false) => {
            Ok(Spec { venue: parts[0].to_string(), symbol: parts[1].to_string(), interval: None })
        }
        // ⚠ The interesting refusal, and the one this parser exists for: a THREE-part spec on a
        // tick kind. The third part is not merely surplus — the store has no `interval=` dimension
        // for these kinds and the wire carries no field to send it in, so accepting it would drop
        // it in silence.
        (3, false) => Err(format!(
            "'{raw}': `--kind {}` has no INTERVAL — a {} series partitions on (venue, symbol) and \
             nothing else, and the read verb carries no interval to send, so '{}' would be \
             dropped in silence. Write it as {want}.",
            kind.as_str(),
            kind.as_str(),
            parts[2]
        )),
        (2, true) => Err(format!(
            "'{raw}': `--kind {}` needs an INTERVAL — a bar series is addressed by \
             (venue, symbol, interval) and there is no default step a store could supply. \
             Write it as {want}.",
            kind.as_str()
        )),
        _ => Err(format!("'{raw}': not a spec — `--kind {}` wants {want}", kind.as_str())),
    }
}

// ─── the FILE FORMAT ─────────────────────────────────────────────────────────────────────────────

/// What `--out` RECEIVES on the remote route.
///
/// ⚠ **On `export`, `--format` names the FILE and not the terminal**, which is §7's own grammar
/// (`export SPEC --out FILE [--format parquet|csv|jsonl]`) and is a CORRECTION: this flag used to
/// reach `crate::cmd::data::parse_format` here, where it meant `table`|`json` — the rendering of
/// the REPORT this verb prints about what it wrote. Two axes wearing one flag on one verb is how
/// an operator comes to believe `--format json` changed the file. The report keeps `--json`, which
/// is what it always was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wire {
    /// One JSON object per row, newline-separated. The same row shape [`super::get`]'s `jsonl`
    /// emits — pinned equal by a test, so the two verbs cannot describe one bar differently.
    Jsonl,
    /// RFC-4180-shaped CSV: one header line, one line per row. See [`csv_field`] for the quoting
    /// rule and [`csv_line`] for the null spelling.
    Csv,
}

impl Wire {
    /// The `--format` word.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Wire::Jsonl => "jsonl",
            Wire::Csv => "csv",
        }
    }

    /// Every format this route serves, in the order a refusal lists them. The ROSTER.
    pub(super) const ALL: [Wire; 2] = [Wire::Jsonl, Wire::Csv];
}

/// The two served formats, rendered from [`Wire::ALL`] — never typed out in a message.
fn served_formats() -> String {
    Wire::ALL.map(Wire::as_str).join(" | ")
}

/// Resolve `--format` for the REMOTE route, or refuse it.
///
/// ⚠ **It is REQUIRED here and defaulted on the local route, which is deliberate rather than an
/// inconsistency.** The local route writes Parquet because that is what the engine writes; this
/// route cannot write Parquet at all ([`parquet_refusal`]), so there is no format to inherit and a
/// default would have to be invented. Making the operator name it is also what keeps `--out
/// btc.csv` from being read as a request — this verb never guesses a format from a file extension,
/// because a name is not a declaration.
///
/// # Errors
///
/// An absent flag, a blank value, `parquet` (unbuilt here, by measurement), a terminal rendering
/// (`table`/`json`, which name the REPORT and not the file), or an unknown word.
pub(super) fn parse_wire(raw: Option<&str>) -> Result<Wire, String> {
    let Some(raw) = raw else {
        return Err(format!(
            "a remote export needs --format: --addr writes ROWS to --out, and the two forms this \
             route serves are {}. The LOCAL route (drop --addr) writes Parquet, which is where \
             its default comes from — there is nothing for this route to inherit, and guessing \
             from the --out file extension would be reading a name as a declaration.",
            served_formats()
        ));
    };
    let trimmed = raw.trim();
    if let Some(w) = Wire::ALL.into_iter().find(|w| w.as_str() == trimmed) {
        return Ok(w);
    }
    match trimmed {
        "" => Err(format!(
            "--format was given an EMPTY value. Name {} — the two forms a remote export writes.",
            served_formats()
        )),
        "parquet" => Err(parquet_refusal()),
        // ⚠ Refused BY NAME rather than silently accepted, and the reason is that both USED to
        // parse here and meant something else. An operator carrying the old spelling gets the axis
        // correction, not "unknown format".
        "table" | "json" => Err(format!(
            "`--format {trimmed}` is a TERMINAL rendering, and on `export` this flag names the \
             FILE that --out receives — §7's own grammar. The report this verb prints about what \
             it wrote is `--json`, which is unchanged. For the file, name {}.",
            served_formats()
        )),
        other => Err(format!(
            "unknown `--format {other}` on a remote export ({}). `parquet` is the LOCAL route's \
             format — drop --addr.",
            served_formats()
        )),
    }
}

/// `--format` on the LOCAL route, where the only file this verb writes is Parquet.
///
/// ⚠ **`parquet` is accepted and does nothing, and that is not the same as being ignored.** It
/// NAMES what this route writes, so an operator who spells it gets the file they asked for; the
/// two remote forms are refused with the flag that reaches them, which is the fact they are
/// missing. Silently accepting `--format csv` here and writing Parquet is the shape this plane
/// refuses everywhere else — and it is what `--addr` itself did on this verb until now.
///
/// # Errors
///
/// A remote-only file format, a terminal rendering (which names the REPORT, not the file), a blank
/// value, or an unknown word.
pub(super) fn refuse_a_wire_on_the_local_route(raw: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed == "parquet" {
        return Ok(());
    }
    if Wire::ALL.into_iter().any(|w| w.as_str() == trimmed) {
        return Err(format!(
            "`--format {trimmed}` is written by the REMOTE route: it streams ROWS out of a \
             datahub's store, while this route spawns the engine against a store on THIS machine \
             and the engine writes Parquet. Add --addr HOST:PORT to take the remote route, or \
             name `parquet`."
        ));
    }
    match trimmed {
        "" => Err("--format was given an EMPTY value. On `export` it names the FILE --out \
                   receives: `parquet` here, or `jsonl`/`csv` with --addr."
            .to_string()),
        "table" | "json" => Err(format!(
            "`--format {trimmed}` is a TERMINAL rendering, and on `export` this flag names the \
             FILE that --out receives — §7's own grammar. The report this verb prints about what \
             it wrote is `--json`, which is unchanged. This route writes `parquet`."
        )),
        other => Err(format!(
            "unknown `--format {other}` on `export`. This route writes `parquet`; {} are the \
             remote route's, reached with --addr.",
            served_formats()
        )),
    }
}

/// Why `--format parquet` is refused over `--addr`, and what to do instead.
///
/// ⚠ **It states the MEASUREMENT rather than calling the feature unbuilt**, because the two lead
/// somewhere different: "not built yet" invites waiting for a phase, while the actual blocker is
/// that the encoder is not reachable from the server's store handle at all. An operator who wants
/// Parquet today has a working command line — the local route — and it is in the message.
pub(super) fn parquet_refusal() -> String {
    format!(
        "`--format parquet` is not served over --addr. The encoder \
         (`DataFusionHist::export_bars_parquet`) is an INHERENT method of the concrete store, \
         while `crates/vike-datahub/src/server.rs` holds an `Arc<dyn HistStore>` and is \
         backend-agnostic by design — so there is no server arm to drive it, and adding one is a \
         layering change plus this wire's first chunked response rather than a flag. \
         Two things work today: drop --addr to write Parquet from a store on THIS machine, or \
         name {} to stream rows from the remote one.",
        served_formats()
    )
}

// ─── the WALK ────────────────────────────────────────────────────────────────────────────────────

/// Resolve `--window SPAN`, or refuse it.
///
/// ⚠ **No new duration parser** — `vike_model::time::parse_span` is this workspace's grammar for
/// `4h` / `1d` / `2w`, and `super::gate::parse_max_gap` narrows it the same way for the same
/// reason: nothing here is forwarded, so the far side never sees this flag and could not refuse it.
/// The two narrowings differ only in what they are about, and each says so at its own site.
///
/// # Errors
///
/// An unreadable span, a non-positive one (a zero-width window advances the walk by nothing and
/// would never terminate), or one of the two span shapes that is not a fixed number of
/// milliseconds.
pub(super) fn parse_window_step(raw: &str, kind: Kind) -> Result<i64, String> {
    let span = parse_span(raw).map_err(|e| format!("{WINDOW_FLAG} {raw:?}: {e}"))?;
    match span {
        Span::Ms(ms) if ms > 0 => Ok(ms),
        // Unreachable through `parse_span`, which refuses a zero count by name — kept because a
        // non-positive step is the one value that would make the walk below not terminate, and a
        // hang is the worst way to learn a flag was wrong.
        Span::Ms(ms) => Err(format!(
            "{WINDOW_FLAG} {raw:?} resolves to {ms}ms — a window of no width asks for no rows and \
             advances the walk by nothing. Name a real duration, e.g. `1d`."
        )),
        Span::Bars(n) => Err(format!(
            "{WINDOW_FLAG} {raw:?} is a BAR COUNT, and this walk steps in WALL-CLOCK time: \
             converting {n} bars to a duration needs an interval, and `--kind {}` has none. \
             Write the step as fixed time — \"1d\", \"7d\".",
            kind.as_str()
        )),
        Span::Months(_) => Err(format!(
            "{WINDOW_FLAG} {raw:?} is a CALENDAR span, and a calendar month is not a fixed number \
             of milliseconds — so the step would change width as the walk crossed a month \
             boundary, and two runs of one command would send different requests. Write it as \
             fixed time — \"30d\"."
        )),
    }
}

/// The walk: `[start, end]` split into consecutive windows of at most `step` milliseconds.
///
/// ⚠ **Both bounds are INCLUSIVE, which is the store's own meaning** (`Request::LoadBars`'s
/// `start`/`end` are documented as inclusive), so consecutive windows must not share an instant:
/// each window begins at the previous one's end **plus one millisecond**. Off by one here does not
/// fail — it DUPLICATES every row that lands exactly on a boundary, which for a `1d` bar tape
/// walked in `1d` windows is every row.
///
/// ⚠ The last window is CLAMPED to `end` rather than allowed to overshoot, so a caller can report
/// the bounds it actually asked for.
///
/// Returns an empty vec for an inverted range; `parse` refuses one before this is reached, and the
/// empty answer is the safe reading if it ever is not.
pub(super) fn windows(start: i64, end: i64, step: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if end < start || step <= 0 {
        return out;
    }
    let mut lo = start;
    loop {
        // `saturating_*` rather than `checked_*`: a window that would run past i64::MAX is clamped
        // to `end` by the `min` below, which is the correct last window rather than an error.
        let hi = lo.saturating_add(step.saturating_sub(1)).min(end);
        out.push((lo, hi));
        if hi >= end {
            return out;
        }
        lo = hi.saturating_add(1);
    }
}

/// What a FAILED window read is told, with the lever that fixes the likeliest cause.
///
/// ⚠ **The likeliest cause is not the only one, and the message says so.** A window whose rows
/// exceed `MAX_FRAME_LEN` fails on the SERVER's `write_frame` and reaches this side as a transport
/// error with a byte count in it, which reads like a network fault; a genuine network fault reads
/// identically. Naming [`WINDOW_FLAG`] as the thing to try is actionable for the first and
/// harmless for the second, and the window that failed is named so a retry can start there rather
/// than from the beginning.
pub(super) fn read_failed_note(
    spec_text: &str,
    lo: i64,
    hi: i64,
    step_ms: i64,
    why: &str,
) -> String {
    format!(
        "reading {spec_text} over [{lo}, {hi}] failed: {why}\n\
         That window is {step_ms}ms wide. A window answers in ONE frame, so one holding more rows \
         than the 64 MiB frame cap fails on the server side and arrives here looking like a \
         transport fault — lower the step with `{WINDOW_FLAG} SPAN` (e.g. `{WINDOW_FLAG} 1h`) and \
         retry from --from {lo}."
    )
}

// ─── the ROW ─────────────────────────────────────────────────────────────────────────────────────

/// One rendered column: its NAME, and its value for this row — `None` when this row does not carry
/// it.
///
/// ⚠ **One roster per kind serves BOTH file formats**, which is `super::get::OPTIONAL_COLUMNS`'s
/// rule one verb over: `jsonl` reads the name as a key and `csv` reads it as a header, so a field
/// cannot arrive in one form and be missing from the other. It is also what lets [`Kind::columns`]
/// answer for an EMPTY export, where there is no row to derive a header from.
type Column<T> = (&'static str, fn(&Spec, &T) -> Option<Value>);

/// ONE row, rendered: every column of its kind's roster, in roster order, each carrying its value
/// or the absence of one.
///
/// ⚠ **An ABSENT column is kept rather than filtered**, which is what makes one type serve both
/// formats: `csv` needs the COLUMN even when the value is missing (dropping it would shift every
/// field to its right by one for that row — the failure a CSV reader cannot detect), while `jsonl`
/// needs the ABSENCE (a key spelled `null` reads as "the venue said null"). An alias rather than
/// the type written out at each of six signatures, so `clippy::type_complexity` has nothing to say
/// and a reader has one place to learn what a cell means.
pub(super) type Cells = Vec<(&'static str, Option<Value>)>;

/// A bar's columns. The first ten are `super::get::json_row`'s, in its order and with its keys —
/// pinned equal by `the_bar_row_is_the_same_shape_get_emits`, so `get --format jsonl` and
/// `export --format jsonl` cannot describe one bar differently. The last three are its
/// `OPTIONAL_COLUMNS`, which are `None` on an OHLCV bar and `Some` on a tick-derived or funding one.
const BAR_COLUMNS: [Column<Bar>; 13] = [
    ("venue", |s: &Spec, _: &Bar| Some(json!(s.venue))),
    ("symbol", |s: &Spec, _: &Bar| Some(json!(s.symbol))),
    // Present by construction: `parse_spec` gives a bar spec an interval. `unwrap_or_default` over
    // an `expect` because a renderer is the wrong place to panic on a malformed row.
    ("interval", |s: &Spec, _: &Bar| Some(json!(s.interval.clone().unwrap_or_default()))),
    ("ts", |_: &Spec, b: &Bar| Some(json!(b.ts))),
    ("ts_utc", |_: &Spec, b: &Bar| Some(json!(epoch_ms_to_utc_timestamp(b.ts)))),
    ("open", |_: &Spec, b: &Bar| Some(json!(b.open))),
    ("high", |_: &Spec, b: &Bar| Some(json!(b.high))),
    ("low", |_: &Spec, b: &Bar| Some(json!(b.low))),
    ("close", |_: &Spec, b: &Bar| Some(json!(b.close))),
    ("volume", |_: &Spec, b: &Bar| Some(json!(b.volume))),
    ("funding", |_: &Spec, b: &Bar| b.funding.map(|v| json!(v))),
    ("bid", |_: &Spec, b: &Bar| b.bid.map(|v| json!(v))),
    ("ask", |_: &Spec, b: &Bar| b.ask.map(|v| json!(v))),
];

/// A quote's columns.
///
/// ⚠ `local_ts` IS carried, and it is the one column here a reader might think is noise. It is the
/// MACHINE RECEIVE instant of a dual-timestamp capture, and `vike_marketdata::QuoteTick`'s own doc
/// says feed latency is *"later REPLAYED, not modeled; uncapturable retroactively"* — so an export
/// that dropped it would hand somebody a tape they cannot ever reconstruct it for. `0` means not
/// stamped, which is that field's own convention and is carried through rather than blanked.
///
/// ⚠ The tick's OWN `symbol` field is NOT a column: it is *"empty for single-symbol paths"* and
/// this read is exactly one of those, so it would be an always-empty column beside the `symbol`
/// the spec already names — a field meaning "not applicable here" wearing the spelling of a value.
/// That is `super::get::json_row`'s argument for not serializing `Bar` wholesale, applied to the
/// two tick shapes.
const QUOTE_COLUMNS: [Column<QuoteTick>; 9] = [
    ("venue", |s: &Spec, _: &QuoteTick| Some(json!(s.venue))),
    ("symbol", |s: &Spec, _: &QuoteTick| Some(json!(s.symbol))),
    ("ts", |_: &Spec, q: &QuoteTick| Some(json!(q.ts))),
    ("ts_utc", |_: &Spec, q: &QuoteTick| Some(json!(epoch_ms_to_utc_timestamp(q.ts)))),
    ("local_ts", |_: &Spec, q: &QuoteTick| Some(json!(q.local_ts))),
    ("bid", |_: &Spec, q: &QuoteTick| Some(json!(q.bid))),
    ("ask", |_: &Spec, q: &QuoteTick| Some(json!(q.ask))),
    ("bid_size", |_: &Spec, q: &QuoteTick| Some(json!(q.bid_size))),
    ("ask_size", |_: &Spec, q: &QuoteTick| Some(json!(q.ask_size))),
];

/// A trade's columns. `local_ts` and the omitted `symbol` field are [`QUOTE_COLUMNS`]'s argument
/// unchanged.
///
/// ⚠ `is_buyer_maker` is the AGGRESSOR side and is a `bool` rather than a word: `true` means the
/// buyer was the resting maker, i.e. the SELLER lifted. It is rendered as JSON `true`/`false` in
/// both forms rather than as `buy`/`sell`, because naming the side is a convention this workspace
/// settles per venue and a file is the wrong place to bake one in.
const TRADE_COLUMNS: [Column<TradeTick>; 8] = [
    ("venue", |s: &Spec, _: &TradeTick| Some(json!(s.venue))),
    ("symbol", |s: &Spec, _: &TradeTick| Some(json!(s.symbol))),
    ("ts", |_: &Spec, t: &TradeTick| Some(json!(t.ts))),
    ("ts_utc", |_: &Spec, t: &TradeTick| Some(json!(epoch_ms_to_utc_timestamp(t.ts)))),
    ("local_ts", |_: &Spec, t: &TradeTick| Some(json!(t.local_ts))),
    ("price", |_: &Spec, t: &TradeTick| Some(json!(t.price))),
    ("size", |_: &Spec, t: &TradeTick| Some(json!(t.size))),
    ("is_buyer_maker", |_: &Spec, t: &TradeTick| Some(json!(t.is_buyer_maker))),
];

/// Render one row's cells against its kind's roster — the ONE fold all three kinds share, so the
/// roster is the only thing that differs between them.
fn cells<T>(roster: &[Column<T>], spec: &Spec, row: &T) -> Cells {
    roster.iter().map(|(name, get)| (*name, get(spec, row))).collect()
}

/// One bar's cells.
pub(super) fn bar_cells(spec: &Spec, bar: &Bar) -> Cells {
    cells(&BAR_COLUMNS, spec, bar)
}

/// One quote's cells.
pub(super) fn quote_cells(spec: &Spec, q: &QuoteTick) -> Cells {
    cells(&QUOTE_COLUMNS, spec, q)
}

/// One trade's cells.
pub(super) fn trade_cells(spec: &Spec, t: &TradeTick) -> Cells {
    cells(&TRADE_COLUMNS, spec, t)
}

/// One row as a JSON object — the `jsonl` line.
///
/// ⚠ **An ABSENT cell is OMITTED, never written as `null`**, which is `super::get::json_row`'s rule
/// and the reason the two are pinned equal: an `Option` field is `None` in the model *because the
/// producer did not know*, and a key spelled `null` reads as "the venue said null". A key that is
/// not there says nobody said.
pub(super) fn jsonl_line(cells: &Cells) -> String {
    let mut row = serde_json::Map::new();
    for (name, value) in cells {
        if let Some(v) = value {
            row.insert((*name).to_string(), v.clone());
        }
    }
    Value::Object(row).to_string()
}

/// One row as a CSV line — and the NULL SPELLING, which is one of the three decisions this format
/// needed.
///
/// ⚠ **An absent cell is an EMPTY FIELD, not the word `null`.** CSV has no null literal, and every
/// reader that matters (pandas, DuckDB, a spreadsheet) treats an empty field as missing while
/// `null` parses as TEXT and poisons the column's type for every row. The cost is stated rather
/// than hidden: **CSV cannot distinguish an absent value from a non-finite one**, because
/// `serde_json` renders a NaN or an infinity as `null` (its `Number::from_f64` refuses them) and
/// both land here as an empty field. `jsonl` is the form that keeps them apart — an absent KEY
/// versus a key whose value is `null` — and that is a reason to prefer it, said here rather than
/// discovered.
pub(super) fn csv_line(cells: &Cells) -> String {
    cells
        .iter()
        .map(|(_, value)| match value {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => csv_field(s),
            Some(v) => csv_field(&v.to_string()),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The CSV HEADER — the second of the three decisions: **always present, exactly once, at the top
/// of the file**, whether or not any row follows.
///
/// A headerless CSV is a file whose columns are a fact about the version of this binary that wrote
/// it, and an EMPTY export with no header is a zero-byte file indistinguishable from a failed one.
pub(super) fn csv_header(kind: Kind) -> String {
    kind.columns().iter().map(|n| csv_field(n)).collect::<Vec<_>>().join(",")
}

/// The QUOTING RULE — the third decision: **RFC 4180 minimal**. A field is quoted if and only if it
/// contains a comma, a double quote, a CR or an LF; inside quotes a `"` is doubled.
///
/// ⚠ **Minimal rather than quote-everything**, so a numeric column stays numeric to a reader that
/// infers types from the absence of quotes — which is most of them. In practice almost nothing here
/// quotes: the values are numbers, timestamps and venue/symbol identifiers. The rule exists for the
/// ones that can — a Polymarket group label, a symbol carrying punctuation — where a bare comma
/// would silently shift every column to its right for one row, the failure mode a CSV reader cannot
/// detect.
pub(super) fn csv_field(raw: &str) -> String {
    if raw.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_string()
    }
}

// ─── the PLAN ────────────────────────────────────────────────────────────────────────────────────

/// `export --addr`'s whole resolved request — WHICH rows, in WHAT file format, over WHAT range,
/// walked in steps of WHAT width.
///
/// ⚠ **It lives HERE rather than beside `crate::cmd::data`'s other per-verb arg structs**, and the
/// reason is not tidiness: every renderer below needs four or five of these fields at once, and
/// passing them individually put [`json_doc`] at nine parameters — past `clippy::too_many_arguments`,
/// which is a merge gate here. A struct the pure half owns is also what lets the walk, the two
/// renderings and the document be tested without building a whole `Args`.
///
/// Its `Some`-ness in `crate::cmd::data::Args::export` is what SELECTS the remote route, so the
/// local route can never read a half-filled one — `RmArgs`' rule, one verb over.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    /// `--kind`, defaulted to [`Kind::Bar`] — see that enum for why the roster is the WIRE's three
    /// rather than a selection from the store's thirteen.
    pub(super) kind: Kind,
    /// The series, parsed against `kind`'s arity: three parts for a bar, two for a tick lane.
    pub(super) spec: Spec,
    /// `--format`, REQUIRED here — [`parse_wire`] argues why this route cannot default it.
    pub(super) wire: Wire,
    /// `--from`/`--to`, BOTH REQUIRED and already epoch-ms.
    ///
    /// ⚠ **The one place this route is STRICTER than the local one, and it is a property of the
    /// walk rather than a preference.** A local export hands its bounds to DataFusion, which scans
    /// whatever the store holds and needs neither; a walk has to know where to take its first step
    /// and when to stop, and an unbounded side has no first or last window. The refusal names
    /// `data hist ls`, which PRINTS each series' recorded span — so the operator has a
    /// one-command path to the two numbers.
    pub(super) bounds: (i64, i64),
    /// `--window SPAN` in milliseconds, or this kind's [`Kind::default_window_ms`].
    pub(super) step_ms: i64,
    /// `true` when the step was DEFAULTED rather than named, so a disclosure can say which one was
    /// in force — `GetArgs::limit_defaulted`'s reason: telling somebody about a flag they did not
    /// use is how a message stops being read.
    pub(super) step_defaulted: bool,
}

// ─── the SUMMARY ─────────────────────────────────────────────────────────────────────────────────

/// What a completed remote export reports: the counts a caller needs to tell a real answer from an
/// empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Written {
    /// Rows written to `--out`.
    pub(super) rows: usize,
    /// Windows the walk asked for. Reported because it is the number [`WINDOW_FLAG`] moves, so an
    /// operator tuning a slow export can see the effect of the flag they just changed.
    pub(super) windows: usize,
}

/// The human summary line, and — when the answer is empty — what that DOES and does not mean.
///
/// ⚠ The empty reading is `super::get::empty_note`'s argument, and it is the same two facts: an
/// empty file means the series is absent from this store OR holds nothing in this window, and
/// nothing on the wire tells the two apart. A bare "0 rows" would let a typo'd symbol read as a gap
/// in somebody's data.
pub(super) fn summary(plan: &Plan, out: &str, w: &Written) -> Vec<String> {
    let mut lines = vec![format!(
        "wrote {} {} rows for {} to {out} ({} window{})",
        w.rows,
        plan.kind.as_str(),
        plan.spec.text(),
        w.windows,
        if w.windows == 1 { "" } else { "s" }
    )];
    if w.rows == 0 {
        // ⚠ The `jsonl` form has NO header, so the file really is zero bytes — and saying "a
        // header and no rows" there would describe a file the operator can see is empty. The two
        // formats get the sentence that is true of each.
        let what = match plan.wire {
            Wire::Csv => "that file has a header and no rows",
            Wire::Jsonl => "that file is empty",
        };
        lines.push(format!(
            "note: {what}. That is ONE of two facts and this verb cannot tell them apart: the \
             series is not in this store, or it holds nothing between those bounds. \
             `vike-cli data hist ls --venue {} --name {}` answers the first.",
            plan.spec.venue, plan.spec.symbol
        ));
    }
    lines
}

/// The note that names [`WINDOW_FLAG`] when the step was DEFAULTED and the walk was long enough
/// for the flag to be worth knowing about.
///
/// `None` otherwise — a note on every run stops being read (`super::get::ceiling_note`'s rule), and
/// a one-window export has nothing to tune.
pub(super) fn step_note(plan: &Plan, w: &Written) -> Option<String> {
    if !plan.step_defaulted || w.windows <= 1 {
        return None;
    }
    Some(format!(
        "note: the walk stepped in {}ms windows, this kind's default. `{WINDOW_FLAG} SPAN` sets \
         it — lower it if a window overruns the frame cap, raise it for fewer round trips.",
        plan.step_ms
    ))
}

/// The `--json` document for a completed remote export.
///
/// ⚠ It is NOT `crate::cmd::data::report_json`'s shape, and the divergence is the whole point: that
/// document carries `engine`, `engine_argv` and the child's stdout VERBATIM, because the local
/// route's whole product is an argv it spawned. This route spawns nothing — there is no engine, no
/// argv and no report to quote — so the fields would be four nulls and a reader could not tell a
/// remote export from a local one that failed to start.
pub(super) fn json_doc(
    subcommand: &str,
    addr: &str,
    out: &str,
    plan: &Plan,
    w: &Written,
) -> String {
    let doc = json!({
        // A PARAMETER, never a literal — `super::get::json_doc`'s correction, for its reason.
        "subcommand": subcommand,
        "route": "remote",
        "addr": addr,
        "kind": plan.kind.as_str(),
        "format": plan.wire.as_str(),
        "out": out,
        "spec": {
            "text": plan.spec.text(),
            "venue": plan.spec.venue,
            "symbol": plan.spec.symbol,
            "interval": plan.spec.interval,
        },
        "window": {
            "from_ts": plan.bounds.0,
            "to_ts": plan.bounds.1,
            "step_ms": plan.step_ms,
            // The FLAG's state, so a machine reader can tell a tuned walk from a defaulted one —
            // the same fact `step_note` carries for a person.
            "step_defaulted": plan.step_defaulted,
        },
        "rows": w.rows,
        "windows": w.windows,
        "columns": plan.kind.columns(),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64) -> Bar {
        Bar {
            ts,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn spec() -> Spec {
        Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: Some("1h".into()) }
    }

    /// A bar plan over `[10, 20]` in 5ms steps, with the step DEFAULTED — the shape the notes and
    /// the document are exercised against.
    fn plan(wire: Wire) -> Plan {
        Plan {
            kind: Kind::Bar,
            spec: spec(),
            wire,
            bounds: (10, 20),
            step_ms: 5,
            step_defaulted: true,
        }
    }

    /// The three kinds are the wire's three row verbs, and the roster is the authority for every
    /// message that lists them.
    #[test]
    fn the_kind_roster_is_the_three_the_wire_can_read() {
        assert_eq!(Kind::ALL.map(Kind::as_str), ["bar", "quote", "trade"]);
        assert_eq!(served_kinds(), "bar | quote | trade");
        // ANTI-VACUITY: the roster is not empty and the rendering is not the empty string, so a
        // future refactor that emptied `ALL` would fail here rather than render blank messages.
        assert_eq!(Kind::ALL.len(), 3);
        assert!(!served_kinds().is_empty());
    }

    /// §9.3.2 outranks this verb's own shape, and a market kind THIS ROUTE cannot read is refused
    /// with a DIFFERENT sentence — the two must not blur.
    ///
    /// ⚠ Renamed from `..._a_book_meets_the_wire_one`: the sentence a book meets is no longer
    /// about the WIRE. `docs/decisions/0084-only-the-datahub-touches-the-store.md` put book,
    /// depth, cohort, perp-metric, equity and exec-fill reads ON the wire, so the bound this
    /// refusal reports moved to this route — and the assertions below moved with it, because a
    /// pinned sentence that has become false is worse than an unpinned one.
    #[test]
    fn an_account_kind_meets_the_plane_sentence_and_a_book_meets_the_route_one() {
        let account = parse_kind(Some("exec_fill")).unwrap_err();
        assert!(account.contains("ACCOUNT data"), "{account}");
        assert!(account.contains("vike-cli account"), "{account}");
        // ...and it does NOT get the wire sentence, which would be a fact about this verb standing
        // in front of a fact about the plane.
        assert!(!account.contains("THIS ROUTE"), "{account}");

        let book = parse_kind(Some("book")).unwrap_err();
        assert!(book.contains("THIS ROUTE"), "{book}");
        // ⚠ ...and it must NOT tell the operator the wire lacks the verb, which is the false
        // statement this change removed and the one that would send them around the server.
        assert!(
            !book.contains("reachable by no read verb"),
            "the refusal must not deny a verb the datahub serves: {book}"
        );
        assert!(book.contains("data hist ls --kind book"), "{book}");
        assert!(!book.contains("ACCOUNT data"), "{book}");

        // The market funding RATE is not an account kind and is not this verb's kind either — it
        // is `bar`, so it is refused as an unserved kind rather than as account data.
        // ⚠ ...and it lands on the OTHER side of the split below: `funding` is not a store kind at
        // all, so no verb reads it anywhere and the refusal must not promise that one exists.
        let funding = parse_kind(Some("funding")).unwrap_err();
        assert!(!funding.contains("ACCOUNT data"), "{funding}");
        assert!(funding.contains("no read verb for it at all"), "{funding}");
    }

    /// ⚠ **The refusal must tell a MISSING ARM from a MISSING VERB**, because the two name
    /// different work and sending a reader at the wrong one is how somebody goes around the server
    /// — the behaviour `docs/decisions/0084-only-the-datahub-touches-the-store.md` exists to stop.
    ///
    /// The completeness half is the part that earns this test: [`Kind::ALL`] plus
    /// [`WIRE_READS_THIS_ROUTE_LACKS`] plus `chain`/`properties` must be exactly the MARKET half of
    /// `vike_data::store_kind::STORE_KINDS`. A new store kind therefore reddens this test until
    /// somebody classifies it, which is the only thing standing in for the shared map that does not
    /// exist.
    #[test]
    fn the_refusal_distinguishes_a_missing_arm_from_a_missing_verb() {
        for kind in WIRE_READS_THIS_ROUTE_LACKS {
            let msg = parse_kind(Some(kind)).unwrap_err();
            assert!(msg.contains("THIS ROUTE"), "{kind}: {msg}");
            assert!(
                msg.contains("DOES answer this kind"),
                "a kind the wire serves must not be told the wire lacks a verb — {kind}: {msg}"
            );
        }
        for kind in ["chain", "properties"] {
            let msg = parse_kind(Some(kind)).unwrap_err();
            assert!(
                msg.contains("no read verb for it at all"),
                "a kind the wire does NOT serve must say so — {kind}: {msg}"
            );
            assert!(!msg.contains("THIS ROUTE"), "{kind}: {msg}");
        }

        // COMPLETENESS: every market store kind is classified by exactly one of the three groups.
        let mut classified: Vec<&str> = Kind::ALL
            .iter()
            .map(|k| k.as_str())
            .chain(WIRE_READS_THIS_ROUTE_LACKS.iter().copied())
            .chain(["chain", "properties"])
            .collect();
        classified.sort_unstable();
        let mut market: Vec<&str> = vike_data::store_kind::STORE_KINDS
            .iter()
            .map(|k| k.kind)
            .filter(|k| !vike_model::is_account_kind(k))
            .collect();
        market.sort_unstable();
        market.dedup();
        assert_eq!(
            classified, market,
            "every MARKET store kind must be in exactly one group: this route reads it, the wire \
             reads it and this route does not, or nothing reads it"
        );
    }

    #[test]
    fn a_blank_kind_is_its_own_refusal_and_the_default_is_bar() {
        assert_eq!(parse_kind(None), Ok(Kind::Bar));
        let err = parse_kind(Some("")).unwrap_err();
        assert!(err.contains("EMPTY"), "{err}");
        // ANTI-VACUITY: a NON-blank unknown value does NOT get the empty sentence.
        assert!(!parse_kind(Some("zzz")).unwrap_err().contains("EMPTY"));
    }

    /// The arity is a function of the kind, and a dropped interval is refused rather than ignored.
    #[test]
    fn the_spec_arity_follows_the_kind_and_a_surplus_interval_is_named() {
        assert_eq!(
            parse_spec("binance:BTCUSDT:1h", Kind::Bar),
            Ok(Spec {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: Some("1h".into())
            })
        );
        assert_eq!(
            parse_spec("binance:BTCUSDT", Kind::Trade),
            Ok(Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: None })
        );

        let surplus = parse_spec("binance:BTCUSDT:1h", Kind::Trade).unwrap_err();
        assert!(surplus.contains("no INTERVAL"), "{surplus}");
        assert!(surplus.contains("'1h'"), "{surplus}");
        assert!(surplus.contains("dropped in silence"), "{surplus}");

        let missing = parse_spec("binance:BTCUSDT", Kind::Bar).unwrap_err();
        assert!(missing.contains("needs an INTERVAL"), "{missing}");

        let blank = parse_spec("binance::1h", Kind::Bar).unwrap_err();
        assert!(blank.contains("non-empty"), "{blank}");
    }

    /// `--format` is REQUIRED on this route, and both of the shapes an operator is likeliest to
    /// carry over are refused by NAME rather than as unknown words.
    #[test]
    fn the_file_format_is_required_and_the_old_axis_is_corrected_by_name() {
        let absent = parse_wire(None).unwrap_err();
        assert!(absent.contains("needs --format"), "{absent}");
        assert!(absent.contains("jsonl | csv"), "{absent}");

        assert_eq!(parse_wire(Some("jsonl")), Ok(Wire::Jsonl));
        assert_eq!(parse_wire(Some("csv")), Ok(Wire::Csv));

        for old in ["table", "json"] {
            let err = parse_wire(Some(old)).unwrap_err();
            assert!(err.contains("TERMINAL rendering"), "{old}: {err}");
            assert!(err.contains("`--json`"), "{old}: {err}");
        }

        let parquet = parse_wire(Some("parquet")).unwrap_err();
        assert!(parquet.contains("backend-agnostic"), "{parquet}");
        assert!(parquet.contains("drop --addr"), "{parquet}");
        // ANTI-VACUITY: the parquet refusal is not the generic unknown-word one.
        assert!(!parse_wire(Some("zzz")).unwrap_err().contains("backend-agnostic"));
    }

    /// The walk covers `[start, end]` exactly once, with no shared instant between windows.
    #[test]
    fn the_walk_tiles_the_range_with_no_overlap_and_no_hole() {
        let w = windows(0, 9, 4);
        assert_eq!(w, vec![(0, 3), (4, 7), (8, 9)]);

        // The tiling property, asserted rather than read off the literal above: every window
        // begins one ms after the previous one ends, the first begins at `start`, the last ends at
        // `end`.
        for (lo, hi) in windows(1_000, 10_000, 1_500) {
            assert!(lo <= hi, "{lo} > {hi}");
        }
        let walk = windows(1_000, 10_000, 1_500);
        assert_eq!(walk.first().map(|w| w.0), Some(1_000));
        assert_eq!(walk.last().map(|w| w.1), Some(10_000));
        for pair in walk.windows(2) {
            assert_eq!(pair[1].0, pair[0].1 + 1, "{pair:?}");
        }

        // A step at least as wide as the range is ONE window, not two.
        assert_eq!(windows(5, 10, 1_000), vec![(5, 10)]);
        // A single instant is one window of width one.
        assert_eq!(windows(7, 7, 4), vec![(7, 7)]);
        // ANTI-VACUITY for the two guards: an inverted range and a non-positive step are empty,
        // and the healthy case above is NOT.
        assert!(windows(10, 5, 4).is_empty());
        assert!(windows(0, 9, 0).is_empty());
        assert!(!windows(0, 9, 4).is_empty());
    }

    /// The step grammar is the workspace's, narrowed — and the two span shapes that are not a fixed
    /// number of milliseconds are refused with what is wrong with each.
    #[test]
    fn the_window_step_reuses_the_workspace_span_grammar() {
        assert_eq!(parse_window_step("4h", Kind::Bar), Ok(4 * 3_600_000));
        assert_eq!(parse_window_step("1d", Kind::Trade), Ok(MS_PER_DAY));

        let bars = parse_window_step("500bars", Kind::Trade).unwrap_err();
        assert!(bars.contains("BAR COUNT"), "{bars}");
        let months = parse_window_step("3mo", Kind::Bar).unwrap_err();
        assert!(months.contains("CALENDAR"), "{months}");
        let junk = parse_window_step("soon", Kind::Bar).unwrap_err();
        assert!(junk.contains("--window"), "{junk}");
    }

    /// The per-kind defaults differ, and the tick lanes are the narrower ones.
    #[test]
    fn the_default_window_is_wider_for_bars_than_for_ticks() {
        assert_eq!(Kind::Bar.default_window_ms(), 30 * MS_PER_DAY);
        assert_eq!(Kind::Quote.default_window_ms(), MS_PER_DAY);
        assert_eq!(Kind::Trade.default_window_ms(), MS_PER_DAY);
        assert!(Kind::Bar.default_window_ms() > Kind::Trade.default_window_ms());
    }

    /// ⚠ THE CROSS-VERB PIN: `export --format jsonl` and `get --format jsonl` must describe one bar
    /// identically, or a file concatenated from both is two schemas wearing one name.
    #[test]
    fn the_bar_row_is_the_same_shape_get_emits() {
        let s = spec();
        let b = bar(1_700_000_000_000);
        let mine: Value = serde_json::from_str(&jsonl_line(&bar_cells(&s, &b))).expect("json");
        let theirs = crate::cmd::data::get::json_row(
            &crate::cmd::data::get::Spec {
                venue: s.venue.clone(),
                symbol: s.symbol.clone(),
                interval: s.interval.clone().expect("a bar spec has one"),
            },
            &b,
        );
        assert_eq!(mine, theirs);

        // ANTI-VACUITY: the comparison is over a non-trivial object, and the optional columns are
        // genuinely ABSENT here rather than present-and-null — which is the property being pinned.
        assert_eq!(mine.as_object().map(|m| m.len()), Some(10));
        assert!(mine.get("bid").is_none(), "{mine}");

        // ...and a bar that HAS them carries all three, in both verbs.
        let mut rich = bar(1);
        rich.funding = Some(0.0001);
        rich.bid = Some(9.0);
        rich.ask = Some(11.0);
        let rich_mine: Value =
            serde_json::from_str(&jsonl_line(&bar_cells(&s, &rich))).expect("json");
        assert_eq!(rich_mine.as_object().map(|m| m.len()), Some(13));
        assert_eq!(
            rich_mine,
            crate::cmd::data::get::json_row(
                &crate::cmd::data::get::Spec {
                    venue: s.venue.clone(),
                    symbol: s.symbol.clone(),
                    interval: s.interval.clone().expect("a bar spec has one"),
                },
                &rich,
            )
        );
    }

    /// The header comes from the roster, and the row comes from the same roster — so the two have
    /// the same width for every kind.
    #[test]
    fn every_kinds_header_and_row_are_the_same_width() {
        let s = Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: None };
        let bar_spec = spec();
        let rows: [(Kind, Cells); 3] = [
            (Kind::Bar, bar_cells(&bar_spec, &bar(1))),
            (
                Kind::Quote,
                quote_cells(
                    &s,
                    &QuoteTick {
                        ts: 1,
                        local_ts: 2,
                        bid: 1.0,
                        ask: 2.0,
                        bid_size: 3.0,
                        ask_size: 4.0,
                        symbol: String::new(),
                    },
                ),
            ),
            (
                Kind::Trade,
                trade_cells(
                    &s,
                    &TradeTick {
                        ts: 1,
                        local_ts: 2,
                        price: 5.0,
                        size: 6.0,
                        is_buyer_maker: true,
                        symbol: String::new(),
                    },
                ),
            ),
        ];
        for (kind, cells) in rows {
            let header = csv_header(kind);
            let line = csv_line(&cells);
            assert_eq!(
                header.split(',').count(),
                line.split(',').count(),
                "{}: header {header:?} vs row {line:?}",
                kind.as_str()
            );
            // ANTI-VACUITY: the widths are not both zero, and the roster names match the cells'.
            assert!(header.split(',').count() >= 7, "{}", kind.as_str());
            assert_eq!(
                kind.columns(),
                cells.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                "{}",
                kind.as_str()
            );
        }
    }

    /// The three CSV decisions, each asserted where it bites.
    #[test]
    fn the_csv_rules_are_minimal_quoting_an_empty_null_and_a_header_that_always_lands() {
        // QUOTING — minimal: a plain value is bare, and only the four characters that break a
        // parser are quoted.
        assert_eq!(csv_field("BTCUSDT"), "BTCUSDT");
        assert_eq!(csv_field("1.5"), "1.5");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");

        // NULL — an absent cell and a non-finite number are BOTH the empty field, which is the
        // cost this rule's doc states out loud.
        let cells = vec![
            ("a", Some(json!(1))),
            ("b", None),
            ("c", Some(Value::Null)),
            ("d", Some(json!("x"))),
        ];
        assert_eq!(csv_line(&cells), "1,,,x");
        // ANTI-VACUITY: the same cells under `jsonl` keep `b` and `c` APART — `b` is absent, `c` is
        // null — which is the reason to prefer that form and would be invisible if both were empty.
        let line = jsonl_line(&cells);
        assert!(!line.contains("\"b\""), "{line}");
        assert!(line.contains("\"c\":null"), "{line}");

        // HEADER — present for every kind, even one whose export has no rows.
        for kind in Kind::ALL {
            let h = csv_header(kind);
            assert!(h.starts_with("venue,symbol,"), "{}: {h}", kind.as_str());
        }
    }

    /// An empty answer says what it does and does not mean, and a non-empty one does not carry the
    /// note at all.
    #[test]
    fn an_empty_export_states_both_readings_and_a_full_one_says_nothing_extra() {
        let empty = summary(&plan(Wire::Jsonl), "out.jsonl", &Written { rows: 0, windows: 3 });
        assert_eq!(empty.len(), 2);
        assert!(empty[0].contains("3 windows"), "{:?}", empty[0]);
        assert!(empty[0].contains("binance:BTCUSDT:1h"), "{:?}", empty[0]);
        assert!(empty[1].contains("ONE of two facts"), "{:?}", empty[1]);
        assert!(empty[1].contains("--venue binance --name BTCUSDT"), "{:?}", empty[1]);
        // ⚠ The two formats describe the empty FILE differently, because a jsonl export of nothing
        // really is zero bytes while a csv one still carries its header.
        assert!(empty[1].contains("that file is empty"), "{:?}", empty[1]);
        let empty_csv = summary(&plan(Wire::Csv), "out.csv", &Written { rows: 0, windows: 1 });
        assert!(empty_csv[1].contains("a header and no rows"), "{:?}", empty_csv[1]);

        let full = summary(&plan(Wire::Jsonl), "out.jsonl", &Written { rows: 9, windows: 1 });
        assert_eq!(full.len(), 1);
        assert!(full[0].contains("1 window)"), "{:?}", full[0]);

        // The step note fires only when the flag was DEFAULTED and there was more than one window
        // to tune — a note on every run stops being read.
        assert!(step_note(&plan(Wire::Jsonl), &Written { rows: 9, windows: 1 }).is_none());
        let mut named = plan(Wire::Jsonl);
        named.step_defaulted = false;
        assert!(step_note(&named, &Written { rows: 9, windows: 4 }).is_none());
        // ANTI-VACUITY: the case it IS for fires, and names the flag.
        let note = step_note(&plan(Wire::Jsonl), &Written { rows: 9, windows: 4 })
            .expect("a defaulted multi-window walk is exactly what this note is for");
        assert!(note.contains(WINDOW_FLAG), "{note}");
    }

    /// The failed-window note names the lever and where to resume, because a byte count does not.
    #[test]
    fn a_failed_window_names_the_flag_that_lowers_it_and_where_to_resume() {
        let note = read_failed_note("binance:BTCUSDT:1h", 100, 200, 101, "connection reset");
        assert!(note.contains("connection reset"), "{note}");
        assert!(note.contains("--window SPAN"), "{note}");
        assert!(note.contains("--from 100"), "{note}");
    }

    /// The `--json` document carries the route and the columns, and names no engine.
    #[test]
    fn the_document_describes_a_remote_route_and_quotes_no_engine() {
        let doc = json_doc(
            "export",
            "127.0.0.1:7878",
            "out.csv",
            &plan(Wire::Csv),
            &Written { rows: 2, windows: 3 },
        );
        let v: Value = serde_json::from_str(&doc).expect("json");
        assert_eq!(v["route"], json!("remote"));
        assert_eq!(v["subcommand"], json!("export"));
        assert_eq!(v["format"], json!("csv"));
        assert_eq!(v["window"]["step_ms"], json!(5));
        assert_eq!(v["rows"], json!(2));
        assert_eq!(v["columns"], json!(Kind::Bar.columns()));
        // ANTI-VACUITY: the fields the LOCAL route's document carries are absent here rather than
        // null, which is the divergence `json_doc`'s doc argues for.
        assert!(v.get("engine").is_none(), "{doc}");
        assert!(v.get("engine_argv").is_none(), "{doc}");
    }
}
