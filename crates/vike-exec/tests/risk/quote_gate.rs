//! The pre-trade gate on a symbol QUOTED in a currency other than the account's.
//!
//! * `quote-rate-stale` denies an OPENING order once the symbol's conversion rate has aged past
//!   `DEFAULT_QUOTE_RATE_TTL_MS` and passes a COVERED reduce (also under `Halted` and `Reducing`): a
//!   stale rate must never trap a position. The boundary is exact: age == TTL passes, TTL + 1 denies.
//! * `below-min-notional` compares in the right currency: the venue states its floor in the QUOTE
//!   currency, the notional is the account's, so the gate converts the VENUE's number
//!   (`min_notional * ctx.quote_rate`), never the engine's.
//! * the per-order cap is in the account's currency.
//! * a rate of exactly 1.0 (a symbol quoted in the account's currency) leaves every verdict as it was.
//!
//! Each lane is driven directly (a `RiskContext`) and through `ExecutionEngine::submit_order`, whose
//! `risk_ctx` builds `quote_rate` / `quote_stale` from the account's rate book and the order's clock.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, DEFAULT_QUOTE_RATE_TTL_MS, EventHandler, ExecutionEngine,
    Outbox, RiskContext, RiskGate, SymbolAdmission,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{
    ComboLeg, ContractValue, OrderRequest, OrderType, RiskLimits, SymbolGrid, TradingState,
};

use crate::support::EngineBuilder;

const VENUE: &str = "oanda";
const MOUNTED: &str = "EURUSD";
const USDJPY: &str = "USDJPY";
const EURGBP: &str = "EURGBP";
const T0: i64 = 1_000_000;
const TTL: i64 = DEFAULT_QUOTE_RATE_TTL_MS;
const R: f64 = 1.0 / 150.0;
const Q: f64 = 100_000.0;

// ---- direct: the lanes of `check_inner` -----------------------------------------------------------

/// A JPY-quoted symbol's context: the venue's unit times the rate, the rate itself, and its staleness.
fn jpy(rate: f64, stale: bool) -> RiskContext {
    RiskContext {
        mark_price: 150.0,
        contract_value: ContractValue::linear(1.0).in_account(rate).expect("a usable rate"),
        quote_rate: rate,
        quote_stale: stale,
        ..RiskContext::default()
    }
}

fn order(side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: VENUE.into(),
        symbol: USDJPY.into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(150.0),
        reduce_only,
        ..Default::default()
    }
}

#[test]
fn a_stale_rate_denies_an_opening_order_and_consumes_no_rate_slot() {
    let limits =
        RiskLimits { max_orders_per_window: Some(1), window_ms: 1_000, ..RiskLimits::new() };
    let mut g = RiskGate::new(limits);
    let v = g.check(&order(1, 1000.0, false), &jpy(R, true));
    assert!(!v.ok && v.reason == "quote-rate-stale", "{v:?}");
    // a denied order must not have taken the one slot of the window: the fresh order gets it...
    assert!(g.check(&order(1, 1000.0, false), &jpy(R, false)).ok, "a fresh rate passes");
    // ...and only then is the window full (so the slot was really there to take)
    let v = g.check(&order(1, 1000.0, false), &jpy(R, false));
    assert!(!v.ok && v.reason == "rate-limited", "{v:?}");
}

#[test]
fn a_covered_reduce_passes_a_stale_rate_in_every_trading_state() {
    for state in [TradingState::Active, TradingState::Reducing, TradingState::Halted] {
        for reduce_only in [true, false] {
            let ctx = RiskContext { position_size: Q, trading_state: state, ..jpy(R, true) };
            let v = RiskGate::new(RiskLimits::new()).check(&order(-1, 40_000.0, reduce_only), &ctx);
            assert!(v.ok, "{state:?} reduce_only={reduce_only}: {v:?}");
        }
    }
}

/// The lanes AHEAD of the staleness lane keep their own words: a halt and a reduce-only state refuse an
/// opening order before the rate is looked at; with those out of the way the staleness lane is what
/// refuses an opening order, and an exit that overshoots the position is not a covered reduce.
#[test]
fn a_stale_rate_is_the_reason_only_where_no_earlier_lane_refuses() {
    let stale = |state, position| RiskContext {
        position_size: position,
        trading_state: state,
        ..jpy(R, true)
    };
    let verdict =
        |ctx: RiskContext, req: OrderRequest| RiskGate::new(RiskLimits::new()).check(&req, &ctx);

    let v = verdict(stale(TradingState::Halted, 0.0), order(1, 1000.0, false));
    assert_eq!(v.reason, "halted");
    let v = verdict(stale(TradingState::Reducing, 0.0), order(1, 1000.0, false));
    assert_eq!(v.reason, "reduce-only");
    let v = verdict(stale(TradingState::Active, 0.0), order(1, 1000.0, false));
    assert_eq!(v.reason, "quote-rate-stale");

    // long 100,000, sell 150,000: 50,000 of it opens a short, so it is not covered
    let v = verdict(stale(TradingState::Active, Q), order(-1, 150_000.0, true));
    assert!(!v.ok && v.reason == "quote-rate-stale", "{v:?}");
    // the covered part alone passes
    assert!(verdict(stale(TradingState::Active, Q), order(-1, 100_000.0, true)).ok);
}

/// 7 USD against a floor of 1,000 JPY (1,000 / 150 = 6.67 USD) passes; 5 USD does not. Judged in the
/// wrong currency (the account's 7 against the venue's 1,000) the 7 USD order would be denied too, which
/// is the assertion that tells a converted floor from an unconverted one.
#[test]
fn below_min_notional_is_judged_in_the_account_currency() {
    let limits = RiskLimits { min_notional: Some(1_000.0), ..RiskLimits::new() };
    let gate = || RiskGate::new(limits.clone());

    // 5 units at 150 JPY = 750 JPY = 5.00 USD
    let v = gate().check(&order(1, 5.0, false), &jpy(R, false));
    assert!(!v.ok && v.reason == "below-min-notional", "5 USD < 6.67 USD: {v:?}");
    // 7 units at 150 JPY = 1,050 JPY = 7.00 USD
    let v = gate().check(&order(1, 7.0, false), &jpy(R, false));
    assert!(v.ok, "7 USD >= 6.67 USD: {v:?}");

    // the lever is the context's rate: the SAME 7 USD order, with the venue's floor left unconverted
    // (rate 1.0 beside the converted unit), is the old comparison 7 < 1,000
    let unconverted_floor = RiskContext { quote_rate: 1.0, ..jpy(R, false) };
    let v = gate().check(&order(1, 7.0, false), &unconverted_floor);
    assert!(!v.ok && v.reason == "below-min-notional", "{v:?}");

    // a covered reduce is exempt, however small
    let held = RiskContext { position_size: 3.0, ..jpy(R, false) };
    assert!(gate().check(&order(-1, 3.0, true), &held).ok, "3 USD of exit passes the floor");
}

#[test]
fn the_per_order_cap_is_in_the_account_currency() {
    let limits = RiskLimits { max_notional_per_order: Some(200_000.0), ..RiskLimits::new() };
    let gate = || RiskGate::new(limits.clone());

    let converted = jpy(R, false);
    assert!(gate().check(&order(1, Q, false), &converted).ok, "100,000 USD is under 200,000");
    let v = gate().check(&order(1, 300_000.0, false), &converted);
    assert!(!v.ok && v.reason == "over-max-notional", "300,000 USD is over it: {v:?}");

    // unconverted, the same 100,000 units are 15,000,000 "USD": the old false refusal
    let unconverted =
        RiskContext { contract_value: ContractValue::linear(1.0), quote_rate: 1.0, ..converted };
    let v = gate().check(&order(1, Q, false), &unconverted);
    assert!(!v.ok && v.reason == "over-max-notional", "{v:?}");
}

/// `r == 1.0` is the identity: a context built with `in_account(1.0)` and the default rate yields the
/// verdict the plain context does, reason AND normalized request, across the lanes that read money.
#[test]
fn a_rate_of_one_leaves_every_verdict_as_it_was() {
    let lanes = [
        RiskLimits::new(),
        RiskLimits { min_notional: Some(1_000.0), ..RiskLimits::new() },
        RiskLimits { max_notional_per_order: Some(5_000.0), ..RiskLimits::new() },
        RiskLimits { max_total_exposure: Some(20_000.0), ..RiskLimits::new() },
        RiskLimits {
            tick_size: Some(0.5),
            lot_size: Some(2.0),
            min_qty: Some(4.0),
            ..RiskLimits::new()
        },
        RiskLimits { im_requirement: Some(0.25), ..RiskLimits::new() },
    ];
    for value in
        [ContractValue::linear(1.0), ContractValue::linear(10.0), ContractValue::inverse(1.0)]
    {
        for qty in [1.0, 5.0, 7.3, 40.0, 400.0] {
            for side in [1, -1] {
                let req = order(side, qty, false);
                let plain = RiskContext {
                    mark_price: 150.0,
                    contract_value: value,
                    equity: 3_000.0,
                    position_size: 3.0,
                    ..RiskContext::default()
                };
                let par = RiskContext {
                    contract_value: value.in_account(1.0).expect("par"),
                    quote_rate: 1.0,
                    quote_stale: false,
                    ..plain
                };
                for limits in &lanes {
                    let a = RiskGate::new(limits.clone()).check(&req, &plain);
                    let b = RiskGate::new(limits.clone()).check(&req, &par);
                    let shape = |v: &vike_exec::RiskVerdict| {
                        (
                            v.ok,
                            v.reason.clone(),
                            v.request
                                .as_ref()
                                .map(|r| (r.qty.to_bits(), r.price.map(f64::to_bits))),
                        )
                    };
                    assert_eq!(shape(&a), shape(&b), "{value:?} qty {qty} side {side} {limits:?}");
                }
            }
        }
    }
}

/// `check_combo` builds each leg's context from the caller's `leg_ctx` and overrides only the account
/// level facts (trading state, clock), so a leg inherits its own staleness.
#[test]
fn a_combo_leg_inherits_the_leg_contexts_staleness() {
    let combo = |legs: Vec<(&str, i32)>| OrderRequest {
        client_order_id: "cb1".into(),
        venue: VENUE.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(-5.0),
        combo_legs: legs
            .into_iter()
            .map(|(s, ratio)| ComboLeg { symbol: s.into(), ratio })
            .collect(),
        ..Default::default()
    };
    let leg_ctx = |position_in_jpy: f64, jpy_stale: bool| {
        move |symbol: &str| {
            if symbol == USDJPY {
                RiskContext {
                    position_size: position_in_jpy,
                    quote_stale: jpy_stale,
                    ..jpy(R, jpy_stale)
                }
            } else {
                RiskContext { mark_price: 1.1, ..RiskContext::default() }
            }
        }
    };
    let ctx = RiskContext::default();
    let mut gate = RiskGate::new(RiskLimits::new());

    // buy the JPY leg (opening) and sell the account-quoted one: the stale leg refuses the whole combo
    let open = combo(vec![(USDJPY, 1), (MOUNTED, -1)]);
    let v = gate.check_combo(&open, &ctx, leg_ctx(0.0, true));
    assert!(!v.ok && v.reason == "leg USDJPY: quote-rate-stale", "{v:?}");
    // ...wherever the stale leg sits in the combo
    let open_last = combo(vec![(MOUNTED, 1), (USDJPY, -1)]);
    let v = gate.check_combo(&open_last, &ctx, leg_ctx(0.0, true));
    assert!(!v.ok && v.reason == "leg USDJPY: quote-rate-stale", "{v:?}");
    // fresh: it goes
    assert!(gate.check_combo(&open, &ctx, leg_ctx(0.0, false)).ok);

    // the JPY leg REDUCES a long it holds (ratio -1 on a buy combo = a sell): covered, so a stale rate
    // does not hold it up; the other leg is account-quoted and was never stale
    let exit = combo(vec![(USDJPY, -1), (MOUNTED, 1)]);
    let v = gate.check_combo(&exit, &ctx, leg_ctx(Q, true));
    assert!(v.ok, "a covered reduce leg passes a stale rate: {v:?}");
}

// ---- through the engine: the context is built from the rate book and the order's clock ------------

fn engine(limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), limits, ..Default::default() }
            .build();
    e.quote_asset = "USD".into();
    e.now_ms = T0;
    e
}

fn admission(
    symbol: &str,
    ccy: &str,
    rate: f64,
    as_of: i64,
    min_notional: Option<f64>,
) -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: symbol.into(),
        grid: SymbolGrid {
            tick_size: Some(0.00001),
            lot_size: Some(1.0),
            min_notional,
            ..SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some(ccy.into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

/// An engine holding USDJPY (rate observed at `T0`, floor `min_notional` JPY) and, when `long`, a
/// 100,000-unit long in it.
fn jpy_engine(
    limits: RiskLimits,
    min_notional: Option<f64>,
    long: bool,
) -> ExecutionEngine<RecordingClient> {
    let mut e = engine(limits);
    assert_eq!(
        e.admit_symbol(&admission(USDJPY, "JPY", R, T0, min_notional)),
        AdmitOutcome::Admitted
    );
    e.price_board.set_mark(VENUE, USDJPY, 150.0, T0);
    if long {
        let f = FillEvent {
            trade_id: "seed".into(),
            client_order_id: String::new(),
            venue: VENUE.into(),
            symbol: USDJPY.into(),
            side: 1,
            last_qty: Q,
            last_px: 150.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: T0,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        e.on_event(&Event::Fill(f), &mut Outbox::default());
    }
    e
}

/// The reason `req` was denied at `now_ms`, or `None` when it reached the venue client.
fn submit(
    e: &mut ExecutionEngine<RecordingClient>,
    req: &OrderRequest,
    now_ms: i64,
) -> Option<String> {
    let mut ob = Outbox::default();
    e.submit_order(req, now_ms, &mut ob);
    ob.0.into_iter().find_map(|ev| match ev {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

fn named(coid: &str, mut req: OrderRequest) -> OrderRequest {
    req.client_order_id = coid.into();
    req
}

/// The boundary of the window, on the account's own book: age == TTL is fresh, TTL + 1 is stale, a
/// symbol quoted in the account's currency never is.
#[test]
fn the_window_boundary_is_exact_on_the_account() {
    let mut a = Account::new(1.0, VENUE, None, BalanceMode::Delta);
    a.admit_quoted(USDJPY, "JPY", None, Some((R, T0)), "USD", T0).expect("taken");
    assert_eq!(a.quote_leg(USDJPY, T0), (R, false), "fresh");
    assert_eq!(a.quote_leg(USDJPY, T0 + TTL), (R, false), "age == TTL passes");
    assert_eq!(a.quote_leg(USDJPY, T0 + TTL + 1), (R, true), "TTL + 1 is stale");
    assert_eq!(
        a.quote_leg(MOUNTED, T0 + 100 * TTL),
        (1.0, false),
        "an account-quoted symbol never is"
    );
}

#[test]
fn through_the_engine_the_boundary_is_the_ttl_and_a_readmission_lifts_it() {
    let mut e = jpy_engine(RiskLimits::new(), None, false);
    let buy = |coid: &str| named(coid, order(1, 1000.0, false));
    assert_eq!(submit(&mut e, &buy("fresh"), T0), None, "fresh passes");
    assert_eq!(submit(&mut e, &buy("edge"), T0 + TTL), None, "age == TTL passes");
    assert_eq!(
        submit(&mut e, &buy("late"), T0 + TTL + 1).as_deref(),
        Some("quote-rate-stale"),
        "TTL + 1 denies"
    );

    // the node re-admits the symbol with a fresh instant: the same order now goes
    let now = T0 + TTL + 1;
    e.now_ms = now;
    assert_eq!(e.admit_symbol(&admission(USDJPY, "JPY", R, now, None)), AdmitOutcome::Refreshed);
    assert_eq!(submit(&mut e, &buy("lifted"), now), None);
}

/// A covered reduce is never held up by a stale rate, in any trading state; an opening order is, in the
/// states that let it get that far.
#[test]
fn through_the_engine_a_covered_reduce_passes_a_stale_rate_also_halted_and_reducing() {
    let late = T0 + TTL + 1;
    for (state, opening_reason) in [
        (TradingState::Active, "quote-rate-stale"),
        (TradingState::Reducing, "reduce-only"),
        (TradingState::Halted, "halted"),
    ] {
        let mut e = jpy_engine(RiskLimits::new(), None, true);
        e.trading_state = state;
        let opening = named("o1", order(1, 1000.0, false));
        assert_eq!(submit(&mut e, &opening, late).as_deref(), Some(opening_reason), "{state:?}");
        let reducing = named("r1", order(-1, 40_000.0, true));
        assert_eq!(submit(&mut e, &reducing, late), None, "{state:?}: the exit goes");
        // and not only because of the flag: an unflagged exit is covered too
        let unflagged = named("r2", order(-1, 40_000.0, false));
        assert_eq!(submit(&mut e, &unflagged, late), None, "{state:?}: the implicit exit goes");
    }
}

/// Staleness is per QUOTE CURRENCY: a stale JPY rate refuses JPY-quoted openings while a fresher GBP
/// rate and the account-quoted mount trade on.
#[test]
fn staleness_is_judged_per_quote_currency() {
    let mut e = jpy_engine(RiskLimits::new(), None, false);
    let gbp_at = T0 + 60_000;
    e.now_ms = gbp_at; // the admission is judged at the engine clock: a stamp from the future is refused
    assert_eq!(
        e.admit_symbol(&admission(EURGBP, "GBP", 1.35, gbp_at, None)),
        AdmitOutcome::Admitted
    );
    e.price_board.set_mark(VENUE, EURGBP, 0.85, T0);

    let on = |symbol: &str, coid: &str| OrderRequest {
        symbol: symbol.into(),
        price: Some(if symbol == USDJPY { 150.0 } else { 0.85 }),
        ..named(coid, order(1, 1000.0, false))
    };
    let now = T0 + TTL + 1; // JPY is one ms too old; GBP is a minute younger
    assert_eq!(submit(&mut e, &on(USDJPY, "j"), now).as_deref(), Some("quote-rate-stale"));
    assert_eq!(submit(&mut e, &on(EURGBP, "g"), now), None);
    assert_eq!(submit(&mut e, &on(MOUNTED, "m"), now), None, "the mount is quoted in USD");

    let later = gbp_at + TTL + 1;
    assert_eq!(submit(&mut e, &on(EURGBP, "g2"), later).as_deref(), Some("quote-rate-stale"));
}

#[test]
fn through_the_engine_min_notional_is_the_venues_floor_converted() {
    let mut e = jpy_engine(RiskLimits::new(), Some(1_000.0), true);
    let buy = |coid: &str, qty: f64| named(coid, order(1, qty, false));
    assert_eq!(submit(&mut e, &buy("five", 5.0), T0).as_deref(), Some("below-min-notional"));
    assert_eq!(
        submit(&mut e, &buy("six", 6.0), T0).as_deref(),
        Some("below-min-notional"),
        "6.00 < 6.67"
    );
    assert_eq!(submit(&mut e, &buy("seven", 7.0), T0), None, "7 USD clears a 1,000 JPY floor");
    // a covered reduce of 3 units (3 USD) is exempt from the floor
    let exit = named("exit", order(-1, 3.0, true));
    assert_eq!(submit(&mut e, &exit, T0), None);
}

#[test]
fn through_the_engine_the_per_order_cap_is_in_the_account_currency() {
    let limits = RiskLimits { max_notional_per_order: Some(200_000.0), ..RiskLimits::new() };
    let mut e = jpy_engine(limits, None, false);
    assert_eq!(
        submit(&mut e, &named("ok", order(1, Q, false)), T0),
        None,
        "100,000 USD <= 200,000"
    );
    assert_eq!(
        submit(&mut e, &named("big", order(1, 300_000.0, false)), T0).as_deref(),
        Some("over-max-notional")
    );
}

/// A symbol that names the account's own currency is judged exactly like one that names none: the same
/// verdicts, lane by lane (the rate is exactly 1.0 and never stale).
#[test]
fn an_admission_naming_the_accounts_currency_is_judged_as_an_ordinary_symbol() {
    let limits = RiskLimits {
        max_notional_per_order: Some(5_000.0),
        min_notional: Some(10.0),
        ..RiskLimits::new()
    };
    let build = |named_ccy: bool| {
        let mut e = engine(limits.clone());
        let a = SymbolAdmission {
            quote_ccy: named_ccy.then(|| "USD".to_string()),
            quote_rate: None,
            rate_as_of_ms: None,
            ..admission("GBPUSD", "USD", 1.0, T0, Some(10.0))
        };
        assert_eq!(e.admit_symbol(&a), AdmitOutcome::Admitted);
        e.price_board.set_mark(VENUE, "GBPUSD", 1.25, T0);
        e
    };
    let (mut named_usd, mut unnamed) = (build(true), build(false));
    for (coid, qty) in [("a", 1.0), ("b", 7.0), ("c", 100.0), ("d", 10_000.0)] {
        let req = OrderRequest {
            symbol: "GBPUSD".into(),
            price: Some(1.25),
            ..named(coid, order(1, qty, false))
        };
        for now in [T0, T0 + 50 * TTL] {
            assert_eq!(
                submit(&mut named_usd, &req, now),
                submit(&mut unnamed, &req, now),
                "qty {qty} at {now}"
            );
        }
    }
}
