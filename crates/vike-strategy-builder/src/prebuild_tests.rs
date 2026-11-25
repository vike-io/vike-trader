//! `prebuild`'s choice of files: the `.rs` mounts, each once, but a mount on an account stored inactive.

use vike_model::AccountTier;
use vike_secrets::AccountRows;
use vike_secrets::profile_store::MountRow;

use super::rust_files;

fn mount(ord: i64, venue: &str, account: Option<&str>, file: Option<&str>) -> MountRow {
    MountRow {
        ord,
        is_primary: ord == 0,
        venue: venue.to_string(),
        asset_class: "CryptoPerp".to_string(),
        symbol: Some("BTCUSDT.P".to_string()),
        interval: None,
        interval_ms: None,
        resolution_ts_ms: None,
        qty: None,
        half_spread: None,
        tick_size: None,
        seed_cash: None,
        data_only: None,
        account: account.map(str::to_string),
        strategy_file: file.map(str::to_string),
        max_leverage: None,
    }
}

#[test]
fn the_rust_mounts_each_once_and_none_on_an_inactive_account() {
    let mounts = [
        mount(0, "bybit", Some("main"), Some("market_making/spread_maker.rs")),
        mount(1, "bybit", None, Some("trend/momentum.rhai")),
        mount(2, "binance", Some("off"), Some("trend/breakout.rs")),
        mount(3, "bybit", Some("main"), Some("market_making/spread_maker.rs")),
        mount(4, "okx", None, Some("trend/paper_only.rs")),
        mount(5, "okx", None, None),
    ];
    let accounts = AccountRows::Known(vec![
        vike_secrets::account_row(1, "bybit", AccountTier::Demo, Some("main"), true),
        vike_secrets::account_row(2, "binance", AccountTier::Live, Some("off"), false),
    ]);
    assert_eq!(
        rust_files(&mounts, Some(&accounts)),
        ["market_making/spread_maker.rs", "trend/paper_only.rs"],
        "the .rhai mount, the inactive account's mount and the repeat are skipped; a mount with no account row is built"
    );
    assert_eq!(
        rust_files(&mounts, None),
        ["market_making/spread_maker.rs", "trend/breakout.rs", "trend/paper_only.rs"],
        "a store that cannot say which accounts are active builds every .rs mount"
    );
    assert!(rust_files(&[], Some(&accounts)).is_empty(), "no mounts, nothing to build");
}
