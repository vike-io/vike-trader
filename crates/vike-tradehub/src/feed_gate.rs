//! **The feed gate: an account whose prices cannot be had is NOT RUNNING, with the reason** (wave 2, W12b;
//! decision 0156's "an account runs at its tier or not at all", decision 0164's D7 for the datahub half).
//!
//! # Why it exists
//!
//! Before wave 2 the CEX, Hyperliquid, deribit and polymarket feeds were this process's own sockets, so
//! a datahub or gatewayhub that was down changed nothing about a start: the feed redialled and the
//! strategy waited for its first tick. Now every feed is a dial to the datahub's strategy lane
//! (`feeds.rs`), and a lane that is not there answers a transient refusal at the first subscribe. That
//! refusal used to end the whole start (`ExitCode::FAILURE`, after the core, the restore and the leases
//! were up), which broke the mixed box the late arm exists for: a `paper` account keeping the process
//! while the others wait.
//!
//! The core cannot add or remove an engine at run time, so the decision has to be taken BEFORE it
//! is built, from the one place every engine's fate is read: the venue backend's arming table. This is a
//! [`VenueBackend`] that wraps the real one and, for the mounted venues, answers that every `demo`/`live`
//! account is NOT RUNNING with [`ArmingBlock::FeedsUnavailable`] ("price feeds unavailable (datahub)": the
//! cause is the PRICE path, so the word says so; it used to borrow `gatewayhub unavailable`, which
//! `vike-cli venues` and the GUI contradict by asking the healthy gatewayhub directly) and refuses to start
//! one ([`BackendError::NotRunning`] with the same block). A `paper` account is untouched:
//! its engine is the simulator, it takes no lease and sends nothing to a venue. The reason in words is
//! logged at `error` by `live_runtime_with` (the datahub's, or the missing key's) and the late arm polls
//! the datahub as well as the gatewayhub.
//!
//! The wrapper is the same shape as `remote_backend::wire::with_data_only`: a table overlay plus the one
//! verb (`start_adapter`) that must agree with it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_core::CoreHandle;
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::preflight::{CredentialGap, ServerTimeGap};
use vike_tradehub_core::{
    AdapterBind, AdapterStartQuery, ArmingRow, ArmingTable, BackendError, IdentityRecord,
    NetReading, PreflightPlan, PreflightQuery, VenueBackend, VenueFacts,
};

/// The words the log, the ready banner and the smokes name a held feed with.
pub(crate) const PRICE_FEEDS_UNAVAILABLE: &str = "PRICE FEEDS UNAVAILABLE";

/// **Whether a feed refusal at wiring time can be SURVIVED, with the core already up.** The probe cannot see
/// everything: a datahub that is up while its own gatewayhub link is away passes it (`ProdFeedCtors::datahub_probe`
/// reads what the datahub advertises), and the first subscribe is then refused as a TRANSIENT
/// (`vike_market_stream::market::is_transient_refusal`: a link that is down, an ack that never came, a full
/// budget). That is the same outage the probe holds the feeds for, and it is survived exactly as the probe's
/// verdict is (no feed for the venue, the late arm waits and restarts the tradehub armed), but only where it
/// can be done honestly after the fact: **the core cannot drop an engine once built**, so a venue with a
/// RUNNING `demo`/`live` account (its route key is in `live_venues`) cannot be left without prices and ends the
/// start as before. Where no live account of the venue runs (every one was already NOT RUNNING with its own
/// block, e.g. `gatewayhub unavailable`, or the venue holds paper accounts only) nothing is traded on a missing
/// price: the paper accounts run without prices on that venue and the start goes on. A refusal that is not
/// transient (a bad interval, credentials, an unsupported verb) is a configuration fault and always ends the
/// start, as it did before wave 2.
#[must_use]
pub(crate) fn survivable_refusal(
    refusal: &str,
    venue: &str,
    live_venues: &HashSet<String>,
) -> bool {
    vike_market_stream::market::is_transient_refusal(refusal)
        && !venue_runs_live(venue, live_venues)
}

/// **Whether a CAP refusal (`vike_market_stream::market::CAP_MARK`) of `venue`'s feed can be survived**: the same
/// condition as [`survivable_refusal`] (no `demo`/`live` account of the venue runs, because the core cannot drop an
/// engine), for the class that needs an OPERATOR. A cap persists until someone raises it or drops a subscription, so
/// no restart can mend it: the venue stays UP and priceless, `paper` and every other venue keep running, the
/// banner names the cap, and NO late-arm restart is ordered for it. A venue with a running live account cannot be
/// held after the fact and ends the start with [`failed_start_exit`]'s status, which the supervisor does not
/// restart on (a plain failure exit would loop the whole process every `RestartSec=`, tearing down the other
/// venues' engines each time).
#[must_use]
pub(crate) fn survivable_cap(refusal: &str, venue: &str, live_venues: &HashSet<String>) -> bool {
    vike_market_stream::market::is_cap_refusal(refusal) && !venue_runs_live(venue, live_venues)
}

/// **The exit status of a start that failed after (or before) its core was up**: [`EXIT_OPERATOR_NEEDED`] for a
/// refusal that is a CAP (`vike_market_stream::market::is_cap_refusal`: nothing but an operator mends it, and the
/// unit's `RestartPreventExitStatus=` keeps the supervisor from restarting into it every `RestartSec=`), else the
/// plain failure it always was.
#[must_use]
pub(crate) fn failed_start_exit(error: &str) -> std::process::ExitCode {
    failed_start_code(error).map_or(std::process::ExitCode::FAILURE, std::process::ExitCode::from)
}

/// [`failed_start_exit`]'s special status as a number (`ExitCode` cannot be compared): `Some(`
/// [`crate::late_arm::EXIT_OPERATOR_NEEDED`]`)` for a cap refusal, `None` for the plain failure.
#[must_use]
pub(crate) fn failed_start_code(error: &str) -> Option<u8> {
    vike_market_stream::market::is_cap_refusal(error)
        .then_some(crate::late_arm::EXIT_OPERATOR_NEEDED)
}

/// Whether any `demo`/`live` account of `venue` runs: `live_venues` holds the ROUTE KEYS of the accounts that got
/// a real exec client (the venue id for the default account, `venue#LABEL` for a labelled one).
#[must_use]
pub(crate) fn venue_runs_live(venue: &str, live_venues: &HashSet<String>) -> bool {
    live_venues.iter().any(|key| {
        key == venue || key.strip_prefix(venue).is_some_and(|rest| rest.starts_with('#'))
    })
}

/// **Whether a `demo`/`live` account of any of `venues` would arm through the gatewayhub**: a row of `table` at
/// those tiers that either runs or waits for the gatewayhub (`gatewayhub unavailable`). The late arm's plan for a
/// held-back FEED needs the gatewayhub only for such an account (a restart arms it through the tradehub's own link);
/// a venue with `paper` accounts alone, or whose live rows are blocked for their own reasons (no credentials, not
/// active), gains nothing from the gatewayhub answering, and a box without a gatewayhub key must not wait for it.
#[must_use]
pub(crate) fn arms_through_gateway(table: &ArmingTable, venues: &[&str]) -> bool {
    table.rows().iter().any(|row| {
        let a = &row.arming;
        matches!(a.tier, Some(AccountTier::Demo | AccountTier::Live))
            && venues.contains(&a.venue)
            && (a.block.runs() || a.block == ArmingBlock::GatewayUnavailable)
    })
}

/// How long a start that FAILS after its core is up may spend stopping it. The root's teardown is hard-capped at
/// the profile's `[daemon] shutdown_deadline_ms`; this path has no profile in hand, and a core stuck in a venue
/// call must fail the start (with the process exiting under it, as a hard-capped teardown does) and not hang it.
pub(crate) const FAILED_START_TEARDOWN: Duration = Duration::from_secs(10);

/// **Stop a start that failed after its core was built**: the forwarder is quiesced first (the order the
/// teardown always keeps: a live pump can otherwise wedge the core join), the feeds already wired are released
/// in order (each unsubscribes what it holds, then shuts down) and the core is told to stop and joined, all
/// bounded by [`FAILED_START_TEARDOWN`]. `true` when it finished in time; past the bound it says so at `error`
/// and returns (the process is about to exit, as a hard-capped teardown does).
///
/// What it does NOT do: cancel, flatten or close anything. `Command::Shutdown` folds what is queued, the core
/// detaches its client and saves its state (the journal's tail snapshot), and an order a strategy of an
/// earlier-wired venue already placed before a later venue refused is left resting at its venue for the next
/// start to restore: a restart loop on a deterministic refusal can repeat that. The refusal is the operator's
/// to fix; the cancel sweep lives in the root teardown behind `flags.cancel_orders_on_shutdown`.
pub(crate) fn tear_down_failed_start(
    handle: CoreHandle,
    feeds: Vec<crate::feeds::LiveFeeds>,
    forwarder_stop: &AtomicBool,
) -> bool {
    forwarder_stop.store(true, Ordering::Relaxed);
    let tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = feeds
        .into_iter()
        .map(|mut feed| Box::new(move || feed.shutdown()) as Box<dyn FnOnce() + Send + 'static>)
        .collect();
    let outcome = vike_boot::shutdown::run_with_deadline(
        tasks,
        Box::new(move || handle.shutdown_and_join()),
        FAILED_START_TEARDOWN,
    );
    let graceful = matches!(outcome, vike_boot::shutdown::ShutdownOutcome::Graceful);
    if !graceful {
        tracing::error!(
            outcome = ?outcome,
            bound_s = FAILED_START_TEARDOWN.as_secs(),
            "the start failed after its core was up, and stopping that core did not finish inside the bound; \
             the process exits under it (the journal's last snapshot is the one before the failure)"
        );
    }
    graceful
}

/// `table` with every `demo`/`live` account of a `held` venue that RUNS read as NOT RUNNING with
/// [`ArmingBlock::FeedsUnavailable`]. A `paper` account, an account that already does not run (its own
/// block stands: it is the more specific reason) and every other venue are untouched.
pub(crate) fn hold_for_feeds(table: &ArmingTable, held: &BTreeSet<String>) -> ArmingTable {
    let facts: BTreeMap<String, VenueFacts> = vike_model::VENUES
        .iter()
        .filter_map(|v| table.facts_of(v).map(|f| ((*v).to_string(), f.clone())))
        .collect();
    let rows: Vec<ArmingRow> = table
        .rows()
        .iter()
        .cloned()
        .map(|mut row| {
            let a = &mut row.arming;
            let goes_live = matches!(a.tier, Some(AccountTier::Demo | AccountTier::Live));
            if held.contains(a.venue) && goes_live && a.block.runs() {
                a.effective = None;
                a.block = ArmingBlock::FeedsUnavailable;
            }
            row
        })
        .collect();
    ArmingTable::new(facts, rows)
}

/// **The refusal a credentialed-data venue's feed would meet at the gatewayhub, found BEFORE a core exists.**
///
/// Alpaca, ctrader, oanda and ig authenticate their market data as the venue's DEFAULT `demo` account, with
/// the same key set that arms the account's exec, so the arming table already knows whether the feed can
/// authenticate: that account's row reads [`ArmingBlock::NoCredentials`]. The old plan refused the live start
/// on it before any core spawned; after wave 2 only the gatewayhub's credentialed stream knew, at the first
/// subscribe, after the core, the restore and the leases were up. `Some(why)` is that refusal, in the old
/// plan's shape (what is missing, the verb that writes it, the way out).
///
/// `None` when the venue is not a credentialed-data one (the plan says which), when it is declared `data_only`
/// (its feed account is then a `paper` row the table cannot judge: the gatewayhub's own refusal stands), when
/// the table has no default `demo` row, and for every other block (an unreachable gatewayhub reads
/// `GatewayUnavailable`, which is not a credential verdict).
pub(crate) fn credentialed_feed_refusal(
    venue: &str,
    plan: &crate::VenuePlan,
    table: &ArmingTable,
    data_only: &std::collections::HashSet<String>,
) -> Option<String> {
    use crate::VenuePlan;
    if !matches!(plan, VenuePlan::Alpaca | VenuePlan::Ctrader | VenuePlan::Oanda | VenuePlan::Ig)
        || data_only.contains(venue)
    {
        return None;
    }
    let refused = table.rows().iter().any(|r| {
        r.arming.venue == venue
            && r.arming.is_default_account()
            && r.arming.tier == Some(AccountTier::Demo)
            && r.arming.block == ArmingBlock::NoCredentials
    });
    refused.then(|| {
        format!(
        "{venue}'s market data is CREDENTIALED (it authenticates as the DEFAULT {venue} account), and \
         that account's demo row holds no usable key set (`no credentials`), so a live {venue} mount \
         cannot run the way a keyless-feed venue runs paper: refusing the mount before any core, \
         restore or lease exists. Give the DEFAULT {venue} account its fields with `vike-cli secrets \
         set --venue {venue} <FIELD>` (the same set arms demo exec; `vike-cli secrets account add \
         --venue {venue} --tier demo --no-label` creates the row, a credential write never does) - it \
         writes the settings database (`settings/db/vike.db`), the only credential store - or drop \
         the live gate for a paper daemon"
        )
    })
}

/// A [`VenueBackend`] that holds the `demo`/`live` accounts of `held` venues NOT RUNNING because their
/// prices cannot be had (the module doc). Everything else is the inner backend's, untouched.
pub(crate) struct FeedGatedBackend {
    inner: Arc<dyn VenueBackend>,
    held: BTreeSet<String>,
    why: String,
}

impl FeedGatedBackend {
    /// `inner` with `held` venues' live accounts held; `why` is the words a refused start carries.
    pub(crate) fn wrap(
        inner: Arc<dyn VenueBackend>,
        held: BTreeSet<String>,
        why: String,
    ) -> Arc<dyn VenueBackend> {
        Arc::new(Self { inner, held, why })
    }
}

impl VenueBackend for FeedGatedBackend {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        Ok(hold_for_feeds(&self.inner.arming_table()?, &self.held))
    }

    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        let mut plan = self.inner.preflight_plan(q)?;
        // A credential read for an account that will not run is a probe of nothing.
        plan.credential_venues.retain(|v| !self.held.contains(v));
        Ok(plan)
    }

    fn server_time_ms(&self, venue: &str, tier: AccountTier) -> Result<i64, ServerTimeGap> {
        self.inner.server_time_ms(venue, tier)
    }

    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        self.inner.authed_read(venue)
    }

    fn net_reading(&self) -> NetReading {
        self.inner.net_reading()
    }

    fn report_not_running(&self, venue: &str, rows: &[vike_config::VenueArming]) {
        self.inner.report_not_running(venue, rows);
    }

    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        events: &EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        // The table above already says these do not run, so the fold does not ask; this is the belt for a
        // caller that does (a start never reaches a venue for an account the table holds back).
        if self.held.contains(q.venue) && q.tier != AccountTier::Paper {
            tracing::error!(venue = q.venue, "{}", self.why);
            return Err(BackendError::NotRunning(ArmingBlock::FeedsUnavailable));
        }
        self.inner.start_adapter(q, events, trigger)
    }

    fn record_identity(
        &self,
        q: &IdentityRecord<'_>,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), BackendError> {
        self.inner.record_identity(q, recon)
    }

    /// FORWARDED: the trait default is a no-op, and the lease the refused start opened is the inner
    /// backend's to forget.
    fn release_refused_start(&self, venue: &str, account: &AccountLabel, route_key: &str) {
        self.inner.release_refused_start(venue, account, route_key);
    }
}

#[cfg(test)]
#[path = "feed_gate_tests.rs"]
mod feed_gate_tests;
