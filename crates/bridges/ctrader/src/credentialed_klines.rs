//! **The cTrader history lane's grant read, its presence probe, its startup line and the spec the
//! gatewayhub's credentialed kline engine runs it through** — the second credentialed kline lane beside
//! OANDA's (`vike_oanda::credentialed_klines`), with the same discipline and one difference of kind.
//! MOVED from the gatewayhub's `rest::ctrader` (owner ruling of 2026-11-12: one-venue code lives in its
//! venue's bridge); the engine is `crates/vike-gatewayhub/src/rest/credentialed_klines.rs`'s
//! `CredentialedKlinesLane` and the contract `vike_bridge_core::credentialed_klines`.
//!
//! The credential is an ACCOUNT's (decision 0156 item 6): the OAuth token pair (`ACCESS_TOKEN`,
//! `REFRESH_TOKEN`) of the ONE active unlabelled ctrader account at tier `demo`, plus the venue's
//! application pair (`CLIENT_ID`, `CLIENT_SECRET`, written once for every account), found by the account
//! ROW's address and read through the [`AccountDirectoryRead`] the gatewayhub hands the spec (its
//! `AccountSource` over the store handle the boot resolved, narrowed to `NoLiveKeys`: a live key has no
//! business in this call even from a mis-tiered row). No credential NAME is composed here and no tier is
//! read out of a name. The addresses are the DEMO rows of the venue's `venue_url` table
//! (`crate::config::ctrader_host` at `Demo`, over the rows the spec is built with); a `live` account's
//! grant is never read and a `paper` row is not the feed's stand-in here (the feed's
//! `data_feed_credentials` falls back to one, this lane deliberately does not: a history fetch is not a
//! strategy's feed).
//!
//! # What the lane does with it: its OWN read-only session
//!
//! [`KlineSourceCtrader`] opens a DEDICATED authenticated socket under that grant and sends nothing but
//! trendbar windows over it. It is not the trading socket and shares nothing with it but the process's
//! `SharedGrant` cell: the session FOLLOWS the cell's current pair and never spends a refresh token, so
//! the single writer of the rotated pair stays the exec or data socket of the account
//! (`crates/bridges/ctrader/CLAUDE.md`'s "The OAuth token loop is CLOSED, and the credential store is the
//! ONE home", rung 4). The argument for a socket of its own is on `crates/bridges/ctrader/src/history.rs`'s
//! module doc: the actor's trendbar command has no reply path and its answer replaces a running
//! strategy's series, and the venue meters historical requests per connection, so a socket of its own
//! cannot slow an order. The datahub holds no key and dials nothing: it asks the gatewayhub through
//! `Request::FetchCredentialed` (the Write scope), and no Observe verb reaches the lane but the presence
//! word.
//!
//! # Read once per CONNECTION, which is once per REQUEST
//!
//! As for OANDA (decision 0097, verdict 2, re-homed by 0164): the connection config is built from the
//! store when the connection source's first session opens, kept in a memo born with that source for the
//! rest of that request, and dropped with the connection. A session reopened after a drop asks the memo,
//! not the store, and the config's own `sync_grant` picks up a pair a sibling rotated meanwhile. A store
//! that exists and will not read is `HistoryConfigError::StoreUnreadable`, never "absent"; demo address
//! rows that do not answer are `HistoryConfigError::AddressRefused`, the row named in an `error!`.
//!
//! ⚠ **The presence word asks about the TOKEN PAIR only.** The store has no boolean-only read of a
//! venue-owned field, so the application pair cannot be asked about without bringing its values into this
//! process, which decision 0102 forbids on an Observe path. `Present` therefore says the demo account's
//! `ACCESS_TOKEN` and `REFRESH_TOKEN` are stored; a missing application pair is a request-time refusal
//! that names it (`HistoryConfigError::NotConfigured`).

use std::path::Path;
use std::sync::Arc;

use vike_bridge_core::connection_memo::{ConnectionMemo, Redacted};
use vike_bridge_core::credentialed_klines::{CredentialedKlinesSource, CredentialedKlinesSpec};
use vike_data::source::KlineSource;
use vike_model::AccountTier;
use vike_model::CredentialPresence;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_venues::venue_urls::VenueUrls;

use crate::config::{ACCESS_TOKEN, CtraderConfig, REFRESH_TOKEN, ctrader_host};
use crate::history::{HistoryConfigError, HistoryConfigProvider, KlineSourceCtrader};
use crate::transport::connection::ConnectionConfig;

/// The venue whose demo account holds the grant.
pub const VENUE: &str = "ctrader";

/// The account's tier: cTrader's demo environment is the account at tier `demo`.
const HISTORY_TIER: AccountTier = AccountTier::Demo;

/// How the lane reads a venue's account rows and the chosen account's fields: the gatewayhub's
/// `AccountSource::directory` over its store handle (narrowed to `NoLiveKeys`), asked once per read with
/// [`VENUE`]. A suite counts the reads through it.
pub type AccountDirectoryRead = Arc<dyn Fn(&str) -> AccountDirectory + Send + Sync>;

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
/// `directory` is the venue's directory, as one [`AccountDirectoryRead`] answered it.
///
/// The config comes from `CtraderConfig::to_connection_config`, which JOINS the process's `SharedGrant` for this
/// refresh token, so a session built from it follows whatever the exec and data sockets rotated. The
/// config's rotation home is NOT set: this lane never refreshes, and a cell that has no home takes the first
/// one a refreshing sibling brings. Its addresses are `urls`' DEMO rows; rows that do not answer are
/// [`HistoryConfigError::AddressRefused`], the refusal (it names the row) at `error!`.
///
/// `pub(crate)`: the result holds the OAuth tokens, and a `pub` here is a function any dependent crate
/// could call to bypass the [`Redacted`] memo.
pub(crate) fn read_history_grant(
    directory: &AccountDirectory,
    urls: &VenueUrls,
) -> Result<Redacted<ConnectionConfig>, HistoryConfigError> {
    if let Some(Err(why)) = directory.rows() {
        log_store_unreadable(why);
        return Err(HistoryConfigError::StoreUnreadable);
    }
    if let Some(Err(why)) = directory.credentials() {
        log_store_unreadable(why);
        return Err(HistoryConfigError::StoreUnreadable);
    }
    let endpoints = ctrader_host(urls, HISTORY_TIER).map_err(|why| {
        tracing::error!(reason = %why, "the cTrader history lane has no demo address; a credentialed \
                                        fetch for ctrader is refused and nothing is dialled");
        HistoryConfigError::AddressRefused
    })?;
    let credentials = directory.credentials_for(VENUE, &AccountLabel::Default, HISTORY_TIER);
    let config = CtraderConfig::from_credentials(&endpoints, &credentials)
        .ok_or(HistoryConfigError::NotConfigured)?;
    Ok(Redacted::new(config.to_connection_config()))
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
/// address the grant read uses, through `vike_secrets::CredentialStore::account_fields_presence`: on the
/// settings DATABASE the value never becomes a Rust value in this process. With no database there is no
/// store and the answer is `Absent`; a store that exists and will not read is `Unreadable`, logged
/// (never folded into `Absent`). See the module doc for what `Present` does not cover. `store` is asked
/// as it is handed: the gatewayhub hands its feed handle, narrowed to `NoLiveKeys` as the account read
/// is.
pub fn ctrader_history_presence(store: &vike_secrets::CredentialStore) -> CredentialPresence {
    let presence = store.account_fields_presence(
        VENUE,
        &AccountLabel::Default,
        HISTORY_TIER,
        &[ACCESS_TOKEN, REFRESH_TOKEN],
    );
    if presence == CredentialPresence::Unreadable {
        tracing::error!(
            "the credential store could not be read to say whether the cTrader history lane's \
             grant is stored; the history-channels answer says UNREADABLE, which is NOT the \
             same as the grant being absent"
        );
    }
    presence
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
             onto a ctrader account at tier {HISTORY_TIER} (`vike-cli account ls` lists them; \
             `vike-cli account add --venue {VENUE} --tier {HISTORY_TIER} --no-label` creates \
             it). The grant is read per connection, so there is nothing to restart"
        ),
    }
}

/// The memo a connection keeps the config in: read once, the answer kept (an error included), dropped
/// with the connection. Its error half is `HistoryConfigError`, which carries no payload.
type GrantMemo = ConnectionMemo<Redacted<ConnectionConfig>, HistoryConfigError>;

/// How the spec builds a connection's source around its config provider: `KlineSourceCtrader::new` in
/// production, a scripted source in a suite.
pub type SourceFor = Arc<dyn Fn(HistoryConfigProvider) -> Box<dyn KlineSource> + Send + Sync>;

/// **The credentialed kline lane's spec for cTrader**: the store handle the presence word asks (a
/// location and a scope, no value), the way an account is found, the venue's `venue_url` rows, and the
/// way a connection's source is built.
pub struct CredentialedKlinesSpecCtrader {
    store: vike_secrets::CredentialStore,
    accounts: AccountDirectoryRead,
    urls: VenueUrls,
    source_for: SourceFor,
    /// The FX login gate's clock every session of the lane reads (`ConnectionConfig::fx_clock`); `None` is no gate.
    fx_clock: Option<Arc<dyn vike_model::Clock + Send + Sync>>,
}

impl CredentialedKlinesSpecCtrader {
    /// The production spec: `KlineSourceCtrader` around each connection's provider, finding the account
    /// through `accounts` and asking `store` for the presence word, dialled at `urls`' demo rows (the
    /// daemon's one read of the `venue_url` table).
    pub fn new(
        store: vike_secrets::CredentialStore,
        accounts: AccountDirectoryRead,
        urls: VenueUrls,
    ) -> Self {
        Self::with_source(
            store,
            accounts,
            urls,
            Arc::new(|config: HistoryConfigProvider| -> Box<dyn KlineSource> {
                Box::new(KlineSourceCtrader::new(config))
            }),
        )
    }

    /// A spec whose accounts come from `accounts` and whose source is built by `source_for` — the seam a
    /// suite scripts through, so the REAL memo and the REAL engine are under test with no cTrader host.
    pub fn with_source(
        store: vike_secrets::CredentialStore,
        accounts: AccountDirectoryRead,
        urls: VenueUrls,
        source_for: SourceFor,
    ) -> Self {
        Self { store, accounts, urls, source_for, fx_clock: None }
    }

    /// This spec with the FX login gate on `fx_clock`: no session of the lane logs in while the FX market is closed
    /// at its time (the gatewayhub hands its wall clock).
    #[must_use]
    pub fn with_fx_clock(
        mut self,
        fx_clock: Option<Arc<dyn vike_model::Clock + Send + Sync>>,
    ) -> Self {
        self.fx_clock = fx_clock;
        self
    }

    /// The provider a connection's source asks when a session opens: the memo's answer, read from the
    /// store the first time. It holds the accounts handle and the memo, never the source.
    fn provider(&self, memo: &Arc<GrantMemo>) -> HistoryConfigProvider {
        let (accounts, urls) = (Arc::clone(&self.accounts), self.urls.clone());
        let (memo, fx_clock) = (Arc::clone(memo), self.fx_clock.clone());
        Arc::new(move || match memo.get_or_read(|| read_history_grant(&accounts(VENUE), &urls)) {
            Ok(config) => {
                Ok(ConnectionConfig { fx_clock: fx_clock.clone(), ..config.expose().clone() })
            }
            Err(e) => Err(*e),
        })
    }
}

impl CredentialedKlinesSpec for CredentialedKlinesSpecCtrader {
    fn venue(&self) -> &'static str {
        VENUE
    }

    /// The connection's source, around a provider whose memo is born with it: the source asks its provider
    /// each time a session opens, so the store is read when the first session opens (a refusal before that
    /// — an interval the venue does not serve — reads nothing) and never again for this connection. The
    /// source's requests are paced inside one call with no stop point, and one chunk is a handful of them.
    fn connection_source(&self) -> Box<dyn CredentialedKlinesSource> {
        let memo = Arc::new(GrantMemo::default());
        Box::new((self.source_for)(self.provider(&memo)))
    }

    fn presence(&self) -> CredentialPresence {
        ctrader_history_presence(&self.store)
    }

    /// One PRESENCE probe of the demo account's token pair, a word and never a value, so the log says
    /// whether a credentialed fetch for ctrader will be answered.
    fn startup_line(&self, settings_dir: Option<&Path>) -> (CredentialPresence, String) {
        let presence = ctrader_history_presence(&self.store);
        (presence, ctrader_history_lane_line(settings_dir, presence))
    }
}

impl std::fmt::Debug for CredentialedKlinesSpecCtrader {
    /// The store's location is a path, never a value; the constructor has nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialedKlinesSpecCtrader").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "credentialed_klines_tests.rs"]
mod credentialed_klines_tests;
