//! The CROSS-EXCHANGE market-maker (xEMM) TUNING SURFACE — the knobs of `vike_mm::XemmMaker`.
//!
//! The sibling of [`maker_params`](super::maker_params) for the two-venue maker, and it lives down
//! here for the same wire-schema reason: it rides the typed live-parameter payload
//! [`crate::StrategyParams::Xemm`], which is a serde field of the runtime's journaled command lane
//! (`vike_exec::lanes::Command::UpdateParams`) — a LOWER layer than any strategy — so the lane must
//! be able to name it.
//!
//! Ports the tuning surface of Hummingbot's cross-exchange market making strategy (Apache-2.0,
//! `hummingbot/strategy/cross_exchange_market_making`): `min_profitability`, `order_amount` (`qty`)
//! and `slippage_buffer` map onto the pricing knobs here; the risk half (staleness bounds, hedge
//! timeout/retries, naked bands, the halt latch) is vike's own — Hummingbot's version hedges
//! best-effort and has no equivalent of the halt/pull discipline the live core makes cheap.
//!
//! # ⚠ THE DEFAULTS SHIP SAFETY **ON**, deliberately inverting this workspace's usual polarity
//!
//! Everywhere else in vike a new knob's default is NEUTRAL, so an unconfigured build is
//! byte-identical to the one before the knob existed. That rule exists to protect EXISTING
//! behaviour. xEMM has none: it is new code, and every one of its guards has an *unsafe* neutral
//! value — `max_ref_age_ms = 0` means "quote off a dead reference forever", `hedge_max_attempts = 0`
//! means "never retry a lost hedge", `naked_hard_band = ∞` means "never flatten". A strategy that
//! signs real orders on two venues at once must not be one forgotten field away from quoting into a
//! frozen feed, so [`XemmParams::default`] sets real bounds and `resume_after_halt_ms = 0`
//! (MANUAL resume only). This is called out because it is a knowing departure, not an oversight.
//!
//! Every field is `Copy` (the two SYMBOLS are mount IDENTITY and live OUTSIDE this bag, on the
//! maker itself), which is what makes `apply_params` one struct copy: a hot re-tune can widen the
//! edge or cut size, but it can never repoint a leg mid-flight while an unhedged position is open.

/// The live tunables of `vike_mm::XemmMaker` — the payload of a [`crate::StrategyParams::Xemm`]
/// update AND the shape the maker reports its current settings as. `f64` fields ⇒ `PartialEq`, no
/// `Eq`.
///
/// See the module doc for why [`Default`] is safety-on rather than neutral.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct XemmParams {
    // --- sizing -------------------------------------------------------------------------------
    /// Base quote size per side on the MAKER venue, in base units (size at flat inventory).
    pub qty: f64,
    /// Fraction of a maker fill to offset on the taker venue. `1.0` = fully hedged (the only
    /// defensible v1 setting); `< 1.0` leaves a deliberate directional residual; `0.0` disables
    /// hedging entirely and turns this into an UNHEDGED maker — which the naked bands below will
    /// then halt, by design.
    pub hedge_ratio: f64,
    /// Residual hedge size below which nothing is sent, in base units. A venue's `min_qty` rejects
    /// dust, and a rejected hedge is an unhedged position that never clears — so a residual under
    /// this is treated as HEDGED and forgiven rather than retried forever. Set it to the taker
    /// venue's `min_qty`.
    pub hedge_dust: f64,

    // --- pricing ------------------------------------------------------------------------------
    /// Required edge per side as a FRACTION of the reference price (e.g. `0.0005` = 5 bps) — the
    /// `min_profitability` of `xemm_maker_quotes`. The maker backs each side off the reference
    /// touch by `min_profitability + total_fee`.
    pub min_profitability: f64,
    /// The round-trip fee as a FRACTION of price: maker-leg fee + taker-leg fee. Resolve it with
    /// [`crate::fees::xemm_round_trip_fee`] — do NOT hand-enter a number, and never default a
    /// venue whose shape has no flat rate to `0.0` (that function refuses instead, for that exact
    /// reason).
    pub total_fee: f64,
    /// Minimum standoff from the maker venue's own touch, in TICKS, applied by the passive clamp.
    /// `>= 1.0`: at `1.0` the clamped quote sits exactly one tick inside the touch (the tightest
    /// non-marketable price); larger values rest deeper and fill less often. Values below `1.0`
    /// are clamped up to `1.0` by the clamp itself — a sub-tick standoff rounds onto the touch and
    /// becomes marketable.
    pub min_edge_ticks: f64,
    /// The MAKER venue's price grid. Every emitted price is snapped onto it — the bid DOWN and the
    /// ask UP, never half-to-even, so a snap can never round a quote back through the clamp and
    /// make it marketable. `<= 0.0` disables snapping (the price is emitted as computed).
    pub maker_tick_size: f64,

    // --- basis (DIAGNOSTIC + halt band; never a price term) ------------------------------------
    /// Halt band on the observed maker-vs-reference basis, in basis points of the reference mid.
    /// `|basis| > max_basis_bps` ⇒ halt + pull both sides. This is what turns "this pair has
    /// decoupled / I am mounted on the wrong instrument / a feed is lying" into an operator-visible
    /// fact instead of a silent one-sided accumulation. `<= 0.0` disables the band.
    pub max_basis_bps: f64,
    /// EVENT-TIME half-life of the basis EWMA, in milliseconds. `<= 0` ⇒ the estimator never warms
    /// and the band above never fires (the estimator is inert, not merely quiet).
    pub basis_halflife_ms: i64,
    /// Absolute clamp on ONE folded basis observation, as a fraction. A single crossed/garbage
    /// touch must not be able to move the estimate arbitrarily far. `<= 0.0` disables clamping.
    pub basis_clamp: f64,

    // --- inventory ----------------------------------------------------------------------------
    /// Inventory the size skew biases the MAKER leg toward (`0.0` = stay flat).
    pub target_inventory: f64,
    /// Inventory band the skew ramps over (`> 0` to engage; `<= 0` disables the skew).
    pub max_inventory: f64,
    /// Skew intensity. `0.0` = neutral (both size multipliers exactly `1.0`).
    pub skew: f64,

    // --- freshness ----------------------------------------------------------------------------
    /// Maximum age of the REFERENCE touch, in milliseconds, before the maker halts and pulls. This
    /// is strictly stronger than a feed-status subscription: it also catches a silently FROZEN feed
    /// that never reports a disconnect. `<= 0` disables the check — which means quoting off a dead
    /// reference forever, so it is never the default.
    pub max_ref_age_ms: i64,
    /// Maximum age of the MAKER venue's OWN touch, in milliseconds. The passive clamp anchors on
    /// it, so a stale own-touch means the clamp is anchored on a price that no longer exists.
    /// `<= 0` disables the check.
    pub max_own_touch_age_ms: i64,
    /// Maximum gap between EMISSIONS, in milliseconds — the all-lanes-quiet watchdog. If neither
    /// venue has produced a tick for this long the maker halts and pulls, because "no ticks" and
    /// "both feeds died" are indistinguishable from inside the strategy. Evaluated on whatever lane
    /// does fire (including the periodic `on_schedule` sweep). `<= 0` disables it.
    pub max_emission_gap_ms: i64,

    // --- hedge discipline ---------------------------------------------------------------------
    /// How long a fired hedge may stay unfilled before it is RE-fired, in milliseconds. The ledger
    /// is a TARGET, not a delta, so a retry can never double the position: it re-sends only the
    /// residual still owed.
    pub hedge_timeout_ms: i64,
    /// How many times one residual may be re-fired before the maker halts with
    /// `HaltReason::HedgeUnfilled`. Exhaustion means the taker venue is not accepting the hedge, so
    /// continuing to quote would keep growing an exposure that cannot be closed. `0` = never retry.
    pub hedge_max_attempts: u32,
    /// SOFT naked band, in base units: while `|unhedged| > naked_band` the side that would GROW the
    /// exposure is suppressed (the reducing side keeps quoting). `<= 0` suppresses on any exposure.
    pub naked_band: f64,
    /// HARD naked band, in base units: `|unhedged| > naked_hard_band` halts the maker, pulls both
    /// sides and fires a `taker_flatten` impulse for the excess over `naked_band`. Must exceed
    /// `naked_band` to be meaningful.
    pub naked_hard_band: f64,
    /// How long after a halt the maker may auto-resume, in milliseconds. `0` (the default) =
    /// NEVER auto-resume: an operator re-tunes or restarts. That is the correct default for a
    /// strategy whose halts all mean "something I cannot see is wrong".
    pub resume_after_halt_ms: i64,

    // --- one-sided-fill breaker (the SYMPTOM guard) --------------------------------------------
    /// Sliding EVENT-TIME window for the maker-leg fill netting, in milliseconds. `<= 0` disables
    /// the breaker.
    pub fill_window_ms: i64,
    /// Net one-directional maker fill size within the window that suppresses that side.
    pub net_fill_threshold: f64,
    /// How long a tripped side stays suppressed, in milliseconds.
    pub suppress_cooldown_ms: i64,

    // --- wire economy -------------------------------------------------------------------------
    /// Order-refresh tolerance (anti-churn). `None` ⇒ every tick re-prices both sides. Consulted
    /// ONLY on the re-price path — a place or a pull is never tolerance-gated.
    pub refresh_tolerance: Option<crate::RefreshTolerance>,
}

impl Default for XemmParams {
    /// SAFETY-ON defaults — see the module doc for why this inverts the crate's usual
    /// neutral-default rule. `qty` is `0.0` (a maker with no configured size quotes nothing, which
    /// is the safe unconfigured state), the two age bounds are 2 s, the emission-gap watchdog 5 s,
    /// the hedge times out at 3 s with 3 attempts, the naked bands are expressed in `qty` by the
    /// builder rather than guessed here (a bare `Default` uses `0.0`/`0.0`, i.e. suppress on ANY
    /// exposure and halt on ANY exposure — maximally conservative), and resume is MANUAL.
    fn default() -> Self {
        XemmParams {
            qty: 0.0,
            hedge_ratio: 1.0,
            hedge_dust: 0.0,
            min_profitability: 0.0,
            total_fee: 0.0,
            min_edge_ticks: 1.0,
            maker_tick_size: 0.0,
            max_basis_bps: 0.0,
            basis_halflife_ms: 60_000,
            basis_clamp: 0.05,
            target_inventory: 0.0,
            max_inventory: 0.0,
            skew: 0.0,
            max_ref_age_ms: 2_000,
            max_own_touch_age_ms: 2_000,
            max_emission_gap_ms: 5_000,
            hedge_timeout_ms: 3_000,
            hedge_max_attempts: 3,
            naked_band: 0.0,
            naked_hard_band: 0.0,
            resume_after_halt_ms: 0,
            fill_window_ms: 0,
            net_fill_threshold: 0.0,
            suppress_cooldown_ms: 0,
            refresh_tolerance: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the inverted polarity: an operator who forgets a guard gets a BOUNDED
    /// one, never an unbounded one. This test is the pin that a future "make the defaults neutral
    /// like everything else" refactor has to argue with.
    #[test]
    fn the_defaults_ship_every_guard_armed() {
        let d = XemmParams::default();
        assert!(d.max_ref_age_ms > 0, "a dead reference must time out by default");
        assert!(d.max_own_touch_age_ms > 0, "a dead own touch must time out by default");
        assert!(d.max_emission_gap_ms > 0, "an all-lanes-quiet gap must time out by default");
        assert!(d.hedge_timeout_ms > 0, "an unfilled hedge must be retried by default");
        assert!(d.hedge_max_attempts > 0, "retries must be bounded AND nonzero by default");
        assert_eq!(d.resume_after_halt_ms, 0, "auto-resume is OFF by default (manual only)");
        assert_eq!(d.hedge_ratio.to_bits(), 1.0_f64.to_bits(), "fully hedged by default");
        assert!(d.min_edge_ticks >= 1.0, "a sub-tick standoff is not a standoff");
    }

    /// A bare `Default` quotes NOTHING (`qty == 0.0`) — the safe unconfigured state, so a
    /// half-built config cannot rest live orders.
    #[test]
    fn a_bare_default_quotes_nothing() {
        assert_eq!(XemmParams::default().qty.to_bits(), 0.0_f64.to_bits());
    }
}
