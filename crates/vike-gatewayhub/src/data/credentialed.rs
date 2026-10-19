//! The CREDENTIALED stream sessions of the strategy lane (wave 2, W10): alpaca, ctrader, ig and oanda,
//! logged in as the account the strategy trades.
//!
//! Their market data authenticates (an OAuth2 trio, a protobuf socket's two-stage auth, an IG login per
//! subscription, a Bearer token on a chunked pricing stream), so there is no keyless price stream to fall
//! back to. The tradehub built these feeds in its own process from a `VenuePlan` that carried the
//! resolved config; after the move it carries nothing: the gatewayhub resolves the account, builds the
//! venue client and serves it on the same `DataOpen` session as a keyless one, under the `Write` scope
//! (`cs_venue=`, the datahub's control key). **No credential, label or tier crosses toward the datahub.**
//!
//! **ibkr is the fifth, and a different kind** (any-symbol price lane, decision 0174; the `ibkr` feature
//! builds it, `CredentialedLane::venues` drops it otherwise): nothing is mounted on it, so its stream is
//! `vike_ibkr::quote_handle::LazyQuotes`, built with NOTHING dialled and nothing subscribed, quotes only. The
//! first symbol the tradehub's control door asks a price for dials the gateway's one data connection (shared
//! with the any-symbol lookup of that stock) through the gate's data lane.
//!
//! **dukascopy is the sixth, and a different kind again** (any-symbol price lane, decision 0174): nothing is mounted on
//! it and its stream logs in to NOTHING. [`super::dukascopy_quotes::DukascopyQuotes`] listens on the process's
//! price bus, where the sidecar the TRADE lane started for the armed `demo` account delivers its ticks, so the
//! first symbol the tradehub's control door asks a price for is one `subscribe` command to a sidecar that is
//! already up, and a box with none refuses in words. It reads no account row and spends no credential.
//!
//! # What moved, unchanged
//!
//! * **The account** is [`AccountDirectory::data_feed_credentials`] (`crate::data::accounts`).
//! * **The tier** is `Demo` for all four, as the plans had it: each venue's exec arm is demo-pinned, so
//!   the feed and the exec side agree on the environment by construction. ibkr's stream takes the same tier
//!   (the DEFAULT `demo` account's gateway session); dukascopy's reads no account row, so it has none.
//! * **The constructors** are the tradehub's `feeds/{alpaca,ctrader,ig,oanda}.rs` calls.
//! * **The refusals** are `venue_plan.rs`'s credential refusals, byte for byte where they can be: an
//!   absent trio REFUSES the open (a feed-less strategy quotes into the void), and the tradehub maps the
//!   refusal onto the same startup refusal it printed (R13).
//!
//! # ⚠ Startup semantics, which differ per venue and are PRESERVED
//!
//! * **ctrader's handshake is SYNCHRONOUS**: `conn::connect_and_auth` runs inside [`build_credentialed`],
//!   so a connect or auth failure is the `DataOpen` refusal (the open reply is the handshake's answer) and
//!   the tradehub refuses to start on it, as it did.
//! * alpaca, ig and oanda construct network-free: a wrong secret shows as a reconnect-looping feed in the
//!   log, never an open error.
//! * ctrader's socket REFRESHES an expiring grant and writes the new pair back through the rotation home
//!   (`state_dir`, the project state directory this daemon's boot declared). It is the one writer of the
//!   rotated pair; the exec side's sockets share the grant (`CtraderConfig::to_conn_config`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use vike_data::{DataClient, LiveDataSink};
use vike_model::AccountTier;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;

use super::accounts::AccountSource;
use crate::registry::{Lane, owned, roster};

/// The tier the account-backed credentialed streams log in at (alpaca, ctrader, ig and oanda, and ibkr's
/// gateway session; dukascopy's reads no account row): the first four's exec arms arm a `demo` account only,
/// and the feed resolves the same one.
const FEED_TIER: AccountTier = AccountTier::Demo;

/// **How long ctrader's synchronous handshake may take inside a `DataOpen`**, and the first link of the
/// refusal-timing chain `tradehub > datahub > gatewayhub`.
///
/// The handshake is up to four sequential replies (application auth, account auth, trader, symbols), each
/// waited for up to `venue_timings::CTRADER`'s 15 s `response_timeout`, so an unbounded handshake runs a
/// minute and more. Every hop above this one waits for the open: the datahub's client for the `DataOpened`
/// reply (`GatewayTimings::lookup_reply`, 15 s) and, above it, the tradehub for the datahub's ack
/// (`StratTimings::sub_reply`, 25 s). An open that outlasts the datahub's wait is a bare "reply timed out"
/// there, the gatewayhub's words are lost, and the datahub redials, which starts the handshake again: a
/// livelock that never says why. So the handshake is bounded HERE, below the datahub's wait with margin,
/// and a venue that is slow past it is a refusal in words. A well venue answers each step inside a second.
const CTRADER_HANDSHAKE_BUDGET: std::time::Duration =
    vike_gatewayhub_client::proto::GW_CREDENTIALED_OPEN_BUDGET;

/// Why a bounded synchronous open produced no answer. Four causes an operator acts on differently: a venue
/// that is slow, a handshake that crashed, a thread the OS would not give, and handshakes still running from
/// earlier opens.
#[derive(Debug, PartialEq, Eq)]
enum NoAnswer {
    /// Nothing within the budget (the handshake keeps running on its thread and is dropped when it ends).
    TimedOut,
    /// The handshake thread ended without an answer: it PANICKED (its hook has reported it).
    Panicked,
    /// The OS refused the thread (the text is the OS's).
    NotStarted(String),
    /// This many earlier handshakes are still running (abandoned after their budget): a new one is refused
    /// until they end, instead of piling threads and concurrent logins on one account.
    TooMany(usize),
}

/// How many abandoned handshakes may still be running before a new open is refused. Each ends within the
/// bridge's own per-step bounds (four steps of 15 s), and the datahub's redial backoff keeps the rate low, so
/// a few is the ordinary worst case and more is a venue that is not answering at all.
const MAX_RUNNING_HANDSHAKES: usize = 4;

/// Handshake threads running now (abandoned ones included), across every credentialed open of the process.
static HANDSHAKES_RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Run `work` on a thread of its own and wait at most `budget` for its answer. The thread is left to finish
/// when the budget passes (a handshake socket closes when its connection is dropped, which is what happens to the
/// late answer: the channel has no reader left and the value is dropped), and counts against
/// [`MAX_RUNNING_HANDSHAKES`] until it does.
fn bounded<T: Send + 'static>(
    budget: std::time::Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, NoAnswer> {
    bounded_in(&HANDSHAKES_RUNNING, MAX_RUNNING_HANDSHAKES, budget, work)
}

/// [`bounded`] over a gauge and cap of the caller's (a test's own, so the process-wide count is not shared).
fn bounded_in<T: Send + 'static>(
    running: &'static std::sync::atomic::AtomicUsize,
    cap: usize,
    budget: std::time::Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, NoAnswer> {
    use std::sync::atomic::Ordering;
    /// Gives the thread's place back when it ends, however it ends (a panic unwinds through it).
    struct Place(&'static std::sync::atomic::AtomicUsize);
    impl Drop for Place {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let held = running.fetch_add(1, Ordering::SeqCst);
    if held >= cap {
        running.fetch_sub(1, Ordering::SeqCst);
        return Err(NoAnswer::TooMany(held));
    }
    let place = Place(running);
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("gwh-cred-open".to_string()).spawn(move || {
        let _place = place;
        let _ = tx.send(work());
    });
    if let Err(e) = spawned {
        // The closure (and its `Place`) was dropped by the failed spawn: the place is already given back.
        return Err(NoAnswer::NotStarted(e.to_string()));
    }
    match rx.recv_timeout(budget) {
        Ok(answer) => Ok(answer),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(NoAnswer::TimedOut),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(NoAnswer::Panicked),
    }
}

/// How the lane builds one credentialed venue's client from a directory. A loopback test passes a
/// scripted one; production is [`build_credentialed`] over the lane's rotation home.
pub type CredClientBuilder = Box<
    dyn Fn(
            &str,
            Arc<dyn LiveDataSink>,
            &AccountDirectory,
        ) -> Result<Box<dyn DataClient + Send>, String>
        + Send
        + Sync,
>;

/// The credentialed-stream half of a data lane.
pub struct CredentialedLane {
    accounts: Arc<dyn AccountSource>,
    build: CredClientBuilder,
}

impl CredentialedLane {
    /// The production lane: [`build_credentialed`] over `accounts`, with `state_dir` (the declared
    /// project state directory, `None` for a box with no project) as ctrader's rotation home and
    /// `venue_settings` (every venue's `venue_setting` rows, the daemon's one read) for the stream that
    /// dials a gateway the rows name (ibkr's `venue.ibkr.<tier>.{host,port,backend}`).
    pub fn new(
        accounts: Arc<dyn AccountSource>,
        state_dir: Option<PathBuf>,
        venue_settings: &BTreeMap<String, VenueSettings>,
    ) -> Self {
        let ibkr = venue_settings.get("ibkr").cloned().unwrap_or_default();
        Self::over(
            accounts,
            Box::new(move |venue, sink, directory| {
                build_credentialed(venue, sink, directory, state_dir.as_deref(), &ibkr)
            }),
        )
    }

    /// A lane over any builder (a test's).
    pub fn over(accounts: Arc<dyn AccountSource>, build: CredClientBuilder) -> Self {
        Self { accounts, build }
    }

    /// The venues this lane serves, in advertisement order: `crate::registry`'s credentialed-stream
    /// roster, less ibkr in a build without the `ibkr` feature (its stream links the bridge; advertising
    /// a venue this build cannot open would be a `cs_venue=` entry nothing serves).
    pub fn venues(&self) -> Vec<String> {
        owned(Lane::CredentialedStream)
            .into_iter()
            .filter(|venue| venue != "ibkr" || cfg!(feature = "ibkr"))
            .collect()
    }

    /// Resolve the account (fresh, every open), refuse on an unreadable store, build the client.
    pub fn open(
        &self,
        venue: &str,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let directory = self.accounts.directory(venue);
        if let Some(Err(why)) = directory.credentials() {
            tracing::error!(
                venue, error = %why,
                "vike-gatewayhub credentialed stream: the credential store exists and would not read"
            );
            return Err(format!(
                "{venue}'s market data is CREDENTIALED, and the credential store could not be read \
                 ({why}) - this is a store fault, not an absent credential; fix the store and retry"
            ));
        }
        (self.build)(venue, sink, &directory)
    }
}

/// WHERE a missing credential goes: the VERB that writes the default account's field and the one that
/// creates the account when there is none (`venue_plan.rs`'s `set_the_fields`, moved).
fn set_the_fields(venue: &str) -> String {
    format!(
        "with `vike-cli secrets set --venue {venue} <FIELD>` on this venue's DEFAULT account, an \
         ACTIVE `demo` row (`vike-cli secrets account add --venue {venue} --tier demo --no-label` \
         creates one; a credential write never does) - it writes the settings database \
         (`settings/db/vike.db`), the only credential store"
    )
}

/// **ibkr's price stream** (any-symbol, decision 0174): the lazy quote client of the DEFAULT `demo` account's IB
/// Gateway session (`vike_ibkr::quote_handle::LazyQuotes`). Building it DIALS NOTHING and subscribes nothing: the
/// first symbol the control door asks a price for dials the gateway's ONE data connection (the same connection
/// the any-symbol lookup of that stock joins) through the gate's data lane, which never delays an order-lane
/// dial and has its own hourly budget. The gateway is the tier's `venue.ibkr.demo.*` rows (`settings`).
#[cfg(feature = "ibkr")]
fn ibkr_stream(
    sink: Arc<dyn LiveDataSink>,
    tier: AccountTier,
    creds: &vike_secrets::account_credentials::AccountCredentials<'_>,
    settings: &VenueSettings,
) -> Result<Box<dyn DataClient + Send>, String> {
    match vike_ibkr::config::load_ibkr_config_from(tier, creds, settings) {
        Some(config) => Ok(Box::new(vike_ibkr::quote_handle::LazyQuotes::for_config(config, sink))),
        None => Err(format!(
            "ibkr's price stream is CREDENTIALED (it rides the logged-in IB Gateway session of an \
             account, and the ACCOUNT field names which), so a stock cannot be priced on absent \
             credentials - give the DEFAULT ibkr account its ACCOUNT field (and a gateway the \
             `venue.ibkr.demo.*` rows can parse) {set}",
            set = set_the_fields("ibkr"),
        )),
    }
}

/// The same arm in a build that links no ibkr bridge: the refusal says so (the lane does not advertise it).
#[cfg(not(feature = "ibkr"))]
fn ibkr_stream(
    _sink: Arc<dyn LiveDataSink>,
    _tier: AccountTier,
    _creds: &vike_secrets::account_credentials::AccountCredentials<'_>,
    _settings: &VenueSettings,
) -> Result<Box<dyn DataClient + Send>, String> {
    Err("gatewayhub: this build links no ibkr bridge (rebuild with `--features ibkr`)".to_string())
}

/// Build one credentialed venue's market-data client, or say WHY not. See the module doc. `ibkr` is the
/// `venue_setting` rows of the ibkr gateway (only ibkr's arm reads a setting).
pub fn build_credentialed(
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
    directory: &AccountDirectory,
    state_dir: Option<&std::path::Path>,
    ibkr: &VenueSettings,
) -> Result<Box<dyn DataClient + Send>, String> {
    let creds = directory.data_feed_credentials(venue);
    match venue {
        "alpaca" => match vike_alpaca::load_alpaca_config_from(FEED_TIER, &creds) {
            Some(config) => Ok(Box::new(vike_alpaca::AlpacaDataClient::new(config, sink, || {}))),
            None => Err(format!(
                "alpaca's market data is CREDENTIALED (the WS auths with the OAuth2 sandbox trio), so \
                 a live alpaca mount cannot run on absent credentials the way a keyless-feed venue \
                 runs paper - give the DEFAULT alpaca account its CLIENT_ID + CLIENT_SECRET + \
                 ACCOUNT_ID fields (the same trio arms sandbox exec) {set}, or drop the live gate for \
                 a paper daemon",
                set = set_the_fields("alpaca"),
            )),
        },

        "ctrader" => {
            let config = vike_ctrader::config::CtraderConfig::from_credentials(FEED_TIER, &creds)
                .ok_or_else(|| {
                    format!(
                        "ctrader's market data is CREDENTIALED (the protobuf socket runs the OAuth \
                         two-stage auth), so a live ctrader mount cannot run on absent credentials \
                         the way a keyless-feed venue runs paper - give the venue its CLIENT_ID + \
                         CLIENT_SECRET (the Spotware app registration, written once for every \
                         account: `vike-cli secrets set --venue ctrader CLIENT_ID`) plus the DEFAULT \
                         ctrader account its ACCESS_TOKEN + REFRESH_TOKEN fields (and optionally \
                         ACCOUNT_ID; the same set arms DEMO exec) {set}, or drop the live gate for a \
                         paper daemon",
                        set = set_the_fields("ctrader"),
                    )
                })?;
            // ⚠ THE ROTATION HOME: this socket refreshes an expiring grant and must write the new pair
            // to the store, or the store keeps the spent one. The grant is SHARED with the exec side's
            // sockets (`CtraderConfig::to_conn_config` joins the process's `SharedGrant`).
            let config = match state_dir {
                Some(dir) => config.with_rotation_home(dir),
                None => config,
            };
            // ⚠ SYNCHRONOUS, and a failure is the open's refusal: a live strategy whose FEED never
            // connected would tick nothing and watch nothing.
            let conn_config = config.to_conn_config();
            let handshake = bounded(CTRADER_HANDSHAKE_BUDGET, move || {
                vike_ctrader::conn::connect_and_auth(conn_config, sink)
            });
            let conn = match handshake {
                Ok(answer) => answer.map_err(|e| {
                    format!(
                        "ctrader DATA connect/auth failed (the dedicated data socket): {e:?} - the \
                         daemon refuses to start rather than mount a feed-less live core; check \
                         demo.ctraderapi.com reachability and the DEMO token pair, then restart"
                    )
                })?,
                Err(NoAnswer::TimedOut) => {
                    return Err(format!(
                        "ctrader DATA connect/auth did not finish within {} s (the dedicated data \
                         socket; the venue answers each handshake step within a second when it is \
                         well) - the daemon refuses to start rather than mount a feed-less live \
                         core; check demo.ctraderapi.com reachability and the DEMO token pair, then \
                         restart",
                        CTRADER_HANDSHAKE_BUDGET.as_secs()
                    ));
                }
                Err(NoAnswer::Panicked) => {
                    return Err(
                        "ctrader DATA connect/auth CRASHED (the handshake thread panicked; \
                         the daemon's log carries the panic) - this is a bug to report, not a slow \
                         venue"
                            .to_string(),
                    );
                }
                Err(NoAnswer::NotStarted(why)) => {
                    return Err(format!(
                        "ctrader DATA connect/auth could not start: the OS refused the handshake \
                         thread ({why}) - the gatewayhub is out of threads or processes; free them \
                         and retry"
                    ));
                }
                Err(NoAnswer::TooMany(running)) => {
                    return Err(format!(
                        "ctrader DATA connect/auth was not attempted: {running} earlier handshakes \
                         are still running (each is abandoned after {} s and ends within the \
                         bridge's own per-step bounds) - the venue is not answering; a new login \
                         would only pile a thread and a concurrent authentication on the account",
                        CTRADER_HANDSHAKE_BUDGET.as_secs()
                    ));
                }
            };
            Ok(Box::new(vike_ctrader::data::CtraderData::new(conn)))
        }

        "ig" => match vike_ig::load_ig_config_from(FEED_TIER, &creds) {
            Some(config) => Ok(Box::new(vike_ig::market_feed::Feeds::new(sink, || {}, config))),
            None => Err(format!(
                "ig's market data is CREDENTIALED (every Lightstreamer subscription opens its own ig \
                 REST login for streaming credentials), so a live ig mount cannot run on absent \
                 credentials the way a keyless-feed venue runs paper - give the DEFAULT ig account \
                 its API_KEY + IDENTIFIER + PASSWORD fields (the same trio arms DEMO exec) {set}, or \
                 drop the live gate for a paper daemon",
                set = set_the_fields("ig"),
            )),
        },

        "oanda" => match vike_oanda::load_oanda_config_from(FEED_TIER, &creds) {
            Some(config) => {
                Ok(Box::new(vike_oanda::market_feed::Feeds::new(sink, || {}).with_config(config)))
            }
            None => Err(format!(
                "oanda's market data is CREDENTIALED on BOTH lanes (the pricing stream and the candles \
                 endpoint are Bearer-authed against one account), so a live oanda mount cannot run on \
                 absent credentials the way a keyless-feed venue runs paper - give the DEFAULT oanda \
                 account its API_KEY + ACCOUNT_ID fields (the same pair arms practice exec) {set}, or \
                 drop the live gate for a paper daemon",
                set = set_the_fields("oanda"),
            )),
        },

        "ibkr" => ibkr_stream(sink, FEED_TIER, &creds, ibkr),

        // No login, no account row, no settings: the stream rides the trade lane's sidecar (module doc).
        "dukascopy" => Ok(Box::new(super::dukascopy_quotes::DukascopyQuotes::on_process_bus(sink))),

        other => Err(format!(
            "gatewayhub: venue `{other}` has no credentialed stream. Table: [{}]",
            roster(Lane::CredentialedStream).join(", ")
        )),
    }
}

#[cfg(test)]
#[path = "credentialed_tests.rs"]
mod credentialed_tests;
