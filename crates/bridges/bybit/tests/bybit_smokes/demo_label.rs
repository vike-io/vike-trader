//! The bybit DEMO account's label, the one rule the classes probe, the modes smoke and the restore
//! smoke resolve their account by. The `bybit_smokes` binary (`tests/bybit_smokes/main.rs`) declares it
//! ONCE, a plain `mod demo_label;`, and each reads it as `crate::demo_label`. It names
//! `vike_secrets::account_directory`, which `scripts/refuse_live_credentials.sh` refuses in a lane
//! target, so no lane smoke may include it, nor join this group.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// The label the CI box gives its bybit demo row, asked for BY NAME.
const DEMO_ROW_LABEL: &str = "DEMO";

/// The label of the bybit DEMO account a smoke trades: the unlabelled one when the store has it
/// (every box the lane docs describe), else the active `demo` row labelled `DEMO` (the CI box's). It is
/// asked for by name, never as "the one labelled demo row": the CI box also holds `INV`, a second
/// labelled demo row on the same keys for the inverse engine, and counting labelled rows made every
/// smoke here skip silently and read green. No such row falls back to the default, and the smoke
/// skips. Only the `demo` tier is ever asked for, so a `live` row is never read, with
/// `VIKE_NO_LIVE_KEYS` up or not.
pub fn demo_label(directory: &vike_secrets::account_directory::AccountDirectory) -> AccountLabel {
    if directory.account_row("bybit", &AccountLabel::Default, AccountTier::Demo).is_some() {
        return AccountLabel::Default;
    }
    let named = AccountLabel::Named(DEMO_ROW_LABEL.to_string());
    if directory.account_row("bybit", &named, AccountTier::Demo).is_some() {
        return named;
    }
    AccountLabel::Default
}

#[cfg(test)]
mod tests {
    use vike_secrets::account_directory::AccountDirectory;

    use super::*;

    fn row(id: i64, label: Option<&str>) -> vike_secrets::AccountRow {
        vike_secrets::AccountRow {
            id,
            venue: "bybit".to_string(),
            tier: AccountTier::Demo,
            label: label.map(str::to_string),
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
            symbol: vike_secrets::fixture_account_symbol("bybit").map(str::to_string),
            facts: Default::default(),
        }
    }

    fn directory(rows: Vec<vike_secrets::AccountRow>) -> AccountDirectory {
        AccountDirectory::from_rows(vike_secrets::AccountRows::Known(rows), None)
    }

    /// the CI box's shape: `DEMO` and its `INV` twin, both labelled, both demo. The smokes trade `DEMO`.
    #[test]
    fn a_second_labelled_demo_row_does_not_move_the_smokes_off_demo() {
        let both = directory(vec![row(4, Some("DEMO")), row(9, Some("INV"))]);
        assert_eq!(demo_label(&both), AccountLabel::Named("DEMO".to_string()));
        let reversed = directory(vec![row(9, Some("INV")), row(4, Some("DEMO"))]);
        assert_eq!(demo_label(&reversed), AccountLabel::Named("DEMO".to_string()));
    }

    /// The unlabelled row still wins where a box has one, and a box with only other labels skips.
    #[test]
    fn the_unlabelled_row_wins_and_no_demo_row_falls_back_to_the_default() {
        let default_and_demo = directory(vec![row(1, None), row(4, Some("DEMO"))]);
        assert_eq!(demo_label(&default_and_demo), AccountLabel::Default);
        let only_inverse = directory(vec![row(9, Some("INV"))]);
        assert_eq!(demo_label(&only_inverse), AccountLabel::Default);
    }
}
