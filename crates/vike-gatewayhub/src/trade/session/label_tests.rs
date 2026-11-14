//! **A session whose label the grammar refuses borrows no other account's identity** (`AccountSession::label`):
//! its live lock is refused rather than taken as the default account's, and an order naming the default account
//! conflicts with it. [`AccountSessions::open_at_row`] refuses such a label before a session exists, so the session is
//! built directly here: this is the second fence.

use std::sync::Arc;

use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, OrderRequest};

use super::{AccountSessions, TradeLeaseOpenRequest};
use crate::budget::ledger::Budget;
use crate::testkit::trade::TradeLaneScript;
use crate::testkit::trade_rig::hub_config;

fn request(account: Option<&str>) -> TradeLeaseOpenRequest {
    TradeLeaseOpenRequest {
        client_id: "tradehub-1".to_string(),
        venue: "binance".to_string(),
        account: account.map(str::to_string),
        tier: AccountTier::Live,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

#[test]
fn a_label_that_does_not_parse_takes_no_lock_and_is_never_the_default_account() {
    let lane = TradeLaneScript::new(&["binance"]);
    let hub = AccountSessions::new(Arc::new(lane), hub_config(), Arc::new(Budget::new()));

    let bad = hub.session_for(&request(Some("not a label")));
    let why = bad.lock_key().expect_err("no live lock for a label that does not parse");
    assert!(why.contains("a bad account label"), "{why}");
    let names = |account: Option<AccountLabel>| OrderRequest {
        venue: "binance".to_string(),
        account,
        ..Default::default()
    };
    assert!(
        bad.identity_conflict(&names(Some(AccountLabel::Default))).is_some(),
        "an order for the default account is not this session's"
    );
    assert!(bad.identity_conflict(&names(None)).is_none(), "the lease is the routing");

    // the accounts that parse keep their own keys
    assert_eq!(hub.session_for(&request(None)).lock_key().as_deref(), Ok("binance"));
    let alt = hub.session_for(&request(Some("ALT")));
    assert_eq!(alt.lock_key().as_deref(), Ok("binance#ALT"));
    assert!(alt.identity_conflict(&names(Some(AccountLabel::Named("ALT".into())))).is_none());
    assert!(alt.identity_conflict(&names(Some(AccountLabel::Default))).is_some());
}
