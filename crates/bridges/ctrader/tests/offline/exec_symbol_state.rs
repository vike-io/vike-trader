//! The broker's own state for a symbol (`ProtoOASymbol.tradingMode`, `ProtoOALightSymbol.enabled`) is
//! read at the handshake and an order that would OPEN in a symbol it forbids is refused BY NAME, in
//! the bridge, with nothing sent: before this suite those two fields were decoded and ignored, so a
//! listed-but-disabled symbol was admitted and died as the broker's bare reject.
//!
//! No socket: the client is built over a plain command channel (`CtraderExec::from_shared`), as in
//! `exec_resting.rs`, so each test reads the `Command` the actor would have been given.
//!
//! A close is NEVER refused: a `reduce_only` exit of either kind, a plain opposite-side MARKET order up
//! to the open size, and every order on a book the venue has not answered for all reach the wire. The
//! decision reads the position book and the order, never the account type, so the HEDGED book (a long
//! AND a short held at once) and the NETTED one (one side) take the same path and both are pinned.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

use vike_ctrader::conn::{Command, ConnShared};
use vike_ctrader::exec::CtraderExec;
use vike_ctrader::positions::{CloseTracker, PositionBook, TrackedPosition};
use vike_ctrader::proto::{
    ProtoOaLightSymbol, ProtoOaNewOrderReq, ProtoOaSymbol, ProtoOaTradingMode,
};
use vike_ctrader::symbols::SymbolMap;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

/// What the broker says about EURUSD: `(light.enabled, full.tradingMode)`.
type Wire = (Option<bool>, Option<i32>);

const ENABLED: Wire = (Some(true), Some(ProtoOaTradingMode::Enabled as i32));
const HIDDEN: Wire = (Some(false), None);
const DISABLED_NO_PENDINGS: Wire =
    (Some(true), Some(ProtoOaTradingMode::DisabledWithoutPendingsExecution as i32));
const DISABLED_PENDINGS: Wire =
    (Some(true), Some(ProtoOaTradingMode::DisabledWithPendingsExecution as i32));
const CLOSE_ONLY: Wire = (Some(true), Some(ProtoOaTradingMode::CloseOnlyMode as i32));
/// The four states that forbid an opening order, each with the word the refusal must carry.
const RESTRICTED: [(Wire, &str); 4] = [
    (HIDDEN, "enabled=false"),
    (DISABLED_NO_PENDINGS, "DISABLED_WITHOUT_PENDINGS_EXECUTION"),
    (DISABLED_PENDINGS, "DISABLED_WITH_PENDINGS_EXECUTION"),
    (CLOSE_ONLY, "CLOSE_ONLY_MODE"),
];

/// EURUSD (id 1) and GBPUSD (id 2, always ordinary), a 0.01-lot grid, EURUSD in the given state.
fn symbols(eurusd: Wire) -> SymbolMap {
    let light = vec![
        ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".into()),
            enabled: eurusd.0,
            ..Default::default()
        },
        ProtoOaLightSymbol {
            symbol_id: 2,
            symbol_name: Some("GBPUSD".into()),
            enabled: Some(true),
            ..Default::default()
        },
    ];
    let grid = |symbol_id, trading_mode| ProtoOaSymbol {
        symbol_id,
        digits: 5,
        min_volume: Some(100_000),
        step_volume: Some(100_000),
        max_volume: Some(10_000_000_000),
        lot_size: Some(10_000_000),
        trading_mode,
        ..Default::default()
    };
    SymbolMap::from_symbols(&light, &[grid(1, eurusd.1), grid(2, None)])
}

const LONG: i64 = 700_001;
const SHORT: i64 = 700_002;

/// A client whose book holds the given EURUSD positions `(position_id, side, volume_centi)`. `fetched`
/// says whether the venue has ANSWERED for the book (`replace_all`) or it is silence (`upsert` only).
fn rig(
    eurusd: Wire,
    held: &[(i64, i32, i64)],
    fetched: bool,
) -> (CtraderExec, mpsc::Receiver<Command>, tokio::sync::mpsc::Receiver<Ingest>) {
    let (tx, commands) = mpsc::channel();
    let rows = held.iter().enumerate().map(|(n, &(position_id, side, volume))| {
        (position_id, TrackedPosition { symbol_id: 1, side, volume, open_ts: 1_000 + n as i64 })
    });
    let mut book = PositionBook::default();
    if fetched {
        book.replace_all(rows, 5_000);
    } else {
        for (id, p) in rows {
            book.upsert(id, p);
        }
    }
    let shared = ConnShared {
        tx,
        symbols: Arc::new(symbols(eurusd)),
        ctid: 99,
        orders: Arc::new(Mutex::new(HashMap::new())),
        positions: Arc::new(Mutex::new(book)),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    let (events, rx) = event_channel(64);
    // A sentinel path nobody creates: the client is wired and the halt stays unengaged.
    let halt = std::env::temp_dir().join(format!("vike-ctrader-symstate-{}", std::process::id()));
    (CtraderExec::from_shared(shared, events).with_halt_path(halt.join("HALT")), commands, rx)
}

#[derive(Default)]
struct Sent {
    new_orders: Vec<ProtoOaNewOrderReq>,
    closes: Vec<(i64, i64)>,
}

fn sent(commands: &mpsc::Receiver<Command>) -> Sent {
    let mut out = Sent::default();
    while let Ok(c) = commands.try_recv() {
        match c {
            Command::NewOrder(o) => out.new_orders.push(o),
            Command::ClosePosition { position_id, volume, .. } => {
                out.closes.push((position_id, volume));
            }
            _ => {}
        }
    }
    out
}

/// The terminal rejection reasons on the lane, and whether `OrderSubmitted` came first.
fn rejected(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderRejected(r)) = ing {
            out.push(r.reason.to_string());
        }
    }
    out
}

fn order(
    symbol: &str,
    side: i32,
    qty: f64,
    order_type: OrderType,
    reduce_only: bool,
) -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "ctrader".into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type,
        reduce_only,
        price: Some(1.1000),
        trigger_price: Some(1.0950),
        ..Default::default()
    }
}

/// Submit and report `(sent, rejection reasons)`.
fn run(
    eurusd: Wire,
    held: &[(i64, i32, i64)],
    fetched: bool,
    req: &OrderRequest,
) -> (Sent, Vec<String>) {
    let (mut exec, commands, mut rx) = rig(eurusd, held, fetched);
    exec.submit(req);
    (sent(&commands), rejected(&mut rx))
}

/// The point: in each restricted state a plain opening order is refused with ONE terminal rejection that
/// names the symbol, the broker's own word and that nothing was sent, and nothing reaches the wire. This
/// is the test that is red on a tree that ignores the two fields (the order goes out as a `NewOrder`).
#[test]
fn an_opening_order_in_a_restricted_symbol_is_refused_by_name_and_never_sent() {
    for (wire, word) in RESTRICTED {
        for (kind, req) in [
            ("market buy", order("EURUSD", 1, 1000.0, OrderType::Market, false)),
            ("market sell", order("EURUSD", -1, 1000.0, OrderType::Market, false)),
            ("limit buy", order("EURUSD", 1, 1000.0, OrderType::Limit, false)),
            ("stop sell", order("EURUSD", -1, 1000.0, OrderType::Stop, false)),
        ] {
            let (got, reasons) = run(wire, &[], true, &req);
            assert!(
                got.new_orders.is_empty() && got.closes.is_empty(),
                "{word}/{kind}: an opening order reached the wire"
            );
            let [reason] = &reasons[..] else {
                panic!("{word}/{kind}: exactly one terminal rejection wanted, got {reasons:?}")
            };
            assert!(
                reason.starts_with("ctrader: EURUSD is not tradable on this account: "),
                "{word}/{kind}: {reason}"
            );
            assert!(reason.contains(word), "{word}/{kind}: the broker's word is missing: {reason}");
            assert!(reason.contains("nothing was sent"), "{word}/{kind}: {reason}");
        }
    }
}

/// A close is never refused, in any restricted state, on either book shape: a `reduce_only` market exit,
/// a `reduce_only` resting stop and limit (a bracket's legs), and a PLAIN opposite-side market order up
/// to the open size (the netted account's usual flatten) all reach the wire and are not rejected.
#[test]
fn a_close_always_gets_through_a_restricted_symbol() {
    for (wire, word) in RESTRICTED {
        // NETTED: one long. HEDGED: a long and a short held at once.
        for (shape, held) in [
            ("netted", vec![(LONG, 1, 100_000)]),
            ("hedged", vec![(LONG, 1, 100_000), (SHORT, -1, 100_000)]),
        ] {
            let tag = format!("{word}/{shape}");
            // reduce_only market SELL: the close route, capped at the long.
            let (got, rej) =
                run(wire, &held, true, &order("EURUSD", -1, 1000.0, OrderType::Market, true));
            assert!(rej.is_empty(), "{tag}: a reduce_only exit was refused: {rej:?}");
            assert_eq!(got.closes, vec![(LONG, 100_000)], "{tag}: reduce_only market");
            // PLAIN market SELL up to the long: a close by position id, no flag needed.
            let (got, rej) =
                run(wire, &held, true, &order("EURUSD", -1, 1000.0, OrderType::Market, false));
            assert!(rej.is_empty(), "{tag}: a plain closing order was refused: {rej:?}");
            assert_eq!(got.closes, vec![(LONG, 100_000)], "{tag}: plain market close");
            // reduce_only resting exits are sent (the broker decides what a resting exit does).
            for (kind, req) in [
                ("stop", order("EURUSD", -1, 1000.0, OrderType::Stop, true)),
                ("limit", order("EURUSD", -1, 1000.0, OrderType::Limit, true)),
            ] {
                let (got, rej) = run(wire, &held, true, &req);
                assert!(rej.is_empty(), "{tag}/{kind}: a reduce_only exit was refused: {rej:?}");
                assert_eq!(got.new_orders.len(), 1, "{tag}/{kind}: the exit must be sent");
            }
        }
    }
}

/// The edge of "close": a plain order LARGER than the opposing position opens the excess (a flip) and is
/// refused; and in a hedged book only the OPPOSING side is closing room, so a BUY past the short is an
/// opening order while a BUY up to the short closes it.
#[test]
fn only_the_part_that_opens_is_refused() {
    for (wire, word) in RESTRICTED {
        let (got, rej) = run(
            wire,
            &[(LONG, 1, 100_000)],
            true,
            &order("EURUSD", -1, 5000.0, OrderType::Market, false),
        );
        assert!(
            got.new_orders.is_empty() && got.closes.is_empty(),
            "{word}: a flip reached the wire"
        );
        assert_eq!(rej.len(), 1, "{word}: a flip past the long is an opening order: {rej:?}");

        let hedged = [(LONG, 1, 100_000), (SHORT, -1, 100_000)];
        let (got, rej) =
            run(wire, &hedged, true, &order("EURUSD", 1, 1000.0, OrderType::Market, false));
        assert!(rej.is_empty(), "{word}: a BUY up to the short closes it: {rej:?}");
        assert_eq!(got.closes, vec![(SHORT, 100_000)], "{word}");
        let (_, rej) =
            run(wire, &hedged, true, &order("EURUSD", 1, 2000.0, OrderType::Market, false));
        assert_eq!(rej.len(), 1, "{word}: a BUY past the short opens: {rej:?}");
    }
}

/// A book the venue has not answered for is SILENCE, not proof that no position is held: an order on it
/// is NOT refused by the bridge (the broker decides), so a restart cannot trap a close behind a refusal.
#[test]
fn an_unfetched_book_refuses_nothing() {
    for (wire, word) in RESTRICTED {
        let (got, rej) =
            run(wire, &[], false, &order("EURUSD", 1, 1000.0, OrderType::Market, false));
        assert!(rej.is_empty(), "{word}: refused on a book that has not spoken: {rej:?}");
        assert_eq!(got.new_orders.len(), 1, "{word}: the order must reach the broker");
    }
}

/// No regression: an ENABLED symbol, an absent tradingMode, and another symbol while EURUSD is
/// restricted are all admitted exactly as before.
#[test]
fn an_ordinary_symbol_is_untouched() {
    let buy = |symbol| order(symbol, 1, 1000.0, OrderType::Market, false);
    for (label, wire, symbol) in [
        ("ENABLED", ENABLED, "EURUSD"),
        ("absent tradingMode", (Some(true), None), "EURUSD"),
        ("nothing stated", (None, None), "EURUSD"),
        ("another symbol, EURUSD closed-only", CLOSE_ONLY, "GBPUSD"),
    ] {
        let (got, rej) = run(wire, &[], true, &buy(symbol));
        assert!(rej.is_empty(), "{label}: refused: {rej:?}");
        assert_eq!(got.new_orders.len(), 1, "{label}: the order must be sent");
    }
}
