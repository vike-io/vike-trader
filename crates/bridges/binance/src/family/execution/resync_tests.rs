//! Which foreign labels each lane's replay reads ([`on_lane`], the `admit` both venues' replays hand
//! `vike_bridge_core::execution_client::resync::replay_foreign`).

use super::*;
use vike_bridge_core::execution_client::resync::replay_foreign;

/// The foreign labels `replay_foreign` reads on the perp lane (`perp`) or the spot lane, `on_lane` as its admit, in
/// the order it reads them.
fn read_on(perp: bool, foreign: &[&str]) -> Vec<String> {
    let foreign: Vec<String> = foreign.iter().map(|label| (*label).to_string()).collect();
    let mut read = Vec::new();
    replay_foreign(
        "binance",
        "SOLUSDT.P",
        &foreign,
        |label| on_lane(label, perp),
        |_| false,
        |_| Vec::new(),
        |label, _| {
            read.push(label.to_string());
            Vec::new()
        },
    );
    read
}

/// **A dated contract is replayed on the PERP engine and never on the spot one** (decision 0290: its user name
/// `BTCUSDT_261225` carries no `.P`, and no spot book lists it): read on the futures lane under its exchange symbol, as
/// binance's own lane name `BTCUSDT_261225.P` is; a perpetual stays the perp lane's and a spot pair the spot lane's,
/// and a tail that is no delivery day is no dated contract.
#[test]
fn a_dated_label_is_replayed_on_the_perp_engine_and_not_on_the_spot_one() {
    let foreign = ["BTCUSDT_261225", "BTCUSDT_261225.P", "ETHUSDT.P", "ETHUSDT", "BTCUSDT_261399"];
    assert_eq!(read_on(true, &foreign), ["BTCUSDT_261225", "BTCUSDT_261225.P", "ETHUSDT.P"]);
    assert_eq!(read_on(false, &foreign), ["ETHUSDT", "BTCUSDT_261399"]);
    for label in ["BTCUSDT_261225", "BTCUSDT_261225.P"] {
        assert_eq!(vike_venues::split_perp(label).0, "BTCUSDT_261225", "the fapi symbol read");
    }
}
