//! The bybit DEMO account's label, the one rule the classes probe, the modes smoke and the restore
//! smoke resolve their account by. The `bybit_smokes` binary (`tests/bybit_smokes/main.rs`) declares it
//! ONCE, a plain `mod demo_label;`, and each reads it as `crate::demo_label`. It names
//! `vike_secrets::account_directory`, which `scripts/refuse_live_credentials.sh` refuses in a lane
//! target, so no lane smoke may include it, nor join this group.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// The label of the bybit DEMO account a smoke trades: the unlabelled one when the store has it
/// (every box the lane docs describe), else the ONE active labelled `demo` row (the CI box's is labelled
/// `DEMO`, and asking for the unlabelled one there skipped silently and read green). Two labelled demo
/// rows are a guess, so they fall back to the default and the smoke skips. Only the `demo` tier is ever
/// asked for, so a `live` row is never read, with `VIKE_NO_LIVE_KEYS` up or not.
pub fn demo_label(directory: &vike_secrets::account_directory::AccountDirectory) -> AccountLabel {
    if directory.account_row("bybit", &AccountLabel::Default, AccountTier::Demo).is_some() {
        return AccountLabel::Default;
    }
    let rows = directory.rows().and_then(Result::ok).and_then(|a| a.known()).unwrap_or_default();
    let mut labels = rows
        .iter()
        .filter(|r| r.active && r.venue == "bybit" && r.tier == AccountTier::Demo)
        .filter_map(|r| r.label.clone());
    match (labels.next(), labels.next()) {
        (Some(label), None) => AccountLabel::Named(label),
        _ => AccountLabel::Default,
    }
}
