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
/// absent file is no list, an unparsable or unreadable one is no list, and [`kept_list`] answers the installed folder's rows
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
    // A file that exists and cannot be read (a directory stands in: a lane may run as root) is no list either, and a
    // reader leaves it where it is.
    std::fs::create_dir(kept_file(dir.path(), "deribit")).unwrap();
    assert_eq!(read_kept_list(dir.path(), "deribit"), None, "an unreadable file is no list");
    assert!(kept_file(dir.path(), "deribit").is_dir(), "not moved by a reader");
    assert_eq!(read_kept_lists(dir.path()), vec![bybit.clone()]);

    use_kept_lists(dir.path().to_path_buf());
    let rows = kept_list("bybit").expect("the installed folder's list");
    assert_eq!(rows.as_slice(), bybit.instruments.as_slice());
    assert!(kept_list("binance").is_none(), "a venue with no file");

    // A refresh inside the reread window: the process's read still answers the old rows, a read NOW the new ones
    // (what an account's start asked again after the refresh reads), and the process's read then holds them too.
    let refreshed = list("bybit", &["BTCUSDT.P", "BTCUSD.P", "ETHUSDT.P"]);
    std::fs::write(kept_file(dir.path(), "bybit"), serde_json::to_vec(&refreshed).unwrap())
        .unwrap();
    assert_eq!(kept_list("bybit").unwrap().len(), 2, "inside the window: the last read");
    assert_eq!(kept_list_now("bybit").unwrap().as_slice(), refreshed.instruments.as_slice());
    assert_eq!(kept_list("bybit").unwrap().len(), 3, "the read now replaced the last read");
}

/// **The process's read answers the STORED names, off the maps it builds once per read** (the owner's ruling «Б»,
/// 2026-11-16): a held row whose stored user name is not the grammar's answers the stored one both ways, and the
/// grammar's name names nothing.
#[test]
fn the_process_read_answers_the_stored_names_off_its_maps() {
    let mut row = test_row("aster", "ETHUSDT.P", "ETH", "USDT", AssetClass::CryptoPerp);
    row.naming = Some(vike_model::InstrumentNaming::User("STOREDETH.P".into()));
    hold_kept_list("aster", vec![row]);
    assert_eq!(
        kept_row_of("aster", " storedeth.p ").map(|r| r.raw_symbol),
        Some("ETHUSDT.P".into())
    );
    assert!(kept_row_of("aster", "ETHUSDT.P").is_none(), "the grammar's name names nothing");
    assert_eq!(kept_raw_of_user_name("aster", "STOREDETH.P").as_deref(), Ok("ETHUSDT.P"));
    assert_eq!(kept_user_name_of_raw("aster", "ETHUSDT.P").as_deref(), Some("STOREDETH.P"));
}
