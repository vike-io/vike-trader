//! The pure half of binding a stop-loss and a take-profit to the trades it covers: the order shapes the venue's `S` / `L`
//! can express, the open-exits parse, the planner (what the venue enforces, MEASURED 2026-10-26 on the demo: a whole trade
//! per exit, one exit of each kind per trade; and what this node adds: one order over several whole trades, and the takeover
//! of an exit a restart left) and the orchestration over a scripted venue. Nothing here has a session; the shim's shape is
//! `tests/fxcm_shim_exit.rs`, the live proof `tests/fxcm_stop_smoke.rs`.

use super::*;
use crate::exit_script::{Script, accepted};
use crate::sys::Side;

const EURUSD: &str = "EUR/USD";
const RATE: f64 = 1.1175;

fn trade(id: &str, buy: bool, units: i64, open_time: f64) -> OpenTrade {
    OpenTrade { trade_id: id.into(), instrument: EURUSD.into(), buy, units, open_time }
}

/// An exit as an OLD shim reports it: no rate, no ids.
fn exit(order: &str, trade: &str, kind: ExitKind) -> OpenExit {
    OpenExit {
        order_id: order.into(),
        trade_id: trade.into(),
        kind,
        units: 1000,
        rate: None,
        account_id: String::new(),
        offer_id: String::new(),
    }
}

/// An exit as the current shim reports it: its rate and the ids a cancel needs.
fn resting(order: &str, trade: &str, kind: ExitKind, units: i64, rate: f64) -> OpenExit {
    OpenExit {
        units,
        rate: Some(rate),
        account_id: "7".into(),
        offer_id: "1".into(),
        ..exit(order, trade, kind)
    }
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

/// The current shim's row carries the exit's rate and the ids a cancel needs; an older one's does not, and its exits are
/// then never taken over (nothing proves they are the one asked for).
#[test]
fn a_row_with_the_rate_and_the_ids_keeps_them_and_a_bad_rate_is_none() {
    let rows = r#"[
      {"order_id":"9","trade_id":"101","type":"S","buysell":"S","amount":1000,"rate":1.117500,"account_id":"7","offer_id":"1"},
      {"order_id":"10","trade_id":"102","type":"L","buysell":"S","amount":2000,"rate":0.0,"account_id":"7","offer_id":"1"},
      {"order_id":"11","trade_id":"103","type":"S","buysell":"S","amount":3000,"rate":"1.1","account_id":7}
    ]"#;
    let parsed = parse_open_exits(rows).expect("parses");
    assert_eq!(parsed[0], resting("9", "101", ExitKind::Stop, 1000, 1.1175));
    assert_eq!(parsed[1].rate, None, "a zero rate is no rate");
    assert_eq!((parsed[1].units, parsed[1].account_id.as_str()), (2000, "7"));
    assert_eq!(parsed[2].rate, None, "a rate that is not a number is no rate");
    assert_eq!(parsed[2].account_id, "", "an id that is not text is no id");
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

fn ask(buy: bool, kind: ExitKind, units: i64) -> Ask<'static> {
    Ask { instrument: EURUSD, buy, kind, units, rate: RATE }
}

fn plan(
    trades: &[OpenTrade],
    exits: &[OpenExit],
    buy: bool,
    kind: ExitKind,
    units: i64,
) -> Result<Vec<LegPlan>, String> {
    plan_exits(trades, exits, &ask(buy, kind, units), &|_| false)
}

/// The trades and sizes a plan covers, in its order.
fn covers(plan: Result<Vec<LegPlan>, String>) -> Vec<(String, i64)> {
    plan.expect("a plan").into_iter().map(|s| (s.trade_id, s.units)).collect()
}

fn bound_to(plan: Result<Vec<LegPlan>, String>) -> (String, i64) {
    let legs = covers(plan);
    assert_eq!(legs.len(), 1, "one trade: {legs:?}");
    legs.into_iter().next().expect("one leg")
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
/// placed for that trade's whole size (the caller logs the cap). With more than one it is refused unless the oldest add up
/// to the size: a cap would cover the oldest and leave the rest naked while the engine believes them covered.
#[test]
fn an_exit_asked_for_more_than_the_one_free_trade_holds_is_capped_to_it() {
    let one = [trade("101", true, 1000, 1.0)];
    assert_eq!(bound_to(plan(&one, &[], false, ExitKind::Stop, 5000)), ("101".into(), 1000));
    // a covered trade does not count as free: the cap goes to the one that is
    let two = [trade("101", true, 1000, 1.0), trade("102", true, 400, 2.0)];
    let covered = [exit("9", "101", ExitKind::Stop)];
    assert_eq!(bound_to(plan(&two, &covered, false, ExitKind::Stop, 1000)), ("102".into(), 400));
}

// ---- an exit that spans trades ------------------------------------------------------------------------------------------

/// The order is the sum of the oldest free trades: one exit per trade, oldest first, under the one order.
#[test]
fn an_exit_that_spans_trades_takes_the_oldest_free_ones_whole() {
    let book = [trade("102", true, 1000, 2.0), trade("101", true, 1000, 1.0)];
    assert_eq!(
        covers(plan(&book, &[], false, ExitKind::Stop, 2000)),
        [("101".to_string(), 1000), ("102".to_string(), 1000)]
    );
    // sizes may differ; the prefix must add up exactly
    let mixed = [
        trade("101", true, 1000, 1.0),
        trade("102", true, 2000, 2.0),
        trade("103", true, 1000, 3.0),
    ];
    assert_eq!(
        covers(plan(&mixed, &[], false, ExitKind::Limit, 3000)),
        [("101".to_string(), 1000), ("102".to_string(), 2000)]
    );
    // a short book takes buy exits
    let shorts = [trade("201", false, 500, 1.0), trade("202", false, 500, 2.0)];
    assert_eq!(covers(plan(&shorts, &[], true, ExitKind::Stop, 1000)).len(), 2);
}

/// A trade that already has an exit of the kind is skipped by the span, as it is by a single exit.
#[test]
fn a_span_skips_the_trades_that_are_covered() {
    let book = [
        trade("101", true, 1000, 1.0),
        trade("102", true, 1000, 2.0),
        trade("103", true, 1000, 3.0),
    ];
    let covered = [exit("9", "101", ExitKind::Stop)];
    assert_eq!(
        covers(plan(&book, &covered, false, ExitKind::Stop, 2000)),
        [("102".to_string(), 1000), ("103".to_string(), 1000)]
    );
}

/// The single trade of exactly the order's size is taken before any span, even past an older smaller trade.
#[test]
fn one_trade_of_the_size_beats_a_span() {
    let book = [trade("101", true, 1000, 1.0), trade("102", true, 2000, 2.0)];
    assert_eq!(bound_to(plan(&book, &[], false, ExitKind::Stop, 2000)), ("102".into(), 2000));
}

/// A size that falls inside a trade, past the sum of the free ones, or that only a non-adjacent subset reaches, is refused
/// by name with every free trade's size: the venue refuses a part of a trade.
#[test]
fn a_size_that_is_not_the_sum_of_the_oldest_trades_is_refused_by_name() {
    let book = [
        trade("101", true, 1000, 1.0),
        trade("102", true, 1000, 2.0),
        trade("103", true, 1000, 3.0),
    ];
    for units in [1500, 2500, 5000] {
        let err = plan(&book, &[], false, ExitKind::Stop, units).expect_err("not whole trades");
        assert!(
            err.contains("WHOLE trades")
                && err.contains("101 (1000), 102 (1000), 103 (1000)")
                && err.contains("ORA-20134"),
            "{units}: {err}"
        );
    }
    // 2000 = 1000 + 1000 exists as a subset of [1000, 1500, 1000] but not as a prefix
    let gap = [
        trade("101", true, 1000, 1.0),
        trade("102", true, 1500, 2.0),
        trade("103", true, 1000, 3.0),
    ];
    assert!(plan(&gap, &[], false, ExitKind::Stop, 2000).is_err());
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

// ---- an exit the venue holds that this process did not place -------------------------------------------------------

fn plan_ours(
    trades: &[OpenTrade],
    exits: &[OpenExit],
    units: i64,
    ours: &[&str],
) -> Result<Vec<LegPlan>, String> {
    let ours = |oid: &str| ours.contains(&oid);
    plan_exits(trades, exits, &ask(false, ExitKind::Stop, units), &ours)
}

/// The exits a restart leaves are at the venue and the node knows none of them: a stop of the SAME rate for the trade is
/// that exit, taken over and not sent.
#[test]
fn an_exit_of_the_same_rate_that_this_process_did_not_place_is_taken_over() {
    let book = [trade("101", true, 1000, 1.0)];
    let left = [resting("9", "101", ExitKind::Stop, 1000, RATE)];
    let steps = plan_ours(&book, &left, 1000, &[]).expect("taken over");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].trade_id, "101");
    assert_eq!(steps[0].takeover.as_ref().map(|e| e.order_id.as_str()), Some("9"));
}

/// ... and it is NOT when it is this process's own (a second stop for a trade this order already covers), when it rests at
/// another rate (it may be a person's: a foreign order is cancelled by hand only), when it is not the trade's whole size or
/// when the shim reported no rate or no ids to cancel it by. Each is the refusal an exit that covers the trade always was.
#[test]
fn an_exit_that_is_not_exactly_the_one_asked_for_is_not_taken_over() {
    let book = [trade("101", true, 1000, 1.0)];
    let own = [resting("9", "101", ExitKind::Stop, 1000, RATE)];
    let err = plan_ours(&book, &own, 1000, &["9"]).expect_err("this process's own exit");
    assert!(err.contains("already has a stop") && err.contains("order 9 on trade 101"), "{err}");

    let elsewhere = [resting("9", "101", ExitKind::Stop, 1000, 1.12)];
    let err = plan_ours(&book, &elsewhere, 1000, &[]).expect_err("another rate");
    assert!(err.contains("already has a stop") && err.contains("at 1.12"), "{err}");

    for odd in [
        // a size that is not the trade's
        resting("9", "101", ExitKind::Stop, 400, RATE),
        // an older shim: no rate
        exit("9", "101", ExitKind::Stop),
        // no ids to cancel it by
        OpenExit { account_id: String::new(), ..resting("9", "101", ExitKind::Stop, 1000, RATE) },
        OpenExit { offer_id: String::new(), ..resting("9", "101", ExitKind::Stop, 1000, RATE) },
    ] {
        let err =
            plan_ours(&book, std::slice::from_ref(&odd), 1000, &[]).expect_err("not taken over");
        assert!(err.contains("already has a stop"), "{odd:?}: {err}");
    }
    // a take-profit is another kind: the stop on the trade is none of its business
    let stop_only = [resting("9", "101", ExitKind::Stop, 1000, RATE)];
    let steps = plan(&book, &stop_only, false, ExitKind::Limit, 1000).expect("free for a limit");
    assert!(steps[0].takeover.is_none());
}

/// A restart left a stop on each of two trades: the stop for both is two takeovers; with one of them left, a takeover and a
/// placement.
#[test]
fn a_span_takes_over_what_rests_and_places_the_rest() {
    let book = [trade("101", true, 1000, 1.0), trade("102", true, 1000, 2.0)];
    let both = [
        resting("9", "101", ExitKind::Stop, 1000, RATE),
        resting("10", "102", ExitKind::Stop, 1000, RATE),
    ];
    let steps = plan_ours(&book, &both, 2000, &[]).expect("two takeovers");
    assert!(steps.iter().all(|s| s.takeover.is_some()));
    let one = [resting("9", "101", ExitKind::Stop, 1000, RATE)];
    let steps = plan_ours(&book, &one, 2000, &[]).expect("a takeover and a placement");
    assert_eq!(
        steps.iter().map(|s| (s.trade_id.as_str(), s.takeover.is_some())).collect::<Vec<_>>(),
        [("101", true), ("102", false)]
    );
}

// ---- the orchestration over a scripted venue -------------------------------------------------------------------------

const TRADES: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.5}
]"#;

const THREE: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.1},
  {"trade_id":"102","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.2},
  {"trade_id":"103","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.3}
]"#;

fn native(code: i32, message: &str) -> FxcmError {
    FxcmError::Native { code, message: message.into() }
}

fn stop_for(units: i64) -> Ask<'static> {
    ask(false, ExitKind::Stop, units)
}

fn bind(venue: &mut Script, units: i64, ours: &[&str]) -> Result<Bound, BindFailure> {
    let ours = |oid: &str| ours.contains(&oid);
    bind_exits(&stop_for(units), &ours, venue)
}

#[test]
fn a_stop_is_read_planned_and_placed_for_the_trades_whole_amount() {
    let mut venue = Script::new(TRADES, "[]");
    venue.placements.push_back(Ok(accepted("416154904", RATE)));
    let bound = bind(&mut venue, 1000, &[]).expect("placed");
    assert_eq!(venue.log, ["place 101 1000 1.1175"]);
    assert_eq!(bound.legs.len(), 1);
    assert_eq!((bound.legs[0].trade_id.as_str(), bound.legs[0].units), ("101", 1000));
    assert_eq!(bound.legs[0].order_id, "416154904");
    assert!(!bound.legs[0].taken_over);
    assert_eq!(bound.units(), 1000);
}

#[test]
fn a_capped_stop_is_placed_for_the_trade_and_says_so() {
    let mut venue = Script::new(TRADES, "[]");
    let bound = bind(&mut venue, 5000, &[]).expect("placed");
    assert_eq!(venue.log, ["place 101 1000 1.1175"], "the trade's own size is what is sent");
    assert_eq!(bound.units(), 1000, "the caller compares this with the size asked");
}

/// One order over three trades: three placements, in order, one leg each, the legs' ids all there for the routes.
#[test]
fn a_stop_that_spans_trades_places_one_exit_per_trade_in_order() {
    let mut venue = Script::new(THREE, "[]");
    let bound = bind(&mut venue, 3000, &[]).expect("placed");
    assert_eq!(
        venue.log,
        ["place 101 1000 1.1175", "place 102 1000 1.1175", "place 103 1000 1.1175"]
    );
    assert_eq!(
        bound
            .legs
            .iter()
            .map(|l| (l.order_id.as_str(), l.trade_id.as_str(), l.units))
            .collect::<Vec<_>>(),
        [("P1", "101", 1000), ("P2", "102", 1000), ("P3", "103", 1000)]
    );
}

/// Nothing is sent for an exit that is taken over, and a leg that is carries the venue's ids so it can be cancelled.
#[test]
fn a_takeover_sends_nothing_and_names_the_venues_order() {
    let rows = r#"[{"order_id":"9","trade_id":"101","type":"S","buysell":"S","amount":1000,"rate":1.1175,"account_id":"7","offer_id":"1"}]"#;
    let mut venue = Script::new(TRADES, rows);
    let bound = bind(&mut venue, 1000, &[]).expect("taken over");
    assert!(venue.log.is_empty(), "no write: {:?}", venue.log);
    let leg = &bound.legs[0];
    assert_eq!(
        (leg.order_id.as_str(), leg.account_id.as_str(), leg.offer_id.as_str()),
        ("9", "7", "1")
    );
    assert!(leg.taken_over);
    assert_eq!(leg.rate, 1.1175);
    // the same row is this process's own once its id is routed: then it is a second stop and is refused
    let mut venue = Script::new(TRADES, rows);
    let refused = bind(&mut venue, 1000, &["9"]).expect_err("own exit");
    assert!(!refused.0 && refused.1.contains("already has a stop"));
    assert!(venue.log.is_empty());
}

/// A book that cannot be read, or cannot be planned, sends NOTHING and is a DEFINITE refusal (`false`).
#[test]
fn a_book_that_cannot_be_read_or_planned_sends_nothing() {
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
        let mut venue = Script::new("[]", "[]");
        venue.trades = trades.map(str::to_string);
        venue.exits = exits.map(str::to_string);
        let units = if expect == "WHOLE trade" { 400 } else { 1000 };
        let failure = bind(&mut venue, units, &[]).expect_err(expect);
        assert!(!failure.0, "a refusal before the wire is definite: {failure:?}");
        assert!(failure.1.contains(expect), "{expect}: {}", failure.1);
        assert!(failure.2.is_none());
        assert!(venue.log.is_empty(), "{expect}: nothing may be sent: {:?}", venue.log);
    }
}

#[test]
fn the_venues_refusal_is_the_orders_and_a_lost_reply_stays_unknown_with_its_lookup() {
    // the venue refuses (its own words ride along)
    let mut venue = Script::new(TRADES, "[]");
    venue
        .placements
        .push_back(Err(native(-6, "ORA-20115: Stop/Limit price did not pass validation")));
    let refused = bind(&mut venue, 1000, &[]).expect_err("refused");
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
    let mut venue = Script::new(TRADES, "[]");
    venue.placements.push_back(Err(FxcmError::SentUnknown {
        code: -11,
        message: "no answer".into(),
        lookup: Some(Box::new(lookup.clone())),
    }));
    let lost = bind(&mut venue, 1000, &[]).expect_err("unknown");
    assert!(lost.0, "a stop whose reply was lost may rest: never a refusal");
    assert_eq!(lost.2, Some(lookup));
}

fn unknown_answer() -> FxcmError {
    FxcmError::SentUnknown { code: -11, message: "no answer".into(), lookup: None }
}

/// The FIRST of several exits having no usable answer keeps the order unknown but with NO lookup: a `confirm` that found
/// that one exit would accept an order whose other exits were never placed.
#[test]
fn a_lost_reply_on_the_first_of_several_legs_is_unknown_and_cannot_be_looked_up() {
    let lookup = PendingLookup {
        request_id: "REQ-1".into(),
        account_id: "7".into(),
        offer_id: "1".into(),
        side: Side::Sell,
        amount: 1000,
        limit: false,
        rate: 1.1175,
    };
    let mut venue = Script::new(THREE, "[]");
    venue.placements.push_back(Err(FxcmError::SentUnknown {
        code: -11,
        message: "no answer".into(),
        lookup: Some(Box::new(lookup)),
    }));
    let lost = bind(&mut venue, 3000, &[]).expect_err("unknown");
    assert!(lost.0 && lost.2.is_none(), "{lost:?}");
    assert_eq!(venue.log, ["place 101 1000 1.1175"], "the other legs were never sent");
}

/// A leg that fails AFTER others rest refuses the order whole and removes what was placed.
#[test]
fn a_leg_that_fails_after_others_were_placed_removes_them_and_refuses_the_order() {
    let mut venue = Script::new(THREE, "[]");
    venue.placements.push_back(Ok(accepted("A1", RATE)));
    venue.placements.push_back(Ok(accepted("A2", RATE)));
    venue.placements.push_back(Err(native(-6, "ORA-20114: more than one order of this type")));
    let failure = bind(&mut venue, 3000, &[]).expect_err("refused whole");
    assert!(!failure.0 && failure.2.is_none());
    assert!(
        failure.1.contains("covers 3 trades")
            && failure.1.contains("trade 103 was refused")
            && failure.1.contains("ORA-20114")
            && failure.1.contains("2 exit(s) it had placed were removed again"),
        "{}",
        failure.1
    );
    assert_eq!(
        venue.log,
        [
            "place 101 1000 1.1175",
            "place 102 1000 1.1175",
            "place 103 1000 1.1175",
            "delete A1",
            "delete A2"
        ]
    );
}

/// A later leg with no usable answer may rest: the order is refused whole, the others are removed, and the words say so.
#[test]
fn a_later_leg_with_no_answer_is_refused_whole_and_says_it_may_rest() {
    let mut venue = Script::new(THREE, "[]");
    venue.placements.push_back(Ok(accepted("A1", RATE)));
    venue.placements.push_back(Err(unknown_answer()));
    let failure = bind(&mut venue, 3000, &[]).expect_err("refused whole");
    assert!(!failure.0, "the order is refused, not left unknown: {failure:?}");
    assert!(
        failure.1.contains("MAY rest at the venue") && failure.1.contains("trade 102"),
        "{}",
        failure.1
    );
    assert_eq!(venue.log.last().map(String::as_str), Some("delete A1"));
}

/// An exit that cannot be removed again is named, so the operator can cancel it by hand.
#[test]
fn an_exit_that_cannot_be_removed_again_is_named() {
    let mut venue = Script::new(THREE, "[]");
    venue.placements.push_back(Ok(accepted("A1", RATE)));
    venue.placements.push_back(Err(native(-6, "refused")));
    venue.delete_refused.insert("A1".into());
    let failure = bind(&mut venue, 3000, &[]).expect_err("refused whole");
    assert!(
        failure.1.contains("could NOT be removed")
            && failure.1.contains("order A1 on trade 101")
            && failure.1.contains("cancel them by hand"),
        "{}",
        failure.1
    );
}

/// The time bound of one command ends the sending: no leg is started past it, and what was placed is removed.
#[test]
fn the_time_bound_of_one_command_refuses_the_order_whole() {
    let mut venue = Script::new(THREE, "[]");
    // `may_start` is asked before the second and the third leg: the first answer is already "no time"
    venue.out_of_time_from = Some(1);
    let failure = bind(&mut venue, 3000, &[]).expect_err("out of time");
    assert!(!failure.0);
    assert!(
        failure.1.contains("time bound of one order ran out") && failure.1.contains("1 of 3"),
        "{}",
        failure.1
    );
    assert_eq!(venue.log, ["place 101 1000 1.1175", "delete P1"]);
}

/// An exit that was taken over was at the venue before this order: a bind that fails later leaves it where it was.
#[test]
fn a_failed_bind_never_removes_an_exit_it_took_over() {
    let rows = r#"[{"order_id":"9","trade_id":"101","type":"S","buysell":"S","amount":1000,"rate":1.1175,"account_id":"7","offer_id":"1"}]"#;
    let mut venue = Script::new(THREE, rows);
    venue.placements.push_back(Err(native(-6, "refused")));
    let failure = bind(&mut venue, 2000, &[]).expect_err("refused whole");
    assert_eq!(venue.log, ["place 102 1000 1.1175"], "no delete of order 9");
    assert!(
        failure.1.contains("1 exit(s) taken over from the venue were left where they were"),
        "{}",
        failure.1
    );
}
