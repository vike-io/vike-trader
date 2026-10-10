//! Mount configuration: the A-S maker's knobs, the strategy-free `MountSpec`, the maker fee floor.

#[cfg(doc)]
use vike_core::{StrategyMount, spawn_core};
#[cfg(doc)]
use vike_mm::SpreadMaker;
use vike_model::{
    AsParams, HorizonMode, PriceDomain, RefreshTolerance, RewardParams, ToxicityParams,
    VarianceMode,
};
#[cfg(doc)]
use vike_paper::PaperExecutionClient;

#[cfg(doc)]
use super::paper::build_paper_strategy_core_with;
#[cfg(doc)]
use super::sink::TickBarSynthesizer;

/// Inventory-skew knobs: [`SpreadMaker::with_skew`]'s three arguments (audit F9). A `Some` with
/// `skew == 0.0` is neutral too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MakerSkew {
    /// Inventory the skew biases toward ([`vike_model::SpreadMakerParams::target_inventory`]).
    pub target_inventory: f64,
    /// Inventory band the skew ramps over ([`vike_model::SpreadMakerParams::max_inventory`]; must be
    /// `> 0` to engage).
    pub max_inventory: f64,
    /// Skew intensity in `[0, 1]` ([`vike_model::SpreadMakerParams::skew`]; `0.0` = off).
    pub skew: f64,
}

/// Per-side fill-rate circuit-breaker knobs: [`SpreadMaker::with_fill_breaker`]'s three arguments
/// (the F9 twin of [`MakerSkew`]). Engages only when ALL THREE are positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MakerBreaker {
    /// Sliding EVENT-TIME window (ms) over which per-side fills are netted
    /// ([`vike_model::SpreadMakerParams::fill_window_ms`]).
    pub fill_window_ms: i64,
    /// NET one-directional fill size within the window that trips suppression
    /// ([`vike_model::SpreadMakerParams::net_fill_threshold`]).
    pub net_fill_threshold: f64,
    /// How long (ms) a tripped side's quote stays pulled
    /// ([`vike_model::SpreadMakerParams::suppress_cooldown_ms`]).
    pub suppress_cooldown_ms: i64,
}

/// Everything needed to stand up the paper maker mount. The A-S knobs live in `as_params`;
/// [`MakerMountConfig::outcome_token`] fills the `[0,1]` outcome-token defaults with
/// `resolution_ts` from a market's `end_date`. Every `Option` knob below defaults to `None`, which
/// never calls its `SpreadMaker` builder (feature off).
#[derive(Clone, Debug)]
pub struct MakerMountConfig {
    /// venue tag, the caller's; also the paper client's + engine's venue.
    pub venue: String,
    /// the outcome CLOB `token_id`: the mount SYMBOL the maker trades.
    pub token_id: String,
    /// the mounted bar-series interval (must match the synthesized bars: [`TickBarSynthesizer`]).
    pub interval: String,
    /// the synth-bar window in ms (e.g. `60_000` for `"1m"`).
    pub interval_ms: i64,
    /// base quote size per side (shares).
    pub qty: f64,
    /// fallback fixed half-spread: UNUSED while A-S prices; the `SpreadMaker::new` seed.
    pub half_spread: f64,
    /// the venue price grid (Polymarket = `0.01`) the A-S L1 lane snaps/clamps onto
    /// (`SpreadMaker::with_quote_style`).
    pub tick_size: f64,
    /// the Avellaneda–Stoikov knobs (`resolution_ts` for the time-to-resolution horizon).
    pub as_params: AsParams,
    /// Optional CROSS-SYMBOL underlying the maker WATCHES ("Option B" routing; e.g. the `btcusdt`
    /// RTDS spot a BTC up/down maker anchors its fair mid on), threaded onto
    /// [`StrategyMount::underlying_symbol`] so its marks reach `on_mark`. Lights the anchored fair
    /// mid / ATM guard together with `as_params.underlying_weight`/`window_secs` and a feed.
    pub underlying_symbol: Option<String>,
    /// Opt-in LIQUIDITY-REWARDS quoting ([`SpreadMaker::with_liquidity_rewards`]): folds the venue's
    /// reward band (`max_spread_cents`/`min_size`/`min_order_age_ms`) into the quote; `weight == 0`
    /// is inert ([`RewardParams`]). ⚠ No caller sets it: the deleted `polymarket_maker_paper` bin's
    /// picker was the only one (module doc).
    pub reward: Option<RewardParams>,
    /// Opt-in FLOW-TOXICITY guard ([`SpreadMaker::with_flow_toxicity`]): widens and cuts size on the
    /// toxic side per [`vike_model::FlowToxicity`]; both knobs `0.0` is inert ([`ToxicityParams`]).
    /// ⚠ A `Some` is inert today whatever its knobs: the only `on_flow` producer was deleted with
    /// the `polymarket_maker_paper` bin (module doc).
    pub toxicity: Option<ToxicityParams>,
    /// Opt-in inventory-skew size shaping ([`SpreadMaker::with_skew`], audit F9).
    pub skew: Option<MakerSkew>,
    /// Opt-in per-side fill-rate circuit breaker ([`SpreadMaker::with_fill_breaker`], audit F9;
    /// also the only live path to the #798 accelerated pull).
    pub breaker: Option<MakerBreaker>,
    /// Opt-in order-refresh TOLERANCE, the anti-churn gate ([`SpreadMaker::with_refresh_tolerance`],
    /// audit F9). `None` ⇒ every tick re-quotes both sides; an all-zero bag is inert too.
    pub refresh_tolerance: Option<RefreshTolerance>,
    /// paper account seed (equity), and the paper fill-cost scalars.
    pub seed_cash: f64,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
}

/// The STRATEGY-FREE half of a mount: everything [`spawn_core`] needs to place ANY strategy on a
/// `(venue, symbol, interval)` series and price its paper fills, with no A-S knob in it (the
/// [`SpreadMaker`] knobs stay on [`MakerMountConfig`]), so any boxed
/// `Strategy<LiveBroker> + Send` gets the identical mount.
///
/// ⚠ The A-S PRICE-DOMAIN coupling does NOT live here: [`MakerMountConfig::outcome_token`] and
/// [`MakerMountConfig::crypto`] differ in `as_params` AND in generic defaults (`seed_cash` 1_000 vs
/// 100_000, `tick_size`/`qty`); a hand-built `MountSpec` states each rather than inheriting a
/// maker's opinion of how much paper equity an asset needs.
#[derive(Clone, Debug)]
pub struct MountSpec {
    /// venue tag; also the paper client's + engine's venue.
    pub venue: String,
    /// the mount SYMBOL: the instrument the strategy trades.
    pub symbol: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` is the venue's DEFAULT
    /// account: the same engine, `route_key` (the bare venue id) and `LIVE-<route_key>.lock`.
    /// `Some(label)` routes orders AND `Broker` reads (position / equity / multiplier / lot size) to
    /// that account's engine, and [`Self::symbol`] is the symbol it is armed on
    /// (`crate::account_symbols_for`).
    ///
    /// ⚠ **A named account that is not ARMED must never fall through to the default one.** The root
    /// checks it against the SAME `crate::venue_account_arming` the fan-out selects with and DROPS
    /// the mount with an error naming venue, account and strategy; `vike_core`'s `assemble_core`
    /// panics on a named account with no engine, the backstop no future root can reach past.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// the mounted bar-series interval (must match the synthesized bars: [`TickBarSynthesizer`]).
    pub interval: String,
    /// the [`TickBarSynthesizer`] window in ms (e.g. `60_000` for `"1m"`): a feed / paper-fill
    /// concern, not a strategy knob.
    pub interval_ms: i64,
    /// Optional CROSS-SYMBOL underlying the strategy WATCHES
    /// ([`StrategyMount::underlying_symbol`]); `None` ⇒ no routing.
    pub underlying_symbol: Option<String>,
    /// Optional explicit mount id ([`StrategyMount::controller_id`]); `None` ⇒
    /// `{venue}__{symbol}__{interval}`. REQUIRED when two mounts share one triple: `assemble_core`
    /// PANICS on a duplicate rather than silently sharing durable state.
    pub controller_id: Option<String>,
    /// Opt-in ADDITIONAL symbols this mount may trade and read ([`StrategyMount::symbols`]).
    ///
    /// ⚠ EMPTY on every mount built today, asserted by [`build_paper_strategy_core_with`]'s
    /// tripwire: a single-book [`PaperExecutionClient`] stamps its own symbol on every fill, so a
    /// multi-leg PAPER rehearsal would LOOK correct while the live core routed the legs apart.
    /// Whoever fills this must switch that builder to `vike_paper::MultiPaperExecutionClient` in
    /// the same change.
    pub legs: Vec<vike_core::MountLeg>,
    /// paper account seed (equity), and the paper fill-cost scalars.
    pub seed_cash: f64,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
}

impl MakerMountConfig {
    /// The STRATEGY-FREE projection (A-S knobs left behind). Each `build_*_maker_core` is exactly
    /// `build_*_strategy_core(Box::new(build_maker(cfg)), &cfg.mount_spec(), …)`.
    pub fn mount_spec(&self) -> MountSpec {
        MountSpec {
            venue: self.venue.clone(),
            symbol: self.token_id.clone(),
            interval: self.interval.clone(),
            interval_ms: self.interval_ms,
            underlying_symbol: self.underlying_symbol.clone(),
            // A maker mount names no explicit id, leg or ACCOUNT (`None` = the default account).
            account: None,
            controller_id: None,
            legs: Vec::new(),
            seed_cash: self.seed_cash,
            slippage: self.slippage,
            maker_fee: self.maker_fee,
            taker_fee: self.taker_fee,
        }
    }

    /// The recommended OUTCOME-TOKEN paper-maker configuration: A-S tuned for `[0,1]` prices, the
    /// time-to-resolution horizon anchored on `resolution_ts_ms` (the market's `end_date`; `None` ⇒
    /// the constant `tau_hold`). The tuning belongs to the PRICE DOMAIN, so the venue is the
    /// caller's (Polymarket today; docs/decisions/0098). Every field is public: tweak after.
    pub fn outcome_token(
        venue: impl Into<String>,
        token_id: impl Into<String>,
        resolution_ts_ms: Option<i64>,
    ) -> Self {
        MakerMountConfig {
            venue: venue.into(),
            token_id: token_id.into(),
            interval: "1m".to_string(),
            interval_ms: 60_000,
            qty: 20.0,
            half_spread: 0.01,
            tick_size: 0.01,
            as_params: AsParams { resolution_ts: resolution_ts_ms, ..AsParams::default() },
            // Underlying routing OFF; the anchored knobs (`underlying_weight`/`window_secs`/
            // `atm_blackout_scale`) stay at their inert `0.0` defaults.
            underlying_symbol: None,
            // Rewards, toxicity, skew, breaker, refresh-tolerance: all OFF.
            reward: None,
            toxicity: None,
            skew: None,
            breaker: None,
            refresh_tolerance: None,
            seed_cash: 1_000.0,
            // Polymarket CLOB fees are effectively 0; a resting maker fill has no slippage.
            slippage: 0.0,
            maker_fee: 0.0,
            taker_fee: 0.0,
        }
    }

    /// A `$`-scale crypto paper-maker configuration, the TWIN of [`Self::outcome_token`] for an
    /// UNBOUNDED `$`-priced asset (a BTC/ETH perp mid ~$64k, an equity) on the SAME
    /// [`vike_mm::SpreadMaker`]. The A-S knobs and WHY:
    /// - [`VarianceMode::RawLocal`]: the Bernoulli cap `p(1−p)` goes NEGATIVE for a price `> 1`.
    /// - [`HorizonMode::ConstantTau`]: an open-ended market never resolves.
    /// - [`PriceDomain::Unbounded`]: no `[tick, 1−tick]` clamp; only the standoff + `bid < s < ask`.
    /// - `min_half_spread_ticks = 2`: the intensity half-spread is SUB-TICK at `$`-scale (~0.02 at a
    ///   $64k mid on a $1 tick), so both sides snapped onto ONE tick and the quote collapsed to
    ///   `None` (the live HL/BTC no-quote bug).
    /// - `max_half_spread_ticks = 60`: `½·γ·σ̂²·H` grows as the SQUARE of the move, so a spike would
    ///   post off the book exactly when spreads are richest. ~$60 half / ~19 bp of a $64k mid keeps
    ///   the widening through an ordinary move (~$9/tick) and stays fillable in a spike (pinned by
    ///   vike-mm's `crypto_width_tuning_tests` workbench).
    /// - `round_trip_fee_rate`: **ARMED HERE from the venue's fee schedule**
    ///   ([`maker_round_trip_fee_for`]), the ONE knob the library default leaves OFF: the
    ///   half-spread is floored at the BREAK-EVEN width `½·m·s`, and a mount that cannot cover its
    ///   round trip under the cap posts NOTHING ([`vike_model::AsParams::round_trip_fee_rate`];
    ///   `vike_mm::avellaneda::bounded_half_spread` for why it refuses instead of widening).
    ///   ⚠ Armed by DEFAULT because the mount it was written for was measured losing money on every
    ///   completed round trip: a silent halt beats a silent bleed, and the halt is attributable (the
    ///   maker's edge-triggered `warn!`, the startup `effective_params` line). A fee SHAPE with no
    ///   flat fraction of price arms nothing and says so, never a silent `0.0`.
    ///
    /// `gamma`/`q_scale` are STARTING values (from the vike-mm width workbench; re-tune freely):
    /// - `gamma = 5e-4`: the vol half-spread is `½·γ·V`, `V = σ̂²·H` in PRICE². A BTC-scale `σ̂²·H` at
    ///   the default `tau_hold_ms` is O(1e3–1e4), so a small `γ` keeps it to a few ticks. Retune
    ///   against live σ̂² (or shrink `as_params.tau_hold_ms`).
    /// - `q_scale = 3e-2`: `q_norm = position/q_scale`; the outcome-token `100` (a SHARE count)
    ///   would make a ~0.005 BTC clip `q_norm ≈ 5e-5` and kill the skew. Per clip the reservation
    ///   skew is `(0.005/0.03)·γV ≈ 0.17·γV`, a THIRD of the `0.5·γV` half-spread: ~3 clips
    ///   (≈0.015 BTC) shift it a full half-spread, ~6 pull the risk-reducing side to the mid. Looser
    ///   than the strict `1e-2` (where ONE clip pinned the ask to the mid); `5e-2` warehouses more.
    pub fn crypto(
        venue: impl Into<String>,
        token_id: impl Into<String>,
        tick_size: f64,
        qty: f64,
    ) -> Self {
        let venue = venue.into();
        let token_id = token_id.into();
        let round_trip_fee_rate = maker_round_trip_fee_for(&venue, &token_id);
        MakerMountConfig {
            venue,
            token_id,
            interval: "1m".to_string(),
            interval_ms: 60_000,
            qty,
            // Unused while A-S prices (the `SpreadMaker::new` seed).
            half_spread: tick_size * 2.0,
            tick_size,
            as_params: AsParams {
                variance_mode: VarianceMode::RawLocal,
                horizon_mode: HorizonMode::ConstantTau,
                price_domain: PriceDomain::Unbounded,
                min_half_spread_ticks: 2.0,
                // 60 ticks ⇒ a ~$120 full spread ≈ 19 bp at a $64k mid (derivation: the fn doc).
                max_half_spread_ticks: 60.0,
                // `None` when the fee SHAPE has no flat fraction of price, never a silent `0.0`.
                round_trip_fee_rate,
                gamma: 5e-4,
                q_scale: 3e-2,
                ..AsParams::default()
            },
            // Underlying, rewards, toxicity, skew, breaker, refresh-tolerance: all OFF.
            underlying_symbol: None,
            reward: None,
            toxicity: None,
            skew: None,
            breaker: None,
            refresh_tolerance: None,
            // Equity for a `$`-scale asset (one BTC clip is ~$300+). Fees 0.0 so `paper_client_for`
            // uses the venue's `fee_schedule_for`.
            seed_cash: 100_000.0,
            slippage: 0.0,
            maker_fee: 0.0,
            taker_fee: 0.0,
        }
    }
}

/// The SINGLE-VENUE round-trip maker fee that arms the A-S break-even floor (the fee half of
/// [`MakerMountConfig::crypto`], beside its diagnostic). Lane-keyed like [`paper_client_for`]'s
/// paper book (on binance/aster the SYMBOL picks the order API, `vike_catalog::fee_lane`'s `.P`
/// split), so the floor and the simulated fills read the SAME schedule.
///
/// ⚠ It LOGS, because a fee nobody could name must not look like a fee of zero:
/// [`vike_model::maker_round_trip_fee`] refuses by SHAPE (`Free` FX/CFD, ibkr per-share,
/// polymarket's `p(1−p)`, deribit's premium cap) with `None`, which arms no floor. So once, at
/// mount: armed at `info`, unarmed at `warn` naming venue, lane and shape.
///
/// ⚠ The rate is the STATIC registry row: the live account rate (`ReconClient::fetch_fee_rates`,
/// preferred by `crate::resolve_fee_schedule`) needs a reconcile-gated `ReconClient`
/// ([`NodeConfig::recon_enabled`]). A VIP tier below tier 0 is floored CONSERVATIVELY: it can
/// refuse a marginally profitable mount, never admit a losing one.
fn maker_round_trip_fee_for(venue: &str, symbol: &str) -> Option<f64> {
    let lane = vike_catalog::fee_lane(venue, symbol);
    let schedule = vike_model::fee_schedule_for(lane);
    match vike_model::maker_round_trip_fee(schedule) {
        Some(rate) => {
            tracing::info!(
                venue,
                symbol,
                fee_lane = lane,
                round_trip_fee_bps = rate * 1e4,
                "maker break-even floor ARMED: a quote narrower than ½·fee·mid will not be posted"
            );
            Some(rate)
        }
        None => {
            tracing::warn!(
                venue,
                symbol,
                fee_lane = lane,
                ?schedule,
                "maker break-even floor NOT armed: this venue's fee shape has no flat fraction of \
                 price, so nothing here can say what a round trip costs. The maker will quote \
                 WITHOUT a break-even floor — this is an absent bar, NOT a zero fee."
            );
            None
        }
    }
}
