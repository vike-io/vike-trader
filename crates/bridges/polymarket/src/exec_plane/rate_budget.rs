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
//! floor) is consulted on the LIVE cancel path: `vike_polymarket::exec_plane::exec`'s `gate_cancel` runs it
//! from `crate::exec_plane::client`'s `ExecCommand::Cancel` arm, the door every SINGLE core cancel comes
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
/// venue treats as one — never consults this at all (`vike_polymarket::exec_plane::exec`'s `gate_cancel` is
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

#[path = "rate_budget_tests.rs"]
#[cfg(test)]
mod rate_budget_tests;
