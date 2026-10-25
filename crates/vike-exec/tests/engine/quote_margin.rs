//! `SymbolAdmission::im_requirement`: a symbol's OWN initial-margin rate (`1 / leverage`: 1.0 is no
//! leverage, 0.25 is 4x), written by the admission into `RiskLimits::im_by_symbol`, which
//! `RiskLimits::im_for` reads ahead of the engine-wide `im_requirement` in every reader (the gate's
//! buying power, `risk_ctx`'s margin fold, the resting-order margin, a combo).
//!
//! A 1x spot-like symbol and a 4x perp share one engine whose engine-wide rate is 0.25; a symbol that
//! states none follows the engine-wide rate exactly as it always did. The admission is the COMPLETE
//! statement of an admitted symbol's row: a repeat that states none REMOVES it, and a rate outside
//! `(0, 1]` is IGNORED (decision 0183 item 11: the same as stating none) and never refuses the
//! admission, which also carries the quote-currency rate refresh. The gate-level lanes without an
//! engine are `tests/risk/per_symbol_margin.rs`.

use serde_json::json;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, EngineSnapshot, EventHandler, ExecutionEngine, Fold, MAX_EXTRA_SYMBOLS, Outbox,
    PriceCfg, SymbolAdmission, state_hash,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{OrderRequest, OrderType, RiskLimits, SymbolGrid};

use crate::support::EngineBuilder;

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT";
const SPOT: &str = "ETHUSDT";
const PERP: &str = "SOLUSDT";
const PLAIN: &str = "XRPUSDT";
const NOW: i64 = 1_000;

/// A perp engine: engine-wide rate 0.25 (4x), `equity` of seed equity, a $0.01 tick and a 0.001 lot.
fn engine(equity: f64) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        im_requirement: Some(0.25),
        tick_size: Some(0.01),
        lot_size: Some(0.001),
        ..RiskLimits::new()
    };
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), limits, ..Default::default() }
            .build();
    e.equity_seed = equity;
    e.now_ms = NOW;
    e
}

fn admission(symbol: &str, im: Option<f64>) -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: im,
    }
}

fn admit(e: &mut ExecutionEngine<RecordingClient>, symbol: &str, im: Option<f64>) -> AdmitOutcome {
    let outcome = e.admit_symbol(&admission(symbol, im));
    e.price_board.set_mark(VENUE, symbol, 100.0, NOW);
    outcome
}

fn buy(coid: &str, symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }
}

/// The reason the gate denied `req`, or `None` when it reached the venue client.
fn denial(e: &mut ExecutionEngine<RecordingClient>, req: &OrderRequest) -> Option<String> {
    let mut ob = Outbox::default();
    e.submit_order(req, NOW, &mut ob);
    ob.0.into_iter().find_map(|ev| match ev {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

fn hold(e: &mut ExecutionEngine<RecordingClient>, id: &'static str, symbol: &str, qty: f64) {
    let f = FillEvent {
        trade_id: id.into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: NOW,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    assert!(matches!(e.on_event(&Event::Fill(f), &mut Outbox::default()), Fold::Applied));
}

type Margins = (Vec<String>, RiskLimits, EngineSnapshot);

fn margins(e: &ExecutionEngine<RecordingClient>) -> Margins {
    (e.extra_symbols.clone(), e.gate.limits.clone(), e.snapshot_state())
}

// (1) ---------------------------------------------------------------------------------------------

/// 600 of notional against 500 of equity: refused at 1x, passes at 4x, and a symbol that states no rate
/// follows the engine-wide one (4x here).
#[test]
fn each_symbol_is_margined_at_its_own_rate() {
    let verdict = |im: Option<f64>, qty: f64| {
        let mut e = engine(500.0);
        admit(&mut e, SPOT, im);
        denial(&mut e, &buy("c1", SPOT, qty))
    };
    assert_eq!(verdict(Some(1.0), 6.0).as_deref(), Some("insufficient-margin"));
    assert_eq!(verdict(Some(0.25), 6.0), None);
    assert_eq!(verdict(None, 6.0), None, "no rate stated: the engine-wide 4x");
    // the refusal at 1x is about margin: a smaller order at 1x goes, a bigger one at 4x does not
    assert_eq!(verdict(Some(1.0), 4.0), None);
    assert_eq!(verdict(Some(0.25), 21.0).as_deref(), Some("insufficient-margin"));

    // all three beside each other on ONE engine
    let mut e = engine(500.0);
    assert_eq!(admit(&mut e, SPOT, Some(1.0)), AdmitOutcome::Admitted);
    assert_eq!(admit(&mut e, PERP, Some(0.25)), AdmitOutcome::Admitted);
    assert_eq!(admit(&mut e, PLAIN, None), AdmitOutcome::Admitted);
    assert_eq!(e.gate.limits.im_for(SPOT), Some(1.0));
    assert_eq!(e.gate.limits.im_for(PERP), Some(0.25));
    assert_eq!(e.gate.limits.im_for(PLAIN), Some(0.25), "the engine-wide rate");
    assert_eq!(e.gate.limits.im_for(MOUNTED), Some(0.25));
    assert_eq!(denial(&mut e, &buy("a", SPOT, 6.0)).as_deref(), Some("insufficient-margin"));
    assert_eq!(denial(&mut e, &buy("b", PERP, 6.0)), None);
}

/// `ctx.margin_used` is each HELD position at its own rate and each RESTING order at its own rate: with
/// 1,000 of equity, 5 held at 1x uses 500 (125 at the engine-wide rate), and the verdicts that follow
/// can only come out as they do if that is what the context carried.
#[test]
fn the_margin_in_use_is_each_positions_and_each_resting_orders_own_rate() {
    let mut e = engine(1_000.0);
    admit(&mut e, SPOT, Some(1.0));
    admit(&mut e, PERP, Some(0.25));
    hold(&mut e, "h1", SPOT, 5.0);

    // the margin fold itself: 5 units at 100, at 1x
    let used = e.resolved_margin_in_use_by(&PriceCfg::default(), |(_v, s, _side), _p| {
        e.gate.limits.im_for(s)
    });
    assert_eq!(used, 500.0, "125 had it been margined at the engine-wide rate");

    // 2,400 of perp notional is 600 of margin; free is 1,000 - 500 = 500: refused. (At 125 in use it
    // would have gone: 875 of free buying power.)
    assert_eq!(denial(&mut e, &buy("p1", PERP, 24.0)).as_deref(), Some("insufficient-margin"));
    // 1,600 is 400: fits
    assert_eq!(denial(&mut e, &buy("p2", PERP, 16.0)), None);

    // a RESTING order commits at its own rate too (a fresh engine, to read it cleanly)
    let mut e = engine(1_000.0);
    admit(&mut e, SPOT, Some(1.0));
    admit(&mut e, PERP, Some(0.25));
    hold(&mut e, "h1", SPOT, 5.0);
    assert_eq!(denial(&mut e, &buy("s1", SPOT, 3.0)), None, "300 fits in the 500 left");
    // 500 held + 300 resting at 1x leaves 200: 1,200 of perp is 300 of margin (refused), 600 is 150 (goes)
    assert_eq!(denial(&mut e, &buy("p3", PERP, 12.0)).as_deref(), Some("insufficient-margin"));
    assert_eq!(denial(&mut e, &buy("p4", PERP, 6.0)), None);
}

// (2) ---------------------------------------------------------------------------------------------

#[test]
fn a_readmission_replaces_the_rate_and_one_that_states_none_removes_it() {
    let mut e = engine(1_000.0);
    assert_eq!(admit(&mut e, SPOT, Some(1.0)), AdmitOutcome::Admitted);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), Some(&1.0));
    assert_eq!(admit(&mut e, SPOT, Some(0.5)), AdmitOutcome::Refreshed);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), Some(&0.5), "replaced");
    assert_eq!(admit(&mut e, SPOT, None), AdmitOutcome::Refreshed);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), None, "None removes what an admission wrote");
    assert_eq!(e.gate.limits.im_for(SPOT), Some(0.25), "back on the engine-wide rate");
    assert!(e.gate.limits.im_by_symbol.is_empty());

    // the rate in force is the one the gate judges with: 600 of notional is 300 of margin at 0.5
    let mut tight = engine(250.0);
    admit(&mut tight, SPOT, Some(0.5));
    assert_eq!(denial(&mut tight, &buy("a", SPOT, 6.0)).as_deref(), Some("insufficient-margin"));
    admit(&mut tight, SPOT, Some(0.25));
    assert_eq!(denial(&mut tight, &buy("b", SPOT, 6.0)), None, "150 fits in 250");
}

/// The venue drops a symbol's leverage from 20x to 5x: the node's next admission states 0.2, and that
/// replaces the 0.05 row. Had the old row stayed, the symbol would be margined at a rate the venue no
/// longer grants (1,000 of notional needs 50 at 20x and 200 at 5x, against 150 of equity). And when
/// the venue states NO figure any more, the old, more permissive row must not keep margining it.
#[test]
fn a_leverage_the_venue_dropped_is_not_margined_at_the_old_row() {
    let mut e = engine(150.0);
    assert_eq!(admit(&mut e, SPOT, Some(0.05)), AdmitOutcome::Admitted);
    assert_eq!(denial(&mut e, &buy("a", SPOT, 10.0)), None, "1,000 of notional is 50 at 20x");
    assert_eq!(admit(&mut e, SPOT, Some(0.2)), AdmitOutcome::Refreshed);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), Some(&0.2), "replaced, not merged");
    assert_eq!(denial(&mut e, &buy("b", SPOT, 10.0)).as_deref(), Some("insufficient-margin"));
    assert_eq!(admit(&mut e, SPOT, None), AdmitOutcome::Refreshed);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), None);
}

/// What a `None` removes is a row an ADMISSION wrote: the first sight of a name has nothing of its own
/// to remove (a row already there is an operator's `Command::SetMargin`), a leg declared at assembly
/// keeps the row it was given, and the mounted symbol's row (which may be the operator's too) is not
/// touched by a `None`.
#[test]
fn a_none_removes_only_a_row_the_admission_itself_wrote() {
    // first sight of the name: an operator's row for it stands
    let mut e = engine(1_000.0);
    e.gate.limits.im_by_symbol.insert(SPOT.into(), 0.5);
    assert_eq!(admit(&mut e, SPOT, None), AdmitOutcome::Admitted);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), Some(&0.5), "not this admission's row");

    // a declared leg keeps its row through a re-admission that states none
    let mut e = engine(1_000.0);
    e.extra_symbols.push("LEGUSDT".into());
    e.gate.limits.grid_by_symbol.insert(
        "LEGUSDT".into(),
        SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
    );
    e.gate.limits.im_by_symbol.insert("LEGUSDT".into(), 0.5);
    assert_eq!(admit(&mut e, "LEGUSDT", None), AdmitOutcome::Refreshed);
    assert_eq!(e.gate.limits.im_by_symbol.get("LEGUSDT"), Some(&0.5), "declared, not admitted");

    // the mounted symbol: a Some writes, a None leaves
    let mut e = engine(1_000.0);
    assert_eq!(e.admit_symbol(&admission(MOUNTED, Some(1.0))), AdmitOutcome::Mounted);
    assert_eq!(e.admit_symbol(&admission(MOUNTED, None)), AdmitOutcome::Mounted);
    assert_eq!(e.gate.limits.im_by_symbol.get(MOUNTED), Some(&1.0));

    // the other admitted symbols' rows are not collateral
    let mut e = engine(1_000.0);
    admit(&mut e, SPOT, Some(1.0));
    admit(&mut e, PERP, Some(0.25));
    admit(&mut e, SPOT, None);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), None);
    assert_eq!(e.gate.limits.im_by_symbol.get(PERP), Some(&0.25));
}

// (3) ---------------------------------------------------------------------------------------------

/// A rate outside `(0, 1]` is IGNORED, exactly as if the admission had stated none (decision 0183 item
/// 11): the symbol is admitted, with its grid, and keeps the engine-wide rate. It does not refuse the
/// admission, because the same admission carries the quote-currency rate refresh.
#[test]
fn a_bad_rate_is_ignored_and_the_symbol_is_admitted_on_the_engine_wide_rate() {
    for bad in [0.0, -0.0, -0.1, 1.0001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        let mut e = engine(1_000.0);
        assert_eq!(e.admit_symbol(&admission(SPOT, Some(bad))), AdmitOutcome::Admitted, "{bad}");
        assert!(e.accepts_symbol(SPOT));
        assert!(e.gate.limits.grid_by_symbol.contains_key(SPOT), "the grid is applied");
        assert!(e.gate.limits.im_by_symbol.is_empty(), "no row is written for {bad}");
        assert_eq!(e.gate.limits.im_for(SPOT), Some(0.25), "the engine-wide rate");

        // ...and it is the very engine a `None` builds
        let mut none = engine(1_000.0);
        none.admit_symbol(&admission(SPOT, None));
        assert_eq!(margins(&e), margins(&none), "{bad} is stated as nothing");
    }
    // the edges of the interval itself are taken
    let mut edge = engine(1_000.0);
    assert_eq!(edge.admit_symbol(&admission(SPOT, Some(1.0))), AdmitOutcome::Admitted);
    assert_eq!(
        edge.admit_symbol(&admission(PERP, Some(f64::MIN_POSITIVE))),
        AdmitOutcome::Admitted
    );
    assert_eq!(edge.gate.limits.im_by_symbol.get(PERP), Some(&f64::MIN_POSITIVE));
}

/// On a symbol that is ALREADY admitted a bad rate is ignored the same way, which for a held symbol
/// means the row it had is removed (the admission states no usable figure) and the new grid IS
/// applied: the refresh goes through.
#[test]
fn a_bad_rate_on_a_held_symbol_removes_the_row_and_the_refresh_goes_through() {
    let mut e = engine(1_000.0);
    admit(&mut e, SPOT, Some(0.5));
    let coarser = SymbolAdmission {
        grid: SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
        ..admission(SPOT, Some(f64::NAN))
    };
    assert_eq!(e.admit_symbol(&coarser), AdmitOutcome::Refreshed);
    assert_eq!(e.gate.limits.im_by_symbol.get(SPOT), None);
    assert_eq!(e.gate.limits.grid_by_symbol[SPOT].tick_size, Some(0.5), "the new grid applied");
}

/// A bad margin rate must not hold back the CONVERSION RATE the same admission carries. The review's
/// case: a foreign-quoted position whose rate is refreshed by re-admission, with a margin figure that is
/// not a number, used to be refused whole and so left the rate to age out (and the opening orders on
/// it to be denied `quote-rate-stale`).
#[test]
fn a_bad_margin_rate_does_not_block_the_quote_rate_refresh() {
    let r1 = 1.0 / 150.0;
    let r2 = 1.0 / 151.0;
    let mut e = engine(1_000.0);
    assert_eq!(e.set_account_ccy("USD"), Ok(()));
    let foreign = |rate: f64, as_of: i64, im: Option<f64>| SymbolAdmission {
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of),
        ..admission(SPOT, im)
    };
    assert_eq!(e.admit_symbol(&foreign(r1, NOW, Some(f64::NAN))), AdmitOutcome::Admitted);
    assert_eq!(e.account.quote_rate_of(SPOT).to_bits(), r1.to_bits());
    assert_eq!(e.admit_symbol(&foreign(r2, NOW + 10, Some(2.0))), AdmitOutcome::Refreshed);
    assert_eq!(e.account.quote_rate_of(SPOT).to_bits(), r2.to_bits(), "the rate refreshed");
    assert_eq!(e.admit_symbol(&foreign(r1, NOW + 20, Some(-1.0))), AdmitOutcome::Refreshed);
    assert_eq!(e.account.quote_rate_of(SPOT).to_bits(), r1.to_bits());
    assert!(e.gate.limits.im_by_symbol.is_empty());
}

/// Where in the order of checks the rate is judged: the empty-symbol check comes first, and a refusal
/// that comes AFTER it (the symbol cap, the grid, a foreign currency with no rate) writes no margin row,
/// because the row is written last.
#[test]
fn the_rate_is_written_last() {
    let mut e = engine(1_000.0);
    // (told its currency, so a foreign admission reaches the rate checks below)
    assert_eq!(e.set_account_ccy("USD"), Ok(()));
    let blank = SymbolAdmission { symbol: "  ".into(), ..admission(SPOT, Some(f64::NAN)) };
    assert_eq!(e.admit_symbol(&blank), AdmitOutcome::Refused("empty-symbol"));

    let no_tick = SymbolAdmission {
        grid: SymbolGrid { tick_size: None, lot_size: Some(0.1), ..SymbolGrid::default() },
        ..admission(SPOT, Some(f64::NAN))
    };
    assert_eq!(e.admit_symbol(&no_tick), AdmitOutcome::Refused("grid-incomplete"));
    assert_eq!(
        e.admit_symbol(&admission(MOUNTED, Some(2.0))),
        AdmitOutcome::Mounted,
        "the mounted symbol's bad rate is ignored too: nothing is written for it"
    );
    assert!(e.gate.limits.im_by_symbol.is_empty());
    let foreign_no_rate =
        SymbolAdmission { quote_ccy: Some("JPY".into()), ..admission(SPOT, Some(f64::NAN)) };
    assert_eq!(e.admit_symbol(&foreign_no_rate), AdmitOutcome::Refused("no-quote-rate"));

    // a VALID rate on an admission another check refuses writes nothing
    let before = margins(&e);
    let no_tick = SymbolAdmission {
        grid: SymbolGrid { tick_size: None, lot_size: Some(0.1), ..SymbolGrid::default() },
        ..admission(SPOT, Some(0.5))
    };
    assert_eq!(e.admit_symbol(&no_tick), AdmitOutcome::Refused("grid-incomplete"));
    let foreign_no_rate =
        SymbolAdmission { quote_ccy: Some("JPY".into()), ..admission(SPOT, Some(0.5)) };
    assert_eq!(e.admit_symbol(&foreign_no_rate), AdmitOutcome::Refused("no-quote-rate"));
    assert!(margins(&e) == before);
    assert!(e.gate.limits.im_by_symbol.is_empty());

    let mut full = engine(1_000.0);
    for i in 0..MAX_EXTRA_SYMBOLS {
        assert_eq!(
            full.admit_symbol(&admission(&format!("COIN{i}USDT"), None)),
            AdmitOutcome::Admitted
        );
    }
    assert_eq!(
        full.admit_symbol(&admission("ONEMOREUSDT", Some(0.5))),
        AdmitOutcome::Refused("symbol-cap")
    );
    assert!(full.gate.limits.im_by_symbol.is_empty());
}

// (4) ---------------------------------------------------------------------------------------------

/// The MOUNTED symbol takes its own rate through an admission and stays `Mounted`: the one thing that is
/// written is its margin row, not a grid, a symbol, a contract value or a currency.
#[test]
fn the_mounted_symbol_takes_its_own_rate_and_stays_mounted() {
    let mut e = engine(500.0);
    let limits_before = e.gate.limits.clone();
    let symbols_before = e.extra_symbols.clone();
    let grid_before = (*e.account.contract_value_grid()).clone();

    assert_eq!(e.admit_symbol(&admission(MOUNTED, Some(1.0))), AdmitOutcome::Mounted);
    assert_eq!(e.gate.limits.im_by_symbol.get(MOUNTED), Some(&1.0));
    assert_eq!(e.gate.limits.im_for(MOUNTED), Some(1.0));
    assert_eq!(e.gate.limits.im_requirement, Some(0.25), "the engine-wide rate is untouched");
    assert_eq!(e.extra_symbols, symbols_before);
    assert!(e.gate.limits.grid_by_symbol.is_empty(), "no grid row for the mounted symbol");
    assert_eq!(*e.account.contract_value_grid(), grid_before);
    let mut written = e.gate.limits.clone();
    written.im_by_symbol.clear();
    assert_eq!(written, limits_before, "nothing else moved");

    // the gate now margins the mount at 1x: 600 against 500 is refused, as the same order on a 4x mount goes
    assert_eq!(denial(&mut e, &buy("a", MOUNTED, 6.0)).as_deref(), Some("insufficient-margin"));
    let mut other = engine(500.0);
    assert_eq!(denial(&mut other, &buy("a", MOUNTED, 6.0)), None);

    // a repeat replaces it; one that states none leaves it; a currency on the mount writes no fx
    assert_eq!(e.admit_symbol(&admission(MOUNTED, Some(0.5))), AdmitOutcome::Mounted);
    assert_eq!(e.gate.limits.im_by_symbol.get(MOUNTED), Some(&0.5));
    assert_eq!(e.admit_symbol(&admission(MOUNTED, None)), AdmitOutcome::Mounted);
    assert_eq!(e.gate.limits.im_by_symbol.get(MOUNTED), Some(&0.5));
    let with_ccy = SymbolAdmission {
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(1.0 / 150.0),
        rate_as_of_ms: Some(NOW),
        ..admission(MOUNTED, Some(0.5))
    };
    assert_eq!(e.admit_symbol(&with_ccy), AdmitOutcome::Mounted);
    assert!(!e.account.has_foreign_quote());
}

// (5) ---------------------------------------------------------------------------------------------

/// Two admitted symbols with their own rates, a third with none, and the mount with one: the snapshot
/// carries the rows, `from_snapshot` reproduces the engine, and the hash is stable across the trip.
#[test]
fn the_snapshot_carries_the_rows_and_from_snapshot_reproduces_them() {
    let mut live = engine(1_000.0);
    admit(&mut live, MOUNTED, Some(1.0));
    admit(&mut live, SPOT, Some(1.0));
    admit(&mut live, PERP, Some(0.25));
    admit(&mut live, PLAIN, None);
    hold(&mut live, "h1", SPOT, 5.0);

    let snap = live.snapshot_state();
    let rows: Vec<(&str, f64)> =
        snap.limits.im_by_symbol.iter().map(|(s, im)| (s.as_str(), *im)).collect();
    assert_eq!(rows, vec![(MOUNTED, 1.0), (SPOT, 1.0), (PERP, 0.25)], "PLAIN stated none");

    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(back.gate.limits.im_by_symbol, live.gate.limits.im_by_symbol);
    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(std::slice::from_ref(&snap)));

    // through the JSON the journal writes
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(
        json.contains(r#""im_by_symbol":{"BTCUSDT":1.0,"ETHUSDT":1.0,"SOLUSDT":0.25}"#),
        "{json}"
    );
    let parsed: EngineSnapshot = serde_json::from_str(&json).expect("parses");
    assert_eq!(parsed, snap);
}

/// A mount-built engine takes the rows an ADMISSION wrote back: the admitted symbols' and the mounted
/// symbol's (a 1x spot mount on a perp's leverage), and not a declared leg's, which is configuration the
/// mount states. A row the mount already holds is the mount's.
#[test]
fn restore_runtime_state_brings_back_the_rows_an_admission_wrote() {
    let mut live = engine(1_000.0);
    admit(&mut live, MOUNTED, Some(1.0));
    admit(&mut live, SPOT, Some(1.0));
    admit(&mut live, PERP, Some(0.25));
    admit(&mut live, PLAIN, None);
    // a DECLARED leg (configuration) with a row of its own
    live.extra_symbols.push("LEGUSDT".into());
    live.gate.limits.grid_by_symbol.insert(
        "LEGUSDT".into(),
        SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
    );
    live.gate.limits.im_by_symbol.insert("LEGUSDT".into(), 0.5);
    let snap = live.snapshot_state();

    let mut booted = engine(1_000.0);
    booted.restore_runtime_state(&snap);
    assert_eq!(booted.gate.limits.im_by_symbol.get(SPOT), Some(&1.0));
    assert_eq!(booted.gate.limits.im_by_symbol.get(PERP), Some(&0.25));
    assert_eq!(booted.gate.limits.im_by_symbol.get(PLAIN), None);
    assert_eq!(
        booted.gate.limits.im_by_symbol.get(MOUNTED),
        Some(&1.0),
        "the mounted symbol's row too: it has no on-demand grid row, so the loop alone missed it"
    );
    assert_eq!(
        booted.gate.limits.im_by_symbol.get("LEGUSDT"),
        None,
        "a declared leg's is the mount's"
    );
    assert!(!booted.accepts_symbol("LEGUSDT"));
    assert_eq!(booted.gate.limits.im_for(PLAIN), Some(0.25), "the engine-wide rate");

    // and it judges with them: the restart did not margin the 1x symbol at the perp's rate
    booted.price_board.set_mark(VENUE, SPOT, 100.0, NOW);
    booted.price_board.set_mark(VENUE, PERP, 100.0, NOW);
    booted.price_board.set_mark(VENUE, MOUNTED, 100.0, NOW);
    let mut tight = booted;
    tight.equity_seed = 500.0;
    assert_eq!(denial(&mut tight, &buy("a", SPOT, 6.0)).as_deref(), Some("insufficient-margin"));
    assert_eq!(denial(&mut tight, &buy("b", PERP, 6.0)), None);
    assert_eq!(
        denial(&mut tight, &buy("c", MOUNTED, 6.0)).as_deref(),
        Some("insufficient-margin"),
        "the 1x mount is not margined at the engine-wide 4x after a restart"
    );
}

/// What a restart must NOT bring back: a row the live engine had already REMOVED (the symbol's venue
/// stated no figure any more) is not in the snapshot, so it is not resurrected, and a row the mount
/// itself holds is not overwritten by the journal's.
#[test]
fn a_restart_neither_resurrects_a_removed_row_nor_overwrites_the_mounts() {
    let mut live = engine(1_000.0);
    admit(&mut live, SPOT, Some(1.0));
    admit(&mut live, PERP, Some(0.25));
    admit(&mut live, SPOT, None); // the venue states no figure for SPOT any more
    let snap = live.snapshot_state();
    assert_eq!(snap.limits.im_by_symbol.get(SPOT), None);

    let mut booted = engine(1_000.0);
    booted.gate.limits.im_by_symbol.insert(PERP.into(), 0.5); // the mount's own row for PERP
    booted.restore_runtime_state(&snap);
    assert_eq!(booted.gate.limits.im_by_symbol.get(SPOT), None, "removed stays removed");
    assert!(booted.accepts_symbol(SPOT), "...though the symbol itself is restored");
    assert_eq!(booted.gate.limits.im_by_symbol.get(PERP), Some(&0.5), "the mount's row wins");
}

// (6) ---------------------------------------------------------------------------------------------

/// Stating no rate anywhere leaves `im_by_symbol` empty and the snapshot as it was: an admission built
/// without the key (an older journal's) is the same engine as one that states `None`, and the old wire
/// reads back as `None`.
#[test]
fn stating_no_rate_anywhere_leaves_the_snapshot_as_before() {
    let mut stated_none = engine(1_000.0);
    admit(&mut stated_none, SPOT, None);
    hold(&mut stated_none, "h1", SPOT, 2.0);

    let old_wire: SymbolAdmission = serde_json::from_value(json!({
        "route_key": VENUE, "symbol": SPOT,
        "grid": {"tick_size": 0.01, "lot_size": 0.001},
    }))
    .expect("a pre-field admission");
    assert_eq!(old_wire.im_requirement, None);
    let mut keyless = engine(1_000.0);
    assert_eq!(keyless.admit_symbol(&old_wire), AdmitOutcome::Admitted);
    keyless.price_board.set_mark(VENUE, SPOT, 100.0, NOW);
    hold(&mut keyless, "h1", SPOT, 2.0);

    assert!(stated_none.gate.limits.im_by_symbol.is_empty());
    assert!(keyless.gate.limits.im_by_symbol.is_empty());
    let (a, b) = (stated_none.snapshot_state(), keyless.snapshot_state());
    assert_eq!(a, b);
    assert_eq!(state_hash(std::slice::from_ref(&a)), state_hash(&[b]));
    let json = serde_json::to_string(&a).expect("serializes");
    assert!(
        json.contains(r#""im_by_symbol":{}"#),
        "the empty map is what the fence always hashed: {json}"
    );

    // and the admission itself writes no key for None
    let wire = serde_json::to_string(&admission(SPOT, None)).expect("serializes");
    assert!(!wire.contains("im_requirement"), "{wire}");
    let wire = serde_json::to_string(&admission(SPOT, Some(0.25))).expect("serializes");
    assert!(wire.contains(r#""im_requirement":0.25"#), "{wire}");
}
