//! Nothing that exists moved: the multi-currency change is IDENTITY for every engine that quotes in its
//! account's currency.
//!
//! 1. **Beside a foreign symbol.** The same events folded into two engines, one plain and one that
//!    additionally holds a foreign-quoted symbol it never trades: every number the account-quoted symbols
//!    produce (positions, realized PnL, closed PnLs, balance, fees, funding, equity) is BIT-identical,
//!    step by step, over a deterministic pseudo-random run that includes fees in a third currency, funding,
//!    a balance frame and forced closes.
//! 2. **Against `origin/main`.** A scenario with NO foreign symbol hashes exactly as `origin/main` hashes
//!    it: `state_hash` and the FNV-1a64 of the canonical `serde_json` bytes of `snapshot_state()` are
//!    pinned to the figures that tree printed. `state_hash` is compared across binary versions by the
//!    replay (`vike_core::replay`), so a byte that moved here is a recorded journal that no longer verifies.
//!
//! ⚠ Test 2 must COMPILE on `origin/main` (the pinned figures were measured there), so this file builds its
//! admissions from JSON, never from a struct literal, and names only APIs that tree has. Test 1's
//! precondition (`contract_value_of` of the foreign symbol is not the default) is the one assertion that
//! cannot hold there; it is not run on that tree.

use serde_json::json;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, EventHandler, ExecutionEngine, MarkSource, Outbox, SymbolAdmission, state_hash,
};
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, PositionLiquidated, PositionSide, TradeId,
};
use vike_model::{ContractValue, OrderRequest, RiskLimits};

use crate::support::EngineBuilder;

const VENUE: &str = "oanda";
const MOUNTED: &str = "EURUSD";
const OTHER: &str = "GBPUSD";
const FOREIGN: &str = "USDJPY";
const T0: i64 = 1_000_000;

fn admission(v: serde_json::Value) -> SymbolAdmission {
    serde_json::from_value(v).expect("a SymbolAdmission")
}

fn account_quoted(symbol: &str) -> SymbolAdmission {
    admission(json!({
        "route_key": VENUE,
        "symbol": symbol,
        "grid": {"tick_size": 0.00001, "lot_size": 1.0},
    }))
}

/// USDJPY on a USD account, with its rate. (On a tree without the currency fields the unknown keys are
/// ignored and this is an ordinary admission.)
fn foreign() -> SymbolAdmission {
    admission(json!({
        "route_key": VENUE,
        "symbol": FOREIGN,
        "grid": {"tick_size": 0.001, "lot_size": 1.0},
        "quote_ccy": "JPY",
        "quote_rate": 1.0 / 150.0,
        "rate_as_of_ms": T0,
    }))
}

fn usd_engine() -> ExecutionEngine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: MOUNTED.into(), ..Default::default() }.build();
    e.quote_asset = "USD".into();
    e.now_ms = T0;
    assert_eq!(e.admit_symbol(&account_quoted(OTHER)), AdmitOutcome::Admitted);
    e
}

/// xorshift-free LCG (Knuth's constants): the events are a function of the index alone, on every
/// platform and every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn fill(i: usize, r: u64) -> FillEvent {
    let symbol = if r.is_multiple_of(2) { MOUNTED } else { OTHER };
    let (commission, asset) = match (r >> 20) % 6 {
        0 => (0.0, ""),
        1 => (0.4, "USD"),
        2 => (-0.1, "USD"),
        3 => (0.002, "BNB"), // a third currency, on an account-quoted symbol: booked as stated
        4 => (0.0, "EUR"),
        _ => (0.7, ""),
    };
    FillEvent {
        trade_id: TradeId::prefixed("id-", i),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: if (r >> 1).is_multiple_of(2) { 1 } else { -1 },
        last_qty: ((r >> 2) % 40 + 1) as f64 * 500.0,
        last_px: 1.0 + ((r >> 8) % 4000) as f64 * 0.0001,
        commission,
        commission_asset: asset.into(),
        liquidity_side: "taker".into(),
        ts: T0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// The event stream of step `i`: a fill, and now and then funding, a balance frame or a forced close.
fn events(i: usize, r: u64) -> Vec<Event> {
    let f = fill(i, r);
    let (symbol, px) = (f.symbol, f.last_px);
    let mut out = vec![Event::Fill(f)];
    if i % 29 == 7 {
        out.push(Event::Funding(FundingEvent {
            venue: VENUE.into(),
            symbol,
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: if r.is_multiple_of(3) { 0.25 } else { -0.5 },
            mark_price: None,
            ts: T0,
            route_key: None,
        }));
    }
    if i % 61 == 13 {
        out.push(Event::AccountState(AccountState {
            venue: VENUE.into(),
            balances: vec![("USD".to_string(), 5_000.0 + i as f64)],
            ts: T0,
            route_key: None,
        }));
    }
    if i % 97 == 50 {
        out.push(Event::PositionLiquidated(PositionLiquidated {
            venue: VENUE.into(),
            symbol,
            position_side: PositionSide::Both,
            qty: 0.0,
            liq_price: px,
            fee: 0.3,
            ts: T0,
            trade_id: format!("liq-{i}").into(),
            route_key: None,
        }));
    }
    out
}

/// Every number the account-quoted book produces, as bits.
fn book(e: &ExecutionEngine<RecordingClient>) -> Vec<u64> {
    let a = &e.account;
    let mut out = vec![
        a.realized_pnl.to_bits(),
        a.balance.to_bits(),
        a.fees_paid.to_bits(),
        a.funding_paid.to_bits(),
        a.closed_pnls.len() as u64,
        a.positions.len() as u64,
    ];
    for ((_, symbol, _), p) in &a.positions {
        out.push(symbol.len() as u64);
        out.push(p.size.to_bits());
        out.push(p.avg_px.to_bits());
    }
    out.extend(a.closed_pnls.iter().map(|x| x.to_bits()));
    out.extend(a.fees_by_asset.values().map(|x| x.to_bits()));
    out.extend(a.balances_by_asset.values().map(|x| x.to_bits()));
    out
}

#[test]
fn the_account_quoted_book_is_bit_identical_beside_a_foreign_symbol() {
    let mut plain = usd_engine();
    let mut beside = usd_engine();
    assert_eq!(beside.admit_symbol(&foreign()), AdmitOutcome::Admitted);
    assert_ne!(
        beside.account.contract_value_of(FOREIGN),
        ContractValue::linear(1.0),
        "precondition: the second engine really holds a foreign-quoted symbol"
    );

    let mut rng = Lcg(0x5EED);
    for i in 0..400 {
        for ev in events(i, rng.next()) {
            let a = plain.on_event(&ev, &mut Outbox::default());
            let b = beside.on_event(&ev, &mut Outbox::default());
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "step {i}: the fold verdict");
        }
        assert_eq!(book(&plain), book(&beside), "step {i}: the book");
    }
    let closed = plain.account.closed_pnls.len();
    assert!(closed > 20, "the run must close positions to mean anything: {closed}");
    assert!(plain.account.positions.len() >= 2 && plain.account.fees_paid != 0.0);

    // equity at identical marks, to the bit
    for (symbol, px) in [(MOUNTED, 1.2345), (OTHER, 1.0987)] {
        plain.account.set_mark_from(VENUE, symbol, px, MarkSource::VenueMark, T0);
        beside.account.set_mark_from(VENUE, symbol, px, MarkSource::VenueMark, T0);
    }
    assert_eq!(
        plain.account.equity_all(1_000.0).to_bits(),
        beside.account.equity_all(1_000.0).to_bits()
    );
    assert_eq!(plain.account.balance_mode, beside.account.balance_mode);
    assert_eq!(
        plain.account.realized_pnl_at_balance_sync,
        beside.account.realized_pnl_at_balance_sync
    );
    assert!(beside.account.positions.keys().all(|(_, s, _)| s.as_str() != FOREIGN));
}

/// The figures `origin/main` printed for this scenario (see the module doc). A move here is a move of
/// the persisted format.
const MAIN_STATE_HASH: u64 = 9_785_489_307_075_776_400;
const MAIN_JSON_FNV: u64 = 14_103_277_529_241_628_817;

/// One admitted ordinary symbol with a contract value and a seeded mark, a resting order, fills with a
/// fee, funding and a balance frame: every part of the account that a snapshot writes.
fn scenario() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        im_requirement: Some(0.25),
        max_notional_per_order: Some(1_000_000.0),
        ..RiskLimits::new()
    };
    let mut e = EngineBuilder {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        limits,
        ..Default::default()
    }
    .build();
    e.now_ms = 1_000;
    e.equity_seed = 10_000.0;
    let a = admission(json!({
        "route_key": "binance",
        "symbol": "ETHUSDT",
        "grid": {"tick_size": 0.01, "lot_size": 0.0001},
        "contract_value": {"kind": "linear", "per_unit": 10.0},
        "initial_mark": 3000.0,
    }));
    assert_eq!(e.admit_symbol(&a), AdmitOutcome::Admitted);

    let order: OrderRequest = serde_json::from_value(json!({
        "client_order_id": "e1", "venue": "binance", "symbol": "ETHUSDT",
        "side": 1, "qty": 0.5, "order_type": "limit", "price": 3000.0
    }))
    .expect("an order");
    e.submit_order(&order, 1_000, &mut Outbox::default());
    assert!(e.registry.contains_key("e1"), "the order passed the gate and rests");

    let fill = |id: &'static str, side: i32, qty: f64, px: f64, fee: f64| {
        Event::Fill(FillEvent {
            trade_id: id.into(),
            client_order_id: "e1".into(),
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            side,
            last_qty: qty,
            last_px: px,
            commission: fee,
            commission_asset: "USDT".into(),
            liquidity_side: "taker".into(),
            ts: 1_000,
            mark_price: None,
            position_side: PositionSide::Both,
        })
    };
    let mut ob = Outbox::default();
    e.on_event(&fill("s1", 1, 0.3, 3000.0, 0.18), &mut ob);
    e.on_event(&fill("s2", -1, 0.1, 3010.0, 0.06), &mut ob);
    e.on_event(
        &Event::Funding(FundingEvent {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: -0.4,
            mark_price: Some(3005.0),
            ts: 1_000,
            route_key: None,
        }),
        &mut ob,
    );
    e.on_event(
        &Event::AccountState(AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 10_000.0), ("BNB".to_string(), 1.5)],
            ts: 1_000,
            route_key: None,
        }),
        &mut ob,
    );
    e
}

#[test]
fn a_scenario_without_a_foreign_symbol_hashes_as_origin_main_does() {
    let e = scenario();
    let snap = e.snapshot_state();
    let bytes = serde_json::to_vec(&snap).expect("serializes");
    let json = String::from_utf8(bytes.clone()).expect("utf-8");

    // what must be ABSENT for the bytes to be main's
    assert!(!json.contains(r#""fx""#) && !json.contains("account_ccy"), "{json}");
    assert!(snap.limits.im_by_symbol.is_empty(), "no admission stated a margin rate");

    // the figures main printed
    let hash = state_hash(std::slice::from_ref(&snap));
    let fnv = vike_util::fnv::fnv1a64(&bytes);
    println!("quote_identity: state_hash={hash} json_fnv1a64={fnv} json_len={}", bytes.len());
    assert_eq!(hash, MAIN_STATE_HASH, "state_hash drifted from origin/main (json_fnv1a64={fnv})");
    assert_eq!(fnv, MAIN_JSON_FNV, "canonical JSON drifted from origin/main (state_hash={hash})");

    // a snapshot restore is the same engine, byte for byte
    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(state_hash(&[back.snapshot_state()]), hash);
    assert_eq!(vike_util::fnv::fnv1a64(&serde_json::to_vec(&back.snapshot_state()).unwrap()), fnv);
}
