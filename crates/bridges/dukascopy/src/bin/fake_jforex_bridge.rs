//! Scripted fake JForex bridge — TEST SUPPORT for `tests/dukascopy_exec.rs`.
//!
//! Speaks the real stdio protocol (`vike_dukascopy::proto`) so the exec client
//! can be integration-tested with no Java and no network. Modes (the first APPLICATION argument —
//! see [`mode_of`], which skips the JVM options a real `java` command line puts in front):
//!   (none)      handshake `ready`, then echo: submit → Accepted + Filled,
//!               cancel → Canceled, shutdown/EOF → exit 0
//!   fatal       emit `fatal`, exit 1 (login-failure path)
//!   silent      exit 1 with no output (silent-death handshake path)
//!   ready-die   emit `ready` then exit 1 (dead-child-after-handshake path)
//!   hang-once <log>    the first launch (counted by appending its pid to the file `<log>`) sends nothing and
//!               never exits (a hung login); any later launch behaves like the default mode
//!   hang-always <log>  every launch hangs and logs its pid
//!   ghost       emit an `event` BEFORE `ready` (protocol violation the reader must
//!               drop), then behave like the default mode
//!   accept-die  `ready`; first submit → Accepted only, then exit 1 (in-flight order
//!               at reader EOF → synthetic "bridge died" rejection path)
//!   old-jar     as the default mode, but the `ready` line announces no features: a jar built before the amend
//!               command (the client must refuse a `modify` for it in words)
//!   modify-reject  as the default mode, but every `modify` command is answered `OrderModifyRejected`
//!               (the default mode answers it with `OrderModified` echoing the requested terms)
//!   ticks-stale / ticks-one-sided / ticks-ghost  as the default mode, but a `subscribe` of EURUSD is answered with a
//!               tick that is a minute old / has a zero bid / is for an instrument nobody subscribed (the price
//!               lane's judgement of a bad tick, `vike_dukascopy::quotes::accept`); the default mode answers every
//!               `subscribe` with one fresh, two-sided tick per symbol, and says nothing to an `unsubscribe`
//!   ticks-no-ccy / ccy-eur / ccy-bad  as the default mode, but the `ready` line states no account currency (a jar of the
//!               price lane's release, before the field) / states `EUR` / states a word that is no currency code; the
//!               default mode states `USD`, and `old-jar` none
//!   balance-zero  as the default mode, but the `ready` line states a balance of 0.0 (the default states
//!               `READY_BALANCE`; a figure the engine must not adopt, `crate::ready_balance`)
//!   ticks-no-acks  as the default mode, but the `ready` line does not announce `tick-acks` and a `subscribe` is
//!               answered with the tick alone: the jar of the release before the answers existed (the client must
//!               not wait for an answer it will never get)
//!   ticks-nack  as the default mode, but a `subscribe` of GBPUSD is DECLINED (a `subscription` line with `ok` false and
//!               a reason) and quoted not at all; the default mode answers every symbol `ok` before its tick
//!   netting     position-per-order book emulation (the Java sidecar's netting truth):
//!               a submit first net-closes opposing orders in book order at the SUBMIT
//!               price (per-close fills under the submitting coid, mirroring
//!               NettingPlan/StrategyBridge), any remainder opens a fresh order; after
//!               every fill the venue-authoritative `position` line goes out (signed
//!               size in units + signed-weighted avg of the remaining orders' entries)
//!
//! It is a real bin target (so `CARGO_BIN_EXE_…` works from integration tests) but is
//! never shipped anywhere — it does nothing unless run by hand or by the tests.
//!
//! # ⚠ It also stands in for the JVM's ARGUMENT PARSING, not just the sidecar's protocol
//!
//! `DukascopyExecutionClient::spawn` starts this program where a real run starts `java`, so the
//! argv it receives is a JVM command line: `[-Duser.home=<dir>] -Xmx512m -jar <path>`. Any option
//! of the JVM section (every leading `-` word, [`jvm_section_len`]) is skipped, so a new JVM flag
//! needs no change here; the heap flag is not interpreted, only tolerated. Two behaviours below
//! model the JVM rather than the sidecar, and both exist so a test can observe something the
//! protocol cannot carry:
//!
//! * [`jvm_option`] splits argv the way `java` does — options up to `-jar`, then the jar, then the
//!   application's own arguments — so the MODE is read from the application section. A flag placed
//!   after `-jar` is invisible to it, which is exactly what a real JVM would do with it;
//! * when `-Duser.home=<dir>` is present, [`report_user_home`] creates `<dir>/JForex` and writes
//!   the received value into [`USER_HOME_REPORT`] inside it — the one observable act of the SDK's
//!   `com/dukascopy/login/utils/NetworkUtil`, whose constant pool holds `user.home`,
//!   `defaultJForexFolderPath` and `"Cannot create the JForex directory."`.
//!
//! ⚠ **This is a MODEL of the JVM, not the JVM.** What a test built on it can prove is that the
//! flag reaches a real child process as one intact argv element, positioned where a JVM reads its
//! own options. That a real JVM then honours `-Duser.home`, and that the SDK builds its folder from
//! it, rests on the bytecode measurement above and on a run on an SDK-staged box — never on this
//! file. Say so in the test rather than letting its name overclaim.

use std::io::{BufRead, Write};
use std::path::Path;

use vike_dukascopy::{Command, Envelope, encode_line, parse_command};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModified,
    OrderModifyRejected, OrderPartiallyFilled,
};

/// The `balance` of the `ready` line: the figure the owner's demo account 8 stated on 2026-10-20
/// (`bridge ready ... balance=10000.0`), which `tests/dukascopy_ready_balance.rs` reads back.
const READY_BALANCE: f64 = 10_000.0;

fn emit(env: &Envelope) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", encode_line(env)).expect("stdout open");
    out.flush().expect("stdout flush");
}

/// The file this program writes the `user.home` it was given into, inside the `JForex` folder it
/// creates beneath that home — `<user.home>/JForex/user-home.txt`.
///
/// A file rather than merely the directory's existence, so a test can compare the exact STRING that
/// survived the trip through the OS argv rather than only "something was created near here": a
/// truncated or re-quoted path would still make a directory, just not the right one.
const USER_HOME_REPORT: &str = "user-home.txt";

/// The value of a JVM option this process was given (`-Dkey=value` → `Some(value)`), or `None` when
/// the JVM section of argv does not carry it.
///
/// ⚠ **Only the section BEFORE `-jar` is searched**, because that is the only section a JVM reads
/// its own options from. A `-Duser.home=` sitting after `-jar <path>` is an argument to
/// `Bridge.main`, and this function must not find it — otherwise a test built on it would pass for
/// a command line the real JVM ignores, which is the exact failure
/// `crates/bridges/dukascopy/src/exec.rs`'s `jvm_args` orders its arguments to avoid.
fn jvm_option(argv: &[String], prefix: &str) -> Option<String> {
    argv[..jvm_section_len(argv)].iter().find_map(|a| a.strip_prefix(prefix).map(str::to_string))
}

/// How many leading argv entries a JVM would treat as its OWN options: everything up to `-jar`, or
/// up to the first bare (non-`-`) word, which is the main class in the other invocation form.
fn jvm_section_len(argv: &[String]) -> usize {
    argv.iter().position(|a| a == "-jar" || !a.starts_with('-')).unwrap_or(argv.len())
}

/// The application's own first argument — this program's MODE — skipping the JVM section and the
/// `-jar <path>` pair, exactly as `Bridge.main`'s `args[0]` would.
///
/// With no JVM options at all (`spawn_with_program(BRIDGE, &["netting".into()], …)`, how every
/// pre-existing test in `tests/dukascopy_exec.rs` drives this file) the JVM section is empty and
/// this is `argv[0]` — byte-identical to the `args().nth(1)` it replaced.
fn mode_of(argv: &[String]) -> String {
    argv.get(app_start(argv)).cloned().unwrap_or_default()
}

/// Index of the application's first argument (the mode).
fn app_start(argv: &[String]) -> usize {
    let mut i = jvm_section_len(argv);
    if argv.get(i).is_some_and(|a| a == "-jar") {
        i += 2; // `-jar` and the jar path itself
    }
    i
}

/// The hang modes' LAUNCH LOG: appends this process's pid as one line to the file named by the
/// argument after the mode and returns how many launches the file now records. A test counts the
/// launches (and checks each pid is gone) without the sidecar protocol, which a hung child never
/// speaks.
fn log_launch(argv: &[String]) -> usize {
    let path = argv.get(app_start(argv) + 1).expect("hang modes take a launch-log path");
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(path).expect("log");
    writeln!(log, "{}", std::process::id()).expect("log write");
    std::fs::read_to_string(path).expect("log read").lines().count()
}

/// Do what the JForex SDK does with the home it is handed: make `<home>/JForex` and record the
/// value. Failures are FATAL here — a stub that silently skipped this would make its test green for
/// the wrong reason.
fn report_user_home(home: &str) {
    let folder = Path::new(home).join("JForex");
    std::fs::create_dir_all(&folder).expect("Cannot create the JForex directory.");
    std::fs::write(folder.join(USER_HOME_REPORT), home).expect("user.home report");
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(home) = jvm_option(&argv, "-Duser.home=") {
        report_user_home(&home);
    }
    let mode = mode_of(&argv);
    match mode.as_str() {
        "fatal" => {
            emit(&Envelope::Fatal { reason: "login failed (scripted)".into() });
            std::process::exit(1);
        }
        "silent" => std::process::exit(1),
        // The login that hangs after `Authorize task in queue`: no `ready`, no `fatal`, no exit.
        // `hang-once` hangs only its FIRST launch (a respawn then behaves like the default mode);
        // `hang-always` hangs every launch.
        "hang-always" => {
            log_launch(&argv);
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
        "hang-once" => {
            if log_launch(&argv) == 1 {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
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

    // A jar of the amend era announces the feature, and one of the price lane's era the ticks; `old-jar` mode is
    // the one before both (says nothing).
    let features = if mode == "old-jar" {
        vec![]
    } else {
        let mut features =
            vec!["modify".to_string(), vike_dukascopy::quotes::FEATURE_TICKS.to_string()];
        if mode != "ticks-no-acks" {
            features.push(vike_dukascopy::quotes::FEATURE_TICK_ACKS.to_string());
        }
        features
    };
    emit(&Envelope::Ready {
        account: "FAKE-DEMO".into(),
        balance: if mode == "balance-zero" { 0.0 } else { READY_BALANCE },
        features,
        currency: ready_currency(&mode),
    });
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
                    position_side: vike_model::events::PositionSide::Both,
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
            // The amend: echo the requested terms as `OrderModified` (the sidecar's ORDER_CHANGED_OK mapping), or
            // refuse in `modify-reject` mode (its ORDER_CHANGED_REJECTED mapping).
            Some(Command::Modify { client_order_id, new_qty, new_price, ts }) => {
                let event = if mode == "modify-reject" {
                    Event::OrderModifyRejected(OrderModifyRejected {
                        client_order_id,
                        reason: "scripted refusal".into(),
                        ts,
                    })
                } else {
                    Event::OrderModified(OrderModified {
                        client_order_id,
                        venue_order_id: Some("FAKE-1".into()),
                        new_qty,
                        new_price,
                        ts,
                    })
                };
                emit(&Envelope::Event { event: Box::new(event) });
            }
            // The price lane: the answer (a jar that gives them) and one tick per subscribed symbol (the real sidecar
            // sends the ticks as the market moves).
            Some(Command::Subscribe { symbols }) => {
                for symbol in &symbols {
                    if answer_subscribe(symbol, &mode) {
                        emit_tick(symbol, &mode);
                    }
                }
            }
            Some(Command::Unsubscribe { .. }) => {}
            Some(Command::Shutdown) => break,
            None => eprintln!("fake bridge: skipped unparseable line"),
        }
    }
}

/// The account currency the `ready` line states in `mode`: USD by default (the demo account's), none for the jars
/// of the releases before the field (`old-jar`, and `ticks-no-ccy` = a jar of the price lane's era that states no
/// currency), another money in `ccy-eur`, and a word that is no currency code in `ccy-bad`.
fn ready_currency(mode: &str) -> Option<String> {
    match mode {
        "old-jar" | "ticks-no-ccy" => None,
        "ccy-eur" => Some("EUR".to_string()),
        "ccy-bad" => Some("usd1".to_string()),
        _ => Some("USD".to_string()),
    }
}

/// The jar's `subscription` answer for one subscribed symbol, in the modes that give one; `false` = it declined the
/// symbol, which is then not quoted.
fn answer_subscribe(symbol: &str, mode: &str) -> bool {
    if mode == "ticks-no-acks" {
        return true;
    }
    let declined = mode == "ticks-nack" && symbol == "GBPUSD";
    let reason = if declined { "scripted decline: unknown instrument" } else { "" };
    emit(&Envelope::Subscription {
        symbol: symbol.to_string(),
        ok: !declined,
        reason: reason.to_string(),
    });
    !declined
}

/// One scripted tick for `symbol`, shaped by the mode (see the module doc). A bad mode spoils EURUSD only, so a test
/// that asks for EURUSD and then for another pair knows the bad tick was already judged when the second pair's good
/// one arrives (the fake answers, and the reader delivers, in order).
fn emit_tick(symbol: &str, mode: &str) {
    let now = vike_model::now_ms();
    let spoiled = symbol == "EURUSD";
    let (symbol, bid, time) = match mode {
        "ticks-stale" if spoiled => (symbol, 1.1, now - 60_000),
        "ticks-one-sided" if spoiled => (symbol, 0.0, now),
        "ticks-ghost" if spoiled => ("GBPUSD", 1.3, now),
        // The conversion pairs of the multi-currency money, at a plausible market (a USD account): a JPY pair
        // is about 150 yen to the dollar, a pound is about 1.35 dollars.
        _ if symbol == "USDJPY" => (symbol, 150.09, now),
        _ if symbol == "GBPUSD" => (symbol, 1.3499, now),
        _ => (symbol, 1.1, now),
    };
    let spread = if bid > 100.0 { 0.02 } else { 0.0002 };
    emit(&Envelope::Tick { symbol: symbol.to_string(), bid, ask: bid + spread, time });
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
            Some(Command::Modify { .. }) => eprintln!("fake bridge: netting mode does not amend"),
            Some(Command::Subscribe { .. } | Command::Unsubscribe { .. }) => {
                eprintln!("fake bridge: netting mode prices nothing");
            }
            Some(Command::Shutdown) => break,
            None => eprintln!("fake bridge: skipped unparseable line"),
        }
    }
}

/// Dual-publish one fill (bare FillEvent then the FSM wrap), mirroring `Proto.orderFilled`.
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
        position_side: vike_model::events::PositionSide::Both,
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
