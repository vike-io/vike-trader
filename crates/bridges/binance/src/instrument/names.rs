//! `names` — **a binance instrument's USER name and its LANE name, both ways** (decision 0290).
//!
//! The engine, the journal, the fills, the store and every door speak the USER name: the spot pair `BTCUSDT`, the
//! USDⓈ-M perpetual `BTCUSDT.P`, a dated USDⓈ-M future `BTCUSDT_261225`, the COIN-M perpetual `BTCUSD.P` and a COIN-M
//! quarterly `BTCUSD_261225`. This bridge's code speaks the LANE name, the KEPT row's own (`raw_symbol`, the datahub's
//! `<project>/instruments/binance.json`): the exchange symbol, with the futures lane's `.P` on every futures row
//! (`BTCUSDT.P`, `BTCUSDT_261225.P`, `BTCUSD_PERP.P`, `BTCUSD_261225.P`), so its `.P` picks the futures book and its
//! stripped form is the wire's (`crate::family::execution::dispatch::split_symbol`). [`lane_name_of`] reads a user
//! name's kept row, so a dated `BTCUSDT_261225` goes to the futures lane by its row and `BTCUSD.P` to COIN-M; no list,
//! no row or two rows is a refusal in words, never a name sent as typed. [`user_name_of`] is the way back for every
//! row the venue sends, applied where events and reports leave the bridge ([`speak_user_names`]).

use vike_model::events::Event;

use crate::refusals::BinanceNameRefusal;

const VENUE: &str = "binance";

/// **The lane name of a USER name**: its KEPT row's (`BTCUSD_PERP.P` for `BTCUSD.P`, `BTCUSDT_261225.P` for
/// `BTCUSDT_261225`, `BTCUSDT` for the spot pair `BTCUSDT`).
///
/// # Errors
/// No binance list is kept in this process, none of its rows is `user`, or two are: each in words naming `user`.
pub fn lane_name_of(user: &str) -> Result<String, BinanceNameRefusal> {
    vike_catalog::kept_raw_of_user_name(VENUE, user)
        .map_err(|why| BinanceNameRefusal::NotKept { why })
}

/// **The USER name a venue row's `lane` name is booked under** (a fill's, a funding payment's, a reconcile report's,
/// each labelled `<exchange symbol>.P` on the futures lane): its kept row's user name, or `lane` itself said at WARN
/// when no kept row is named so (an instrument traded by hand that the list does not carry: no name this process
/// signed).
#[must_use]
pub fn user_name_of(lane: &str) -> String {
    vike_catalog::kept_user_name_of_raw(VENUE, lane).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            lane,
            "a venue row names an instrument no kept row names (the datahub's <project>/instruments/binance.json); \
             it is booked under the venue's own name"
        );
        lane.to_string()
    })
}

/// Whether the LANE name or exchange symbol `symbol` names a COIN-M contract: the `dapi` book's own spelling, a dollar
/// pair and `_PERP` or `_YYMMDD` (`BTCUSD_PERP`, `BTCUSD_261225`, with or without the lane's `.P`). The ONE reading of a
/// COIN-M venue name in this bridge (BIN-5): the exec plane's host and margining
/// (`crate::execution::rest::coin_m`), the feed's tables and the symbol facts all ask it. A USER name (`BTCUSD.P`) is
/// the catalog's, read by its kept row's margining (`vike_catalog::is_inverse_contract`), and the bridge reaches this
/// spelling through that row ([`lane_name_of`]). Asked on the futures lane only (a spot pair never reaches a futures
/// client); the USDⓈ-M dated book is `<COIN>USDT_YYMMDD`.
#[must_use]
pub fn is_coin_m(symbol: &str) -> bool {
    let wire = symbol.to_ascii_uppercase();
    let wire = vike_venues::split_perp(&wire).0;
    let Some((pair, tail)) = wire.split_once('_') else { return false };
    let dated = tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit());
    pair.len() > "USD".len() && pair.ends_with("USD") && (tail == "PERP" || dated)
}

/// **The coin a COIN-M lane name or exchange symbol settles in**: its base, which the `dapi` spelling names first
/// (`BTCUSD_PERP` and `BTCUSD_261225.P` settle in `BTC`); `None` for a name that is no COIN-M one ([`is_coin_m`]).
#[must_use]
pub fn coin_m_settle_coin(symbol: &str) -> Option<String> {
    if !is_coin_m(symbol) {
        return None;
    }
    let upper = symbol.to_ascii_uppercase();
    upper.split('_').next()?.strip_suffix("USD").map(str::to_string)
}

/// **`events` as they leave the bridge**: every symbol they carry respelled from the lane name to its USER name
/// ([`user_name_of`]).
#[must_use]
pub fn speak_user_names(mut events: Vec<Event>) -> Vec<Event> {
    vike_bridge_core::event_map::respell_symbols(&mut events, user_name_of);
    events
}

/// The lane names [`kept_list_for_tests`] holds beside the recorded COIN-M rows, with each row's base and quote: every
/// one a binance test names, so each test of a path that reads a user name holds the SAME list (the tests of one
/// binary share the process). A name without the `.P` is a spot pair, one with it a perpetual, a `_YYMMDD` one a
/// future delivered that day; a `USD` quote is coin-margined.
#[cfg(any(test, feature = "test-support"))]
const TEST_ROWS: &[(&str, &str, &str)] = &[
    ("BTCUSDT", "BTC", "USDT"),
    ("ETHUSDT", "ETH", "USDT"),
    ("SOLUSDT", "SOL", "USDT"),
    ("XRPUSDT", "XRP", "USDT"),
    ("BNBUSDT", "BNB", "USDT"),
    ("ETHBTC", "ETH", "BTC"),
    ("BTCUSDC", "BTC", "USDC"),
    ("BTCUSDT.P", "BTC", "USDT"),
    ("BTCUSDC.P", "BTC", "USDC"),
    ("ETHUSDT.P", "ETH", "USDT"),
    ("SOLUSDT.P", "SOL", "USDT"),
    ("XRPUSDT.P", "XRP", "USDT"),
    ("BNBUSDT.P", "BNB", "USDT"),
    ("XAUUSDT.P", "XAU", "USDT"),
    ("TSLAUSDT.P", "TSLA", "USDT"),
    ("1000PEPEUSDT.P", "1000PEPE", "USDT"),
    ("BTCUSDT_261225.P", "BTC", "USDT"),
    ("ETHUSDT_261225.P", "ETH", "USDT"),
    ("BTCUSD_260925.P", "BTC", "USD"),
];

/// **Test support: hold binance's kept list in this process** (`vike_catalog::hold_kept_list`): the recorded COIN-M rows
/// (`vike_catalog::recorded_kept_lists`) and a row of every other lane name a test names ([`TEST_ROWS`]). What a test
/// that holds no datahub hands every reader of a user name (`BTCUSD.P` is `BTCUSD_PERP.P`); a second call changes
/// nothing.
#[cfg(any(test, feature = "test-support"))]
pub fn kept_list_for_tests() {
    use vike_model::{Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily};
    use vike_model::{ContractPerp, Instrument, Margining};
    // 08:00 UTC of `20YY-MM-DD` (`YYMMDD`), the civil-day count of Howard Hinnant's `days_from_civil`
    let delivered = |day: &str| -> i64 {
        let number = |range: std::ops::Range<usize>| day[range].parse::<i64>().unwrap_or_default();
        let (y, m, d) = (2000 + number(0..2), number(2..4), number(4..6));
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
        days * vike_model::MS_PER_DAY + 8 * vike_model::MS_PER_HOUR
    };
    let mut rows: Vec<Instrument> = vike_catalog::recorded_kept_lists()
        .into_iter()
        .filter(|list| list.venue == VENUE)
        .flat_map(|list| list.instruments)
        .collect();
    // and the admission-cap tests' perpetuals (`S0USDT.P` on), more than the engine admits
    let capped: Vec<(String, String)> =
        (0..32).map(|i| (format!("S{i}USDT.P"), format!("S{i}"))).collect();
    let named = TEST_ROWS.iter().map(|(lane, base, quote)| (*lane, *base, *quote));
    for (lane, base, quote) in
        named.chain(capped.iter().map(|(l, b)| (l.as_str(), b.as_str(), "USDT")))
    {
        let (exchange, on_futures) = vike_venues::split_perp(lane);
        let margining = if quote == "USD" { Margining::Inverse } else { Margining::Linear };
        let settle = if quote == "USD" { base } else { quote }.to_string();
        let contract = match exchange.split_once('_') {
            _ if !on_futures => Contract::CryptoSpot,
            Some((_, day)) => Contract::Future(ContractFuture {
                margining,
                settle,
                value_currency: None,
                expiry_ms: delivered(day),
                tenor: None,
                delivery: ContractFutureDelivery::Cash,
                family: ContractFutureFamily::Crypto,
            }),
            None => Contract::Perp(ContractPerp {
                margining,
                settle,
                value_currency: None,
                funding_interval_ms: None,
                settling_ms: None,
            }),
        };
        rows.push(Instrument::new(VENUE, lane, base, quote, contract));
    }
    vike_catalog::hold_kept_list(VENUE, rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded COIN-M rows (`vike_catalog::recorded_kept_lists`, held by [`kept_list_for_tests`], never alone: a
    /// sibling test reading another row shares this process's list) round trip: a user name to its lane name and
    /// back; the old laned name is no user name.
    #[test]
    fn a_user_name_and_its_lane_name_round_trip_through_the_kept_row() {
        kept_list_for_tests();
        for (user, lane) in [
            ("BTCUSD.P", "BTCUSD_PERP.P"),
            ("ETHUSD.P", "ETHUSD_PERP.P"),
            ("BTCUSD_261225", "BTCUSD_261225.P"),
        ] {
            assert_eq!(lane_name_of(user).as_deref(), Ok(lane), "{user}");
            assert_eq!(lane_name_of(&user.to_ascii_lowercase()).as_deref(), Ok(lane), "{user}");
            assert_eq!(user_name_of(lane), user, "{lane}");
        }
        let why = lane_name_of("BTCUSD_PERP.P").expect_err("the lane name is no user name");
        assert_eq!(why.id(), crate::refusals::BinanceNameRefusalId::NotKept, "{why}");
    }

    /// **A dated USDⓈ-M user name goes to the FUTURES lane by its kept row** (decision 0290): `BTCUSDT_261225` is the
    /// lane's `BTCUSDT_261225.P`, which the dispatcher splits onto the futures book as the exchange's `BTCUSDT_261225`
    /// (USDⓈ-M, never COIN-M); the spot pair stays on the spot lane, and `BTCUSD.P` is the COIN-M perpetual, inverse.
    #[test]
    fn a_dated_user_name_routes_to_the_futures_lane_and_btcusd_p_is_coin_m() {
        kept_list_for_tests();
        let dated = lane_name_of("BTCUSDT_261225").expect("a kept dated row");
        assert_eq!(dated, "BTCUSDT_261225.P");
        assert_eq!(
            crate::family::execution::dispatch::split_symbol(&dated),
            ("BTCUSDT_261225".to_string(), true),
            "the futures lane, the exchange symbol on the wire"
        );
        assert!(!is_coin_m(&dated), "a USDⓈ-M future");
        assert!(vike_catalog::on_futures_lane(VENUE, "btcusdt_261225"));
        assert_eq!(user_name_of(&dated), "BTCUSDT_261225");

        let spot = lane_name_of("BTCUSDT").expect("the spot pair");
        assert!(!crate::family::execution::dispatch::split_symbol(&spot).1, "the spot lane");
        assert!(!vike_catalog::on_futures_lane(VENUE, "BTCUSDT"));

        let coin_m = lane_name_of("BTCUSD.P").expect("the COIN-M perpetual");
        assert!(is_coin_m(&coin_m) && coin_m_settle_coin(&coin_m).as_deref() == Some("BTC"));
        assert!(vike_catalog::is_inverse_contract(VENUE, "BTCUSD.P"));
        assert!(!vike_catalog::is_inverse_contract(VENUE, "BTCUSDT.P"));
    }
}
