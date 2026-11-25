//! The delivery-guard table and its one verdict.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_delivery_guard_matrix_is_pinned_verbatim`] | a window moved, or a venue added to the roster with no row |
//! | [`the_verdict_is_okxs_rule`] | a reduce-only order trapped inside the window, or an order sent on a delivered contract |
//! | [`the_verdict_agrees_with_okxs_boundaries`] | the shared rule one millisecond off the okx code it was read from |
//! | [`each_venue_reads_its_dated_names_delivery_instant`] | an expiry read off a name at the wrong hour or day |
//! | [`a_name_that_states_no_day_has_no_expiry`] | a perpetual, a spot or FX pair, an ig epic or a combo guarded |
//! | [`delivery_verdict_is_the_rows_verdict_on_the_names_expiry`] | the one call skipping the name or the row |

use super::*;
use vike_model::VENUES;

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;

/// The whole table, verbatim: the window, the delivery time of day and whether an option's day is in the venue's name,
/// for every roster venue, in milliseconds.
#[test]
fn the_delivery_guard_matrix_is_pinned_verbatim() {
    #[rustfmt::skip]
    const MATRIX: &[(&str, Option<i64>, i64, bool)] = &[
        ("binance",     Some(DAY),     8 * HOUR, false),
        ("bybit",       Some(DAY),     8 * HOUR, false),
        ("okx",         Some(DAY),     8 * HOUR, false),
        ("deribit",     Some(DAY),     8 * HOUR, true),
        ("oanda",       None,          0,        false),
        ("ig",          None,          0,        false),
        ("fxcm",        None,          0,        false),
        ("dukascopy",   None,          0,        false),
        ("polymarket",  None,          0,        false),
        ("ibkr",        Some(5 * DAY), 0,        false),
        ("ctrader",     None,          0,        false),
        ("alpaca",      None,          0,        false),
        ("aster",       None,          0,        false),
        ("hyperliquid", None,          0,        false),
        // vike:new-venue:row ("{venue}", None, 0, false), // TODO(new-venue: {venue}): move with the row above
    ];
    assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
    let fallback = delivery_guard_for("no-such-venue");
    for &(venue, window_ms, time_of_day_ms, option_day) in MATRIX {
        assert!(VENUES.contains(&venue), "{venue} is not a roster venue");
        let row = delivery_guard_for(venue);
        assert_eq!(row.window_ms, window_ms, "{venue}");
        assert_eq!(row.delivery_time_of_day_ms, time_of_day_ms, "{venue}");
        assert_eq!(row.option_day_in_venue_name, option_day, "{venue}");
        assert_ne!(row, fallback, "{venue} rides the unknown-venue fallback");
        assert!(row.why.len() > 30, "{venue}: a row without a reason");
    }
    assert_eq!(fallback.window_ms, None, "an unknown venue has no guard");
}

/// **Each venue's dated user name reads its delivery instant**: the name's day at the venue's time of day (08:00 UTC
/// for binance, bybit, okx and deribit, a deribit option's `DDMMMYY` included; 00:00 UTC of ibkr's last trading day).
#[test]
fn each_venue_reads_its_dated_names_delivery_instant() {
    #[rustfmt::skip]
    const NAMES: &[(&str, &str, i64)] = &[
        ("binance", "BTCUSDT_261225.P",        1_798_185_600_000), // 2026-12-25 08:00 UTC
        ("binance", "BTCUSD_261225.P",         1_798_185_600_000), // COIN-M, same day
        ("bybit",   "ETHUSDT_261101",          1_793_520_000_000), // 2026-11-01 08:00 UTC
        ("okx",     "BTCUSDT_261225",          1_798_185_600_000), // okx's own `expTime` of the day
        ("deribit", "BTCUSD_261103",           1_793_692_800_000), // 2026-11-03 08:00 UTC
        ("deribit", "BTC_USDC-27MAR26-2500-C", 1_774_598_400_000), // 2026-03-27 08:00 UTC
        ("deribit", "ETH-3NOV26-3000-P",       1_793_692_800_000), // a one-digit day
        ("ibkr",    "ES_261218",               1_797_552_000_000), // 2026-12-18 00:00 UTC
    ];
    for &(venue, name, expiry) in NAMES {
        assert_eq!(delivery_guard_for(venue).expiry_of(name), Some(expiry), "{venue} {name}");
    }
}

/// **A name that states no day has no expiry**: a perpetual, a spot or FX pair, a stock, any ig epic (ig has no
/// window), a deribit combo, a day the calendar lacks, an option shape on a venue whose options are not named so, and
/// every name of a venue with no window.
#[test]
fn a_name_that_states_no_day_has_no_expiry() {
    #[rustfmt::skip]
    const NAMES: &[(&str, &str)] = &[
        ("binance",     "BTCUSDT.P"),
        ("binance",     "BTCUSDT"),
        ("binance",     "BTCUSDT_260231.P"),               // no 31 February
        ("bybit",       "BTCUSDT.P"),
        ("okx",         "BTCUSDT"),
        ("deribit",     "BTCUSD.P"),
        ("deribit",     "BTCUSDC"),
        ("deribit",     "BTC-CS-29APR22-39300_39600"),     // a combo: no single day
        ("deribit",     "BTC-FS-27DEC24_PERP"),
        ("deribit",     ""),                               // a combo's empty symbol
        ("deribit",     "BTC-27MAR26-2500"),               // no right
        ("deribit",     "BTC-27MAR26-2500-C-X"),
        ("okx",         "BTC_USDC-27MAR26-2500-C"),        // an option shape off deribit
        ("ibkr",        "EURUSD"),
        ("ibkr",        "AAPL"),
        ("ig",          "IX.D.DAX.FBMU1.IP"),
        ("ig",          "CS.D.EURUSD.MINI.IP"),
        ("ig",          "BTCUSDT_261225"),                 // ig has no window
        ("oanda",       "EUR_USD"),
        ("hyperliquid", "BTCUSDC.P"),
        ("no-such-venue", "BTCUSDT_261225"),
    ];
    for &(venue, name) in NAMES {
        assert_eq!(delivery_guard_for(venue).expiry_of(name), None, "{venue} {name:?}");
    }
}

/// **The one call** is the row's verdict on the name's expiry, and `Taken` where the name states no day or the venue has
/// no window.
#[test]
fn delivery_verdict_is_the_rows_verdict_on_the_names_expiry() {
    let expiry = 1_798_185_600_000; // BTCUSDT_261225's delivery, 2026-12-25 08:00 UTC
    assert_eq!(
        delivery_verdict("binance", "BTCUSDT_261225.P", expiry - HOUR, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: HOUR }
    );
    assert_eq!(
        delivery_verdict("binance", "BTCUSDT_261225.P", expiry - HOUR, true),
        DeliveryGuardVerdict::Taken,
        "a closing order is taken until the expiry"
    );
    assert_eq!(
        delivery_verdict("okx", "BTCUSDT_261225", expiry, true),
        DeliveryGuardVerdict::Delivered
    );
    assert_eq!(
        delivery_verdict("binance", "BTCUSDT_261225.P", expiry - 2 * DAY, false),
        DeliveryGuardVerdict::Taken
    );
    assert_eq!(
        delivery_verdict("binance", "BTCUSDT.P", expiry, false),
        DeliveryGuardVerdict::Taken,
        "a perpetual"
    );
    assert_eq!(
        delivery_verdict("ig", "IX.D.DAX.FBMU1.IP", expiry, false),
        DeliveryGuardVerdict::Taken,
        "no window"
    );
    assert_eq!(
        delivery_verdict("ibkr", "ES_261218", 1_797_552_000_000 - 4 * DAY, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: 4 * DAY },
        "ibkr's five days to 00:00 UTC of its last trading day"
    );
}

/// Inside the window an opening order is refused and a reduce-only one taken; at the expiry both are refused; a row
/// without a window takes everything.
#[test]
fn the_verdict_is_okxs_rule() {
    let okx = delivery_guard_for("okx");
    let expiry = 1_798_185_600_000; // 2026-12-25 08:00 UTC, okx's delivery hour
    let at = |now, reduce_only| okx.verdict(expiry, now, reduce_only);
    assert_eq!(at(expiry - 2 * DAY, false), DeliveryGuardVerdict::Taken, "outside the window");
    assert_eq!(
        at(expiry - 3_600_000, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: 3_600_000 },
        "an opening order an hour before the delivery"
    );
    assert_eq!(
        at(expiry - 3_600_000, true),
        DeliveryGuardVerdict::Taken,
        "a closing order is taken"
    );
    assert_eq!(at(expiry, true), DeliveryGuardVerdict::Delivered, "delivered: nothing is taken");
    assert_eq!(at(expiry + DAY, false), DeliveryGuardVerdict::Delivered);

    let ig = delivery_guard_for("ig");
    assert_eq!(ig.verdict(expiry, expiry + DAY, false), DeliveryGuardVerdict::Taken, "no guard");

    let ibkr = delivery_guard_for("ibkr");
    assert_eq!(
        ibkr.verdict(expiry, expiry - 4 * DAY, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: 4 * DAY },
        "ibkr's window is five days"
    );
    assert_eq!(ibkr.verdict(expiry, expiry - 6 * DAY, false), DeliveryGuardVerdict::Taken);
}

/// The two boundaries are the ones okx's own per-order check drew before B3, to the millisecond: `now >= expiry` is delivered and
/// `now >= expiry - window` refuses an opening order.
#[test]
fn the_verdict_agrees_with_okxs_boundaries() {
    let row = delivery_guard_for("okx");
    let expiry = 1_798_185_600_000;
    assert_eq!(row.verdict(expiry, expiry - DAY - 1, false), DeliveryGuardVerdict::Taken);
    assert_eq!(
        row.verdict(expiry, expiry - DAY, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: DAY }
    );
    assert_eq!(
        row.verdict(expiry, expiry - 1, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: 1 }
    );
    assert_eq!(row.verdict(expiry, expiry - 1, true), DeliveryGuardVerdict::Taken);
    assert_eq!(row.verdict(expiry, expiry, true), DeliveryGuardVerdict::Delivered);
    assert_eq!(
        row.verdict(i64::MIN + 1, i64::MIN, false),
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms: 1 },
        "a window reaching before the epoch range saturates rather than overflowing"
    );
}
