//! **The cTrader history lane's grant read, its presence probe, its startup line and the
//! connection-scoped memo the read lives in** — the second credentialed kline lane beside
//! [`super::oanda`], with the same discipline and one difference of kind.
//!
//! The credential is an ACCOUNT's (decision 0156 item 6): the OAuth token pair (`ACCESS_TOKEN`,
//! `REFRESH_TOKEN`) of the ONE active unlabelled ctrader account at tier `demo`, plus the venue's
//! application pair (`CLIENT_ID`, `CLIENT_SECRET`, written once for every account), found by the account
//! ROW's address and read through [`crate::data::accounts::AccountSource`] (the store handle the boot
//! resolved, narrowed to `NoLiveKeys`: a live key has no business in this call even from a mis-tiered
//! row). No credential NAME is composed here and no tier is read out of a name. The host is the DEMO host
//! (`CtraderConfig::from_credentials` at `Demo`); a `live` account's grant is never read and a `paper`
//! row is not the feed's stand-in here (the feed's `data_feed_credentials` falls back to one, this lane
//! deliberately does not: a history fetch is not a strategy's feed).
//!
//! # What the lane does with it: its OWN read-only session
//!
//! `vike_ctrader::klines::CtraderKlines` opens a DEDICATED authenticated socket under that grant and
//! sends nothing but trendbar windows over it. It is not the trading socket and shares nothing with
//! it but the process's `SharedGrant` cell: the session FOLLOWS the cell's current pair and never spends
//! a refresh token, so the single writer of the rotated pair stays the exec or data socket of the account
//! (`crates/bridges/ctrader/CLAUDE.md`'s "The OAuth token loop is CLOSED, and the credential store is the
//! ONE home", rung 4). The argument for a socket of its own
//! is on `crates/bridges/ctrader/src/klines.rs`'s module doc: the actor's trendbar command has no reply
//! path and its answer replaces a running strategy's series, and the venue meters historical requests per
//! connection, so a socket of its own cannot slow an order. The datahub holds no key and dials nothing:
//! it asks this daemon through `Request::FetchCredentialed` (the Write scope), and no Observe verb
//! reaches the lane but the presence word.
//!
//! # Read once per CONNECTION, which is once per REQUEST
//!
//! As for OANDA (decision 0097, verdict 2, re-homed by 0164): the connection config is built from
//! the store when the source's first session opens, kept in the connection's [`ConnectionScope`] slot for
//! the rest of that request, and dropped with the connection. A session reopened after a drop asks the
//! memo, not the store, and the config's own `sync_grant` picks up a pair a sibling rotated meanwhile.
//! A store that exists and will not read is `HistoryConfigError::StoreUnreadable`, never "absent".
//!
//! ⚠ **The presence word asks about the TOKEN PAIR only.** The store has no boolean-only read of a
//! venue-owned field, so the application pair cannot be asked about without bringing its values into this
//! process, which decision 0102 forbids on an Observe path. `Present` therefore says the demo account's
//! `ACCESS_TOKEN` and `REFRESH_TOKEN` are stored; a missing application pair is a request-time refusal
//! that names it (`HistoryConfigError::NotConfigured`).

use std::path::Path;
use std::sync::{Arc, OnceLock};

use vike_ctrader::config::{ACCESS_TOKEN, CtraderConfig, REFRESH_TOKEN};
use vike_ctrader::conn::ConnConfig;
use vike_ctrader::klines::{CtraderKlines, HistoryConfigError, HistoryConfigProvider};
use vike_data::source::KlineSource;
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::FetchFailure;
use vike_marketdata::Bar;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::{FetchCtx, cancelled};
use crate::data::accounts::{AccountSource, StoreAccounts, feed_store};
use crate::session::{ConnectionMemo, ConnectionScope, Redacted};

/// The venue whose demo account holds the grant.
pub const VENUE: &str = "ctrader";

/// The account's tier: cTrader's demo environment is the account at tier `demo`.
const HISTORY_TIER: AccountTier = AccountTier::Demo;

/// The grant, in words, for a log line or a refusal: which fields of which account. Names only.
fn grant_words() -> String {
    format!(
        "the {ACCESS_TOKEN} and {REFRESH_TOKEN} of the ACTIVE unlabelled {VENUE} account at tier \
         `{HISTORY_TIER}`, with the venue's application pair"
    )
}

/// **One read of the demo grant as a connection config** — the three states of 0097's verdict 6, each its
/// own answer: `Ok` (present), [`HistoryConfigError::NotConfigured`] (no such account, or a field of the
/// pair is absent or blank) and [`HistoryConfigError::StoreUnreadable`] (the store, or its account rows,
/// would not read; the cause is in the log, which carries paths and OS reasons and never a value).
///
/// The config comes from `CtraderConfig::to_conn_config`, which JOINS the process's `SharedGrant` for this
/// refresh token, so a session built from it follows whatever the exec and data sockets rotated. The
/// config's rotation home is NOT set: this lane never refreshes, and a cell that has no home takes the first
/// one a refreshing sibling brings.
pub(crate) fn read_history_grant(
    accounts: &dyn AccountSource,
) -> Result<Redacted<ConnConfig>, HistoryConfigError> {
    let directory = accounts.directory(VENUE);
    if let Some(Err(why)) = directory.rows() {
        log_store_unreadable(why);
        return Err(HistoryConfigError::StoreUnreadable);
    }
    if let Some(Err(why)) = directory.credentials() {
        log_store_unreadable(why);
        return Err(HistoryConfigError::StoreUnreadable);
    }
    let credentials = directory.credentials_for(VENUE, &AccountLabel::Default, HISTORY_TIER);
    let config = CtraderConfig::from_credentials(HISTORY_TIER, &credentials)
        .ok_or(HistoryConfigError::NotConfigured)?;
    Ok(Redacted::new(config.to_conn_config()))
}

/// The `error!` an unreadable store earns on the lane's grant read. `why` is the store's own text.
fn log_store_unreadable(why: &str) {
    tracing::error!(
        error = %why,
        "the credential store could not be read for the cTrader history lane's grant; a credentialed \
         fetch for ctrader is refused, and it is NOT the same as the grant being absent"
    );
}

/// **Whether the demo account's token pair is STORED — a presence word, never a value.** The same
/// address the grant read uses, through `vike_secrets::CredentialStore::account_field_present`: on the
/// settings DATABASE the value never becomes a Rust value in this process. With no database there is no
/// store and the answer is `Absent`; a store that exists and will not read is `Unreadable`, logged
/// (never folded into `Absent`). See the module doc for what `Present` does not cover.
pub fn ctrader_history_presence(store: &vike_secrets::CredentialStore) -> CredentialPresence {
    if store.location().is_none() {
        return CredentialPresence::Absent;
    }
    let store = feed_store(store);
    for field in [ACCESS_TOKEN, REFRESH_TOKEN] {
        match store.account_field_present(VENUE, &AccountLabel::Default, HISTORY_TIER, field) {
            Ok(true) => {}
            Ok(false) => return CredentialPresence::Absent,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "the credential store could not be read to say whether the cTrader history lane's \
                     grant is stored; the history-channels answer says UNREADABLE, which is NOT the \
                     same as the grant being absent"
                );
                return CredentialPresence::Unreadable;
            }
        }
    }
    CredentialPresence::Present
}

/// The startup line that says whether the cTrader history lane is ARMED — 0097's verdict 6: the log
/// says it, and never says a value. `presence` is one probe's answer, a word, so no value can reach this
/// text; the grant is named by its account and FIELDS ([`grant_words`]).
///
/// Arming is the act of storing the grant on that account (`ctrader_authorize` writes it): there is no
/// second switch, and the text says the grant is read per connection, so there is nothing to restart. The
/// `ctrader history lane:` PREFIX is what an operator greps and does not change.
pub fn ctrader_history_lane_line(
    settings_dir: Option<&Path>,
    presence: CredentialPresence,
) -> String {
    let grant = grant_words();
    match (settings_dir, presence) {
        (_, CredentialPresence::Present) => format!(
            "ctrader history lane: ARMED — {grant}: the token pair is present in this daemon's \
             credential store (the application pair is not asked about here). The grant is read when a \
             connection's first FetchCredentialed for ctrader arrives (that request needs the Control \
             key) and dropped when the connection closes; each request opens a read-only session of \
             its own on the demo host, and a connection that holds only the Observe key never reaches \
             the lane (docs/decisions/0164, amending 0097)"
        ),
        (None, _) => format!(
            "ctrader history lane: not armed — this daemon resolved no settings directory, so there \
             is no credential store to hold {grant}, and a credentialed fetch for ctrader is refused"
        ),
        (Some(_), CredentialPresence::Unreadable) => format!(
            "ctrader history lane: UNKNOWN — the credential store could not be read (the error above \
             names it), so whether {grant} is stored cannot be told, and a credentialed fetch for \
             ctrader is refused until it can be. This is not the same as the grant being absent"
        ),
        (Some(_), _) => format!(
            "ctrader history lane: not armed — {grant} is absent from this daemon's credential \
             store, so a credentialed fetch for ctrader is refused naming the fix. On THIS box, the \
             venue's `CLIENT_ID` and `CLIENT_SECRET` are written once (`vike-cli secrets set --venue \
             ctrader CLIENT_ID`), and `ctrader_authorize --account-id <ID>` writes the token pair \
             onto a ctrader account at tier {HISTORY_TIER} (`vike-cli secrets accounts` lists them; \
             `vike-cli secrets account add --venue {VENUE} --tier {HISTORY_TIER} --no-label` creates \
             it). The grant is read per connection, so there is nothing to restart"
        ),
    }
}

/// The memo a connection keeps the config in: read once, the answer kept (an error included), dropped
/// with the connection. Its error half is `HistoryConfigError`, which carries no payload.
type GrantMemo = ConnectionMemo<Redacted<ConnConfig>, HistoryConfigError>;

/// What one connection holds for the cTrader lane: the config memo and the source built around it.
///
/// The memo is an `Arc` of its own so the provider can hold it WITHOUT holding the slot (the cycle the
/// OANDA lane's module doc names), and the source is built on the connection's first fetch.
#[derive(Default)]
struct CtraderSlot {
    grant: Arc<GrantMemo>,
    source: OnceLock<Box<dyn KlineSource>>,
}

/// How the lane builds a connection's source around its config provider: `CtraderKlines::new` in
/// production, a scripted source in a suite.
pub type SourceFor = Arc<dyn Fn(HistoryConfigProvider) -> Box<dyn KlineSource> + Send + Sync>;

/// **The credentialed kline lane for cTrader**: the store handle (a location and a scope, no value), the
/// way an account is found, and the way a connection's source is built.
pub struct CtraderLane {
    store: vike_secrets::CredentialStore,
    accounts: Arc<dyn AccountSource>,
    source_for: SourceFor,
}

impl CtraderLane {
    /// The production lane: `CtraderKlines` around each connection's provider, reading `store`.
    pub fn new(store: vike_secrets::CredentialStore) -> Self {
        let accounts = StoreAccounts::source(store.clone());
        Self::with_source(
            store,
            accounts,
            Arc::new(|config: HistoryConfigProvider| -> Box<dyn KlineSource> {
                Box::new(CtraderKlines::new(config))
            }),
        )
    }

    /// A lane whose accounts come from `accounts` and whose source is built by `source_for` — the seam
    /// a suite scripts through, so the REAL memo and the REAL server are under test with no cTrader host.
    pub fn with_source(
        store: vike_secrets::CredentialStore,
        accounts: Arc<dyn AccountSource>,
        source_for: SourceFor,
    ) -> Self {
        Self { store, accounts, source_for }
    }

    /// The store handle the lane reads (for the startup read the composition root makes).
    pub fn store(&self) -> &vike_secrets::CredentialStore {
        &self.store
    }

    /// The presence word of the lane's credential, read afresh.
    pub fn presence(&self) -> CredentialPresence {
        ctrader_history_presence(&self.store)
    }

    /// `FetchCredentialed { Klines }`: the connection's source, fetched once. The source asks its
    /// provider each time a session opens, and the provider answers from the connection's memo, so the
    /// store is read when the first session opens (a refusal before that — an interval the venue does
    /// not serve — reads nothing) and never again for this connection.
    ///
    /// A cancel is seen before the call only: the source's requests are paced inside one call with no
    /// probe, and one chunk is a handful of them.
    pub fn fetch(
        &self,
        scope: &ConnectionScope,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        ctx: &FetchCtx,
    ) -> Result<Vec<Bar>, FetchFailure> {
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        let slot = scope.slot::<CtraderSlot>();
        let source = slot.source.get_or_init(|| (self.source_for)(self.provider(&slot.grant)));
        source.fetch(symbol, interval, start_ms, end_ms).map_err(FetchFailure::from_source_error)
    }

    /// The provider a connection's source asks when a session opens: the memo's answer, read from the
    /// store the first time. It holds the accounts handle and the memo, never the slot.
    fn provider(&self, memo: &Arc<GrantMemo>) -> HistoryConfigProvider {
        let accounts = Arc::clone(&self.accounts);
        let memo = Arc::clone(memo);
        Arc::new(move || match memo.get_or_read(|| read_history_grant(accounts.as_ref())) {
            Ok(config) => Ok(config.expose().clone()),
            Err(e) => Err(*e),
        })
    }
}

impl std::fmt::Debug for CtraderLane {
    /// The store's location is a path, never a value; the constructor has nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtraderLane").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "ctrader_tests.rs"]
mod ctrader_tests;
