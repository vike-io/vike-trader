//! vike-model — the vike trading core's domain model.
//!
//! Exact ports (origin/main `023d6e8`) of:
//! - `core/model.py`        → `vike_marketdata::Bar` (that crate's, not re-exported here), [`money::position::{Position, Fill, Trade}`]
//! - `core/ticks.py`        → `vike_marketdata::{TickQuote, TickTrade}` (likewise)
//! - `core/fill.py`         → [`money::fill::{compute_fill, FillOutcome}`] (THE cost-basis primitive)
//! - `core/orders.py`       → [`orders::order::OrderResting`] (resting record; the fill-trigger fn,
//!   R1's [`orders::fill_trigger::order_fill_price`], lives here since 2026-07-07 — it was ported
//!   into `vt-sim`, the simulator crate later renamed `vike-backtest`)
//! - `core/order_intent.py` → [`orders::order::{OrderRequest, OrderResting::from_request}`]
//! - `exec/events.py`       → [`events`] (the full event union)
//! - `data/instrument_db.py` (filter row shape) → [`instrument::SymbolProperties`]
//!
//! Plus the [`time::clock::Clock`] trait (Test/Live), the backtest=live seam.
//!
//! The market-data vocabulary (`Bar`, the tick types, `L2Book`, `L2BookUpdate`, `OptionKind`, the
//! consolidator) is `vike-marketdata`'s, and this crate does NOT re-export it: its own signatures
//! name `vike_marketdata::X`, and so does every consumer (one name per symbol; decision 0067's
//! amendment).
//!
//! PARITY RULES (apply to every file in this crate + vike-simulator/vike-execution):
//! - f64 end-to-end, expressions in the SAME order as Python; no `mul_add`, no fast-math.
//! - Exact-zero flat checks stay `== 0.0` where Python has `== 0.0`.
//! - Timestamps are epoch-ms i64 (live events may ADD a recv_ns stamp later; never replace ms).

#![warn(unreachable_pub)]

// Account identity: the multi-account credential-key grammar and the account-tier vocabulary.
pub mod accounts;
pub mod diagnostic;
// Where an event stood in a gateway's per-account event ring: the ONE cursor the lease wire, the journal and
// the client's duplicate guard share. Re-exported at this root as `EventRingCursor`.
mod event_ring_cursor;
pub mod events;
pub mod fair;
pub mod feed_status;
pub mod finite;
pub mod host_build;
pub mod instrument;
/// The SHARED text parser behind every crate's `libm_platform_probe` (decision 0074). Gated the
/// same way as `strategy::OrderBrokerMock` — a default build compiles none of it — because it is test
/// machinery that eleven crates reach as a dev-dependency feature, not production vocabulary.
/// Its own planted-fixture self-test deliberately lives in `tests/`; the module doc says why.
#[cfg(any(test, feature = "test-support"))]
pub mod libm_walk;
pub mod market_slippage;
// Positions and money: cost basis, equity, margin and liquidation, sizing and fees.
pub mod money;
// Order vocabulary: the request, its client id, how it triggers and fills against a bar, its
// barriers, and whether HALT admits it.
pub mod orders;
pub mod params;
pub mod pysum;
pub mod rate_limits;
// The pre-trade risk CONFIGURATION: the gate's limits and the run profile's `[risk]` table (the
// gate itself stays in vike-execution). Named at this root; `risk::surface` is the one public child.
pub mod risk;
pub mod run_request;
pub mod scalar;
/// The pure SOURCE SCANNER behind the settings-registry gate and every other gate that reads Rust text
/// (`env::var` sites, map lookups, named calls, comment stripping). It was `vike_ops::scan` until
/// 2026-10-08; it moved here so a gate living in ANY crate can take it as a dev-dependency without
/// taking `vike-ops`. Gated exactly like `libm_walk` above, for the same reason: test machinery with
/// no production caller, reached as a dev-dependency feature.
#[cfg(any(test, feature = "test-support"))]
pub mod scan;
// A value that never prints: `Secret<T>`, redacting `Debug`, no `Display`, no `Serialize`.
pub mod secret;
// The store-plane classifier: whether a store kind holds market data or an account's data. Its
// names are spelled from this root (`StorePlane`, `ACCOUNT_KINDS`, `plane_of`, `is_account_kind`).
pub mod store_plane;
pub mod strategy;
// The live-stream vocabulary, every name re-exported at this root: the stream-health disclosure every
// layer speaks (`StreamStatus`: the health state machine, the data seam, the datahub market-data wire),
// how far behind the market a stream is (`StreamDelay`: a label, never a gate, decision 0140) and the
// tick-lane words a stream is named by (`StreamLabel`, `TAPE_HOLE_WORD`).
mod stream;
/// Test helpers several crates' tests used to copy — gated exactly like `libm_walk` above, for the
/// same reason: test machinery reached as a dev-dependency feature, never production vocabulary.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
// Time: the UTC epoch/label conversions over `vike_utility::calendar` (its own items), the `Clock`
// seam and the `TimeRule` firing rule.
pub mod time;
// The venue ROSTER (`VENUES`, `roster_id`), what a venue's adapter declares and what its startup
// probes answer, and what a venue tells us: the reconcile report rows and the position words beside
// them. The per-venue tables that iterate the roster are `vike-venues`' (decision 0162).
pub mod venues;

pub use accounts::arming::ArmingBlock;
pub use accounts::book_id::BookId;
pub use accounts::link_session::LinkSessionRow;
pub use accounts::tier::{AccountTier, AccountTierError};
pub use accounts::venue_network::VenueNetwork;
pub use diagnostic::{Diagnostic, DiagnosticSeverity};
pub use event_ring_cursor::EventRingCursor;
pub use fair::p_up;
pub use finite::FiniteNumbers;
pub use instrument::SymbolProperties;
pub use instrument::asset_class::AssetClass;
pub use instrument::contract::{
    Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
    ContractCombo, ContractComboKind, ContractComboLeg, ContractEquity, ContractFuture,
    ContractFutureDelivery, ContractFutureFamily, ContractFutureTenor, ContractFx, ContractOption,
    ContractOptionExerciseStyle, ContractPerp, ContractPredictionOutcome, Margining,
};
pub use instrument::record::{Instrument, InstrumentVenueIds};
pub use instrument::tick_scheme::{
    GridTickScheme, GridTickTier, MAX_GRID_TICK_TIERS, TickSchemeError, round_price_tiered,
};
pub use money::contract_value::{ContractValue, NotionalKind};
pub use money::currency::{
    AccountPerQuote, AccountPerQuoteLeg, QuoteRate, QuoteRateError, currency_code, same_currency,
};
pub use money::equity::EquitySample;
pub use money::fees::{FeeSchedule, OrderFees, maker_round_trip_fee, xemm_round_trip_fee};
pub use money::fill::{
    FillKind, FillOutcome, TradeFold, TradeFoldClose, TradeFoldStep, compute_fill,
    compute_fill_value,
};
pub use money::ledger::{AccountLedgerBlock, LedgerSums, ledger_sums};
pub use money::liquidation::{
    LiquidationCandidate, LiquidationPools, cross_liquidation_plan, partition_pools, pool_breached,
};
pub use money::lot::{LOT_DUST, Lot, LotClose, close_lots_fifo};
pub use money::margin::{
    amount_to_order, clamp_leverage, free_buying_power, has_sufficient_margin, initial_margin,
    liquidation_price, maintenance_margin,
};
pub use money::margin_mode::{MarginMode, MarginModeSwitch};
pub use money::position::{Fill, Position, Trade};
pub use money::trade_closed::TradeClosedRow;
pub use orders::cancel::OrderCancelIntent;
pub use orders::client_id::origin::{OrderClientIdOrigin, OrderClientIdOriginError};
pub use orders::client_id::{
    is_valid_crypto_order_client_id, retired_order_client_id_key, retired_order_client_id_refusal,
};
pub use orders::drop::OrderDrop;
pub use orders::fill_trigger::{
    apply_stop_adjust, limit_part, one_price_bar, order_fill_price, order_fill_price_granular,
    trailing_activation_reached,
};
pub use orders::halt_admit::{
    ArmingHaltAdmit, HaltAdmit, HaltVerify, effective_halt_admit, halt_admit_arming,
    halt_verify_support,
};
pub use orders::order::{
    ComboError, ContingencyType, ExerciseSpec, LadderSpec, LadderTarget, OrderAttributes,
    OrderBracket, OrderCombo, OrderComboLeg, OrderKind, OrderList, OrderListLeg, OrderPeg,
    OrderPegAnchor, OrderRelease, OrderReleasePrice, OrderReleasePriceSide, OrderRequest,
    OrderResting, OrderStopAdjust, OrderStopAdjustTo, OrderType, OrderTypeError, TimeInForce,
    TimeInForceError, TriggerBy, attributes_refusal, build_bracket, build_combo, build_ladder,
    build_order_list, combo_legs_cross, combo_net, combo_net_cross, combo_net_from_legs,
    display_refusal, lifetime_refusal, order_terms_refusal, price_source_refusal, side_from_word,
    stop_adjust_refusal, time_in_force_expired,
};
pub use orders::order_side::OrderSide;
pub use orders::own_order::{
    OrderBookOwn, OrderBookOwnEntry, OrderBookOwnQuantityFilter, OrderBookOwnSide,
    OrderBookOwnStatus, OrderBookOwnStatusMask,
};
pub use orders::placement::OrderPlacement;
pub use orders::record::{
    OrderRecord, OrderStatus, OrderTransitionError, modified_order, modified_price_is_trigger,
    modified_prices,
};
pub use pysum::py_sum;
pub use rate_limits::{DEFAULT_UTILIZATION, MAX_UTILIZATION, MIN_UTILIZATION, RateLimitConfig};
pub use risk::dead_man::DeadManAction;
pub use risk::grid::{GridFetch, GridSource};
pub use risk::limits::{Grid, PriceCollar, RiskLimits, SymbolGrid};
pub use risk::margin_call::MarginCallConfig;
pub use risk::profile::{ProfileRisk, ProfileRiskError};
pub use risk::trading_state::TradingState;
pub use scalar::{
    closing_side, floor_to_step, gross_notional, is_covered_reduce, is_reducing_direction, nz_step,
    order_notional, round_to, round_to_step, share_quantity, signed_notional, snap_limit,
    snap_stop,
};
pub use store_plane::{ACCOUNT_KINDS, StorePlane, is_account_kind, plane_of};
pub use strategy::triple_barrier::{
    PositionControllerParams, TripleBarrier, TripleBarrierHit, TripleBarrierKind,
    evaluate_barriers, evaluate_barriers_at_price, stop_loss_price, take_profit_price,
    time_barrier_hit,
};
pub use strategy::{
    AsParams, FeedStatus, FlowToxicity, HorizonMode, KappaMode, LadderLevel, LadderOffsetUnit,
    LadderParams, LadderSizeProfile, OrderBroker, OrderEvent, OrderEventKind, PriceDomain,
    QuoteStyle, RESERVED_SOURCE_KEY, RefreshTolerance, ReservationModel, RewardParams,
    SpreadMakerParams, SpreadModel, SpreadSource, Strategy, StrategyParams, TickMark,
    ToxicityParams, VarianceMode, XemmParams, tag_busy_reason,
};
pub use stream::delay::StreamDelay;
pub use stream::label::{StreamLabel, TAPE_HOLE_WORD};
pub use stream::status::StreamStatus;
pub use time::clock::{Clock, ClockLive, ClockTest, now_ms, now_ms_u64, now_ns, now_us};
pub use time::{
    MS_PER_DAY, epoch_ns_to_utc_date, parse_date_label, parse_utc_timestamp_ms, parse_ymd, utc_day,
    utc_weekday,
};
pub use venues::VENUES;
pub use venues::declaration::{
    ClockDeclarationKind, ClockRisk, VenueAdapterDeclarationView, Withhold,
};
pub use venues::position::{PositionLeg, PositionMode, PositionPrediction, PositionRows};
pub use venues::preflight::{CredentialGap, NetReading, ServerTimeGap};
pub use venues::read::{
    MAX_VENUE_OPEN_ORDERS, MAX_VENUE_POSITIONS, VenueRead, VenueReadAllowance,
    VenueReadAssetBalance, VenueReadBalance, VenueReadKind, VenueReadOpenOrder, VenueReadPosition,
    not_offered_words, open_orders_read,
};
pub use venues::report::{FillReport, OrderStatusReport, PositionStatusReport};

/// Decode a fixture f64 encoded as a hex bit-pattern string (e.g. "3ff0000000000000").
/// Golden fixtures carry floats as `f64::to_bits` hex so last-ulp divergences are unambiguous.
pub fn f64_from_hex_bits(s: &str) -> Result<f64, std::num::ParseIntError> {
    Ok(f64::from_bits(u64::from_str_radix(s, 16)?))
}

/// Encode an f64 as its hex bit-pattern string (the fixture wire format).
pub fn f64_to_hex_bits(x: f64) -> String {
    format!("{:016x}", x.to_bits())
}
