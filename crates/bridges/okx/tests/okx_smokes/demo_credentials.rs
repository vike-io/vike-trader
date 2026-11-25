//! The OKX live smokes' one way to the DEMO credentials: the fields of the DEFAULT okx account at
//! tier `demo`, resolved through `vike_secrets::smoke_directory`, so no other account's key can
//! reach a smoke that uses it. The `okx_smokes` binary (`main.rs` beside it) declares it ONCE by a
//! plain `mod demo_credentials;` (the only shape `scripts/refuse_live_credentials.sh` follows into a lane
//! target) and each member reads it as `crate::demo_credentials`; it reads the
//! credential store, so it stays a test file and never moves into a library.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::{Credentials, api_credentials};

/// The double-gate every smoke uses: the store's default DEMO account, or `None` after a
/// `tracing::warn!`, so the caller self-skips with `let Some(..) = .. else { return };`.
///
/// It first holds the crate's kept okx list in this process (`vike_okx::instrument::kept_list_for_tests`): every
/// member names an instrument to the engine side by its USER name (decision 0290: `BTCUSDT.P` is `BTC-USDT-SWAP`),
/// which the spawn, the reconcile client's admissions and its reports translate through a kept row.
pub fn load_demo_credentials() -> Option<Credentials> {
    vike_okx::instrument::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}
