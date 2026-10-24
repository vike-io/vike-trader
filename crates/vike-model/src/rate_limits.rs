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
//! ## The OTHER half: [`PaceSample`] — what was MEASURED, not what was configured
//!
//! [`RateLimitConfig`] above is a RISK PREFERENCE an operator sets. [`PaceSample`] is the opposite
//! kind of number: an OBSERVATION a pager made about a venue — the wall clock of a request, the
//! weight one request cost, and the budget both were measured against. The pacer that produces and
//! consumes it lives in `vike_bridge_core::pacer`; the bridges' paced pagers take one as a seed and
//! hand one back.
//!
//! ⚠ The PERSISTED half is gone: a `PaceBook` of `MeasuredPace` rows keyed `(venue, market)`, aged
//! against `DEFAULT_MAX_PACE_AGE_MS`, carried one run's measurement into the next through a
//! binary-side pace file. docs/decisions/0094 deleted the one-shot kline programs that were its only
//! writers and readers, and the types went with them.
//!
//! **A measurement informs SPEED, never the LIMIT.** Nothing here carries a ceiling. A pacer's
//! budget comes from DISCOVERY (the venue's own published `REQUEST_WEIGHT`), and a sample can only
//! tell it how long a request took and what one cost — never how much it may spend.
//! [`PaceSample::budget_per_min`] is recorded precisely so a consumer can REFUSE a sample that was
//! measured against a different budget; it is evidence, not permission.

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
// Measured pace — the observation half (see the module doc's second section).
// ---------------------------------------------------------------------------------------------

/// The narrow, venue-free pace observation a pacer produces and consumes: how long one request took,
/// what one request cost, and which budget those two numbers were measured against.
///
/// It carries no identity fields because a pacer legitimately knows none — it has no venue string,
/// no market, and (by contract) no clock. This is the whole payload that crosses the pacer boundary
/// in either direction.
///
/// Every field is `#[serde(default)]` via the struct attribute, so a partial sample still
/// deserializes — and the DERIVED `Default` is deliberately the all-zero shape, which
/// [`Self::is_usable`] rejects: a sample that says nothing must never look like a measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PaceSample {
    /// Observed per-request wall clock, in MILLISECONDS. Milliseconds and not seconds so a value
    /// reads at a glance: `280` is checkable where `0.28` invites a units mistake.
    pub request_ms: u64,
    /// Observed cost of ONE request in the venue's own weight units (binance spot `/klines` = 2,
    /// fapi = 5). The number a hardcoded seed constant guesses.
    ///
    /// ⚠ Only meaningful where the venue publishes a weight counter. A sample whose `budget_per_min`
    /// is `None` was measured on a venue that has no weight concept (bybit/okx/deribit), where the
    /// producing pacer never read this field and simply reported its own seed constant; it is
    /// carried because [`PaceSample::is_usable`] requires a positive weight, and it steers nothing —
    /// a `None` budget confines the sample to another fixed-delay pacer. Read `request_ms` there.
    pub per_request_weight: f64,
    /// The venue's published budget, weight units per MINUTE, at the time of measurement — or `None`
    /// when the venue publishes none (most of the roster). This is the MATCH KEY, not a ceiling: a
    /// consumer compares it against the budget IT discovered and discards the record when they
    /// disagree, because a request time and a weight measured on another host describe another host.
    pub budget_per_min: Option<u64>,
    /// How many requests this observation summarizes. `0` means "nothing was measured", which
    /// [`Self::is_usable`] rejects — a sample with nothing behind it is a guess wearing a
    /// measurement's clothes.
    pub samples: u64,
}

impl PaceSample {
    /// Is this sample safe to seed a pacer with?
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

#[path = "rate_limits_tests.rs"]
#[cfg(test)]
mod rate_limits_tests;
