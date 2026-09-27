//! `vike-cli data hist get SPEC (--days N | --from/--to) [--limit N] [--format table|json|jsonl]`
//! — BOUNDED ROWS TO STDOUT, the surface design's §8.2
//! (`docs/superpowers/specs/2026-09-20-cli-data-surface-design.md`).
//!
//! # The hole this fills
//!
//! Every competitor CLI has a row reader — `alpaca data bars`, `quantrocket history get`,
//! `gctcli gethistoriccandles`, `freqtrade list-data`. vike had none: `export` writes Parquet to a
//! LOCAL file through an engine spawn, so **there was no way to get rows out of a REMOTE store at
//! all**, in any quantity, in any format. Every other read verb in [`crate::cmd::data`] answers
//! about a store — `ls` its catalog, `gaps` its holes, `coverage` its cross-kind join, `gate` its
//! readiness. None of them shows you a PRICE.
//!
//! # ⚠ §8.2's TWO RULES ARE THE DESIGN, and both are enforced here
//!
//! **1. It refuses an unbounded request BY NAME.** A window is REQUIRED — [`Window`], built by
//! [`parse_window`], which refuses the no-window line rather than defaulting to the whole series.
//! And a default row ceiling applies, [`ROW_CEILING`], which [`parse_limit`] lets `--limit` LOWER
//! and refuses to let it exceed. **Hitting the ceiling is REPORTED, never truncated in silence** —
//! [`ceiling_note`], on every rendering.
//!
//! **2. The compute-to-data rule** — *"never pulls a raw UPSTREAM data slice across the wire"*,
//! which is why `crate::cmd::data::tape_health` refuses to run row-level checks. §8.2 answers it:
//! **printing is not compute**. This verb folds nothing, derives nothing and judges nothing; it
//! renders what the store already holds. What it must not become is a bulk extractor —
//! `data hist export` is that verb, and §11's P4 puts the server-side extraction arm THERE
//! *"precisely so that a large pull never crosses this socket"*.
//!
//! # ⚠ The residual, stated because it is the interesting half
//!
//! **The WINDOW bounds what crosses the wire; the LIMIT bounds what reaches stdout.** They are not
//! the same guard and this verb cannot make them one: `Request::LoadBars` carries a range and no
//! row count, so the whole window arrives here and [`ROW_CEILING`] is applied to the `Vec` in this
//! process. A row limit that reached the SERVER would be a protocol arm, which §8.2 explicitly
//! does not buy for this verb.
//!
//! That residual pays for one thing, though, and it is worth having: because the whole window
//! arrives, **the ceiling note can say exactly how many rows were withheld** rather than "there may
//! be more". A server-side limit would have had to guess.
//!
//! # ⚠ IT READS BARS, and that is a bound rather than a first instalment
//!
//! The spec is `VENUE:SYMBOL:INTERVAL` — `crate::cmd::data`'s [`super::check_spec`], the same three
//! mandatory parts `fetch` and `export` demand, because an interval is exactly what a BAR series
//! has and no tick-shaped kind does. There is no `--kind`: a quote row, a trade print and a book
//! level are three more row shapes, three more RPCs and three more renderings, and §7's own tree
//! spells this verb without one. `--kind` is therefore refused BY NAME in
//! [`crate::cmd::data`]'s `Sub::Get` arm — and an ACCOUNT kind named there is refused with §9.3.2's
//! shared sentence FIRST, because "this plane does not serve your fills" is a fact about the plane
//! and outranks a fact about this verb's shape.
//!
//! # The output forms, and why `jsonl` lands HERE
//!
//! [`Render`] is `table` | `json` | `jsonl`, and the third one is the point.
//! `crate::cmd::data`'s `UNBUILT_FORMATS` refused `jsonl` on every verb of this plane with the
//! reason *"no verb this axis reaches emits rows"* — **this is that verb**, so the refusal one
//! module up now names it as the place `jsonl` is served rather than as a phase to wait for
//! (`crate::cmd::data::ROW_VERB`, one spelling, rendered by both planes' refusals). The catalog
//! verbs still emit a CATALOG and still refuse `jsonl`, unchanged.
//!
//! ⚠ **The DEFAULT is `table` whatever stdout is**, which is the opposite of
//! `crate::cmd::data::realtime`'s `watch`. That verb follows the DESTINATION because §8.3 says so
//! for a STREAM; this one follows the PLANE, like `realtime status` — `data hist ls`,
//! `data catalog ls` and `data source ls` all render `table` into a pipe, and a verb that broke
//! ranks would be the one place in `data` where `| less` answered in JSON.
//!
//! ⚠ **Under `jsonl`, STDOUT CARRIES ROWS AND NOTHING ELSE.** The counts, the ceiling note and the
//! empty note go to STDERR, so `| jq` reads a clean stream and `> rows.jsonl` is data rather than a
//! transcript of this binary's opinions — `realtime`'s rule for `watch`, applied for its reason.

use serde_json::{Value, json};
use vike_model::time::epoch_ms_to_utc_timestamp;
// ⚠ `MS_PER_DAY` is IMPORTED rather than declared, for `super::gate`'s stated reason: it is
// `crates/vike-model/src/order.rs`'s constant, re-exported at that crate's root, and a local copy
// here would be a second declaration of a number the workspace already owns.
use vike_model::{Bar, MS_PER_DAY, epoch_ms_to_utc_date, parse_date_label};

use super::col;

/// The default number of rows this verb will print, and the ceiling `--limit` may not exceed.
///
/// ⚠ **ONE number, not a default AND a maximum.** §8.2: *"a default row ceiling that `--limit` may
/// lower and may not silently exceed"* — so the default IS the ceiling, a second knob would be a
/// second thing to keep in step, and `--limit 50000` is refused BY NAME rather than clamped. A
/// silent clamp is precisely the "truncated in silence" that rule forbids, one level up: the
/// operator would believe they had asked for fifty thousand rows and received all of them.
///
/// The value is a peek rather than an extraction: a thousand rows is more than a person reads and
/// about what `| jq` stays pleasant over, and anything larger is `data hist export`'s job — which
/// is where §11's server-side extraction arm belongs *"precisely so that a large pull never crosses
/// this socket"*.
pub(super) const ROW_CEILING: usize = 1000;

// ─── the SPEC ────────────────────────────────────────────────────────────────────────────────────

/// The bar series to read: one venue, one symbol, one interval.
///
/// ⚠ **All three parts are REQUIRED and the shape check is `crate::cmd::data`'s
/// [`super::check_spec`], not a second grammar.** `gate` needed its own parser because its spec is
/// a SELECTOR over what a store already holds — an optional interval, a `VENUE:@GROUP` alternative.
/// This one is an ADDRESS handed straight to `load_bars_ms`, which takes a venue, a symbol and an
/// interval and has no other shape to express; a two-part spec here could only ever be sent as a
/// series that cannot exist. Same three parts as `fetch` and `export`, same message when they are
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Spec {
    pub(super) venue: String,
    pub(super) symbol: String,
    pub(super) interval: String,
}

impl Spec {
    /// The spec as the operator would have typed it — rebuilt from the parsed parts rather than
    /// echoed from the raw string, so a header line and a `--json` document cannot disagree about a
    /// trimmed part. [`super::gate::Spec::text`]'s reason, one verb over.
    pub(super) fn text(&self) -> String {
        format!("{}:{}:{}", self.venue, self.symbol, self.interval)
    }
}

/// Parse the positional spec, or refuse it with the sentence `fetch` and `export` already give.
pub(super) fn parse_spec(raw: &str) -> Result<Spec, String> {
    super::check_spec(raw)?;
    // Present by construction: `check_spec` has just held this to three non-empty parts.
    let parts: Vec<&str> = raw.split(':').map(str::trim).collect();
    Ok(Spec {
        venue: parts[0].to_string(),
        symbol: parts[1].to_string(),
        interval: parts[2].to_string(),
    })
}

// ─── the WINDOW ──────────────────────────────────────────────────────────────────────────────────

/// The bound §8.2 requires, in the two forms an operator writes it.
///
/// ⚠ **A THIRD window type in this plane, and the divergence from both siblings is the reason.**
/// `crate::cmd::data`'s `Window` carries STRINGS because a `fetch`'s bounds used to be forwarded to
/// a child that parsed them, and it refuses `--from` without `--to` because a fetch with half a
/// window would decide how much of a venue rate limit to spend. Its `ExportRange` carries strings
/// too and requires NEITHER bound. This one is resolved to epoch-ms HERE — nothing is forwarded,
/// the comparison is the store's — and it requires AT LEAST ONE bound: §7.0's own worked example is
/// `data hist get binance:BTCUSDT:1h --from 2026-09-01 --limit 100`, a single `--from`.
///
/// ⚠ **A single bound is a HALF-OPEN window, and the ceiling is what makes that safe.** `--to`
/// alone leaves the start unbounded, which reads like the unbounded request §8.2 refuses — and it
/// would be, if the window were the only guard. It is not: [`ROW_CEILING`] bounds stdout whatever
/// the window says, and refusing a bound the store itself accepts would be a rule with nothing
/// behind it. What IS refused is naming no window at all, because that is an operator who has not
/// decided rather than one who has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Window {
    /// `--days N`, counting back from now. Resolved against the clock at EXECUTE time, so the
    /// parser stays pure — `crate::cmd::data`'s `parse` reads no clock and does no I/O.
    Days(i64),
    /// `--from`/`--to`, already epoch-ms. At least one is `Some` by construction.
    Range { start: Option<i64>, end: Option<i64> },
}

impl Window {
    /// The two bounds `load_bars_ms` takes, given the clock reading the CALLER made.
    ///
    /// `now` is a parameter rather than a read, which is what makes every case below testable
    /// against a fixed instant instead of against whenever the suite happened to run.
    pub(super) fn bounds(self, now: i64) -> (Option<i64>, Option<i64>) {
        match self {
            Window::Days(d) => (Some(now - d.saturating_mul(MS_PER_DAY)), Some(now)),
            Window::Range { start, end } => (start, end),
        }
    }
}

/// Build the window from the three flags, or refuse the line.
///
/// `parse_label` is `crate::cmd::data`'s own bound parser passed in, so this function stays pure and
/// this crate still owns exactly one timestamp grammar.
///
/// # Errors
///
/// No window at all (§8.2's named refusal); `--days` mixed with an explicit bound (two ways to say
/// one thing — `window_from`'s rule, kept); a non-positive or unreadable `--days`; a bound this side
/// cannot read; and an INVERTED range, refused rather than swapped for `membership_window`'s reason
/// — a range whose ends are the wrong way round has two readable meanings and picking one discards
/// half of what the operator typed.
pub(super) fn parse_window(
    days: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Window, String> {
    if days.is_some() && (from.is_some() || to.is_some()) {
        return Err("--days and --from/--to are two ways to say the same thing — pass one. \
                    `--days N` counts back from now; `--from`/`--to` name the dates."
            .to_string());
    }
    if let Some(d) = days {
        let n: i64 = d
            .trim()
            .parse()
            .map_err(|_| format!("--days {d:?} is not a whole number of days (e.g. --days 7)"))?;
        if n <= 0 {
            return Err(format!(
                "--days {n} covers no time at all, so the read would return nothing and say the \
                 store was empty. Name the window you want to look at"
            ));
        }
        // The multiply is checked at the same rung `gate::parse_require_days` checks its own: a
        // release build WRAPS, and a wrapped start bound is a window pointing into the far future
        // that quietly returns nothing.
        if n.checked_mul(MS_PER_DAY).is_none() {
            return Err(format!(
                "--days {n} does not fit: that many days is more milliseconds than a 64-bit count \
                 holds, so the start bound cannot be computed. The largest that can be asked for \
                 is {} days",
                i64::MAX / MS_PER_DAY
            ));
        }
        return Ok(Window::Days(n));
    }
    let start = bound("--from", from)?;
    let end = bound("--to", to)?;
    match (start, end) {
        (None, None) => {
            Err("get needs a window: --days N, or --from/--to (epoch-ms or YYYY-MM-DD), either of \
             which stands alone. An unbounded read of a remote store is what `data hist export` \
             is for — this verb prints a bounded peek, and refusing to guess the bound is the \
             whole of the cost guard."
                .to_string())
        }
        (Some(f), Some(t)) if f > t => Err(format!(
            "--from ({}) is AFTER --to ({}) — an inverted window holds nothing, so the answer \
             would be an empty listing that reads exactly like an empty store. Pass them the \
             other way round.",
            epoch_ms_to_utc_date(f),
            epoch_ms_to_utc_date(t)
        )),
        _ => Ok(Window::Range { start, end }),
    }
}

/// One `--from`/`--to` bound, parsed HERE because nothing is forwarded — the same rule, and the
/// same function ([`parse_date_label`]), `crate::cmd::data`'s `membership_window` and
/// `fetch_window_ms` already follow.
fn bound(flag: &str, raw: Option<&str>) -> Result<Option<i64>, String> {
    let Some(raw) = raw else { return Ok(None) };
    parse_date_label(raw).map(Some).map_err(|e| {
        format!(
            "{flag} {raw:?} is not a timestamp this side can read ({e}). `get` bounds the READ \
             rather than forwarding the text, so the bound is parsed here: epoch-ms, or YYYY-MM-DD"
        )
    })
}

// ─── the CEILING ─────────────────────────────────────────────────────────────────────────────────

/// Resolve `--limit`, or refuse it — §8.2's *"may LOWER and may not silently exceed"*.
pub(super) fn parse_limit(raw: Option<&str>) -> Result<usize, String> {
    let Some(raw) = raw else { return Ok(ROW_CEILING) };
    let n: usize = raw
        .trim()
        .parse()
        .map_err(|_| format!("--limit {raw:?} is not a whole number of rows (e.g. --limit 100)"))?;
    if n == 0 {
        return Err(
            "--limit 0 asks for no rows at all, which is a read that cannot tell you anything and \
             reads exactly like an empty store. Omit the flag for the default, or name the number \
             of rows you want to see"
                .to_string(),
        );
    }
    if n > ROW_CEILING {
        return Err(format!(
            "--limit {n} is above this verb's ceiling of {ROW_CEILING} rows, and the ceiling is \
             not clamped silently — you would believe you had received {n}. `get` prints a \
             BOUNDED peek; `vike-cli data hist export SPEC --out FILE` writes the whole slice, \
             which is the verb bulk extraction belongs to"
        ));
    }
    Ok(n)
}

/// What was withheld, said out loud — §8.2's *"hitting the ceiling is reported, never truncated in
/// silence"*.
///
/// `None` when nothing was withheld; a note that fires on every run stops being read.
///
/// ⚠ It names the EXACT count rather than "there may be more", and that is only possible because
/// the whole window crosses the wire — the residual this module's doc states, paying for something.
pub(super) fn ceiling_note(returned: usize, limit: usize, defaulted: bool) -> Option<String> {
    let withheld = returned.checked_sub(limit).filter(|n| *n > 0)?;
    let how = if defaulted {
        format!("the default ceiling of {limit} rows")
    } else {
        format!("the --limit {limit} you named")
    };
    Some(format!(
        "note: {withheld} more rows are in this window and are NOT shown — the answer was cut to \
         {how}. Narrow the window, raise --limit (up to {ROW_CEILING}), or write the whole slice \
         with `vike-cli data hist export`."
    ))
}

/// What an EMPTY answer means, and — the half that matters — what it does not.
///
/// ⚠ **Two different facts arrive as the same empty `Vec` and this verb cannot tell them apart.**
/// `load_bars` answers with no rows both for a series the store has never held and for a series it
/// holds nothing of in THIS window, and nothing on the wire distinguishes them. Printing "no rows"
/// alone would let an operator read a typo'd symbol as a gap in their data — so the note states
/// both readings and names the verb that answers the first.
pub(super) fn empty_note(spec: &Spec) -> String {
    format!(
        "no bars for {} in that window. That is ONE of two facts and this verb cannot tell them \
         apart: the series is not in this store, or it holds nothing between those bounds. \
         `vike-cli data hist ls --venue {} --name {}` answers the first.",
        spec.text(),
        spec.venue,
        spec.symbol
    )
}

// ─── the RENDERINGS ──────────────────────────────────────────────────────────────────────────────

/// HOW the rows are rendered.
///
/// A type of this verb's own rather than `crate::cmd::data`'s `Format`, for
/// `crate::cmd::data::realtime`'s stated reason: that axis is the CATALOG verbs' (`ls`, `catalog`,
/// `source`), it is right to refuse `jsonl` there, and widening it would make `data catalog ls
/// --format jsonl` valid for a verb that emits no rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Render {
    /// Aligned columns and the notes, for a person. The DEFAULT — see the module doc for why this
    /// verb follows the PLANE rather than the destination.
    Table,
    /// ONE JSON document: the request, the resolved window, the counts and the rows.
    Json,
    /// ONE JSON object per ROW, newline-separated. The pipeline form, and the reason this verb is
    /// where `jsonl` lands at all.
    Jsonl,
}

/// The formats §8.2 names for this verb and this verb does not serve, with what each is waiting on
/// — the same choice `crate::cmd::data`'s `UNBUILT_FORMATS` makes, for the same reason: an operator
/// who typed one has read the design, and "unknown format" would send them to check their spelling.
/// ⚠ **The `csv` row's reason was FALSIFIED by `data hist export --addr --format csv` and is
/// replaced rather than dropped.** It read *"a header row, a quoting rule and a null spelling are
/// three decisions nothing in this workspace has made yet"* — [`super::export`] makes all three
/// (`csv_header`, `csv_field`, `csv_line`, each arguing its own). So the value is no longer
/// unwritten anywhere; what is still true is that it is a FILE format and this verb PRINTS, which
/// is the same reason `parquet` has always given. The two rows now say one thing, and the verb that
/// writes both is named once ([`super::FILE_VERB`]).
const UNSERVED_RENDERS: &[(&str, &str)] = &[
    (
        "csv",
        "it is a FILE format rather than something to print — `--format jsonl | jq -r` builds one \
         to whatever shape a pipeline needs, and the verb that WRITES a csv file out of a store is",
    ),
    (
        "parquet",
        "it is a FILE format rather than something to print, and the verb that writes one is",
    ),
];

/// Parse a `--format` value for THIS verb, or refuse it with the reason that fits.
pub(super) fn parse_render(value: &str) -> Result<Render, String> {
    match value {
        "table" => Ok(Render::Table),
        "json" => Ok(Render::Json),
        "jsonl" => Ok(Render::Jsonl),
        "" => Err("--format was given an EMPTY value. Name `table` (the default), `json` (one \
                   document) or `jsonl` (one object per row)."
            .to_string()),
        other => {
            if let Some((_, why)) = UNSERVED_RENDERS.iter().find(|(name, _)| *name == other) {
                // ⚠ The verb is RENDERED from `super::FILE_VERB` rather than typed into each row,
                // which is why both rows' reasons now END on the word "is" — three copies of a
                // verb name is exactly the drift `super::ROW_VERB` exists to have prevented once.
                return Err(format!(
                    "`--format {other}` is not served by `get`: {why} {}. This verb renders \
                     `table` (the default), `json` and `jsonl`.",
                    super::FILE_VERB
                ));
            }
            Err(format!("unknown `--format {other}` (table | json | jsonl)"))
        }
    }
}

/// The rendering a line actually gets: what `--format` named, else what `--json` is shorthand for,
/// else the plane's default.
///
/// ⚠ The `--json` CONTRADICTIONS are not refused here — `crate::cmd::data`'s `parse` refuses both
/// of them above this call (`--format table` and `--format jsonl`), in the one place that spells
/// those sentences for every verb of the group. A second refusal here would be a second wording of
/// one rule. So `json_flag` reaching this function is only ever the SHORTHAND, never a
/// disagreement.
pub(super) fn render_for(format: Option<&str>, json_flag: bool) -> Result<Render, String> {
    match format {
        Some(raw) => parse_render(raw),
        None if json_flag => Ok(Render::Json),
        None => Ok(Render::Table),
    }
}

/// One bar as a JSON object — the row shape BOTH machine forms use.
///
/// ⚠ **The identity is repeated on every row, and that is deliberate.** `realtime watch` does the
/// opposite (its key goes to stderr once), because a live tape is one subscription per invocation.
/// A ROW form is meant to be concatenated: `get a … --format jsonl >> rows.jsonl` then `get b …`
/// produces a file whose rows are indistinguishable unless each carries its series. Three repeated
/// strings buy a self-describing stream.
///
/// ⚠ **`vike_model::Bar` is NOT serialized wholesale**, though it derives `Serialize` and that
/// would be one line. Its `symbol` field is *"attached by the engine dispatch"* and a store read
/// leaves it `None`, so echoing the struct would put a null `symbol` on every row of a verb whose
/// document already names one — a field that means "not applicable here" wearing the spelling of
/// "unknown". The three genuinely-optional fields are OMITTED when absent for the same reason:
/// `bid`/`ask` are tick-derived and `None` for OHLCV bars, and `funding` is `Some` only on the
/// funding lane. Which three, and how each is read, is [`OPTIONAL_COLUMNS`] — ONE roster serving
/// this document and [`table_lines`] both, so a field can never be in one form and not the other.
pub(super) fn json_row(spec: &Spec, bar: &Bar) -> Value {
    let mut row = json!({
        "venue": spec.venue,
        "symbol": spec.symbol,
        "interval": spec.interval,
        "ts": bar.ts,
        "ts_utc": epoch_ms_to_utc_timestamp(bar.ts),
        "open": bar.open,
        "high": bar.high,
        "low": bar.low,
        "close": bar.close,
        "volume": bar.volume,
    });
    let map = row.as_object_mut().expect("the literal above is an object");
    for (key, _, get) in OPTIONAL_COLUMNS {
        if let Some(v) = get(bar) {
            map.insert(key.to_string(), json!(v));
        }
    }
    row
}

/// The `--format json` document.
///
/// ⚠ It carries `returned` BESIDE `shown`, which is the machine form of [`ceiling_note`]: a
/// consumer folding these rows has to be able to tell a complete answer from a cut one, and a bare
/// array cannot say which it is. `truncated` is derived from the two rather than being a third
/// independent field for them to disagree with.
///
/// ⚠ **[`empty_note`] has NO field counterpart in the counts, so it gets one of its own.** The
/// ceiling fact is fully carried by `returned`/`shown`/`truncated`; the EMPTY fact is not — a
/// document reading `returned: 0, shown: 0, bars: []` states one of two readings and cannot say
/// which, which is precisely the misreading that function exists to prevent. It shipped stated
/// nowhere under this rendering: not as a field, not on stderr, while the equally machine-facing
/// `jsonl` form put it on stderr. So `note` is present exactly when the answer is empty — a
/// sometimes-absent key, the same shape [`OPTIONAL_COLUMNS`] already gives a row, because a note
/// that fires on every run stops being read.
pub(super) fn json_doc(
    subcommand: &str,
    addr: &str,
    spec: &Spec,
    bounds: (Option<i64>, Option<i64>),
    limit: usize,
    returned: usize,
    shown: &[Bar],
) -> String {
    let mut doc = json!({
        // ⚠ A PARAMETER, never a literal. `crate::cmd::data`'s four sibling renderers each typed
        // the verb name in, so the group split renamed `list` -> `ls` in the CLI while their
        // documents kept saying the old word — four copies of one fact, found by the rename rather
        // than by reading. The caller passes `Sub::as_str()`.
        "subcommand": subcommand,
        "addr": addr,
        "spec": {
            "text": spec.text(),
            "venue": spec.venue,
            "symbol": spec.symbol,
            "interval": spec.interval,
        },
        "window": {
            "from_ts": bounds.0,
            "to_ts": bounds.1,
            "from_date": bounds.0.map(epoch_ms_to_utc_date),
            "to_date": bounds.1.map(epoch_ms_to_utc_date),
        },
        "limit": limit,
        "returned": returned,
        "shown": shown.len(),
        "truncated": returned > shown.len(),
        "bars": shown.iter().map(|b| json_row(spec, b)).collect::<Vec<_>>(),
    });
    if returned == 0 {
        doc.as_object_mut()
            .expect("the literal above is an object")
            .insert("note".to_string(), json!(empty_note(spec)));
    }
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

/// A price or a size, printed ROUND-TRIP EXACT rather than rounded to a column width.
///
/// ⚠ Rounding here would print a number the store does not hold, in a verb whose whole product is
/// the number. A wide value makes the column wide; that is the correct trade for a peek at data,
/// and it is the reading-side twin of the discipline
/// `crates/vike-bridge-core/tests/wire_quantizer_probe.rs` gates on the writing side — where one
/// ULP of rounding becomes a whole tick on the wire.
fn num(v: f64) -> String {
    format!("{v}")
}

/// One column EVERY bar has: its header, and the cell it reads.
///
/// An alias rather than the tuple spelled at each site, because `clippy::type_complexity` refuses
/// the bare form — and the alias earns its place beyond silencing that: it is where the SHAPE of a
/// column is stated once for the two rosters below.
type CoreColumn = (&'static str, fn(&Bar) -> f64);

/// One column a bar MAY have: its JSON key, its table header, and the cell it reads. The key comes
/// first because [`json_row`] is the form that cannot omit it.
type OptionalColumn = (&'static str, &'static str, fn(&Bar) -> Option<f64>);

/// The five columns EVERY bar has.
///
/// Function pointers rather than a match per column, so the header roster and the cell roster are
/// one list and cannot go out of step — the failure a hand-aligned table makes silently, by
/// printing a `HIGH` header over the `low` value.
const CORE_COLUMNS: [CoreColumn; 5] = [
    ("OPEN", |b: &Bar| b.open),
    ("HIGH", |b: &Bar| b.high),
    ("LOW", |b: &Bar| b.low),
    ("CLOSE", |b: &Bar| b.close),
    ("VOLUME", |b: &Bar| b.volume),
];

/// The three columns a bar MAY have — the tick-derived pair and the funding lane's rate.
///
/// ⚠ **ONE roster for BOTH output forms.** [`json_row`] reads the key and [`table_lines`] the
/// header, so a field cannot arrive in the machine form and be missing from the human one (or the
/// reverse), which is exactly how the two would drift if each held its own list.
const OPTIONAL_COLUMNS: [OptionalColumn; 3] = [
    ("funding", "FUNDING", |b: &Bar| b.funding),
    ("bid", "BID", |b: &Bar| b.bid),
    ("ask", "ASK", |b: &Bar| b.ask),
];

/// The human table: a header, one line per bar, and nothing else. The NOTES are the caller's — see
/// [`ceiling_note`] and [`empty_note`] — because under `jsonl` they go to a different stream.
///
/// ⚠ **An OPTIONAL column appears only when some row in THIS answer carries it.** A column that is
/// `-` all the way down says nothing and costs a reader width; a column present in only some rows
/// must still appear, or a funding tape would render as plain OHLCV. The rule is per-ANSWER rather
/// than per-row, so the table stays rectangular.
///
/// ⚠ **A row that genuinely has none renders `-`, never a zero.** A bar with no recorded bid is not
/// a bar whose bid was 0, and this is the one verb in the plane where the two would be read as the
/// same number.
pub(super) fn table_lines(spec: &Spec, bars: &[Bar]) -> Vec<String> {
    // The cells first, then the widths, then the render — so every width is measured against the
    // exact string that will be printed rather than against a second formatting of the same value.
    let optional: Vec<&OptionalColumn> = OPTIONAL_COLUMNS
        .iter()
        .filter(|(_, _, get)| bars.iter().any(|b| get(b).is_some()))
        .collect();
    let headers: Vec<&str> = std::iter::once("TS (UTC)")
        .chain(CORE_COLUMNS.iter().map(|(h, _)| *h))
        .chain(optional.iter().map(|(_, h, _)| *h))
        .collect();
    let rows: Vec<Vec<String>> = bars
        .iter()
        .map(|b| {
            std::iter::once(epoch_ms_to_utc_timestamp(b.ts))
                .chain(CORE_COLUMNS.iter().map(|(_, get)| num(get(b))))
                .chain(
                    optional.iter().map(|(_, _, get)| get(b).map_or_else(|| "-".to_string(), num)),
                )
                .collect()
        })
        .collect();
    let widths: Vec<usize> =
        headers.iter().enumerate().map(|(i, h)| col(h, rows.iter().map(|r| r[i].len()))).collect();

    // The timestamp is LEFT-aligned and every number RIGHT-aligned, the split `execute_list`'s
    // table already makes: a label reads down its left edge, a number down its decimal point.
    let render = |cells: &[String]| -> String {
        let mut line = format!("{:<w$}", cells[0], w = widths[0]);
        for (cell, w) in cells[1..].iter().zip(&widths[1..]) {
            line.push_str(&format!("  {cell:>w$}", w = *w));
        }
        line
    };
    let header: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    let mut out = vec![format!("{} — {} rows", spec.text(), bars.len()), render(&header)];
    for row in &rows {
        out.push(render(row));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    fn spec() -> Spec {
        Spec {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
        }
    }

    fn bar(ts: i64, close: f64) -> Bar {
        Bar {
            ts,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close,
            volume: 10.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// The spec is the THREE-part one, and the shape rule is `check_spec`'s rather than a second
    /// grammar — so the shapes `gate` accepts are refused here, by the message `fetch` gives.
    #[test]
    fn the_spec_is_three_parts_and_reuses_the_plane_s_one_shape_check() {
        let s = parse_spec(" binance : BTCUSDT : 1h ").expect("whitespace is trimmed");
        assert_eq!(s, spec());
        assert_eq!(s.text(), "binance:BTCUSDT:1h", "the text is REBUILT, never echoed");
        for bad in ["binance:BTCUSDT", "binance:@group", "a:b:c:d", "binance::1h"] {
            let err = parse_spec(bad).expect_err("not a three-part spec");
            assert!(err.contains("VENUE:SYMBOL:INTERVAL"), "{bad}: {err}");
        }
    }

    /// **§8.2 RULE 1, the window half.** No window at all is refused BY NAME, a single bound is
    /// accepted (§7.0's own worked example), and the two forms may not be mixed.
    ///
    /// ⚠ The anti-vacuity control is the SECOND half of each pair: a parser that refused everything
    /// would satisfy every refusal assertion here, so each one is paired with a line that must
    /// PARSE.
    #[test]
    fn a_window_is_required_by_name_and_one_bound_is_a_window() {
        let err = parse_window(None, None, None).expect_err("an unbounded read is refused");
        assert!(err.contains("get needs a window"), "{err}");
        assert!(err.contains("--days") && err.contains("--from"), "…naming both forms: {err}");
        assert!(err.contains("export"), "…and the verb bulk extraction belongs to: {err}");

        // THE CONTROLS: each of the three forms parses, so the refusal above is about absence.
        assert_eq!(parse_window(Some("7"), None, None).expect("--days"), Window::Days(7));
        assert_eq!(
            parse_window(None, Some("0"), None).expect("--from alone is §7.0's example"),
            Window::Range { start: Some(0), end: None }
        );
        assert_eq!(
            parse_window(None, None, Some("0")).expect(
                "--to alone is half-open, and bounded by \
                                                       the ceiling"
            ),
            Window::Range { start: None, end: Some(0) }
        );

        let err = parse_window(Some("7"), Some("0"), None).expect_err("two ways to say one thing");
        assert!(err.contains("pass one"), "{err}");
    }

    /// The bounds a `--days` window resolves to are computed from the CALLER's clock reading, so
    /// the arithmetic is pinned against a fixed instant rather than against whenever this ran.
    #[test]
    fn the_days_window_counts_back_from_the_callers_clock() {
        let now = 10 * DAY_MS;
        assert_eq!(Window::Days(3).bounds(now), (Some(7 * DAY_MS), Some(now)));
        // ...and an explicit range is carried through untouched, including its half-open forms.
        assert_eq!(
            Window::Range { start: Some(5), end: None }.bounds(now),
            (Some(5), None),
            "a resolved range does not consult the clock at all"
        );
    }

    /// The window refusals that are about ARITHMETIC rather than about shape, each paired with the
    /// value on the other side of the boundary.
    #[test]
    fn a_window_that_cannot_be_computed_is_refused_rather_than_wrapped() {
        assert!(parse_window(Some("0"), None, None).unwrap_err().contains("no time at all"));
        assert!(parse_window(Some("-1"), None, None).unwrap_err().contains("no time at all"));
        assert!(parse_window(Some("x"), None, None).unwrap_err().contains("whole number"));
        // The `checked_mul` rung: `i64::MAX` days is more milliseconds than an i64 holds, and a
        // release build would WRAP it into a start bound in the far future.
        let err = parse_window(Some("200000000000"), None, None).expect_err("does not fit");
        assert!(err.contains("does not fit"), "{err}");
        assert_eq!(
            parse_window(Some("1"), None, None).expect("one day still fits"),
            Window::Days(1),
            "the control: the refusal is about the size, not about --days"
        );
        // An inverted range is REFUSED, never swapped.
        let err = parse_window(None, Some("2026-02-01"), Some("2026-01-01")).expect_err("inverted");
        assert!(err.contains("other way round"), "{err}");
        assert!(
            parse_window(None, Some("2026-01-01"), Some("2026-02-01")).is_ok(),
            "the control: the same two labels the right way round"
        );
    }

    /// **§8.2 RULE 1, the ceiling half.** `--limit` LOWERS and may not exceed, and the refusal names
    /// the ceiling and the verb that does bulk.
    #[test]
    fn the_limit_may_lower_the_ceiling_and_may_not_exceed_it() {
        assert_eq!(parse_limit(None).expect("the default IS the ceiling"), ROW_CEILING);
        assert_eq!(parse_limit(Some("10")).expect("lowering is the point"), 10);
        assert_eq!(
            parse_limit(Some(&ROW_CEILING.to_string())).expect("the ceiling itself is allowed"),
            ROW_CEILING
        );
        let err = parse_limit(Some(&(ROW_CEILING + 1).to_string())).expect_err("above the ceiling");
        assert!(err.contains(&ROW_CEILING.to_string()), "the ceiling is NAMED: {err}");
        assert!(err.contains("export"), "…and so is the verb that does bulk: {err}");
        assert!(parse_limit(Some("0")).unwrap_err().contains("no rows at all"));
        assert!(parse_limit(Some("-1")).unwrap_err().contains("whole number"));
    }

    /// **§8.2 RULE 1, the disclosure.** Hitting the ceiling is REPORTED with the exact count
    /// withheld, and NOT reported when nothing was.
    #[test]
    fn the_ceiling_is_reported_with_what_it_withheld_and_is_silent_otherwise() {
        let note = ceiling_note(1_500, 1_000, true).expect("500 rows were cut");
        assert!(note.contains("500 more rows"), "the EXACT count, not `there may be more`: {note}");
        assert!(note.contains("default ceiling"), "…and which ceiling it was: {note}");
        let named = ceiling_note(50, 10, false).expect("40 rows were cut");
        assert!(
            named.contains("--limit 10"),
            "a named limit says so rather than `default`: {named}"
        );
        // THE CONTROLS: nothing withheld is no note, on either side of the boundary.
        assert_eq!(ceiling_note(1_000, 1_000, true), None, "a full answer is not a cut one");
        assert_eq!(ceiling_note(3, 10, false), None);
    }

    /// An empty answer states BOTH readings, because the wire carries only one.
    ///
    /// ⚠ **…in EVERY rendering, which is the half that shipped broken.** `table` printed the note
    /// and `jsonl` put it on stderr, while `json` carried it nowhere at all — so the one form a
    /// SCRIPT reads was the one form that let a typo'd symbol read as a gap in the data. The
    /// document's `note` field is asserted here, beside the string, so the two cannot part again.
    #[test]
    fn an_empty_answer_never_claims_the_series_is_empty() {
        let note = empty_note(&spec());
        assert!(note.contains("binance:BTCUSDT:1h"), "{note}");
        assert!(note.contains("ONE of two facts"), "the ambiguity is stated: {note}");
        assert!(note.contains("data hist ls"), "…and the verb that answers the other: {note}");

        let text = json_doc("get", "a", &spec(), (Some(0), None), ROW_CEILING, 0, &[]);
        let doc: Value = serde_json::from_str(&text).expect("one document");
        assert_eq!(doc["returned"], 0, "the case is an EMPTY answer");
        assert_eq!(doc["note"], note, "…and it carries the SAME sentence, not a second wording");
        // THE CONTROL: a non-empty answer carries no `note`, so the assertion above is about the
        // emptiness rather than about the key always being there.
        let bars = [bar(0, 1.0)];
        let text = json_doc("get", "a", &spec(), (Some(0), None), ROW_CEILING, 1, &bars);
        let doc: Value = serde_json::from_str(&text).expect("one document");
        assert!(doc.get("note").is_none(), "a note on every run stops being read: {doc}");
    }

    /// The format axis THIS verb serves — and `jsonl`, which is the reason it exists.
    #[test]
    fn the_row_verb_serves_jsonl_and_refuses_the_unserved_ones_by_name() {
        assert_eq!(parse_render("table").unwrap(), Render::Table);
        assert_eq!(parse_render("json").unwrap(), Render::Json);
        assert_eq!(parse_render("jsonl").unwrap(), Render::Jsonl);
        for (name, _) in UNSERVED_RENDERS {
            let err = parse_render(name).expect_err("not served here");
            assert!(err.contains(name), "{name}: {err}");
            assert!(err.contains("jsonl"), "{name} must name what IS served: {err}");
        }
        assert!(parse_render("yaml").unwrap_err().contains("unknown"));
        assert!(parse_render("").unwrap_err().contains("EMPTY"));
    }

    /// The default follows the PLANE, not the destination — `table` unless something said otherwise.
    #[test]
    fn the_default_render_is_the_table_and_json_is_the_shorthand() {
        assert_eq!(render_for(None, false).unwrap(), Render::Table);
        assert_eq!(render_for(None, true).unwrap(), Render::Json, "--json IS --format json");
        assert_eq!(render_for(Some("jsonl"), false).unwrap(), Render::Jsonl);
        // ⚠ `json_flag` is ignored whenever `--format` named something, and that is not this
        // function being lax: `crate::cmd::data`'s `parse` has already refused every way the two
        // can DISAGREE, so a `true` reaching here beside an explicit format is unreachable from a
        // real command line. Spelled out rather than left to be discovered, because the reachable
        // shape a reader would guess — "jsonl wins over --json" — is a rule nobody has to know.
        assert_eq!(render_for(Some("json"), true).unwrap(), Render::Json);
    }

    /// A row carries its own identity and omits the fields the store did not record — never a null
    /// that reads as "unknown".
    #[test]
    fn a_row_is_self_describing_and_omits_what_was_not_recorded() {
        let plain = json_row(&spec(), &bar(DAY_MS, 3.5));
        assert_eq!(plain["venue"], "binance");
        assert_eq!(plain["symbol"], "BTCUSDT");
        assert_eq!(plain["interval"], "1h");
        assert_eq!(plain["close"], 3.5);
        assert!(plain["ts_utc"].as_str().expect("a rendered timestamp").contains("1970"));
        for absent in ["funding", "bid", "ask"] {
            assert!(plain.get(absent).is_none(), "an unrecorded {absent} is OMITTED: {plain}");
        }
        // THE CONTROL: a recorded one is present, so the omission above is about absence rather
        // than about the builder never emitting these keys.
        let funded = json_row(&spec(), &Bar { funding: Some(-0.0001), ..bar(0, 1.0) });
        assert_eq!(funded["funding"], -0.0001);
    }

    /// The document's counts are what let a consumer tell a complete answer from a cut one.
    #[test]
    fn the_document_carries_both_counts_and_derives_truncated_from_them() {
        let bars = [bar(0, 1.0), bar(DAY_MS, 2.0)];
        let text = json_doc("get", "127.0.0.1:7878", &spec(), (Some(0), Some(DAY_MS)), 2, 9, &bars);
        let doc: Value = serde_json::from_str(&text).expect("one document");
        assert_eq!(doc["subcommand"], "get");
        assert_eq!(doc["returned"], 9, "what the store answered with");
        assert_eq!(doc["shown"], 2, "…and what was printed");
        assert_eq!(doc["truncated"], true);
        assert_eq!(doc["bars"].as_array().expect("an array").len(), 2);
        assert_eq!(doc["window"]["from_date"], "1970-01-01");
        // THE CONTROL: an uncut answer says so, so `truncated` is derived rather than pinned true.
        let whole = json_doc("get", "a", &spec(), (None, Some(0)), 1000, 2, &bars);
        let doc: Value = serde_json::from_str(&whole).expect("one document");
        assert_eq!(doc["truncated"], false);
        assert_eq!(doc["window"]["from_ts"], Value::Null, "an unbounded side is null, not 0");
    }

    /// The table is rectangular, the header names every column it renders, and an OPTIONAL column
    /// appears only when the answer has one.
    #[test]
    fn the_table_omits_an_optional_column_no_row_carries() {
        let plain = table_lines(&spec(), &[bar(0, 1.0), bar(DAY_MS, 2.0)]);
        assert!(plain[0].contains("binance:BTCUSDT:1h") && plain[0].contains("2 rows"));
        assert!(plain[1].contains("CLOSE") && plain[1].contains("VOLUME"));
        for absent in ["FUNDING", "BID", "ASK"] {
            assert!(!plain[1].contains(absent), "no row carries a {absent}: {}", plain[1]);
        }
        assert_eq!(plain.len(), 4, "a header line, a column header, and one line per bar");

        // THE CONTROL: with ONE row carrying a funding rate the column appears for EVERY row, and
        // the row that has none renders `-` rather than a zero.
        let mixed =
            table_lines(&spec(), &[Bar { funding: Some(0.25), ..bar(0, 1.0) }, bar(DAY_MS, 2.0)]);
        assert!(mixed[1].contains("FUNDING"), "{}", mixed[1]);
        assert!(mixed[2].contains("0.25"), "{}", mixed[2]);
        assert!(mixed[3].ends_with('-'), "an unrecorded cell is `-`, never 0: {}", mixed[3]);
    }
}
