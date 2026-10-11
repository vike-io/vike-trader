//! vike-model — the vike trading core's domain model.
//!
//! Exact ports (origin/main `023d6e8`) of:
//! - `core/model.py`        → `vike_marketdata::Bar` (re-exported), [`money::position::{Position, Fill, Trade}`]
//! - `core/ticks.py`        → `vike_marketdata::{QuoteTick, TradeTick}` (re-exported)
//! - `core/fill.py`         → [`money::fill::{compute_fill, FillOutcome}`] (THE cost-basis primitive)
//! - `core/orders.py`       → [`orders::order::WorkingOrder`] (resting record; the fill-trigger fn,
//!   R1's [`orders::fill_trigger::order_fill_price`], lives here since 2026-07-07 — it was ported
//!   into `vt-sim`, the simulator crate later renamed `vike-backtest`)
//! - `core/order_intent.py` → [`orders::order::{OrderRequest, order_request_to_working}`]
//! - `exec/events.py`       → [`events`] (the full event union)
//! - `data/instrument_db.py` (filter row shape) → [`instrument::SymbolProperties`]
//!
//! Plus the [`time::clock::Clock`] trait (Test/Live), the backtest=live seam.
//!
//! PARITY RULES (apply to every file in this crate + vike-sim/vike-exec):
//! - f64 end-to-end, expressions in the SAME order as Python; no `mul_add`, no fast-math.
//! - Exact-zero flat checks stay `== 0.0` where Python has `== 0.0`.
//! - Timestamps are epoch-ms i64 (live events may ADD a recv_ns stamp later; never replace ms).

#![warn(unreachable_pub)]

// Account identity: the multi-account credential-key grammar and the venue-handshake confirmation.
pub mod accounts;
pub mod change_journal;
pub mod credential_keys;
pub mod diagnostic;
pub mod events;
pub mod fair;
pub mod feed_status;
pub mod finite;
pub mod host_build;
pub mod instance_origin;
pub mod instrument;
/// The SHARED text parser behind every crate's `libm_platform_probe` (decision 0074). Gated the
/// same way as `strategy::MockBroker` — a default build compiles none of it — because it is test
/// machinery that eleven crates reach as a dev-dependency feature, not production vocabulary.
/// Its own planted-fixture self-test deliberately lives in `tests/`; the module doc says why.
#[cfg(any(test, feature = "test-support"))]
pub mod libm_walk;
pub mod market_slippage;
// Positions and money: cost basis, cash, equity, margin and liquidation, sizing and fees.
pub mod money;
// Order vocabulary: the request, its client id, how it triggers and fills against a bar, its
// barriers, whether HALT admits it, and the venue reports reconcile reads back.
pub mod orders;
// Filesystem-path resolution: the project-root walk, the hist store and the live-tick store, plus
// the store-plane classifier (whether a store kind holds market data or account data).
pub mod paths;
pub mod pricing;
pub mod pysum;
pub mod rate_limits;
// The pre-trade risk CONFIGURATION: the gate's limits and the run profile's `[risk]` table (the
// gate itself stays in vike-exec). Named at this root; `risk::surface` is the one public child.
pub mod risk;
/// What a RUN leaves behind: the manifest every producer writes identically, beside a report only
/// that producer's kind understands. Here rather than beside the first producer because the
/// READERS — the Studio's Research tab and `vike-cli backtest ls` — sit in crates that may not
/// depend on `vike-backtest`; the module's own doc carries the whole argument.
pub mod runs;
pub mod scalar;
/// The pure SOURCE SCANNER behind the settings-registry gate and every other gate that reads Rust text
/// (`env::var` sites, map lookups, named calls, comment stripping). It was `vike_ops::scan` until
/// 2026-10-08; it moved here so a gate living in ANY crate can take it as a dev-dependency without
/// taking `vike-ops`. Gated exactly like `libm_walk` above, for the same reason: test machinery with
/// no production caller, reached as a dev-dependency feature.
#[cfg(any(test, feature = "test-support"))]
pub mod scan;
pub mod scratch;
pub mod strategy;
/// Test helpers several crates' tests used to copy — gated exactly like `libm_walk` above, for the
/// same reason: test machinery reached as a dev-dependency feature, never production vocabulary.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
// Time: the UTC civil calendar (its own items), the `Clock` seam and the `TimeRule` firing rule.
pub mod time;
// The venue roster and the per-venue capability tables that iterate it (`venues/mod.rs` declares
// them; the roster of tables is DERIVED by `just new-venue-sites`, never listed in prose).
pub mod venues;

pub use diagnostic::{Diagnostic, Severity};
pub use fair::p_up;
pub use finite::FiniteNumbers;
pub use instance_origin::{InstanceOrigin, OriginError};
pub use instrument::SymbolProperties;
pub use instrument::asset_class::AssetClass;
pub use instrument::tick_scheme::{
    MAX_TICK_TIERS, TickScheme, TickSchemeError, TickTier, round_price_tiered,
};
pub use money::cash::RateBook;
pub use money::equity::EquitySample;
pub use money::fees::{
    FeeSchedule, POLYMARKET_PROB_CURVE, POLYMARKET_V2_FEE_CURVE, fee_schedule_for,
    fee_schedule_for_with_pm_curve, maker_round_trip_fee, xemm_round_trip_fee,
};
pub use money::fill::{ClosedTrade, FillKind, FillOutcome, FoldStep, TradeFold, compute_fill};
pub use money::liquidation::{
    LiqCandidate, PoolPartition, cross_liquidation_plan, cross_liquidation_price_est,
    partition_pools, pool_breached,
};
pub use money::margin::{
    amount_to_order, clamp_leverage, free_buying_power, has_sufficient_margin, initial_margin,
    liquidation_price, maintenance_margin,
};
pub use money::position::{Fill, Position, Trade};
pub use money::sizing::{units_from_percent, units_from_value};
pub use orders::barrier::{ControllerParams, TripleBarrier};
pub use orders::client_order_id::is_valid_crypto_coid;
pub use orders::fill_trigger::{one_price_bar, order_fill_price, order_fill_price_granular};
pub use orders::halt_admit::{
    HaltAdmit, HaltAdmitArming, HaltVerify, effective_halt_admit, halt_admit_arming,
    halt_verify_support,
};
pub use orders::order::{
    BracketSpec, ComboError, ComboLeg, ComboSpec, MS_PER_DAY, OrderKind, OrderRequest, TimeInForce,
    TriggerBy, WorkingOrder, build_bracket, build_combo, combo_net, combo_net_cross,
    combo_net_from_legs, order_request_to_working, tif_expired, utc_day,
};
pub use orders::own_book::{OwnOrder, OwnOrderBook, OwnQtyFilter, OwnSide, OwnStatus, StatusMask};
pub use orders::reports::{FillReport, OrderStatusReport, PositionStatusReport};
pub use paths::store_plane::{ACCOUNT_KINDS, StorePlane, is_account_kind, plane_of};
pub use pricing::impact::{
    ImpactDeny, TakeScope, fillable_veto, impact_veto, scoped_impact_veto, take_scope,
};
pub use pricing::spread_quote::{
    SpreadLeg, executable_spread, spread_carry_cost, spread_edge_clears_cost,
    spread_roundtrip_cost, spread_total_cost,
};
pub use pysum::py_sum;
pub use rate_limits::{DEFAULT_UTILIZATION, MAX_UTILIZATION, MIN_UTILIZATION, RateLimitConfig};
pub use risk::limits::{PriceCollar, ResolvedGrid, RiskLimits, SymbolGrid};
pub use risk::profile::{GridSource, ProfileError, ProfileRisk};
pub use scalar::{
    closing_side, gross_notional, is_covered_reduce, is_implicit_reduce, is_reducing_direction,
    nz_step, order_notional, round_to, round_to_step, signed_notional,
};
pub use strategy::{
    AsParams, Broker, FeedStatus, FlowToxicity, HftBroker, HorizonMode, KappaMode, LadderLevel,
    LadderOffsetUnit, LadderParams, LadderSizeProfile, MarkTick, MultiHftBroker, OrderEventKind,
    OrderLifecycle, PriceDomain, QuoteStyle, RESERVED_SRC_KEY, RefreshTolerance, ReservationModel,
    RewardParams, SpreadMakerParams, SpreadModel, SpreadSource, Strategy, StrategyParams,
    ToxicityParams, VarianceMode, XemmParams,
};
pub use time::clock::{Clock, LiveClock, TestClock, now_ms, now_ms_u64, now_ns, now_us};
pub use time::{epoch_ns_to_utc_date, parse_date_label, parse_ymd, utc_weekday};
pub use venues::VENUES;
pub use venues::link_deadman::{LinkDeadMan, link_deadman_default};
pub use venues::session::{
    DAY_MINUTES, SessionCalendar, SessionSegment, SessionState, WEEK_MINUTES, session_for,
    venue_is_open,
};
pub use venues::venue_amend::{AmendSemantics, amend_caps_note, amend_semantics};
pub use venues::venue_caps::{
    LiveVerb, PreflightDeny, TriggerType, VenueCaps, caps_for, preflight_order, preflight_order_at,
};
pub use venues::venue_hold::{
    MS_PER_SECOND, POLYMARKET_ITODE_HOLD_MS, POLYMARKET_SPORTS_GAME_HOLD_MS,
};
pub use venues::venue_margin_support::{MarginMode, SwitchMechanism, venue_margin_support};
pub use venues::venue_rate_limits::{
    History, Market, Meter, Provenance, RateLimits, rate_limits_for,
};
// The MARKET-DATA vocabulary, re-exported at this crate root from `vike-marketdata` (layer 5).
// Those three modules lived HERE until the split; the types are the domain vocabulary's own nouns
// — a `Broker` method takes an `&L2Book`, a `Strategy` is fed a `&Bar` — so spelling them
// `vike_model::Bar` is public API rather than a compatibility shim.
// `crates/vike-marketdata/src/lib.rs`'s module doc argues the split and names what did NOT move.
pub use vike_marketdata::{
    Bar, BarConsolidator, BookLevel, BookUpdate, BookUpdateKind, DeltaDecision, FillSim, L2Book,
    QuoteTick, SeqPolicy, TradeTick, book_taker_price, consolidate_quotes, consolidate_trades,
    quote_tick_to_bar, trade_tick_to_bar,
};

/// Decode a fixture f64 encoded as a hex bit-pattern string (e.g. "3ff0000000000000").
/// Golden fixtures carry floats as `f64::to_bits` hex so last-ulp divergences are unambiguous.
pub fn f64_from_hex_bits(s: &str) -> Result<f64, std::num::ParseIntError> {
    Ok(f64::from_bits(u64::from_str_radix(s, 16)?))
}

/// Encode an f64 as its hex bit-pattern string (the fixture wire format).
pub fn f64_to_hex_bits(x: f64) -> String {
    format!("{:016x}", x.to_bits())
}
