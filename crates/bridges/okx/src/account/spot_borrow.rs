//! **Can a spot order this bridge sends BORROW on this account?** A LIVE start refuses, in words, an account on which
//! it can (decision 0232, amended 2026-11-23; the owner's "yes agree all! merge if ok").
//!
//! The engine books an okx spot pair as a fully funded `Cash` position (decision 0232 item 2), but the order goes out
//! as `tdMode` `cross` and is sent as `cash` only on the level's `51000` tdMode refusal
//! (`crate::execution::rest::VenueRestOkx`'s `post_order_params`), so wherever the venue takes `cross` the order is a
//! margin-capable one and the venue may lend what the balance lacks. The configuration is `data[0]` of
//! `GET /api/v5/account/config`, the SAME answer the withdraw gate's read returns
//! ([`crate::account::identity::fetch_account_config`]): no second request. A demo or paper start reads nothing and is
//! never refused here.
//!
//! | `acctLv` (OKX's name) | a spot order this bridge sends can borrow when | source |
//! |---|---|---|
//! | `1` (Spot mode) | `enableSpotBorrow` is true | docs: `enableSpotBorrow` "Whether borrow is allowed or not in Spot mode", and the borrow history's event type `auto_borrow`. ⚠ The docs do not say whether an ORDER borrows by itself, so on is refused (the safe reading) |
//! | `2` (Futures mode) | always | docs, the place-order `tdMode` by account mode: Futures mode "Cross MARGIN: cross", "SPOT: cash", so the `cross` sent first is a MARGIN order; OKX's help page "Comparing futures mode and multi-currency margin mode": "the system will borrow funds for you to purchase cryptocurrency. As a result, a margin position will be created". No setting turns it off (`set-auto-loan` is "Only applicable to Multi-currency margin and Portfolio margin") |
//! | `3` (Multi-currency margin) | `autoLoan` is true | docs: `tdMode` "Cross SPOT: cross"; `autoLoan` "Whether to borrow coins automatically". MEASURED with it off on the demo account (2026-10-30, `tests/okx_classes_measure_smoke.rs`'s `okx_classes_round3`): a sell of 5 BTC against about 1 was refused `51008` |
//! | `4` (Portfolio margin) | `autoLoan` is true | docs: `tdMode` "Cross SPOT: cross", and `set-auto-loan` applies to it |
//!
//! Only level 3 is measured (the captured DEMO answer, `tests/fixtures/captured/account_config_demo.json`: `acctLv` 3,
//! both flags off); levels 1, 2 and 4 follow the docs alone. At level 2 the order body also states no `ccy`, which the
//! docs call required for a Futures-mode cross MARGIN order, so the venue may refuse it instead, and a MARGIN order's
//! rows are dropped by the order mapper (`crate::execution::event_mapper`): neither is the fully funded order the
//! engine books, so the level is refused either way. A flag the level does not name (`autoLoan` at level 1,
//! `enableSpotBorrow` at 2, 3 or 4) is not read.
//!
//! ⚠ **An ANSWER that does not show the account cannot borrow is refused**: no `acctLv`, a level not in the table, or
//! no true/false flag where the level needs one. The docs say nothing of such an answer, so the safe reading holds.
//! ⚠ **A failed READ follows the withdraw gate's posture** (`vike_secrets::key_permissions::withdraw_gate_before_start`):
//! no answer is Unknown, and Unknown ARMS (fail-open), warned once here beside the withdraw gate's own warning. There is
//! no override flag: the cure is the account's own setting at OKX.

use serde_json::Value;
use vike_bridge_core::json::json_number;

use crate::execution::rest::VENUE;

/// Futures mode's refusal: there the `cross` spot order is a MARGIN order (the module doc's table).
const FUTURES_MODE: &str = "the account is in Futures mode (acctLv 2), where the `cross` spot \
    order this bridge sends is a MARGIN order that borrows, and no setting turns that off. Switch \
    the account to Multi-currency margin with auto-borrow off in the OKX app or web (Settings, \
    Account mode)";

/// The cure for an answer that cannot say whether a spot order may borrow.
const SAFE_MODE: &str = "Use Multi-currency margin with auto-borrow off";

/// Why a spot order this bridge sends could borrow on the account `row` describes (`data[0]` of
/// `GET /api/v5/account/config`), ending in what to change at OKX; `None` when the row shows it
/// cannot. PURE.
#[must_use]
pub fn spot_borrow_refusal(row: &Value) -> Option<String> {
    let level = row.get("acctLv").and_then(json_number).map(|level| level.to_string());
    match level.as_deref() {
        Some("1") => flag_on(row, "enableSpotBorrow", "Spot mode (acctLv 1)", "spot borrowing"),
        Some("2") => Some(FUTURES_MODE.to_string()),
        Some("3") => flag_on(row, "autoLoan", "Multi-currency margin (acctLv 3)", "auto-borrow"),
        Some("4") => flag_on(row, "autoLoan", "Portfolio margin (acctLv 4)", "auto-borrow"),
        Some(other) => Some(format!(
            "account/config answers acctLv {other}, not a level this bridge knows, so whether a \
             spot order may borrow is unknown. {SAFE_MODE}"
        )),
        None => Some(format!(
            "account/config carries no acctLv, so whether a spot order may borrow is unknown. \
             {SAFE_MODE}"
        )),
    }
}

/// The refusal of an account in `mode` whose flag `name` (the `setting` in the OKX app's words) is
/// not shown false.
fn flag_on(row: &Value, name: &str, mode: &str, setting: &str) -> Option<String> {
    match row.get(name).and_then(Value::as_bool) {
        Some(false) => None,
        Some(true) => Some(format!(
            "the account is in {mode} with {setting} ON (`{name}` true), so a spot order may \
             borrow what the balance lacks while the engine books a spot pair fully funded. Turn \
             {setting} off in the OKX app or web"
        )),
        None => Some(format!(
            "the account is in {mode} and account/config carries no true/false `{name}`, so \
             whether a spot order may borrow is unknown. Make sure {setting} is off in the OKX \
             app or web"
        )),
    }
}

/// **Does this LIVE start refuse to arm?** `config` is the `account/config` answer the withdraw
/// gate's read returned, `None` when that read failed. `true` is REFUSED, logged at `error!` in
/// words naming the setting: the caller builds nothing and the account does NOT RUN, the withdraw
/// refusal's own outcome. A failed read arms, warned (the module doc's posture). The caller asks
/// on a live start only.
pub fn refuses_live_start(account: &str, config: Option<&Value>) -> bool {
    let Some(answer) = config else {
        tracing::warn!(
            venue = VENUE,
            account,
            "the account configuration was not read → whether a spot order can borrow is \
             Unknown (fail-open, the withdraw gate's posture); the spot-borrow refusal refuses \
             only a configuration it has read"
        );
        return false;
    };
    let Some(why) = spot_borrow_refusal(&answer["data"][0]) else { return false };
    tracing::error!(
        venue = VENUE,
        account,
        "⚠ REFUSING to arm {VENUE} LIVE: {why}, then restart this account's engine. The account \
         does NOT RUN (the same outcome absent credentials produce)."
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The DEMO account's captured row (`okx_key_permissions_capture`, 2026-11-22): Multi-currency
    /// margin (`acctLv` "3"), `autoLoan` false, `enableSpotBorrow` false.
    fn demo_row() -> Value {
        let text = include_str!("../../tests/fixtures/captured/account_config_demo.json");
        let file: Value = serde_json::from_str(text).expect("the captured fixture is valid JSON");
        file["frames"][0]["data"][0].clone()
    }

    /// The captured row with `changes` written over it (`Value::Null` removes the field): the
    /// configurations the demo account cannot be captured in without changing its settings.
    fn variant(changes: &[(&str, Value)]) -> Value {
        let mut row = demo_row();
        let fields = row.as_object_mut().expect("a row is an object");
        for (name, value) in changes {
            if value.is_null() {
                fields.remove(*name);
            } else {
                fields.insert((*name).to_string(), value.clone());
            }
        }
        row
    }

    /// The captured row with `changes` over it is refused, in words holding `words`.
    fn assert_refused(changes: &[(&str, Value)], words: &str) {
        let why = spot_borrow_refusal(&variant(changes)).expect("refused");
        assert!(why.contains(words), "{changes:?}: {words:?} in {why}");
    }

    /// Each borrowing configuration is refused, in words naming the setting to change.
    #[test]
    fn each_borrowing_configuration_is_refused_and_names_its_setting() {
        assert_refused(&[("autoLoan", json!(true))], "(acctLv 3) with auto-borrow ON");
        assert_refused(&[("autoLoan", json!(true))], "Turn auto-borrow off in the OKX app or web");
        assert_refused(
            &[("acctLv", json!("4")), ("autoLoan", json!(true))],
            "Portfolio margin (acctLv 4) with auto-borrow ON",
        );
        assert_refused(
            &[("acctLv", json!("1")), ("enableSpotBorrow", json!(true))],
            "Spot mode (acctLv 1) with spot borrowing ON (`enableSpotBorrow` true)",
        );
        assert_refused(&[("acctLv", json!("2"))], "Futures mode (acctLv 2)");
        assert_refused(&[("acctLv", json!(2))], "no setting turns that off");
    }

    /// The captured demo account and every configuration that cannot borrow are admitted; a flag
    /// the level does not name is not read.
    #[test]
    fn the_non_borrowing_configurations_are_admitted() {
        assert_eq!(spot_borrow_refusal(&demo_row()), None, "the captured demo account");
        for changes in [
            vec![("acctLv", json!(3))],
            vec![("acctLv", json!("4"))],
            vec![("acctLv", json!("1"))],
            vec![("enableSpotBorrow", json!(true))],
            vec![("acctLv", json!("1")), ("autoLoan", json!(true))],
        ] {
            assert_eq!(spot_borrow_refusal(&variant(&changes)), None, "{changes:?}");
        }
    }

    /// An answer that does not show the account cannot borrow is refused: no level, an unknown
    /// level, no flag where the level needs one, a flag that is not true/false, no row at all.
    #[test]
    fn an_answer_that_does_not_show_the_account_cannot_borrow_is_refused() {
        for (row, words) in [
            (variant(&[("acctLv", Value::Null)]), "carries no acctLv"),
            (variant(&[("acctLv", json!("5"))]), "acctLv 5, not a level"),
            (variant(&[("autoLoan", Value::Null)]), "no true/false `autoLoan`"),
            (variant(&[("autoLoan", json!("false"))]), "no true/false `autoLoan`"),
            (
                variant(&[("acctLv", json!("1")), ("enableSpotBorrow", Value::Null)]),
                "no true/false `enableSpotBorrow`",
            ),
            (Value::Null, "carries no acctLv"),
        ] {
            let why = spot_borrow_refusal(&row).expect("refused");
            assert!(why.contains(words), "{words:?} in {why}");
        }
    }
}
