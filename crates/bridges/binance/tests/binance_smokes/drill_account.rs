//! The account the binance demo DRILLS trade (the classes, classes-exec and modes drills): the
//! store's default binance account at tier `demo`, refused when the owner's fence names its id.
//! The `binance_smokes` binary declares it ONCE by a plain `mod drill_account;` and each drill reads
//! it as `crate::drill_account`; it reads the credential store, so it stays a test file and never
//! moves into a library.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::{Credentials, api_credentials};

/// The owner's fence: these account ids are never driven by a drill, whatever their tier says.
const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];

/// The resolved demo account, or `None` (with a `SKIP:` line) when it is absent or fenced off.
pub fn account() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        println!("SKIP: BINANCE_DEMO creds absent");
        return None;
    };
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("DRILL account id {id:?} (the default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return None;
    }
    Some(creds)
}
