//! The read verbs over the node's frame: the cases the in-process channel pinned against its
//! snapshot cell, now over the `CoreSnapshotWire` the node publishes for the same state.

use super::*;
use vike_model::TradingState;
use vike_tradehub_client::wire::{
    OrderViewWire, PositionViewWire, QuoteRateViewWire, VenueBlockWire,
};

/// One venue block carrying only the fields these renders read. `own_book` is the node's
/// `BalanceMode::Delta` (`balance_is_own_book`), `false` its `Authoritative`.
fn block(venue: &str, equity: f64, own_book: bool) -> VenueBlockWire {
    VenueBlockWire {
        account_code: None,
        order_client_id_shape: None,
        venue: venue.to_string(),
        balance: 0.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        equity,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_buying_power: 0.0,
        trading_state: TradingState::Active,
        positions: Vec::new(),
        account: None,
        route_key: venue.to_string(),
        symbols: Vec::new(),
        tier: None,
        contract_values: Vec::new(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: String::new(),
        ledger_currency: String::new(),
        currency_assumed: false,
        balance_is_own_book: own_book,
    }
}

fn snap_of(venues: Vec<VenueBlockWire>) -> CoreSnapshotWire {
    let mut snap = CoreSnapshotWire::empty();
    snap.venues = venues;
    snap
}

/// One open position row, carrying only the fields `/positions` prints.
fn pos(venue: &str, symbol: &str, size: f64) -> PositionViewWire {
    PositionViewWire {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: "BOTH".into(),
        size,
        average_price: 100.0,
        unrealized: 3.0,
        leverage: 1.0,
        liquidation_price: 0.0,
        mark_source: None,
    }
}

/// ⚠ The the CI box shape (2026-08-17): nine paper mounts at 1000 seed plus one bybit mount that
/// adopted a SHARED account's 53647 wallet. `/status` and `/equity` used to answer an operator
/// with the 62647.11 sum of the two. Both must name each quantity, and neither may print the total.
#[test]
fn the_read_verbs_never_answer_with_a_wallet_added_to_paper_seed() {
    let mut venues = vec![block("binance", 1_000.0, true)];
    for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
        venues.push(block(v, 1_000.0, true));
    }
    venues.push(block("bybit", 53_647.10600813, false));
    let snap = snap_of(venues);
    for verb in [ReadVerb::Status, ReadVerb::Equity] {
        let out = render_read(verb, &snap);
        assert!(!out.contains("62647.11"), "{verb:?} must not print the conflated total: {out}");
        assert!(out.contains("9000.00"), "{verb:?} must name the book-kept half: {out}");
        assert!(out.contains("53647.11"), "{verb:?} must name the wallet half: {out}");
        assert!(out.contains("(bybit)"), "{verb:?} must name WHOSE wallet it quoted: {out}");
    }
}

/// A running account whose key changed is one calm line (route key, the shared sentence) after
/// `/status`; no changed key adds no text at all.
#[test]
fn key_changed_is_one_line_per_account_and_nothing_when_empty() {
    assert_eq!(key_changed_lines(&[]), "");
    let out = key_changed_lines(&["bybit#ALT".to_string(), "okx".to_string()]);
    assert_eq!(
        out,
        "\nbybit#ALT: key changed in the database - the old key is still in use until a restart\
         \nokx: key changed in the database - the old key is still in use until a restart"
    );
    let mut snap = snap_of(vec![block("okx", 1.0, true)]);
    snap.key_changed = vec!["okx".to_string()];
    assert!(render_read(ReadVerb::Status, &snap).ends_with(&key_changed_lines(&snap.key_changed)));
    assert!(!render_read(ReadVerb::Equity, &snap).contains("key changed"), "/status only");
}

/// A held balance is one calm line per account (the shared wording: route key, reason, consequence) after
/// `/equity` and `/status`; nothing held adds no text at all.
#[test]
fn a_held_balance_is_one_line_per_account_and_nothing_when_empty() {
    assert_eq!(balance_held_lines(&[]), "");
    let held = vec![
        BalanceWithheldWire { route_key: "oanda".into(), reason: "assumed USD".into() },
        BalanceWithheldWire {
            route_key: "ig#ALT".into(),
            reason: "stated GBP not anchored".into(),
        },
    ];
    let out = balance_held_lines(&held);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "a leading newline, then one line per account: {out:?}");
    assert!(lines[0].is_empty(), "{out:?}");
    assert!(lines[1].starts_with("oanda: balance held: assumed USD"), "{out:?}");
    assert!(lines[2].starts_with("ig#ALT: balance held: stated GBP not anchored"), "{out:?}");
    let mut snap = snap_of(vec![block("oanda", 1.0, true)]);
    snap.balance_withheld = held;
    for verb in [ReadVerb::Status, ReadVerb::Equity] {
        assert!(render_read(verb, &snap).ends_with(&out), "{verb:?}");
    }
    assert!(!render_read(ReadVerb::Orders, &snap).contains("balance held"));
}

/// A pure-paper node has no wallet to name, so the attribution must vanish entirely rather
/// than leave `equity_wallet=0.00 ()` behind.
#[test]
fn a_pure_paper_node_renders_a_zero_wallet_with_no_attribution() {
    let out = render_read(ReadVerb::Equity, &snap_of(vec![block("binance", 1_000.0, true)]));
    assert!(out.contains("equity_book=1000.00"), "{out}");
    assert!(out.contains("equity_wallet=0.00\n"), "no dangling parenthesis: {out}");
}

/// ⚠ **`/status` and `/equity` never add a EUR engine to the dollar figures** (decision 0183, item
/// 13): the lines above are the primary's currency alone, and the EUR group is listed under its own
/// `[EUR]` heading. A node of one currency renders none of it.
#[test]
fn a_second_currency_is_listed_under_its_own_heading_never_added() {
    let mut usd = block("binance", 1_000.0, true);
    usd.realized_pnl = 10.0;
    let mut eur = block("oanda", 7_000.0, true);
    eur.ledger_currency = "EUR".into();
    eur.realized_pnl = -300.0;
    eur.fees_paid = 4.5;
    eur.margin_used = 120.0;
    let snap = snap_of(vec![usd, eur]);

    let status = render_read(ReadVerb::Status, &snap);
    assert!(
        status.contains("equity book=1000.00 wallet=0.00\nrealized=10.00 fees=0.00"),
        "{status}"
    );
    assert!(status.contains("ledgers: the figures above are USD only"), "{status}");
    assert!(status.contains("[EUR] equity book=7000.00 wallet=0.00"), "{status}");
    assert!(status.contains("[EUR] realized=-300.00 fees=4.50"), "{status}");

    let equity = render_read(ReadVerb::Equity, &snap);
    assert!(equity.contains("equity_book=1000.00\n"), "{equity}");
    assert!(
        equity.contains("realized=10.00\n"),
        "the dollar realized, without the EUR -300: {equity}"
    );
    assert!(equity.contains("[EUR] equity_book=7000.00"), "{equity}");
    assert!(equity.contains("[EUR] realized=-300.00"), "{equity}");
    assert!(equity.contains("[EUR] margin_used=120.00"), "{equity}");
}

/// The other-currency sections of `/status` and `/equity` byte for byte (one skeleton renders both):
/// nothing for a node of one currency; otherwise the header line, then each figure under its `[CCY]` tag.
#[test]
fn the_other_currency_sections_are_pinned_byte_for_byte() {
    let mut snap = snap_of(vec![block("binance", 1_000.0, true)]);
    assert_eq!(other_ledgers_status(&snap), "", "one currency: no section");
    assert_eq!(other_ledgers_equity(&snap), "", "one currency: no section");

    let mut eur = block("oanda", 7_000.0, true);
    eur.ledger_currency = "EUR".into();
    eur.realized_pnl = -300.0;
    eur.fees_paid = 4.5;
    eur.funding_paid = 1.25;
    eur.margin_used = 120.0;
    snap.venues.push(eur);
    assert_eq!(
        other_ledgers_status(&snap),
        "\nledgers: the figures above are USD only\n[EUR] equity book=7000.00 wallet=0.00\n[EUR] \
         realized=-300.00 fees=4.50"
    );
    assert_eq!(
        other_ledgers_equity(&snap),
        "\nledgers: the figures above are USD only\n[EUR] equity_book=7000.00\n[EUR] \
         equity_wallet=0.00\n[EUR] realized=-300.00\n[EUR] fees=4.50\n[EUR] funding=1.25\n[EUR] \
         margin_used=120.00"
    );
}

/// **`/equity` shows the conversion rate behind an account's figures, and a stale one says so** (decision
/// 0183): a stale rate that values an open position is spelled out, a fresh one is plain, and a node with no
/// account that holds a symbol quoted in another currency prints nothing more than it did.
#[test]
fn equity_shows_the_conversion_rates_and_says_when_one_is_stale_under_a_position() {
    let plain = render_read(ReadVerb::Equity, &snap_of(vec![block("binance", 1_000.0, true)]));
    assert!(!plain.contains("conversion rates"), "{plain}");

    let mut fxcm = block("fxcm", 7_000.0, true);
    fxcm.ledger_currency = "EUR".into();
    fxcm.quote_rates_in = "EUR".into();
    fxcm.quote_rates = vec![
        QuoteRateViewWire {
            currency: "USD".into(),
            rate: 0.862_069,
            as_of_ms: 1,
            stale: true,
            held: true,
        },
        QuoteRateViewWire {
            currency: "JPY".into(),
            rate: 0.005_747,
            as_of_ms: 1,
            stale: false,
            held: false,
        },
    ];
    let out = render_read(ReadVerb::Equity, &snap_of(vec![block("binance", 1_000.0, true), fxcm]));
    assert!(
        out.contains(
            "\nfxcm: conversion rates: 1 USD = 0.862069 EUR STALE (an open position is valued \
             at it); 1 JPY = 0.005747 EUR"
        ),
        "{out}"
    );
    assert!(!out.contains("binance: conversion rates"), "{out}");
}

/// A node of one currency prints no ledger lines at all: the report is the one it always was.
#[test]
fn one_currency_renders_no_ledger_lines() {
    let snap = snap_of(vec![block("binance", 1_000.0, true), block("bybit", 500.0, true)]);
    for verb in [ReadVerb::Status, ReadVerb::Equity] {
        let out = render_read(verb, &snap);
        assert!(!out.contains("ledgers:") && !out.contains('['), "{verb:?}: {out}");
    }
}

/// The OTHER the CI box shape, the one #1329 measured: the PRIMARY engine is the untraded binance
/// paper mount and every fill happened on the bybit mount beside it. The frame's top-level scalars
/// are the primary's, so `snap.positions` is its empty vec.
fn prod2_shaped_snapshot() -> CoreSnapshotWire {
    let mut binance = block("binance", 1_000.0, true);
    binance.balance = 1_000.0;
    let mut bybit = block("bybit", 1_042.25, true);
    bybit.balance = 1_042.25;
    bybit.realized_pnl = 42.25;
    bybit.fees_paid = 1.75;
    bybit.funding_paid = 0.5;
    bybit.positions = vec![pos("bybit", "BTCUSDT", 0.5)];
    snap_of(vec![binance, bybit])
}

/// ⚠ The defect #1329 fixed in `summary_line`: numbers describing the untraded primary engine while
/// the account traded elsewhere. REDDENS on any read that goes back to the primary-only scalars.
#[test]
fn the_read_verbs_report_the_venue_that_traded_not_the_untraded_primary() {
    let snap = prod2_shaped_snapshot();

    let status = render_read(ReadVerb::Status, &snap);
    assert!(status.contains("realized=42.25"), "/status must fold every venue: {status}");
    assert!(status.contains("fees=1.75"), "/status must fold every venue: {status}");
    assert!(status.contains("positions=1"), "/status must count every venue: {status}");

    let equity = render_read(ReadVerb::Equity, &snap);
    assert!(equity.contains("realized=42.25"), "/equity must fold every venue: {equity}");
    assert!(equity.contains("fees=1.75"), "/equity must fold every venue: {equity}");
    assert!(equity.contains("funding=0.50"), "/equity must fold every venue: {equity}");

    let positions = render_read(ReadVerb::Positions, &snap);
    assert!(
        positions.contains("bybit/BTCUSDT size=0.5"),
        "/positions must list every venue's rows: {positions}"
    );
}

/// **`/positions` and `/orders` print an engine number as `vike-cli`'s tables do.** The engine holds a
/// size or a price as `n * step`, one last bit off its decimal, and `f64::to_string` prints all of it
/// (`8_092_593.0 * 0.01` is `80925.93000000001`, measured on the OKX demo). The order's status is the
/// node's `OrderStatus::as_str` word on the wire and prints as it always did.
#[test]
fn the_read_verbs_print_a_number_without_its_float_tail() {
    let (size, price) = (0.4 * 3.0, 8_092_593.0_f64 * 0.01);
    assert_eq!(price.to_string(), "80925.93000000001", "the fixture is the measured value");
    assert_eq!(size.to_string(), "1.2000000000000002");

    let mut okx = block("okx", 1_000.0, true);
    okx.positions = vec![PositionViewWire { average_price: price, ..pos("okx", "S", size) }];
    let mut snap = snap_of(vec![okx]);
    snap.orders.push(OrderViewWire {
        order_client_id: "c".into(),
        venue: "okx".into(),
        account: None,
        symbol: "S".into(),
        side: 1,
        qty: size,
        order_type: "limit".into(),
        price: Some(price),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted.as_str().to_string(),
        venue_order_id: None,
        filled_qty: size,
        average_fill_price: 0.0,
        data_delay: None,
        refusal_id: None,
        reason: None,
        age_ms: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    });

    assert_eq!(render_read(ReadVerb::Positions, &snap), "okx/S size=1.2 avg=80925.93 unrl=3.00");
    assert_eq!(
        render_read(ReadVerb::Orders, &snap),
        "c okx/S buy 1.2 @ 80925.93 [Accepted] filled=1.2"
    );
}

/// **A terminal order is not working, whichever spelling the node sends**: the `as_str` word a
/// current node writes, and a word this build does not know is listed rather than hidden.
#[test]
fn only_a_working_order_is_listed_and_an_unknown_status_word_is_shown_as_sent() {
    let order = |order_client_id: &str, status: &str| OrderViewWire {
        order_client_id: order_client_id.into(),
        venue: "okx".into(),
        account: None,
        symbol: "S".into(),
        side: -1,
        qty: 1.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        status: status.into(),
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        data_delay: None,
        refusal_id: None,
        reason: None,
        age_ms: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let mut snap = snap_of(vec![block("okx", 1.0, true)]);
    snap.orders = vec![
        order("done", vike_model::OrderStatus::Filled.as_str()),
        order("odd", "SOMETHING_NEW"),
    ];
    assert_eq!(
        render_read(ReadVerb::Orders, &snap),
        "odd okx/S sell 1 @ mkt [SOMETHING_NEW] filled=0"
    );
    assert!(render_read(ReadVerb::Status, &snap).contains("orders=2 (working 1)"));
}

/// **An OLDER node's Debug spelling reads as the desktop reads it** (the shared
/// `vike_tradehub_client::observe_decode::order_status_of`): its `Filled` is terminal, not an unknown word
/// listed as working, and its `Accepted` still works. Before the shared reading both were unknown here.
#[test]
fn an_older_nodes_debug_spelled_status_reads_as_the_desktop_reads_it() {
    let order = |order_client_id: &str, status: &str| OrderViewWire {
        order_client_id: order_client_id.into(),
        venue: "okx".into(),
        account: None,
        symbol: "S".into(),
        side: -1,
        qty: 1.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        status: status.into(),
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        data_delay: None,
        refusal_id: None,
        reason: None,
        age_ms: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let mut snap = snap_of(vec![block("okx", 1.0, true)]);
    snap.orders = vec![order("done", "Filled"), order("live", "Accepted")];
    assert_eq!(render_read(ReadVerb::Orders, &snap), "live okx/S sell 1 @ mkt [Accepted] filled=0");
    assert!(render_read(ReadVerb::Status, &snap).contains("orders=2 (working 1)"));
}

/// **`/orders` lists a REFUSED order the frame still carries, with the refusal's words**, under the working ones:
/// an order the chat sent that the venue then rejected is where `OUTCOME UNKNOWN` sends the operator. A filled or
/// cancelled order is not listed, and a node with no refused order renders as before.
#[test]
fn orders_lists_a_refused_order_with_its_words() {
    let order = |order_client_id: &str, status: vike_model::OrderStatus, reason: Option<&str>| {
        OrderViewWire {
            order_client_id: order_client_id.into(),
            venue: "okx".into(),
            account: None,
            symbol: "S".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(2.0),
            trigger_price: None,
            status: status.as_str().into(),
            venue_order_id: None,
            filled_qty: 0.0,
            average_fill_price: 0.0,
            data_delay: None,
            refusal_id: None,
            reason: reason.map(str::to_string),
            age_ms: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
        }
    };
    use vike_model::OrderStatus::{Accepted, Canceled, Rejected};
    let mut snap = snap_of(vec![block("okx", 1.0, true)]);
    snap.orders = vec![order("live", Accepted, None), order("gone", Canceled, None)];
    assert_eq!(render_read(ReadVerb::Orders, &snap), "live okx/S buy 1 @ 2 [Accepted] filled=0");

    snap.orders.push(order("bad", Rejected, Some("insufficient margin")));
    assert_eq!(
        render_read(ReadVerb::Orders, &snap),
        "live okx/S buy 1 @ 2 [Accepted] filled=0\nrefused in the last hour:\nbad okx/S [Rejected]: \
         insufficient margin"
    );
    snap.orders.retain(|o| o.order_client_id != "live");
    assert_eq!(
        render_read(ReadVerb::Orders, &snap),
        "no working orders\nrefused in the last hour:\nbad okx/S [Rejected]: insufficient margin"
    );
    // The refusal's catalog id, when the node sent one, leads its words.
    let bad = snap.orders.iter_mut().find(|o| o.order_client_id == "bad").expect("listed");
    bad.refusal_id = Some("order.venue.rejected".into());
    assert_eq!(
        render_read(ReadVerb::Orders, &snap),
        "no working orders\nrefused in the last hour:\nbad okx/S [Rejected]: error [order.venue.rejected]: \
         insufficient margin"
    );
}

/// The cash figure is NAMED per venue, never summed across provenances and never the primary
/// engine's alone — see [`balance_by_venue`].
#[test]
fn equity_names_whose_cash_each_balance_is() {
    let equity = render_read(ReadVerb::Equity, &prod2_shaped_snapshot());
    assert!(
        equity.contains("balance=binance:1000.00 bybit:1042.25"),
        "every venue's cash, attributed and untotalled: {equity}"
    );
}

/// A halted venue must not be hidden behind an `Active` primary — the one line an operator
/// reads to answer "am I trading".
#[test]
fn status_names_a_venue_whose_trading_state_disagrees_with_the_primary() {
    let mut snap = prod2_shaped_snapshot();
    snap.venues[1].trading_state = TradingState::Halted;
    let status = render_read(ReadVerb::Status, &snap);
    assert!(status.contains("state=Active (bybit=Halted)"), "{status}");
}

/// **The fold is the node's own**: over the frame's blocks the figures are `vike_model::ledger_sums`,
/// the law `AccountLedgerTotals::of` folds the same blocks by, bit for bit (the book/wallet split included).
#[test]
fn the_ledger_is_the_nodes_fold_bit_for_bit() {
    let mut snap = prod2_shaped_snapshot();
    snap.venues[0].margin_used = 0.1;
    snap.venues[1].margin_used = 0.2;
    snap.venues[0].realized_pnl = 0.1;
    snap.venues[0].fees_paid = 0.7;
    snap.venues[0].funding_paid = -0.3;
    snap.venues.push(block("bybit", 53_647.1, false));
    let all = vike_model::ledger_sums(snap.venues.iter().map(money));
    let got = ledger(&snap, "");
    let bits = f64::to_bits;
    assert_eq!(bits(got.realized_pnl), bits(all.realized_pnl));
    assert_eq!(bits(got.fees_paid), bits(all.fees_paid));
    assert_eq!(bits(got.funding_paid), bits(all.funding_paid));
    assert_eq!(bits(got.margin_used), bits(all.margin_used));
    assert_eq!(
        bits(got.equity_book),
        bits(vike_math::summation::compensated_sum([1_000.0, 1_042.25]))
    );
    assert_eq!(bits(got.equity_wallet), bits(53_647.1));
    // …and an empty frame reads `+0.0`, never an empty fold's `-0.0`.
    let none = ledger(&CoreSnapshotWire::empty(), "");
    assert_eq!(bits(none.margin_used), bits(0.0));
}

/// A single-venue node renders exactly as before: nothing to disagree, nothing to widen.
#[test]
fn a_single_venue_node_renders_the_plain_state_and_its_own_numbers() {
    let mut only = block("bybit", 1_000.0, true);
    only.realized_pnl = 7.5;
    only.positions = vec![pos("bybit", "BTCUSDT", 0.5)];
    let status = render_read(ReadVerb::Status, &snap_of(vec![only]));
    assert!(status.contains("state=Active\n"), "nothing diverges, no parenthetical: {status}");
    assert!(status.contains("realized=7.50"), "{status}");
}

/// Before the first build there are no venue blocks at all: the totals are zero and the cash
/// falls back to the scalar rather than printing an empty `balance=`.
#[test]
fn an_empty_snapshot_still_renders_a_balance() {
    let snap = CoreSnapshotWire::empty();
    let equity = render_read(ReadVerb::Equity, &snap);
    assert!(equity.contains("balance=0.00"), "{equity}");
    assert!(equity.contains("realized=0.00"), "{equity}");
    assert_eq!(render_read(ReadVerb::Positions, &snap), "no open positions");
}
