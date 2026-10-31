//! The pure half of closing a trade by its id: the routing table, the open-trades parse, the planner, the
//! orchestration over two scripted closures, the fill tracker, and the proof that the closing fill's id survives the
//! core's dedup where the opening fill's would not. Nothing here has a session; the wire is `tests/fxcm_shim_close.rs`
//! (the shim's shape) and the demo smoke (`tests/fxcm_close_smoke.rs`).

use super::*;
use crate::event_mapper::map_fxcm_event;
use vike_exec::{Account, BalanceMode, FillFold};
use vike_model::events::{FillEvent, OrderFilled};

const EURUSD: &str = "EUR/USD";

fn trade(id: &str, instrument: &str, buy: bool, units: i64, open_time: f64) -> OpenTrade {
    OpenTrade { trade_id: id.into(), instrument: instrument.into(), buy, units, open_time }
}

// ---- the account's mode --------------------------------------------------------------------------------------------

#[test]
fn only_a_maintenance_type_of_zero_is_a_netting_account() {
    let mode = |t: &str| {
        AccountMode::from_traits(Some(&format!(
            r#"{{"maintenance_type":"{t}","account_kind":"32","hedge_margin_pct":0.0}}"#
        )))
    };
    assert_eq!(mode("0"), AccountMode::Netted);
    assert_eq!(mode("1"), AccountMode::Hedged);
    assert_eq!(mode("2"), AccountMode::Hedged);
    // Not read is NOT assumed either way: an old shim, a failed read, a missing or empty value, junk.
    assert_eq!(AccountMode::from_traits(None), AccountMode::Unknown);
    assert_eq!(AccountMode::from_traits(Some("not json")), AccountMode::Unknown);
    assert_eq!(AccountMode::from_traits(Some(r#"{"account_kind":"32"}"#)), AccountMode::Unknown);
    assert_eq!(AccountMode::from_traits(Some(r#"{"maintenance_type":""}"#)), AccountMode::Unknown);
    assert_eq!(AccountMode::from_traits(Some(r#"{"maintenance_type":0}"#)), AccountMode::Unknown);
}

// ---- the routing table -----------------------------------------------------------------------------------------------
const GAP: &str = "the shim exports no `fc_close_trade`";
const EXIT_GAP: &str = "the shim exports no `fc_place_exit`";
const HOLD: &str = "a close of EUR/USD is of unknown outcome";
const MARKET: Shape = Shape::Market;
const PRICED: Shape = Shape::Limit { priced: true };
const UNPRICED: Shape = Shape::Limit { priced: false };

#[test]
fn a_netting_account_and_a_plain_order_go_out_exactly_as_before() {
    for mode in [AccountMode::Netted, AccountMode::Hedged, AccountMode::Unknown] {
        for shape in [MARKET, PRICED, UNPRICED] {
            for gap in [None, Some(GAP)] {
                for hold in [None, Some(HOLD)] {
                    assert_eq!(
                        route(mode, shape, false, gap, hold, Some(EXIT_GAP)),
                        Route::Place,
                        "plain {shape:?}, {mode:?}"
                    );
                }
            }
        }
    }
    for shape in [MARKET, PRICED, UNPRICED] {
        for gap in [None, Some(GAP)] {
            assert_eq!(
                route(AccountMode::Netted, shape, true, gap, Some(HOLD), Some(EXIT_GAP)),
                Route::Place,
                "a netting account closes with the opposite order, old shim or new, hold or none"
            );
        }
    }
}

#[test]
fn a_reduce_only_market_order_closes_by_trade_unless_the_shim_cannot() {
    for mode in [AccountMode::Hedged, AccountMode::Unknown] {
        assert_eq!(route(mode, MARKET, true, None, None, None), Route::CloseByTrade);
        // The OLD-SHIM answer is a refusal that names the gap, never the order that would lock a pair.
        assert_eq!(
            route(mode, MARKET, true, Some(GAP), None, None),
            Route::Refuse(GAP.to_string())
        );
        // ...and the gap outranks a hold: the shim is the thing to fix.
        assert_eq!(
            route(mode, MARKET, true, Some(GAP), Some(HOLD), None),
            Route::Refuse(GAP.to_string())
        );
        // A hold refuses a reduce-only MARKET order in its own words.
        assert_eq!(
            route(mode, MARKET, true, None, Some(HOLD), None),
            Route::Refuse(HOLD.to_string())
        );
    }
}

/// **The capability row's `reduce_only_kinds` is this table's verdict, executed.** For every kind the row accepts, on a
/// HEDGING account with a shim that has both entry points, a reduce-only order (a PRICED limit stands for a bracket's
/// take-profit) routes to a placement, never a refusal, exactly when the row lists the kind. What the table refuses
/// beyond that is a fact about the account (a stop on a netting one) or the moment (a hold), which a per-venue row
/// cannot state and which therefore stays the adapter's, by name.
#[test]
fn the_row_lists_exactly_the_reduce_only_kinds_a_hedging_account_places() {
    let caps = vike_venues::caps_for("fxcm");
    for kind in caps.supported_order_kinds {
        let shape = Shape::of(&OrderType::from_stored(kind), Some(1.1))
            .expect("a kind the row accepts has a routing shape");
        let routed = route(AccountMode::Hedged, shape, true, None, None, None);
        assert_eq!(
            !matches!(routed, Route::Refuse(_)),
            caps.reduce_only_kinds.contains(kind),
            "fxcm/{kind}: {routed:?}"
        );
    }
}

/// **A reduce-only PRICED limit on a hedging account is the trade's take-profit** (`L`), where it used to be refused: its
/// plain venue form is an entry order, which OPENS a trade when it is hit. An unpriced one rests at a fixed distance from
/// the quote, which can only be an entry, so it stays refused by name; an old shim names its gap. A lost close reply holds
/// MARKET closes only: placing an exit closes nothing.
#[test]
fn a_reduce_only_limit_is_bound_to_a_trade_when_it_names_its_price() {
    for mode in [AccountMode::Hedged, AccountMode::Unknown] {
        for hold in [None, Some(HOLD)] {
            assert_eq!(
                route(mode, PRICED, true, Some(GAP), hold, None),
                Route::BindExit(ExitKind::Limit),
                "the close gap and a hold are about closes, not about an exit"
            );
        }
        assert_eq!(
            route(mode, PRICED, true, None, None, Some(EXIT_GAP)),
            Route::Refuse(EXIT_GAP.to_string()),
            "an old shim: a named gap, never the entry order"
        );
        for exit_gap in [None, Some(EXIT_GAP)] {
            let Route::Refuse(why) = route(mode, UNPRICED, true, None, None, exit_gap) else {
                panic!("an unpriced reduce-only limit on {mode:?} must be refused");
            };
            assert!(why.contains("reduce-only LIMIT") && why.contains("OPENS"), "{why}");
            assert!(why.contains(mode.describe()), "the account is named: {why}");
        }
    }
}

/// **A stop exists on this venue only as the protective order of a trade.** Reduce-only on a hedging account it is bound;
/// not reduce-only it would be a stop ENTRY, which opens a trade when it is hit; on a netting account there is no
/// per-trade order at all; on an old shim the gap is named. None of them is `Place`.
#[test]
fn a_stop_is_bound_to_a_trade_and_refused_everywhere_it_would_open_one() {
    for mode in [AccountMode::Hedged, AccountMode::Unknown] {
        assert_eq!(
            route(mode, Shape::Stop, true, None, None, None),
            Route::BindExit(ExitKind::Stop)
        );
        assert_eq!(
            route(mode, Shape::Stop, true, Some(GAP), Some(HOLD), Some(EXIT_GAP)),
            Route::Refuse(EXIT_GAP.to_string()),
            "the close gap is not the exit gap"
        );
        assert_eq!(
            route(mode, Shape::Stop, true, Some(GAP), Some(HOLD), None),
            Route::BindExit(ExitKind::Stop),
            "a stop is held by neither the close gap nor a lost close reply"
        );
        let Route::Refuse(why) = route(mode, Shape::Stop, false, None, None, None) else {
            panic!("a stop that is not reduce-only on {mode:?} must be refused");
        };
        assert!(why.contains("not reduce-only") && why.contains("OPENS"), "{why}");
        assert!(why.contains(mode.describe()), "the account is named: {why}");
    }
    for reduce_only in [true, false] {
        for exit_gap in [None, Some(EXIT_GAP)] {
            let Route::Refuse(why) =
                route(AccountMode::Netted, Shape::Stop, reduce_only, None, None, exit_gap)
            else {
                panic!("a stop on a netting account must be refused");
            };
            assert!(why.contains("netting account") && why.contains("per-trade"), "{why}");
        }
    }
}

#[test]
fn the_routing_shape_of_an_order_type() {
    assert_eq!(Shape::of(&OrderType::Market, None), Some(Shape::Market));
    assert_eq!(Shape::of(&OrderType::Limit, Some(1.1)), Some(Shape::Limit { priced: true }));
    assert_eq!(Shape::of(&OrderType::Limit, None), Some(Shape::Limit { priced: false }));
    assert_eq!(Shape::of(&OrderType::Stop, None), Some(Shape::Stop));
    for other in [OrderType::StopLimit, OrderType::TakeProfit, OrderType::from_stored("iceberg")] {
        assert_eq!(Shape::of(&other, Some(1.1)), None, "{other}");
    }
}

// ---- the open-trades parse -------------------------------------------------------------------------------------------

const ROWS: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":2000,"open_time":46321.500000000},
  {"trade_id":"102","instrument":"EUR/USD","buysell":"S","amount":1000,"open_time":46321.600000000}
]"#;

#[test]
fn the_shims_rows_parse_into_trades() {
    let trades = parse_open_trades(ROWS).expect("the shim's own shape");
    assert_eq!(
        trades,
        vec![trade("101", EURUSD, true, 2000, 46321.5), trade("102", EURUSD, false, 1000, 46321.6)]
    );
    assert_eq!(parse_open_trades("[]"), Ok(vec![]), "an empty table is the venue saying so");
}

#[test]
fn a_row_the_planner_cannot_read_refuses_the_whole_read() {
    // Every key is read: dropping any one of the five fails the parse, and so does a wrong type or side.
    let row: Value = serde_json::from_str(ROWS).map(|v: Vec<Value>| v[0].clone()).unwrap();
    for key in ["trade_id", "instrument", "buysell", "amount", "open_time"] {
        let mut ablated = row.clone();
        ablated.as_object_mut().unwrap().remove(key);
        let err = parse_open_trades(&format!("[{ablated}]")).expect_err(key);
        assert!(err.contains(key), "the refusal names the key: {err}");
    }
    for bad in [
        r#"[{"trade_id":"1","instrument":"EUR/USD","buysell":"X","amount":1,"open_time":1.0}]"#,
        r#"[{"trade_id":"1","instrument":"EUR/USD","buysell":"B","amount":0,"open_time":1.0}]"#,
        r#"[{"trade_id":"1","instrument":"EUR/USD","buysell":"B","amount":1.5,"open_time":1.0}]"#,
        r#"[{"trade_id":1,"instrument":"EUR/USD","buysell":"B","amount":1,"open_time":1.0}]"#,
        r#"{"not":"an array"}"#,
    ] {
        assert!(parse_open_trades(bad).is_err(), "{bad}");
    }
    // A good row beside a bad one does not rescue the read: skipping it could close the wrong trade.
    let mixed = format!(
        r#"[{ok},{{"trade_id":"2","instrument":"EUR/USD","buysell":"B","amount":0,"open_time":1.0}}]"#,
        ok = row
    );
    assert!(parse_open_trades(&mixed).is_err());
}

// ---- the planner -----------------------------------------------------------------------------------------------------

fn legs(trades: &[OpenTrade], instrument: &str, buy: bool, units: i64) -> Vec<(String, i64)> {
    plan_close(trades, instrument, buy, units).expect("a plan")
}

#[test]
fn a_sell_closes_the_long_trade_for_its_whole_amount() {
    let book = [trade("101", EURUSD, true, 1000, 1.0)];
    assert_eq!(legs(&book, EURUSD, false, 1000), vec![("101".to_string(), 1000)]);
}

#[test]
fn a_partial_amount_closes_part_of_one_trade() {
    let book = [trade("101", EURUSD, true, 2000, 1.0)];
    assert_eq!(legs(&book, EURUSD, false, 1000), vec![("101".to_string(), 1000)]);
}

#[test]
fn the_oldest_trade_is_closed_first_and_the_amount_runs_across_trades() {
    // Listed newest first on purpose: the order of the table is not the order of the opens.
    let book = [
        trade("103", EURUSD, true, 1000, 3.0),
        trade("101", EURUSD, true, 1000, 1.0),
        trade("102", EURUSD, true, 3000, 2.0),
    ];
    assert_eq!(
        legs(&book, EURUSD, false, 2500),
        vec![("101".to_string(), 1000), ("102".to_string(), 1500)]
    );
}

#[test]
fn equal_open_times_fall_back_to_the_trade_id_numerically() {
    let book = [trade("1000", EURUSD, true, 1000, 1.0), trade("999", EURUSD, true, 1000, 1.0)];
    assert_eq!(legs(&book, EURUSD, false, 1000), vec![("999".to_string(), 1000)]);
}

#[test]
fn a_reduce_only_order_never_closes_more_than_the_trades_hold() {
    let book = [trade("101", EURUSD, true, 1000, 1.0), trade("102", EURUSD, true, 1000, 2.0)];
    // 5000 asked, 2000 held: the excess is dropped, never turned into an opposite trade.
    assert_eq!(
        legs(&book, EURUSD, false, 5000),
        vec![("101".to_string(), 1000), ("102".to_string(), 1000)]
    );
}

#[test]
fn only_trades_facing_the_order_on_that_instrument_are_closed() {
    let book = [
        trade("101", EURUSD, false, 1000, 1.0), // a SHORT: a SELL does not face it
        trade("102", "GBP/USD", true, 1000, 1.0), // another instrument
        trade("103", EURUSD, true, 1000, 5.0),  // the one
    ];
    assert_eq!(legs(&book, EURUSD, false, 1000), vec![("103".to_string(), 1000)]);
    // ...and a BUY faces the short.
    assert_eq!(legs(&book, EURUSD, true, 1000), vec![("101".to_string(), 1000)]);
}

#[test]
fn a_locked_pair_closes_the_trade_facing_the_order_and_not_its_twin() {
    let book = [trade("101", EURUSD, true, 1000, 1.0), trade("102", EURUSD, false, 1000, 2.0)];
    assert_eq!(legs(&book, EURUSD, false, 1000), vec![("101".to_string(), 1000)]);
    assert_eq!(legs(&book, EURUSD, true, 1000), vec![("102".to_string(), 1000)]);
}

#[test]
fn no_trade_to_close_is_a_named_refusal_not_an_opening_order() {
    for book in [
        vec![],
        vec![trade("101", EURUSD, false, 1000, 1.0)],
        vec![trade("102", "GBP/USD", true, 1000, 1.0)],
    ] {
        let why = plan_close(&book, EURUSD, false, 1000).expect_err("nothing faces the order");
        assert!(
            why.contains("no open long trade on EUR/USD") && why.contains("never opens"),
            "{why}"
        );
    }
}

// ---- the orchestration -----------------------------------------------------------------------------------------------

fn placed(order_id: &str) -> PlacedOrder {
    PlacedOrder {
        order_id: order_id.into(),
        offer_id: "1".into(),
        account_id: "A".into(),
        rate: 0.0,
    }
}

fn native(code: i32, message: &str) -> FxcmError {
    FxcmError::Native { code, message: message.into() }
}

const TWO_LONGS: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":1.0},
  {"trade_id":"102","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":2.0}
]"#;

/// What a scripted close came to, and the `(trade id, amount)` of every leg the sender saw.
type Run = (Result<Closed, (bool, String)>, Vec<(String, i32)>);

/// Run `close_by_trade` over `book` with a scripted leg sender; returns the verdict and what the sender saw.
fn run_close(
    book: &str,
    units: i64,
    mut answer: impl FnMut(&str) -> Result<PlacedOrder, FxcmError>,
    mut time_left: impl FnMut() -> bool,
) -> Run {
    let mut seen = Vec::new();
    let out = close_by_trade(
        EURUSD,
        false,
        units,
        || Ok(book.to_string()),
        |id, amount| {
            seen.push((id.to_string(), amount));
            answer(id)
        },
        &mut time_left,
    );
    (out, seen)
}

#[test]
fn two_legs_are_sent_oldest_first_and_both_order_ids_are_kept() {
    let (out, seen) = run_close(TWO_LONGS, 2000, |id| Ok(placed(&format!("order-{id}"))), || true);
    let done = out.expect("both legs sent");
    assert_eq!(seen, vec![("101".to_string(), 1000), ("102".to_string(), 1000)]);
    assert_eq!(done.first.order_id, "order-101");
    assert_eq!(done.more_ids, vec!["order-102".to_string()]);
    assert_eq!((done.units, done.stopped), (2000, None));
}

#[test]
fn a_partial_amount_is_what_the_leg_is_sent_with() {
    let (out, seen) = run_close(TWO_LONGS, 1500, |id| Ok(placed(id)), || true);
    out.expect("sent");
    assert_eq!(seen, vec![("101".to_string(), 1000), ("102".to_string(), 500)]);
}

#[test]
fn a_refusal_of_the_first_leg_is_the_orders_and_nothing_else_is_sent() {
    let (out, seen) = run_close(TWO_LONGS, 2000, |_| Err(native(-6, "no")), || true);
    let (unknown, why) = out.expect_err("the first leg was refused");
    assert!(!unknown && why.contains("-6") && why.contains("no"), "{why}");
    assert_eq!(seen.len(), 1);
}

#[test]
fn a_first_leg_with_an_unknown_outcome_stays_unknown_and_is_not_followed() {
    let lost = || FxcmError::SentUnknown { code: -11, message: "no answer".into(), lookup: None };
    let (out, seen) = run_close(TWO_LONGS, 2000, |_| Err(lost()), || true);
    let (unknown, _) = out.expect_err("unknown");
    assert!(unknown, "the close may have happened: this is never a refusal");
    assert_eq!(seen.len(), 1, "no second order on top of a close that may have happened");
}

#[test]
fn a_later_leg_that_fails_ends_the_sending_and_names_itself() {
    let (out, seen) = run_close(
        TWO_LONGS,
        2000,
        |id| if id == "101" { Ok(placed("o1")) } else { Err(native(-3, "gone")) },
        || true,
    );
    let done = out.expect("the first leg stands");
    assert_eq!(seen.len(), 2);
    assert_eq!((done.units, done.more_ids.len()), (1000, 0));
    let why = done.stopped.expect("the second leg was not closed");
    assert!(why.contains("trade 102") && why.contains("gone"), "{why}");
}

#[test]
fn the_time_bound_stops_the_legs_after_the_first_and_says_how_many_were_left() {
    let (out, seen) = run_close(TWO_LONGS, 2000, |id| Ok(placed(id)), || false);
    let done = out.expect("the first leg is always sent");
    assert_eq!(seen.len(), 1);
    let why = done.stopped.expect("named");
    assert!(why.contains("trade 102") && why.contains("0 more"), "{why}");
}

#[test]
fn a_book_that_cannot_be_read_or_planned_sends_nothing() {
    let sent = std::cell::Cell::new(0);
    let send = |_: &str, _: i32| {
        sent.set(sent.get() + 1);
        Ok(placed("x"))
    };
    let unreadable = close_by_trade(
        EURUSD,
        false,
        1000,
        || Err(native(-8, "timeout waiting for the refresh-table response")),
        send,
        || true,
    );
    let (unknown, why) = unreadable.expect_err("refused");
    assert!(!unknown && why.contains("nothing was sent") && why.contains("timeout"), "{why}");
    let junk = close_by_trade(EURUSD, false, 1000, || Ok("[{}]".into()), send, || true);
    assert!(junk.expect_err("refused").1.contains("open trade row 0"));
    let flat = close_by_trade(EURUSD, false, 1000, || Ok("[]".into()), send, || true);
    assert!(flat.expect_err("refused").1.contains("never opens"));
    assert_eq!(sent.get(), 0, "no closing order went out on any of the three");
}

// ---- a close whose reply was lost ------------------------------------------------------------------------------------

fn lost() -> FxcmError {
    FxcmError::SentUnknown { code: -11, message: "no answer".into(), lookup: None }
}

fn refused(desk: &mut CloseDesk, instrument: &str, at: Instant) -> Option<String> {
    match desk.route(instrument, MARKET, true, at) {
        Route::Refuse(why) => Some(why),
        Route::CloseByTrade => None,
        Route::Place | Route::BindExit(_) => {
            panic!("a reduce-only market order on a hedging desk is only ever closed or refused")
        }
    }
}

/// **A first leg whose reply was lost holds further reduce-only closes of THAT instrument, in words, for a minute.**
/// The close may have executed with nothing to route its fill (only the request id came back), and a re-sent PARTIAL
/// order would be planned against the trades AFTER it and close the next trade as well.
#[test]
fn a_lost_first_reply_holds_reduce_only_closes_of_that_instrument_for_a_minute() {
    let mut desk = CloseDesk::new(AccountMode::Hedged, None, None);
    let t0 = Instant::now();
    assert_eq!(refused(&mut desk, EURUSD, t0), None, "no hold before anything is lost");

    // The close of EUR/USD is sent and its reply is lost.
    let (outcome, _) = run_close(TWO_LONGS, 1000, |_| Err(lost()), || true);
    assert!(desk.settle(EURUSD, &outcome, t0), "an unknown outcome starts a hold");

    let why = refused(&mut desk, EURUSD, t0 + Duration::from_secs(1)).expect("held");
    assert!(
        why.contains("EUR/USD") && why.contains("MAY have happened") && why.contains("60 s"),
        "the refusal names the instrument, why and how long: {why}"
    );
    assert!(
        refused(&mut desk, EURUSD, t0 + Duration::from_secs(59)).is_some(),
        "still held at 59 s"
    );
    // The hold is per instrument and per kind of order: another pair closes, and an OPENING order is not a close.
    assert_eq!(refused(&mut desk, "GBP/USD", t0 + Duration::from_secs(1)), None);
    assert_eq!(desk.route(EURUSD, MARKET, false, t0 + Duration::from_secs(1)), Route::Place);
    // ...and neither is a stop or a take-profit: placing an exit closes nothing, so a lost close reply does not hold it.
    assert_eq!(
        desk.route(EURUSD, Shape::Stop, true, t0 + Duration::from_secs(1)),
        Route::BindExit(ExitKind::Stop)
    );
    assert_eq!(
        desk.route(EURUSD, PRICED, true, t0 + Duration::from_secs(1)),
        Route::BindExit(ExitKind::Limit)
    );
    // It ends, and a flatten re-sent after it is planned against the venue's fresh trades.
    assert_eq!(refused(&mut desk, EURUSD, t0 + Duration::from_secs(60)), None, "over at 60 s");
    assert_eq!(refused(&mut desk, EURUSD, t0 + Duration::from_secs(61)), None, "and stays over");
}

#[test]
fn a_definite_refusal_starts_no_hold() {
    let mut desk = CloseDesk::new(AccountMode::Hedged, None, None);
    let t0 = Instant::now();
    let (outcome, _) = run_close(TWO_LONGS, 1000, |_| Err(native(-6, "refused")), || true);
    assert!(
        !desk.settle(EURUSD, &outcome, t0),
        "a refusal is a verdict: nothing may have happened"
    );
    let nothing_to_close =
        close_by_trade(EURUSD, false, 1000, || Ok("[]".into()), |_, _| Ok(placed("x")), || true);
    assert!(!desk.settle(EURUSD, &nothing_to_close, t0));
    assert_eq!(refused(&mut desk, EURUSD, t0), None);
}

/// A LATER leg's lost reply is the same hazard (its closing order has no route either), and the order says so instead of
/// claiming the trade "was not closed".
#[test]
fn a_lost_reply_on_a_later_leg_holds_too_and_the_order_says_it_may_have_happened() {
    let (outcome, seen) = run_close(
        TWO_LONGS,
        2000,
        |id| if id == "101" { Ok(placed("o1")) } else { Err(lost()) },
        || true,
    );
    assert_eq!(seen.len(), 2);
    let done = outcome.as_ref().expect("the first leg stands");
    assert!(done.unknown && done.units == 1000);
    let why = done.stopped.as_deref().expect("named");
    assert!(
        why.contains("trade 102") && why.contains("UNKNOWN") && why.contains("may have happened"),
        "{why}"
    );
    let mut desk = CloseDesk::new(AccountMode::Hedged, None, None);
    let t0 = Instant::now();
    assert!(desk.settle(EURUSD, &outcome, t0));
    assert!(refused(&mut desk, EURUSD, t0 + Duration::from_secs(5)).is_some());
}

#[test]
fn a_later_leg_that_is_definitely_refused_is_not_unknown() {
    let (outcome, _) = run_close(
        TWO_LONGS,
        2000,
        |id| if id == "101" { Ok(placed("o1")) } else { Err(native(-3, "gone")) },
        || true,
    );
    let done = outcome.expect("the first leg stands");
    assert!(!done.unknown && done.stopped.as_deref().is_some_and(|s| s.contains("was not closed")));
}

/// The desk carries the shim's gap and the account's mode into the routing, which the exec thread used to do inline.
#[test]
fn the_desk_routes_with_the_gap_the_mode_and_the_hold_together() {
    let t0 = Instant::now();
    // A gap refuses a reduce-only market order by name, on a hedging or an unreadable account; a netting one is untouched.
    for mode in [AccountMode::Hedged, AccountMode::Unknown] {
        let mut desk = CloseDesk::new(mode, Some(GAP.to_string()), None);
        assert_eq!(desk.route(EURUSD, MARKET, true, t0), Route::Refuse(GAP.to_string()));
    }
    let mut netted = CloseDesk::new(AccountMode::Netted, Some(GAP.to_string()), None);
    assert_eq!(netted.route(EURUSD, MARKET, true, t0), Route::Place);
    // A hold never reaches a netting account either, however the close ended.
    let (outcome, _) = run_close(TWO_LONGS, 1000, |_| Err(lost()), || true);
    assert!(netted.settle(EURUSD, &outcome, t0));
    assert_eq!(netted.route(EURUSD, MARKET, true, t0 + Duration::from_secs(1)), Route::Place);
}

/// The desk also carries the exit gap: a stop on an old shim is refused in its words and a stop on a shim that has the
/// exports is bound, whatever the close gap says (it is a gap about CLOSING a trade).
#[test]
fn the_desk_routes_a_stop_with_the_exit_gap_and_not_the_close_gap() {
    let t0 = Instant::now();
    let mut old =
        CloseDesk::new(AccountMode::Hedged, Some(GAP.to_string()), Some("no exits".into()));
    assert_eq!(old.route(EURUSD, Shape::Stop, true, t0), Route::Refuse("no exits".to_string()));
    let mut current = CloseDesk::new(AccountMode::Hedged, Some(GAP.to_string()), None);
    assert_eq!(current.route(EURUSD, Shape::Stop, true, t0), Route::BindExit(ExitKind::Stop));
}

// ---- the tracker -----------------------------------------------------------------------------------------------------

fn fill_event(order_id: &str, trade_id: &str, side: &str, amount: i64) -> Vec<Event> {
    let v = serde_json::json!({
        "kind": "fill", "order_id": order_id, "trade_id": trade_id, "instrument": EURUSD,
        "side": side, "amount": amount, "rate": 1.1, "commission": 0.0, "ts": 0
    });
    map_fxcm_event(&v, "c-1")
}

fn names(evs: &[Event]) -> Vec<&'static str> {
    evs.iter()
        .map(|e| match e {
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            _ => "other",
        })
        .collect()
}

#[test]
fn a_one_leg_close_ends_with_the_plain_order_filled() {
    let mut track = CloseTrack::new(1000, None);
    let (evs, done) = track.relabel(fill_event("o1", "101-close-o1", "S", 1000));
    assert_eq!(names(&evs), ["Fill", "OrderFilled"]);
    assert!(done);
}

#[test]
fn several_legs_are_partial_fills_until_the_last() {
    let mut track = CloseTrack::new(2000, None);
    let (first, done) = track.relabel(fill_event("o1", "101-close-o1", "S", 1000));
    assert_eq!(names(&first), ["Fill", "OrderPartiallyFilled"]);
    assert!(!done);
    let (last, done) = track.relabel(fill_event("o2", "102-close-o2", "S", 1000));
    assert_eq!(names(&last), ["Fill", "OrderFilled"]);
    assert!(done, "the close is complete: the caller forgets it");
}

#[test]
fn legs_that_were_not_sent_end_the_order_cancelled_after_its_last_fill() {
    let mut track = CloseTrack::new(1000, Some("trade 102 was not closed: gone".into()));
    let (evs, done) = track.relabel(fill_event("o1", "101-close-o1", "S", 1000));
    // PartiallyFilled -> Canceled is a legal path; Filled would claim the whole order, and Rejected is illegal there.
    assert_eq!(names(&evs), ["Fill", "OrderPartiallyFilled", "OrderCanceled"]);
    assert!(done);
    let Some(Event::OrderCanceled(c)) = evs.last() else { panic!("a cancel last") };
    assert_eq!(c.client_order_id, "c-1");
    assert!(c.reason.contains("trade 102"), "{}", c.reason);
}

#[test]
fn what_is_not_a_fill_passes_through_untouched() {
    let mut track = CloseTrack::new(1000, Some("x".into()));
    let canceled = vec![Event::OrderCanceled(OrderCanceled {
        client_order_id: "c-1".into(),
        reason: "venue".into(),
        ts: 0,
    })];
    let before = format!("{canceled:?}");
    let (evs, done) = track.relabel(canceled);
    assert_eq!(format!("{evs:?}"), before);
    assert!(!done);
}

// ---- the closing fill's id -------------------------------------------------------------------------------------------

/// `Account::apply_fill` over the events a mapped envelope carries.
fn fold(account: &mut Account, evs: &[Event]) -> Vec<FillFold> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(account.apply_fill(f)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_close_under_the_opening_fills_trade_id_is_refused_by_the_core_and_the_close_id_is_not() {
    let mut account = Account::new(1.0, "fxcm", None, BalanceMode::Delta);
    assert_eq!(fold(&mut account, &fill_event("o0", "101", "B", 2000)), [FillFold::Applied]);
    assert_eq!(account.net_qty("EURUSD"), 2000.0);

    // The collision the closing id exists to avoid: the venue's trade id is the same on the way out as on the way in,
    // the core has folded it, and a REAL close is dropped. (The same class bit Dukascopy on 2026-10-26.)
    let same_id = fill_event("o1", "101", "S", 2000);
    assert_eq!(fold(&mut account, &same_id), [FillFold::Collision]);
    assert_eq!(account.net_qty("EURUSD"), 2000.0, "the position never closed");

    // The id `fcshim.cpp` mints, `<trade id>-close-<closing order id>`, is its own fill, and it closes the position.
    let closing = fill_event("o1", "101-close-o1", "S", 2000);
    assert_eq!(fold(&mut account, &closing), [FillFold::Applied]);
    assert_eq!(account.net_qty("EURUSD"), 0.0);
    // ...and the SAME close delivered twice (a reconnect replay re-derives the same id) is folded once.
    assert_eq!(fold(&mut account, &closing), [FillFold::Duplicate]);
}

#[test]
fn two_partial_closes_of_one_trade_are_two_fills() {
    // Both rows may carry the same trade id; the closing order tells them apart.
    let mut account = Account::new(1.0, "fxcm", None, BalanceMode::Delta);
    fold(&mut account, &fill_event("o0", "101", "B", 2000));
    assert_eq!(
        fold(&mut account, &fill_event("o1", "101-close-o1", "S", 1000)),
        [FillFold::Applied]
    );
    assert_eq!(
        fold(&mut account, &fill_event("o2", "101-close-o2", "S", 1000)),
        [FillFold::Applied]
    );
    assert_eq!(account.net_qty("EURUSD"), 0.0);
}

/// A closing fill decodes to the SELL of a BUY trade it closed, in base units, at the close rate.
#[test]
fn the_closing_envelope_decodes_to_the_opposite_side_at_the_close_rate() {
    let evs = fill_event("o1", "101-close-o1", "S", 1000);
    let Some(Event::OrderFilled(OrderFilled { fill, .. })) = evs.get(1) else {
        panic!("a fill dual-publishes: {evs:?}")
    };
    let FillEvent { side, last_qty, trade_id, .. } = fill;
    assert_eq!((*side, *last_qty, trade_id.as_str()), (-1, 1000.0, "101-close-o1"));
}
