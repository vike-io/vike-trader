use super::*;
use crate::test_support::test_row;
use vike_model::AssetClass;

fn list(venue: &str, symbols: &[&str]) -> VenueInstrumentList {
    VenueInstrumentList {
        venue: venue.into(),
        refreshed_ms: Some(1),
        truncated: false,
        last_error: None,
        last_try_ms: Some(1),
        instruments: symbols
            .iter()
            .map(|s| test_row(venue, s, "BTC", "USDT", AssetClass::CryptoPerp))
            .collect(),
        added_by_operator: Vec::new(),
    }
}

/// **One reader, one file per venue, and this process's read of them**: a written list reads back whole, an
/// absent file is no list, an unparsable one is no list, and [`kept_list`] answers the installed folder's rows
/// (a venue with no file: `None`).
#[test]
fn a_kept_list_reads_back_and_the_process_read_answers_from_the_installed_folder() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_kept_list(dir.path(), "bybit"), None, "no file is no list");
    let bybit = list("bybit", &["BTCUSDT.P", "BTCUSD.P"]);
    std::fs::write(kept_file(dir.path(), "bybit"), serde_json::to_vec(&bybit).unwrap()).unwrap();
    std::fs::write(kept_file(dir.path(), "okx"), b"not json").unwrap();
    assert_eq!(read_kept_list(dir.path(), "bybit"), Some(bybit.clone()));
    assert_eq!(read_kept_list(dir.path(), "okx"), None, "an unparsable file is no list");
    assert_eq!(read_kept_lists(dir.path()), vec![bybit.clone()]);

    use_kept_lists(dir.path().to_path_buf());
    let rows = kept_list("bybit").expect("the installed folder's list");
    assert_eq!(rows.as_slice(), bybit.instruments.as_slice());
    assert!(kept_list("binance").is_none(), "a venue with no file");
}
