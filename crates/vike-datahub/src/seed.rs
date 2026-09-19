//! **The chart-gap seed lane: the bounds only the SERVER can have**, and the one thing that arms
//! it.
//!
//! `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` classifies
//! [`Request::SeedSeries`](vike_datahub_client::proto::Request::SeedSeries) as
//! `VerbScope::Observe` on a rule that reduces to one sentence — **the client names a series and
//! every other term of the cost is a server constant** — and this module is where those constants
//! live. Its siblings in `crates/vike-datahub-client/src/seed.rs` are the ones BOTH ends need (the
//! window, the interval set, the two validators); the ones here are deliberately invisible to a
//! client, because a client that knew a token bucket's state could only ever mis-predict it.
//!
//! # What it bounds, and which objection each bound answers
//!
//! | bound | answers |
//! |---|---|
//! | [`SEED_VENUE_BURST`] / [`SEED_VENUE_REFILL`] | "an observe client can spend the box's venue budget" |
//! | [`SEED_MAX_SERIES`] | "an observe client can grow the operator's disk without limit" |
//! | the ledger's REPEAT arm | "a chart re-asks every frame" — proved server-side, not only by a well-behaved client |
//! | [`SeedLane`] existing at all | "the operator never consented" — an unarmed daemon builds none |
//!
//! # ⚠ The arming is what makes the SCOPE argument honest, so it is not a convenience
//!
//! An unarmed daemon constructs no `SeedLane`, and `crate::server`'s `seed_series_verb` then answers
//! a SUCCESSFUL `SeedDone { armed: false, .. }` having called no venue and written no row. That is
//! 0057's reach property 3, and it is the leg that separates this from a write an Observe client can
//! always cause. Making the lane default-ON, or removing the switch, is in that record's reopen list
//! — it is not a tidy-up.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Fetches this lane will let through to ONE venue back-to-back, before the refill rate binds.
///
/// Sized for the act this lane exists to serve: an operator restores a workspace and several charts
/// come up empty at once. Eight is above any layout a human arranges on one screen and below the
/// point where the burst itself is worth pricing — 8 x weight 5 is 40 weight against binance
/// `fapi`'s 2,400/min, i.e. inside one minute's rounding.
pub const SEED_VENUE_BURST: u32 = 8;

/// How often ONE token returns to a venue's bucket — the STEADY-STATE bound, and the number the
/// order-signing daemon's reservation is computed from.
///
/// **The arithmetic, from MEASURED figures in `crates/vike-model/src/rate_limits.rs`** (binance spot
/// publishes 6000 weight/min with `/klines?limit=1000` at weight 2; `fapi` publishes 2400/min with
/// the same call at weight 5, and `fapi` is what a `.P` chart resolves to):
///
/// * 5 s -> **12 fetches/min/venue** saturated;
/// * against `fapi`: 12 x 5 = **60 weight/min = 2.5%** of 2,400;
/// * against spot: 12 x 2 = 24 of 6,000 = 0.4%.
///
/// ⚠ **What that costs the order-signing daemon, stated as a number rather than as a reassurance.**
/// `crate::md::MD_LINGER`'s doc fixes the market-data plane's share at 800/min — one third of the
/// same IP budget — leaving the daemon 1,600/min, two thirds. This lane widens the OBSERVE side to
/// 860/min, so the daemon's floor moves from 66.7% to **64.2%**.
///
/// It is accepted for one reason, and it is the reason: **60/min is a SATURATION ceiling with a
/// ZERO steady state.** A seed happens once per series per process (the ledger below makes a repeat
/// free), after which this lane spends nothing at all, where MD's 800/min is what that plane costs
/// merely by being up. Reaching 60/min requires a client continuously naming series it has never
/// named before.
///
/// A budget SHARED between the two observe lanes is the correct eventual shape and is 0057's fourth
/// reopener; it needs the two subscription drivers merged, which `crate::datahub_cli` already
/// carries as a named follow-up for an unrelated reason.
pub const SEED_VENUE_REFILL: Duration = Duration::from_secs(5);

/// Distinct `(venue, symbol, interval)` series this PROCESS will ever seed.
///
/// The bound on the one dimension the request still names. 0057 declares that residual rather than
/// claiming parity with `crate::md::MD_MAX_KEYS_PER_VENUE`: a venue's instrument set is not a roster
/// this tree holds, so the symbol cannot be bounded by an allowlist the way a venue or an interval
/// can — only by a rate and by this count.
///
/// 256 series x [`vike_datahub_client::seed::SEED_BARS`] is on the order of 150k bar rows, single-
/// digit megabytes in the store. For scale, the box's own live plane is 14 series and a heavy human
/// layout is perhaps 50.
///
/// ⚠ **It is a LIFETIME count, not a concurrent one**, which is genuinely stricter than every MD cap
/// and is declared as such: an operator who charts more than 256 distinct series without restarting
/// gets a refusal naming this constant. That is accepted because the alternative — expiring ledger
/// entries — would re-admit the "one fetch per frame forever" failure this lane must not have, and
/// because a restart is the ordinary answer.
pub const SEED_MAX_SERIES: u32 = 256;

// The burst may not exceed the lifetime cap: a bucket that could admit more fetches in one breath
// than the process will ever perform would make the rate bound unreachable, and the two numbers
// would then be describing different lanes. The `crate::server` / `crate::md` idiom — an inequality,
// so a deliberate tweak compiles and a broken claim does not.
const _: () = assert!(
    SEED_VENUE_BURST <= SEED_MAX_SERIES,
    "SEED_VENUE_BURST must not exceed SEED_MAX_SERIES — a burst larger than the process's whole \
     series budget makes the refill rate unreachable and the two bounds stop describing one lane."
);

// The refill must be long enough that a SATURATED lane stays a small share of the tightest budget
// this workspace has MEASURED (binance `fapi`, 2,400 weight/min, weight 5 per kline page — see
// `SEED_VENUE_REFILL`'s derivation). Expressed as the inequality that arithmetic reduces to:
// 60 s / refill_secs fetches per minute, at weight 5, must stay at or under 5% of 2,400 = 120/min,
// i.e. 24 fetches/min, i.e. a refill of at least 2.5 s. The bound is deliberately looser than the
// 5 s chosen, so lowering the constant is possible without a recompile failure until it would
// genuinely start competing with the order-signing daemon — at which point this stops compiling and
// the author re-runs `SEED_VENUE_REFILL`'s arithmetic and 0057's reservation paragraph instead of
// quietly falsifying both.
const _: () = assert!(
    SEED_VENUE_REFILL.as_millis() >= 2_500,
    "SEED_VENUE_REFILL below 2.5 s pushes this lane past 5% of binance fapi's MEASURED 2,400 \
     weight/min budget, which is the share `docs/decisions/0057` reserved against the \
     order-signing daemon on the same public IP. Re-run that record's arithmetic, do not widen \
     this assertion."
);

/// The per-venue token bucket, as a plain value so the refill arithmetic is testable without a
/// clock.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: u32,
    last_refill: Instant,
}

impl Bucket {
    fn fresh(now: Instant) -> Self {
        Self { tokens: SEED_VENUE_BURST, last_refill: now }
    }

    /// Fold elapsed time into tokens, then spend one if there is one. `false` = refused.
    ///
    /// Integer division on purpose, with `last_refill` advanced by the WHOLE periods consumed
    /// rather than to `now`: advancing to `now` would discard the remainder every call, so a caller
    /// polling faster than the refill period would never accrue a token at all — the classic bucket
    /// bug, and the reason this is a method with a test rather than two lines at the call site.
    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let periods = (elapsed.as_millis() / SEED_VENUE_REFILL.as_millis()) as u64;
        if periods > 0 {
            self.tokens = self
                .tokens
                .saturating_add(u32::try_from(periods).unwrap_or(u32::MAX))
                .min(SEED_VENUE_BURST);
            self.last_refill += SEED_VENUE_REFILL * u32::try_from(periods).unwrap_or(u32::MAX);
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

/// What [`SeedLane::admit`] decided about one request, before any venue is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedAdmission {
    /// Fetch it. A token was spent and the series is now in the ledger.
    Fetch,
    /// This process has already seeded this series. No token spent, no venue call — the answer is a
    /// successful `SeedDone { repeated: true, rows_written: 0, .. }` off the store's own contents.
    ///
    /// ⚠ **This arm is what proves "exactly ONE fetch" server-side**, independently of the client's
    /// own once-per-session ledger. Two legs, neither substituting for the other: the client does
    /// not ask twice, and a client that does asks for nothing.
    Repeated,
    /// Refused, with the operator-facing reason. A `Response::Error`, because unlike an unarmed lane
    /// this IS a request the server declined rather than a configuration it is reporting.
    Refused(String),
}

/// The armed chart-seed lane: the venue buckets and the process's series ledger.
///
/// Built by `crate::datahub_cli` only when the operator set `VIKE_DATAHUB_CHART_SEED=1`, so its
/// mere EXISTENCE is the arming — there is no `enabled: bool` to get out of step with the
/// advertisement. `crate::server`'s `served_features` keys `FEATURE_SEED_SERIES` on
/// `Option::is_some` for exactly that reason, the same "advertised per mounted thing" rule
/// `FEATURE_BACKFILL` and `FEATURE_MARKET_DATA` already follow.
#[derive(Debug)]
pub struct SeedLane {
    state: Mutex<LaneState>,
}

#[derive(Debug)]
struct LaneState {
    /// venue -> bucket. One entry per venue actually asked for, so an unused venue costs nothing.
    buckets: std::collections::HashMap<String, Bucket>,
    /// Every `(venue, symbol, interval)` this process has admitted, ever.
    seeded: HashSet<(String, String, String)>,
}

impl Default for SeedLane {
    fn default() -> Self {
        Self::new()
    }
}

impl SeedLane {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LaneState {
                buckets: std::collections::HashMap::new(),
                seeded: HashSet::new(),
            }),
        }
    }

    /// Decide one request against the lane's own bounds. `now` is injected so the refill is testable
    /// without sleeping.
    ///
    /// Order matters and is cheapest-and-most-specific first: the LEDGER before the bucket, so a
    /// repeat is free rather than spending a token it does not need; the SERIES CAP before the
    /// bucket for the same reason; and the bucket last, because it is the only check with a side
    /// effect a refusal should not have paid for.
    pub fn admit(&self, venue: &str, symbol: &str, interval: &str, now: Instant) -> SeedAdmission {
        let key = (venue.to_string(), symbol.to_string(), interval.to_string());
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.seeded.contains(&key) {
            return SeedAdmission::Repeated;
        }
        let held = st.seeded.len() as u32;
        if held >= SEED_MAX_SERIES {
            return SeedAdmission::Refused(format!(
                "seed: this process has already seeded {held} distinct series, which is \
                 SEED_MAX_SERIES = {SEED_MAX_SERIES}. That is a LIFETIME bound on the chart-seed \
                 lane, not a concurrent one — restart the daemon to clear it. It exists because a \
                 series' SYMBOL is the one dimension of this verb a client still names, and a venue's \
                 instrument set is not a roster this server can bound it against."
            ));
        }
        let bucket = st.buckets.entry(venue.to_string()).or_insert_with(|| Bucket::fresh(now));
        if !bucket.take(now) {
            return SeedAdmission::Refused(format!(
                "seed: the `{venue}` fetch budget is spent — this lane admits {SEED_VENUE_BURST} \
                 back-to-back and then one per {} s, and that rate is what keeps it a small share \
                 of the venue-API budget this box shares with its order-signing daemon. Nothing was \
                 fetched; try this series again shortly.",
                SEED_VENUE_REFILL.as_secs()
            ));
        }
        st.seeded.insert(key);
        SeedAdmission::Fetch
    }

    /// Distinct series admitted so far — for the startup/diagnostic line, never for a decision.
    pub fn seeded_count(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).seeded.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> SeedLane {
        SeedLane::new()
    }

    #[test]
    fn a_repeat_is_free_and_spends_no_token() {
        // The server-side leg of "exactly ONE fetch, not one per frame". A client that ignores its
        // own ledger and asks a thousand times gets one fetch and 999 free answers, and — the part
        // that matters — does not drain the venue bucket doing it.
        let l = lane();
        let t0 = Instant::now();
        assert_eq!(l.admit("binance", "BTCUSDT", "5m", t0), SeedAdmission::Fetch);
        for _ in 0..1_000 {
            assert_eq!(l.admit("binance", "BTCUSDT", "5m", t0), SeedAdmission::Repeated);
        }
        // ...and the bucket still has its full burst minus the ONE real fetch: seven more distinct
        // series go through without waiting.
        for i in 0..(SEED_VENUE_BURST - 1) {
            assert_eq!(
                l.admit("binance", &format!("SYM{i}USDT"), "5m", t0),
                SeedAdmission::Fetch,
                "series {i} should still be inside the burst"
            );
        }
        assert_eq!(l.seeded_count(), SEED_VENUE_BURST as usize);
    }

    #[test]
    fn the_burst_is_exhausted_then_refills_one_token_per_period() {
        let l = lane();
        let t0 = Instant::now();
        for i in 0..SEED_VENUE_BURST {
            assert_eq!(l.admit("binance", &format!("S{i}"), "1m", t0), SeedAdmission::Fetch);
        }
        match l.admit("binance", "OVER", "1m", t0) {
            SeedAdmission::Refused(why) => {
                assert!(why.contains("budget is spent"), "{why}");
                assert!(why.contains("order-signing daemon"), "the reason is named: {why}");
            }
            other => panic!("the {}th fetch must be refused, got {other:?}", SEED_VENUE_BURST + 1),
        }
        // One refill period later, exactly ONE more goes through.
        let t1 = t0 + SEED_VENUE_REFILL;
        assert_eq!(l.admit("binance", "AFTER1", "1m", t1), SeedAdmission::Fetch);
        assert!(matches!(l.admit("binance", "AFTER2", "1m", t1), SeedAdmission::Refused(_)));
    }

    #[test]
    fn polling_faster_than_the_refill_still_accrues_tokens() {
        // The bucket bug this `take` is written against: advancing `last_refill` to `now` on every
        // call would discard the remainder, so a caller polling every second against a 5 s period
        // would never accrue anything. Drain, then poll at 1 s intervals and require a token by the
        // 5th — which is the behaviour a per-frame GUI caller actually produces.
        let l = lane();
        let t0 = Instant::now();
        for i in 0..SEED_VENUE_BURST {
            assert_eq!(l.admit("binance", &format!("S{i}"), "1m", t0), SeedAdmission::Fetch);
        }
        let mut got = None;
        for s in 1..=6u64 {
            let t = t0 + Duration::from_secs(s);
            if l.admit("binance", &format!("P{s}"), "1m", t) == SeedAdmission::Fetch {
                got = Some(s);
                break;
            }
        }
        assert_eq!(got, Some(SEED_VENUE_REFILL.as_secs()), "a token must accrue on schedule");
    }

    #[test]
    fn the_buckets_are_per_venue_so_one_venue_cannot_starve_another() {
        let l = lane();
        let t0 = Instant::now();
        for i in 0..SEED_VENUE_BURST {
            assert_eq!(l.admit("binance", &format!("S{i}"), "1m", t0), SeedAdmission::Fetch);
        }
        assert!(matches!(l.admit("binance", "X", "1m", t0), SeedAdmission::Refused(_)));
        assert_eq!(l.admit("okx", "BTC-USDT", "1m", t0), SeedAdmission::Fetch);
        assert_eq!(l.admit("bybit", "BTCUSDT", "1m", t0), SeedAdmission::Fetch);
    }

    #[test]
    fn the_lifetime_series_cap_refuses_by_name_and_says_a_restart_clears_it() {
        let l = lane();
        // Walk the clock so the bucket never binds first — this test is about the OTHER cap, and a
        // bucket refusal would make it pass for the wrong reason.
        let mut t = Instant::now();
        for i in 0..SEED_MAX_SERIES {
            assert_eq!(
                l.admit("binance", &format!("S{i}"), "1m", t),
                SeedAdmission::Fetch,
                "series {i}"
            );
            t += SEED_VENUE_REFILL;
        }
        match l.admit("binance", "ONE_TOO_MANY", "1m", t) {
            SeedAdmission::Refused(why) => {
                assert!(why.contains("SEED_MAX_SERIES"), "{why}");
                assert!(why.contains("restart"), "the operator's action is named: {why}");
            }
            other => panic!("the cap must refuse, got {other:?}"),
        }
    }

    #[test]
    fn the_series_identity_is_all_three_dimensions() {
        // A second interval on the same symbol is a different series and gets its own fetch — the
        // whole defect this verb exists for is a 5m chart beside a 1m one.
        let l = lane();
        let t0 = Instant::now();
        assert_eq!(l.admit("binance", "BTCUSDT", "1m", t0), SeedAdmission::Fetch);
        assert_eq!(l.admit("binance", "BTCUSDT", "5m", t0), SeedAdmission::Fetch);
        assert_eq!(l.admit("bybit", "BTCUSDT", "1m", t0), SeedAdmission::Fetch);
        assert_eq!(l.admit("binance", "ETHUSDT", "1m", t0), SeedAdmission::Fetch);
        assert_eq!(l.seeded_count(), 4);
    }
}
