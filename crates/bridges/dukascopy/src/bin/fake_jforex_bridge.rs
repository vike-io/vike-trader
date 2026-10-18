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

    // A jar of the amend era announces the feature; `old-jar` mode is the one before it (says nothing).
    let features = if mode == "old-jar" { vec![] } else { vec!["modify".to_string()] };
    emit(&Envelope::Ready { account: "FAKE-DEMO".into(), balance: 100_000.0, features });
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
            Some(Command::Modify { .. }) => eprintln!("fake bridge: netting mode does not amend"),
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
