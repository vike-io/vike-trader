//! The lane-twin table (the rows it DERIVES are `crates/vike-secrets/src/db/accounts/lane_twins_tests.rs`').
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_twins_shape_is_its_label_its_lane_and_an_open_tier`] | a twin judged by its label alone, or by a symbol off its lane, or at a tier the table does not open |
//! | [`the_table_opens_binance_at_demo_and_live_on_a_valid_perp_lane`] | a paper tier, a second venue or a label the grammar refuses opened by an edit nobody meant; the owner's `live` opening lost; a picker name that does not start with the label |

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::*;

/// **The one question every reader asks of an engine**: the table's label and a symbol on the twin's LANE at a tier
/// the table opens (the symbol itself is the parent row's, owner 2026-11-13). A block that does not say its tier is no
/// twin, a venue spelled inside the key is no label, and a label on the spot lane is an operator's account.
#[test]
fn the_twins_shape_is_its_label_its_lane_and_an_open_tier() {
    let twin = lane_twin("binance").expect("the row");
    assert_eq!(twin.route_key(), "binance#PERP");
    let shaped = |key, symbol, tier| twin.is_shaped(key, symbol, tier);
    for tier in [AccountTier::Demo, AccountTier::Live] {
        assert!(shaped("binance#PERP", "BTCUSDT.P", Some(tier)), "{tier:?}");
        assert!(shaped("binance#PERP", "ETHUSDT.P", Some(tier)), "any symbol on the lane");
        assert!(shaped("binance#PERP", "BTCUSDT_261225", Some(tier)), "a dated contract's lane");
        assert!(!shaped("binance#ALT", "BTCUSDT.P", Some(tier)), "another label");
        assert!(!shaped("binance", "BTCUSDT.P", Some(tier)), "the parent");
        assert!(!shaped("binanceus#PERP", "BTCUSDT.P", Some(tier)), "another venue's key");
        assert!(!shaped("binance#PERP", "BTCUSDT", Some(tier)), "the spot lane");
        assert!(!shaped("binance#PERP", ".P", Some(tier)), "a lane suffix is no symbol");
    }
    assert!(!shaped("binance#PERP", "BTCUSDT.P", Some(AccountTier::Paper)), "a tier not opened");
    assert!(!shaped("binance#PERP", "BTCUSDT.P", None), "a tier not said");
}

#[test]
fn the_table_opens_binance_at_demo_and_live_on_a_valid_perp_lane() {
    assert_eq!(LANE_TWINS.len(), 1, "one venue has two lanes under one row today");
    let binance = lane_twin("binance").expect("listed");
    assert_eq!(
        binance.tiers,
        &[AccountTier::Demo, AccountTier::Live],
        "demo, and live (the owner's ruling, 2026-10-19); a paper row has no venue to run a lane on"
    );
    assert_eq!(
        binance.lane, ".P",
        "the twin trades the perpetual lane, and the table names no symbol"
    );
    assert!(binance.on_lane("BTCUSDT.P") && !binance.on_lane("BTCUSDT") && !binance.on_lane(".P"));
    assert!(binance.on_lane("BTCUSDT_261225"), "a dated contract is on the futures lane too");
    assert!(!binance.on_lane("_261225") && !binance.on_lane("BTCUSDT_2612"), "no pair, or no day");
    assert!(
        AccountLabel::parse(binance.label).is_ok(),
        "the label is a label the grammar accepts, or no route key could carry it"
    );
    assert!(vike_model::VENUES.contains(&binance.venue));
    for other in ["bybit", "okx", "aster", "hyperliquid", "deribit", "oanda", ""] {
        assert!(lane_twin(other).is_none(), "{other:?} has no second lane under one row");
    }
    assert!(is_lane_twin_label("binance", "PERP") && !is_lane_twin_label("binance", "ALT"));
    assert!(!is_lane_twin_label("bybit", "PERP"), "only a listed venue's label is the twin");
    for twin in LANE_TWINS {
        assert!(twin.picker_name.starts_with(twin.label), "{}: {:?}", twin.venue, twin.picker_name);
    }
    assert_eq!(binance.picker_name, "PERP (perp lane)", "the menu's words, unchanged by the move");
}
