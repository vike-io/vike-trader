//! "Arbitrary input never panics" harness for the cTrader decoders `framing_props.rs` does NOT
//! reach. That file pins the length-prefix frame layer (`FrameReader`); this one covers everything
//! a decoded frame feeds, through the crate's PUBLIC API:
//!
//! - the protobuf payload layer: arbitrary bytes, and structurally valid messages with bytes
//!   flipped / truncated, through `prost::Message::decode` for every inbound payload type
//!   `conn::on_inbound` and the reconcile client decode (`ProtoMessage`, spot / execution /
//!   reconcile / deal-list / trendbars / order-error / error responses);
//! - the pure mappers over prost messages built from ARBITRARY field values (the generated
//!   structs have public fields, so every `Option` is `None`/`Some` and every number is an edge
//!   case): `event_mapper::{spot_to_quote, spot_to_bars, trendbar_to_bar, deal_fill,
//!   exec_event_to_events, exec_event_is_terminal, lifecycle_ts, order_to_new_order}`,
//!   `recon_client::{parse_orders, parse_positions, parse_raw_positions, parse_fills}`,
//!   `positions::{tracked_position, reconcile_rows, plan_reduce, snap_close_volume,
//!   opposing_available}`, `symbols::SymbolMap`, `catalog::classify`;
//! - the state holders that fold decoded events: `positions::CloseTracker` and
//!   `positions::PositionBook`, driven by a short random operation sequence on ONE instance;
//! - the OAuth token body (`oauth::parse_token_response`) and the expiry arithmetic
//!   (`token_store::{expires_at_ms, refresh_due}`).
//!
//! The property is TOTALITY: a hostile or truncated frame may decode to nothing, but it must never
//! panic the actor thread (a dead actor is a venue that silently goes quiet), and one event must
//! never fabricate an event flood. The private dispatchers in `conn.rs` (`on_inbound`,
//! `update_position_map`, `map_close_event`, `rebuild_position_book`) are NOT reachable from an
//! integration test; the protobuf decode + the public mappers they call are what is covered here.
//!
//! Volumes fed to `plan_reduce` / `CloseTracker` reach 2^61, so eight legs overflow a plain `i64`
//! sum: those wire sums saturate (they used to panic in a dev/test build). The order quantity is
//! bounded to 1e12 units: the OUTBOUND `round_volume` multiplies a stepped quantity in plain `i64`,
//! and that quantity comes from the caller's own request, not from the wire.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it.
//!
//! Features: the crate has none.

use proptest::prelude::*;
use prost::Message;
use serde_json::{Value, json};
use vike_ctrader::catalog::classify;
use vike_ctrader::event_mapper::{
    deal_fill, exec_event_is_terminal, exec_event_to_events, interval_for_trendbar_period,
    lifecycle_ts, order_to_new_order, spot_to_bars, spot_to_quote, trendbar_period_for_interval,
    trendbar_period_ms, trendbar_to_bar,
};
use vike_ctrader::oauth::parse_token_response;
use vike_ctrader::positions::{
    CloseEmit, ClosePlan, CloseTracker, PositionBook, TrackedPosition, is_open, opposing_available,
    plan_reduce, reconcile_rows, snap_close_volume, tracked_position,
};
use vike_ctrader::proto::{
    ProtoMessage, ProtoOaDeal, ProtoOaDealListRes, ProtoOaErrorRes, ProtoOaExecutionEvent,
    ProtoOaGetTrendbarsRes, ProtoOaLightSymbol, ProtoOaOrder, ProtoOaOrderErrorEvent,
    ProtoOaPosition, ProtoOaReconcileRes, ProtoOaSpotEvent, ProtoOaSymbol, ProtoOaTradeData,
    ProtoOaTrendbar, ProtoOaTrendbarPeriod,
};
use vike_ctrader::recon_client::{parse_fills, parse_orders, parse_positions, parse_raw_positions};
use vike_ctrader::symbols::SymbolMap;
use vike_ctrader::token_store::{expires_at_ms, refresh_due};
use vike_model::OrderRequest;

/// One execution event legitimately emits a fill and its wrap (the dual-publish contract).
const MAX_EVENTS_PER_EXEC_EVENT: usize = 2;

/// Wire volumes up to 2^61: eight of them overflow a plain `i64` sum, which the planner and the
/// tracker saturate (see the module doc).
const MAX_VOLUME: i64 = 1 << 61;

// ---------------------------------------------------------------------------------------------
// Scalar generators
// ---------------------------------------------------------------------------------------------

/// An i64 wire number: zero, +-1, the extremes, realistic epoch-ms / volumes, anything.
fn i64s() -> BoxedStrategy<i64> {
    prop_oneof![
        Just(0i64),
        Just(1i64),
        Just(-1i64),
        Just(i64::MIN),
        Just(i64::MAX),
        -5i64..5_000,
        1_600_000_000_000i64..1_800_000_000_000,
        any::<i64>(),
    ]
    .boxed()
}

fn u64s() -> BoxedStrategy<u64> {
    prop_oneof![Just(0u64), Just(1u64), Just(u64::MAX), 0u64..10_000_000, any::<u64>()].boxed()
}

fn u32s() -> BoxedStrategy<u32> {
    prop_oneof![Just(0u32), Just(u32::MAX), 0u32..40, any::<u32>()].boxed()
}

/// f64 including every non-finite and extreme value.
fn f64s() -> BoxedStrategy<f64> {
    prop_oneof![
        Just(0.0f64),
        Just(-0.0f64),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(f64::MAX),
        Just(f64::MIN_POSITIVE),
        -1.0e6f64..1.0e6,
        any::<f64>(),
    ]
    .boxed()
}

/// A protobuf enum number: every defined value, then out-of-range ones.
fn enum_i32() -> BoxedStrategy<i32> {
    prop_oneof![0i32..16, Just(-1i32), Just(i32::MAX), Just(i32::MIN), any::<i32>()].boxed()
}

/// A symbol id the generated `SymbolMap`s know (ids 0..4), so lookups hit as well as miss.
fn small_id() -> BoxedStrategy<i64> {
    (0i64..5).boxed()
}

fn volume() -> BoxedStrategy<i64> {
    prop_oneof![Just(0i64), -5i64..5, 0i64..MAX_VOLUME, -MAX_VOLUME..MAX_VOLUME].boxed()
}

fn short_text() -> BoxedStrategy<String> {
    prop_oneof![Just(String::new()), "[ -~]{0,8}", "\\PC{0,6}"].boxed()
}

fn symbol_name() -> BoxedStrategy<String> {
    prop_oneof![
        Just("EURUSD".to_string()),
        Just("US500".to_string()),
        Just("Apple.US".to_string()),
        Just(String::new()),
        "[A-Z]{6}",
        Just("\u{00c4}BCDEF".to_string()),
        "\\PC{0,8}",
    ]
    .boxed()
}

// ---------------------------------------------------------------------------------------------
// Protobuf message generators (struct literals: public fields, `..Default::default()`)
// ---------------------------------------------------------------------------------------------

fn trade_data() -> BoxedStrategy<ProtoOaTradeData> {
    (small_id(), volume(), enum_i32(), prop::option::of(i64s()), prop::option::of(short_text()))
        .prop_map(|(symbol_id, volume, trade_side, open_timestamp, label)| ProtoOaTradeData {
            symbol_id,
            volume,
            trade_side,
            open_timestamp,
            label,
            ..Default::default()
        })
        .boxed()
}

fn order() -> BoxedStrategy<ProtoOaOrder> {
    (
        (small_id(), trade_data(), enum_i32(), enum_i32()),
        (
            prop::option::of(f64s()),
            prop::option::of(volume()),
            prop::option::of(i64s()),
            prop::option::of(f64s()),
            prop::option::of(f64s()),
        ),
        (prop::option::of(short_text()), prop::option::of(small_id())),
    )
        .prop_map(
            |(
                (order_id, trade_data, order_type, order_status),
                (
                    execution_price,
                    executed_volume,
                    utc_last_update_timestamp,
                    limit_price,
                    stop_price,
                ),
                (client_order_id, position_id),
            )| ProtoOaOrder {
                order_id,
                trade_data,
                order_type,
                order_status,
                execution_price,
                executed_volume,
                utc_last_update_timestamp,
                limit_price,
                stop_price,
                client_order_id,
                position_id,
                ..Default::default()
            },
        )
        .boxed()
}

fn deal() -> BoxedStrategy<ProtoOaDeal> {
    (
        (small_id(), small_id(), small_id(), volume(), volume(), small_id()),
        (
            i64s(),
            i64s(),
            prop::option::of(f64s()),
            enum_i32(),
            enum_i32(),
            prop::option::of(i64s()),
            prop::option::of(u32s()),
        ),
    )
        .prop_map(
            |(
                (deal_id, order_id, position_id, vol, filled_volume, symbol_id),
                (
                    create_timestamp,
                    execution_timestamp,
                    execution_price,
                    trade_side,
                    deal_status,
                    commission,
                    money_digits,
                ),
            )| ProtoOaDeal {
                deal_id,
                order_id,
                position_id,
                volume: vol,
                filled_volume,
                symbol_id,
                create_timestamp,
                execution_timestamp,
                execution_price,
                trade_side,
                deal_status,
                commission,
                money_digits,
                ..Default::default()
            },
        )
        .boxed()
}

fn position() -> BoxedStrategy<ProtoOaPosition> {
    (small_id(), trade_data(), enum_i32(), prop::option::of(f64s()), prop::option::of(i64s()))
        .prop_map(|(position_id, trade_data, position_status, price, ts)| ProtoOaPosition {
            position_id,
            trade_data,
            position_status,
            price,
            utc_last_update_timestamp: ts,
            ..Default::default()
        })
        .boxed()
}

fn exec_event() -> BoxedStrategy<ProtoOaExecutionEvent> {
    (
        enum_i32(),
        prop::option::weighted(0.7, position()),
        prop::option::weighted(0.8, order()),
        prop::option::weighted(0.8, deal()),
        prop::option::of(short_text()),
        i64s(),
    )
        .prop_map(|(execution_type, position, order, deal, error_code, ctid)| {
            ProtoOaExecutionEvent {
                ctid_trader_account_id: ctid,
                execution_type,
                position,
                order,
                deal,
                error_code,
                ..Default::default()
            }
        })
        .boxed()
}

fn trendbar() -> BoxedStrategy<ProtoOaTrendbar> {
    (
        volume(),
        prop::option::weighted(0.8, enum_i32()),
        prop::option::weighted(0.8, i64s()),
        prop::option::weighted(0.8, u64s()),
        prop::option::weighted(0.8, u64s()),
        prop::option::weighted(0.8, u64s()),
        prop::option::weighted(0.8, u32s()),
    )
        .prop_map(|(volume, period, low, delta_open, delta_close, delta_high, minutes)| {
            ProtoOaTrendbar {
                volume,
                period,
                low,
                delta_open,
                delta_close,
                delta_high,
                utc_timestamp_in_minutes: minutes,
            }
        })
        .boxed()
}

fn spot_event() -> BoxedStrategy<ProtoOaSpotEvent> {
    (
        small_id(),
        prop::option::weighted(0.8, u64s()),
        prop::option::weighted(0.8, u64s()),
        prop::collection::vec(trendbar(), 0..4),
        prop::option::of(i64s()),
    )
        .prop_map(|(symbol_id, bid, ask, trendbar, timestamp)| ProtoOaSpotEvent {
            symbol_id,
            bid,
            ask,
            trendbar,
            timestamp,
            ..Default::default()
        })
        .boxed()
}

fn reconcile_res() -> BoxedStrategy<ProtoOaReconcileRes> {
    (i64s(), prop::collection::vec(position(), 0..5), prop::collection::vec(order(), 0..5))
        .prop_map(|(ctid, position, order)| ProtoOaReconcileRes {
            ctid_trader_account_id: ctid,
            position,
            order,
            ..Default::default()
        })
        .boxed()
}

fn deal_list_res() -> BoxedStrategy<ProtoOaDealListRes> {
    (i64s(), prop::collection::vec(deal(), 0..5), any::<bool>())
        .prop_map(|(ctid, deal, has_more)| ProtoOaDealListRes {
            ctid_trader_account_id: ctid,
            deal,
            has_more,
            ..Default::default()
        })
        .boxed()
}

fn trendbars_res() -> BoxedStrategy<ProtoOaGetTrendbarsRes> {
    (i64s(), enum_i32(), prop::collection::vec(trendbar(), 0..5), prop::option::of(small_id()))
        .prop_map(|(ctid, period, trendbar, symbol_id)| ProtoOaGetTrendbarsRes {
            ctid_trader_account_id: ctid,
            period,
            trendbar,
            symbol_id,
            ..Default::default()
        })
        .boxed()
}

/// A `SymbolMap` from random light + full symbol lists over ids 0..4 (digits anywhere in i32,
/// volume grids anywhere in i64).
fn symbol_map() -> BoxedStrategy<SymbolMap> {
    let light =
        prop::collection::vec((small_id(), prop::option::weighted(0.8, symbol_name())), 0..6);
    let full = prop::collection::vec(
        (
            small_id(),
            prop_oneof![-3i32..12, any::<i32>()],
            prop::option::of(i64s()),
            prop::option::of(i64s()),
            prop::option::of(i64s()),
            prop::option::of(i64s()),
        ),
        0..6,
    );
    (light, full)
        .prop_map(|(light, full)| {
            let light: Vec<ProtoOaLightSymbol> = light
                .into_iter()
                .map(|(symbol_id, symbol_name)| ProtoOaLightSymbol {
                    symbol_id,
                    symbol_name,
                    ..Default::default()
                })
                .collect();
            let full: Vec<ProtoOaSymbol> = full
                .into_iter()
                .map(|(symbol_id, digits, lot_size, min_volume, step_volume, max_volume)| {
                    ProtoOaSymbol {
                        symbol_id,
                        digits,
                        pip_position: 0,
                        lot_size,
                        min_volume,
                        step_volume,
                        max_volume,
                        ..Default::default()
                    }
                })
                .collect();
            SymbolMap::from_symbols(&light, &full)
        })
        .boxed()
}

fn tracked_position_strategy() -> BoxedStrategy<TrackedPosition> {
    (
        small_id(),
        prop_oneof![Just(1i32), Just(-1i32), Just(0i32), any::<i32>()],
        0..MAX_VOLUME,
        i64s(),
    )
        .prop_map(|(symbol_id, side, volume, open_ts)| TrackedPosition {
            symbol_id,
            side,
            volume,
            open_ts,
        })
        .boxed()
}

/// Byte flips (`(index, xor)`) plus an optional truncation point.
type Mutation = (Vec<(usize, u8)>, Option<usize>);

/// A mutation of an encoded message: byte flips plus an optional truncation.
fn mutation() -> BoxedStrategy<Mutation> {
    (prop::collection::vec((any::<usize>(), any::<u8>()), 0..4), prop::option::of(any::<usize>()))
        .boxed()
}

fn mutate(mut bytes: Vec<u8>, flips: &[(usize, u8)], cut: Option<usize>) -> Vec<u8> {
    if !bytes.is_empty() {
        for &(i, x) in flips {
            let n = bytes.len();
            bytes[i % n] ^= x;
        }
        if let Some(c) = cut {
            let n = bytes.len() + 1;
            bytes.truncate(c % n);
        }
    }
    bytes
}

// ---------------------------------------------------------------------------------------------
// What a decoded message feeds (the public halves of `conn::on_inbound` and the recon client)
// ---------------------------------------------------------------------------------------------

fn feed_spot(ev: &ProtoOaSpotEvent, symbols: &SymbolMap) -> Result<(), TestCaseError> {
    if let Some((_, bid, ask)) = spot_to_quote(ev, symbols) {
        prop_assert!(bid.is_finite() && bid >= 0.0 && ask.is_finite() && ask >= 0.0);
    }
    prop_assert!(spot_to_bars(ev, symbols).len() <= ev.trendbar.len());
    Ok(())
}

fn feed_exec(
    ev: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
) -> Result<(), TestCaseError> {
    let events = exec_event_to_events(ev, symbols, money_digits);
    prop_assert!(events.len() <= MAX_EVENTS_PER_EXEC_EVENT, "event flood: {}", events.len());
    let _ = exec_event_is_terminal(ev);
    let _ = lifecycle_ts(ev);
    let _ = deal_fill(ev, "coid", symbols, money_digits);
    if let Some(p) = &ev.position {
        let _ = tracked_position(p);
        let _ = is_open(p.position_status);
    }
    Ok(())
}

fn feed_reconcile(res: &ProtoOaReconcileRes, symbols: &SymbolMap) -> Result<(), TestCaseError> {
    prop_assert_eq!(parse_orders(&res.order, symbols).len(), res.order.len());
    prop_assert_eq!(parse_positions(&res.position, symbols).len(), res.position.len());
    for p in parse_raw_positions(&res.position, symbols) {
        prop_assert!(p.volume > 0 && (p.side == 1 || p.side == -1));
    }
    let (tracked, unreadable) = reconcile_rows(&res.position);
    prop_assert!(tracked.len() + unreadable <= res.position.len());
    Ok(())
}

fn feed_deals(res: &ProtoOaDealListRes, money_digits: u32) {
    for id in 0..5 {
        let _ = parse_fills(&res.deal, id, "EURUSD", money_digits);
    }
}

fn feed_trendbars(res: &ProtoOaGetTrendbarsRes) {
    // The `on_get_trendbars_res` body: period enum, then every trendbar through the mapper.
    if let Ok(period) = ProtoOaTrendbarPeriod::try_from(res.period) {
        let _ = interval_for_trendbar_period(period);
        let _ = trendbar_period_ms(period);
    }
    let _: Vec<_> = res.trendbar.iter().filter_map(trendbar_to_bar).collect();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Arbitrary bytes into EVERY inbound payload decoder: a `Result`, never a panic; whatever
    /// decodes is fed through the mappers.
    #[test]
    fn arbitrary_bytes_decode_and_map(
        bytes in prop::collection::vec(any::<u8>(), 0..1024),
        symbols in symbol_map(),
        money_digits in u32s(),
    ) {
        let _ = ProtoMessage::decode(bytes.as_slice());
        let _ = ProtoOaErrorRes::decode(bytes.as_slice());
        let _ = ProtoOaOrderErrorEvent::decode(bytes.as_slice());
        if let Ok(ev) = ProtoOaSpotEvent::decode(bytes.as_slice()) {
            feed_spot(&ev, &symbols)?;
        }
        if let Ok(ev) = ProtoOaExecutionEvent::decode(bytes.as_slice()) {
            feed_exec(&ev, &symbols, money_digits)?;
        }
        if let Ok(res) = ProtoOaReconcileRes::decode(bytes.as_slice()) {
            feed_reconcile(&res, &symbols)?;
        }
        if let Ok(res) = ProtoOaDealListRes::decode(bytes.as_slice()) {
            feed_deals(&res, money_digits);
        }
        if let Ok(res) = ProtoOaGetTrendbarsRes::decode(bytes.as_slice()) {
            feed_trendbars(&res);
        }
    }

    /// Structurally valid messages, encoded, then corrupted (flips + truncation) and decoded: the
    /// shapes a real peer or a damaged stream produces, which random bytes almost never reach.
    #[test]
    fn corrupted_valid_messages_decode_and_map(
        spot in spot_event(),
        exec in exec_event(),
        reconcile in reconcile_res(),
        deals in deal_list_res(),
        bars in trendbars_res(),
        (flips, cut) in mutation(),
        symbols in symbol_map(),
        money_digits in u32s(),
    ) {
        let spot_bytes = mutate(spot.encode_to_vec(), &flips, cut);
        if let Ok(ev) = ProtoOaSpotEvent::decode(spot_bytes.as_slice()) {
            feed_spot(&ev, &symbols)?;
        }
        let exec_bytes = mutate(exec.encode_to_vec(), &flips, cut);
        if let Ok(ev) = ProtoOaExecutionEvent::decode(exec_bytes.as_slice()) {
            feed_exec(&ev, &symbols, money_digits)?;
        }
        let reconcile_bytes = mutate(reconcile.encode_to_vec(), &flips, cut);
        if let Ok(res) = ProtoOaReconcileRes::decode(reconcile_bytes.as_slice()) {
            feed_reconcile(&res, &symbols)?;
        }
        let deal_bytes = mutate(deals.encode_to_vec(), &flips, cut);
        if let Ok(res) = ProtoOaDealListRes::decode(deal_bytes.as_slice()) {
            feed_deals(&res, money_digits);
        }
        let bar_bytes = mutate(bars.encode_to_vec(), &flips, cut);
        if let Ok(res) = ProtoOaGetTrendbarsRes::decode(bar_bytes.as_slice()) {
            feed_trendbars(&res);
        }
    }

    /// The same mappers over UNcorrupted messages whose every field is an edge case.
    #[test]
    fn mappers_are_total_over_arbitrary_messages(
        spot in spot_event(),
        exec in exec_event(),
        reconcile in reconcile_res(),
        deals in deal_list_res(),
        bars in trendbars_res(),
        symbols in symbol_map(),
        money_digits in u32s(),
    ) {
        feed_spot(&spot, &symbols)?;
        feed_exec(&exec, &symbols, money_digits)?;
        feed_reconcile(&reconcile, &symbols)?;
        feed_deals(&deals, money_digits);
        feed_trendbars(&bars);
    }

    /// A trendbar maps to a bar with finite prices, on the exact minute timestamp.
    #[test]
    fn trendbar_to_bar_is_finite(tb in trendbar()) {
        if let Some(bar) = trendbar_to_bar(&tb) {
            prop_assert!(bar.open.is_finite() && bar.high.is_finite());
            prop_assert!(bar.low.is_finite() && bar.close.is_finite());
            prop_assert_eq!(Some(bar.ts), tb.utc_timestamp_in_minutes.map(|m| i64::from(m) * 60_000));
        }
    }

    /// The interval table is a bijection with the wire period enum.
    #[test]
    fn interval_table_round_trips(interval in prop_oneof![
        prop::sample::select(vec!["1m", "2m", "3m", "4m", "5m", "10m", "15m", "30m", "1h", "4h", "12h", "1d", "1w", "1M"]).prop_map(str::to_string),
        any::<String>(),
    ]) {
        if let Some(period) = trendbar_period_for_interval(&interval) {
            prop_assert_eq!(interval_for_trendbar_period(period), interval.as_str());
            prop_assert!(trendbar_period_ms(period) > 0);
        }
    }

    /// The outbound order builder reads the wire-derived symbol grid and a request with
    /// edge-case fields. `qty` is bounded to +-1e12 units (NaN allowed): see the module doc.
    #[test]
    fn order_builder_is_total(
        symbols in symbol_map(),
        name in symbol_name(),
        order_type in prop::sample::select(vec!["market", "limit", "stop", "take_profit", ""]),
        side in -2i32..3,
        qty in prop_oneof![-1.0e12f64..1.0e12, Just(f64::NAN), Just(0.0), Just(-0.0)],
        price in prop::option::of(f64s()),
        trigger_price in prop::option::of(f64s()),
        ctid in i64s(),
    ) {
        let req = OrderRequest {
            client_order_id: "coid".to_string(),
            symbol: name,
            side,
            qty,
            order_type: order_type.to_string(),
            price,
            trigger_price,
            ..Default::default()
        };
        if let Some(wire) = order_to_new_order(&req, ctid, &symbols) {
            prop_assert_eq!(wire.label.as_deref(), Some("coid"));
            prop_assert_eq!(wire.ctid_trader_account_id, ctid);
        }
    }

    /// `SymbolMap` lookups over any id / name after any construction; `risk_properties` never
    /// divides by zero into a panic.
    #[test]
    fn symbol_map_queries_are_total(symbols in symbol_map(), id in i64s(), name in symbol_name()) {
        let _ = (symbols.id_of(&name), symbols.name_of(id), symbols.scale(id));
        let _ = symbols.volume_grid(id);
        let _ = symbols.risk_properties(&name);
        for known in symbols.names() {
            let _ = symbols.risk_properties(known);
        }
        prop_assert_eq!(symbols.ids().len(), symbols.names().count());
    }

    /// The catalog classifier over any string: an FX pair is exactly 3 + 3 ASCII letters.
    #[test]
    fn classify_is_total(name in prop_oneof![any::<String>(), "[A-Z]{6}", "[A-Za-z0-9.]{0,10}", symbol_name()]) {
        let (class, base, quote) = classify(&name);
        if matches!(class, vike_model::AssetClass::Fx) {
            prop_assert_eq!((base.len(), quote.len()), (3, 3));
        }
    }

    /// The position classifier and the reconcile fold over any wire rows.
    #[test]
    fn position_rows_are_total(rows in prop::collection::vec(position(), 0..8)) {
        let (tracked, unreadable) = reconcile_rows(&rows);
        prop_assert!(tracked.len() + unreadable <= rows.len());
        for p in &rows {
            if let Some((id, t)) = tracked_position(p) {
                prop_assert!(t.volume > 0 && (t.side == 1 || t.side == -1));
                prop_assert_eq!(id, p.position_id);
            }
        }
    }

    /// The FIFO reduce planner: when it closes, the legs sum to exactly the amount the rules say.
    #[test]
    fn plan_reduce_legs_add_up(
        open in prop::collection::vec((small_id(), tracked_position_strategy()), 0..8),
        order_side in -2i32..3,
        requested in prop_oneof![-5i64..5, 0i64..(2 * MAX_VOLUME)],
        reduce_only in any::<bool>(),
    ) {
        match plan_reduce(order_side, requested, reduce_only, &open) {
            ClosePlan::Open => {}
            ClosePlan::Close(legs) => {
                let available = opposing_available(order_side, &open);
                let total: i64 = legs.iter().map(|&(_, v)| v).sum();
                prop_assert!(!legs.is_empty() && legs.iter().all(|&(_, v)| v > 0));
                let want = if reduce_only { requested.min(available) } else { requested };
                prop_assert_eq!(total, want);
            }
        }
    }

    /// Partial-close snapping never exceeds the position volume (bounded to < 2^62 — see the
    /// module doc) and never panics on any raw volume or step.
    #[test]
    fn snap_close_volume_is_bounded(
        raw in i64s(),
        position_volume in 0i64..(1 << 62),
        step in i64s(),
    ) {
        let snapped = snap_close_volume(raw, position_volume, step);
        prop_assert!(snapped <= position_volume);
    }

    /// The OAuth token body: junk text and structurally plausible objects.
    #[test]
    fn token_bodies_are_total(
        text in prop_oneof![
            any::<String>(),
            prop::collection::vec(any::<u8>(), 0..128)
                .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
            token_object().prop_map(|v| v.to_string()),
        ],
        obtained in i64s(),
        expires_in in u64s(),
        at in prop::option::of(i64s()),
        now in i64s(),
    ) {
        let _ = parse_token_response(&text);
        let _ = expires_at_ms(obtained, expires_in);
        let _ = refresh_due(at, now);
    }
}

/// A token-endpoint-shaped object: both spellings of every field, hostile leaves.
fn token_object() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        Just(json!(u64::MAX)),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        "[ -~]{0,12}".prop_map(Value::String),
    ];
    let keys = [
        "accessToken",
        "access_token",
        "refreshToken",
        "refresh_token",
        "expiresIn",
        "expires_in",
        "errorCode",
        "error_code",
        "description",
    ];
    prop::collection::vec((prop::sample::select(keys.to_vec()), leaf), 0..10)
        .prop_map(|kvs| Value::Object(kvs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()))
        .boxed()
}

/// One operation against the in-flight close aggregator.
#[derive(Debug, Clone)]
enum CloseOp {
    Register(String, Vec<(i64, i64)>),
    Event(String, i64),
    Failure(String),
    Forget(String),
}

fn close_op() -> BoxedStrategy<CloseOp> {
    let coid = prop::sample::select(vec!["a", "b", "c"]).prop_map(str::to_string);
    prop_oneof![
        (coid.clone(), prop::collection::vec((small_id(), -MAX_VOLUME..MAX_VOLUME), 0..4))
            .prop_map(|(c, legs)| CloseOp::Register(c, legs)),
        (coid.clone(), prop_oneof![Just(0i64), -MAX_VOLUME..MAX_VOLUME])
            .prop_map(|(c, v)| CloseOp::Event(c, v)),
        coid.clone().prop_map(CloseOp::Failure),
        coid.prop_map(CloseOp::Forget),
    ]
    .boxed()
}

/// One operation against the open-position book.
#[derive(Debug, Clone)]
enum BookOp {
    Upsert(i64, TrackedPosition),
    Remove(i64),
    ReplaceAll(Vec<(i64, TrackedPosition)>, i64),
    ReplaceUnverified(Vec<(i64, TrackedPosition)>),
    Invalidate,
}

fn book_op() -> BoxedStrategy<BookOp> {
    let rows = prop::collection::vec((small_id(), tracked_position_strategy()), 0..5);
    prop_oneof![
        (small_id(), tracked_position_strategy()).prop_map(|(i, t)| BookOp::Upsert(i, t)),
        small_id().prop_map(BookOp::Remove),
        (rows.clone(), i64s()).prop_map(|(r, now)| BookOp::ReplaceAll(r, now)),
        rows.prop_map(BookOp::ReplaceUnverified),
        Just(BookOp::Invalidate),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (c) A random 1..8-operation sequence into ONE `CloseTracker`: a finished or failed coid is
    /// forgotten, an unfinished one stays tracked.
    #[test]
    fn close_tracker_sequences(ops in prop::collection::vec(close_op(), 1..8)) {
        let mut tracker = CloseTracker::default();
        for op in ops {
            match op {
                CloseOp::Register(coid, legs) => tracker.register(&coid, &legs),
                CloseOp::Event(coid, fill) => {
                    let emit = tracker.on_event(&coid, fill);
                    if matches!(emit, CloseEmit::Final { .. }) {
                        prop_assert!(!tracker.is_tracked(&coid));
                    } else {
                        prop_assert!(tracker.is_tracked(&coid));
                    }
                }
                CloseOp::Failure(coid) => {
                    let _ = tracker.resolve_failure(&coid);
                    prop_assert!(!tracker.is_tracked(&coid));
                }
                CloseOp::Forget(coid) => {
                    tracker.forget(&coid);
                    prop_assert!(!tracker.is_tracked(&coid));
                }
            }
            for position_id in 0..5 {
                let _ = (tracker.is_closing(position_id), tracker.coid_for(position_id));
            }
            let _ = tracker.has_progress("a");
        }
    }

    /// (c) A random 1..8-operation sequence into ONE `PositionBook`: the evidence flag follows the
    /// last replace / invalidate, and the per-symbol views add up to the whole.
    #[test]
    fn position_book_sequences(ops in prop::collection::vec(book_op(), 1..8)) {
        let mut book = PositionBook::default();
        for op in ops {
            let (check_fetched, expect) = match op {
                BookOp::Upsert(id, t) => { book.upsert(id, t); (false, false) }
                BookOp::Remove(id) => { book.remove(id); (false, false) }
                BookOp::ReplaceAll(rows, now) => { book.replace_all(rows, now); (true, true) }
                BookOp::ReplaceUnverified(rows) => { book.replace_unverified(rows); (true, false) }
                BookOp::Invalidate => { book.invalidate(); (true, false) }
            };
            if check_fetched {
                prop_assert_eq!(book.is_fetched(), expect);
            }
            let by_symbol: usize = (0..5).map(|s| book.for_symbol(s).len()).sum();
            prop_assert_eq!(by_symbol, book.len());
            for symbol_id in 0..5 {
                let _ = book.unauthoritative_for(symbol_id);
            }
            prop_assert_eq!(book.fetched_at_ms().is_some(), book.is_fetched());
        }
    }
}
