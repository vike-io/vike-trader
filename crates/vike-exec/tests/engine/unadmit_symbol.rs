//! `ExecutionEngine::unadmit_symbol` (decision 0205): an engine GIVES BACK an on-demand symbol, and the slot it
//! held (one of `MAX_EXTRA_SYMBOLS`) is free for the next one.
//!
//! The law these tests pin is "the engine is the one that never admitted it": a released symbol leaves no row,
//! no value, no rate and no mark behind (the `state_hash` of the engine equals that of a fresh one), and a
//! symbol the engine still has business in (a position, a live order, a recent one) is REFUSED with a stable
//! word and nothing written. Replay safety rides the same facts: every verdict below is a function of folded
//! state and the engine clock only.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, EventHandler, ExecutionEngine, MAX_EXTRA_SYMBOLS, Outbox, SymbolAdmission,
    UNADMIT_QUIET_MS, UnadmitOutcome, state_hash,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{ContractValue, ManagedOrder, OrderRequest, OrderStatus, RiskLimits, SymbolGrid};

use crate::quote_currency::{GBPJPY, T0, USDJPY, engine as fx_engine, quoted};
use crate::support::EngineBuilder;

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT";
const SECOND: &str = "ETHUSDT";

fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
    EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), limits, ..Default::default() }
        .build()
}

/// ETH on its own, finer, grid.
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
        release: false,
        margin_mode: None,
    }
}

/// An engine that has admitted ETH.
fn with_eth() -> ExecutionEngine<RecordingClient> {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&eth()), AdmitOutcome::Admitted);
    e
}

fn hash(e: &ExecutionEngine<RecordingClient>) -> u64 {
    state_hash(&[e.snapshot_state()])
}

fn fill(trade_id: &'static str, symbol: &str, side: i32, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: 3000.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 2,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn buy(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.5,
        order_type: vike_model::OrderType::Limit,
        price: Some(3000.0),
        ts: 1,
        ..Default::default()
    }
}

/// An order of this engine's own registering that has ENDED, created at `created_ms`.
fn ended(coid: &str, symbol: &str, created_ms: Option<i64>) -> ManagedOrder {
    let mut mo = ManagedOrder::new(buy(coid, symbol));
    mo.status = OrderStatus::Canceled;
    mo.created_ms = created_ms;
    mo
}

#[test]
fn a_symbol_given_back_leaves_the_engine_as_if_it_was_never_admitted() {
    let never = engine();
    let mut e = engine();
    // Everything an admission can write: a grid row, a margin row, a contract value and a first mark.
    let rich = SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        initial_mark: Some(3000.0),
        im_requirement: Some(0.25),
        ..eth()
    };
    assert_eq!(e.admit_symbol(&rich), AdmitOutcome::Admitted);
    assert_ne!(hash(&e), hash(&never), "the admission wrote something");
    assert_eq!(e.mark_of(SECOND), 3000.0);

    assert_eq!(e.unadmit_symbol(SECOND), UnadmitOutcome::Released);

    assert!(!e.accepts_symbol(SECOND));
    assert!(e.extra_symbols.is_empty());
    assert!(e.published_extra_symbols().as_slice().is_empty());
    assert!(e.gate.limits.grid_by_symbol.get(SECOND).is_none());
    assert!(e.gate.limits.im_by_symbol.get(SECOND).is_none());
    assert_eq!(e.mark_of(SECOND), 0.0, "an old stamp is not left to price a later admission");
    assert!(e.price_board.cell(VENUE, SECOND).is_none());
    assert_eq!(hash(&e), hash(&never), "the whole fenced state is a fresh engine's");
}

#[test]
fn a_released_symbol_is_dropped_by_the_fold_again() {
    let mut e = with_eth();
    assert_eq!(e.unadmit_symbol(SECOND), UnadmitOutcome::Released);
    // The fold drops an external fill on a symbol the engine does not accept (the admission opened it).
    e.on_event(&fill("t1", SECOND, 1, 1.0), &mut Outbox::default());
    assert!(e.account.positions.is_empty());
}

#[test]
fn a_slot_comes_back_for_the_next_symbol() {
    let mut e = engine();
    for i in 0..MAX_EXTRA_SYMBOLS {
        let a = SymbolAdmission { symbol: format!("SYM{i}USDT"), ..eth() };
        assert_eq!(e.admit_symbol(&a), AdmitOutcome::Admitted);
    }
    let over = SymbolAdmission { symbol: "OVERUSDT".into(), ..eth() };
    assert_eq!(e.admit_symbol(&over), AdmitOutcome::Refused("symbol-cap"));

    assert_eq!(e.unadmit_symbol("SYM3USDT"), UnadmitOutcome::Released);
    assert_eq!(e.extra_symbols.len(), MAX_EXTRA_SYMBOLS - 1);
    assert_eq!(
        e.admit_symbol(&over),
        AdmitOutcome::Admitted,
        "the freed slot takes the new symbol"
    );
    assert_eq!(e.extra_symbols.len(), MAX_EXTRA_SYMBOLS);
    assert!(!e.accepts_symbol("SYM3USDT") && e.accepts_symbol("OVERUSDT"));
    // The order of the others is kept: the list is what the read model publishes.
    assert_eq!(e.extra_symbols[3], "SYM4USDT");
}

#[test]
fn a_release_statement_is_not_an_admission() {
    let mut e = with_eth();
    let before = hash(&e);
    let outcome = e.admit_symbol(&SymbolAdmission::releasing(VENUE, SECOND));
    assert_eq!(outcome, AdmitOutcome::Refused("release-statement"));
    assert!(e.accepts_symbol(SECOND));
    assert_eq!(hash(&e), before);
}

/// One refusal case: what it is, how to build the engine and the symbol asked for, and the word expected.
type Case =
    (&'static str, Box<dyn Fn() -> (ExecutionEngine<RecordingClient>, String)>, &'static str);

#[test]
fn the_refusals_name_their_cause_and_write_nothing() {
    let cases: Vec<Case> = vec![
        ("empty", Box::new(|| (with_eth(), "  ".to_string())), "empty-symbol"),
        ("mounted", Box::new(|| (with_eth(), MOUNTED.to_string())), "mounted-symbol"),
        ("never admitted", Box::new(|| (with_eth(), "XRPUSDT".to_string())), "not-admitted"),
        (
            "a declared leg with a row",
            Box::new(|| {
                let limits = RiskLimits { tick_size: Some(0.1), ..RiskLimits::new() };
                let mut e = EngineBuilder {
                    venue: VENUE.into(),
                    symbol: MOUNTED.into(),
                    limits,
                    extra_symbols: vec!["SOLUSDT".into()],
                    ..Default::default()
                }
                .build();
                let row =
                    SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.1), ..Default::default() };
                e.gate.limits.grid_by_symbol.insert("SOLUSDT".into(), row);
                (e, "SOLUSDT".to_string())
            }),
            "declared-symbol",
        ),
        (
            "a leg with no row at all (a combo leg)",
            Box::new(|| {
                let mut e = engine();
                e.extra_symbols.push("SOLUSDT".into());
                (e, "SOLUSDT".to_string())
            }),
            "declared-symbol",
        ),
        (
            "a position",
            Box::new(|| {
                let mut e = with_eth();
                e.on_event(&fill("t1", SECOND, 1, 1.0), &mut Outbox::default());
                (e, SECOND.to_string())
            }),
            "holds-position",
        ),
        (
            "a short position",
            Box::new(|| {
                let mut e = with_eth();
                e.on_event(&fill("t1", SECOND, -1, 1.0), &mut Outbox::default());
                (e, SECOND.to_string())
            }),
            "holds-position",
        ),
        (
            "a live order",
            Box::new(|| {
                let mut e = with_eth();
                e.price_board.set_mark(VENUE, SECOND, 3000.0, 1);
                e.submit_order(&buy("c1", SECOND), 1, &mut Outbox::default());
                assert!(e.registry.contains_key("c1"), "the order was registered");
                (e, SECOND.to_string())
            }),
            "has-live-order",
        ),
        (
            "an order that ended a moment ago",
            Box::new(|| {
                let mut e = with_eth();
                e.now_ms = 1_000_000;
                e.registry.insert("c1".into(), ended("c1", SECOND, Some(1_000_000 - 1)));
                (e, SECOND.to_string())
            }),
            "recently-traded",
        ),
    ];
    for (what, build, word) in cases {
        let (mut e, symbol) = build();
        let before = hash(&e);
        let kept = e.extra_symbols.clone();
        assert_eq!(e.unadmit_symbol(&symbol), UnadmitOutcome::Refused(word), "{what}");
        assert_eq!(hash(&e), before, "{what}: a refusal writes nothing");
        assert_eq!(e.extra_symbols, kept, "{what}");
    }
}

#[test]
fn a_flat_symbol_with_ended_orders_can_be_given_back() {
    let mut e = with_eth();
    e.now_ms = 10 * UNADMIT_QUIET_MS;
    // A round trip that is over, an order of another symbol that is still live, and one adopted by
    // reconcile (no creation time): none of them holds the symbol.
    e.on_event(&fill("t1", SECOND, 1, 1.0), &mut Outbox::default());
    e.on_event(&fill("t2", SECOND, -1, 1.0), &mut Outbox::default());
    e.registry.insert("old".into(), ended("old", SECOND, Some(e.now_ms - UNADMIT_QUIET_MS)));
    e.registry.insert("adopted".into(), ended("adopted", SECOND, None));
    e.registry.insert("btc".into(), ManagedOrder::new(buy("btc", MOUNTED)));
    let flat = e.account.positions.get(&(VENUE.into(), SECOND.into(), PositionSide::Both));
    assert_eq!(flat.map(|p| p.size), Some(0.0), "a flat entry is what a round trip leaves");

    assert_eq!(e.unadmit_symbol(SECOND), UnadmitOutcome::Released);
    assert!(!e.accepts_symbol(SECOND));
}

#[test]
fn the_quiet_window_is_judged_on_the_engine_clock() {
    let mut e = with_eth();
    e.registry.insert("c1".into(), ended("c1", SECOND, Some(500)));
    e.now_ms = 500 + UNADMIT_QUIET_MS - 1;
    assert_eq!(e.unadmit_symbol(SECOND), UnadmitOutcome::Refused("recently-traded"));
    e.now_ms = 500 + UNADMIT_QUIET_MS;
    assert_eq!(e.unadmit_symbol(SECOND), UnadmitOutcome::Released);
}

#[test]
fn a_later_admission_gets_a_fresh_first_mark_not_the_old_stamp() {
    let mut e = engine();
    e.admit_symbol(&SymbolAdmission { initial_mark: Some(3000.0), ..eth() });
    assert_eq!(e.unadmit_symbol(SECOND), UnadmitOutcome::Released);
    // A seed lands only when the engine holds NO mark: it would not, had the old one been kept.
    assert_eq!(
        e.admit_symbol(&SymbolAdmission { initial_mark: Some(4000.0), ..eth() }),
        AdmitOutcome::Admitted
    );
    assert_eq!(e.mark_of(SECOND), 4000.0);
}

#[test]
fn a_symbol_quoted_in_another_currency_leaves_the_rate_book_with_it() {
    let never = fx_engine();
    let mut e = fx_engine();
    let rate = 1.0 / 150.0;
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", rate, T0)), AdmitOutcome::Admitted);
    assert_eq!(e.admit_symbol(&quoted(GBPJPY, "JPY", rate, T0)), AdmitOutcome::Admitted);
    assert!(e.account.is_foreign_quoted(USDJPY));

    assert_eq!(e.unadmit_symbol(USDJPY), UnadmitOutcome::Released);
    assert!(!e.account.is_foreign_quoted(USDJPY));
    assert_eq!(e.account.quote_ccy_of(GBPJPY), Some("JPY"), "the sibling keeps its currency");
    assert!(e.account.has_foreign_quote(), "and the standing rate it is valued with");
    assert_eq!(e.account.contract_value_of(USDJPY), e.account.contract_value_default());

    assert_eq!(e.unadmit_symbol(GBPJPY), UnadmitOutcome::Released);
    assert!(!e.account.has_foreign_quote(), "the last foreign symbol takes the book with it");
    assert_eq!(hash(&e), hash(&never), "the account that gave every one back never held one");
}

#[test]
fn a_snapshot_taken_after_a_release_restores_without_the_symbol() {
    let mut live = engine();
    live.admit_symbol(&SymbolAdmission {
        contract_value: Some(ContractValue::linear(10.0)),
        im_requirement: Some(0.5),
        ..eth()
    });
    live.admit_symbol(&SymbolAdmission { symbol: "SOLUSDT".into(), ..eth() });
    assert_eq!(live.unadmit_symbol(SECOND), UnadmitOutcome::Released);

    let snap = live.snapshot_state();
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    assert!(!booted.accepts_symbol(SECOND), "a released symbol is not resurrected by a restart");
    assert!(booted.accepts_symbol("SOLUSDT"), "the one still admitted is");
    assert!(booted.gate.limits.grid_by_symbol.get(SECOND).is_none());
    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(hash(&back), hash(&live));
}

#[test]
fn the_release_statement_has_a_pinned_wire_shape_and_an_admission_keeps_its_bytes() {
    let release =
        serde_json::to_string(&SymbolAdmission::releasing("binance#ALT", SECOND)).unwrap();
    assert_eq!(
        release,
        r#"{"route_key":"binance#ALT","symbol":"ETHUSDT","grid":{},"release":true}"#
    );
    let admission = serde_json::to_string(&eth()).unwrap();
    assert!(
        !admission.contains("release"),
        "an admission writes the bytes it always did: {admission}"
    );
    let back: SymbolAdmission = serde_json::from_str(&release).unwrap();
    assert!(back.release && back.symbol == SECOND);
    let old: SymbolAdmission = serde_json::from_str(&admission).unwrap();
    assert!(!old.release, "a record from before the key reads as an admission");
}
