//! Scripted fake JForex bridge — TEST SUPPORT for `tests/dukascopy_exec.rs`.
//!
//! Speaks the real stdio protocol (`vike_dukascopy::proto`) so the exec client
//! can be integration-tested with no Java and no network. Modes (first CLI arg):
//!   (none)      handshake `ready`, then echo: submit → Accepted + Filled,
//!               cancel → Canceled, shutdown/EOF → exit 0
//!   fatal       emit `fatal`, exit 1 (login-failure path)
//!   silent      exit 1 with no output (silent-death handshake path)
//!   ready-die   emit `ready` then exit 1 (dead-child-after-handshake path)
//!   ghost       emit an `event` BEFORE `ready` (protocol violation the reader must
//!               drop), then behave like the default mode
//!   accept-die  `ready`; first submit → Accepted only, then exit 1 (in-flight order
//!               at reader EOF → synthetic "bridge died" rejection path)
//!   netting     position-per-order book emulation (the Java sidecar's netting truth):
//!               a submit first net-closes opposing orders in book order at the SUBMIT
//!               price (per-close fills under the submitting coid, mirroring
//!               NettingPlan/StrategyBridge), any remainder opens a fresh order; after
//!               every fill the venue-authoritative `position` line goes out (signed
//!               size in units + signed-weighted avg of the remaining orders' entries)
//!
//! It is a real bin target (so `CARGO_BIN_EXE_…` works from integration tests) but is
//! never shipped anywhere — it does nothing unless run by hand or by the tests.

use std::io::{BufRead, Write};

use vike_dukascopy::{Command, Envelope, encode_line, parse_command};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderPartiallyFilled,
};

fn emit(env: &Envelope) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", encode_line(env)).expect("stdout open");
    out.flush().expect("stdout flush");
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "fatal" => {
            emit(&Envelope::Fatal { reason: "login failed (scripted)".into() });
            std::process::exit(1);
        }
        "silent" => std::process::exit(1),
        "ghost" => {
            // Protocol violation: an event before the ready handshake — the Rust
            // reader must log + drop it, never pump it into ingest.
            emit(&Envelope::Event {
                event: Box::new(Event::OrderAccepted(OrderAccepted {
                    client_order_id: "ghost".into(),
                    venue_order_id: Some("GHOST-0".into()),
                    ts: 0,
                })),
            });
        }
        _ => {}
    }

    emit(&Envelope::Ready { account: "FAKE-DEMO".into(), balance: 100_000.0 });
    if mode == "ready-die" {
        std::process::exit(1);
    }
    if mode == "netting" {
        run_netting();
        return;
    }

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        match parse_command(&line) {
            Some(Command::Submit { order }) => {
                emit(&Envelope::Event {
                    event: Box::new(Event::OrderAccepted(OrderAccepted {
                        client_order_id: order.client_order_id.clone(),
                        venue_order_id: Some("FAKE-1".into()),
                        ts: order.ts,
                    })),
                });
                if mode == "accept-die" {
                    // Die with the order accepted but never terminal: the client's
                    // reader-EOF drain must synthesize its rejection.
                    std::process::exit(1);
                }
                // Dual-publish (mirrors the real sidecar's Proto.orderFilled): the bare
                // FillEvent (Account folds position/PnL) then the OrderFilled wrap (FSM).
                let fill = FillEvent {
                    trade_id: "FAKE-1:1".into(),
                    client_order_id: order.client_order_id.clone(),
                    venue: "dukascopy".into(),
                    symbol: order.symbol.clone().into(),
                    side: order.side,
                    last_qty: order.qty,
                    last_px: order.price.unwrap_or(1.1),
                    commission: 0.0,
                    commission_asset: String::new().into(),
                    liquidity_side: LiquiditySide::Taker,
                    ts: order.ts,
                    mark_price: None,
                    position_side: "BOTH".into(),
                };
                emit(&Envelope::Event { event: Box::new(Event::Fill(fill.clone())) });
                emit(&Envelope::Event {
                    event: Box::new(Event::OrderFilled(OrderFilled {
                        client_order_id: order.client_order_id.clone(),
                        fill,
                        ts: order.ts,
                    })),
                });
            }
            Some(Command::Cancel { client_order_id }) => {
                emit(&Envelope::Event {
                    event: Box::new(Event::OrderCanceled(OrderCanceled {
                        client_order_id,
                        reason: String::new().into(),
                        ts: 0,
                    })),
                });
            }
            Some(Command::Shutdown) => break,
            None => eprintln!("fake bridge: skipped unparseable line"),
        }
    }
}

/// One open position-per-order entry in the netting mode's book: (side, qty units, entry px).
type OpenOrder = (i32, f64, f64);

/// The Java sidecar's net-position emulation in miniature (see `NettingPlan.java` /
/// `StrategyBridge.onMessage` ORDER_CLOSE_OK): opposing FILLED orders are closed in book order,
/// each close realized at that order's OWN entry on the venue side, and the authoritative
/// `position` line follows every fill — the exact stream shape the exec client re-anchors from.
fn run_netting() {
    let mut book: Vec<OpenOrder> = Vec::new();
    let mut trade_seq = 0u64;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        match parse_command(&line) {
            Some(Command::Submit { order }) => {
                let coid = order.client_order_id.clone();
                let px = order.price.unwrap_or(1.1);
                emit(&Envelope::Event {
                    event: Box::new(Event::OrderAccepted(OrderAccepted {
                        client_order_id: coid.clone(),
                        venue_order_id: Some(format!("NET-{}", trade_seq + 1).into()),
                        ts: order.ts,
                    })),
                });
                // Plan pass (NettingPlan candidate order): which opposing entries close, how much.
                let mut remaining = order.qty;
                let mut closes: Vec<(usize, f64)> = Vec::new();
                for (i, entry) in book.iter().enumerate() {
                    if remaining <= 0.0 {
                        break;
                    }
                    if entry.0 == order.side {
                        continue; // not opposing
                    }
                    let amt = remaining.min(entry.1);
                    closes.push((i, amt));
                    remaining -= amt;
                }
                // Apply pass, one leg at a time: every non-final piece is an
                // OrderPartiallyFilled wrap, exactly one terminal OrderFilled per coid (C2),
                // and the `position` line after EACH fill reflects the book at THAT moment
                // (like the real sidecar computing from engine.getOrders per ORDER_CLOSE_OK).
                let pieces = closes.len() + usize::from(remaining > 0.0);
                for (n, (i, amt)) in closes.iter().enumerate() {
                    book[*i].1 -= amt;
                    trade_seq += 1;
                    emit_netting_fill(
                        n + 1 == pieces,
                        &coid,
                        &order.symbol,
                        order.side,
                        *amt,
                        px,
                        order.ts,
                        trade_seq,
                    );
                    emit_position(&book, &order.symbol, order.ts);
                }
                book.retain(|e| e.1 > 0.0);
                if remaining > 0.0 {
                    book.push((order.side, remaining, px));
                    trade_seq += 1;
                    emit_netting_fill(
                        true,
                        &coid,
                        &order.symbol,
                        order.side,
                        remaining,
                        px,
                        order.ts,
                        trade_seq,
                    );
                    emit_position(&book, &order.symbol, order.ts);
                }
            }
            Some(Command::Cancel { client_order_id }) => {
                emit(&Envelope::Event {
                    event: Box::new(Event::OrderCanceled(OrderCanceled {
                        client_order_id,
                        reason: String::new().into(),
                        ts: 0,
                    })),
                });
            }
            Some(Command::Shutdown) => break,
            None => eprintln!("fake bridge: skipped unparseable line"),
        }
    }
}

/// Dual-publish one fill (bare FillEvent then the FSM wrap), mirroring `Proto.orderFilled`.
#[allow(clippy::too_many_arguments)]
fn emit_netting_fill(
    full: bool,
    coid: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
    ts: i64,
    trade_seq: u64,
) {
    let fill = FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("NET:", trade_seq),
        client_order_id: coid.to_string(),
        venue: "dukascopy".into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: LiquiditySide::Taker,
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    };
    emit(&Envelope::Event { event: Box::new(Event::Fill(fill.clone())) });
    let event = if full {
        Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts })
    } else {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid.to_string(),
            fill,
            ts,
        })
    };
    emit(&Envelope::Event { event: Box::new(event) });
}

/// The venue-authoritative net position: signed unit sum + signed-weighted average entry over
/// the still-open entries (`NetPosition.java`'s law — economically the net basis even for a
/// mixed book). Fully-closed (zero-qty) entries are skipped, like CLOSED orders dropping out
/// of `engine.getOrders`.
fn emit_position(book: &[OpenOrder], symbol: &str, ts: i64) {
    let mut size = 0.0;
    let mut notional = 0.0;
    for (side, qty, px) in book.iter().filter(|e| e.1 > 0.0) {
        let signed = *side as f64 * qty;
        size += signed;
        notional += signed * px;
    }
    let avg_px = if size == 0.0 { 0.0 } else { notional / size };
    emit(&Envelope::Position { symbol: symbol.to_string(), size, avg_px, ts });
}
