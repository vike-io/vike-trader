//! Client-side mirror of Polymarket's per-signer REST token-bucket limiter (PM deep-dive finding #2).
//!
//! Polymarket runs **separate order + cancel token buckets per signer**, tiered by 30-day maker
//! volume and refreshed roughly every 3h. The limiter entered *warning* mode 2026-07-24; live
//! enforcement is ~2 weeks out. The hazard the deep-dive flagged: `DELETE /cancel-all` debits **one
//! token up front plus one per order actually canceled**, which can drive the cancel bucket
//! *negative* and block **all** subsequent cancels — the worst possible failure for a maker resting
//! quotes across rolling 5m markets (getting cancel-locked with live orders out).
//!
//! This module is a **pure, deterministic mirror** — time is injected (`now_ms`), no
//! `Instant::now()` inside — so the exec layer can:
//!
//! 1. gate submits locally *before* they cost a token (observe-only until enforcement is live);
//! 2. keep routine ladder-churn cancels from draining the cancel bucket below a reserve floor, so an
//!    emergency flatten is never rate-limit-locked (the **cancel-token reserve invariant**);
//! 3. prefer **targeted** cancels over `cancel-all` under pressure — targeted spends to exactly zero
//!    and stops, where `cancel-all`'s `1 + N` debit can overshoot into a lockout; and
//! 4. reconcile the mirror to the venue-authoritative `Poly-RateLimit-*` response headers and
//!    surface the warning during the grace window.
//!
//! ⚠ **Capabilities 2 AND 3 are ARMED.** [`RateBudget::routine_cancel_decision`] (the reserve
//! floor) is consulted on the LIVE cancel path: `vike_polymarket::exec`'s `gate_cancel` runs it
//! from `crate::client`'s `ExecCommand::Cancel` arm, the door every SINGLE core cancel comes
//! through, for a `vike_exec::CancelIntent::Routine` cancel and for nothing else — so this bucket
//! is no longer a write-only counter and the reserve genuinely holds churn back. What made that
//! possible was giving the shared `ExecutionClient::cancel` seam an INTENT (`cancel_with_intent`),
//! since the classification originates in `vike_core`'s runtime, not here.
//!
//! The targeted-vs-`cancel-all` preference (capability 3) took a SECOND shared-seam change to
//! reach, and the reason is worth keeping: `plan_cancels`/`cancel_orders` had no in-tree caller
//! because `ExecActor::cancel_batch_with_intent` fanned a batch out into `n` per-id commands before
//! any venue code ran, so the `1 + n` overshoot they guard against could not occur — the guards
//! guarded nothing. `ExecCommand::CancelBatch` (this venue declares the lane via
//! `ExecActor::with_bulk_cancel`) carries the batch intact to the exec thread, which runs it
//! through `cancel_orders`. Both the strategy choice and the reserve now apply to a batch.
//!
//! The per-tier rates below are the deep-dive's documented figures; the mirror is intentionally
//! conservative and self-correcting — [`RateBudget::reconcile`] snaps each bucket to the venue's
//! reported `remaining` whenever a response carries it, so an imperfect local model can never drift
//! more permissive than the server for longer than one round-trip.

/// Maker fee/limit tier, selected by trailing-30-day maker volume (USD). The three concrete points
/// are the deep-dive's documented values; higher tiers fall through to the highest known rate until
/// the live schema is confirmed. Both the order and the cancel bucket refill at the tier's rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    /// Default tier — ~40 req/s per bucket.
    #[default]
    Standard,
    /// `$30k+` trailing-30d maker volume — ~60 req/s per bucket.
    Copper,
    /// `$100k+` trailing-30d maker volume — ~200 req/s per bucket.
    Silver,
}

impl Tier {
    /// Classify a trailing-30-day maker volume (USD) into its tier.
    pub fn from_volume_usd_30d(usd: f64) -> Self {
        if usd >= 100_000.0 {
            Tier::Silver
        } else if usd >= 30_000.0 {
            Tier::Copper
        } else {
            Tier::Standard
        }
    }

    /// Per-bucket refill rate in tokens/sec (the venue applies this to the order AND cancel buckets).
    pub const fn per_sec(self) -> f64 {
        match self {
            Tier::Standard => 40.0,
            Tier::Copper => 60.0,
            Tier::Silver => 200.0,
        }
    }
}

/// The bucket holds `per_sec * BURST_SECS` tokens at rest — a 1-second burst window, the
/// conservative reading of a per-second limit. Widened only if the live headers prove a larger
/// burst; [`RateBudget::reconcile`] corrects it either way.
const BURST_SECS: f64 = 1.0;

/// A single leaky/token bucket. `tokens` is allowed to go **negative** via [`Self::take_saturating`]
/// so the mirror can faithfully model `cancel-all`'s over-debit and refuse to pretend the bucket is
/// healthier than the venue's.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_ms: i64,
}

impl TokenBucket {
    /// A full bucket as of `now_ms`.
    pub fn new(capacity: f64, refill_per_sec: f64, now_ms: i64) -> Self {
        Self { tokens: capacity, capacity, refill_per_sec, last_ms: now_ms }
    }

    /// Accrue tokens for the elapsed wall-time since the last touch (clamped at `capacity`, never
    /// backwards for a non-monotonic clock).
    fn refill_to(&mut self, now_ms: i64) {
        let dt = (now_ms - self.last_ms).max(0) as f64 / 1000.0;
        self.tokens = (self.tokens + dt * self.refill_per_sec).min(self.capacity);
        self.last_ms = now_ms;
    }

    /// Tokens available as of `now_ms` (refills first).
    pub fn available(&mut self, now_ms: i64) -> f64 {
        self.refill_to(now_ms);
        self.tokens
    }

    /// Take `n` tokens iff at least `n` are available; returns whether the take succeeded.
    pub fn try_take(&mut self, n: f64, now_ms: i64) -> bool {
        self.refill_to(now_ms);
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Debit `n` unconditionally — may drive `tokens` negative (the `cancel-all` over-debit model).
    pub fn take_saturating(&mut self, n: f64, now_ms: i64) {
        self.refill_to(now_ms);
        self.tokens -= n;
    }

    /// Snap the bucket to the venue-authoritative `remaining` (from a `Poly-RateLimit-*` header),
    /// clamped to capacity — the server is always the source of truth.
    pub fn set_remaining(&mut self, remaining: f64, now_ms: i64) {
        self.refill_to(now_ms);
        self.tokens = remaining.min(self.capacity);
    }

    /// Milliseconds until at least `n` tokens are available (0 if already, given a positive rate).
    fn refill_eta_ms(&self, n: f64) -> i64 {
        let deficit = n - self.tokens;
        if deficit <= 0.0 || self.refill_per_sec <= 0.0 {
            0
        } else {
            (deficit / self.refill_per_sec * 1000.0).ceil() as i64
        }
    }
}

/// Whether a submit is within local budget right now. Observe-only until venue enforcement is live —
/// the exec layer logs a `Throttle` rather than blocking, so the mirror can never itself become a
/// new liveness hazard before it is proven against the real headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitDecision {
    /// Order bucket has a token.
    Allow,
    /// Order bucket is dry; `retry_ms` until the next token refills.
    Throttle { retry_ms: i64 },
}

/// Whether a **routine** ladder-churn cancel should fire now, or defer to protect the reserve.
/// A cancel the reserve may not shed — a risk-off flatten, or an UNCLASSIFIED cancel, which this
/// venue treats as one — never consults this at all (`vike_polymarket::exec`'s `gate_cancel` is
/// where that is decided): it always fires, because a 429 on a cancel beats not trying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelDecision {
    /// Cancel bucket is above the reserve floor — routine churn is fine.
    Allow,
    /// At/below the reserve — shed routine churn; `retry_ms` until back above the floor.
    Defer { retry_ms: i64 },
}

/// How to cancel a set of `n` resting orders under the current cancel budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStrategy {
    /// One `DELETE /cancel-all` (cheapest wire round-trip) — safe: its `1 + n` debit stays ≥ 0.
    CancelAll,
    /// Cancel targeted one-by-one — `cancel-all` would over-debit the bucket **negative** and lock
    /// out every future cancel; targeted spends to exactly zero, stops, and retries after refill.
    Targeted,
}

/// A per-signer client-side mirror of the venue's dual buckets, plus the reserve invariant.
#[derive(Debug, Clone)]
pub struct RateBudget {
    tier: Tier,
    order: TokenBucket,
    cancel: TokenBucket,
    /// Cancel tokens held back from routine churn for emergency flattening (the reserve invariant).
    cancel_reserve: f64,
}

/// Default share of cancel capacity reserved for risk-off flattening.
const DEFAULT_RESERVE_FRAC: f64 = 0.25;

impl RateBudget {
    /// A fresh full budget for `tier`, reserving [`DEFAULT_RESERVE_FRAC`] of cancel capacity.
    pub fn new(tier: Tier, now_ms: i64) -> Self {
        Self::with_reserve_frac(tier, DEFAULT_RESERVE_FRAC, now_ms)
    }

    /// A fresh full budget with an explicit reserve fraction (clamped to `[0, 1]`).
    pub fn with_reserve_frac(tier: Tier, reserve_frac: f64, now_ms: i64) -> Self {
        let cap = tier.per_sec() * BURST_SECS;
        Self {
            tier,
            order: TokenBucket::new(cap, tier.per_sec(), now_ms),
            cancel: TokenBucket::new(cap, tier.per_sec(), now_ms),
            cancel_reserve: cap * reserve_frac.clamp(0.0, 1.0),
        }
    }

    /// The current tier.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Re-tier after a venue volume-window refresh (~3h). Capacities/rates change; the reserve is
    /// re-derived; existing fills are clamped to the new capacity, never inflated.
    pub fn retier(&mut self, tier: Tier, reserve_frac: f64, now_ms: i64) {
        let cap = tier.per_sec() * BURST_SECS;
        self.order.refill_to(now_ms);
        self.cancel.refill_to(now_ms);
        self.order.capacity = cap;
        self.order.refill_per_sec = tier.per_sec();
        self.order.tokens = self.order.tokens.min(cap);
        self.cancel.capacity = cap;
        self.cancel.refill_per_sec = tier.per_sec();
        self.cancel.tokens = self.cancel.tokens.min(cap);
        self.cancel_reserve = cap * reserve_frac.clamp(0.0, 1.0);
        self.tier = tier;
    }

    /// Snap either bucket to the venue-authoritative `remaining` reported in a response's
    /// `Poly-RateLimit-*` headers. The server always wins — this is what keeps an imperfect local
    /// model from drifting more permissive than reality for longer than one round-trip.
    pub fn reconcile(&mut self, signal: &RateLimitSignal, now_ms: i64) {
        if let Some(r) = signal.order_remaining {
            self.order.set_remaining(r, now_ms);
        }
        if let Some(r) = signal.cancel_remaining {
            self.cancel.set_remaining(r, now_ms);
        }
    }

    /// Whether a submit is within budget (observe-only — see [`SubmitDecision`]). Does **not** debit.
    pub fn submit_decision(&mut self, now_ms: i64) -> SubmitDecision {
        if self.order.available(now_ms) >= 1.0 {
            SubmitDecision::Allow
        } else {
            SubmitDecision::Throttle { retry_ms: self.order.refill_eta_ms(1.0) }
        }
    }

    /// Debit one order token for a submit that went out.
    pub fn on_submit(&mut self, now_ms: i64) {
        self.order.take_saturating(1.0, now_ms);
    }

    /// Whether a **routine** ladder-churn cancel should fire, honoring the reserve floor. A cancel
    /// the reserve may not shed does NOT call this — it fires unconditionally.
    pub fn routine_cancel_decision(&mut self, now_ms: i64) -> CancelDecision {
        let avail = self.cancel.available(now_ms);
        if avail - 1.0 >= self.cancel_reserve {
            CancelDecision::Allow
        } else {
            // Wait until we're one token above the reserve again.
            CancelDecision::Defer { retry_ms: self.cancel.refill_eta_ms(self.cancel_reserve + 1.0) }
        }
    }

    /// Debit one cancel token for a single targeted cancel.
    pub fn on_cancel(&mut self, now_ms: i64) {
        self.cancel.take_saturating(1.0, now_ms);
    }

    /// Debit `cancel-all`'s `1 + n_canceled` tokens (the venue's over-debit model). May go negative
    /// — which is exactly why [`Self::cancel_strategy`] avoids `cancel-all` under pressure.
    pub fn on_cancel_all(&mut self, n_canceled: usize, now_ms: i64) {
        self.cancel.take_saturating(1.0 + n_canceled as f64, now_ms);
    }

    /// Choose how to cancel `n_resting` orders. `cancel-all` is cheaper on the wire but its `1 + n`
    /// debit can drive the bucket **negative** (a full cancel lockout); prefer targeted whenever
    /// that would happen, so the bucket only ever floors at zero.
    pub fn cancel_strategy(&mut self, n_resting: usize, now_ms: i64) -> CancelStrategy {
        let avail = self.cancel.available(now_ms);
        if 1.0 + n_resting as f64 <= avail {
            CancelStrategy::CancelAll
        } else {
            CancelStrategy::Targeted
        }
    }

    /// Cancel tokens available as of `now_ms` (for telemetry / maker load-shed decisions).
    pub fn cancel_available(&mut self, now_ms: i64) -> f64 {
        self.cancel.available(now_ms)
    }

    /// Order tokens available as of `now_ms` (for telemetry / maker load-shed decisions).
    pub fn order_available(&mut self, now_ms: i64) -> f64 {
        self.order.available(now_ms)
    }
}

/// Parsed `Poly-RateLimit-*` response-header signal. `warning` is the reliably-documented header
/// (finding #2); the numeric fields are best-effort until the live schema is confirmed via arbdub.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimitSignal {
    /// The venue is signaling the signer is over budget (grace window today, enforced soon).
    pub warning: bool,
    /// Raw `Poly-RateLimit-Warning` header value, if present.
    pub raw_warning: Option<String>,
    /// Order bucket tokens remaining, if the venue reports it.
    pub order_remaining: Option<f64>,
    /// Cancel bucket tokens remaining, if the venue reports it.
    pub cancel_remaining: Option<f64>,
    /// Seconds until the buckets refresh, if the venue reports it.
    pub reset_secs: Option<i64>,
}

impl RateLimitSignal {
    /// Whether this signal carries anything worth surfacing/reconciling.
    pub fn is_actionable(&self) -> bool {
        self.warning
            || self.order_remaining.is_some()
            || self.cancel_remaining.is_some()
            || self.reset_secs.is_some()
    }
}

/// Parse the rate-limit signal out of an iterator of `(header_name, header_value)` pairs
/// (case-insensitive on the name). Unknown headers are ignored; a missing set yields the default
/// (non-actionable) signal.
pub fn parse_headers<'a>(headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> RateLimitSignal {
    let mut sig = RateLimitSignal::default();
    for (name, value) in headers {
        let lname = name.to_ascii_lowercase();
        match lname.as_str() {
            "poly-ratelimit-warning" => {
                let v = value.trim();
                // Any non-empty, non-"0"/"false" value is a warning.
                sig.warning = !v.is_empty() && !v.eq_ignore_ascii_case("false") && v != "0";
                sig.raw_warning = Some(value.to_string());
            }
            "poly-ratelimit-order-remaining" => sig.order_remaining = value.trim().parse().ok(),
            "poly-ratelimit-cancel-remaining" => sig.cancel_remaining = value.trim().parse().ok(),
            "poly-ratelimit-reset" => sig.reset_secs = value.trim().parse().ok(),
            _ => {}
        }
    }
    sig
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn tier_selection_matches_documented_thresholds() {
        assert_eq!(Tier::from_volume_usd_30d(0.0), Tier::Standard);
        assert_eq!(Tier::from_volume_usd_30d(29_999.0), Tier::Standard);
        assert_eq!(Tier::from_volume_usd_30d(30_000.0), Tier::Copper);
        assert_eq!(Tier::from_volume_usd_30d(99_999.0), Tier::Copper);
        assert_eq!(Tier::from_volume_usd_30d(100_000.0), Tier::Silver);
        assert_eq!(Tier::from_volume_usd_30d(5_000_000.0), Tier::Silver);
        assert!(approx(Tier::Standard.per_sec(), 40.0));
        assert!(approx(Tier::Copper.per_sec(), 60.0));
        assert!(approx(Tier::Silver.per_sec(), 200.0));
    }

    #[test]
    fn bucket_refills_at_rate_and_clamps_to_capacity() {
        let mut b = TokenBucket::new(40.0, 40.0, 0);
        assert!(b.try_take(40.0, 0)); // drain
        assert!(!b.try_take(1.0, 0)); // empty
        assert!(approx(b.available(500), 20.0)); // 0.5s * 40/s = 20
        assert!(approx(b.available(10_000), 40.0)); // long wait clamps at capacity
    }

    #[test]
    fn bucket_never_refills_backwards_on_non_monotonic_clock() {
        let mut b = TokenBucket::new(40.0, 40.0, 1_000);
        b.take_saturating(10.0, 1_000);
        // A clock that went backwards must not accrue tokens.
        assert!(approx(b.available(500), 30.0));
    }

    #[test]
    fn take_saturating_goes_negative_for_cancel_all_model() {
        let mut b = TokenBucket::new(5.0, 40.0, 0);
        b.take_saturating(8.0, 0); // 1 + 7 canceled, only 5 in bucket
        assert!(b.available(0) < 0.0);
        assert!(approx(b.available(0), -3.0));
    }

    #[test]
    fn submit_allows_then_throttles_with_eta() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        // Drain the order bucket.
        for _ in 0..40 {
            assert_eq!(rb.submit_decision(0), SubmitDecision::Allow);
            rb.on_submit(0);
        }
        match rb.submit_decision(0) {
            SubmitDecision::Throttle { retry_ms } => assert!(retry_ms > 0 && retry_ms <= 25),
            other => panic!("expected throttle, got {other:?}"),
        }
        // 25ms → exactly one token at 40/s.
        assert_eq!(rb.submit_decision(25), SubmitDecision::Allow);
    }

    #[test]
    fn routine_cancel_defers_at_reserve_but_flatten_always_fires() {
        // Standard: cap 40, reserve 25% = 10.
        let mut rb = RateBudget::new(Tier::Standard, 0);
        // Drain cancel bucket down toward the reserve via routine cancels.
        let mut allowed = 0;
        for _ in 0..40 {
            if let CancelDecision::Allow = rb.routine_cancel_decision(0) {
                rb.on_cancel(0);
                allowed += 1;
            } else {
                break;
            }
        }
        // Routine churn stops leaving ~the reserve (10) intact: allowed ~= 30.
        assert!((29..=30).contains(&allowed), "allowed={allowed}");
        assert!(rb.cancel_available(0) >= 10.0 - EPS);
        // A risk-off flatten ignores the decision and fires regardless, spending into the reserve.
        rb.on_cancel(0);
        rb.on_cancel(0);
        assert!(rb.cancel_available(0) < 10.0);
    }

    #[test]
    fn cancel_strategy_prefers_targeted_when_cancel_all_would_go_negative() {
        let mut rb = RateBudget::new(Tier::Standard, 0); // cap 40
        // Plenty of budget -> cancel-all is fine.
        assert_eq!(rb.cancel_strategy(20, 0), CancelStrategy::CancelAll);
        // Drain to 5 tokens.
        rb.cancel.take_saturating(35.0, 0);
        assert!(approx(rb.cancel_available(0), 5.0));
        // 1 + 10 = 11 > 5 → would go negative → targeted.
        assert_eq!(rb.cancel_strategy(10, 0), CancelStrategy::Targeted);
        // 1 + 3 = 4 <= 5 → still safe to cancel-all.
        assert_eq!(rb.cancel_strategy(3, 0), CancelStrategy::CancelAll);
    }

    #[test]
    fn reconcile_snaps_buckets_to_venue_remaining() {
        let mut rb = RateBudget::new(Tier::Silver, 0); // cap 200
        let sig = RateLimitSignal {
            order_remaining: Some(3.0),
            cancel_remaining: Some(150.0),
            ..Default::default()
        };
        rb.reconcile(&sig, 0);
        assert!(approx(rb.order_available(0), 3.0));
        assert!(approx(rb.cancel_available(0), 150.0));
        // Server value above capacity is clamped, never inflated.
        let over = RateLimitSignal { order_remaining: Some(9_999.0), ..Default::default() };
        rb.reconcile(&over, 0);
        assert!(approx(rb.order_available(0), 200.0));
    }

    #[test]
    fn retier_rescales_capacity_and_reserve() {
        let mut rb = RateBudget::new(Tier::Standard, 0); // cap 40, reserve 10
        rb.retier(Tier::Silver, DEFAULT_RESERVE_FRAC, 0); // cap 200, reserve 50
        assert_eq!(rb.tier(), Tier::Silver);
        assert!(rb.cancel_available(0) <= 200.0 + EPS);
        // After retier a full-ish standard bucket is clamped to <= old fill, then refills to new cap.
        assert!(approx(rb.order_available(10_000), 200.0));
    }

    #[test]
    fn parse_headers_reads_warning_and_numbers_case_insensitively() {
        let sig = parse_headers([
            ("Poly-RateLimit-Warning", "1"),
            ("poly-ratelimit-order-remaining", "7"),
            ("POLY-RATELIMIT-CANCEL-REMAINING", "12"),
            ("Poly-RateLimit-Reset", "180"),
            ("content-type", "application/json"),
        ]);
        assert!(sig.warning);
        assert_eq!(sig.raw_warning.as_deref(), Some("1"));
        assert_eq!(sig.order_remaining, Some(7.0));
        assert_eq!(sig.cancel_remaining, Some(12.0));
        assert_eq!(sig.reset_secs, Some(180));
        assert!(sig.is_actionable());
    }

    #[test]
    fn parse_headers_treats_zero_false_empty_as_no_warning() {
        for v in ["0", "false", "False", "", "   "] {
            let sig = parse_headers([("poly-ratelimit-warning", v)]);
            assert!(!sig.warning, "value {v:?} should not warn");
        }
        // No rate headers at all → non-actionable default.
        let none = parse_headers([("content-type", "application/json")]);
        assert!(!none.is_actionable());
    }
}
