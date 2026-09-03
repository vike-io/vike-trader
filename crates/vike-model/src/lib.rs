//! vike-model — the vike trading core's domain model.
//!
//! Exact ports (origin/main `023d6e8`) of:
//! - `core/model.py`        → [`bar::Bar`], [`position::{Position, Fill, Trade}`]
//! - `core/ticks.py`        → [`bar::{QuoteTick, TradeTick}`]
//! - `core/fill.py`         → [`fill::{compute_fill, FillOutcome}`] (THE cost-basis primitive)
//! - `core/orders.py`       → [`order::WorkingOrder`] (resting record; the fill-trigger fn is vt-sim/R1)
//! - `core/order_intent.py` → [`order::{OrderRequest, order_request_to_working}`]
//! - `exec/events.py`       → [`events`] (the full event union)
//! - `data/instrument_db.py` (filter row shape) → [`instrument::SymbolProperties`]
//!
//! Plus the [`clock::Clock`] trait (Test/Live), the backtest=live seam.
//!
//! PARITY RULES (apply to every file in this crate + vt-sim/vike-exec):
//! - f64 end-to-end, expressions in the SAME order as Python; no `mul_add`, no fast-math.
//! - Exact-zero flat checks stay `== 0.0` where Python has `== 0.0`.
//! - Timestamps are epoch-ms i64 (live events may ADD a recv_ns stamp later; never replace ms).

pub mod account_keys;
pub mod attribution;
pub mod bar;
pub mod barrier;
pub mod cash;
pub mod change_journal;
pub mod client_order_id;
pub mod clock;
pub mod consolidator;
pub mod credential_keys;
pub mod equity;
pub mod events;
pub mod fair;
pub mod feed_status;
pub mod fees;
pub mod fill;
pub mod fill_trigger;
pub mod finite;
pub mod halt_admit;
pub mod impact;
pub mod instrument;
pub mod liquidation;
pub mod margin;
pub mod market_slippage;
pub mod order;
pub mod orderbook;
pub mod position;
pub mod pysum;
pub mod rate_limits;
pub mod reports;
pub mod scalar;
pub mod schedule;
pub mod scratch;
pub mod session;
pub mod sizing;
pub mod spread_quote;
pub mod state_path;
pub mod store_path;
pub mod strategy;
pub mod tick_scheme;
pub mod tick_store_path;
pub mod time;
pub mod venue_amend;
pub mod venue_caps;
pub mod venue_hold;
pub mod venue_margin_support;
pub mod venue_rate_limits;
pub mod venues;

pub use bar::{Bar, QuoteTick, TradeTick};
pub use barrier::{ControllerParams, TripleBarrier};
pub use cash::RateBook;
pub use client_order_id::{is_valid_crypto_coid, ClientOrderIdGenerator};
pub use clock::{now_ms, now_ms_u64, now_ns, now_us, Clock, LiveClock, TestClock};
pub use consolidator::{
    consolidate_quotes, consolidate_trades, quote_tick_to_bar, trade_tick_to_bar, BarConsolidator,
};
pub use equity::EquitySample;
pub use events::Event;
pub use fair::p_up;
pub use fees::{
    fee_schedule_for, fee_schedule_for_with_pm_curve, maker_round_trip_fee, xemm_round_trip_fee,
    FeeSchedule, POLYMARKET_PROB_CURVE, POLYMARKET_V2_FEE_CURVE,
};
pub use fill::{compute_fill, ClosedTrade, FillKind, FillOutcome, FoldStep, TradeFold};
pub use fill_trigger::{one_price_bar, order_fill_price, order_fill_price_granular};
pub use finite::FiniteNumbers;
pub use halt_admit::{
    effective_halt_admit, halt_admit_arming, halt_verify_support, HaltAdmit, HaltAdmitArming,
    HaltVerify,
};
pub use impact::{
    fillable_veto, impact_veto, scoped_impact_veto, take_scope, ImpactDeny, TakeScope,
};
pub use instrument::SymbolProperties;
pub use liquidation::{
    cross_liquidation_plan, cross_liquidation_price_est, partition_pools, pool_breached,
    LiqCandidate, PoolPartition,
};
pub use margin::{
    amount_to_order, clamp_leverage, free_buying_power, has_sufficient_margin, initial_margin,
    liquidation_price, maintenance_margin,
};
pub use order::{
    build_bracket, build_combo, combo_net, combo_net_cross, combo_net_from_legs,
    order_request_to_working, tif_expired, utc_day, BracketSpec, ComboError, ComboLeg, ComboSpec,
    OrderKind, OrderRequest, TimeInForce, TriggerBy, WorkingOrder, MS_PER_DAY,
};
pub use orderbook::{
    book_taker_price, BookUpdate, BookUpdateKind, DeltaDecision, FillSim, L2Book, Level, SeqPolicy,
};
pub use position::{Fill, Position, Trade};
pub use pysum::py_sum;
pub use rate_limits::{RateLimitConfig, DEFAULT_UTILIZATION, MAX_UTILIZATION, MIN_UTILIZATION};
pub use reports::{FillReport, OrderStatusReport, PositionStatusReport};
pub use scalar::{
    closing_side, gross_notional, is_covered_reduce, is_implicit_reduce, is_reducing_direction,
    nz_step, order_notional, round_to, round_to_step, signed_notional,
};
pub use schedule::TimeRule;
pub use session::{
    session_for, venue_is_open, SessionCalendar, SessionSegment, SessionState, DAY_MINUTES,
    WEEK_MINUTES,
};
pub use sizing::{units_from_percent, units_from_value};
pub use spread_quote::{
    executable_spread, spread_carry_cost, spread_edge_clears_cost, spread_roundtrip_cost,
    spread_total_cost, SpreadLeg,
};
pub use strategy::{
    AsParams, Broker, FeedStatus, FlowToxicity, HftBroker, HorizonMode, KappaMode, LadderLevel,
    LadderOffsetUnit, LadderParams, LadderSizeProfile, MarkTick, MultiHftBroker, OrderEventKind,
    OrderLifecycle, PriceDomain, QuoteStyle, RefreshTolerance, ReservationModel, RewardParams,
    SpreadMakerParams, SpreadModel, SpreadSource, Strategy, StrategyParams, ToxicityParams,
    VarianceMode, XemmParams,
};
pub use tick_scheme::{round_price_tiered, TickScheme, TickSchemeError, TickTier, MAX_TICK_TIERS};
pub use time::{
    civil_from_days, days_from_civil, epoch_ms_to_utc_date, epoch_ns_to_utc_date, parse_date_label,
    parse_hour_label, parse_ymd, utc_weekday,
};
pub use venue_amend::{amend_semantics, AmendSemantics};
pub use venue_caps::{
    caps_for, preflight_order, preflight_order_at, LiveDataCaps, LiveVerb, PreflightDeny,
    TriggerType, VenueCaps,
};
pub use venue_hold::{MS_PER_SECOND, POLYMARKET_ITODE_HOLD_MS, POLYMARKET_SPORTS_GAME_HOLD_MS};
pub use venue_margin_support::{
    venue_margin_support, MarginMode, SwitchMechanism, VenueMarginSupport,
};
pub use venue_rate_limits::{rate_limits_for, History, Market, Meter, Provenance, RateLimits};
pub use venues::VENUES;

/// Decode a fixture f64 encoded as a hex bit-pattern string (e.g. "3ff0000000000000").
/// Golden fixtures carry floats as `f64::to_bits` hex so last-ulp divergences are unambiguous.
pub fn f64_from_hex_bits(s: &str) -> Result<f64, std::num::ParseIntError> {
    Ok(f64::from_bits(u64::from_str_radix(s, 16)?))
}

/// Encode an f64 as its hex bit-pattern string (the fixture wire format).
pub fn f64_to_hex_bits(x: f64) -> String {
    format!("{:016x}", x.to_bits())
}
