//! The one-lane refusal both venues' factories ask. The goldens are the words each venue's own copy
//! (`binance::exec`'s and `aster::exec`'s private `lane_symbol`, before they became this one) produced
//! for the same input: only the venue's name differs.

use super::*;

/// An engine runs ONE lane: an order on the other lane is refused with a reason naming both, a
/// same-lane order yields its upper-cased exchange symbol (the `.P` stripped on a perp).
#[test]
fn an_order_on_the_other_lane_is_refused_not_signed() {
    for venue in ["binance", "aster"] {
        assert_eq!(lane_symbol(venue, "ETHUSDT.P", true), Ok("ETHUSDT".to_string()));
        assert_eq!(lane_symbol(venue, "ethusdt", false), Ok("ETHUSDT".to_string()));
    }
}

#[test]
fn each_venue_words_the_refusal_as_its_own_copy_did() {
    assert_eq!(
        lane_symbol("binance", "ETHUSDT", true),
        Err("binance: cannot sign \"ETHUSDT\": this account's engine runs the perp lane and \
             \"ETHUSDT\" is a spot symbol; the spot lane has no user-data stream here, so its fills \
             would never arrive. The order was not sent"
            .to_string())
    );
    assert_eq!(
        lane_symbol("aster", "ETHUSDT.P", false),
        Err("aster: cannot sign \"ETHUSDT.P\": this account's engine runs the spot lane and \
             \"ETHUSDT.P\" is a perp symbol; the perp lane has no user-data stream here, so its \
             fills would never arrive. The order was not sent"
            .to_string())
    );
}
