//! LIVE demo READ-ONLY smoke for the SIGN of the two cTrader figures the bridge books without a measurement
//! behind them (A26): the swap a closing deal realized (`ProtoOAClosePositionDetail.swap`, booked as funding by
//! `crate::event_mapper::deal_swap_funding`) and `pnlConversionFee` (booked as a cost on the closing fill's
//! commission). It prints every CLOSING deal of the last [`WINDOW_HOURS`] hours, the account's balance after it
//! included, so the operator kit's t26 cTrader script can set each balance step beside the deal's own figures:
//!
//!     cargo test -p vike-ctrader --test ctrader_swap_sign_smoke -- --ignored --nocapture
//!
//! The lines are `ctrader_currency_smoke`'s `CTDEAL id=... gross=... swap=... comm=... balance=... convfee=...`
//! (the detail's figures as the wire states them, descaled), oldest first, plus `CTDEALS count=<n>`. A closing
//! deal whose position was held across a rollover carries a non-zero `swap`; with none in the window the script
//! says the sign is still unmeasured.
//!
//! READ-ONLY: places NO order, cancels nothing, changes nothing on the account; one reconcile socket and one
//! deal list. No credential and no account id reaches a line. Credentials: the DEFAULT demo account's, resolved
//! through `vike_secrets::smoke_directory`; absent creds -> the test SKIPS.

use vike_ctrader::CtraderReconClient;
use vike_ctrader::config::CtraderConfig;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, now_ms};

/// How far back the closing deals are listed: five days, inside the venue's one-week cap on a deal-list window
/// and long enough to hold a position that went through a few rollovers.
const WINDOW_HOURS: i64 = 120;

/// The symbol the reconcile client is opened on (any listed symbol: the deal list is account-wide).
const SYMBOL: &str = "EURUSD";

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn ctrader_swap_and_conversion_fee_signs_are_read_against_the_balance() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = CtraderConfig::from_credentials(AccountTier::Demo, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };
    let client = CtraderReconClient::connect(&config.to_conn_config(), SYMBOL)
        .expect("a dedicated reconcile socket against the real cTrader demo endpoint");
    let since = now_ms() - WINDOW_HOURS * 3_600_000;
    let deals = client.close_details(since).expect("the account's deal list");
    for deal in &deals {
        println!("{}", deal.line());
    }
    println!("CTDEALS count={} window_hours={WINDOW_HOURS}", deals.len());
}
