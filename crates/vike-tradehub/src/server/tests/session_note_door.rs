//! The control door's CLOSED-MARKET NOTE (decision 0236): an order whose market is closed is still SENT, and the
//! door's answer carries one plain sentence saying so.
//!
//! Two layers, as the symbol door's suite has. The SENTENCE ([`note_for`]) is a pure function of the venue, the
//! symbol, its KEPT row (the class the datahub's list states) and the clock, so each case has a row, with the
//! daylight-saving boundaries of New York pinned on both sides of each change (the FX week closes at 17:00 New York
//! on Friday and reopens at 17:00 on Sunday: 21:00 or 22:00 UTC by the season). The END-TO-END tests drive
//! `accept_command` into a REAL core over a scripted venue client with the door's clock stood on a closed or an open
//! instant and its kept rows handed in: the order reaches the venue client either way, and only the answer differs.

use vike_model::{AccountTier, Contract, ContractEquity, ContractFx, Instrument};
use vike_tradehub_client::wire::{CommandWire, OrderBracketWire};

use super::admits_any_symbol::{Node, limit};
use crate::server::control::{ControlAcceptError, ControlAcceptReply, accept_command};
use crate::server::session_note::{closed_market_note, note_for};

/// Epoch-ms of a UTC instant, from the workspace's one civil calendar.
fn ms(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
    vike_utility::calendar::days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000 + mi * 60_000
}

/// The kept row of `symbol` on `venue` the datahub's list holds, of the class `contract` states.
fn row(venue: &str, symbol: &str, contract: Contract) -> Instrument {
    Instrument::new(venue, symbol, "BASE", "USD", contract)
}

/// An FX pair's kept row.
fn fx(venue: &str, symbol: &str) -> Instrument {
    row(venue, symbol, Contract::Fx(ContractFx::default()))
}

/// The sentence for an FX pair the list carries.
fn said(venue: &str, symbol: &str, at: i64) -> Option<String> {
    note_for(venue, symbol, Some(&fx(venue, symbol)), at)
}

/// The kept rows of the end-to-end tests: oanda's `EUR_USD` (the venue's own name), the FX pair whose user name
/// is `EURUSD` (decision 0290: the note finds the row by the order's user name).
fn oanda_pairs() -> Vec<Instrument> {
    // named as the datahub's write stores them (`vike_catalog::spelled`)
    vike_catalog::spelled(vec![Instrument::new(
        "oanda",
        "EUR_USD",
        "EUR",
        "USD",
        Contract::Fx(ContractFx::default()),
    )])
}

fn fx_sentence(symbol: &str, until: &str) -> String {
    format!(
        "{symbol}: the FX market is closed until {until} UTC: the venue may hold, refuse or queue this order"
    )
}

/// The sentence of the brief, to the letter: Friday night 30 October 2026 (17:00 New York is 21:00 UTC under
/// daylight time), reopening Sunday 1 November at 17:00 under standard time, which is 22:00 UTC.
#[test]
fn friday_night_names_the_sunday_reopen_in_utc() {
    let closed = ms(2026, 10, 30, 21, 30);
    assert_eq!(said("oanda", "EURUSD", closed), Some(fx_sentence("EURUSD", "Sunday 22:00")));
    // the same pair in each broker's own spelling
    for (venue, symbol) in [
        ("dukascopy", "EUR/USD"),
        ("fxcm", "EUR/USD"),
        ("oanda", "EUR_USD"),
        ("ctrader", "EURUSD"),
        ("ig", "CS.D.EURUSD.CFD.IP"),
    ] {
        assert_eq!(
            said(venue, symbol, closed),
            Some(fx_sentence(symbol, "Sunday 22:00")),
            "{venue}"
        );
    }
    // Saturday, mid-weekend: the same reopen
    assert_eq!(
        said("fxcm", "USDJPY", ms(2026, 10, 31, 12, 0)),
        Some(fx_sentence("USDJPY", "Sunday 22:00"))
    );
}

/// Either side of each daylight-saving change, on the minute: open until the New York close, silent after
/// the reopen, and the reopen it names is the real one.
#[test]
fn the_note_follows_new_yorks_daylight_saving_on_both_sides_of_each_change() {
    // Friday 30 Oct (EDT): the market is open at 20:59 UTC, closed from 21:00
    assert_eq!(said("oanda", "EUR_USD", ms(2026, 10, 30, 20, 59)), None);
    assert!(said("oanda", "EUR_USD", ms(2026, 10, 30, 21, 0)).is_some());
    // Sunday 1 Nov (the clocks went back that morning): still closed at 21:59 UTC, open from 22:00
    assert_eq!(
        said("oanda", "EUR_USD", ms(2026, 11, 1, 21, 59)),
        Some(fx_sentence("EUR_USD", "Sunday 22:00"))
    );
    assert_eq!(said("oanda", "EUR_USD", ms(2026, 11, 1, 22, 0)), None);
    // Friday 6 Nov (EST): the close moved to 22:00 UTC
    assert_eq!(said("oanda", "EUR_USD", ms(2026, 11, 6, 21, 59)), None);
    assert!(said("oanda", "EUR_USD", ms(2026, 11, 6, 22, 0)).is_some());
    // Friday 6 Mar 2026 (EST) closes at 22:00 UTC, and the Sunday the clocks go forward (8 March) reopens at
    // 17:00 EDT, which is 21:00 UTC
    assert_eq!(said("fxcm", "EUR/USD", ms(2026, 3, 6, 21, 59)), None);
    assert_eq!(
        said("fxcm", "EUR/USD", ms(2026, 3, 6, 22, 0)),
        Some(fx_sentence("EUR/USD", "Sunday 21:00"))
    );
    assert_eq!(
        said("fxcm", "EUR/USD", ms(2026, 3, 8, 20, 59)),
        Some(fx_sentence("EUR/USD", "Sunday 21:00"))
    );
    assert_eq!(said("fxcm", "EUR/USD", ms(2026, 3, 8, 21, 0)), None);
}

/// A symbol whose kept row states no class with a calendar, or whose venue has none, gets NO note, whatever
/// the clock says, and so does a symbol the list does not carry: its class is unknown, never guessed.
#[test]
fn a_symbol_with_no_known_calendar_says_nothing() {
    let weekend = ms(2026, 10, 31, 12, 0);
    let share = || Contract::Equity(ContractEquity::default());
    for (venue, symbol, contract) in [
        ("oanda", "SPX500_USD", Contract::Index), // an index: hours of its own
        ("ctrader", "XAUUSD", Contract::Commodity), // a metal
        ("ig", "IX.D.DAX.DAILY.IP", Contract::Index), // an index epic
        ("ibkr", "AAPL.SMART.USD", share()), // a global book: a stock's region is not in its class
        ("ibkr", "EUR.USD.IDEALPRO", Contract::Fx(ContractFx::default())), // IBKR says nothing
        ("binance", "BTCUSDT", Contract::CryptoSpot), // always open
        ("hyperliquid", "BTC", Contract::CryptoSpot),
        ("alpaca", "BTC/USD", Contract::CryptoSpot), // a coin: always open
    ] {
        let kept = row(venue, symbol, contract);
        assert_eq!(note_for(venue, symbol, Some(&kept), weekend), None, "{venue} {symbol}");
    }
    for (venue, symbol) in [("oanda", "EURUSD"), ("fxcm", "EUR/USD"), ("alpaca", "AAPL")] {
        assert_eq!(
            note_for(venue, symbol, None, weekend),
            None,
            "{venue} {symbol}: not on the list"
        );
    }
}

/// The one stock calendar that exists (alpaca's equity row, the US regular session), exact to the season.
#[test]
fn a_us_share_on_alpaca_names_the_next_regular_session() {
    let sentence = |symbol: &str, until: &str| {
        format!(
            "{symbol}: the US stock market's regular session is closed until {until} UTC: the venue may hold, refuse or queue this order"
        )
    };
    let aapl = row("alpaca", "AAPL", Contract::Equity(ContractEquity::default()));
    let said = |at: i64| note_for("alpaca", "AAPL", Some(&aapl), at);
    // Saturday 31 Oct 2026: Monday's 09:30 EST open is 14:30 UTC
    assert_eq!(said(ms(2026, 10, 31, 15, 0)), Some(sentence("AAPL", "Monday 14:30")));
    // Wednesday 1 Jul 2026 (EDT): 13:30 UTC is the open, 20:00 the close
    assert_eq!(said(ms(2026, 7, 1, 13, 30)), None);
    assert_eq!(said(ms(2026, 7, 1, 20, 0)), Some(sentence("AAPL", "Thursday 13:30")));
}

/// Only an ORDER gets a note: a submit and a bracket do, a cancel does not.
#[test]
fn only_an_order_command_gets_a_note() {
    let closed = ms(2026, 10, 30, 21, 30);
    let sentence = fx_sentence("EURUSD", "Sunday 22:00");
    assert_eq!(
        closed_market_note(&limit("c-1", "oanda", "EURUSD", 1.0, 1.1), closed, oanda_pairs),
        Some(sentence.clone())
    );
    let bracket = CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(1.1),
        stop_loss: 1.0,
        take_profit: 1.2,
        account: None,
    });
    assert_eq!(closed_market_note(&bracket, closed, oanda_pairs), Some(sentence));
    assert_eq!(closed_market_note(&CommandWire::Cancel("c-1".into()), closed, oanda_pairs), None);
    // ...and with no kept list the pair's class is unknown: no note.
    assert_eq!(closed_market_note(&bracket, closed, Vec::new), None);
}

fn friday_night() -> i64 {
    ms(2026, 10, 30, 21, 30)
}

fn wednesday_noon() -> i64 {
    ms(2026, 10, 28, 12, 0)
}

/// One command through the ONE acceptance path with the whole verdict kept (the harness's `send` keeps the
/// order client id alone).
fn accept(node: &mut Node, cmd: CommandWire) -> Result<ControlAcceptReply, ControlAcceptError> {
    let blocks = node.blocks();
    let sink = node.core.command_sink();
    accept_command(
        cmd,
        None,
        &mut node.limits,
        &sink,
        None,
        &[],
        &[],
        &[],
        &blocks,
        &[],
        &[],
        &[],
        None,
        None,
        None,
    )
}

/// **End to end**: an FX order on a closed Friday night is sent (it reaches the venue client) and its answer
/// carries the note; the same order on an open Wednesday carries none. The door never refuses for it.
#[test]
fn an_order_into_a_closed_market_is_sent_and_the_answer_carries_the_note() {
    let mut node = Node::start("oanda", "EURUSD", AccountTier::Demo);
    node.limits.set_session_rows(oanda_pairs);

    node.limits.set_clock(friday_night);
    let answer = accept(&mut node, limit("closed-1", "oanda", "EURUSD", 1.0, 1.1))
        .expect("a closed market is a note, never a refusal");
    assert_eq!(
        answer,
        ControlAcceptReply::OrderClientId {
            order_client_id: "closed-1".into(),
            notes: vec![fx_sentence("EURUSD", "Sunday 22:00")],
            emulated: None,
        }
    );
    assert_eq!(node.venue_saw("closed-1").symbol, "EURUSD", "and the order was still SENT");

    node.limits.set_clock(wednesday_noon);
    let answer = accept(&mut node, limit("open-1", "oanda", "EURUSD", 1.0, 1.1))
        .expect("an open market is accepted as it always was");
    assert_eq!(
        answer,
        ControlAcceptReply::OrderClientId {
            order_client_id: "open-1".into(),
            notes: Vec::new(),
            emulated: None
        }
    );
    node.venue_saw("open-1");
    node.core.shutdown_and_join();
}

/// A venue with no calendar for the symbol answers with no note even when the clock is on a weekend, and the
/// order is sent as ever.
#[test]
fn a_crypto_order_on_a_weekend_carries_no_note() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.limits.set_clock(friday_night);
    let answer =
        accept(&mut node, limit("crypto-1", "bybit", "BTCUSDT", 0.01, 3_000.0)).expect("accepted");
    assert_eq!(
        answer,
        ControlAcceptReply::OrderClientId {
            order_client_id: "crypto-1".into(),
            notes: Vec::new(),
            emulated: None
        }
    );
    node.venue_saw("crypto-1");
    node.core.shutdown_and_join();
}

/// **The EMULATED note** (decision 0275), the same shape: a pegged order and a visible-size limit on a venue that
/// holds neither (bybit) are accepted with ONE note each, the route's own sentence, and its word; a plain limit on
/// the same node carries neither.
#[test]
fn an_order_the_node_emulates_is_answered_with_its_one_note_and_its_word() {
    use vike_tradehub_client::wire::OrderPegWire;
    use vike_venues::OrderEmulationKind;

    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.limits.set_clock(wednesday_noon);
    let CommandWire::Submit(mut pegged) = limit("peg-1", "bybit", "BTCUSDT", 0.01, 3_000.0) else {
        unreachable!()
    };
    pegged.order_type = "pegged".into();
    pegged.price = None;
    pegged.peg = Some(OrderPegWire { anchor: "mid".into(), offset: 0.0 });
    let CommandWire::Submit(mut shown) = limit("ice-1", "bybit", "BTCUSDT", 0.01, 3_000.0) else {
        unreachable!()
    };
    shown.display_quantity = Some(0.002);
    for (cmd, order_client_id, route) in [
        (CommandWire::Submit(pegged), "peg-1", OrderEmulationKind::Chase),
        (CommandWire::Submit(shown), "ice-1", OrderEmulationKind::Iceberg),
    ] {
        assert_eq!(
            accept(&mut node, cmd).expect("an emulated order is accepted"),
            ControlAcceptReply::OrderClientId {
                order_client_id: order_client_id.into(),
                notes: vec![route.note("bybit")],
                emulated: Some(route.as_word()),
            },
            "{order_client_id}"
        );
    }
    let plain = accept(&mut node, limit("plain-1", "bybit", "BTCUSDT", 0.01, 3_000.0));
    assert_eq!(
        plain.expect("accepted"),
        ControlAcceptReply::OrderClientId {
            order_client_id: "plain-1".into(),
            notes: Vec::new(),
            emulated: None
        }
    );
    node.core.shutdown_and_join();
}
