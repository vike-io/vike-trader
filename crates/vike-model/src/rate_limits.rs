//! [`RateLimitConfig`] — the OPERATOR-facing knob for request pacing: what FRACTION of a venue's own
//! published rate budget vike is allowed to spend.
//!
//! **Why a fraction and not milliseconds.** The obvious knob is "min gap between requests, in ms",
//! and it is the wrong one on three counts: it is venue-SPECIFIC (150 ms is generous on one venue
//! and a ban on the next), it is meaningless to the human moving the slider (nobody knows what
//! Binance's weight budget is in milliseconds), and it is UNBOUNDED — a slider that goes to `0 ms`
//! goes to "banned". A target UTILIZATION is venue-INDEPENDENT (`0.40` means the same thing on every
//! venue), reads as a sentence an operator can check ("spend at most 40 % of what the venue says I
//! may"), and **cannot express a ban**: the ceiling is `1.0`, and 100 % is still the VENUE'S OWN
//! number, not ours. The pacer that consumes this scales the venue's published budget by the
//! fraction (`permits = budget × utilization`) — this module owns the number and its bounds, not the
//! pacing mechanism (that is `vike_bridge_core::ratelimit`'s sliding window).
//!
//! **The default is a deliberately conservative CHOICE, not a measurement.** `default_utilization =
//! 0.40` ([`DEFAULT_UTILIZATION`]) leaves enough headroom that a burst (a post-reconnect resync, a
//! backfill page loop) rides on top of steady traffic without touching the venue's ceiling, and is
//! not so small that ordinary work queues behind the pacer. A config file naming NO venue and NO
//! default is therefore already safe — every field is `#[serde(default)]`, so a `[rate_limits]`
//! table may be absent, empty, or partial.
//!
//! ⚠ An earlier draft of this doc called 0.40 "the measured optimum". It is not, and the correction
//! matters more than the number: what WAS measured (2026-08-04, from live `exchangeInfo` and
//! `x-mbx-used-weight` headers) is each venue's BUDGET — binance spot 6000/min, binance fapi and
//! aster 2400/min — and the per-request WEIGHT. No measurement was ever taken of which *fraction*
//! of a budget is optimal to consume; that is a risk preference, and it is exactly why this knob is
//! exposed to the operator instead of being frozen. Do not re-label it "measured" without an
//! experiment to cite.
//!
//! **Two safety properties this module is responsible for**, because the value arrives from a GUI
//! slider or a hand-edited TOML file and reaches a live order path:
//! 1. [`RateLimitConfig::utilization_for`] CLAMPS into the inclusive range [`MIN_UTILIZATION`] ..=
//!    [`MAX_UTILIZATION`] and never returns `0`, a negative, a NaN, or anything above `1`. `0` would
//!    be a pacer that never progresses (a silent hang, indistinguishable from a dead venue); `3.0`
//!    would be three times the venue's budget, i.e. a ban. A slider cannot reach either, no matter
//!    what it sends.
//! 2. [`RateLimitConfig::validate`] REJECTS nonsense up front with an operator-readable reason —
//!    including a `per_venue` key that is not in [`crate::VENUES`]. A typo (`"binanace"`, or the
//!    wrong case) would otherwise be silently ignored: the operator sets a value, sees no error, and
//!    the venue keeps running at the default. Clamping is the last line of defence for a value that
//!    is already live; `validate` is the one that TELLS the operator, and every offending entry is
//!    reported in one pass (fix them all at once, not one round trip per typo).
//!
//! The per-venue map is an [`indexmap::IndexMap`], matching the crate convention: insertion order is
//! preserved, so `validate`'s error text is deterministic (the same bad file always produces the same
//! message) and the GUI lists overrides in the order the operator added them.
//!
//! ## The OTHER half: [`PaceBook`] — what was MEASURED, not what was configured
//!
//! [`RateLimitConfig`] above is a RISK PREFERENCE an operator sets. Everything below it is the
//! opposite kind of number: an OBSERVATION a run made about a venue, kept so the NEXT run does not
//! have to re-derive it from zero. Every backfill measures its own pace — the wall clock of a
//! request, the weight one request costs — and then throws it away at process exit, so run N+1
//! opens with the same pessimistic constants run N did (`SEED_WEIGHT`, a hand-set `page_delay`).
//! [`MeasuredPace`] is one such observation and [`PaceBook`] is the keyed set of them; the pacer
//! that consumes them lives in `vike_bridge_core::pacer`, and the file I/O lives in a BINARY (see
//! `vike_backfill::pace_book`), because this crate does no I/O.
//!
//! **Three laws this half is responsible for**, all of them about not trusting a stale number:
//! 1. **Keyed on `(venue, market)`, never on venue alone.** Binance publishes 6000 weight/min on
//!    `api.binance.com` and 2400 on `fapi.binance.com`, and `/klines?limit=1000` costs weight 2 on
//!    the first and 5 on the second. One shared row would seed the perp pager with the spot weight
//!    and pace it 2.5x over target — the exact failure the split spot/perp specs exist to prevent.
//! 2. **Age is a first-class field.** [`PaceBook::is_stale`] answers against a caller-supplied
//!    `max_age_ms`, an ABSENT record is stale (there is nothing to trust), and so is a
//!    FUTURE-dated one (a clock that moved backwards makes a record's age meaningless, and a
//!    record that claims to be from tomorrow can never expire).
//! 3. **A measurement informs SPEED, never the LIMIT.** Nothing here carries a ceiling. A pacer's
//!    budget comes from DISCOVERY (the venue's own published `REQUEST_WEIGHT`), and a persisted
//!    record can only tell it how long a request took and what one cost — never how much it may
//!    spend. [`MeasuredPace::budget_per_min`] is recorded precisely so a consumer can REFUSE a
//!    record that was measured against a different budget; it is evidence, not permission.

/// Default target utilization — the fraction of a venue's published budget vike spends when nothing
/// is configured. A deliberately conservative CHOICE, not a measured optimum (see the module doc):
/// the gap up to [`MAX_UTILIZATION`] is burst headroom a reconnect resync or a backfill page loop
/// rides in.
pub const DEFAULT_UTILIZATION: f64 = 0.40;

/// Floor of the accepted range. Below this a pacer is slow enough to look broken (and `0.0` would
/// never progress at all), so the slider stops here rather than at zero.
pub const MIN_UTILIZATION: f64 = 0.05;

/// Ceiling of the accepted range: the venue's OWN published budget, spent in full. There is
/// deliberately nothing above it — exceeding a venue's stated limit is how an account gets banned,
/// and no operator setting may express it.
pub const MAX_UTILIZATION: f64 = 0.95;

/// Operator config for request pacing: one default fraction plus optional per-venue overrides.
///
/// Serde-shaped for a TOML `[rate_limits]` table (`#[serde(default)]` on the struct — an absent or
/// partial table deserializes to the defaults, which are already the measured optimum). Keys of
/// `per_venue` are canonical lowercase venue ids from [`crate::VENUES`]; [`Self::validate`] enforces
/// that, and [`Self::utilization_for`] matches them exactly (no case folding — a mis-cased key is a
/// typo `validate` reports, not a silent near-miss).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    /// Target fraction of each venue's published budget, for every venue without an override.
    /// Defaults to [`DEFAULT_UTILIZATION`].
    pub default_utilization: f64,
    /// Per-venue overrides, keyed by canonical lowercase venue id. Absent venue = the default.
    /// Insertion-ordered so `validate`'s message and any GUI listing are deterministic.
    pub per_venue: indexmap::IndexMap<String, f64>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            default_utilization: DEFAULT_UTILIZATION,
            per_venue: indexmap::IndexMap::new(),
        }
    }
}

impl RateLimitConfig {
    /// The target fraction for `venue`, CLAMPED to a safe range. Never returns 0 or > 1.
    ///
    /// Resolution order: the `per_venue` override, else `default_utilization`, else (only if THAT is
    /// NaN too) the compiled-in [`DEFAULT_UTILIZATION`] — so there is no NaN escape path out of this
    /// function. A NaN at either level falls back rather than clamping, because NaN carries no
    /// intent: clamping it would have to pick an arbitrary end of the range, while falling back
    /// yields the value the operator would have had by leaving the field out. Infinities are NOT
    /// NaN-like — they carry a direction, so `+inf` clamps to [`MAX_UTILIZATION`] and `-inf` to
    /// [`MIN_UTILIZATION`].
    ///
    /// This is the LAST line of defence, not the first: a value that fails [`Self::validate`] still
    /// resolves to something safe here, so a live pacer can never be handed a hang (`0`) or a ban
    /// (`> 1`) even if the config was never validated.
    pub fn utilization_for(&self, venue: &str) -> f64 {
        let raw = self
            .per_venue
            .get(venue)
            .copied()
            .filter(|v| !v.is_nan())
            .unwrap_or_else(|| self.resolved_default());
        raw.clamp(MIN_UTILIZATION, MAX_UTILIZATION)
    }

    /// `default_utilization`, or the compiled-in constant when it is NaN. Not clamped — the caller
    /// clamps once, at the end of the resolution chain.
    fn resolved_default(&self) -> f64 {
        if self.default_utilization.is_nan() {
            DEFAULT_UTILIZATION
        } else {
            self.default_utilization
        }
    }

    /// Reject nonsense before it reaches a pacer. Err carries an operator-readable reason.
    ///
    /// Three failure classes, all reported in ONE pass (every offending entry named, joined by
    /// `"; "`, in `per_venue` insertion order — one edit fixes the whole file):
    /// - `default_utilization` not a number, or outside `[MIN_UTILIZATION, MAX_UTILIZATION]`;
    /// - a `per_venue` value not a number, or outside that range;
    /// - a `per_venue` KEY that is not a venue in [`crate::VENUES`] — the typo case, which is the
    ///   whole reason this function exists: an unknown key changes nothing at runtime, so without
    ///   this check the operator's setting silently does not apply.
    ///
    /// Bounds are INCLUSIVE: exactly `MIN_UTILIZATION` and exactly `MAX_UTILIZATION` are legal
    /// settings (100 % is the venue's own published budget, not an overspend).
    pub fn validate(&self) -> Result<(), String> {
        let mut problems: Vec<String> = Vec::new();

        if !in_range(self.default_utilization) {
            problems.push(format!(
                "default_utilization = {} is not in [{MIN_UTILIZATION}, {MAX_UTILIZATION}]",
                self.default_utilization
            ));
        }
        for (venue, &value) in &self.per_venue {
            if !crate::VENUES.contains(&venue.as_str()) {
                problems.push(format!(
                    "per_venue key {venue:?} is not a known venue (see vike_model::VENUES)"
                ));
            }
            if !in_range(value) {
                problems.push(format!(
                    "per_venue[{venue:?}] = {value} is not in [{MIN_UTILIZATION}, {MAX_UTILIZATION}]"
                ));
            }
        }

        if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
    }
}

/// A fraction inside the INCLUSIVE accepted range. NaN and both infinities are `false` — a range
/// `contains` is a pair of `PartialOrd` comparisons, and every comparison against NaN is false,
/// which is exactly the answer wanted here: NaN is not a valid setting.
fn in_range(v: f64) -> bool {
    (MIN_UTILIZATION..=MAX_UTILIZATION).contains(&v)
}

// ---------------------------------------------------------------------------------------------
// Measured pace — the persisted observation half (see the module doc's second section).
// ---------------------------------------------------------------------------------------------

/// Default maximum age of a [`MeasuredPace`] a consumer should still trust: **7 days**.
///
/// A pace record ages for reasons nothing in this crate can see — a re-priced endpoint, a moved
/// egress, a venue that changed hosts, a laptop that measured from a different continent. Seven days
/// is long enough that a weekly backfill still benefits and short enough that a stale route is
/// re-measured rather than re-used forever. It is a DEFAULT, not a law: [`PaceBook::is_stale`] takes
/// the age from the caller, so an operator running hourly can tighten it without touching this.
pub const DEFAULT_MAX_PACE_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// EWMA weight given to the NEWEST `request_ms` when [`PaceBook::record`] merges an observation into
/// an existing row. Deliberately the same 0.25 as `vike_bridge_core::pacer`'s in-run smoothing and
/// for the same reason: this number is SUBTRACTED from a sleep, so an outlier shortens the next
/// run's opening gap — the unsafe direction — and a quarter-weight blend caps one bad run's pull at
/// 25 % while still converging on a genuine step change (a slower route) within a few runs.
pub const PACE_ALPHA: f64 = 0.25;

/// The narrow, venue-free pace observation a pacer produces and consumes: how long one request took,
/// what one request cost, and which budget those two numbers were measured against.
///
/// Separate from [`MeasuredPace`] because a pacer legitimately knows none of the identity fields —
/// it has no venue string, no market, and (by contract) no clock. This is the whole payload that
/// crosses the pacer boundary in either direction; [`MeasuredPace`] is this plus the bookkeeping
/// needed to persist it.
///
/// Every field is `#[serde(default)]` via the struct attribute, so a partial row still loads — and
/// the DERIVED `Default` is deliberately the all-zero shape, which [`Self::is_usable`] rejects: a
/// row that says nothing must never look like a measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PaceSample {
    /// Observed per-request wall clock, in MILLISECONDS. Milliseconds and not seconds because this
    /// is written to a human-readable file an operator reads: `280` is checkable at a glance where
    /// `0.28` invites a units mistake.
    pub request_ms: u64,
    /// Observed cost of ONE request in the venue's own weight units (binance spot `/klines` = 2,
    /// fapi = 5). The number a hardcoded seed constant guesses.
    ///
    /// ⚠ Only meaningful where the venue publishes a weight counter. A row whose `budget_per_min` is
    /// `None` was measured on a venue that has no weight concept (bybit/okx/deribit), where the
    /// producing pacer never read this field and simply reported its own seed constant; it is
    /// carried because [`PaceSample::is_usable`] requires a positive weight, and it steers nothing —
    /// a `None` budget confines the record to another fixed-delay pacer. Read `request_ms` there.
    pub per_request_weight: f64,
    /// The venue's published budget, weight units per MINUTE, at the time of measurement — or `None`
    /// when the venue publishes none (most of the roster). This is the MATCH KEY, not a ceiling: a
    /// consumer compares it against the budget IT discovered and discards the record when they
    /// disagree, because a request time and a weight measured on another host describe another host.
    pub budget_per_min: Option<u64>,
    /// How many requests this observation summarizes. `0` means "nothing was measured", which
    /// [`MeasuredPace::is_usable`] rejects — a record with no samples behind it is a guess wearing a
    /// measurement's clothes.
    pub samples: u64,
}

impl PaceSample {
    /// Is this sample safe to persist or to seed a pacer with?
    ///
    /// Rejects the shapes that would poison a pace rather than inform it: a non-finite or
    /// non-positive weight (a `0` divisor is a zero-delay hammer; a NaN propagates into
    /// `Duration::from_secs_f64`, which panics), a zero `request_ms` (nothing was timed), and a
    /// zero `samples` (nothing was observed). Deliberately NOT a `Result`: there is no caller
    /// action that distinguishes the reasons — an unusable sample is simply not used.
    pub fn is_usable(&self) -> bool {
        self.per_request_weight.is_finite()
            && self.per_request_weight > 0.0
            && self.request_ms > 0
            && self.samples > 0
    }

    /// [`Self::request_ms`] as SECONDS, the unit a pacer's arithmetic is denominated in.
    pub fn request_secs(&self) -> f64 {
        self.request_ms as f64 / 1_000.0
    }
}

/// One persisted pace observation: a [`PaceSample`] plus the identity and the timestamp that make it
/// storable and expirable.
///
/// The fields are flattened rather than nesting a `PaceSample`, so the on-disk JSON is one flat
/// object per row an operator can read and hand-edit; [`Self::sample`] recovers the narrow view.
///
/// The DERIVED `Default` is the empty/all-zero shape — what an absent or hollow row deserializes to
/// — and [`Self::is_usable`] rejects it, so a row that says nothing can never seed anything.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MeasuredPace {
    /// Canonical lowercase venue id (see [`crate::VENUES`]).
    pub venue: String,
    /// Whatever PARTITIONS this venue's pace evidence — half the key (see the module doc's law 1),
    /// and never decoration.
    ///
    /// The vocabulary is per-venue, because the thing being partitioned is. Binance splits
    /// `"spot"`/`"perp"`: its two hosts publish different budgets and price the same endpoint
    /// differently, so one shared row would pace the other host wrong. bybit/okx/deribit take a
    /// single `"klines"` row each — one endpoint, one IP limit, no published budget — because
    /// splitting there would only halve the evidence behind a number that describes the same
    /// endpoint either way.
    pub market: String,
    /// Observed per-request wall clock, in milliseconds.
    pub request_ms: u64,
    /// Observed cost of one request, in the venue's weight units.
    pub per_request_weight: f64,
    /// The venue's published per-minute budget at measurement time, or `None` when it publishes
    /// none. A match key, never a ceiling — see the module doc's law 3.
    pub budget_per_min: Option<u64>,
    /// Epoch milliseconds at which this was measured, for [`PaceBook::is_stale`].
    pub measured_at_ms: i64,
    /// How many requests this row summarizes, accumulated across merges.
    pub samples: u64,
}

impl MeasuredPace {
    /// Build a persistable row from a pacer's [`PaceSample`] plus the identity and clock the pacer
    /// does not have. The caller owns the clock — this crate reads none.
    pub fn new(
        venue: impl Into<String>,
        market: impl Into<String>,
        sample: PaceSample,
        measured_at_ms: i64,
    ) -> Self {
        MeasuredPace {
            venue: venue.into(),
            market: market.into(),
            request_ms: sample.request_ms,
            per_request_weight: sample.per_request_weight,
            budget_per_min: sample.budget_per_min,
            measured_at_ms,
            samples: sample.samples,
        }
    }

    /// The narrow pacer-facing view of this row.
    pub fn sample(&self) -> PaceSample {
        PaceSample {
            request_ms: self.request_ms,
            per_request_weight: self.per_request_weight,
            budget_per_min: self.budget_per_min,
            samples: self.samples,
        }
    }

    /// Is this row safe to persist or to seed with? [`PaceSample::is_usable`] plus a non-empty
    /// `(venue, market)` — an unkeyed row would collide with every other unkeyed row.
    pub fn is_usable(&self) -> bool {
        !self.venue.is_empty() && !self.market.is_empty() && self.sample().is_usable()
    }
}

/// The persisted set of [`MeasuredPace`] rows, keyed `(venue, market)`.
///
/// An [`indexmap::IndexMap`] for the crate convention and for one concrete property: the on-disk
/// file is written in insertion order, so a re-save after a no-op run produces the same bytes rather
/// than a shuffled map — a diff of the file shows what actually changed.
///
/// `#[serde(default)]` throughout: an ABSENT file, an empty object and a partial row are all valid,
/// which is what makes the whole feature additive (no file ⇒ an empty book ⇒ nothing to seed with ⇒
/// every pacer starts exactly where it always did).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PaceBook {
    /// Rows keyed by [`PaceBook::key`]. Public so a loader can inspect/prune, but prefer
    /// [`Self::get`]/[`Self::record`], which own the keying and the merge law.
    pub entries: indexmap::IndexMap<String, MeasuredPace>,
}

impl PaceBook {
    /// The composite map key for a `(venue, market)` pair: `"{venue}:{market}"`.
    ///
    /// A flat string rather than a tuple because JSON object keys are strings — a tuple key would
    /// serialize as an array-of-pairs and stop being hand-editable, which is the point of a
    /// human-readable pace file.
    pub fn key(venue: &str, market: &str) -> String {
        format!("{venue}:{market}")
    }

    /// The row for `(venue, market)`, whatever its age. Use [`Self::fresh`] to get one that is also
    /// young enough to trust — this accessor deliberately does NOT filter, so a diagnostic can print
    /// a stale row and say so.
    pub fn get(&self, venue: &str, market: &str) -> Option<&MeasuredPace> {
        self.entries.get(&Self::key(venue, market))
    }

    /// Fold an observation into the book.
    ///
    /// An UNUSABLE row ([`MeasuredPace::is_usable`]) is a silent no-op — a run that measured nothing
    /// must not be able to overwrite a good record with zeros, and there is no caller action that
    /// would follow from being told.
    ///
    /// Merge law, for a `(venue, market)` that already has a usable row:
    /// * `budget_per_min` DIFFERS (including one side `None`) ⇒ **replace outright**. The two rows
    ///   describe different worlds — a re-priced venue, another host, a run that had no discovery —
    ///   and averaging across them would produce a number that was never true of either.
    /// * `budget_per_min` MATCHES ⇒ blend `request_ms` by [`PACE_ALPHA`] toward the new value, take
    ///   the new `per_request_weight` VERBATIM (latest-wins, mirroring the pacer's own rule: the
    ///   weight is a step function the venue sets, not a noisy sample), accumulate `samples`
    ///   (saturating), and take the new `measured_at_ms` — the row is as fresh as its newest
    ///   evidence.
    pub fn record(&mut self, m: MeasuredPace) {
        if !m.is_usable() {
            return;
        }
        let key = Self::key(&m.venue, &m.market);
        let merged = match self.entries.get(&key) {
            Some(prev) if prev.is_usable() && prev.budget_per_min == m.budget_per_min => {
                MeasuredPace {
                    request_ms: ewma_ms(prev.request_ms, m.request_ms),
                    samples: prev.samples.saturating_add(m.samples),
                    ..m
                }
            }
            _ => m,
        };
        self.entries.insert(key, merged);
    }

    /// Is `(venue, market)` missing, expired, or future-dated — i.e. is there NOTHING here a
    /// consumer should trust?
    ///
    /// `true` for an ABSENT row (nothing to trust is the same answer as nothing trustworthy), for an
    /// UNUSABLE one, for one older than `max_age_ms`, and for one whose `measured_at_ms` is AFTER
    /// `now_ms` (a clock that moved backwards makes an age meaningless, and a future-dated row would
    /// otherwise never expire). A non-positive `max_age_ms` makes everything stale, which is the
    /// clean way for an operator to turn seeding off without deleting the file.
    pub fn is_stale(&self, venue: &str, market: &str, now_ms: i64, max_age_ms: i64) -> bool {
        self.fresh(venue, market, now_ms, max_age_ms).is_none()
    }

    /// The row for `(venue, market)` when it is present, usable, and within `max_age_ms` of
    /// `now_ms` — the accessor a consumer that intends to ACT on the record should use. The exact
    /// negation of [`Self::is_stale`], so the two can never disagree.
    ///
    /// Bounds are INCLUSIVE at the young end: a row measured exactly `max_age_ms` ago is still
    /// fresh, matching every other inclusive-boundary convention in this workspace.
    pub fn fresh(
        &self,
        venue: &str,
        market: &str,
        now_ms: i64,
        max_age_ms: i64,
    ) -> Option<&MeasuredPace> {
        if max_age_ms <= 0 {
            return None;
        }
        let m = self.get(venue, market)?;
        if !m.is_usable() {
            return None;
        }
        let age = now_ms.checked_sub(m.measured_at_ms)?;
        (0..=max_age_ms).contains(&age).then_some(m)
    }

    /// Number of rows held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No rows held — the state a missing or empty file loads to, and the state in which this whole
    /// feature is a no-op.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Blend a new millisecond observation into a previous one at [`PACE_ALPHA`], rounded back to an
/// integer millisecond. Saturating on the cast so an absurd pair cannot wrap into a tiny value.
fn ewma_ms(prev: u64, new: u64) -> u64 {
    let blended = prev as f64 * (1.0 - PACE_ALPHA) + new as f64 * PACE_ALPHA;
    if blended.is_finite() && blended >= 0.0 { blended.round() as u64 } else { new }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(default_utilization: f64, overrides: &[(&str, f64)]) -> RateLimitConfig {
        RateLimitConfig {
            default_utilization,
            per_venue: overrides.iter().map(|(v, u)| ((*v).to_string(), *u)).collect(),
        }
    }

    /// The measured optimum is the default, and it is what an unconfigured venue resolves to — this
    /// is the "nothing needs configuring" contract, so the number is pinned bit-for-bit.
    #[test]
    fn default_is_forty_percent() {
        let c = RateLimitConfig::default();
        assert_eq!(c.default_utilization.to_bits(), 0.40_f64.to_bits());
        assert!(c.per_venue.is_empty());
        assert_eq!(c.utilization_for("binance").to_bits(), 0.40_f64.to_bits());
        // An unknown venue string still resolves (the pacer asks by name; it never panics).
        assert_eq!(c.utilization_for("not-a-venue").to_bits(), 0.40_f64.to_bits());
        c.validate().expect("the shipped default must validate");
    }

    #[test]
    fn a_per_venue_override_wins_over_the_default() {
        let c = cfg(0.40, &[("binance", 0.75)]);
        assert_eq!(c.utilization_for("binance").to_bits(), 0.75_f64.to_bits());
        // …and only for the venue it names.
        assert_eq!(c.utilization_for("bybit").to_bits(), 0.40_f64.to_bits());
        c.validate().expect("a roster venue with an in-range value is valid");
    }

    /// The GUI-slider guarantee: nothing an operator can send produces a pacer that never progresses
    /// (`0`) or one that outspends the venue's budget (`> 1`). Both ends, both levels.
    #[test]
    fn utilization_is_clamped_at_both_ends() {
        let c = cfg(5.0, &[("binance", 0.0), ("bybit", 3.0), ("okx", -1.0)]);
        assert_eq!(c.utilization_for("binance"), MIN_UTILIZATION);
        assert_eq!(c.utilization_for("okx"), MIN_UTILIZATION);
        assert_eq!(c.utilization_for("bybit"), MAX_UTILIZATION);
        // The DEFAULT is clamped too — an out-of-range default must not leak through an
        // un-overridden venue.
        assert_eq!(c.utilization_for("deribit"), MAX_UTILIZATION);
        // Infinities carry a direction, so they clamp rather than falling back.
        let inf = cfg(0.40, &[("binance", f64::INFINITY), ("bybit", f64::NEG_INFINITY)]);
        assert_eq!(inf.utilization_for("binance"), MAX_UTILIZATION);
        assert_eq!(inf.utilization_for("bybit"), MIN_UTILIZATION);
        // Exactly the bounds survive untouched.
        let edge = cfg(0.40, &[("binance", MIN_UTILIZATION), ("bybit", MAX_UTILIZATION)]);
        assert_eq!(edge.utilization_for("binance"), MIN_UTILIZATION);
        assert_eq!(edge.utilization_for("bybit"), MAX_UTILIZATION);
    }

    /// NaN carries no intent, so it FALLS BACK rather than clamping to an arbitrary end — at both
    /// levels, and the resolution chain terminates in the compiled-in constant.
    #[test]
    fn nan_falls_back_to_the_default() {
        let c = cfg(0.30, &[("binance", f64::NAN)]);
        assert_eq!(c.utilization_for("binance").to_bits(), 0.30_f64.to_bits());
        // A NaN default too: the chain ends at DEFAULT_UTILIZATION, never at NaN.
        let both = cfg(f64::NAN, &[("binance", f64::NAN)]);
        assert_eq!(both.utilization_for("binance").to_bits(), DEFAULT_UTILIZATION.to_bits());
        assert_eq!(both.utilization_for("bybit").to_bits(), DEFAULT_UTILIZATION.to_bits());
        // Whatever the config, the resolved value is a usable fraction.
        for c in [&c, &both] {
            for &v in crate::VENUES {
                let u = c.utilization_for(v);
                assert!(in_range(u), "{v}: {u} escaped the accepted range");
            }
        }
    }

    /// The typo case — an unknown key changes NOTHING at runtime, so it must be an error the
    /// operator can read, naming the offending key.
    #[test]
    fn an_unknown_venue_key_fails_validate_by_name() {
        let err = cfg(0.40, &[("binanace", 0.5)]).validate().unwrap_err();
        assert!(err.contains("binanace"), "the offending key must be named: {err}");
        // Case matters: ids are canonical lowercase (see `crate::venues`), so a mis-cased key is a
        // typo, not a near-miss that silently applies.
        let err = cfg(0.40, &[("Binance", 0.5)]).validate().unwrap_err();
        assert!(err.contains("Binance"), "{err}");
        assert_eq!(cfg(0.40, &[("Binance", 0.5)]).utilization_for("binance"), 0.40);
    }

    /// Every offending entry is reported in ONE pass — one edit fixes the whole file.
    #[test]
    fn validate_reports_every_offender_at_once() {
        let c = cfg(2.0, &[("binance", 9.0), ("nope", 0.5), ("bybit", 0.5)]);
        let err = c.validate().unwrap_err();
        assert!(err.contains("default_utilization"), "{err}");
        assert!(err.contains("binance"), "{err}");
        assert!(err.contains("nope"), "{err}");
        assert!(!err.contains("bybit"), "a valid row must not be reported: {err}");
        // NaN is not a valid setting even though `utilization_for` can survive it.
        assert!(cfg(f64::NAN, &[]).validate().is_err());
        assert!(cfg(0.40, &[("binance", f64::NAN)]).validate().is_err());
        // The inclusive bounds are legal settings.
        cfg(MIN_UTILIZATION, &[("binance", MAX_UTILIZATION)]).validate().expect("bounds are legal");
        // Every roster venue is an accepted key.
        let all: Vec<(&str, f64)> = crate::VENUES.iter().map(|v| (*v, 0.5)).collect();
        cfg(0.40, &all).validate().expect("every roster venue is a valid key");
    }

    /// Round-trips through the TOML-shaped JSON the workspace config layer uses (serde_json is the
    /// in-tree serde witness; the field/table shape is what a `[rate_limits]` TOML table produces).
    #[test]
    fn serde_round_trip() {
        let c = cfg(0.25, &[("binance", 0.6), ("okx", 0.1)]);
        let json = serde_json::to_string(&c).expect("serialize");
        let back: RateLimitConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.default_utilization.to_bits(), 0.25_f64.to_bits());
        assert_eq!(back.per_venue.len(), 2);
        assert_eq!(back.utilization_for("binance").to_bits(), 0.6_f64.to_bits());
        assert_eq!(back.utilization_for("okx").to_bits(), 0.1_f64.to_bits());
        // Insertion order survives the round trip (the IndexMap contract this module relies on for
        // deterministic error text).
        let keys: Vec<&str> = back.per_venue.keys().map(String::as_str).collect();
        assert_eq!(keys, ["binance", "okx"]);
    }

    /// The `#[serde(default)]` contract: an ABSENT table, an empty one, and a partial one are all
    /// valid config, and each falls back to the measured optimum for whatever it did not say.
    #[test]
    fn absent_fields_fall_back_to_defaults() {
        let empty: RateLimitConfig = serde_json::from_str("{}").expect("an empty table is valid");
        assert_eq!(empty.default_utilization.to_bits(), DEFAULT_UTILIZATION.to_bits());
        assert!(empty.per_venue.is_empty());

        let only_default: RateLimitConfig =
            serde_json::from_str(r#"{"default_utilization":0.8}"#).expect("partial table");
        assert_eq!(only_default.default_utilization.to_bits(), 0.8_f64.to_bits());
        assert!(only_default.per_venue.is_empty());

        let only_overrides: RateLimitConfig =
            serde_json::from_str(r#"{"per_venue":{"bybit":0.9}}"#).expect("partial table");
        assert_eq!(only_overrides.default_utilization.to_bits(), DEFAULT_UTILIZATION.to_bits());
        assert_eq!(only_overrides.utilization_for("bybit").to_bits(), 0.9_f64.to_bits());
        assert_eq!(
            only_overrides.utilization_for("binance").to_bits(),
            DEFAULT_UTILIZATION.to_bits()
        );
        only_overrides.validate().expect("a partial table is valid config");
    }

    // ----- the measured-pace half ---------------------------------------------------------------

    /// A realistic row: the MEASURED binance fapi shape (2400/min budget, weight-5 `/klines`,
    /// ~280 ms round trip from the CI box) — the numbers this whole mechanism exists to stop discarding.
    fn pace(
        market: &str,
        request_ms: u64,
        weight: f64,
        budget: Option<u64>,
        at: i64,
    ) -> MeasuredPace {
        MeasuredPace::new(
            "binance",
            market,
            PaceSample {
                request_ms,
                per_request_weight: weight,
                budget_per_min: budget,
                samples: 1,
            },
            at,
        )
    }

    #[test]
    fn record_then_get_round_trips_the_observation() {
        let mut b = PaceBook::default();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(b.get("binance", "perp"), None);

        b.record(pace("perp", 280, 5.0, Some(2400), 1_000));
        let got = b.get("binance", "perp").expect("the row just recorded");
        assert_eq!(got.request_ms, 280);
        assert_eq!(got.per_request_weight.to_bits(), 5.0_f64.to_bits());
        assert_eq!(got.budget_per_min, Some(2400));
        assert_eq!(got.samples, 1);
        assert_eq!(b.len(), 1);
        // ...and the narrow pacer view recovers exactly what went in.
        assert_eq!(got.sample().request_secs().to_bits(), 0.280_f64.to_bits());
    }

    /// Law 1: `(venue, market)` is the key, NOT the venue. Binance's two hosts publish different
    /// budgets and price `/klines` differently — one shared row would seed the perp pager with the
    /// spot weight and pace it 2.5x over target.
    #[test]
    fn spot_and_perp_are_separate_rows_on_the_same_venue() {
        let mut b = PaceBook::default();
        b.record(pace("spot", 120, 2.0, Some(6000), 1_000));
        b.record(pace("perp", 280, 5.0, Some(2400), 1_000));
        assert_eq!(b.len(), 2, "one venue, two markets, two rows");
        assert_eq!(b.get("binance", "spot").unwrap().per_request_weight, 2.0);
        assert_eq!(b.get("binance", "perp").unwrap().per_request_weight, 5.0);
        assert_eq!(b.get("binance", "spot").unwrap().budget_per_min, Some(6000));
        assert_eq!(b.get("binance", "perp").unwrap().budget_per_min, Some(2400));
        // A venue that was never recorded is simply absent — no fallback to a sibling market.
        assert_eq!(b.get("bybit", "spot"), None);
        assert_eq!(b.get("binance", "futures"), None);
    }

    /// The merge law on a MATCHING budget: `request_ms` blends at `PACE_ALPHA`, the weight is
    /// latest-wins, samples accumulate, and the row takes the newest timestamp.
    #[test]
    fn a_second_observation_merges_by_ewma_and_accumulates_samples() {
        let mut b = PaceBook::default();
        b.record(pace("perp", 280, 5.0, Some(2400), 1_000));
        b.record(pace("perp", 680, 5.0, Some(2400), 9_000));
        let m = b.get("binance", "perp").unwrap();
        assert_eq!(m.request_ms, 380, "280*0.75 + 680*0.25 = 380");
        assert_eq!(m.samples, 2, "samples accumulate across merges");
        assert_eq!(m.measured_at_ms, 9_000, "the row is as fresh as its newest evidence");
        assert_eq!(b.len(), 1, "a merge updates in place, it does not append");

        // ONE slow run must not become the record — the reason this is smoothed and not latest-wins.
        let mut c = PaceBook::default();
        c.record(pace("perp", 280, 5.0, Some(2400), 1_000));
        c.record(pace("perp", 5_000, 5.0, Some(2400), 2_000));
        let slow = c.get("binance", "perp").unwrap().request_ms;
        assert!(slow < 5_000 / 2, "one outlier must not become the estimate: {slow}");

        // The WEIGHT is latest-wins: it is a step function the venue sets, not a noisy sample.
        let mut d = PaceBook::default();
        d.record(pace("spot", 120, 2.0, Some(6000), 1_000));
        d.record(pace("spot", 120, 3.0, Some(6000), 2_000));
        assert_eq!(d.get("binance", "spot").unwrap().per_request_weight, 3.0);
    }

    /// A CHANGED budget replaces outright rather than blending: the two rows describe different
    /// worlds (a re-priced venue, a run with no discovery), and an average across them was never
    /// true of either.
    #[test]
    fn a_changed_budget_replaces_rather_than_blending() {
        let mut b = PaceBook::default();
        b.record(pace("perp", 280, 5.0, Some(2400), 1_000));
        b.record(pace("perp", 680, 5.0, Some(4800), 2_000));
        let m = b.get("binance", "perp").unwrap();
        assert_eq!(m.request_ms, 680, "not blended — the old budget's timing is not evidence here");
        assert_eq!(m.samples, 1, "and the sample count restarts with the new world");
        assert_eq!(m.budget_per_min, Some(4800));

        // `None` vs `Some` is a difference too — a fallback-mode run's timing is not a discovered
        // run's timing.
        b.record(pace("perp", 900, 5.0, None, 3_000));
        assert_eq!(b.get("binance", "perp").unwrap().request_ms, 900);
        assert_eq!(b.get("binance", "perp").unwrap().budget_per_min, None);
    }

    /// A run that measured nothing must not be able to overwrite a good record with zeros.
    #[test]
    fn an_unusable_observation_is_a_silent_no_op() {
        let good = pace("perp", 280, 5.0, Some(2400), 1_000);
        for bad in [
            MeasuredPace { samples: 0, ..good.clone() }, // nothing observed
            MeasuredPace { request_ms: 0, ..good.clone() }, // nothing timed
            MeasuredPace { per_request_weight: 0.0, ..good.clone() },
            MeasuredPace { per_request_weight: -1.0, ..good.clone() },
            MeasuredPace { per_request_weight: f64::NAN, ..good.clone() },
            MeasuredPace { per_request_weight: f64::INFINITY, ..good.clone() },
            MeasuredPace { venue: String::new(), ..good.clone() },
            MeasuredPace { market: String::new(), ..good.clone() },
        ] {
            assert!(!bad.is_usable(), "{bad:?} must not be usable");
            // into an EMPTY book: nothing is stored at all.
            let mut empty = PaceBook::default();
            empty.record(bad.clone());
            assert!(empty.is_empty(), "{bad:?} must not create a row");
            // into a book that already holds a good row: the good row survives untouched.
            let mut held = PaceBook::default();
            held.record(good.clone());
            held.record(bad.clone());
            assert_eq!(held.get("binance", "perp").unwrap().request_ms, 280);
            assert_eq!(held.get("binance", "perp").unwrap().samples, 1);
        }
        assert!(good.is_usable());
    }

    /// Law 2: an absent row, an expired one and a FUTURE-dated one are all stale, and `fresh` is the
    /// exact negation of `is_stale` so the two can never disagree.
    #[test]
    fn staleness_covers_absent_expired_and_future_dated_rows() {
        let now = 1_000_000i64;
        let day = 86_400_000i64;
        let mut b = PaceBook::default();

        // absent ⇒ stale, and there is nothing to act on.
        assert!(b.is_stale("binance", "perp", now, DEFAULT_MAX_PACE_AGE_MS));
        assert_eq!(b.fresh("binance", "perp", now, DEFAULT_MAX_PACE_AGE_MS), None);

        b.record(pace("perp", 280, 5.0, Some(2400), now - day));
        assert!(
            !b.is_stale("binance", "perp", now, DEFAULT_MAX_PACE_AGE_MS),
            "one day old is fresh"
        );
        assert!(b.fresh("binance", "perp", now, DEFAULT_MAX_PACE_AGE_MS).is_some());
        // ...against a tighter window the SAME row is stale — the age is the caller's policy.
        assert!(b.is_stale("binance", "perp", now, day / 2));

        // Exactly at the bound is still fresh (inclusive, like every other boundary here).
        assert!(!b.is_stale("binance", "perp", now, day));
        assert!(b.is_stale("binance", "perp", now, day - 1));

        // A row from the FUTURE never expires by subtraction, so it is stale by rule.
        let mut future = PaceBook::default();
        future.record(pace("perp", 280, 5.0, Some(2400), now + day));
        assert!(future.is_stale("binance", "perp", now, DEFAULT_MAX_PACE_AGE_MS));
        assert_eq!(future.fresh("binance", "perp", now, DEFAULT_MAX_PACE_AGE_MS), None);
        // ...but `get` still returns it, so a diagnostic can print the row and say WHY it is stale.
        assert!(future.get("binance", "perp").is_some());

        // A non-positive max age is the off switch: everything is stale, nothing is deleted.
        assert!(b.is_stale("binance", "perp", now, 0));
        assert!(b.is_stale("binance", "perp", now, -1));

        // Absurd timestamps saturate into "stale" rather than panicking on an i64 overflow.
        let mut wild = PaceBook::default();
        wild.record(pace("perp", 280, 5.0, Some(2400), i64::MIN));
        assert!(wild.is_stale("binance", "perp", i64::MAX, DEFAULT_MAX_PACE_AGE_MS));
    }

    /// Disk round-trip through JSON (serde_json is the in-tree witness), including the `None` budget
    /// and the insertion order the file's stable-diff property depends on.
    #[test]
    fn pace_book_serde_round_trip_preserves_order_and_values() {
        let mut b = PaceBook::default();
        b.record(pace("perp", 280, 5.0, Some(2400), 1_700_000_000_000));
        b.record(pace("spot", 120, 2.0, Some(6000), 1_700_000_000_000));
        b.record(MeasuredPace::new(
            "bybit",
            "spot",
            PaceSample {
                request_ms: 90,
                per_request_weight: 1.0,
                budget_per_min: None,
                samples: 7,
            },
            1_700_000_000_000,
        ));

        let json = serde_json::to_string(&b).expect("serialize");
        let back: PaceBook = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, b, "the disk round-trip is lossless");
        let keys: Vec<&str> = back.entries.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["binance:perp", "binance:spot", "bybit:spot"],
            "insertion order survives, so a re-save of an unchanged book is a byte-identical file"
        );
        // The `None` budget survives as `None`, not as a 0 that would then MATCH some real budget.
        assert_eq!(back.get("bybit", "spot").unwrap().budget_per_min, None);
        assert_eq!(back.get("binance", "perp").unwrap().budget_per_min, Some(2400));
    }

    /// The `#[serde(default)]` contract, which is what makes the whole feature additive: an empty
    /// object, a partial row, and a row missing every optional field all load rather than failing —
    /// and an unusable partial row is simply never fresh.
    #[test]
    fn missing_fields_default_and_an_empty_file_is_an_empty_book() {
        let empty: PaceBook = serde_json::from_str("{}").expect("an empty object is a valid book");
        assert!(empty.is_empty());
        assert!(empty.is_stale("binance", "perp", 1_000, DEFAULT_MAX_PACE_AGE_MS));

        let partial: PaceBook = serde_json::from_str(
            r#"{"entries":{"binance:perp":{"venue":"binance","market":"perp","request_ms":280,
                "per_request_weight":5.0,"measured_at_ms":1000,"samples":3}}}"#,
        )
        .expect("a row without `budget_per_min` is valid");
        let m = partial.get("binance", "perp").expect("the partial row loads");
        assert_eq!(m.budget_per_min, None, "the absent optional field defaults to None");
        assert_eq!(m.samples, 3);
        assert!(partial.fresh("binance", "perp", 1_000, DEFAULT_MAX_PACE_AGE_MS).is_some());

        // A row that is only a key is defaulted to zeros — and zeros are never usable, so it can
        // never seed anything.
        let hollow: PaceBook =
            serde_json::from_str(r#"{"entries":{"binance:perp":{}}}"#).expect("a bare row loads");
        assert!(!hollow.get("binance", "perp").unwrap().is_usable());
        assert!(hollow.is_stale("binance", "perp", 1_000, DEFAULT_MAX_PACE_AGE_MS));

        // A bare sample defaults to the unusable all-zero shape rather than a plausible-looking one.
        let s: PaceSample = serde_json::from_str("{}").expect("an empty sample is valid");
        assert_eq!(s, PaceSample::default());
        assert!(!s.is_usable());
    }
}
