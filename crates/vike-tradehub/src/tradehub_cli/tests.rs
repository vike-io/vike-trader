//! The unit tests of `tradehub_cli`, one child per concern; `NullSink`, `wired_1m_cfg`,
//! `assert_wired_and_advertised` and the credential fixtures (`fields`, `creds`, `policy_holding`)
//! are shared by several.

use super::*;
use crate::hub_server::{account_admin_source, keys_for_account_capability};
use crate::summary::summary_line;
use crate::venue_arming::deadman::DeadManActionToCore;
use crate::venue_arming::deadman_absent_warning;
use vike_exec::CoreSnapshot;
use vike_hub_proto::auth::Scope;
use vike_secrets::account_credentials::{AccountCredentials, CredentialBook, CredentialFields};

#[cfg(test)]
mod accepted_argv;
#[cfg(test)]
mod admin_barrier;
#[cfg(test)]
mod alpaca_ctrader;
#[cfg(test)]
mod args;
#[cfg(test)]
mod cex_arm;
#[cfg(test)]
mod deadman;
#[cfg(test)]
mod deribit;
#[cfg(test)]
mod exec_badge;
#[cfg(test)]
mod feed_splice;
#[cfg(test)]
mod ig;
#[cfg(test)]
mod oanda;
#[cfg(test)]
mod paths_and_alerts;
#[cfg(test)]
mod precedence;
#[cfg(test)]
mod ready_banner;
#[cfg(test)]
mod settings_and_reconcile;
#[cfg(test)]
mod stop_and_deadlines;
#[cfg(test)]
mod summary_line;

/// A `LiveDataSink` that discards everything — enough to construct a real venue `Feeds`, which
/// connects nothing until `subscribe_*` is called, so the tests below build genuine feed objects
/// without touching the network.
struct NullSink;

impl LiveDataSink for NullSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: vike_marketdata::QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_marketdata::L2Book>) {}
}

/// A 1m `MakerConfig` for `venue` on the symbol `assemble_runtime` actually mounts it on, at the
/// venue's own `tick_size` and a `qty` — the plan input every per-venue feed-arm child builds.
fn wired_1m_cfg(venue: &str, tick_size: f64, qty: f64) -> MakerConfig {
    let symbol = wired_symbol(venue).unwrap_or_else(|| panic!("assemble_runtime mounts {venue}"));
    let mut cfg = MakerConfig::crypto(venue, symbol, tick_size, qty);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// One owner's credential FIELDS, by field word — a credential belongs to its ACCOUNT and is read
/// by field, never by a composed name (decision 0156 item 6). The words are the venue's row of
/// `crates/vike-venues/src/credential_schema.rs`'s `VENUE_KEY_SCHEMAS`.
fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

/// The view a plan or a loader is handed: `account`'s fields as account row 1, and the venue's
/// application fields (`venue` — cTrader's OAuth client) when it has any.
fn creds<'a>(
    account: &'a CredentialFields,
    venue: Option<&'a CredentialFields>,
) -> AccountCredentials<'a> {
    AccountCredentials::new(Some(1), Some(account), venue)
}

/// A mount policy planting ONE active, unlabelled `venue` account row at `tier` (row id 1) that
/// holds `pairs` — the `account` snapshot the REAL mount path resolves that account's credentials
/// out of (`AccountDirectory::credentials_for`). Rows first, credentials after:
/// `AdapterPolicy::with_account` rebuilds the directory and would drop a book planted before it.
fn policy_holding(
    venue: &str,
    tier: vike_model::AccountTier,
    pairs: &[(&str, &str)],
) -> vike_tradehub_core::AdapterPolicy {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        tier,
    );
    let mut book = CredentialBook::new();
    for (field, value) in pairs {
        book.insert_account(1, *field, *value);
    }
    vike_tradehub_core::AdapterPolicy {
        accounts: policy.accounts.clone().with_credentials(book),
        ..policy
    }
}

/// `slug` is joined to the exec plane the way every live-wired slug is: an engine row in
/// `vike_tradehub_core::wired_markets` AND a `FEED_ARMS` row.
fn assert_wired_and_advertised(slug: &str) {
    assert!(
        wired_symbol(slug).is_some(),
        "{slug} has a live feed arm but `assemble_runtime` builds no engine for it"
    );
    assert!(crate::feeds::has_feed_arm(slug), "{slug} has no FEED_ARMS row");
}
