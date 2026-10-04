//! `vike-cli data hist export SPEC --out FILE --addr H:P --format jsonl|csv [--kind K]
//! [--window SPAN] [--from L] [--to L]` — ROWS OUT OF A **REMOTE** STORE, the surface design's §7
//! and §11's P4 (`docs/superpowers/specs/2026-09-20-cli-data-surface-design.md`).
//!
//! # The hole this fills, which is not the one [`super::get`] filled
//!
//! `get` shows you a PRICE: a bounded peek, a thousand rows at most, refused above its ceiling by
//! name. It is deliberately not an extractor — §8.2 says so and [`super::get::ROW_CEILING`]'s doc
//! repeats it, naming `export` as the verb bulk extraction belongs to. And `export` could not do
//! it: it spawned the ENGINE against a store on THIS machine, so a store that lives on a datahub —
//! which is every store this CLI otherwise talks to — had **no bulk extraction path at all**.
//! (That engine route reads through a datahub too since 2026-09-26 — see the ⚠ under THE TWO
//! ROUTES below — but it writes Parquet only, and bars only.)
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
//! export SPEC --out F                      ENGINE: spawn the engine, write Parquet.
//! export SPEC --out F --addr H:P --format  REMOTE: walk the wire, write rows. This module.
//! ```
//!
//! ⚠ **`--addr` is the switch, and it is the EXPLICIT one** — `crate::cmd::data::Args::addr` is
//! always resolved to a default, so `addr_given` is the field that answers "did the operator ask
//! for the remote route". `rm` already turns on exactly that distinction, for exactly that reason.
//!
//! ⚠ **Since 2026-09-26 BOTH routes read through a datahub, and the first row above said
//! "LOCAL … Unchanged" until then.** Decision 0084's amendment closed the local READ door on every
//! history reader and named `export` as the one it had left open: the engine route opened the store
//! at `--store` itself. It asks a datahub for the bars now — the one its settings name,
//! `config.datahub_addr` with `VIKE_DATAHUB_ADDR` above it, loopback by default — and `--store` is
//! refused on this verb by name. So the
//! two routes no longer differ in WHERE they read; they differ in who ENCODES and what: the engine
//! writes Parquet (the encoder this binary must not link), this module writes rows it walked
//! itself. What "local" still means on the engine route is the ENCODING, which is why this page
//! calls it the ENGINE route rather than the local one wherever it names it now.
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
//! frame comes back as the server's named refusal (bars) or the transport's own error (ticks) —
//! which [`read_failed_note`] wraps with the flag that fixes it rather than leaving an operator to
//! read a byte count.
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
//! (This paragraph is the measurement as it stood before 2026-09-26; the ⚠ below says what moved.)
//! `DataFusionHist::export_bars_parquet` existed and would have been the body of a streaming arm.
//! The server could not reach it: `crates/vike-datahub/src/server.rs` holds an
//! `Arc<dyn HistStore + Send + Sync>` and says of itself that it is BACKEND-AGNOSTIC, while that
//! method is an INHERENT method of the concrete `DataFusionHist` and is not on the `HistStore`
//! trait. Closing that is a layering decision in `vike-data`/`vike-datahub` — put a Parquet encoder
//! on the trait every implementer must answer (including the DataFusion-free doubles), or thread a
//! second `serve-datafusion`-gated store handle through `serve`/`handle_connection`/
//! `handle_request`, which today have no such split at all — plus this wire's FIRST chunked
//! response. That is not this verb's change. So `--format parquet --addr` is refused BY NAME, with
//! the route that does write Parquet ([`parquet_refusal`]).
//!
//! ⚠ **Half of that measurement moved on 2026-09-26.** The encoder is a FREE function now,
//! `vike_data::write_bars_parquet`, because the engine route stopped opening a store (decision
//! 0084's amendment): it reads the bars over the same paged `LoadBars` this module walks and encodes
//! them on its side. The inherent method is DELETED — it was that function over the store's own
//! `load_bars`, and nothing called it any more. So the "inherent method" half no longer holds; what still does is the WIRE's
//! half — an answer that is a file rather than rows is a new verb plus the first chunked response,
//! and the Parquet route already exists without one.
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
    /// Derived OHLCV — `Request::LoadBars`. The DEFAULT, because it is what the ENGINE route's
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
/// of `vike_data::store::store_kind::STORE_KINDS`, which is what
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
/// ⚠ **It is REQUIRED here and defaulted on the engine route, which is deliberate rather than an
/// inconsistency.** The engine route writes Parquet because that is what the engine writes; this
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
             route serves are {}. The ENGINE route (drop --addr) writes Parquet, which is where \
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
            "unknown `--format {other}` on a remote export ({}). `parquet` is the ENGINE route's \
             format — drop --addr.",
            served_formats()
        )),
    }
}

/// `--format` on the ENGINE route, where the only file this verb writes is Parquet.
///
/// ⚠ The function keeps its `local_route` name: what is local about that route since 2026-09-26 is
/// the ENCODING, not the read — the engine asks a datahub for the bars like every other reader
/// (decision 0084's amendment). A rename across every caller would buy nothing a reader of this
/// line does not already have.
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
            "`--format {trimmed}` is written by the REMOTE route: this binary walks the datahub \
             at --addr and writes the ROWS itself, while this route spawns the engine, which \
             reads the bars from the datahub its settings name and writes Parquet. Add --addr \
             HOST:PORT to take the remote route, or name `parquet`."
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
/// that the wire carries ROWS and never a FILE. An operator who wants Parquet today has a working
/// command line — the engine route — and it is in the message.
///
/// ⚠ **What the engine route IS changed on 2026-09-26, and this message said "from a store on THIS
/// machine" until then.** Decision 0084's amendment closed the local READ door, so the engine no
/// longer opens a store: it asks a datahub for the bars over the ordinary paged `LoadBars` and
/// encodes them with `vike_data::write_bars_parquet`. That also moved the encoder out of the
/// concrete store's inherent methods, which is what this message used to cite as the blocker; the
/// blocker that remains is the wire's own shape — a Parquet answer would be a new verb plus this
/// wire's first chunked response. The engine reads the datahub ITS settings name
/// (`config.datahub_addr`, with `VIKE_DATAHUB_ADDR` above it), so an operator aiming the Parquet
/// route at another hub sets one of those rather than `--addr`.
pub(super) fn parquet_refusal() -> String {
    format!(
        "`--format parquet` is not served over --addr. This binary links no Parquet encoder, and \
         `crates/vike-datahub/src/server.rs` is backend-agnostic by design and answers with ROWS, \
         never a file — a Parquet answer would be a new wire verb plus this wire's first chunked \
         response rather than a flag. Two things work today: drop --addr and the ENGINE writes \
         the Parquet file, reading the bars through the datahub its settings name \
         (config.datahub_addr, or VIKE_DATAHUB_ADDR above it; default 127.0.0.1:7878 — point \
         either at this one); or name {} to stream rows from the remote one.",
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
/// ⚠ **The likeliest cause is not the only one, and the message says so.** A window holding more
/// rows than one reply can carry fails in one of two ways, by kind. A BAR window over the server's
/// ceiling (`vike_datahub::server`'s `LOAD_BARS_CEILING`, the most bars one 64 MiB frame can carry)
/// is REFUSED BY NAME before the rest of it is read, and `why` carries that refusal, naming the
/// ceiling (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`). A TICK window
/// over its own kind's ceiling is refused by name the same way (`vike_datahub::server`'s
/// `ReadCeilings`, `docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`), and
/// so is a window UNDER its ceiling whose rows' JSON still passes `MAX_FRAME_LEN` (the server's
/// byte cap, `fit_to_frame`) — so an oversized window now always arrives as a named refusal, and a
/// transport error here is a network fault. ⚠ This doc said every oversized window arrived "looking
/// like a transport fault" until the bar ceiling landed, and that TICK windows still did until the
/// byte cap did. Naming [`WINDOW_FLAG`] as the thing to try is actionable for every oversized case
/// and harmless for a network fault, and the window that failed is named so a retry can start there
/// rather than from the beginning.
pub(super) fn read_failed_note(
    spec_text: &str,
    lo: i64,
    hi: i64,
    step_ms: i64,
    why: &str,
) -> String {
    format!(
        "reading {spec_text} over [{lo}, {hi}] failed: {why}\n\
         That window is {step_ms}ms wide. A window answers in ONE frame, so one holding more than \
         the 64 MiB frame cap can carry is refused by name on the server side — lower the step \
         with `{WINDOW_FLAG} SPAN` (e.g. `{WINDOW_FLAG} 1h`) and retry from --from {lo}."
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
/// engine route can never read a half-filled one — `RmArgs`' rule, one verb over.
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
    /// ⚠ **The one place this route is STRICTER than the engine one, and it is a property of the
    /// walk rather than a preference.** The engine export hands its bounds to one `load_bars`
    /// through `vike_datahub_client::RemoteHistStore`, whose paged `LoadBars` pages by ROW count —
    /// each page continues from the last timestamp the one before returned — so it needs neither
    /// bound (it scanned the store with DataFusion itself until 2026-09-26, and needed neither then
    /// either); this walk steps a wall-clock WINDOW of its own, so it has to know where to take its
    /// first step and when to stop, and an unbounded side has no first or last window. The refusal
    /// names `data hist ls`, which PRINTS each series' recorded span — so the operator has a
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

#[path = "export_tests.rs"]
#[cfg(test)]
mod export_tests;
