//! `vike-cli data hist gate SPEC --require-days N [--max-gap D] [--require-kind K]` — the DATA
//! plane's readiness gate, whose PRODUCT is an exit code.
//!
//! # The failure it catches
//!
//! A backtest that runs for an hour on a store that was missing three weeks in the middle — the
//! surface design's §8.4 (`docs/superpowers/specs/2026-09-20-cli-data-surface-design.md`). Every
//! other read verb in [`crate::cmd::data`] renders that store faithfully and leaves the DECISION to
//! a person: `ls` prints a span, `gaps` prints holes, `health` prints contradictions. None of them
//! answers *is this enough to run on*, and a CI step cannot read a table. This verb is the seam
//! `vike-cli backtest gate` already is for the compute plane, pointed at the data the compute plane
//! reads.
//!
//! It needs nothing new on the wire, which is why §11 lets it ride P1 or P2: the evidence is
//! `inventory()` and `series_gaps`, both of which ship and both of which `crate::cmd::data`'s
//! `execute_list` already drives.
//!
//! # The verdict is a DOCUMENT; the rung is its summary
//!
//! §7.1 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: *"The verdict is a
//! document naming every criterion that passed and failed, never a bare number."* So [`lines`] and
//! [`json_criteria`] render EVERY criterion — on a pass as well as on a breach, because a CI step
//! that prints its gate's verdict on success is the normal case — and [`rung`] computes the exit
//! from the same `Vec<Judgement>` the renderers walk. Splitting the words from the number over two
//! passes is how a renderer comes to say `pass` while the process exits on a breach, which is the
//! drift `crates/vike-cli/src/cmd/runs/gate.rs` splits `render_human` from `rung` to prevent.
//!
//! [`Outcome`] is REUSED from `crates/vike-cli/src/cmd/runs/failif.rs` rather than re-declared
//! here, and that is the load-bearing half: its DECLARATION ORDER is the ranking (`Breach` beats
//! `Unevaluated` beats `Pass`), so a real breach can never be masked by a criterion that went
//! unevaluated, and an unevaluated one can never read as a pass. A second three-variant enum in
//! this file would be a second ranking to keep in step and a second set of words for an operator to
//! learn — `data hist gate` printing `missing` where `backtest gate` prints `unevaluated`.
//!
//! ⚠ **The two functions that TURN that ranking into a number and a word live there too**, and the
//! first cut of this file did not: `rung` and `verdict_word` were copied in, byte-identical to
//! `crates/vike-cli/src/cmd/runs/gate.rs`'s, which is precisely the drift the paragraph above
//! claims to prevent arriving one level out. A PR retuning the compute plane's vocabulary — say
//! `Exit::Empty` printing `not-evaluated` to match a new report field — would have left this verb
//! printing `unevaluated`, both planes compiling and both suites green, each asserting its own
//! copy. `crate::cmd::runs::failif::rung` and `…::verdict_word` are now the one declaration of
//! each, and [`rung`] below is a projection onto them rather than a second match.
//!
//! # ⚠ `--require-days` measures the recorded SPAN, not the days that are actually there
//!
//! The days criterion is `(last_ts - first_ts)` in whole days, folded from the coverage
//! `inventory()` already carries — so a series whose span is a year and whose middle three weeks
//! are missing PASSES `--require-days 365`. That is exactly the failure this verb exists for, and
//! it is why `--max-gap` is not decoration: the two criteria are the two halves of *"a whole year,
//! whole"*, and neither one alone is the question. The human rendering therefore DISCLOSES a run
//! that named no `--max-gap` — a gate that silently checked one half is the "green means nothing
//! ran" shape this repository is organised against.
//!
//! The alternative — subtracting the holes and gating COVERED days — was rejected: it makes
//! `--require-days` cost one round trip per series even when nobody asked about holes, and it turns
//! an exchange's weekly maintenance window into a slow erosion of the number an operator wrote
//! down. Two criteria that each say one thing beat one criterion that says two.
//!
//! # ⚠ A gap is a WHOLE UTC DAY, so `--max-gap 4h` means "no missing day at all"
//!
//! MEASURED, not assumed: `crates/vike-data/src/datafusion_hist.rs`'s `series_gaps` derives its
//! holes from the manifest's `date=` partition set and widens each one through
//! `crates/vike-data/src/datafusion_hist/gaps.rs`'s `day_gap_to_ms_range`, so the SMALLEST gap this
//! store can report is one whole day. A tolerance below 24h is therefore satisfiable only by a
//! series with no missing day whatsoever. That is a perfectly good assertion and the flag is not
//! refused for it — §7.0's own example spells `--max-gap 4h` — but an operator who believes they
//! are tolerating a four-hour outage is believing something the store cannot express, so the human
//! rendering says so.
//!
//! # Three rungs, and the two ways to reach the third
//!
//! `crates/vike-cli/src/exit.rs` declares all three. `Ok` when the store holds what was required;
//! `Breach` when a DECLARED THRESHOLD was breached (the command WORKED — a wrapper re-runs a `1`
//! and escalates this); `Empty` when nothing was evaluated, so that *the gate passed* and *the gate
//! checked nothing* stop sharing a number.
//!
//! `Empty` is reached two ways and they are different mistakes:
//!
//! * **the SPEC matched no stored series of any kind** — `crate::cmd::data`'s `execute_gate`
//!   refuses on that rung before any criterion exists, because there is nothing to judge. The
//!   operator named a series this store does not have: a typo, or the wrong datahub.
//! * **a criterion could not be evaluated** — a gap probe that failed, or a span the store reports
//!   as ending before it begins. [`rung`] folds either onto the same number.
//!
//! What is deliberately NOT `Empty` is a REQUIRED KIND the store does not hold: the operator
//! declared it required, so its absence is a BREACH. The two are different actions — *fix your
//! command* against *go and fetch that tape* — and a gate that answered one number for both would
//! send a CI step to the wrong end of the pipe.
//!
//! # ⚠ This side never CONSTRUCTS a series identity
//!
//! [`Spec`] is a SELECTOR over what `inventory()` returned, never a `vike_data::SeriesId` built out
//! of a colon-string. `crate::cmd::data`'s `Sub::Gaps` doc carries the whole argument — a series is
//! four dimensions with a grouped/per-symbol alternative inside them, and a verb that guessed which
//! one an operator meant would hand the server an id matching nothing. The ids come back from the
//! server and go straight back to it; this module only decides which of them the gate is ABOUT.
//!
//! The match is EXACT rather than the case-insensitive substring `crate::cmd::data`'s `Filter`
//! applies, and that difference is the difference between browsing and asserting. A filter that
//! matched too widely costs a reader one extra row; a GATE that matched too widely passes on a
//! series the operator did not name, which is a green build over the wrong data.

use crate::cmd::runs::failif::{self, Outcome, verdict_word};
use crate::exit::Exit;
// ⚠ The day constant is IMPORTED rather than declared, and the correction is worth keeping. This
// file opened with a local `const DAY_MS: i64 = 86_400_000;` whose doc read "`vike-model`, which
// this crate does link, exports no day constant". That sentence was FALSE when it was written:
// `crates/vike-model/src/order.rs`'s `MS_PER_DAY` is the constant, and
// `crates/vike-model/src/lib.rs` re-exports it at the crate ROOT. It was inherited verbatim from
// `crate::cmd::data::tape_health`'s own twin — corrected with it — and the half of that argument
// that IS true (`vike-data` is a DEV-dependency here, so `crates/vike-data/src/coverage.rs`'s copy
// cannot be imported) never carried the `vike-model` half.
use vike_model::time::{Span, parse_span};
use vike_model::{MS_PER_DAY, epoch_ms_to_utc_date};

use super::col;

/// The machine token for "the store holds a series of this kind, matching the spec".
///
/// A `const` per criterion rather than a literal at the two sites that spell it (the judgement and
/// the `--json` document), because a wrapper alerting on one of these must not have to match on
/// prose — the same rule `crate::cmd::data::tape_health`'s `Finding` gives for its `code`.
const KIND_PRESENT: &str = "require-kind";
/// The machine token for the recorded-span criterion.
const REQUIRE_DAYS: &str = "require-days";
/// The machine token for the largest-hole criterion.
const MAX_GAP: &str = "max-gap";

/// The kind a gate is about when the operator names none — §7.1's rule for this grammar, spelled
/// once.
///
/// ⚠ A DEFAULT rather than a requirement, because the overwhelming case is a bar series and making
/// every line carry `--require-kind bar` would be a flag that says what the spec's own INTERVAL
/// part already implies. It is not a roster: one value, which the operator overrides by naming any
/// kind they like — this side validates no kind against any table, for `crate::cmd::data`'s stated
/// reason that the reachable set belongs to a remote process this crate cannot see at parse time.
pub(super) const DEFAULT_KIND: &str = "bar";

/// The ONE store kind whose leaf sub-partitions by bar step — and therefore the only kind a spec's
/// third part can ever select.
///
/// ⚠ **Not [`DEFAULT_KIND`] wearing a second hat, though both spell `bar` today.** One answers
/// *what is a gate about when nobody said*; this one answers *what can the store's path layout
/// express*. Folding them into one constant would make a change to either silently move the other,
/// and they are changed by different arguments.
///
/// ⚠ **A value this crate cannot import, so it is PINNED rather than asserted.**
/// `crates/vike-data/src/store_kind.rs` is the layout authority and `vike-data` is a
/// DEV-dependency here (the type wall `crate::cmd::data::tape_health`'s module doc names), so no
/// production path can read it. The test build CAN —
/// `the_interval_bearing_kind_is_the_one_the_store_itself_declares` below folds
/// `vike_data::store_kind::STORE_KINDS` and fails on any other answer, which is the same shape
/// `crates/vike-model/src/store_plane.rs`'s `ACCOUNT_KINDS` is held to by
/// `crates/vike-data/tests/store_kind_gate.rs`. It is deliberately NOT a third declaration in
/// `vike-model`: one value that a test can pin against the authority DIRECTLY needs no hop.
const INTERVAL_BEARING_KIND: &str = "bar";

// ─── the SPEC, as a selector ─────────────────────────────────────────────────────────────────────

/// What the gate is about: one venue, one series NAME, and optionally one bar interval.
///
/// §7.1's grammar, and only the part of it this verb needs: `VENUE:SYMBOL[:INTERVAL]` for a
/// per-symbol series, `VENUE:@GROUP` for a grouped one. The `@` is not decoration — a grouped
/// series' label is its GROUP and a per-symbol series' is its SYMBOL, the two are different
/// namespaces, and a selector that ignored the distinction would match a group whose name happens
/// to spell a symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Spec {
    pub(super) venue: String,
    /// The symbol, or the group with its `@` already stripped — `vike_data::SeriesId`'s `label()`
    /// on the far side.
    pub(super) name: String,
    /// `true` when the spec spelled `@NAME`.
    pub(super) grouped: bool,
    /// `Some` narrows to one bar step; `None` leaves the interval unconstrained, which is what a
    /// tick-shaped kind needs.
    pub(super) interval: Option<String>,
}

impl Spec {
    /// Does this stored series belong to the gate's subject?
    ///
    /// EXACT on every dimension the spec named, and silent about the one it did not: an absent
    /// `interval` matches a series of any step INCLUDING one with none, which is what makes
    /// `polymarket:@election-2026` reach a `book` series while `binance:BTCUSDT:1h` reaches exactly
    /// one bar series.
    ///
    /// ⚠ The corollary, which is why [`refuse_a_kind_the_spec_can_never_select`] exists: a spec
    /// that DID name an interval can never match a tick-shaped series, because every one of those
    /// carries `interval: None` (`crates/vike-data/src/series.rs`'s `SeriesId::interval` — "`Some`
    /// for bars; `None` for tick series"). The exactness is right; accepting a criterion it makes
    /// unsatisfiable was not.
    pub(super) fn matches(
        &self,
        venue: &str,
        name: &str,
        grouped: bool,
        interval: Option<&str>,
    ) -> bool {
        self.venue == venue
            && self.name == name
            && self.grouped == grouped
            && match &self.interval {
                None => true,
                Some(want) => interval == Some(want.as_str()),
            }
    }

    /// The spec as the operator would have typed it — rebuilt from the parsed parts rather than
    /// echoed from the raw string, so the header line and the `--json` document cannot disagree
    /// about what was actually selected (a trimmed part, a stripped `@`).
    pub(super) fn text(&self) -> String {
        let at = if self.grouped { "@" } else { "" };
        match &self.interval {
            Some(i) => format!("{}:{at}{}:{i}", self.venue, self.name),
            None => format!("{}:{at}{}", self.venue, self.name),
        }
    }
}

/// Parse the positional spec, or refuse it with the sentence that names the shape.
///
/// ⚠ **A SEPARATE parser from `crate::cmd::data`'s `check_spec`, and deliberately not a widening of
/// it.** That one demands three non-empty parts because a `fetch` always writes `kind=bar` per
/// symbol and has no other shape to express — relaxing it there would let a spec that cannot be
/// fetched reach the wire. This verb's spec is a SELECTOR over series that already exist, so the
/// two spellings §7.1 declares (an optional interval, and a `@GROUP` alternative) are exactly the
/// shapes it has to accept. One parser could not answer both without one of the two verbs holding a
/// shape it cannot use.
pub(super) fn parse_spec(raw: &str) -> Result<Spec, String> {
    const WANT: &str = "VENUE:SYMBOL[:INTERVAL] for a per-symbol series, or VENUE:@GROUP for a \
                        grouped one — e.g. binance:BTCUSDT:1h, polymarket:@election-2026";
    let parts: Vec<&str> = raw.split(':').map(str::trim).collect();
    if parts.len() < 2 || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("'{raw}' is not a series spec — {WANT}"));
    }
    let (venue, label) = (parts[0], parts[1]);
    let grouped = label.starts_with('@');
    let name = label.strip_prefix('@').unwrap_or(label);
    if name.is_empty() {
        return Err(format!(
            "'{raw}' names an EMPTY group: `@` marks a GROUPED series and the group's name follows \
             it — {WANT}"
        ));
    }
    let interval = parts.get(2).map(|i| (*i).to_string());
    // ⚠ The same rule `crate::cmd::data`'s `parse` applies to `--group` with `--interval` on `rm`
    // and `repair`: a grouped series' leaf carries no `interval=` segment at all, so an interval
    // here could only ever select nothing — and a gate that selects nothing is the rung that says
    // the operator named a series the store does not have, which would be the wrong diagnosis.
    if grouped && interval.is_some() {
        return Err(format!(
            "'{raw}': an interval does not apply to a GROUPED series — its leaf has no `interval=` \
             segment at all, so this spec could only ever select nothing. Drop the third part"
        ));
    }
    Ok(Spec { venue: venue.to_string(), name: name.to_string(), grouped, interval })
}

/// Refuse a line whose SPEC and whose CRITERIA cannot both be satisfied by any store.
///
/// ⚠ **The mirror of [`parse_spec`]'s grouped refusal, and it exists because only one of the two
/// halves was written.** That one argues: a grouped series' leaf carries no `interval=` segment, so
/// a grouped spec with a third part could only ever select nothing. The identical argument holds
/// one dimension over — [`INTERVAL_BEARING_KIND`] is the only kind whose leaf carries one, so a
/// spec that named an interval can only ever select `bar` series, and a `--require-kind` naming
/// anything else is a criterion [`Spec::matches`] rejects every candidate for.
///
/// Without this, the line is accepted, costs an `inventory()` round trip, and BREACHES with
/// `none (this spec holds: bar)` — which reads as *go and fetch that tape* about a tape that is
/// already on disk. A structurally unsatisfiable criterion is a command-line mistake and must be
/// diagnosed as one; that is the whole reason [`crate::exit::Exit::Breach`] and
/// [`crate::exit::Exit::Empty`] are different rungs.
///
/// PURE, and called from `crate::cmd::data`'s `Sub::Gate` arm — before a socket is opened, for
/// `parse_max_gap`'s stated reason: nothing here is forwarded, so the far side could never refuse
/// it.
pub(super) fn refuse_a_kind_the_spec_can_never_select(
    spec: &Spec,
    kinds: &[String],
) -> Result<(), String> {
    let Some(interval) = &spec.interval else { return Ok(()) };
    let Some(kind) = kinds.iter().find(|k| k.as_str() != INTERVAL_BEARING_KIND) else {
        return Ok(());
    };
    // The remedy spelling is `VENUE:NAME` with no `@`: [`parse_spec`] has already refused an
    // interval on a GROUPED spec, so a spec reaching here is per-symbol by construction.
    Err(format!(
        "'{}' names the bar step `{interval}` and `--require-kind {kind}` names a kind that has \
         no step at all — only `{INTERVAL_BEARING_KIND}` series sub-partition by one, so this spec \
         could only ever select nothing of kind `{kind}` and the gate would BREACH over a tape \
         that is already in the store. Drop the third part — `{}:{}` gates every kind you named, \
         across every bar step — or gate `{kind}` in a run of its own",
        spec.text(),
        spec.venue,
        spec.name,
    ))
}

// ─── the criteria, parsed ────────────────────────────────────────────────────────────────────────

/// `--require-days N` — a whole positive count of days whose millisecond threshold fits an `i64`.
///
/// ⚠ ZERO is refused rather than accepted as a vacuous pass. `--require-days 0` is satisfied by an
/// empty store, so a gate carrying it exits 0 having asserted nothing about the data — the one
/// answer a CI step must never get, and the reason `crates/vike-cli/src/cmd/runs/gate.rs`'s
/// `refuse_an_ungateable_line` refuses an empty `--fail-if` at the door rather than evaluating it
/// to a pass.
///
/// ⚠ **And so is a count whose `* MS_PER_DAY` does not FIT**, which is the same failure wearing
/// arithmetic. [`judge_days`] compares against `require_days * MS_PER_DAY`, and a release build
/// wraps rather than panicking: `--require-days 200000000000` (a script variable holding
/// nanoseconds, or one zero too many) computes `1.728e19`, wraps to about `-1.12e18`, and EVERY
/// span is then `>=` it — a gate that exits 0 having asserted nothing, reached by the one route
/// the zero refusal above does not cover. The discipline is
/// `vike_model::time::parse_span`'s, whose every conversion is a `checked_mul` refusing with
/// "count does not fit"; this parser dropped it and now keeps it.
pub(super) fn parse_require_days(raw: &str) -> Result<i64, String> {
    let v = raw.trim();
    let n: i64 = v.parse().map_err(|_| {
        format!("--require-days {raw:?} is not a whole number of days (e.g. --require-days 365)")
    })?;
    if n <= 0 {
        return Err(format!(
            "--require-days {n} asserts nothing: a span of zero days or less is what an EMPTY \
             series already has, so the gate would exit 0 over a store holding no rows. Name the \
             history the run actually needs"
        ));
    }
    if n.checked_mul(MS_PER_DAY).is_none() {
        return Err(format!(
            "--require-days {n} does not fit: {n} days is more milliseconds than a 64-bit count \
             holds, so the threshold this gate would compare against cannot be computed. No store \
             holds that span — the largest one that can be asked for is {} days",
            i64::MAX / MS_PER_DAY
        ));
    }
    Ok(n)
}

/// `--max-gap D` — a fixed wall-clock duration, in epoch-ms.
///
/// ⚠ **No new duration parser.** `vike_model::time::parse_span`
/// (`crates/vike-model/src/time.rs`) is this workspace's grammar for exactly this shape — `4h`,
/// `1d`, `2w` — and it is already reachable here, `vike-model` being a normal dependency of this
/// crate. What this function adds is the narrowing to the subset a GAP TOLERANCE can be, and the
/// two refusals below are `crates/vike-backtest/src/harness/profile.rs`'s `max_gap_ms` arguments,
/// which govern the same key on the compute plane. They are stated here rather than delegated
/// because nothing is forwarded: this flag is consumed in this process, so the far side never sees
/// it and could not refuse it.
pub(super) fn parse_max_gap(raw: &str) -> Result<i64, String> {
    let span = parse_span(raw).map_err(|e| format!("--max-gap {raw:?}: {e}"))?;
    match span {
        Span::Ms(ms) if ms > 0 => Ok(ms),
        // Unreachable through `parse_span`, which refuses a zero count by name — kept because a
        // non-positive tolerance would mean "tolerate nothing", which is `--max-gap 1d` on a store
        // whose smallest reportable gap is a day, i.e. a spelling that can only mislead.
        Span::Ms(ms) => Err(format!(
            "--max-gap {raw:?} resolves to {ms}ms — a non-positive tolerance tolerates nothing, \
             which this verb already spells as the absence of a gap. Name a real duration"
        )),
        Span::Bars(n) => Err(format!(
            "--max-gap {raw:?} is a BAR COUNT, and a hole in this store is a wall-clock span in \
             epoch-ms: converting {n} bars to a duration needs an interval, and a tick series has \
             none. Write the tolerance as fixed time — \"1d\", \"7d\""
        )),
        Span::Months(_) => Err(format!(
            "--max-gap {raw:?} is a CALENDAR span, and a calendar month is not a fixed number of \
             milliseconds (one month from 31 January is 28 days, from 31 March 30) — so the \
             tolerance would depend on where the hole happens to sit, and two gates naming the \
             same tolerance would accept different amounts. Write it in fixed time — \"30d\""
        )),
    }
}

// ─── the evidence ────────────────────────────────────────────────────────────────────────────────

/// One stored series the [`Spec`] selected, flattened out of the wire types on arrival.
///
/// Flattened for `crate::cmd::data::tape_health`'s `SeriesFacts` reason: the wire types name a
/// DEV-dependency, so this side reads their public fields and keeps none of their shapes — and
/// every judgement below is then a pure function over plain numbers, unit-testable against a
/// planted store nobody has to build.
///
/// The venue is deliberately absent: the spec matched it EXACTLY, so every candidate carries the
/// same one and a per-row copy would be a column that cannot vary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) kind: String,
    /// `Some` for bars, `None` for every tick-shaped kind — the store's own answer.
    pub(super) interval: Option<String>,
    pub(super) first_ts: i64,
    pub(super) last_ts: i64,
    pub(super) rows: u64,
    /// The holes, inclusive epoch-ms, as `series_gaps` answered. `None` means the probe was not
    /// MADE — either no `--max-gap` was given, or this series' kind is not one the gate is about —
    /// and an EMPTY `Some` is the real "this series has no holes". `gaps_error` tells a failed
    /// probe from an unasked one.
    pub(super) gaps: Option<Vec<(i64, i64)>>,
    /// Why this one series' gap probe failed, when it did.
    pub(super) gaps_error: Option<String>,
}

impl Candidate {
    /// The recorded span in milliseconds, or `None` when the store's own numbers do not describe
    /// one.
    ///
    /// Two shapes answer `None`, and they are different facts the caller must not merge with a
    /// real span of zero:
    ///
    /// * the EMPTY FOLD — zero rows and an all-zero coverage, which is how a store says "this
    ///   series has no rows" (`crate::cmd::data`'s `span_cell` renders it as `-` for the same
    ///   reason). That is a span of nothing and the days criterion BREACHES on it.
    /// * an INVERTED span — `last_ts` before `first_ts`, which is impossible of any series that
    ///   exists and is precisely what `data hist health` reports as `span-inverted`. Nothing
    ///   honest can be concluded from it, so the days criterion goes UNEVALUATED.
    ///
    /// They are told apart by [`Candidate::empty_fold`] rather than by this function, which
    /// answers only about the arithmetic.
    pub(super) fn span_ms(&self) -> Option<i64> {
        if self.empty_fold() {
            return Some(0);
        }
        if self.last_ts < self.first_ts {
            return None;
        }
        Some(self.last_ts - self.first_ts)
    }

    /// The store's own representation of "no rows": zero rows AND an all-zero coverage. The pair,
    /// never `rows == 0` alone — a zero-row series carrying real timestamps is a contradiction
    /// rather than an emptiness, and `data hist health` is the verb that names it.
    pub(super) fn empty_fold(&self) -> bool {
        self.rows == 0 && self.first_ts == 0 && self.last_ts == 0
    }

    /// The LARGEST hole in this series, in milliseconds, or `None` when no probe was made.
    ///
    /// ⚠ The bounds `series_gaps` returns are INCLUSIVE, so a hole's length is `to - from + 1` —
    /// which makes one missing UTC day measure exactly 24h rather than one millisecond short of
    /// it. Dropping the `+1` would report a day-long hole as 86_399_999ms, a number that appears
    /// in no grammar an operator can type and that reads as an off-by-one in the store.
    pub(super) fn largest_gap_ms(&self) -> Option<i64> {
        let ranges = self.gaps.as_ref()?;
        ranges.iter().map(|(from, to)| to - from + 1).max().or(Some(0))
    }
}

// ─── the judgement ───────────────────────────────────────────────────────────────────────────────

/// One criterion, judged, carrying every cell the renderers print so nothing is recomputed between
/// the table and the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Judgement {
    /// The STABLE machine token — see [`KIND_PRESENT`].
    pub(super) criterion: &'static str,
    /// WHAT was judged: a kind for a presence criterion, one series of it for the rest.
    pub(super) subject: String,
    /// What the operator asked for, rendered.
    pub(super) required: String,
    /// What the store answered, rendered.
    pub(super) observed: String,
    pub(super) outcome: Outcome,
}

/// Judge every criterion the line declared against every series the spec matched.
///
/// PURE — `matched` is the whole spec-matched set of ANY kind, because the two questions this verb
/// asks need different slices of it: the presence criterion asks whether a declared kind is in
/// there, and needs to be able to name the kinds that ARE when it is not; the per-series criteria
/// run only over the declared kinds, because a `properties` grid's span says nothing about whether
/// a backtest can run and gating one would redden every instrument that has one.
///
/// The ORDER is the declaration order of `kinds` first, then `matched`'s own order — so a caller
/// that sorts its candidates gets a byte-identical verdict from two runs over one store, which is
/// what makes this diffable across a backfill.
pub(super) fn judge(g: &super::GateArgs, matched: &[Candidate]) -> Vec<Judgement> {
    let mut out = Vec::new();
    // The kinds this spec actually holds, computed ONCE and rendered only into a breach — see the
    // `observed` cell below for why a bare count is not enough to act on.
    let held: Vec<&str> = {
        let mut k: Vec<&str> = matched.iter().map(|c| c.kind.as_str()).collect();
        k.sort_unstable();
        k.dedup();
        k
    };
    for kind in &g.kinds {
        let n = matched.iter().filter(|c| &c.kind == kind).count();
        out.push(Judgement {
            criterion: KIND_PRESENT,
            subject: kind.clone(),
            required: "1 series or more".to_string(),
            // ⚠ The breach names what the store DOES hold for this spec, because the count alone
            // cannot be acted on: an operator told "0 series" has to run a second command to learn
            // whether they typed the wrong kind or genuinely have not fetched the tape.
            observed: if n == 0 {
                format!("none (this spec holds: {})", held.join(", "))
            } else {
                format!("{n} series")
            },
            outcome: if n == 0 { Outcome::Breach } else { Outcome::Pass },
        });
    }
    for c in matched.iter().filter(|c| g.kinds.iter().any(|k| k == &c.kind)) {
        out.push(judge_days(g.require_days, c));
        if let Some(max) = g.max_gap_ms {
            out.push(judge_gap(max, c));
        }
    }
    out
}

/// The subject cell for a per-series criterion: the kind and, for a bar series, its step. The
/// venue and the name are the SPEC's and are printed once in the header rather than on every row.
fn subject_of(c: &Candidate) -> String {
    match &c.interval {
        Some(i) => format!("{} {i}", c.kind),
        None => c.kind.clone(),
    }
}

/// `--require-days` against one series' recorded span. See [`Candidate::span_ms`] for the two
/// shapes that are not a span at all.
fn judge_days(require_days: i64, c: &Candidate) -> Judgement {
    let required = format!(">={require_days}d");
    let subject = subject_of(c);
    match c.span_ms() {
        Some(ms) => Judgement {
            criterion: REQUIRE_DAYS,
            subject,
            required,
            observed: format!(
                "{}d ({} .. {})",
                ms / MS_PER_DAY,
                span_end(c, c.first_ts),
                span_end(c, c.last_ts)
            ),
            // ⚠ SATURATING, not `*`. [`parse_require_days`] refuses every count that would not
            // fit, so this cannot saturate through the verb — and if it ever did, `i64::MAX` is
            // the direction that FAILS CLOSED. A wrapping multiply is the other one: it produces
            // a negative threshold every span satisfies, which is a gate exiting 0 having
            // asserted nothing.
            outcome: if ms >= require_days.saturating_mul(MS_PER_DAY) {
                Outcome::Pass
            } else {
                Outcome::Breach
            },
        },
        None => Judgement {
            criterion: REQUIRE_DAYS,
            subject,
            required,
            observed: format!("last_ts {} is BEFORE first_ts {}", c.last_ts, c.first_ts),
            outcome: Outcome::Unevaluated(
                "the store reports a span that ends before it begins, so no honest day count can \
                 be derived from it — `vike-cli data hist health` names this contradiction"
                    .to_string(),
            ),
        },
    }
}

/// A span endpoint as a UTC date, or `-` for a series the store folded to empty — the same rule
/// (and the same reason) `crate::cmd::data`'s `span_cell` applies: a zero-row series' `0` is a
/// perfectly well-formed `1970-01-01` that reads as data.
fn span_end(c: &Candidate, ts: i64) -> String {
    if c.empty_fold() { "-".to_string() } else { epoch_ms_to_utc_date(ts) }
}

/// `--max-gap` against one series' largest hole.
///
/// ⚠ A FAILED probe is `Unevaluated`, never a pass — the whole reason this rung exists. That is a
/// deliberate divergence from `crate::cmd::data`'s `execute_list`, where a failed probe DEGRADES
/// the row and leaves the listing whole: a listing is what was asked for and the gaps annotate it,
/// while here the probe IS the evidence and a criterion with no evidence has not been checked.
fn judge_gap(max_gap_ms: i64, c: &Candidate) -> Judgement {
    let required = format!("<={}", duration_cell(max_gap_ms));
    let subject = subject_of(c);
    if let Some(e) = &c.gaps_error {
        return Judgement {
            criterion: MAX_GAP,
            subject,
            required,
            observed: "unavailable".to_string(),
            outcome: Outcome::Unevaluated(format!("the gap probe failed: {e}")),
        };
    }
    match c.largest_gap_ms() {
        Some(ms) => Judgement {
            criterion: MAX_GAP,
            subject,
            required,
            observed: if ms == 0 {
                "no gaps".to_string()
            } else {
                format!("{} largest of {}", duration_cell(ms), c.gaps.as_ref().map_or(0, Vec::len))
            },
            outcome: if ms <= max_gap_ms { Outcome::Pass } else { Outcome::Breach },
        },
        // Unreachable through `execute_gate`, which probes every series it is about to judge —
        // spelled as an honest sentence rather than a panic, because a renderer has no business
        // aborting a run that already succeeded.
        None => Judgement {
            criterion: MAX_GAP,
            subject,
            required,
            observed: "not probed".to_string(),
            outcome: Outcome::Unevaluated("no gap probe was made for this series".to_string()),
        },
    }
}

/// A duration as the operator's own grammar spells it — whole days where it divides, hours where it
/// divides, milliseconds otherwise.
///
/// ⚠ It renders `vike_model::time::parse_span`'s vocabulary and nothing else, so a value printed
/// here can be pasted straight back into `--max-gap`. A cell reading `1.5d` would name a spelling
/// that grammar refuses.
fn duration_cell(ms: i64) -> String {
    if ms % MS_PER_DAY == 0 {
        format!("{}d", ms / MS_PER_DAY)
    } else if ms % 3_600_000 == 0 {
        format!("{}h", ms / 3_600_000)
    } else {
        format!("{ms}ms")
    }
}

/// The RUNG the same judgement set exits on — the twin of [`lines`], so the words and the number
/// are decided over one `Vec<Judgement>` and cannot come to disagree about which criterion is
/// which.
///
/// ⚠ **The MAPPING is not here.** `crate::cmd::runs::failif::rung` owns it, beside the [`Outcome`]
/// whose declaration order IS the ranking; what this function contributes is the projection from
/// this module's own `Judgement` to its outcome, and nothing else. It used to be a second copy of
/// the match — byte-identical to `crates/vike-cli/src/cmd/runs/gate.rs`'s modulo comments — which
/// meant a PR retuning one plane's vocabulary left the other printing the old word while both
/// suites passed, each asserting its own copy. The module doc above argues at length against a
/// second `Outcome`; copying the two functions that produce its words and its number was the same
/// mistake one level out.
pub(super) fn rung(judgements: &[Judgement]) -> Exit {
    failif::rung(judgements.iter().map(|j| &j.outcome))
}

// ─── rendering ───────────────────────────────────────────────────────────────────────────────────

/// The human verdict — PURE, so every column rule and every disclosure below is unit-tested rather
/// than only seen.
///
/// `reported` is how many series the datahub enumerated in total, which is what tells an operator
/// whose gate matched two series out of four whether they are looking at the store they meant.
pub(super) fn lines(
    g: &super::GateArgs,
    judgements: &[Judgement],
    exit: Exit,
    matched: usize,
    reported: usize,
) -> Vec<String> {
    let mut out = vec![
        format!("gate {}", g.spec.text()),
        format!("  {matched} of {reported} stored series match this spec"),
        String::new(),
    ];
    let crit_w = col("CRITERION", judgements.iter().map(|j| j.criterion.len()));
    let subj_w = col("SUBJECT", judgements.iter().map(|j| j.subject.len()));
    let req_w = col("REQUIRED", judgements.iter().map(|j| j.required.len()));
    let obs_w = col("OBSERVED", judgements.iter().map(|j| j.observed.len()));
    out.push(format!(
        "  {:<crit_w$}  {:<subj_w$}  {:<req_w$}  {:<obs_w$}  VERDICT",
        "CRITERION", "SUBJECT", "REQUIRED", "OBSERVED"
    ));
    for j in judgements {
        out.push(format!(
            "  {:<crit_w$}  {:<subj_w$}  {:<req_w$}  {:<obs_w$}  {}",
            j.criterion,
            j.subject,
            j.required,
            j.observed,
            j.outcome.word()
        ));
        // ⚠ The REASON goes on its own line under the row rather than being left to be discovered.
        // An unevaluated criterion whose row said only `unevaluated` is the shape that makes a
        // person believe the gate checked something.
        if let Outcome::Unevaluated(why) = &j.outcome {
            out.push(format!("  {:<crit_w$}  {why}", ""));
        }
    }
    for note in notes(g) {
        out.push(String::new());
        out.push(format!("  note: {note}"));
    }
    out.push(String::new());
    let n = judgements.iter().filter(|j| j.outcome != Outcome::Pass).count();
    out.push(format!(
        "  {} — {n} of {} criteria",
        verdict_word(exit).to_uppercase(),
        judgements.len()
    ));
    out
}

/// What this run did NOT check, in the operator's own terms — see the module doc's two ⚠ sections.
///
/// ⚠ **The disclosures go to the HUMAN rendering only**, the rule `crate::cmd::data`'s
/// `account_exclusion_note` already follows: `--json` is consumed by a program, and a note appended
/// to a document is noise at best and a parse error at worst. The document carries the FACTS both
/// notes are derived from — a null `max_gap_ms`, and its value when it is not null — so a consumer
/// can re-derive either rather than be told.
fn notes(g: &super::GateArgs) -> Vec<String> {
    let mut out = Vec::new();
    match g.max_gap_ms {
        None => out.push(
            "the HOLES inside each span were not checked — this gate judged how much time the \
             store covers, not whether it covers it WHOLE. `--max-gap 1d` is the other half"
                .to_string(),
        ),
        // Below one day the tolerance is unreachable rather than wrong — see the module doc's
        // measurement of the store's gap resolution.
        Some(ms) if ms < MS_PER_DAY => out.push(format!(
            "`--max-gap {}` is finer than this store can answer: a gap is derived from the \
             `date=` partition set, so the smallest hole it can report is a whole UTC day. This \
             criterion therefore means `no missing day at all`",
            duration_cell(ms)
        )),
        Some(_) => {}
    }
    out
}

/// The `criteria` array of the `--json` document — one object per judged criterion, in the same
/// order the table renders them.
///
/// `why` is `null` for everything but an unevaluated criterion, which is the shape
/// `crates/vike-cli/src/cmd/runs/gate.rs`'s own document already has: a reader's key set does not
/// change between runs.
pub(super) fn json_criteria(judgements: &[Judgement]) -> Vec<serde_json::Value> {
    judgements
        .iter()
        .map(|j| {
            serde_json::json!({
                "criterion": j.criterion,
                "subject": j.subject,
                "required": j.required,
                "observed": j.observed,
                "verdict": j.outcome.word(),
                "why": match &j.outcome {
                    Outcome::Unevaluated(why) => serde_json::Value::String(why.clone()),
                    _ => serde_json::Value::Null,
                },
            })
        })
        .collect()
}

/// The `series` array of the `--json` document — the EVIDENCE every verdict above was derived
/// from, so a consumer can re-derive a judgement rather than trust it.
///
/// ⚠ The store's numbers are carried UNTOUCHED, a zero-row series' all-zero timestamps included —
/// which [`lines`] renders as `-`. A render may decline to show a sentinel; a document may not edit
/// one, because a caller folding several stores' verdicts would have no way to tell this side's
/// judgement from a field the server never sent.
pub(super) fn json_series(matched: &[Candidate]) -> Vec<serde_json::Value> {
    matched
        .iter()
        .map(|c| {
            serde_json::json!({
                "kind": c.kind,
                "interval": c.interval,
                "first_ts": c.first_ts,
                "last_ts": c.last_ts,
                "rows": c.rows,
                "span_ms": c.span_ms(),
                "span_days": c.span_ms().map(|ms| ms / MS_PER_DAY),
                "largest_gap_ms": c.largest_gap_ms(),
                // Gap ranges are OBJECTS, never two-element arrays, for the reason
                // `crate::cmd::data`'s `list_json` gives: the wire shape is a tuple and a caller
                // reading `g[0]` has to remember an order nothing in the document states.
                "gaps": c.gaps.as_ref().map(|ranges| {
                    ranges
                        .iter()
                        .map(|(from, to)| serde_json::json!({"from_ts": from, "to_ts": to}))
                        .collect::<Vec<_>>()
                }),
                "gaps_error": c.gaps_error,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(raw: &str) -> Spec {
        parse_spec(raw).unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn gate_args(
        spec: Spec,
        require_days: i64,
        max_gap_ms: Option<i64>,
        kinds: &[&str],
    ) -> super::super::GateArgs {
        super::super::GateArgs {
            spec,
            require_days,
            max_gap_ms,
            kinds: kinds.iter().map(|k| (*k).to_string()).collect(),
        }
    }

    fn bars(days: i64) -> Candidate {
        Candidate {
            kind: "bar".to_string(),
            interval: Some("1h".to_string()),
            first_ts: 0,
            last_ts: days * MS_PER_DAY,
            rows: 24 * days as u64,
            gaps: None,
            gaps_error: None,
        }
    }

    /// §7.1's two spellings both parse, and the parts land where the selector reads them. The
    /// interval is OPTIONAL here where `crate::cmd::data`'s `check_spec` requires one — the whole
    /// reason this verb has a parser of its own.
    #[test]
    fn the_spec_grammar_carries_an_optional_interval_and_a_group_alternative() {
        assert_eq!(
            spec("binance:BTCUSDT:1h"),
            Spec {
                venue: "binance".into(),
                name: "BTCUSDT".into(),
                grouped: false,
                interval: Some("1h".into()),
            }
        );
        assert_eq!(
            spec("binance:BTCUSDT"),
            Spec {
                venue: "binance".into(),
                name: "BTCUSDT".into(),
                grouped: false,
                interval: None,
            }
        );
        assert_eq!(
            spec("polymarket:@election-2026"),
            Spec {
                venue: "polymarket".into(),
                name: "election-2026".into(),
                grouped: true,
                interval: None,
            }
        );
        // ...and the round trip, so the header line names what was actually selected.
        for raw in ["binance:BTCUSDT:1h", "binance:BTCUSDT", "polymarket:@election-2026"] {
            assert_eq!(spec(raw).text(), raw);
        }
    }

    /// The shapes that are refused, each for its own stated reason. The ANTI-VACUITY control is the
    /// test above: every refusal here has a sibling spelling that parses, so a parser that refused
    /// everything would redden that one.
    #[test]
    fn a_malformed_spec_is_refused_with_the_shape_it_should_have_had() {
        for (raw, needle) in [
            ("BTCUSDT", "not a series spec"),
            ("binance:BTCUSDT:1h:extra", "not a series spec"),
            ("binance:", "not a series spec"),
            (":BTCUSDT", "not a series spec"),
            ("binance:@", "EMPTY group"),
            ("polymarket:@election-2026:1h", "GROUPED"),
        ] {
            let e = parse_spec(raw).expect_err(raw);
            assert!(e.contains(needle), "{raw}: {e}");
        }
    }

    /// **The MIRROR of that grouped refusal, and the half that was missing.** A spec naming a bar
    /// step and a `--require-kind` naming a kind that has none can never both be satisfied, so the
    /// line is refused before a socket opens — rather than costing an `inventory()` round trip and
    /// then BREACHING over a tape that is already on disk.
    ///
    /// The ANTI-VACUITY control is the second half: three satisfiable shapes, each one character
    /// from a refused one, so a predicate that refused everything reddens here.
    #[test]
    fn a_required_kind_the_spec_could_never_select_is_refused_before_a_socket_opens() {
        let kinds = |k: &[&str]| -> Vec<String> { k.iter().map(|s| (*s).to_string()).collect() };
        let refuse = |raw: &str, k: &[&str]| -> String {
            refuse_a_kind_the_spec_can_never_select(&spec(raw), &kinds(k))
                .expect_err(&format!("{raw} {k:?} can only ever select nothing"))
        };
        let e = refuse("binance:BTCUSDT:1h", &["bar", "trade"]);
        assert!(e.contains("`1h`"), "it quotes the step that did it: {e}");
        assert!(e.contains("--require-kind trade"), "…and the criterion it contradicts: {e}");
        assert!(e.contains("`binance:BTCUSDT` gates"), "…and the spec that WOULD work: {e}");
        assert!(e.contains("BREACH"), "…and what would otherwise have happened: {e}");
        // Every non-bar kind, not a roster of tick kinds: this side validates no kind against any
        // table, so `properties` and a kind nobody has ever declared are refused identically.
        for kind in ["quote", "trade", "book", "depth", "properties", "a_kind_nobody_declared"] {
            assert!(
                refuse("binance:BTCUSDT:1h", &[kind]).contains(kind),
                "{kind} carries no bar step either"
            );
        }

        for (raw, k) in [
            ("binance:BTCUSDT:1h", &["bar"][..]),
            ("binance:BTCUSDT", &["bar", "trade"][..]),
            ("polymarket:@election-2026", &["book"][..]),
        ] {
            assert!(
                refuse_a_kind_the_spec_can_never_select(&spec(raw), &kinds(k)).is_ok(),
                "{raw} {k:?} is satisfiable and must not be refused"
            );
        }
    }

    /// **The pin behind [`INTERVAL_BEARING_KIND`]** — the store's own answer, not this file's.
    /// `vike-data` is a DEV-dependency here, so no production path can read the layout authority;
    /// the TEST build can, which is what turns a local constant into a pin. Its twin one crate
    /// down is `crates/vike-data/src/store_kind.rs`'s own
    /// `grouping_requires_a_row_level_symbol_column`.
    #[test]
    fn the_interval_bearing_kind_is_the_one_the_store_itself_declares() {
        let bearing: Vec<&str> = vike_data::store_kind::STORE_KINDS
            .iter()
            .filter(|k| k.partition == vike_data::store_kind::Partition::SymbolInterval)
            .map(|k| k.kind)
            .collect();
        assert_eq!(
            bearing,
            vec![INTERVAL_BEARING_KIND],
            "the refusal above is only correct while EXACTLY ONE stored kind sub-partitions by \
             interval — a second one means a spec's third part can select more than bars, and the \
             refusal has to name the set instead of the value"
        );
    }

    /// The selector is EXACT on every dimension the spec named and silent about the one it did not.
    /// The `@` is load-bearing: a group and a symbol spelling the same word are different series,
    /// and a selector that ignored it would gate the wrong one.
    #[test]
    fn the_selector_is_exact_and_an_absent_interval_matches_every_step() {
        let s = spec("binance:BTCUSDT:1h");
        assert!(s.matches("binance", "BTCUSDT", false, Some("1h")));
        assert!(!s.matches("binance", "BTCUSDT", false, Some("4h")), "the step was named");
        assert!(!s.matches("bybit", "BTCUSDT", false, Some("1h")), "the venue was named");
        assert!(!s.matches("binance", "BTCUSD", false, Some("1h")), "no substring match");
        assert!(!s.matches("BINANCE", "BTCUSDT", false, Some("1h")), "and no case folding");

        let loose = spec("binance:BTCUSDT");
        assert!(loose.matches("binance", "BTCUSDT", false, Some("1h")));
        assert!(loose.matches("binance", "BTCUSDT", false, None), "a tick series has no step");

        let group = spec("polymarket:@election-2026");
        assert!(group.matches("polymarket", "election-2026", true, None));
        assert!(
            !group.matches("polymarket", "election-2026", false, None),
            "a GROUP and a SYMBOL of the same spelling are different series"
        );
        assert!(!loose.matches("binance", "BTCUSDT", true, None), "…and the reverse");
    }

    /// `--require-days` takes a whole positive count, and ZERO is refused rather than accepted as a
    /// gate that passes over an empty store.
    #[test]
    fn require_days_refuses_the_count_that_would_assert_nothing() {
        assert_eq!(parse_require_days("365"), Ok(365));
        assert_eq!(parse_require_days(" 7 "), Ok(7));
        let z = parse_require_days("0").expect_err("zero");
        assert!(z.contains("asserts nothing"), "{z}");
        let n = parse_require_days("-3").expect_err("negative");
        assert!(n.contains("asserts nothing"), "{n}");
        let x = parse_require_days("a week").expect_err("prose");
        assert!(x.contains("whole number of days"), "{x}");
    }

    /// ⚠ **A count that does not FIT is the zero refusal wearing arithmetic, and it is reached by
    /// a route that one does not cover.** The threshold is `n * MS_PER_DAY`, and a RELEASE build
    /// wraps rather than panicking: 2e11 days is 1.728e19ms, which wraps to about -1.12e18, and
    /// every span is then `>=` it — a gate exiting 0 having asserted nothing. The control is the
    /// boundary itself, which must still parse.
    #[test]
    fn require_days_refuses_a_count_whose_threshold_cannot_be_computed() {
        let biggest = i64::MAX / MS_PER_DAY;
        assert_eq!(parse_require_days(&biggest.to_string()), Ok(biggest), "the boundary parses");
        let e = parse_require_days(&(biggest + 1).to_string()).expect_err("one day past it");
        assert!(e.contains("does not fit"), "{e}");
        assert!(e.contains(&biggest.to_string()), "…and names the largest that does: {e}");
        let n = parse_require_days("200000000000").expect_err("a nanosecond count, fat-fingered");
        assert!(n.contains("does not fit"), "{n}");

        // ...and the judgement itself fails CLOSED for a count the parser never let through: a
        // saturating multiply breaches, where the wrapping one this replaced PASSED over two days.
        let g = gate_args(spec("binance:BTCUSDT:1h"), 200_000_000_000, None, &["bar"]);
        assert_eq!(rung(&judge(&g, &[bars(2)])), Exit::Breach);
    }

    /// `--max-gap` reaches `vike_model::time::parse_span` — this workspace's duration grammar, not a
    /// second one — and narrows it to the subset a wall-clock tolerance can be. The two rejected
    /// variants each say why in terms an operator can act on.
    #[test]
    fn max_gap_reuses_the_workspace_span_grammar_and_narrows_it_to_fixed_time() {
        assert_eq!(parse_max_gap("4h"), Ok(4 * 3_600_000));
        assert_eq!(parse_max_gap("1d"), Ok(MS_PER_DAY));
        assert_eq!(parse_max_gap("2w"), Ok(14 * MS_PER_DAY));
        let bars = parse_max_gap("500bars").expect_err("a bar count");
        assert!(bars.contains("BAR COUNT") && bars.contains("interval"), "{bars}");
        let months = parse_max_gap("3mo").expect_err("a calendar span");
        assert!(months.contains("CALENDAR") && months.contains("31 January"), "{months}");
        // ...and the grammar's own refusals arrive verbatim, prefixed with the flag that carried
        // them, rather than being re-derived here.
        let bare = parse_max_gap("90").expect_err("a bare number");
        assert!(bare.contains("--max-gap") && bare.contains("no unit"), "{bare}");
    }

    /// A hole's length is `to - from + 1`, because `series_gaps` returns INCLUSIVE bounds — so one
    /// missing UTC day measures exactly 24h. The control is the second row: two missing days
    /// measure 48h, which an implementation that clamped to a day would get wrong.
    #[test]
    fn a_holes_length_is_inclusive_so_one_missing_day_is_exactly_a_day() {
        let one = Candidate { gaps: Some(vec![(3 * MS_PER_DAY, 4 * MS_PER_DAY - 1)]), ..bars(10) };
        assert_eq!(one.largest_gap_ms(), Some(MS_PER_DAY));
        let two = Candidate { gaps: Some(vec![(3 * MS_PER_DAY, 5 * MS_PER_DAY - 1)]), ..bars(10) };
        assert_eq!(two.largest_gap_ms(), Some(2 * MS_PER_DAY));
        // The LARGEST, not the first or the sum.
        let many = Candidate {
            gaps: Some(vec![
                (3 * MS_PER_DAY, 4 * MS_PER_DAY - 1),
                (8 * MS_PER_DAY, 11 * MS_PER_DAY - 1),
                (20 * MS_PER_DAY, 21 * MS_PER_DAY - 1),
            ]),
            ..bars(30)
        };
        assert_eq!(many.largest_gap_ms(), Some(3 * MS_PER_DAY));
        // An EMPTY probe is a real answer — no holes — and is NOT the same as an unmade one.
        let clean = Candidate { gaps: Some(Vec::new()), ..bars(10) };
        assert_eq!(clean.largest_gap_ms(), Some(0));
        assert_eq!(bars(10).largest_gap_ms(), None, "no probe was made");
    }

    /// The days criterion judges the recorded SPAN, and the spec's own example is the pin: 365 days
    /// required, 365 days present is a pass and one day short is a breach.
    #[test]
    fn the_days_criterion_is_the_span_and_its_boundary_is_inclusive() {
        let g = gate_args(spec("binance:BTCUSDT:1h"), 365, None, &["bar"]);
        let pass = judge(&g, &[bars(365)]);
        assert_eq!(pass[1].criterion, REQUIRE_DAYS);
        assert_eq!(pass[1].outcome, Outcome::Pass, "{:?}", pass[1]);
        let breach = judge(&g, &[bars(364)]);
        assert_eq!(breach[1].outcome, Outcome::Breach, "{:?}", breach[1]);
        assert!(breach[1].observed.starts_with("364d"), "{:?}", breach[1]);
        assert_eq!(rung(&breach), Exit::Breach);
        assert_eq!(rung(&pass), Exit::Ok);
    }

    /// **The failure §8.4 names, and the reason `--max-gap` is not decoration.** A store whose span
    /// is a full year and whose middle three weeks are missing PASSES the days criterion — and
    /// breaches the gap one. Both halves are asserted, because the first alone reads like a bug and
    /// the second alone would not prove the first is the behaviour on purpose.
    #[test]
    fn a_year_wide_span_with_three_weeks_missing_passes_days_and_breaches_the_gap() {
        let holed =
            Candidate { gaps: Some(vec![(100 * MS_PER_DAY, 121 * MS_PER_DAY - 1)]), ..bars(365) };
        let days_only = gate_args(spec("binance:BTCUSDT:1h"), 365, None, &["bar"]);
        assert_eq!(rung(&judge(&days_only, std::slice::from_ref(&holed))), Exit::Ok);

        let both = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(4 * 3_600_000), &["bar"]);
        let js = judge(&both, &[holed]);
        assert_eq!(js[1].outcome, Outcome::Pass, "the span is still a year: {:?}", js[1]);
        assert_eq!(js[2].criterion, MAX_GAP);
        assert_eq!(js[2].outcome, Outcome::Breach, "{:?}", js[2]);
        assert_eq!(rung(&js), Exit::Breach);
    }

    /// A DECLARED kind the store does not hold is a BREACH, not "nothing was evaluated" — the
    /// operator said it was required. The observed cell names the kinds the spec DOES hold, because
    /// a bare `0` cannot be acted on without a second command.
    #[test]
    fn a_required_kind_the_store_lacks_breaches_and_names_what_it_does_hold() {
        let g = gate_args(spec("binance:BTCUSDT"), 1, None, &["trade"]);
        let js = judge(&g, &[bars(30)]);
        assert_eq!(
            js.len(),
            1,
            "no series of the declared kind, so no per-series criteria: {js:?}"
        );
        assert_eq!(js[0].criterion, KIND_PRESENT);
        assert_eq!(js[0].outcome, Outcome::Breach);
        assert!(js[0].observed.contains("bar"), "it names what IS there: {:?}", js[0]);
        assert_eq!(rung(&js), Exit::Breach);
    }

    /// A kind the gate is NOT about is never judged for days or gaps, which is what keeps a
    /// one-row `properties` grid from reddening every instrument that has one. The control is the
    /// second half: declaring that kind DOES judge it.
    #[test]
    fn an_undeclared_kind_is_evidence_for_presence_and_is_never_judged_itself() {
        let props = Candidate {
            kind: "properties".to_string(),
            interval: None,
            first_ts: MS_PER_DAY,
            last_ts: MS_PER_DAY,
            rows: 1,
            gaps: None,
            gaps_error: None,
        };
        let g = gate_args(spec("binance:BTCUSDT"), 30, None, &["bar"]);
        let js = judge(&g, &[bars(90), props.clone()]);
        assert_eq!(js.len(), 2, "one presence criterion and one bar series: {js:?}");
        assert_eq!(rung(&js), Exit::Ok);

        let declared = gate_args(spec("binance:BTCUSDT"), 30, None, &["bar", "properties"]);
        let js = judge(&declared, &[bars(90), props]);
        assert_eq!(js.len(), 4, "two presence criteria and two series: {js:?}");
        assert_eq!(rung(&js), Exit::Breach, "a one-row grid does not span 30 days");
    }

    /// A failed gap probe is UNEVALUATED and lands on the nothing-was-evaluated rung — never a
    /// pass. That is the deliberate divergence from `execute_list`'s degrade-the-row rule, and the
    /// row carries the reason so a person is not left believing the gate checked something.
    #[test]
    fn a_failed_gap_probe_is_unevaluated_rather_than_a_pass() {
        let broken = Candidate {
            gaps: None,
            gaps_error: Some("cannot read manifest".to_string()),
            ..bars(400)
        };
        let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
        let js = judge(&g, &[broken]);
        assert_eq!(js[1].outcome, Outcome::Pass, "the days half still answered: {:?}", js[1]);
        assert!(matches!(js[2].outcome, Outcome::Unevaluated(_)), "{:?}", js[2]);
        assert_eq!(rung(&js), Exit::Empty);
    }

    /// ⚠ A BREACH outranks an unevaluated criterion, in EITHER order — a real failure must never be
    /// masked by a probe that could not answer elsewhere in the same run. Asserted both ways round,
    /// because a `.max()` replaced by "the first non-pass wins" would pass one of them.
    #[test]
    fn a_breach_outranks_an_unevaluated_criterion_in_either_order() {
        // Probed and clean: its BREACH comes from the days criterion alone, so the ranking below
        // is genuinely breach-against-unevaluated rather than two unevaluated rows.
        let short = Candidate { gaps: Some(Vec::new()), ..bars(1) };
        let broken = Candidate {
            gaps: None,
            gaps_error: Some("cannot read manifest".to_string()),
            ..bars(400)
        };
        let g = gate_args(spec("binance:BTCUSDT"), 365, Some(MS_PER_DAY), &["bar"]);
        assert_eq!(rung(&judge(&g, &[short.clone(), broken.clone()])), Exit::Breach);
        assert_eq!(rung(&judge(&g, &[broken, short])), Exit::Breach);
    }

    /// An INVERTED span is not a breach and not a pass: the store's own numbers are impossible, so
    /// nothing honest can be derived from them. The row names the sibling verb that reports it.
    #[test]
    fn an_impossible_span_goes_unevaluated_and_names_the_verb_that_reports_it() {
        let inverted = Candidate { first_ts: 10 * MS_PER_DAY, last_ts: 0, rows: 5, ..bars(1) };
        let g = gate_args(spec("binance:BTCUSDT:1h"), 1, None, &["bar"]);
        let js = judge(&g, &[inverted]);
        assert!(matches!(js[1].outcome, Outcome::Unevaluated(_)), "{:?}", js[1]);
        let Outcome::Unevaluated(why) = &js[1].outcome else { panic!("{:?}", js[1]) };
        assert!(why.contains("data hist health"), "{why}");
        assert_eq!(rung(&js), Exit::Empty);
    }

    /// A series the store folded to EMPTY spans nothing and BREACHES — it is absence, not an
    /// impossibility, and a gate that passed over it would be green on a store holding no rows.
    /// Its dates render as `-` rather than as `1970-01-01`, which would read as data.
    #[test]
    fn an_empty_series_breaches_and_renders_no_dates() {
        let empty = Candidate { first_ts: 0, last_ts: 0, rows: 0, ..bars(1) };
        let g = gate_args(spec("binance:BTCUSDT:1h"), 1, None, &["bar"]);
        let js = judge(&g, &[empty]);
        assert_eq!(js[1].outcome, Outcome::Breach, "{:?}", js[1]);
        assert!(js[1].observed.contains("0d (- .. -)"), "{:?}", js[1]);
    }

    /// An empty judgement set is not a pass. Unreachable through the verb — the parser defaults the
    /// kind roster to one entry, so a presence criterion always exists — and pinned anyway, because
    /// the one thing this rung may never do is answer `0` for "nothing happened".
    #[test]
    fn no_criteria_at_all_is_not_a_pass() {
        assert_eq!(rung(&[]), Exit::Empty);
    }

    /// The word and the number are ONE decision, and it is the COMPUTE plane's — this verb reaches
    /// `crate::cmd::runs::failif`'s vocabulary through its own judgements rather than through a
    /// copy of the mapping. A renderer that said `pass` while the process exited on a breach is the
    /// drift `runs/gate.rs`'s split exists to prevent; two planes printing different words for one
    /// outcome is the drift the shared declaration prevents.
    ///
    /// The three rows are reached through real `judge` output rather than by naming `Exit`
    /// variants, so a projection that read the wrong field would redden this even while
    /// `failif`'s own pin stayed green.
    #[test]
    fn the_verdict_word_and_the_rung_are_the_same_decision() {
        let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
        let pass = judge(&g, &[Candidate { gaps: Some(Vec::new()), ..bars(400) }]);
        assert_eq!(verdict_word(rung(&pass)), "pass");

        let breach = judge(&g, &[Candidate { gaps: Some(Vec::new()), ..bars(10) }]);
        assert_eq!(verdict_word(rung(&breach)), "breach");

        let unevaluated = judge(
            &g,
            &[Candidate {
                gaps: None,
                gaps_error: Some("cannot read manifest".to_string()),
                ..bars(400)
            }],
        );
        assert_eq!(verdict_word(rung(&unevaluated)), "unevaluated");
    }

    /// **The disclosure that keeps a half-checked gate from reading as a whole one.** A run with no
    /// `--max-gap` says which half it did not check; a run WITH one does not carry that note —
    /// the anti-vacuity control, since a note that fires on every run stops being read.
    #[test]
    fn a_gate_that_checked_no_holes_says_so_and_one_that_did_stays_quiet() {
        // ⚠ Each fixture carries the gap evidence its OWN run would have: unprobed where no
        // `--max-gap` was given, probed-and-clean where one was. `execute_gate` produces exactly
        // that pairing, and rendering the other combination would assert a table no run reaches.
        let none = gate_args(spec("binance:BTCUSDT:1h"), 365, None, &["bar"]);
        let js = judge(&none, &[bars(400)]);
        let text = lines(&none, &js, rung(&js), 1, 4).join("\n");
        assert!(text.contains("HOLES"), "{text}");
        assert!(text.contains("--max-gap"), "…and how to check them: {text}");

        let with = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(7 * MS_PER_DAY), &["bar"]);
        let js = judge(&with, &[Candidate { gaps: Some(Vec::new()), ..bars(400) }]);
        let text = lines(&with, &js, rung(&js), 1, 4).join("\n");
        assert!(
            !text.contains("HOLES"),
            "the note must not fire when the holes WERE checked: {text}"
        );
    }

    /// **The second disclosure, and it is a MEASUREMENT of the store rather than a style rule.** A
    /// tolerance below a day cannot be satisfied by any gap at all, because the store derives its
    /// holes from `date=` partitions. `--max-gap 1d` and wider carry no such note.
    #[test]
    fn a_sub_day_tolerance_says_the_store_cannot_answer_that_finely() {
        let clean = || Candidate { gaps: Some(Vec::new()), ..bars(400) };
        let fine = gate_args(spec("binance:BTCUSDT:1h"), 1, Some(4 * 3_600_000), &["bar"]);
        let js = judge(&fine, &[clean()]);
        let text = lines(&fine, &js, rung(&js), 1, 1).join("\n");
        assert!(text.contains("whole UTC day"), "{text}");
        assert!(text.contains("no missing day at all"), "{text}");

        let coarse = gate_args(spec("binance:BTCUSDT:1h"), 1, Some(MS_PER_DAY), &["bar"]);
        let js = judge(&coarse, &[clean()]);
        let text = lines(&coarse, &js, rung(&js), 1, 1).join("\n");
        assert!(!text.contains("whole UTC day"), "a day-or-wider tolerance is answerable: {text}");
    }

    /// The table names every criterion — the passing ones included — because §7.1 says the verdict
    /// is a document rather than a number, and because a CI step that prints its gate on success is
    /// the normal case.
    #[test]
    fn the_table_renders_every_criterion_and_its_own_summary() {
        let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
        // PROBED with an empty answer — the shape `execute_gate` always produces when `--max-gap`
        // was given, so only the days criterion fails and the summary's count is the one a run
        // would print. A `gaps: None` here would go UNEVALUATED and make this case assert a
        // rendering no operator can reach.
        let js = judge(&g, &[Candidate { gaps: Some(Vec::new()), ..bars(200) }]);
        let exit = rung(&js);
        let text = lines(&g, &js, exit, 1, 9).join("\n");
        assert!(text.starts_with("gate binance:BTCUSDT:1h"), "{text}");
        assert!(text.contains("1 of 9 stored series match this spec"), "{text}");
        assert!(text.contains("CRITERION") && text.contains("VERDICT"), "{text}");
        assert!(text.contains(REQUIRE_DAYS) && text.contains(MAX_GAP), "{text}");
        assert!(text.contains(KIND_PRESENT), "the presence criterion is rendered too: {text}");
        assert!(text.contains("BREACH — 1 of 3 criteria"), "{text}");
    }

    /// A duration prints in the grammar `--max-gap` accepts, so a cell can be pasted straight back
    /// into the flag. `1.5d` would name a spelling that grammar refuses.
    #[test]
    fn a_duration_cell_is_pasteable_back_into_the_flag() {
        assert_eq!(duration_cell(MS_PER_DAY), "1d");
        assert_eq!(duration_cell(7 * MS_PER_DAY), "7d");
        assert_eq!(duration_cell(4 * 3_600_000), "4h");
        assert_eq!(duration_cell(36 * 3_600_000), "36h");
        assert_eq!(duration_cell(1_500), "1500ms");
        for cell in [duration_cell(MS_PER_DAY), duration_cell(4 * 3_600_000)] {
            assert!(parse_max_gap(&cell).is_ok(), "{cell} must parse back");
        }
    }

    /// The document carries the numbers each verdict was derived FROM, so a consumer can re-derive
    /// a judgement rather than trust it — and an unmade gap probe is `null` rather than an empty
    /// array, which would say "no holes".
    #[test]
    fn the_document_carries_the_evidence_and_never_fakes_an_unmade_probe() {
        let probed = Candidate { gaps: Some(vec![(MS_PER_DAY, 2 * MS_PER_DAY - 1)]), ..bars(10) };
        let docs = json_series(&[probed, bars(10)]);
        assert_eq!(docs[0]["span_days"], 10);
        assert_eq!(docs[0]["span_ms"], 10 * MS_PER_DAY);
        assert_eq!(docs[0]["largest_gap_ms"], MS_PER_DAY);
        assert_eq!(docs[0]["gaps"][0]["from_ts"], MS_PER_DAY);
        assert_eq!(docs[0]["gaps"][0]["to_ts"], 2 * MS_PER_DAY - 1);
        assert!(docs[1]["gaps"].is_null(), "an unmade probe is null, never []: {}", docs[1]);
        assert!(docs[1]["largest_gap_ms"].is_null(), "{}", docs[1]);
    }

    /// An unevaluated criterion carries its reason into the document; everything else carries
    /// `null` under the same key, so a reader's key set does not change shape between runs.
    #[test]
    fn the_document_explains_only_what_went_unevaluated() {
        let broken = Candidate {
            gaps: None,
            gaps_error: Some("cannot read manifest".to_string()),
            ..bars(400)
        };
        let g = gate_args(spec("binance:BTCUSDT:1h"), 365, Some(MS_PER_DAY), &["bar"]);
        let docs = json_criteria(&judge(&g, &[broken]));
        assert_eq!(docs[1]["verdict"], "pass");
        assert!(docs[1]["why"].is_null(), "{}", docs[1]);
        assert_eq!(docs[2]["verdict"], "unevaluated");
        assert!(
            docs[2]["why"].as_str().unwrap_or_default().contains("cannot read manifest"),
            "{}",
            docs[2]
        );
    }
}
