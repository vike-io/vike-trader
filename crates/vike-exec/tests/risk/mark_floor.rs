//! The MARK FLOOR (`RiskGate::require_mark`): an engine that holds no mark at all for its own MOUNTED
//! symbol (never priced: absent, zero, negative or non-finite) denies an OPENING order on it, by name
//! (`no-mark-for-mounted-symbol`), instead of judging it at notional zero. It tests PRESENCE, not
//! age: `an_old_mark_still_stands_because_no_freshness_window_is_set` pins that limit.
//!
//! The shape is a binance LANE TWIN: a labelled engine mounted on `BTCUSDT.P` whose symbol the
//! venue's default engine does not accept, so the quote route never prices it. Without a mark a
//! MARKET order has `ctx.mark_price == 0.0` and `over-max-notional`, `over-max-exposure`,
//! `over-account-exposure` and `insufficient-margin` all see notional zero and pass: "a 0.0 mark
//! VACATES the exposure cap at any size". The floor is driven here through `submit_order`, whose
//! `risk_ctx` builds the fact, and the admission that seeds the mounted symbol's first mark.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, EventHandler, ExecutionEngine, MarkSource, Outbox, SymbolAdmission, state_hash,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{AccountTier, OrderRequest, OrderType, RiskLimits, SymbolGrid};

use crate::support::EngineBuilder;

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT.P";
/// Per-order and per-symbol ceiling: 0.1 BTC at the mark is far over it, 0.01 BTC is far under.
const CAP: f64 = 5_000.0;
const MARK: f64 = 74_300.0;

/// Turn the floor on: what the live assembly does for the derived lane twin engine, and for no other.
fn arm(e: &mut ExecutionEngine<RecordingClient>) {
    e.gate.require_mark = true;
}

/// The twin: `binance#PERP` on `BTCUSDT.P`, at the `live` tier, both ceilings armed, no mark yet.
fn twin(armed: bool) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        max_notional_per_order: Some(CAP),
        max_total_exposure: Some(CAP),
        ..RiskLimits::new()
    };
    let mut e = EngineBuilder {
        venue: VENUE.into(),
        symbol: MOUNTED.into(),
        limits,
        extra_symbols: vec!["ETHUSDT".into()],
        ..Default::default()
    }
    .build();
    e.route_key = "binance#PERP".into();
    e.tier = AccountTier::Live;
    if armed {
        arm(&mut e);
    }
    e
}

fn order(
    coid: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    order_type: OrderType,
    price: Option<f64>,
    reduce_only: bool,
) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type,
        price,
        reduce_only,
        ts: 1,
        ..Default::default()
    }
}

fn market(coid: &str, side: i32, qty: f64) -> OrderRequest {
    order(coid, MOUNTED, side, qty, OrderType::Market, None, false)
}

/// The denial word of `req`, or `None` when it reached the venue client.
fn submit(e: &mut ExecutionEngine<RecordingClient>, req: &OrderRequest) -> Option<String> {
    let mut ob = Outbox::default();
    e.submit_order(req, 1, &mut ob);
    ob.0.into_iter().find_map(|ev| match ev {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

/// A venue price for the mounted symbol, the way the mark lane files one.
fn price(e: &mut ExecutionEngine<RecordingClient>, px: f64) {
    e.price_board.set_mark(VENUE, MOUNTED, px, 1);
    e.account.set_mark_from(VENUE, MOUNTED, px, MarkSource::VenueMark, 1);
}

fn long_fill(qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: "t1".into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: MOUNTED.into(),
        side: 1,
        last_qty: qty,
        last_px: 74_000.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 2,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn admission(symbol: &str, initial_mark: Option<f64>) -> SymbolAdmission {
    SymbolAdmission {
        route_key: "binance#PERP".into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.1), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    }
}

/// **The headline.** No mark: a MARKET buy far above the cap is denied by NAME and never reaches the
/// venue. (Unarmed, the same order passes at notional zero: the test below pins that.)
#[test]
fn an_unmarked_mounted_symbol_denies_an_opening_market_order_by_name() {
    let mut e = twin(true);
    let big = market("big", 1, 0.1);
    assert_eq!(submit(&mut e, &big).as_deref(), Some("no-mark-for-mounted-symbol"));
    assert!(e.client.submissions.is_empty(), "nothing reached the venue");
}

/// A LIMIT order is no better off: against its own price the collar compares the price with itself and
/// the caps read the order's number. With no mark of the engine's own it is denied too.
#[test]
fn an_unmarked_mounted_symbol_denies_an_opening_limit_order_too() {
    let mut e = twin(true);
    let small = order("lim", MOUNTED, 1, 0.01, OrderType::Limit, Some(MARK), false);
    assert_eq!(submit(&mut e, &small).as_deref(), Some("no-mark-for-mounted-symbol"));
    assert!(e.client.submissions.is_empty());
}

/// **After one mark the caps judge at it**: the big order is `over-max-notional`, the small one rests.
#[test]
fn a_mark_lifts_the_floor_and_the_ceilings_judge_at_it() {
    let mut e = twin(true);
    price(&mut e, MARK);
    assert_eq!(submit(&mut e, &market("big", 1, 0.1)).as_deref(), Some("over-max-notional"));
    assert_eq!(submit(&mut e, &market("small", 1, 0.01)), None, "743 of notional is under the cap");
    assert_eq!(e.client.submissions.len(), 1);
}

/// **An OPEN LIMIT, pinned and not endorsed: the floor asks whether a mark EVER arrived, not whether it is
/// current.** No shipped path sets a freshness window (`CoreConfig::price_cfg` is `PriceCfg::default()`),
/// so a mark ten days old, from a feed that died, lifts the floor and the ceilings judge at it. A window is a
/// behaviour change of its own; when one is set this test is the one to turn.
#[test]
fn an_old_mark_still_stands_because_no_freshness_window_is_set() {
    let mut e = twin(true);
    price(&mut e, MARK); // stamped at ts 1
    let mut ob = Outbox::default();
    e.submit_order(&market("old", 1, 0.1), 10 * 24 * 3_600_000, &mut ob);
    let denial = ob.0.into_iter().find_map(|ev| match ev {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    });
    assert_eq!(denial.as_deref(), Some("over-max-notional"), "judged at the old mark: no floor");
}

/// PINNED, not endorsed: an engine the floor is not armed on judges the same order at notional zero,
/// which is what every engine but the derived lane twin still does today.
#[test]
fn an_engine_without_the_floor_judges_an_unmarked_market_order_at_zero() {
    let mut e = twin(false);
    assert_eq!(submit(&mut e, &market("big", 1, 0.1)), None, "the cap is vacated at mark zero");
    assert_eq!(e.client.submissions.len(), 1);
}

/// A COVERED reduce is never held up by the floor (a position must stay closable with no price), and
/// it goes whether or not the caller set `reduce_only`. A `reduce_only` FLAG on a flat book covers
/// nothing, so it is an opening order to the floor.
#[test]
fn a_covered_reduce_passes_without_a_mark() {
    let held = || {
        let mut e = twin(true);
        e.on_event(&long_fill(0.05), &mut Outbox::default());
        e
    };
    let mut e = held();
    let flag = order("close", MOUNTED, -1, 0.05, OrderType::Market, None, true);
    assert_eq!(submit(&mut e, &flag), None, "the flagged close reaches the venue");
    // An opposite-direction close covered by the position goes without the flag too.
    let mut e = held();
    assert_eq!(submit(&mut e, &market("close2", -1, 0.05)), None);
    // ...while an OPENING order on the same unmarked engine is still denied, and so is the flag on a
    // book that holds nothing to reduce.
    let mut e = held();
    assert_eq!(
        submit(&mut e, &market("open", 1, 0.01)).as_deref(),
        Some("no-mark-for-mounted-symbol")
    );
    let mut flat = twin(true);
    let bare_flag = order("bare", MOUNTED, -1, 0.05, OrderType::Market, None, true);
    assert_eq!(submit(&mut flat, &bare_flag).as_deref(), Some("no-mark-for-mounted-symbol"));
}

/// The floor is about the MOUNTED symbol. A declared leg (an `extra_symbols` entry with no grid row)
/// keeps the judgement it always had, and an on-demand admitted symbol keeps `no-reference-price`.
#[test]
fn only_the_mounted_symbol_is_asked() {
    let mut e = twin(true);
    let leg = order("leg", "ETHUSDT", 1, 0.01, OrderType::Limit, Some(3_000.0), false);
    assert_eq!(submit(&mut e, &leg), None, "a declared leg is judged as before");

    let mut admitted = twin(true);
    assert_eq!(admitted.admit_symbol(&admission("SOLUSDT", None)), AdmitOutcome::Admitted);
    let sol = order("sol", "SOLUSDT", 1, 0.01, OrderType::Market, None, false);
    assert_eq!(submit(&mut admitted, &sol).as_deref(), Some("no-reference-price"));
}

/// **An EMPTY symbol is the mounted one** (`ExecutionEngine::symbol_denial`: the adapters read it so, and
/// sign the order on the mounted symbol). The floor must judge it as the mounted symbol: before the mark
/// it is denied by name, and AFTER the mark the ceilings judge it AT the mark, not at the notional zero
/// an unmatched `""` lookup yields (that is the very hole the floor closes, reopened by a blank).
#[test]
fn an_empty_symbol_is_the_mounted_symbol_to_the_floor() {
    let blank = |coid: &str, qty: f64| order(coid, "", 1, qty, OrderType::Market, None, false);
    let mut e = twin(true);
    assert_eq!(
        submit(&mut e, &blank("blank-0", 0.1)).as_deref(),
        Some("no-mark-for-mounted-symbol")
    );
    assert!(e.client.submissions.is_empty(), "nothing reached the venue");

    price(&mut e, MARK);
    assert_eq!(
        submit(&mut e, &blank("blank-big", 0.1)).as_deref(),
        Some("over-max-notional"),
        "judged at the mounted symbol's mark, not at notional zero"
    );
    assert_eq!(submit(&mut e, &blank("blank-small", 0.01)), None, "743 is under the cap");
    assert_eq!(e.client.submissions.len(), 1);
}

/// PINNED, not endorsed: the blank-symbol lookup is changed for an ARMED engine only. An engine without
/// the floor judges a blank-symbol market order exactly as it did (at notional zero), because every other
/// engine of the fleet would otherwise change its verdicts in a commit about the lane twin.
#[test]
fn an_engine_without_the_floor_judges_a_blank_symbol_order_as_before() {
    let mut e = twin(false);
    price(&mut e, MARK);
    let blank = order("blank", "", 1, 0.1, OrderType::Market, None, false);
    assert_eq!(
        submit(&mut e, &blank),
        None,
        "an unarmed engine's blank symbol is still priced at 0"
    );
}

/// The board's own dead-price guard keeps 0, negatives and NaN out, but lets `+inf` through. A
/// non-finite mark is no mark: the floor denies it by name rather than judging at infinity.
#[test]
fn a_non_finite_board_mark_is_no_mark() {
    let mut e = twin(true);
    e.price_board.set_mark(VENUE, MOUNTED, f64::INFINITY, 1);
    assert_eq!(
        submit(&mut e, &market("big", 1, 0.1)).as_deref(),
        Some("no-mark-for-mounted-symbol")
    );
}

/// **The mounted symbol's own admission seeds its first mark** (it answered `Mounted` and dropped the
/// price): the door's observed price is the only one a lane twin's engine can get from the quote route.
#[test]
fn the_mounted_symbols_admission_seeds_its_first_mark() {
    let mut e = twin(true);
    assert_eq!(e.admit_symbol(&admission(MOUNTED, Some(MARK))), AdmitOutcome::Mounted);
    assert_eq!(
        e.account.mark_of(VENUE, MOUNTED),
        Some(MARK),
        "the account slot a snapshot carries"
    );
    assert_eq!(submit(&mut e, &market("big", 1, 0.1)).as_deref(), Some("over-max-notional"));
    assert_eq!(submit(&mut e, &market("small", 1, 0.01)), None);
}

/// A mark the engine already holds is not overwritten by an older observation, and a price that is not
/// a finite positive number is ignored (the mounted symbol stays unmarked and the floor still bites).
#[test]
fn the_mounted_admission_never_overwrites_a_mark_and_ignores_a_bad_price() {
    let mut e = twin(true);
    price(&mut e, MARK);
    assert_eq!(e.admit_symbol(&admission(MOUNTED, Some(1.0))), AdmitOutcome::Mounted);
    assert_eq!(e.account.mark_of(VENUE, MOUNTED), Some(MARK), "the held mark stands");
    assert_eq!(submit(&mut e, &market("big", 1, 0.1)).as_deref(), Some("over-max-notional"));

    for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        let mut e = twin(true);
        assert_eq!(e.admit_symbol(&admission(MOUNTED, Some(bad))), AdmitOutcome::Mounted);
        assert_eq!(e.account.mark_of(VENUE, MOUNTED), None, "{bad} is no mark");
        assert_eq!(
            submit(&mut e, &market("big", 1, 0.1)).as_deref(),
            Some("no-mark-for-mounted-symbol"),
            "{bad}"
        );
    }
}

/// The floor is live-assembly policy and nothing a journal can see: an armed engine snapshots to the same
/// bytes as an unarmed one (so `state_hash` and every recorded journal are untouched), and an engine
/// built FROM a snapshot, which is what a replay or a refold runs on, never has it (a replay has no
/// market data, so it would deny every opening order the live run admitted).
#[test]
fn the_floor_is_invisible_to_the_snapshot_and_the_hash_and_a_replay_engine_never_has_it() {
    let (armed, plain) = (twin(true), twin(false));
    let snap = armed.snapshot_state();
    assert_eq!(snap, plain.snapshot_state());
    assert_eq!(state_hash(std::slice::from_ref(&snap)), state_hash(&[plain.snapshot_state()]));
    let replay = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert!(!replay.gate.require_mark);
}

/// **The account mark slot refuses what is not a price**: zero and negative as well as NaN and
/// infinity. A refused write leaves the mark that was there.
#[test]
fn the_account_mark_slot_refuses_zero_negative_and_non_finite_marks() {
    let mut e = twin(true);
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!e.account.set_mark_from(VENUE, MOUNTED, bad, MarkSource::VenueMark, 1), "{bad}");
    }
    assert_eq!(e.account.mark_of(VENUE, MOUNTED), None);
    assert!(e.account.set_mark_from(VENUE, MOUNTED, MARK, MarkSource::VenueMark, 1));
    assert!(!e.account.set_mark_from(VENUE, MOUNTED, 0.0, MarkSource::VenueMark, 2));
    assert_eq!(e.account.mark_of(VENUE, MOUNTED), Some(MARK), "a refused write leaves the mark");
}
