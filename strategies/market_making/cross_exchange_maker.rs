//! The cross-exchange maker: rests a bid and an ask on its mount's own venue, priced off a REFERENCE
//! venue's touch, and hedges every fill there. The maker itself (its struct, its `Strategy` impl,
//! its quote emission, its saved state and its white-box tests) lives here, moved out of `vike-market-making`;
//! the xEMM maths stays in `vike_market_making::xemm`. Paper only
//! (`docs/decisions/0033-xemm-is-built-but-mounted-by-nothing.md`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! routes = "multi_leg"
//! multi_leg_why = """\
//!   TWO-LEG: `maker_symbol` names the leg it quotes on and `hedge_symbol` the leg it hedges on, and \
//!   the two must differ (`XemmMaker::new` refuses equal legs), so at most ONE of them could ever \
//!   equal the mount's own symbol and requiring both to would be incoherent. A hedge on another \
//!   venue's instrument of the SAME user name is named by its account, `BTCUSDT.P@bybit/LABEL` \
//!   (decision 0293): the mount binds it, this file names no account"""
//!
//! # Every key `XemmMaker::from_params` reads.
//! [params]
//! maker_symbol = { type = "text", route = "symbol" }
//! hedge_symbol = { type = "text", route = "symbol" }
//! qty = { type = "number" }
//! min_profitability = { type = "number" }
//! total_fee = { type = "number" }
//! maker_tick_size = { type = "number" }
//! min_edge_ticks = { type = "number" }
//! hedge_ratio = { type = "number" }
//! hedge_dust = { type = "number" }
//! hedge_timeout_ms = { type = "integer" }
//! hedge_max_attempts = { type = "integer" }
//! max_basis_bps = { type = "number" }
//! basis_halflife_ms = { type = "integer" }
//! basis_clamp = { type = "number" }
//! target_inventory = { type = "number" }
//! max_inventory = { type = "number" }
//! skew = { type = "number" }
//! max_ref_age_ms = { type = "integer" }
//! max_own_touch_age_ms = { type = "integer" }
//! max_emission_gap_ms = { type = "integer" }
//! naked_band = { type = "number" }
//! naked_hard_band = { type = "number" }
//! resume_after_halt_ms = { type = "integer" }
//! fill_window_ms = { type = "integer" }
//! net_fill_threshold = { type = "number" }
//! suppress_cooldown_ms = { type = "integer" }
//! refresh_price_bps = { type = "number" }
//! refresh_size_bps = { type = "number" }
//! ```
//!
//! [`XemmMaker`], the CROSS-EXCHANGE market maker: rest passively on venue A at prices derived
//! from venue B's touch, hedge every fill on B. Ports Hummingbot's cross-exchange market making
//! strategy (Apache-2.0, `hummingbot/strategy/cross_exchange_market_making`); the pricing
//! arithmetic is in [`pricing`], the risk half ([`guards`], [`hedge`](vike_market_making::xemm::hedge), [`basis`](vike_market_making::xemm::basis)) is vike's own.
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
//! taker fee, since **no roster venue supports post-only** (`vike_venues::venue_caps`, pinned). Worse,
//! paper and backtest book that as a MAKER fill (`is_maker = kind == Limit` at both fill sites), so
//! the rehearsal looks profitable.
//!
//! The fix is placement, not a price term: [`pricing::passive_clamp`] anchors each side inside
//! venue A's OWN touch, which the mount already receives. It needs no estimate and preserves the
//! hedge identity by construction. Read its doc for why the obvious alternative — rebasing the
//! reference touch by an estimated basis — is a trap that makes every fill *look* hedged while
//! guaranteeing a loss. [`BasisEwma`] survives only as a DIAGNOSTIC and a halt band.
//!
//! # Four independent safety layers, because none alone suffices
//!
//! 1. **[`pricing::passive_clamp`]** — structural, needs no estimate, cannot be mis-tuned;
//! 2. **the basis halt band** — catches a decoupled pair / wrong-instrument mount / lying feed;
//! 3. **the one-sided-fill breaker** (`vike_market_making::skew::net_signed_fills`) — fires on the SYMPTOM
//!    regardless of whether any model is right, so a wrong assumption costs bounded one-sided flow;
//! 4. **the naked bands** — soft suppression of the growing side, then a hard halt plus a
//!    `vike_market_making::taker_flatten` impulse on the excess.
//!
//! # Why this maker never reads the broker for inventory
//!
//! [`HedgeLedger`]'s doc has the full argument, and ⚠ it SHRANK: the two RUNTIME defects it
//! used to lead with (`declared_views` resolving a foreign leg against the mount's engine, and the
//! EMPTY per-symbol tables at the fill dispatch a hedge fires from) are fixed. What is left is
//! intrinsic — no broker read can see a sent-but-unacked hedge, which is the quantity a safe retry
//! is computed from. Folding the fill stream stays the more truthful source and still needs zero
//! plumbing. `XemmMaker` therefore reads NO `position` / `price` / `equity` off the broker anywhere, and
//! a poisoned-broker test pins it.
//!
//! # Which trait, and why
//!
//! `impl<B: OrderBroker> Strategy<B>` (decision 0277's step 6). The maker legs are tagged limits on
//! the mount's own symbol (`place("", tag, ..)`, `amend`, `cancel`: queue-preserving), and the hedge
//! is an UNTAGGED market on the hedge symbol (`place(hedge_symbol, "", .., Market)`), the
//! VENUE-ROUTABLE form — a quote placed on `""` resolves to the drain's series, so the runtime's
//! `resolve_intent_venue` (which finds a leg BY SYMBOL) can never route one to the taker venue.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use vike_market_making::refresh::within_tolerance;
use vike_market_making::skew::{FillRec, net_signed_fills, skew_multipliers};
use vike_market_making::taker_flatten::taker_flatten;
use vike_market_making::xemm::basis::BasisEwma;
use vike_market_making::xemm::guards::{self, Halt, HaltReason};
use vike_market_making::xemm::hedge::{HedgeLedger, InFlight, LegValues};
use vike_market_making::xemm::pricing::{self, Touch};
use vike_marketdata::{L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::params::{ParamValue, Params};
use vike_model::{
    FeedStatus, Fill, OrderBroker, OrderEvent, OrderPlacement, RefreshTolerance, Strategy,
    StrategyParams, XemmParams,
};

// ---- The entry points -----------------------------------------------------------------------

/// The strategy file's entry: the maker `params` describe ([`XemmMaker::from_params`]).
///
/// # Panics
///
/// On a table without two distinct legs, which [`refuse`] refuses before this runs.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(XemmMaker::from_params(params))
}

/// The whole-table refusal: the two legs [`XemmMaker::from_params`] cannot do without, in the words
/// it panics with.
pub fn refuse(params: &Params) -> Option<String> {
    let leg = |key: &str| params.get(key).and_then(ParamValue::as_str);
    match (leg("maker_symbol"), leg("hedge_symbol")) {
        (None, _) => {
            Some("xemm: `maker_symbol` is required (an xEMM leg has no defensible default)".into())
        }
        (_, None) => {
            Some("xemm: `hedge_symbol` is required (an xEMM leg has no defensible default)".into())
        }
        (Some(maker), Some(hedge)) if maker == hedge => Some(format!(
            "xEMM legs must be DISTINCT symbols (got {maker:?} twice): the runtime finds a declared \
             leg by its name, so a same-named leg would route the maker's own quotes to the taker \
             venue, and `on_fill` would have no way to tell the legs apart; name a hedge of the same \
             instrument on another venue by its account, `{maker}@<venue>/<LABEL>` (decision 0293)"
        )),
        _ => None,
    }
}

// ---- The maker ------------------------------------------------------------------------------

/// ONE side's runtime quote state: the fields of the spread maker's `SideState` this maker reads.
/// [`Default`] is the fresh state (nothing resting, nothing suppressed).
#[derive(Default)]
struct SideState {
    /// The maker's own resting quote on this side as `(price, size)`, or `None`: the INTENDED
    /// quote, not the venue-side remainder (see `refresh_stale`).
    own: Option<(f64, f64)>,
    /// EVENT-ts until which this side is suppressed by the fill-rate breaker (`0` = not
    /// suppressed).
    suppressed_until: i64,
    /// Whether a quote is currently resting on this side.
    placed: bool,
    /// Set by a fill on this side, cleared when the side is next placed/re-priced/pulled: after a
    /// PARTIAL fill the intended-vs-target comparison would read "no change", so this forces the
    /// next tick through the modify arm once.
    refresh_stale: bool,
}

/// The cross-exchange maker. See the module doc for the design; per-knob semantics live on
/// [`XemmParams`]'s field docs.
///
/// CONFIG-vs-STATE split, exactly as `SpreadMaker` has it, and for the same reason: `config` is the
/// whole live-tunable bag and is replaced wholesale by [`XemmMaker::apply_params`], while every
/// runtime field is a SIBLING the swap never touches — so a hot re-tune preserves resting orders,
/// venue queue position, the hedge ledger, the breaker deadlines, the warm basis estimate and the
/// halt latch **by type shape**, not by a hand-maintained list of omissions.
///
/// The two SYMBOLS are IDENTITY, not config, and live outside `config` for that reason: a re-tune must
/// never be able to repoint a leg while an unhedged position is open.
pub struct XemmMaker {
    /// Every live-tunable knob, one `Copy` vike-model struct — the shape the live-params plane
    /// transports as [`vike_model::StrategyParams::Xemm`].
    config: XemmParams,
    /// The symbol this maker RESTS on (the mount's own). IDENTITY — see the type doc.
    maker_symbol: String,
    /// The leg this maker HEDGES on, by its NAME (a declared `MountLeg::at(symbol, taker_venue)`, or
    /// `SYMBOL@venue/LABEL` for one on another venue's account, decision 0293: the name its orders and
    /// fills carry). Must differ from `maker_symbol`: it is the only discriminator `on_fill` has (a
    /// `Fill` carries the leg's name but no venue), AND the runtime finds a declared leg BY ITS NAME,
    /// so a leg named like the mount's own symbol would route EVERY intent — including symbol-less
    /// tagged maker quotes — to the taker venue.
    hedge_symbol: String,
    /// The REFERENCE venue's last touch — the fair-value input. `None` until the first
    /// `on_reference_quote`, and a `None` here means NO QUOTES (never a fallback to the local book:
    /// quoting off the venue you are resting on is not a cross-exchange maker, it is an unpriced
    /// one).
    b_touch: Option<Touch>,
    /// The MAKER venue's OWN last touch — [`pricing::passive_clamp`]'s anchor. Also `None` ⇒ no
    /// quotes: without it, "non-marketable" is unprovable.
    a_touch: Option<Touch>,
    /// The BID side's runtime quote state ([`SideState`], the spread maker's, as this maker reads it).
    bid: SideState,
    /// The ASK side's [`SideState`] — the exact mirror.
    ask: SideState,
    /// The ONLY inventory authority. See [`HedgeLedger`]'s doc for why the broker seam is
    /// not used.
    hedge: HedgeLedger,
    /// Recent MAKER-leg fills inside the breaker window — the one-sided-flow netting accumulator
    /// (`vike_market_making::skew::net_signed_fills`). Hedge-leg fills are deliberately excluded: they are this
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
    last_tick_timestamp: i64,
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
    /// If `hedge_symbol == maker_symbol`. This is not a preference: the runtime finds a declared leg BY
    /// ITS NAME, so a leg named like the mount's own symbol routes EVERY intent — including the
    /// symbol-less tagged maker quotes — to the taker venue. The maker's own book would silently end up
    /// on the venue it meant to hedge on. It also destroys `on_fill`'s only leg discriminator. Fail at
    /// construction, loudly. A same-named instrument on another venue is a hedge named by its account,
    /// `BTCUSDT.P@bybit/LABEL` (decision 0293), which differs.
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
            "xEMM legs must be DISTINCT symbols (got {maker_symbol:?} twice): the runtime finds a \
             declared leg by its name, so a same-named leg would route the maker's own quotes to the \
             taker venue, and `on_fill` would have no way to tell the legs apart; name the hedge by \
             its account, `{maker_symbol}@<venue>/<LABEL>` (decision 0293)"
        );
        XemmMaker {
            config: XemmParams {
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
            last_tick_timestamp: 0,
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
        self.config.max_basis_bps = max_basis_bps;
        self.config.basis_halflife_ms = halflife_ms;
        self.config.basis_clamp = clamp;
        self
    }

    /// Inventory-skew SIZE shaping on the maker leg, off the OWNED ledger's `maker_position`. Neutral
    /// (`skew <= 0` or `max_inventory <= 0`) yields multipliers of exactly `1.0`.
    pub fn with_skew(mut self, target_inventory: f64, max_inventory: f64, skew: f64) -> Self {
        self.config.target_inventory = target_inventory;
        self.config.max_inventory = max_inventory;
        self.config.skew = skew;
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
        self.config.max_ref_age_ms = max_ref_age_ms;
        self.config.max_own_touch_age_ms = max_own_touch_age_ms;
        self.config.max_emission_gap_ms = max_emission_gap_ms;
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
        self.config.hedge_timeout_ms = hedge_timeout_ms;
        self.config.hedge_max_attempts = hedge_max_attempts;
        self.config.hedge_dust = hedge_dust;
        self
    }

    /// The two NAKED-inventory bands, in base units: `soft` suppresses the side that would GROW the
    /// exposure, `hard` halts + pulls + fires a `taker_flatten` impulse for the excess over `soft`.
    pub fn with_naked_bands(mut self, soft: f64, hard: f64) -> Self {
        self.config.naked_band = soft;
        self.config.naked_hard_band = hard;
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
        self.config.fill_window_ms = fill_window_ms;
        self.config.net_fill_threshold = net_fill_threshold;
        self.config.suppress_cooldown_ms = suppress_cooldown_ms;
        self
    }

    /// Anti-churn refresh tolerance (basis points of the RESTING value, both axes). Consulted ONLY
    /// on the re-price path — a place or a pull is never gated. `None`/all-zero = off.
    pub fn with_refresh_tolerance(mut self, price_bps: f64, size_bps: f64) -> Self {
        self.config.refresh_tolerance = Some(RefreshTolerance { price_bps, size_bps });
        self
    }

    /// How long after a halt the maker may AUTO-resume, in EVENT milliseconds. `0` (the default)
    /// means never: every halt reason names something an operator should look at.
    pub fn with_resume_after(mut self, resume_after_halt_ms: i64) -> Self {
        self.config.resume_after_halt_ms = resume_after_halt_ms;
        self
    }

    /// Read the maker's whole current tuning — the inverse of [`XemmMaker::apply_params`].
    pub fn params(&self) -> XemmParams {
        self.config
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
        (self.hedge.maker_position, self.hedge.hedge_position)
    }

    /// Hot-swap the whole tunable bag (the live-parameter plane). ONE struct copy, which is what
    /// makes it safe BY TYPE SHAPE: every runtime field — resting orders and their queue position,
    /// the hedge ledger, the breaker deadlines, the warm basis estimate, the halt latch, the
    /// learned tick — is a sibling of `config` and is untouched, so nothing can be forgotten here.
    ///
    /// NO ORDER VERB FIRES: the new knobs take effect on the NEXT tick, where `requote` re-prices
    /// the resting quotes IN PLACE.
    pub fn apply_params(&mut self, p: &XemmParams) {
        self.config = *p;
        self.params_epoch += 1;
    }

    // --- internals ---------------------------------------------------------------------------

    /// One side's runtime state, indexed by `is_bid` (the `SpreadMaker` convention — no mirrored
    /// `bid_*`/`ask_*` field pairs).
    fn side(&self, is_bid: bool) -> &SideState {
        if is_bid { &self.bid } else { &self.ask }
    }

    fn side_mut(&mut self, is_bid: bool) -> &mut SideState {
        if is_bid { &mut self.bid } else { &mut self.ask }
    }

    /// The maker venue's price grid: the tick LEARNED from its L2 book when one has been seen, else
    /// the configured param — so the standoff and the directional snap can never resolve against
    /// two different grids on the two lanes.
    fn effective_tick(&self) -> f64 {
        self.learned_maker_tick.unwrap_or(self.config.maker_tick_size)
    }

    fn breaker_enabled(&self) -> bool {
        self.config.fill_window_ms > 0 && self.config.net_fill_threshold > 0.0
    }

    /// Record a touch from EITHER venue and fold the basis when BOTH are known — paired
    /// observations only (see [`BasisEwma`]'s discipline).
    fn observe_touch(&mut self, is_reference: bool, touch: Touch) {
        if is_reference {
            self.b_touch = Some(touch);
        } else {
            self.a_touch = Some(touch);
        }
        self.last_tick_timestamp = self.last_tick_timestamp.max(touch.timestamp);
        if let (Some(a), Some(b)) = (self.a_touch, self.b_touch)
            && a.is_sane()
            && b.is_sane()
        {
            self.basis.observe(
                a.mid(),
                b.mid(),
                touch.timestamp,
                self.config.basis_halflife_ms,
                self.config.basis_clamp,
            );
        }
    }

    /// Read what a unit of each leg is worth (the broker's `contract_value`, the default
    /// unit where the broker does not say) and the price the two convert at (the reference touch's
    /// mid, else the maker venue's), into the ledger before it is used: a unit on one venue is not a
    /// unit on the other (`hedge::LegValues`). Two legs of the same value convert nothing. A value
    /// enters here from outside the money path, so a degenerate unit is folded to one
    /// (`ContractValue::folded`), as every such door does.
    fn refresh_legs<B: OrderBroker>(&mut self, broker: &B) {
        let value = |s: &str| broker.contract_value(s).unwrap_or_default().folded();
        let px = [self.b_touch, self.a_touch]
            .into_iter()
            .flatten()
            .find(Touch::is_sane)
            .map_or(f64::NAN, |t| t.mid());
        self.hedge.legs = LegValues {
            maker: value(&self.maker_symbol),
            hedge: value(&self.hedge_symbol),
            price: px,
        };
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
        if self.config.resume_after_halt_ms > 0 && now - since >= self.config.resume_after_halt_ms {
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
        if guards::stale(self.b_touch.map(|t| t.timestamp), now, self.config.max_ref_age_ms) {
            return Some(HaltReason::ReferenceStale);
        }
        if guards::stale(self.a_touch.map(|t| t.timestamp), now, self.config.max_own_touch_age_ms) {
            return Some(HaltReason::OwnTouchStale);
        }
        if guards::stale(
            (self.last_tick_timestamp > 0).then_some(self.last_tick_timestamp),
            now,
            self.config.max_emission_gap_ms,
        ) {
            return Some(HaltReason::FeedImpaired);
        }
        if self.hedge.attempts_exhausted(
            now,
            self.config.hedge_ratio,
            self.config.hedge_dust,
            self.config.hedge_timeout_ms,
            self.config.hedge_max_attempts,
        ) {
            return Some(HaltReason::HedgeUnfilled);
        }
        if guards::over_hard_band(self.hedge.naked(), self.config.naked_hard_band) {
            return Some(HaltReason::NakedHardBand);
        }
        if self.basis.out_of_band(self.config.max_basis_bps) {
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
    fn requote<B: OrderBroker>(&mut self, broker: &mut B, now: i64) {
        // (0)
        self.refresh_legs(broker);
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
            self.config.min_profitability,
            self.config.total_fee,
        );
        // (2) the passive clamp — `min_edge_ticks` floored at 1.0, because a sub-tick standoff
        // rounds onto the touch and stops being a standoff at all.
        let tick = self.effective_tick();
        let standoff = tick * self.config.min_edge_ticks.max(1.0);
        let Some((bid_price, ask_price)) =
            pricing::passive_clamp(raw_bid, raw_ask, Some(a), standoff)
        else {
            // An unprovable quote is not emitted — the clamp refuses rather than degrading.
            self.pull_all(broker);
            return;
        };
        // (3) DIRECTIONAL snap onto the maker venue's grid.
        let bid_price = pricing::snap_down(bid_price, tick);
        let ask_price = pricing::snap_up(ask_price, tick);
        if ask_price <= bid_price {
            self.pull_all(broker);
            return;
        }
        // (4) sizes, skewed off the OWNED maker-leg inventory.
        let (bid_multiplier, ask_multiplier) = skew_multipliers(
            self.hedge.maker_position,
            self.config.target_inventory,
            self.config.max_inventory,
            self.config.skew,
        );
        let bid_quantity = self.config.qty * bid_multiplier;
        let ask_quantity = self.config.qty * ask_multiplier;
        // (5) suppression.
        let (naked_bid, naked_ask) =
            guards::soft_band_suppression(self.hedge.naked(), self.config.naked_band);
        let breaker_bid = self.bid.suppressed_until > 0 && now < self.bid.suppressed_until;
        let breaker_ask = self.ask.suppressed_until > 0 && now < self.ask.suppressed_until;
        let bid_sup = naked_bid || breaker_bid || !(bid_quantity.is_finite() && bid_quantity > 0.0);
        let ask_sup = naked_ask || breaker_ask || !(ask_quantity.is_finite() && ask_quantity > 0.0);
        // (6)
        self.emit_side(broker, true, (bid_price, bid_quantity), bid_sup, now);
        self.emit_side(broker, false, (ask_price, ask_quantity), ask_sup, now);
    }

    /// Send whatever the hedge ledger still OWES on the taker venue — the target-residual
    /// discipline in one place.
    ///
    /// ⚠ An UNTAGGED market `place` on the hedge symbol (`place(hedge_symbol, "", .., Market)`, the
    /// row the old untagged market submit buffered) is the only verb used here, and that is a hard contract,
    /// not a style choice: it names its symbol, so the runtime's `resolve_intent_venue` finds the
    /// declared leg BY SYMBOL, where a quote placed on `""` resolves to the drain's series. A quote's
    /// verb here would place the hedge on the MAKER venue, doubling the exposure it was sent to close.
    ///
    /// Nothing is sent while a hedge is in flight and not yet timed out, or once the residual's
    /// attempt budget is spent (`detect_fault` then raises `HedgeUnfilled`). A retry re-sends the
    /// RESIDUAL, which a late original fill has already shrunk — so it can never double the
    /// position.
    fn drive_hedge<B: OrderBroker>(&mut self, broker: &mut B, now: i64) {
        self.refresh_legs(broker);
        let Some((side, qty)) =
            self.hedge.residual(self.config.hedge_ratio, self.config.hedge_dust)
        else {
            self.hedge.settle();
            return;
        };
        if self.hedge.in_flight.is_some()
            && !self.hedge.timed_out(now, self.config.hedge_timeout_ms)
        {
            return;
        }
        if self.hedge.attempts >= self.config.hedge_max_attempts {
            return;
        }
        broker.place(&self.hedge_symbol, "", side, qty, OrderPlacement::Market, false);
        self.hedge.fire(now);
    }

    /// The HARD-BAND impulse leg: cross the taker venue to cut the exposure back to the SOFT band
    /// (`vike_market_making::taker_flatten`'s flatten-the-excess law).
    ///
    /// Fires at most ONCE per halt (`flatten_fired`, cleared on resume) and ONLY while the hedge
    /// ledger owes nothing — an in-flight hedge is ALREADY closing the exposure, so a second market
    /// on top of it would over-hedge. In the fully-hedged configuration (`hedge_ratio == 1.0`) the
    /// residual and the naked exposure are the same quantity, so this leg is unreachable by
    /// construction; it exists for the deliberately-partial (`hedge_ratio < 1.0`) case and for an
    /// exposure that arrived some way the ledger's own retry cannot close.
    fn drive_flatten<B: OrderBroker>(&mut self, broker: &mut B) {
        self.refresh_legs(broker);
        if self.flatten_fired
            || self.hedge.residual(self.config.hedge_ratio, self.config.hedge_dust).is_some()
        {
            return;
        }
        if !guards::over_hard_band(self.hedge.naked(), self.config.naked_hard_band) {
            return;
        }
        let Some((side_sign, qty)) = taker_flatten(self.hedge.naked(), self.config.naked_band)
        else {
            return;
        };
        // `taker_flatten` cuts the MAKER-unit exposure; the order goes out in the hedge leg's units.
        let qty = self.hedge.legs.to_hedge(qty).abs();
        if !(qty.is_finite() && qty > 0.0) {
            return; // an excess that cannot be sized on the hedge leg is not sent
        }
        let side = if side_sign > 0.0 { 1 } else { -1 };
        broker.place(&self.hedge_symbol, "", side, qty, OrderPlacement::Market, false);
        self.flatten_fired = true;
    }

    /// Fold ONE maker-leg fill into the breaker window and arm the tripped side's cooldown.
    /// Hedge-leg fills never reach this: they are the strategy's own mechanical offset, and netting
    /// them would cancel every real one-sided signal to zero.
    fn record_maker_fill(&mut self, side: i32, size: f64, ts: i64) {
        if !self.breaker_enabled() {
            return;
        }
        self.fills.push_back(FillRec { side, size, timestamp: ts });
        let cutoff = ts - self.config.fill_window_ms;
        while self.fills.front().is_some_and(|f| f.timestamp < cutoff) {
            self.fills.pop_front();
        }
        let net = net_signed_fills(&self.fills, ts, self.config.fill_window_ms);
        if net >= self.config.net_fill_threshold {
            self.bid.suppressed_until = ts + self.config.suppress_cooldown_ms;
        } else if net <= -self.config.net_fill_threshold {
            self.ask.suppressed_until = ts + self.config.suppress_cooldown_ms;
        }
    }

    /// Build from a `[strategy.params]` table — a READER, not a schema: unknown keys are
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
    pub fn from_params(params: &Params) -> Self {
        let maker = params
            .get("maker_symbol")
            .and_then(ParamValue::as_str)
            .expect("xemm: `maker_symbol` is required (an xEMM leg has no defensible default)");
        let hedge = params
            .get("hedge_symbol")
            .and_then(ParamValue::as_str)
            .expect("xemm: `hedge_symbol` is required (an xEMM leg has no defensible default)");
        let get = |k: &str| params.get(k).and_then(ParamValue::as_f64);
        let get_i = |k: &str| params.get(k).and_then(ParamValue::as_integer);
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
        let derived = m.config;
        m.config = XemmParams {
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

// The xEMM's QUOTE-EMISSION arm — place / re-price / pull ONE side of the maker book.
//
// Structurally the same three mutually-exclusive branches as `SpreadMaker::requote_single_side`
// (the spread maker's quote emission), reproduced here rather than shared because the two makers differ entirely in
// how they arrive at a price (`SpreadMaker` prices off its OWN book; this one off a foreign
// venue's touch) and share only this ~30-line tail, which calls four `SpreadMaker`-private
// helpers. The invariants, however, are carried over VERBATIM and are pinned by tests:
//
// 1. **A PLACE and a PULL are never tolerance-gated.** `within_tolerance` is reachable only from
//    the re-price arm, so a quote that must go on — or come off — the book always does, whatever
//    the anti-churn knob is set to.
// 2. **A FILL invalidates that side's snapshot.** `SideState::own` is the maker's INTENDED quote,
//    not the venue-side remainder, so after a PARTIAL fill an intended-vs-target comparison reads
//    "no change" and the size top-up would be silently lost. `refresh_stale` forces the next tick
//    through the modify arm exactly once.
// 3. **`mass_cancel` is NEVER used.** In the live runtime it is scoped to the DISPATCHING series
//    (`OrderIntent::MassCancel { venue, symbol }` built from `drain_broker`'s arguments), so from a
//    hedge-fill dispatch it would target the TAKER venue and cancel nothing on the maker venue —
//    a pull that silently does not pull. [`XemmMaker::pull_all`] is two `cancel`s by tag, which key
//    off the tag registry and therefore reach the right orders from every lane.
//
// The tags are the bare literals `"bid"`/`"ask"`. A strategy never sees an order-client-id: the
// runtime resolves `{mount_index}|{venue}|{symbol}|{tag}` at drain time, and that key is the MOUNT's
// own series on EVERY lane — built by `CoreThread::tag_key`, not from `drain_broker`'s arguments —
// so a tag names ONE quote regardless of which series woke the strategy. ⚠ Invariant 3 above is
// untouched by that: `mass_cancel` lowers to `OrderIntent::MassCancel { venue, symbol }` built from
// the DRAIN's arguments, which is a different code path from the tag registry and still
// series-scoped, so it stays unused here.

/// The bid side's tag.
const BID_TAG: &str = "bid";
/// The ask side's tag.
const ASK_TAG: &str = "ask";

impl XemmMaker {
    /// Place, re-price or pull ONE side. `target` is that side's already-clamped, already-snapped,
    /// already-skewed `(price, size)`; `suppressed` is the combined breaker / naked-band /
    /// zero-size verdict for this side this tick.
    fn emit_side<B: OrderBroker>(
        &mut self,
        broker: &mut B,
        is_bid: bool,
        target: (f64, f64),
        suppressed: bool,
        _ts: i64,
    ) {
        let (px, qty) = target;
        let (tag, side_code) = if is_bid { (BID_TAG, 1) } else { (ASK_TAG, -1) };
        if suppressed {
            self.pull_side(broker, is_bid);
        } else if !self.side(is_bid).placed {
            // PLACE — never tolerance-gated.
            broker.place("", tag, side_code, qty, OrderPlacement::Limit { price: px }, false);
            let side = self.side_mut(is_bid);
            side.placed = true;
            side.own = Some((px, qty));
            side.refresh_stale = false;
        } else if !self.refresh_skips(is_bid, px, qty) {
            // RE-PRICE IN PLACE — modify, never cancel/replace, so venue queue priority survives.
            broker.amend(tag, Some(qty), Some(px), None);
            let side = self.side_mut(is_bid);
            side.own = Some((px, qty));
            side.refresh_stale = false;
        }
    }

    /// PULL one side if it is resting — never tolerance-gated (invariant 1 above). Idempotent: a
    /// side that is not placed sends nothing, so a halted maker does not spam cancels every tick.
    fn pull_side<B: OrderBroker>(&mut self, broker: &mut B, is_bid: bool) {
        if !self.side(is_bid).placed {
            return;
        }
        broker.cancel(if is_bid { BID_TAG } else { ASK_TAG });
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
    }

    /// Take BOTH sides off the book — every halt, every refusal to price, every fault. Two
    /// `cancel`s by tag rather than `mass_cancel`, for the reason in the module doc.
    fn pull_all<B: OrderBroker>(&mut self, broker: &mut B) {
        self.pull_side(broker, true);
        self.pull_side(broker, false);
    }

    /// `true` when the fresh target is close enough to what this side already has RESTING that
    /// re-issuing the modify would be pure wire churn.
    ///
    /// Short-circuits to `false` (ALWAYS re-quote) when the side's snapshot is `refresh_stale` —
    /// invariant 2 — and when no tolerance is configured, which is the default and keeps an
    /// unconfigured maker re-pricing on every tick.
    fn refresh_skips(&self, is_bid: bool, px: f64, qty: f64) -> bool {
        let side = self.side(is_bid);
        if side.refresh_stale {
            return false;
        }
        let (Some(tolerance), Some(resting)) = (self.config.refresh_tolerance, side.own) else {
            return false;
        };
        within_tolerance(resting, (px, qty), tolerance)
    }
}

// `impl<B: OrderBroker> Strategy<B> for XemmMaker` — the tick / reference / fill / control /
// order-event lanes, plus the durable-state DTO.
//
// # ⚠ WHICH LANES MAY EMIT, AND WHY IT IS NOT UNIFORM
//
// The live runtime drains a strategy's buffered intents against the DISPATCHING series, not the
// mount's declared one — `drain_broker(ctx, venue, symbol, …)` is called with the bar's / tick's /
// **fill's** `(venue, symbol)`, and a SYMBOL-LESS intent resolves to exactly that. For a two-venue
// mount the drain series therefore differs between lanes. So:
//
// | lane | drain series | this maker's behaviour |
// |---|---|---|
// | `on_quote_tick` / `on_order_book` / `on_trade_tick` (maker venue) | MAKER | update `a_touch`, **requote** |
// | `on_reference_quote` (taker venue) | **MAKER** — the lane drains on the mount's own series | update `b_touch`, **requote** |
// | `on_fill`, maker leg | MAKER | fold, arm breaker, **fire the hedge**, requote |
// | `on_fill`, hedge leg | **TAKER** | fold + hedge retry ONLY — **never a tagged SUBMIT** |
// | `on_feed_status` (maker venue only) | MAKER | halt + pull |
// | `on_schedule` | MAKER | the all-feeds-dead safety sweep |
// | `on_params_updated` | MAKER | hot swap, **no order verb** |
// | `on_order_event` | the ORDER's | a quote's busy refusal adopts its side, **no order verb** |
//
// The hedge-leg row is the sharp one, and ⚠ **half its hazard is now HISTORY** — worth recording,
// because the surviving half is the one that would actually bite a future edit here. It used to be
// both: a `cancel_tagged("bid")` from a hedge-fill dispatch looked up
// `{mount}|TAKER_VENUE|HEDGE_SYM|bid`, found nothing, and SILENTLY LEFT A REAL QUOTE RESTING on the
// maker venue. That is fixed — `CoreThread::tag_key` keys the registry on the MOUNT's own series
// for the insert and both lookups, so a tag names one quote from every lane
// (`crates/vike-core/tests/wiring/multi_symbol_reads.rs`). The emission side went next (one broker
// plan, step 12): a quote placed on the mount's own symbol `""` on a mount that declares legs now
// resolves to the MOUNT's own symbol and venue (decision 0277's rule 1), no longer to the DRAIN's
// series, which from the hedge arm would have placed the maker's quote on the TAKER venue. The
// untagged market on the hedge symbol (`place(hedge_symbol, "", .., Market)`) names its symbol, so
// `resolve_intent_venue` routes it correctly from EITHER dispatch. `on_fill`'s hedge arm still
// returns without touching `requote`/`pull_all`: keeping the whole arm order-free is one rule rather
// than a per-verb audit.
//
// `on_reference_quote` is safe to emit from for a second, independent reason:
// `CoreThread::drive_strategy_reference_quote` deliberately drains on the MOUNT's own series, so a
// quote PLACED there lands on the maker venue rather than the reference one (pinned by
// `vike-core/tests/wiring/xemm_reference_quote.rs`). Making the reference lane the PRIMARY re-quote clock
// is the whole point: a reference move is what makes a resting quote stale, and if venue A is
// quiet while venue B moves, an observe-only reference lane would leave the maker to be picked
// off — the exact adverse selection this strategy exists to avoid.
//
// `on_order_event` answers ONE event: the busy refusal of a quote (P1,
// `OrderEvent::is_tag_busy_refusal`), a place under `"bid"`/`"ask"` refused because the tag still
// names a working order. That order is the side's, so the side is ADOPTED as `SpreadMaker` adopts
// one (placed, terms unknown, re-priced on the next tick) instead of being placed again and refused
// on every tick. Every other event is still unanswered, for a reason that has CHANGED: it used to be
// that `OrderEvent` carried an order-client-id and a transition but no symbol, while this maker
// never sees an order client id, so it had no way to attribute an event to a leg at all. `OrderEvent::tag`
// now carries the maker's OWN tag, so attribution IS available: a per-leg tag names the leg. What
// remains is that wiring the rest means deciding how a rejected or externally-cancelled HEDGE leg
// folds back into `HedgeLedger`/`InFlight` — a hedge accounting question, not a plumbing one, and the
// ledger is the one piece of state that must never be wrong. Until that is designed, no reaction is
// better than a guessed one. (`SpreadMaker`'s own `on_order_event` is the single-quote precedent:
// see `strategies/market_making/spread_maker.rs`'s `on_order_event`.)

/// Wire version this build writes and accepts. Bump on any breaking shape change to
/// [`XemmStateV1`]; [`XemmMaker::load_state`] ignores (fail-open, warns) any other value rather
/// than guessing at a migration — the `SpreadMakerStateV1` convention.
const XEMM_STATE_VERSION: u32 = 1;

/// Durable-state wire DTO — a DEDICATED type, never a derive on the live struct, so the maker's
/// internals can be reshaped without a version bump.
///
/// Persists the two things a restart must NOT silently reset:
///
/// 1. **the hedge ledger** — an unhedged position is REAL money at risk on two venues; forgetting
///    it means the maker restarts believing it is flat and quotes on top of an open exposure;
/// 2. **the halt latch** — a maker halted because its hedge would not fill, or because the pair
///    decoupled, must not un-halt merely by being restarted. That is the whole failure mode a
///    manual-resume default exists to prevent, and a restart is the easiest accidental resume.
///
/// Deliberately EXCLUDES: the two touches (a restart's first ticks re-seed them, and a stale saved
/// touch would be worse than none), the basis estimate (re-warms from live paired observations —
/// and a saved estimate could halt a healthy maker on a regime that has since ended), the breaker's
/// raw fill tape (the DEADLINES are its behavioural effect and are cheap to keep), and `config` (a
/// restart with a different tuning must price with the NEW tuning).
#[derive(Debug, Serialize, Deserialize)]
struct XemmStateV1 {
    v: u32,
    maker_position: f64,
    hedge_position: f64,
    hedge_attempts: u32,
    /// EVENT ts of an in-flight hedge, if one was outstanding at save time.
    hedge_in_flight_timestamp: Option<i64>,
    /// `Some((reason, since))` when halted.
    halt: Option<(HaltReason, i64)>,
    bid_suppressed_until: i64,
    ask_suppressed_until: i64,
}

impl<B: OrderBroker> Strategy<B> for XemmMaker {
    /// The MAKER venue's L1 lane: record venue A's own touch (the passive clamp's anchor) and
    /// re-quote. Note this is NOT the pricing input — venue B's touch is (`on_reference_quote`);
    /// A's book is only ever used to keep the quote non-marketable.
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.observe_touch(false, Touch { bid: q.bid, ask: q.ask, timestamp: q.timestamp });
        self.requote(broker, q.timestamp);
    }

    /// The MAKER venue's L2 lane. Learns the venue's REAL tick grid from the book and adopts it on
    /// the L1 lane too (`learned_maker_tick`), so the standoff and the directional snap can never
    /// resolve against two different grids. A one-sided book has no touch to anchor on, so it
    /// updates nothing and lets the freshness bound speak.
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        if book.tick_size > 0.0 {
            self.learned_maker_tick = Some(book.tick_size);
        }
        let now = broker.now();
        if let (Some(L2BookLevel { price: bid, .. }), Some(L2BookLevel { price: ask, .. })) =
            (book.best_bid(), book.best_ask())
        {
            self.observe_touch(false, Touch { bid, ask, timestamp: now });
        }
        self.requote(broker, now);
    }

    /// The MAKER venue's trade tape. Carries no touch, so it updates nothing — but it DOES advance
    /// the clock through `requote`, which is what lets a freshness bound fire on a venue that is
    /// printing trades while its quote feed has silently frozen.
    fn on_trade_tick(&mut self, broker: &mut B, t: &TickTrade) {
        self.requote(broker, t.timestamp);
    }

    /// **THE PRICING INPUT** — the REFERENCE (taker) venue's touch, and the maker's PRIMARY
    /// re-quote clock. See the module doc for why emitting from this lane is safe (the runtime
    /// drains it on the mount's OWN series) and why observe-only would be worse than useless.
    fn on_reference_quote(&mut self, broker: &mut B, _venue: &str, q: &TickQuote) {
        self.observe_touch(true, Touch { bid: q.bid, ask: q.ask, timestamp: q.timestamp });
        self.requote(broker, q.timestamp);
    }

    /// Fills, routed by `Fill::symbol` — the only leg discriminator available (a `Fill` carries a
    /// symbol but no venue), which is why distinct leg symbols are a construction-time assertion.
    ///
    /// An EMPTY symbol is read as the MAKER leg, following the same `""` means "my mount"
    /// convention `LiveBroker::named` uses for the order verbs. Live fills always carry a real
    /// symbol (`dispatch_applied_fills` stamps `f.symbol`), so this only affects test/paper doubles.
    ///
    /// A fill for NEITHER leg fails CLOSED (`HaltReason::UnknownFillSymbol`): the maker can no
    /// longer account for its own inventory, and continuing to quote on top of an exposure it
    /// cannot measure is the worst available option. It latches only — no order verb — because the
    /// dispatch series of an unattributable fill is unknown, and a tagged cancel from the wrong
    /// series is a silent no-op. The next maker-venue lane pulls both sides through `requote`'s
    /// halt branch.
    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        if fill.symbol == self.hedge_symbol {
            // ⚠ THE HEDGE LEG — this dispatch drains on the TAKER venue's series. Fold and (if the
            // hedge is still short of its target) re-fire, using the SYMBOL-CARRYING market verb
            // only. No tagged verb, no `requote`, no `pull_all`: see the module doc.
            self.hedge.on_hedge_fill(fill.side, fill.size);
            self.drive_hedge(broker, fill.timestamp);
            return;
        }
        if !fill.symbol.is_empty() && fill.symbol != self.maker_symbol {
            self.enter_halt(HaltReason::UnknownFillSymbol, fill.timestamp);
            return;
        }
        // The MAKER leg — this dispatch drains on the mount's OWN series, so every verb is safe.
        self.hedge.on_maker_fill(fill.side, fill.size);
        if fill.side != 0 {
            // `SideState::own` is the INTENDED quote, not the venue-side remainder, so a partial
            // fill's top-up would otherwise read as "no change" and be silently lost.
            self.side_mut(fill.side > 0).refresh_stale = true;
        }
        self.record_maker_fill(fill.side, fill.size, fill.timestamp);
        // HEDGE FIRST, quote second: the exposure this fill just created is the urgent thing, and
        // re-quoting first would leave it open for the length of the quote path.
        self.drive_hedge(broker, fill.timestamp);
        self.requote(broker, fill.timestamp);
        self.drive_flatten(broker);
    }

    /// The MAKER venue's feed health (the only one the runtime routes here — `FeedStatus` carries
    /// neither venue nor symbol, so the reference venue's status is not deliverable). Any non-`Live`
    /// status halts and pulls: without its own book the passive clamp has no anchor.
    ///
    /// The REFERENCE venue's liveness is covered by `max_ref_age_ms`, which is strictly stronger
    /// than a status subscription anyway — it also catches a feed that freezes without ever
    /// reporting a disconnect.
    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {
        if status != FeedStatus::Live {
            self.enter_halt(HaltReason::FeedImpaired, broker.now());
            self.pull_all(broker);
        }
    }

    /// THE ALL-FEEDS-DEAD SAFETY SWEEP. Every other lane fires off a market message, so if BOTH
    /// venues go silent none of them run and a stale quote would rest forever. The runtime's
    /// wall-clock schedule is the one lane that fires regardless, and it drains on the mount's own
    /// series — so it can safely re-run the whole funnel: the freshness bounds trip, the maker
    /// halts, both quotes come off, and any owed hedge gets its retry.
    fn on_schedule(&mut self, broker: &mut B, _tag: &str) {
        let now = broker.now();
        self.drive_hedge(broker, now);
        self.requote(broker, now);
        self.drive_flatten(broker);
    }

    /// The busy refusal of a quote ADOPTS its side (module doc): placed, its terms unknown and
    /// stale, so the next tick amends the order the tag still names. No order verb; every other
    /// event is unanswered.
    fn on_order_event(&mut self, _broker: &mut B, event: &OrderEvent) {
        if !event.is_tag_busy_refusal() {
            return;
        }
        let is_bid = match event.tag.as_deref() {
            Some(BID_TAG) => true,
            Some(ASK_TAG) => false,
            _ => return,
        };
        let side = self.side_mut(is_bid);
        side.placed = true;
        side.own = None;
        side.refresh_stale = true;
    }

    /// The live-parameter plane. Consumes ONLY its own variant; a foreign one (e.g. a sibling
    /// `SpreadMaker` mount's) is ignored, which is the live-params contract. The broker is
    /// UNTOUCHED — the new knobs take effect on the next tick's in-place re-price, so a re-tune
    /// that does not move a price does not disturb the book at all.
    fn on_params_updated(&mut self, _broker: &mut B, params: &StrategyParams) {
        if let StrategyParams::Xemm(p) = params {
            self.apply_params(p);
        }
    }

    /// The READ half: this maker's whole current tuning, via [`XemmMaker::params`] — the accessor
    /// that is already documented as the inverse of `apply_params`. It answers honestly because
    /// `config` IS the live bag (nothing is stripped out of it into a separate estimator, unlike
    /// `SpreadMaker`'s A-S sub-bag), so what comes back is exactly what the next requote prices
    /// from. The two SYMBOLS stay out of it deliberately — they are mount IDENTITY, not tunables.
    fn params(&self) -> Option<StrategyParams> {
        Some(StrategyParams::Xemm(XemmMaker::params(self)))
    }

    /// Persist the hedge ledger + the halt latch — see [`XemmStateV1`] for exactly what and why.
    /// Always `Some`: even a flat, running maker has a ledger worth round-tripping, and a `None`
    /// here would be indistinguishable from "this build forgot to save".
    fn save_state(&self) -> Option<vike_model::StrategyState> {
        let dto = XemmStateV1 {
            v: XEMM_STATE_VERSION,
            maker_position: self.hedge.maker_position,
            hedge_position: self.hedge.hedge_position,
            hedge_attempts: self.hedge.attempts,
            hedge_in_flight_timestamp: self.hedge.in_flight.map(|f| f.timestamp),
            halt: match self.halt {
                Halt::Running => None,
                Halt::Halted { reason, since } => Some((reason, since)),
            },
            bid_suppressed_until: self.bid.suppressed_until,
            ask_suppressed_until: self.ask.suppressed_until,
        };
        vike_model::StrategyState::of(&dto).ok()
    }

    /// FAIL-OPEN restore: an unparsable payload or a version mismatch is warned and IGNORED, so a
    /// shape this build does not recognize can never be half-applied.
    ///
    /// ⚠ A restore can only make the maker MORE cautious, never less: a saved halt re-latches, a
    /// saved exposure is re-adopted. It never clears a halt the freshly-constructed maker holds
    /// (a fresh maker is `Running`, so `None` here simply leaves it running).
    fn load_state(&mut self, state: &vike_model::StrategyState) {
        let dto: XemmStateV1 = match state.read() {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(%err, "XemmMaker::load_state: unparsable saved state, keeping fresh");
                return;
            }
        };
        if dto.v != XEMM_STATE_VERSION {
            tracing::warn!(
                found_version = dto.v,
                expected_version = XEMM_STATE_VERSION,
                "XemmMaker::load_state: saved state version mismatch, keeping fresh state"
            );
            return;
        }
        self.hedge = HedgeLedger {
            maker_position: dto.maker_position,
            hedge_position: dto.hedge_position,
            attempts: dto.hedge_attempts,
            in_flight: dto.hedge_in_flight_timestamp.map(|ts| InFlight { timestamp: ts }),
            legs: self.hedge.legs,
        };
        self.halt = match dto.halt {
            None => Halt::Running,
            Some((reason, since)) => Halt::Halted { reason, since },
        };
        self.bid.suppressed_until = dto.bid_suppressed_until;
        self.ask.suppressed_until = dto.ask_suppressed_until;
    }
}

#[cfg(test)]
mod tests {
    //! The cross-exchange maker's white-box tests, moved from `vike-market-making` with the maker, one nested
    //! module per former file (`xemm_guards`, `xemm_hedge`, `xemm_mount`), every body as it was but for
    //! its paths, over the shared `LiveBroker` rig below (`vike-market-making`'s test fixtures). They read the
    //! maker's own fields through `super::*` and name every other crate by its absolute path: inside
    //! this file `crate::` is the strategy-lab crate that compiles it.

    use super::*;
    // The concrete live broker the tests mount the maker on, built via a struct literal (its
    // buffered-order fields are `pub`); a dev-dependency of the strategy-lab crate.
    use vike_core::LiveBroker;

    /// A bare `LiveBroker` at a given inventory + EVENT ts, empty order buffers — the same struct the
    /// runtime builds per tick/fill, so what the strategy pushes into it is exactly what the live drain
    /// would route.
    fn broker(position: f64, now: i64) -> LiveBroker {
        LiveBroker {
            data_delay: None,
            inputs: None,
            closed_trades: None,
            positions: Vec::new(),
            prices: Vec::new(),
            bar_views: Vec::new(),
            leg_values: None,
            position,
            average_price: 0.0,
            price: 0.0,
            equity: 0.0,
            bars: std::sync::Arc::new(Vec::new()),
            index: 0,
            now,
            contract_value: vike_model::ContractValue::default(),
            quote_rate: 1.0,
            lot_size: 0.0,
            tick_size: 0.0,
            min_qty: 0.0,
            min_notional: 0.0,
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            order_lists: Vec::new(),
            combos: Vec::new(),
            order_emulation_arms: Vec::new(),
            mass_cancel: false,
            forecasts: Vec::new(),
        }
    }

    fn submitted(b: &LiveBroker, tag: &str) -> bool {
        b.submissions.iter().any(|s| s.tag.as_deref() == Some(tag))
    }
    fn modified(b: &LiveBroker, tag: &str) -> bool {
        b.modifications.iter().any(|m| m.tag == tag)
    }
    fn canceled(b: &LiveBroker, tag: &str) -> bool {
        b.cancels.iter().any(|t| t == tag)
    }

    /// Compiles only if the maker `S` mounts on BOTH a family's own foreign `OrderBroker` `M` and the
    /// concrete `LiveBroker` — the genericity proof, made at type-check time (a
    /// `Strategy<LiveBroker>`-only impl fails the `M` bound).
    fn assert_mounts_on<S: Strategy<M> + Strategy<LiveBroker>, M: OrderBroker>() {}

    /// One buffered submission, rendered — `tag`, `symbol`, `side`, `qty`, `price`.
    ///
    /// `BufferedSubmit`/`BufferedModify` are runtime LANE PAYLOADS with no `Debug` impl, so an
    /// assertion that fails on them can print nothing useful. The xemm families' failures are only
    /// diagnosable with the whole row (which leg? tagged or not? what size?), so they render through
    /// this instead.
    type SubmitRow = (Option<String>, Option<String>, i32, f64, Option<f64>);

    /// Every buffered submission as a [`SubmitRow`], for assertion messages.
    fn submits_dbg(b: &LiveBroker) -> Vec<SubmitRow> {
        b.submissions
            .iter()
            .map(|s| (s.tag.clone(), s.symbol.clone(), s.side, s.qty, s.price))
            .collect()
    }

    /// The tags of every buffered modify — the `BufferedModify` twin of [`submits_dbg`].
    fn modifies_dbg(b: &LiveBroker) -> Vec<String> {
        b.modifications.iter().map(|m| m.tag.clone()).collect()
    }

    // --- the cross-exchange maker's rig, shared by `xemm_guards`, `xemm_hedge` and `xemm_mount` ----

    const MAKER: &str = "BTC"; // the mount's own symbol, on the maker venue
    const HEDGE: &str = "BTC-USDT-SWAP"; // the declared leg, on the taker venue
    const REF_VENUE: &str = "okx";

    /// A reference (taker-venue) touch.
    fn ref_q(ts: i64, bid: f64, ask: f64) -> TickQuote {
        TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: HEDGE.into(),
        }
    }

    /// A maker-venue touch.
    fn own_q(ts: i64, bid: f64, ask: f64) -> TickQuote {
        TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: MAKER.into(),
        }
    }

    fn fill_of(symbol: &str, side: i32, size: f64, ts: i64) -> Fill {
        Fill {
            side,
            size,
            price: 100.0,
            fee: 0.0,
            timestamp: ts,
            is_maker: true,
            symbol: symbol.into(),
            tag: None,
        }
    }

    /// A maker with the naked bands opened wide, so the HEDGE mechanics are what is under test rather
    /// than the exposure guards (those get their own file). 10 bp edge, 6.5 bp fee, 0.01 grid.
    fn maker() -> XemmMaker {
        XemmMaker::new(MAKER, HEDGE, 1.0, 0.001, 0.00065, 0.01).with_naked_bands(1e9, 1e9)
    }

    /// Feed both venues' touches so the maker is warm and quoting: reference 100.00/100.50, maker
    /// venue 100.00/100.02 (a tight local book straddling the reference bid).
    fn warm(m: &mut XemmMaker, ts: i64) {
        m.on_reference_quote(&mut broker(0.0, ts), REF_VENUE, &ref_q(ts, 100.0, 100.5));
        m.on_quote_tick(&mut broker(0.0, ts), &own_q(ts, 100.0, 100.02));
    }

    mod xemm_guards {
        //! The xEMM SAFETY layer — every way the maker stops quoting, and the one read it must never make.
        //!
        //! A cross-exchange maker's characteristic failure is not a bad price, it is quoting confidently
        //! off information it no longer has: a frozen reference feed, an anchor that has moved, an exposure
        //! it cannot see. Each test here drives one of those to its verdict.
        //!
        //! The first test is the structural one: `the_maker_never_reads_position_price_or_equity_through_the_broker`
        //! pins that the maker's output is BIT-IDENTICAL against a broker whose every read surface is
        //! poisoned. That is what lets the whole strategy own its inventory in a ledger instead of trusting
        //! three live-runtime read paths that are all silently wrong for a foreign-venue leg.

        use super::*;

        /// A bar every field of which is unusable — so a maker that reads HISTORY, not just the scalars,
        /// still moves under [`poisoned`].
        fn poison_bar(ts: i64) -> vike_marketdata::Bar {
            vike_marketdata::Bar {
                timestamp: ts,
                open: f64::NAN,
                high: -1e300,
                low: f64::NAN,
                close: -1e300,
                volume: f64::NAN,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        }

        /// A broker whose EVERY read surface is poisoned — the scalars, and the per-symbol tables a
        /// declared-multi mount carries (`bar_views` included, which also makes this the declared-mount
        /// shape: an uncarried symbol reads EMPTY here rather than falling through to the scalars).
        /// If the maker consults any of them the emitted prices move.
        fn poisoned(now: i64) -> LiveBroker {
            LiveBroker {
                data_delay: None,
                inputs: None,
                closed_trades: None,
                positions: vec![(MAKER.into(), -1e300), (HEDGE.into(), f64::NAN)],
                prices: vec![(MAKER.into(), f64::NAN), (HEDGE.into(), -1e300)],
                bar_views: vec![
                    (MAKER.into(), std::sync::Arc::new(vec![poison_bar(now)])),
                    (HEDGE.into(), std::sync::Arc::new(vec![poison_bar(now)])),
                ],
                leg_values: Some(std::sync::Arc::from(vec![
                    (MAKER.to_string(), vike_model::ContractValue::linear(f64::NAN), None, None),
                    (HEDGE.to_string(), vike_model::ContractValue::linear(f64::NAN), None, None),
                ])),
                position: f64::NAN,
                average_price: 0.0,
                price: -1e300,
                equity: f64::NAN,
                bars: std::sync::Arc::new(Vec::new()),
                index: 0,
                now,
                contract_value: vike_model::ContractValue::linear(f64::NAN),
                quote_rate: f64::NAN,
                lot_size: f64::NAN,
                tick_size: f64::NAN,
                min_qty: f64::NAN,
                min_notional: f64::NAN,
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                order_lists: Vec::new(),
                combos: Vec::new(),
                order_emulation_arms: Vec::new(),
                mass_cancel: false,
                forecasts: Vec::new(),
            }
        }

        // --- G-1: THE STRUCTURAL PIN --------------------------------------------------------------------

        /// The maker must read NO inventory, price or equity through the `Broker`/`HftBroker` seam.
        ///
        /// This is not stylistic, and ⚠ the reason CHANGED rather than went away. Two of the three read
        /// paths this doc used to name — `declared_views` resolving a declared leg against the MOUNT's
        /// engine, and `dispatch_applied_fills` (the hook a hedge fires from) building its broker with EMPTY
        /// per-symbol tables — were live-runtime defects, and both are fixed
        /// (`crates/vike-core/tests/wiring/multi_symbol_reads.rs`). The third is intrinsic: `HftBroker::position`
        /// takes NO symbol and returns the DISPATCHING engine's scalar, so on a hedge-fill dispatch it
        /// confidently describes the wrong book. And no broker read of any shape can see a sent-but-unacked
        /// hedge, which is the quantity `crates/vike-market-making/src/xemm/hedge.rs`'s `HedgeLedger` computes a safe
        /// retry from.
        ///
        /// Driving the whole hook surface against a poisoned broker and getting BIT-IDENTICAL output is the
        /// pin that catches a future refactor quietly reintroducing one of those reads.
        #[test]
        fn the_maker_never_reads_position_price_or_equity_through_the_broker() {
            fn drive(
                mut b: impl FnMut(i64) -> LiveBroker,
            ) -> Vec<(Option<String>, Option<f64>, f64)> {
                let mut m = maker().with_skew(0.0, 10.0, 0.8);
                m.on_reference_quote(&mut b(1_000), REF_VENUE, &ref_q(1_000, 100.0, 100.5));
                m.on_quote_tick(&mut b(1_000), &own_q(1_000, 100.0, 100.02));
                m.on_fill(&mut b(1_100), &fill_of(MAKER, 1, 3.0, 1_100));
                m.on_trade_tick(
                    &mut b(1_200),
                    &TickTrade {
                        timestamp: 1_200,
                        local_timestamp: 0,
                        price: 100.1,
                        size: 1.0,
                        is_buyer_maker: false,
                        symbol: MAKER.into(),
                    },
                );
                let mut last = b(1_300);
                m.on_reference_quote(&mut last, REF_VENUE, &ref_q(1_300, 100.2, 100.7));
                // everything the maker asked the venue for, in order.
                let mut out: Vec<(Option<String>, Option<f64>, f64)> =
                    last.submissions.iter().map(|s| (s.tag.clone(), s.price, s.qty)).collect();
                out.extend(
                    last.modifications
                        .iter()
                        .map(|m| (Some(m.tag.clone()), m.new_price, m.new_qty.unwrap_or(0.0))),
                );
                out
            }

            let clean = drive(|now| broker(0.0, now));
            let dirty = drive(poisoned);
            assert!(
                !clean.is_empty(),
                "precondition: the maker actually emitted something to compare"
            );
            assert_eq!(
                clean.len(),
                dirty.len(),
                "the poisoned broker changed HOW MANY orders were sent — a read leaked in"
            );
            for (c, d) in clean.iter().zip(dirty.iter()) {
                assert_eq!(c.0, d.0, "tag differs");
                assert_eq!(
                    c.1.map(f64::to_bits),
                    d.1.map(f64::to_bits),
                    "PRICE differs under a poisoned broker: the maker read the broker's price/position"
                );
                assert_eq!(
                    c.2.to_bits(),
                    d.2.to_bits(),
                    "SIZE differs: the maker read the broker's position"
                );
            }
        }

        // --- G-2 / G-3 / G-7: freshness ------------------------------------------------------------------

        /// A REFERENCE touch older than `max_ref_age_ms` halts and pulls BOTH sides. This bound is strictly
        /// stronger than a feed-status subscription: it also catches a feed that freezes without ever
        /// reporting a disconnect, which is the failure a socket-liveness check cannot see.
        #[test]
        fn a_stale_reference_touch_pulls_both_sides() {
            let mut m = maker().with_freshness(2_000, 1_000_000, 1_000_000);
            warm(&mut m, 1_000);
            let mut b0 = broker(0.0, 1_000);
            m.on_quote_tick(&mut b0, &own_q(1_000, 100.0, 100.02));
            assert!(submitted(&b0, "bid") || modified(&b0, "bid"), "precondition: quoting");
            // the maker venue keeps ticking; the reference does not.
            let mut b = broker(0.0, 4_000);
            m.on_quote_tick(&mut b, &own_q(4_000, 100.0, 100.02));
            assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale));
            assert!(canceled(&b, "bid") && canceled(&b, "ask"), "both quotes come off");
        }

        /// A stale OWN touch is just as fatal: `passive_clamp` anchors on it, so without a fresh one the
        /// maker cannot prove its quote is non-marketable — and an unprovable quote is not emitted.
        #[test]
        fn a_stale_own_touch_pulls_both_sides() {
            let mut m = maker().with_freshness(1_000_000, 2_000, 1_000_000);
            warm(&mut m, 1_000);
            // the reference keeps ticking; the maker venue's own book does not.
            let mut b = broker(0.0, 4_000);
            m.on_reference_quote(&mut b, REF_VENUE, &ref_q(4_000, 100.0, 100.5));
            assert_eq!(m.halt_reason(), Some(HaltReason::OwnTouchStale));
            assert!(canceled(&b, "bid") && canceled(&b, "ask"));
        }

        /// The ALL-LANES-QUIET watchdog. When BOTH venues go silent no market lane fires at all, so the
        /// per-venue age bounds cannot speak. The wall-clock `on_schedule` sweep is the one lane that still
        /// runs — and it takes the quotes off.
        #[test]
        fn a_total_feed_outage_is_caught_by_the_schedule_sweep() {
            let mut m = maker().with_freshness(1_000_000, 1_000_000, 5_000);
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 20_000);
            m.on_schedule(&mut b, "sweep");
            assert_eq!(
                m.halt_reason(),
                Some(HaltReason::FeedImpaired),
                "nothing has ticked for 19s"
            );
            assert!(canceled(&b, "bid") && canceled(&b, "ask"));
        }

        /// A non-`Live` feed status on the MAKER venue halts and pulls immediately — no need to wait for an
        /// age bound when the data layer has already said the stream is down.
        #[test]
        fn a_dead_maker_feed_halts_and_pulls() {
            for status in [FeedStatus::Disconnected, FeedStatus::Stale] {
                let mut m = maker();
                warm(&mut m, 1_000);
                let mut b = broker(0.0, 1_100);
                m.on_feed_status(&mut b, status);
                assert_eq!(m.halt_reason(), Some(HaltReason::FeedImpaired), "{status:?}");
                assert!(
                    canceled(&b, "bid") && canceled(&b, "ask"),
                    "{status:?}: both quotes come off"
                );
            }
            // ...and a `Live` status is inert: it never places, cancels or un-halts anything.
            let mut m = maker();
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_100);
            m.on_feed_status(&mut b, FeedStatus::Live);
            assert_eq!(m.halt_reason(), None);
            assert!(
                b.cancels.is_empty() && b.submissions.is_empty(),
                "a Live status emits nothing"
            );
        }

        // --- G-5 / G-6: the naked bands ------------------------------------------------------------------

        /// The SOFT band withholds the side that would GROW the exposure and leaves the reducing side
        /// quoting, so the maker trades its way back toward flat rather than stopping dead.
        #[test]
        fn the_soft_naked_band_suppresses_only_the_growing_side() {
            // hedging off (ratio 0) so the exposure persists and the band is what is under test.
            let mut m = maker().with_naked_bands(2.0, 1e9);
            m.apply_params(&XemmParams { hedge_ratio: 0.0, ..m.params() });
            warm(&mut m, 1_000);
            // Accumulate +3 on the maker leg: net LONG past the 2.0 band ⇒ stop BUYING. The reaction lands
            // in the FILL's own dispatch — `on_fill` re-quotes immediately rather than waiting for the next
            // market tick, because the exposure it just created is what the band is measuring.
            let mut b = broker(0.0, 1_100);
            m.on_fill(&mut b, &fill_of(MAKER, 1, 3.0, 1_100));
            assert!(canceled(&b, "bid"), "long past the band ⇒ the BID is pulled");
            assert!(!canceled(&b, "ask"), "the reducing side keeps quoting: {:?}", b.cancels);
            assert!(modified(&b, "ask") || submitted(&b, "ask"), "and is actively maintained");
            // ...and it STAYS withheld on subsequent ticks (nothing re-places it while the band holds).
            let mut b2 = broker(0.0, 1_200);
            m.on_quote_tick(&mut b2, &own_q(1_200, 100.0, 100.02));
            assert!(!submitted(&b2, "bid") && !modified(&b2, "bid"), "the bid stays off the book");
            assert!(modified(&b2, "ask"), "while the ask keeps being maintained");
        }

        /// The HARD band halts, pulls, and fires a `taker_flatten` impulse for the EXCESS over the soft
        /// band — the flatten-the-excess law, matched exactly against the pure helper so the two can never
        /// disagree about how much to cut.
        #[test]
        fn the_hard_naked_band_halts_and_flattens_the_excess() {
            let mut m = maker().with_naked_bands(2.0, 4.0);
            m.apply_params(&XemmParams { hedge_ratio: 0.0, ..m.params() });
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_100);
            m.on_fill(&mut b, &fill_of(MAKER, 1, 6.0, 1_100)); // 6 naked, hard band 4

            assert_eq!(m.halt_reason(), Some(HaltReason::NakedHardBand));
            assert!(canceled(&b, "bid") && canceled(&b, "ask"), "both quotes come off");
            let expected = vike_market_making::taker_flatten::taker_flatten(6.0, 2.0)
                .expect("over the soft band");
            let flat: Vec<_> = b
                .submissions
                .iter()
                .filter(|s| s.tag.is_none())
                .map(|s| (s.symbol.clone(), s.side, s.qty))
                .collect();
            assert_eq!(
                flat,
                vec![(Some(HEDGE.to_string()), -1, expected.1)],
                "one flatten market on the HEDGE leg, cutting the excess over the SOFT band"
            );
            // FIRE-ONCE: repeated ticks while halted must not spam the taker venue.
            let mut b2 = broker(0.0, 1_200);
            m.on_schedule(&mut b2, "sweep");
            assert!(
                b2.submissions.iter().all(|s| s.tag.is_some()),
                "the flatten impulse fires once per halt, not once per tick: {:?}",
                submits_dbg(&b2)
            );
        }

        // --- G-8 / G-9: the model guard and the symptom guard --------------------------------------------

        /// A basis outside the band halts: it means the pair has decoupled, the mount names the wrong
        /// instrument, or a feed is lying. One number catches all three, and a halt is the only honest
        /// response to "I do not know which".
        #[test]
        fn a_basis_outside_the_band_halts_and_pulls() {
            // A 10 ms half-life so ONE observation dominates the estimate — this test is about the BAND,
            // not about how fast the EWMA converges (that is pinned in `basis.rs`'s own tests).
            let mut m = maker().with_basis_band(50.0, 10, 0.5);
            warm(&mut m, 1_000);
            // the maker venue re-prices 2% below the reference — ~220 bp, far outside the 50 bp band.
            let mut b = broker(0.0, 1_100);
            m.on_quote_tick(&mut b, &own_q(1_100, 98.0, 98.02));
            assert_eq!(m.halt_reason(), Some(HaltReason::BasisOutOfBand));
            assert!(canceled(&b, "bid") && canceled(&b, "ask"));
            assert!(
                m.basis().is_some_and(|v| v < -0.01),
                "the estimate is published: {:?}",
                m.basis()
            );
        }

        /// The one-sided-fill breaker fires on the SYMPTOM — repeated same-side maker fills — regardless of
        /// whether the pricing model is right, which is what bounds the cost of a wrong assumption. It
        /// suppresses ONE side; the other keeps quoting. (Mirrors `tests/breaker.rs` for `SpreadMaker`.)
        #[test]
        fn one_sided_maker_flow_trips_the_breaker() {
            let mut m = maker().with_fill_breaker(1_000, 2.5, 5_000);
            m.apply_params(&XemmParams { hedge_ratio: 0.0, ..m.params() });
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_010);
            for t in [1_010, 1_020, 1_030] {
                b = broker(0.0, t);
                m.on_fill(&mut b, &fill_of(MAKER, 1, 1.0, t));
            }
            assert!(canceled(&b, "bid"), "3 net bid fills in the window suppress the BID");
            assert!(!canceled(&b, "ask"), "the ask side is untouched: {:?}", b.cancels);
            // and it stays withheld for the cooldown.
            let mut b2 = broker(0.0, 1_040);
            m.on_quote_tick(&mut b2, &own_q(1_040, 100.0, 100.02));
            assert!(!submitted(&b2, "bid") && !modified(&b2, "bid"), "still suppressed");
            assert!(modified(&b2, "ask"), "the other side keeps quoting throughout");
        }

        /// HEDGE-leg fills must NOT feed the breaker: they are the strategy's own mechanical offset, so
        /// netting them would cancel every real one-sided signal to exactly zero and the guard would never
        /// fire. (Same three maker fills as above, but each hedged — the breaker must still trip.)
        #[test]
        fn hedge_fills_do_not_cancel_the_breakers_signal() {
            let mut m = maker().with_fill_breaker(1_000, 2.5, 5_000);
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_010);
            for t in [1_010, 1_020, 1_030] {
                b = broker(0.0, t);
                m.on_fill(&mut b, &fill_of(MAKER, 1, 1.0, t));
                m.on_fill(&mut broker(0.0, t), &fill_of(HEDGE, -1, 1.0, t)); // perfectly hedged
            }
            assert_eq!(
                m.naked_exposure().to_bits(),
                0.0_f64.to_bits(),
                "precondition: fully hedged"
            );
            assert!(
                canceled(&b, "bid"),
                "the breaker must see the MAKER flow only — folding the hedges in would net it to zero"
            );
        }

        // --- G-10 / G-11 / G-12: the halt latch and the off-path controls --------------------------------

        /// A halt LATCHES: nothing is placed until it clears, and with `resume_after_halt_ms == 0` (the
        /// default) it never clears on its own. A restart-by-accident must not be the way a halted maker
        /// resumes.
        #[test]
        fn a_halt_latches_and_never_auto_resumes_by_default() {
            let mut m = maker().with_freshness(2_000, 1_000_000, 1_000_000);
            warm(&mut m, 1_000);
            m.on_quote_tick(&mut broker(0.0, 5_000), &own_q(5_000, 100.0, 100.02)); // reference stale
            assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale));
            // even with BOTH feeds healthy again, the latch holds.
            let mut b = broker(0.0, 6_000);
            m.on_reference_quote(&mut b, REF_VENUE, &ref_q(6_000, 100.0, 100.5));
            m.on_quote_tick(&mut b, &own_q(6_000, 100.0, 100.02));
            assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale), "the latch holds");
            assert!(b.submissions.is_empty(), "and nothing is placed: {:?}", submits_dbg(&b));
        }

        /// An operator who opts into auto-resume gets it — and the maker quotes again once the feeds are
        /// genuinely healthy. This is the control that proves the latch above is the DEFAULT, not the only
        /// behaviour.
        #[test]
        fn an_opted_in_resume_window_lets_the_maker_quote_again() {
            let mut m =
                maker().with_freshness(2_000, 1_000_000, 1_000_000).with_resume_after(1_000);
            warm(&mut m, 1_000);
            m.on_quote_tick(&mut broker(0.0, 5_000), &own_q(5_000, 100.0, 100.02));
            assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale));
            let mut b = broker(0.0, 7_000);
            m.on_reference_quote(&mut b, REF_VENUE, &ref_q(7_000, 100.0, 100.5));
            m.on_quote_tick(&mut b, &own_q(7_000, 100.0, 100.02));
            assert_eq!(m.halt_reason(), None, "the resume window expired and both feeds are fresh");
            assert!(submitted(&b, "bid") && submitted(&b, "ask"), "and it quotes again");
        }

        /// OFF-PATH CONTROL: with NO reference touch the maker places nothing at all — it never falls back
        /// to pricing off the venue it rests on, which would not be a cross-exchange maker but an unpriced
        /// one.
        #[test]
        fn no_reference_quote_means_no_orders() {
            let mut m = maker();
            let mut b = broker(0.0, 1_000);
            for ts in [1_000, 2_000, 3_000] {
                m.on_quote_tick(&mut b, &own_q(ts, 100.0, 100.02));
            }
            assert!(
                b.submissions.is_empty(),
                "a maker with no reference quotes nothing: {:?}",
                submits_dbg(&b)
            );
            assert_eq!(
                m.halt_reason(),
                None,
                "a cold start is not a FAULT — nobody should be paged for it"
            );
        }

        /// OFF-PATH CONTROL: the maker never uses `mass_cancel`. In the live runtime it is scoped to the
        /// DISPATCHING series, so from a hedge-fill dispatch it would target the taker venue and cancel
        /// nothing on the maker venue — a pull that silently does not pull.
        #[test]
        fn the_maker_never_mass_cancels() {
            let mut m = maker().with_freshness(2_000, 2_000, 2_000);
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 9_000);
            m.on_schedule(&mut b, "sweep"); // drives every guard into a halt + pull
            m.on_fill(&mut b, &fill_of(MAKER, 1, 1.0, 9_000));
            m.on_fill(&mut b, &fill_of(HEDGE, -1, 1.0, 9_000));
            assert!(!b.mass_cancel, "pulls are per-tag; mass_cancel is scoped to the wrong series");
        }

        // --- G-13 / G-14 / G-15: the emission invariants carried over from SpreadMaker --------------------

        /// A PULL is never tolerance-gated. Even with a tolerance so wide that no re-price would ever be
        /// sent, a quote that must come off the book comes off.
        #[test]
        fn a_pull_is_never_tolerance_gated() {
            let mut m = maker()
                .with_refresh_tolerance(10_000.0, 10_000.0)
                .with_freshness(2_000, 1e9 as i64, 1e9 as i64);
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 5_000);
            m.on_quote_tick(&mut b, &own_q(5_000, 100.0, 100.02));
            assert!(
                canceled(&b, "bid") && canceled(&b, "ask"),
                "a 100% tolerance cannot strand a quote"
            );
        }

        /// A FILL invalidates that side's snapshot so the size TOP-UP is re-issued. `SideState::own` is the
        /// maker's INTENDED quote, not the venue-side remainder, so after a PARTIAL fill an
        /// intended-vs-target compare reads "no change" and the top-up would be silently lost — the maker
        /// would quote less size than configured, forever, with an anti-churn tolerance configured.
        ///
        /// ⚠ The top-up lands in the FILL's OWN dispatch, not on the next market tick: unlike `SpreadMaker`
        /// (whose `on_fill` only sets the flag), this maker re-quotes inside `on_fill` because that is
        /// where it also fires the hedge. So the flag is set and consumed in the same call, and the
        /// following tick is legitimately skipped again.
        #[test]
        fn a_partial_fill_re_issues_the_top_up_in_the_fills_own_dispatch() {
            let mut m = maker().with_refresh_tolerance(10_000.0, 10_000.0);
            warm(&mut m, 1_000);
            // an identical tick IS skipped entirely by the huge tolerance...
            let mut quiet = broker(0.0, 1_100);
            m.on_quote_tick(&mut quiet, &own_q(1_100, 100.0, 100.02));
            assert!(
                !modified(&quiet, "bid"),
                "precondition: the tolerance does skip an unchanged re-quote"
            );
            // ...but a fill on that side is not.
            let mut b = broker(0.0, 1_200);
            m.on_fill(&mut b, &fill_of(MAKER, 1, 0.4, 1_200));
            assert!(
                modified(&b, "bid"),
                "the filled side is re-issued at its full configured size"
            );
            assert_eq!(
                b.modifications
                    .iter()
                    .find(|m| m.tag == "bid")
                    .and_then(|m| m.new_qty)
                    .map(f64::to_bits),
                Some(1.0_f64.to_bits()),
                "and the size is the CONFIGURED one, not the post-fill remainder"
            );
        }

        /// A REFERENCE move re-prices IN PLACE — modify, never cancel/replace — so the venue queue position
        /// survives. This is also the proof that the reference lane is the maker's primary re-quote clock:
        /// venue A never ticked between the two assertions.
        #[test]
        fn a_reference_move_re_prices_in_place() {
            let mut m = maker();
            warm(&mut m, 1_000);
            let before = {
                let mut b = broker(0.0, 1_000);
                m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_000, 100.0, 100.5));
                b
            };
            assert!(
                modified(&before, "bid") || submitted(&before, "bid"),
                "precondition: a bid is resting"
            );
            let mut b = broker(0.0, 1_100);
            m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_100, 99.0, 99.5));
            assert!(
                modified(&b, "bid") && modified(&b, "ask"),
                "both sides re-price off B's move alone"
            );
            assert!(
                b.cancels.is_empty(),
                "never cancel/replace — queue position is the maker's asset"
            );
            assert!(b.submissions.is_empty(), "and nothing is re-submitted");
        }
    }

    mod xemm_hedge {
        //! The xEMM HEDGE lane — the #997 regression surface, and the most dangerous code in the crate.
        //!
        //! A maker fill on venue A creates a DIRECTIONAL exposure that only an order on venue B can close.
        //! Every test here pins one way that can go wrong in a way no other test would catch:
        //!
        //! - the hedge must NAME its symbol and carry NO tag, because the runtime resolves a declared leg's
        //!   venue BY SYMBOL and a tagged submit carries `symbol: None` — a tagged hedge would be placed on
        //!   the MAKER venue, DOUBLING the exposure it was sent to close;
        //! - a hedge-leg fill drains on the TAKER venue's series, so a tagged verb from that lane looks up
        //!   the wrong tag-registry key and silently leaves a real quote resting on the maker venue;
        //! - a retry must re-send the RESIDUAL, not a remembered delta, or a late original fill doubles the
        //!   position while the maker believes it is flat.

        use super::*;

        /// Every UNTAGGED submission as `(symbol, side, qty)` — the hedge/flatten orders. Tagged ones are
        /// the maker's own resting quotes and are excluded.
        fn hedges(b: &LiveBroker) -> Vec<(Option<String>, i32, f64)> {
            b.submissions
                .iter()
                .filter(|s| s.tag.is_none())
                .map(|s| (s.symbol.clone(), s.side, s.qty))
                .collect()
        }

        // --- H-1 / H-5: the hedge-leg lane may touch NOTHING but the symbol-carrying market verb --------

        /// THE SINGLE MOST IMPORTANT TEST IN THE FILE. A fill on the HEDGE leg is dispatched — and drained
        /// — on the TAKER venue's series. From there:
        ///
        /// - a TAGGED verb keys `{mount}|TAKER_VENUE|HEDGE_SYM|bid`, which the maker's own quotes were
        ///   never registered under, so `cancel_tagged` becomes a silent no-op that leaves a REAL order
        ///   resting on the maker venue;
        /// - a SYMBOL-LESS submit resolves to the hedge symbol and is placed on the TAKER venue.
        ///
        /// So this lane may emit exactly one shape: a symbol-carrying, untagged market order.
        #[test]
        fn a_hedge_leg_fill_emits_only_symbol_carrying_untagged_orders() {
            let mut m = maker();
            warm(&mut m, 1_000);
            // A maker BUY of 5 owes a hedge SELL of 5; let it fire, then partially fill it.
            m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100));
            let mut b = broker(0.0, 1_200);
            m.on_fill(&mut b, &fill_of(HEDGE, -1, 2.0, 1_200));

            assert!(
                b.cancels.is_empty(),
                "a hedge-leg fill must never cancel a tagged quote: {:?}",
                b.cancels
            );
            assert!(
                b.modifications.is_empty(),
                "a hedge-leg fill must never re-price a tagged quote: {:?}",
                modifies_dbg(&b)
            );
            for s in &b.submissions {
                assert!(
                    s.tag.is_none(),
                    "a hedge-leg fill buffered a TAGGED submission ({:?}) — it would be placed on the \
                     taker venue and be uncancellable from the maker lane",
                    s.tag
                );
                assert_eq!(
                    s.symbol.as_deref(),
                    Some(HEDGE),
                    "every order from the hedge lane must NAME the hedge symbol; a symbol-less one \
                     resolves to whatever series dispatched it"
                );
            }
        }

        // --- H-2: a maker fill fires exactly one hedge, correctly sized and sided ------------------------

        /// The core claim: one maker fill ⇒ one market order on the HEDGE symbol, opposite side, sized
        /// bit-for-bit by `hedge_quantity(size, ratio)`.
        #[test]
        fn a_maker_fill_fires_exactly_one_market_hedge_naming_the_hedge_symbol() {
            let mut m = maker();
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_100);
            m.on_fill(&mut b, &fill_of(MAKER, 1, 5.0, 1_100));

            let h = hedges(&b);
            assert_eq!(h.len(), 1, "exactly one hedge order, got {h:?}");
            let (symbol, side, qty) = &h[0];
            assert_eq!(
                symbol.as_deref(),
                Some(HEDGE),
                "the hedge NAMES its leg — that is what routes it"
            );
            assert_eq!(*side, -1, "a maker BUY is hedged by a taker SELL");
            assert_eq!(qty.to_bits(), 5.0_f64.to_bits(), "fully hedged at ratio 1.0, bit-for-bit");
            assert_eq!(m.leg_positions(), (5.0, 0.0), "the ledger books the maker leg immediately");
            assert_eq!(
                m.naked_exposure().to_bits(),
                5.0_f64.to_bits(),
                "and reports it as naked until B fills"
            );
            // the mirror: a maker SELL is hedged by a taker BUY.
            let mut m2 = maker();
            warm(&mut m2, 1_000);
            let mut b2 = broker(0.0, 1_100);
            m2.on_fill(&mut b2, &fill_of(MAKER, -1, 2.5, 1_100));
            assert_eq!(hedges(&b2), vec![(Some(HEDGE.to_string()), 1, 2.5)]);
        }

        /// A declared-mount broker whose legs are worth `maker` and `hedge` a unit (`OrderBroker::contract_value`).
        fn valued(
            now: i64,
            maker: vike_model::ContractValue,
            hedge: vike_model::ContractValue,
        ) -> LiveBroker {
            LiveBroker {
                bar_views: vec![
                    (MAKER.into(), std::sync::Arc::new(Vec::new())),
                    (HEDGE.into(), std::sync::Arc::new(Vec::new())),
                ],
                leg_values: Some(std::sync::Arc::from(vec![
                    (MAKER.to_string(), maker, None, None),
                    (HEDGE.to_string(), hedge, None, None),
                ])),
                ..broker(0.0, now)
            }
        }

        /// THE UNIT BUG (wave-2 audit): the maker leg is a Deribit inverse perpetual (a contract is ONE
        /// DOLLAR) and the hedge leg linear BTC. A maker buy of 10 contracts is 10 dollars, hedged by
        /// `10 / 100.25` BTC at the reference mid; it was hedged by 10 BTC, 100 times the exposure here and
        /// 65,000 times at a real price. The hedge's fill nets the exposure to zero, in maker units.
        #[test]
        fn a_hedge_is_sized_in_the_hedge_legs_units_by_value() {
            use vike_model::ContractValue;
            let (deribit, btc) = (ContractValue::inverse(1.0), ContractValue::linear(1.0));
            let mut m = maker();
            m.on_reference_quote(
                &mut valued(1_000, deribit, btc),
                REF_VENUE,
                &ref_q(1_000, 100.0, 100.5),
            );
            m.on_quote_tick(&mut valued(1_000, deribit, btc), &own_q(1_000, 100.0, 100.02));
            let mut b = valued(1_100, deribit, btc);
            m.on_fill(&mut b, &fill_of(MAKER, 1, 10.0, 1_100));
            let want = 10.0 / 100.25;
            assert_eq!(
                hedges(&b),
                vec![(Some(HEDGE.to_string()), -1, want)],
                "10 dollars, not 10 BTC"
            );
            assert_eq!(m.naked_exposure(), 10.0, "10 dollars naked until the hedge fills");
            m.on_fill(&mut valued(1_200, deribit, btc), &fill_of(HEDGE, -1, want, 1_200));
            assert!(m.naked_exposure().abs() < 1e-9, "{}", m.naked_exposure());
            // two legs of one value hedge unit for unit, bit for bit
            let mut same = maker();
            same.on_reference_quote(
                &mut valued(1_000, btc, btc),
                REF_VENUE,
                &ref_q(1_000, 100.0, 100.5),
            );
            let mut b = valued(1_100, btc, btc);
            same.on_fill(&mut b, &fill_of(MAKER, 1, 10.0, 1_100));
            assert_eq!(hedges(&b), vec![(Some(HEDGE.to_string()), -1, 10.0)]);
        }

        // --- H-3: partial hedge fills leave only the residual -------------------------------------------

        /// A partially-filled hedge owes the REMAINDER on its next send, and nothing once it completes.
        #[test]
        fn a_partial_hedge_fill_leaves_only_the_residual_owed() {
            let mut m = maker();
            warm(&mut m, 1_000);
            m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100)); // fires SELL 5
            m.on_fill(&mut broker(0.0, 1_200), &fill_of(HEDGE, -1, 2.0, 1_200)); // 2 of 5 land
            assert_eq!(m.naked_exposure().to_bits(), 3.0_f64.to_bits(), "3 still naked");
            // the retry (after the timeout) sends exactly the 3 that remain — never the original 5.
            let mut b = broker(0.0, 10_000);
            m.on_schedule(&mut b, "sweep");
            let h = hedges(&b);
            assert_eq!(
                h,
                vec![(Some(HEDGE.to_string()), -1, 3.0)],
                "the retry re-sends the RESIDUAL"
            );
            // completing it settles: nothing further is sent, ever.
            m.on_fill(&mut broker(0.0, 10_100), &fill_of(HEDGE, -1, 3.0, 10_100));
            let mut b2 = broker(0.0, 20_000);
            m.on_schedule(&mut b2, "sweep");
            assert!(hedges(&b2).is_empty(), "a settled hedge sends nothing: {:?}", hedges(&b2));
            assert_eq!(m.naked_exposure().to_bits(), 0.0_f64.to_bits(), "and reports flat");
        }

        // --- H-4: THE TARGET-NOT-DELTA LAW --------------------------------------------------------------

        /// Fire, time out, retry — and THEN have the ORIGINAL land late. A delta ledger would have
        /// decremented once and sent twice, doubling the position while reporting flat. Because the residual
        /// is DERIVED from the two legs' positions, the late fill shrinks it and the retry cannot double
        /// anything.
        #[test]
        fn a_hedge_retry_never_doubles_the_position() {
            let mut m = maker();
            warm(&mut m, 1_000);
            let mut b0 = broker(0.0, 1_100);
            m.on_fill(&mut b0, &fill_of(MAKER, 1, 5.0, 1_100));
            assert_eq!(hedges(&b0), vec![(Some(HEDGE.to_string()), -1, 5.0)], "fire #1");

            // the ack never comes; the safety sweep re-fires the same residual after the timeout.
            let mut b1 = broker(0.0, 10_000);
            m.on_schedule(&mut b1, "sweep");
            assert_eq!(
                hedges(&b1),
                vec![(Some(HEDGE.to_string()), -1, 5.0)],
                "fire #2 — same residual"
            );

            // NOW the original lands, late.
            m.on_fill(&mut broker(0.0, 10_100), &fill_of(HEDGE, -1, 5.0, 10_100));
            assert_eq!(
                m.naked_exposure().to_bits(),
                0.0_f64.to_bits(),
                "the late fill settles the target"
            );

            // a third sweep sends NOTHING: there is no third order to double with.
            let mut b2 = broker(0.0, 20_000);
            m.on_schedule(&mut b2, "sweep");
            assert!(
                hedges(&b2).is_empty(),
                "a settled ledger must send nothing — a delta ledger would still owe 5 here: {:?}",
                hedges(&b2)
            );

            // and if the RETRY's own fill also lands, the over-hedge is owed BACK rather than ignored.
            m.on_fill(&mut broker(0.0, 20_100), &fill_of(HEDGE, -1, 5.0, 20_100));
            let mut b3 = broker(0.0, 30_000);
            m.on_schedule(&mut b3, "sweep");
            assert_eq!(
                hedges(&b3),
                vec![(Some(HEDGE.to_string()), 1, 5.0)],
                "an over-hedge is unwound, not silently carried"
            );
        }

        // --- H-6: an unattributable fill fails CLOSED ---------------------------------------------------

        /// A fill for NEITHER leg means the maker can no longer account for its own inventory. It halts —
        /// but emits NOTHING from that hook, because the dispatch series of an unattributable fill is
        /// unknown and a tagged cancel from the wrong series is a silent no-op. The pull lands on the next
        /// maker-venue lane.
        #[test]
        fn a_fill_for_neither_leg_halts_and_the_next_tick_pulls() {
            let mut m = maker();
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_100);
            assert!(submitted(&b, "bid") || m.halt_reason().is_none(), "precondition: running");
            m.on_fill(&mut b, &fill_of("DOGEUSDT", 1, 1.0, 1_100));
            assert_eq!(m.halt_reason(), Some(HaltReason::UnknownFillSymbol));
            assert!(
                b.submissions.is_empty() && b.cancels.is_empty(),
                "the unknown-fill hook emits nothing"
            );
            // the next maker-venue tick takes both quotes off.
            let mut b2 = broker(0.0, 1_200);
            m.on_quote_tick(&mut b2, &own_q(1_200, 100.0, 100.02));
            assert!(canceled(&b2, "bid") && canceled(&b2, "ask"), "the halt pulls both sides");
        }

        /// A fill with an EMPTY symbol is the MAKER leg, following the `""` means "my mount" convention the
        /// order verbs use. Live fills always carry a real symbol, so this only affects test/paper doubles —
        /// but reading it as "unknown" would halt every paper rehearsal.
        #[test]
        fn an_empty_fill_symbol_is_the_maker_leg() {
            let mut m = maker();
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_100);
            m.on_fill(&mut b, &fill_of("", 1, 5.0, 1_100));
            assert_eq!(m.halt_reason(), None, "an empty symbol is not an unknown one");
            assert_eq!(
                hedges(&b),
                vec![(Some(HEDGE.to_string()), -1, 5.0)],
                "and it hedges normally"
            );
        }

        // --- H-7 / H-8: the two ways hedging stops -------------------------------------------------------

        /// `hedge_ratio == 0.0` is the deliberately-unhedged configuration: it fires NO order rather than a
        /// zero-size one. The exposure is still fully visible, and the naked bands still measure it.
        #[test]
        fn a_zero_hedge_ratio_fires_nothing_but_still_reports_the_exposure() {
            let mut m = maker().with_naked_bands(1e9, 1e9);
            m.apply_params(&vike_model::XemmParams { hedge_ratio: 0.0, ..m.params() });
            warm(&mut m, 1_000);
            let mut b = broker(0.0, 1_100);
            m.on_fill(&mut b, &fill_of(MAKER, 1, 5.0, 1_100));
            assert!(hedges(&b).is_empty(), "ratio 0 ⇒ no hedge order: {:?}", hedges(&b));
            assert_eq!(
                m.naked_exposure().to_bits(),
                5.0_f64.to_bits(),
                "the exposure is fully visible"
            );
        }

        /// A residual that survives its whole retry budget is a REAL open position the taker venue has
        /// refused to close. The maker halts (`HedgeUnfilled`) and pulls both quotes rather than continuing
        /// to accumulate on top of it.
        #[test]
        fn attempt_exhaustion_halts_and_pulls() {
            // Freshness opened wide: this test drives the clock far ahead to expire the hedge timeout, and
            // a stale-feed halt would mask the `HedgeUnfilled` one under test.
            let mut m = maker().with_hedge_discipline(1_000, 2, 0.0).with_freshness(0, 0, 0);
            warm(&mut m, 1_000);
            m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100)); // send #1
            let mut b1 = broker(0.0, 5_000);
            m.on_schedule(&mut b1, "sweep"); // send #2 — budget now spent
            assert_eq!(hedges(&b1).len(), 1, "the second send happens");
            assert_eq!(m.halt_reason(), None, "and does not itself halt");

            let mut b2 = broker(0.0, 9_000);
            m.on_schedule(&mut b2, "sweep");
            assert!(hedges(&b2).is_empty(), "no third send: the budget is spent");
            assert_eq!(m.halt_reason(), Some(HaltReason::HedgeUnfilled));
            assert!(canceled(&b2, "bid") && canceled(&b2, "ask"), "and both quotes come off");
        }
    }

    mod xemm_mount {
        //! The xEMM MOUNT surface — genericity, the live-parameter plane, the construction guard, and
        //! durable state.
        //!
        //! These are the properties a runtime depends on rather than a market does: that the maker is not
        //! welded to the concrete live broker, that a hot re-tune cannot silently drop runtime state, that
        //! a same-symbol pair is refused at construction rather than misrouting live orders, and that a
        //! restart cannot un-halt a halted maker or forget an open exposure.

        use super::*;
        use vike_model::OrderBroker;

        // --- M-1: genericity ------------------------------------------------------------------------------

        /// A minimal second `OrderBroker` — enough to prove the maker is written against the TRAIT, not
        /// against `LiveBroker`. Mirrors `live_params.rs`'s `MockHft`.
        #[derive(Default)]
        struct MockHft {
            now: i64,
            submits: Vec<(String, i32, f64, f64)>,
            modifies: Vec<String>,
            cancels: Vec<String>,
            markets: Vec<(String, i32, f64)>,
        }

        /// The order seam the maker drives (decision 0277's step 6): a tagged limit is a `submits` row, an
        /// untagged market a `markets` row (the hedge), an amend a `modifies` row, a cancel a `cancels` row.
        impl OrderBroker for MockHft {
            fn place(
                &mut self,
                symbol: &str,
                tag: &str,
                side: i32,
                qty: f64,
                order: OrderPlacement,
                _reduce_only: bool,
            ) {
                match order {
                    OrderPlacement::Market if tag.is_empty() => {
                        self.markets.push((symbol.to_string(), side, qty));
                    }
                    OrderPlacement::Limit { price } => {
                        self.submits.push((tag.to_string(), side, qty, price))
                    }
                    _ => {}
                }
            }
            fn place_group(
                &mut self,
                _symbol: &str,
                _legs: Vec<(String, vike_model::OrderListLeg)>,
            ) {
            }
            fn amend(
                &mut self,
                tag: &str,
                _quantity: Option<f64>,
                _price: Option<f64>,
                _trigger: Option<f64>,
            ) {
                self.modifies.push(tag.to_string());
            }
            fn cancel(&mut self, tag: &str) {
                self.cancels.push(tag.to_string());
            }
            fn deny(&mut self, _symbol: &str, _tag: &str, _quantity: f64, _reason: &str) {}
            fn average_price(&self, _symbol: &str) -> f64 {
                f64::NAN // poisoned: the maker must never read it
            }
            fn position(&self, _symbol: &str) -> f64 {
                f64::NAN // poisoned: the maker must never read it
            }
            fn price(&self, _symbol: &str) -> f64 {
                f64::NAN
            }
            fn equity(&self) -> f64 {
                f64::NAN
            }
            fn bars(&self, _symbol: &str) -> &[vike_marketdata::Bar] {
                &[]
            }
            fn index(&self) -> usize {
                0
            }
            fn now(&self) -> i64 {
                self.now
            }
        }

        /// The TYPE-CHECK-TIME proof that `XemmMaker` is engine-agnostic: it mounts on a foreign
        /// `OrderBroker` and on the live one. If it ever named a `LiveBroker`-specific verb this stops
        /// compiling — which is the point of asserting it in the type system rather than at runtime.
        #[test]
        fn the_maker_mounts_on_any_hft_broker() {
            assert_mounts_on::<XemmMaker, MockHft>();
        }

        /// ...and BEHAVIOURALLY: driven through a foreign broker it places, re-prices and hedges exactly as
        /// it does through the live one.
        #[test]
        fn the_maker_behaves_identically_on_a_foreign_broker() {
            let mut m = maker();
            let mut b = MockHft { now: 1_000, ..Default::default() };
            m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_000, 100.0, 100.5));
            m.on_quote_tick(&mut b, &own_q(1_000, 100.0, 100.02));
            assert_eq!(b.submits.len(), 2, "a two-sided quote is placed: {:?}", b.submits);
            b.now = 1_100;
            m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_100, 99.0, 99.5));
            assert_eq!(b.modifies.len(), 2, "and re-priced in place off the reference move");
            m.on_fill(&mut b, &fill_of(MAKER, 1, 2.0, 1_100));
            assert_eq!(
                b.markets,
                vec![(HEDGE.to_string(), -1, 2.0)],
                "the hedge is a symbol-carrying market on the hedge leg"
            );
        }

        // --- M-2: the live-parameter plane ----------------------------------------------------------------

        /// A hot re-tune replaces the CONFIG bag and touches NO runtime state — resting orders and their
        /// venue queue position, the hedge ledger, the breaker deadlines, the warm basis estimate and the
        /// halt latch all survive. That safety is by TYPE SHAPE (one struct copy over `config`, which is a
        /// sibling of every runtime field), not by a hand-maintained list of omissions, and this test is
        /// what keeps the shape honest.
        #[test]
        fn apply_params_preserves_every_runtime_state_field() {
            let mut m =
                maker().with_basis_band(1_000.0, 1_000, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
            warm(&mut m, 1_000);
            // A fill UNDER the breaker threshold, so the resting quotes survive to be checked below (a
            // tripped breaker would legitimately pull the bid and the "queue position survived" assertion
            // would be testing nothing).
            m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 1.0, 1_100));
            let (basis_before, legs_before) = (m.basis(), m.leg_positions());
            assert!(basis_before.is_some(), "precondition: the estimator is warm");
            assert_eq!(legs_before, (1.0, 0.0), "precondition: the ledger carries a position");

            let mut b = broker(0.0, 1_200);
            m.on_params_updated(
                &mut b,
                &StrategyParams::Xemm(XemmParams { qty: 7.0, ..m.params() }),
            );

            assert_eq!(m.params().qty.to_bits(), 7.0_f64.to_bits(), "the new tuning landed");
            assert_eq!(m.params_epoch, 1, "and the epoch advanced");
            assert_eq!(
                m.basis().map(f64::to_bits),
                basis_before.map(f64::to_bits),
                "the basis survives"
            );
            assert_eq!(m.leg_positions(), legs_before, "the hedge ledger survives");
            assert!(
                b.submissions.is_empty() && b.modifications.is_empty() && b.cancels.is_empty(),
                "a bare re-tune emits NOTHING — the new knobs take effect on the next tick's in-place \
                 re-price, so the book is not disturbed: {:?}",
                submits_dbg(&b)
            );
            // the resting quotes are still tracked: the next tick MODIFIES rather than re-submitting.
            let mut b2 = broker(0.0, 1_300);
            m.on_quote_tick(&mut b2, &own_q(1_300, 100.0, 100.02));
            assert!(
                modified(&b2, "bid"),
                "the resting bid kept its queue position across the re-tune"
            );
            assert!(b2.submissions.is_empty(), "nothing was re-placed");
        }

        /// A FOREIGN params variant is ignored — the live-params contract. A sibling `SpreadMaker` mount
        /// being re-tuned must not disturb this maker.
        #[test]
        fn a_foreign_params_variant_is_ignored() {
            let mut m = maker();
            let before = m.params();
            m.on_params_updated(
                &mut broker(0.0, 0),
                &StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
                    qty: 999.0,
                    ..super::super::super::spread_maker::SpreadMaker::new(1.0, 0.5).params()
                }),
            );
            assert_eq!(m.params().qty.to_bits(), before.qty.to_bits(), "untouched");
            assert_eq!(m.params_epoch, 0, "and the epoch does not advance");
        }

        // --- M-3: the construction guard -----------------------------------------------------------------

        /// A same-named pair is refused AT CONSTRUCTION. This is not tidiness: the live runtime finds a
        /// declared leg BY ITS NAME, so a leg named like the mount's own symbol would route EVERY intent —
        /// including the symbol-less tagged maker quotes — to the taker venue. It also destroys
        /// `on_fill`'s only leg discriminator (a `Fill` carries the leg's name, not a venue). Both failures
        /// are silent at runtime, so they must be loud here (decision 0293 names the same instrument on
        /// another venue `BTCUSDT@bybit/LABEL`, which differs).
        #[test]
        #[should_panic(expected = "DISTINCT symbols")]
        fn identical_leg_symbols_are_refused_at_construction() {
            let _ = XemmMaker::new("BTCUSDT", "BTCUSDT", 1.0, 0.001, 0.0, 0.01);
        }

        /// The legs are IDENTITY, not config: they are readable but there is no way to re-point them, so a
        /// live re-tune can never move a leg while an unhedged position is open.
        #[test]
        fn the_legs_are_identity_and_not_part_of_the_tunable_bag() {
            let m = maker();
            assert_eq!(m.legs(), (MAKER, HEDGE));
            // `XemmParams` is `Copy` and has no symbol field at all — the compile-time half of the claim.
            let p: XemmParams = m.params();
            let _: XemmParams = p; // Copy, not Clone-with-strings
        }

        // --- M-4: durable state ---------------------------------------------------------------------------

        /// A restart must not un-halt a halted maker nor forget an open, unhedged position — the two facts
        /// that are REAL MONEY rather than a cache. Both round-trip; the tuning deliberately does not (a
        /// restart with a new config must price with the NEW config).
        #[test]
        fn save_state_load_state_round_trips_the_ledger_and_the_halt_latch() {
            let mut m = maker()
                .with_freshness(2_000, 1_000_000, 1_000_000)
                .with_fill_breaker(1_000, 2.5, 5_000);
            warm(&mut m, 1_000);
            // an open, partially-hedged position...
            m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100));
            m.on_fill(&mut broker(0.0, 1_150), &fill_of(HEDGE, -1, 2.0, 1_150));
            // ...and a halt.
            m.on_quote_tick(&mut broker(0.0, 5_000), &own_q(5_000, 100.0, 100.02));
            assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale), "precondition: halted");
            assert_eq!(m.leg_positions(), (5.0, -2.0), "precondition: 3 units naked");

            let saved =
                Strategy::<LiveBroker>::save_state(&m).expect("XemmMaker always saves Some");

            // a FRESH maker, same config — what a restart constructs before load_state.
            let mut m2 = maker()
                .with_freshness(2_000, 1_000_000, 1_000_000)
                .with_fill_breaker(1_000, 2.5, 5_000);
            assert_eq!(m2.halt_reason(), None, "precondition: a fresh maker starts running");
            assert_eq!(m2.leg_positions(), (0.0, 0.0), "precondition: and flat");

            Strategy::<LiveBroker>::load_state(&mut m2, &saved);
            assert_eq!(
                m2.halt_reason(),
                Some(HaltReason::ReferenceStale),
                "a restart cannot un-halt"
            );
            assert_eq!(m2.leg_positions(), (5.0, -2.0), "the open exposure is re-adopted");
            assert_eq!(m2.naked_exposure().to_bits(), 3.0_f64.to_bits());
        }

        /// FAIL-OPEN on a shape this build does not recognise: warn and keep the freshly-constructed state
        /// rather than panicking or half-applying it.
        #[test]
        fn load_state_fails_open_on_a_version_mismatch_or_garbage() {
            for payload in [serde_json::json!({"v": 999}), serde_json::json!("nonsense")] {
                let mut m = maker();
                Strategy::<LiveBroker>::load_state(
                    &mut m,
                    &vike_model::StrategyState::from_json_value(&payload),
                );
                assert_eq!(m.halt_reason(), None, "{payload}: the fresh state is kept");
                assert_eq!(m.leg_positions(), (0.0, 0.0), "{payload}: and the ledger stays flat");
            }
        }

        // --- M-5: the busy refusal (P1) -------------------------------------------------------------------

        /// A place under a tag whose older order still works is refused (P1). The side is ADOPTED: the order
        /// behind the tag is the side's and its terms are unknown, so the next tick re-prices it by tag even
        /// where the anti-churn tolerance would skip the quote the maker MEANT to place (left as the maker
        /// meant it, the older order would keep whatever price it had), and nothing is placed again.
        #[test]
        fn a_busy_refusal_adopts_the_side_and_the_next_tick_amends_it() {
            let mut m = maker().with_refresh_tolerance(10_000.0, 10_000.0);
            warm(&mut m, 1_000);
            let busy = vike_model::OrderEvent {
                order_client_id: "r1".into(),
                tag: Some("bid".into()),
                kind: vike_model::OrderEventKind::Denied {
                    reason: vike_model::tag_busy_reason("bid"),
                },
            };
            m.on_order_event(&mut broker(0.0, 1_050), &busy);

            let mut b = broker(0.0, 1_100);
            m.on_quote_tick(&mut b, &own_q(1_100, 100.0, 100.02));
            assert!(b.submissions.is_empty(), "nothing is placed again: {:?}", submits_dbg(&b));
            assert_eq!(
                modifies_dbg(&b),
                vec!["bid".to_string()],
                "the adopted bid alone is re-priced"
            );
        }

        // --- the TOML reader ------------------------------------------------------------------------------

        /// `from_params` is a READER, not a schema: unknown keys are ignored and missing ones keep the
        /// (safety-ON) default — the `SpreadMaker::from_params` convention.
        #[test]
        fn from_params_reads_the_legs_and_keeps_armed_defaults_for_the_rest() {
            let v: Params = toml::from_str(
                r#"
                maker_symbol = "BTC"
                hedge_symbol = "BTC-USDT-SWAP"
                qty = 2
                min_profitability = 0.0005
                total_fee = 0.00065
                maker_tick_size = 0.5
                max_basis_bps = 40.0
                an_unknown_key = "ignored"
                "#,
            )
            .expect("valid toml");
            let m = XemmMaker::from_params(&v);
            assert_eq!(m.legs(), ("BTC", "BTC-USDT-SWAP"));
            let p = m.params();
            assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "an integer reads as a float");
            assert_eq!(p.max_basis_bps.to_bits(), 40.0_f64.to_bits());
            assert_eq!(p.max_ref_age_ms, 2_000, "an unnamed guard keeps its ARMED default");
            assert_eq!(p.hedge_max_attempts, 3);
            assert_eq!(p.resume_after_halt_ms, 0, "auto-resume stays off");
            assert_eq!(
                p.naked_band.to_bits(),
                2.0_f64.to_bits(),
                "bands derive from qty when unnamed"
            );
            assert_eq!(p.naked_hard_band.to_bits(), 6.0_f64.to_bits());
        }

        /// ONE user name on two venues (binance × bybit `BTCUSDT.P`): the mount binds the hedge to its
        /// account by naming it `BTCUSDT.P@bybit/DEMO` (decision 0293), which the file reads as a leg like
        /// any other and refuses nothing.
        #[test]
        fn a_hedge_named_by_its_account_is_a_distinct_leg() {
            let v: Params = toml::from_str(
                "maker_symbol = \"BTCUSDT.P\"\nhedge_symbol = \"BTCUSDT.P@bybit/DEMO\"",
            )
            .expect("toml");
            assert_eq!(refuse(&v), None);
            assert_eq!(XemmMaker::from_params(&v).legs(), ("BTCUSDT.P", "BTCUSDT.P@bybit/DEMO"));
        }

        /// A profile that names a leg twice is refused for exactly the reason the constructor is.
        #[test]
        #[should_panic(expected = "DISTINCT symbols")]
        fn from_params_refuses_a_same_symbol_pair() {
            let v: Params =
                toml::from_str("maker_symbol = \"BTCUSDT\"\nhedge_symbol = \"BTCUSDT\"")
                    .expect("toml");
            let _ = XemmMaker::from_params(&v);
        }
    }
}
