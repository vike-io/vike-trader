//! The pure half of closing a trade by its id: the routing table, the open-trades parse, the planner, the
//! orchestration over two scripted closures, the fill tracker, and the proof that the closing fill's id survives the
//! core's dedup where the opening fill's would not. Nothing here has a session; the wire is `tests/fxcm_shim/fxcm_shim_close.rs`
//! (the shim's shape) and the demo smoke (`tests/fxcm_smokes/fxcm_close_smoke.rs`).

use super::*;
use crate::execution::event_mapper::map_fxcm_event;
use vike_execution::{Account, BalanceMode, FillFold};
use vike_model::events::{FillEvent, OrderFilled};

const EURUSD: &str = "EUR/USD";

fn trade(id: &str, instrument: &str, buy: bool, units: i64, open_time: f64) -> OpenTrade {
    OpenTrade { trade_id: id.into(), instrument: instrument.into(), buy, units, open_time }
}

/// What a late close asks of a netted account (`ExecutionClientActorLateCloses`): the trades of its instrument, long positive.
#[test]
fn net_units_nets_the_trades_of_one_instrument() {
    let trades = [
        trade("1", EURUSD, true, 5000, 1.0),
        trade("2", EURUSD, false, 2000, 2.0),
        trade("3", "USD/JPY", true, 9000, 3.0),
    ];
    assert_eq!(net_units(&trades, EURUSD), 3000.0);
    assert_eq!(net_units(&trades, "GBP/USD"), 0.0, "no trade reads flat");
    let desk = |mode| CloseDesk::new(mode, None, None).nets();
    assert!(desk(PositionMode::OneWay));
    assert!(!desk(PositionMode::Hedge) && !desk(PositionMode::Unknown));
}

/// **A reduce-only close of the mixed-case CFD `USOIL.CFD` finds its facing trades** (audit flag FXCM-1, decision 0290),
/// over the kept rows as the catalog makes them of the measured Offers table: the order's user name sends its row's
/// wire name `USOil` (case kept), which is exactly what the shim reports a trade of that row under, so a hedging
/// account closes the trades by id and a netting account's late close reads the position it holds. It used to send
/// the upper-cased `USOIL`, which no trade's `USOil` matched: the close was refused and the netting position read 0.
#[test]
fn a_mixed_case_cfd_close_finds_its_facing_trades_by_its_row() {
    let wire = crate::instrument::names::wire_name("USOIL.CFD").expect("the kept row");
    assert_eq!(wire, "USOil");
    assert_eq!(crate::instrument::names::wire_name("usoil.cfd").as_deref(), Ok("USOil"));
    let book = r#"[
      {"trade_id":"201","instrument":"USOil","buysell":"B","amount":10,"open_time":1.0},
      {"trade_id":"202","instrument":"USOil","buysell":"B","amount":20,"open_time":2.0},
      {"trade_id":"203","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":0.5}
    ]"#;
    // the hedging account: route, then the legs, oldest first, on USOil's trades alone
    let t0 = Instant::now();
    let mut hedged = CloseDesk::new(PositionMode::Hedge, None, None);
    assert_eq!(hedged.route(&wire, MARKET, true, t0), Route::CloseByTrade);
    let mut seen = Vec::new();
    let closed = close_by_trade(
        &wire,
        false,
        25,
        || Ok(book.to_string()),
        |id, amount| {
            seen.push((id.to_string(), amount));
            Ok(placed(id))
        },
        || true,
    )
    .expect("USOil's trades face the sell");
    assert_eq!(seen, vec![("201".to_string(), 10), ("202".to_string(), 15)]);
    assert_eq!(closed.units, 25);
    // the netting account: a late close asks what the position holds
    let trades = parse_open_trades(book).expect("the shim's shape");
    assert!(CloseDesk::new(PositionMode::OneWay, None, None).nets());
    assert_eq!(net_units(&trades, &wire), 30.0);
    assert_eq!(net_units(&trades, "USOIL"), 0.0, "the old upper-cased spelling is no row's");
}

/// **A trade is on the order's instrument when both name its row alike** (FXCM-1, decision 0290): the shim names a CFD's
/// trade `USOil` or `Copper` and a pair's `EUR/USD`, the order sends its kept row's wire name, the same spelling, so a
/// reduce-only close finds the trades facing it and a late close on a netting account reads the position; another
/// case or slash of the name (`USOIL`, `eurusd`) is no row's and reads flat.
#[test]
fn a_trade_is_on_the_orders_instrument_when_both_name_its_row_alike() {
    let trades = [
        trade("1", "USOil", true, 10, 1.0),
        trade("2", "Copper", true, 20, 2.0),
        trade("3", EURUSD, true, 1000, 3.0),
    ];
    for (wire, units) in [("USOil", 10), ("Copper", 20), (EURUSD, 1000)] {
        assert_eq!(net_units(&trades, wire), units as f64, "{wire}");
        let legs = plan_close(&trades, wire, false, units).expect(wire);
        assert_eq!(legs.iter().map(|(_, u)| *u).sum::<i64>(), units, "{wire}");
    }
    for other in ["USOIL", "COPPER", "EURUSD", "eur/usd", "UsOil"] {
        assert_eq!(net_units(&trades, other), 0.0, "{other}");
    }
}

// ---- the account's mode --------------------------------------------------------------------------------------------

#[test]
fn only_a_maintenance_type_of_zero_is_a_netting_account() {
    let mode = |t: &str| {
        position_mode(Some(&format!(
            r#"{{"maintenance_type":"{t}","account_kind":"32","hedge_margin_pct":0.0}}"#
        )))
    };
    assert_eq!(mode("0"), PositionMode::OneWay);
    assert_eq!(mode("1"), PositionMode::Hedge);
    assert_eq!(mode("2"), PositionMode::Hedge);
    // Not read is NOT assumed either way: an old shim, a failed read, a missing or empty value, junk.
    assert_eq!(position_mode(None), PositionMode::Unknown);
    assert_eq!(position_mode(Some("not json")), PositionMode::Unknown);
    assert_eq!(position_mode(Some(r#"{"account_kind":"32"}"#)), PositionMode::Unknown);
    assert_eq!(position_mode(Some(r#"{"maintenance_type":""}"#)), PositionMode::Unknown);
    assert_eq!(position_mode(Some(r#"{"maintenance_type":0}"#)), PositionMode::Unknown);
}

// ---- the routing table -----------------------------------------------------------------------------------------------
const GAP: &str = "the shim exports no `forex_connect_close_trade`";
const EXIT_GAP: &str = "the shim exports no `forex_connect_place_exit`";
const HOLD: &str = "a close of EUR/USD is of unknown outcome";
const MARKET: Shape = Shape::Market;
const PRICED: Shape = Shape::Limit { priced: true };
const UNPRICED: Shape = Shape::Limit { priced: false };

#[test]
fn a_netting_account_and_a_plain_order_go_out_exactly_as_before() {
    for mode in [PositionMode::OneWay, PositionMode::Hedge, PositionMode::Unknown] {
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
                route(PositionMode::OneWay, shape, true, gap, Some(HOLD), Some(EXIT_GAP)),
                Route::Place,
                "a netting account closes with the opposite order, old shim or new, hold or none"
            );
        }
    }
}

#[test]
fn a_reduce_only_market_order_closes_by_trade_unless_the_shim_cannot() {
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
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
    let capabilities = vike_venues::capabilities_for("fxcm");
    for kind in capabilities.supported_order_kinds {
        let shape = Shape::of(&OrderType::from_stored(kind), Some(1.1))
            .expect("a kind the row accepts has a routing shape");
        let routed = route(PositionMode::Hedge, shape, true, None, None, None);
        assert_eq!(
            !matches!(routed, Route::Refuse(_)),
            capabilities.reduce_only_kinds.contains(kind),
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
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
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
            assert!(why.contains(position_mode_words(mode)), "the account is named: {why}");
        }
    }
}

/// **A stop exists on this venue only as the protective order of a trade.** Reduce-only on a hedging account it is bound;
/// not reduce-only it would be a stop ENTRY, which opens a trade when it is hit; on a netting account there is no
/// per-trade order at all; on an old shim the gap is named. None of them is `Place`.
#[test]
fn a_stop_is_bound_to_a_trade_and_refused_everywhere_it_would_open_one() {
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
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
        assert!(why.contains(position_mode_words(mode)), "the account is named: {why}");
    }
    for reduce_only in [true, false] {
        for exit_gap in [None, Some(EXIT_GAP)] {
            let Route::Refuse(why) =
                route(PositionMode::OneWay, Shape::Stop, reduce_only, None, None, exit_gap)
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
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
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
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
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
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
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
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
        let mut desk = CloseDesk::new(mode, Some(GAP.to_string()), None);
        assert_eq!(desk.route(EURUSD, MARKET, true, t0), Route::Refuse(GAP.to_string()));
    }
    let mut netted = CloseDesk::new(PositionMode::OneWay, Some(GAP.to_string()), None);
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
        CloseDesk::new(PositionMode::Hedge, Some(GAP.to_string()), Some("no exits".into()));
    assert_eq!(old.route(EURUSD, Shape::Stop, true, t0), Route::Refuse("no exits".to_string()));
    let mut current = CloseDesk::new(PositionMode::Hedge, Some(GAP.to_string()), None);
    assert_eq!(current.route(EURUSD, Shape::Stop, true, t0), Route::BindExit(ExitKind::Stop));
}

// ---- a fired plain stop (the owner's ruling of 2026-11-08: the first closes, the rest opens) -------------------------

/// The long a stop protects (trade 101) and a person's deliberate hedge beside it (trade 102, a short).
const LONG_AND_HEDGE: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":2000,"open_time":1.0},
  {"trade_id":"102","instrument":"EUR/USD","buysell":"S","amount":500,"open_time":2.0}
]"#;

/// A market order as the core sends it. A fired plain stop is `vike-core`'s `submit_fired`: the part its position
/// covers flagged `reduce_only`, the part past it plain, behind the close. A manual opposite order is the plain one.
fn market(side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest { side, qty, order_type: OrderType::Market, reduce_only, ..Default::default() }
}

/// What the exec thread does with `req` on `desk` over the venue's `book` at the quote `(bid, ask)`: the route
/// (`CloseDesk::route_order`), and for a close the `(trade id, amount, price bound)` of every leg it sends.
fn legs_of(
    desk: &mut CloseDesk,
    req: &OrderRequest,
    book: &str,
    quote: Option<(f64, f64)>,
) -> (Route, Vec<(String, i32, Option<f64>)>) {
    let routed = desk.route_order(EURUSD, req, || quote, || None, Instant::now());
    let mut seen = Vec::new();
    if routed == Route::CloseByTrade {
        let bound = close_bound(req);
        let closed = close_by_trade(
            EURUSD,
            req.side >= 0,
            req.qty as i64,
            || Ok(book.to_string()),
            |id, amount| {
                seen.push((id.to_string(), amount, bound));
                Ok(placed(id))
            },
            || true,
        );
        desk.settle(EURUSD, &closed, Instant::now());
        closed.expect("the close was sent");
    }
    (routed, seen)
}

/// [`legs_of`] for a market order, whose close has no price bound.
fn dispatch(desk: &mut CloseDesk, req: &OrderRequest, book: &str) -> (Route, Vec<(String, i32)>) {
    let (routed, legs) = legs_of(desk, req, book, None);
    let legs = legs
        .into_iter()
        .map(|(id, amount, bound)| {
            assert_eq!(bound, None, "a market close has no price bound");
            (id, amount)
        })
        .collect();
    (routed, legs)
}

/// **A fired plain stop CLOSES the long by its trade id, never more than that trade holds.** On a hedging account (or one
/// whose mode is unreadable) the stop's flagged close is one `TrueMarketClose` of trade 101, never an opposite order
/// beside it, and the person's hedge (102) is not touched. A core book ahead of the venue asks for more: the trade's size
/// is the cap.
#[test]
fn a_fired_plain_stop_closes_the_long_by_its_trade_id_and_never_more_than_it_holds() {
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
        let mut desk = CloseDesk::new(mode, None, None);
        for (asked, closed) in [(1500.0, 1500), (2000.0, 2000), (5000.0, 2000)] {
            assert_eq!(
                dispatch(&mut desk, &market(-1, asked, true), LONG_AND_HEDGE),
                (Route::CloseByTrade, vec![("101".to_string(), closed)]),
                "{mode:?} {asked}"
            );
        }
    }
}

/// **Only the part of a fired stop past the position opens the opposite trade.** It comes plain, behind the close; a
/// close that went through holds nothing, so it goes out as the shim's opening order (a new short trade).
#[test]
fn the_part_of_a_fired_stop_past_the_position_opens_the_opposite_trade() {
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
        let mut desk = CloseDesk::new(mode, None, None);
        let (close, legs) = dispatch(&mut desk, &market(-1, 2000.0, true), LONG_AND_HEDGE);
        assert_eq!((close, legs.len()), (Route::CloseByTrade, 1), "{mode:?}");
        assert_eq!(
            dispatch(&mut desk, &market(-1, 1000.0, false), LONG_AND_HEDGE),
            (Route::Place, vec![]),
            "{mode:?}"
        );
    }
}

/// **A MANUAL opposite order stays a hedge** (the owner, 2026-10-26): it is plain, so with a long facing it it OPENS a
/// trade beside the long and closes nothing. The bridge cannot tell it from a stop's opening part, and need not: the core
/// flags what a stop closes and nothing else.
#[test]
fn a_manual_opposite_order_is_still_a_hedge() {
    for mode in [PositionMode::Hedge, PositionMode::Unknown] {
        let mut desk = CloseDesk::new(mode, None, None);
        assert_eq!(
            dispatch(&mut desk, &market(-1, 2000.0, false), LONG_AND_HEDGE),
            (Route::Place, vec![]),
            "{mode:?}"
        );
    }
}

// ---- a fired stop-limit (the owner's word of 2026-11-15) ---------------------------------------------------------------

/// A long of 10 000 EUR/USD bought at 1.1000 (trade 101).
const LONG_10K: &str =
    r#"[{"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":10000,"open_time":1.0}]"#;
/// A short of 10 000 EUR/USD sold at 1.1000 (trade 201): the mirror.
const SHORT_10K: &str =
    r#"[{"trade_id":"201","instrument":"EUR/USD","buysell":"S","amount":10000,"open_time":1.0}]"#;

/// A stop-limit's fire as the core releases it (`vike-core`'s `submit_fired`): a reduce-only LIMIT of 10 000 at its limit
/// price, so "sell at 1.0950, not below 1.0945" fires as a sell limit at 1.0945. A trailing stop-limit's fire is the same
/// order at its moving trigger's limit: the bridge sees nothing else.
fn fired_limit(side: i32, limit: f64) -> OrderRequest {
    OrderRequest {
        side,
        qty: 10_000.0,
        order_type: OrderType::Limit,
        price: Some(limit),
        reduce_only: true,
        ..Default::default()
    }
}

/// The trade-bound `L` an unreached limit becomes (`exit::plan_exits` over `book`, no exit resting): `(trade id, units, rate)`.
fn rests_as(req: &OrderRequest, book: &str) -> Vec<(String, i64, f64)> {
    let trades = parse_open_trades(book).expect("a book");
    let ask = crate::execution::exit::Ask {
        instrument: EURUSD,
        buy: req.side >= 0,
        kind: ExitKind::Limit,
        units: req.qty as i64,
        rate: req.price.expect("priced"),
    };
    crate::execution::exit::plan_exits(&trades, &[], &ask, &|_: &str| false)
        .expect("the take-profit binds")
        .into_iter()
        .map(|leg| (leg.trade_id, leg.units, ask.rate))
        .collect()
}

/// **Example 1: a plain stop-loss at 1.0950 is UNCHANGED.** A reduce-only stop is the trade's native `S` (the venue closes
/// at market when it fires) and asks for no quote; a core-held plain stop's fire is a market close by trade id with no price
/// bound.
#[test]
fn example_1_a_plain_stop_loss_is_unchanged() {
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    let stop = OrderRequest {
        side: -1,
        qty: 10_000.0,
        order_type: OrderType::Stop,
        trigger_price: Some(1.0950),
        reduce_only: true,
        ..Default::default()
    };
    let routed = desk.route_order(
        EURUSD,
        &stop,
        || panic!("a stop reads no quote"),
        || None,
        Instant::now(),
    );
    assert_eq!(routed, Route::BindExit(ExitKind::Stop));
    assert_eq!(
        legs_of(&mut desk, &market(-1, 10_000.0, true), LONG_10K, Some((1.0950, 1.0952))),
        (Route::CloseByTrade, vec![("101".to_string(), 10_000, None)])
    );
}

/// **Example 2: "sell at 1.0950, not below 1.0945", the price falls smoothly.** At bid 1.0950 the market reaches the limit,
/// so trade 101 is CLOSED BY ITS ID, whole, by a Close Limit bounded at 1.0945: it fills at 1.0950 and never below 1.0945.
/// It used to go out as the trade's `L`, which the venue refuses on that side of the market (`ORA-20115`). A bid exactly
/// at the limit reaches it too.
#[test]
fn example_2_a_stop_limit_the_market_reaches_closes_the_trade_by_its_id_never_below_the_limit() {
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    for bid in [1.0950, 1.0945] {
        assert_eq!(
            legs_of(&mut desk, &fired_limit(-1, 1.0945), LONG_10K, Some((bid, bid + 0.0002))),
            (Route::CloseByTrade, vec![("101".to_string(), 10_000, Some(1.0945))]),
            "bid {bid}"
        );
    }
}

/// **Example 3: the price gaps to 1.0940, then comes back to 1.0946.** At 1.0940 the limit is not reached: nothing closes,
/// and the order rests AT THE VENUE as trade 101's take-profit `L` at 1.0945, above the market, the side a long's
/// take-profit takes (measured: `docs/ops/fxcm-forexconnect.md`'s "Trade-bound stop-loss and take-profit"). When the bid is
/// back at 1.0945 or above, the venue fills that `L` and closes trade 101 by its own id; it survives a node outage.
#[test]
fn example_3_a_gapped_stop_limit_rests_as_the_trades_take_profit_at_its_limit() {
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    let fired = fired_limit(-1, 1.0945);
    assert_eq!(
        legs_of(&mut desk, &fired, LONG_10K, Some((1.0940, 1.0942))),
        (Route::BindExit(ExitKind::Limit), vec![])
    );
    assert_eq!(rests_as(&fired, LONG_10K), vec![("101".to_string(), 10_000, 1.0945)]);
}

/// **Example 4: the price gaps to 1.0940 and never comes back.** Wherever the bid stays below 1.0945, no close leg is sent:
/// the `L` rests and never fills, and the trade stays open. That is the stop-limit's own meaning.
#[test]
fn example_4_a_stop_limit_the_price_never_comes_back_to_leaves_the_trade_open() {
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    for bid in [1.0940, 1.0930, 1.0944] {
        assert_eq!(
            legs_of(&mut desk, &fired_limit(-1, 1.0945), LONG_10K, Some((bid, bid + 0.0002))),
            (Route::BindExit(ExitKind::Limit), vec![]),
            "bid {bid}"
        );
    }
}

/// **The short mirror: "buy at 1.1050, not above 1.1055".** Judged on the ASK: at 1.1050 trade 201 closes by its id never
/// above 1.1055; gapped to 1.1060 it rests as trade 201's `L` at 1.1055 (below the market, a short's take-profit) and never
/// back, the trade stays open. A sell is judged on the bid and a buy on the ask, never the other side.
#[test]
fn the_short_mirror_buys_on_the_ask_never_above_the_limit() {
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    let fired = fired_limit(1, 1.1055);
    assert_eq!(
        legs_of(&mut desk, &fired, SHORT_10K, Some((1.1048, 1.1050))),
        (Route::CloseByTrade, vec![("201".to_string(), 10_000, Some(1.1055))])
    );
    assert_eq!(
        legs_of(&mut desk, &fired, SHORT_10K, Some((1.1058, 1.1060))),
        (Route::BindExit(ExitKind::Limit), vec![])
    );
    assert_eq!(rests_as(&fired, SHORT_10K), vec![("201".to_string(), 10_000, 1.1055)]);
    // The other side of the quote decides nothing.
    assert!(!reaches(false, 1.0945, 1.0944, 1.0946), "a sell is judged on the bid");
    assert!(!reaches(true, 1.1055, 1.1054, 1.1056), "a buy is judged on the ask");
}

/// A reached limit on a shim without `forex_connect_close_trade_limit` is refused by NAME, and so is one while a close of the
/// instrument is of unknown outcome (the hold a market close has). A netting account never asks for a quote.
#[test]
fn a_reached_limit_names_the_gap_and_the_hold() {
    let t0 = Instant::now();
    let fired = fired_limit(-1, 1.0945);
    let reached = Some((1.0950, 1.0952));
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    assert_eq!(
        desk.route_order(EURUSD, &fired, || reached, || Some("no close limit".into()), t0),
        Route::Refuse("no close limit".to_string())
    );
    assert!(desk.settle(EURUSD, &Err((true, "lost".to_string())), t0));
    let held = desk.route_order(EURUSD, &fired, || reached, || None, t0 + Duration::from_secs(1));
    assert!(matches!(&held, Route::Refuse(why) if why.contains("MAY have happened")), "{held:?}");
    let mut netted = CloseDesk::new(PositionMode::OneWay, None, None);
    assert_eq!(
        netted.route_order(
            EURUSD,
            &fired,
            || panic!("a netting account reads no quote"),
            || None,
            t0
        ),
        Route::Place
    );
}

/// **No quote is not "not reached"** (the live demo of 2026-11-16, account fxcm #9: the quote read found no row, the
/// stop-limit "sell 1000 at 1.10041, not below 1.10011" fired with the bid at 1.1004, went out as trade's `L` at 1.10011 and
/// was refused `ORA-20115` four times). With no quote the Close Limit goes FIRST, bounded at the limit, so the trade is
/// closed by its id when the market is there and a miss rests the `L` (the exec thread's `rest_refused`). Only a shim
/// that cannot close under a price, or a close hold, keeps the `L` with no quote.
#[test]
fn with_no_quote_a_fired_stop_limit_closes_the_trade_by_id_under_its_limit_first() {
    let t0 = Instant::now();
    let fired = OrderRequest {
        order_client_id: LIVE_FIRED_CLOSE.into(),
        side: -1,
        qty: 1000.0,
        order_type: OrderType::Limit,
        price: Some(1.10011),
        reduce_only: true,
        ..Default::default()
    };
    let one_long = r#"[{"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":1.0}]"#;
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    assert_eq!(
        legs_of(&mut desk, &fired, one_long, None),
        (Route::CloseByTrade, vec![("101".to_string(), 1000, Some(1.10011))])
    );
    assert_eq!(
        desk.route_order(EURUSD, &fired, || None, || Some("no close limit".into()), t0),
        Route::BindExit(ExitKind::Limit),
        "an old shim: the take-profit, as before"
    );
    assert!(desk.settle(EURUSD, &Err((true, "lost".to_string())), t0));
    assert_eq!(
        desk.route_order(EURUSD, &fired, || None, || None, t0 + Duration::from_secs(1)),
        Route::BindExit(ExitKind::Limit),
        "a close hold: the take-profit, which closes nothing"
    );
}

/// The id the core gave the live fired stop-limit's close on 2026-11-16 (`E`: the engine's own), and the operator's own
/// stop order of the same session (`C`: the CLI).
const LIVE_FIRED_CLOSE: &str = "E000009lq6ie7tdnopzCL";
const LIVE_OPERATOR_ORDER: &str = "C000009uwoofvyjgw8qCL";

/// **With no quote, ONLY a fired close goes Close Limit first** (the review of batch 1d, finding 4). An ordinary take-profit
/// (the operator's, a strategy's, a bracket's leg, an id from before v2) is the trade's `L`, as it always was: a Close Limit
/// tried for it would gain nothing, and its lost reply would start the close hold that then refuses a protective stop's
/// close. A quote still judges every one of them alike.
#[test]
fn with_no_quote_only_a_fired_close_tries_the_close_limit_and_a_take_profit_rests() {
    let t0 = Instant::now();
    let limit = |id: &str| OrderRequest {
        order_client_id: id.into(),
        side: -1,
        qty: 1000.0,
        order_type: OrderType::Limit,
        price: Some(1.10011),
        reduce_only: true,
        ..Default::default()
    };
    let mut desk = CloseDesk::new(PositionMode::Hedge, None, None);
    assert!(fired_by_the_engine(LIVE_FIRED_CLOSE));
    assert_eq!(
        desk.route_order(EURUSD, &limit(LIVE_FIRED_CLOSE), || None, || None, t0),
        Route::CloseByTrade
    );
    for take_profit in
        [LIVE_OPERATOR_ORDER, "S000109lq6ie7tdnopzCL", "E000009lq6ie7tdnopzCLTP1", "deadbeef17", ""]
    {
        assert!(!fired_by_the_engine(take_profit), "{take_profit}");
        assert_eq!(
            desk.route_order(EURUSD, &limit(take_profit), || None, || None, t0),
            Route::BindExit(ExitKind::Limit),
            "{take_profit}: the take-profit rests"
        );
        assert_eq!(
            desk.route_order(EURUSD, &limit(take_profit), || Some((1.1002, 1.1004)), || None, t0),
            Route::CloseByTrade,
            "{take_profit}: a quote that reaches its limit closes it, as for any limit"
        );
    }
}

/// A quote judges a limit only when both sides are positive and finite (`forex_connect_quote` writes zeros when it reads none).
#[test]
fn a_quote_judges_a_limit_only_with_two_positive_sides() {
    assert_eq!(usable_quote(1.095, 1.0952), Some((1.095, 1.0952)));
    for (bid, ask) in
        [(0.0, 1.0952), (1.095, 0.0), (f64::NAN, 1.0952), (1.095, f64::INFINITY), (-1.0, 1.0)]
    {
        assert_eq!(usable_quote(bid, ask), None, "{bid} {ask}");
    }
}

// ---- the tracker -----------------------------------------------------------------------------------------------------

fn fill_event(order_id: &str, trade_id: &str, side: &str, amount: i64) -> Vec<Event> {
    let v = serde_json::json!({
        "kind": "fill", "order_id": order_id, "trade_id": trade_id, "instrument": EURUSD,
        "side": side, "amount": amount, "rate": 1.1, "commission": 0.0, "timestamp": 0
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
    let mut track = CloseTracker::new(1000, None);
    let (evs, done) = track.relabel("o1", fill_event("o1", "101-close-o1", "S", 1000));
    assert_eq!(names(&evs), ["Fill", "OrderFilled"]);
    assert!(done);
}

#[test]
fn several_legs_are_partial_fills_until_the_last() {
    let mut track = CloseTracker::new(2000, None);
    let (first, done) = track.relabel("o1", fill_event("o1", "101-close-o1", "S", 1000));
    assert_eq!(names(&first), ["Fill", "OrderPartiallyFilled"]);
    assert!(!done);
    let (last, done) = track.relabel("o2", fill_event("o2", "102-close-o2", "S", 1000));
    assert_eq!(names(&last), ["Fill", "OrderFilled"]);
    assert!(done, "the close is complete: the caller forgets it");
}

#[test]
fn legs_that_were_not_sent_end_the_order_cancelled_after_its_last_fill() {
    let mut track = CloseTracker::new(1000, Some("trade 102 was not closed: gone".into()));
    let (evs, done) = track.relabel("o1", fill_event("o1", "101-close-o1", "S", 1000));
    // PartiallyFilled -> Canceled is a legal path; Filled would claim the whole order, and Rejected is illegal there.
    assert_eq!(names(&evs), ["Fill", "OrderPartiallyFilled", "OrderCanceled"]);
    assert!(done);
    let Some(Event::OrderCanceled(c)) = evs.last() else { panic!("a cancel last") };
    assert_eq!(c.order_client_id, "c-1");
    assert!(c.reason.contains("trade 102"), "{}", c.reason);
}

#[test]
fn what_is_not_a_fill_passes_through_untouched() {
    let mut track = CloseTracker::new(1000, Some("x".into()));
    let canceled = vec![Event::OrderCanceled(OrderCanceled {
        order_client_id: "c-1".into(),
        reason: "venue".into(),
        timestamp: 0,
        post_only_would_cross: false,
        refusal_id: None,
    })];
    let before = format!("{canceled:?}");
    let (evs, done) = track.relabel("o1", canceled);
    assert_eq!(format!("{evs:?}"), before, "a removal of a venue order the close did not send");
    assert!(!done);
}

/// The venue removing a sent leg: an Orders Delete with status `R` (`rejected`) or `C` (`canceled`), as the shim writes it.
fn removal(order_id: &str, kind: &str) -> Vec<Event> {
    let v = serde_json::json!({"kind": kind, "order_id": order_id, "timestamp": 7});
    map_fxcm_event(&v, "c-1")
}

fn rest_at(rate: f64) -> LimitRest {
    LimitRest { instrument: EURUSD.into(), buy: false, rate }
}

/// **Finding 1(a) of the 2026-11-16 review: a sent leg the venue removes unfilled is a MISS, never an `OrderRejected`.** The
/// core holds the order `Accepted`, and the FSM refuses a rejection from there: the order stayed `Accepted` for ever. A
/// MARKET close whose one leg is removed ends `OrderCanceled` (legal from `Accepted`), in words, whether the venue said `R`
/// or `C`.
#[test]
fn a_market_close_the_venue_removes_unfilled_ends_cancelled_in_words_never_rejected() {
    for kind in ["rejected", "canceled"] {
        let mut track = CloseTracker::new(10_000, None).with_legs(vec![("o1".into(), 10_000)]);
        let (evs, done) = track.relabel("o1", removal("o1", kind));
        assert_eq!(names(&evs), ["OrderCanceled"], "{kind}");
        assert!(done, "{kind}");
        let Some(Event::OrderCanceled(c)) = evs.last() else { panic!("a cancel") };
        assert!(
            c.reason.contains("without filling it")
                && c.reason.contains("order o1")
                && c.timestamp == 7,
            "{c:?}"
        );
    }
}

/// A CLOSE LIMIT whose every leg is removed unfilled emits nothing: it waits, out of `closes`, for the exec thread to rest
/// its limit as the trade's `L` (`take_missed`), with the words, the units the legs owed and the removal's time.
#[test]
fn a_close_limit_that_missed_whole_is_left_to_rest_as_the_trades_take_profit() {
    let mut closes = HashMap::new();
    let track = CloseTracker::new(10_000, None)
        .with_legs(vec![("o1".into(), 10_000)])
        .resting_at(rest_at(1.0945));
    closes.insert("c-1".to_string(), track);
    let (evs, done) = closes.get_mut("c-1").unwrap().relabel("o1", removal("o1", "rejected"));
    assert!(evs.is_empty() && !done, "{evs:?}");
    let missed = take_missed(&mut closes);
    assert!(closes.is_empty(), "taken out of the table");
    let [(order_client_id, missed)] = &missed[..] else { panic!("one miss: {missed:?}") };
    assert_eq!(order_client_id, "c-1");
    assert_eq!((missed.rest.clone(), missed.units, missed.at), (rest_at(1.0945), 10_000, 7));
    assert!(
        missed.why.contains("removed the close of 10000 units")
            && missed.why.contains("rejected the order after accepting it"),
        "{}",
        missed.why
    );
    assert!(take_missed(&mut closes).is_empty(), "taken once");
}

/// Two legs, one missed: the order goes on while the other owes, and ends `OrderCanceled` in words after its fill (a
/// partly filled close cannot rest a part of a trade as an `L`). In the other order the same: the fill first is partial,
/// the miss ends it.
#[test]
fn a_leg_that_misses_beside_one_that_fills_ends_the_order_cancelled_after_the_fill() {
    let fresh = || {
        CloseTracker::new(2000, None)
            .with_legs(vec![("o1".into(), 1000), ("o2".into(), 1000)])
            .resting_at(rest_at(1.0945))
    };
    let mut track = fresh();
    let (evs, done) = track.relabel("o1", removal("o1", "canceled"));
    assert!(evs.is_empty() && !done, "the other leg still owes: {evs:?}");
    let (evs, done) = track.relabel("o2", fill_event("o2", "102-close-o2", "S", 1000));
    assert_eq!(names(&evs), ["Fill", "OrderPartiallyFilled", "OrderCanceled"]);
    assert!(done);
    let mut track = fresh();
    let (evs, _) = track.relabel("o1", fill_event("o1", "101-close-o1", "S", 1000));
    assert_eq!(names(&evs), ["Fill", "OrderPartiallyFilled"]);
    let (evs, done) = track.relabel("o2", removal("o2", "rejected"));
    assert_eq!(names(&evs), ["OrderCanceled"]);
    assert!(done);
    let mut closes = HashMap::from([("c-1".to_string(), track)]);
    assert!(take_missed(&mut closes).is_empty(), "something filled: nothing to rest");
}

/// An IOC Close Limit that filled in PART and was then removed: only what it still owed is a miss, and the order ends
/// `OrderCanceled` after the part (never rested: a part of a trade is no `L`).
#[test]
fn a_close_limit_that_filled_in_part_then_was_removed_ends_cancelled() {
    let mut track = CloseTracker::new(10_000, None)
        .with_legs(vec![("o1".into(), 10_000)])
        .resting_at(rest_at(1.0945));
    let (evs, done) = track.relabel("o1", fill_event("o1", "101-close-o1", "S", 4000));
    assert_eq!(names(&evs), ["Fill", "OrderPartiallyFilled"]);
    assert!(!done);
    let (evs, done) = track.relabel("o1", removal("o1", "canceled"));
    assert_eq!(names(&evs), ["OrderCanceled"]);
    assert!(done);
    let Some(Event::OrderCanceled(c)) = evs.last() else { panic!("a cancel") };
    assert!(c.reason.contains("close of 6000 units"), "{}", c.reason);
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
