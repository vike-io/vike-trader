//! **The venue's rate in the engine's hands** (decision 0183): a conversion pair's tick, turned into a rate by
//! `vike_dukascopy::home_conversion` and handed to the engine on an admission exactly as the node hands it
//! (`SymbolAdmission::{quote_ccy, quote_rate, rate_as_of_ms}`), books a unit closed on a JPY- or GBP-quoted
//! instrument in DOLLARS, at goldens. The engine's own contract is `crates/vike-exec/tests/engine/quote_money.rs`'s
//! (for OANDA); this file proves the DUKASCOPY half of the orientation reaches it: the number the bridge computes
//! is the number the engine multiplies by.
//!
//! Decision 0021: no tolerance is widened to make a number pass. DOCS-SHAPED: the ticks are scripted
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>), no live tick was captured.

use vike_dukascopy::home_conversion::{AccountPerQuote, conversion_pair};
use vike_dukascopy::quotes::PriceTick;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate,
    SymbolAdmission,
};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{RiskLimits, SymbolGrid};

const VENUE: &str = "dukascopy";
const MOUNTED: &str = "EURUSD";
/// The engine clock of every fixture, ms; every tick is stamped a moment before it.
const T0: i64 = 1_760_000_000_000;
/// 100,000 units: one standard lot.
const Q: f64 = 100_000.0;

/// `got` is `want` up to a relative `1e-12`: the closed form differs from the code's only in association.
fn close(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-12 * want.abs().max(1.0)
}

/// A USD account mounting `EURUSD`, told its currency as the node tells it at assembly.
fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    e.set_account_ccy("USD").expect("a currency code");
    e.now_ms = T0;
    e
}

/// The rate the bridge computes for a pair quoted in `quote` on a USD account from one tick `bid`/`ask`.
fn rate(quote: &str, symbol: &str, bid: f64, ask: f64) -> AccountPerQuote {
    let pair = conversion_pair(quote, "USD").expect("a conversion pair");
    assert_eq!(pair.symbol, symbol);
    let tick = PriceTick { symbol: symbol.into(), bid, ask, ts: T0 - 200 };
    pair.account_per_quote(&tick).expect("a rate")
}

/// What the node hands the engine for `symbol` quoted in `quote` at `rate`, stamped by the tick.
fn admission(symbol: &str, quote: &str, rate: AccountPerQuote) -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: symbol.into(),
        grid: SymbolGrid {
            tick_size: Some(0.001),
            lot_size: Some(1000.0),
            ..SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some(quote.into()),
        quote_rate: Some(rate.get()),
        rate_as_of_ms: Some(T0 - 200),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

/// What the node hands the engine for a pair quoted in the account's own money: no currency, no rate.
fn plain(symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        ..admission(symbol, "", rate("JPY", "USDJPY", 150.09, 150.11))
    }
}

fn fill(trade_id: &'static str, symbol: &str, side: i32, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: T0,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn fold(e: &mut ExecutionEngine<RecordingClient>, ev: Event) {
    e.on_event(&ev, &mut Outbox::default());
}

/// USDJPY on a USD account: the position is worth 100,000 dollars, and 0.10 of a yen on a lot is 66.62 dollars.
#[test]
fn a_lot_of_usdjpy_closed_ten_pips_higher_books_66_62_dollars_not_10000() {
    let mut e = engine();
    let at_150 = rate("JPY", "USDJPY", 149.99, 150.01);
    assert_eq!(e.admit_symbol(&admission("USDJPY", "JPY", at_150)), AdmitOutcome::Admitted);
    fold(&mut e, fill("o1", "USDJPY", 1, Q, 150.0));

    let value = e.account.contract_value_of("USDJPY");
    let notional = value.gross_notional(Q, 150.0);
    assert!(close(notional, 100_000.0), "the position is worth 100,000 USD: {notional}");
    assert!((notional - 15_000_000.0).abs() > 1.0, "NOT the 15,000,000 \"USD\" a raw engine books");

    // the market rises 0.10; the node re-reads the rate before the close folds (the tick of 150.10)
    let at_150_10 = rate("JPY", "USDJPY", 150.09, 150.11);
    e.now_ms += 1_000;
    let refresh = SymbolAdmission {
        rate_as_of_ms: Some(T0 + 800), // the second tick's own time: later than the first
        ..admission("USDJPY", "JPY", at_150_10)
    };
    assert_eq!(e.admit_symbol(&refresh), AdmitOutcome::Refreshed);
    fold(&mut e, fill("c1", "USDJPY", -1, Q, 150.10));

    let realized = e.account.realized_pnl;
    assert!(close(realized, 10_000.0 / 150.10), "10000 / 150.10 = 66.62: {realized}");
    assert_eq!((realized * 100.0).round() / 100.0, 66.62);
    assert!((realized - 10_000.0).abs() > 1.0, "NOT the 10,000 \"USD\" a raw engine books");
}

/// The flipped rate (150.10 where 1/150.10 belongs) is the 22,500x error: pinned as what it is NOT.
#[test]
fn the_flipped_rate_would_book_22500_times_too_much_and_the_bridge_does_not_produce_it() {
    let right = rate("JPY", "USDJPY", 150.09, 150.11).get();
    let flipped = (150.09 + 150.11) / 2.0;
    assert!(close(flipped / right, 150.10 * 150.10), "{}", flipped / right);
    assert!(right < 1.0, "a yen is a fraction of a dollar: {right}");
}

/// EURGBP on a USD account: 85,000 pounds is 114,750 dollars, and 0.01 on a lot is 1,350 dollars, not 1,000.
#[test]
fn a_lot_of_eurgbp_is_booked_at_the_pound_price_in_dollars() {
    let mut e = engine();
    let gbp = rate("GBP", "GBPUSD", 1.3499, 1.3501);
    assert!(gbp.get() > 1.0, "a pound is more than a dollar: {}", gbp.get());
    assert_eq!(e.admit_symbol(&admission("EURGBP", "GBP", gbp)), AdmitOutcome::Admitted);
    fold(&mut e, fill("o1", "EURGBP", 1, Q, 0.85));

    let notional = e.account.contract_value_of("EURGBP").gross_notional(Q, 0.85);
    assert!(close(notional, 114_750.0), "85,000 GBP at 1.35 is 114,750 USD: {notional}");
    assert!((notional - 85_000.0).abs() > 1.0, "NOT the 85,000 \"USD\" a raw engine books (low)");

    fold(&mut e, fill("c1", "EURGBP", -1, Q, 0.86));
    assert!(close(e.account.realized_pnl, 1_350.0), "{}", e.account.realized_pnl);
    assert!((e.account.realized_pnl - 1_000.0).abs() > 1.0, "NOT the 1,000 pounds");
}

/// An instrument quoted in the account's own currency is not converted at all: the same engine, no foreign row,
/// the booked number is the raw one (the identity case, so nothing existing moved).
#[test]
fn an_instrument_in_the_accounts_own_money_is_booked_as_it_always_was() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&plain("GBPUSD")), AdmitOutcome::Admitted);
    fold(&mut e, fill("o1", "GBPUSD", 1, Q, 1.25));
    fold(&mut e, fill("c1", "GBPUSD", -1, Q, 1.26));
    assert!(close(e.account.realized_pnl, 1_000.0), "{}", e.account.realized_pnl);
    assert!(!e.account.has_foreign_quote());
}

/// RESTORE KEEPS THE RATE ROW: the engine's snapshot carries the quote currency and the rate beside the base
/// unit, and the engine rebuilt from it values the position at the same rate (the bridge's rate survives a
/// restart of the node; a fresh one replaces it at the next admission).
#[test]
fn a_restored_engine_keeps_the_rate_row_the_bridge_gave_it() {
    let mut e = engine();
    let at_150_10 = rate("JPY", "USDJPY", 150.09, 150.11);
    assert_eq!(e.admit_symbol(&admission("USDJPY", "JPY", at_150_10)), AdmitOutcome::Admitted);
    fold(&mut e, fill("o1", "USDJPY", 1, Q, 150.10));
    let snap = e.snapshot_state();
    let fx = snap.account.fx.as_ref().expect("a foreign-quoted engine writes its fx table");
    assert_eq!(fx.account_ccy, "USD");
    assert_eq!(fx.symbols, vec![("USDJPY".to_string(), "JPY".to_string())]);
    assert_eq!(fx.rates.len(), 1);
    assert_eq!(fx.rates[0].ccy, "JPY");
    assert_eq!(fx.rates[0].rate.to_bits(), at_150_10.get().to_bits());
    assert_eq!(fx.rates[0].as_of_ms, T0 - 200);

    let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(back.account.quote_ccy_of("USDJPY"), Some("JPY"));
    assert_eq!(back.account.quote_rate_of("USDJPY").to_bits(), at_150_10.get().to_bits());
    let notional = back.account.contract_value_of("USDJPY").gross_notional(Q, 150.10);
    assert!(close(notional, 100_000.0), "{notional}");
    assert_eq!(back.snapshot_state(), snap);
}

/// Fail closed: a rate the bridge could not produce is never a rate the engine is handed. The engine refuses an
/// admission that states a currency and no usable rate (and the node never builds one), and the bridge's rate
/// type cannot hold zero, a negative or a non-number.
#[test]
fn an_unusable_rate_is_refused_by_the_engine_too_and_never_valued_at_par() {
    let mut e = engine();
    for bad in [0.0, -1.0 / 150.0, f64::NAN, f64::INFINITY] {
        assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
        let a = SymbolAdmission {
            quote_rate: Some(bad),
            ..admission("USDJPY", "JPY", rate("JPY", "USDJPY", 150.09, 150.11))
        };
        assert!(matches!(e.admit_symbol(&a), AdmitOutcome::Refused(_)), "{bad}");
    }
    let no_rate = SymbolAdmission {
        quote_rate: None,
        ..admission("USDJPY", "JPY", rate("JPY", "USDJPY", 150.09, 150.11))
    };
    assert!(matches!(e.admit_symbol(&no_rate), AdmitOutcome::Refused(_)));
    assert!(!e.accepts_symbol("USDJPY"), "nothing was admitted, at par or otherwise");
}
