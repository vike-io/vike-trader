//! The Binance live smokes' one way to the DEMO credentials: the fields of the DEFAULT binance
//! account at tier `demo`, resolved through `vike_secrets::smoke_directory`, so no other account's
//! key can reach a smoke that uses it. The `binance_smokes` binary (`main.rs` beside it) declares it
//! ONCE by a plain `mod demo_credentials;` (the only shape `scripts/refuse_live_credentials.sh` follows
//! into a lane target) and each member reads it as `crate::demo_credentials`; it reads the
//! credential store, so it stays a test file and never moves into a library.

use vike_binance::execution::BinanceUrls;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::{Credentials, api_credentials};
use vike_venues::venue_urls::VenueUrls;

/// The DEMO account's exec addresses, looked up in the release's seed
/// (`crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`) exactly as a demo start looks them up
/// (`vike_binance::execution::BinanceUrls::resolve`): the hosts every smoke here dials.
pub fn demo_hosts() -> BinanceUrls {
    BinanceUrls::resolve(&VenueUrls::seed("binance"), AccountTier::Demo)
        .expect("the seed holds every binance demo row")
}

/// The double-gate every smoke uses: the store's default DEMO account, or `None` after a
/// `tracing::warn!`, so the caller self-skips with `let Some(..) = .. else { return };`.
pub fn load_demo_credentials() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "binance");
    if creds.is_none() {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

/// [`load_demo_credentials`], and the absence also said on stdout, for a smoke whose skip must be SEEN: the
/// fine-tick smokes, enrolled in the order-lifecycle job whose skip-honesty step greps for `SKIP:`.
///
/// ⚠ The `println!` is not decoration and not a duplicate: this skip's whole visibility rests on
/// one line of output, and `vike_log::test_init` builds its filter from `RUST_LOG`/`VIKE_LOG` — so
/// under `RUST_LOG=error` the `tracing::warn!` alone is dropped and an unconfigured box reads as a
/// green proof.
pub fn load_demo_credentials_loudly() -> Option<Credentials> {
    let creds = load_demo_credentials();
    if creds.is_none() {
        println!("SKIP: BINANCE_DEMO creds absent — nothing was measured");
    }
    creds
}
