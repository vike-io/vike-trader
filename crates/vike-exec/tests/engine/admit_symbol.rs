//! `ExecutionEngine::admit_symbol` (decision 0173): an account's engine takes a second symbol ON
//! DEMAND, with that symbol's own facts, and everything downstream of the admission reads the
//! ORDER's symbol rather than the mounted one.
//!
//! Before it, the only ways a symbol joined an engine were `extra_symbols` written at assembly and
//! `lower_combo`'s submit-time push, and neither brought a grid: an order on the second symbol was
//! snapped on the MOUNTED symbol's tick and lot (proved on the CI box, 2026-10-17: an ETH price rounded to
//! BTC's 0.1 tick). These tests pin the admitted behaviour, and, just as hard, that an UNadmitted
//! symbol and the mounted one behave exactly as they did.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, EventHandler, ExecutionEngine, Fold, MAX_EXTRA_SYMBOLS, Outbox, SymbolAdmission,
    state_hash,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{ContractValue, OrderRequest, RiskLimits, SymbolGrid};

use crate::support::EngineBuilder;

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT";
const SECOND: &str = "ETHUSDT";

/// A BTC engine on the coarse grid its instrument has: tick 0.1, lot 0.001.
fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
    EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), limits, ..Default::default() }
        .build()
}

/// ETH's own, finer, grid.
fn eth() -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: SECOND.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    }
}

fn buy(coid: &str, symbol: &str, px: f64, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(px),
        ts: 1,
        ..Default::default()
    }
}

fn fill(coid: &str, trade_id: &'static str, symbol: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 2,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn size(e: &ExecutionEngine<RecordingClient>, symbol: &str) -> Option<f64> {
    e.account.positions.get(&(VENUE.into(), symbol.into(), PositionSide::Both)).map(|p| p.size)
}

/// A price for `symbol` on the engine's board, so the admitted symbol has a mark like a fed one.
fn priced(e: &mut ExecutionEngine<RecordingClient>, symbol: &str, px: f64) {
    e.price_board.set_mark(VENUE, symbol, px, 1);
}

fn submitted(e: &mut ExecutionEngine<RecordingClient>, req: &OrderRequest) -> OrderRequest {
    e.submit_order(req, 1, &mut Outbox::default());
    e.client.submissions.pop().expect("the order reached the client")
}

/// PINNED, not endorsed: with no admission, a foreign symbol keeps being judged on the mounted
/// grid. Every path that submits on a symbol it did not admit (declared strategy legs, combos,
/// deribit option tickets, backtests) depends on this staying byte-identical until the engine is
/// made to fail closed per venue (decision 0173 item 7).
#[test]
fn an_unadmitted_foreign_symbol_is_still_judged_on_the_mounted_grid() {
    let mut e = engine();
    let sent = submitted(&mut e, &buy("c1", SECOND, 3000.456, 0.12346));
    assert!((sent.price.unwrap() - 3000.5).abs() < 1e-9, "price {:?}", sent.price);
    assert!((sent.qty - 0.123).abs() < 1e-9, "qty {}", sent.qty);
}

#[test]
fn an_admitted_symbol_is_snapped_on_its_own_grid() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Admitted);
    priced(&mut e, SECOND, 3000.0);
    let sent = submitted(&mut e, &buy("c1", SECOND, 3000.456, 0.12346));
    assert!((sent.price.unwrap() - 3000.46).abs() < 1e-9, "price {:?}", sent.price);
    assert!((sent.qty - 0.1235).abs() < 1e-9, "qty {}", sent.qty);
    // The mounted symbol is untouched by it.
    let btc = submitted(&mut e, &buy("c2", MOUNTED, 60_000.06, 0.12346));
    assert!((btc.price.unwrap() - 60_000.1).abs() < 1e-9, "price {:?}", btc.price);
    assert!((btc.qty - 0.123).abs() < 1e-9, "qty {}", btc.qty);
}

#[test]
fn admission_opens_the_symbol_to_the_fold() {
    let mut e = engine();
    // An external fill (no coid of ours) on a symbol the engine does not trade is the account-wide
    // stream's noise and is dropped.
    let early = e.on_event(&fill("", "t1", SECOND, 1.0, 3000.0), &mut Outbox::default());
    assert!(matches!(early, Fold::Dropped), "{early:?}");
    assert_eq!(size(&e, SECOND), None);

    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Admitted);
    assert!(e.accepts_symbol(SECOND));
    e.on_event(&fill("", "t2", SECOND, 1.0, 3000.0), &mut Outbox::default());
    assert_eq!(size(&e, SECOND), Some(1.0), "an external fill on an admitted symbol folds");
}

#[test]
fn a_fill_of_an_order_on_the_admitted_symbol_folds_into_that_symbol_only() {
    let mut e = engine();
    e.admit_symbol(&eth());
    priced(&mut e, SECOND, 3000.0);
    submitted(&mut e, &buy("c1", SECOND, 3000.0, 0.5));
    e.on_event(&fill("c1", "t1", SECOND, 0.5, 3000.0), &mut Outbox::default());
    assert_eq!(size(&e, SECOND), Some(0.5));
    assert_eq!(size(&e, MOUNTED), None, "the mounted symbol's position never moved");
}

#[test]
fn a_fill_of_an_unknown_symbol_is_still_refused() {
    let mut e = engine();
    e.admit_symbol(&eth());
    let r = e.on_event(&fill("", "t1", "SOLUSDT", 1.0, 150.0), &mut Outbox::default());
    assert!(matches!(r, Fold::Dropped), "{r:?}");
    assert_eq!(size(&e, "SOLUSDT"), None);
    assert!(!e.accepts_symbol("SOLUSDT"));
}

/// Each refusal names its reason and changes NOTHING: not the symbol set, not the limits.
#[test]
fn a_half_resolved_or_unwelcome_admission_is_refused_and_changes_nothing() {
    let mut e = engine();
    let before_extra = e.extra_symbols.clone();
    let before_limits = e.gate.limits.clone();

    let no_tick = SymbolAdmission {
        grid: SymbolGrid { tick_size: None, lot_size: Some(0.1), ..SymbolGrid::default() },
        ..eth()
    };
    assert_eq!(e.admit_symbol(&no_tick), AdmitOutcome::Refused("grid-incomplete"));
    let no_lot = SymbolAdmission {
        grid: SymbolGrid { tick_size: Some(0.1), lot_size: None, ..SymbolGrid::default() },
        ..eth()
    };
    assert_eq!(e.admit_symbol(&no_lot), AdmitOutcome::Refused("grid-incomplete"));
    let blank = SymbolAdmission { symbol: "  ".into(), ..eth() };
    assert_eq!(e.admit_symbol(&blank), AdmitOutcome::Refused("empty-symbol"));

    assert_eq!(e.extra_symbols, before_extra);
    assert_eq!(e.gate.limits, before_limits);
}

#[test]
fn the_mounted_symbol_is_a_no_op() {
    let mut e = engine();
    let before = e.gate.limits.clone();
    let mounted = SymbolAdmission { symbol: MOUNTED.into(), ..eth() };
    assert_eq!(e.admit_symbol(&mounted), AdmitOutcome::Mounted);
    assert!(e.extra_symbols.is_empty());
    assert_eq!(
        e.gate.limits, before,
        "its scalars ARE its grid; a row would be a copy that drifts"
    );
}

/// Repeating the command (the node re-sends it after a restart) refreshes the row and grows
/// nothing.
#[test]
fn a_repeated_admission_refreshes_the_grid_and_adds_nothing() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Admitted);
    priced(&mut e, SECOND, 3000.0);
    let coarser = SymbolAdmission {
        grid: SymbolGrid { tick_size: Some(0.05), lot_size: Some(0.0001), ..SymbolGrid::default() },
        ..eth()
    };
    assert_eq!(e.admit_symbol(&coarser), AdmitOutcome::Refreshed);
    assert_eq!(e.extra_symbols, vec![SECOND.to_string()]);
    let sent = submitted(&mut e, &buy("c1", SECOND, 3000.026, 0.1));
    assert!((sent.price.unwrap() - 3000.05).abs() < 1e-9, "the fresh tick rules: {:?}", sent.price);
}

#[test]
fn the_extra_symbol_cap_refuses_the_next_symbol_but_not_a_refresh() {
    let mut e = engine();
    for i in 0..MAX_EXTRA_SYMBOLS {
        let a = SymbolAdmission { symbol: format!("COIN{i}USDT"), ..eth() };
        assert_eq!(e.admit_symbol(&a), AdmitOutcome::Admitted, "symbol {i}");
    }
    let one_more = SymbolAdmission { symbol: "ONEMOREUSDT".into(), ..eth() };
    assert_eq!(e.admit_symbol(&one_more), AdmitOutcome::Refused("symbol-cap"));
    assert!(!e.accepts_symbol("ONEMOREUSDT"));
    let refresh = SymbolAdmission { symbol: "COIN0USDT".into(), ..eth() };
    assert_eq!(
        e.admit_symbol(&refresh),
        AdmitOutcome::Refreshed,
        "a known symbol is not 'one more'"
    );
}

#[test]
fn a_contract_value_is_installed_only_when_it_is_not_the_default() {
    let mut e = engine();
    let ten = ContractValue::linear(10.0);
    e.admit_symbol(&SymbolAdmission { contract_value: Some(ten), ..eth() });
    assert_eq!(e.account.contract_value_of(SECOND), ten);
    assert_eq!(e.account.contract_value_of(MOUNTED), ContractValue::linear(1.0), "BTC unchanged");
    // Re-admitted with the default value, the row leaves the sparse grid again.
    e.admit_symbol(&SymbolAdmission { contract_value: Some(ContractValue::linear(1.0)), ..eth() });
    assert!(e.account.contract_value_grid().is_empty());
    // A degenerate factor folds to 1.0 like every other value entering the money path.
    e.admit_symbol(&SymbolAdmission { contract_value: Some(ContractValue::linear(-3.0)), ..eth() });
    assert!(e.account.contract_value_grid().is_empty());
}

/// A restart restores the positions AND the admitted symbols (their set, their `on_demand` grid
/// rows and their contract values): a position the engine does not know it trades would be judged
/// on the mounted grid until the node's re-admission landed. The node still re-sends the command
/// with fresh facts, which refreshes the row.
#[test]
fn a_restart_keeps_the_position_and_the_admission() {
    let mut live = engine();
    live.admit_symbol(&SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        ..eth()
    });
    priced(&mut live, SECOND, 3000.0);
    submitted(&mut live, &buy("c1", SECOND, 3000.0, 0.5));
    live.on_event(&fill("c1", "t1", SECOND, 0.5, 3000.0), &mut Outbox::default());
    let snap = live.snapshot_state();

    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    assert_eq!(size(&booted, SECOND), Some(0.5), "the position came back");
    assert!(booted.accepts_symbol(SECOND), "so did the symbol");
    assert_eq!(booted.gate.limits.grid_by_symbol, live.gate.limits.grid_by_symbol);
    assert_eq!(booted.account.contract_value_of(SECOND), ContractValue::linear(10.0));
    // The node's re-admission refreshes it; it does not grow the set.
    assert_eq!(booted.admit_symbol(&eth()), AdmitOutcome::Refreshed);
    assert_eq!(booted.extra_symbols, vec![SECOND.to_string()]);
}

/// Only an ON-DEMAND admission is restored: a declared leg is configuration the mount re-states,
/// and the cap and a declared row are never overwritten by it.
#[test]
fn a_restart_restores_only_on_demand_admissions() {
    let mut live = engine();
    live.extra_symbols.push("LEGUSDT".into());
    live.gate.limits.grid_by_symbol.insert(
        "LEGUSDT".into(),
        SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
    );
    live.admit_symbol(&eth());
    let snap = live.snapshot_state();

    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    assert!(booted.accepts_symbol(SECOND));
    assert!(!booted.accepts_symbol("LEGUSDT"), "a declared leg is the mount's to state");
    assert!(!booted.gate.limits.grid_by_symbol.contains_key("LEGUSDT"));
}

/// Replay determinism at the engine: an engine rebuilt from its own snapshot (the replay base) is
/// the same state, `state_hash` included, with an admitted symbol, its grid row and its contract
/// value in it; and two engines fed the same admission and orders hash alike.
#[test]
fn an_admitted_engine_survives_a_snapshot_round_trip_with_the_same_hash() {
    let build = || {
        let mut e = engine();
        e.admit_symbol(&SymbolAdmission {
            contract_value: Some(ContractValue::linear(10.0)),
            initial_mark: Some(3000.0),
            ..eth()
        });
        submitted(&mut e, &buy("c1", SECOND, 3000.0, 0.5));
        e.on_event(&fill("c1", "t1", SECOND, 0.5, 3000.0), &mut Outbox::default());
        e
    };
    let (a, b) = (build(), build());
    assert_eq!(state_hash(&[a.snapshot_state()]), state_hash(&[b.snapshot_state()]));

    let snap = a.snapshot_state();
    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(&[snap]));
    assert!(back.accepts_symbol(SECOND));
    assert_eq!(back.gate.limits, a.gate.limits);
}

fn failing_closed() -> ExecutionEngine<RecordingClient> {
    let mut e = engine();
    e.gate.limits.deny_unadmitted_symbols = true;
    e
}

fn sent_count(e: &ExecutionEngine<RecordingClient>) -> usize {
    e.client.submissions.len()
}

fn denied_reasons(outbox: &mut Outbox) -> Vec<String> {
    std::mem::take(&mut outbox.0)
        .into_iter()
        .filter_map(|ev| match ev {
            Event::OrderDenied(d) => Some(d.reason.to_string()),
            _ => None,
        })
        .collect()
}

/// **The engine fails closed** where the mount asks it to: an order on a symbol it was never told
/// about is DENIED, never rounded onto the mounted grid and signed. A refused or lost admission
/// therefore stops the order. The mounted symbol, a declared leg and an admitted symbol pass.
#[test]
fn a_fail_closed_engine_denies_a_symbol_it_was_never_told_about() {
    let mut e = failing_closed();
    let mut outbox = Outbox::default();
    e.submit_order(&buy("c1", SECOND, 3000.0, 0.5), 1, &mut outbox);
    assert_eq!(sent_count(&e), 0, "nothing reached the venue client");
    assert_eq!(denied_reasons(&mut outbox), vec!["symbol-not-admitted".to_string()]);

    // A refused admission (a half-resolved grid) leaves it unadmitted, so the order is still denied.
    let no_lot = SymbolAdmission {
        grid: SymbolGrid { tick_size: Some(0.1), lot_size: None, ..SymbolGrid::default() },
        ..eth()
    };
    assert!(matches!(e.admit_symbol(&no_lot), AdmitOutcome::Refused(_)));
    e.submit_order(&buy("c2", SECOND, 3000.0, 0.5), 1, &mut outbox);
    assert_eq!(sent_count(&e), 0);
    assert_eq!(denied_reasons(&mut outbox), vec!["symbol-not-admitted".to_string()]);

    // The mounted symbol is unchanged; a declared leg passes; an admitted symbol passes.
    e.submit_order(&buy("c3", MOUNTED, 60_000.0, 0.5), 1, &mut outbox);
    e.extra_symbols.push("LEGUSDT".into());
    e.submit_order(&buy("c4", "LEGUSDT", 10.0, 0.5), 1, &mut outbox);
    e.admit_symbol(&eth());
    priced(&mut e, SECOND, 3000.0);
    e.submit_order(&buy("c5", SECOND, 3000.0, 0.5), 1, &mut outbox);
    assert_eq!(sent_count(&e), 3);
    assert!(denied_reasons(&mut outbox).is_empty());
}

/// Without the flag nothing changed: the pinned behaviour of every path that never opted in.
#[test]
fn the_flag_off_engine_still_judges_a_foreign_symbol_on_the_mounted_grid() {
    let mut e = engine();
    assert!(!e.gate.limits.deny_unadmitted_symbols);
    let sent = submitted(&mut e, &buy("c1", SECOND, 3000.456, 0.12346));
    assert!((sent.price.unwrap() - 3000.5).abs() < 1e-9);
}

/// An admitted symbol's row is its WHOLE grid: a floor the venue publishes none of is unconstrained,
/// never the mounted symbol's number (which would deny a valid small order here).
#[test]
fn an_admitted_grid_without_a_floor_does_not_inherit_the_mounted_symbols() {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        min_qty: Some(1.0),
        min_notional: Some(100_000.0),
        ..RiskLimits::new()
    };
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), limits, ..Default::default() }
            .build();
    e.admit_symbol(&eth());
    priced(&mut e, SECOND, 3000.0);
    let sent = submitted(&mut e, &buy("c1", SECOND, 3000.0, 0.01));
    assert!((sent.qty - 0.01).abs() < 1e-9, "no inherited min_qty 1.0: {}", sent.qty);
    assert_eq!(e.client.submissions.len(), 0);
    let mut btc = buy("c2", MOUNTED, 60_000.0, 0.01);
    btc.ts = 1;
    let mut outbox = Outbox::default();
    e.submit_order(&btc, 1, &mut outbox);
    assert_eq!(
        denied_reasons(&mut outbox),
        vec!["below-min-qty".to_string()],
        "BTC keeps its floor"
    );
}

/// Re-admitting a symbol the mount DECLARED (a strategy leg) keeps its declared row and does not
/// mark it on-demand, so its market orders (it has a feed) are not denied; a refresh without a
/// contract value leaves the one an earlier admission installed.
#[test]
fn a_declared_leg_keeps_its_row_and_a_refresh_keeps_the_contract_value() {
    let mut e = engine();
    e.extra_symbols.push(SECOND.into());
    let declared =
        SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() };
    e.gate.limits.grid_by_symbol.insert(SECOND.into(), declared.clone());
    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Refreshed);
    assert_eq!(
        e.gate.limits.grid_by_symbol.get(SECOND),
        Some(&declared),
        "the declared row stands"
    );

    let mut f = engine();
    f.admit_symbol(&SymbolAdmission {
        contract_value: Some(ContractValue::inverse(10.0)),
        ..eth()
    });
    f.admit_symbol(&eth());
    assert_eq!(f.account.contract_value_of(SECOND), ContractValue::inverse(10.0));
}

// ---- the published list of extra symbols (`published_extra_symbols`) ----

/// The list a publish reads is shared while the engine's symbols stand and is EQUAL to the field
/// whatever happened to it: an admission, a refresh, and the direct assignments combo lowering and
/// builders make (a swap, a shorter and a longer list, a same-length list of other names, the empty
/// one). The cache behind it is compared on every read, never trusted, so each step below is a case
/// where trusting it would publish a stale list.
#[test]
fn the_published_symbols_are_shared_until_they_change_and_always_equal_the_field() {
    let mut e = engine();
    let empty = e.published_extra_symbols();
    assert!(empty.is_empty());
    assert!(
        !empty.is_shared_with(&e.published_extra_symbols()),
        "an empty list holds no allocation"
    );

    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Admitted);
    let one = e.published_extra_symbols();
    assert_eq!(one, e.extra_symbols);
    assert!(one.is_shared_with(&e.published_extra_symbols()), "unchanged symbols are shared");
    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Refreshed);
    assert!(one.is_shared_with(&e.published_extra_symbols()), "a refresh changes no symbol");

    let steps: [&[&str]; 8] = [
        &["A", "B"],
        &["B", "A"], // the same names in another order is another list
        &["B", "A", "C"],
        &["B"],
        &["D"], // the same length, another name
        &[],
        &["D"],
        &["D", "D"], // the field is a Vec: duplicates are the caller's, published as they are
    ];
    for step in steps {
        e.extra_symbols = step.iter().map(|s| (*s).to_string()).collect();
        let published = e.published_extra_symbols();
        assert_eq!(published, e.extra_symbols, "{step:?}");
        assert_eq!(published.as_slice(), e.extra_symbols.as_slice(), "{step:?}");
        let again = e.published_extra_symbols();
        assert_eq!(
            published.is_shared_with(&again),
            !step.is_empty(),
            "{step:?}: shared when held"
        );
    }
}

/// At the cap: the list a 16-symbol engine publishes is the 16 symbols, in admission order, and the
/// next admission past the cap is refused without touching it.
#[test]
fn the_published_symbols_at_the_cap_are_all_of_them_in_admission_order() {
    let mut e = engine();
    for i in 0..MAX_EXTRA_SYMBOLS {
        let admission = SymbolAdmission { symbol: format!("SYM{i}USDT"), ..eth() };
        assert_eq!(e.admit_symbol(&admission), AdmitOutcome::Admitted);
    }
    let full = e.published_extra_symbols();
    let want: Vec<String> = (0..MAX_EXTRA_SYMBOLS).map(|i| format!("SYM{i}USDT")).collect();
    assert_eq!(full, want);
    let over = SymbolAdmission { symbol: "OVERUSDT".into(), ..eth() };
    assert_eq!(e.admit_symbol(&over), AdmitOutcome::Refused("symbol-cap"));
    assert!(full.is_shared_with(&e.published_extra_symbols()), "a refusal changes nothing");
}

fn collared() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        price_collar: Some(vike_model::PriceCollar { pct: 0.10, abs_floor: 0.0 }),
        ..RiskLimits::new()
    };
    EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), limits, ..Default::default() }
        .build()
}

/// **The first order on a freshly admitted symbol is collared too.** The engine sees quotes only for
/// symbols it accepts, so right after `AdmitSymbol` it holds no mark; the gate used to take the
/// ORDER's own price as the mark, compare it with itself, and wave a limit at 100x the market
/// through. With the door's observed price seeded by the admission (`initial_mark`, journaled with
/// the command) the collar has something to judge against.
#[test]
fn a_seeded_mark_collars_the_first_order_on_a_new_symbol() {
    let mut e = collared();
    e.admit_symbol(&SymbolAdmission { initial_mark: Some(3000.0), ..eth() });
    let mut outbox = Outbox::default();
    e.submit_order(&buy("far", SECOND, 300_000.0, 0.5), 1, &mut outbox);
    assert_eq!(denied_reasons(&mut outbox), vec!["price-collar".to_string()]);
    assert!(e.client.submissions.is_empty(), "nothing reached the venue");
    e.submit_order(&buy("near", SECOND, 3010.0, 0.5), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "a price inside the band goes");
    assert!(denied_reasons(&mut outbox).is_empty());
}

/// With NO mark (no seed, no quote yet) an opening LIMIT order on an admitted symbol is denied too,
/// not judged against itself; a covered reduce is never stranded.
#[test]
fn an_admitted_symbol_with_no_mark_refuses_an_opening_limit_order() {
    let mut e = collared();
    e.admit_symbol(&eth());
    let mut outbox = Outbox::default();
    e.submit_order(&buy("blind", SECOND, 300_000.0, 0.5), 1, &mut outbox);
    assert_eq!(denied_reasons(&mut outbox), vec!["no-reference-price".to_string()]);
    assert!(e.client.submissions.is_empty());
    // The mounted symbol is judged exactly as before (its order price stands in for a missing mark).
    e.submit_order(&buy("btc", MOUNTED, 60_000.0, 0.5), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1);
}

/// A seed never overwrites a mark the engine already holds, and a junk one is ignored.
#[test]
fn a_seed_does_not_replace_a_mark_and_junk_is_ignored() {
    let mut e = collared();
    priced(&mut e, SECOND, 3000.0);
    e.admit_symbol(&SymbolAdmission { initial_mark: Some(9.0), ..eth() });
    let mut outbox = Outbox::default();
    e.submit_order(&buy("a", SECOND, 3010.0, 0.5), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "the held mark (3000), not the seed (9)");
    let mut f = collared();
    f.admit_symbol(&SymbolAdmission { initial_mark: Some(f64::NAN), ..eth() });
    f.submit_order(&buy("b", SECOND, 3010.0, 0.5), 1, &mut outbox);
    assert!(f.client.submissions.is_empty(), "NaN seeded nothing");
}

/// The seed rides the journaled command: serialised and read back it is the same admission.
#[test]
fn the_seed_survives_the_wire() {
    let admission = SymbolAdmission { initial_mark: Some(3000.5), ..eth() };
    let json = serde_json::to_string(&admission).unwrap();
    assert!(json.contains("\"initial_mark\":3000.5"), "{json}");
    assert_eq!(serde_json::from_str::<SymbolAdmission>(&json).unwrap(), admission);
    assert!(!serde_json::to_string(&eth()).unwrap().contains("initial_mark"), "absent when None");
}
