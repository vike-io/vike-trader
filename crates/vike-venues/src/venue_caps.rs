//! `VenueCapabilities` — the STATIC per-venue capability matrix (audit br6).
//!
//! Today a venue's capabilities are only ever discovered at RUNTIME: [`vike_model::orders::order::OrderRequest`]
//! modify/batch land on silent `ExecutionClient` trait defaults, live-data verbs return
//! `DataClientError::Unsupported`, and the GUI sends `Command::Modify` unconditionally — so an
//! unsupported action becomes a post-hoc reject. This module makes each adapter's real capabilities
//! a DECLARED, compile-time constant a caller can consult BEFORE acting.
//!
//! ## Where this lives and why
//! The struct + the per-venue values live in `vike-model` (not `vike-bridge-core`) deliberately:
//! - `VenueCapabilities` is a pure data descriptor with no I/O — exactly the shape of everything else in
//!   this crate, and its `supported_tifs` field is over [`vike_model::orders::order::TimeInForce`] which is
//!   already here.
//! - `vike-model` is the bottom layer every other crate already depends on, so the GUI shell
//!   (`vike-desktop` + `vike-app-core`), the chart widget (`vike-chart`) and the Trade window's
//!   widget (`vike-order-entry`), the venue bridges, and `vike-backfill` can ALL reach the same values
//!   with **no new dependency and no layering inversion**. A registry that instead lived in a
//!   bridge crate could not be reached by `vike-chart`; one that lived in `vike-bridge-core` could
//!   not be reached by `vike-chart` either (it depends on `vike-execution`, which `vike-chart` must not
//!   pull in).
//!
//! Each bridge crate RE-EXPORTS its own row as `pub const CAPABILITIES` (e.g. `vike_binance::CAPABILITIES`) so the
//! declaration is discoverable next to the adapter.
//!
//! ## ⚠ What a bridge's `capabilities_test` actually proves — read before trusting one
//!
//! This paragraph used to claim the bridges "are the source of the TRUTH those values are tested
//! against". **They are not, and never were.** All 14 `capabilities_test` modules open with
//! `assert_eq!(super::CAPABILITIES, vike_venues::capabilities_for("<venue>"))`, where `CAPABILITIES` is DEFINED as that same
//! const — the equality holds by construction, and the detour through the non-const `capabilities_for`
//! exists only to dodge `clippy::assertions_on_constants` (`crates/bridges/binance/src/lib.rs` says
//! so in as many words). Every assertion after it compares fields of that same constant against
//! literals, with prose citing the adapter.
//!
//! So a `capabilities_test` is a SECOND, co-located restatement of this table plus one human's reading of
//! the adapter. That is worth something — it pins a row against a careless edit and records the
//! reasoning next to the code — but it does not execute the adapter, so it cannot catch a row that
//! was wrong when written, nor one that went stale when the adapter changed underneath it. Both
//! happened: IBKR's `live_data` and `backfill_bars` were written 2026-07-14 and contradicted the
//! NEXT DAY by #306 (the real five-verb `DataClient`) and #311 (the real bar collector), and stayed
//! wrong for three weeks with every `capabilities_test` green.
//!
//! The checks that DO tie a row to something outside this table — the pattern to extend:
//! - `crates/bridges/deribit/src/execution/execution_tests.rs`'s `modify_answers_with_an_event_and_the_capabilities_say_it_is_supported` —
//!   CALLS the real `rest.modify_order`, asserts it answers with an event (never `[]`) without touching
//!   the socket, AND asserts `capabilities_for("deribit").supports_modify`, in one test.
//! - `crates/bridges/hyperliquid/src/execution/execution_tests.rs`'s `stop_limit_kind_is_declared_and_built` — puts a
//!   real `"stop_limit"` [`OrderRequest`] through `build_order_wire` and asserts BOTH the wire shape
//!   it produces and that this row declares the kind.
//! - the two backfill axes: `vike_backfill::capabilities::backfill_capabilities` READS them as its venue-direct row,
//!   so `crates/vike-backfill/src/capabilities_tests.rs`'s `a_venue_module_that_exists_must_not_claim_it_serves_nothing`
//!   (a filesystem walk) and `a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes` (a source scan) judge
//!   them directly.
//! - `crates/vike-venues/src/venue_time_in_force_tests.rs`'s `venue_capabilities_cross_pin_the_time_in_force_table` (the TIF
//!   axes) and this module's own `margin_modes_cross_pin_venue_margin_support` (the margin axis).
//! - [`WHOLE_EXIT_VENUES`]' three bridge tests (its doc names them): each finds its row's
//!   `bridge_words` in what that bridge's adapter or sidecar says.
//!
//! ## Honesty rule
//! Every field reflects what the ADAPTER wires TODAY, not what the exchange could theoretically do.
//! Polymarket's CLOB has a post-only flag, but no order was ever placed there from a build that sends
//! it (a live money venue), so `POLYMARKET.supported_tifs` does not list `PostOnly`; the venues whose
//! rows do list it (binance, bybit, okx, deribit, hyperliquid) each map it on the wire. Where
//! a capability could not be verified from Rust (e.g. whether the Dukascopy Java sidecar honors
//! reduce-only), it is declared conservatively `false`.

use vike_model::AccountTier;
use vike_model::money::margin_mode::MarginMode;
use vike_model::orders::order::{ContingencyType, OrderRequest, OrderType, TimeInForce};

use crate::venue_amend_mode::{AmendMode, amend_mode_for};

mod types;
pub use types::{DataClientCapabilities, DataClientVerb, TriggerType, VenueCapabilities};

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each cites the adapter code the value was read from (audit br6 inspection).
// ---------------------------------------------------------------------------------------------

/// Binance USDS-M futures (`vike_binance::execution::rest::perp::VenueRestBinancePerp` via `ExecutionClientRest`). Native
/// modify (`PUT /fapi/v1/order`) + native batch (`/fapi/v1/batchOrders`) + `reduceOnly`, all wired
/// through `VenueRest`. TIF FLIPPED (tif step-2, demo-smoke-proven): the family builder maps
/// GTC/IOC/FOK 1:1 on LIMIT; Day is loud-denied at submit (`deny_unsupported_time_in_force`); GTD is
/// LANE-SPLIT — the PERP lane wires native fapi GTD (`timeInForce=GTD` + mandatory
/// `goodTillDate` from `gtd_expiry`; the `venue_time_in_force` `"binance-perp"` lane sub-key, gated by
/// `vike_binance::execution::rest::perp::deny_invalid_gtd`) while the spot `/api/v3` has no GTD at all and keeps
/// the loud deny. This registry is keyed by the ONE venue id `"binance"` for BOTH lanes, so
/// `supported_tifs` stays the CONSERVATIVE lane intersection (no `Gtd`): a caller consulting it
/// is never led to offer GTD on the lane that must refuse it. Per-lane TIF truth lives in
/// [`crate::venue_time_in_force::venue_time_in_force`] (`"binance"` = spot, `"binance-perp"` = perp). `PostOnly` is on
/// BOTH lanes (perp `timeInForce=GTX`, spot `type=LIMIT_MAKER`), so it is in the intersection
/// too and `supports_post_only` is true. Live
/// feed serves bars + `@aggTrade` trades + `@depth20` depth (not quotes/book). Kline backfill in
/// `vike-backfill`.
/// Expanded axes: `accepted_tifs` adds `Gtd` (the lane UNION — the perp lane wires native fapi
/// GTD, so the core preflight must not refuse it; the spot lane's `deny_unsupported_time_in_force` still
/// loud-denies it there). Order kinds are the lane union too. On the perp lane `"stop"` and
/// `"stop_limit"` are Algo Service conditionals (`crates/bridges/binance/src/execution/rest/algo.rs`'s
/// `build_perp_algo_params`: `type=STOP_MARKET` with `triggerPrice`, or `type=STOP` with the limit
/// `price` and `timeInForce=GTC`; `is_algo_kind` routes both kinds there, decision 0186). On spot
/// they are `STOP_LOSS` and `STOP_LOSS_LIMIT` (`stopPrice`, and `price` with `timeInForce=GTC` for
/// the limit), built in binance's own spot client (`crates/bridges/binance/src/execution/rest/spot.rs`'s
/// `VenueRestBinanceSpot::build_order_params`), never in the `family/` builder aster shares. A stop-limit
/// asking another lifetime than GTC is refused by name on both lanes
/// (`crates/bridges/binance/src/execution/rest/perp.rs`'s `deny_unbuilt_kind`). `"take_profit"` is perp
/// `TAKE_PROFIT_MARKET`, or `TAKE_PROFIT` with its limit and GTC, on the same Algo Service
/// (`crates/bridges/binance/src/execution/rest/algo.rs`'s `build_perp_algo_params`), and spot `TAKE_PROFIT` /
/// `TAKE_PROFIT_LIMIT`; a take-profit's trigger is a `TakeProfit` trigger. `supports_display_quantity` is the
/// spot iceberg (`icebergQty` on a GTC `LIMIT`, `STOP_LOSS_LIMIT` or `TAKE_PROFIT_LIMIT`), refused by name
/// on any other spot shape and on the WHOLE perpetual, which has no visible size (`perp.rs`'s
/// `iceberg_refusal`): the lane union, as with GTD. Flipped behind the demo pass of
/// `crates/bridges/binance/tests/binance_smokes/take_profit_smoke.rs`.
/// `crates/bridges/binance/src/lib.rs`'s `stop_limit_kind_is_declared_and_built` and
/// `take_profit_and_iceberg_are_built_on_their_lanes` drive both lanes' real submit against this row.
/// A resting stop, stop-limit or take-profit is not modified in place (`algo.rs`'s `MODIFY_REFUSAL`;
/// spot amends a `LIMIT` only): `venue_amend_mode`'s `NO_NATIVE_KIND` makes all three cancel-then-new.
/// `margin_modes: &[Cross]` — the order builder never reads `margin_mode` (only the account
/// default, cross, is honored). `max_batch: 5` = the fapi batchOrders chunk cap (`BATCH_MAX`).
/// Reduce-only: every kind carries the flag onto the wire (`family/order_map.rs`'s
/// `build_perp_order_params` for market and limit, `algo.rs`'s `build_perp_algo_params` for the
/// stop, the stop-limit and the take-profit) and the adapter refuses none by name, so
/// `reduce_only_kinds` is the kind set.
/// The two trailing kinds are refused by name on both lanes (`TRIGGER_UNSUPPORTED` here, and
/// `crates/bridges/binance/src/execution/rest/perp.rs`'s `deny_unbuilt_kind` behind it): the venue trails by a PERCENT on
/// the perpetual (the algo `TRAILING_STOP_MARKET`'s `callbackRate`) and in basis points on spot
/// (`trailingDelta`), never by a price, and a percent and a price are different orders once the market
/// moves, so neither is built. Spot's trailing `STOP_LOSS_LIMIT` rests a FIXED limit price, not an offset
/// from the stop, so it is no `trailing_limit` either.
/// `percent_trail_kinds` (the attributes wave) is `"trailing"` by a PERCENT on both lanes: spot `STOP_LOSS`
/// with `trailingDelta` (the percent in whole basis points; the symbol's `TRAILING_DELTA` filter is the
/// venue's to judge), or with an activation `TAKE_PROFIT` with `stopPrice` (`crates/bridges/binance/src/execution/rest/spot.rs`),
/// the perpetual the Algo Service's `TRAILING_STOP_MARKET` with `callbackRate` and `activatePrice`
/// (`crates/bridges/binance/src/execution/rest/algo.rs`), reduce-only on the perpetual, read back as a resting `Trailing` by
/// both lanes' recon. FLIPPED behind both lanes' demo smokes in
/// `crates/bridges/binance/tests/binance_smokes/trailing_percent_smoke.rs` (`perp_percent_trailing_on_the_demo_algo_service`,
/// `spot_percent_trailing_on_the_demo_host`): smoke green 2026-11-09, both lanes (spot: a BUY `STOP_LOSS`
/// delta 1000 and a BUY and a SELL `TAKE_PROFIT` armed, delta 500, read back as live trailing orders, modify
/// refused, cancels confirmed; the venue refuses a delta of 5, "Filter failure: TRAILING_DELTA", and a BUY
/// armed above the market, "Stop price would trigger immediately."). The row is the lane union, so it
/// opened only when BOTH passed. The price trail and `trailing_limit` stay refused by name, and a resting
/// one is never modified.
pub const BINANCE: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: true,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::PostOnly,
    ],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    reduce_only_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: true,
    supports_display_quantity: true,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &["trailing"],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 5,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: false,
        trades: true,
        book: false,
        depth: true,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Bybit V5 linear perp (the shared `ExecutionClientActor`). Native amend
/// (`/v5/order/amend`, `ExecutionClientActorCommand::Modify`) + `reduceOnly`; batch is NOT wired at the client
/// seam (falls through to the fan-out default, though `VenueRestBybit` has the methods). TIF
/// FLIPPED (tif step-2, demo-smoke-proven): GTC/IOC/FOK map 1:1 on Limit; GTD/Day are
/// loud-denied at submit (`deny_unsupported_time_in_force`). Live feed serves bars + `publicTrade` trades
/// (live-verified 2026-07-20, `bybit_trades_feed_smoke`) + `orderbook.200` depth. Kline backfill
/// in `vike-backfill`.
/// Expanded axes: `"stop"` is a native conditional (`build_order_params` wires
/// `triggerPrice`/`triggerDirection`/`triggerBy:"LastPrice"`, Market-on-trigger), and `"stop_limit"`
/// the same conditional firing as a `Limit` at its `price` (`crates/bridges/bybit/src/execution/rest/mod.rs`'s
/// `build_order_params`). On a spot pair the stop-limit is the spot book's own conditional
/// (`orderFilter=StopOrder` with `triggerPrice`, no direction and no source; its demo smoke
/// `bybit_spot_conditional_smoke` is written and not run) and a stop is
/// refused whole, by name (`spot_stop_refusal`): the row is per venue, not per book. The stop-limit
/// flipped behind its demo smoke
/// (`crates/bridges/bybit/tests/bybit_smokes/bybit_stop_limit_smoke.rs`, green on the CI box's bybit demo
/// account 2026-11-08: placed, listed back as a live `StopLimit`, its limit amended in place with the
/// trigger left where it was, cancelled). `"take_profit"` is the stop's conditional firing the other
/// way (a BUY on a fall, a SELL on a rise; a `Limit` at its `price` or a `Market`), a `TakeProfit`
/// trigger, flipped behind the demo pass of
/// `crates/bridges/bybit/tests/bybit_smokes/bybit_take_profit_smoke.rs` (`unbuilt_kind_refusal` is the
/// bridge's floor under this row). A stop-limit's and a take-profit's trigger move in place on
/// `/v5/order/amend` with a `triggerPrice` (`crates/bridges/bybit/src/execution/rest/mod.rs`'s `amend`, through
/// `modify_trigger`), flipped behind
/// `crates/bridges/bybit/tests/bybit_smokes/bybit_trigger_amend_smoke.rs`, and a stop's change goes
/// out as that `triggerPrice` too (`modify_order`), so every bybit kind amends in place. TIF admit set == the
/// honored trio (GTD/Day are already loud-denied venue-side; the preflight only moves that
/// earlier). `margin_mode` is never read (UTA account-level mode) → `&[Cross]`. The V5 batch
/// endpoints exist on `VenueRestBybit` but are NOT wired at the `ExecutionClient` seam →
/// `max_batch: 0`, consistent with `supports_native_batch: false`.
/// Reduce-only: `build_order_params` writes `reduceOnly` on every order, the stop included, and the
/// adapter refuses none by name. `reduce_only_kinds` is the kind set but for `"stop_limit"` and
/// `"take_profit"`: the flag rides their wire as it rides the stop's, but no reduce-only one was ever
/// placed (the take-profit smoke places none either; the demo
/// account was flat for the smoke) and the venue's own page does not say the flag holds for a
/// conditional `Limit` (https://bybit-exchange.github.io/docs/v5/order/create-order, `reduceOnly`:
/// "Valid for `linear`, `inverse` & `option`", nothing per conditional kind), so a reduce-only one is
/// refused at the core edge until a demo placement proves it (`venue_capabilities_tests.rs`'s
/// `REDUCE_ONLY_EXCEPTIONS`). The reduce-only stop (a conditional `Market`) was recorded on the demo host.
pub const BYBIT: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
    accepted_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    reduce_only_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: true,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: false,
        trades: true,
        book: false,
        depth: true,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// OKX SWAP perp (the shared `ExecutionClientActor`). Native amend
/// (`ExecutionClientActorCommand::Modify` → `VenueRestOkx::modify_order`) + `reduceOnly`; batch not wired at the
/// client seam (fan-out default). TIF FLIPPED (tif step-2, demo-smoke-proven): OKX carries TIF on
/// `ordType` — GTC stays the un-emitted venue default (`ordType:"limit"`, byte-identical);
/// IOC/FOK map to the `ioc`/`fok` ordTypes; GTD/Day are loud-denied at submit
/// (`deny_unsupported_time_in_force`). Live feed serves bars + `trades` prints (live-verified 2026-07-20,
/// `okx_trades_feed_smoke`) + `books` depth. Kline backfill in `vike-backfill`.
/// Expanded axes: `"stop"` routes to the native ALGO conditional (`submit_stop_algo` —
/// `/trade/order-algo`, `slTriggerPx` from `trigger_price`, fires as market: `slOrdPx=-1`), and
/// `"stop_limit"` to the same conditional with `slOrdPx` the limit `price`
/// (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `submit_stop_algo`; the recon read maps a priced
/// `slOrdPx` back to the stop-limit, `crates/bridges/okx/src/reconcile/mod.rs`'s `parse_algo_pending`).
/// A resting algo order is amended in place on `amend-algos` by its `algoClOrdId`, its trigger too
/// (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `amend_algo`; `VenueRestOkx`'s `modify_order` and
/// `modify_trigger`), flipped behind the demo pass of
/// `crates/bridges/okx/tests/okx_smokes/okx_algo_amend_smoke.rs`: no `venue_amend_mode` exception. The
/// order endpoint takes market and limit only: `crates/bridges/okx/src/execution/rest/mod.rs`'s `build_order_params_in`
/// refuses every other kind by name (`"market_to_limit"`, an unrecognised word, a trigger kind through
/// the batch), never as the `ordType:"market"` it once fell into. `"take_profit"` is the same
/// conditional algo order's take-profit side (`tpTriggerPx`, `tpOrdPx` the limit or `-1`;
/// `crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `build_stop_algo_params`, read back by
/// `crates/bridges/okx/src/reconcile/mod.rs`'s `conditional_kind`), a `TakeProfit` trigger, flipped behind
/// the demo pass of `crates/bridges/okx/tests/okx_smokes/okx_take_profit_smoke.rs`.
/// THE margin venue: `swap_td_mode`
/// honors a per-order `margin_mode` — `Cross` (and unset) → `tdMode:"cross"`, `Isolated` →
/// `"isolated"`, `Cash` denied venue-side (spot-only mode; the preflight moves that deny
/// earlier) → `margin_modes: &[Cross, Isolated]`. Batch endpoints exist on `VenueRestOkx` but
/// are not wired at the client seam → `max_batch: 0`.
/// Reduce-only: the order builder (`perp.rs`) and the stop algo (`perp/stop_algo.rs`, the stop, the
/// stop-limit and the take-profit) both write `reduceOnly`, and the adapter refuses none by name →
/// `reduce_only_kinds` is the kind set.
/// `"trailing"` is the venue's `move_order_stop` algo order (`callbackSpread` the trail in price,
/// `activePx` the activation; `crates/bridges/okx/src/execution/rest/stop_algo.rs`), listed since its demo smoke
/// (`crates/bridges/okx/tests/okx_smokes/okx_trailing_smoke.rs`) ran green on the demo host, reduce-only
/// too (the algo writes `reduceOnly`), and read back by the recon as a resting `Trailing`. A resting one
/// is never modified (`venue_amend_mode`'s `amend_mode` refuses every trailing amend). `"trailing_limit"`
/// stays refused: the venue's trailing stop fires as a market order.
/// `percent_trail_kinds` (the attributes wave) is `"trailing"` by a PERCENT: the same `move_order_stop`
/// with `callbackRatio` (the percent / 100) in place of `callbackSpread`, `activePx` the activation, reduce-only
/// too (`crates/bridges/okx/src/execution/rest/stop_algo.rs`), read back as the same resting `Trailing`, FLIPPED behind
/// the percent row of `crates/bridges/okx/tests/okx_smokes/okx_trailing_smoke.rs`: smoke green 2026-11-09 on
/// the demo (3 of 3, `callbackRatio` "0.015" read back). A percent `"trailing_limit"` stays refused like the
/// price one; the ratio's bounds and whether the venue trails from the extreme since activation are unmeasured.
pub const OKX: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
    accepted_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit", "take_profit", "trailing"],
    reduce_only_kinds: &["market", "limit", "stop", "stop_limit", "take_profit", "trailing"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: true,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &["trailing"],
    margin_modes: &[MarginMode::Cross, MarginMode::Isolated],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: false,
        trades: true,
        book: false,
        depth: true,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Aster DEX (the shared `ExecutionClientActor`). USDⓈ-M perp + spot; a Binance-fork API.
/// `VenueRestAsterPerp` overrides `modify_order` (`PUT /fapi/v3/order`) + native batch
/// (`POST /fapi/v3/batchOrders`, ≤5) and builds `reduceOnly`. TIF defaults GTC. Live data
/// (`crates/bridges/aster/src/feed/mod.rs`'s `DataClientSpecAster` on the shared `DataClientWebsocket`)
/// serves bars/trades/depth; L1 quotes + lossless L2 book run through the separate HFT tick track.
/// Expanded axes: shares binance's family builders, so the kinds match binance (perp
/// STOP_MARKET stop; `take_profit` → the MARKET fallthrough → refused). TIF is still the
/// unflipped `Ignored{GTC}` row — only a GTC request matches what actually rests, so
/// `accepted_tifs: &[Gtc]`: a non-GTC limit is preflight-refused instead of silently resting
/// GTC (the loud-deny flip this axis exists for). `margin_mode` never read → `&[Cross]`;
/// `max_batch: 5` = the `/fapi/v3/batchOrders` chunk cap.
/// Reduce-only: the shared family builder (`build_perp_order_params`) writes `reduceOnly` on all
/// three kinds, and the adapter refuses none by name → `reduce_only_kinds` is the kind set.
/// The two trailing kinds are refused by name (`TRIGGER_UNSUPPORTED`): aster's `TRAILING_STOP_MARKET`
/// trails by a PERCENT (`callbackRate`), never by a price, and its builders are binance's family ones
/// (`crates/bridges/binance/src/family/`, decision 0186), which build no trailing order.
pub const ASTER: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: true,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop"],
    reduce_only_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 5,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: false,
        trades: true,
        book: false,
        depth: true,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Deribit options (`vike_deribit::execution::rest::VenueRestDeribit` via `ExecutionClientRest`). Implements
/// `VenueRest` and overrides `modify_order` (`private/edit`, an in-place edit of a resting LIMIT
/// order: `supports_modify: true`, moved together with `crate::venue_amend`'s `DERIBIT` row) but NOT
/// the batch methods → no native batch. Builds `reduce_only` on the JSON-RPC order. TIF FLIPPED (tif step-2,
/// demo-smoke-proven): Gtc stays the un-emitted venue default (byte-identical); Ioc/Fok/Day map
/// to `immediate_or_cancel`/`fill_or_kill`/`good_til_day` on the limit path; Gtd (no
/// good-till-DATE on Deribit) is loud-denied at submit. **The ONE combo-capable venue**:
/// `submit_order` resolves a non-empty `combo_legs` through the combo book
/// (`private/create_combo` + the orientation/scale solve in `vike_deribit::execution::combo`, then buy/sell
/// on the resolved combo id) — proven offline by the bridge's `combo_submit` tests and live by
/// its gated `deribit_combo_smoke` order lifecycle. Live `DataClient`: `DataClientWebsocket` over
/// `crates/bridges/deribit/src/feed/mod.rs`'s `DataClientSpecDeribit` (split-plane I9) serves
/// bars (`chart.trades.*` + REST seed), quotes
/// (`quote.*`), trades (`trades.*`) and the lossless folded book (`book.*`, the `change_id`
/// DeltaSync chain) and — since the datahub began serving this venue's DOM, 2026-10-04 — the
/// conflating DOM `depth` lane (`subscribe_depth`: the top 200 levels of that same fold, through
/// `DataClientSink::l2_snapshot`), so this venue declares BOTH L2 lanes.
/// `vike-backfill` DOES backfill bars — `vike_deribit::history::KlineSourceDeribit` (a
/// `KlineSource` impl moved into the bridge under docs/decisions/0094, one of
/// `crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`'s rows (the same record moved the registry to the datahub; decision 0164 on to the gatewayhub)) pages the keyless public `get_tradingview_chart_data` through
/// `klines::ingest_klines` →
/// `append_bars`, dispatched by the datahub's `Backfill` verb (this row said `false` until the
/// cross-pin below caught it; the collector landed in #1030 as a `deribit_backfill` program, which
/// docs/decisions/0094 has since deleted).
/// Expanded axes: the NATIVE trigger orders (`vike_deribit::execution::rest`'s `order_params_on`):
/// `"stop"` is `stop_market`, `"stop_limit"` is `stop_limit` (`price` is the limit), and
/// `"take_profit"` is `take_market` (`take_limit` when a `price` is set), each with
/// `trigger_price` on the instrument's tick grid and `trigger` from the order's `trigger_by`
/// (`vike_bridge_core::trigger::venue_trigger_by`'s deribit row; `None` is `last_price`). The
/// venue holds the trigger, so a stop survives the node. Futures and perpetuals only: the venue
/// refuses a trigger order on an option, and the bridge refuses it first by name
/// (`trigger_refusal`). Before the stops were wired every non-limit kind was wire-`"market"`, so
/// a `"stop"` would have fired IMMEDIATELY instead of resting; the trigger kinds were refused
/// (`trigger_types: &[]`) until `order_params_on` built them. `post_only` is forced FALSE on the
/// wire except for a `PostOnly` order, which writes the pair `post_only` + `reject_post_only`
/// (a crossing order is refused, never re-priced). Margin is the fixed subaccount cross model →
/// `&[Cross]`.
/// Reduce-only: `order_params_on` writes `reduce_only` on every kind, trigger kinds included, so
/// `reduce_only_kinds` is the kind set. ⚠ The one refusal that remains is per SYMBOL, not per
/// kind: `trigger_refusal` turns a trigger order on an OPTION away by name, and a row cannot say
/// "a stop on a future but not on an option" (a bracket on an option is refused at the adapter).
/// `"trailing"` is BUILT as the venue's `trailing_stop` (`crates/bridges/deribit/src/execution/rest.rs`'s
/// `wire_type`: the trail as `trigger_offset` on the tick grid; the venue's trailing stop has no
/// activation, so one that states it is refused by name), listed since
/// `crates/bridges/deribit/tests/deribit_smokes/deribit_trailing_stop_testnet_smoke.rs` ran green on the
/// testnet, reduce-only too (`order_params_on` writes `reduce_only` on it as on every kind). A resting
/// one is never modified (`venue_amend_mode`'s `amend_mode` refuses every trailing amend).
/// `"trailing_limit"` is refused (no such venue order).
///
/// Two wave-2 axes are BUILT, each listed only once its testnet drill has run (wave 2 of the order
/// kinds runs no venue order;
/// `crates/bridges/deribit/tests/deribit_smokes/deribit_iceberg_and_market_limit_testnet_smoke.rs`):
///
/// `supports_display_quantity`: a `display_quantity` is the venue's `display_amount` on a plain limit, restated
/// on every edit and refused by name per order on an option, a combo, any other kind and a non-positive
/// slice (`crates/bridges/deribit/src/execution/rest.rs`'s `display_refusal`); LISTED, flipped behind
/// `deribit_iceberg_on_the_testnet`.
///
/// `"market_to_limit"` is the venue's `market_limit` (`wire_type`; reduce-only like every kind); LISTED,
/// flipped once `deribit_market_limit_on_the_testnet` showed the remainder resting as a limit at the
/// first fill's price, the plan of the order kinds' condition. Proved on a linear USDC perpetual (the
/// testnet's `ALT_USDC-PERPETUAL`, 2026-11-08: 186445 of 186446 filled, the 1-lot remainder resting
/// as a limit at the fill's price; the BTC inverse books were too deep to reach a remainder); the
/// remainder reads `order_type: "limit"`. A reduce-only market-to-limit was not exercised. Its resting
/// remainder is amended as the limit it is (`venue_amend_mode`'s `AmendKind::of`), in place by
/// `private/edit`.
pub const DERIBIT: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: true,
    supports_combo: true,
    supported_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Day,
        TimeInForce::PostOnly,
    ],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Day,
        TimeInForce::PostOnly,
    ],
    supported_order_kinds: &[
        "market",
        "limit",
        "stop",
        "stop_limit",
        "take_profit",
        "market_to_limit",
        "trailing",
    ],
    reduce_only_kinds: &[
        "market",
        "limit",
        "stop",
        "stop_limit",
        "take_profit",
        "market_to_limit",
        "trailing",
    ],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: true,
    supports_display_quantity: true,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: true,
        book: true,
        depth: true,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// OANDA v20 (the shared `ExecutionClientActor`). Wires submit/cancel and
/// modify; no batch. `supports_modify` is OANDA's order REPLACE (`PUT /orders/@{order_client_id}`, a cancel +
/// new order under a new venue id that keeps the client id), true since that override landed and
/// moved together with `crate::venue_amend`'s `OANDA` row (`CancelReplace`); only a resting limit or
/// stop can be replaced. No reduce-only on the wire. The RICH TIF
/// venue: `oanda_time_in_force` maps the full GTC/IOC/FOK/GTD/DAY set.
///
/// `backfill_bars: true` since the datahub's CREDENTIALED lane dispatches `vike_oanda::KlineSourceOanda`
/// (docs/decisions/0097): mid candles, fetched with the practice-tier token a datahub's own store
/// holds. It was `false` ("No `vike-backfill` backfill") until then; `backfill_ticks` stays `false`
/// because OANDA serves no tick history at all.
///
/// `live_data { bars: true, quotes: true }` — `vike_oanda::feed::Feeds` (the SPLIT-PLANE
/// feed) serves `subscribe_quotes` from the venue's chunked-HTTP pricing stream and
/// `subscribe_bars` by polling the candles REST endpoint (OANDA publishes candles no other way).
/// `subscribe_trades` (no trade tape published), `subscribe_book`/`subscribe_depth` (the pricing
/// ladder is an unsequenced top-of-book snapshot, not a delta-synced L2 book) all return
/// `Unsupported`, so those stay `false`.
///
/// Expanded axes: `build_order_body` wires `"limit"` → LIMIT and `"stop"` → native STOP
/// (`trigger_price` as the order level). A `"take_profit"` with no limit price is native
/// `MARKET_IF_TOUCHED` at its trigger, resting like a stop (`crates/bridges/oanda/src/execution/mod.rs`'s
/// `build_order_body_on`, `rests_at_venue`; the reconcile reads it back as a `TakeProfit`), a
/// `TakeProfit` trigger flipped behind the demo pass of
/// `crates/bridges/oanda/tests/oanda_smokes/oanda_demo_smoke.rs`'s
/// `oanda_demo_place_and_cancel_far_from_market_mit` (green 2026-11-09) and of
/// `crates/bridges/oanda/tests/oanda_smokes/oanda_reduce_only_smoke.rs`'s
/// `a_reduce_only_mit_larger_than_its_position_rests_saying_reduce_only` (a reduce-only take-profit must not
/// open a position; green 2026-11-09 on practice account 14). One WITH a limit (limit-if-touched) is refused by
/// name per order (`unbuilt_kind`: OANDA's `priceBound` cancels past the bound instead of resting a
/// limit), as is every other kind and an auction lifetime. The replace rebuilds a limit or a stop only
/// (`build_replace_body_on`; a take-profit's modify answers `OrderModifyRejected`), so
/// `venue_amend_mode`'s `NO_NATIVE_KIND` makes the take-profit's amend cancel-then-new.
/// `margin_mode` never read (shared FX account margin) →
/// `&[Cross]`. Reduce-only: every reduce-only order the adapter builds carries `positionFill: REDUCE_ONLY`, on
/// any account (`crates/bridges/oanda/src/execution/mod.rs`'s `REDUCE_ONLY_FILL`; the venue's answer on a resting one is
/// `crates/bridges/oanda/tests/oanda_smokes/oanda_reduce_only_smoke.rs`, green 2026-11-09), and it refuses no
/// kind for the flag, so `reduce_only_kinds` is the kind set.
pub const OANDA: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    supported_order_kinds: &["market", "limit", "stop", "take_profit"],
    reduce_only_kinds: &["market", "limit", "stop", "take_profit"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: false,
        book: false,
        depth: false,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Alpaca Broker API (US equities + crypto). Single pinned account. No native order modify
/// (submit/cancel only in Phase 1); TIFs the /v1/trading orders API accepts. Phase 2 (Task 11)
/// landed the market-data WS (`DataClientWebsocket` over `crates/bridges/alpaca/src/feed/client.rs`'s
/// `DataClientSpecAlpaca`) serving bars/quotes/trades — no
/// live L2 book/depth feed, so `book`/`depth` stay `false`.
/// Expanded axes: `build_order_body` maps `"limit"`/`"stop"`/`"stop_limit"` natively
/// (`stop_price` from `trigger_price`) — the one adapter wiring a `"stop_limit"` kind;
/// `"take_profit"` falls to the `_ => market` coercion → refused. `accepted_tifs` adds `Gtd`
/// (coerced to `gtc` today — live behavior, encoded as-accepted). `margin_mode` never read
/// (shared Reg-T account margin) → `&[Cross]`. Reduce-only: the adapter never reads the flag and
/// refuses nothing for it (a plain order goes out), so `reduce_only_kinds` is the kind set.
pub const ALPACA: VenueCapabilities = VenueCapabilities {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Day, TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    accepted_tifs: &[
        TimeInForce::Day,
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
    ],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit"],
    reduce_only_kinds: &["market", "limit", "stop", "stop_limit"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: true,
        book: false,
        depth: false,
    },
    backfill_bars: false,
    backfill_ticks: false,
};

/// IG (the shared `ExecutionClientActor`). Wires submit/cancel (positions +
/// working orders) and a PRICE-ONLY modify of a resting LIMIT/STOP (`PUT /workingorders/otc/{dealId}`
/// v2, answered through `/confirms` with status AMENDED → `OrderModified`; IG's body has no size field,
/// so a size change is an explicit `OrderModifyRejected`, never a silent no-op). No batch, no wire
/// reduce-only. TIF not mapped → effectively GTC. `backfill_bars: true` since the CREDENTIALED history
/// lane (`crates/bridges/ig/src/history/klines.rs`'s `KlineSourceIg`, a `KlineSource` the gatewayhub's `FetchCredentialed`
/// dispatches, decision 0194): mid bars of `GET /prices/{epic}`, a weekly allowance counted by the
/// gatewayhub. No ticks (`backfill_ticks` stays `false`): IG serves none as history.
/// LIVE DATA: `DataClientWebsocket` over `crates/bridges/ig/src/feed/mod.rs`'s `DataClientSpecIg`
/// (Lightstreamer TLCP) serves `bars` (`CHART:{epic}:
/// {scale}`, MERGE) and `quotes` (`PRICE:{accountId}:{epic}` on the `Pricing` data adapter, MERGE:
/// `crates/bridges/ig/src/feed/decode/mod.rs`'s `price_item`, decision 0191) — and NOTHING else,
/// structurally: IG's streaming API publishes the dealer's L1 BID/OFFER with no depth ladder behind it (`book` and
/// `depth` both false), and a dealer venue has no public trade tape (`trades` false).
/// Expanded axes: `build_request` wires `"limit"`/`"stop"` as working orders (`level` from
/// `price`/`trigger_price`) and everything else as a MARKET position open → `"take_profit"`
/// refused. TIF is never read (working orders hardcode `GOOD_TILL_CANCELLED`), so
/// `accepted_tifs: &[Gtc]` — a non-GTC limit is preflight-refused instead of silently resting
/// GTC (the loud-deny flip). `margin_mode` never read → `&[Cross]`.
/// **Reduce-only is a MARKET close and nothing else**: `crates/bridges/ig/src/execution/mod.rs` routes a
/// `reduce_only` market to the close endpoint (`/positions/otc` + `_method: DELETE`, by epic) and
/// terminally refuses a `reduce_only` limit or stop by name (`CLOSE_MARKET_ONLY`, measured on the
/// demo host: the close endpoint cannot rest an order). So `reduce_only_kinds` is `["market"]`:
/// a bracket's stop-loss and take-profit are both such orders, and a bracket on IG is refused
/// whole at the core edge instead of filling its entry and then having both exits refused.
/// An operator's own reduce-only STOP on IG is not refused: the CORE holds it (`vike-core`'s
/// `arm_core_held_stop` arms on this row's `PreflightDeny::ReduceOnly`, decision 0199's amendment of
/// 2026-10-27) and fires the reduce-only MARKET this row lists, capped to the position.
pub const IG: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop"],
    reduce_only_kinds: &["market"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: false,
        book: false,
        depth: false,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// FXCM ForexConnect (the shared `ExecutionClientActor`; behind the `fxcm`
/// feature). `ExecutionClientActorCommand::Modify` is a PRICE-ONLY amend of a resting entry (ForexConnect `EditOrder`,
/// `fcshim.cpp`'s `forex_connect_edit_order`, shim ABI 2): `OrderModified` or a non-terminal
/// `OrderModifyRejected`, a size change refused. A bound stop or take-profit (below) moves its price in
/// place too (the same venue order id, MEASURED 2026-11-09) and its SIZE only in whole trades: the bridge
/// adds a venue order on the oldest free trade or deletes one, and refuses a size inside a trade in words
/// (`ORA-20134`); no edit ever changes a venue order's amount (`crate::venue_amend_mode`'s module doc).
/// No batch, no wire reduce-only. Places a resting LIMIT
/// entry with no TIF → GTC. No live `DataClient`, no `vike-backfill` backfill.
/// Expanded axes: the exec loop reads `order_type` and splits on it — `"market"` places a TRUE
/// MARKET order (`fcshim.cpp`'s `forex_connect_place_market`, ForexConnect `O2G2::Orders::TrueMarketOpen`),
/// a limit places a resting LIMIT entry — AT the requested price when the request names one
/// (`place_limit_at`, ABI 2), else at the fixed `RESTING_PIPS` distance (`place_limit_entry`). Those
/// are the two kinds the adapter actually wires, hence exactly the two declared here. The earlier
/// `"limit"`-only row recorded a vike SHIM gap, not a venue one (ForexConnect always supported
/// market orders); the shim now has the placement, so `"market"` is declared again.
/// LIVE-VERIFIED (market-fill path only): the `fcshim.cpp:122` real-SDK build break
/// (`getInstrument` on `IO2GTradeRow`, which lives only on `IO2GTradeTableRow`) is fixed — the
/// base trade row's instrument is resolved via its offer id — and a demo market-order round-trip
/// (BUY → fill @ 1.14268 → net-flat SELL) is green on the FXCM demo (`vike-fxcm`
/// `tests/fxcm_smokes/fxcm_live_smoke.rs::fxcm_market_round_trip`). The phantom-cancel fix's other two
/// `Orders(Delete)` branches — async reject (`getStatus()=='R'`) and venue-initiated cancel
/// (`=='C'`) — follow the SDK's `OrderMonitor::onOrderDeleted` convention (not in the vendored
/// headers) and are STILL unexercised live. ⚠ **Attempted again on the CI box (2026-08-23, market open,
/// real demo orders) and this records WHICH CONDITION could not be produced, which is more useful
/// than "pending":**
/// * `'C'` — a resting limit entry was placed and cancelled (`fxcm_login_and_limit_cancel`, green,
///   venue order 237472952). Under `RUST_LOG=trace` **no** `map_drained_event` drop-warning
///   appeared, so nothing shows the shim emitted a `canceled` envelope at all within the run. Our
///   OWN cancel is the wrong instrument for this branch twice over: the synchronous
///   `forex_connect_delete_order` return already produced the terminal, `crates/bridges/fxcm/src/execution/mod.rs`'s
///   `drain_events` has already dropped the routing row by then (`closes_routing`), and the smoke
///   detaches immediately after. The branch exists for a cancel the VENUE initiates — order expiry,
///   a margin call, instrument suspension — and none of those is inducible on a demo account
///   holding $49,999 against one lot of the most liquid pair in the world.
/// * `'R'` — unreachable by placement: a rejected placement returns non-zero from `forex_connect_place`
///   SYNCHRONOUSLY and becomes an `OrderRejected` through `map_placement`. Reaching the async arm
///   needs an order the venue ACCEPTS and then rejects later, which is the same class of venue-side
///   condition as above and equally not inducible here.
///
/// A limit that names a `price` rests AT that rate (`forex_connect_place_entry_at`, shim ABI 2); the shim refuses
/// a rate off the instrument's price grid and the venue refuses one on the wrong side of the market,
/// each as a terminal `OrderRejected` carrying the reason. Until the ABI 2 shim there was no such
/// entry point (the only one took a pip distance from the quote), so `vike_fxcm::execution::event_mapper`'s
/// `preflight_request` refused every priced limit (#1470); a box still running an older shim now gets
/// a rejection naming the missing entry point and the remedy, never a rest at a price nobody chose.
/// An UNPRICED limit is unchanged and rests at the fixed `RESTING_PIPS` distance. A priced entry limit
/// RESTED on the demo (`fxcm_subscribe_smoke.rs`'s explicit-price limit, `docs/ops/fxcm-forexconnect.md`'s
/// "Subscribing an instrument") and the amend of a bound exit is live-proven (`fxcm_stop_smoke.rs`,
/// 2026-10-26 and 2026-11-09); the amend of a resting ENTRY limit is still unexercised on the demo.
/// TIF/`margin_mode` never read → `&[Gtc]`/`&[Cross]`.
///
/// ⚠ **WHAT AN OPERATOR ACCEPTS BY MOUNTING THIS VENUE.** Three properties this table has no field
/// for, each true today, each stated here because a caps row is where this workspace records what a
/// venue cannot do:
///
/// 1. **`qty` is BASE UNITS, and an inexact size is REFUSED.** ⚠ It was a LOT COUNT until the
///    conversion landed, which is the one that mattered: `fcshim.cpp` sets
///    `Amount = base_unit_size * lots`, so a caller sending the units it sends at every other venue
///    placed that many LOTS — `qty: 10000` on EUR/USD was ten thousand lots, a thousand times the
///    intended size, on a real account, with no event saying so. The shim now exports
///    `forex_connect_base_unit_size` (`IO2GTradingSettingsProvider::getBaseUnitSize` — the SAME lookup the
///    placement multiplies by), `crates/bridges/fxcm/src/execution/mod.rs` reads it once per instrument per
///    session, and `vike_fxcm::execution::event_mapper`'s `lots_for` divides. What this venue still cannot do
///    is place a FRACTION of a lot, so a `qty` that is not an exact multiple of the base unit is a
///    terminal reject naming the two placeable sizes either side — not a rounded substitute, which
///    is what the pre-#1470 `(qty.round() as i32).max(1)` produced for every one of them.
/// 2. **A fill this process did not place is DROPPED — so keep reconcile on for this venue or
///    accept losing fills across a restart.** The async fill lane routes on an in-memory map
///    built at accept time, so after a restart every trade the venue re-surfaces names an order the
///    new process never saw. It is `warn!`-visible rather than silent, and the recovery is the
///    reconcile client's `fetch_fill_reports`, which reads the same Trades table keyed by the same
///    `trade_id`. The bridge's own mount builds that client (`crates/bridges/fxcm/src/adapter.rs`'s
///    `VenueAdapterFxcm`, since the venue mount contract, docs/decisions/0096) and
///    `vike_tradehub_core::assemble_runtime` wires the handle, LAZILY: it is built only when reconciliation is
///    enabled, which a live mount is BY DEFAULT under `quarantine`
///    (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`; `VIKE_RECONCILE=1` is
///    only a force-on), so the hole is open exactly when the operator refused it with
///    `VIKE_RECONCILE_OFF=1`.
///    ⚠ It is NOT closed by routing on the venue order id instead: the Trades table is the only
///    place the linkage survives a process, and an in-process map cannot be made to.
/// 3. **A failed login is not refused at mount time.** The exec session fails the login and
///    rejects every order it is sent, each reject carrying the login's reason
///    (`crates/bridges/fxcm/src/execution/mod.rs`'s `refuse_every_command`; ⚠ this said the thread returned
///    at login and later submits became no-ops, which that function made false). The mount refuses
///    only the case it CAN detect before any session exists (an unopenable SDK shim, via
///    `vike_fxcm::sdk_available`; the "stub build" that used to be that case is gone), but a live
///    binary with a bad password mounts "live" and trades nothing.
///
/// **`"stop"` and `StopLoss` since 2026-10-26: the stop-loss rests AT the venue.** A reduce-only `stop` is bound to ONE open
/// trade as the venue's own per-trade stop (`O2G2::Orders::Stop`, `S`, with the trade's `TradeID`;
/// `crates/bridges/fxcm/src/execution/exit/mod.rs`, `fcshim.cpp`'s `forex_connect_place_exit`) and a reduce-only PRICED `limit` as its take-profit (`L`),
/// on a HEDGING account only: a plain resting order there is an ENTRY order that OPENS a trade when it is hit (measured), a
/// netting account has no per-trade order, and a stop that is not reduce-only is refused by name. The venue keeps the order,
/// so it survives a node outage and a restart; it is removed by the venue when its trade closes any other way. MEASURED on the
/// FXCM demo (`docs/ops/fxcm-forexconnect.md`, "Trade-bound stop-loss and take-profit"): placed, edited in place, deleted, a
/// real stop that FIRED (a closing fill under the stop's own order id, the take-profit deleted by the venue), a partial
/// close of the trade (the exits follow the remainder: through the bridge on 2026-11-09 a stop and a take-profit of 2000
/// kept their order ids at 1000 after a close of 1000, with no event under either, so the bridge tells the core the
/// size each now covers, `crates/bridges/fxcm/src/execution/mod.rs`'s `note_split`); and the bridge's own run on the demo
/// (`crates/bridges/fxcm/tests/fxcm_smokes/fxcm_stop_smoke.rs`, 2026-10-26: accepted, amended, cancelled, removed with the trade, and a stop
/// that FIRED as a fill under its own order client id). Not run through the core's own submit path. An exit covers a WHOLE trade
/// (the venue refuses a part, `ORA-20134`); a stop-limit and a take-profit TRIGGER kind stay unsupported (a take-profit is a
/// reduce-only limit). `supports_reduce_only` stays `false`: a NETTING account has no backstop for a reduce-only order.
///
/// **`reduce_only_kinds` lists all three kinds** (`crates/bridges/fxcm/src/execution/close.rs`'s `route`): on a hedging
/// account a reduce-only market closes the trades facing it by id, a reduce-only stop and a PRICED reduce-only limit
/// are bound to a trade (`exit.rs`), and on a netting account a reduce-only market or limit is placed as it stands. What
/// `route` refuses is not a KIND: an unpriced reduce-only limit (a bracket's take-profit is always priced), a stop on a
/// NETTING account (per account), and a close while an earlier one's outcome is unknown (per moment). The row
/// cannot say those; the adapter does, by name.
///
/// `backfill_bars: true` since the KEYLESS candle-archive source (`crates/bridges/fxcm/src/history.rs`'s
/// `KlineSourceFxcm`, a `KlineSource` the gatewayhub's `KLINE_SOURCES` dispatches): weekly and yearly `.csv.gz` files
/// from FXCM's public archive, no ForexConnect session — which is why `live_data` above stays `NONE` and the
/// two facts do not touch. Bars only (the archive holds no ticks), so `backfill_ticks` stays `false`.
pub const FXCM: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop"],
    reduce_only_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities::NONE,
    backfill_bars: true,
    backfill_ticks: false,
};

/// Dukascopy (`vike_dukascopy::execution::ExecutionClientDukascopy`; JForex Java sidecar). Wires
/// submit/cancel and MODIFY over JSON-lines stdio: the sidecar's `modify` command calls
/// `IOrder.setOpenPrice` / `setRequestedAmount` on a resting (`OPENED`) order and answers
/// `OrderModified` / `OrderModifyRejected` from JForex's `ORDER_CHANGED_OK` / `ORDER_CHANGED_REJECTED`
/// (`StrategyBridge.java`'s `handleModify`); `supports_modify` moved together with `crate::venue_amend`'s
/// `DUKASCOPY` row (`Unknown`). A stop-loss or take-profit (a price on the whole order, below) moves its
/// PRICE in place on every leg and refuses a SIZE in the sidecar's words, "a stop's size cannot be amended:
/// cancel it and place a new stop" (`handleStopModify`; MEASURED on the demo 2026-11-09;
/// `crate::venue_amend_mode`'s module doc). A jar built before that command logs it as an unknown command and never
/// answers: the engine's modify then has no event, so the jar must be released with this change. reduce-only is declared
/// CONSERVATIVELY `false` — the request is forwarded to the sidecar but whether `NettingPlan.java`
/// honors a per-order reduce-only flag is not verifiable from Rust (it does not: a reduce-only MARKET still
/// nets against the opposing orders and OPENS the remainder, a reduce-only LIMIT is the native take-profit of a jar
/// that announces it and is refused by the exec client by name for one that does not, and a `"stop"` is reduce-only
/// here, by construction, below). No live `DataClient`, but
/// `vike-backfill` backfills keyless `.bi5` TICKS (`append_quotes`) — and BARS from them, via
/// `hist.resample_quotes_to_bars` in `vike_backfill::venues::dukascopy`, which is
/// why `backfill_bars` is `true` here. It was `false` for as long as this field meant "the venue
/// serves a NATIVE bar endpoint"; that reading contradicted `vike_backfill::capabilities`, which has always
/// declared `{bar: true, quote: true}` for this venue. The field doc above settles the split in
/// favour of "can produce", so the two tables now agree and the cross-pin closes with no exception.
/// Expanded axes: the Rust side forwards the `OrderRequest` verbatim; the Java sidecar
/// (`StrategyBridge.java`) accepts `"market"`/`"limit"`/`"stop"` and terminally rejects every
/// other `order_type` ("unsupported order_type") — the preflight only moves that refusal to
/// the core edge. TIF/`margin_mode` are forwarded but never honored → `&[Gtc]`/`&[Cross]`.
/// **`"stop"` is the STOP-LOSS of an open position, and only that**: JForex has no reduce-only stop order
/// (`BUYSTOP`/`SELLSTOP` are entry orders and are not wired), so `StrategyBridge.java`'s `handleStop` sets
/// `IOrder.setStopLossPrice` on the open orders it protects, which the VENUE then holds and executes (it
/// survives a node outage), and refuses a `"stop"` that is not `reduce_only` or that has nothing to protect.
/// `trigger_types` is `[StopLoss]`: it names the `"stop"` trigger KIND, and the take-profit is not a `"take_profit"`
/// kind here but a reduce-only `"limit"` (next paragraph). The row is per venue and cannot say "reduce-only
/// stop only", so a stop sent without `reduce_only` passes this preflight and is refused by the sidecar in words.
/// The jar is a release asset: a jar from before this change answers a `"stop"` with its own terminal
/// "unsupported order_type: stop". Proven against the Java by `StopLossTest.java`; not yet against a real order
/// (the live drill is `docs/ops/known-limitations-2026-10.md`'s Dukascopy stop-loss row).
/// **`reduce_only_kinds` is `["market", "limit", "stop"]`: a reduce-only LIMIT is the venue's TAKE-PROFIT.** A jar of the
/// release that adds it takes a reduce-only limit as `IOrder.setTakeProfitPrice` on the open orders it closes
/// (`StrategyBridge.java`'s `handleStop` with `Protection.TAKE_PROFIT`: the same whole-order, oldest-first plan as the
/// stop-loss), which the VENUE holds and executes, so a bracket's take-profit leg rests natively beside its stop-loss
/// and passes the core's atomic preflight. The row cannot say "only for a jar that announces `take-profit`":
/// `ExecutionClientDukascopy::submit` refuses a reduce-only limit by name when the sidecar's `ready` does not announce
/// it (such a jar would place a plain pending order that OPENS a position when hit), so the refusal comes after the
/// edge for a skewed install only. A reduce-only market is netted against the open orders (`NettingPlan.java`) and a
/// reduce-only stop is the venue's stop-loss. ⚠ Not in the row: a protection still needs open orders to protect
/// (`StopPlan.java`), a per-moment fact only the sidecar knows, so an entry that nets against an opposite order first
/// can still have its stop or take-profit refused.
///
/// Flipped on its demo pass, 11.1 green on demo #8, 2026-11-09
/// (`crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s
/// `dukascopy_jforex_stop_limit_and_if_touched_rest_amend_survive_a_restart_and_cancel`): the venue's own
/// conditional ENTRY orders through the sidecar (`TriggerEntry.java`), a `"stop_limit"` as
/// `BUYSTOP`/`SELLSTOP` at its trigger with the limit as the slippage and a `"take_profit"` (a
/// `TakeProfit` trigger) as `BUYLIMIT`/`SELLLIMIT` with it (no limit: slippage NaN), for a jar announcing
/// `trigger-entry` (`crates/bridges/dukascopy/src/execution/mod.rs`'s `FEATURE_TRIGGER_ENTRY`; an older jar's
/// order is refused by name). Neither is placed reduce-only (an entry order OPENS a position when hit):
/// the client refuses that by name, so `reduce_only_kinds` leaves both out. An amend moves the limit
/// (`setOpenPrice(trigger, slippage)`; a market-if-touched's is refused in words) and the trigger too,
/// in place (`ExecutionClientDukascopy`'s `modify_trigger`, the sidecar recomputing the slippage so the
/// limit stays): no `venue_amend_mode` exception. ⚠ A limit amend with the trigger kept can lack the
/// SDK's `ORDER_CHANGED_OK`, on an own-session order too (measured on that pass); the sidecar then
/// confirms it by the venue's replacement notice (`crates/bridges/dukascopy/CLAUDE.md`).
pub const DUKASCOPY: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    reduce_only_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities::NONE,
    backfill_bars: true,
    backfill_ticks: true,
};

/// Polymarket CLOB (the shared `ExecutionClientActor`; behind
/// the `polymarket` feature). Wires submit/cancel only; no modify, no reduce-only (prediction
/// market). `supports_native_batch: false` stays FALSE and the row is unchanged even though this
/// adapter now overrides `cancel_batch`: the flag means a batch-of-N-ids endpoint whose capacity
/// `max_batch` states, and what the CLOB offers is an ACCOUNT-WIDE `DELETE /cancel-all` that takes
/// no id list at all — a mass cancel, not a batch, with no N to declare. `vike_polymarket::execution::rest`'s
/// `plan_cancels` is where the choice to use it lives, and there is no submit-side batch here.
/// `order_type_of` wires GTC + FOK LIVE-PROVEN (IOC coerces to FOK; GTD is
/// built but unproven) → declared `&[Gtc, Fok]`. Live tick feed serves quotes + trades + book (NOT bars — bars
/// via resample — and NOT the DOM `depth` snapshot lane). No `vike-backfill` backfill.
/// Expanded axes: the CLOB has ONE submit path — every order is a signed limit at `price` (unset →
/// 0.0), and `order_type` is read only to refuse a market order that names a price, before anything
/// is signed (`crates/bridges/polymarket/src/execution/mod.rs`). `"market"` is therefore encoded
/// AS-ACCEPTED (it
/// emulates: a sell at 0.0 is marketable — the shape `Flatten`/`MarketExit` rely on) — modeling
/// a real marketable order is the noted follow-up; trigger kinds would rest as nonsense limits
/// → refused. `accepted_tifs` is all five: Ioc→FOK and Day→GTC are the live coercions (a
/// separate flip), Gtd is emitted with a real wire `expiration` derived from `gtd_expiry`
/// (`client.rs::expiration_secs_of`, which refuses a missing/too-near expiry locally rather than
/// shipping the GTC `"0"`) — it stays OUT of `supported_tifs` because that wire path has never
/// been exercised against the live CLOB, and this venue trades real money on mainnet.
/// Fully collateralized → `margin_modes: &[Cash]`. Reduce-only: the CLOB has one submit path that
/// never reads the flag and refuses nothing for it, so `reduce_only_kinds` is the kind set.
pub const POLYMARKET: VenueCapabilities = VenueCapabilities {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Fok],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    supported_order_kinds: &["market", "limit"],
    reduce_only_kinds: &["market", "limit"],
    trigger_types: &[],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cash],
    default_margin_mode: MarginMode::Cash,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: false,
        quotes: true,
        trades: true,
        book: true,
        depth: false,
    },
    backfill_bars: false,
    backfill_ticks: false,
};

/// Interactive Brokers (the shared `ExecutionClientActor`; Phase 1 socket
/// transport). Wires submit/cancel and MODIFY on BOTH backends: the socket re-places the order under its
/// own `orderId` (TWS's amend; `crates/bridges/vike-ibkr/src/transport/socket.rs`'s `modify_order`, confirmed
/// by the venue's `openOrder` echo, `exec/amend.rs`), cpapi calls its amend endpoint. `supports_modify` was
/// `false` on the socket row until that override landed, and moved together with the
/// `crate::venue_amend` row (`Unknown`: the partial-fill convention is not measured) and with the deletion of
/// the separate cpapi row this one now covers. No batch (OCA / order-lists are a later phase); no wire reduce-only (multi-asset, not
/// wired in Phase 1). TIF maps the full GTC/DAY/IOC/FOK/GTD set.
///
/// **`live_data` was WRONG here for three weeks** — this row was authored 2026-07-14 saying "no
/// live `DataClient` yet (Phase 3)", and that statement was falsified the next day and never
/// revisited: `vike_ibkr::IbkrFeeds` (#306, 2026-07-15; today the socket backend of
/// `vike_ibkr::feed::price::DataClientBusSpecIbkr`) implemented ALL FIVE
/// `DataClient` verbs for real — `subscribe_bars` (`bars_pump`: a `historical_data` seed then
/// `realtime_bars` folded to the requested interval), `subscribe_quotes` (`quotes_pump` →
/// `sink.quote`), `subscribe_trades` (`trades_pump` → `sink.trade`), and
/// `subscribe_book`/`subscribe_depth` (both `depth_pump`, which maintains the position-indexed
/// `reqMktDepth` ladder and emits it as BOTH `sink.book` and `sink.l2_snapshot`). Not one returned
/// `Unsupported`, and `crates/vike-run/src/bin/ibkr_mount.rs` mounted it — until that bin was
/// deleted on 2026-09-28. Today the gatewayhub runs it QUOTES ONLY, as `DataClientBus` over
/// `DataClientBusSpecIbkr::for_config` (the any-symbol price lane); the bars and trades lanes are a
/// capability the bridge has, not one any process uses. Behind `ibkr-socket`, exactly as polymarket's row sits behind its own feature — a
/// feature gate is not a capability denial. How that lane CONNECTS (a native session over the gateway's
/// socket) is `crate::venue_connection::IBKR`'s market row; this row says only what it offers.
///
/// **`book` and `depth` are `false` since docs/decisions/0140** (2026-10-12): the accounts hold no
/// market-depth entitlement on demo or live, so the socket backend's `reqMktDepth` pump was retired and
/// `subscribe_book`/`subscribe_depth` answer `Unsupported` on BOTH backends, driven off this row
/// (`vike_ibkr`'s `data_door::no_book_lane`). The cpapi backend's data client (`vike_ibkr`'s
/// `cpapi_feed`, the CP Gateway `smd` topic) serves the same three lanes, so this one row covers
/// both backends.
///
/// `backfill_bars` has its own, separate history: `true` from 2026-07-15 (`vike_backfill::
/// venues::ibkr` #311 + `src/bin/ibkr_backfill.rs`, live-verified on paper with 251 daily AAPL
/// bars) until docs/decisions/0094 deleted that bin and its planner (2026-09-28), measured
/// unused — no `venue=ibkr` series existed in any store. `vike_ibkr::HistoricalFetcher` stays in
/// the bridge, so an IBKR bar lane remains buildable; nothing in this crate pages it today, hence
/// `false`.
///
/// The cost of the `live_data` gap was not cosmetic: `vike_data::require_live_verb` derives
/// `DataClientError::Unsupported` from this row, and `vike_app_core::ui::feed_lifecycle` treats that
/// error as PROVABLY PERMANENT and never retries. (No IBKR verb routes through `require_live_verb`
/// today, so correcting the row is runtime-inert here — it removes a latent trap rather than
/// changing a live path.)
/// Expanded axes: `crates/bridges/vike-ibkr/src/execution/order.rs`'s `map_order_request` wires `"market"` → MKT,
/// `"limit"` → LMT, `"stop"` → STP (`trigger_price` as `aux_price`) and `"stop_limit"` → STP LMT (`price`
/// as the limit, `trigger_price` as `aux_price`), which both backends put on the wire
/// (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `build_order`,
/// `crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `order_json`), and the amend re-places a
/// stop-limit natively with its limit moved and its trigger re-sent as it was
/// (`crates/bridges/vike-ibkr/src/execution/mod.rs`'s `amend`; no `venue_amend_mode` exception). It returns a
/// `Result`: an unrecognised word is its `Err`, a terminal `OrderRejected` naming the kind, never the
/// MKT the old `_ => MKT` fallthrough sent. BUILT and NOT listed until the paper-gateway pass runs
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_order_kinds_w2_smoke.rs` (wave 2 of the order kinds
/// runs no venue order), so this row refuses each first: `"take_profit"` as `MIT` (no price) or `LIT`,
/// `"market_to_limit"` as `MTL` (refused on a `CASH` contract), the auction lifetimes as TIF `OPG` on a
/// `MKT`/`LMT` and the order types `MOC`/`LOC` sent `DAY` (stocks and futures only), and a `display_quantity` as
/// `displaySize` on a plain `LMT` (`map_order_request`). The iceberg is OPEN (`supports_display_quantity: true`)
/// behind `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_iceberg_lot_smoke.rs`'s `ibkr_iceberg_display_sizes`
/// (paper account #12, 2026-11-10): a display must be a multiple of the contract's ROUND LOT (IB's
/// `suggestedSizeIncrement`; for a US stock 100, 40, 10 or 1 shares by its price, Regulation NMS since
/// 2025-11-03). An AAPL buy of 200 with display 40 (its round lot) was ACCEPTED straight through `ibapi` and
/// through the bridge, and cancelled; displays 1 and 100 were refused by IB with `[201] Display size should be
/// a multiple of lot`, and the bridge refuses them first, in words, nothing sent (the socket transport's
/// `lot_refusal`). With the account's extended-hours routing on, the bridge refuses an iceberg on a stock
/// limit by name (`overnight_refusal`: IB's `[201]` on its overnight route). The Client Portal backend
/// refuses `MIT`, `LIT`, `MTL`, `MOC`, `LOC` and an iceberg by name (`order_json`): its ticket names none of
/// them.
/// `"trailing"` is BUILT on the socket backend as `TRAIL` (the trail as `auxPrice`, no `trailStopPrice`),
/// refused by name on the Client Portal backend, and NOT listed by a price until the paper-gateway pass runs
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_trailing_smoke.rs`. `"trailing_limit"` (`TRAIL LIMIT`) is
/// NOT built and listed on neither axis: the bridge refuses it by name (`crates/bridges/vike-ibkr/src/execution/order.rs`'s
/// `no_starting_stop`; IB answered `[321] Please enter a stop price` on paper 2026-11-09: it wants a starting
/// stop, which only a market price gives). A trailing order's ACTIVATION is refused by name too, whatever its
/// kind (`map_order_request`: the `TRAIL` sent behind IB's price condition got no order status on that run);
/// the transport keeps the condition for the run that proves it
/// (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `activation_condition`). No row axis names an
/// activation, so this row stays as it is when that refusal lifts.
/// `"trailing_take_profit"` (the attributes wave) is the socket backend's `TRAIL MIT` (no limit offset), the
/// trail as `auxPrice` (STK only, refused by name on another product), refused by name on the Client Portal
/// backend, never amended (`venue_amend_mode`), and LISTED behind the paper-gateway pass of
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_attributes_smoke.rs`: smoke green 2026-11-09 (paper account
/// #12) for `TRAIL MIT`; listed reduce-only too, as every ibkr kind (the bridge never reads the flag). With a
/// limit offset it would be `TRAIL LIT`, which IB refused the same way as `TRAIL LIMIT` (321): this row
/// cannot tell the two forms apart (one kind, the offset a term), so the bridge refuses the offset form by
/// name (`no_starting_stop`).
/// `percent_trail_kinds` are the trailing kinds by a PERCENT that passed, IB's `trailingPercent` in place of
/// the `auxPrice`: `"trailing"` (`TRAIL`) and `"trailing_take_profit"` (`TRAIL MIT`), behind the same attributes
/// smoke, green 2026-11-09 for both (the socket backend; the Client Portal refuses every trailing order by
/// name). Not `"trailing_limit"` (refused by name, above); a percent take-profit with a limit offset is the
/// same refusal. Its axis is not the kind axis: a price `trailing` stays behind `ibkr_trailing_smoke.rs`.
/// `"pegged"` is BUILT on the socket backend for the MID anchor alone, `PEG MID` (the offset as `auxPrice` in
/// IB's sign, no `lmtPrice`), refused by name on the Client Portal backend, and LISTED (in both kind sets, as
/// every kind of this row) behind the paper-gateway pass of
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_pegged_smoke.rs`: smoke green 2026-11-09 (paper account
/// #12) for `PEG MID`. `PEG MKT`, `REL` and `PEG BEST` are refused by name in the bridge (`map_order_request`,
/// each with what the gateway did on that run): the row has one kind for every anchor, so it opened only
/// together with those refusals.
/// The stop-limit is a `StopLoss` trigger, so `trigger_types` is unchanged. `accepted_tifs` == the
/// mapped seven: GTC, DAY, IOC, FOK, `Gtd` (the socket's `goodTillDate`, back in the set behind the
/// attributes smoke's GTD row; the Client Portal refuses a date by name) and the two auctions
/// (`AtOpen` as TIF `OPG` on a `MKT`/`LMT`, `AtClose` as `MOC`/`LOC` sent DAY, behind
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_order_kinds_w2_smoke.rs`'s
/// `ibkr_order_kinds_w2_auctions_place_cancel`, green 2026-11-10 on paper account #12: all four
/// accepted, read back off IB's row and cancelled; the Client Portal refuses `MOC`/`LOC` by name). IB
/// has no post-only order. `margin_mode` never read (account-level Reg-T margin) → `&[Cross]`. Reduce-only:
/// `map_order_request` never reads the flag and the adapter refuses nothing for it (a plain order
/// goes out), so `reduce_only_kinds` is the kind set.
/// `order_attributes` (the attributes wave) are the socket backend's IB fields, set only when present
/// (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `build_order`), FLIPPED behind the paper-gateway
/// passes of `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_attributes_smoke.rs` (paper account #12):
/// `hidden` (a display of 0) as `hidden` on a `LMT` (STK only, refused by name on another product) and
/// `adjust` as the adjustable stop's `triggerPrice` and `adjusted*` fields on a `STP`/`STP LMT` (its amend
/// refused by name), smoke green 2026-11-09 for `hidden` and for an adjustment to each target (`STP`,
/// `STP LMT`, `TRAIL` by an amount and by a percent); `all_or_none` as `allOrNone`, `sweep_to_fill` as
/// `sweepToFill` and `discretionary` as `discretionaryAmt` on a `LMT` (the last two STK only), smoke green
/// 2026-11-09 on the regular route (accepted, read back on IB's row, cancelled), and refused by name while the
/// account's extended-hours routing applies (IB ended each with `[201]` on its overnight route; the socket
/// transport's `overnight_refusal`). `release`: a TIME-only release rests at IB from the accept as
/// `goodAfterTime` (UTC), the core holding none (the owner's 2026-11-08 answer "A": "it has to be under IBKR
/// control"), smoke green 2026-11-09 on the regular route; on an account with the extended-hours switch on the
/// core holds the release and sends a plain order at its time, as before (IB ended a `goodAfterTime` with
/// `[201]` on its overnight route, so `native_time_release` is false there and the bridge's refusal by name
/// is only a backstop); a release with a price condition is still held by the core. This row is the VENUE's and covers both backends, so whether the core sends a time release at
/// all is the ACCOUNT's fact (`crates/bridges/vike-ibkr/src/config.rs`'s `native_time_release`: the socket
/// and oauth backends; a Client Portal account's release stays the core's, as before). The Client Portal
/// backend writes `allOrNone` (schema snippet, no capture) and refuses the other words by name (`order_json`).
pub const IBKR: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Day,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::AtOpen,
        TimeInForce::AtClose,
    ],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Day,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::AtOpen,
        TimeInForce::AtClose,
    ],
    supported_order_kinds: &[
        "market",
        "limit",
        "stop",
        "stop_limit",
        "trailing_take_profit",
        "pegged",
    ],
    reduce_only_kinds: &["market", "limit", "stop", "stop_limit", "trailing_take_profit", "pegged"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    supports_display_quantity: true,
    peg_anchors: &[vike_model::OrderPegAnchor::Mid],
    order_attributes: &[
        "all_or_none",
        "sweep_to_fill",
        "discretionary",
        "hidden",
        "adjust",
        "release",
    ],
    percent_trail_kinds: &["trailing", "trailing_take_profit"],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    // No book, no depth, on EITHER backend (docs/decisions/0140, Context 7): the accounts hold no
    // depth entitlement, so both IBKR data clients refuse the two verbs off this row.
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: true,
        book: false,
        depth: false,
    },
    // docs/decisions/0094 deleted ibkr_backfill; HistoricalFetcher stays in the bridge.
    backfill_bars: false,
    backfill_ticks: false,
};

/// Hyperliquid (`ExecutionClientActor` over `vike_hyperliquid::execution::client::spawn`'s own command loop; spot +
/// perp). Modify is wired as venue cancel-replace (`supports_modify: true`); HL is batch-first so
/// the exec seam uses the native batch endpoint (`supports_native_batch: true`); perp orders carry
/// `reduceOnly`. On the wire TIF is `Gtc`/`Ioc` (post-only maps to `Alo`, market is an emulated
/// `Ioc`, post-only is `Alo`), so `supported_tifs = &[Gtc, Ioc, PostOnly]`. The live `DataClient` (`DataClientWebsocket` over
/// `crates/bridges/hyperliquid/src/feed/mod.rs`'s `DataClientSpecHyperliquid`) serves
/// candle bars + `bbo` quotes + `trades` + the `l2Book` full-snapshot DOM `depth` lane (no separate
/// lossless `book` tick lane in v1). `vike-backfill` DOES backfill bars now —
/// `vike_hyperliquid::history::klines::KlineSourceHyperliquid` (a `KlineSource` impl in the bridge, one of
/// `crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`'s rows (docs/decisions/0094 moved the registry to
/// the datahub; decision 0164 on to the gatewayhub)) pages `candleSnapshot` through `klines::ingest_klines` → `append_bars`, dispatched
/// by the datahub's `Backfill` verb; this row's "no backfill yet (a later add)" outlived the add.
/// See `docs/superpowers/specs/2026-07-16-hyperliquid-bridge-design.md`.
/// Expanded axes: the RICH trigger venue — `build_order_wire` routes `"stop"` AND
/// `"take_profit"` through `build_trigger_kind` (`triggerPx` from `trigger_price`,
/// `tpsl:"sl"/"tp"`, market-or-limit by whether `price` is set), and `"market"` is the emulated
/// IOC limit at ±5% off mid. `accepted_tifs` is all five (Fok→Ioc, Gtd/Day→Gtc are the live
/// coercions — a separate flip), plus `PostOnly` → the wire `Alo` (`supports_post_only: true`; a
/// crossing `Alo` order is refused by the venue, never filled). `margin_mode` never read on
/// the order path → `&[Cross]`. The batch action carries N wires with NO adapter-side cap →
/// `max_batch: usize::MAX`.
///
/// **`"stop_limit"` was MISSING from `supported_order_kinds` and is now declared.** The adapter's
/// trigger routing keys off `req.trigger_price.is_some()` (`exec.rs`'s `is_trigger`), not off the
/// `order_type` spelling, so a `"stop_limit"` request has ALWAYS reached `build_trigger_kind`,
/// where `is_market = req.price.is_none()` makes a present limit price a correct RESTING
/// stop-limit at that price (`tpsl: "sl"`). Omitting the kind here meant `preflight_order` returned
/// `TRIGGER_UNSUPPORTED` and `vike-core` terminally rejected an order this adapter builds
/// correctly — a live capability lost to a declaration gap, not to missing code.
/// `crates/bridges/hyperliquid/src/execution/execution_tests.rs`'s `stop_limit_kind_is_declared_and_built` now ties the
/// two together in ONE test, so the row cannot drift back without the wire assertion moving too.
/// Reduce-only: `build_order_wire` writes `reduce_only: req.reduce_only` on the wire of every kind
/// (trigger kinds and the emulated market included) and refuses none for it, so `reduce_only_kinds`
/// is the kind set.
/// `"trailing"` is BUILT as the venue's `trailingStop` action (`retracement.px` the trail in price,
/// `activationPx` the activation), accepted with `trigger_by` Mark only and refused by name with any other
/// (every hyperliquid trigger reads the mark), and NOT listed until its testnet smoke runs (wave 3 of the
/// order kinds runs no venue order). `"trailing_limit"` stays refused: the action fires as a market order.
pub const HYPERLIQUID: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: true,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::PostOnly],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
        TimeInForce::PostOnly,
    ],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    reduce_only_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: true,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: usize::MAX,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: true,
        book: false,
        depth: true,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

/// cTrader Open API (`vike_ctrader::ExecutionClientCtrader` / `DataClientBus<DataClientBusSpecCtrader>` over the
/// shared-socket actor).
/// AUDITED against the adapter (was the [`VenueCapabilities::UNSUPPORTED`] fallback the #498 roster PR
/// pinned as "known-stale pending cTrader's own audit" — this row IS that audit, and it flips two
/// fields, a deliberate GUI-visible behavior change: the GUI's modify gate now OPENS for cTrader).
///
/// - `supports_modify: true` — `ExecutionClientCtrader::modify` (exec.rs) resolves the venue `orderId` and
///   enqueues a real `Command::AmendOrder` (`ProtoOaAmendOrderReq`) that the actor writes as
///   `AMEND_ORDER_REQ` (2109); the venue's `ORDER_REPLACED` maps back to `OrderModified`
///   (event_mapper.rs). An unknown order client id is a NON-terminal `OrderModifyRejected` (order stays live).
///   This is a native in-place amend end-to-end — the gate opening reaches a real wire path.
/// - `live_data { bars: true, quotes: true }` — `DataClientBusSpecCtrader` (data.rs) serves `subscribe_bars`
///   (trendbars, 300-bar seed then live) + `subscribe_quotes` (spot L1 bid/ask). `subscribe_trades`
///   /`subscribe_book`/`subscribe_depth` all return `Unsupported`, so those stay `false`.
///
/// Conservative-`false` fields (adapter does NOT wire them): no `submit_batch`/`cancel_batch`
/// override (fan-out default) → `supports_native_batch: false`; `order_to_new_order` sets no
/// `reduceOnly` → `supports_reduce_only: false`; no `combo_legs` read → `supports_combo: false`;
/// the new-order request sends no explicit TIF (defaults GTC), so — like IG — `supported_tifs =
/// &[Gtc]`; `backfill_bars: true` since the credentialed candle source (`crates/bridges/ctrader/src/history.rs`'s
/// `KlineSourceCtrader`, bid-side bars, dispatched by the gatewayhub's credentialed lane) and `backfill_ticks: false`
/// (the tick request is not wired).
/// Expanded axes: `order_type_of` (`crates/bridges/ctrader/src/feed/decode.rs`) maps exactly
/// `"market"`/`"limit"`/`"stop"`/`"stop_limit"` (Stop wires `trigger_price`; a priceless conditional
/// is rejected) and any OTHER kind is ALREADY a local terminal `OrderRejected` ("unmappable order") —
/// the preflight matches that reality at the core edge. A `"stop_limit"` is the venue's `STOP_LIMIT`
/// (`order_to_new_order`: `stopPrice` the trigger, `slippageInPoints` the limit's distance from it in
/// points, `stop_limit_slippage`); a limit on the wrong side of the trigger, or one that is not a whole
/// number of points away, is refused by name there, per order and not per kind. A resting stop-limit is
/// amended in place: `crates/bridges/ctrader/src/feed/decode.rs`'s `amend_order_req` restates its
/// `stopPrice` and its `slippageInPoints` from the moved limit, so the amend matrix's cell is native and
/// `venue_amend_mode` has no ctrader exception. TIF is never emitted (protocol default) →
/// `accepted_tifs: &[Gtc]` (non-GTC limits flip from silent venue-default to a loud refusal). The
/// preflight judges that set on a limit only, so a stop-limit asking any lifetime but GTC is refused by
/// the bridge, by name (`event_mapper.rs`'s `STOP_LIMIT_LIFETIME_UNSUPPORTED`): it agrees with the row.
/// An AUCTION lifetime (`at-open`, `at-close`) is outside `accepted_tifs`, so the preflight refuses it on
/// a market or a limit, and the bridge refuses it by name before any route on every kind
/// (`crates/bridges/ctrader/src/execution/mod.rs`'s `AUCTION_LIFETIME_UNSUPPORTED`: no time in force is sent, so
/// the order would execute now). The venue's `MARKET_ON_OPEN` is NOT built: the plan of the order kinds
/// opens it only after a demo check states its meaning on a CFD, and the proto has no at-close value.
/// `margin_mode` never read → `&[Cross]`.
///
/// Bridge-side pin: `vike_ctrader::CAPABILITIES` re-exports this row and its `capabilities_test` module ties it to
/// what `exec.rs`/`data.rs` actually wire (mirroring every sibling bridge crate), so an adapter
/// drift trips there too — not just in this module's own matrix tests
/// (`expanded_axes_matrix_is_pinned`, `ctrader_capabilities_reflect_the_adapter`).
///
/// **`reduce_only_kinds` lists all four kinds**: `ExecutionClientCtrader::submit` closes a reduce-only market by the open
/// positions and, on a hedge-capable account, BINDS a reduce-only limit, stop or stop-limit to the position it faces
/// (`ProtoOaNewOrderReq::position_id`). What it refuses is a fact about the moment, not the kind (no open position to
/// bind: `NO_POSITION_TO_BIND_AN_EXIT`; one exit larger than any position's room: `EXIT_SPANS_POSITIONS`; the kind
/// already covers every position: `EXIT_ALREADY_COVERED`), so those stay the adapter's, by name. A bracket's exits are
/// released after the entry fills, which is when a position exists to bind to.
pub const CTRADER: VenueCapabilities = VenueCapabilities {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit"],
    reduce_only_kinds: &["market", "limit", "stop", "stop_limit"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    supports_display_quantity: false,
    peg_anchors: &[],
    order_attributes: &[],
    percent_trail_kinds: &[],
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: DataClientCapabilities {
        bars: true,
        quotes: true,
        trades: false,
        book: false,
        depth: false,
    },
    backfill_bars: true,
    backfill_ticks: false,
};

// vike:new-venue:row /// TODO(new-venue: {venue}): cite the adapter code EVERY field below was read from, the way
// vike:new-venue:row /// the rows above do. The scaffolded value is the MINIMUM row that satisfies
// vike:new-venue:row /// `expanded_axes_invariants_hold` (which refuses an empty kind set and an empty
// vike:new-venue:row /// margin-mode set, so `VenueCapabilities::UNSUPPORTED` is NOT a legal roster row here).
// vike:new-venue:row pub const {VENUE}: VenueCapabilities = VenueCapabilities {
// vike:new-venue:row     supports_modify: false,
// vike:new-venue:row     supports_native_batch: false,
// vike:new-venue:row     supports_reduce_only: false,
// vike:new-venue:row     supports_combo: false,
// vike:new-venue:row     supported_tifs: &[TimeInForce::Gtc],
// vike:new-venue:row     accepted_tifs: &[TimeInForce::Gtc],
// vike:new-venue:row     supported_order_kinds: &["market", "limit"],
// vike:new-venue:row     reduce_only_kinds: &["market", "limit"],
// vike:new-venue:row     trigger_types: &[],
// vike:new-venue:row     supports_post_only: false,
// vike:new-venue:row     supports_display_quantity: false,
// vike:new-venue:row     peg_anchors: &[],
// vike:new-venue:row     order_attributes: &[],
// vike:new-venue:row     percent_trail_kinds: &[],
// vike:new-venue:row     margin_modes: &[MarginMode::Cross],
// vike:new-venue:row     default_margin_mode: MarginMode::Cross,
// vike:new-venue:row     max_batch: 0,
// vike:new-venue:row     live_data: DataClientCapabilities::NONE,
// vike:new-venue:row     backfill_bars: false,
// vike:new-venue:row     backfill_ticks: false,
// vike:new-venue:row };
// vike:new-venue:row
/// The registry: the declared [`VenueCapabilities`] for a canonical venue string — one arm per
/// [`vike_model::VENUES`] entry (the completeness test pins that). An unknown venue returns
/// [`VenueCapabilities::UNSUPPORTED`] — a caller then offers nothing until the venue is known
/// (fail-closed). This is the small keyed registry the GUI consults with a Trade window's venue
/// key.
pub fn capabilities_for(venue: &str) -> VenueCapabilities {
    match venue {
        "binance" => BINANCE,
        "bybit" => BYBIT,
        "okx" => OKX,
        "deribit" => DERIBIT,
        "oanda" => OANDA,
        "ig" => IG,
        "fxcm" => FXCM,
        "dukascopy" => DUKASCOPY,
        "polymarket" => POLYMARKET,
        "ibkr" => IBKR,
        "ctrader" => CTRADER,
        "alpaca" => ALPACA,
        "aster" => ASTER,
        "hyperliquid" => HYPERLIQUID,
        // vike:new-venue:row "{venue}" => {VENUE}, // TODO(new-venue: {venue}): the row above must be real before this ships
        _ => VenueCapabilities::UNSUPPORTED,
    }
}

/// **Whether the reconcile balance of an engine mounted on `symbol` is WALLET CASH**: a figure an open
/// position moves only by realized PnL, fees and funding, so a pass may take it beside an open position
/// (the statement decision 0179 waits for; `vike_core`'s `adopt_cash_the_book_left_out` is the one reader).
///
/// Read from each `ReconcileClient`'s parser, not assumed: a perpetual's `balance` on binance and aster
/// (`crates/bridges/binance/src/family/recon.rs`'s `parse_perp_balance`), `walletBalance` on bybit
/// (`crates/bridges/bybit/src/reconcile/mod.rs`'s `parse_wallet_balance`), `cashBal` on okx
/// (`crates/bridges/okx/src/reconcile/mod.rs`'s `parse_balance`). A spot lane is not: its USDT moves by the
/// whole notional of a trade (decision 0290's user names: a derivative is marked, a perpetual by `.P`, a dated
/// contract by `_YYMMDD`, beside the unmarked pair; bybit's and okx's spelling is their [`crate::spot_pair_row`]). Every other venue
/// answers `false`: an unmeasured or other figure (hyperliquid's classic `accountValue`, the whole-notional
/// cash of ibkr, alpaca and polymarket; the FX venues' balances are not measured for this) waits for a flat
/// book. A venue joins by its own measured arm here, never by having a spot row.
pub fn balance_is_wallet_cash(venue: &str, symbol: &str) -> bool {
    match venue {
        // the futures lane by decision 0290's user grammar: a perpetual's `.P`, a dated contract's `_YYMMDD`
        "binance" | "aster" => crate::SpotPairSpelling::Unmarked.names_derivative(symbol),
        "bybit" | "okx" => {
            crate::spot_pair_row(venue).is_some_and(|row| row.spelling.names_derivative(symbol))
        }
        _ => false,
    }
}

/// The ORDER ATTRIBUTE words a row's [`VenueCapabilities::order_attributes`] is written in, each an
/// [`ATTRIBUTE_UNSUPPORTED` refusal's](PreflightDeny::Attribute) `attribute=` word:
/// - `all_or_none`, `sweep_to_fill`, `discretionary`: the members of [`vike_model::OrderAttributes`];
/// - `hidden`: a display of ZERO ([`OrderRequest::display_quantity`]), a whole order the venue shows none
///   of (a display above zero is [`VenueCapabilities::supports_display_quantity`]'s);
/// - `adjust`: a stop's [`vike_model::OrderStopAdjust`] (IB's adjustable stop, the auto trailing stop);
/// - `release`: the venue holds a TIME release itself (IB `goodAfterTime`); never a refusal, since
///   the core holds a release wherever the row lacks the word.
pub const ORDER_ATTRIBUTES: &[&str] =
    &["all_or_none", "sweep_to_fill", "discretionary", "hidden", "adjust", "release"];

/// Why [`preflight_order`] refused an [`OrderRequest`] — one variant per checked axis. The
/// rendered reason ([`core::fmt::Display`]) is MACHINE-READABLE:
/// `CATEGORY_CONDITION: key=value ...` (e.g. `TIF_UNSUPPORTED: tif=Fok venue=ig`), so an
/// operator surface or a log grep can dispatch on the prefix without parsing prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightDeny {
    /// `order_type` is a non-trigger kind the venue adapter does not wire, or an
    /// [`OrderType::Unrecognized`] kind (refused on every venue).
    OrderKind {
        /// the requested kind's word (an unrecognised one verbatim)
        kind: String,
        /// the venue whose row refused it
        venue: String,
    },
    /// `order_type` is a trigger kind ([`OrderType::is_trigger`]) the venue adapter does not wire — the
    /// silent alternative would be the `_ => market` coercion class (a resting stop firing
    /// immediately).
    Trigger {
        /// the requested trigger kind's word
        kind: String,
        /// the venue whose row refused it
        venue: String,
    },
    /// a `reduce_only` order of a kind the venue's adapter does not place reduce-only
    /// ([`VenueCapabilities::reduce_only_kinds`]): it would be refused by name AFTER the fact, and for a
    /// bracket's exit that is after the entry has filled.
    ReduceOnly {
        /// the requested kind's word
        kind: String,
        /// the venue whose row refused it
        venue: String,
    },
    /// a limit order's `time_in_force` is outside the venue's [`VenueCapabilities::accepted_tifs`], or a
    /// market order's AUCTION lifetime is ([`TimeInForce::is_auction`]: a market-on-open the adapter
    /// does not wire would execute now).
    IbapiTimeInForce {
        /// the requested TIF
        tif: TimeInForce,
        /// the venue whose row refused it
        venue: String,
    },
    /// a limit order's `time_in_force` is one the venue's adapter would run as ANOTHER lifetime
    /// ([`crate::venue_time_in_force::TimeInForceOutcome::Coerced`]: hyperliquid's `Fok` as `Ioc` and its `Gtd` / `Day` as
    /// `Gtc`, polymarket's `Ioc` as `Fok` and its `Day` as `Gtc`, alpaca's `Gtd` as `Gtc`). Admitted
    /// by [`VenueCapabilities::accepted_tifs`] for the compatibility law, but refused here once an operator or a
    /// strategy can NAME a lifetime: the order that rests is not the one that was asked for.
    TimeInForceCoerced {
        /// the requested lifetime
        tif: TimeInForce,
        /// the lifetime the adapter would run it as
        becomes: TimeInForce,
        /// the venue whose row refused it
        venue: String,
    },
    /// an explicit `margin_mode` request the venue adapter cannot honor.
    MarginMode {
        /// the requested mode
        mode: MarginMode,
        /// the venue whose row refused it
        venue: String,
    },
    /// a `PostOnly` order that is not a LIMIT order: a market order or a stop has no resting price
    /// to protect, so the flag would be dropped by every adapter and the order would take
    /// liquidity. Refused on every venue, whatever its [`VenueCapabilities::supports_post_only`].
    PostOnlyNeedsLimit {
        /// the requested kind's word
        kind: String,
        /// the venue whose row was consulted
        venue: String,
    },
    /// an order with a `display_quantity` (an iceberg) on a venue whose adapter does not wire one
    /// ([`VenueCapabilities::supports_display_quantity`]) and that [`emulation_of`] does not emulate either (an IOC
    /// or FOK lifetime, an attribute, an order list's leg, a venue of [`EMULATION_REFUSED_VENUES`]): it
    /// would rest with its whole size visible.
    DisplayQuantity {
        /// the venue whose row refused it
        venue: String,
    },
    /// an order carrying an ORDER ATTRIBUTE ([`ORDER_ATTRIBUTES`]: all-or-none, sweep-to-fill, a
    /// discretion, a hidden display of zero, a stop's adjustment) the venue's adapter does not wire
    /// ([`VenueCapabilities::order_attributes`]): it would go out as the plain order, the attribute dropped.
    Attribute {
        /// the attribute's word
        attribute: &'static str,
        /// the venue whose row refused it
        venue: String,
    },
}

impl core::fmt::Display for PreflightDeny {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PreflightDeny::OrderKind { kind, venue } => {
                write!(f, "ORDER_KIND_UNSUPPORTED: kind={kind} venue={venue}")
            }
            PreflightDeny::Trigger { kind, venue } => {
                write!(f, "TRIGGER_UNSUPPORTED: kind={kind} venue={venue}")
            }
            PreflightDeny::ReduceOnly { kind, venue } => {
                write!(f, "REDUCE_ONLY_UNSUPPORTED: kind={kind} venue={venue}")
            }
            PreflightDeny::IbapiTimeInForce { tif, venue } => {
                write!(f, "TIF_UNSUPPORTED: tif={tif:?} venue={venue}")
            }
            PreflightDeny::TimeInForceCoerced { tif, becomes, venue } => {
                write!(f, "TIF_COERCED: tif={tif:?} would run as {becomes:?} venue={venue}")
            }
            PreflightDeny::MarginMode { mode, venue } => {
                write!(f, "MARGIN_MODE_UNSUPPORTED: mode={mode:?} venue={venue}")
            }
            PreflightDeny::PostOnlyNeedsLimit { kind, venue } => {
                write!(f, "POST_ONLY_NEEDS_LIMIT: kind={kind} venue={venue}")
            }
            PreflightDeny::DisplayQuantity { venue } => {
                write!(f, "DISPLAY_QTY_UNSUPPORTED: venue={venue}")
            }
            PreflightDeny::Attribute { attribute, venue } => {
                write!(f, "ATTRIBUTE_UNSUPPORTED: venue={venue} attribute={attribute}")
            }
        }
    }
}

/// The CORE-EDGE capability preflight: validate an outgoing [`OrderRequest`] against its venue's
/// declared [`VenueCapabilities`] row BEFORE it reaches the `ExecutionClient` — `Err` means the caller
/// must synthesize a loud TERMINAL refusal (the `supports_combo` arm's `OrderSubmitted` →
/// `OrderRejected` shape in `vike-core`), never send the order, and never let it silently
/// vanish.
///
/// THE COMPAT LAW this function is built around: it refuses NOTHING a venue adapter
/// accepts-and-honors today. Concretely:
/// - an UNKNOWN venue string (not in [`vike_model::VENUES`]) passes untouched — there is no
///   declared row to enforce, and the paper/sim engines behind non-roster venue ids must keep
///   working (the registry's fail-closed `UNSUPPORTED` fallback is for OFFER surfaces, not for
///   refusing traffic the venue side owns);
/// - a COMBO request (`combo_legs` non-empty) passes — the `OrderIntent::Combo` lowering already
///   resolved `supports_combo` for it;
/// - the TIF check runs on the LIMIT path only, mirroring `deny_unsupported_time_in_force`'s scope — no
///   venue reads a request TIF on its market/trigger paths — plus a MARKET order's auction lifetime,
///   which no request carried before the lifetime existed;
/// - coerced-but-live TIFs are in [`VenueCapabilities::accepted_tifs`], so they pass (refusing a
///   coercion is a separate per-venue flip);
/// - a `margin_mode` of `None` always passes (it means "the venue's default behavior");
/// - a `reduce_only` request passes unless its kind is absent from [`VenueCapabilities::reduce_only_kinds`],
///   which lists what the adapter places today (a venue that carries or ignores the flag lists
///   every kind it accepts), so only an order the adapter would refuse by name is refused here.
///   Run over a bracket's three legs by `lower_bracket`, it is what stops a bracket whose exit
///   the venue cannot rest from filling its entry first. (This function judges as a demo or live
///   engine would; [`preflight_order_at`] with a stated `paper` tier skips this one bullet.)
///
/// What DOES get refused is the silent-lie class: an [`OrderType::Unrecognized`] kind, on EVERY
/// venue (checked before the unknown-venue and combo passes: no engine or adapter knows what it
/// means, and most would coerce it into a market order), kinds the adapter would coerce into
/// different semantics (a "stop" becoming an immediate market order), TIFs the adapter would
/// silently ignore (aster/ig resting GTC regardless), explicit margin modes no adapter
/// honors, a `display_quantity` no adapter shows, and an order ATTRIBUTE no adapter wires
/// ([`VenueCapabilities::order_attributes`]: a display of ZERO is judged by `hidden`, not by
/// [`VenueCapabilities::supports_display_quantity`]). A request with a `trail_percent` is judged against
/// [`VenueCapabilities::percent_trail_kinds`] instead of the kind and reduce-only axes, and a market-to-limit
/// with an auction lifetime on the TIF axis (an adapter that does not wire it would execute now).
///
/// ⚠ **`req.venue` is a ROUTING string, not necessarily the venue whose row applies.** This
/// function reads it because for every request this workspace builds the two are the same, but a
/// caller that has ALREADY resolved which engine the request routed to knows better and must say
/// so through [`preflight_order_at`] — see that function's doc for what goes wrong here otherwise
/// (the first bullet of THE COMPAT LAW above, "an UNKNOWN venue string passes untouched", turns
/// from a compatibility affordance into a silent bypass of every check below).
pub fn preflight_order(req: &OrderRequest) -> Result<(), PreflightDeny> {
    preflight_order_at(req, &req.venue, None)
}

/// [`preflight_order`] with the capability venue supplied EXPLICITLY: `venue` is the canonical
/// exchange id of the engine this request routed to (`Engine::venue`), which is the only
/// thing the declared [`VenueCapabilities`] row is a fact about.
///
/// **Why a caller would ever need this.** `OrderRequest::venue` does two jobs at once — it is what
/// `vike_core`'s `CoreThread::engine_index_for_route_key` routes on AND what selects the caps row
/// here. Those are the two jobs the canonical/routing split separated on `Engine`, and
/// this payload has only one field for them, so the moment a request has to carry a per-ACCOUNT
/// routing key the caps lookup reads a string `vike_model::VENUES` does not contain — and
/// [`preflight_order`]'s unknown-venue affordance then returns `Ok(())` for it. That is not
/// `VenueCapabilities::UNSUPPORTED` (which would refuse loudly): it is EVERY check below skipped, silently,
/// for one account of one exchange. `vike_core` therefore resolves the engine first and passes
/// `Engine::venue` here.
///
/// Identical to [`preflight_order`] whenever `venue == req.venue` and `tier` is `None`, which is
/// every request in this tree but a paper book's.
///
/// **`tier` is the engine's STATED tier** (`vike_execution::Engine::stated_tier`), and it
/// relaxes exactly ONE axis: `Some(AccountTier::Paper)` skips [`VenueCapabilities::reduce_only_kinds`]. A
/// paper book's orders go to the simulator and never reach the venue's adapter, and the row's
/// reduce-only kinds are the refusals THAT ADAPTER makes by name (IG places a reduce-only market
/// only), so judging a simulated book by them refuses
/// what the simulator holds (a bracket's exits). Every other axis is the venue's trading rules,
/// which the simulator models, and stays enforced. `None` (an engine nobody told, so not PROVEN
/// a simulator), `Some(Demo)` and `Some(Live)` are judged by the whole row, as they always were.
/// The cost: a strategy that works on paper through this relaxation is refused on the demo and
/// live account; the caller that admits one calls this again with `None` to learn what the live
/// adapter would say (`vike_core`'s `CoreThread::note_paper_admit`) and says so, once.
///
/// It judges ONE order. What an order list's venue says of the list as a whole is
/// [`order_list_refusal`], which the caller asks beside this on every leg.
pub fn preflight_order_at(
    req: &OrderRequest,
    venue: &str,
    tier: Option<AccountTier>,
) -> Result<(), PreflightDeny> {
    // An UNRECOGNISED kind is refused FIRST, on every venue (a paper/sim id and a combo
    // included): it has no meaning anywhere. Only the stored door mints one (a journal record or
    // a venue's own text on an adopted order, `OrderType::from_stored`), and every engine and
    // most adapters would otherwise coerce it into a MARKET order (`OrderResting::from_request`'s
    // else branch; alpaca, oanda, ibkr, ig and the binance perp lane did exactly that).
    if let OrderType::Unrecognized(text) = &req.order_type {
        return Err(PreflightDeny::OrderKind { kind: text.clone(), venue: venue.to_string() });
    }
    if !vike_model::VENUES.contains(&venue) || !req.combo_legs.is_empty() {
        return Ok(());
    }
    let capabilities = capabilities_for(venue);
    // Allocation-free classification: the row's kinds stay lowercase strings, compared against the
    // canonical word (`OrderType::as_str`) — this runs on EVERY order submit, including the
    // SpreadMaker's per-tick requote path, so no `String` is minted on the (overwhelmingly common)
    // accepted path; an owned `kind` is built ONLY when a refusal actually happens.
    let kind = req.order_type.as_str();
    // A PERCENT trail is judged on its own axis, reduce-only included (a row lists a kind there only
    // where its reduce-only exit works too), so a venue's price trail and its percent trail open
    // apart. One discriminant load for every order without one.
    let percent = req.trail_percent.is_some();
    let kinds =
        if percent { capabilities.percent_trail_kinds } else { capabilities.supported_order_kinds };
    if !kinds.contains(&kind) {
        let kind = kind.to_string();
        return Err(if req.order_type.is_trigger() {
            PreflightDeny::Trigger { kind, venue: venue.to_string() }
        } else {
            PreflightDeny::OrderKind { kind, venue: venue.to_string() }
        });
    }
    // One bool load for every order that is not reduce-only; a reduce-only one costs a scan of the
    // row's few kind words, and nothing is allocated unless it is refused. The tier is compared
    // last, only for an order the row would refuse (see the function's doc).
    if req.reduce_only
        && !percent
        && !capabilities.reduce_only_kinds.contains(&kind)
        && tier != Some(AccountTier::Paper)
    {
        return Err(PreflightDeny::ReduceOnly { kind: kind.to_string(), venue: venue.to_string() });
    }
    // The TIF axis is judged on the limit path, and on a MARKET order (or a market-to-limit) whose
    // lifetime is an auction (market-on-open means something there; an adapter that does not wire it
    // would send the order now). Two enum compares for every other order.
    let time_in_force_path = req.order_type == OrderType::Limit
        || (matches!(req.order_type, OrderType::Market | OrderType::MarketToLimit)
            && req.time_in_force.is_auction());
    if time_in_force_path && !capabilities.accepted_tifs.contains(&req.time_in_force) {
        return Err(PreflightDeny::IbapiTimeInForce {
            tif: req.time_in_force,
            venue: venue.to_string(),
        });
    }
    // A lifetime the adapter would run as ANOTHER one is refused by name too (the table's `Coerced`
    // rows). Only a non-default lifetime can be coerced, so the default `Gtc` order costs one enum
    // compare; the lookup is a few string compares for the rare named lifetime.
    if time_in_force_path
        && req.time_in_force != TimeInForce::Gtc
        && let crate::venue_time_in_force::TimeInForceOutcome::Coerced { to, .. } =
            crate::venue_time_in_force::venue_time_in_force(venue, req.time_in_force)
    {
        return Err(PreflightDeny::TimeInForceCoerced {
            tif: req.time_in_force,
            becomes: to,
            venue: venue.to_string(),
        });
    }
    // One enum compare for every order that is not post-only. A post-only order that is not a
    // limit order is the one TIF that has meaning off the limit path, and every adapter would drop
    // it there (see `PreflightDeny::PostOnlyNeedsLimit`).
    if req.time_in_force == TimeInForce::PostOnly && req.order_type != OrderType::Limit {
        return Err(PreflightDeny::PostOnlyNeedsLimit {
            kind: kind.to_string(),
            venue: venue.to_string(),
        });
    }
    if let Some(mode) = req.margin_mode
        && !capabilities.margin_modes.contains(&mode)
    {
        return Err(PreflightDeny::MarginMode { mode, venue: venue.to_string() });
    }
    // One discriminant load for every order that shows its whole size. A display of ZERO hides the
    // whole order: the `hidden` attribute's, below, not the visible-slice axis.
    let hidden = req.display_quantity == Some(0.0);
    if req.display_quantity.is_some() && !hidden && !capabilities.supports_display_quantity {
        return Err(PreflightDeny::DisplayQuantity { venue: venue.to_string() });
    }
    // One discriminant load per key for every order that carries no attribute.
    if hidden || req.attributes.is_some() || req.adjust.is_some() {
        let a = req.attributes.unwrap_or_default();
        let asked = [
            (a.all_or_none, "all_or_none"),
            (a.sweep_to_fill, "sweep_to_fill"),
            (a.discretionary_amount.is_some(), "discretionary"),
            (hidden, "hidden"),
            (req.adjust.is_some(), "adjust"),
        ];
        if let Some(&(_, attribute)) =
            asked.iter().find(|(on, word)| *on && !capabilities.order_attributes.contains(word))
        {
            return Err(PreflightDeny::Attribute { attribute, venue: venue.to_string() });
        }
    }
    Ok(())
}

/// The engine's own route for an order kind the venue does not hold: one plain limit at a time, worked
/// by `vike_execution::emulated`'s law (decision 0275). The owner sees ONE order; the venue sees plain limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderEmulationKind {
    /// A `pegged` order: one limit re-priced on every move of its anchor.
    Chase,
    /// A limit with a visible size (`display_quantity` above zero): one slice of the visible size at a
    /// time, at the order's price.
    Iceberg,
}

impl OrderEmulationKind {
    /// The route's word: the published `emulated` field and the tag the law's own child carries.
    pub fn as_word(self) -> &'static str {
        match self {
            OrderEmulationKind::Chase => "chase",
            OrderEmulationKind::Iceberg => "iceberg",
        }
    }

    /// The ONE note an accepted emulated order carries (decision 0236's shape), on the submit answer and
    /// the core's ring line alike.
    pub fn note(self, venue: &str) -> String {
        match self {
            OrderEmulationKind::Chase => format!(
                "EMULATED: {venue} holds no such pegged order; the node works it as plain limits, one \
                 at a time, re-priced on every move of its anchor"
            ),
            OrderEmulationKind::Iceberg => format!(
                "EMULATED: {venue} shows no visible size; the node works it as plain limits of the \
                 visible size, one at a time, at its price"
            ),
        }
    }
}

/// The venues no emulation places a child on, each with its reason (keyed by roster id,
/// `vike_model::VENUES`). An order there keeps the row's refusal.
pub const EMULATION_REFUSED_VENUES: &[(&str, &str)] = &[
    (
        "polymarket",
        "no emulated order places children on polymarket: it has no demo account to prove them on",
    ),
    (
        "aster",
        "no emulated order places children on aster: it has no demo account and its reads are \
         authenticated mainnet reads",
    ),
];

/// **Native, emulated or neither** (decision 0275): the route of `req` on `venue`'s row at `tier`.
///
/// * `None` for every order that is neither `pegged` nor a limit with a visible size above zero (one
///   enum compare and one `Option` test), and for one the venue holds natively: a `pegged` order whose
///   anchor is on the row's [`VenueCapabilities::peg_anchors`] (with `pegged` in its kinds) on an engine that is
///   not a paper book (no book vike runs holds a peg), a visible size where the row says
///   [`VenueCapabilities::supports_display_quantity`].
/// * `None` too where no honest emulation exists, so the row's refusal stands in its own words: a hidden
///   order (a display of zero: a slice of zero rests nothing), an IOC or FOK lifetime (nothing rests), a
///   leg of a bracket or an order list, a pegged order that also states a visible size, a pegged order on a
///   roster venue that cannot amend a resting limit in place (`venue_amend_mode::amend_mode`: a chase
///   re-prices its child in place), an attribute on a visible-size order, and a venue of
///   [`EMULATION_REFUSED_VENUES`].
/// * `Some` otherwise. A venue off the roster (a sim or test id) holds no peg, so its peg is emulated, and
///   its visible size goes to its own book as before.
///
/// The caller that emulates judges the order as its CHILD would be judged ([`emulated_child`] through
/// [`preflight_order_at`]), so every axis that refuses a child refuses at accept. It reads the request and
/// the table alone, so a replay core decides the same.
pub fn emulation_of(
    req: &OrderRequest,
    venue: &str,
    tier: Option<AccountTier>,
) -> Option<OrderEmulationKind> {
    let pegged = req.order_type == OrderType::Pegged;
    let shown = req.order_type == OrderType::Limit && req.display_quantity.is_some_and(|d| d > 0.0);
    if !pegged && !shown {
        return None;
    }
    if matches!(req.time_in_force, TimeInForce::Ioc | TimeInForce::Fok)
        || req.parent_order_id.is_some()
        || !req.linked_order_ids.is_empty()
        || req.order_list_id.is_some()
        || req.contingency_type.is_some()
        || !req.combo_legs.is_empty()
        || EMULATION_REFUSED_VENUES.iter().any(|(v, _)| v.eq_ignore_ascii_case(venue))
    {
        return None;
    }
    let roster = vike_model::VENUES.contains(&venue);
    let capabilities = capabilities_for(venue);
    if pegged {
        let anchor = req.peg.map(|p| p.anchor);
        let native = roster
            && tier != Some(AccountTier::Paper)
            && capabilities.supported_order_kinds.contains(&"pegged")
            && anchor.is_some_and(|a| capabilities.peg_anchors.contains(&a));
        // A chase re-prices its child in place: a roster venue that amends a resting limit only by
        // cancel-then-new would end the child at every move, so no honest chase exists there.
        let amends = !roster
            || crate::venue_amend_mode::amend_mode(
                venue,
                crate::venue_amend_mode::AmendKind::Limit,
            ) == crate::venue_amend_mode::AmendMode::Native;
        return (!native && amends && req.peg.is_some() && req.display_quantity.is_none())
            .then_some(OrderEmulationKind::Chase);
    }
    (roster && !capabilities.supports_display_quantity && req.attributes.is_none())
        .then_some(OrderEmulationKind::Iceberg)
}

/// The plain limit an emulated `parent` works through, before the law gives it a side, size and price:
/// the parent's venue, symbol, account, lifetime, flags and margin mode, as a `limit` with no peg, no
/// visible size and no order client id (each child is minted). [`preflight_order_at`] judges the parent
/// by this shape at accept.
pub fn emulated_child(parent: &OrderRequest) -> OrderRequest {
    OrderRequest {
        order_client_id: String::new(),
        order_type: OrderType::Limit,
        display_quantity: None,
        peg: None,
        ..parent.clone()
    }
}

/// A venue whose resting exit must cover a WHOLE trade or position, so it cannot hold an OUO group (a
/// ladder: a target's fill shrinks the stop, the stop's fill shrinks the targets). Its row cannot say
/// so: every kind it rests is placed reduce-only and amended, but only at the size of whole trades or
/// positions. [`order_list_refusal`] refuses a list with an OUO leg here WHOLE, before its entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WholeExitVenue {
    /// the venue id
    pub venue: &'static str,
    /// why the venue cannot resize an exit, in the refusal's words
    pub why: &'static str,
    /// a fragment of the BRIDGE's own refusal, which that bridge's cross-pin test finds in what its
    /// adapter says
    pub bridge_words: &'static str,
    /// what would reopen the venue to an OUO group
    pub reopen: &'static str,
}

/// The pinned exception to the DERIVED rules of [`order_list_refusal`] (wave 4 of the order kinds:
/// fxcm, dukascopy and ctrader refuse an OUO group or a ladder whole). Each row quotes its bridge's
/// refusal and says what reopens it. A venue not listed is judged by its row alone. The cross-pins,
/// one per row, each finding `bridge_words` in what its bridge says:
/// - fxcm: `crates/bridges/fxcm/src/execution/exit/exit_tests.rs`'s
///   `a_ladder_target_is_a_part_of_a_trade_in_the_words_the_whole_exit_row_quotes` runs the exit
///   planner on a partial target;
/// - dukascopy: `crates/bridges/dukascopy/src/lib.rs`'s
///   `the_whole_exit_row_quotes_the_sidecars_refusal_of_a_part_of_an_order` reads the sidecar's
///   `StopPlan.java`;
/// - ctrader: `crates/bridges/ctrader/src/execution/positions_tests.rs`'s
///   `a_ladders_stop_leaves_no_room_for_its_replacement_in_the_words_the_whole_exit_row_quotes` binds
///   a ladder's exits to one position.
pub const WHOLE_EXIT_VENUES: &[WholeExitVenue] = &[
    WholeExitVenue {
        venue: "fxcm",
        why: "an exit is bound to a WHOLE trade and a trade carries one stop and one take-profit, \
              so neither a partial target nor a stop shrunk by a target's fill can be placed",
        bridge_words: "ORA-20134",
        reopen: "a partial target the venue can hold: the stop's shrink is there (MEASURED \
                 2026-11-09: a PARTIAL close leaves a trade's stop and take-profit on the remainder, \
                 the amount cut, and the bridge reports the cut), but a take-profit for a part of a \
                 trade is refused (ORA-20134)",
    },
    WholeExitVenue {
        venue: "dukascopy",
        why: "a stop-loss and a take-profit are a price on a WHOLE order, one of each per order, so \
              a partial target lands inside the entry's order and is refused",
        bridge_words: "covers a whole order",
        reopen: "a ladder whose entry is placed as one JForex order per target, each carrying its \
                 own stop-loss and take-profit, built and demo-proven",
    },
    WholeExitVenue {
        venue: "ctrader",
        why: "an exit is bound to ONE position and exits of one kind never cover more than it \
              holds, so the stop sized to the whole position has no room beside it for its \
              resized replacement",
        bridge_words: "covering its whole volume",
        reopen: "the venue's native ladder (`advancedTakeProfit` on a newer Open API proto) wired \
                 by the bridge",
    },
];

/// **What an order list's venue says of it as a WHOLE**: `None` when the list may go, else the words
/// that refuse it, all before its entry is sent (decision 0219's shape: a group whose held exit the
/// venue cannot rest, or whose links it cannot follow, fills its entry and then strands the
/// position). The caller still runs [`preflight_order_at`] on every leg; this is what no single leg
/// says:
///
/// 1. every exit leg (`reduce_only`) is of a kind the row places reduce-only
///    ([`VenueCapabilities::reduce_only_kinds`]): an exit is HELD until its parent fills and is never judged
///    again;
/// 2. every OUO leg ([`ContingencyType::Ouo`]: a sibling's fill shrinks it) is of a kind the venue
///    can change ([`amend_mode_for`] is not [`AmendMode::Refused`]), or the fill would leave it at
///    its old size;
/// 3. every OUO leg the node would replace CANCEL-FIRST (a target limit, or any leg but a reduce-only
///    trigger order: `crates/vike-execution/src/amend.rs`'s `AmendPlacement::of`) is amended IN PLACE
///    ([`AmendMode::Native`]): its shrink is an amend, and decision 0244 refuses a cancel-first
///    replacement of a linked leg. A reduce-only stop may be [`AmendMode::CancelThenNew`]: it is
///    replaced new-first;
/// 4. a list with an OUO leg is not on a [`WHOLE_EXIT_VENUES`] venue.
///
/// The first three are DERIVED from the rows, so a row that moves moves this answer; the fourth is the
/// pinned exception. The words name the first refused leg by its index. An unknown venue (a paper or
/// sim id) is `None`, as [`preflight_order`]'s compat law says. Every rule is a LIVE adapter's fact:
/// a stated `paper` engine's book holds the exits and resizes them in place, so its caller does not
/// ask, as [`preflight_order_at`] skips the reduce-only kinds for it (decision 0201).
#[must_use]
pub fn order_list_refusal(venue: &str, legs: &[OrderRequest]) -> Option<String> {
    if !vike_model::VENUES.contains(&venue) {
        return None;
    }
    let capabilities = capabilities_for(venue);
    for (i, leg) in legs.iter().enumerate() {
        let kind = leg.order_type.as_str();
        if leg.reduce_only && !capabilities.reduce_only_kinds.contains(&kind) {
            return Some(format!("REDUCE_ONLY_UNSUPPORTED: leg={i} kind={kind} venue={venue}"));
        }
        if leg.contingency_type != Some(ContingencyType::Ouo) {
            continue;
        }
        match amend_mode_for(venue, &leg.order_type) {
            Some(AmendMode::Refused) => {
                return Some(format!(
                    "OUO_AMEND_REFUSED: leg={i} kind={kind} venue={venue}: the venue cannot change \
                     this kind of resting order, so a sibling's fill could not shrink it"
                ));
            }
            // `Placement::of`'s rule: only a reduce-only trigger order is replaced new-first.
            Some(AmendMode::CancelThenNew) if !(leg.reduce_only && leg.order_type.is_trigger()) => {
                return Some(format!(
                    "OUO_TARGET_AMEND_NOT_IN_PLACE: leg={i} kind={kind} venue={venue}: the venue \
                     changes this kind of resting order only by cancelling it first, so a \
                     sibling's fill could not shrink it in place"
                ));
            }
            _ => {}
        }
    }
    let ouo = legs.iter().position(|l| l.contingency_type == Some(ContingencyType::Ouo))?;
    let row = WHOLE_EXIT_VENUES.iter().find(|w| w.venue == venue)?;
    Some(format!("OUO_WHOLE_EXIT: leg={ouo} venue={venue}: {}", row.why))
}

/// Why an option EXERCISE on `venue` is refused, in that venue's own words (wave 6 of the order
/// kinds): no venue takes one yet, so every roster venue answers `Some`, deribit, ibkr and alpaca
/// with their own reason and the rest because this build trades no option there. An id off the roster
/// (a paper or sim engine's) is `None`, as [`order_list_refusal`]'s compat law, and its caller says it
/// in its own words. The first venue that takes an exercise turns this into a capability row (the
/// playbook's step 2).
#[must_use]
pub fn exercise_refusal(venue: &str) -> Option<String> {
    if !vike_model::VENUES.contains(&venue) {
        return None;
    }
    Some(match venue {
        "deribit" => "deribit's options are European and cash settled: each one exercises itself \
                      at expiry and none is exercised before it (decision 0233)"
            .to_string(),
        "ibkr" => "an ibkr option stays refused with its design (decision 0234), so no option is \
                   held there to exercise"
            .to_string(),
        "alpaca" => "alpaca is deferred: its exercise call is not built".to_string(),
        other => format!("{other} trades no option in this build, so there is nothing to exercise"),
    })
}

/// A bridge's LAST-LINE kind floor, under the core's [`preflight_order_at`]: why `venue`'s adapter
/// builds no order of `req`'s kind, in words, or `None`. The kinds built are the row's
/// `supported_order_kinds`, so a bridge's builder never re-spells the set and never sends a kind it
/// does not build as another one. A venue's own extras (a kind built only with certain terms, an
/// auction lifetime it has no order for) stay that bridge's refusals beside this one. A venue off
/// the roster has no row and refuses every kind (a bridge passes its own roster id).
#[must_use]
pub fn unbuilt_kind_refusal(venue: &str, req: &OrderRequest) -> Option<String> {
    let kind = req.order_type.as_str();
    (!capabilities_for(venue).supported_order_kinds.contains(&kind))
        .then(|| format!("{venue} builds no `{kind}` order; refused, nothing was sent"))
}

#[path = "venue_capabilities_tests.rs"]
#[cfg(test)]
mod venue_capabilities_tests;
