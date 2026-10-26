//! The pure half of binding a stop-loss and a take-profit to a trade: the order shapes the venue's `S` / `L` can express,
//! the open-exits parse, the planner (what the venue enforces, MEASURED 2026-10-26 on the demo: a whole trade per exit,
//! one exit of each kind per trade) and the orchestration over three scripted closures. Nothing here has a session; the
//! shim's shape is `tests/fxcm_shim_exit.rs`, the live proof `tests/fxcm_stop_smoke.rs`.

use super::*;
use crate::sys::Side;

const EURUSD: &str = "EUR/USD";

fn trade(id: &str, buy: bool, units: i64, open_time: f64) -> OpenTrade {
    OpenTrade { trade_id: id.into(), instrument: EURUSD.into(), buy, units, open_time }
}

fn exit(order: &str, trade: &str, kind: ExitKind) -> OpenExit {
    OpenExit { order_id: order.into(), trade_id: trade.into(), kind }
}

// ---- the shapes the venue's S / L can express --------------------------------------------------------------------------

#[test]
fn a_stop_rests_at_its_trigger_and_a_limit_at_its_price() {
    assert_eq!(
        exit_shape(&OrderType::Stop, None, Some(1.1175), None),
        Ok((ExitKind::Stop, 1.1175))
    );
    assert_eq!(
        exit_shape(&OrderType::Stop, None, Some(1.1175), Some(TriggerBy::Last)),
        Ok((ExitKind::Stop, 1.1175)),
        "the venue's own series is what `Last` asks for"
    );
    assert_eq!(
        exit_shape(&OrderType::Limit, Some(1.1215), None, None),
        Ok((ExitKind::Limit, 1.1215))
    );
}

#[test]
fn what_the_venue_cannot_express_is_refused_in_words() {
    let why = |r: Result<(ExitKind, f64), String>| r.expect_err("must be refused");
    // a stop-LIMIT, a stop with no trigger or a bad one, a source the venue does not offer
    assert!(
        why(exit_shape(&OrderType::Stop, Some(1.1), Some(1.1175), None)).contains("stop-LIMIT")
    );
    assert!(
        why(exit_shape(&OrderType::Stop, None, None, None)).contains("trigger price is missing")
    );
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            why(exit_shape(&OrderType::Stop, None, Some(bad), None)).contains("not a positive"),
            "{bad}"
        );
        assert!(
            why(exit_shape(&OrderType::Limit, Some(bad), None, None)).contains("not a positive"),
            "{bad}"
        );
    }
    for source in [TriggerBy::Mark, TriggerBy::Index] {
        let text = why(exit_shape(&OrderType::Stop, None, Some(1.1175), Some(source)));
        assert!(text.contains("its own quote") && text.contains("cannot be chosen"), "{text}");
    }
    // an unpriced limit is an entry at a fixed distance, never an exit
    assert!(
        why(exit_shape(&OrderType::Limit, None, None, None)).contains("limit price is missing")
    );
    // every other kind is neither
    for other in [OrderType::Market, OrderType::StopLimit, OrderType::TakeProfit] {
        assert!(why(exit_shape(&other, Some(1.1), Some(1.1), None)).contains("neither"), "{other}");
    }
}

// ---- the open-exits parse ---------------------------------------------------------------------------------------------

const ROWS: &str = r#"[
  {"order_id":"416154904","trade_id":"186983241","type":"S","buysell":"S","amount":1000},
  {"order_id":"416154901","trade_id":"186983241","type":"L","buysell":"S","amount":1000}
]"#;

#[test]
fn the_shims_rows_parse_into_exits() {
    assert_eq!(
        parse_open_exits(ROWS),
        Ok(vec![
            exit("416154904", "186983241", ExitKind::Stop),
            exit("416154901", "186983241", ExitKind::Limit)
        ])
    );
    assert_eq!(parse_open_exits("[]"), Ok(vec![]), "no resting exit is the venue saying so");
}

#[test]
fn a_row_the_binder_cannot_read_refuses_the_whole_read() {
    // Every key is read: dropping any one of the five fails the parse, and so does a wrong type, side or amount.
    let row: Value = serde_json::from_str(ROWS).map(|v: Vec<Value>| v[0].clone()).unwrap();
    for key in ["order_id", "trade_id", "type", "buysell", "amount"] {
        let mut ablated = row.clone();
        ablated.as_object_mut().unwrap().remove(key);
        let err = parse_open_exits(&format!("[{ablated}]")).expect_err(key);
        assert!(err.contains(key), "the refusal names the key: {err}");
    }
    for bad in [
        r#"[{"order_id":"1","trade_id":"2","type":"LE","buysell":"S","amount":1}]"#,
        r#"[{"order_id":"1","trade_id":"2","type":"S","buysell":"X","amount":1}]"#,
        r#"[{"order_id":"1","trade_id":"2","type":"S","buysell":"S","amount":0}]"#,
        r#"[{"order_id":"1","trade_id":"2","type":"S","buysell":"S","amount":1.5}]"#,
        r#"[{"order_id":1,"trade_id":"2","type":"S","buysell":"S","amount":1}]"#,
        r#"{"not":"an array"}"#,
    ] {
        assert!(parse_open_exits(bad).is_err(), "{bad}");
    }
    // A good row beside a bad one does not rescue the read: skipping it could report a covered trade as free.
    let mixed = format!(
        r#"[{ok},{{"order_id":"2","trade_id":"3","type":"S","buysell":"S","amount":0}}]"#,
        ok = row
    );
    assert!(parse_open_exits(&mixed).is_err());
}

// ---- the planner -----------------------------------------------------------------------------------------------------

fn plan(
    trades: &[OpenTrade],
    exits: &[OpenExit],
    buy: bool,
    kind: ExitKind,
    units: i64,
) -> Result<ExitPlan, String> {
    plan_exit(trades, exits, EURUSD, buy, kind, units)
}

fn bound_to(plan: Result<ExitPlan, String>) -> (String, i64) {
    let plan = plan.expect("a plan");
    (plan.trade_id, plan.units)
}

#[test]
fn a_sell_stop_binds_to_the_long_trade_of_its_size() {
    let book = [trade("101", true, 1000, 1.0)];
    assert_eq!(bound_to(plan(&book, &[], false, ExitKind::Stop, 1000)), ("101".into(), 1000));
}

#[test]
fn a_buy_stop_binds_to_the_short_trade_and_a_sell_never_binds_to_a_short() {
    let book = [trade("101", false, 1000, 1.0)];
    assert_eq!(bound_to(plan(&book, &[], true, ExitKind::Stop, 1000)), ("101".into(), 1000));
    let err = plan(&book, &[], false, ExitKind::Stop, 1000).expect_err("a sell faces longs");
    assert!(err.contains("no open long trade on EUR/USD") && err.contains("never opens"), "{err}");
}

/// Both exits of a bracket bind the SAME trade: the stop-loss does not make the trade busy for the take-profit (the venue
/// keeps the pair as one OCO group), and only an exit of the SAME kind does.
#[test]
fn a_bracket_binds_its_stop_and_its_take_profit_to_the_same_trade() {
    let book = [trade("101", true, 1000, 1.0)];
    let stop_on_it = [exit("9", "101", ExitKind::Stop)];
    assert_eq!(
        bound_to(plan(&book, &stop_on_it, false, ExitKind::Limit, 1000)),
        ("101".into(), 1000)
    );
    let both = [exit("9", "101", ExitKind::Stop), exit("10", "101", ExitKind::Limit)];
    for kind in [ExitKind::Stop, ExitKind::Limit] {
        let err = plan(&book, &both, false, kind, 1000).expect_err("covered");
        assert!(err.contains("already has a") && err.contains("order "), "{err}");
    }
}

#[test]
fn a_covered_trade_is_skipped_for_the_next_one_of_the_size_and_the_oldest_is_taken_first() {
    let book = [
        trade("102", true, 1000, 2.0),
        trade("101", true, 1000, 1.0),
        trade("103", true, 1000, 3.0),
    ];
    // oldest first by open time, whatever order the table came in
    assert_eq!(bound_to(plan(&book, &[], false, ExitKind::Stop, 1000)).0, "101");
    // 101 has its stop: the next stop goes to the next oldest, 102
    let covered = [exit("9", "101", ExitKind::Stop)];
    assert_eq!(bound_to(plan(&book, &covered, false, ExitKind::Stop, 1000)).0, "102");
    // ... and a take-profit is still free to go to 101
    assert_eq!(bound_to(plan(&book, &covered, false, ExitKind::Limit, 1000)).0, "101");
}

#[test]
fn an_exit_goes_to_the_trade_of_exactly_its_size_not_to_an_older_one_of_another() {
    let book = [trade("101", true, 3000, 1.0), trade("102", true, 1000, 2.0)];
    assert_eq!(bound_to(plan(&book, &[], false, ExitKind::Stop, 1000)), ("102".into(), 1000));
}

#[test]
fn a_part_of_a_trade_is_refused_because_the_venue_refuses_it() {
    let book = [trade("101", true, 2000, 1.0)];
    let err = plan(&book, &[], false, ExitKind::Stop, 1000).expect_err("a part of a trade");
    assert!(
        err.contains("WHOLE trade") && err.contains("101 (2000)") && err.contains("ORA-20134"),
        "{err}"
    );
}

/// A reduce-only order never closes more than the position holds: with exactly ONE free trade, an exit asked for more is
/// placed for that trade's whole size (the caller logs the cap). With more than one it is refused: a cap would cover the
/// oldest and leave the rest naked while the engine believes them covered.
#[test]
fn an_exit_asked_for_more_than_the_one_free_trade_holds_is_capped_to_it() {
    let one = [trade("101", true, 1000, 1.0)];
    assert_eq!(bound_to(plan(&one, &[], false, ExitKind::Stop, 5000)), ("101".into(), 1000));
    // a covered trade does not count as free: the cap goes to the one that is
    let two = [trade("101", true, 1000, 1.0), trade("102", true, 400, 2.0)];
    let covered = [exit("9", "101", ExitKind::Stop)];
    assert_eq!(bound_to(plan(&two, &covered, false, ExitKind::Stop, 1000)), ("102".into(), 400));
}

#[test]
fn an_exit_that_would_span_several_trades_is_refused_by_name() {
    let book = [trade("101", true, 1000, 1.0), trade("102", true, 1000, 2.0)];
    let err = plan(&book, &[], false, ExitKind::Stop, 2000).expect_err("spans two trades");
    assert!(err.contains("101 (1000), 102 (1000)") && err.contains("one per trade"), "{err}");
}

#[test]
fn nothing_facing_the_order_and_a_bad_size_are_named_refusals() {
    for units in [1000, 0, -5] {
        let err = plan(&[], &[], false, ExitKind::Stop, units).expect_err("no trade");
        assert!(err.contains("no open long trade") && err.contains("stop"), "{err}");
    }
    // another instrument's trades do not face it
    let other = [OpenTrade { instrument: "EUR/GBP".into(), ..trade("101", true, 1000, 1.0) }];
    assert!(plan(&other, &[], false, ExitKind::Limit, 1000).is_err());
}

// ---- the orchestration over the three closures -----------------------------------------------------------------------

const TRADES: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.5}
]"#;

fn placed(order_id: &str, rate: f64) -> PlacedOrder {
    PlacedOrder { order_id: order_id.into(), offer_id: "1".into(), account_id: "7".into(), rate }
}

fn native(code: i32, message: &str) -> FxcmError {
    FxcmError::Native { code, message: message.into() }
}

#[test]
fn a_stop_is_read_planned_and_placed_for_the_trades_whole_amount() {
    let sent = std::cell::RefCell::new(Vec::new());
    let bound = bind_exit(
        EURUSD,
        false,
        ExitKind::Stop,
        1000,
        || Ok(TRADES.to_string()),
        || Ok("[]".to_string()),
        |trade, units| {
            sent.borrow_mut().push((trade.to_string(), units));
            Ok(placed("416154904", 1.1175))
        },
    )
    .expect("placed");
    assert_eq!(*sent.borrow(), vec![("101".to_string(), 1000)]);
    assert_eq!((bound.trade_id.as_str(), bound.units), ("101", 1000));
    assert_eq!(bound.placed.order_id, "416154904");
}

#[test]
fn a_capped_stop_is_placed_for_the_trade_and_says_so() {
    let sent = std::cell::RefCell::new(Vec::new());
    let bound = bind_exit(
        EURUSD,
        false,
        ExitKind::Stop,
        5000,
        || Ok(TRADES.to_string()),
        || Ok("[]".to_string()),
        |trade, units| {
            sent.borrow_mut().push((trade.to_string(), units));
            Ok(placed("1", 1.1))
        },
    )
    .expect("placed");
    assert_eq!(
        *sent.borrow(),
        vec![("101".to_string(), 1000)],
        "the trade's own size is what is sent"
    );
    assert_eq!(bound.units, 1000, "the caller compares this with the size asked");
}

/// A book that cannot be read, or cannot be planned, sends NOTHING and is a DEFINITE refusal (`false`).
#[test]
fn a_book_that_cannot_be_read_or_planned_sends_nothing() {
    let never = |_: &str, _: i32| -> Result<PlacedOrder, FxcmError> { panic!("must not be sent") };
    for (trades, exits, expect) in [
        (Err(native(-8, "timeout")), Ok("[]"), "could not read the account's open trades"),
        (Ok(TRADES), Err(native(-8, "timeout")), "could not read the account's resting exits"),
        (Ok("not json"), Ok("[]"), "not a JSON array"),
        (Ok(TRADES), Ok("not json"), "not a JSON array"),
        (Ok("[]"), Ok("[]"), "no open long trade"),
        (
            Ok(TRADES),
            Ok(r#"[{"order_id":"9","trade_id":"101","type":"S","buysell":"S","amount":1000}]"#),
            "already has a stop",
        ),
        (Ok(TRADES), Ok("[]"), "WHOLE trade"),
    ] {
        let units = if expect == "WHOLE trade" { 400 } else { 1000 };
        let failure = bind_exit(
            EURUSD,
            false,
            ExitKind::Stop,
            units,
            || trades.map(str::to_string),
            || exits.map(str::to_string),
            never,
        )
        .expect_err(expect);
        assert!(!failure.0, "a refusal before the wire is definite: {failure:?}");
        assert!(failure.1.contains(expect), "{expect}: {}", failure.1);
        assert!(failure.2.is_none());
    }
}

#[test]
fn the_venues_refusal_is_the_orders_and_a_lost_reply_stays_unknown_with_its_lookup() {
    // the venue refuses (its own words ride along)
    let refused = bind_exit(
        EURUSD,
        false,
        ExitKind::Stop,
        1000,
        || Ok(TRADES.to_string()),
        || Ok("[]".to_string()),
        |_, _| Err(native(-6, "ORA-20115: Stop/Limit price did not pass validation")),
    )
    .expect_err("refused");
    assert!(!refused.0 && refused.1.contains("ORA-20115") && refused.2.is_none());

    // sent, no usable answer: UNKNOWN, with what `confirm` looks the order up by
    let lookup = PendingLookup {
        request_id: "REQ-1".into(),
        account_id: "7".into(),
        offer_id: "1".into(),
        side: Side::Sell,
        amount: 1000,
        limit: false,
        rate: 1.1175,
    };
    let lost = bind_exit(
        EURUSD,
        false,
        ExitKind::Stop,
        1000,
        || Ok(TRADES.to_string()),
        || Ok("[]".to_string()),
        |_, _| {
            Err(FxcmError::SentUnknown {
                code: -11,
                message: "no answer".into(),
                lookup: Some(Box::new(lookup.clone())),
            })
        },
    )
    .expect_err("unknown");
    assert!(lost.0, "a stop whose reply was lost may rest: never a refusal");
    assert_eq!(lost.2, Some(lookup));
}
