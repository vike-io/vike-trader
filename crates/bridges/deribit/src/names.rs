//! `names` — **a deribit instrument's USER name and the venue's own name, both ways** (decision 0290).
//!
//! The engine, the journal, the fills, the store and every door speak the USER name: the inverse perpetual `BTCUSD.P`
//! (the venue's `BTC-PERPETUAL`), the USDC one `BTCUSDC.P` (`BTC_USDC-PERPETUAL`), a dated future `BTCUSD_261103`
//! (`BTC-3NOV26`), the spot pair `BTCUSDC` (`BTC_USDC`), and an option deribit's own name (`ETH_USDC-27MAR26-2500-C`,
//! decision 0290's O-3). The venue's name lives only inside this bridge: it is the KEPT row's (the datahub's
//! `<project>/instruments/deribit.json`, kept verbatim, a decimal strike's lower-case `d` included: DER-3), read by
//! [`venue_name_of`]; [`user_name_of`] is the way back for every row the venue sends, applied where events and
//! reports leave the bridge ([`speak_user_names`]). No list, no row or two rows is a refusal in words, never a name
//! sent as typed. The one case rule of a typed name is the door's (`vike_catalog::door_symbol`).

use vike_model::events::Event;

use crate::refusals::DeribitNameRefusal;

const VENUE: &str = "deribit";

/// **The venue's instrument name of a USER name**: its KEPT row's (`BTC-PERPETUAL` for `BTCUSD.P`). What every order,
/// read, socket and history request of the name signs or asks.
///
/// # Errors
/// A name the door refuses by its shape (`vike_catalog::door_symbol`: a coin-settled option, which has no user name,
/// in the door's own words), no deribit list kept in this process (`vike-cli data catalog refresh --venue deribit`
/// writes it), none of its rows is `user`, or two are: each in words naming `user`.
pub fn venue_name_of(user: &str) -> Result<String, DeribitNameRefusal> {
    let refused = |why: String| DeribitNameRefusal::NotKept { why };
    if let Some(Err(why)) = vike_catalog::door_symbol(VENUE, user) {
        return Err(refused(why));
    }
    vike_catalog::kept_raw_of_user_name(VENUE, user).map_err(refused)
}

/// **The USER name a venue row's `venue_name` is booked under** (a fill, a funding settlement, a reconcile report):
/// its kept row's user name, or the venue's own name said at WARN when no kept row is named so (an instrument the
/// operator traded by hand that the list does not carry: no name this process signed).
#[must_use]
pub fn user_name_of(venue_name: &str) -> String {
    vike_catalog::kept_user_name_of_raw(VENUE, venue_name).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            venue_name,
            "a venue row names an instrument no kept row names (the datahub's <project>/instruments/deribit.json); \
             it is booked under the venue's own name"
        );
        venue_name.to_string()
    })
}

/// **`events` as they leave the bridge**: every symbol they carry respelled from the venue's name to its USER name
/// ([`user_name_of`]).
#[must_use]
pub fn speak_user_names(mut events: Vec<Event>) -> Vec<Event> {
    vike_bridge_core::event_map::respell_symbols(&mut events, user_name_of);
    events
}

/// **Test support: hold deribit's kept list in this process** (`vike_catalog::hold_kept_list`): the bridge's committed
/// listings (`tests/fixtures/deribit_get_instruments.json`, every book, and the captured testnet class rows
/// `tests/fixtures/deribit_classes_testnet_instruments.json`) parsed by the catalog's own parser, and a row of every
/// other name a test signs ([`TEST_VENUE_NAMES`]). What a test that holds no datahub hands every reader of a user name;
/// every test holds the SAME list (the tests of one binary share the process), and a second call changes nothing.
#[cfg(any(test, feature = "test-support"))]
pub fn hold_deribit_kept_rows() {
    let listing: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/deribit_get_instruments.json"))
            .expect("the committed listing is JSON");
    let classes: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/deribit_classes_testnet_instruments.json"
    ))
    .expect("the captured class rows are JSON");
    let captured = classes
        .as_object()
        .into_iter()
        .flat_map(|rows| rows.values())
        .map(|row| serde_json::json!({ "result": [row] }));
    // one row per instrument: the fixture's books overlap (its unfiltered BTC book repeats the others' rows)
    let mut rows: std::collections::BTreeMap<String, vike_model::Instrument> = listing
        .as_object()
        .into_iter()
        .flat_map(|books| books.values().cloned())
        .chain(captured)
        .flat_map(|book| crate::catalog::parse_instruments(&book))
        .map(|row| (row.raw_symbol.clone(), row))
        .collect();
    for name in TEST_VENUE_NAMES {
        let row = test_row(name).unwrap_or_else(|| panic!("{name} spells no deribit row"));
        rows.entry(row.raw_symbol.clone()).or_insert(row);
    }
    vike_catalog::hold_kept_list(VENUE, rows.into_values().collect());
}

/// The venue names a test signs that no committed listing carries, each held by [`hold_deribit_kept_rows`] as a row of
/// the class its name spells ([`test_row`]).
#[cfg(any(test, feature = "test-support"))]
const TEST_VENUE_NAMES: &[&str] = &[
    "ETH-PERPETUAL",
    "SOL-PERPETUAL",
    "ETH_USDC-PERPETUAL",
    "ETH_USDT-PERPETUAL",
    "BTC-20JUL26",
    "BTC-25DEC26",
    "BTC-26JUN26",
    "BTC-27MAR27",
    "ETH-26JUN26",
    "ETH-27MAR27",
    "SOL_USDC-5OCT26-90-P",
    "XRP_USDC-6NOV26-2d5-C",
];

/// A `get_instrument` row of the class the venue name `name` spells (`X-PERPETUAL` coin-margined, `X_USDC-PERPETUAL`
/// linear in USDC, `X-DDMMMYY` a dated future delivered that day at 08:00 UTC, `X-DDMMMYY-STRIKE-C` an option),
/// parsed by the catalog's own parser. `None` for a name of none of those forms.
#[cfg(any(test, feature = "test-support"))]
fn test_row(name: &str) -> Option<vike_model::Instrument> {
    let (head, rest) = name.split_once('-')?;
    let (base, settle) =
        head.split_once('_').map_or((head, None), |(base, coin)| (base, Some(coin)));
    let (margining, settlement, quote) =
        settle.map_or(("reversed", base, "USD"), |coin| ("linear", coin, coin));
    let mut row = serde_json::json!({
        "instrument_name": name, "base_currency": base, "quote_currency": quote, "counter_currency": quote,
        "instrument_type": margining, "settlement_currency": settlement, "is_active": true,
        "tick_size": 0.5, "min_trade_amount": 1.0, "contract_size": 1.0, "settlement_period": "month",
    });
    let delivered = |day: &str| {
        vike_venues::deribit_expiry(day).map(|(year, month, day)| {
            vike_utility::calendar::days_from_civil(year, month, day) * vike_model::MS_PER_DAY
                + 8 * vike_model::MS_PER_HOUR
        })
    };
    match rest.split('-').collect::<Vec<_>>().as_slice() {
        ["PERPETUAL"] => {
            row["kind"] = "future".into();
            row["settlement_period"] = "perpetual".into();
        }
        [day] => {
            row["kind"] = "future".into();
            row["expiration_timestamp"] = delivered(day)?.into();
        }
        [day, strike, right] => {
            row["kind"] = "option".into();
            row["expiration_timestamp"] = delivered(day)?.into();
            row["strike"] = strike.replace('d', ".").parse::<f64>().ok()?.into();
            row["option_type"] = match *right {
                "C" => "call",
                "P" => "put",
                _ => return None,
            }
            .into();
        }
        _ => return None,
    }
    crate::catalog::parse_instruments(&serde_json::json!({ "result": [row] })).pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user name and its venue name round trip through the kept row (the recorded deribit rows of the bridge's
    /// committed listing); a name with no row is refused, and a venue name no row holds is booked as it came.
    #[test]
    fn a_user_name_and_its_venue_name_round_trip_through_the_kept_row() {
        hold_deribit_kept_rows();
        for (user, venue) in [("BTCUSD.P", "BTC-PERPETUAL"), ("BTCUSDC.P", "BTC_USDC-PERPETUAL")] {
            assert_eq!(venue_name_of(user).as_deref(), Ok(venue), "{user}");
            assert_eq!(venue_name_of(&user.to_ascii_lowercase()).as_deref(), Ok(venue), "{user}");
            assert_eq!(user_name_of(venue), user, "{venue}");
        }
        let why = venue_name_of("BTC-PERPETUAL").expect_err("the venue name is no user name");
        assert_eq!(why.id(), crate::refusals::DeribitNameRefusalId::NotKept, "{why}");
        assert_eq!(user_name_of("NOPE-PERPETUAL"), "NOPE-PERPETUAL");
    }
}
