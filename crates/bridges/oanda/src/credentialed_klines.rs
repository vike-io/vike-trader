//! **The OANDA history lane's one practice-token read, its presence probe, its startup line and the
//! spec the gatewayhub's credentialed kline engine runs it through** — MOVED from the gatewayhub's
//! `transport::oanda` (owner ruling of 2026-11-12: one-venue code lives in its venue's bridge), which had
//! moved it from the datahub's `oanda_history` module (decision 0164 amends 0097's verdicts 2 and 3: who
//! reads the credential; verdict 1, that all history goes through the server, stands). The engine is
//! `crates/vike-gatewayhub/src/rest/credentialed_klines.rs`'s `CredentialedKlinesLane`; the contract is
//! `vike_bridge_core::credentialed_klines`.
//!
//! The token is an ACCOUNT's credential (decision 0156 item 6): the `API_KEY` field of the ONE
//! ACTIVE oanda `account` row at tier `demo` with no label — [`VENUE`], `PRACTICE_TIER` and
//! `PRACTICE_FIELD` say it once — read through `vike_secrets::CredentialStore::account_field`, the
//! narrowest read the store offers. No credential NAME is composed or read here, and no tier is read
//! out of one: the account row's own `tier` and `active` are the address. The account id, a
//! live-named key and every other venue's key are outside the lane, and the host is the practice
//! host (this crate's). Practice tier only.
//!
//! # Read once per CONNECTION, which is once per REQUEST
//!
//! 0097's verdict 2 rules that the token is read when a request arrives and dropped when it ends: a
//! chunked ingest of thousands of day fetches reads the store ONCE, and a key stored, rotated or
//! removed between two requests is what the next one sees. The datahub's source is built per request
//! and holds one lazily dialled connection for its life
//! (`vike_gatewayhub_client::history::KlineSourceGateway`), so the daemon's unit of "a request" is "a
//! connection", and the engine builds ONE source per connection through
//! [`CredentialedKlinesSpecOanda`]'s `connection_source`, keeps it in the connection's scope and drops
//! it with the connection. The source asks its provider for the token on its first call, and the
//! provider answers from a [`ConnectionMemo`] born with the source: the answer, an error included, is
//! kept for the connection. Nothing in the spec itself holds a value between connections: it holds a
//! settings DIRECTORY (the store handle) and a constructor.
//!
//! ⚠ **The memo must not be reachable from the thing it is stored in.** The provider handed to
//! `KlineSourceOanda::new` holds the memo (an `Arc`), the source holds the provider, and the memo holds
//! neither, so dropping the connection drops the source, then the provider, then the token. A
//! provider that held the source would be a cycle that keeps the token for the life of the process.
//!
//! ⚠ **Unreadable is not absent.** A store that exists and will not open is
//! `HistoryTokenError::StoreUnreadable` (kept by the memo, logged once per connection) and
//! `CredentialPresence::Unreadable`; folding it into `NotConfigured`/`Absent` would send the operator
//! to store a key they already stored (0097's verdict 6). The error type carries no payload, so a
//! token cannot reach an error text by construction.

use std::path::Path;
use std::sync::Arc;

use vike_bridge_core::connection_memo::{ConnectionMemo, Redacted};
use vike_bridge_core::credentialed_klines::{CredentialedKlinesSource, CredentialedKlinesSpec};
use vike_data::source::KlineSource;
use vike_model::CredentialPresence;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::venue_urls::VenueUrls;

use crate::history::klines::{HistoryTokenError, HistoryTokenProvider, KlineSourceOanda};

/// The venue whose practice account holds the token.
pub const VENUE: &str = "oanda";

/// The practice account's tier: OANDA's practice environment is the account at tier `demo`.
const PRACTICE_TIER: vike_model::AccountTier = vike_model::AccountTier::Demo;

/// The token's FIELD on that account — the oanda row of `vike_venues::credential_schema`'s
/// `VENUE_KEY_SCHEMAS` names it `API_KEY`.
const PRACTICE_FIELD: &str = vike_venues::credential_keys::API_KEY;

/// The token, in words, for a log line or a refusal: which field of which account. Names and
/// words only — there is no value in scope here.
fn practice_token_words() -> String {
    format!(
        "the {PRACTICE_FIELD} of the ACTIVE unlabelled {VENUE} account at tier \
         `{PRACTICE_TIER}`"
    )
}

/// The `error!` an unreadable store earns on the lane's token read — one spelling for both of
/// [`oanda_history_credentials`]'s reads.
fn log_token_store_unreadable(e: &vike_secrets::StoreReadError) {
    tracing::error!(
        error = %e,
        "the credential store could not be read for the OANDA history lane's practice \
         token; a credentialed fetch for oanda is refused, and it is NOT the same as the key being \
         absent"
    );
}

/// **The OANDA history lane's ONE credential, read out of the store** —
/// docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md,
/// verdict 2. The read is `vike_secrets::CredentialStore::account_field` at the practice account's
/// address (this module's doc): one field of one account row, and nothing else.
///
/// ⚠ **What "one field" guarantees.** The store is the settings DATABASE (the only credential store
/// since 2026-10-07): the read selects that one row's value and no other, so no other credential
/// enters this process, and `crates/bridges/oanda/src/credentialed_klines_tests.rs`'s
/// `the_oanda_history_read_holds_the_one_practice_key_and_nothing_else` pins what comes back among
/// the practice account's own other field, its live, paper and labelled siblings and another
/// venue's key.
///
/// ⚠ **`pub(crate)`, as the datahub's was `pub(super)`.** This returns the plaintext token, and the
/// lane holds it in a `Redacted` inside a `ConnectionMemo`; a `pub` here is a function any dependent
/// crate could call to bypass both. The gatewayhub reaches it only through the spec.
///
/// ⚠ **`announce` is whether the store's permission FINDING is logged, and only the startup read
/// announces.** A connection's memo calls this once per connection, and a finding repeated per
/// request would bury the log it is meant to be read in — the memo's read logs NOTHING on the path
/// that finds a store. The field read returns no findings, so the announcing call takes them from a
/// scoped read of NO names (`vike_secrets::CredentialStore::read` over an empty
/// `vike_secrets::CredentialKeyScope`: it selects no row) and hands them to
/// `vike_secrets::CredentialScopedRead::report`, the store's one report body. A store that exists and
/// cannot be read is logged either way: that read ends the request, so it is one line per failed
/// request, and the refusal the operator reads (`HistoryTokenError::StoreUnreadable`'s) sends them to
/// this log for the cause.
///
/// `None` is that unreadable store — never folded into "no credential", because a permissions bug
/// wearing the unconfigured answer would send the operator to store a key they already stored. A
/// store with no such account, or an account without the field (or with it blank), is `Some(None)`.
/// No line this function logs carries a value: the permission finding and the error's `Display`
/// carry paths, modes and OS reasons.
pub(crate) fn oanda_history_credentials(
    store: &vike_secrets::CredentialStore,
    announce: bool,
) -> Option<Option<vike_model::secret::Secret<String>>> {
    if announce {
        // The store's findings, surfaced by the ANNOUNCING read: a read returns them as data and
        // logs nothing, `vike_secrets::CredentialScopedRead::report` is where they become lines, and a
        // credential store readable by others is not read in silence.
        match store.read(&vike_secrets::CredentialKeyScope::default()) {
            Ok(findings) => findings.report(),
            Err(e) => {
                log_token_store_unreadable(&e);
                return None;
            }
        }
    }
    let token = store.account_field(VENUE, &AccountLabel::Default, PRACTICE_TIER, PRACTICE_FIELD);
    match token {
        Ok(token) => Some(token),
        Err(e) => {
            log_token_store_unreadable(&e);
            None
        }
    }
}

/// **One read of the practice token** — the three states 0097's verdict 6 names, each its own
/// answer: `Ok` (present), [`HistoryTokenError::NotConfigured`] (absent from a store that answered —
/// no such account, or no non-blank field on it — or no settings directory at all, so no store to
/// hold it) and [`HistoryTokenError::StoreUnreadable`]. The `String` is the only copy that leaves.
/// `store` is the boot's handle, opened over the settings directory the boot resolved — a handle with
/// no location is "no settings directory".
pub(crate) fn read_oanda_history_token(
    store: &vike_secrets::CredentialStore,
    announce: bool,
) -> Result<String, HistoryTokenError> {
    if store.location().is_none() {
        return Err(HistoryTokenError::NotConfigured);
    }
    let token =
        oanda_history_credentials(store, announce).ok_or(HistoryTokenError::StoreUnreadable)?;
    token.map(vike_model::secret::Secret::into_inner).ok_or(HistoryTokenError::NotConfigured)
}

/// **Whether the OANDA history lane's practice token is STORED — a presence word, never a value.**
/// What the history-channels read reports for OANDA's credentialed row
/// (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2.4; the owner's Q1).
///
/// The same address and the same store decision `oanda_history_credentials` uses, through
/// `vike_secrets::CredentialStore::account_fields_presence`: on the settings DATABASE — the only
/// credential store — that read selects a boolean and never the value, so a presence request does
/// not bring the token into this process as a value at all; with no database there is no store and
/// the answer is `Absent`. "Present" means a non-blank value, the rule the token read applies, so
/// this word cannot say a key is armed that the lane then refuses.
///
/// Three states and the no-directory case, each its own answer, and `Unreadable` never collapsed
/// into `Absent` (0097's verdict 6): no settings directory → `Absent` (no store to hold it, the
/// answer `read_oanda_history_token` gives); a store without the account or the field → `Absent`;
/// with it → `Present`; a store that exists and will not read → `Unreadable`, logged (never a value),
/// once per request that met it.
pub fn oanda_history_presence(store: &vike_secrets::CredentialStore) -> CredentialPresence {
    let presence = store.account_fields_presence(
        VENUE,
        &AccountLabel::Default,
        PRACTICE_TIER,
        &[PRACTICE_FIELD],
    );
    if presence == CredentialPresence::Unreadable {
        tracing::error!(
            "the credential store could not be read to say whether the OANDA history lane's \
             practice token is stored; the history-channels answer says UNREADABLE, which is \
             NOT the same as the key being absent"
        );
    }
    presence
}

/// The startup line that says whether the OANDA history lane is ARMED — 0097's verdict 6: the log
/// says it, and never says a value. `presence` is one read's answer with the token already dropped
/// (`Result<(), _>`), so no value can reach this text by construction; it names the token by its
/// account and FIELD (`practice_token_words`), never by a composed name.
///
/// Arming is the act of storing the key on that account: there is no second switch, and the text
/// says that the key is read per connection so an operator does not restart for it.
///
/// ⚠ The words are the gatewayhub's, not the datahub's the lane was moved from: the verb is
/// `FetchCredentialed` (a `Write`-scope request, which a connection holds the Control key for), the
/// unit of "a request" is a CONNECTION (a client's history source holds one for the request it was
/// built for), and a connection that authenticated with the Observe key alone never reaches the lane.
/// The `oanda history lane:` PREFIX is what an operator greps and does not change.
pub fn oanda_history_lane_line(
    settings_dir: Option<&Path>,
    presence: Result<(), HistoryTokenError>,
) -> String {
    let key = practice_token_words();
    match (settings_dir, presence) {
        (_, Ok(())) => format!(
            "oanda history lane: ARMED — {key}, the practice account's API token, is present in \
             this daemon's credential store. It is read when a connection's first FetchCredentialed \
             for oanda arrives (that request needs the Control key) and dropped when that \
             connection closes; nothing holds it in between, and a connection that holds only the \
             Observe key never reaches the lane (docs/decisions/0164, amending 0097)"
        ),
        (None, Err(_)) => format!(
            "oanda history lane: not armed — this daemon resolved no settings directory, so there \
             is no credential store to hold {key}, and a credentialed fetch for oanda is refused"
        ),
        (Some(_), Err(HistoryTokenError::StoreUnreadable)) => format!(
            "oanda history lane: UNKNOWN — the credential store could not be read (the error above \
             names it), so whether {key} is stored cannot be told, and a credentialed fetch for \
             oanda is refused until it can be. This is not the same as the key being absent"
        ),
        (Some(_), Err(HistoryTokenError::NotConfigured)) => format!(
            "oanda history lane: not armed — {key}, the practice account's API token, is absent \
             from this daemon's credential store, so a credentialed fetch for oanda is refused \
             naming the fix. On THIS box, `vike-cli secrets set \
             --id <ID> {PRACTICE_FIELD}` arms it, <ID> being that account's row \
             (`vike-cli account ls` lists them; `vike-cli account add --venue \
             {VENUE} --tier {PRACTICE_TIER} --no-label` creates it). The key is read per \
             connection, so there is nothing to restart"
        ),
    }
}

/// The memo a connection keeps the token in: read once, the answer kept (an error included), dropped
/// with the connection. Its error half is `HistoryTokenError`, which carries no payload.
type TokenMemo = ConnectionMemo<Redacted<String>, HistoryTokenError>;

/// How the lane builds a connection's source around its token provider: `KlineSourceOanda::new` in
/// production, a scripted source in a suite.
pub type SourceFor = Arc<dyn Fn(HistoryTokenProvider) -> Box<dyn KlineSource> + Send + Sync>;

/// **The credentialed kline lane's spec for OANDA**: the store handle (a location and a scope, no value)
/// and the way a connection's source is built.
pub struct CredentialedKlinesSpecOanda {
    store: vike_secrets::CredentialStore,
    source_for: SourceFor,
}

impl CredentialedKlinesSpecOanda {
    /// The production spec: `KlineSourceOanda` around each connection's provider, reading `store`, on the
    /// practice REST base of `urls` (oanda's `venue_url` rows; a refused row fails every fetch as `Fetch`,
    /// naming it).
    pub fn new(store: vike_secrets::CredentialStore, urls: &VenueUrls) -> Self {
        let urls = urls.clone();
        Self::with_source(
            store,
            Arc::new(move |token: HistoryTokenProvider| -> Box<dyn KlineSource> {
                Box::new(KlineSourceOanda::new(token, &urls))
            }),
        )
    }

    /// A spec whose source is built by `source_for` — the seam a suite scripts a source through, so
    /// the REAL memo, the REAL store read and the REAL engine are under test with no OANDA host.
    pub fn with_source(store: vike_secrets::CredentialStore, source_for: SourceFor) -> Self {
        Self { store, source_for }
    }

    /// The provider a connection's source asks: the memo's answer, read from the store on the first
    /// ask. It holds the store HANDLE and the memo, never the source.
    fn provider(&self, memo: &Arc<TokenMemo>) -> HistoryTokenProvider {
        let store = self.store.clone();
        let memo = Arc::clone(memo);
        Arc::new(move || {
            match memo.get_or_read(|| read_oanda_history_token(&store, false).map(Redacted::new)) {
                Ok(token) => Ok(token.expose().clone()),
                Err(e) => Err(*e),
            }
        })
    }
}

impl CredentialedKlinesSpec for CredentialedKlinesSpecOanda {
    fn venue(&self) -> &'static str {
        VENUE
    }

    /// The connection's source, around a provider whose memo is born with it: the store is read on the
    /// first fetch that gets as far as needing a token (a refusal before the token — an interval OANDA
    /// does not serve — reads nothing) and never again for this connection. The source pages inside one
    /// call with no stop point.
    fn connection_source(&self) -> Box<dyn CredentialedKlinesSource> {
        let memo = Arc::new(TokenMemo::default());
        Box::new((self.source_for)(self.provider(&memo)))
    }

    fn presence(&self) -> CredentialPresence {
        oanda_history_presence(&self.store)
    }

    /// The ONE announcing read of the practice token — the read that logs the credential store's
    /// permission finding — dropped before the line is built, so the line reports PRESENCE and never a
    /// value.
    fn startup_line(&self, settings_dir: Option<&Path>) -> (CredentialPresence, String) {
        let read = read_oanda_history_token(&self.store, true).map(drop);
        let presence = match read {
            Ok(()) => CredentialPresence::Present,
            Err(HistoryTokenError::NotConfigured) => CredentialPresence::Absent,
            Err(HistoryTokenError::StoreUnreadable) => CredentialPresence::Unreadable,
        };
        (presence, oanda_history_lane_line(settings_dir, read))
    }
}

impl std::fmt::Debug for CredentialedKlinesSpecOanda {
    /// The store's location is a path, never a value; the constructor has nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialedKlinesSpecOanda").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "credentialed_klines_tests.rs"]
mod credentialed_klines_tests;
