//! [`XemmMaker`] — the CROSS-EXCHANGE market maker: rest passively on venue A at prices derived
//! from venue B's touch, hedge every fill on B. Ports Hummingbot's cross-exchange market making
//! strategy (Apache-2.0, `hummingbot/strategy/cross_exchange_market_making`); the pricing
//! arithmetic is in [`pricing`], the risk half ([`guards`], [`hedge`], [`basis`]) is vike's own.
//!
//! # What it earns, and what it deliberately does NOT assume
//!
//! xEMM earns the MAKER SPREAD. It rests passively on A at `reference ± (edge + fee)` and offloads
//! any fill on B, so it profits from **being filled**, not from a pre-existing price gap. That
//! distinction is load-bearing: the cross-venue price DISLOCATION this strategy is often
//! mis-explained by does not survive a four-crossing cost floor on the pairs measured here, and a
//! design justified by dislocation would be justified by nothing.
//!
//! # The one genuinely hard problem: a PERSISTENT basis
//!
//! Two venues quoting the "same" instrument sit at systematically different prices (hyperliquid
//! trades below the CEXes on some instruments in >99% of minutes). A maker that treats B's touch as
//! fair value for A therefore quotes MARKETABLE on one side continuously — filled instantly and
//! forever on the side the basis favours, never filled on the other — while paying an unbudgeted
//! taker fee, since **no roster venue supports post-only** (`vike_model::venue_caps`, pinned). Worse,
//! paper and backtest book that as a MAKER fill (`is_maker = kind == Limit` at both fill sites), so
//! the rehearsal looks profitable.
//!
//! The fix is placement, not a price term: [`pricing::passive_clamp`] anchors each side inside
//! venue A's OWN touch, which the mount already receives. It needs no estimate and preserves the
//! hedge identity by construction. Read its doc for why the obvious alternative — rebasing the
//! reference touch by an estimated basis — is a trap that makes every fill *look* hedged while
//! guaranteeing a loss. [`basis::BasisEwma`] survives only as a DIAGNOSTIC and a halt band.
//!
//! # Four independent safety layers, because none alone suffices
//!
//! 1. **[`pricing::passive_clamp`]** — structural, needs no estimate, cannot be mis-tuned;
//! 2. **the basis halt band** — catches a decoupled pair / wrong-instrument mount / lying feed;
//! 3. **the one-sided-fill breaker** (`crate::skew::net_signed_fills`) — fires on the SYMPTOM
//!    regardless of whether any model is right, so a wrong assumption costs bounded one-sided flow;
//! 4. **the naked bands** — soft suppression of the growing side, then a hard halt plus a
//!    `crate::taker_flatten` impulse on the excess.
//!
//! # Why this maker never reads the `Broker` seam for inventory
//!
//! [`hedge::HedgeLedger`]'s doc has the full argument, and ⚠ it SHRANK: the two RUNTIME defects it
//! used to lead with (`declared_views` resolving a foreign leg against the mount's engine, and the
//! EMPTY per-symbol tables at the fill dispatch a hedge fires from) are fixed. What is left is
//! intrinsic — `HftBroker::position` takes no symbol, and no broker read can see a sent-but-unacked
//! hedge, which is the quantity a safe retry is computed from. Folding the fill stream stays the
//! more truthful source and still needs zero plumbing. `XemmMaker` therefore calls NO
//! `Broker::position` / `price` / `equity` / `HftBroker::position` anywhere, and a poisoned-broker
//! test pins it.
//!
//! # Which trait, and why
//!
//! `impl<B: HftBroker> Strategy<B>`, forced rather than chosen. The maker legs need
//! `submit_limit_tagged`/`modify_tagged`/`cancel_tagged` (queue-preserving, absent from `Broker`);
//! the hedge needs `Broker::submit_market(symbol, …)`, the only VENUE-ROUTABLE verb — a tagged
//! submit carries `symbol: None` by contract, so the runtime's `resolve_intent_venue` (which finds
//! a leg BY SYMBOL) can never route one to the taker venue. `HftBroker: Broker`, so one bound gives
//! both. Not `MultiHftBroker`: it has zero implementors, and its only plausible backing is the
//! wrong-engine read above.

pub(crate) mod basis;
pub(crate) mod emit;
pub(crate) mod guards;
pub(crate) mod hedge;
pub(crate) mod pricing;
mod strategy_impl;

use std::collections::VecDeque;

use toml::Value;
use vike_model::{HftBroker, RefreshTolerance, XemmParams};

use crate::skew::{net_signed_fills, skew_multipliers, FillRec};
use crate::taker_flatten::taker_flatten;
use crate::SideState;

use basis::BasisEwma;
use guards::Halt;
pub use guards::HaltReason;
use hedge::HedgeLedger;
use pricing::Touch;

/// The cross-exchange maker. See the module doc for the design; per-knob semantics live on
/// [`XemmParams`]'s field docs.
///
/// CONFIG-vs-STATE split, exactly as `SpreadMaker` has it, and for the same reason: `cfg` is the
/// whole live-tunable bag and is replaced wholesale by [`XemmMaker::apply_params`], while every
/// runtime field is a SIBLING the swap never touches — so a hot re-tune preserves resting orders,
/// venue queue position, the hedge ledger, the breaker deadlines, the warm basis estimate and the
/// halt latch **by type shape**, not by a hand-maintained list of omissions.
///
/// The two SYMBOLS are IDENTITY, not config, and live outside `cfg` for that reason: a re-tune must
/// never be able to repoint a leg while an unhedged position is open.
pub struct XemmMaker {
    /// Every live-tunable knob, one `Copy` vike-model struct — the shape the live-params plane
    /// transports as [`vike_model::StrategyParams::Xemm`].
    cfg: XemmParams,
    /// The symbol this maker RESTS on (the mount's own). IDENTITY — see the type doc.
    maker_symbol: String,
    /// The symbol this maker HEDGES on (a declared `MountLeg::at(sym, taker_venue)`). Must differ
    /// from `maker_symbol`: it is the only discriminator `on_fill` has (a `Fill` carries a symbol
    /// but no venue), AND the runtime's `resolve_intent_venue` finds a leg BY SYMBOL ALONE, so a
    /// leg carrying the mount's own symbol would route EVERY intent — including symbol-less tagged
    /// maker quotes — to the taker venue.
    hedge_symbol: String,
    /// The REFERENCE venue's last touch — the fair-value input. `None` until the first
    /// `on_reference_quote`, and a `None` here means NO QUOTES (never a fallback to the local book:
    /// quoting off the venue you are resting on is not a cross-exchange maker, it is an unpriced
    /// one).
    b_touch: Option<Touch>,
    /// The MAKER venue's OWN last touch — [`pricing::passive_clamp`]'s anchor. Also `None` ⇒ no
    /// quotes: without it, "non-marketable" is unprovable.
    a_touch: Option<Touch>,
    /// The BID side's runtime quote state — reused verbatim from `SpreadMaker` (`crate::SideState`,
    /// private to the crate ROOT and therefore visible to this descendant module).
    bid: SideState,
    /// The ASK side's [`SideState`](crate::SideState) — the exact mirror.
    ask: SideState,
    /// The ONLY inventory authority. See [`hedge::HedgeLedger`]'s doc for why the broker seam is
    /// not used.
    hedge: HedgeLedger,
    /// Recent MAKER-leg fills inside the breaker window — the one-sided-flow netting accumulator
    /// (`crate::skew::net_signed_fills`). Hedge-leg fills are deliberately excluded: they are this
    /// strategy's own mechanical offset, not market flow, and netting them would cancel every real
    /// signal to exactly zero.
    fills: VecDeque<FillRec>,
    /// The observed maker-vs-reference basis — DIAGNOSTIC + halt band ONLY, never a price term.
    basis: BasisEwma,
    /// The run/halt latch. Persisted across a restart, so a restart cannot un-halt a halted maker.
    halt: Halt,
    /// EVENT ts of the last touch from EITHER venue (`0` = none yet) — the all-lanes-quiet
    /// watchdog's clock. Distinct from the two per-venue ages: those catch ONE dead feed each,
    /// this catches the case where nothing at all is arriving and no per-venue lane can fire.
    last_tick_ts: i64,
    /// Fire-once latch for the hard-band `taker_flatten` impulse, cleared on resume — so a halted
    /// maker being ticked does not send a flatten market every tick.
    flatten_fired: bool,
    /// The venue price grid LEARNED from the maker venue's L2 book lane, adopted on its L1 quote
    /// lane so the standoff and the directional snap resolve against the SAME tick on both. `None`
    /// until a book with a positive `tick_size` arrives; a pure-L1 feed falls back to the
    /// configured [`XemmParams::maker_tick_size`]. Mirrors `SpreadMaker::learned_book_tick`.
    learned_maker_tick: Option<f64>,
    /// Live-params version: bumped once per applied [`XemmMaker::apply_params`]. Observable so a
    /// caller/test can confirm a re-tune landed.
    pub params_epoch: u64,
}

impl XemmMaker {
    /// A cross-exchange maker resting `qty` per side on `maker_symbol`, priced off the reference
    /// venue's touch backed off by `min_profitability + total_fee`, hedging on `hedge_symbol`.
    ///
    /// `total_fee` must come from [`vike_model::xemm_round_trip_fee`] — never a hand-entered
    /// number, and never `0.0` because a venue's fee shape has no flat rate (that function refuses
    /// instead, precisely so the refusal cannot be defaulted away).
    ///
    /// The naked bands default to `qty` (soft) and `3·qty` (hard): a maker that has accumulated one
    /// full quote size of unhedged exposure stops growing it, and three sizes is a fault. An
    /// operator tunes both with [`XemmMaker::with_naked_bands`]. Every other guard takes its armed
    /// [`XemmParams::default`] value — see that type's module doc for why the defaults are
    /// safety-ON rather than neutral.
    ///
    /// # Panics
    ///
    /// If `hedge_symbol == maker_symbol`. This is not a preference: the runtime's
    /// `resolve_intent_venue` finds a declared leg BY SYMBOL ALONE, so a leg carrying the mount's
    /// own symbol routes EVERY intent — including the symbol-less tagged maker quotes — to the
    /// taker venue. The maker's own book would silently end up on the venue it meant to hedge on.
    /// It also destroys `on_fill`'s only leg discriminator. Fail at construction, loudly.
    pub fn new(
        maker_symbol: impl Into<String>,
        hedge_symbol: impl Into<String>,
        qty: f64,
        min_profitability: f64,
        total_fee: f64,
        maker_tick_size: f64,
    ) -> Self {
        let maker_symbol = maker_symbol.into();
        let hedge_symbol = hedge_symbol.into();
        assert!(
            maker_symbol != hedge_symbol,
            "xEMM legs must be DISTINCT symbols (got {maker_symbol:?} twice): the runtime resolves \
             a declared leg's venue by symbol alone, so a same-symbol leg would route the maker's \
             own quotes to the taker venue, and `on_fill` would have no way to tell the legs apart"
        );
        XemmMaker {
            cfg: XemmParams {
                qty,
                min_profitability,
                total_fee,
                maker_tick_size,
                naked_band: qty,
                naked_hard_band: 3.0 * qty,
                ..XemmParams::default()
            },
            maker_symbol,
            hedge_symbol,
            b_touch: None,
            a_touch: None,
            bid: SideState::default(),
            ask: SideState::default(),
            hedge: HedgeLedger::default(),
            fills: VecDeque::new(),
            basis: BasisEwma::default(),
            halt: Halt::Running,
            last_tick_ts: 0,
            flatten_fired: false,
            learned_maker_tick: None,
            params_epoch: 0,
        }
    }

    /// Arm the BASIS halt band: `max_basis_bps` of observed `|mid_A/mid_B − 1|` halts the maker;
    /// `halflife_ms` is the estimator's EVENT-time half-life and `clamp` bounds ONE observation.
    /// `max_basis_bps <= 0` (the [`XemmParams::default`]) leaves the band off — the estimator still
    /// warms and publishes, it just never halts.
    pub fn with_basis_band(mut self, max_basis_bps: f64, halflife_ms: i64, clamp: f64) -> Self {
        self.cfg.max_basis_bps = max_basis_bps;
        self.cfg.basis_halflife_ms = halflife_ms;
        self.cfg.basis_clamp = clamp;
        self
    }

    /// Inventory-skew SIZE shaping on the maker leg, off the OWNED ledger's `maker_pos`. Neutral
    /// (`skew <= 0` or `max_inventory <= 0`) yields multipliers of exactly `1.0`.
    pub fn with_skew(mut self, target_inventory: f64, max_inventory: f64, skew: f64) -> Self {
        self.cfg.target_inventory = target_inventory;
        self.cfg.max_inventory = max_inventory;
        self.cfg.skew = skew;
        self
    }

    /// The three FRESHNESS bounds, in EVENT milliseconds: reference-touch age, own-touch age, and
    /// the all-lanes-quiet emission gap. Any `<= 0` disables that bound — which for the first two
    /// means "quote off a dead feed forever", so it is never the default.
    pub fn with_freshness(
        mut self,
        max_ref_age_ms: i64,
        max_own_touch_age_ms: i64,
        max_emission_gap_ms: i64,
    ) -> Self {
        self.cfg.max_ref_age_ms = max_ref_age_ms;
        self.cfg.max_own_touch_age_ms = max_own_touch_age_ms;
        self.cfg.max_emission_gap_ms = max_emission_gap_ms;
        self
    }

    /// Hedge discipline: how long a sent hedge may stay unfilled before the residual is re-sent,
    /// how many SENDS one residual gets in total (`0` = never hedge at all), and the size below
    /// which a residual is forgiven as dust (set it to the taker venue's `min_qty`, or a rejected
    /// sub-minimum order is retried until the budget halts the maker).
    pub fn with_hedge_discipline(
        mut self,
        hedge_timeout_ms: i64,
        hedge_max_attempts: u32,
        hedge_dust: f64,
    ) -> Self {
        self.cfg.hedge_timeout_ms = hedge_timeout_ms;
        self.cfg.hedge_max_attempts = hedge_max_attempts;
        self.cfg.hedge_dust = hedge_dust;
        self
    }

    /// The two NAKED-inventory bands, in base units: `soft` suppresses the side that would GROW the
    /// exposure, `hard` halts + pulls + fires a `taker_flatten` impulse for the excess over `soft`.
    pub fn with_naked_bands(mut self, soft: f64, hard: f64) -> Self {
        self.cfg.naked_band = soft;
        self.cfg.naked_hard_band = hard;
        self
    }

    /// The per-side ONE-SIDED-FILL breaker over MAKER-leg fills — the symptom guard that fires
    /// regardless of whether the pricing model is right. All-zero (the default) = off.
    pub fn with_fill_breaker(
        mut self,
        fill_window_ms: i64,
        net_fill_threshold: f64,
        suppress_cooldown_ms: i64,
    ) -> Self {
        self.cfg.fill_window_ms = fill_window_ms;
        self.cfg.net_fill_threshold = net_fill_threshold;
        self.cfg.suppress_cooldown_ms = suppress_cooldown_ms;
        self
    }

    /// Anti-churn refresh tolerance (basis points of the RESTING value, both axes). Consulted ONLY
    /// on the re-price path — a place or a pull is never gated. `None`/all-zero = off.
    pub fn with_refresh_tolerance(mut self, price_bps: f64, size_bps: f64) -> Self {
        self.cfg.refresh_tolerance = Some(RefreshTolerance { price_bps, size_bps });
        self
    }

    /// How long after a halt the maker may AUTO-resume, in EVENT milliseconds. `0` (the default)
    /// means never: every halt reason names something an operator should look at.
    pub fn with_resume_after(mut self, resume_after_halt_ms: i64) -> Self {
        self.cfg.resume_after_halt_ms = resume_after_halt_ms;
        self
    }

    /// Read the maker's whole current tuning — the inverse of [`XemmMaker::apply_params`].
    pub fn params(&self) -> XemmParams {
        self.cfg
    }

    /// The maker's symbol pair as `(maker, hedge)` — mount IDENTITY, never re-tunable.
    pub fn legs(&self) -> (&str, &str) {
        (&self.maker_symbol, &self.hedge_symbol)
    }

    /// The current halt reason, or `None` while running. The operator-facing observable.
    pub fn halt_reason(&self) -> Option<HaltReason> {
        match self.halt {
            Halt::Running => None,
            Halt::Halted { reason, .. } => Some(reason),
        }
    }

    /// The observed maker-vs-reference basis as a FRACTION, or `None` while cold. PUBLISHED for the
    /// operator to size `min_profitability` against basis VOLATILITY; never read by a price.
    pub fn basis(&self) -> Option<f64> {
        self.basis.value()
    }

    /// The UNHEDGED exposure the strategy is currently carrying (`maker + hedge` net), off the
    /// owned ledger. `0.0` when perfectly hedged.
    pub fn naked_exposure(&self) -> f64 {
        self.hedge.naked()
    }

    /// The two legs' signed positions as `(maker, hedge)`, off the owned ledger.
    pub fn leg_positions(&self) -> (f64, f64) {
        (self.hedge.maker_pos, self.hedge.hedge_pos)
    }

    /// Hot-swap the whole tunable bag (the live-parameter plane). ONE struct copy, which is what
    /// makes it safe BY TYPE SHAPE: every runtime field — resting orders and their queue position,
    /// the hedge ledger, the breaker deadlines, the warm basis estimate, the halt latch, the
    /// learned tick — is a sibling of `cfg` and is untouched, so nothing can be forgotten here.
    ///
    /// NO ORDER VERB FIRES: the new knobs take effect on the NEXT tick, where `requote` re-prices
    /// the resting quotes IN PLACE.
    pub fn apply_params(&mut self, p: &XemmParams) {
        self.cfg = *p;
        self.params_epoch += 1;
    }

    // --- internals ---------------------------------------------------------------------------

    /// One side's runtime state, indexed by `is_bid` (the `SpreadMaker` convention — no mirrored
    /// `bid_*`/`ask_*` field pairs).
    pub(crate) fn side(&self, is_bid: bool) -> &SideState {
        if is_bid {
            &self.bid
        } else {
            &self.ask
        }
    }

    pub(crate) fn side_mut(&mut self, is_bid: bool) -> &mut SideState {
        if is_bid {
            &mut self.bid
        } else {
            &mut self.ask
        }
    }

    /// The maker venue's price grid: the tick LEARNED from its L2 book when one has been seen, else
    /// the configured param — so the standoff and the directional snap can never resolve against
    /// two different grids on the two lanes.
    fn effective_tick(&self) -> f64 {
        self.learned_maker_tick.unwrap_or(self.cfg.maker_tick_size)
    }

    fn breaker_enabled(&self) -> bool {
        self.cfg.fill_window_ms > 0 && self.cfg.net_fill_threshold > 0.0
    }

    /// Record a touch from EITHER venue and fold the basis when BOTH are known — paired
    /// observations only (see [`basis::BasisEwma`]'s discipline).
    fn observe_touch(&mut self, is_reference: bool, touch: Touch) {
        if is_reference {
            self.b_touch = Some(touch);
        } else {
            self.a_touch = Some(touch);
        }
        self.last_tick_ts = self.last_tick_ts.max(touch.ts);
        if let (Some(a), Some(b)) = (self.a_touch, self.b_touch) {
            if a.is_sane() && b.is_sane() {
                self.basis.observe(
                    a.mid(),
                    b.mid(),
                    touch.ts,
                    self.cfg.basis_halflife_ms,
                    self.cfg.basis_clamp,
                );
            }
        }
    }

    /// Latch a halt, keeping the FIRST reason if one is already latched (the first fault is the
    /// diagnostic one; a cascade of consequences would otherwise overwrite it).
    fn enter_halt(&mut self, reason: HaltReason, now: i64) {
        if matches!(self.halt, Halt::Running) {
            tracing::warn!(
                ?reason,
                maker = %self.maker_symbol,
                hedge = %self.hedge_symbol,
                naked = self.hedge.naked(),
                "xEMM halted: pulling both quotes"
            );
            self.halt = Halt::Halted { reason, since: now };
        }
    }

    /// Clear a latched halt once `resume_after_halt_ms` of EVENT time has passed. `0` (the default)
    /// never resumes — an operator re-tunes or restarts, which is correct for faults that all mean
    /// "something I cannot see is wrong".
    fn maybe_resume(&mut self, now: i64) {
        let Halt::Halted { since, .. } = self.halt else { return };
        if self.cfg.resume_after_halt_ms > 0 && now - since >= self.cfg.resume_after_halt_ms {
            tracing::info!(maker = %self.maker_symbol, "xEMM resuming after halt");
            self.halt = Halt::Running;
            self.flatten_fired = false;
        }
    }

    /// The FAULT verdict for this tick, in the order a diagnosis should read: freshness first (the
    /// most common and most recoverable), then the exposure faults, then the model fault.
    ///
    /// A COLD START — either touch simply not yet seen — is deliberately NOT a fault: it is not
    /// something an operator should be paged about, and `requote` pulls on it anyway.
    fn detect_fault(&self, now: i64) -> Option<HaltReason> {
        if self.b_touch.is_none() || self.a_touch.is_none() {
            return None;
        }
        if guards::stale(self.b_touch.map(|t| t.ts), now, self.cfg.max_ref_age_ms) {
            return Some(HaltReason::ReferenceStale);
        }
        if guards::stale(self.a_touch.map(|t| t.ts), now, self.cfg.max_own_touch_age_ms) {
            return Some(HaltReason::OwnTouchStale);
        }
        if guards::stale(
            (self.last_tick_ts > 0).then_some(self.last_tick_ts),
            now,
            self.cfg.max_emission_gap_ms,
        ) {
            return Some(HaltReason::FeedImpaired);
        }
        if self.hedge.attempts_exhausted(
            now,
            self.cfg.hedge_ratio,
            self.cfg.hedge_dust,
            self.cfg.hedge_timeout_ms,
            self.cfg.hedge_max_attempts,
        ) {
            return Some(HaltReason::HedgeUnfilled);
        }
        if guards::over_hard_band(self.hedge.naked(), self.cfg.naked_hard_band) {
            return Some(HaltReason::NakedHardBand);
        }
        if self.basis.out_of_band(self.cfg.max_basis_bps) {
            return Some(HaltReason::BasisOutOfBand);
        }
        None
    }

    /// THE ONE QUOTE FUNNEL — every lane whose drain series is the MOUNT'S OWN calls this.
    ///
    /// Stage order is semantic; each stage consumes the previous stage's `(price, size)`:
    /// (0) resume/fault/halt — a halted or not-yet-ready maker PULLS BOTH SIDES and stops;
    /// (1) the pure hedge identity, [`pricing::xemm_maker_quotes`], untouched;
    /// (2) [`pricing::passive_clamp`] against venue A's OWN touch — strictly conservative on both
    ///     axes at once, so the edge survives AND the quote cannot be marketable;
    /// (3) DIRECTIONAL grid snap (bid down / ask up), because half-to-even could round back through
    ///     the clamp;
    /// (4) inventory skew on the SIZES, off the OWNED ledger — never the broker;
    /// (5) per-side suppression: breaker ∪ soft naked band ∪ non-positive size;
    /// (6) emit, bid first then ask (the `SpreadMaker` ordering).
    pub(crate) fn requote<B: HftBroker>(&mut self, broker: &mut B, now: i64) {
        // (0)
        self.maybe_resume(now);
        if let Some(reason) = self.detect_fault(now) {
            self.enter_halt(reason, now);
        }
        let (Some(a), Some(b)) = (self.a_touch, self.b_touch) else {
            // Cold start: no reference (or no own book) means NO QUOTES — never a fallback to
            // pricing off the venue we rest on.
            self.pull_all(broker);
            return;
        };
        if !matches!(self.halt, Halt::Running) {
            self.pull_all(broker);
            return;
        }
        // (1) THE HEDGE IDENTITY. Nothing below may move a quote toward the reference touch.
        let (raw_bid, raw_ask) = pricing::xemm_maker_quotes(
            b.bid,
            b.ask,
            self.cfg.min_profitability,
            self.cfg.total_fee,
        );
        // (2) the passive clamp — `min_edge_ticks` floored at 1.0, because a sub-tick standoff
        // rounds onto the touch and stops being a standoff at all.
        let tick = self.effective_tick();
        let standoff = tick * self.cfg.min_edge_ticks.max(1.0);
        let Some((bid_px, ask_px)) = pricing::passive_clamp(raw_bid, raw_ask, Some(a), standoff)
        else {
            // An unprovable quote is not emitted — the clamp refuses rather than degrading.
            self.pull_all(broker);
            return;
        };
        // (3) DIRECTIONAL snap onto the maker venue's grid.
        let bid_px = pricing::snap_down(bid_px, tick);
        let ask_px = pricing::snap_up(ask_px, tick);
        if ask_px <= bid_px {
            self.pull_all(broker);
            return;
        }
        // (4) sizes, skewed off the OWNED maker-leg inventory.
        let (bid_mult, ask_mult) = skew_multipliers(
            self.hedge.maker_pos,
            self.cfg.target_inventory,
            self.cfg.max_inventory,
            self.cfg.skew,
        );
        let bid_qty = self.cfg.qty * bid_mult;
        let ask_qty = self.cfg.qty * ask_mult;
        // (5) suppression.
        let (naked_bid, naked_ask) =
            guards::soft_band_suppression(self.hedge.naked(), self.cfg.naked_band);
        let breaker_bid = self.bid.suppressed_until > 0 && now < self.bid.suppressed_until;
        let breaker_ask = self.ask.suppressed_until > 0 && now < self.ask.suppressed_until;
        let bid_sup = naked_bid || breaker_bid || !(bid_qty.is_finite() && bid_qty > 0.0);
        let ask_sup = naked_ask || breaker_ask || !(ask_qty.is_finite() && ask_qty > 0.0);
        // (6)
        self.emit_side(broker, true, (bid_px, bid_qty), bid_sup, now);
        self.emit_side(broker, false, (ask_px, ask_qty), ask_sup, now);
    }

    /// Send whatever the hedge ledger still OWES on the taker venue — the target-residual
    /// discipline in one place.
    ///
    /// ⚠ `Broker::submit_market` (SYMBOL-CARRYING, tag-less) is the only verb used here, and that
    /// is a hard contract, not a style choice: it is the sole venue-routable verb (the runtime's
    /// `resolve_intent_venue` finds a declared leg BY SYMBOL, and a tagged submit carries
    /// `symbol: None`). A tagged verb here would place the hedge on the MAKER venue, doubling the
    /// exposure it was sent to close.
    ///
    /// Nothing is sent while a hedge is in flight and not yet timed out, or once the residual's
    /// attempt budget is spent (`detect_fault` then raises `HedgeUnfilled`). A retry re-sends the
    /// RESIDUAL, which a late original fill has already shrunk — so it can never double the
    /// position.
    pub(crate) fn drive_hedge<B: HftBroker>(&mut self, broker: &mut B, now: i64) {
        let Some((side, qty)) = self.hedge.residual(self.cfg.hedge_ratio, self.cfg.hedge_dust)
        else {
            self.hedge.settle();
            return;
        };
        if self.hedge.in_flight.is_some() && !self.hedge.timed_out(now, self.cfg.hedge_timeout_ms) {
            return;
        }
        if self.hedge.attempts >= self.cfg.hedge_max_attempts {
            return;
        }
        vike_model::Broker::submit_market(broker, &self.hedge_symbol, side, qty);
        self.hedge.fire(now);
    }

    /// The HARD-BAND impulse leg: cross the taker venue to cut the exposure back to the SOFT band
    /// (`crate::taker_flatten`'s flatten-the-excess law).
    ///
    /// Fires at most ONCE per halt (`flatten_fired`, cleared on resume) and ONLY while the hedge
    /// ledger owes nothing — an in-flight hedge is ALREADY closing the exposure, so a second market
    /// on top of it would over-hedge. In the fully-hedged configuration (`hedge_ratio == 1.0`) the
    /// residual and the naked exposure are the same quantity, so this leg is unreachable by
    /// construction; it exists for the deliberately-partial (`hedge_ratio < 1.0`) case and for an
    /// exposure that arrived some way the ledger's own retry cannot close.
    pub(crate) fn drive_flatten<B: HftBroker>(&mut self, broker: &mut B) {
        if self.flatten_fired
            || self.hedge.residual(self.cfg.hedge_ratio, self.cfg.hedge_dust).is_some()
        {
            return;
        }
        if !guards::over_hard_band(self.hedge.naked(), self.cfg.naked_hard_band) {
            return;
        }
        let Some((side_sign, qty)) = taker_flatten(self.hedge.naked(), self.cfg.naked_band) else {
            return;
        };
        vike_model::Broker::submit_market(
            broker,
            &self.hedge_symbol,
            if side_sign > 0.0 { 1 } else { -1 },
            qty,
        );
        self.flatten_fired = true;
    }

    /// Fold ONE maker-leg fill into the breaker window and arm the tripped side's cooldown.
    /// Hedge-leg fills never reach this: they are the strategy's own mechanical offset, and netting
    /// them would cancel every real one-sided signal to zero.
    fn record_maker_fill(&mut self, side: i32, size: f64, ts: i64) {
        if !self.breaker_enabled() {
            return;
        }
        self.fills.push_back(FillRec { side, size, ts });
        let cutoff = ts - self.cfg.fill_window_ms;
        while self.fills.front().is_some_and(|f| f.ts < cutoff) {
            self.fills.pop_front();
        }
        let net = net_signed_fills(&self.fills, ts, self.cfg.fill_window_ms);
        if net >= self.cfg.net_fill_threshold {
            self.bid.suppressed_until = ts + self.cfg.suppress_cooldown_ms;
        } else if net <= -self.cfg.net_fill_threshold {
            self.ask.suppressed_until = ts + self.cfg.suppress_cooldown_ms;
        }
    }

    /// A lenient TOML `f64` reader (float OR integer), matching `SpreadMaker::from_params`'
    /// convention so `qty = 1` and `qty = 1.0` both work.
    fn as_f64(v: &Value) -> Option<f64> {
        v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
    }

    /// Build from a TOML `[strategy.params]` table — a READER, not a schema: unknown keys are
    /// ignored and missing keys keep the (safety-ON) default, exactly as
    /// `SpreadMaker::from_params` behaves.
    ///
    /// | key | meaning |
    /// |---|---|
    /// | `maker_symbol` / `hedge_symbol` | the two legs (REQUIRED — see Panics) |
    /// | `qty` | base quote size per side |
    /// | `min_profitability` | required edge, fraction of price |
    /// | `total_fee` | round-trip fee, fraction of price (`vike_model::xemm_round_trip_fee`) |
    /// | `min_edge_ticks` / `maker_tick_size` | the standoff and the maker venue's grid |
    /// | `hedge_ratio` / `hedge_dust` / `hedge_timeout_ms` / `hedge_max_attempts` | hedge discipline |
    /// | `max_basis_bps` / `basis_halflife_ms` / `basis_clamp` | the basis band |
    /// | `target_inventory` / `max_inventory` / `skew` | size skew |
    /// | `max_ref_age_ms` / `max_own_touch_age_ms` / `max_emission_gap_ms` | freshness |
    /// | `naked_band` / `naked_hard_band` / `resume_after_halt_ms` | exposure + resume |
    /// | `fill_window_ms` / `net_fill_threshold` / `suppress_cooldown_ms` | the breaker |
    /// | `refresh_price_bps` / `refresh_size_bps` | anti-churn tolerance |
    ///
    /// # Panics
    ///
    /// If `maker_symbol`/`hedge_symbol` are absent or equal. Unlike every tunable above, the legs
    /// have no defensible default: an xEMM with one leg is not a degraded xEMM, it is a different
    /// (and unhedged) strategy.
    pub fn from_params(params: &Value) -> Self {
        let maker = params
            .get("maker_symbol")
            .and_then(Value::as_str)
            .expect("xemm: `maker_symbol` is required (an xEMM leg has no defensible default)");
        let hedge = params
            .get("hedge_symbol")
            .and_then(Value::as_str)
            .expect("xemm: `hedge_symbol` is required (an xEMM leg has no defensible default)");
        let get = |k: &str| params.get(k).and_then(Self::as_f64);
        let get_i = |k: &str| params.get(k).and_then(Value::as_integer);
        let d = XemmParams::default();
        let mut m = XemmMaker::new(
            maker,
            hedge,
            get("qty").unwrap_or(d.qty),
            get("min_profitability").unwrap_or(d.min_profitability),
            get("total_fee").unwrap_or(d.total_fee),
            get("maker_tick_size").unwrap_or(d.maker_tick_size),
        );
        // `new` derived the bands from qty; re-read them only if the profile names them.
        let derived = m.cfg;
        m.cfg = XemmParams {
            min_edge_ticks: get("min_edge_ticks").unwrap_or(d.min_edge_ticks),
            hedge_ratio: get("hedge_ratio").unwrap_or(d.hedge_ratio),
            hedge_dust: get("hedge_dust").unwrap_or(d.hedge_dust),
            hedge_timeout_ms: get_i("hedge_timeout_ms").unwrap_or(d.hedge_timeout_ms),
            hedge_max_attempts: get_i("hedge_max_attempts")
                .map(|v| v.max(0) as u32)
                .unwrap_or(d.hedge_max_attempts),
            max_basis_bps: get("max_basis_bps").unwrap_or(d.max_basis_bps),
            basis_halflife_ms: get_i("basis_halflife_ms").unwrap_or(d.basis_halflife_ms),
            basis_clamp: get("basis_clamp").unwrap_or(d.basis_clamp),
            target_inventory: get("target_inventory").unwrap_or(d.target_inventory),
            max_inventory: get("max_inventory").unwrap_or(d.max_inventory),
            skew: get("skew").unwrap_or(d.skew),
            max_ref_age_ms: get_i("max_ref_age_ms").unwrap_or(d.max_ref_age_ms),
            max_own_touch_age_ms: get_i("max_own_touch_age_ms").unwrap_or(d.max_own_touch_age_ms),
            max_emission_gap_ms: get_i("max_emission_gap_ms").unwrap_or(d.max_emission_gap_ms),
            naked_band: get("naked_band").unwrap_or(derived.naked_band),
            naked_hard_band: get("naked_hard_band").unwrap_or(derived.naked_hard_band),
            resume_after_halt_ms: get_i("resume_after_halt_ms").unwrap_or(d.resume_after_halt_ms),
            fill_window_ms: get_i("fill_window_ms").unwrap_or(d.fill_window_ms),
            net_fill_threshold: get("net_fill_threshold").unwrap_or(d.net_fill_threshold),
            suppress_cooldown_ms: get_i("suppress_cooldown_ms").unwrap_or(d.suppress_cooldown_ms),
            refresh_tolerance: match (get("refresh_price_bps"), get("refresh_size_bps")) {
                (None, None) => d.refresh_tolerance,
                (p, s) => Some(RefreshTolerance {
                    price_bps: p.unwrap_or(0.0),
                    size_bps: s.unwrap_or(0.0),
                }),
            },
            ..derived
        };
        m
    }
}
