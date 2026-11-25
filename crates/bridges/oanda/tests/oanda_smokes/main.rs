//! **The OANDA demo smokes that wait on an ingest lane, ONE test binary** (`--test oanda_smokes`), so
//! `ingest_wait` compiles ONCE instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-oanda --test oanda_smokes -- --ignored --nocapture oanda_demo_smoke::` (each test's
//! name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation". Each member keeps its own file, module doc and tests.
//!
//! ⚠ **Plain `mod` lines only, never a path attribute or an include macro** (in any member too):
//! `.github/workflows/live-smokes.yml` runs `oanda_demo_smoke` from this binary, and
//! `scripts/refuse_live_credentials.sh` scans EVERY file the binary it names compiles, following plain `mod`
//! declarations from this file; it refuses one it cannot follow, and refuses the whole lane if any member
//! asks for a live tier. A new member is scanned by the lane too.

mod ingest_wait;

mod oanda_demo_smoke;
mod oanda_reduce_only_smoke;
mod oanda_restore_smoke;
mod oanda_unit_semantics_smoke;

/// **Hold the demo account's own instrument list as this process's oanda kept list** (`vike_catalog::hold_kept_list`):
/// what the bridge reads a user name's wire name from (decision 0290: an order on `EURUSD` goes out as `EUR_USD`),
/// read here as a running node's symbol facts read it for the datahub to keep. A member calls it once its account's
/// credentials are known.
fn hold_the_accounts_list(
    creds: &vike_secrets::account_credentials::AccountCredentials<'_>,
    urls: &vike_venues::venue_urls::VenueUrls,
) {
    use vike_catalog::CatalogProvider;
    let catalog = vike_oanda::CatalogProviderOanda::new(creds, urls).expect("the seed's demo rows");
    let rows = catalog.list_instruments().expect("the account's instrument list");
    vike_catalog::hold_kept_list("oanda", rows);
}

/// The USER name of the venue instrument `wire` (`XAU_USD` is `XAUUSD`) on the list [`hold_the_accounts_list`] held.
fn user_name_of(wire: &str) -> String {
    let rows = vike_catalog::kept_list("oanda").expect("the account's list is held");
    let row = rows
        .iter()
        .find(|row| row.raw_symbol == wire)
        .unwrap_or_else(|| panic!("{wire} is not in the account's list"));
    vike_catalog::user_name_of_row(row).unwrap_or_else(|why| panic!("{why}"))
}
