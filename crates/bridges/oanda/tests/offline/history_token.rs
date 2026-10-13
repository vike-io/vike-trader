//! The credential half of OANDA's HISTORY lane: how the one token is read out of an account's
//! credentials.
//!
//! The datahub reads the DEMO (practice) oanda account's `API_KEY` field;
//! `load_oanda_history_token` is that same read over any account's credentials (decision 0156 item
//! 6: a credential belongs to its account and is read by FIELD, never by a composed name).
//! Everything here is over plain fields — no store, no network, no real credential (every value is
//! an obviously fake constant).

use std::assert_matches;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_oanda::{MountableTier, load_oanda_config_from, load_oanda_history_token, mountable_tier};
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

fn creds(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// A candles read is account-free: an account holding the token and no account id serves history
/// and arms nothing — the very case the config loader (which needs both) refuses.
#[test]
fn the_token_needs_no_account_id() {
    let token_only = fields(&[("API_KEY", "tok-abc")]);
    assert_eq!(load_oanda_history_token(&creds(&token_only)).as_deref(), Some("tok-abc"));
    assert!(
        load_oanda_config_from(AccountTier::Demo, &creds(&token_only)).is_none(),
        "the control: the config loader needs the account id too, so this account arms nothing"
    );
}

#[test]
fn an_absent_or_blank_token_is_none() {
    assert_eq!(load_oanda_history_token(&AccountCredentials::none()), None);
    assert_eq!(load_oanda_history_token(&creds(&fields(&[("API_KEY", "")]))), None);
    assert_eq!(load_oanda_history_token(&creds(&fields(&[("API_KEY", "   \t ")]))), None);
    // An account id alone is not a token.
    assert_eq!(load_oanda_history_token(&creds(&fields(&[("ACCOUNT_ID", "101-004-1-001")]))), None);
}

#[test]
fn the_token_is_trimmed() {
    let token = fields(&[("API_KEY", "  tok-abc  ")]);
    assert_eq!(load_oanda_history_token(&creds(&token)).as_deref(), Some("tok-abc"));
}

/// Only the account's `API_KEY` FIELD is ever consulted: an account holding every other field, and
/// a VENUE-owned `API_KEY` (an application row, not the account's), leave nothing to read.
#[test]
fn the_loader_reads_the_accounts_api_key_and_nothing_else() {
    let others = fields(&[
        ("ACCOUNT_ID", "101-004-1-001"),
        ("API_SECRET", "not-a-token"),
        ("TOKEN", "not-the-field"),
    ]);
    assert_eq!(load_oanda_history_token(&creds(&others)), None);

    let venue_rows = fields(&[("API_KEY", "venue-tok")]);
    let venue_only = AccountCredentials::new(Some(1), None, Some(&venue_rows));
    assert_eq!(load_oanda_history_token(&venue_only), None, "a venue row is no account's token");
}

/// When both fields are configured the history loader finds the token the config loader finds — it
/// is the same field, read by the same rule, not a second opinion.
#[test]
fn it_finds_the_token_the_config_loader_finds() {
    let pair = fields(&[("API_KEY", " tok-abc "), ("ACCOUNT_ID", "101-004-1-001")]);
    let cfg =
        load_oanda_config_from(AccountTier::Demo, &creds(&pair)).expect("both fields present");
    assert_eq!(load_oanda_history_token(&creds(&pair)), Some(cfg.api_token));
}

/// **The history read is tier-blind; the mount's decision is not** — the opposite disposition from
/// `mountable_tier`, on purpose: that refusal protects ORDERS from silently trading the wrong
/// account, and history places none, so WHICH account's token to read is the caller's choice (the
/// datahub's is the practice account). The same pair on a `live` account is refused by the mount
/// and still read here, so neither can drift into the other.
#[test]
fn the_history_read_is_not_the_mounts_refusal() {
    let pair = fields(&[("API_KEY", "tok-abc"), ("ACCOUNT_ID", "001-001-1-001")]);
    assert_eq!(load_oanda_history_token(&creds(&pair)).as_deref(), Some("tok-abc"));
    assert_matches!(
        mountable_tier(&AccountLabel::Default, AccountTier::Live, &creds(&pair)),
        MountableTier::LiveUnreachable(_)
    );
}
