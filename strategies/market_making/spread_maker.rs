//! The spread maker: a two-sided market maker resting a bid and an ask by tag on its mount's own
//! symbol and re-pricing them in place. It prices with Avellaneda–Stoikov by default;
//! `spread_model = "gueant"` prices the same knobs with the Guéant–Lehalle closed form (the maker
//! that was named `gueant_maker`). The maker itself — its struct, its `Strategy` impl, its quote
//! emission, its saved state and its white-box tests (the `tests` module at the end); the maths it
//! prices with is `vike_market_making`'s.
//!
//! ```toml
//! [strategy]
//! runs_on = "live"
//! why = """\
//!   mountable, but the daemon does NOT resolve it through this arm and that is what makes it safe. \
//!   `SpreadMaker::from_params` reads `[strategy.params]` and NOTHING else, so a mount resolved here \
//!   would run the bare reader's defaults (`qty = 1`, `tick_size = 0`, the Bernoulli [0,1] wall \
//!   clamp) — the configuration that posted ZERO orders on a $64k hyperliquid asset before the crypto \
//!   preset existed. `vike_tradehub::config::ProfileDaemon::mounted_maker` builds it from the \
//!   profile's table over the venue's preset, the mount's own grid and its fee lane's floor — the \
//!   SAME bag the absent-`[strategy]` default path reads — and refuses an unknown or mistyped key \
//!   against `SpreadMaker::PARAM_KEYS`, so the named and default spellings are ONE \
//!   construction that cannot differ. This arm is the BACKTEST's, where `[strategy.params]` is the \
//!   only configuration that exists"""
//! optimistic_quotes = true
//!
//! # Every key `SpreadMaker::from_params` reads (`SpreadMaker::PARAM_KEYS`, below). A mount's
//! # `tick_size`, default preset and fee floor are its facts (`mount_facts` below), not keys.
//! [params]
//! preset = { type = "text" }
//! qty = { type = "number" }
//! tick_size = { type = "number" }
//! half_spread = { type = "number" }
//! gamma = { type = "number" }
//! q_scale = { type = "number" }
//! min_half_spread_ticks = { type = "number" }
//! max_half_spread_ticks = { type = "number" }
//! round_trip_fee_rate = { type = "number" }
//! variance_mode = { type = "text" }
//! horizon_mode = { type = "text" }
//! price_domain = { type = "text" }
//! band_lo = { type = "number" }
//! band_hi = { type = "number" }
//! pricing = { type = "text" }
//! style = { type = "text" }
//! depth_levels = { type = "number" }
//! target_inventory = { type = "number" }
//! max_inventory = { type = "number" }
//! skew = { type = "number" }
//! fill_window_ms = { type = "number" }
//! net_fill_threshold = { type = "number" }
//! suppress_cooldown_ms = { type = "number" }
//! pull_accel_ramp_ms = { type = "number" }
//! pull_accel_max = { type = "number" }
//! kappa_mode = { type = "text" }
//! kappa_default = { type = "number" }
//! kappa_min = { type = "number" }
//! kappa_max = { type = "number" }
//! n_min = { type = "number" }
//! trade_window_ms = { type = "number" }
//! resolution_ts = { type = "number" }
//! tau_hold_ms = { type = "number" }
//! resolution_blackout_ms = { type = "number" }
//! terminal_penalty_gamma = { type = "number" }
//! terminal_ramp_ms = { type = "number" }
//! underlying_weight = { type = "number" }
//! underlying_beta = { type = "number" }
//! window_secs = { type = "number" }
//! atm_blackout_scale = { type = "number" }
//! spread_model = { type = "text", default = "avellaneda_stoikov" }
//! base_intensity_a = { type = "number" }
//! reservation_model = { type = "text" }
//! lmsr_b = { type = "number" }
//! spread_source = { type = "text" }
//! ls_lmsr_alpha = { type = "number" }
//! gm_mu = { type = "number" }
//! alpha_beta_imbalance = { type = "number" }
//! alpha_lambda_ofi = { type = "number" }
//! ofi_decay = { type = "number" }
//! running_penalty_phi = { type = "number" }
//! flatten_by_ms = { type = "number" }
//! flatten_strength = { type = "number" }
//! toxicity_widen = { type = "number" }
//! toxicity_size_cut = { type = "number" }
//! ofi_toxicity_scale = { type = "number" }
//! refresh_price_bps = { type = "number" }
//! refresh_size_bps = { type = "number" }
//! ```
//!
//! It rests its quotes by TAG on the order seam ([`OrderBroker`]: a
//! tagged limit `place`d on the mount's own symbol `""`, `amend`ed in place, `cancel`led, and the
//! signed position read as `position("")`), so it is written `impl<B: OrderBroker> Strategy<B>`
//! and stays engine-agnostic: the live `LiveBroker` (vike-core's runtime) and the simulator's
//! `OrderBrokerSimulator` mount it through the same verbs (decision 0277's step 6). A broker with no order
//! seam cannot mount it, so the type system keeps documenting the requirement (per the
//! write-once rule in `vike_model`'s strategy doc).
//!
//! `SpreadMaker` also carries **inventory-skew size shaping**: it reads its own signed position
//! off the broker (`position("")`) and biases the bid/ask SIZES toward a target inventory (default 0 =
//! stay flat) — quoting a larger ask + smaller bid when long (lean to sell down) and the mirror
//! when short. The shaping is a pure function of position (`skew_multipliers`), unit-tested in
//! isolation; with the skew intensity at its neutral default (`0.0`) both multipliers are exactly
//! `1.0`, reducing to the original fixed-size behavior bit-for-bit.
//!
//! On top of the skew it carries a **per-side fill-rate circuit breaker** (audit mm1) — the
//! adverse-selection guard the plain maker lacked. It overrides [`Strategy::on_fill`](vike_model::Strategy::on_fill) to track
//! recent fills per side over a sliding EVENT-TIME window, and NETS the two sides against each
//! other (round-trip netting): a bid fill later offset by an ask fill is a completed, inventory-
//! neutral round-trip (the desired MM outcome) and must NOT trip the guard — only NET
//! one-directional accumulation on one side does. When the net (this-side minus other-side) fill
//! size within the window trips the threshold, that ONE side is suppressed for a cooldown window:
//! its resting quote is PULLED ([`OrderBroker::cancel`]) and not re-quoted until the
//! cooldown ts expires, while the other side keeps quoting (and re-skewing) normally. The trigger
//! is a pure function of the fill window (`net_signed_fills`), unit-tested in isolation; with the
//! breaker at its neutral defaults (window/threshold/cooldown all `0`) it never engages and the
//! strategy reduces EXACTLY to the skew-only maker above.
//!
//! On top of BOTH of those it carries a **quoting-style registry** + **own-order-book filtration**
//! (audit mm3) — the two gaps the single hardcoded mid ± fixed-spread formula left:
//! - [`QuoteStyle`] (`Top`/`Join`/`Mid`/`Depth`) are PURE functions of the book, selected via
//!   [`SpreadMaker::with_quote_style`]. `Mid` (midpoint ± `half_spread`) is the DEFAULT and reproduces the
//!   original pricing bit-for-bit, so an unconfigured maker is byte-identical to before.
//! - Own-order-book filtration (opt-in, [`SpreadMaker::with_own_order_filtration`]) subtracts the
//!   maker's OWN resting quotes from the public book before pricing (`filter_own`), so it never
//!   joins or leans on its own order — the correctness fix for quoting on the same feed it
//!   consumes. Off by default → the maker prices straight off the feed exactly as before.
//!
//! Both are pure (`BookView::priced` + `filter_own`, unit-tested in isolation) and COMPOSE with the
//! skew (which shapes the SIZES) and the breaker (which decides WHICH sides quote): the style/filter
//! decide the PRICES. The whole pipeline is shared by the L1 quote lane ([`Strategy::on_quote_tick`](vike_model::Strategy::on_quote_tick))
//! and the L2 book lane ([`Strategy::on_order_book`](vike_model::Strategy::on_order_book)) through one internal `requote`.
//!
//! On top of all of that it carries an **order-refresh TOLERANCE** (the anti-churn gate): before
//! re-issuing a modify for a side, the freshly computed target `(price, size)` is compared against
//! what that side already has RESTING, and a drift inside the configured
//! [`RefreshTolerance`] (basis points of the resting value, so one tuning reads sanely on both a
//! 0..1 prediction market and a five-figure crypto book) SKIPS the modify entirely — the resting
//! order, and its venue queue position, are left alone. Without it the maker emits two venue
//! modifies per book delta forever, which on a rate-limited CLOB reached over a long-haul link is
//! its biggest wire cost. `None` (the DEFAULT, and an all-zero bag too) ⇒ OFF: every tick re-quotes
//! both sides exactly as before, byte-identical. The gate is consulted ONLY on the re-price path —
//! a PULL (the breaker's suppression cancel) is never tolerance-gated, so tolerance can never
//! strand a quote that should come off the book. A FILL likewise invalidates that side's snapshot
//! ([`Strategy::on_fill`](vike_model::Strategy::on_fill)), so a partial fill's size top-up is always re-issued rather than being
//! read as "no change" against the maker's INTENDED (pre-fill) quote.
//!
//! `SpreadMaker` is also LIVE-RE-TUNABLE (the live-parameter plane): its whole tunable bag is
//! hot-swapped by [`Strategy::on_params_updated`](vike_model::Strategy::on_params_updated) from a [`StrategyParams::SpreadMaker`](vike_model::StrategyParams::SpreadMaker) payload,
//! WITHOUT unmounting, so the resting bid/ask keep their venue queue position (the next tick
//! re-prices them in place via modify — never cancel/replace unless the price actually moves).
//!
//! [`OrderBroker`]: vike_model::OrderBroker
//! [`OrderBroker::cancel`]: vike_model::OrderBroker::cancel

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use vike_market_making::alpha::ofi_toxicity;
use vike_market_making::avellaneda::AsState;
use vike_market_making::ladder;
use vike_market_making::refresh::within_tolerance;
use vike_market_making::reward::{clamp_into_band, moas_holds};
use vike_market_making::settlement::accelerated_pull;
use vike_market_making::skew::{FillRec, net_signed_fills, skew_multipliers};
use vike_market_making::underlying::UnderlyingTracker;
use vike_market_making::{
    BookView, MakerParamError, OwnBookFiltration, SpreadMakerPreset, filter_own,
};
use vike_marketdata::{L2Book, TickQuote, TickTrade};
use vike_model::params::{ParamValue, Params};
use vike_model::{
    AsParams, Fill, FlowToxicity, HorizonMode, KappaMode, LadderParams, OrderBookOwnSide,
    OrderBroker, OrderEvent, OrderPlacement, PriceDomain, QuoteStyle, RefreshTolerance,
    ReservationModel, RewardParams, SpreadMakerParams, SpreadModel, SpreadSource, Strategy,
    StrategyParams, TickMark, ToxicityParams, VarianceMode,
};

// ---- The entry points and the mount's facts -------------------------------------------------

/// What the MOUNT this file runs on knows and no params table can say: its venue (which preset an
/// unnamed table starts from), its price grid and what a maker round trip costs there. Today
/// `vike_tradehub::config::ProfileDaemon::maker_params` adds the first three to the profile's table
/// and the mount's fee re-arm (`vike_market_making::rearm_fee_floor`) the fourth; [`maker_params`] is that
/// lowering, step for step.
pub struct MountFacts {
    /// The mount's venue: `polymarket` starts an unnamed table from `outcome_token`, every other
    /// venue from `crypto` (today's `ProfileDaemon::default_preset`).
    pub venue: String,
    /// The mount's price grid, which its feed sizes its book on too. It replaces a stated
    /// `tick_size`.
    pub tick_size: f64,
    /// The break-even round trip of the venue's STATIC fee lane
    /// (`vike_tradehub_core::maker_round_trip_fee_for`), `None` when that fee shape has no flat
    /// fraction of price.
    pub static_round_trip_fee_rate: Option<f64>,
    /// The round trip of the fee schedule the mount's ACCOUNT resolved (`vike_model::maker_round_trip_fee`
    /// of it), `None` when no engine of the account answered or its shape has no flat fraction.
    pub account_round_trip_fee_rate: Option<f64>,
}

/// THE one door the mount-facts document (`vike_model::params::MountFacts`) fills: a LIVE mount's
/// facts, told by its `tick_size` being stated. A BACKTEST states `symbol` and `interval` alone, so
/// it reads `None`: a backtest's maker is `[strategy.params]` alone, as the registry's
/// `spread_maker` arm built it, and a fact added there would change its result.
fn mount_facts(facts: &vike_model::params::MountFacts) -> Option<MountFacts> {
    Some(MountFacts {
        venue: facts.venue.clone(),
        tick_size: facts.tick_size?,
        static_round_trip_fee_rate: facts.static_round_trip_fee_rate,
        account_round_trip_fee_rate: facts.account_round_trip_fee_rate,
    })
}

/// The params bag the maker is built from on a mount with `facts`: the profile's `table` over the
/// preset it names (unnamed: the venue's, [`MountFacts::venue`]), the mount's grid, the static fee
/// lane's break-even floor under a preset that arms one and a table that states none, then the
/// account's own rate whenever it resolved one (it replaces whatever floor was held, as
/// `vike_market_making::rearm_fee_floor` does).
pub fn maker_params(table: &Params, facts: &MountFacts) -> Params {
    let mut bag = table.clone();
    let preset = match SpreadMakerPreset::named(&bag) {
        Ok(Some(named)) => Some(named),
        Ok(None) => {
            let preset = if facts.venue == "polymarket" {
                SpreadMakerPreset::OutcomeToken
            } else {
                SpreadMakerPreset::Crypto
            };
            bag.insert("preset", preset.word());
            Some(preset)
        }
        // A word the maker refuses: left for `SpreadMaker::from_params` to refuse.
        Err(_) => None,
    };
    bag.insert("tick_size", facts.tick_size);
    if preset.is_some_and(SpreadMakerPreset::arms_fee_floor)
        && !bag.contains_key("round_trip_fee_rate")
        && let Some(rate) = facts.static_round_trip_fee_rate
    {
        bag.insert("round_trip_fee_rate", rate);
    }
    if let Some(rate) = facts.account_round_trip_fee_rate {
        bag.insert("round_trip_fee_rate", rate);
    }
    bag
}

/// The bag this run builds from: [`maker_params`] on a live mount, `params` itself otherwise.
fn maker_bag(params: &Params, facts: &vike_model::params::MountFacts) -> Params {
    match mount_facts(facts) {
        Some(facts) => maker_params(params, &facts),
        None => params.clone(),
    }
}

/// The strategy file's entry: the maker `params` describe on the mount `facts` tells (see
/// [`maker_bag`]).
///
/// # Panics
///
/// On a table the maker refuses, which [`refuse`] refuses before this runs.
pub fn build<B: OrderBroker + 'static>(
    params: &Params,
    facts: &vike_model::params::MountFacts,
) -> Box<dyn Strategy<B> + Send> {
    match SpreadMaker::from_params(&maker_bag(params, facts)) {
        Ok(maker) => Box::new(maker),
        Err(e) => panic!("{e} (`refuse` refuses this table before `build` runs)"),
    }
}

/// The whole-table refusal: an enum key naming no variant ([`MakerParamError`]), in its own words.
/// It is asked before any mount exists, so over `params` alone; no fact can make a table refused.
pub fn refuse(params: &Params) -> Option<String> {
    SpreadMaker::from_params(params).err().map(|e| e.to_string())
}

/// What mounted, as `key=value` rows: joined with spaces, today's maker line of
/// `vike_tradehub::config::ProfileDaemon::effective_params`, the knobs that set the posted width.
/// shortcut: the template asks `echo` over the params alone (no facts), so on a LIVE mount it
/// shows the table before the mount's grid and fee floor; give the ABI's echo the facts when F5
/// wires the live maker.
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    let Ok(maker) = SpreadMaker::from_params(params) else {
        return Vec::new();
    };
    let m = maker.params();
    // `pricing = "style"`: no A-S layer, so the placement prices and the A-S knobs configure nothing.
    let Some(p) = m.avellaneda_stoikov else {
        return vec![
            ("qty", m.qty.to_string()),
            ("tick_size", m.tick_size.to_string()),
            ("pricing", "style".to_string()),
            ("style", format!("{:?}", m.style)),
            ("depth_levels", m.depth_levels.to_string()),
        ];
    };
    vec![
        ("qty", m.qty.to_string()),
        ("tick_size", m.tick_size.to_string()),
        ("min_half_spread_ticks", p.min_half_spread_ticks.to_string()),
        ("max_half_spread_ticks", p.max_half_spread_ticks.to_string()),
        ("round_trip_fee_rate", format!("{:?}", p.round_trip_fee_rate)),
        ("kappa_default", p.kappa_default.to_string()),
        ("tau_hold_ms", p.tau_hold_ms.to_string()),
        ("price_domain", format!("{:?}", p.price_domain)),
        ("variance_mode", format!("{:?}", p.variance_mode)),
        ("horizon_mode", format!("{:?}", p.horizon_mode)),
        ("spread_model", format!("{:?}", p.spread_model)),
        ("gamma", p.gamma.to_string()),
        ("resolution_ts", format!("{:?}", p.resolution_ts)),
    ]
}

// ---- The maker ------------------------------------------------------------------------------

/// A minimal two-sided market maker with optional inventory-skew size shaping, a selectable
/// [`QuoteStyle`], and opt-in own-order-book filtration. On every quote/book update it keeps a
/// resting bid and ask priced by the style (default `Mid` = a fixed `half_spread` off the mid),
/// RE-PRICING (and re-sizing) them in place via `modify` (which preserves venue queue priority)
/// instead of cancel/replace. The tags `"bid"`/`"ask"` name the two resting orders so the runtime
/// can resolve them across ticks without the strategy ever seeing an order-client-id.
///
/// Sizes are shaped from the current inventory vs the configured `target_inventory`
/// ([`SpreadMakerParams::target_inventory`]): when long the
/// ask grows and the bid shrinks (bias to reduce the position), when short the reverse. Construct
/// with [`SpreadMaker::new`] for the fixed-size mid-quoting maker, then opt into skew with
/// [`SpreadMaker::with_skew`], the fill-rate breaker with [`SpreadMaker::with_fill_breaker`], a
/// non-`Mid` style with [`SpreadMaker::with_quote_style`], and/or own-order filtration with
/// [`SpreadMaker::with_own_order_filtration`]. They all COMPOSE — the breaker decides WHICH sides
/// quote, the skew decides their SIZES, the style + filter decide their PRICES.
pub struct SpreadMaker {
    /// The maker's whole CONFIG bag — every live-tunable knob, held as the SAME
    /// [`SpreadMakerParams`] shape the live-parameter plane transports (audit F10), so
    /// [`SpreadMaker::params`]/`apply_params` are one struct copy instead of a field-by-field
    /// splat. Per-knob semantics live on [`SpreadMakerParams`]'s field docs and on the `with_*`
    /// builders below (skew/breaker/style/filtration/tolerance/ladder/reward/toxicity); a new knob
    /// now touches the vike-model struct + [`SpreadMaker::new`]'s literal + its consumer — never
    /// `params()`/`apply_params` again.
    ///
    /// ONE exception: the A-S sub-bag. `config.avellaneda_stoikov` is PINNED `None` — the live A-S
    /// params' single authority is `as_state.params` (which `with_spread_model`/`set_params`
    /// mutate in place), so holding a second copy here would drift. [`SpreadMaker::params`]
    /// re-attaches the bag from `as_state` on the way out; `apply_params` strips it on the way in.
    ///
    /// Deliberately CONFIG-ONLY — the config-vs-state split that makes `apply_params`
    /// hot-swap-safe: the runtime/mount-structural state (`last_flow`, `own_book`, the two
    /// [`SideState`]s, `underlying`, `learned_book_tick`) lives as sibling fields the swap never
    /// touches, so a live re-tune keeps resting orders, queue position, and learned state.
    config: SpreadMakerParams,
    /// The latest per-side FLOW-TOXICITY reading delivered by [`Strategy::on_flow`](vike_model::Strategy::on_flow), applied at the
    /// NEXT quote by the [`SpreadMakerParams::toxicity`] guard. `None` until a reading arrives (and on every
    /// maker that is never fed one), which is what keeps a toxicity-off maker byte-identical. RUNTIME
    /// state, NOT a config knob: mount-structural like the resting-order snapshot, so `apply_params`
    /// DELIBERATELY leaves it untouched across a re-tune.
    last_flow: Option<FlowToxicity>,
    /// Whether `last_flow` was set by an EXTERNAL [`Strategy::on_flow`](vike_model::Strategy::on_flow) reading (`true`) rather than by
    /// the OFI-toxicity synthesis fallback (Group-B, PR-3). Set once [`Strategy::on_flow`](vike_model::Strategy::on_flow) ever fires
    /// and never cleared, so an external toxicity feed takes PRECEDENCE permanently: while `true`, the
    /// internal OFI synthesis in `requote` is skipped and never clobbers the real reading. `false` on a
    /// maker that is never fed one — the state that lets the synthesis populate `last_flow` as its
    /// fallback. RUNTIME state, not a config knob (like `last_flow` itself): `apply_params` leaves it
    /// untouched. Inert (and byte-identical) whenever `ofi_toxicity_scale == 0` or no toxicity guard is
    /// configured, since the synthesis it gates never runs then.
    flow_external: bool,
    /// The BID side's per-side runtime quote state (resting snapshot, ladder rungs, breaker
    /// deadline, refresh staleness, placement clock) — see [`SideState`] (audit F5: formerly eight
    /// mirrored `bid_*`/`ask_*` field pairs). RUNTIME state, not config: `apply_params` never
    /// touches it, so resting orders + queue position survive a re-tune.
    bid: SideState,
    /// The ASK side's [`SideState`] — the exact mirror of `bid`.
    ask: SideState,
    /// Opt-in own-order LADDER ([`SpreadMaker::with_own_order_book`]). `None` (default) ⇒ the
    /// per-side `SideState::own` snapshot pair above is what `filter_own` subtracts, byte-identical
    /// to before. `Some` ⇒ filtration instead consults this multi-order, accepted-buffer-gated
    /// [`OrderBookOwn`], which fixes the two things a single snapshot cannot express: N own orders
    /// at one level, and the accept→public-feed race (see `vike_model::OrderBookOwn`'s module doc). Both paths
    /// are still gated by `filter_own`, so turning filtration off live disables either one.
    ///
    /// Deliberately NOT part of [`SpreadMakerParams`] (a vike-model wire type): the ladder is a
    /// mount-time structural choice, so `apply_params` leaves it untouched — same treatment as the
    /// resting-order state and the breaker window.
    own_book: Option<OwnBookFiltration>,
    /// recent fills (side, size, EVENT-ts) inside the window — the netting accumulator
    fills: VecDeque<FillRec>,
    /// Live-params version (the live-parameter plane): bumped once per applied
    /// [`Strategy::on_params_updated`](vike_model::Strategy::on_params_updated). Starts at `0`; observable so a caller/test can confirm a
    /// re-tune actually landed. Read-only from outside — only `on_params_updated` advances it.
    pub params_epoch: u64,
    /// Optional Avellaneda–Stoikov pricing layer (audit mm-quote). `None` (default, via
    /// [`SpreadMaker::new`]) ⇒ the maker prices off the fixed [`QuoteStyle`]/`half_spread` exactly as
    /// before, BYTE-IDENTICAL. `Some` (via [`SpreadMaker::with_avellaneda_stoikov`]) ⇒ the two prices
    /// come from an inventory-aware reservation price + optimal spread instead; that output still
    /// flows UNCHANGED into the size-skew → breaker → filtration → modify-in-place tail below. Holds
    /// the online σ̂²/κ estimators as plain `&mut self` state (single-writer core: no lock).
    as_state: Option<AsState>,
    /// Cross-symbol UNDERLYING tracker ("Option B"): turns the stream of underlying-spot marks routed
    /// in via [`Strategy::on_mark`](vike_model::Strategy::on_mark) into the `(s_now, s_open,
    /// sigma_per_sec)` triple the A-S underlying-anchored fair mid / ATM guard consume through
    /// `AsState::set_underlying`. Mount-STRUCTURAL (like `own_book`) — NOT part of [`SpreadMakerParams`]
    /// — so `apply_params` never touches it. INERT unless BOTH an underlying mark is routed in AND the
    /// A-S layer is on with `underlying_weight`/`atm_blackout_scale` raised; a maker that is never fed
    /// a mark keeps this cold, so it is byte-identical to before.
    underlying: UnderlyingTracker,
    /// The venue price grid LEARNED from the L2 book lane, adopted on the L1 quote lane so the
    /// `min/max_half_spread_ticks` floor/cap AND the grid snap resolve against the SAME tick on both
    /// lanes. `None` until the first L2 book with a positive `tick_size` arrives; a pure-L1 feed never
    /// sets it, so it falls back to the configured [`SpreadMakerParams::tick_size`] (the param stays the
    /// L1-only source).
    /// This closes the silent per-lane spread divergence when the configured `tick_size` disagrees
    /// with the book's grid (the quote lane used the param, the book lane the book, so ONE
    /// `*_half_spread_ticks` produced two different spreads). Runtime state, NOT a config knob —
    /// `apply_params` never touches it, like the resting-order snapshot. Byte-identical whenever the
    /// param already equals the book grid (the correct config), which is the only supported setup.
    learned_book_tick: Option<f64>,
    /// Latch for the UNPRICED-BOOK hold edge (`SpreadMaker::note_no_quote`): `true` while `requote`
    /// is returning early because the pricing step yielded no two-sided quote.
    ///
    /// It exists for the same reason [`AsState::fee_floor_refusing`] does, one layer UP — and the
    /// layer that was still silent. `requote`'s `let Some(..) = priced else { return }` swallows
    /// EVERY unpriced tick without a word: a one-sided book, a book whose sides never arrive, a
    /// blackout pinned at a wall. So a maker can hold forever with `orders:0`, `fault:null` and an
    /// empty journal, which is exactly the state the CI box sat in on 2026-08-17 across ~112k summary
    /// `seq` while its bybit market-data socket carried keepalives and no `orderbook.50` data. The
    /// fee-floor warn could not speak, because `AsState::price` returns at `fair_value(view)?`
    /// BEFORE reaching it — the guard against silent holds was itself downstream of one.
    ///
    /// Edge-triggered for the same reason as its sibling: `requote` is the vike-core hot fold, whose
    /// rule is "instrument per-order boundaries and fault transitions only". RUNTIME state, not a
    /// config knob — `apply_params` leaves it untouched, like `learned_book_tick`. NOT persisted: a
    /// restart re-derives it from the first tick.
    no_quote_holding: bool,
}

/// ONE side's (bid's or ask's) runtime quote state, held as the [`SpreadMaker::bid`]/
/// [`SpreadMaker::ask`] pair (audit F5 — formerly eight mirrored `bid_*`/`ask_*` field pairs
/// driving fully mirrored per-side arms; the per-side helpers now take `is_bid` and index one
/// state via [`SpreadMaker::side`]/`side_mut`). All of it is RUNTIME state, not config:
/// `apply_params` leaves both sides untouched across a re-tune, so resting orders and their venue
/// queue position survive. [`Default`] is exactly the fresh-maker state (nothing resting, nothing
/// suppressed, clocks at `0`).
#[derive(Default)]
struct SideState {
    /// Our OWN currently-resting quote on this side as `(price, size)`, or `None` — what
    /// single-snapshot filtration subtracts from the public book next tick. Tracked whenever the
    /// side is placed/modified, cleared when pulled. NOTE it is the maker's INTENDED quote, not
    /// the venue-side remainder — see `refresh_stale`.
    own: Option<(f64, f64)>,
    /// Resting ladder rungs as `(price, size)`, index `k` ⇒ tag `"bid{k}"`/`"ask{k}"`,
    /// deepest-last — the multi-order twin of `own`, tracked ONLY on the active-ladder path. EMPTY
    /// on the default single-quote path (which uses `own`/`placed` instead), which is what keeps
    /// that path byte-identical: the ladder retire/diff code is a guarded no-op while this is
    /// empty.
    rungs: Vec<(f64, f64)>,
    /// EVENT-ts until which this side is suppressed by the fill-rate breaker (`0` = not
    /// suppressed).
    suppressed_until: i64,
    /// Whether a single quote is currently resting on this side (per-side so one side can be
    /// pulled alone).
    placed: bool,
    /// Order-refresh tolerance INVALIDATION flag: set by [`Strategy::on_fill`](vike_model::Strategy::on_fill) whenever a fill
    /// lands on this side, cleared the moment the side is next placed/re-priced/pulled.
    ///
    /// WHY it exists: `own` is the maker's INTENDED quote, not what actually remains resting at
    /// the venue. After a PARTIAL fill the venue-side size is smaller than the intended size, yet
    /// an intended-vs-target comparison still reads "no change" — so a tolerance-gated maker would
    /// never top the side back up and would silently quote less size than configured. The flag
    /// marks that side's snapshot STALE, forcing the next tick through the modify arm (which
    /// re-issues the full configured size) exactly once.
    ///
    /// INERT WHEN THE GATE IS OFF: it is read ONLY by `refresh_skips`/`reward_moas_holds` (and the
    /// ladder's per-rung twin), which short-circuit to "re-quote" — so setting it changes nothing
    /// on the default path (byte-identical), which is why `on_fill` may set it before the
    /// breaker's early return.
    refresh_stale: bool,
    /// Event-ts this side was last placed/re-priced at (`0` = not currently resting) — the reward
    /// MIN-ORDER-AGE (moas) clock (`reward_moas_holds`) AND the OwnFillFit exposure clock
    /// (`feed_own_outcome`), inert dead state while both are off (byte-identical). Refreshed on
    /// every real place/re-price, reset to `0` on a pull, and DELIBERATELY not touched on a
    /// tolerance/moas SKIP — so a held side's age keeps accumulating toward the floor.
    quoted_timestamp: i64,
    /// `Some` while this side's resting order was RESTORED from a saved state and nothing has
    /// confirmed it alive yet (`Some(0)`: grace clock not started; `Some(t)`: started at event-ts
    /// `t`, as `quoted_timestamp` reads `0` for "unset"). `None` on every side this maker placed itself.
    /// The safety net in [`SpreadMaker::restored_quote_expired`].
    unverified: Option<i64>,
}

/// The shared front half of every enum reader below: resolve `key` to its string, or say WHICH of
/// the three states the table is in. `Ok(None)` is the ABSENT key (keep the default); `Err` is the
/// present-but-not-a-string case; `Ok(Some(s))` is the value AS WRITTEN — each reader lowercases
/// for its own match and keeps this copy, so a failure quotes the operator's own spelling back at
/// them rather than a normalized one. `accepted` is threaded through only so a failure can print it.
fn param_str(
    params: &Params,
    key: &'static str,
    accepted: &'static str,
) -> Result<Option<String>, MakerParamError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(MakerParamError::NotAString { key, found: v.type_str(), accepted }),
        },
    }
}

/// Parse the `variance_mode` param string (case-insensitive) into a [`VarianceMode`]. `Ok(None)`
/// only for an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] mode; a
/// present-but-unrecognized value is a [`MakerParamError`], never a silent fallback.
fn read_variance_mode(params: &Params) -> Result<Option<VarianceMode>, MakerParamError> {
    const KEY: &str = "variance_mode";
    const ACCEPTED: &str = "local_capped | capped | pure_bernoulli | bernoulli | raw_local | raw";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "local_capped" | "capped" => Ok(Some(VarianceMode::LocalCapped)),
        "pure_bernoulli" | "bernoulli" => Ok(Some(VarianceMode::PureBernoulli)),
        "raw_local" | "raw" => Ok(Some(VarianceMode::RawLocal)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `spread_model` param string (case-insensitive) into a [`SpreadModel`]. `Ok(None)` only
/// for an ABSENT key (keep the [`AsParams::default`] model); a present-but-unrecognized value is a
/// [`MakerParamError`] — this is the reader whose silent fallback ran A-S under a profile naming GLFT.
fn read_spread_model(params: &Params) -> Result<Option<SpreadModel>, MakerParamError> {
    const KEY: &str = "spread_model";
    const ACCEPTED: &str = "avellaneda_stoikov | avellaneda | as | gueant | glft | gueant_lehalle";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "avellaneda_stoikov" | "avellaneda" | "as" => Ok(Some(SpreadModel::AvellanedaStoikov)),
        "gueant" | "glft" | "gueant_lehalle" => Ok(Some(SpreadModel::Gueant)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `reservation_model` param string (case-insensitive) into a [`ReservationModel`].
/// `Ok(None)` only for an ABSENT key, so [`SpreadMaker::from_params`] keeps the A-S linear
/// reservation; a present-but-unrecognized value is a [`MakerParamError`].
fn read_reservation_model(params: &Params) -> Result<Option<ReservationModel>, MakerParamError> {
    const KEY: &str = "reservation_model";
    const ACCEPTED: &str = "as_linear | as | linear | lmsr | logistic";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "as_linear" | "as" | "linear" => Ok(Some(ReservationModel::AsLinear)),
        "lmsr" | "logistic" => Ok(Some(ReservationModel::Lmsr)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `spread_source` param string (case-insensitive) into a [`SpreadSource`]. `Ok(None)`
/// only for an ABSENT key, so [`SpreadMaker::from_params`] keeps the A-S optimal half-spread; a
/// present-but-unrecognized value is a [`MakerParamError`].
fn read_spread_source(params: &Params) -> Result<Option<SpreadSource>, MakerParamError> {
    const KEY: &str = "spread_source";
    const ACCEPTED: &str =
        "as_optimal | as | optimal | ls_lmsr | lslmsr | glosten_milgrom | glosten | gm";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "as_optimal" | "as" | "optimal" => Ok(Some(SpreadSource::AsOptimal)),
        "ls_lmsr" | "lslmsr" => Ok(Some(SpreadSource::LsLmsr)),
        "glosten_milgrom" | "glosten" | "gm" => Ok(Some(SpreadSource::GlostenMilgrom)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `style` param string (case-insensitive) into a [`QuoteStyle`] — the PLACEMENT of the two
/// quotes on the book (`top` rests at the touch, `join` joins the best, `mid` = reservation ±
/// half-spread inside the spread, `depth` rests `depth_levels` into the book). `Ok(None)` only for an
/// ABSENT key, so [`SpreadMaker::from_params`] keeps [`QuoteStyle::Mid`] (the original behavior); a
/// present-but-unrecognized value is a [`MakerParamError`]. Exposing this lets a backtest profile sweep
/// the fill-rate-driving placement, not just the pricing model.
fn read_quote_style(params: &Params) -> Result<Option<QuoteStyle>, MakerParamError> {
    const KEY: &str = "style";
    const ACCEPTED: &str = "top | join | mid | depth";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "top" => Ok(Some(QuoteStyle::Top)),
        "join" => Ok(Some(QuoteStyle::Join)),
        "mid" => Ok(Some(QuoteStyle::Mid)),
        "depth" => Ok(Some(QuoteStyle::Depth)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `pricing` param selector (case-insensitive): decides whether
/// [`SpreadMaker::from_params`] chains the A-S layer. `"as"` — and absent/unrecognized, today's
/// behavior, byte-identical — ⇒ `true`: [`SpreadMaker::with_avellaneda_stoikov`] is chained and
/// PRICES, which makes the `style` key INERT (`requote` bypasses the [`QuoteStyle`] placement
/// whenever A-S state is `Some`). `"style"` ⇒ `false`: the A-S layer is SKIPPED so the selected
/// placement (`style`/`depth_levels`) actually prices — the registry path where the `style` key
/// finally does something (audit F8).
///
/// ⚠ Deliberately NOT one of the [`MakerParamError`] readers: it maps to a `bool`, not to an enum
/// variant, so it has no `_ => None` arm to disambiguate. The residual that leaves is real and
/// named here rather than left implicit — `pricing = "styel"` reads as `"as"` and chains A-S, the
/// same silent-fallback shape the enum readers no longer have. Closing it is a behavior change to
/// this selector's documented "absent/unrecognized ⇒ `as`" contract, not a `None`-vs-`None` fix.
fn read_pricing_chains_as(params: &Params) -> bool {
    !matches!(
        params.get("pricing").and_then(ParamValue::as_str).map(str::to_ascii_lowercase).as_deref(),
        Some("style" | "quote_style")
    )
}

/// Parse the `horizon_mode` param string (case-insensitive) into a [`HorizonMode`]. `Ok(None)` only
/// for an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] mode; a
/// present-but-unrecognized value is a [`MakerParamError`].
fn read_horizon_mode(params: &Params) -> Result<Option<HorizonMode>, MakerParamError> {
    const KEY: &str = "horizon_mode";
    const ACCEPTED: &str = "time_to_resolution | resolution | constant_tau | constant";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "time_to_resolution" | "resolution" => Ok(Some(HorizonMode::TimeToResolution)),
        "constant_tau" | "constant" => Ok(Some(HorizonMode::ConstantTau)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `kappa_mode` param string (case-insensitive) into a [`KappaMode`]. `Ok(None)` only for
/// an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] mode (`Fixed` —
/// the online fits stay "available but gated"); a present-but-unrecognized value is a
/// [`MakerParamError`]. This is the audit-F3 selector that makes the whole #790 κ-fit family reachable
/// from a registry/harness table.
fn read_kappa_mode(params: &Params) -> Result<Option<KappaMode>, MakerParamError> {
    const KEY: &str = "kappa_mode";
    const ACCEPTED: &str = "fixed | live_fit | livefit | live | own_fill_fit | ownfillfit | own";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "fixed" => Ok(Some(KappaMode::Fixed)),
        "live_fit" | "livefit" | "live" => Ok(Some(KappaMode::LiveFit)),
        "own_fill_fit" | "ownfillfit" | "own" => Ok(Some(KappaMode::OwnFillFit)),
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `price_domain` param string (case-insensitive) into a [`PriceDomain`]. `Ok(None)` only
/// for an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] domain; a
/// present-but-unrecognized value is a [`MakerParamError`].
/// `"band"` reads its walls from `band_lo`/`band_hi` (defaulting to the `[0, 1]` unit interval).
fn read_price_domain(params: &Params) -> Result<Option<PriceDomain>, MakerParamError> {
    const KEY: &str = "price_domain";
    const ACCEPTED: &str = "unit_interval | unit | unbounded | band";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "unit_interval" | "unit" => Ok(Some(PriceDomain::UnitInterval)),
        "unbounded" => Ok(Some(PriceDomain::Unbounded)),
        "band" => {
            let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);
            Ok(Some(PriceDomain::Band {
                lo: f("band_lo").unwrap_or(0.0),
                hi: f("band_hi").unwrap_or(1.0),
            }))
        }
        _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// The TOML value kinds ([`ParamValue::type_str`]) a lenient numeric read takes.
const NUMBER: &[&str] = &["float", "integer"];
/// ...and a string read.
const STRING: &[&str] = &["string"];

impl SpreadMaker {
    /// Every `[strategy.params]` key [`Self::from_params`] reads, with the value kinds
    /// ([`ParamValue::type_str`]) its reader takes: what a mount refuses an unknown or mistyped key
    /// against, since the reader itself ignores both (`qtyy = 0.005` is `qty` at its default). Pinned
    /// to the reader in both directions by this file's `tests::from_params`.
    pub const PARAM_KEYS: &[(&str, &[&str])] = &[
        ("preset", STRING),
        ("qty", NUMBER),
        ("tick_size", NUMBER),
        ("half_spread", NUMBER),
        ("gamma", NUMBER),
        ("q_scale", NUMBER),
        ("min_half_spread_ticks", NUMBER),
        ("max_half_spread_ticks", NUMBER),
        ("round_trip_fee_rate", NUMBER),
        ("variance_mode", STRING),
        ("horizon_mode", STRING),
        ("price_domain", STRING),
        ("band_lo", NUMBER),
        ("band_hi", NUMBER),
        ("pricing", STRING),
        ("style", STRING),
        ("depth_levels", NUMBER),
        ("target_inventory", NUMBER),
        ("max_inventory", NUMBER),
        ("skew", NUMBER),
        ("fill_window_ms", NUMBER),
        ("net_fill_threshold", NUMBER),
        ("suppress_cooldown_ms", NUMBER),
        ("pull_accel_ramp_ms", NUMBER),
        ("pull_accel_max", NUMBER),
        ("kappa_mode", STRING),
        ("kappa_default", NUMBER),
        ("kappa_min", NUMBER),
        ("kappa_max", NUMBER),
        ("n_min", NUMBER),
        ("trade_window_ms", NUMBER),
        ("resolution_ts", NUMBER),
        ("tau_hold_ms", NUMBER),
        ("resolution_blackout_ms", NUMBER),
        ("terminal_penalty_gamma", NUMBER),
        ("terminal_ramp_ms", NUMBER),
        ("underlying_weight", NUMBER),
        ("underlying_beta", NUMBER),
        ("window_secs", NUMBER),
        ("atm_blackout_scale", NUMBER),
        ("spread_model", STRING),
        ("base_intensity_a", NUMBER),
        ("reservation_model", STRING),
        ("lmsr_b", NUMBER),
        ("spread_source", STRING),
        ("ls_lmsr_alpha", NUMBER),
        ("gm_mu", NUMBER),
        ("alpha_beta_imbalance", NUMBER),
        ("alpha_lambda_ofi", NUMBER),
        ("ofi_decay", NUMBER),
        ("running_penalty_phi", NUMBER),
        ("flatten_by_ms", NUMBER),
        ("flatten_strength", NUMBER),
        ("toxicity_widen", NUMBER),
        ("toxicity_size_cut", NUMBER),
        ("ofi_toxicity_scale", NUMBER),
        ("refresh_price_bps", NUMBER),
        ("refresh_size_bps", NUMBER),
    ];
}

impl SpreadMaker {
    /// Fixed-size two-sided maker — NO inventory skew (`skew = 0`), identical to the original
    /// behavior. `target_inventory`/`max_inventory` default to `0.0`/`1.0` (inert while `skew`
    /// is `0`).
    pub fn new(qty: f64, half_spread: f64) -> Self {
        SpreadMaker {
            // The one remaining knob-listing site (with the vike-model struct itself): a new knob
            // is added here with its neutral default, and `params()`/`apply_params` pick it up for
            // free as struct copies.
            config: SpreadMakerParams {
                qty,
                half_spread,
                target_inventory: 0.0,
                max_inventory: 1.0,
                skew: 0.0,
                fill_window_ms: 0,
                net_fill_threshold: 0.0,
                suppress_cooldown_ms: 0,
                style: QuoteStyle::Mid,
                depth_levels: 1,
                tick_size: 0.0,
                filter_own: false,
                // PINNED None — the A-S bag's single authority is `as_state.params` (see `config`).
                avellaneda_stoikov: None,
                refresh_tolerance: None,
                ladder: None,
                reward: None,
                toxicity: None,
            },
            last_flow: None,
            flow_external: false,
            bid: SideState::default(),
            ask: SideState::default(),
            own_book: None,
            fills: VecDeque::new(),
            params_epoch: 0,
            as_state: None,
            underlying: UnderlyingTracker::new(),
            learned_book_tick: None,
            no_quote_holding: false,
        }
    }

    /// Read a harness/registry params table into a working Avellaneda–Stoikov maker — the
    /// `BuyHold::from_params` reader convention (a READER, not a schema: unknown keys ignored, missing
    /// keys fall back to a default), so the backtest strategy registry can resolve + tune a
    /// `SpreadMaker` by name. It builds the fixed-size `Mid` seed ([`SpreadMaker::new`]) then layers
    /// A-S on via [`SpreadMaker::with_avellaneda_stoikov`] (unless `pricing = "style"` — see the
    /// table), so a bare `{}` yields the recommended A-S maker ([`AsParams::default`]) and each
    /// present key overrides one knob; a `preset` key starts from that [`SpreadMakerPreset`]'s bag
    /// instead, and the table's own keys override it (the daemon's maker profile is exactly this: a
    /// params table over the preset it names). The reward bag is not read here (it rides
    /// [`Self::with_liquidity_rewards`] and the live-params plane); the toxicity bag IS readable here
    /// via the `toxicity_*` keys below. [`Self::PARAM_KEYS`] is the whole key set.
    ///
    /// | key | meaning | default |
    /// |---|---|---|
    /// | `preset` | `outcome_token` \| `crypto`: the bag the other keys override ([`SpreadMakerPreset`]) | unset (the defaults below) |
    /// | `qty` | base quote size per side | `1.0` |
    /// | `tick_size` | venue price grid (the A-S L1 snap/clamp) | `0.0` |
    /// | `half_spread` | fixed-spread seed (unused while A-S prices) | `2·tick_size`, else `0.01` |
    /// | `gamma` | A-S risk aversion `γ` | `AsParams::default` (`0.1`) |
    /// | `q_scale` | A-S inventory normaliser | `AsParams::default` (`100.0`) |
    /// | `min_half_spread_ticks` | A-S half-spread floor (ticks) | `0.0` |
    /// | `max_half_spread_ticks` | A-S half-spread ceiling (ticks) | `0.0` |
    /// | `round_trip_fee_rate` | venue round-trip maker fee (fraction of price) — floors the half-spread at BREAK-EVEN `½·m·s`, and REFUSES the quote when that exceeds the ceiling | unset (no floor) |
    /// | `variance_mode` | `local_capped` \| `pure_bernoulli` \| `raw_local` | `local_capped` |
    /// | `horizon_mode` | `time_to_resolution` \| `constant_tau` | `time_to_resolution` |
    /// | `price_domain` | `unit_interval` \| `unbounded` \| `band` (+ `band_lo`/`band_hi`) | `unit_interval` |
    /// | `pricing` | `as` (A-S prices; the `style` key is then INERT) \| `style` (skip A-S — the [`QuoteStyle`] placement prices) | `as` |
    /// | `style` / `depth_levels` | [`QuoteStyle`] placement (`top`/`join`/`mid`/`depth`) — prices only under `pricing = "style"` | `mid` / `1` |
    /// | `target_inventory` / `max_inventory` / `skew` | inventory-skew size shaping ([`Self::with_skew`]) | `0.0` / `1.0` / `0.0` (off) |
    /// | `fill_window_ms` / `net_fill_threshold` / `suppress_cooldown_ms` | fill-rate breaker ([`Self::with_fill_breaker`]) | `0` / `0.0` / `0` (off) |
    /// | `pull_accel_ramp_ms` / `pull_accel_max` | breaker pull-ACCEL near resolution (needs `resolution_ts`) | `0` / `0.0` (off) |
    /// | `kappa_mode` | `fixed` \| `live_fit` \| `own_fill_fit` (the κ-fit family) | `fixed` |
    /// | `kappa_default` / `kappa_min` / `kappa_max` | fixed κ + fitted-κ clamps | `AsParams::default` |
    /// | `n_min` / `trade_window_ms` | κ-MLE sample floor / tape window (ms) | `AsParams::default` |
    /// | `resolution_ts` | resolution anchor `T` (epoch-ms) — EVERY settlement feature keys off it | unset |
    /// | `tau_hold_ms` / `resolution_blackout_ms` | holding tenor / blackout window (ms) | `AsParams::default` |
    /// | `terminal_penalty_gamma` / `terminal_ramp_ms` | terminal settlement-variance penalty | `0.0` / `0` (off) |
    /// | `underlying_weight` / `underlying_beta` / `window_secs` / `atm_blackout_scale` | underlying-anchored fair mid + ATM blackout guard | `AsParams::default` (off) |
    /// | `flatten_by_ms` / `flatten_strength` | settlement force-flatten schedule | `0` / `0.0` (off) |
    /// | `toxicity_widen` / `toxicity_size_cut` | [`ToxicityParams`] flow-toxicity guard — EITHER key present builds the bag | unset (no bag) |
    /// | `ofi_toxicity_scale` | internal OFI→toxicity synthesis scale (needs the guard + `alpha_lambda_ofi`) | `0.0` (off) |
    /// | `refresh_price_bps` / `refresh_size_bps` | anti-churn re-quote tolerance (bps) | `0.0` (off) |
    ///
    /// NOTE the crypto profile: an unbounded `$`-scale asset wants `price_domain = "unbounded"`,
    /// `variance_mode = "raw_local"`, `horizon_mode = "constant_tau"`, `min_half_spread_ticks = 2`,
    /// `max_half_spread_ticks = 60` — the exact knobs `preset = "crypto"` sets.
    ///
    /// # Errors
    ///
    /// A key from the ENUM column above (`preset`, `variance_mode`, `horizon_mode`, `price_domain`,
    /// `spread_model`, `reservation_model`, `spread_source`, `kappa_mode`, `style`) that is PRESENT
    /// but names no variant is a [`MakerParamError`] — the profile fails to resolve rather than running
    /// a different model than it names. An ABSENT key keeps its default, exactly as the reader
    /// convention documents and as the table above states; that path is unchanged, so every
    /// profile that resolves today still resolves. The numeric keys stay lenient (an unparsable
    /// `gamma` is not an enum's problem and has no wrong-variant failure mode).
    pub fn from_params(params: &Params) -> Result<Self, MakerParamError> {
        // A named PRESET is the bag the table's own keys override; absent, the defaults below.
        let preset = SpreadMakerPreset::named(params)?;
        let merged;
        let params = match preset {
            Some(preset) => {
                merged = preset.under(params);
                &merged
            }
            None => params,
        };
        let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);
        // i64 twin of `f` for the epoch-ms / window knobs (`x = 5000` == `x = 5000.0`).
        let ms = |k: &str| f(k).map(|v| v as i64);
        let d = AsParams::default();
        let as_params = AsParams {
            gamma: f("gamma").unwrap_or(d.gamma),
            q_scale: f("q_scale").unwrap_or(d.q_scale),
            min_half_spread_ticks: f("min_half_spread_ticks").unwrap_or(d.min_half_spread_ticks),
            max_half_spread_ticks: f("max_half_spread_ticks").unwrap_or(d.max_half_spread_ticks),
            // ABSENT ⇒ `None` ⇒ NO break-even floor, not a zero fee (the `resolution_ts` shape:
            // `.or(default)`, never `.unwrap_or(0.0)`). A harness that wants the floor states the
            // venue's round-trip maker rate; the LIVE mount resolves it from `fee_schedule_for`.
            round_trip_fee_rate: f("round_trip_fee_rate").or(d.round_trip_fee_rate),
            variance_mode: read_variance_mode(params)?.unwrap_or(d.variance_mode),
            horizon_mode: read_horizon_mode(params)?.unwrap_or(d.horizon_mode),
            price_domain: read_price_domain(params)?.unwrap_or(d.price_domain),
            // κ-fit family (audit F3 — the #790 family was unreachable): the mode selector plus the
            // fixed/fitted κ clamps and the MLE's tape window/sample floor. Absent ⇒ `Fixed` at
            // `kappa_default`, byte-identical (the online fits stay "available but gated", exactly
            // as `AsParams::default` documents).
            kappa_mode: read_kappa_mode(params)?.unwrap_or(d.kappa_mode),
            kappa_default: f("kappa_default").unwrap_or(d.kappa_default),
            kappa_min: f("kappa_min").unwrap_or(d.kappa_min),
            kappa_max: f("kappa_max").unwrap_or(d.kappa_max),
            n_min: f("n_min").map(|v| v as usize).unwrap_or(d.n_min),
            trade_window_ms: ms("trade_window_ms").unwrap_or(d.trade_window_ms),
            // SETTLEMENT family (audit F4): `resolution_ts` is the anchor EVERY settlement feature
            // keys off (force-flatten τ / blackout / terminal penalty / underlying anchor /
            // pull-accel — all of them read it), plus the tenor + blackout + terminal-penalty +
            // underlying/ATM knobs. Absent ⇒ each stays at its inert default, byte-identical.
            resolution_ts: ms("resolution_ts").or(d.resolution_ts),
            tau_hold_ms: ms("tau_hold_ms").unwrap_or(d.tau_hold_ms),
            resolution_blackout_ms: ms("resolution_blackout_ms")
                .unwrap_or(d.resolution_blackout_ms),
            terminal_penalty_gamma: f("terminal_penalty_gamma").unwrap_or(d.terminal_penalty_gamma),
            terminal_ramp_ms: ms("terminal_ramp_ms").unwrap_or(d.terminal_ramp_ms),
            atm_blackout_scale: f("atm_blackout_scale").unwrap_or(d.atm_blackout_scale),
            underlying_weight: f("underlying_weight").unwrap_or(d.underlying_weight),
            underlying_beta: f("underlying_beta").unwrap_or(d.underlying_beta),
            window_secs: f("window_secs").unwrap_or(d.window_secs),
            // GLFT selector + its base intensity `A` (see `SpreadModel`). Absent ⇒ A-S, byte-identical.
            spread_model: read_spread_model(params)?.unwrap_or(d.spread_model),
            base_intensity_a: f("base_intensity_a").unwrap_or(d.base_intensity_a),
            // GROUP-B pluggables (see `ReservationModel`/`SpreadSource`). Absent ⇒ the A-S/GLFT path,
            // byte-identical. `lmsr_b`/`ls_lmsr_alpha`/`gm_mu` are read only by their selected model.
            reservation_model: read_reservation_model(params)?.unwrap_or(d.reservation_model),
            lmsr_b: f("lmsr_b").unwrap_or(d.lmsr_b),
            spread_source: read_spread_source(params)?.unwrap_or(d.spread_source),
            ls_lmsr_alpha: f("ls_lmsr_alpha").unwrap_or(d.ls_lmsr_alpha),
            gm_mu: f("gm_mu").unwrap_or(d.gm_mu),
            // GROUP-B reservation shifts (see the `alpha`/`settlement` modules). Absent ⇒ 0 ⇒ inert,
            // byte-identical; `ofi_decay` is read only when `alpha_lambda_ofi != 0`.
            alpha_beta_imbalance: f("alpha_beta_imbalance").unwrap_or(d.alpha_beta_imbalance),
            alpha_lambda_ofi: f("alpha_lambda_ofi").unwrap_or(d.alpha_lambda_ofi),
            ofi_decay: f("ofi_decay").unwrap_or(d.ofi_decay),
            running_penalty_phi: f("running_penalty_phi").unwrap_or(d.running_penalty_phi),
            flatten_by_ms: ms("flatten_by_ms").unwrap_or(d.flatten_by_ms),
            flatten_strength: f("flatten_strength").unwrap_or(d.flatten_strength),
            // Breaker PULL-ACCEL + OFI-toxicity synthesis scale (audit F2/F1 — the #798 pair was
            // unreachable). Absent ⇒ 0 ⇒ inert, byte-identical (the accel also needs a
            // `resolution_ts`; the synthesis also needs a toxicity guard + `alpha_lambda_ofi != 0`).
            pull_accel_ramp_ms: ms("pull_accel_ramp_ms").unwrap_or(d.pull_accel_ramp_ms),
            pull_accel_max: f("pull_accel_max").unwrap_or(d.pull_accel_max),
            ofi_toxicity_scale: f("ofi_toxicity_scale").unwrap_or(d.ofi_toxicity_scale),
            ..d
        };
        let qty = f("qty").unwrap_or(1.0);
        let tick_size = f("tick_size").unwrap_or(0.0);
        let default_half = if tick_size > 0.0 { tick_size * 2.0 } else { 0.01 };
        let half_spread = f("half_spread").unwrap_or(default_half);
        // Opt-in re-quote tolerance (bps of the resting value): the maker HOLDS its resting quote
        // while the target price/size stay within tolerance, instead of re-quoting every tick. This
        // is load-bearing for a queue-position fill backtest — a quote that re-prices every tick
        // forfeits its FIFO slot and can never be hit by a taker trade. `0.0`/`0.0` (the default)
        // ⇒ every tick re-quotes, byte-identical to before ([`with_refresh_tolerance`]'s OFF path).
        let refresh_price_bps = f("refresh_price_bps").unwrap_or(0.0);
        let refresh_size_bps = f("refresh_size_bps").unwrap_or(0.0);
        // Placement style + depth (the fill-rate lever): `style` selects Top/Join/Mid/Depth, absent ⇒
        // Mid (the original behavior, byte-identical). `depth_levels` is only read by `Depth`. NOTE
        // the style only PRICES under `pricing = "style"` — while the A-S layer is chained (the
        // default), `requote` bypasses the QuoteStyle placement entirely.
        let style = read_quote_style(params)?.unwrap_or(QuoteStyle::Mid);
        let depth_levels = f("depth_levels").map(|v| v as usize).filter(|&n| n >= 1).unwrap_or(1);
        // Inventory-skew size shaping (audit F1): `with_skew`'s three knobs verbatim. The absent-key
        // defaults (`0.0`/`1.0`/`0.0`) are exactly `SpreadMaker::new`'s neutral values, so a keyless
        // table is byte-identical (skew off).
        let target_inventory = f("target_inventory").unwrap_or(0.0);
        let max_inventory = f("max_inventory").unwrap_or(1.0);
        let skew = f("skew").unwrap_or(0.0);
        // Fill-rate breaker (audit F2): `with_fill_breaker`'s three knobs verbatim. Absent ⇒
        // `0`/`0.0`/`0` — the breaker's own OFF gate (`breaker_enabled` needs all three positive) —
        // byte-identical. The pull-accel pair above composes with it near a resolution.
        let fill_window_ms = ms("fill_window_ms").unwrap_or(0);
        let net_fill_threshold = f("net_fill_threshold").unwrap_or(0.0);
        let suppress_cooldown_ms = ms("suppress_cooldown_ms").unwrap_or(0);
        // Flow-toxicity guard (audit F1): the bag is built ONLY when a `toxicity_*` key is present,
        // so an absent pair keeps `toxicity = None` — byte-identical (an all-zero bag would also be
        // inert, but `None` keeps the observable `params()` surface identical too). Pairs with
        // `ofi_toxicity_scale` above so the internally-synthesized OFI reading (#798) can actually
        // fire from a registry table — no external `on_flow` feed exists in a backtest.
        let toxicity = (params.get("toxicity_widen").is_some()
            || params.get("toxicity_size_cut").is_some())
        .then(|| ToxicityParams {
            widen: f("toxicity_widen").unwrap_or(0.0),
            size_cut: f("toxicity_size_cut").unwrap_or(0.0),
        });
        let mut maker = SpreadMaker::new(qty, half_spread)
            .with_quote_style(style, depth_levels, tick_size)
            .with_skew(target_inventory, max_inventory, skew)
            .with_fill_breaker(fill_window_ms, net_fill_threshold, suppress_cooldown_ms);
        // The tolerance bag: always built on a bare table (an all-zero bag is inert, and this
        // reader's `params()` has always carried one); under a PRESET only when a refresh key is
        // stated, the mount's opt-in convention, so a preset maker reports what a mount's maker
        // always reported (`None` until asked).
        let refresh_stated =
            params.contains_key("refresh_price_bps") || params.contains_key("refresh_size_bps");
        if preset.is_none() || refresh_stated {
            maker = maker.with_refresh_tolerance(refresh_price_bps, refresh_size_bps);
        }
        if let Some(tox) = toxicity {
            maker = maker.with_flow_toxicity(tox);
        }
        // Pricing selector (audit F8, the #803 inert-`style` finding): `"as"` (absent/default)
        // chains the A-S layer exactly as before; `"style"` SKIPS it so the selected QuoteStyle
        // placement finally prices in the registry path.
        if read_pricing_chains_as(params) {
            maker = maker.with_avellaneda_stoikov(as_params);
        }
        Ok(maker)
    }

    /// Builder: enable inventory-skew size shaping. Bias the bid/ask sizes to pull the position
    /// toward `target_inventory`, ramping over `max_inventory`, at intensity `skew` in `[0, 1]`.
    /// `with_skew(t, m, 0.0)` is equivalent to the fixed-size maker.
    pub fn with_skew(mut self, target_inventory: f64, max_inventory: f64, skew: f64) -> Self {
        self.config.target_inventory = target_inventory;
        self.config.max_inventory = max_inventory;
        self.config.skew = skew;
        self
    }

    /// Builder: enable the per-side fill-rate circuit breaker (audit mm1). Over a sliding
    /// `window_ms` EVENT-TIME window, net this-side-minus-other-side fill size; when it reaches
    /// `net_fill_threshold` on one side (net one-directional accumulation = adverse selection),
    /// suppress that side's quoting for `cooldown_ms`. `with_fill_breaker(_, 0.0, _)` or any
    /// non-positive `window_ms`/`cooldown_ms` leaves the breaker OFF (the maker is unchanged).
    pub fn with_fill_breaker(
        mut self,
        window_ms: i64,
        net_fill_threshold: f64,
        cooldown_ms: i64,
    ) -> Self {
        self.config.fill_window_ms = window_ms;
        self.config.net_fill_threshold = net_fill_threshold;
        self.config.suppress_cooldown_ms = cooldown_ms;
        self
    }

    /// Builder: choose the [`QuoteStyle`] (default [`QuoteStyle::Mid`], which reproduces the original
    /// midpoint pricing). `depth_levels` is how many levels into the book [`QuoteStyle::Depth`] rests
    /// (ignored by the other styles); `tick_size` is the venue grid [`QuoteStyle::Top`] steps by on
    /// the L1 quote lane (the L2 book lane uses the book's own tick size, so pass `0.0` there).
    /// `with_quote_style(QuoteStyle::Mid, _, _)` leaves the maker mid-quoting exactly as before.
    pub fn with_quote_style(
        mut self,
        style: QuoteStyle,
        depth_levels: usize,
        tick_size: f64,
    ) -> Self {
        self.config.style = style;
        self.config.depth_levels = depth_levels;
        self.config.tick_size = tick_size;
        self
    }

    /// Builder: enable own-order-book filtration — subtract this maker's own resting quotes from the
    /// public book before pricing, so it never joins or leans on its own order (the correctness fix
    /// for quoting on the same feed it consumes). OFF by default, so the default maker prices straight
    /// off the feed exactly as before. Matching needs a known tick grid: the L2 book carries one; for
    /// the L1 quote lane set `tick_size` via [`SpreadMaker::with_quote_style`].
    pub fn with_own_order_filtration(mut self) -> Self {
        self.config.filter_own = true;
        self
    }

    /// Builder: enable the order-refresh TOLERANCE (the anti-churn gate) — stop re-issuing a modify
    /// for a side whose target barely moved. Both thresholds are RELATIVE, in BASIS POINTS of what
    /// that side currently has RESTING (1 bp = 1e-4), so ONE tuning reads sanely on both a 0..1
    /// prediction market and a five-figure crypto book, with no tick grid needed:
    ///
    /// - `price_bps` — how far the target PRICE may drift from the resting price and still be left
    ///   alone (e.g. `25.0` = 25 bp = 0.25%).
    /// - `size_bps` — the same for the target SIZE. `0.0` means "no tolerance on this axis": any
    ///   size change at all still re-quotes (the conservative choice when the inventory skew is
    ///   shaping sizes).
    ///
    /// A side is skipped only when BOTH axes are inside tolerance. `with_refresh_tolerance(0.0,
    /// 0.0)` is INERT (never skips) — identical to leaving the gate off. OFF by default ⇒ the
    /// default maker re-quotes every tick exactly as before, byte-identical. The gate never touches
    /// the PULL path: a suppressed side is still canceled whatever the tolerance is.
    ///
    /// SIZING THE `size_bps` AXIS AGAINST THE SKEW: because `0.0` demands BIT-EXACT size equality,
    /// it only fires on a FIXED-size maker (`skew == 0.0`, i.e. no [`SpreadMaker::with_skew`]).
    /// With inventory skew active the sizes are recomputed as `qty · bid_multiplier`/`qty · ask_multiplier`
    /// from a CONTINUOUSLY varying position every tick, so the size axis would essentially never
    /// compare equal and the gate would never fire at all — a NON-ZERO `size_bps` is effectively
    /// REQUIRED there. Pick one that swallows the inventory jitter you don't want to chase while
    /// still re-quoting on a real re-size (and see [`RefreshTolerance`] for the matching ceiling on
    /// the price axis relative to `half_spread`).
    pub fn with_refresh_tolerance(mut self, price_bps: f64, size_bps: f64) -> Self {
        self.config.refresh_tolerance = Some(RefreshTolerance { price_bps, size_bps });
        self
    }

    /// Builder: enable LADDER quoting — rest `params.levels` rungs per side instead of one, stepping
    /// out from the same reservation price + half-spread the single quote uses (rung 0 IS today's
    /// quote; rung `k` sits `k · offset_step` further out at a [`LadderSizeProfile`](vike_model::LadderSizeProfile)-shaped
    /// size). The rungs carry tags `"bid0".."bidN"` / `"ask0".."askN"`, diff against the resting set
    /// each tick (minimal place/modify/cancel), honor the [`SpreadMaker::with_refresh_tolerance`] gate
    /// per rung, and — under the fill-rate breaker — a suppressed side pulls ALL its rungs (never
    /// strands one). COMPOSES with everything else: the style/A-S set rung 0's price, the skew shapes
    /// the base size every rung multiplies, own-order filtration (prefer
    /// [`SpreadMaker::with_own_order_book`], which tracks N orders) subtracts them. A `levels <= 1`
    /// bag is INERT — identical to leaving the ladder off — so `with_ladder(LadderParams { levels: 1,
    /// .. })` is today's single-quote maker, byte-identical. OFF by default. Rides the live-parameter
    /// plane, so `on_params_updated` can turn it on/off / re-shape it without a remount.
    pub fn with_ladder(mut self, params: LadderParams) -> Self {
        self.config.ladder = Some(params);
        self
    }

    /// Builder: enable own-order filtration backed by the full [`OrderBookOwn`](vike_model::OrderBookOwn) LADDER instead of
    /// the single `(price, size)` snapshot per side — and gate it on an **accepted-buffer** so a
    /// freshly-accepted order the public feed has not yet echoed is NOT double-subtracted from
    /// displayed depth. Implies [`SpreadMaker::with_own_order_filtration`] (it sets `filter_own`),
    /// so this one call is the whole opt-in. OFF by default ⇒ byte-identical to before.
    ///
    /// - `tick_size` — the venue grid own prices and public level prices are BOTH quantized on.
    ///   Pass the grid the maker actually quotes on; a non-positive value builds an INERT book that
    ///   subtracts nothing (the same no-grid rule `vike_market_making`'s `book::subtract_own` already follows). NOTE this
    ///   is the ladder's OWN fixed grid: unlike the snapshot path — which reads whatever
    ///   `tick_size` the live [`L2Book`](vike_marketdata::L2Book) carries — it does not adopt the book's grid per tick, so on
    ///   the L2 lane pass that venue's grid here explicitly.
    /// - `accepted_buffer` — how long after an accept our size is assumed NOT yet visible in the
    ///   public feed, in the SAME unit as the maker's event clock (epoch-MILLIS on the live
    ///   quote/book lanes). `0` trusts the accept immediately, which reduces this path to the
    ///   pre-existing snapshot behavior.
    ///
    /// The maker drives the ladder itself, keyed by its `"bid"`/`"ask"` order tags with an
    /// OPTIMISTIC ack (a `Strategy` never sees order-client-ids or venue accepts) — so the buffer
    /// is measured from SEND. A runtime that knows the tag↔order client id mapping can drive real venue
    /// accepts/cancels through [`SpreadMaker::own_book_mut`], after which the buffer covers
    /// market-data propagation alone. See `OwnBookFiltration::place` and `vike_model::OrderBookOwn`'s module doc
    /// for the full contract.
    pub fn with_own_order_book(mut self, tick_size: f64, accepted_buffer: i64) -> Self {
        self.own_book = Some(OwnBookFiltration::new(tick_size, accepted_buffer));
        self.config.filter_own = true;
        self
    }

    /// Read the own-order ladder, or `None` when [`SpreadMaker::with_own_order_book`] was not used.
    pub fn own_book(&self) -> Option<&OwnBookFiltration> {
        self.own_book.as_ref()
    }

    /// MUTABLE access to the own-order ladder — the seam a mount/runtime uses to drive REAL venue
    /// lifecycle events ([`OrderBookOwn::on_submit`](vike_model::OrderBookOwn::on_submit)/[`on_accepted`](vike_model::OrderBookOwn::on_accepted)/
    /// [`on_partial_fill`](vike_model::OrderBookOwn::on_partial_fill)/
    /// [`on_cancel_pending`](vike_model::OrderBookOwn::on_cancel_pending)/
    /// [`on_terminal`](vike_model::OrderBookOwn::on_terminal)) into the book, replacing the maker's optimistic
    /// stance with the venue's actual acks. `None` when the ladder is not enabled.
    pub fn own_book_mut(&mut self) -> Option<&mut OwnBookFiltration> {
        self.own_book.as_mut()
    }

    /// Builder: enable the Avellaneda–Stoikov PRICING layer (audit mm-quote). The two quote PRICES
    /// then come from an inventory-aware reservation price `r = s − q_norm·γ·V` and optimal
    /// half-spread `δ = ½·[γ·V + (2/γ)·ln(1+γ/κ)]`, adapted to Polymarket's 0–1 bounded prices by the
    /// bounded variance `V = min(σ̂²·H, p(1−p))` (all three modeling choices are [`AsParams`] knobs).
    /// Its `(bid, ask)` output flows UNCHANGED into the existing size-skew → fill-rate breaker →
    /// own-order filtration → modify-in-place tail, so A-S inherits the adverse-selection guard it
    /// classically lacks. Leaving this OFF (the default) keeps the maker byte-identical to before.
    /// `tick_size` (for the grid snap / wall clamp) is taken from the L2 book on the book lane, and
    /// from the configured `tick_size` (set via [`SpreadMaker::with_quote_style`]) on the L1 lane.
    pub fn with_avellaneda_stoikov(mut self, params: AsParams) -> Self {
        self.as_state = Some(AsState::new(params));
        self
    }

    /// Builder: override the pricing [`SpreadModel`] on an already-built A-S maker (e.g. the
    /// `gueant_maker` registry alias forces [`SpreadModel::Gueant`] so the same `spread_maker` knobs
    /// price with the GLFT closed form). No-op if the maker has no A-S state.
    pub fn with_spread_model(mut self, model: SpreadModel) -> Self {
        if let Some(st) = self.as_state.as_mut() {
            st.params.spread_model = model;
        }
        self
    }

    /// Builder: enable LIQUIDITY-REWARDS-aware quoting — fold a reward-EV term into the quote so it
    /// stays in a venue's reward band at `min_size` on both sides, `weight`-deep toward the mid,
    /// holding an in-band quote at least `min_order_age_ms` so it does not churn below the reward
    /// floor. See [`SpreadMakerParams::reward`] / [`RewardParams`]. `with_liquidity_rewards(RewardParams {
    /// weight: 0.0, .. })` is INERT (OFF), identical to not calling it. OFF by default ⇒ the maker is
    /// byte-identical to before. The A-S blackout / fill-rate breaker stay the safety authority.
    pub fn with_liquidity_rewards(mut self, params: RewardParams) -> Self {
        self.config.reward = Some(params);
        self
    }

    /// Builder: enable the FLOW-TOXICITY guard (RTDS wallet-toxicity, 5c) — react to the per-side
    /// toxic-flow reading routed in via [`Strategy::on_flow`](vike_model::Strategy::on_flow) by WIDENING the toxic side away from mid
    /// (`tox · widen · half_spread`) and CUTTING its size (`× (1 − tox · size_cut).max(0)`), the
    /// size-to-zero case routing through the maker's existing per-side suppression PULL path.
    /// `with_flow_toxicity(ToxicityParams { widen: 0.0, size_cut: 0.0 })` is INERT (OFF), identical to
    /// not calling it. OFF by default ⇒ the maker is byte-identical to before, and it stays inert until
    /// a reading actually arrives. Rides the live-parameter plane like every other knob. See
    /// [`ToxicityParams`].
    pub fn with_flow_toxicity(mut self, params: ToxicityParams) -> Self {
        self.config.toxicity = Some(params);
        self
    }

    /// This side's [`SideState`] (`is_bid` selects [`SpreadMaker::bid`]/[`SpreadMaker::ask`]) —
    /// the read half of the per-side indexing every collapsed `is_bid`-parameterized helper uses.
    fn side(&self, is_bid: bool) -> &SideState {
        if is_bid { &self.bid } else { &self.ask }
    }

    /// Mutable twin of [`SpreadMaker::side`].
    fn side_mut(&mut self, is_bid: bool) -> &mut SideState {
        if is_bid { &mut self.bid } else { &mut self.ask }
    }

    /// The breaker engages only when ALL three knobs are positive; any non-positive value leaves
    /// it OFF, so the strategy reduces EXACTLY to the skew-only maker (no fill tracking, no
    /// suppression). This is the single gate every breaker code path checks.
    fn breaker_enabled(&self) -> bool {
        self.config.fill_window_ms > 0
            && self.config.net_fill_threshold > 0.0
            && self.config.suppress_cooldown_ms > 0
    }

    /// The time-accelerated fill-rate-breaker pull MULTIPLIER for the current event ts (Group-B, PR-3)
    /// — the factor [`Strategy::on_fill`](vike_model::Strategy::on_fill) DIVIDES the effective net-fill threshold by so the breaker
    /// trips sooner as τ = (resolution_ts − event_ts) → 0 near a binary resolution. Exactly `1.0`
    /// (inert, so the threshold is unchanged bit-for-bit) unless the A-S layer is on with a known
    /// `resolution_ts` AND `pull_accel_ramp_ms > 0`; see [`accelerated_pull`], which
    /// guarantees the result is always `>= 1.0`. The `pull_accel_*` knobs live on [`AsParams`], so a
    /// breaker-only maker with no A-S layer reads `1.0` here and is byte-identical.
    fn pull_accel_multiplier(&self, event_ts: i64) -> f64 {
        let Some(params) = self.as_state.as_ref().map(|st| &st.params) else {
            return 1.0;
        };
        if params.pull_accel_ramp_ms <= 0 {
            return 1.0;
        }
        let Some(t_res) = params.resolution_ts else {
            return 1.0;
        };
        accelerated_pull(t_res - event_ts, params.pull_accel_ramp_ms, params.pull_accel_max)
    }

    /// Mirror a place/re-price of the tagged quote into the own-order ladder. While the ladder is
    /// `None` (the default) this is one `Option` test and a return — no state is touched, which is
    /// what keeps the requote tail byte-identical. See [`OwnBookFiltration::place`] for the
    /// optimistic-ack semantics.
    fn own_book_place(&mut self, tag: &str, side: OrderBookOwnSide, price: f64, qty: f64, ts: i64) {
        if let Some(own) = self.own_book.as_mut() {
            own.place(tag, side, price, qty, ts);
        }
    }

    /// Mirror a PULL (suppression cancel) of the tagged quote into the own-order ladder. No-op
    /// while the ladder is `None`.
    fn own_book_pull(&mut self, tag: &str) {
        if let Some(own) = self.own_book.as_mut() {
            own.pull(tag);
        }
    }

    /// Feed one own-order OUTCOME into the A-S κ MLE's own-fill tape, for
    /// [`KappaMode::OwnFillFit`](vike_model::KappaMode). A GUARDED no-op unless the A-S layer is on
    /// AND its `kappa_mode` is `OwnFillFit` — so every other maker (default / `Fixed` / `LiveFit`) is
    /// byte-identical, this whole path being inert.
    ///
    /// `is_bid` picks which resting snapshot + placement clock the `(δ, exposure)` is read from;
    /// `filled` marks a fill vs a censored (breaker-pulled) exposure. δ is the resting quote's
    /// distance to the A-S fair mid it was last priced against (`last_quote_mid`); exposure is the
    /// event-time it rested since its last place/re-price (`*_quoted_ts`). Skipped when that side has
    /// no tracked resting order, no placement clock, or no fair mid yet.
    fn feed_own_outcome(&mut self, is_bid: bool, filled: bool, ts: i64) {
        let side = self.side(is_bid);
        let (resting, placed_timestamp) = (side.own, side.quoted_timestamp);
        let Some((rpx, _)) = resting else { return };
        if placed_timestamp <= 0 {
            return;
        }
        let Some(as_state) = self.as_state.as_mut() else { return };
        if as_state.params.kappa_mode != KappaMode::OwnFillFit {
            return;
        }
        let Some(mid) = as_state.last_quote_mid else { return };
        let delta = (rpx - mid).abs();
        let exposure = (ts - placed_timestamp).max(0) as f64;
        as_state.record_own_outcome(delta, exposure, filled, ts);
    }

    /// Snapshot the maker's current live tunables as a [`SpreadMakerParams`] — the READ side of the
    /// live-parameter plane (e.g. for a GUI to seed its tuning editor). This is exactly the bag
    /// [`Strategy::on_params_updated`](vike_model::Strategy::on_params_updated) consumes.
    pub fn params(&self) -> SpreadMakerParams {
        SpreadMakerParams {
            // The A-S bag's authority is the live `as_state.params` (see `config`'s doc — the copy in
            // `config` is pinned `None`), re-attached here on the way out.
            avellaneda_stoikov: self.as_state.as_ref().map(|st| st.params),
            ..self.config
        }
    }

    /// Hot-swap ALL live tunables ATOMICALLY from `p` and bump [`SpreadMaker::params_epoch`]. Only
    /// the CONFIG knobs change — the per-side [`SideState`]s (resting orders/rungs, staleness,
    /// moas clocks, suppression deadlines) and the fill window are DELIBERATELY left untouched, so resting orders +
    /// queue position survive a re-tune: the next `requote` re-prices them IN PLACE via modify (never
    /// cancel/replace, and a no-move re-price doesn't disturb the book at all). Runs on the single
    /// core thread between ticks, so the whole swap is atomic w.r.t. tick dispatch.
    fn apply_params(&mut self, p: &SpreadMakerParams) {
        // ONE struct copy swaps every plain knob (audit F10) — and by construction it can ONLY
        // touch config: all runtime state (the per-side [`SideState`]s — resting orders, rungs,
        // moas clocks, suppression deadlines — plus the fill window, `last_flow`, `own_book`,
        // `underlying`, `learned_book_tick`) lives OUTSIDE `config`, so the swap leaves it alone by
        // type-shape, not by hand-copied omission. Consequences, unchanged from the field-by-field
        // era: a re-tune keeps resting orders + queue position (the next tick re-prices in place);
        // resting ladder RUNGS are reconciled to the new shape on the next `requote` (re-price
        // kept rungs, submit new ones, cancel beyond the new count — turning the ladder off
        // retires them on the next tick, never here); reward re-shapes take effect next tick with
        // the accrued moas age kept; a toxicity re-tune keeps whatever `on_flow` last delivered.
        // No order op ever fires on a bare re-tune. The A-S sub-bag is STRIPPED (pinned `None`,
        // see `config`'s doc) — it is applied to the live `as_state` below instead.
        self.config = SpreadMakerParams { avellaneda_stoikov: None, ..*p };
        // A-S plane: a re-tune PRESERVES the warm σ̂²/κ estimators (like the breaker window / resting
        // orders) — only the params swap. `Some` on a live maker updates in place; `Some` on an
        // A-S-off maker turns it on; `None` turns A-S off.
        match p.avellaneda_stoikov {
            Some(ap) => match self.as_state.as_mut() {
                Some(st) => st.set_params(ap),
                None => self.as_state = Some(AsState::new(ap)),
            },
            None => self.as_state = None,
        }
        self.params_epoch += 1;
    }
}

// `impl<B: OrderBroker> Strategy<B> for SpreadMaker` — the tick/fill/params lanes, on the order seam
// (`place`, `amend`, `cancel`, `position("")`, decision 0277's step 6).
//
// Also owns the durable-state DTO (portfolio-observer PR-4 T4): [`SpreadMaker::save_state`]/
// [`SpreadMaker::load_state`] persist the fill-rate breaker's two suppression deadlines, (when
// Avellaneda–Stoikov is enabled) the online estimator's accumulators, and each side's RESTING
// single quote, so a crash-restart no longer un-trips a tripped breaker, re-learns σ̂²/κ from cold
// or quotes a second bid/ask beside the one the previous session left resting. See
// [`SpreadMakerStateV1`] for exactly what is (and isn't) persisted.

/// Wire version this build writes and accepts. Bump on any breaking shape change to
/// [`SpreadMakerStateV1`]/[`AsStateV1`]; [`SpreadMaker::load_state`] ignores (fail-open, warns)
/// any other value rather than guessing at a migration — mirrors vike-data's
/// `datafusion_hist::manifest::MANIFEST_FORMAT` versioning shape.
const SPREAD_MAKER_STATE_VERSION: u32 = 1;

/// Durable-state wire DTO for [`SpreadMaker`] (portfolio-observer PR-4 T4). A DEDICATED type
/// (never `#[derive(Serialize, Deserialize)]` directly on the live `SpreadMaker`/`AsState`
/// structs) so the wire format is decoupled from the internal representation — the maker's live
/// fields can be renamed/reshaped freely without a version bump, since only this struct's shape is
/// a compatibility contract.
///
/// Persists the per-side fill-rate breaker's suppression deadlines, (if A-S is enabled) the online
/// estimator's accumulators, and each side's RESTING SINGLE QUOTE ([`SideQuoteV1`]) — the state a
/// crash-restart would otherwise silently reset. Deliberately EXCLUDES:
/// - `fills` (the raw sliding-window fill tape): the suppression DEADLINES are its behavioral
///   effect — replaying the raw window isn't needed to reproduce a tripped/untripped side.
/// - `SideState::rungs` (the ladder's per-rung snapshot) and the optional own-order ladder
///   (`own_book`): neither is reachable from any binary's default config, and a rung's tag is its
///   INDEX, so restoring one without the others would renumber tags under live orders. A restarted
///   LADDER maker re-places its rungs.
/// - `AsState::params`/`alpha`: come fresh from the live config/`AsParams` at mount time (a
///   restart with a DIFFERENT tuning should price with the new tuning, not a stale saved one).
///
/// ⚠ **Nothing rediscovers a resting order for the maker.** An earlier version of this doc said the
/// per-side order cache was "rediscovered by the runtime's reconcile pass"; it is not — a LIVE
/// mount's reconcile pass reports divergence and folds none of it into a strategy, so a maker that
/// booted with `placed = false` quoted a NEW bid/ask at its first tick while the old order kept
/// resting (and, with the core's restored tag registry, the re-submit overwrote the tag entry and
/// orphaned the old order). The maker therefore persists what it needs to CONTINUE the quote: the
/// core puts the order and its tag back, the maker modifies/cancels it by that tag and hears its
/// terminal event, exactly as in the session that placed it.
///
/// ADDITIVE on the wire: `bid`/`ask` are `#[serde(default)]`, so a file written before they existed
/// still loads (both sides unplaced), and `v` is not bumped.
#[derive(Debug, Serialize, Deserialize)]
struct SpreadMakerStateV1 {
    /// Wire version; anything other than [`SPREAD_MAKER_STATE_VERSION`] is ignored (fail-open).
    v: u32,
    bid_suppressed_until: i64,
    ask_suppressed_until: i64,
    as_state: Option<AsStateV1>,
    /// The bid side's resting quote; absent in a file from before it was persisted.
    #[serde(default)]
    bid: SideQuoteV1,
    /// The ask side's resting quote — the mirror of `bid`.
    #[serde(default)]
    ask: SideQuoteV1,
}

/// Wire DTO for one side's resting single quote: exactly what a post-restart tick needs and no
/// more. `own` is the maker's INTENDED `(price, size)` — the pair `refresh_skips` and the reward
/// moas gate compare a re-price against, and what single-snapshot own-order filtration subtracts —
/// not a venue handle (the maker never sees an order-client-id; the core resolves the tag).
/// `quoted_timestamp` is the placement clock (moas age, OwnFillFit exposure). `refresh_stale` is NOT
/// persisted: a restored side is always stale (see [`SpreadMaker::restore_side`]).
#[derive(Debug, Default, Serialize, Deserialize)]
struct SideQuoteV1 {
    #[serde(default)]
    placed: bool,
    #[serde(default)]
    own: Option<(f64, f64)>,
    #[serde(default)]
    quoted_timestamp: i64,
}

impl SideQuoteV1 {
    /// The wire form of `side`. A snapshot with a non-finite number is dropped (JSON has no NaN, and
    /// one such value would make the whole payload unparsable), which leaves the side unplaced.
    fn of(side: &SideState) -> Self {
        match side.own {
            Some((px, qty)) if side.placed && px.is_finite() && qty.is_finite() => {
                Self { placed: true, own: Some((px, qty)), quoted_timestamp: side.quoted_timestamp }
            }
            _ => Self::default(),
        }
    }
}

/// Wire DTO for `AsState`'s persisted accumulators — everything EXCEPT `params`/`alpha` (see
/// [`SpreadMakerStateV1`]'s doc for why). Field-for-field the `AsState` accumulators
/// (`avellaneda.rs`), with `trades` a `Vec` on the wire (JSON has no `VecDeque`; converted at the
/// `save_state`/`load_state` boundary).
#[derive(Debug, Serialize, Deserialize)]
struct AsStateV1 {
    sigma2: Option<f64>,
    last_mid: Option<(f64, i64)>,
    last_quote_mid: Option<f64>,
    trades: Vec<(f64, f64, i64)>,
    sum_w: f64,
    sum_w_delta: f64,
}

impl<B: OrderBroker> Strategy<B> for SpreadMaker {
    /// L1 quote lane: build a one-level-per-side [`BookView`] from the touch and run the shared
    /// `requote`. The grid is the venue tick LEARNED from the L2 book lane (`learned_book_tick`) when
    /// one has been seen, else the configured `tick_size` param (pure-L1 fallback) —
    /// so the `*_half_spread_ticks` floor/cap and grid snap resolve against the SAME tick the book
    /// lane uses, never a divergent one. Byte-identical when the param already matches the book grid
    /// (the correct config). With the default (`Mid`, no filtration) this is the original
    /// `mid ± half_spread` maker.
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        let tick = self.learned_book_tick.unwrap_or(self.config.tick_size);
        let book = BookView::from_quote(q, tick);
        self.requote(broker, &book, q.timestamp);
    }

    /// L2 book lane (R8 HFT track): build a [`BookView`] from the top of the [`L2Book`] and run the
    /// SAME `requote`. This is where `Depth` gets real levels and own-order filtration can shift the
    /// derived best. We take `depth_levels + 2` levels per side — enough to index the `Depth` level
    /// AND still expose a genuine best after filtration removes our own top-of-book level. The tick
    /// ts is the mount's `now` (the runtime stamps it on the tick context).
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        // Learn the venue's REAL grid from the book and adopt it on the L1 quote lane too (see
        // `learned_book_tick`) — so one `*_half_spread_ticks` can't resolve to two different spreads.
        if book.tick_size > 0.0 {
            self.learned_book_tick = Some(book.tick_size);
        }
        let view = BookView::from_l2(book, self.config.depth_levels + 2);
        self.requote(broker, &view, broker.now());
    }

    /// Trade-tape lane for the A-S κ fit (audit mm-quote): when A-S is enabled, fold each executed
    /// trade's size-weighted distance-from-mid into the online intensity estimator's running sums
    /// over an event-time window (O(1) amortised). A NO-OP when A-S is off (`as_state` is `None`) —
    /// the default maker never had an `on_trade_tick`, so this stays additive. The κ fit is only
    /// USED to price in [`KappaMode::LiveFit`](vike_model::KappaMode::LiveFit); in the default
    /// [`KappaMode::Fixed`](vike_model::KappaMode::Fixed) the tape is kept warm but the fixed
    /// `κ_default` prices.
    fn on_trade_tick(&mut self, _broker: &mut B, t: &TickTrade) {
        if let Some(as_state) = self.as_state.as_mut() {
            as_state.record_trade(t);
        }
    }

    /// Underlying-mark lane ("Option B" cross-symbol routing): the runtime routes a mark of the
    /// DIFFERENT symbol this maker's A-S underlying-anchored fair mid / ATM guard WATCH (e.g. the
    /// `btcusdt` RTDS spot for a BTC up/down PM token) here. It folds the mark into the
    /// [`UnderlyingTracker`] and, once that has a window-open
    /// reference + a σ estimate, feeds the derived `(s_now, s_open, sigma_per_sec)` into the A-S state
    /// via `set_underlying`. A NO-OP when A-S is off (`as_state` is `None`) — every non-A-S maker is
    /// byte-identical, the trait's default `on_mark` being a no-op — and INERT until the A-S
    /// `underlying_weight` / `atm_blackout_scale` knobs are raised (a stored underlying that no knob
    /// reads changes no quote). The broker is intentionally UNTOUCHED (no order placed here): the
    /// anchor takes effect on the NEXT quote/book tick's `requote`.
    fn on_mark(&mut self, _broker: &mut B, mark: &TickMark) {
        // Read the A-S window params (and bail if A-S is off) WITHOUT holding the borrow across the
        // `self.underlying` update below — `as_state` and `underlying` are sibling fields of `self`,
        // so the mutable-borrow overlap must be broken by copying the two params out first.
        let (window_secs, resolution_ts) = match self.as_state.as_ref() {
            Some(st) => (st.params.window_secs, st.params.resolution_ts),
            None => return,
        };
        if let Some((s_now, s_open, sigma)) =
            self.underlying.observe(mark.price, mark.timestamp, window_secs, resolution_ts)
            && let Some(as_state) = self.as_state.as_mut()
        {
            as_state.set_underlying(s_now, s_open, sigma);
        }
    }

    /// Flow-toxicity lane (RTDS wallet-toxicity guard, 5c): STORE the latest per-side toxic-flow
    /// reading and return — the reaction (widen + size-cut on the toxic side) is applied at the NEXT
    /// quote in `requote`, not here, so no order is placed/canceled from this hook (the broker is
    /// intentionally UNTOUCHED, mirroring `on_mark`). INERT until the maker's `toxicity` guard
    /// is configured with a non-zero knob: a stored reading no active knob reads changes no quote, so a
    /// maker without the guard is byte-identical to before (the trait's default `on_flow` is a no-op).
    fn on_flow(&mut self, _broker: &mut B, flow: FlowToxicity) {
        self.last_flow = Some(flow);
        // Mark the reading EXTERNAL so the OFI-toxicity synthesis fallback (Group-B, PR-3) never
        // clobbers it: once any real `on_flow` has arrived, the external feed takes precedence
        // permanently. Inert on a maker with no toxicity guard / `ofi_toxicity_scale == 0` (the
        // synthesis this gates never runs there anyway).
        self.flow_external = true;
    }

    /// Adverse-selection guard (audit mm1): record every fill into the sliding EVENT-TIME window
    /// and, when the NET one-directional accumulation on one side trips the threshold, arm that
    /// side's cooldown. `requote` (from either the quote or book lane) acts on the arm (pulls +
    /// withholds the side). Round-trip netting lives entirely in `net_signed_fills`: an offset
    /// bid/ask pair nets to ~0 and never arms. A no-op (and no allocation/tracking) when the breaker
    /// is disabled, so a maker without `with_fill_breaker` behaves exactly as before (the trait's
    /// default `on_fill` was a no-op).
    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        // Order-refresh TOLERANCE invalidation — deliberately BEFORE the breaker's early return,
        // because it is independent of the breaker: the filled side's resting snapshot
        // (`own_bid`/`own_ask`) is the maker's INTENDED quote, while a PARTIAL fill leaves a
        // SMALLER remainder at the venue. Left alone, the intended-vs-target comparison would read
        // "no change" and the size top-up would be silently lost, so the maker would quote less
        // size than configured. Marking the side stale forces the next tick through the modify arm
        // (which re-issues the full configured size) exactly once. INERT when the gate is off: the
        // flag is only ever read by `refresh_skips`, which short-circuits while `refresh_tolerance`
        // is `None` — so a maker without the gate is byte-identical to before.
        if fill.side != 0 {
            let side = self.side_mut(fill.side > 0);
            side.refresh_stale = true;
            // A fill on this side can only come from the one order we hold there, so a RESTORED
            // side that fills is confirmed alive (see `SideState::unverified`).
            side.unverified = None;
        }
        // OwnFillFit κ MLE: fold this fill as a FILLED own-order outcome. A guarded no-op unless A-S
        // is on AND kappa_mode is OwnFillFit (see `SpreadMaker::feed_own_outcome`), so every other
        // maker is byte-identical. Deliberately BEFORE the breaker early-return: OwnFillFit does not
        // require the fill-rate breaker to be enabled.
        if fill.side != 0 {
            self.feed_own_outcome(fill.side > 0, true, fill.timestamp);
        }
        if !self.breaker_enabled() {
            return;
        }
        // record this fill at its EVENT ts, then evict everything older than the window (the fill
        // ts is "now" — the window rides the latest fill, matching the runtime's event clock)
        self.fills.push_back(FillRec {
            side: fill.side,
            size: fill.size,
            timestamp: fill.timestamp,
        });
        let cutoff = fill.timestamp - self.config.fill_window_ms;
        while self.fills.front().is_some_and(|f| f.timestamp < cutoff) {
            self.fills.pop_front();
        }
        // net over the window ending at THIS fill's event time; a one-sided run survives the sum,
        // a round-trip cancels. Arm (or refresh) the over-hit side's cooldown from the event ts.
        let net = net_signed_fills(&self.fills, fill.timestamp, self.config.fill_window_ms);
        // ACCELERATED PULL (Group-B, PR-3): near a binary resolution DIVIDE the effective net-fill
        // threshold by the time-acceleration multiplier so the breaker trips SOONER as τ → 0 (a larger
        // multiplier ⇒ a lower effective threshold ⇒ trips on less one-directional fill). The pure
        // `net_signed_fills` above is UNTOUCHED — the multiplier is applied ONLY at this comparison
        // site. `pull_accel_multiplier` is `>= 1.0` always and exactly `1.0` when the knobs are off (or no
        // `resolution_ts` is known), so `net_fill_threshold / 1.0 == net_fill_threshold` bit-for-bit and
        // the default path compares against the unchanged threshold.
        let eff_threshold =
            self.config.net_fill_threshold / self.pull_accel_multiplier(fill.timestamp);
        if net >= eff_threshold {
            self.bid.suppressed_until = fill.timestamp + self.config.suppress_cooldown_ms;
        } else if net <= -eff_threshold {
            self.ask.suppressed_until = fill.timestamp + self.config.suppress_cooldown_ms;
        }
    }

    /// Order-DEATH lane: when one of this maker's own tagged quotes reaches a terminal state, free
    /// the [`SideState`] slot it was resting in, so the next tick's
    /// `requote_single_side` takes the PLACE arm instead of the re-price arm.
    ///
    /// ## ⚠ The wedge this exists to close
    ///
    /// `requote_single_side` is a three-arm gate on `SideState::placed`, and `placed` was written in
    /// exactly three places — its own suppression arm, its own place arm, and the ladder-mode
    /// `retire_single_order`. NOTHING cleared it when the order died at the VENUE. So after a full
    /// fill `placed` stayed `true`, every later tick took the modify arm, the runtime resolved the
    /// tag to the now-terminal order-client-id, and `vike_execution::Engine::modify_order`'s
    /// not-modifiable early return swallowed it — no error, no event, no ring line. That side never
    /// quoted again for the life of the process. Measured on the the CI box live mount: `orders:2` was a
    /// permanent ceiling and `working` sat at `0` across every session.
    ///
    /// The maker could not fix this alone, and that is the point: it never sees an order-client-id
    /// (the tag registry is the runtime's), so until [`OrderEvent::tag`] existed there was no
    /// name in this event it could match against anything it owns. Guessing from `on_fill` instead
    /// was not an option either — [`Fill`] carries no order identity and no remaining quantity, so
    /// "was that the whole order" is unanswerable from it, and answered wrong by any size heuristic
    /// on a partial-fill sequence.
    ///
    /// ## What it covers
    ///
    /// ALL THREE ways a single quote dies, through the one hook: a full FILL
    /// ([`OrderEventKind::Filled`]), a CANCEL/EXPIRY (venue-side or external), and a
    /// REJECT/`RiskGate` DENY — the last of which wedged the maker just as hard as a fill, because
    /// the place arm sets `placed` the moment it buffers the submit, before any verdict.
    /// [`OrderEventKind::Accepted`] is non-terminal and frees nothing: on a side that is placed it
    /// only confirms the order alive, and on a side that holds nothing it ADOPTS the order (below).
    ///
    /// ## Adoption: the crash window `save_state` cannot close
    ///
    /// A tagged NON-terminal single-quote event on a side with `placed == false` names a live order
    /// this maker does not know it has: after a restart the core restored the tag and told us at
    /// the first reconcile pass (a synthetic tagged `Accepted`), but the saved state predates the
    /// order (it was placed after the last `save_state`, then the daemon died). Ignoring it made the
    /// first tick submit a SECOND order under the same tag and orphan the older one. So the side is
    /// ADOPTED: `placed`, price/size unknown (`own = None`), `refresh_stale` so the next tick takes
    /// the modify-by-tag arm and re-prices price AND full size once. The ladder (`"bid0"`..) is
    /// not covered, as above.
    ///
    /// ⚠ A side adopted and not yet re-priced saves as UNPLACED (`SideQuoteV1::of` needs a quote),
    /// so a save in that one-tick window re-opens the gap; the persisted shape is unchanged.
    ///
    /// ## The busy refusal (P1)
    ///
    /// A place under a tag whose older order still works is refused by both brokers, a tagged
    /// `Denied` ([`OrderEvent::is_tag_busy_refusal`]). It is terminal for the order never sent,
    /// not for the side: the older order rests under the tag. So it ADOPTS the side exactly as the
    /// non-terminal event above does, and the next tick amends that order instead of placing again,
    /// which would be refused on every tick. No path of this maker places on a working tag (a side
    /// is placed only while it holds nothing), so the arm is a guard. The simulator sends the
    /// refusal and every end of a quote as live does, on its tick lane too, so a backtest frees a
    /// filled side here and re-quotes it on the next tick, as live does.
    ///
    /// ⚠ **The LADDER path is NOT covered.** A rung's tag is `"bid{k}"`/`"ask{k}"` where `k` is its
    /// INDEX into `SideState::rungs`, so forgetting one rung would renumber every deeper rung's tag
    /// away from the order actually resting under it — and rebuilding the whole side instead would
    /// churn queue position on every fill, which is most of what a laddered maker is trying to keep.
    /// Untangling that needs `rungs` to hold a per-rung liveness slot rather than a dense vec, which
    /// is a reshape of the ladder diff and not this fix. A laddered maker therefore still wedges
    /// per-rung; the ladder is off by default (`levels >= 2`), and the single-quote path — the one
    /// the live mount runs — is closed.
    ///
    /// ## What it deliberately does NOT do
    ///
    /// It feeds NO own-order outcome into the OwnFillFit κ MLE. A `Filled` is already fed as a
    /// FILLED observation by [`Strategy::on_fill`], and a cancel the maker itself issued is already
    /// fed as CENSORED by `requote_single_side`'s suppression arm (which clears `placed` first, so
    /// this hook early-returns on it) — feeding again would double-count the same exposure. An
    /// EXTERNALLY-originated cancel/reject records no observation at all, which is the conservative
    /// choice for an estimator and is in any case strictly more than the nothing it recorded while
    /// the side was wedged.
    ///
    /// [`Fill`]: vike_model::Fill
    /// [`OrderEventKind::Accepted`]: vike_model::OrderEventKind::Accepted
    /// [`OrderEventKind::Filled`]: vike_model::OrderEventKind::Filled
    /// [`OrderEvent::tag`]: vike_model::OrderEvent::tag
    fn on_order_event(&mut self, _broker: &mut B, event: &OrderEvent) {
        // Only the SINGLE-quote tags this maker owns. A ladder rung (`"bid0"`), a foreign
        // strategy's tag, or an untagged order (`None`) falls through untouched — an exact match,
        // never a prefix, so `"bid0"` can never be mistaken for `"bid"`.
        let is_bid = match event.tag.as_deref() {
            Some("bid") => true,
            Some("ask") => false,
            _ => return,
        };
        if event.is_tag_busy_refusal() {
            // The place was refused because the tag still names a working order (P1): the side is
            // that order's, so it is ADOPTED and amended next tick, never placed again and refused
            // on every tick. See "The busy refusal" above.
            self.adopt_unplaced_side(is_bid);
            return;
        }
        if !event.kind.is_terminal() {
            // A tagged non-terminal event (the core resolved this tag against its registry) is a
            // sign of life for a RESTORED side — it need not be pulled and re-placed. Nothing else
            // changes: the side stays resting. On a side this maker holds NOTHING for, the same
            // event is an ADOPTION (see `SpreadMaker::adopt_unplaced_side`).
            if !self.side(is_bid).placed {
                self.adopt_unplaced_side(is_bid);
            }
            self.side_mut(is_bid).unverified = None;
            return;
        }
        if !self.side(is_bid).placed {
            return; // already free — this maker pulled it itself, or a duplicate terminal
        }
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
        side.quoted_timestamp = 0;
        side.unverified = None;
        // Mirror into the own-order ladder exactly as the suppression PULL does, so own-filtration
        // stops subtracting a quote that is no longer on the book. A no-op while the ladder is
        // `None` (the default), which is what keeps every non-ladder maker byte-identical.
        self.own_book_pull(if is_bid { "bid" } else { "ask" });
    }

    /// Live-parameter plane: hot-swap this maker's tunables from a [`StrategyParams::SpreadMaker`]
    /// update WITHOUT unmounting (the runtime routes it here off `Command::UpdateParams`). The
    /// broker is intentionally UNTOUCHED — no order is placed/canceled/modified here; the new knobs
    /// take effect on the NEXT quote/book tick, where `requote` re-prices the resting bid/ask IN
    /// PLACE (modify, not cancel/replace), so queue position is preserved and a re-tune that doesn't
    /// move the price doesn't disturb the book at all. The maker consumes ONLY its own
    /// [`StrategyParams::SpreadMaker`] arm; a foreign variant (e.g. the position-executor
    /// [`StrategyParams::PositionController`]) is IGNORED — matching one's own variant and ignoring
    /// the rest is the live-params contract (`Strategy::on_params_updated`).
    fn on_params_updated(&mut self, _broker: &mut B, params: &StrategyParams) {
        if let StrategyParams::SpreadMaker(p) = params {
            self.apply_params(p);
        }
    }

    /// The READ half of the same plane: route the maker's own [`SpreadMaker::params`] accessor out
    /// through the trait, so the runtime can publish this mount's live tunables on its snapshot. The
    /// accessor is AUTHORITATIVE by construction — it reads the fields the next tick will price
    /// from, and re-attaches the A-S bag from the live `as_state` (which `set_params` may have
    /// clamped) rather than echoing whatever the last update carried.
    fn params(&self) -> Option<StrategyParams> {
        Some(StrategyParams::SpreadMaker(SpreadMaker::params(self)))
    }

    /// Durable-state SAVE (portfolio-observer PR-4 T4): see [`SpreadMakerStateV1`] for exactly
    /// what is (and isn't) persisted. Always `Some` — unlike the trait's `None`-by-default (a
    /// strategy with genuinely nothing to save), a `SpreadMaker` always has at least the two
    /// (possibly-zero) breaker deadlines worth round-tripping.
    fn save_state(&self) -> Option<serde_json::Value> {
        let dto = SpreadMakerStateV1 {
            v: SPREAD_MAKER_STATE_VERSION,
            bid_suppressed_until: self.bid.suppressed_until,
            ask_suppressed_until: self.ask.suppressed_until,
            bid: SideQuoteV1::of(&self.bid),
            ask: SideQuoteV1::of(&self.ask),
            as_state: self.as_state.as_ref().map(|s| AsStateV1 {
                sigma2: s.sigma2,
                last_mid: s.last_mid,
                last_quote_mid: s.last_quote_mid,
                trades: s.trades.iter().copied().collect(),
                sum_w: s.sum_w,
                sum_w_delta: s.sum_w_delta,
            }),
        };
        serde_json::to_value(&dto).ok()
    }

    /// Durable-state LOAD (portfolio-observer PR-4 T4). FAIL-OPEN: a payload that doesn't parse
    /// as [`SpreadMakerStateV1`], or whose `v` isn't [`SPREAD_MAKER_STATE_VERSION`], is warned and
    /// ignored — this maker keeps its freshly-constructed state rather than panicking or
    /// partially applying a shape it doesn't recognize.
    ///
    /// On a recognized payload: the two breaker deadlines always restore. The A-S accumulators
    /// restore ONLY when BOTH this (live, freshly-mounted) maker has A-S enabled (`self.as_state`
    /// is `Some` — this config wants A-S) AND the saved payload has an `as_state` too —
    /// overwriting just the accumulator fields on the existing `AsState`, so its live
    /// `params`/`alpha` (already set from the current config) are untouched. Either side being
    /// `None` (A-S off in this config, or the saved maker never had A-S on) skips the A-S restore
    /// entirely, leaving the estimator fresh — it never turns A-S on/off as a side effect.
    fn load_state(&mut self, state: &serde_json::Value) {
        let dto: SpreadMakerStateV1 = match serde_json::from_value(state.clone()) {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(
                    %err,
                    "SpreadMaker::load_state: unparsable saved state, keeping fresh state"
                );
                return;
            }
        };
        if dto.v != SPREAD_MAKER_STATE_VERSION {
            tracing::warn!(
                found_version = dto.v,
                expected_version = SPREAD_MAKER_STATE_VERSION,
                "SpreadMaker::load_state: saved state version mismatch, keeping fresh state"
            );
            return;
        }
        self.bid.suppressed_until = dto.bid_suppressed_until;
        self.ask.suppressed_until = dto.ask_suppressed_until;
        self.restore_side(true, dto.bid);
        self.restore_side(false, dto.ask);
        if let (Some(live), Some(saved)) = (self.as_state.as_mut(), dto.as_state) {
            live.sigma2 = saved.sigma2;
            live.last_mid = saved.last_mid;
            live.last_quote_mid = saved.last_quote_mid;
            live.trades = saved.trades.into_iter().collect();
            live.sum_w = saved.sum_w;
            live.sum_w_delta = saved.sum_w_delta;
        }
    }
}

impl SpreadMaker {
    /// ADOPT a live order the core named by this side's single-quote tag while the side held
    /// nothing (see [`Strategy::on_order_event`]'s "Adoption"), or that a busy refusal says still
    /// holds the tag ("The busy refusal (P1)"). The quoted `(price, size)` is
    /// unknown, so `own` stays `None` — every reader of it (`refresh_skips`, `reward_moas_holds`,
    /// `filter_own`, `feed_own_outcome`) already treats `None` as "nothing to compare" — and
    /// `refresh_stale` forces the next tick through the modify arm regardless of tolerance or the
    /// reward moas hold. `unverified` is `None`: the event itself is the proof of life.
    fn adopt_unplaced_side(&mut self, is_bid: bool) {
        let side = self.side_mut(is_bid);
        side.placed = true;
        side.own = None;
        side.refresh_stale = true;
        side.unverified = None;
    }

    /// Put one side's saved resting quote back, so the maker CONTINUES it instead of quoting a
    /// second one beside it. Only a `placed` side that carries a usable `(price, size)` is
    /// restored (anything else stays the fresh, unplaced side — never a half-trusted one).
    ///
    /// The restored side is marked:
    /// - `refresh_stale`, because `own` is the INTENDED quote and a partial fill may have landed
    ///   while the daemon was down: the first tick then re-issues price AND full size through the
    ///   modify arm, bypassing the refresh tolerance and the reward moas hold once (the same
    ///   reason `on_fill` sets it);
    /// - `unverified` (`Some(0)`), the safety net's clock: nothing proves the order still rests
    ///   (the core's ownership entry may have expired, or the venue dropped it while we were
    ///   down), and a modify on an order the core cannot resolve is swallowed silently — the same
    ///   shape as the wedge `on_order_event` closes. A tagged event or a fill on the side clears
    ///   it; otherwise [`SpreadMaker::restored_quote_expired`] pulls the side
    ///   after `RESTORED_QUOTE_VERIFY_MS` and the next tick places a fresh quote.
    ///
    /// The own-order LADDER (`own_book`, unreachable from any binary) is not fed: it needs an
    /// accept timestamp this hook does not have, and it is off by default.
    fn restore_side(&mut self, is_bid: bool, saved: SideQuoteV1) {
        let Some((px, qty)) = saved.own else { return };
        if !saved.placed || !px.is_finite() || !qty.is_finite() || qty <= 0.0 {
            return;
        }
        let side = self.side_mut(is_bid);
        side.placed = true;
        side.own = Some((px, qty));
        side.quoted_timestamp = saved.quoted_timestamp;
        side.refresh_stale = true;
        side.unverified = Some(0);
    }
}

// The maker's QUOTE-EMISSION unit — the one shared tick step both `Strategy` lanes drive.
//
// [`SpreadMaker::requote`] is the pipeline: own-filter the public book → price (A-S layer or the
// pure [`QuoteStyle`](vike_model::QuoteStyle) placement) → reward-band clamp → inventory-skew the
// sizes → flow-toxicity widen/cut → per-side breaker suppression → LADDER or SINGLE placement. The
// STAGE ORDER is semantic (each stage consumes the previous stage's `(price, size)`), and every
// stage's "off ⇒ byte-identical" contract is stated inline at the stage that owns it. It stays ONE
// straight-line function rather than a guard-stage pipeline: this crate runs on the p99 core
// thread, and inline is what makes the neutral-reduction argument checkable by reading.
//
// The tail helpers are the per-side arms it dispatches to — `requote_single_side` /
// `requote_ladder_side` (place-or-diff), `retire_single_order` / `retire_ladder_orders` (the
// mode-switch retirements), and the two RE-PRICE gates `refresh_skips` / `reward_moas_holds`,
// which are reachable ONLY from the re-price arm: a place or a pull is never gated. That arm also
// hosts `restored_quote_expired`, the safety net for a side restored from a saved state.

/// How long a side RESTORED from a saved state may be continued in place (modify by tag) with no
/// sign of life before it is pulled and re-placed — in the maker's EVENT-clock unit, milliseconds
/// on every live lane (the unit of `fill_window_ms`/`suppress_cooldown_ms`). See
/// [`SpreadMaker::restored_quote_expired`].
const RESTORED_QUOTE_VERIFY_MS: i64 = 60_000;

/// The per-side inputs to [`SpreadMaker::requote_ladder_side`], bundled into one struct so the arg
/// count stays sane (and because `half`/`tick`/`ts` are shared across a tick's two sides). All fields
/// come straight from the tick's already-computed single-quote pricing.
struct LadderSide {
    /// This side's level-0 price — the single quote's `bid_price` / `ask_price` (rung 0 rests here).
    level0_price: f64,
    /// The level-0 half-spread `(ask_price − bid_price)/2` — the `HalfSpread` unit scale between rungs.
    half: f64,
    /// The venue tick grid — the `Ticks` unit scale between rungs.
    tick: f64,
    /// This side's already-skewed base (level-0) quote size, which every rung multiplies.
    base_quantity: f64,
    /// Whether the fill-rate breaker is suppressing THIS side this tick (⇒ pull all its rungs).
    suppressed: bool,
}

impl SpreadMaker {
    /// The shared quote step, driven by BOTH the L1 quote lane ([`Strategy::on_quote_tick`]) and the
    /// L2 book lane ([`Strategy::on_order_book`]). One pipeline: (1) optionally own-filter the public
    /// book (`filter_own`), (2) derive the two prices via the selected [`QuoteStyle`], (3) shape the
    /// sizes with the inventory skew, (4) apply the breaker's per-side suppression, then place-or-
    /// modify each side in place. `ts` is the triggering event's EVENT ts (quote/book), never
    /// wall-clock. With the default (`Mid`, no filtration, breaker/skew neutral) this reduces to the
    /// original `mid ± half_spread` placement bit-for-bit.
    ///
    /// [`Strategy::on_quote_tick`]: vike_model::Strategy::on_quote_tick
    /// [`Strategy::on_order_book`]: vike_model::Strategy::on_order_book
    /// [`QuoteStyle`]: vike_model::QuoteStyle
    /// Warn on the EDGE into the unpriced-book hold — see [`SpreadMaker::no_quote_holding`] for why
    /// this early return may not be silent, and why it is edge-triggered rather than per-tick.
    ///
    /// ⚠ Deliberately says NOTHING, and deliberately does not latch, when the break-even refusal is
    /// the cause: `AsState::note_fee_floor` owns that episode end-to-end — it has already announced
    /// the hold with the two numbers that make it actionable, and it announces its own recovery. A
    /// generic "no two-sided quote" line on top would bury the specific one and then emit a second,
    /// redundant RESUMING beside it. Leaving the latch alone keeps this warn's episodes disjoint
    /// from the fee floor's, so a mount that goes fee-floor-hold → book-hold reports both edges in
    /// the right order rather than collapsing them.
    fn note_no_quote(&mut self, view: &BookView) {
        if self.as_state.as_ref().is_some_and(|st| st.is_fee_floor_refusing()) {
            return;
        }
        if !self.no_quote_holding {
            self.no_quote_holding = true;
            tracing::warn!(
                bid_levels = view.bids.len(),
                ask_levels = view.asks.len(),
                tick_size = view.tick_size,
                "maker HOLDING: the book priced no two-sided quote, so nothing was placed or \
                 amended. Zero levels on a side means the FEED is not delivering that side — check \
                 the venue subscription before touching the maker's tuning."
            );
        }
    }

    /// The exit edge of [`SpreadMaker::note_no_quote`]. A no-op unless a hold episode is actually
    /// open, so a healthy maker emits nothing on any tick.
    fn note_quote_resumed(&mut self) {
        if self.no_quote_holding {
            self.no_quote_holding = false;
            tracing::warn!("maker RESUMING: the book priced a two-sided quote again");
        }
    }

    fn requote<B: OrderBroker>(&mut self, broker: &mut B, book: &BookView, ts: i64) {
        // (1)+(2) price off the (optionally own-filtered) book. On the default path we borrow the raw
        // book with NO clone; only when filtration is on do we clone+subtract our own resting orders.
        let filtered;
        let view = if self.config.filter_own {
            // Two filtration backends behind the ONE `filter_own` gate: the opt-in `OrderBookOwn`
            // ladder (multi-order + accepted-buffer race gate) when it is mounted, else the
            // original single-snapshot pair — the verbatim prior call, so the default path is
            // byte-identical.
            filtered = match self.own_book.as_ref() {
                Some(own) => own.filtered_view(book, ts),
                None => filter_own(book, self.bid.own, self.ask.own),
            };
            &filtered
        } else {
            book
        };
        // Read the signed inventory ONCE — the A-S reservation price and the size-skew both need it,
        // and it can't change during a single requote (single-writer core).
        let position = broker.position("");
        // (2) derive the two PRICES. When the A-S layer is enabled it computes an inventory-aware
        // reservation price + optimal spread (and advances its online σ̂² off this tick's mid);
        // otherwise the pure [`QuoteStyle`] prices straight off the (own-filtered) book, unchanged.
        let priced = if let Some(as_state) = self.as_state.as_mut() {
            as_state.price(view, position, ts)
        } else {
            view.priced(self.config.style, self.config.half_spread, self.config.depth_levels)
        };
        let Some((bid_price, ask_price)) = priced else {
            // No valid two-sided quote (book lacks a side / A-S pinned at a wall) — hold the current
            // resting orders untouched this tick, exactly as the one-sided-book path always has.
            // ⚠ …but SAY SO on the edge. This early return was the maker's one wholly silent hold,
            // and it sits ABOVE the fee-floor guard that exists to make holds attributable.
            self.note_no_quote(view);
            return;
        };
        // Priced ⇒ any hold episode is over. Announce the exit edge so an earlier warn cannot
        // outlive its cause and leave a working maker looking permanently broken.
        self.note_quote_resumed();
        // (2b) LIQUIDITY-REWARDS fold (steal/mm-rewards-quoting): the reward band is scored off the
        // BOOK midpoint (not the A-S reservation the candidate is centred on), so compute it from the
        // touch. `priced` being `Some` guarantees both sides are present, so the fallback is
        // unreachable — kept only to stay total.
        let mid = match (view.bids.first(), view.asks.first()) {
            (Some(b), Some(a)) => 0.5 * (b.price + a.price),
            _ => 0.5 * (bid_price + ask_price),
        };
        // Reward chasing is active only for a positive weight; `None`/`weight <= 0` ⇒ the whole fold
        // below is skipped and the quote is byte-identical to before.
        let reward = self.config.reward.filter(|r| r.weight > 0.0);
        // The A-S near-resolution BLACKOUT is the SAFETY authority: inside it the maker quotes at the
        // walls (A-S widest) and must NOT be reward-clamped back into the band — that would farm
        // rewards straight into the resolution vol spike. So skip the price clamp while blacked out.
        let blackout = self.as_state.as_ref().is_some_and(|st| st.in_blackout(ts));
        let (bid_price, ask_price) = match reward {
            Some(r) if !blackout => clamp_into_band(mid, bid_price, ask_price, r),
            _ => (bid_price, ask_price),
        };
        // (3) shape the two sizes from CURRENT inventory vs target — neutral params yield (1.0, 1.0),
        // so qty is unchanged (skew composes with the style: PRICES from the style/A-S, SIZES here).
        let (bid_multiplier, ask_multiplier) = skew_multipliers(
            position,
            self.config.target_inventory,
            self.config.max_inventory,
            self.config.skew,
        );
        let mut bid_quantity = self.config.qty * bid_multiplier;
        let mut ask_quantity = self.config.qty * ask_multiplier;
        // (3b) REWARD min_size floor: a reward-active side must rest at least `min_size` shares to be
        // eligible, so bump each side UP to it (never down). Skipped when reward is off (or the floor
        // is zero), keeping the sizes byte-identical to before on the default path. Also flows into
        // the ladder below, whose rung 0 multiplies this already-floored base size.
        if let Some(r) = reward.filter(|r| r.min_size > 0.0) {
            bid_quantity = bid_quantity.max(r.min_size);
            ask_quantity = ask_quantity.max(r.min_size);
        }
        // (3c) FLOW-TOXICITY guard (RTDS wallet-toxicity, 5c): on the side under toxic taker pressure,
        // WIDEN the level-0 quote away from mid and CUT its size, driven by the latest `on_flow`
        // reading. The widen reuses THIS tick's per-side half-spread `(ask_price − bid_price)/2` (the mid→
        // quote gap), captured BEFORE any widen so both sides scale off the SAME base spread. Applied
        // before the suppression / ladder-vs-single split so a widened level-0 flows through to the
        // rungs, and so a size the cut drives to `0` can route through the per-side suppression PULL
        // path below. `flow_guard` is `Some` ONLY when a non-zero `toxicity` bag is configured AND an
        // `on_flow` reading has arrived; `None` (the default / never-fed / all-zero bag) leaves the
        // quote — and every value below — BYTE-IDENTICAL to before (no float is touched).
        // (3c-i) OFI-TOXICITY synthesis (Group-B, PR-3): feed the tracked Cont–Kukanov–Stoikov OFI into
        // the EXISTING flow-toxicity guard as an INTERNAL FALLBACK reading — but ONLY when a toxicity
        // guard is configured, `ofi_toxicity_scale != 0`, and NO external `on_flow` reading has ever
        // arrived (`!self.flow_external`). PRECEDENCE: an external `on_flow` reading takes precedence
        // permanently (once `flow_external` is set this never runs), so a real reading is never
        // clobbered — the synthesis is the fallback for a maker with no external toxicity feed. SIDE
        // MAPPING: positive OFI = buy pressure ⇒ aggressive BUY takers lift our ASK ⇒ the ASK is the
        // toxic/adverse side (`flow.ask = tox`, matching how the guard below pushes the ASK further
        // from mid); negative OFI (sell pressure) ⇒ the BID. Placed AFTER `as_state.price()` above so it
        // reads the OFI this tick's book update advanced. INERT (byte-identical, no float touched) when
        // the scale is 0, no toxicity guard is set, or an external reading exists.
        if !self.flow_external && self.config.toxicity.is_some() {
            let synth = self.as_state.as_ref().and_then(|st| {
                let scale = st.params.ofi_toxicity_scale;
                (scale != 0.0).then(|| {
                    let ofi = st.ofi();
                    let tox = ofi_toxicity(ofi, scale);
                    if ofi >= 0.0 {
                        FlowToxicity { bid: 0.0, ask: tox, timestamp: ts }
                    } else {
                        FlowToxicity { bid: tox, ask: 0.0, timestamp: ts }
                    }
                })
            });
            if let Some(flow) = synth {
                self.last_flow = Some(flow);
            }
        }
        let flow_guard = self
            .last_flow
            .zip(self.config.toxicity.filter(|t| t.widen != 0.0 || t.size_cut != 0.0));
        let (bid_price, ask_price, bid_quantity, ask_quantity) = match flow_guard {
            Some((flow, tox)) => {
                // per-side half-spread this tick — the mid→quote gap the widen scales by.
                let half = 0.5 * (ask_price - bid_price);
                (
                    // BID: toxic SELL takers hit our bid ⇒ push it LOWER (further from mid), cut size.
                    bid_price - flow.bid * tox.widen * half,
                    // ASK: toxic BUY takers lift our ask ⇒ push it HIGHER (further from mid), cut size.
                    ask_price + flow.ask * tox.widen * half,
                    bid_quantity * (1.0 - flow.bid * tox.size_cut).max(0.0),
                    ask_quantity * (1.0 - flow.ask * tox.size_cut).max(0.0),
                )
            }
            None => (bid_price, ask_price, bid_quantity, ask_quantity),
        };
        // (4) suppression is decided against the triggering event's EVENT ts (never wall-clock): a
        // side is suppressed while `ts` is still before its cooldown deadline. Disabled breaker →
        // both `*_suppressed_until` stay 0 → never suppressed → the per-side place/modify below is
        // byte-identical to the skew-only maker (submit both first, modify both after). A toxicity
        // size-cut that reached `0` folds in here too — routing that side through the SAME PULL path
        // (never place a zero-size order) — but ONLY while the guard is active (`flow_guard.is_some()`),
        // so the default path, where a side can reach `0` only via an extreme inventory skew whose
        // long-standing place-at-zero behavior is preserved, stays byte-identical.
        let enabled = self.breaker_enabled();
        let tox_active = flow_guard.is_some();
        let bid_suppressed =
            (enabled && ts < self.bid.suppressed_until) || (tox_active && bid_quantity <= 0.0);
        let ask_suppressed =
            (enabled && ts < self.ask.suppressed_until) || (tox_active && ask_quantity <= 0.0);
        // (5) LADDER vs SINGLE quote. An ACTIVE ladder (`levels >= 2`) rests N rungs per side stepping
        // out from THIS tick's `(bid_price, ask_price)` reservation, diffing against the resting rungs; the
        // single-order tail below is skipped. On the DEFAULT path (no ladder) `is_active()` is `false`
        // ⇒ the two `retire_ladder_orders` calls are guarded no-ops (empty rung vecs) and the tail runs
        // verbatim — byte-identical. Rung 0 is bit-for-bit today's quote; the style/A-S price it, the
        // skew shapes the base size each rung multiplies, the breaker's suppression pulls the WHOLE
        // side, and the refresh tolerance gates each rung's re-price.
        if self.config.ladder.is_some_and(|l| l.is_active()) {
            // rung geometry needs the level-0 half-spread and the venue grid, taken from THIS tick.
            let half = 0.5 * (ask_price - bid_price);
            let tick = view.tick_size;
            let bid_side = LadderSide {
                level0_price: bid_price,
                half,
                tick,
                base_quantity: bid_quantity,
                suppressed: bid_suppressed,
            };
            let ask_side = LadderSide {
                level0_price: ask_price,
                half,
                tick,
                base_quantity: ask_quantity,
                suppressed: ask_suppressed,
            };
            // if we just switched FROM single mode, retire the leftover `"bid"`/`"ask"` first so it is
            // not stranded (no-op after the first laddered tick — `*_placed` is then false).
            self.retire_single_order(broker, true);
            self.retire_single_order(broker, false);
            self.requote_ladder_side(broker, true, bid_side, ts);
            self.requote_ladder_side(broker, false, ask_side, ts);
            return;
        }
        // Not laddering: retire any rungs left from a previous laddered tick (a guarded no-op on the
        // default path, where the rung vecs are always empty — which is what keeps the tail below
        // byte-identical), then run today's exact single-order tail.
        self.retire_ladder_orders(broker, true);
        self.retire_ladder_orders(broker, false);
        // Per-side single-order tail — ONE parameterized arm run for each side (audit F5: formerly
        // two fully mirrored bid/ask blocks). Bid first, then ask, exactly the original order.
        self.requote_single_side(broker, true, (bid_price, bid_quantity), bid_suppressed, mid, ts);
        self.requote_single_side(broker, false, (ask_price, ask_quantity), ask_suppressed, mid, ts);
    }

    /// Place-or-re-quote ONE side's SINGLE resting order (tag `"bid"`/`"ask"`) — the collapsed
    /// per-side tail of [`SpreadMaker::requote`] (audit F5: formerly two fully mirrored arms).
    /// `target` is this side's already-priced/-shaped `(price, size)` (the same pair shape the
    /// [`SideState::own`] snapshot holds); `suppressed` is the breaker/toxicity verdict for this
    /// side this tick; `mid` feeds the reward moas gate.
    ///
    /// - SUPPRESSED: PULL the resting quote (cancel) so it stops getting hit in the adverse run —
    ///   recording a CENSORED own outcome for the OwnFillFit κ MLE BEFORE the resting snapshot /
    ///   placement clock is cleared.
    /// - not resting: SUBMIT it.
    /// - resting: MODIFY in place — additionally gated by the order-refresh TOLERANCE
    ///   (`refresh_skips`) and the reward min-order-age hold (`reward_moas_holds`): a target that
    ///   barely moved leaves the resting order — and its queue position — untouched, sending
    ///   nothing. OFF by default (`refresh_tolerance == None` ⇒ `refresh_skips` is a constant
    ///   `false`), which is what keeps this tail byte-identical. The PLACE and PULL arms are
    ///   deliberately NOT gated: a quote that must go on or come off the book always does.
    ///
    /// Every arm tracks this side's [`SideState`] (`own`, so the NEXT tick's filtration can
    /// subtract the resting order; `placed`/`refresh_stale`/`quoted_timestamp`) and mirrors its verb into
    /// the own-order LADDER (when mounted) keyed by the SAME tag, so it tracks exactly what the
    /// snapshot pair tracks — plus the lifecycle/race state a snapshot cannot hold. Every
    /// `own_book_*` call is a no-op while the ladder is `None` (the default), which is what keeps
    /// this tail byte-identical.
    ///
    fn requote_single_side<B: OrderBroker>(
        &mut self,
        broker: &mut B,
        is_bid: bool,
        target: (f64, f64),
        suppressed: bool,
        mid: f64,
        ts: i64,
    ) {
        let (px, qty) = target;
        let (tag, side_code, own_side) = if is_bid {
            ("bid", 1, OrderBookOwnSide::Bid)
        } else {
            ("ask", -1, OrderBookOwnSide::Ask)
        };
        if suppressed {
            if self.side(is_bid).placed {
                // OwnFillFit κ MLE: a breaker PULL is a CENSORED own outcome (came off the book
                // unfilled). Recorded BEFORE the resting snapshot / placement clock is cleared.
                self.feed_own_outcome(is_bid, false, ts);
                broker.cancel(tag);
                let side = self.side_mut(is_bid);
                side.placed = false;
                side.own = None;
                side.refresh_stale = false;
                side.quoted_timestamp = 0;
                side.unverified = None;
                self.own_book_pull(tag);
            }
        } else if !self.side(is_bid).placed {
            broker.place("", tag, side_code, qty, OrderPlacement::Limit { price: px }, false);
            let side = self.side_mut(is_bid);
            side.placed = true;
            side.own = Some((px, qty));
            side.refresh_stale = false;
            side.quoted_timestamp = ts;
            side.unverified = None;
            self.own_book_place(tag, own_side, px, qty, ts);
        } else if self.restored_quote_expired(is_bid, ts) {
            // SAFETY NET for a RESTORED side nothing has confirmed alive within the grace: PULL it
            // (cancel by tag — a no-op if the order is already gone) and place NOTHING this tick.
            // The next tick takes the place arm. Never cancel-and-place in one call: the core drains
            // submits BEFORE cancels and resolves a cancel's tag at drain time, so a same-call
            // `cancel("bid")` would resolve to the NEW order and kill it.
            broker.cancel(tag);
            let side = self.side_mut(is_bid);
            side.placed = false;
            side.own = None;
            side.refresh_stale = false;
            side.quoted_timestamp = 0;
            side.unverified = None;
            self.own_book_pull(tag);
        } else if !(self.refresh_skips(
            self.side(is_bid).own,
            px,
            qty,
            self.side(is_bid).refresh_stale,
        ) || self.reward_moas_holds(is_bid, self.side(is_bid).own, mid, ts))
        {
            broker.amend(tag, Some(qty), Some(px), None);
            let side = self.side_mut(is_bid);
            side.own = Some((px, qty));
            side.refresh_stale = false;
            side.quoted_timestamp = ts;
            self.own_book_place(tag, own_side, px, qty, ts);
        }
    }

    /// Diff ONE side's DESIRED ladder rungs against what is resting and emit the minimal verb set —
    /// the multi-order twin of the single-order arms in `requote`, keyed by `"bid{k}"`/`"ask{k}"`.
    ///
    /// - SUPPRESSED (breaker): pull EVERY resting rung of this side (never strand a rung), and place
    ///   nothing — the whole-side analog of the single path's suppression cancel.
    /// - otherwise: for each desired rung `k`, SUBMIT it if there is no rung `k` resting yet, else
    ///   MODIFY it in place unless the per-rung refresh tolerance says the drift is churn (a fill on
    ///   this side sets `*_refresh_stale`, forcing every rung through the modify arm once so a partial
    ///   fill's size top-up is never read as "no change"); then CANCEL any resting rung BEYOND the
    ///   desired count (the ladder shrank — a re-tune with fewer levels, or a decayed/`unit_scale`
    ///   guard).
    ///
    /// Reached ONLY when the ladder is active (`levels >= 2`), so it never runs on the default path.
    /// The resting-rung vec is taken by value for the duration (so `own_book_*`'s `&mut self` calls do
    /// not alias it) and stored back at the end.
    fn requote_ladder_side<B: OrderBroker>(
        &mut self,
        broker: &mut B,
        is_bid: bool,
        side: LadderSide,
        ts: i64,
    ) {
        let Some(lp) = self.config.ladder else {
            return; // unreachable: the caller gates on `is_active()`, but keep it total
        };
        let (prefix, side_code, own_side, sign) = if is_bid {
            ("bid", 1, OrderBookOwnSide::Bid, -1.0)
        } else {
            ("ask", -1, OrderBookOwnSide::Ask, 1.0)
        };
        let stale = self.side(is_bid).refresh_stale;
        // take the resting-rung vec out so the `&mut self` `own_book_*` calls below don't alias it.
        let mut resting = std::mem::take(&mut self.side_mut(is_bid).rungs);
        if side.suppressed {
            // pull the WHOLE side — every rung comes off the book (the tolerance never gates a pull).
            for k in 0..resting.len() {
                let tag = format!("{prefix}{k}");
                broker.cancel(&tag);
                self.own_book_pull(&tag);
            }
            resting.clear();
        } else {
            let desired = ladder::desired_side(
                lp,
                side.level0_price,
                sign,
                side.half,
                side.tick,
                side.base_quantity,
            );
            for (k, &(px, sz)) in desired.iter().enumerate() {
                let tag = format!("{prefix}{k}");
                if k >= resting.len() {
                    // a NEW rung deeper than anything currently resting — submit it.
                    broker.place(
                        "",
                        &tag,
                        side_code,
                        sz,
                        OrderPlacement::Limit { price: px },
                        false,
                    );
                    self.own_book_place(&tag, own_side, px, sz, ts);
                    resting.push((px, sz));
                } else {
                    // an existing rung — re-price in place unless the refresh tolerance (never while
                    // `stale`) says the drift is pure churn. Mirrors the single path's re-price arm.
                    let skip = !stale
                        && self.config.refresh_tolerance.is_some_and(|tolerance| {
                            within_tolerance(resting[k], (px, sz), tolerance)
                        });
                    if !skip {
                        broker.amend(&tag, Some(sz), Some(px), None);
                        self.own_book_place(&tag, own_side, px, sz, ts);
                        resting[k] = (px, sz);
                    }
                }
            }
            // the ladder shrank: cancel every rung beyond the new desired count (never strand one).
            for k in desired.len()..resting.len() {
                let tag = format!("{prefix}{k}");
                broker.cancel(&tag);
                self.own_book_pull(&tag);
            }
            resting.truncate(desired.len());
        }
        // store the resting-rung vec back and clear this side's fill-staleness (the side re-quoted).
        let state = self.side_mut(is_bid);
        state.rungs = resting;
        state.refresh_stale = false;
    }

    /// Retire the SINGLE-order quote on one side (`"bid"`/`"ask"`) when switching INTO ladder mode, so
    /// the pre-ladder order is not stranded. A no-op unless that side currently has a single quote
    /// resting (`*_placed`) — so after the first laddered tick, and on a maker that starts laddered,
    /// it does nothing.
    fn retire_single_order<B: OrderBroker>(&mut self, broker: &mut B, is_bid: bool) {
        if !self.side(is_bid).placed {
            return;
        }
        let tag = if is_bid { "bid" } else { "ask" };
        broker.cancel(tag);
        self.own_book_pull(tag);
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
        side.unverified = None;
    }

    /// The restored-quote SAFETY NET's verdict for one side: `true` ⇒ pull it now (see the arm in
    /// [`SpreadMaker::requote_single_side`]).
    ///
    /// Only a side marked [`SideState::unverified`] by `load_state` can answer `true`; a side this
    /// maker placed itself is `None` there and is never aged out. The grace clock starts at the
    /// FIRST tick that visits the restored side (`load_state` has no clock), so a daemon that waits
    /// minutes for its feed before the first tick does not burn the grace. Inside the grace the
    /// side is continued in place; a tagged non-terminal event or a fill clears the mark first, so
    /// a confirmed order is never churned. After the grace the order is, as far as the maker can
    /// know, either gone or unreachable by tag — both of which wedge it silently (a modify the core
    /// cannot resolve is swallowed) — so it is pulled and re-placed ONCE.
    ///
    fn restored_quote_expired(&mut self, is_bid: bool, ts: i64) -> bool {
        let side = self.side_mut(is_bid);
        match side.unverified {
            None => false,
            // `Some(0)` = clock not started; `max(1)` keeps a synthetic ts of 0 from re-arming it.
            Some(0) => {
                side.unverified = Some(ts.max(1));
                false
            }
            Some(since) => ts - since >= RESTORED_QUOTE_VERIFY_MS,
        }
    }

    /// Retire any resting LADDER rungs on one side when switching OUT of ladder mode (back to a single
    /// quote, or the ladder turned off), so no rung is stranded. A GUARDED no-op while that side's rung
    /// vec is empty — which is ALWAYS the case on the default (never-laddered) path, so it adds no
    /// verbs and touches no state there, keeping the single-order tail byte-identical.
    fn retire_ladder_orders<B: OrderBroker>(&mut self, broker: &mut B, is_bid: bool) {
        let n = self.side(is_bid).rungs.len();
        if n == 0 {
            return;
        }
        let prefix = if is_bid { "bid" } else { "ask" };
        for k in 0..n {
            let tag = format!("{prefix}{k}");
            broker.cancel(&tag);
            self.own_book_pull(&tag);
        }
        let side = self.side_mut(is_bid);
        side.rungs.clear();
        side.refresh_stale = false;
    }

    /// The order-refresh TOLERANCE decision for ONE side: `true` ⇒ SKIP this side's modify and
    /// leave the resting order (and its queue position) alone.
    ///
    /// `resting` is that side's tracked `(price, size)` — the [`SideState::own`] snapshot the
    /// requote tail already maintains on every place/re-price. THREE ways to answer `false`
    /// immediately, the first two of which are the DEFAULT path: no tolerance configured (`None`),
    /// no tracked resting order to compare against, or `stale` — that side's snapshot invalidated
    /// by a fill ([`SideState::refresh_stale`]), where the snapshot is the maker's
    /// INTENDED quote but the venue-side remainder is smaller, so the comparison would wrongly read
    /// "no change" and the partial-fill size top-up would be lost. The arithmetic itself is the pure
    /// [`refresh::within_tolerance`].
    ///
    /// NOTE it is deliberately only reachable from the re-price arm of `requote`: the place and
    /// pull arms must never consult it.
    ///
    /// [`refresh::within_tolerance`]: vike_market_making::refresh::within_tolerance
    fn refresh_skips(
        &self,
        resting: Option<(f64, f64)>,
        price: f64,
        qty: f64,
        stale: bool,
    ) -> bool {
        if stale {
            return false;
        }
        match (self.config.refresh_tolerance, resting) {
            (Some(tolerance), Some(resting)) => within_tolerance(resting, (price, qty), tolerance),
            _ => false,
        }
    }

    /// The reward MIN-ORDER-AGE (moas) hold for one side (`is_bid` picks bid/ask): `true` ⇒ leave the
    /// resting quote to keep ageing toward the reward floor instead of re-pricing (churning) it.
    /// `false` on the DEFAULT path — reward chasing off, that side fill-stale (which must always top
    /// up, mirroring `refresh_skips`' `stale` short-circuit), or no tracked resting order. Delegates
    /// the actual age + still-in-band test to the pure [`reward::moas_holds`], so a safety re-price (a
    /// mid that ran the quote out of the band) always fires. Composed with `refresh_skips` (either may
    /// skip) and, like it, reachable ONLY from the re-price arm — never a place / pull.
    ///
    /// [`reward::moas_holds`]: vike_market_making::reward::moas_holds
    fn reward_moas_holds(
        &self,
        is_bid: bool,
        resting: Option<(f64, f64)>,
        mid: f64,
        now: i64,
    ) -> bool {
        let side = self.side(is_bid);
        if side.refresh_stale {
            return false;
        }
        let quoted_ts = side.quoted_timestamp;
        match (self.config.reward.filter(|r| r.weight > 0.0), resting) {
            (Some(r), Some((rpx, _))) => moas_holds(r, rpx, quoted_ts, mid, now, is_bid),
            _ => false,
        }
    }
}

// ---- The maker's own tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! `SpreadMaker`'s unit tests: every white-box test of the maker (it reads the struct's private
    //! fields or drives it), moved VERBATIM from `vike-market-making` with the maker (plan 2b), one nested
    //! module per former file and every test under its old name; `vike-market-making` keeps the tests of its
    //! maths. They run in the check crate's library unit tests
    //! (`crates/vike-strategies-check/src/makers.rs`), so `crate::` is that crate: the maker is
    //! `super::*` and everything else an absolute path (`vike_market_making::…`, `vike_core::LiveBroker`).
    //! This module owns only what more than one family needs: the shared imports and the
    //! `LiveBroker` fixture helpers. A per-family helper (`ofi_tox_params`, `all_new_params`,
    //! `MockHft`, `approx_rel`, `mid_ladder`, `reward_bag`, …) stays in the family that uses it.

    use super::*;
    // The ladder vocabulary the ladder families name and the maker itself does not import.
    use vike_model::{LadderOffsetUnit, LadderSizeProfile};
    // The concrete live broker the tests build directly (a dev-dependency of the check crate); it
    // `impl OrderBroker`, so the generic `SpreadMaker` mounts on it unchanged. The tests build it via
    // a struct literal, which is why its buffered-order fields are `pub`.
    use vike_core::LiveBroker;
    // The shared L1-quote builder (sizes 1.0, empty symbol) — the families reach it through their
    // `use super::*`.
    use vike_marketdata::test_support::quote;

    /// Capture every `tracing` event emitted on this thread while `f` runs, as text — one line per
    /// event, `LEVEL target: message field=value …`, so a test can ask whether a sentence or a field
    /// was said and how many times.
    ///
    /// ⚠ **The only way to test the thing the maker's HOLD warns are FOR.** Both
    /// `AsState::note_fee_floor` and `SpreadMaker::note_no_quote` exist so a hold is SAID; a test that
    /// asserts only the `None` return passes with the `warn!` deleted, which is the
    /// declaration-pinning shape this repo has been bitten by three times — it would pin the mechanism
    /// against a copy of itself and prove nothing about what an operator sees. `silent_hold` reads
    /// this copy; `vike-market-making` keeps the one its `avellaneda::fee_floor_tests` read.
    ///
    /// ⚠ It reads through `vike_log::capture::captured`, the workspace's one scoped capture, and is a
    /// rendering of what that returns. It used to install a `tracing-subscriber` formatter of its own
    /// with a bare `with_default`, and that could come back EMPTY under `cargo test`: the HOLD lines
    /// are also reached UNCAPTURED by sibling tests here (`the_prod2_bybit_mount_posts_nothing_…` is
    /// one), and while one capture is the only live dispatcher, the first thread to reach a callsite
    /// decides its cached interest for every thread. The shared capture's module doc carries the
    /// mechanism and the anchor that closes it.
    pub(crate) fn captured_logs(f: impl FnOnce()) -> String {
        let ((), events) = vike_log::capture::captured(f);
        let mut text = String::new();
        for e in &events {
            text.push_str(&format!("{} {}: {}", e.level, e.target, e.message));
            for (k, v) in &e.fields {
                text.push_str(&format!(" {k}={v}"));
            }
            text.push('\n');
        }
        text
    }

    /// A bare in-crate `LiveBroker` at a given inventory + EVENT ts, empty order buffers — the
    /// same struct the runtime builds per tick/fill, so what the strategy pushes into it is exactly
    /// what the live drain would route. `strategy_impl_tests` builds its brokers with it too.
    pub(crate) fn broker(position: f64, now: i64) -> LiveBroker {
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
        }
    }

    /// A-S params anchored on a 300 s time-to-resolution window closing at T = 300_000 ms, with the
    /// underlying blend at `weight` (a `0.0` weight = the blend OFF control). Blackout off + fixed κ, so
    /// the ONLY thing that moves between two makers built from it is the underlying anchor. Read by
    /// `strategy_impl_tests` (`vike-market-making` keeps the copy its `underlying_anchor_tests` read).
    pub(crate) fn blend_params(weight: f64) -> AsParams {
        AsParams {
            underlying_weight: weight,
            underlying_beta: 1.0,
            window_secs: 300.0,
            horizon_mode: HorizonMode::TimeToResolution,
            resolution_ts: Some(300_000),
            resolution_blackout_ms: 0,
            gamma: 0.5,
            kappa_mode: KappaMode::Fixed,
            q_scale: 1.0,
            min_standoff_ticks: 1.0,
            ..AsParams::default()
        }
    }

    /// A fill at `side` (+1 bid/buy, −1 ask/sell), `size`, EVENT `ts`.
    fn a_fill(side: i32, size: f64, ts: i64) -> Fill {
        Fill {
            side,
            size,
            price: 100.0,
            fee: 0.0,
            timestamp: ts,
            is_maker: true,
            symbol: String::new(),
            tag: None,
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

    /// `(price, qty)` of a tagged SUBMIT; `new_price` of a tagged MODIFY — the two reads the
    /// style/filtration tests assert placement prices with.
    fn submit_at(b: &LiveBroker, tag: &str) -> (f64, f64) {
        let s = b.submissions.iter().find(|s| s.tag.as_deref() == Some(tag)).expect("a submit");
        (s.price.expect("limit submit has a price"), s.qty)
    }

    pub(crate) fn modify_price(b: &LiveBroker, tag: &str) -> f64 {
        let m = b.modifications.iter().find(|m| m.tag == tag).expect("a modify");
        m.new_price.expect("re-price has a price")
    }

    /// The the CI box grid: bybit BTCUSDT on its real `0.1` tick (`SpreadMakerPreset::Crypto`'s knobs), read
    /// by `silent_hold` (`vike-market-making` keeps the copy its `avellaneda::fee_floor_tests` read).
    pub(crate) const TICK: f64 = 0.1;

    pub(crate) const MID: f64 = 63_050.0;

    /// A healthy two-sided view — one level a side, straddling [`MID`].
    pub(crate) fn two_sided() -> BookView {
        BookView {
            tick_size: TICK,
            bids: vec![vike_marketdata::L2BookLevel::new(MID - 0.5 * TICK, 5.0)],
            asks: vec![vike_marketdata::L2BookLevel::new(MID + 0.5 * TICK, 5.0)],
        }
    }

    /// One buffered submission, rendered — `tag`, `symbol`, `side`, `qty`, `price`.
    ///
    /// `BufferedSubmit`/`BufferedModify` are runtime LANE PAYLOADS with no `Debug` impl, so an
    /// assertion that fails on them can print nothing useful. A family's failures are only
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

    mod as_pricing {
        //! The Avellaneda–Stoikov pricing layer (audit mm-quote): analytic bit-exact anchors, golden
        //! numeric vectors from an INDEPENDENT reference, invariant sweeps, and the A-S-off reduction.

        use super::*;
        use vike_model::test_support::maker::as_test_params;

        // ---- Avellaneda–Stoikov pricing layer (audit mm-quote) ----
        //
        // A-S is net-new Rust (no Python oracle), so correctness is pinned three ways per the mm-quote
        // spec §6: (a) ANALYTIC bit-exact anchors on the pure fns, (b) GOLDEN numeric vectors computed by
        // an INDEPENDENT reference (a throwaway `rustc` scratch using the TEXTBOOK groupings, never
        // vike-trader-app) at ≤1e-12 relative, and (c) invariant sweeps + neutral-reduction.

        /// True when `a` is within `rel` RELATIVE of `b` (spec's ≤1e-12 tolerance class for the
        /// exp/ln-derived pieces). Guards tiny `b` with a `max(1.0)` denominator.
        fn approx_rel(a: f64, b: f64, rel: f64) -> bool {
            (a - b).abs() <= rel * b.abs().max(1.0)
        }

        /// Mount an A-S maker (tick grid 0.01 via `with_quote_style`) and quote ONCE at mid 0.50 with the
        /// given inventory; return the submitted `(bid_price, ask_price)`.
        fn as_quote_at(params: AsParams, position: f64) -> (f64, f64) {
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(params);
            let mut b = broker(position, 1_000);
            mm.on_quote_tick(&mut b, &quote(1_000, 0.49, 0.51)); // mid 0.50 on a 0.01 grid
            (submit_at(&b, "bid").0, submit_at(&b, "ask").0)
        }

        // MOUNTED (spec §6): flat ⇒ symmetric around the mid; long ⇒ both quotes skew DOWN; short ⇒ UP.
        #[test]
        fn mounted_as_maker_skews_quotes_with_inventory() {
            let p = as_test_params(0.5);
            let (b_flat, a_flat) = as_quote_at(p, 0.0);
            assert!(b_flat < 0.5 && 0.5 < a_flat, "flat straddles the mid: {b_flat}/{a_flat}");
            assert!(
                ((0.5 - b_flat) - (a_flat - 0.5)).abs() < 1e-9,
                "flat quotes symmetric about 0.5"
            );

            let (b_long, a_long) = as_quote_at(p, 2.0);
            assert!(
                b_long < b_flat && a_long < a_flat,
                "long ⇒ reservation price pulls both sides down"
            );

            let (b_short, a_short) = as_quote_at(p, -2.0);
            assert!(b_short > b_flat && a_short > a_flat, "short ⇒ both sides pushed up");
            // (a full ±q mirror does NOT hold at this inventory: the long side's ask is pulled to the
            // s+standoff floor — the never-cross-the-mid clamp — while the short side's is not.)
        }

        // MOUNTED: a larger γ widens the quoted spread (the vol term dominates at p=0.5).
        #[test]
        fn mounted_as_maker_widens_with_gamma() {
            let (b_lo, a_lo) = as_quote_at(as_test_params(0.3), 0.0);
            let (b_hi, a_hi) = as_quote_at(as_test_params(0.9), 0.0);
            assert!(
                (a_hi - b_hi) > (a_lo - b_lo),
                "larger γ ⇒ wider spread: {} vs {}",
                a_hi - b_hi,
                a_lo - b_lo
            );
        }

        // MOUNTED: the Bernoulli cap tightens the spread toward the walls — flat spread at p=0.5 is WIDER
        // than at p=0.9 (V = p(1−p) collapses near the wall).
        #[test]
        fn mounted_as_maker_spread_tightens_toward_the_walls() {
            let p = as_test_params(0.6);
            let mid_spread = {
                let (b, a) = as_quote_at(p, 0.0); // mid 0.50
                a - b
            };
            // quote at mid 0.90 (near the upper wall)
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(p);
            let mut b = broker(0.0, 1_000);
            mm.on_quote_tick(&mut b, &quote(1_000, 0.89, 0.91)); // mid 0.90
            let (bid, ask) = (submit_at(&b, "bid").0, submit_at(&b, "ask").0);
            assert!(
                (ask - bid) < mid_spread,
                "spread tighter near the wall: {} !< {}",
                ask - bid,
                mid_spread
            );
            assert!(bid < 0.9 && 0.9 < ask, "still straddles the mid near the wall");
        }

        // MOUNTED: inside the near-resolution blackout the maker quotes maximally WIDE (at the walls);
        // outside it quotes normally. This is the §2.3 resolution-vol-spike stance.
        #[test]
        fn mounted_as_maker_blackout_quotes_at_the_walls() {
            // resolution at T=2000, blackout 200ms ⇒ ts≥1800 is blacked out
            let params = AsParams {
                horizon_mode: HorizonMode::TimeToResolution,
                resolution_ts: Some(2_000),
                resolution_blackout_ms: 200,
                variance_mode: VarianceMode::PureBernoulli,
                kappa_mode: KappaMode::Fixed,
                gamma: 0.5,
                q_scale: 1.0,
                min_standoff_ticks: 1.0,
                ..AsParams::default()
            };
            // OUTSIDE the blackout (ts=1000 < 1800): a normal, narrow, mid-straddling quote
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(params);
            let mut b0 = broker(0.0, 1_000);
            mm.on_quote_tick(&mut b0, &quote(1_000, 0.49, 0.51));
            let (bid0, ask0) = (submit_at(&b0, "bid").0, submit_at(&b0, "ask").0);
            assert!(bid0 > 0.01 && ask0 < 0.99, "outside blackout ⇒ normal quote: {bid0}/{ask0}");

            // INSIDE the blackout (ts=1900 ≥ 1800): pushed to the walls [tick, 1−tick]
            let mut b1 = broker(0.0, 1_900);
            mm.on_quote_tick(&mut b1, &quote(1_900, 0.49, 0.51));
            assert_eq!(
                modify_price(&b1, "bid").to_bits(),
                0.01_f64.to_bits(),
                "blackout bid at the wall"
            );
            assert_eq!(
                modify_price(&b1, "ask").to_bits(),
                0.99_f64.to_bits(),
                "blackout ask at the wall"
            );
        }

        // NEUTRAL REDUCTION (the core discipline): a maker with A-S OFF (`as_state == None`) prices
        // BIT-FOR-BIT off the fixed mid formula, exactly as before this feature — the new pricing branch
        // is never taken, proving `None` reduces to today's maker.
        #[test]
        fn as_disabled_is_bit_identical_to_plain_maker() {
            for &(bid, ask, hs) in &[(100.0, 100.2, 0.5), (0.41, 0.44, 0.005), (0.5, 0.52, 0.01)] {
                let mut mm = SpreadMaker::new(1.0, hs); // A-S OFF (default)
                assert!(mm.as_state.is_none(), "A-S is off by default");
                let mut b = broker(3.0, 1); // even a non-flat inventory must NOT skew the price with A-S off
                mm.on_quote_tick(&mut b, &quote(1, bid, ask));
                let mid = 0.5 * (bid + ask);
                assert_eq!(
                    submit_at(&b, "bid").0.to_bits(),
                    (mid - hs).to_bits(),
                    "bid == mid − hs exactly"
                );
                assert_eq!(
                    submit_at(&b, "ask").0.to_bits(),
                    (mid + hs).to_bits(),
                    "ask == mid + hs exactly"
                );
            }
        }

        // The κ trade tape keeps correct running sums and evicts by the event-time window (O(1) core).
        #[test]
        fn as_state_trade_tape_running_sums_and_eviction() {
            let params = AsParams {
                trade_window_ms: 1_000,
                kappa_mode: KappaMode::LiveFit,
                n_min: 1,
                ..as_test_params(0.5)
            };
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(params);
            // a quote first, so last_quote_mid = 0.50 (trades before a quote are ignored)
            mm.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.49, 0.51));
            // two trades δ=0.02 (prices 0.52 and 0.48), weight 1 each, inside the window
            mm.on_trade_tick(
                &mut broker(0.0, 100),
                &TickTrade {
                    timestamp: 100,
                    local_timestamp: 0,
                    price: 0.52,
                    size: 1.0,
                    is_buyer_maker: false,
                    symbol: String::new(),
                },
            );
            mm.on_trade_tick(
                &mut broker(0.0, 200),
                &TickTrade {
                    timestamp: 200,
                    local_timestamp: 0,
                    price: 0.48,
                    size: 1.0,
                    is_buyer_maker: false,
                    symbol: String::new(),
                },
            );
            {
                let st = mm.as_state.as_ref().unwrap();
                assert_eq!(st.trades.len(), 2, "two trades in the window");
                assert!(approx_rel(st.sum_w, 2.0, 1e-12), "Σw = 2");
                assert!(approx_rel(st.sum_w_delta, 0.04, 1e-9), "Σwδ = 2·0.02 = 0.04");
                // κ̂ = Σw/Σwδ = 2/0.04 = 50
                assert!(approx_rel(st.effective_kappa(), 50.0, 1e-9), "κ̂ = 50 from the tape");
            }
            // a trade far in the future evicts both old ones (cutoff = 2000 − 1000 = 1000)
            mm.on_trade_tick(
                &mut broker(0.0, 2_000),
                &TickTrade {
                    timestamp: 2_000,
                    local_timestamp: 0,
                    price: 0.51,
                    size: 3.0,
                    is_buyer_maker: false,
                    symbol: String::new(),
                },
            );
            let st = mm.as_state.as_ref().unwrap();
            assert_eq!(st.trades.len(), 1, "old trades evicted by the event-time window");
            assert!(approx_rel(st.sum_w, 3.0, 1e-12), "Σw = the surviving trade's weight 3");
            assert!(approx_rel(st.sum_w_delta, 3.0 * 0.01, 1e-9), "Σwδ = 3·|0.51−0.50| = 0.03");
        }

        // A live re-tune of the A-S bag PRESERVES the warm σ/κ estimator state (only the params swap),
        // and setting the bag to `None` turns A-S off.
        #[test]
        fn as_params_retune_preserves_estimators_and_none_disables() {
            let params = AsParams { trade_window_ms: 100_000, n_min: 1, ..as_test_params(0.5) };
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(params);
            mm.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.49, 0.51));
            mm.on_trade_tick(
                &mut broker(0.0, 100),
                &TickTrade {
                    timestamp: 100,
                    local_timestamp: 0,
                    price: 0.53,
                    size: 2.0,
                    is_buyer_maker: false,
                    symbol: String::new(),
                },
            );
            let sw_before = mm.as_state.as_ref().unwrap().sum_w;
            // re-tune γ via the params plane — the tape must survive
            let retuned = SpreadMakerParams {
                avellaneda_stoikov: Some(AsParams { gamma: 0.9, ..params }),
                ..mm.params()
            };
            mm.on_params_updated(&mut broker(0.0, 200), &StrategyParams::SpreadMaker(retuned));
            let st = mm.as_state.as_ref().unwrap();
            assert!(approx_rel(st.sum_w, sw_before, 1e-12), "re-tune preserves the warm κ tape");
            assert!((st.params.gamma - 0.9).abs() < 1e-12, "the new γ landed");
            // now disable A-S via a None bag
            let off = SpreadMakerParams { avellaneda_stoikov: None, ..mm.params() };
            mm.on_params_updated(&mut broker(0.0, 300), &StrategyParams::SpreadMaker(off));
            assert!(mm.as_state.is_none(), "a None A-S bag turns the layer off");
        }
    }

    mod breaker {
        //! The per-side fill-rate circuit breaker (audit mm1) end-to-end: round-trip netting, per-side
        //! suppression + cooldown expiry, the Group-B accelerated pull near a binary resolution, the
        //! OFI→toxicity synthesis that feeds the same guard, and the breaker-OFF reduction.

        use super::*;

        // Repeated same-side (bid) fills with NO offsetting asks → net accumulates past the threshold
        // → the BID is pulled and withheld, while the un-hit ASK keeps quoting.
        #[test]
        fn repeated_same_side_fills_suppress_only_that_side() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 5000);
            // tick 1 rests both sides
            let mut b0 = broker(0.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick places both sides");

            // three bid (buy) fills inside the window, no asks → net +3 >= 2.5 trips the bid
            for t in [110, 120, 130] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            // next tick, still inside the cooldown (140 < 130+5000)
            let mut b1 = broker(3.0, 140);
            mm.on_quote_tick(&mut b1, &quote(140, 99.0, 100.0));
            assert!(canceled(&b1, "bid"), "the over-hit bid is pulled");
            assert!(
                !submitted(&b1, "bid") && !modified(&b1, "bid"),
                "and NOT re-quoted while suppressed"
            );
            assert!(modified(&b1, "ask"), "the un-hit ask keeps quoting in place");
            assert!(!canceled(&b1, "ask"), "the ask side is never pulled");
        }

        // Interleaved bid/ask fills that net out are COMPLETED round-trips (inventory-neutral) — the
        // desired MM outcome — and must NOT trip suppression on either side.
        #[test]
        fn round_trip_netting_does_not_suppress() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 5000);
            let mut b0 = broker(0.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));

            // alternate bid(+1)/ask(−1): the running net only ever reaches +1, never the 2.5 threshold
            for (side, t) in [(1, 110), (-1, 115), (1, 120), (-1, 125), (1, 130), (-1, 135)] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(side, 1.0, t));
            }
            let mut b1 = broker(0.0, 140);
            mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
            assert!(b1.cancels.is_empty(), "round-trips net to neutral → no side suppressed");
            assert!(modified(&b1, "bid") && modified(&b1, "ask"), "both sides keep quoting");
        }

        // Suppression is a fixed EVENT-TIME cooldown: pulled while `q.ts` is before the deadline,
        // re-quoted (re-submitted) once the quote-tick ts reaches it.
        #[test]
        fn suppression_expires_after_the_cooldown_timestamp() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 500);
            mm.on_quote_tick(&mut broker(0.0, 100), &quote(100, 100.0, 101.0)); // rest both

            // trip the bid at ts 130 → suppressed until 130 + 500 = 630
            for t in [110, 120, 130] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            // during the cooldown (200 < 630): pulled, not re-quoted
            let mut during = broker(3.0, 200);
            mm.on_quote_tick(&mut during, &quote(200, 99.0, 100.0));
            assert!(canceled(&during, "bid"), "pulled during the cooldown");
            assert!(!submitted(&during, "bid"), "and not re-quoted during the cooldown");

            // at the deadline ts (630 is NOT < 630 → expired): the bid re-enters the book
            let mut after = broker(3.0, 630);
            mm.on_quote_tick(&mut after, &quote(630, 99.0, 100.0));
            assert!(submitted(&after, "bid"), "bid re-submitted once the cooldown ts passes");
            assert!(!canceled(&after, "bid"), "no longer pulled");
        }

        // --- Group-B PR-3: accelerated pull (time-accelerated breaker near a binary resolution) --------

        // As τ = (resolution_ts − t) → 0, the effective net-fill threshold is DIVIDED by the acceleration
        // multiplier, so a net one-directional fill that is BELOW the raw threshold still trips the breaker.
        #[test]
        fn accelerated_pull_trips_the_breaker_sooner_near_resolution() {
            let t_res = 1_000_000;
            // knobs ON: at τ=0 the multiplier is 1 + 1 = 2, so the effective threshold is 2.5 / 2 = 1.25.
            let mut on = SpreadMaker::new(1.0, 0.5)
                .with_fill_breaker(1_000, 2.5, 5_000)
                .with_avellaneda_stoikov(AsParams {
                    resolution_ts: Some(t_res),
                    pull_accel_ramp_ms: 10_000,
                    pull_accel_max: 1.0,
                    ..AsParams::default()
                });
            // two same-side bid fills AT resolution → net +2.0 ≥ 1.25 (accelerated) → the bid trips.
            for _ in 0..2 {
                on.on_fill(&mut broker(0.0, t_res), &a_fill(1, 1.0, t_res));
            }
            assert_ne!(
                on.bid.suppressed_until, 0,
                "accelerated breaker trips on net +2 (< the raw 2.5)"
            );

            // knobs OFF (default 0/0), same resolution_ts: the SAME net +2.0 is below the raw 2.5 → no trip.
            let mut off = SpreadMaker::new(1.0, 0.5)
                .with_fill_breaker(1_000, 2.5, 5_000)
                .with_avellaneda_stoikov(AsParams {
                    resolution_ts: Some(t_res),
                    ..AsParams::default()
                });
            for _ in 0..2 {
                off.on_fill(&mut broker(0.0, t_res), &a_fill(1, 1.0, t_res));
            }
            assert_eq!(
                off.bid.suppressed_until, 0,
                "un-accelerated breaker holds at net +2 (< 2.5)"
            );
        }

        // BYTE-IDENTICAL default: with the pull-accel knobs at their `0` defaults the multiplier is exactly
        // `1.0`, so the effective threshold equals `net_fill_threshold` bit-for-bit — the raw breaker's trip
        // point (net +2 < 2.5 holds; a third fill, net +3 ≥ 2.5, trips) is unchanged.
        #[test]
        fn breaker_trip_point_is_byte_identical_with_pull_accel_off() {
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_fill_breaker(1_000, 2.5, 5_000)
                .with_avellaneda_stoikov(AsParams {
                    resolution_ts: Some(1_000_000),
                    ..AsParams::default()
                });
            assert_eq!(
                mm.pull_accel_multiplier(999_000).to_bits(),
                1.0_f64.to_bits(),
                "off ⇒ multiplier 1.0"
            );
            for t in [100, 110] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            assert_eq!(mm.bid.suppressed_until, 0, "net +2 < 2.5 ⇒ no trip (raw threshold)");
            mm.on_fill(&mut broker(0.0, 120), &a_fill(1, 1.0, 120));
            assert_ne!(mm.bid.suppressed_until, 0, "net +3 ≥ 2.5 ⇒ trips at the raw threshold");
        }

        // A breaker-only maker with NO A-S layer can never accelerate (the `pull_accel_*` knobs live on
        // AsParams): `pull_accel_multiplier` is `1.0` and the trip point is the raw threshold.
        #[test]
        fn pull_accel_multiplier_is_one_without_the_as_layer() {
            let mm = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
            assert_eq!(
                mm.pull_accel_multiplier(0).to_bits(),
                1.0_f64.to_bits(),
                "no A-S ⇒ multiplier 1.0"
            );
        }

        // --- Group-B PR-3: OFI-toxicity synthesis into the flow-toxicity guard --------------------------

        /// A-S params that keep the OFI tracker ADVANCED (nonzero `alpha_lambda_ofi`) with a pure-sum decay,
        /// plus the `ofi_toxicity_scale` under test. Fixed κ / unit `q_scale` so nothing else moves.
        fn ofi_tox_params(scale: f64) -> AsParams {
            AsParams {
                alpha_lambda_ofi: 0.01, // nonzero ⇒ the OFI tracker is advanced each book update
                ofi_decay: 1.0,         // pure sum ⇒ OFI builds cleanly over the fed ticks
                ofi_toxicity_scale: scale,
                gamma: 0.1,
                kappa_mode: KappaMode::Fixed,
                q_scale: 1.0,
                ..AsParams::default()
            }
        }

        // Rising bid+ask build POSITIVE OFI (buy pressure); the synthesis maps that onto the ASK side (the
        // side aggressive BUY takers lift) and populates `last_flow` internally.
        #[test]
        fn ofi_toxicity_synthesizes_ask_side_reading_on_buy_pressure() {
            let mut mm = SpreadMaker::new(1.0, 0.01)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(ofi_tox_params(0.5))
                .with_flow_toxicity(ToxicityParams { widen: 1.0, size_cut: 0.5 });
            for (ts, bid, ask) in
                [(100, 0.40, 0.42), (110, 0.41, 0.43), (120, 0.42, 0.44), (130, 0.43, 0.45)]
            {
                mm.on_quote_tick(&mut broker(0.0, ts), &quote(ts, bid, ask));
            }
            let flow = mm.last_flow.expect("OFI-toxicity synthesized a reading");
            assert!(flow.ask > 0.0, "positive OFI ⇒ the ASK is the toxic side: {flow:?}");
            assert_eq!(
                flow.bid.to_bits(),
                0.0_f64.to_bits(),
                "the bid carries no synthesized toxicity"
            );
            assert!(!mm.flow_external, "a synthesized reading is internal, not external");
        }

        // BYTE-IDENTICAL: `ofi_toxicity_scale == 0` ⇒ no synthesis, so `last_flow` stays `None` even as the
        // OFI builds — the toxicity guard sees nothing and the quote is unchanged from before this PR.
        #[test]
        fn ofi_toxicity_off_leaves_last_flow_none() {
            let mut mm = SpreadMaker::new(1.0, 0.01)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(ofi_tox_params(0.0))
                .with_flow_toxicity(ToxicityParams { widen: 1.0, size_cut: 0.5 });
            for (ts, bid, ask) in [(100, 0.40, 0.42), (110, 0.41, 0.43), (120, 0.42, 0.44)] {
                mm.on_quote_tick(&mut broker(0.0, ts), &quote(ts, bid, ask));
            }
            assert!(mm.last_flow.is_none(), "scale 0 ⇒ no synthesized reading ⇒ byte-identical");
        }

        // PRECEDENCE: an external `on_flow` reading is never clobbered by the OFI synthesis — once the feed
        // is external, strong OFI does not overwrite the real reading.
        #[test]
        fn external_on_flow_takes_precedence_over_ofi_synthesis() {
            let mut mm = SpreadMaker::new(1.0, 0.01)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(ofi_tox_params(0.5))
                .with_flow_toxicity(ToxicityParams { widen: 1.0, size_cut: 0.5 });
            let external = FlowToxicity { bid: 0.9, ask: 0.1, timestamp: 50 };
            mm.on_flow(&mut broker(0.0, 50), external);
            assert!(mm.flow_external, "on_flow marks the feed external");
            // build strong POSITIVE OFI (which alone would synthesize an ASK-side reading).
            for (ts, bid, ask) in
                [(100, 0.40, 0.42), (110, 0.41, 0.43), (120, 0.42, 0.44), (130, 0.43, 0.45)]
            {
                mm.on_quote_tick(&mut broker(0.0, ts), &quote(ts, bid, ask));
            }
            assert_eq!(mm.last_flow, Some(external), "the external reading survives untouched");
        }

        // Breaker OFF (defaults) must reduce EXACTLY to the skew-only maker: on_fill tracks nothing,
        // a one-sided run that WOULD trip an enabled breaker never suppresses, and quoting stays the
        // submit-both-then-modify-both path with skew-shaped sizes.
        #[test]
        fn disabled_breaker_reduces_to_skew_only_maker() {
            let mut mm = SpreadMaker::new(2.0, 0.5).with_skew(0.0, 4.0, 0.5);
            assert!(!mm.breaker_enabled(), "the defaults leave the breaker off");

            let mut b0 = broker(0.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "both sides placed");
            assert!(b0.cancels.is_empty(), "nothing pulled");

            // a heavy one-sided run that would trip an ENABLED breaker
            for t in [110, 120, 130, 140, 150] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            assert!(mm.fills.is_empty(), "disabled on_fill records nothing — zero tracking");

            let mut b1 = broker(3.0, 160);
            mm.on_quote_tick(&mut b1, &quote(160, 100.0, 101.0));
            assert!(modified(&b1, "bid") && modified(&b1, "ask"), "both re-quoted in place");
            assert!(b1.cancels.is_empty(), "no suppression is ever possible with the breaker off");

            // and the skew still shapes the sizes (long → smaller bid, larger ask), proving compose
            let bid_quantity =
                b1.modifications.iter().find(|m| m.tag == "bid").unwrap().new_qty.unwrap();
            let ask_quantity =
                b1.modifications.iter().find(|m| m.tag == "ask").unwrap().new_qty.unwrap();
            assert!(bid_quantity < 2.0, "skew still shrinks the bid when long: {bid_quantity}");
            assert!(ask_quantity > 2.0, "skew still grows the ask when long: {ask_quantity}");
        }
    }

    mod flow_toxicity {
        //! The FLOW-TOXICITY guard (RTDS wallet-toxicity, 5c): the toxic side widens away from mid and
        //! loses size, a full size-cut routes through the per-side PULL path, and the clean side is untouched.

        use super::*;

        // ---- flow-toxicity guard (RTDS wallet-toxicity, 5c) ----

        // The guard OFF (or configured-but-never-fed) quotes BYTE-IDENTICAL to a plain maker; ON with a
        // BID-side toxic reading it widens the bid AWAY from mid (lower) and cuts its size, while the ask
        // (no toxic pressure) stays byte-identical — the per-side reaction.
        #[test]
        fn toxicity_widens_and_cuts_only_the_toxic_side() {
            let q = quote(1, 100.0, 100.2); // Mid, half_spread 0.5 ⇒ bid ≈ 99.6 / ask ≈ 100.6, size 1.0
            let tp = ToxicityParams { widen: 0.5, size_cut: 0.5 };

            // baseline: a plain Mid maker, no toxicity guard.
            let mut base = SpreadMaker::new(1.0, 0.5);
            let mut bb = broker(0.0, 1);
            base.on_quote_tick(&mut bb, &q);
            let (base_bid_price, base_bid_quantity) = submit_at(&bb, "bid");
            let (base_ask_price, base_ask_quantity) = submit_at(&bb, "ask");

            // guard CONFIGURED but never fed a reading (`last_flow` None) ⇒ byte-identical to baseline.
            let mut unfed = SpreadMaker::new(1.0, 0.5).with_flow_toxicity(tp);
            let mut bu = broker(0.0, 1);
            unfed.on_quote_tick(&mut bu, &q);
            let (u_bid_price, u_bid_quantity) = submit_at(&bu, "bid");
            assert_eq!(
                u_bid_price.to_bits(),
                base_bid_price.to_bits(),
                "no reading ⇒ bid px unchanged"
            );
            assert_eq!(
                u_bid_quantity.to_bits(),
                base_bid_quantity.to_bits(),
                "no reading ⇒ bid qty unchanged"
            );

            // guard ON + a BID-side toxic reading (ask side clean).
            let mut tox = SpreadMaker::new(1.0, 0.5).with_flow_toxicity(tp);
            tox.on_flow(&mut broker(0.0, 1), FlowToxicity { bid: 1.0, ask: 0.0, timestamp: 1 });
            let mut bt = broker(0.0, 1);
            tox.on_quote_tick(&mut bt, &q);
            let (tox_bid_price, tox_bid_quantity) = submit_at(&bt, "bid");
            let (tox_ask_price, tox_ask_quantity) = submit_at(&bt, "ask");

            // BID (toxic): widened LOWER by flow·widen·half = 1.0·0.5·(½·(ask−bid)=0.5) = 0.25; size cut
            // to (1 − flow·size_cut)× = (1 − 0.5)× = 0.5×.
            assert!(tox_bid_price < base_bid_price, "toxic bid widened away from mid (lower)");
            assert!(
                (base_bid_price - tox_bid_price - 0.25).abs() < 1e-9,
                "bid widened by flow·widen·half = 0.25 (base {base_bid_price} → {tox_bid_price})"
            );
            assert!(tox_bid_quantity < base_bid_quantity, "toxic bid size cut below baseline");
            assert_eq!(
                tox_bid_quantity.to_bits(),
                0.5_f64.to_bits(),
                "bid size cut to exactly 0.5"
            );

            // ASK (clean): byte-identical to baseline — flow.ask = 0 ⇒ no widen, no cut.
            assert_eq!(
                tox_ask_price.to_bits(),
                base_ask_price.to_bits(),
                "clean ask px byte-identical"
            );
            assert_eq!(
                tox_ask_quantity.to_bits(),
                base_ask_quantity.to_bits(),
                "clean ask qty byte-identical"
            );
        }

        // A size the cut drives to 0 WITHHOLDS that side through the SAME per-side suppression path the
        // fill-rate breaker uses (never a zero-size order): a first tick with a full bid-side cut places
        // the ask but NOT the bid, while the clean ask is placed normally.
        #[test]
        fn toxicity_full_size_cut_withholds_the_side() {
            let tp = ToxicityParams { widen: 0.0, size_cut: 1.0 };
            let mut tox = SpreadMaker::new(1.0, 0.5).with_flow_toxicity(tp);
            // full bid toxicity ⇒ bid size × (1 − 1·1) = 0 ⇒ that side is suppressed; ask clean.
            tox.on_flow(&mut broker(0.0, 1), FlowToxicity { bid: 1.0, ask: 0.0, timestamp: 1 });
            let mut b = broker(0.0, 1);
            tox.on_quote_tick(&mut b, &quote(1, 100.0, 100.2));
            assert!(!submitted(&b, "bid"), "a fully size-cut bid is withheld (no zero-size order)");
            assert!(submitted(&b, "ask"), "the clean ask is placed normally");
        }
    }

    mod from_params {
        //! `SpreadMaker::from_params` — the registry/harness params reader: every feature family's
        //! keys parse into the maker, and a KEYLESS table is byte-identical to today's construction.

        use super::*;
        use std::assert_matches;

        /// `spread_model = "gueant"` (+ `base_intensity_a`) flows through `from_params` into the maker's
        /// `AsParams`, so the SAME `spread_maker` registry arm backtests GLFT with zero new registry
        /// surface. Absent ⇒ A-S default (byte-identical). This is the backtest-wiring proof.
        #[test]
        fn from_params_reads_spread_model_and_base_intensity() {
            let gueant: Params =
                toml::from_str("spread_model = \"gueant\"\nbase_intensity_a = 3.0\ngamma = 0.2")
                    .unwrap();
            let as_p =
                SpreadMaker::from_params(&gueant).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.spread_model, SpreadModel::Gueant, "spread_model = gueant is read");
            assert_eq!(as_p.base_intensity_a, 3.0, "base_intensity_a is read");
            assert_eq!(as_p.gamma, 0.2, "sibling A-S knobs still read");

            // Absent ⇒ A-S default (byte-identical to the pre-GLFT path).
            let plain: Params = toml::from_str("gamma = 0.2").unwrap();
            let as_d =
                SpreadMaker::from_params(&plain).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_d.spread_model, SpreadModel::AvellanedaStoikov, "absent ⇒ A-S");
            assert_eq!(
                as_d.base_intensity_a,
                AsParams::default().base_intensity_a,
                "absent ⇒ default A"
            );
        }

        /// `with_spread_model` forces the model on an already-built maker — the mechanism the
        /// `gueant_maker` registry alias uses to price the `spread_maker` knobs with GLFT.
        #[test]
        fn with_spread_model_forces_the_model() {
            let plain: Params = toml::from_str("gamma = 0.2").unwrap();
            let m =
                SpreadMaker::from_params(&plain).unwrap().with_spread_model(SpreadModel::Gueant);
            assert_eq!(
                m.params().avellaneda_stoikov.unwrap().spread_model,
                SpreadModel::Gueant,
                "the alias forces Gueant regardless of the (absent) spread_model param"
            );
            // No A-S state (bare maker) ⇒ no-op, no panic.
            let _ = SpreadMaker::new(0.001, 0.01).with_spread_model(SpreadModel::Gueant);
        }

        // --- from_params reachability (audit F1–F4/F8): every feature family has a key ---------------

        /// F1 (skew): the three `with_skew` knobs parse from the flat table into the maker verbatim.
        #[test]
        fn from_params_reads_the_skew_keys() {
            let p: Params =
                toml::from_str("target_inventory = 5.0\nmax_inventory = 40.0\nskew = 0.6").unwrap();
            let m = SpreadMaker::from_params(&p).unwrap();
            assert_eq!(m.config.target_inventory, 5.0);
            assert_eq!(m.config.max_inventory, 40.0);
            assert_eq!(m.config.skew, 0.6);
        }

        /// F2 (breaker): the three `with_fill_breaker` knobs parse and ENGAGE the breaker
        /// (`breaker_enabled` needs all three positive), plus the `AsParams` pull-accel pair that makes
        /// the #798 accelerated pull reachable from the same table.
        #[test]
        fn from_params_reads_the_breaker_and_pull_accel_keys() {
            let p: Params = toml::from_str(
                "fill_window_ms = 5000\nnet_fill_threshold = 60.0\nsuppress_cooldown_ms = 10000\n\
                 pull_accel_ramp_ms = 60000\npull_accel_max = 4.0",
            )
            .unwrap();
            let m = SpreadMaker::from_params(&p).unwrap();
            assert_eq!(m.config.fill_window_ms, 5_000);
            assert_eq!(m.config.net_fill_threshold, 60.0);
            assert_eq!(m.config.suppress_cooldown_ms, 10_000);
            assert!(m.breaker_enabled(), "all three positive ⇒ the breaker engages");
            let as_p = m.params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.pull_accel_ramp_ms, 60_000);
            assert_eq!(as_p.pull_accel_max, 4.0);
        }

        /// F3 (the #790 κ-fit family): `kappa_mode` selects the fit and the clamp/window/sample-floor
        /// keys parse. ⚠ The line that used to close this test — `kappa_mode = "nonsense"` ⇒ `Fixed`,
        /// "the safe default" — is GONE: a mode that names no variant now fails the whole read (see
        /// `unrecognized_kappa_mode_is_an_error`), because "safe" was only ever true if the operator had
        /// not meant one of the real fits.
        #[test]
        fn from_params_reads_the_kappa_mode_family() {
            let p: Params = toml::from_str(
                "kappa_mode = \"live_fit\"\nkappa_default = 25.0\nkappa_min = 2.0\n\
                 kappa_max = 500.0\nn_min = 8\ntrade_window_ms = 30000",
            )
            .unwrap();
            let as_p = SpreadMaker::from_params(&p).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.kappa_mode, KappaMode::LiveFit);
            assert_eq!(as_p.kappa_default, 25.0);
            assert_eq!(as_p.kappa_min, 2.0);
            assert_eq!(as_p.kappa_max, 500.0);
            assert_eq!(as_p.n_min, 8);
            assert_eq!(as_p.trade_window_ms, 30_000);
            let own: Params = toml::from_str("kappa_mode = \"own_fill_fit\"").unwrap();
            let as_own =
                SpreadMaker::from_params(&own).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_own.kappa_mode, KappaMode::OwnFillFit);
        }

        /// F4 (settlement): `resolution_ts` — the anchor every settlement feature keys off — plus the
        /// tenor/blackout/terminal/underlying/ATM knobs all parse into `AsParams`.
        #[test]
        fn from_params_reads_the_settlement_and_underlying_keys() {
            let p: Params = toml::from_str(
                "resolution_ts = 1700000000000\ntau_hold_ms = 600000\nresolution_blackout_ms = 30000\n\
                 terminal_penalty_gamma = 0.5\nterminal_ramp_ms = 120000\natm_blackout_scale = 2.0\n\
                 underlying_weight = 0.7\nunderlying_beta = 1.5\nwindow_secs = 300.0",
            )
            .unwrap();
            let as_p = SpreadMaker::from_params(&p).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.resolution_ts, Some(1_700_000_000_000));
            assert_eq!(as_p.tau_hold_ms, 600_000);
            assert_eq!(as_p.resolution_blackout_ms, 30_000);
            assert_eq!(as_p.terminal_penalty_gamma, 0.5);
            assert_eq!(as_p.terminal_ramp_ms, 120_000);
            assert_eq!(as_p.atm_blackout_scale, 2.0);
            assert_eq!(as_p.underlying_weight, 0.7);
            assert_eq!(as_p.underlying_beta, 1.5);
            assert_eq!(as_p.window_secs, 300.0);
            // absent ⇒ no resolution known (the default None), not a zero.
            let none: Params = toml::from_str("qty = 1.0").unwrap();
            let as_none =
                SpreadMaker::from_params(&none).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_none.resolution_ts, None, "absent resolution_ts stays None");
        }

        /// F1 (toxicity): a present `toxicity_*` key builds the [`ToxicityParams`] guard (either key
        /// alone works, the other axis defaulting `0.0`) and `ofi_toxicity_scale` reaches `AsParams`,
        /// so the #798 OFI synthesis can actually fire from a registry table. Absent ⇒ NO bag at all.
        #[test]
        fn from_params_reads_the_toxicity_keys() {
            let p: Params = toml::from_str(
                "toxicity_widen = 1.5\ntoxicity_size_cut = 0.8\nofi_toxicity_scale = 2.0\n\
                 alpha_lambda_ofi = 0.1",
            )
            .unwrap();
            let m = SpreadMaker::from_params(&p).unwrap();
            assert_eq!(m.config.toxicity, Some(ToxicityParams { widen: 1.5, size_cut: 0.8 }));
            let as_p = m.params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.ofi_toxicity_scale, 2.0);
            // one key alone still builds the bag (the other axis stays 0.0 — off).
            let one: Params = toml::from_str("toxicity_widen = 1.0").unwrap();
            assert_eq!(
                SpreadMaker::from_params(&one).unwrap().config.toxicity,
                Some(ToxicityParams { widen: 1.0, size_cut: 0.0 })
            );
            // absent ⇒ no bag at all (not an all-zero Some), keeping `params()` byte-identical.
            let none: Params = toml::from_str("qty = 1.0").unwrap();
            assert!(SpreadMaker::from_params(&none).unwrap().config.toxicity.is_none());
        }

        /// F8 (pricing selector, the #803 inert-`style` finding): `pricing = "style"` SKIPS the A-S
        /// layer so the QuoteStyle placement actually prices (under `"as"`, `requote` bypasses the
        /// style whenever A-S state is `Some`); absent/`"as"`/unrecognized keeps today's A-S chaining.
        #[test]
        fn from_params_pricing_style_skips_the_as_layer() {
            let style: Params =
                toml::from_str("pricing = \"style\"\nstyle = \"top\"\ntick_size = 0.01").unwrap();
            let m = SpreadMaker::from_params(&style).unwrap();
            assert!(m.params().avellaneda_stoikov.is_none(), "style pricing mounts NO A-S state");
            assert_eq!(
                m.config.style,
                QuoteStyle::Top,
                "the style key now genuinely selects placement"
            );
            for tbl in ["pricing = \"as\"", "pricing = \"nonsense\"", "qty = 1.0"] {
                let v: Params = toml::from_str(tbl).unwrap();
                assert!(
                    SpreadMaker::from_params(&v).unwrap().params().avellaneda_stoikov.is_some(),
                    "absent/as/unrecognized ⇒ the A-S layer chains as before ({tbl})"
                );
            }
        }

        /// The absent-key guarantee, whole-surface: a keyless table builds a maker whose ENTIRE
        /// observable config (`params()`) equals today's construction — the exact pre-audit chain
        /// (`new → with_quote_style → with_avellaneda_stoikov → with_refresh_tolerance`) — so every key
        /// the reachability audit added is proven additive in one assert.
        #[test]
        fn from_params_keyless_table_is_byte_identical_to_todays_construction() {
            let empty = Params::new();
            let got = SpreadMaker::from_params(&empty).unwrap().params();
            let want = SpreadMaker::new(1.0, 0.01)
                .with_quote_style(QuoteStyle::Mid, 1, 0.0)
                .with_avellaneda_stoikov(AsParams::default())
                .with_refresh_tolerance(0.0, 0.0)
                .params();
            assert_eq!(got, want, "a keyless params table must equal today's maker exactly");
        }

        // --- present-but-unrecognized enum values are ERRORS, not fallbacks ---------------------------
        //
        // One test per string→enum reader. `None` used to mean BOTH "absent" and "present and
        // unrecognized", and `unwrap_or(default)` could not tell them apart — so a one-character typo ran
        // a DIFFERENT model than the profile named, silently, and a startup line reported the fallback as
        // if it had been chosen. The absent-key half of the contract is unchanged and proven above by
        // `from_params_keyless_table_is_byte_identical_to_todays_construction`.

        /// The shared assertion: `key = "<bad>"` fails the whole read, and the failure hands the operator
        /// the three things they need to fix their profile — the KEY, their OWN spelling back verbatim,
        /// and the ACCEPTED spellings (`a_spelling` is one of them, spot-checked).
        fn assert_unrecognized(key: &str, bad: &str, a_spelling: &str) {
            let v: Params = toml::from_str(&format!("{key} = \"{bad}\"")).unwrap();
            // `let Err(..) else`, not `expect_err`: the Ok side is a `SpreadMaker`, which is not `Debug`.
            let Err(err) = SpreadMaker::from_params(&v) else {
                panic!("{key} = {bad:?} must not resolve to the default");
            };
            let MakerParamError::Unrecognized { key: got_key, value, accepted } = &err else {
                panic!("expected MakerParamError::Unrecognized, got {err:?}");
            };
            assert_eq!(*got_key, key, "the error names the offending key");
            assert_eq!(value, bad, "the error quotes the operator's own value back");
            assert!(
                accepted.contains(a_spelling),
                "accepted spellings list {a_spelling}: {accepted}"
            );
            let msg = err.to_string();
            for needle in [key, bad, a_spelling] {
                assert!(msg.contains(needle), "the message must name {needle:?}, got: {msg}");
            }
        }

        /// The motivating case: `gueant_lehale` is ONE `l` short of the accepted `gueant_lehalle`, and it
        /// used to run Avellaneda–Stoikov under a profile that named GLFT. Also pins the two halves that
        /// must NOT change — an accepted spelling still resolves, case-insensitively.
        #[test]
        fn unrecognized_spread_model_is_an_error() {
            assert_unrecognized("spread_model", "gueant_lehale", "gueant_lehalle");
            let ok: Params = toml::from_str("spread_model = \"GLFT\"").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(
                as_p.spread_model,
                SpreadModel::Gueant,
                "accepted spellings stay case-insensitive"
            );
            // Present but not a string at all — `ParamValue::as_str` answered `None` for this too, so it
            // rode the very same silent fallback.
            let typed: Params = toml::from_str("spread_model = 3").unwrap();
            let Err(err) = SpreadMaker::from_params(&typed) else {
                panic!("a non-string value must not fall back to the default");
            };
            assert_matches!(
                err,
                MakerParamError::NotAString { key: "spread_model", found: "integer", .. },
                "got {err:?}"
            );
            assert!(err.to_string().contains("must be a string"), "{err}");
        }

        #[test]
        fn unrecognized_variance_mode_is_an_error() {
            assert_unrecognized("variance_mode", "raw_locale", "raw_local");
            let ok: Params = toml::from_str("variance_mode = \"bernoulli\"").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.variance_mode, VarianceMode::PureBernoulli);
        }

        #[test]
        fn unrecognized_horizon_mode_is_an_error() {
            assert_unrecognized("horizon_mode", "constant_taus", "constant_tau");
            let ok: Params = toml::from_str("horizon_mode = \"constant\"").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.horizon_mode, HorizonMode::ConstantTau);
        }

        /// `price_domain` is the one reader with a non-trivial arm (`band` also reads `band_lo`/`band_hi`),
        /// so the accepted half checks that arm rather than a bare variant.
        #[test]
        fn unrecognized_price_domain_is_an_error() {
            assert_unrecognized("price_domain", "unbound", "unbounded");
            let ok: Params =
                toml::from_str("price_domain = \"band\"\nband_lo = 0.2\nband_hi = 0.9").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.price_domain, PriceDomain::Band { lo: 0.2, hi: 0.9 });
        }

        #[test]
        fn unrecognized_kappa_mode_is_an_error() {
            assert_unrecognized("kappa_mode", "nonsense", "own_fill_fit");
            let ok: Params = toml::from_str("kappa_mode = \"live\"").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.kappa_mode, KappaMode::LiveFit);
        }

        #[test]
        fn unrecognized_reservation_model_is_an_error() {
            assert_unrecognized("reservation_model", "lmsr_logistic", "logistic");
            let ok: Params = toml::from_str("reservation_model = \"lmsr\"").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.reservation_model, ReservationModel::Lmsr);
        }

        #[test]
        fn unrecognized_spread_source_is_an_error() {
            assert_unrecognized("spread_source", "glosten_milgram", "glosten_milgrom");
            let ok: Params = toml::from_str("spread_source = \"ls_lmsr\"").unwrap();
            let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
            assert_eq!(as_p.spread_source, SpreadSource::LsLmsr);
        }

        /// ⚠ The KEY is `style`, not `quote_style` — and unlike its siblings this one lands on `config`, not
        /// on `AsParams`, so it is read even when the A-S layer is skipped.
        #[test]
        fn unrecognized_quote_style_is_an_error() {
            assert_unrecognized("style", "topp", "top");
            let ok: Params = toml::from_str("pricing = \"style\"\nstyle = \"join\"").unwrap();
            assert_eq!(SpreadMaker::from_params(&ok).unwrap().config.style, QuoteStyle::Join);
        }

        // --- the presets, the key set and the fee re-arm (`preset.rs`) --------------------------------

        fn table(src: &str) -> Params {
            toml::from_str(src).unwrap()
        }

        #[test]
        fn unrecognized_preset_is_an_error() {
            assert_unrecognized("preset", "cryptoo", "crypto");
        }

        /// The two presets hold exactly the knobs `vike_tradehub_core::MakerConfig::{outcome_token,
        /// crypto}` held before step 10 moved them here, every other knob at its default.
        #[test]
        fn the_presets_hold_the_knobs_the_mount_presets_held() {
            let o =
                SpreadMaker::from_params(&table("preset = \"outcome_token\"")).unwrap().params();
            assert_eq!((o.qty, o.tick_size, o.half_spread), (20.0, 0.01, 0.01));
            assert_eq!(o.avellaneda_stoikov, Some(AsParams::default()));
            let c = SpreadMaker::from_params(&table("preset = \"crypto\"")).unwrap().params();
            assert_eq!(
                (c.qty, c.tick_size, c.half_spread),
                (0.005, 1.0, 2.0),
                "half_spread is 2 ticks"
            );
            assert_eq!(
                c.avellaneda_stoikov,
                Some(AsParams {
                    variance_mode: VarianceMode::RawLocal,
                    horizon_mode: HorizonMode::ConstantTau,
                    price_domain: PriceDomain::Unbounded,
                    min_half_spread_ticks: 2.0,
                    max_half_spread_ticks: 60.0,
                    gamma: 5e-4,
                    q_scale: 3e-2,
                    ..AsParams::default()
                }),
                "no fee floor: the MOUNT arms it from its venue (`SpreadMakerPreset::arms_fee_floor`)"
            );
            for (p, only) in [(o, SpreadMakerPreset::OutcomeToken), (c, SpreadMakerPreset::Crypto)]
            {
                assert_eq!(
                    (p.refresh_tolerance, p.toxicity, p.reward, p.ladder),
                    (None, None, None, None),
                    "{only:?}: every opt-in bag OFF"
                );
            }
            assert!(SpreadMakerPreset::Crypto.arms_fee_floor());
            assert!(!SpreadMakerPreset::OutcomeToken.arms_fee_floor());
        }

        /// A table's key overrides its preset's, key by key; and under a preset the tolerance bag is built
        /// only when a refresh key asks (the mount's convention), while a bare table keeps its inert
        /// all-zero bag (the registry's, pinned by
        /// `from_params_keyless_table_is_byte_identical_to_todays_construction`).
        #[test]
        fn a_table_overrides_its_preset_and_the_tolerance_bag_is_opt_in_under_one() {
            let p = SpreadMaker::from_params(&table(
                "preset = \"crypto\"\ntick_size = 0.1\nqty = 0.001\ngamma = 0.002\nresolution_ts = 7",
            ))
            .unwrap()
            .params();
            assert_eq!((p.qty, p.tick_size, p.half_spread), (0.001, 0.1, 0.2));
            let a = p.avellaneda_stoikov.unwrap();
            assert_eq!((a.gamma, a.q_scale, a.resolution_ts), (0.002, 3e-2, Some(7)));
            assert_eq!(p.refresh_tolerance, None, "no refresh key: no bag under a preset");
            let asked =
                SpreadMaker::from_params(&table("preset = \"crypto\"\nrefresh_price_bps = 25"))
                    .unwrap()
                    .params();
            assert_eq!(
                asked.refresh_tolerance,
                Some(RefreshTolerance { price_bps: 25.0, size_bps: 0.0 })
            );
            assert_eq!(
                SpreadMaker::from_params(&Params::new()).unwrap().params().refresh_tolerance,
                Some(RefreshTolerance { price_bps: 0.0, size_bps: 0.0 }),
                "a bare table keeps the registry's bag"
            );
        }

        /// `SpreadMaker::PARAM_KEYS` against the reader's SOURCE, both directions: every key looked up
        /// (`f("…")`, `ms("…")`, `get("…")`, `contains_key("…")`, an enum reader's `KEY`) is declared, and
        /// every declared key is looked up. A key read and not declared is one a mount REFUSES though the
        /// maker takes it; a key declared and never read is one a mount ACCEPTS while the knob runs at its
        /// default.
        #[test]
        fn the_declared_keys_are_exactly_the_keys_the_reader_looks_up() {
            // The reader is this file above its test module, plus `vike_market_making::SpreadMakerPreset::named`
            // (in `crates/vike-market-making/src/lib.rs`), which reads `preset`.
            let file = include_str!("spread_maker.rs");
            let reader =
                &file[..file.find("\n#[cfg(test)]\nmod tests {").expect("the test module")];
            let src = [reader, include_str!("../../crates/vike-market-making/src/lib.rs")].concat();
            let mut read = std::collections::BTreeSet::new();
            for needle in ["f(\"", "ms(\"", "get(\"", "contains_key(\"", "KEY: &str = \""] {
                for (at, _) in src.match_indices(needle) {
                    let rest = &src[at + needle.len()..];
                    read.insert(&rest[..rest.find('"').unwrap()]);
                }
            }
            let declared: std::collections::BTreeSet<&str> =
                SpreadMaker::PARAM_KEYS.iter().map(|(k, _)| *k).collect();
            assert_eq!(read, declared, "the reader and SpreadMaker::PARAM_KEYS disagree");
            assert_eq!(declared.len(), SpreadMaker::PARAM_KEYS.len(), "a key is declared twice");
            for preset in [SpreadMakerPreset::OutcomeToken, SpreadMakerPreset::Crypto] {
                for key in preset.params().keys() {
                    assert!(
                        declared.contains(key.as_str()),
                        "{preset:?} sets `{key}`, which nothing reads"
                    );
                }
            }
        }

        /// ...and behaviourally: two values of every declared key, each of a kind the key takes, build two
        /// different makers (`band_lo`/`band_hi` inside the `band` domain that reads them).
        #[test]
        fn every_declared_key_moves_the_maker() {
            let contrast = |key: &str| -> (ParamValue, ParamValue) {
                let (a, b) = match key {
                    "preset" => ("outcome_token", "crypto"),
                    "variance_mode" => ("local_capped", "raw_local"),
                    "horizon_mode" => ("time_to_resolution", "constant_tau"),
                    "price_domain" => ("unit_interval", "unbounded"),
                    "pricing" => ("as", "style"),
                    "style" => ("top", "join"),
                    "kappa_mode" => ("fixed", "live_fit"),
                    "spread_model" => ("avellaneda_stoikov", "gueant"),
                    "reservation_model" => ("as_linear", "lmsr"),
                    "spread_source" => ("as_optimal", "ls_lmsr"),
                    _ => return (3.0.into(), 7.0.into()),
                };
                (a.into(), b.into())
            };
            for (key, kinds) in SpreadMaker::PARAM_KEYS {
                let (a, b) = contrast(key);
                assert!(
                    kinds.contains(&a.type_str()) && kinds.contains(&b.type_str()),
                    "{key}'s probes"
                );
                let built = |v: ParamValue| {
                    let mut t = Params::new();
                    if key.starts_with("band_") {
                        t.insert("price_domain", "band");
                    }
                    t.insert(*key, v);
                    format!("{:?}", SpreadMaker::from_params(&t).unwrap().params())
                };
                assert_ne!(
                    built(a),
                    built(b),
                    "`{key}` is declared, and two values build ONE maker"
                );
            }
        }
    }

    mod ladder_quoting {
        //! LADDER quoting (steal/mm-ladder-quoting): N rungs per side diffed to the minimal verb set, the
        //! single-quote reduction at `levels <= 1`, per-rung tolerance, whole-side pulls, and live toggling.

        use super::*;

        // ---- ladder quoting (steal/mm-ladder-quoting) ----
        //
        // A LADDER rests N rungs per side (tags bid0..bidN / ask0..askN) stepping out from the SAME
        // reservation the single quote uses. These pin: the OFF/1-level path is byte-identical to today's
        // two-tag maker; an active ladder places the right rungs at the right prices/sizes and diffs
        // minimally; a suppressed side pulls ALL its rungs (never strands one); the size profile shapes
        // rung sizes; and the whole thing is live-tunable across the single↔ladder boundary.

        /// A Mid ladder with `levels` rungs per side: one half-spread between rungs, flat sizes — the
        /// simplest shape (the geometry/size tests below vary this).
        fn mid_ladder(levels: usize) -> LadderParams {
            LadderParams {
                levels,
                offset_step: 1.0,
                offset_unit: LadderOffsetUnit::HalfSpread,
                size_profile: LadderSizeProfile::Linear,
                size_ratio: 1.0,
            }
        }

        // THE OFF-PATH PROOF: neither a maker with NO ladder NOR one with an inert (empty/1-level) ladder
        // changes the pre-feature verbs — a SINGLE order per side, tagged EXACTLY "bid"/"ask" (never
        // "bid0"), priced by the unchanged mid formula, re-priced in place next tick. This is the
        // byte-identical-when-off gate.
        #[test]
        fn off_and_single_level_ladder_reproduce_todays_two_tag_maker() {
            for ladder in [None, Some(mid_ladder(0)), Some(mid_ladder(1))] {
                let mut mm = SpreadMaker::new(1.0, 0.5);
                if let Some(lp) = ladder {
                    mm = mm.with_ladder(lp);
                }
                let mut b = broker(0.0, 1);
                mm.on_quote_tick(&mut b, &quote(1, 100.0, 100.2));
                // exactly the two single-quote submits, with the pre-feature tags (no rung tags)
                assert_eq!(b.submissions.len(), 2, "one order per side ({ladder:?})");
                assert!(submitted(&b, "bid") && submitted(&b, "ask"), "tags bid/ask ({ladder:?})");
                assert!(!submitted(&b, "bid0"), "never a bid0 rung on the off path ({ladder:?})");
                assert!(!submitted(&b, "ask0"), "never an ask0 rung on the off path ({ladder:?})");
                assert!(
                    b.modifications.is_empty() && b.cancels.is_empty(),
                    "nothing else ({ladder:?})"
                );
                // and the price is today's mid formula, bit-for-bit
                let mid = 0.5 * (100.0_f64 + 100.2);
                assert_eq!(
                    submit_at(&b, "bid").0.to_bits(),
                    (mid - 0.5).to_bits(),
                    "bid unchanged"
                );
                assert_eq!(
                    submit_at(&b, "ask").0.to_bits(),
                    (mid + 0.5).to_bits(),
                    "ask unchanged"
                );
                // second tick modifies the SAME two tags in place — no rung tag ever appears
                let mut b2 = broker(0.0, 2);
                mm.on_quote_tick(&mut b2, &quote(2, 100.0, 100.4));
                assert_eq!(b2.modifications.len(), 2, "two in-place modifies ({ladder:?})");
                assert!(modified(&b2, "bid") && modified(&b2, "ask"), "same tags ({ladder:?})");
                assert!(
                    b2.submissions.is_empty() && b2.cancels.is_empty(),
                    "nothing else ({ladder:?})"
                );
            }
        }

        // An ACTIVE 3-rung ladder rests three orders per side at the reservation ± k half-spreads, tagged
        // bid0/bid1/bid2 & ask0/ask1/ask2, then re-prices them in place next tick (no submit/cancel churn).
        #[test]
        fn active_ladder_rests_rungs_at_stepped_prices() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3));
            let mut b = broker(0.0, 1);
            // mid 100.5, half_spread 0.5 → bid0 100.0 / ask0 101.0, half 0.5 between rungs
            mm.on_quote_tick(&mut b, &quote(1, 100.0, 101.0));
            assert_eq!(b.submissions.len(), 6, "three rungs per side");
            // bid rungs step DOWN one half-spread (0.5) each; ask rungs step UP; flat size 1.0
            for (tag, px) in [
                ("bid0", 100.0f64),
                ("bid1", 99.5),
                ("bid2", 99.0),
                ("ask0", 101.0),
                ("ask1", 101.5),
                ("ask2", 102.0),
            ] {
                assert_eq!(submit_at(&b, tag).0.to_bits(), px.to_bits(), "{tag} price");
                assert_eq!(submit_at(&b, tag).1.to_bits(), 1.0_f64.to_bits(), "{tag} flat size");
            }
            assert!(b.cancels.is_empty(), "nothing pulled on the first ladder tick");
            // next tick, same book → six in-place modifies, no new submits, no cancels (minimal diff)
            let mut b2 = broker(0.0, 2);
            mm.on_quote_tick(&mut b2, &quote(2, 100.0, 101.0));
            assert_eq!(b2.modifications.len(), 6, "all six rungs re-priced in place");
            assert!(b2.submissions.is_empty() && b2.cancels.is_empty(), "no churn: modify-only");
        }

        // The per-rung refresh tolerance gates each rung's re-price: an unchanged/sub-tolerance tick
        // re-quotes NOTHING (all rungs inside tolerance); without the gate the same tick re-quotes all six.
        #[test]
        fn ladder_honors_refresh_tolerance_per_rung() {
            // gate OFF: an identical second tick still re-prices every rung
            let mut plain = SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3));
            plain.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0));
            let mut p2 = broker(0.0, 2);
            plain.on_quote_tick(&mut p2, &quote(2, 100.0, 101.0));
            assert_eq!(p2.modifications.len(), 6, "gate off ⇒ every rung re-quotes");

            // gate ON (wide, price-only): a microscopic move leaves ALL rungs alone
            let mut gated = SpreadMaker::new(1.0, 0.5)
                .with_ladder(mid_ladder(3))
                .with_refresh_tolerance(25.0, 0.0);
            gated.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0));
            let mut g2 = broker(0.0, 2);
            gated.on_quote_tick(&mut g2, &quote(2, 100.001, 101.001));
            assert!(g2.modifications.is_empty(), "a sub-tolerance move skips every rung");
            assert!(g2.submissions.is_empty() && g2.cancels.is_empty(), "nothing sent at all");
            // a real move re-quotes every rung again
            let mut g3 = broker(0.0, 3);
            gated.on_quote_tick(&mut g3, &quote(3, 100.5, 101.5));
            assert_eq!(g3.modifications.len(), 6, "a real move re-prices all rungs");
        }

        // Under the fill-rate breaker, a suppressed side pulls EVERY rung (never strands one), while the
        // un-hit side keeps laddering in place.
        #[test]
        fn ladder_suppression_pulls_all_rungs_of_the_hit_side() {
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_ladder(mid_ladder(3))
                .with_fill_breaker(1000, 2.5, 5000);
            let mut b0 = broker(0.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
            assert_eq!(b0.submissions.len(), 6, "both sides laddered first");

            // three bid fills net +3 >= 2.5 → suppress the bid side
            for t in [110, 120, 130] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            let mut b1 = broker(3.0, 140);
            mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
            // EVERY bid rung is pulled — none stranded, none re-quoted while suppressed
            for tag in ["bid0", "bid1", "bid2"] {
                assert!(canceled(&b1, tag), "{tag} pulled under suppression");
                assert!(!submitted(&b1, tag) && !modified(&b1, tag), "{tag} not re-quoted");
            }
            // and the un-hit ask side keeps quoting all its rungs in place
            for tag in ["ask0", "ask1", "ask2"] {
                assert!(modified(&b1, tag), "{tag} keeps quoting");
                assert!(!canceled(&b1, tag), "{tag} never pulled");
            }
        }

        // A live re-tune to FEWER levels cancels exactly the now-extra rungs and re-prices the kept ones —
        // the minimal diff, never a full cancel/replace of the side.
        #[test]
        fn ladder_shrink_cancels_only_the_extra_rungs() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3));
            mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0)); // rest 6 rungs

            // re-tune to 2 levels per side; the update itself touches no order (queue preserved)
            let two = SpreadMakerParams { ladder: Some(mid_ladder(2)), ..mm.params() };
            let mut bp = broker(0.0, 2);
            mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(two));
            assert!(
                bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
                "a re-tune never disturbs an order itself"
            );

            let mut b = broker(0.0, 3);
            mm.on_quote_tick(&mut b, &quote(3, 100.0, 101.0));
            // the deepest rung each side is canceled; the two kept rungs re-price in place
            assert!(canceled(&b, "bid2") && canceled(&b, "ask2"), "the extra rungs are pulled");
            for tag in ["bid0", "bid1", "ask0", "ask1"] {
                assert!(modified(&b, tag), "{tag} kept + re-priced");
                assert!(!canceled(&b, tag), "{tag} never pulled");
            }
            assert!(b.submissions.is_empty(), "no rung is re-submitted (queue preserved)");
        }

        // The size profile shapes rung SIZES off the base quote: geometric ×2 gives 1, 2, 4 per rung.
        #[test]
        fn ladder_size_profile_shapes_rung_sizes() {
            let lp = LadderParams {
                levels: 3,
                offset_step: 1.0,
                offset_unit: LadderOffsetUnit::HalfSpread,
                size_profile: LadderSizeProfile::Geometric,
                size_ratio: 2.0,
            };
            let mut mm = SpreadMaker::new(1.0, 0.5).with_ladder(lp);
            let mut b = broker(0.0, 1);
            mm.on_quote_tick(&mut b, &quote(1, 100.0, 101.0));
            for (tag, sz) in [
                ("bid0", 1.0f64),
                ("bid1", 2.0),
                ("bid2", 4.0),
                ("ask0", 1.0),
                ("ask1", 2.0),
                ("ask2", 4.0),
            ] {
                assert_eq!(submit_at(&b, tag).1.to_bits(), sz.to_bits(), "{tag} geometric size");
            }
        }

        // Live-tunable across the single↔ladder boundary WITHOUT a remount: turning the ladder ON retires
        // the single "bid"/"ask" and rests the rungs; turning it OFF retires the rungs and restores the
        // single quote — no order is stranded either way.
        #[test]
        fn ladder_toggles_live_without_stranding_orders() {
            let mut mm = SpreadMaker::new(1.0, 0.5); // single-quote maker
            mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0));

            // turn the ladder ON over the plane
            let on = SpreadMakerParams { ladder: Some(mid_ladder(3)), ..mm.params() };
            mm.on_params_updated(&mut broker(0.0, 2), &StrategyParams::SpreadMaker(on));
            let mut b = broker(0.0, 3);
            mm.on_quote_tick(&mut b, &quote(3, 100.0, 101.0));
            assert!(canceled(&b, "bid") && canceled(&b, "ask"), "the single quote is retired");
            for tag in ["bid0", "bid1", "bid2", "ask0", "ask1", "ask2"] {
                assert!(submitted(&b, tag), "{tag} rung placed");
            }

            // turn it back OFF
            let off = SpreadMakerParams { ladder: None, ..mm.params() };
            mm.on_params_updated(&mut broker(0.0, 4), &StrategyParams::SpreadMaker(off));
            let mut b2 = broker(0.0, 5);
            mm.on_quote_tick(&mut b2, &quote(5, 100.0, 101.0));
            for tag in ["bid0", "bid1", "bid2", "ask0", "ask1", "ask2"] {
                assert!(canceled(&b2, tag), "{tag} rung retired");
            }
            assert!(submitted(&b2, "bid") && submitted(&b2, "ask"), "the single quote is restored");
        }
    }

    mod liquidity_rewards {
        //! Liquidity-rewards-aware quoting (steal/mm-rewards-quoting): the band clamp at `min_size`, the
        //! min-order-age hold, and the two safety authorities (A-S blackout / breaker pull) that outrank it.

        use super::*;

        // ---- liquidity-rewards-aware quoting (steal/mm-rewards-quoting) ----
        //
        // The reward fold pulls the quote into a venue's reward band at min_size on both sides and holds
        // an in-band quote to the moas floor. These pin: OFF-by-default is byte-identical; ON pulls a
        // too-wide quote into the band at min_size; the A-S blackout is never reward-clamped; and the moas
        // holds a young in-band quote then releases it.

        /// A rewardable bag (weight, band-cents, min_size, moas) for the integration tests.
        fn reward_bag(
            weight: f64,
            max_spread_cents: f64,
            min_size: f64,
            moas: i64,
        ) -> RewardParams {
            RewardParams { weight, max_spread_cents, min_size, min_order_age_ms: moas }
        }

        // OFF BY DEFAULT (the neutral-reduction gate): an unconfigured maker — and one with an explicit
        // weight-0 bag — quotes the unchanged mid formula at the unchanged size, bit-for-bit.
        #[test]
        fn reward_off_by_default_quotes_unchanged() {
            let mut mm = SpreadMaker::new(1.0, 0.05); // half_spread 0.05 ⇒ candidate 0.45/0.55 on a 0.50 mid
            assert!(mm.config.reward.is_none(), "reward chasing is OFF by default");
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 0.48, 0.52)); // book mid 0.50
            let mid = 0.5 * (0.48_f64 + 0.52);
            assert_eq!(
                submit_at(&b0, "bid").0.to_bits(),
                (mid - 0.05).to_bits(),
                "bid unchanged (mid − hs)"
            );
            assert_eq!(
                submit_at(&b0, "ask").0.to_bits(),
                (mid + 0.05).to_bits(),
                "ask unchanged (mid + hs)"
            );
            assert_eq!(submit_at(&b0, "bid").1.to_bits(), 1.0_f64.to_bits(), "bid size unchanged");

            // an explicitly weight-0 bag is likewise inert — same price AND size
            let mut inert = SpreadMaker::new(1.0, 0.05)
                .with_liquidity_rewards(reward_bag(0.0, 3.0, 100.0, 30_000));
            let mut b1 = broker(0.0, 1);
            inert.on_quote_tick(&mut b1, &quote(1, 0.48, 0.52));
            assert_eq!(
                submit_at(&b1, "bid").0.to_bits(),
                (mid - 0.05).to_bits(),
                "weight 0 ⇒ price unchanged"
            );
            assert_eq!(
                submit_at(&b1, "bid").1.to_bits(),
                1.0_f64.to_bits(),
                "weight 0 ⇒ size unchanged (no min_size floor)"
            );
        }

        // ON with a rewardable market: a too-wide candidate is PULLED INTO the band, and both sides are
        // floored UP to min_size — the core reward-eligibility behavior.
        #[test]
        fn reward_on_pulls_quote_into_band_at_min_size() {
            // weight 0.5, band 3¢ (0.03), min_size 100; the half_spread-0.05 candidate is OUT of band.
            let mut mm =
                SpreadMaker::new(1.0, 0.05).with_liquidity_rewards(reward_bag(0.5, 3.0, 100.0, 0));
            let mut b = broker(0.0, 1);
            mm.on_quote_tick(&mut b, &quote(1, 0.48, 0.52)); // book mid 0.50, candidate 0.45 / 0.55
            let (bid_price, bid_size) = submit_at(&b, "bid");
            let (ask_price, ask_size) = submit_at(&b, "ask");
            // pulled INTO the band: each side within 3¢ (0.03) of the mid 0.50 …
            assert!((0.50 - bid_price) <= 0.03 + 1e-12, "bid pulled in-band: {bid_price}");
            assert!((ask_price - 0.50) <= 0.03 + 1e-12, "ask pulled in-band: {ask_price}");
            // … specifically to s_target = band·(1−weight) = 0.03·0.5 = 0.015 (the candidate was wider)
            assert!((bid_price - 0.485).abs() < 1e-12, "bid at mid − 0.015: {bid_price}");
            assert!((ask_price - 0.515).abs() < 1e-12, "ask at mid + 0.015: {ask_price}");
            // sizes floored UP to min_size on BOTH sides (eligibility)
            assert_eq!(bid_size.to_bits(), 100.0_f64.to_bits(), "bid size floored to min_size");
            assert_eq!(ask_size.to_bits(), 100.0_f64.to_bits(), "ask size floored to min_size");
        }

        // SAFETY: inside the A-S near-resolution blackout the maker quotes at the WALLS and is NEVER
        // reward-clamped back into the band (that would farm rewards into the resolution vol spike).
        #[test]
        fn reward_never_clamps_through_an_as_blackout() {
            // A-S in time-to-resolution with a blackout; reward chasing ON at full weight.
            let as_params = AsParams {
                horizon_mode: HorizonMode::TimeToResolution,
                resolution_ts: Some(2_000),
                resolution_blackout_ms: 200, // ts >= 1800 is blacked out
                variance_mode: VarianceMode::PureBernoulli,
                kappa_mode: KappaMode::Fixed,
                gamma: 0.5,
                q_scale: 1.0,
                min_standoff_ticks: 1.0,
                ..AsParams::default()
            };
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(as_params)
                .with_liquidity_rewards(reward_bag(1.0, 3.0, 0.0, 0));
            // INSIDE the blackout (ts 1900): pushed to the walls [tick, 1−tick] = [0.01, 0.99], NOT
            // reward-clamped back into the ±3¢ band around 0.50.
            let mut b = broker(0.0, 1_900);
            mm.on_quote_tick(&mut b, &quote(1_900, 0.49, 0.51)); // mid 0.50
            assert_eq!(
                submit_at(&b, "bid").0.to_bits(),
                0.01_f64.to_bits(),
                "blackout bid stays at the wall (not reward-clamped in)"
            );
            assert_eq!(
                submit_at(&b, "ask").0.to_bits(),
                0.99_f64.to_bits(),
                "blackout ask stays at the wall (not reward-clamped in)"
            );
        }

        // The moas gate HOLDS a young in-band quote (so it does not churn below the reward floor) then
        // RELEASES it once it has rested >= min_order_age_ms.
        #[test]
        fn reward_moas_holds_a_young_in_band_quote_then_releases() {
            // reward ON with a 30s moas; NO refresh tolerance, so absent the moas every tick would
            // re-price — the moas is what holds the young in-band quote.
            let mut mm = SpreadMaker::new(1.0, 0.05)
                .with_liquidity_rewards(reward_bag(0.5, 3.0, 0.0, 30_000));
            // tick 1 (ts 1_000): rests both sides in-band
            let mut b0 = broker(0.0, 1_000);
            mm.on_quote_tick(&mut b0, &quote(1_000, 0.48, 0.52));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

            // tick 2 (ts 11_000, age 10s < 30s), same in-band mid: the young quote is HELD — nothing sent
            let mut b1 = broker(0.0, 11_000);
            mm.on_quote_tick(&mut b1, &quote(11_000, 0.48, 0.52));
            assert!(
                b1.modifications.is_empty(),
                "young in-band quote is held for the reward floor"
            );
            assert!(
                b1.submissions.is_empty() && b1.cancels.is_empty(),
                "nothing sent while holding"
            );

            // tick 3 (ts 31_000, age 30s >= 30s): released — re-prices in place (no cancel/resubmit)
            let mut b2 = broker(0.0, 31_000);
            mm.on_quote_tick(&mut b2, &quote(31_000, 0.48, 0.52));
            assert!(
                modified(&b2, "bid") && modified(&b2, "ask"),
                "re-prices once the moas elapses"
            );
            assert!(
                b2.submissions.is_empty() && b2.cancels.is_empty(),
                "released via modify, not churn"
            );
        }

        // SAFETY: a suppression PULL (fill-rate breaker) is never held by the moas — a quote that must
        // come off the book always does, even mid-moas.
        #[test]
        fn reward_moas_never_holds_a_breaker_pull() {
            let mut mm = SpreadMaker::new(1.0, 0.02)
                .with_fill_breaker(1_000, 2.5, 5_000)
                .with_liquidity_rewards(reward_bag(0.5, 3.0, 0.0, 30_000));
            let mut b0 = broker(0.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 0.49, 0.51));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "both sides rest");
            // a one-sided bid run trips the breaker's bid suppression
            for t in [110, 120, 130] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            // next tick is well inside BOTH the cooldown and the moas — the pull must still fire
            let mut b1 = broker(3.0, 140);
            mm.on_quote_tick(&mut b1, &quote(140, 0.49, 0.51));
            assert!(canceled(&b1, "bid"), "the breaker PULL is never held by the moas");
            assert!(!modified(&b1, "bid") && !submitted(&b1, "bid"), "and it is not re-quoted");
        }

        // The reward plane is LIVE-TUNABLE without a remount: turn it on, then off, mid-session — and the
        // update itself never touches an order (queue position preserved).
        #[test]
        fn reward_is_live_tunable_without_a_remount() {
            let mut mm = SpreadMaker::new(1.0, 0.05); // reward OFF
            mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 0.48, 0.52));
            // gate OFF ⇒ the candidate is the plain mid ∓ hs (0.45 / 0.55), out of any band
            let mut b1 = broker(0.0, 2);
            mm.on_quote_tick(&mut b1, &quote(2, 0.48, 0.52));
            assert!(
                (0.5 - modify_price(&b1, "bid") - 0.05).abs() < 1e-12,
                "reward off ⇒ bid at mid − hs (out of band)"
            );

            // turn reward ON over the plane (weight 0.5, band 3¢) — the update itself sends nothing
            let tuned =
                SpreadMakerParams { reward: Some(reward_bag(0.5, 3.0, 0.0, 0)), ..mm.params() };
            let mut bp = broker(0.0, 3);
            mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(tuned));
            assert_eq!(mm.params().reward, tuned.reward, "the reward bag round-trips");
            assert!(
                bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
                "the update itself must not disturb the resting orders"
            );

            // the NEXT tick re-prices IN PLACE into the band (bid pulled from 0.45 up to 0.485)
            let mut b2 = broker(0.0, 4);
            mm.on_quote_tick(&mut b2, &quote(4, 0.48, 0.52));
            assert!(
                (modify_price(&b2, "bid") - 0.485).abs() < 1e-12,
                "live-tuned reward pulls into the band"
            );
            assert!(
                b2.submissions.is_empty() && b2.cancels.is_empty(),
                "re-priced in place, no churn"
            );

            // a `None` bag turns it straight back off
            let off = SpreadMakerParams { reward: None, ..mm.params() };
            mm.on_params_updated(&mut broker(0.0, 5), &StrategyParams::SpreadMaker(off));
            assert!(mm.config.reward.is_none(), "a None reward bag turns the fold off");
        }
    }

    mod live_params {
        //! The live-parameter plane (audit co8) — `on_params_updated` hot-swaps every knob without
        //! unmounting (queue position preserved) — plus the genericity proof that `SpreadMaker` mounts on
        //! ANY [`OrderBroker`], not just the concrete `LiveBroker`.

        use super::*;

        // ---- live-parameter plane (audit co8) ----

        /// A full [`SpreadMakerParams`] with distinct, non-default values in every field — so a
        /// hot-swap that missed a field would be caught.
        fn all_new_params() -> SpreadMakerParams {
            SpreadMakerParams {
                qty: 3.0,
                half_spread: 0.25,
                target_inventory: 1.0,
                max_inventory: 2.0,
                skew: 0.4,
                fill_window_ms: 1_000,
                net_fill_threshold: 5.0,
                suppress_cooldown_ms: 2_000,
                style: QuoteStyle::Join,
                depth_levels: 3,
                tick_size: 0.5,
                filter_own: true,
                avellaneda_stoikov: Some(AsParams {
                    gamma: 0.7,
                    horizon_mode: HorizonMode::ConstantTau,
                    tau_hold_ms: 120_000,
                    resolution_ts: Some(1_700_000_000_000),
                    resolution_blackout_ms: 5_000,
                    variance_mode: VarianceMode::PureBernoulli,
                    kappa_mode: KappaMode::LiveFit,
                    kappa_default: 30.0,
                    kappa_min: 2.0,
                    kappa_max: 500.0,
                    sigma_half_life: 16.0,
                    trade_window_ms: 30_000,
                    n_min: 10,
                    q_scale: 50.0,
                    min_standoff_ticks: 2.0,
                    use_micro_price: true,
                    terminal_penalty_gamma: 0.35,
                    terminal_ramp_ms: 45_000,
                    underlying_weight: 0.4,
                    underlying_beta: 1.7,
                    window_secs: 300.0,
                    atm_blackout_scale: 1.5,
                    price_domain: PriceDomain::Unbounded,
                    min_half_spread_ticks: 3.0,
                    max_half_spread_ticks: 40.0,
                    round_trip_fee_rate: Some(4e-4),
                    spread_model: SpreadModel::Gueant,
                    base_intensity_a: 2.5,
                    reservation_model: ReservationModel::Lmsr,
                    lmsr_b: 75.0,
                    spread_source: SpreadSource::LsLmsr,
                    ls_lmsr_alpha: 0.04,
                    gm_mu: 0.2,
                    alpha_beta_imbalance: 0.15,
                    alpha_lambda_ofi: 0.03,
                    ofi_decay: 0.8,
                    running_penalty_phi: 0.05,
                    flatten_by_ms: 20_000,
                    flatten_strength: 0.6,
                    ofi_toxicity_scale: 0.09,
                    pull_accel_ramp_ms: 15_000,
                    pull_accel_max: 2.0,
                }),
                refresh_tolerance: Some(RefreshTolerance { price_bps: 12.5, size_bps: 7.5 }),
                ladder: Some(LadderParams {
                    levels: 4,
                    offset_step: 1.5,
                    offset_unit: LadderOffsetUnit::Ticks,
                    size_profile: LadderSizeProfile::Geometric,
                    size_ratio: 1.25,
                }),
                reward: Some(RewardParams {
                    weight: 0.3,
                    max_spread_cents: 2.0,
                    min_size: 50.0,
                    min_order_age_ms: 30_000,
                }),
                toxicity: Some(ToxicityParams { widen: 1.25, size_cut: 0.6 }),
            }
        }

        // on_params_updated hot-swaps EVERY tunable atomically, bumps the epoch, and `params()` reads
        // the applied bag back — the write/read halves of the plane.
        #[test]
        fn on_params_updated_hot_swaps_all_knobs_and_bumps_epoch() {
            let mut mm = SpreadMaker::new(1.0, 0.5);
            assert_eq!(mm.params_epoch, 0, "starts at epoch 0");
            assert_eq!(mm.config.style, QuoteStyle::Mid, "Mid by default");
            let new = all_new_params();
            let mut b = broker(0.0, 0);
            mm.on_params_updated(&mut b, &StrategyParams::SpreadMaker(new));
            assert_eq!(mm.params_epoch, 1, "epoch bumped once");
            // every field landed
            assert_eq!(mm.config.qty.to_bits(), 3.0_f64.to_bits());
            assert_eq!(mm.config.half_spread.to_bits(), 0.25_f64.to_bits());
            assert_eq!(mm.config.target_inventory.to_bits(), 1.0_f64.to_bits());
            assert_eq!(mm.config.max_inventory.to_bits(), 2.0_f64.to_bits());
            assert_eq!(mm.config.skew.to_bits(), 0.4_f64.to_bits());
            assert_eq!(mm.config.fill_window_ms, 1_000);
            assert_eq!(mm.config.net_fill_threshold.to_bits(), 5.0_f64.to_bits());
            assert_eq!(mm.config.suppress_cooldown_ms, 2_000);
            assert_eq!(mm.config.style, QuoteStyle::Join);
            assert_eq!(mm.config.depth_levels, 3);
            assert_eq!(mm.config.tick_size.to_bits(), 0.5_f64.to_bits());
            assert!(mm.config.filter_own);
            assert_eq!(
                mm.config.refresh_tolerance,
                Some(RefreshTolerance { price_bps: 12.5, size_bps: 7.5 }),
                "the refresh-tolerance sub-bag hot-swaps too"
            );
            assert_eq!(
                mm.config.ladder,
                Some(LadderParams {
                    levels: 4,
                    offset_step: 1.5,
                    offset_unit: LadderOffsetUnit::Ticks,
                    size_profile: LadderSizeProfile::Geometric,
                    size_ratio: 1.25,
                }),
                "the ladder sub-bag hot-swaps too"
            );
            assert_eq!(
                mm.config.reward,
                Some(RewardParams {
                    weight: 0.3,
                    max_spread_cents: 2.0,
                    min_size: 50.0,
                    min_order_age_ms: 30_000,
                }),
                "the reward sub-bag hot-swaps too"
            );
            assert_eq!(
                mm.config.toxicity,
                Some(ToxicityParams { widen: 1.25, size_cut: 0.6 }),
                "the toxicity sub-bag hot-swaps too"
            );
            // the A-S sub-bag also hot-swaps: the layer turned on and reports the applied knobs back
            assert!(mm.as_state.is_some(), "the A-S sub-bag turned the pricing layer on");
            assert_eq!(
                mm.params().avellaneda_stoikov,
                new.avellaneda_stoikov,
                "A-S bag round-trips"
            );
            // the read side mirrors the applied bag exactly
            assert_eq!(mm.params(), new, "params() round-trips the applied bag");
            // the update itself never touches the broker (no order op on a re-tune)
            assert!(
                b.submissions.is_empty() && b.modifications.is_empty() && b.cancels.is_empty(),
                "a param update must not place/modify/cancel any order itself"
            );
            // a second update bumps the epoch again (epoch is monotonic, per applied update)
            mm.on_params_updated(&mut b, &StrategyParams::SpreadMaker(new));
            assert_eq!(mm.params_epoch, 2, "epoch monotonically increments");
        }

        // A live re-tune re-prices the resting bid/ask IN PLACE on the NEXT tick (modify, never
        // cancel/resubmit), so venue queue position is preserved — the core queue-preservation gate.
        #[test]
        fn retune_reprices_in_place_on_next_tick_without_cancel() {
            let mut mm = SpreadMaker::new(1.0, 0.5); // Mid, half_spread 0.5
            // tick 1 rests both sides (mid 100.1 → bid 99.6, ask 100.6)
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

            // live re-tune mid-session: widen the spread to 1.0 and grow the size to 2.0
            let widened = SpreadMakerParams { qty: 2.0, half_spread: 1.0, ..mm.params() };
            let mut bp = broker(0.0, 2);
            mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(widened));
            assert_eq!(mm.params_epoch, 1, "the re-tune landed");
            assert!(
                bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
                "the update alone must not disturb the resting orders (queue preserved)"
            );

            // tick 2 (same feed): the new spread/size take effect via an in-place MODIFY, never a
            // cancel + resubmit (which would forfeit queue priority)
            let mut b1 = broker(0.0, 3);
            mm.on_quote_tick(&mut b1, &quote(3, 100.0, 100.2));
            assert!(b1.cancels.is_empty(), "no side is canceled on a re-tune");
            assert!(
                !submitted(&b1, "bid") && !submitted(&b1, "ask"),
                "re-priced in place — NOT cancel + resubmit"
            );
            // new mid 100.1 ± 1.0 (same-expression bit compare to dodge literal ULP drift), qty 2.0
            let mid = 0.5 * (100.0_f64 + 100.2);
            let bid = b1.modifications.iter().find(|m| m.tag == "bid").expect("bid modified");
            let ask = b1.modifications.iter().find(|m| m.tag == "ask").expect("ask modified");
            assert_eq!(
                bid.new_price.unwrap().to_bits(),
                (mid - 1.0).to_bits(),
                "bid at new half_spread"
            );
            assert_eq!(
                ask.new_price.unwrap().to_bits(),
                (mid + 1.0).to_bits(),
                "ask at new half_spread"
            );
            assert_eq!(bid.new_qty.unwrap().to_bits(), 2.0_f64.to_bits(), "bid at new qty");
            assert_eq!(ask.new_qty.unwrap().to_bits(), 2.0_f64.to_bits(), "ask at new qty");
        }

        // A strategy that does NOT override on_params_updated is entirely UNAFFECTED by an update —
        // the default trait hook is a pure no-op (no panic, no state change, no broker touch).
        #[test]
        fn default_on_params_updated_is_a_noop() {
            #[derive(Default)]
            struct Ignorer {
                bars_seen: u32,
            }
            impl Strategy<LiveBroker> for Ignorer {
                fn on_bar(&mut self, _b: &mut LiveBroker, _bar: &vike_marketdata::Bar) {
                    self.bars_seen += 1;
                }
                // deliberately does NOT override on_params_updated → uses the default no-op
            }
            let mut s = Ignorer::default();
            let mut b = broker(0.0, 0);
            s.on_params_updated(&mut b, &StrategyParams::SpreadMaker(all_new_params()));
            assert_eq!(s.bars_seen, 0, "default hook changes no strategy state");
            assert!(
                b.submissions.is_empty() && b.modifications.is_empty() && b.cancels.is_empty(),
                "default hook touches no orders"
            );
        }

        // ---- generic over OrderBroker (vike-market-making extraction, step 1; decision 0277's step 6) ----

        /// A minimal in-test [`OrderBroker`] that is NOT `LiveBroker` — it just records the tagged verbs
        /// the maker drives. Its whole point is to prove `SpreadMaker` is generic over the `OrderBroker`
        /// TRAIT, not wired to the concrete live broker: the exact same maker code drives this mock and
        /// `LiveBroker`. Only the tagged verbs and the position read carry behavior; the rest is inert.
        #[derive(Default)]
        struct MockHft {
            pos: f64,
            timestamp: i64,
            submits: Vec<(String, i32, f64, f64)>,
            modifies: Vec<(String, Option<f64>, Option<f64>)>,
            cancels: Vec<String>,
        }

        /// The maker's three tagged verbs on the order seam (decision 0277's step 6): a tagged limit is a
        /// `submits` row, an amend a `modifies` row, a cancel a `cancels` row; nothing else is placed.
        impl OrderBroker for MockHft {
            fn place(
                &mut self,
                _symbol: &str,
                tag: &str,
                side: i32,
                qty: f64,
                order: OrderPlacement,
                _reduce_only: bool,
            ) {
                if let OrderPlacement::Limit { price } = order {
                    self.submits.push((tag.to_string(), side, qty, price));
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
                qty: Option<f64>,
                price: Option<f64>,
                _trigger: Option<f64>,
            ) {
                self.modifies.push((tag.to_string(), qty, price));
            }
            fn cancel(&mut self, tag: &str) {
                self.cancels.push(tag.to_string());
            }
            fn deny(&mut self, _symbol: &str, _tag: &str, _quantity: f64, _reason: &str) {}
            fn average_price(&self, _symbol: &str) -> f64 {
                0.0
            }
            fn position(&self, _symbol: &str) -> f64 {
                self.pos
            }
            fn price(&self, _symbol: &str) -> f64 {
                0.0
            }
            fn equity(&self) -> f64 {
                0.0
            }
            fn bars(&self, _symbol: &str) -> &[vike_marketdata::Bar] {
                &[]
            }
            fn index(&self) -> usize {
                0
            }
            fn now(&self) -> i64 {
                self.timestamp
            }
        }

        // SpreadMaker is `impl<B: OrderBroker> Strategy<B>`, so it must mount on ANY OrderBroker — the
        // in-test `MockHft` AND the concrete `LiveBroker`.
        #[test]
        fn spread_maker_mounts_on_any_order_broker() {
            assert_mounts_on::<SpreadMaker, MockHft>();
        }

        // And behaviorally: the SAME maker code drives a NON-LiveBroker OrderBroker end-to-end — a first
        // quote rests a tagged bid+ask, the next re-prices them IN PLACE via amend (never a
        // fresh submit) — proving the broker-access abstraction holds over the trait, with LiveBroker
        // just the `B = LiveBroker` case the runtime mounts.
        #[test]
        fn generic_maker_drives_a_non_live_broker() {
            let mut mm = SpreadMaker::new(1.0, 0.5); // default Mid, half_spread 0.5
            let mut b0 = MockHft::default();
            // this call type-checks ONLY because SpreadMaker: Strategy<MockHft>
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            assert_eq!(
                b0.submits.len(),
                2,
                "rests a tagged bid + ask on a non-LiveBroker OrderBroker"
            );
            let bid = b0.submits.iter().find(|s| s.0 == "bid").expect("a tagged bid");
            let ask = b0.submits.iter().find(|s| s.0 == "ask").expect("a tagged ask");
            let mid = 0.5 * (100.0_f64 + 100.2);
            assert_eq!(bid.3.to_bits(), (mid - 0.5).to_bits(), "bid priced by the shared requote");
            assert_eq!(ask.3.to_bits(), (mid + 0.5).to_bits(), "ask priced by the shared requote");

            // next tick re-prices in place via amend — the same queue-preserving path
            // LiveBroker takes, here exercised through the trait on a different concrete type
            let mut b1 = MockHft { timestamp: 2, ..MockHft::default() };
            mm.on_quote_tick(&mut b1, &quote(2, 100.0, 100.4));
            assert!(b1.submits.is_empty(), "no re-submit — resting orders are modified in place");
            assert_eq!(b1.modifies.len(), 2, "both tagged sides re-priced via amend");
            assert!(b1.cancels.is_empty(), "nothing pulled on a plain re-quote");
        }
    }

    mod quote_style {
        //! The quoting-style registry + own-order-book filtration (audit mm3): each [`QuoteStyle`] priced
        //! off a fixture book, the `Mid` bit-for-bit reduction, and filtration on both the L1 and L2 lanes.

        use super::*;
        use vike_marketdata::L2BookLevel;

        // End-to-end on the L1 lane: a DEFAULT maker (Mid, filtration OFF) quotes off the feed exactly as
        // before — the submitted prices equal the original formula bit-for-bit, and the new knobs are all
        // at their inert defaults.
        #[test]
        fn default_maker_quotes_mid_off_the_feed_unchanged() {
            let mut mm = SpreadMaker::new(1.0, 0.5);
            assert_eq!(mm.config.style, QuoteStyle::Mid, "Mid is the default style");
            assert!(!mm.config.filter_own, "filtration is OFF by default");
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            let (bid_price, _) = submit_at(&b0, "bid");
            let (ask_price, _) = submit_at(&b0, "ask");
            let mid = 0.5 * (100.0_f64 + 100.2);
            assert_eq!(
                bid_price.to_bits(),
                (mid - 0.5).to_bits(),
                "bid unchanged from the mid formula"
            );
            assert_eq!(
                ask_price.to_bits(),
                (mid + 0.5).to_bits(),
                "ask unchanged from the mid formula"
            );
        }

        // The style COMPOSES with BOTH the skew (sizes) and the breaker (which sides quote): a `Join`
        // maker prices at the touch while the skew still shapes the sizes and the breaker still pulls the
        // over-hit side.
        #[test]
        fn quote_style_composes_with_skew_and_breaker() {
            let mut mm = SpreadMaker::new(2.0, 0.5)
                .with_quote_style(QuoteStyle::Join, 0, 1.0)
                .with_skew(0.0, 4.0, 0.5)
                .with_fill_breaker(1000, 2.5, 5000);
            // long inventory (3): Join prices AT the touch, skew shrinks the bid + grows the ask SIZE
            let mut b0 = broker(3.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
            let (bid_price, bid_quantity) = submit_at(&b0, "bid");
            let (ask_price, ask_quantity) = submit_at(&b0, "ask");
            assert_eq!(bid_price.to_bits(), 100.0_f64.to_bits(), "Join → bid at the touch");
            assert_eq!(ask_price.to_bits(), 101.0_f64.to_bits(), "Join → ask at the touch");
            assert!(
                bid_quantity < 2.0,
                "skew still shrinks the bid size when long: {bid_quantity}"
            );
            assert!(ask_quantity > 2.0, "skew still grows the ask size when long: {ask_quantity}");
            // a one-sided bid run trips the breaker → the over-hit bid is pulled under the Join style too
            for t in [110, 120, 130] {
                mm.on_fill(&mut broker(3.0, t), &a_fill(1, 1.0, t));
            }
            let mut b1 = broker(3.0, 140);
            mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
            assert!(
                canceled(&b1, "bid"),
                "breaker pulls the over-hit bid regardless of quote style"
            );
            assert!(modified(&b1, "ask"), "the un-hit ask keeps quoting in place");
        }

        // On the L2 book lane, `Depth` rests genuinely deeper — one level INTO the book each side.
        #[test]
        fn on_order_book_depth_style_rests_deeper() {
            let mut mm = SpreadMaker::new(1.0, 0.0).with_quote_style(QuoteStyle::Depth, 1, 0.0);
            let mut book = L2Book::new(0.5);
            book.apply_snapshot(
                1,
                &[
                    L2BookLevel::new(100.0, 5.0),
                    L2BookLevel::new(99.5, 8.0),
                    L2BookLevel::new(99.0, 3.0),
                ],
                &[
                    L2BookLevel::new(100.5, 4.0),
                    L2BookLevel::new(101.0, 9.0),
                    L2BookLevel::new(101.5, 2.0),
                ],
            );
            let mut b0 = broker(0.0, 10);
            mm.on_order_book(&mut b0, &book);
            assert_eq!(
                submit_at(&b0, "bid").0.to_bits(),
                99.5_f64.to_bits(),
                "Depth 1 into the bids"
            );
            assert_eq!(
                submit_at(&b0, "ask").0.to_bits(),
                101.0_f64.to_bits(),
                "Depth 1 into the asks"
            );
        }

        // On the L2 book lane, own-order filtration shifts the best across two updates: once we are the
        // whole top level (quoting the feed we consume), the next update leans on the genuine next level
        // instead of joining our own quote.
        #[test]
        fn on_order_book_own_filtration_shifts_best_when_alone() {
            // qty 5.0 == the top level's size, so after we rest there we ARE the whole best level.
            let mut mm = SpreadMaker::new(5.0, 0.0)
                .with_quote_style(QuoteStyle::Join, 0, 0.0)
                .with_own_order_filtration();
            let mut book = L2Book::new(0.5);
            book.apply_snapshot(
                1,
                &[L2BookLevel::new(100.0, 5.0), L2BookLevel::new(99.5, 8.0)],
                &[L2BookLevel::new(100.5, 5.0), L2BookLevel::new(101.0, 8.0)],
            );
            // update 1: no own orders yet → Join at the touch, rests both
            let mut b0 = broker(0.0, 10);
            mm.on_order_book(&mut b0, &book);
            assert_eq!(
                submit_at(&b0, "bid").0.to_bits(),
                100.0_f64.to_bits(),
                "first quote joins touch"
            );
            // update 2 (same book still shows our top-of-book): filtration removes our own best level, so
            // Join re-prices onto the genuine next bid (99.5), never joining itself.
            let mut b1 = broker(0.0, 20);
            mm.on_order_book(&mut b1, &book);
            assert_eq!(
                modify_price(&b1, "bid").to_bits(),
                99.5_f64.to_bits(),
                "filter → don't join our own"
            );
        }
    }

    mod refresh_tolerance {
        //! The order-refresh TOLERANCE (the anti-churn gate): sub-tolerance drift skips the modify,
        //! supra-tolerance re-prices in place, a fill always forces the size top-up, and a PULL is never
        //! tolerance-gated.

        use super::*;

        // ---- order-refresh tolerance (the anti-churn gate) ----
        //
        // The maker used to emit TWO venue modifies per quote/book tick forever (nothing below it
        // dedupes a no-change modify). These pin the gate that stops that churn — and, just as
        // importantly, that it is OFF by default and never swallows a PULL.

        // OFF BY DEFAULT (the neutral-reduction gate): an unconfigured maker re-issues both modifies on
        // every tick — including ticks whose target barely moved, or did not move at all — exactly as
        // before this feature. An explicitly all-zero (inert) bag behaves identically.
        #[test]
        fn refresh_tolerance_off_by_default_re_quotes_every_tick() {
            let mut mm = SpreadMaker::new(1.0, 0.5);
            assert!(mm.config.refresh_tolerance.is_none(), "the gate is OFF by default");

            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

            // a run of ticks whose target is unchanged / microscopically moved: every single one still
            // issues EXACTLY two modifies (today's per-tick behavior, reproduced)
            let ticks = [(100.0, 100.2), (100.000_001, 100.200_001), (100.0, 100.2)];
            for (i, &(bid, ask)) in ticks.iter().enumerate() {
                let ts = 2 + i as i64;
                let mut b = broker(0.0, ts);
                mm.on_quote_tick(&mut b, &quote(ts, bid, ask));
                assert!(
                    modified(&b, "bid") && modified(&b, "ask"),
                    "tick {i}: both sides re-priced"
                );
                assert_eq!(b.modifications.len(), 2, "tick {i}: exactly two modifies, as today");
                assert!(b.submissions.is_empty() && b.cancels.is_empty(), "tick {i}: nothing else");
            }

            // an explicitly INERT bag (both axes 0) is likewise never a skip
            let mut inert = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(0.0, 0.0);
            inert.on_quote_tick(&mut broker(0.0, 10), &quote(10, 100.0, 100.2));
            let mut c1 = broker(0.0, 11);
            inert.on_quote_tick(&mut c1, &quote(11, 100.0, 100.2));
            assert_eq!(
                c1.modifications.len(),
                2,
                "an all-zero tolerance still re-quotes every tick"
            );
        }

        // (a) SUB-tolerance drift sends NOTHING: the resting orders (and their queue position) are left
        // alone — no modify, no submit, no cancel.
        #[test]
        fn sub_tolerance_drift_skips_the_modify() {
            // 25 bp of price; sizes are constant here, so the (0 bp) size axis passes on exact equality
            let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
            // tick 1 rests both sides off mid 100.1 → bid 99.6 / ask 100.6
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

            // tick 2: the mid moves 100.1 → 100.101, so each target drifts 0.001 ≈ 0.1 bp — far inside
            // the 25 bp gate. NOTHING may go on the wire.
            let mut b1 = broker(0.0, 2);
            mm.on_quote_tick(&mut b1, &quote(2, 100.001, 100.201));
            assert!(b1.modifications.is_empty(), "sub-tolerance drift must not re-price");
            assert!(b1.submissions.is_empty(), "and must not re-submit either");
            assert!(b1.cancels.is_empty(), "and must not pull anything");
        }

        // (b) SUPRA-tolerance drift still re-prices IN PLACE (modify, never cancel + resubmit) — the
        // gate suppresses churn, never a genuine re-quote.
        #[test]
        fn supra_tolerance_drift_still_re_prices_in_place() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));

            // the mid jumps 100.1 → 101.1: each target moves 1.0 ≈ 100 bp, well past the 25 bp gate
            let mut b1 = broker(0.0, 2);
            mm.on_quote_tick(&mut b1, &quote(2, 101.0, 101.2));
            assert!(
                modified(&b1, "bid") && modified(&b1, "ask"),
                "a real move re-prices both sides"
            );
            assert!(b1.submissions.is_empty(), "re-priced in place — never cancel + resubmit");
            assert!(b1.cancels.is_empty(), "nothing pulled by a plain re-quote");
            let mid = 0.5 * (101.0_f64 + 101.2);
            assert_eq!(
                modify_price(&b1, "bid").to_bits(),
                (mid - 0.5).to_bits(),
                "at the new mid − hs"
            );
            assert_eq!(
                modify_price(&b1, "ask").to_bits(),
                (mid + 0.5).to_bits(),
                "at the new mid + hs"
            );
        }

        // (c) A SIZE change beyond tolerance re-quotes even when the PRICE is pinned — the two axes are
        // ANDed, so an inventory-skew re-size is never swallowed by a wide price tolerance.
        #[test]
        fn size_drift_beyond_tolerance_re_quotes_with_the_price_pinned() {
            // a very wide 500 bp price gate + a tight 10 bp size gate; the skew moves the SIZES
            let mut mm = SpreadMaker::new(2.0, 0.5)
                .with_skew(0.0, 4.0, 0.5)
                .with_refresh_tolerance(500.0, 10.0);
            // flat inventory → both sides rest at the base size 2.0
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            let (rested_bid_price, rested_bid_quantity) = submit_at(&b0, "bid");
            assert_eq!(
                rested_bid_quantity.to_bits(),
                2.0_f64.to_bits(),
                "flat rests the base size"
            );

            // the SAME book (price target identical — zero price drift) but now LONG 2.0: the skew
            // shrinks the bid to 1.5 and grows the ask to 2.5 (25% ≫ 10 bp), so BOTH sides re-quote.
            let mut b1 = broker(2.0, 2);
            mm.on_quote_tick(&mut b1, &quote(2, 100.0, 100.2));
            let bid = b1.modifications.iter().find(|m| m.tag == "bid").expect("bid re-sized");
            let ask = b1.modifications.iter().find(|m| m.tag == "ask").expect("ask re-sized");
            assert!(bid.new_qty.unwrap() < 2.0, "long → the bid shrank: {:?}", bid.new_qty);
            assert!(ask.new_qty.unwrap() > 2.0, "long → the ask grew: {:?}", ask.new_qty);
            assert_eq!(
                bid.new_price.unwrap().to_bits(),
                rested_bid_price.to_bits(),
                "the price never moved — the SIZE axis alone forced the re-quote"
            );

            // and the mirror: holding that same inventory (identical price AND size targets) is a skip
            let mut b2 = broker(2.0, 3);
            mm.on_quote_tick(&mut b2, &quote(3, 100.0, 100.2));
            assert!(b2.modifications.is_empty(), "an unchanged price AND size sends nothing");
        }

        // (d) SAFETY: a FILL invalidates the FILLED side's resting snapshot, so the partial-fill size
        // top-up can never be swallowed by the gate. `SideState::own` holds the maker's INTENDED
        // quote, not the venue-side remainder: after a partial fill the remainder is smaller, yet the
        // intended-vs-target comparison still reads "no change". The next tick must therefore go
        // through the modify arm — re-issuing the FULL configured size — even with the price unmoved.
        #[test]
        fn a_fill_forces_the_filled_side_through_the_modify_arm() {
            // NO breaker (so `on_fill`'s early return is exercised) + an armed 25 bp price gate
            let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

            // precondition: with NO fill, an identical tick sends nothing (the gate really is armed)
            let mut b1 = broker(0.0, 2);
            mm.on_quote_tick(&mut b1, &quote(2, 100.0, 100.2));
            assert!(b1.modifications.is_empty(), "precondition: an unchanged tick is skipped");

            // a PARTIAL fill on the bid: 0.4 of the resting 1.0 traded, only 0.6 remains at the venue
            mm.on_fill(&mut broker(0.0, 3), &a_fill(1, 0.4, 3));

            // the very next tick — same book, so the computed price AND size are bit-identical to what
            // the bid's `own` snapshot holds — MUST still re-issue the bid, topping it back up to the full size
            let mut b2 = broker(0.0, 4);
            mm.on_quote_tick(&mut b2, &quote(4, 100.0, 100.2));
            let bid = b2
                .modifications
                .iter()
                .find(|m| m.tag == "bid")
                .expect("the filled side re-quotes");
            assert_eq!(
                bid.new_qty.unwrap().to_bits(),
                1.0_f64.to_bits(),
                "topped back up to the full configured size"
            );
            assert!(!modified(&b2, "ask"), "the UNFILLED side is still gated — no churn there");

            // the invalidation is ONE-SHOT: the following unchanged tick is skipped again
            let mut b3 = broker(0.0, 5);
            mm.on_quote_tick(&mut b3, &quote(5, 100.0, 100.2));
            assert!(b3.modifications.is_empty(), "the stale mark clears on the re-quote it forced");

            // the mirror: a SELL fill invalidates the ASK side only
            mm.on_fill(&mut broker(0.0, 6), &a_fill(-1, 0.4, 6));
            let mut b4 = broker(0.0, 7);
            mm.on_quote_tick(&mut b4, &quote(7, 100.0, 100.2));
            assert!(modified(&b4, "ask"), "a sell fill re-quotes the ask side");
            assert!(!modified(&b4, "bid"), "…and leaves the untouched bid gated");
        }

        // (e) SAFETY: a PULL is never tolerance-gated. Even with a tolerance wide enough to swallow any
        // re-price, the fill-rate breaker's suppression still cancels the over-hit side — tolerance may
        // only ever skip a re-price, never strand a quote that must come off the book.
        #[test]
        fn suppression_pull_ignores_the_refresh_tolerance() {
            // 10_000 bp = 100%: every conceivable re-price is inside this gate
            let mut mm = SpreadMaker::new(1.0, 0.5)
                .with_fill_breaker(1_000, 2.5, 5_000)
                .with_refresh_tolerance(10_000.0, 10_000.0);
            let mut b0 = broker(0.0, 100);
            mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
            assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "both sides rest");

            // a one-sided bid run trips the breaker's bid suppression
            for t in [110, 120, 130] {
                mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }
            let mut b1 = broker(0.0, 140);
            mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
            assert!(canceled(&b1, "bid"), "the breaker's PULL is never tolerance-gated");
            assert!(!modified(&b1, "bid") && !submitted(&b1, "bid"), "and it is not re-quoted");
            // the un-hit ask, whose target did not move at all, is legitimately left resting — but it
            // is NOT pulled: the gate only ever skips a re-price.
            assert!(!modified(&b1, "ask"), "the unchanged ask side is left alone (no churn)");
            assert!(!canceled(&b1, "ask"), "…and is NOT pulled by the tolerance");

            // once the cooldown expires the bid re-enters the book with a fresh SUBMIT — the place path
            // is likewise never tolerance-gated.
            let mut b2 = broker(0.0, 5_200);
            mm.on_quote_tick(&mut b2, &quote(5_200, 100.0, 101.0));
            assert!(submitted(&b2, "bid"), "a PLACE is never tolerance-gated either");
        }

        // Drift is measured against what is actually RESTING, not against the previous target — so a
        // slow one-way crawl of small steps still re-prices once it has genuinely moved, and can never
        // ratchet away from the book one sub-tolerance step at a time.
        #[test]
        fn drift_is_measured_against_the_resting_order_not_the_last_target() {
            let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
            // rest at mid 100.1 → bid 99.6 / ask 100.6; 25 bp of 99.6 ≈ 0.249
            let mut b0 = broker(0.0, 1);
            mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
            assert!(submitted(&b0, "bid"), "first tick rests the bid");

            // four successive +0.05 steps of the mid: each is a NEW target, but every one is still
            // within 0.249 of the RESTING 99.6, so nothing goes on the wire
            for step in 1..=4_i32 {
                let shift = 0.05 * f64::from(step);
                let ts = 1 + i64::from(step);
                let mut b = broker(0.0, ts);
                mm.on_quote_tick(&mut b, &quote(ts, 100.0 + shift, 100.2 + shift));
                assert!(
                    b.modifications.is_empty(),
                    "step {step}: still inside the resting tolerance"
                );
            }
            // the fifth step (mid +0.25 → bid target 99.85, i.e. 0.25 off the resting 99.6 ≈ 25.1 bp)
            // finally trips the gate — the accumulated drift is NOT lost
            let mut b5 = broker(0.0, 6);
            mm.on_quote_tick(&mut b5, &quote(6, 100.25, 100.45));
            assert!(modified(&b5, "bid"), "accumulated drift past the tolerance DOES re-price");
            // the ask's own 0.25 drift is a smaller RELATIVE move off its higher resting price
            // (0.25/100.6 ≈ 24.9 bp), so it is still inside — the two sides gate independently
            assert!(!modified(&b5, "ask"), "each side is gated against its own resting price");
        }

        // The gate rides the live-parameter plane: it can be turned on, and back off, mid-session
        // WITHOUT unmounting — and the update itself never touches an order (queue position preserved).
        #[test]
        fn refresh_tolerance_is_live_tunable_without_a_remount() {
            let mut mm = SpreadMaker::new(1.0, 0.5); // gate OFF
            mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 100.2));
            let mut b1 = broker(0.0, 2);
            mm.on_quote_tick(&mut b1, &quote(2, 100.001, 100.201));
            assert_eq!(
                b1.modifications.len(),
                2,
                "gate off ⇒ even a 0.1 bp move re-quotes both sides"
            );

            // turn the gate ON over the plane
            let tuned = SpreadMakerParams {
                refresh_tolerance: Some(RefreshTolerance { price_bps: 25.0, size_bps: 0.0 }),
                ..mm.params()
            };
            let mut bp = broker(0.0, 3);
            mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(tuned));
            assert_eq!(
                mm.params().refresh_tolerance,
                tuned.refresh_tolerance,
                "the bag round-trips"
            );
            assert!(
                bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
                "the update itself must not disturb the resting orders"
            );

            // the same microscopic move is now skipped
            let mut b2 = broker(0.0, 4);
            mm.on_quote_tick(&mut b2, &quote(4, 100.002, 100.202));
            assert!(
                b2.modifications.is_empty(),
                "the live-tuned gate takes effect on the next tick"
            );

            // and a `None` bag turns it straight back off
            let off = SpreadMakerParams { refresh_tolerance: None, ..mm.params() };
            mm.on_params_updated(&mut broker(0.0, 5), &StrategyParams::SpreadMaker(off));
            assert!(mm.config.refresh_tolerance.is_none(), "a None bag turns the gate off");
            let mut b3 = broker(0.0, 6);
            mm.on_quote_tick(&mut b3, &quote(6, 100.002, 100.202));
            assert_eq!(b3.modifications.len(), 2, "gate off again ⇒ back to re-quoting every tick");
        }
    }

    // The two edge-triggered HOLD warns, tested as BEHAVIOUR rather than as return values.
    mod silent_hold {
        //! The maker's UNPRICED-BOOK hold, tested as behaviour: `requote`'s
        //! `let Some((bid_price, ask_price)) = priced else { return }` may not be silent.
        //!
        //! ## Why this family exists — a measured cost, not a hypothetical
        //!
        //! On 2026-08-17 the CI box's live `spread_maker` (bybit BTCUSDT, demo) ran for hours emitting
        //! `orders:0 working:0 fees:0.0 fault:null trading_state:"Active"` across ~112,000 summary `seq`
        //! and **zero** non-summary log lines. Every field an operator can see said the daemon was healthy;
        //! it had placed nothing at all.
        //!
        //! The investigation cost was paid entirely in the silence. `#1336` had just shipped the break-even
        //! refusal, whose arithmetic said it MUST be engaged on that mount (a 60-tick `$6.00` ceiling
        //! against a `$12.61` break-even), so the refusal was the obvious suspect. It was not the cause —
        //! and the only thing that proved it was the ABSENCE of its warn, because
        //! [`AsState::price`](vike_market_making::avellaneda::AsState) returns at `fair_value(view)?` **before** reaching
        //! `note_fee_floor`. The guard built to make holds attributable was itself sitting downstream of an
        //! unattributable one. The real cause was upstream of the maker entirely: the venue's market-data
        //! socket was connected but subscribed to nothing (39 KB in five hours, a 1:1 send/receive ratio —
        //! ping/pong keepalive, no `orderbook.50` data), so `requote` was never reaching a priced book.
        //!
        //! So the requirement these tests encode is not "return `None`" — it is **an operator can tell a
        //! holding maker from a healthy one, and is pointed at the feed rather than at the tuning**. A test
        //! asserting only the return value passes with the `warn!` deleted.
        //!
        //! ## Why edge-triggered
        //!
        //! `requote` is the vike-core hot fold, whose rule is *instrument per-order boundaries and fault
        //! transitions only* (CLAUDE.md). A per-tick warn here is an unbounded log write on the money path
        //! driven by a book feed — the same shape that once wrote a 341 GB journal on a live box. Both
        //! edges are therefore latched and O(1) per episode, and
        //! `a_hold_warns_once_per_episode_never_once_per_tick` is a PERFORMANCE contract as much as a
        //! legibility one.

        use super::*;
        use vike_marketdata::L2BookLevel;

        /// The pathological view: bids arrive, asks never do. This is what a half-subscribed or
        /// partially-applied venue feed hands the maker, and what `fair_value` refuses to price.
        fn bids_only() -> BookView {
            BookView {
                tick_size: TICK,
                bids: vec![L2BookLevel::new(MID - 0.5 * TICK, 5.0)],
                asks: Vec::new(),
            }
        }

        /// A plain `Mid`-style maker on the the CI box grid — no A-S layer, so this exercises the
        /// `view.priced(..)` arm of `requote` rather than the A-S one. Both flow through the same
        /// `let Some(..) = priced else` return, which is the point: the hold warn covers BOTH.
        fn maker() -> SpreadMaker {
            SpreadMaker::new(0.001, 0.5)
        }

        /// **THE HEADLINE — a maker that cannot price says so.** Reddens with the `warn!` in
        /// `SpreadMaker::note_no_quote` deleted, while every pre-existing one-sided-book test stays green,
        /// because none of them asserts anything an operator could see.
        #[test]
        fn an_unpriceable_book_is_announced_not_silently_held() {
            let mut mm = maker();
            let mut b = broker(0.0, 0);
            let logs = captured_logs(|| mm.requote(&mut b, &bids_only(), 0));

            assert!(
                b.submissions.is_empty() && b.modifications.is_empty(),
                "the hold itself is unchanged — nothing may be placed or amended: {:?}",
                submits_dbg(&b)
            );
            assert!(logs.contains("maker HOLDING"), "…but it must be ANNOUNCED — got: {logs:?}");
            // The operator must be pointed at the FEED. Getting this wrong is what cost the 2026-08-17
            // investigation its first hours: the visible symptom invited a tuning hypothesis, and the
            // cause was a venue subscription delivering nothing.
            assert!(
                logs.contains("ask_levels=0"),
                "the warn must name the EMPTY side — got: {logs:?}"
            );
            assert!(
                logs.contains("bid_levels=1"),
                "…and the populated one, so 'no data at all' is distinguishable from 'one side missing' \
                 — got: {logs:?}"
            );
            assert!(
                logs.contains("FEED"),
                "…and must send the reader to the feed before the tuning — got: {logs:?}"
            );
        }

        /// EDGE-triggered. See this module's header: a per-tick warn on a book feed is an unbounded write
        /// on the hot fold, so "exactly one" is a performance contract, not tidiness.
        #[test]
        fn a_hold_warns_once_per_episode_never_once_per_tick() {
            let mut mm = maker();
            let mut b = broker(0.0, 0);
            let logs = captured_logs(|| {
                for ts in 0..64 {
                    mm.requote(&mut b, &bids_only(), ts);
                }
            });
            assert_eq!(
                logs.matches("maker HOLDING").count(),
                1,
                "64 unpriceable ticks must produce exactly ONE warn — got: {logs:?}"
            );
        }

        /// The NEGATIVE control, and the half that keeps the headline honest: a maker that CAN price says
        /// nothing. Without it, a `warn!` fired unconditionally on every tick would satisfy the test above.
        #[test]
        fn a_healthy_maker_says_nothing() {
            let mut mm = maker();
            let mut b = broker(0.0, 0);
            let logs = captured_logs(|| mm.requote(&mut b, &two_sided(), 0));

            assert!(
                !b.submissions.is_empty(),
                "the control must actually quote: {:?}",
                submits_dbg(&b)
            );
            assert!(
                !logs.contains("maker HOLDING") && !logs.contains("maker RESUMING"),
                "a working maker must be silent on the hot path — got: {logs:?}"
            );
        }

        /// The EXIT edge. Without it a recovered maker keeps looking permanently broken in the journal,
        /// which is its own operator-facing lie — the mirror of the defect this family closes.
        #[test]
        fn recovery_is_announced_so_a_warn_cannot_outlive_its_cause() {
            let mut mm = maker();
            let mut b = broker(0.0, 0);
            let logs = captured_logs(|| {
                mm.requote(&mut b, &bids_only(), 0);
                mm.requote(&mut b, &two_sided(), 1);
            });
            assert!(logs.contains("maker HOLDING"), "the entry edge — got: {logs:?}");
            assert!(logs.contains("maker RESUMING"), "…and the exit edge — got: {logs:?}");
        }

        /// A re-entered hold warns AGAIN — the latch must reset on recovery, not fire once per process.
        /// A latch that never rearms would report the first outage of a session and silently swallow every
        /// later one, which is the same silence with extra steps.
        #[test]
        fn a_second_outage_warns_again() {
            let mut mm = maker();
            let mut b = broker(0.0, 0);
            let logs = captured_logs(|| {
                mm.requote(&mut b, &bids_only(), 0);
                mm.requote(&mut b, &two_sided(), 1);
                mm.requote(&mut b, &bids_only(), 2);
            });
            assert_eq!(
                logs.matches("maker HOLDING").count(),
                2,
                "two distinct outages are two distinct episodes — got: {logs:?}"
            );
        }

        /// ⚠ The two HOLD warns must not both announce the SAME hold.
        ///
        /// When the break-even refusal is the cause, `AsState::note_fee_floor` has already said so WITH the
        /// numbers that make it actionable (`break_even_half_spread` vs `max_half_spread`). A generic
        /// "no two-sided quote" line on top would bury the specific one — so `note_no_quote` stays silent
        /// and does not latch. This pins that arrangement; see `SpreadMaker::note_no_quote`'s doc.
        #[test]
        fn the_fee_floor_refusal_is_not_double_reported() {
            let mut mm = maker();
            // Arm the the CI box mount exactly: 2-tick floor, 60-tick ($6.00) ceiling, bybit's 4 bp round trip
            // — a $12.61 break-even the ceiling cannot reach, so A-S refuses on a perfectly healthy book.
            mm = mm.with_avellaneda_stoikov(AsParams {
                variance_mode: VarianceMode::RawLocal,
                horizon_mode: HorizonMode::ConstantTau,
                price_domain: PriceDomain::Unbounded,
                min_half_spread_ticks: 2.0,
                max_half_spread_ticks: 60.0,
                round_trip_fee_rate: Some(4e-4),
                gamma: 5e-4,
                q_scale: 3e-2,
                ..AsParams::default()
            });
            let mut b = broker(0.0, 0);
            let logs = captured_logs(|| mm.requote(&mut b, &two_sided(), 0));

            assert!(b.submissions.is_empty(), "the refusal still refuses: {:?}", submits_dbg(&b));
            assert_eq!(
                logs.matches("maker HOLDING").count(),
                1,
                "exactly ONE hold message, the specific one — got: {logs:?}"
            );
            assert!(
                logs.contains("break_even_half_spread"),
                "…and it must be the FEE-FLOOR message, which carries the numbers — got: {logs:?}"
            );
            assert!(
                !logs.contains("ask_levels"),
                "…not the generic unpriced-book one, whose book fields are irrelevant here — got: {logs:?}"
            );
        }
    }

    mod strategy_impl_tests {
        use super::*;
        use vike_model::OrderEventKind;

        /// Minimal test-only accessors for [`SpreadMaker`] fields that are private to this file, and for
        /// `vike_market_making::avellaneda::AsState`'s accumulators: the tests below want a clean read API rather than
        /// reaching into `self.as_state.as_ref()...` inline.
        impl SpreadMaker {
            pub(crate) fn bid_suppressed_until(&self) -> i64 {
                self.bid.suppressed_until
            }
            pub(crate) fn ask_suppressed_until(&self) -> i64 {
                self.ask.suppressed_until
            }
            pub(crate) fn as_enabled(&self) -> bool {
                self.as_state.is_some()
            }
            pub(crate) fn as_sigma2(&self) -> Option<f64> {
                self.as_state.as_ref().and_then(|s| s.sigma2)
            }
            /// `(sum_w, sum_w_delta, trades.len())` — the κ-MLE accumulators, `None` if A-S is off.
            pub(crate) fn as_accumulators(&self) -> Option<(f64, f64, usize)> {
                self.as_state.as_ref().map(|s| (s.sum_w, s.sum_w_delta, s.trades.len()))
            }
            /// `(own_obs.len(), own_n_fill, cached_own_kappa)` — the OwnFillFit own-fill tape stats, or
            /// `None` if A-S is off.
            pub(crate) fn own_fill_stats(&self) -> Option<(usize, usize, Option<f64>)> {
                self.as_state.as_ref().map(|s| (s.own_obs.len(), s.own_n_fill, s.cached_own_kappa))
            }
            /// The A-S state's stored underlying `(s_now, s_open, sigma_per_sec)`, or `None` when A-S is off
            /// or no routed mark has warmed the tracker yet — the observable of the "Option B" `on_mark`
            /// wiring.
            pub(crate) fn as_underlying(&self) -> Option<(f64, f64, f64)> {
                self.as_state.as_ref().and_then(|s| s.underlying)
            }
        }

        fn trade(ts: i64, price: f64, size: f64) -> TickTrade {
            TickTrade {
                timestamp: ts,
                local_timestamp: 0,
                price,
                size,
                is_buyer_maker: false,
                symbol: String::new(),
            }
        }

        fn a_fill(side: i32, size: f64, ts: i64) -> Fill {
            Fill {
                side,
                size,
                price: 100.0,
                fee: 0.0,
                timestamp: ts,
                is_maker: true,
                symbol: String::new(),
                tag: None,
            }
        }

        /// A maker with BOTH the fill-rate breaker and A-S enabled — the fixed config the roundtrip
        /// test builds two instances of (the "driven" maker and the "fresh, restart-simulating" one
        /// that loads its saved state — a real mount always reconstructs from the same config first).
        fn maker_with_breaker_and_as() -> SpreadMaker {
            SpreadMaker::new(1.0, 0.5)
                .with_fill_breaker(1_000, 2.5, 5_000)
                .with_avellaneda_stoikov(AsParams::default())
        }

        // The full round trip (portfolio-observer PR-4 T4): drive a maker until BOTH the breaker has
        // tripped a side AND the A-S estimator has warmed accumulators, save it, load it into a FRESH
        // same-config maker (as a restart would construct before load_state), and confirm both the
        // breaker deadline and the A-S accumulators — but NOT the params/alpha, which come from the
        // fresh config either way — land bit-for-bit.
        #[test]
        fn spreadmaker_state_roundtrips_breaker_and_as() {
            let mut m = maker_with_breaker_and_as();

            // two priced quote ticks seed sigma2/last_mid/last_quote_mid (the first only seeds
            // last_mid; the second computes the first sigma2 sample from the mid delta over dt).
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            m.on_quote_tick(&mut broker(0.0, 200), &quote(200, 0.41, 0.43));
            // a trade AFTER the first priced tick folds into the kappa-MLE running sums.
            m.on_trade_tick(&mut broker(0.0, 200), &trade(210, 0.415, 5.0));

            // three same-side (bid) fills inside the window trip the breaker's bid suppression.
            for t in [110, 120, 130] {
                m.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
            }

            assert_ne!(m.bid_suppressed_until(), 0, "precondition: the breaker tripped");
            assert!(
                m.as_sigma2().is_some(),
                "precondition: sigma2 seeded by the second priced tick"
            );
            let (sum_w, _sum_w_delta, n_trades) = m.as_accumulators().expect("A-S is enabled");
            assert!(sum_w > 0.0, "precondition: the trade tick folded into sum_w");
            assert_eq!(n_trades, 1, "precondition: one trade recorded");

            let saved =
                Strategy::<LiveBroker>::save_state(&m).expect("SpreadMaker always saves Some");

            // a FRESH maker, same config — as a restart would construct at mount, before load_state
            let mut m2 = maker_with_breaker_and_as();
            assert_eq!(m2.bid_suppressed_until(), 0, "precondition: fresh maker starts untripped");
            assert_eq!(m2.as_sigma2(), None, "precondition: fresh maker's estimator starts cold");

            Strategy::<LiveBroker>::load_state(&mut m2, &saved);

            assert_eq!(
                m2.bid_suppressed_until(),
                m.bid_suppressed_until(),
                "breaker deadline restored"
            );
            assert_eq!(
                m2.ask_suppressed_until(),
                m.ask_suppressed_until(),
                "untripped side stays 0"
            );
            assert_eq!(m2.as_sigma2(), m.as_sigma2(), "sigma2 accumulator restored");
            assert_eq!(
                m2.as_accumulators(),
                m.as_accumulators(),
                "kappa-MLE sums + trade tape restored"
            );
        }

        // A future/foreign version tag must be ignored (fail-open): no panic, the maker keeps its
        // freshly-constructed state rather than misapplying a payload shape it doesn't recognize.
        #[test]
        fn spreadmaker_load_ignores_version_mismatch() {
            let mut m = maker_with_breaker_and_as();
            Strategy::<LiveBroker>::load_state(&mut m, &serde_json::json!({"v": 999}));
            assert_eq!(
                m.bid_suppressed_until(),
                0,
                "fail-open: deadlines stay at constructed defaults"
            );
            assert_eq!(
                m.ask_suppressed_until(),
                0,
                "fail-open: deadlines stay at constructed defaults"
            );
            assert_eq!(m.as_sigma2(), None, "fail-open: A-S estimator stays cold");
        }

        // A maker with A-S DISABLED loading a payload that DOES carry an as_state (e.g. saved by a
        // differently-configured maker) must restore the breaker deadlines but silently skip the A-S
        // restore — never turning A-S on as a side effect of loading state.
        #[test]
        fn spreadmaker_load_without_as_skips_as_restore() {
            let mut m = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
            assert!(!m.as_enabled(), "precondition: A-S is off on this config");

            let payload = serde_json::json!({
                "v": 1,
                "bid_suppressed_until": 777,
                "ask_suppressed_until": 0,
                "as_state": {
                    "sigma2": 0.001,
                    "last_mid": [0.41, 200],
                    "last_quote_mid": 0.41,
                    "trades": [[0.01, 5.0, 210]],
                    "sum_w": 5.0,
                    "sum_w_delta": 0.05
                }
            });
            Strategy::<LiveBroker>::load_state(&mut m, &payload);

            assert_eq!(m.bid_suppressed_until(), 777, "the breaker deadline still restores");
            assert!(!m.as_enabled(), "A-S stays off — the payload's as_state is silently skipped");
        }

        /// A maker with A-S in [`KappaMode::OwnFillFit`] mode. The fixed config the wiring tests build.
        fn own_fill_fit_maker() -> SpreadMaker {
            SpreadMaker::new(1.0, 0.5)
                .with_avellaneda_stoikov(AsParams {
                    kappa_mode: KappaMode::OwnFillFit,
                    ..AsParams::default()
                })
                .with_fill_breaker(1_000, 2.5, 5_000)
        }

        // OwnFillFit WIRING: a fill folds a FILLED own outcome, and a subsequent breaker PULL folds a
        // CENSORED one — the two live sources that feed the censored-hazard κ MLE.
        #[test]
        fn own_fill_fit_records_fills_and_breaker_pulls() {
            let mut m = own_fill_fit_maker();
            // a priced quote tick places the bid and sets its placement clock + the A-S fair mid.
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            // a bid fill folds a FILLED own outcome (and the 5.0 same-side size trips the bid breaker).
            m.on_fill(&mut broker(0.0, 110), &a_fill(1, 5.0, 110));
            let (len1, nfill1, _) = m.own_fill_stats().expect("A-S enabled");
            assert_eq!((len1, nfill1), (1, 1), "the fill recorded one FILLED own outcome");
            // the next tick: the tripped breaker PULLS the bid, folding a CENSORED own outcome.
            m.on_quote_tick(&mut broker(0.0, 120), &quote(120, 0.40, 0.42));
            let (len2, nfill2, _) = m.own_fill_stats().expect("A-S enabled");
            assert_eq!(
                (len2, nfill2),
                (2, 1),
                "the breaker pull recorded one CENSORED own outcome"
            );
        }

        // OFF / byte-identical: a maker NOT selecting OwnFillFit (here the DEFAULT Fixed κ, A-S on)
        // never touches the own-fill tape — the whole wiring is inert, so its behavior is unchanged. It
        // drives the exact fill + breaker-pull sequence the OwnFillFit test does; nothing is recorded.
        #[test]
        fn non_own_fill_fit_maker_never_records_own_outcomes() {
            let mut m = SpreadMaker::new(1.0, 0.5)
                .with_avellaneda_stoikov(AsParams::default()) // KappaMode::Fixed, the default
                .with_fill_breaker(1_000, 2.5, 5_000);
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            m.on_fill(&mut broker(0.0, 110), &a_fill(1, 5.0, 110));
            m.on_quote_tick(&mut broker(0.0, 120), &quote(120, 0.40, 0.42));
            assert_eq!(
                m.own_fill_stats(),
                Some((0, 0, None)),
                "Fixed κ never records own outcomes — the OwnFillFit wiring is inert"
            );
        }

        // --- the order-DEATH lane: a dead quote frees its side, so the next tick PLACES ---------------

        /// A plain single-quote maker on the 0.01 grid — no breaker, no A-S, no ladder, no tolerance.
        /// The exact default shape the live the CI box mount runs, so the wedge below is the live one.
        fn plain_maker() -> SpreadMaker {
            SpreadMaker::new(1.0, 0.01)
        }

        /// `(tag, price)` of every SUBMIT on a driven broker.
        fn submits(b: &LiveBroker) -> Vec<(String, f64)> {
            b.submissions
                .iter()
                .map(|s| (s.tag.clone().unwrap_or_default(), s.price.unwrap_or(f64::NAN)))
                .collect()
        }

        /// The tags of every MODIFY on a driven broker.
        fn modify_tags(b: &LiveBroker) -> Vec<String> {
            b.modifications.iter().map(|m| m.tag.clone()).collect()
        }

        /// A terminal lifecycle event naming one of the maker's own tags.
        fn terminal(tag: &str, kind: OrderEventKind) -> OrderEvent {
            OrderEvent { order_client_id: "order-client-id-1".into(), tag: Some(tag.into()), kind }
        }

        // ⚠ THE WEDGE. A FULL FILL kills the resting bid, but nothing used to clear `SideState::placed`
        // — so the next tick took the modify arm, the runtime resolved `"bid"` to the now-terminal
        // order client id, and `Engine::modify_order` returned silently. That side never quoted again.
        // Drive exactly that sequence and assert the next tick SUBMITS the bid rather than modifying it.
        #[test]
        fn a_fully_filled_quote_is_resubmitted_not_modified_on_the_next_tick() {
            let mut m = plain_maker();

            // tick 1: a two-sided quote goes on the book.
            let mut b1 = broker(0.0, 100);
            m.on_quote_tick(&mut b1, &quote(100, 0.40, 0.42));
            assert_eq!(submits(&b1).len(), 2, "precondition: both sides placed on the first tick");
            assert!(modify_tags(&b1).is_empty(), "precondition: nothing to modify yet");

            // the bid fills COMPLETELY: the fill itself, then the order's death by tag.
            m.on_fill(&mut broker(1.0, 110), &a_fill(1, 1.0, 110));
            m.on_order_event(&mut broker(1.0, 110), &terminal("bid", OrderEventKind::Filled));

            // tick 2 at a MOVED book, so a still-resting side genuinely wants a re-price.
            let mut b2 = broker(1.0, 120);
            m.on_quote_tick(&mut b2, &quote(120, 0.41, 0.43));

            let submitted: Vec<String> = submits(&b2).into_iter().map(|(t, _)| t).collect();
            assert!(
                submitted.contains(&"bid".to_string()),
                "the dead bid must be SUBMITTED again, got submits {submitted:?} / modifies {:?}",
                modify_tags(&b2)
            );
            assert!(
                !modify_tags(&b2).contains(&"bid".to_string()),
                "the dead bid must NOT be re-priced — that is the silent no-op that wedged the mount"
            );
            // ...and the surviving ask must still take the WORKING modify lane, untouched.
            assert_eq!(
                modify_tags(&b2),
                vec!["ask".to_string()],
                "the resting ask still re-prices"
            );
            assert!(
                !submitted.contains(&"ask".to_string()),
                "the resting ask must NOT be re-submitted (that would orphan it)"
            );
        }

        // The other two death modes reach the same slot through the same hook: an externally-originated
        // CANCEL, and a REJECT of a submit the place arm had already marked `placed`. Each frees only
        // its own side.
        #[test]
        fn a_canceled_or_rejected_quote_also_frees_its_side() {
            for (tag, kind) in [
                ("bid", OrderEventKind::Canceled { reason: "venue pull".into() }),
                ("ask", OrderEventKind::Rejected { reason: "post-only would cross".into() }),
                ("bid", OrderEventKind::Denied { reason: "risk: min_qty".into() }),
                ("ask", OrderEventKind::Expired),
            ] {
                let mut m = plain_maker();
                m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
                m.on_order_event(&mut broker(0.0, 110), &terminal(tag, kind.clone()));

                let mut b = broker(0.0, 120);
                m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
                let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
                assert_eq!(submitted, vec![tag.to_string()], "{kind:?} on {tag} must re-place it");
                let other = if tag == "bid" { "ask" } else { "bid" };
                assert_eq!(
                    modify_tags(&b),
                    vec![other.to_string()],
                    "{kind:?} on {tag} must leave the OTHER side on the modify lane"
                );
            }
        }

        // P1: a place under a tag whose older order still works is REFUSED, a tagged `Denied` in
        // `vike_model::tag_busy_reason`'s words. The order behind the tag is ALIVE, so the side is adopted,
        // never freed: the next tick re-prices it by tag and places nothing. Freed (a plain `Denied`, the
        // test above), it would place again and be refused on every tick.
        #[test]
        fn a_busy_refusal_adopts_its_side_and_the_next_tick_amends_it() {
            for tag in ["bid", "ask"] {
                let busy = OrderEventKind::Denied { reason: vike_model::tag_busy_reason(tag) };
                let mut m = plain_maker();
                m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
                m.on_order_event(&mut broker(0.0, 110), &terminal(tag, busy));

                let mut b = broker(0.0, 120);
                m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
                assert!(
                    b.submissions.is_empty(),
                    "{tag}: nothing is placed again: {:?}",
                    submits(&b)
                );
                assert_eq!(modify_tags(&b), vec!["bid".to_string(), "ask".to_string()], "{tag}");
                let size = b.modifications.iter().find(|a| a.tag == tag).and_then(|a| a.new_qty);
                assert_eq!(
                    size,
                    Some(1.0),
                    "{tag}: the adopted order is re-priced with its full size"
                );
            }
        }

        // INERT for everything that is not this maker's own dead single quote: a non-terminal Accept, a
        // LADDER rung tag (`"bid0"` — a prefix of `"bid"`'s tag, and explicitly out of scope), and an
        // UNTAGGED event. Each must leave both sides resting, i.e. the next tick still modifies both.
        #[test]
        fn non_terminal_foreign_and_untagged_events_change_nothing() {
            for event in [
                terminal("bid", OrderEventKind::Accepted),
                terminal("bid0", OrderEventKind::Filled),
                terminal("hedge", OrderEventKind::Canceled { reason: String::new() }),
                OrderEvent {
                    order_client_id: "order-client-id-9".into(),
                    tag: None,
                    kind: OrderEventKind::Filled,
                },
            ] {
                let mut m = plain_maker();
                m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
                m.on_order_event(&mut broker(0.0, 110), &event);

                let mut b = broker(0.0, 120);
                m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
                assert!(b.submissions.is_empty(), "{event:?} must place nothing");
                assert_eq!(
                    modify_tags(&b),
                    vec!["bid".to_string(), "ask".to_string()],
                    "{event:?} must leave BOTH sides on the working modify lane"
                );
            }
        }

        // --- restart: the resting quote is PERSISTED, so the maker continues it ----------------------
        //
        // After a restart the core puts the previous session's resting orders (and their tags) back, so
        // a maker that boots with `placed = false` would submit a SECOND bid/ask under the tag the first
        // one still rests under — overwriting the core's tag entry and orphaning the old order.

        /// A maker that quoted both sides at ts 100, saved, and a FRESH same-config maker that loaded it —
        /// the shape of a real restart (`(restored, original)`).
        fn restart_pair() -> (SpreadMaker, SpreadMaker) {
            let mut before = plain_maker();
            before.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            let saved = Strategy::<LiveBroker>::save_state(&before).expect("always Some");
            let mut after = plain_maker();
            Strategy::<LiveBroker>::load_state(&mut after, &saved);
            (after, before)
        }

        // The new fields round-trip: `placed`, the intended `(price, size)` the refresh logic compares
        // against, and the placement clock. A fresh maker would hold none of them.
        #[test]
        fn a_resting_quote_roundtrips_through_save_and_load() {
            let (after, before) = restart_pair();
            for is_bid in [true, false] {
                let (a, b) = (after.side(is_bid), before.side(is_bid));
                assert!(
                    b.placed && b.own.is_some(),
                    "precondition: the original maker rests both sides"
                );
                assert!(a.placed, "placed restored (is_bid={is_bid})");
                assert_eq!(a.own, b.own, "intended (price, size) restored (is_bid={is_bid})");
                assert_eq!(
                    a.quoted_timestamp, b.quoted_timestamp,
                    "placement clock restored (is_bid={is_bid})"
                );
                assert!(
                    a.refresh_stale,
                    "a restored snapshot is stale: a partial fill may have landed"
                );
                assert_eq!(a.unverified, Some(0), "restored but not yet confirmed alive");
            }
        }

        // A state file written BEFORE this field existed (the shape in the wild) still loads: the breaker
        // deadline restores and both sides come up unplaced, exactly as they always did.
        #[test]
        fn json_of_the_old_shape_loads_with_both_sides_unplaced() {
            let mut m = plain_maker();
            let old = serde_json::json!({
                "v": 1,
                "bid_suppressed_until": 777,
                "ask_suppressed_until": 0,
                "as_state": null
            });
            Strategy::<LiveBroker>::load_state(&mut m, &old);
            assert_eq!(m.bid_suppressed_until(), 777, "the old fields still restore");
            for is_bid in [true, false] {
                let s = m.side(is_bid);
                assert!(
                    !s.placed && s.own.is_none() && s.quoted_timestamp == 0,
                    "unplaced (is_bid={is_bid})"
                );
                assert_eq!(s.unverified, None, "not a restored side (is_bid={is_bid})");
                assert!(!s.refresh_stale);
            }
            // ...and its first tick quotes as a fresh maker does.
            let mut b = broker(0.0, 100);
            m.on_quote_tick(&mut b, &quote(100, 0.40, 0.42));
            assert_eq!(submits(&b).len(), 2, "an unplaced side submits on its first tick");
        }

        // A `placed = true` that carries no usable quote (hand-edited / half-written) is not trusted:
        // there is nothing to compare a refresh against, so the side stays unplaced.
        #[test]
        fn a_placed_flag_without_a_quote_is_not_restored() {
            for bid in [
                serde_json::json!({"placed": true}),
                serde_json::json!({"placed": true, "own": [0.4, 0.0], "quoted_timestamp": 5}),
                serde_json::json!({"placed": false, "own": [0.4, 1.0], "quoted_timestamp": 5}),
            ] {
                let mut m = plain_maker();
                let payload = serde_json::json!({
                    "v": 1, "bid_suppressed_until": 0, "ask_suppressed_until": 0,
                    "as_state": null, "bid": bid
                });
                Strategy::<LiveBroker>::load_state(&mut m, &payload);
                assert!(
                    !m.bid.placed && m.bid.own.is_none() && m.bid.unverified.is_none(),
                    "{bid}"
                );
            }
        }

        // THE POINT: a restored resting side does NOT submit on its first tick (that would orphan the old
        // order). It takes the modify arm — by tag, which the core resolves through its restored registry —
        // and it does so even with a wide refresh tolerance, because the persisted snapshot is only the
        // INTENDED quote and a partial fill may have landed while the daemon was down.
        #[test]
        fn a_restored_resting_side_does_not_requote_on_its_first_tick() {
            let (mut after, _) = restart_pair();
            let mut b = broker(0.0, 5_000);
            after.on_quote_tick(&mut b, &quote(5_000, 0.40, 0.42));
            assert!(
                b.submissions.is_empty(),
                "no NEW quote while the old one rests: {:?}",
                submits(&b)
            );
            assert!(b.cancels.is_empty(), "nothing is pulled on the first tick");
            assert_eq!(
                modify_tags(&b),
                vec!["bid".to_string(), "ask".to_string()],
                "re-priced by tag"
            );

            // the same under a wide tolerance, where an UNCHANGED target would otherwise be skipped
            let mut tolerance = plain_maker().with_refresh_tolerance(1_000.0, 1_000.0);
            let mut before = broker(0.0, 100);
            tolerance.on_quote_tick(&mut before, &quote(100, 0.40, 0.42));
            let saved = Strategy::<LiveBroker>::save_state(&tolerance).expect("always Some");
            let mut tolerance2 = plain_maker().with_refresh_tolerance(1_000.0, 1_000.0);
            Strategy::<LiveBroker>::load_state(&mut tolerance2, &saved);
            let mut b2 = broker(0.0, 200);
            tolerance2.on_quote_tick(&mut b2, &quote(200, 0.40, 0.42));
            assert!(b2.submissions.is_empty());
            assert_eq!(
                modify_tags(&b2).len(),
                2,
                "stale snapshot bypasses the tolerance skip once"
            );
        }

        // A tagged TERMINAL event for the restored order (the core stamps `tag` from its restored registry)
        // clears the side exactly as it does for a live one; the next tick re-quotes it, and the other
        // side is untouched.
        #[test]
        fn a_tagged_terminal_event_after_restore_clears_the_side_and_the_next_tick_requotes() {
            let (mut after, _) = restart_pair();
            after.on_order_event(&mut broker(0.0, 200), &terminal("bid", OrderEventKind::Filled));
            assert!(!after.bid.placed && after.bid.own.is_none() && after.bid.unverified.is_none());
            assert!(after.ask.placed, "the other side is untouched");

            let mut b = broker(0.0, 300);
            after.on_quote_tick(&mut b, &quote(300, 0.40, 0.42));
            let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
            assert_eq!(submitted, vec!["bid".to_string()], "the freed side re-places");
            assert_eq!(
                modify_tags(&b),
                vec!["ask".to_string()],
                "the surviving restored ask re-prices"
            );
        }

        // SAFETY NET. A restored `placed = true` whose order is gone with no event (the core's ownership
        // entry expired, or the venue dropped it while the daemon was down) must not wedge: the modify arm
        // resolves the tag to nothing and is swallowed, forever. After `RESTORED_QUOTE_VERIFY_MS` without
        // a sign of life the side is PULLED (cancel by tag — a no-op if the order is gone), and the NEXT
        // tick places. The pull and the place are NEVER in one handler call: the core drains submits
        // BEFORE cancels, so a same-tick place("bid") is refused as busy and cancel("bid") then pulls the
        // old order: the side ends with nothing resting (`crates/vike-market-making/CLAUDE.md`).
        #[test]
        fn an_unconfirmed_restored_side_is_pulled_after_the_grace_then_requoted_on_the_next_tick() {
            use super::super::RESTORED_QUOTE_VERIFY_MS;
            let (mut after, _) = restart_pair();
            let t0 = 1_000;
            // inside the grace: the quote is continued in place (the clock starts at the first tick)
            for ts in [t0, t0 + 1, t0 + RESTORED_QUOTE_VERIFY_MS - 1] {
                let mut b = broker(0.0, ts);
                after.on_quote_tick(&mut b, &quote(ts, 0.40, 0.42));
                assert!(b.submissions.is_empty() && b.cancels.is_empty(), "grace: ts={ts}");
                assert_eq!(modify_tags(&b).len(), 2, "grace: still continued in place, ts={ts}");
            }
            // grace elapsed: pull both, place nothing, modify nothing
            let ts = t0 + RESTORED_QUOTE_VERIFY_MS;
            let mut pull = broker(0.0, ts);
            after.on_quote_tick(&mut pull, &quote(ts, 0.40, 0.42));
            assert_eq!(pull.cancels, vec!["bid".to_string(), "ask".to_string()], "pulled by tag");
            assert!(pull.submissions.is_empty(), "never cancel and place the same tag in one call");
            assert!(modify_tags(&pull).is_empty());
            assert!(!after.bid.placed && !after.ask.placed && after.bid.unverified.is_none());

            // the next tick places a fresh pair — and from then on it is an ordinary live side
            let mut next = broker(0.0, ts + 1);
            after.on_quote_tick(&mut next, &quote(ts + 1, 0.40, 0.42));
            assert_eq!(submits(&next).len(), 2, "re-placed");
            assert!(next.cancels.is_empty() && next.modifications.is_empty());
            let mut later = broker(0.0, ts + 10 * RESTORED_QUOTE_VERIFY_MS);
            after.on_quote_tick(&mut later, &quote(ts + 10 * RESTORED_QUOTE_VERIFY_MS, 0.41, 0.43));
            assert!(later.cancels.is_empty() && later.submissions.is_empty(), "no further pulls");
            assert_eq!(modify_tags(&later).len(), 2);
        }

        // A sign of life ends the unverified state: a non-terminal tagged event (the core could resolve
        // the tag) or a fill on that side (only one of our orders rests there). The confirmed side is
        // then never pulled, however old; the other, still unconfirmed side is.
        #[test]
        fn a_sign_of_life_confirms_a_restored_side_and_spares_it_the_pull() {
            use super::super::RESTORED_QUOTE_VERIFY_MS;
            let (mut after, _) = restart_pair();
            after.on_order_event(&mut broker(0.0, 500), &terminal("bid", OrderEventKind::Accepted));
            after.on_fill(&mut broker(0.0, 500), &a_fill(-1, 0.1, 500));
            assert_eq!(after.bid.unverified, None, "a tagged Accepted confirms the bid");
            assert_eq!(after.ask.unverified, None, "a fill on the ask confirms the ask");
            let mut b = broker(0.0, 500);
            after.on_quote_tick(&mut b, &quote(500, 0.40, 0.42));
            let ts = 500 + 3 * RESTORED_QUOTE_VERIFY_MS;
            let mut late = broker(0.0, ts);
            after.on_quote_tick(&mut late, &quote(ts, 0.40, 0.42));
            assert!(late.cancels.is_empty(), "confirmed sides are never pulled");
            assert_eq!(modify_tags(&late).len(), 2);

            // an unrelated / foreign / untagged event confirms nothing
            let (mut other, _) = restart_pair();
            other
                .on_order_event(&mut broker(0.0, 500), &terminal("bid0", OrderEventKind::Accepted));
            other.on_order_event(
                &mut broker(0.0, 500),
                &terminal("hedge", OrderEventKind::Accepted),
            );
            assert_eq!((other.bid.unverified, other.ask.unverified), (Some(0), Some(0)));
        }

        // A side that was NOT restored behaves exactly as today: the fresh side submits on its first tick,
        // is never pulled for age, and a half-restored maker (bid only) restores only the bid.
        #[test]
        fn a_side_that_was_not_restored_behaves_as_before() {
            use super::super::RESTORED_QUOTE_VERIFY_MS;
            let mut fresh = plain_maker();
            let mut b = broker(0.0, 100);
            fresh.on_quote_tick(&mut b, &quote(100, 0.40, 0.42));
            assert_eq!(submits(&b).len(), 2);
            let ts = 100 + 100 * RESTORED_QUOTE_VERIFY_MS;
            let mut old = broker(0.0, ts);
            fresh.on_quote_tick(&mut old, &quote(ts, 0.40, 0.42));
            assert!(
                old.cancels.is_empty() && old.submissions.is_empty(),
                "a live side is never aged out"
            );
            assert_eq!(fresh.bid.unverified, None);

            // only the bid placed in the saved state
            let mut m = plain_maker();
            let payload = serde_json::json!({
                "v": 1, "bid_suppressed_until": 0, "ask_suppressed_until": 0, "as_state": null,
                "bid": {"placed": true, "own": [0.39, 1.0], "quoted_timestamp": 90}
            });
            Strategy::<LiveBroker>::load_state(&mut m, &payload);
            let mut b = broker(0.0, 100);
            m.on_quote_tick(&mut b, &quote(100, 0.40, 0.42));
            let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
            assert_eq!(submitted, vec!["ask".to_string()], "only the unrestored ask is placed");
            assert_eq!(modify_tags(&b), vec!["bid".to_string()], "the restored bid continues");
        }

        // --- adoption: a tagged live event on a side that holds nothing -------------------------------
        //
        // The crash window `save_state` cannot close: the maker placed AFTER its last save and the daemon
        // died, so the restored maker is unplaced while the core has put the order and its tag back and
        // tells the maker through a synthetic tagged `Accepted`. Ignoring it made the first tick submit a
        // SECOND order under the tag and orphan the first.

        /// One side of `m`, by tag.
        fn side_of<'a>(m: &'a SpreadMaker, tag: &str) -> &'a SideState {
            if tag == "bid" { &m.bid } else { &m.ask }
        }

        // THE POINT: an unplaced maker told a tagged order lives ADOPTS it. Its next tick modifies that tag
        // (price and full size, even under a wide tolerance, since the quoted price/size are unknown) and
        // submits nothing on that side; the other side, which heard nothing, still places.
        #[test]
        fn an_unplaced_maker_adopts_a_tagged_accepted_and_modifies_instead_of_submitting() {
            for (tag, other) in [("bid", "ask"), ("ask", "bid")] {
                for tolerance in [None, Some((1_000.0, 1_000.0))] {
                    let mut m = plain_maker();
                    if let Some((px, qty)) = tolerance {
                        m = m.with_refresh_tolerance(px, qty);
                    }
                    m.on_order_event(
                        &mut broker(0.0, 100),
                        &terminal(tag, OrderEventKind::Accepted),
                    );
                    let s = side_of(&m, tag);
                    assert!(
                        s.placed && s.own.is_none() && s.refresh_stale && s.unverified.is_none()
                    );
                    assert!(!side_of(&m, other).placed, "the other side heard nothing");

                    let mut b = broker(0.0, 200);
                    m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
                    let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
                    assert_eq!(
                        submitted,
                        vec![other.to_string()],
                        "{tag}/{tolerance:?}: no second {tag}"
                    );
                    assert_eq!(
                        modify_tags(&b),
                        vec![tag.to_string()],
                        "{tag}/{tolerance:?}: re-priced"
                    );
                    assert!(b.cancels.is_empty(), "nothing is pulled");
                    let s = side_of(&m, tag);
                    assert!(
                        s.own.is_some() && !s.refresh_stale,
                        "the modify recorded what it asked for"
                    );
                }
            }
        }

        // Both sides adopted: the first tick re-prices both and submits nothing at all.
        #[test]
        fn an_unplaced_maker_that_adopts_both_sides_quotes_nothing_new() {
            let mut m = plain_maker();
            m.on_order_event(&mut broker(0.0, 100), &terminal("bid", OrderEventKind::Accepted));
            m.on_order_event(&mut broker(0.0, 100), &terminal("ask", OrderEventKind::Accepted));
            let mut b = broker(0.0, 200);
            m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
            assert!(b.submissions.is_empty(), "{:?}", submits(&b));
            assert_eq!(modify_tags(&b), vec!["bid".to_string(), "ask".to_string()]);
        }

        // A side that IS placed keeps today's behaviour: the event only confirms it alive. Its quoted
        // `(price, size)` and freshness are untouched, so a tolerance still skips an unchanged re-price.
        #[test]
        fn a_tagged_accepted_on_a_placed_side_changes_nothing_but_the_confirmation() {
            let mut m = plain_maker().with_refresh_tolerance(1_000.0, 1_000.0);
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            let (bid, ask) = (m.bid.own, m.ask.own);
            assert!(bid.is_some() && ask.is_some());
            m.bid.unverified = Some(0); // as a restored side would read
            m.on_order_event(&mut broker(0.0, 110), &terminal("bid", OrderEventKind::Accepted));
            assert_eq!(
                (m.bid.own, m.ask.own),
                (bid, ask),
                "the quoted price/size is not forgotten"
            );
            assert!(!m.bid.refresh_stale, "no staleness was invented");
            assert_eq!(m.bid.unverified, None, "confirmed alive");

            let mut b = broker(0.0, 120);
            m.on_quote_tick(&mut b, &quote(120, 0.40, 0.42));
            assert!(
                b.submissions.is_empty() && b.modifications.is_empty(),
                "the tolerance still skips"
            );
        }

        // A tagged terminal after the adoption frees the side like any other death; the next tick places.
        #[test]
        fn a_tagged_terminal_after_adoption_frees_the_side_and_the_next_tick_places() {
            let mut m = plain_maker();
            m.on_order_event(&mut broker(0.0, 100), &terminal("bid", OrderEventKind::Accepted));
            m.on_order_event(
                &mut broker(0.0, 150),
                &terminal("bid", OrderEventKind::Canceled { reason: "restore".into() }),
            );
            assert!(!m.bid.placed && m.bid.own.is_none() && !m.bid.refresh_stale);

            let mut b = broker(0.0, 200);
            m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
            let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
            assert_eq!(submitted, vec!["bid".to_string(), "ask".to_string()]);
            assert!(b.modifications.is_empty());
        }

        // The core refuses a submit over a restored tag with a tagged terminal `Denied`: the place arm had
        // already marked the side placed, so the Denied must free it and the next tick must place again.
        #[test]
        fn a_denied_submit_frees_its_side_and_the_next_tick_places_again() {
            let mut m = plain_maker();
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            let denied =
                OrderEventKind::Denied { reason: "restart: a restored order still holds".into() };
            m.on_order_event(&mut broker(0.0, 110), &terminal("bid", denied));
            assert!(!m.bid.placed && m.ask.placed);

            let mut b = broker(0.0, 200);
            m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
            let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
            assert_eq!(submitted, vec!["bid".to_string()]);
            assert_eq!(modify_tags(&b), vec!["ask".to_string()]);
        }

        // Adoption covers the SINGLE-quote tags only. A ladder rung (`"bid0"`, `"ask1"`: the tag is the
        // rung's index), a foreign tag and an untagged event leave an unplaced maker unplaced.
        #[test]
        fn adoption_ignores_ladder_foreign_and_untagged_events() {
            for event in [
                terminal("bid0", OrderEventKind::Accepted),
                terminal("ask1", OrderEventKind::Accepted),
                terminal("hedge", OrderEventKind::Accepted),
                OrderEvent {
                    order_client_id: "order-client-id-9".into(),
                    tag: None,
                    kind: OrderEventKind::Accepted,
                },
            ] {
                let mut m = plain_maker();
                m.on_order_event(&mut broker(0.0, 100), &event);
                assert!(!m.bid.placed && !m.ask.placed, "{event:?} adopted nothing");

                let mut b = broker(0.0, 200);
                m.on_quote_tick(&mut b, &quote(200, 0.40, 0.42));
                assert_eq!(submits(&b).len(), 2, "{event:?}: both sides place as on a fresh maker");
                assert!(b.modifications.is_empty());
            }
        }

        // --- "Option B" cross-symbol underlying routing (on_mark → set_underlying) -------------------

        /// An A-S maker on the 0.01 grid with the underlying blend at `weight` (its L1 quote lane snaps
        /// on `tick_size = 0.01`).
        fn underlying_maker(weight: f64) -> SpreadMaker {
            SpreadMaker::new(1.0, 0.01)
                .with_quote_style(QuoteStyle::Mid, 1, 0.01)
                .with_avellaneda_stoikov(blend_params(weight))
        }

        /// A mark of the underlying series (a DIFFERENT symbol than the token the maker trades).
        fn a_mark(price: f64, ts: i64) -> TickMark {
            TickMark { symbol: "btcusdt".into(), price, timestamp: ts }
        }

        /// The `"bid"`-tagged submit's price on a driven broker.
        fn bid_price(b: &LiveBroker) -> f64 {
            b.submissions
                .iter()
                .find(|s| s.tag.as_deref() == Some("bid"))
                .and_then(|s| s.price)
                .expect("a bid submit with a price")
        }

        // on_mark ROUTES the underlying into the A-S state: the first mark captures s_open only (no σ
        // yet ⇒ nothing fed), the second (a real move) seeds σ and feeds the full triple.
        #[test]
        fn on_mark_feeds_the_underlying_into_the_as_state() {
            let mut m = underlying_maker(0.5);
            assert_eq!(
                m.as_underlying(),
                None,
                "cold: no underlying until a mark warms the tracker"
            );
            m.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
            assert_eq!(m.as_underlying(), None, "one mark ⇒ s_open only, still no σ ⇒ not fed");
            m.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
            let (s_now, s_open, sigma) = m.as_underlying().expect("fed after two marks");
            assert_eq!(s_now.to_bits(), 100.05_f64.to_bits(), "s_now is the latest mark");
            assert_eq!(
                s_open.to_bits(),
                100.0_f64.to_bits(),
                "s_open is the window-open reference"
            );
            assert!(sigma > 0.0, "a real move ⇒ a positive per-second σ, got {sigma}");
        }

        // OFF / byte-identical: a maker WITHOUT the A-S layer has no `as_state`, so on_mark is a no-op
        // (never panics, feeds nothing) — the trait default is a no-op and this override honours it.
        #[test]
        fn on_mark_is_inert_without_the_as_layer() {
            let mut m = SpreadMaker::new(1.0, 0.01);
            assert!(!m.as_enabled(), "precondition: no A-S layer");
            m.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
            assert!(!m.as_enabled(), "on_mark never turns A-S on as a side effect");
            assert_eq!(m.as_underlying(), None, "nothing fed");
        }

        // The routed underlying reaches the PRICING: warm two makers identically with an UP-drift
        // underlying and differ ONLY in the blend weight — the weighted maker anchors toward p_up > 0.5,
        // so its next quote sits STRICTLY above the book-mid-anchored control's.
        #[test]
        fn on_mark_blend_shifts_the_next_quote() {
            let mut fed = underlying_maker(0.5);
            let mut ctrl = underlying_maker(0.0);
            // identical warmup (same marks) so only the weight differs.
            fed.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
            fed.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
            ctrl.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
            ctrl.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
            assert!(fed.as_underlying().is_some() && ctrl.as_underlying().is_some(), "both warmed");
            // one quote tick at a 0.50 book mid; both place a two-sided quote.
            let q = quote(152_000, 0.49, 0.51);
            let mut bf = broker(0.0, 152_000);
            let mut bc = broker(0.0, 152_000);
            fed.on_quote_tick(&mut bf, &q);
            ctrl.on_quote_tick(&mut bc, &q);
            assert!(
                bid_price(&bf) > bid_price(&bc),
                "the underlying blend pulls the bid up: fed {} vs ctrl {}",
                bid_price(&bf),
                bid_price(&bc)
            );
        }
    }

    /// The own-order LADDER wired into the maker end to end (`vike_market_making::OwnBookFiltration`).
    mod wire_in_tests {
        use super::*;
        use vike_marketdata::L2BookLevel;

        /// A bare in-crate `LiveBroker` at a given EVENT ts — mirrors `lib.rs`/`strategy_impl.rs`'s own
        /// test helpers of the same shape (this module can't reach those private copies).
        fn broker(now: i64) -> LiveBroker {
            LiveBroker {
                data_delay: None,
                inputs: None,
                closed_trades: None,
                positions: Vec::new(),
                prices: Vec::new(),
                bar_views: Vec::new(),
                leg_values: None,
                position: 0.0,
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
            }
        }

        /// The price the tagged order was submitted at.
        fn submit_price(b: &LiveBroker, tag: &str) -> f64 {
            let s = b.submissions.iter().find(|s| s.tag.as_deref() == Some(tag)).expect("a submit");
            s.price.expect("limit submit has a price")
        }

        /// Every buffered verb as a comparable `(kind, tag, price, qty)` tape — the equivalence probe.
        fn tape(b: &LiveBroker) -> Vec<(&'static str, String, Option<u64>, Option<u64>)> {
            let submits = b.submissions.iter().map(|s| {
                (
                    "submit",
                    s.tag.clone().unwrap_or_default(),
                    s.price.map(f64::to_bits),
                    Some(s.qty.to_bits()),
                )
            });
            let modifies = b.modifications.iter().map(|m| {
                (
                    "modify",
                    m.tag.clone(),
                    m.new_price.map(f64::to_bits),
                    m.new_qty.map(f64::to_bits),
                )
            });
            submits.chain(modifies).collect()
        }

        /// bids `[(100.0, 5.0), (99.5, 8.0)]` / asks `[(100.5, 5.0), (101.0, 8.0)]` on a 0.5 grid —
        /// the top level is exactly our quote size, so once we rest there we ARE the whole best level.
        fn l2() -> L2Book {
            let mut book = L2Book::new(0.5);
            book.apply_snapshot(
                1,
                &[L2BookLevel::new(100.0, 5.0), L2BookLevel::new(99.5, 8.0)],
                &[L2BookLevel::new(100.5, 5.0), L2BookLevel::new(101.0, 8.0)],
            );
            book
        }

        /// A `Join` maker quoting exactly the top level's size, with the ladder on at `buffer`.
        fn ladder_maker(buffer: i64) -> SpreadMaker {
            SpreadMaker::new(5.0, 0.0)
                .with_quote_style(QuoteStyle::Join, 0, 0.0)
                .with_own_order_book(0.5, buffer)
        }

        // THE RACE, end to end. Inside the accepted-buffer our just-placed order is NOT yet echoed by
        // the public feed, so filtration must leave the displayed depth alone; once the buffer elapses
        // it subtracts and the derived best shifts to the genuine next level. The contrast maker (same
        // config, ZERO buffer) shifts immediately — proving the buffer is what makes the difference.
        #[test]
        fn accepted_buffer_holds_the_public_book_then_subtracts_after_it() {
            let book = l2();
            let mut mm = ladder_maker(1_000);

            // tick 1 @ ts 0: nothing of ours rests yet → Join at the touch
            let mut b0 = broker(0);
            mm.on_order_book(&mut b0, &book);
            assert_eq!(
                submit_price(&b0, "bid").to_bits(),
                100.0_f64.to_bits(),
                "first quote joins touch"
            );
            assert_eq!(
                submit_price(&b0, "ask").to_bits(),
                100.5_f64.to_bits(),
                "ask joins the touch too"
            );

            // tick 2 @ ts 500 — INSIDE the buffer (accepted at 0, public at 1000). Our size is not in
            // the feed yet, so subtracting it would invent a phantom-empty level: the maker must still
            // see the full public book and re-quote at the SAME touch.
            let mut b1 = broker(500);
            mm.on_order_book(&mut b1, &book);
            assert_eq!(
                modify_price(&b1, "bid").to_bits(),
                100.0_f64.to_bits(),
                "inside the buffer: public depth is NOT double-subtracted"
            );
            assert_eq!(
                modify_price(&b1, "ask").to_bits(),
                100.5_f64.to_bits(),
                "same on the ask side"
            );

            // tick 3 @ ts 1000 — the buffer has elapsed, so our order IS assumed public: filtration
            // removes the level we wholly own and Join re-prices onto the genuine next level.
            let mut b2 = broker(1_000);
            mm.on_order_book(&mut b2, &book);
            assert_eq!(
                modify_price(&b2, "bid").to_bits(),
                99.5_f64.to_bits(),
                "buffer elapsed: our own level is subtracted, best shifts"
            );
            assert_eq!(
                modify_price(&b2, "ask").to_bits(),
                101.0_f64.to_bits(),
                "ask shifts out too"
            );

            // CONTRAST: the identical maker with a ZERO buffer shifts at the very next tick instead.
            let mut zero = ladder_maker(0);
            zero.on_order_book(&mut broker(0), &book);
            let mut z1 = broker(500);
            zero.on_order_book(&mut z1, &book);
            assert_eq!(
                modify_price(&z1, "bid").to_bits(),
                99.5_f64.to_bits(),
                "zero buffer trusts the ack at once — the buffer alone caused the hold above"
            );
        }

        // A zero-buffer ladder must reproduce the pre-existing single-snapshot filtration EXACTLY: the
        // ladder is a strictly richer representation, not a different policy. Bit-for-bit over the
        // whole buffered verb tape, across a multi-tick run that moves our order between levels.
        #[test]
        fn zero_buffer_ladder_matches_the_snapshot_path_bit_for_bit() {
            let book = l2();
            let mut ladder = ladder_maker(0);
            let mut snapshot = SpreadMaker::new(5.0, 0.0)
                .with_quote_style(QuoteStyle::Join, 0, 0.0)
                .with_own_order_filtration();

            for ts in [10, 20, 30, 40] {
                let (mut a, mut b) = (broker(ts), broker(ts));
                ladder.on_order_book(&mut a, &book);
                snapshot.on_order_book(&mut b, &book);
                assert_eq!(tape(&a), tape(&b), "tick {ts}: ladder and snapshot agree bit-for-bit");
            }
        }

        // The ladder is OFF by default and the snapshot opt-in does NOT turn it on — so both
        // pre-existing configurations keep their exact prior code path.
        #[test]
        fn ladder_is_off_by_default_and_snapshot_filtration_does_not_enable_it() {
            let plain = SpreadMaker::new(1.0, 0.5);
            assert!(plain.own_book().is_none(), "no ladder by default");
            assert!(!plain.params().filter_own, "and filtration is off by default");

            let snapshot = SpreadMaker::new(1.0, 0.5).with_own_order_filtration();
            assert!(snapshot.own_book().is_none(), "snapshot filtration uses no ladder");
            assert!(snapshot.params().filter_own, "but filtration is on");

            // the ladder builder implies filtration — one call is the whole opt-in
            let ladder = ladder_maker(100);
            assert!(ladder.own_book().is_some(), "ladder mounted");
            assert!(ladder.params().filter_own, "and filtration turned on with it");
        }

        // A runtime driving a REAL venue partial fill through `own_book_mut` shrinks what filtration
        // subtracts, so the maker stops treating a level it only partly owns as wholly its own.
        #[test]
        fn runtime_driven_partial_fill_shrinks_the_subtraction() {
            let book = l2();
            let mut mm = ladder_maker(0);
            mm.on_order_book(&mut broker(0), &book);
            assert_eq!(
                mm.own_book().and_then(|f| f.book.get("bid")).map(|o| o.qty.to_bits()),
                Some(5.0_f64.to_bits()),
                "the maker's own bid is tracked at full size"
            );

            // the venue partially fills 2.0 of our 5.0 — a runtime folds it into the ladder
            assert!(
                mm.own_book_mut().expect("ladder mounted").book.on_partial_fill("bid", 2.0),
                "the tagged order is known to the ladder"
            );

            // now only 3.0 of the 5.0 top level is ours, so 2.0 of genuine market depth remains: the
            // level SURVIVES filtration and Join keeps quoting the touch (without the fill it would
            // have been wholly ours and dropped, shifting the quote to 99.5).
            let mut b1 = broker(10);
            mm.on_order_book(&mut b1, &book);
            assert_eq!(
                modify_price(&b1, "bid").to_bits(),
                100.0_f64.to_bits(),
                "partial fill leaves real depth at the level, so the best does not shift"
            );
        }

        // A ladder built on an unknown (non-positive) grid is INERT — it tracks nothing and subtracts
        // nothing, so the maker prices straight off the feed. The same no-grid rule the snapshot path
        // already follows, and the documented footgun of the ladder's fixed grid.
        #[test]
        fn inert_no_grid_ladder_subtracts_nothing() {
            let book = l2();
            let mut mm = SpreadMaker::new(5.0, 0.0)
                .with_quote_style(QuoteStyle::Join, 0, 0.0)
                .with_own_order_book(0.0, 0);

            mm.on_order_book(&mut broker(0), &book);
            assert!(mm.own_book().expect("mounted").book.is_empty(), "no grid ⇒ nothing tracked");

            let mut b1 = broker(10);
            mm.on_order_book(&mut b1, &book);
            assert_eq!(
                modify_price(&b1, "bid").to_bits(),
                100.0_f64.to_bits(),
                "an inert ladder never subtracts — the maker keeps joining the raw touch"
            );
        }

        // A suppression PULL removes the quote from the ladder, so the depth we no longer have resting
        // stops being subtracted from the public book.
        #[test]
        fn a_pulled_quote_leaves_the_ladder() {
            let book = l2();
            // breaker armed so a single 5.0 bid fill trips the bid side and pulls that quote
            let mut mm = SpreadMaker::new(5.0, 0.0)
                .with_quote_style(QuoteStyle::Join, 0, 0.0)
                .with_own_order_book(0.5, 0)
                .with_fill_breaker(1_000, 2.5, 5_000);

            mm.on_order_book(&mut broker(0), &book);
            assert!(
                mm.own_book().expect("mounted").book.contains("bid"),
                "bid rests in the ladder"
            );

            // a big same-side fill trips the bid breaker; the next tick pulls that side
            let fill = vike_model::Fill {
                side: 1,
                size: 5.0,
                price: 100.0,
                fee: 0.0,
                timestamp: 10,
                is_maker: true,
                symbol: String::new(),
                tag: None,
            };
            Strategy::<LiveBroker>::on_fill(&mut mm, &mut broker(10), &fill);

            let mut b1 = broker(20);
            mm.on_order_book(&mut b1, &book);
            assert!(b1.cancels.iter().any(|t| t == "bid"), "the suppressed side is canceled");
            assert!(
                !mm.own_book().expect("mounted").book.contains("bid"),
                "and it leaves the ladder, so its size is no longer subtracted"
            );
            // the ask side is untouched by the bid-side pull — it still rests and is still tracked.
            // NOTE it has RE-PRICED 100.5 → 101.0: unsuppressed, it re-quoted this tick, and with the
            // zero buffer its own 5.0 at 100.5 was subtracted, wholly emptying that level so `Join`
            // moved to the next genuine ask. So its ladder entry moved levels with it.
            let own = mm.own_book().expect("mounted");
            assert!(own.book.contains("ask"), "the ask still rests");
            assert_eq!(
                own.book.quantity_at(OrderBookOwnSide::Ask, 101.0, &own.filter_at(20)).to_bits(),
                5.0_f64.to_bits(),
                "the ask's own size is tracked at the level it re-priced onto"
            );
            assert_eq!(
                own.book.quantity_at(OrderBookOwnSide::Ask, 100.5, &own.filter_at(20)).to_bits(),
                0.0_f64.to_bits(),
                "and nothing is left behind at the level it vacated"
            );
        }
    }
}
