//! **The account watcher: an account follows its `active` flag in the RUNNING tradehub** (decision
//! 0169, slice S1 — a `demo` account is STARTED; slice S2 — an account is STOPPED; slice S3 — a `live`
//! account and a venue the node mounted nothing for are STARTED, and a transient refusal is retried).
//!
//! One named thread (`account-watch`, outside `vike-core`, the shape of `gateway-late-arm`) holds a
//! read-only connection to the settings database and asks `PRAGMA data_version` once a second
//! ([`vike_secrets::StoreWatch`]); the number moves exactly when ANOTHER connection committed, so
//! `vike-cli account activate`, a hand edit and a restored database are all seen, and a
//! commit is never seen half written. On a change it re-reads the account rows, resolves every
//! account through the ONE authority the boot path calls (`vike_config::account_arming::account_tier`)
//! and acts on the difference between what the rows say and what runs. STOPS come first, then starts:
//!
//! * a RUNNING account whose row now says `account inactive` (or whose last non-`paper` row was
//!   deactivated, a `paper` sibling remaining) is STOPPED, in the order of decision 0169 item 4: it is
//!   put on the published NOT RUNNING list FIRST (the control edge then refuses every new order for
//!   it, with the reason), then [`HotAccounts::stop_account`] retires its reconcile leg and tells the
//!   core to cancel its OWN resting orders, begin its client's detach and tombstone its engine. A
//!   position it holds is NOT closed and NOT hidden: it is named in the reason ("position open at
//!   venue") and logged at `error`. The account stays stopped until its answer changes, and a later
//!   start appends a new engine;
//! * an account whose row says it runs and that has no engine is STARTED, one at a time, through
//!   [`HotAccounts::start_account`] (the code startup runs for an account, over a fresh arming
//!   table): the engine joins the running core, the account's reconcile leg joins the driver, and the
//!   published NOT RUNNING list loses it;
//! * a `live` account is started through the SAME gates as boot (`HotAccounts::start_account`'s module
//!   doc lists them), and a `live` start with `flags.tradehub_live` off is refused
//!   (`ArmingBlock::LiveSwitchOff`) here, where the start is decided;
//! * a refusal is that one account NOT RUNNING with its reason, said at `ARMING:`, and never a panic,
//!   a refused node or a restart. A refusal that is only a matter of time is **retried with a
//!   backoff** ([`poll_ladder`]: [`FIRST_POLL`] doubling to `MAX_POLL`, the late arm's own pauses): the gatewayhub
//!   that did not answer (`gatewayhub unavailable`, also an account the node booted without for want
//!   of it: the late-arm restart is not ordered for those any more, decision 0169 item 9), and an
//!   `adapter failed` within [`LEASE_CLOSE_GRACE`] of this watcher's own stop of the same account (its
//!   old lease was still closing, so switching an account off and on within seconds works), and an account whose
//!   venue could not be reached at its adapter start or whose SDK is not up (`venue unreachable`, `SDK not loaded`:
//!   an IB Gateway that was down), at the boot or later, until it starts: it joins the running core by itself, with
//!   its deactivated mounts and the book the boot held for it, when its venue connects (owner 2026-11-15; a node of
//!   such accounts alone starts with no engine, `vike_core::spawn_core_without_accounts`). [`is_transient`] is the
//!   one list. A REFUSAL (`adapter failed`: a key the venue declined; `preflight failed`) is said once and parks. Any other
//!   refusal stands until the rows change or the gatewayhub says what that start reads has moved (a key
//!   corrected with `vike-cli secrets set`, a venue setting, a flag: [`AccountFollowWatcher::follow_key_changes`]), and
//!   then that account is asked again once: nothing loops on it;
//! * **an account of an FX venue the gatewayhub did not start because the FX market is closed**
//!   (`ArmingBlock::FxMarketClosed`, owner ruling 2026-11-13: no login while it is closed) is NOT RUNNING with the
//!   words that name the open, and is asked again ONCE, at that open ([`fx_retry_due`]), never on a backoff; one the
//!   boot met closed is seeded the same way;
//! * a RUNNING account whose row now states ANOTHER tier (`demo` to `live`, a `paper` row left alone beside
//!   a deactivated `demo` one, ...) is STOPPED the same way and STARTED again at the tier the row states, in
//!   the same pass (decision 0169 item 11, slice S4): the stop's cancel of its own resting orders and the
//!   close of its session come first, the start crosses every gate a start crosses, and a refusal is that
//!   account NOT RUNNING with its reason, never the old tier and never a stand-in;
//! * **a RUNNING account whose row is RENAMED is the SAME account** (`AccountFollowWatcher::follow_renames`, keyed by the row's
//!   id): it keeps its one engine and its one lease under the label it started with, nothing is started under the
//!   new label (the gatewayhub keys a session by `(venue, label)`, so that would be a second session and a second
//!   engine on the same keys), and the row's answer under the new label still stops it or changes its tier. The new
//!   label takes effect at the account's next stop and start; said once at `warn`;
//! * what is not followed yet is REPORTED, never ignored (item 11): an account whose price feed was not wired
//!   at boot (that venue still waits for the late-arm restart), and a RUNNING account whose row was deleted
//!   or stopped answering (an unreadable table is never taken for "no accounts") — each an `error` line
//!   saying so, once;
//! * **a key changed under a RUNNING account is never a stop of the account here** (owner ruling 2026-10-17:
//!   stopping the account cancels its resting orders). The credential verbs tell the running gatewayhub instead
//!   (decision 0287, the owner's ruling of 2026-11-16), which reconnects that account's adapter alone: this watcher
//!   sees nothing of it but its lease dropping once and its client's re-teaching. What no verb told (a hand edit, a
//!   restored database, a verb that could not reach the gatewayhub) is a WARNING: the
//!   gatewayhub, which alone holds the key values (this tradehub reads none), fingerprints the
//!   key set each adapter started with and names the accounts whose key in the database differs; the
//!   watcher asks on every change of the rows, lists them in the observe frame (`CoreSnapshotWire::key_changed`)
//!   and in one `ARMING:` line, and drops the entry when the account stops or starts anew.
//!
//! * **a lane twin (binance's `PERP`, derived from the default account's row while it states a margin leverage)
//!   whose row vanished is kept UP while it holds a position or an order, and stopped once it is flat**
//!   (`AccountFollowWatcher::follow_lane_twins`): the stop would cancel its protection and leave the position with
//!   nothing watching and no vike path to close it.
//!
//! * **an account whose venue stated its currency AFTER the engine was started without it is stopped and started
//!   again, when it holds nothing** (`AccountFollowWatcher::remount_asked`, asked by the lease's worker through
//!   `venue_backend::gateway::late_currency::request_remount`; no row changed): the engine was anchored to an assumed
//!   currency on the mount its row states, and the start that follows moves the mount (fxcm's GBP account: `EURGBP`)
//!   exactly as a boot would. A position or a live order makes it ONE `warn!` and changes nothing.
//!
//! A writer that already knows it committed (the account-administration verbs of the observe server) may
//! [`kick`] the thread so the row is followed within one slice instead of one poll; the row stays the one
//! authority and the poll stays the only trigger a hand edit has.
//!
//! The decision (what is wanted, what runs, what to say) is a plain state machine over the `AccountFollowWorld`
//! trait, so the boundary cases are tests with a scripted world; `AccountFollowWorldLive` is the production one.
//! The late arm stays the fallback for the price feeds of a venue (`late_arm.rs`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use vike_config::account_arming::{account_tier, known_accounts};
use vike_execution::ArmingNotRunningView;
use vike_model::AccountTier;
use vike_model::ArmingBlock;
use vike_model::accounts::account_keys::{
    AccountLabel, AccountNamed, label_of_route_key, route_key_of,
};
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub_client::wire::KEY_CHANGED_WARNING;
use vike_tradehub_core::{
    ArmingNotRunning, HotAccountStart, HotAccountStop, HotAccounts, ReconcileLegChange,
    VenueBackend,
};
use vike_utility::errors::{CarriedRefusal, Declared};
use vike_venues::reconnect::ReconnectBackoff;

use crate::late_arm::{FIRST_POLL, poll_ladder};
use crate::refusals::NotRunningReason;
use crate::venue_backend::gateway::trade_lease_registry::{TradeLeaseRegistry, rows_arm_a_cap};

/// How often the rows are asked whether another connection committed. A constant of the watcher, not
/// a setting (decision 0169 item 3): `data_version` is a header read, and the worst-case delay of a
/// start is one tick.
pub const POLL: Duration = Duration::from_secs(1);

/// How long after this watcher stopped an account a refusal of its start as `adapter failed` is taken
/// for the old lease still closing and retried: the client's own goodbye bound is 3 s
/// (`vike_gatewayhub_client::trade::STOP_BOUND`), and a gatewayhub that fences the old connection first
/// adds its fence, so the grace is ten times that. Past it the refusal is the venue's own and stands
/// until the rows change.
pub const LEASE_CLOSE_GRACE: Duration = Duration::from_secs(30);

/// How finely the thread watches its stop flag between polls.
const SLICE: Duration = Duration::from_millis(100);

/// How long a stop waits for the venue to answer the cancels of the account's resting orders before it
/// closes the lease anyway: longer than the lease's own ack drain (2 s), so a healthy venue always settles
/// inside it, and short enough that a dead one does not hold the other accounts' follow for long.
const CANCEL_SETTLE: Duration = Duration::from_secs(5);

/// How often a stop looks at the published snapshot while it waits.
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// A maker of backends over a FRESH arming table (the gatewayhub is asked again, now), for a start of
/// an account the boot table did not know. `VenueBackendFactoryGateway`'s; `None` for a backend that cannot follow.
pub type AccountFollowBackend =
    Box<dyn Fn(AccountDirectory, HashSet<String>) -> Arc<dyn VenueBackend> + Send + Sync>;

/// The settings database as the watcher sees it: a commit counter and the account rows.
pub struct AccountFollowRows {
    /// Opened BEFORE the boot read the account rows, so a commit made while the node was assembling is
    /// reported by the first poll instead of being lost between the boot's read and the watch.
    pub watch: vike_secrets::StoreWatch,
    /// The account rows now (the boot's own reader, so the two cannot disagree about what a row is).
    pub read: Box<dyn Fn() -> AccountDirectory + Send>,
}

/// Everything the watcher needs, assembled by the live mount.
pub struct AccountFollowParts {
    /// What starts one more account in the core.
    pub hot: HotAccounts,
    /// The rows to follow.
    pub rows: AccountFollowRows,
    /// A maker of fresh backends; `None` = this mount cannot follow (an in-process backend), and every
    /// start is reported as a restart.
    pub fresh_backend: Option<AccountFollowBackend>,
    /// The venues the mount withholds exec credentials from (`data_only`): applied to the rows of
    /// every start as the boot applied them.
    pub data_only: HashSet<String>,
    /// Venues whose price feed was NOT wired at boot (held back or capped): an account started there
    /// would trade without prices, so it keeps the restart path.
    pub held_venues: HashSet<String>,
    /// The network Hyperliquid's price feed was wired for at the boot (a mount's feed or the price-only one),
    /// `None` when none was: a `live` account hot-started on a TEST-network feed would be priced and
    /// ceiling-checked off the test book, so it keeps the restart path ([`hyperliquid_feed_refusal`]).
    pub hl_feed_network: Option<vike_model::VenueNetwork>,
    /// The reconcile driver's leg mailbox; `None` when reconcile is off.
    pub reconcile_legs: Option<Sender<ReconcileLegChange>>,
    /// The accounts the node did not run at boot, with why: the list the watcher keeps current.
    pub not_running: Vec<ArmingNotRunningView>,
    /// `flags.tradehub_live`, as the mount was booted with it (a restart-class setting): a `live`
    /// account is not started while it is off.
    pub live_switch: bool,
    /// The control door's facts registry (`None` on a mount without one): told what each hot-started
    /// engine's account ceiling is, and when an engine stops, so the door answers for the engine that
    /// runs and not for the boot's rows (`TradeLeaseRegistry::engine_starting`, `TradeLeaseRegistry::engine_stopped`).
    pub facts: Option<Arc<TradeLeaseRegistry>>,
    /// Whether the box-wide `policy.max_account_exposure` row was set at the boot (the figure every
    /// engine, hot-started ones included, is built under).
    pub policy_cap: bool,
}

/// **Run one start so the door knows the ceiling of the engine it builds BEFORE that engine exists**:
/// the ceiling is read from the rows the start is built from and announced (with a new engine
/// generation) ahead of `start`, so the door never reads the previous engine's answer for the new one;
/// a start that built no engine puts the previous answer back.
pub(crate) fn start_with_cap(
    facts: Option<&TradeLeaseRegistry>,
    policy_cap: bool,
    accounts: &AccountDirectory,
    venue: &str,
    label: &AccountLabel,
    start: impl FnOnce() -> HotAccountStart,
) -> HotAccountStart {
    let Some(facts) = facts else { return start() };
    let key = route_key_of(venue, label);
    let before = facts.engine_starting(&key, rows_arm_a_cap(policy_cap, accounts, venue, label));
    let outcome = start();
    if !matches!(outcome, HotAccountStart::Started { .. }) {
        facts.engine_start_refused(&key, before);
    }
    outcome
}

/// The symbol the DERIVED lane twin `label` of `venue` is mounted on (the symbol its derived row states: its parent's
/// `lane_twin_symbol` fact), or `None` for any other account and for a twin whose parent states none. The rows MARK a
/// derived twin ([`AccountDirectory::is_derived_lane_twin`]); an operator's own account that spells the label is a
/// real account and is not asked.
pub(crate) fn derived_twin_symbol(
    accounts: &AccountDirectory,
    venue: &str,
    label: &AccountLabel,
) -> Option<String> {
    let text = label.text()?;
    if !accounts.is_derived_lane_twin(venue, text) {
        return None;
    }
    vike_config::account_arming::account_symbol(Some(accounts), venue, label).map(str::to_string)
}

/// **Open a started lane twin's price lane at once** (`TradeLeaseRegistry::warm_price`). The twin's mounted symbol
/// (`BTCUSDT.P`) is on no strategy mount, so no boot feed prices it and nothing else would until the door's
/// first opening order asked, which then waited out the facts look-up, the datahub dial and the pump's first
/// frame. Asked here, the lane is priced by the time the venue balance has landed and the door lets an
/// opening order through. On a thread of its own (a lease round trip and a datahub subscribe, each with its
/// own bound, must not hold up the following of the other accounts' rows); a thread that cannot start is
/// said, and the door's own ask stands.
fn warm_twin_price(
    facts: &Arc<TradeLeaseRegistry>,
    accounts: &AccountDirectory,
    venue: &str,
    label: &AccountLabel,
) {
    let Some(symbol) = derived_twin_symbol(accounts, venue, label) else { return };
    let (facts, route_key, venue) =
        (Arc::clone(facts), route_key_of(venue, label), venue.to_string());
    let spawned = std::thread::Builder::new()
        .name("twin-price-warm".to_string())
        .spawn(move || facts.warm_price(&route_key, &venue, &symbol));
    if let Err(e) = spawned {
        tracing::warn!(
            error = %e,
            "the lane twin's price lane could not be opened at its start; its first order asks"
        );
    }
}

/// **Wait, at most `bound` ([`vike_tradehub_core::FIRST_BALANCE_BOUND`] in production), until the door no longer holds `route_key` for its venue balance**
/// (decision 0246): the started engine judges orders on `seed_cash` until its leg's first pass lands the venue's
/// figure, and the account is reported as running only after that, so an operator who sees it running can trade it.
/// Returns at once for an account no start announced a wait for (a `paper` account, a start with no reconcile
/// leg). Blocks the ONE `account-watch` thread like [`AccountFollowWorldLive::await_cancels`] does, so for up to the bound no
/// other account's start, stop or retry is looked at; it ends at once when the process stop flag is raised. A
/// venue that did not answer in time is said once, and the door's hold (with the leg's reason) is what is left.
fn await_first_balance(
    facts: &TradeLeaseRegistry,
    route_key: &str,
    stop: &AtomicBool,
    bound: Duration,
) {
    use crate::server::symbol_admission::AccountReadiness;
    let started = Instant::now();
    while facts.awaiting_balance(route_key).is_some() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if started.elapsed() >= bound {
            let why = facts.awaiting_balance(route_key).map(|w| w.why).unwrap_or_default();
            tracing::warn!(
                "ARMING: {route_key} is RUNNING, but its venue balance has not reached its engine after \
                 {bound:?} ({why}); the door refuses its OPENING orders, by name, until the reconcile \
                 pass answers. Reducing orders, cancels and exits are not held"
            );
            return;
        }
        std::thread::sleep(SETTLE_POLL);
    }
}

/// What the rows say about an account that may be a LANE TWIN (`vike_secrets::AccountRows::derive_lane_twins`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountFollowTwin {
    /// An ordinary account: the row is the operator's own.
    No,
    /// A twin the rows DERIVE now (`AccountDirectory::is_derived_lane_twin`).
    Derived,
    /// A twin that runs and whose derived row is gone from the view ([`AccountFollowWatcher::follow_lane_twins`]).
    Vanished,
}

/// One account the rows name, resolved through the one authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountFollowTarget {
    pub(crate) venue: String,
    pub(crate) label: AccountLabel,
    pub(crate) route_key: String,
    /// The tier it runs at, or the block that says it does not.
    pub(crate) tier: Result<AccountTier, ArmingBlock>,
    pub(crate) twin: AccountFollowTwin,
    /// The id of the active row it runs on at that tier ([`row_id`]); `None` for a derived lane twin (its row
    /// carries its parent's id) and an account that does not run. What a rename leaves in place.
    pub(crate) id: Option<i64>,
}

/// Every account the rows know of on any roster venue, resolved by `account_tier`: the function the
/// boot path calls, so a row means the same thing to both.
pub(crate) fn wanted_accounts(accounts: &AccountDirectory) -> Vec<AccountFollowTarget> {
    let mut out = Vec::new();
    for venue in vike_model::VENUES.iter() {
        for label in known_accounts(venue, Some(accounts)) {
            let tier = account_tier(Some(accounts), venue, &label);
            let derived =
                label.text().is_some_and(|text| accounts.is_derived_lane_twin(venue, text));
            let id = match tier {
                Ok(tier) if !derived => row_id(accounts, venue, &label, tier),
                _ => None,
            };
            out.push(AccountFollowTarget {
                venue: (*venue).to_string(),
                route_key: route_key_of(venue, &label),
                label,
                tier,
                twin: if derived { AccountFollowTwin::Derived } else { AccountFollowTwin::No },
                id,
            });
        }
    }
    out
}

/// The id of the active row `label`'s account of `venue` runs on at `tier` (the lowest of several, the book
/// `AccountDirectory::account_row` picks), or `None` when the rows hold none.
fn row_id(
    accounts: &AccountDirectory,
    venue: &str,
    label: &AccountLabel,
    tier: AccountTier,
) -> Option<i64> {
    let rows = accounts.rows()?.ok()?.known()?;
    rows.iter()
        .filter(|r| {
            r.active && r.venue == venue && r.tier == tier && r.label.as_deref() == label.text()
        })
        .map(|r| r.id)
        .min()
}

/// The route key row `id` carries NOW, active or not, or `None` when no row has that id or the rows were not
/// read. A derived lane twin's row carries its parent's id and is never the one.
fn route_key_of_row(accounts: &AccountDirectory, id: i64) -> Option<String> {
    let rows = accounts.rows()?.ok()?.known()?;
    let row = rows.iter().find(|r| {
        r.id == id
            && !r.label.as_deref().is_some_and(|l| accounts.is_derived_lane_twin(&r.venue, l))
    })?;
    let label = match row.label.as_deref() {
        None => AccountLabel::Default,
        Some(text) => AccountLabel::parse(text).ok()?,
    };
    Some(route_key_of(&row.venue, &label))
}

/// **The route key of every account the rows configure, active or not, by venue, or why they could not be read**:
/// what the door counts a bare venue by (decision 0254). A derived lane twin is one account's second lane, no account,
/// so it is left out; a default account nobody wrote a row for is no account either, which is why this does not ask
/// [`known_accounts`]. A label the grammar refuses names no addressable account, as there.
pub(crate) fn configured_accounts(
    accounts: &AccountDirectory,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let rows = match accounts.rows() {
        Some(Ok(rows)) => rows,
        Some(Err(why)) => return Err(why.to_string()),
        None => return Err("the rows were not read".to_string()),
    };
    let rows = rows.known().ok_or("this box has no account table")?;
    let mut by_venue: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        let label = match row.label.as_deref() {
            None => AccountLabel::Default,
            Some(text) if accounts.is_derived_lane_twin(&row.venue, text) => continue,
            Some(text) => match AccountLabel::parse(text) {
                Ok(label) => label,
                Err(_) => continue,
            },
        };
        let keys = by_venue.entry(row.venue.clone()).or_default();
        let key = route_key_of(&row.venue, &label);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Ok(by_venue)
}

/// The route keys of the operator's REAL accounts that spell a lane twin's label: the rows hold an account under
/// the label (so `derive_lane_twins` stands down for the venue), and it is not a derived twin. What the control
/// door must be told, because the published block of such an engine is the derived twin's to the letter when it is
/// mounted on the twin's symbol ([`AccountFollowWorld::publish_lane_twin_flags`]).
fn real_twin_label_keys(wanted: &[AccountFollowTarget]) -> Vec<String> {
    wanted
        .iter()
        .filter(|w| {
            w.twin == AccountFollowTwin::No
                && w.label
                    .text()
                    .is_some_and(|text| vike_venues::is_lane_twin_label(&w.venue, text))
        })
        .map(|w| w.route_key.clone())
        .collect()
}

/// How often the ERROR for a lane twin that is kept up (it still holds something) is repeated.
const TWIN_HELD_REPEAT: Duration = Duration::from_secs(60);

/// The least time between two ACCEPTED late-currency restarts of one account (`AccountFollowWatcher::remount_asked`). A restart
/// logs the venue out and in again, and a login that meets a transient conflict right after the teardown is started
/// on an assumed currency again, which arms the watch and, when the retry opens the session, asks for another
/// restart: each one refuses the account's orders for the length of a start, so the loop is bounded here.
const REMOUNT_WINDOW: Duration = Duration::from_secs(600);

/// The most late-currency restarts of one account in this process, whatever the time between them.
const REMOUNT_MAX: usize = 3;

/// The reason a lane twin of `venue` is listed with when it stops because its row went (the operator
/// un-stated the margin leverage, or the parent row stopped being a default account of a tier the twin is
/// derived at): the sentence the door's refusal carries (`not_running_refusal`), so the operator hears what
/// to do and not "account inactive" about an account that is active. `ran_at` is the tier the engine ran
/// at, so a `live` twin is told to state a leverage on the LIVE account and never sent to the demo one
/// (`None`, a tier nobody recorded, names both).
///
/// `conflict` is the other way a twin's row goes while the figure is still stated: the default account's
/// rows state TWO active tiers (a demo and a live one), the account does not run
/// (`ArmingBlock::TierConflict`, decision 0156) and the table derives no twin of either. The sentence then
/// says that, never "state a leverage" about an account that has one.
pub(crate) fn lane_twin_stopped_why(
    venue: &str,
    ran_at: Option<AccountTier>,
    conflict: bool,
) -> CarriedRefusal {
    let venue = venue.to_string();
    if conflict {
        return NotRunningReason::PerpLaneTwoTiers { venue }.carried();
    }
    let tier = ran_at.map_or("demo or live", AccountTier::as_str);
    NotRunningReason::PerpLaneNoLeverage { venue, tier }.carried()
}

/// What the rows say NOW about an account that runs, when that is not "keep running".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// The row went inactive: stop it and leave it stopped until its answer changes.
    Stop,
    /// The row states ANOTHER tier: stop it, then start it at that tier.
    Restart,
}

/// **Does the answer the rows give NOW change an account that runs?** Decision 0169 item 1: `active = 0`
/// stops it. The answer is `AccountInactive` when every row of the account is inactive: a [`Change::Stop`].
/// A different TIER (the row was changed in place, or the last non-`paper` row was deactivated while a
/// `paper` one stays active, or the reverse) is a [`Change::Restart`]: the account runs at the tier its
/// active row states or not at all (decision 0156), so the engine of the old tier goes and one of the new
/// tier is started. Anything else is NOT a change here (a deleted row, an unreadable table, two active
/// tiers): it is reported, and the account keeps running. In particular `NoAccountRow` and
/// `AccountNotInStore` are what an unread or unreadable table answers, and a stop must never be decided on
/// the absence of an answer.
fn change(ran_at: Option<AccountTier>, now: &Result<AccountTier, ArmingBlock>) -> Option<Change> {
    match now {
        Err(ArmingBlock::AccountInactive) => Some(Change::Stop),
        Ok(tier) if ran_at.is_some_and(|ran| ran != *tier) => Some(Change::Restart),
        Ok(_) | Err(_) => None,
    }
}

/// The reason an account is listed with when it stops: the arming word (a badge, no id), and each position it
/// still holds at the venue (`SYMBOL size`), because a stop does not close a position and must not hide one.
pub(crate) fn stop_reason(open: &[(String, f64)]) -> CarriedRefusal {
    with_positions(ArmingBlock::AccountInactive.as_str().into(), open)
}

/// The key under which the warning about an engine that cannot be started again is said once.
fn remount_held_key(key: &str) -> String {
    format!("remount-held:{key}")
}

/// The reason an account is listed with while it restarts at another tier: it is not inactive, it is about
/// to run again. The positions it holds are named for the reason a stop names them.
pub(crate) fn restart_reason(to: AccountTier, open: &[(String, f64)]) -> CarriedRefusal {
    with_positions(NotRunningReason::TierChanged { to }.carried(), open)
}

/// What an account that runs is stopped FOR ([`AccountFollowWorld::stopping`]): which words lead its NOT RUNNING
/// row and which block a reader keys on, set here rather than rewritten into the words afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountStop<'a> {
    /// Its rows went inactive: [`stop_reason`], keyed `ArmingBlock::AccountInactive`.
    Inactive,
    /// Its row states another tier, which it comes back at: [`restart_reason`], no block.
    Restart(AccountTier),
    /// A lane twin whose row went, in the sentence that says what to do ([`lane_twin_stopped_why`]): no block.
    TwinGone(&'a CarriedRefusal),
}

impl AccountStop<'_> {
    /// The row's reason (its id when it is the node's own sentence), with each position `open` still holds at the
    /// venue, and its block.
    pub(crate) fn entry_words(
        self,
        open: &[(String, f64)],
    ) -> (CarriedRefusal, Option<ArmingBlock>) {
        match self {
            AccountStop::Inactive => (stop_reason(open), Some(ArmingBlock::AccountInactive)),
            AccountStop::Restart(to) => (restart_reason(to, open), None),
            AccountStop::TwinGone(why) => (with_positions(why.clone(), open), None),
        }
    }
}

/// `word`, or with a position open the row that names it: its own id (`position_open`) is the news, the words it
/// wraps held bare.
fn with_positions(word: CarriedRefusal, open: &[(String, f64)]) -> CarriedRefusal {
    if open.is_empty() {
        return word;
    }
    let held: Vec<String> = open.iter().map(|(symbol, size)| format!("{symbol} {size}")).collect();
    NotRunningReason::PositionOpen { reason: word.to_string(), held: held.join(", ") }.carried()
}

/// How many orders of `route_key` are still live in the published `orders`.
pub(crate) fn live_orders_of(orders: &[vike_execution::OrderView], route_key: &str) -> usize {
    orders
        .iter()
        .filter(|o| {
            o.status.is_live()
                && route_key_of(&o.venue, &o.account.clone().unwrap_or_default()) == route_key
        })
        .count()
}

/// The positions the newest engine of `route_key` holds, from the published engine blocks. The NEWEST,
/// because a stopped account that started again has two blocks under one key and the old one is history.
pub(crate) fn open_positions(
    blocks: &[vike_execution::VenueBlock],
    route_key: &str,
) -> Vec<(String, f64)> {
    blocks
        .iter()
        .rfind(|b| b.route_key == route_key)
        .map(|b| {
            b.positions
                .iter()
                .filter(|p| p.size.abs() > 1e-12)
                .map(|p| (p.symbol.to_string(), p.size))
                .collect()
        })
        .unwrap_or_default()
}

/// **Whether the published snapshot says `route_key`'s engine holds something**: an open position
/// ([`open_positions`]) or a live order ([`live_orders_of`]), the two reads a stop makes. ⚠ With no publisher (no
/// observe address, so no snapshot) NOTHING can be read, and that is answered `true`: "unknown" is not "flat".
/// This is the question a kept-up lane twin is stopped on, and a wrong "flat" cancels a protective order and
/// leaves a position with nothing watching it.
fn holds_something(publisher: Option<&crate::publish::PublisherHandle>, route_key: &str) -> bool {
    let Some(publisher) = publisher else { return true };
    !open_positions(&publisher.engine_blocks(), route_key).is_empty()
        || live_orders_of(&publisher.published_orders(), route_key) > 0
}

/// What one dial of the gatewayhub said about the keys. Route keys and counters: this process reads no value.
#[derive(Debug, Default)]
pub(crate) struct AccountFollowKeyAnswer {
    /// The accounts that run on a key set the database no longer holds.
    pub(crate) changed: BTreeSet<String>,
    /// Route key -> how far what a start of that account reads has moved (`ArmingTable::start_inputs`).
    pub(crate) start_inputs: BTreeMap<String, u64>,
}

/// What the watcher touches. The production one is [`AccountFollowWorldLive`].
pub(crate) trait AccountFollowWorld {
    /// Whether another connection committed to the settings database since the last call. `Err` is a
    /// store that exists and cannot be read (said at `error`; what runs does not change).
    fn rows_changed(&mut self) -> Result<bool, String>;
    /// The account rows now.
    fn accounts(&mut self) -> AccountDirectory;
    /// Tell the door what the rows configure ([`configured_accounts`]), or why they could not be read.
    fn publish_configured_accounts(&mut self, _: Result<BTreeMap<String, Vec<String>>, String>) {}
    /// Try to start ONE account. Blocking.
    fn start(
        &mut self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> HotAccountStart;
    /// The accounts that do not run, as they stand: the wire's list.
    fn publish_not_running(&mut self, list: &[ArmingNotRunningView]);
    /// The entry `route_key` is listed with once stopped for `stop` ([`AccountStop::entry_words`]) and the positions it
    /// holds (said at `error` when there are any). Reads only; stops nothing.
    fn stopping(
        &mut self,
        venue: &str,
        label: &AccountLabel,
        route_key: &str,
        tier: Option<AccountTier>,
        stop: AccountStop<'_>,
    ) -> ArmingNotRunningView;
    /// Stop ONE account, phase one: its reconcile leg is retired and the core cancels its orders and refuses
    /// new ones. Non-blocking; the session is closed by [`Self::settle`].
    fn stop(&mut self, entry: &ArmingNotRunningView) -> HotAccountStop;
    /// Phase two for every account `stop` answered `Stopped` in one pass: wait ONCE, bounded, for the venue
    /// to answer their cancels, then close all their sessions.
    fn settle(&mut self, stopped: &[ArmingNotRunningView]);
    /// The clock the retry backoff is read on.
    fn now(&mut self) -> Instant;
    /// **The wall clock, epoch ms**: when the FX week opens for an account its close refused ([`fx_retry_due`]), and
    /// the reason that names that open. DEFAULT the process's.
    fn wall_ms(&mut self) -> i64 {
        vike_model::now_ms()
    }
    /// **When `venue`'s kept instrument list was last written** (`<project>/instruments/<venue>.json`, the datahub's
    /// refresh), `None` when there is none or no project: what tells the watcher an operator refreshed it
    /// ([`AccountFollowWatcher::ask_again_when_a_list_moved`]). DEFAULT the booted project's file.
    fn kept_list_written(&mut self, venue: &str) -> Option<std::time::SystemTime> {
        let dir = vike_paths::state_path::instruments_dir_beside(
            crate::tradehub_cli::settings::booted_settings_dir(),
        )?;
        std::fs::metadata(vike_catalog::kept_file(&dir, venue)).and_then(|m| m.modified()).ok()
    }
    /// **Whether the engine of `route_key` still HOLDS something at the venue**: an open position, or an
    /// order that is live (resting at the venue, or held by the core as an exit or a conditional), as the
    /// published snapshot says. A read, never the fold. ⚠ `true` when nothing can be read (no observe
    /// address, so no snapshot): an answer nobody can give is not "flat", and a stop that would cancel a
    /// protective order and leave a position unwatched must not rest on it. A lane twin whose row is gone is
    /// then KEPT UP (said at ERROR) until a restart, or until a snapshot can say it is flat.
    fn has_exposure(&mut self, route_key: &str) -> bool;
    /// **The watcher's word on the lane twins, for the control door** ([`AccountFollowWatcher::publish_twin_flags`]):
    /// `real` are the route keys of the operator's own accounts that spell a twin's label (no twin is derived
    /// for them), `closing` the twins kept up only to be closed. Called when either changed. DEFAULT nothing.
    fn publish_lane_twin_flags(&mut self, real: &[String], closing: &[String]) {
        let _ = (real, closing);
    }
    /// **The route keys of the lane twins the rows DERIVED and that RUN**, for the control door
    /// ([`AccountFollowWatcher::publish_derived_twins`]): the positive evidence a `live` engine needs before the door treats it
    /// as the twin (`AccountReadiness::derived_lane_twin`). Called when the set changed. DEFAULT nothing.
    fn publish_derived_lane_twins(&mut self, derived: &[String]) {
        let _ = derived;
    }
    /// **What the process that holds the venue keys (the gatewayhub; this tradehub reads no key value) says
    /// about them now.** `None` = it could not say (no gatewayhub to ask, no answer): what was said before
    /// stands. DEFAULT `None`.
    fn key_changed(&mut self, accounts: &AccountDirectory) -> Option<AccountFollowKeyAnswer> {
        let _ = accounts;
        None
    }
    /// The route keys of the RUNNING accounts to warn about, as they stand: the wire's list. DEFAULT nothing.
    fn publish_key_changed(&mut self, list: &[String]) {
        let _ = list;
    }
    /// **HOLD ONE account's engine closing-only, or release it** (`Command::HoldAccount`): a lane twin kept up
    /// only to be closed is held, so a strategy MOUNTED on it cannot open exposure (the control door's
    /// closing-only rule judges the operator's orders, not a mount's). The hold lives IN THE CORE as a floor a
    /// node-wide `trade halt` / `trade resume` cannot loosen, and only raises: the core remembers what the
    /// engine held before and a release lifts exactly that, so the watcher never has to know. A release of an
    /// engine that has no hold (it was restarted meanwhile: a NEW engine) does nothing. `false` = the core is
    /// gone. DEFAULT delivered (a world with no core).
    fn hold_account(&mut self, route_key: &str, hold: bool) -> bool {
        let _ = (route_key, hold);
        true
    }
    /// **Whether the lease worker of `route_key` asked for its account to be started again** (the venue stated
    /// the currency after the engine was started without it, `venue_backend::gateway::late_currency`), taking the
    /// request. DEFAULT no.
    fn take_remount(&mut self, route_key: &str) -> bool {
        let _ = route_key;
        false
    }
    /// **Why an account that runs must NOT be stopped to be started again**, or `None`: the start that follows
    /// the stop would be refused with a restart (a price feed not wired at the boot, a mount that cannot
    /// follow the rows), so the engine would not come back. DEFAULT none.
    fn remount_refusal(
        &mut self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> Option<String> {
        let _ = (accounts, venue, label);
        None
    }
}

/// When a transiently refused account is asked again, and the ladder of pauses that follows that attempt (the late
/// arm's, [`crate::late_arm::poll_ladder`]).
#[derive(Debug, Clone)]
struct Retry {
    due: Instant,
    ladder: ReconnectBackoff,
}

impl Retry {
    /// Asked at `due`, its first pause ([`FIRST_POLL`]) already spent: a refusal at `due` waits twice that.
    fn spent(due: Instant) -> Self {
        let mut ladder = poll_ladder();
        ladder.failed();
        Retry { due, ladder }
    }
}

/// **Whether a refusal is only a matter of time** (decision 0169 item 9; owner 2026-11-15: only a TRANSIENT cause is
/// asked again, a real refusal parks), the one list of the TRANSIENT causes, asked again on the backoff until they
/// pass, the account joining the running node by itself:
///
/// * `gatewayhub unavailable`: the gatewayhub did not answer;
/// * `price feeds unavailable`: the datahub's price path did not answer;
/// * `venue unreachable`: a venue call of the adapter start failed on a network failure (its gateway not up yet, a
///   refused or reset connection, DNS, a timeout, a TLS handshake) or another kind a later try can pass, on any
///   venue (`ArmingBlock::VenueUnreachable`, owner 2026-11-16);
/// * `SDK not loaded`: the venue's SDK is not up on the gatewayhub's box yet;
/// * `adapter failed` within [`LEASE_CLOSE_GRACE`] of this watcher's own stop of the same account (`since_stop`: how
///   long ago, `None` when it never stopped it): its old lease was still closing.
///
/// Every other block is a REFUSAL, a fact about the account, its keys, the venue's answer or the build: said once,
/// the account NOT RUNNING with it until the operator changes the rows or what the start reads (a corrected key:
/// [`AccountFollowWatcher::follow_key_changes`]) or restarts. That includes `adapter failed` itself (a key the venue
/// declined, a tier it bound wrong, an answer it cannot use) and `preflight failed` (the venue answered and refused,
/// or its clock is out of band; a probe that did not answer or could not reach the venue stops nothing). The FX
/// market's close (`FX market closed`) is neither: it is asked once, at the open ([`fx_retry_due`]).
pub(crate) fn is_transient(block: ArmingBlock, since_stop: Option<Duration>) -> bool {
    match block {
        ArmingBlock::GatewayUnavailable
        | ArmingBlock::FeedsUnavailable
        | ArmingBlock::VenueUnreachable
        | ArmingBlock::SdkAbsent => true,
        ArmingBlock::AdapterFailed => since_stop.is_some_and(|ago| ago < LEASE_CLOSE_GRACE),
        _ => false,
    }
}

/// **When an account the FX market's close refused (`ArmingBlock::FxMarketClosed`) is asked again: ONCE, at the
/// open** (owner ruling 2026-11-13: no login before it, and one timer to it, never a backoff of retries). The open is
/// `venue`'s next one read on the wall clock `wall_ms` (`vike_venues::session::fx_login_closed_until`), put on `now`'s
/// clock. An open that has already come (the gatewayhub's clock a few seconds behind this one) is [`FIRST_POLL`] away.
pub(crate) fn fx_retry_due(venue: &str, now: Instant, wall_ms: i64) -> Instant {
    vike_venues::session::fx_login_closed_until(venue, wall_ms).map_or(now + FIRST_POLL, |open| {
        now + Duration::from_millis(u64::try_from(open.saturating_sub(wall_ms)).unwrap_or(0))
    })
}

/// **When a box can run NOTHING before the FX week opens** (owners 2026-11-13 and 2026-11-14): every account its rows
/// run is a `demo`/`live` account of a venue on the FX week, and that week is closed at `now_ms`. `Some(open)` then: the
/// node would have no engine at all before the open (such a start used to end in `RuntimeError::NoAccountRuns` and be
/// restarted by the supervisor every few seconds until the open). `None` when some account could run now (a `paper` one, a crypto
/// one, an open market) or the rows run none.
pub(crate) fn nothing_runs_before_the_fx_open(
    accounts: &AccountDirectory,
    now_ms: i64,
) -> Option<i64> {
    let mut open = None;
    for wanted in wanted_accounts(accounts) {
        let Ok(tier) = wanted.tier else { continue };
        if tier == AccountTier::Paper {
            return None;
        }
        open = Some(vike_venues::session::fx_login_closed_until(&wanted.venue, now_ms)?);
    }
    open
}

/// **When a box can run NOTHING without its hubs** (owner 2026-11-14: never a restart loop): every account its rows
/// run is `demo`/`live`, and none of those runs without the gatewayhub (`venue_backend::gateway::needs_gateway`).
/// `Some(datahub_too)` then: `datahub_too` is whether every one of them is on a `mounted` venue too, because the price
/// gate holds exactly those accounts while the datahub is away (`feed_gate.rs`), so then none runs without the datahub
/// either. `None` when a `paper` account runs, or the rows run none.
pub(crate) fn nothing_runs_without_the_hubs(
    accounts: &AccountDirectory,
    mounted: &[String],
) -> Option<bool> {
    let mut datahub_too = None;
    for wanted in wanted_accounts(accounts) {
        let Ok(tier) = wanted.tier else { continue };
        if tier == AccountTier::Paper {
            return None;
        }
        datahub_too = Some(datahub_too.unwrap_or(true) && mounted.contains(&wanted.venue));
    }
    datahub_too
}

/// The state machine: what runs, what is published as not running, and what was already said.
pub(crate) struct AccountFollowWatcher<W: AccountFollowWorld> {
    world: W,
    /// Route key -> the tier it runs at (`None` = the rows no longer say, at construction).
    running: BTreeMap<String, Option<AccountTier>>,
    /// Route key of a RUNNING engine -> the id of the row it was started on ([`AccountFollowTarget::id`]), so a rename of that
    /// row is followed as the same account ([`Self::follow_renames`]).
    ids: BTreeMap<String, i64>,
    /// Route key -> the answer the rows gave when the account was stopped. The account is not started
    /// again while its answer is still that one (a `paper` sibling would otherwise be "restart required
    /// to start" and replace the stop's reason on every commit); a different answer lifts it.
    stopped: BTreeMap<String, Result<AccountTier, ArmingBlock>>,
    /// Route key -> when this watcher stopped it (the start of the account's lease closing).
    stopped_at: BTreeMap<String, Instant>,
    /// Route key -> the next attempt of an account whose last start was refused for a transient reason.
    retry: BTreeMap<String, Retry>,
    /// `flags.tradehub_live`: a `live` account is not started while it is off.
    live_switch: bool,
    not_running: Vec<ArmingNotRunningView>,
    /// The RUNNING accounts (route keys) whose key in the database is no longer the one their session
    /// started with: published as a warning, never acted on (stopping the account cancels its resting orders,
    /// so taking the new key stays the operator's, in his next batched restart).
    key_changed: BTreeSet<String>,
    /// Lines already said, so a commit that changes nothing about an account does not repeat them.
    said: BTreeSet<String>,
    /// The RUNNING accounts (route keys) that are DERIVED lane twins: the rows named them as one while they
    /// ran. The label cannot say it (an operator's own account may spell it); only this memory of what the
    /// rows derived can, and an engine not in it is never stopped for want of a row.
    twins: BTreeSet<String>,
    /// The lane twins whose derived row is gone but which are KEPT UP because they still hold a position or
    /// an order ([`Self::follow_lane_twins`]), with when that was last said: the ERROR repeats every
    /// [`TWIN_HELD_REPEAT`] while they hold.
    held_twins: BTreeMap<String, Instant>,
    /// The kept-up twins whose ENGINE the core has been told to hold `Reducing`
    /// ([`Self::sync_twin_holds`]): what [`Self::held_twins`] was when the core was last told.
    held_told: BTreeSet<String>,
    /// The kept-up twins ([`Self::held_twins`]) that are held NOT because their row vanished but because their
    /// row now states ANOTHER tier while they hold something: route key -> the tier the rows want. A tier
    /// restart would replace the engine under the same route key with one that does not know the old one's
    /// position or resting orders (review r2, finding 5).
    restart_held: BTreeMap<String, AccountTier>,
    /// Route key -> when the last late-currency restart of it was accepted, and how many were
    /// ([`REMOUNT_WINDOW`], [`REMOUNT_MAX`]). Kept for the life of the process.
    remounted: BTreeMap<String, (Instant, usize)>,
    /// The `(real, closing)` lists the door was last told ([`Self::publish_twin_flags`]).
    twin_flags: (Vec<String>, Vec<String>),
    /// The derived twins the door was last told ([`Self::publish_derived_twins`]), sorted.
    derived_told: Vec<String>,
    /// The door was last told the rows could not be read, so it is told again as soon as they can.
    rows_unreadable: bool,
    /// The rows the last change was followed on: a commit that leaves them equal is no news ([`Self::tick`]). `None` until
    /// then on a boot that did not account for every row, whose first commit is news whatever it says.
    followed: Option<AccountDirectory>,
    /// The gatewayhub's last counters of what each account's start reads ([`Self::follow_key_changes`]).
    start_inputs: BTreeMap<String, u64>,
    /// Venue -> its kept instrument list's write time when last read, for the venues with an account a refusal parked
    /// ([`Self::ask_again_when_a_list_moved`]).
    lists_seen: BTreeMap<String, Option<std::time::SystemTime>>,
    /// Set when the core is gone: the process is going down and there is nothing left to follow.
    done: bool,
}

impl<W: AccountFollowWorld> AccountFollowWatcher<W> {
    /// A watcher over `world` for a node that runs `running_keys` (the route keys of the engines the
    /// core was spawned with) and did not run `not_running`. An account the node booted without for want
    /// of the gatewayhub, or whose venue could not be reached or whose SDK was not up at its adapter start, is asked
    /// again [`FIRST_POLL`] from now, and on the backoff after that; one the FX market's close held is asked once, at
    /// the open ([`fx_retry_due`]); a refusal is not asked by the clock.
    pub(crate) fn new(
        mut world: W,
        running_keys: Vec<String>,
        not_running: Vec<ArmingNotRunningView>,
    ) -> Self {
        let boot_rows = world.accounts();
        let wanted = wanted_accounts(&boot_rows);
        // Every row agrees with what the boot did: one that runs is wanted, and one that is wanted runs or has its reason stated.
        // Then only a change of them is news; a row moved while the node assembled is news to the first commit it sees.
        let boot_knew_every_row = wanted.iter().all(|w| {
            let runs = running_keys.contains(&w.route_key);
            if w.tier.is_err() {
                !runs
            } else {
                runs || not_running.iter().any(|nr| nr.route_key() == w.route_key)
            }
        });
        let followed = boot_knew_every_row.then(|| boot_rows.clone());
        let twins: BTreeSet<String> = running_keys
            .iter()
            .filter(|key| {
                wanted.iter().any(|w| &w.route_key == *key && w.twin == AccountFollowTwin::Derived)
            })
            .cloned()
            .collect();
        let ids = wanted
            .iter()
            .filter(|w| running_keys.contains(&w.route_key))
            .filter_map(|w| Some((w.route_key.clone(), w.id?)))
            .collect();
        let running = running_keys
            .into_iter()
            .map(|key| {
                let tier = wanted
                    .iter()
                    .find(|w| w.route_key == key)
                    .and_then(|w| w.tier.as_ref().ok().copied());
                (key, tier)
            })
            .collect();
        let booted = world.now();
        let wall = world.wall_ms();
        // ...and one the boot met with a TRANSIENT cause of the start itself (its venue unreachable, its SDK not up), on the
        // backoff until it starts (`is_transient`; a refusal is not asked by the clock)...
        let transient_at_start = |nr: &ArmingNotRunningView| {
            matches!(nr.block, Some(ArmingBlock::VenueUnreachable | ArmingBlock::SdkAbsent))
        };
        // ...and one the FX market's close held at the boot is asked at the open, once.
        let retry = not_running
            .iter()
            .filter_map(|nr| {
                let due = if nr.block == Some(ArmingBlock::GatewayUnavailable)
                    || transient_at_start(nr)
                {
                    booted + FIRST_POLL
                } else if nr.block == Some(ArmingBlock::FxMarketClosed) {
                    fx_retry_due(nr.account.venue(), booted, wall)
                } else {
                    return None;
                };
                Some((nr.route_key(), Retry::spent(due)))
            })
            .collect();
        // The door learns at once which accounts are an operator's own under a twin's label, so a boot
        // that runs one never has its perpetual orders rewritten onto it while the first poll is pending.
        let real = real_twin_label_keys(&wanted);
        world.publish_lane_twin_flags(&real, &[]);
        // …and which running engines are the DERIVED twins: at `live` the door treats an engine as the twin
        // only on this positive word.
        let derived_told: Vec<String> = twins.iter().cloned().collect();
        world.publish_derived_lane_twins(&derived_told);
        // …and what the rows configure, which a bare venue is counted by.
        let configured = configured_accounts(&boot_rows);
        let rows_unreadable = configured.is_err();
        world.publish_configured_accounts(configured);
        // What the first commit's answer is compared with, so that commit can already be the corrected key.
        let start_inputs =
            world.key_changed(&boot_rows).map(|answer| answer.start_inputs).unwrap_or_default();
        let mut watcher = AccountFollowWatcher {
            world,
            running,
            ids,
            stopped: BTreeMap::new(),
            stopped_at: BTreeMap::new(),
            retry,
            live_switch: true,
            not_running,
            key_changed: BTreeSet::new(),
            said: BTreeSet::new(),
            twins,
            held_twins: BTreeMap::new(),
            held_told: BTreeSet::new(),
            restart_held: BTreeMap::new(),
            remounted: BTreeMap::new(),
            twin_flags: (real, Vec::new()),
            derived_told,
            rows_unreadable,
            followed,
            start_inputs,
            lists_seen: BTreeMap::new(),
            done: false,
        };
        watcher.say_ignored_live_leverages(&boot_rows);
        watcher.say_shared_addresses(&boot_rows);
        watcher
    }

    /// **Say, once each, an address two or more ACTIVE rows answer** (decision 0274:
    /// `vike_secrets::AccountRows::shared_addresses`). The store's writer refuses to make such a pair; a
    /// store that already holds one is NOT refused or rewritten here, so a running box keeps running
    /// exactly as it did (two active tiers under one label stay `two active tiers`). WARN.
    fn say_shared_addresses(&mut self, accounts: &AccountDirectory) {
        let Some(Ok(rows)) = accounts.rows() else { return };
        for (venue, label, ids) in rows.shared_addresses() {
            self.say(&format!("shared-address:{venue}:{label}:{ids:?}"), || {
                tracing::warn!(
                    venue = %venue,
                    "ACCOUNTS: rows {ids:?} are all ACTIVE {venue} accounts under the one address \
                     `{venue}/{label}`, which must name exactly one account (the address carries no \
                     tier). Nothing is changed or refused here. Give all but one a label of its own \
                     (`vike-cli account rename --id <N> --label <LABEL>`) or deactivate it: \
                     the store refuses the next edit that would make such a pair"
                );
            });
        }
    }

    /// **Say, once each, the stated leverages the rows' view DROPPED** because they sit on a LIVE row that is not a
    /// lane twin's parent (`AccountDirectory::ignored_live_leverages`): the owner opened the live twin only, so a
    /// figure on a labelled live account (a hand-edited store; the verb refuses it) is asked of nobody and written
    /// nowhere, and the account starts as it always did. ERROR, so it is not read as a figure in force.
    fn say_ignored_live_leverages(&mut self, accounts: &AccountDirectory) {
        for ignored in accounts.ignored_live_leverages() {
            let account = ignored.label.as_deref().map_or_else(
                || format!("{} default account", ignored.venue),
                |label| format!("{}#{label}", ignored.venue),
            );
            self.say(&format!("live-leverage-ignored:{}:{}", ignored.id, ignored.figure), || {
                tracing::error!(
                    id = ignored.id,
                    "ARMING: account {} ({account}) is `live` and states a leverage of {}x, which is IGNORED: \
                     at `live` only the default account's perpetual lane twin takes a stated leverage (the \
                     owner's 2026-10-19 ruling), so nothing is asked of or written to the exchange for this \
                     row and it starts exactly as it did without one. Clear it with `vike-cli account \
                     set-leverage --id {} --leverage none`",
                    ignored.id,
                    ignored.figure,
                    ignored.id
                );
            });
        }
    }

    /// This watcher with `flags.tradehub_live` as the mount was booted with it.
    #[must_use]
    pub(crate) fn with_live_switch(mut self, on: bool) -> Self {
        self.live_switch = on;
        self
    }

    /// Whether the core is gone (the loop ends).
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Whether some refused account is due to be asked again.
    fn retry_due(&mut self) -> bool {
        let now = self.world.now();
        self.retry.values().any(|r| r.due <= now)
    }

    /// One poll: when a row moved, follow the rows; else ask again the accounts whose backoff has run out.
    ///
    /// ⚠ **A commit is not a moved row.** The store has one change counter, so a session-history row (one every few seconds
    /// in a storm: `crates/vike-gatewayhub/src/link_writer.rs`), a `config set` and a venue setting all wake this. The rows are
    /// read once and compared with the last ones followed: equal, and nothing is stopped, no backoff is reset and no account is
    /// asked again for that alone, so a key the venue refused gets no more failed logins. What the rows cannot show is the
    /// gatewayhub's to say, in one dial per commit ([`Self::follow_key_changes`]): `secrets set` replaces a key IN PLACE, so its
    /// row, id and name stay, and this process reads no value.
    pub(crate) fn tick(&mut self) {
        // A refreshed instrument list asks a parked account again in THIS poll (`Self::ask_again_when_a_list_moved`).
        self.ask_again_when_a_list_moved();
        match self.world.rows_changed() {
            Ok(moved) => {
                let read = moved.then(|| self.world.accounts());
                if let Some(accounts) = read.as_ref().filter(|a| self.followed.as_ref() != Some(*a))
                {
                    self.followed = Some(accounts.clone());
                    self.follow(false);
                } else {
                    if self.rows_unreadable {
                        // It answers again and no row moved, so nothing else would tell the door.
                        let accounts = self.world.accounts();
                        self.publish_configured(&accounts);
                    }
                    if let Some(accounts) = &read {
                        self.follow_key_changes(accounts);
                    }
                    if self.retry_due() {
                        self.follow(true);
                    }
                }
            }
            Err(e) => {
                self.rows_unreadable = true;
                self.world.publish_configured_accounts(Err(e.clone()));
                self.say(&format!("unreadable:{e}"), || {
                    tracing::error!(
                        error = %e,
                        "ARMING: the settings database exists and cannot be read, so the account rows \
                         are NOT being followed (nothing that runs changes; this is NOT \"no \
                         accounts\"), and a bare venue is refused until they can. The next poll tries \
                         again"
                    );
                });
            }
        }
        self.remount_asked();
        self.watch_held_twins();
        self.sync_twin_holds();
    }

    /// **An account a refusal parked (`adapter failed`) is asked again, once, when its venue's kept instrument list is
    /// refreshed** (owner 2026-11-16: refreshes are manual, `vike-cli data catalog refresh --venue <venue>`; a start
    /// reads the list, bybit's book of the mounted symbol, so a symbol the list lacked can be there now). Event-driven:
    /// the file's write time (`AccountFollowWorld::kept_list_written`) is read on this poll, and only a MOVE of it asks,
    /// at once and once; the start reads the list as it is now (`vike_catalog::kept_list_now`). No timer and no backoff:
    /// a start the list still does not answer parks again until the next refresh.
    fn ask_again_when_a_list_moved(&mut self) {
        let parked = |nr: &ArmingNotRunningView| nr.block == Some(ArmingBlock::AdapterFailed);
        let venues: BTreeSet<String> = self
            .not_running
            .iter()
            .filter(|nr| parked(nr))
            .map(|nr| nr.account.venue().to_string())
            .collect();
        self.lists_seen.retain(|venue, _| venues.contains(venue));
        let now = self.world.now();
        for venue in venues {
            let written = self.world.kept_list_written(&venue);
            // `None`: first seen now (the park itself), so nothing has moved yet.
            let Some(was) = self.lists_seen.insert(venue.clone(), written) else { continue };
            if was == written {
                continue;
            }
            for nr in self.not_running.iter().filter(|nr| parked(nr) && nr.account.venue() == venue)
            {
                tracing::info!(
                    "ARMING: {venue}'s instrument list was refreshed, so {} (NOT RUNNING: {}) is asked again once",
                    nr.route_key(),
                    nr.reason
                );
                self.retry.insert(nr.route_key(), Retry::spent(now));
            }
        }
    }

    /// **Tell the core which kept-up twins to hold closing-only, and which to let go.** A twin kept up only to be
    /// closed ([`Self::held_twins`]: its row vanished, or its tier restart waits for it to be flat) is refused
    /// opening orders at the control door, but a strategy MOUNTED on it places orders the door never sees, so the
    /// engine itself is HELD (`AccountFollowWorld::hold_account`, `Command::HoldAccount`: no other account is touched).
    ///
    /// The hold is the CORE's, not this watcher's: a floor at `Reducing` that a node-wide `SetTradingState`
    /// (`trade halt` then `trade resume`, the profile's `initial_trading_state`) cannot loosen, that only raises
    /// (an engine an operator halted stays halted), and whose release lifts exactly what it set. So the sends
    /// here are edge-triggered ONCE and need no re-assertion, and a release sent for an account that was
    /// restarted meanwhile (a NEW engine, which carries no hold) does nothing. The set is `held_told`, and a twin
    /// is recorded as told only AFTER the core took the message, so a failed send is tried again next poll.
    /// Runs at the end of every poll, the one place every change of the set has settled.
    ///
    /// ⚠ **This is the one sender of a journaled record that needs no operator action**: the first twin kept up
    /// with something open stamps the journal segment format 21 (`vike_journal`'s `ACCOUNT_STATE_VERSION`), which
    /// an older build refuses, so the rollback door closes on a box that never ran `trade account`.
    fn sync_twin_holds(&mut self) {
        if self.done {
            return;
        }
        let held: BTreeSet<String> = self.held_twins.keys().cloned().collect();
        for key in held.difference(&self.held_told).cloned().collect::<Vec<_>>() {
            if !self.world.hold_account(&key, true) {
                self.done = true;
                return;
            }
            self.held_told.insert(key);
        }
        for key in self.held_told.difference(&held).cloned().collect::<Vec<_>>() {
            // A twin that is no longer RUNNING was stopped, and its engine and hold went with it.
            if self.running.contains_key(&key) && !self.world.hold_account(&key, false) {
                self.done = true;
                return;
            }
            self.held_told.remove(&key);
        }
    }

    /// **A lane twin that was kept up stops once it is flat.** The rows do not move when a position closes,
    /// so this runs on EVERY poll: while a kept-up twin still holds something it repeats the ERROR every
    /// [`TWIN_HELD_REPEAT`], and the poll it holds nothing any more it is stopped through the ordinary
    /// stop ([`Self::stop_what_the_rows_stopped`]) with the rows as they stand now.
    fn watch_held_twins(&mut self) {
        if self.held_twins.is_empty() || self.done {
            return;
        }
        let now = self.world.now();
        let mut flat = false;
        for key in self.held_twins.keys().cloned().collect::<Vec<_>>() {
            if !self.world.has_exposure(&key) {
                flat = true;
            } else if self
                .held_twins
                .get(&key)
                .is_some_and(|said| now.saturating_duration_since(*said) >= TWIN_HELD_REPEAT)
            {
                self.held_twins.insert(key.clone(), now);
                match self.restart_held.get(&key) {
                    Some(to) => Self::say_twin_restart_is_held(&key, *to),
                    None => Self::say_twin_is_kept(&key),
                }
            }
        }
        if flat {
            // The rows are followed whole: a twin that was kept up for a vanished row is stopped, and one that
            // was kept up for a tier restart is stopped and STARTED at the tier the row states now.
            self.follow(false);
        }
    }

    /// The ERROR a lane twin held at its OLD tier is announced with, first and then every [`TWIN_HELD_REPEAT`].
    fn say_twin_restart_is_held(key: &str, to: AccountTier) {
        tracing::error!(
            route_key = key,
            "ARMING: the perp lane twin {key}'s row now states tier `{to}`, but it holds a position or a resting \
             order at the venue (or this node has no snapshot to show that it holds none), and a restart would \
             give that route key a NEW engine that does not know them: they would sit at the venue with nothing \
             watching. It stays UP at the tier it runs at and takes no new position meanwhile. Flatten it \
             (`close-all`, naming the account PERP) and cancel its orders; it restarts at `{to}` within a \
             second of being flat. Stating the old tier again keeps it. This is said again every minute while it \
             holds"
        );
    }

    /// The ERROR a kept-up lane twin is announced with, first and then every [`TWIN_HELD_REPEAT`].
    fn say_twin_is_kept(key: &str) {
        tracing::error!(
            route_key = key,
            "ARMING: the perp lane twin {key} stays UP: its row is gone (the margin leverage is no longer \
             stated on the account), but it holds a position or a resting order at the venue (or this node \
             has no snapshot to show that it holds none), and \
             stopping it would leave them with nothing watching. It takes no new position meanwhile. Flatten it (`close-all`, naming the \
             account PERP) and cancel its orders; it stops by itself within a second of being flat. \
             Stating the leverage again keeps it. This is said again every minute while it holds"
        );
    }

    /// Say `f` once per distinct `key`.
    fn say(&mut self, key: &str, f: impl FnOnce()) {
        if self.said.insert(key.to_string()) {
            f();
        }
    }

    /// Tell the door what `accounts` configure.
    fn publish_configured(&mut self, accounts: &AccountDirectory) {
        let configured = configured_accounts(accounts);
        self.rows_unreadable = configured.is_err();
        self.world.publish_configured_accounts(configured);
    }

    /// Re-read the rows and act on the difference with what runs. `retry_only`: this is not a change of the
    /// rows but a backoff running out, so only the accounts that are DUE are asked, and nothing is stopped
    /// or reported again (the rows are the ones the last change was followed on).
    fn follow(&mut self, retry_only: bool) {
        let accounts = self.world.accounts();
        self.publish_configured(&accounts);
        self.say_ignored_live_leverages(&accounts);
        self.say_shared_addresses(&accounts);
        let mut wanted = wanted_accounts(&accounts);
        self.follow_lane_twins(&mut wanted, &accounts);
        let renamed = self.follow_renames(&accounts);
        if !retry_only {
            // Stops first (decision 0169 / the plan's section 2): releasing an account never waits
            // behind a start's gatewayhub round trip.
            self.stop_what_the_rows_stopped(&wanted, &renamed);
            self.report_running_accounts_that_differ(&wanted, &renamed);
        }
        let now = self.world.now();
        for w in &wanted {
            if self.done {
                return;
            }
            let key = &w.route_key;
            // A renamed account still running under its old label is this one: never a second engine.
            let runs_renamed =
                renamed.iter().any(|(old, new)| new == key && self.running.contains_key(old));
            if self.running.contains_key(key) || w.tier.is_err() || runs_renamed {
                self.retry.remove(key);
                continue;
            }
            if self.stopped.get(key) == Some(&w.tier) {
                self.retry.remove(key);
                continue; // stopped on this very answer: nothing has changed that would start it
            }
            if retry_only {
                if !self.retry.get(key).is_some_and(|r| r.due <= now) {
                    continue;
                }
            } else {
                // A change of the rows is news: the backoff starts over from its first pause.
                self.retry.remove(key);
            }
            // The clock is read AFTER the start: a start may block (a preflight, a dial), and the next pause
            // runs from when it ended.
            let outcome = self.start(&accounts, w);
            let ended = self.world.now();
            self.record(w, outcome, ended);
        }
        // An account the rows no longer name has nothing to be asked again.
        self.retry.retain(|key, _| wanted.iter().any(|w| &w.route_key == key));
        // Asked LAST, so a session this very pass restarted is judged by the key it started with now.
        if !retry_only {
            self.follow_key_changes(&accounts);
        }
    }

    /// **A running LANE TWIN whose derived row is gone: kept up while it holds something, stopped once it is
    /// flat.**
    ///
    /// A twin (`vike_secrets::AccountRows::derive_lane_twins`) exists in the view only while its parent row states
    /// a margin leverage, so un-stating the leverage (or changing the parent's tier) makes its row VANISH. The
    /// watcher's rule is that an absent row stops nothing ([`change`]: a deleted row, an unreadable table), which
    /// for a derived row would leave the perpetual engine trading after the operator took the figure away. But
    /// the ordinary stop cancels the engine's resting orders (a protective stop-loss included), closes its lease
    /// and leaves the position at the venue with NOTHING watching it, and the door would then refuse every order
    /// to it, a flatten included. So, for a twin the rows DERIVED while it ran ([`Self::twins`], never the label):
    ///
    /// * the row is back (leverage stated again): nothing to do;
    /// * a REAL account row now carries its label: said once, nothing else changes (the engine runs under that
    ///   key on the parent's keys until a restart);
    /// * the row is gone and the engine HOLDS a position or an order ([`AccountFollowWorld::has_exposure`]): it stays UP, as
    ///   the tier it runs at, with an ERROR now and every [`TWIN_HELD_REPEAT`] ([`Self::watch_held_twins`]);
    /// * the row is gone and it is flat: it gets the `AccountInactive` answer and the ordinary stop follows,
    ///   listed with [`lane_twin_stopped_why`].
    ///
    /// ONLY when the table was ANSWERED: a store that exists and will not open reads as no rows at all, and that
    /// must never stop an account.
    fn follow_lane_twins(
        &mut self,
        wanted: &mut Vec<AccountFollowTarget>,
        accounts: &AccountDirectory,
    ) {
        if !matches!(accounts.rows(), Some(Ok(rows)) if rows.known().is_some()) {
            return;
        }
        let real = real_twin_label_keys(wanted);
        for w in wanted.iter() {
            if w.twin == AccountFollowTwin::Derived && self.running.contains_key(&w.route_key) {
                self.twins.insert(w.route_key.clone());
            }
        }
        for key in self.twins.clone() {
            let Some(&ran_at) = self.running.get(&key) else {
                self.twins.remove(&key);
                self.held_twins.remove(&key);
                self.restart_held.remove(&key);
                continue;
            };
            // **A tier restart of a twin that HOLDS something is deferred until it is flat** (review r2,
            // finding 5): the restart stops the engine and starts a new one under the same route key, which does
            // not know the old one's position or resting orders (including a protective stop), and the NOT
            // RUNNING entry that named them is cleared by the new engine's start. So the row's new tier is
            // masked here, the engine stays up at the tier it runs at, closing-only (`held_twins` is what the
            // door is told is closing), and [`Self::watch_held_twins`] restarts it the poll it is flat.
            let wants = wanted
                .iter()
                .find(|w| w.route_key == key && w.twin == AccountFollowTwin::Derived)
                .and_then(|w| change(ran_at, &w.tier).zip(w.tier.as_ref().ok().copied()));
            if let Some((Change::Restart, to)) = wants
                && self.world.has_exposure(&key)
            {
                if let Some(w) = wanted.iter_mut().find(|w| w.route_key == key) {
                    w.tier = Ok(ran_at.unwrap_or(AccountTier::Demo));
                }
                if !self.held_twins.contains_key(&key) {
                    let now = self.world.now();
                    self.held_twins.insert(key.clone(), now);
                    Self::say_twin_restart_is_held(&key, to);
                }
                self.restart_held.insert(key.clone(), to);
                continue;
            }
            self.restart_held.remove(&key);
            match wanted.iter().find(|w| w.route_key == key) {
                Some(w) if w.twin == AccountFollowTwin::Derived => {
                    self.held_twins.remove(&key);
                    continue;
                }
                Some(_) => {
                    self.say(&format!("twin-label-taken:{key}"), || {
                        tracing::error!(
                            "ARMING: {key} is the perp lane twin this node derived and started, but the \
                             account table now holds a REAL account under that label. The running engine \
                             stays on the parent account's keys under the real account's route key, and \
                             nothing changes until a restart: flatten it first, then restart this tradehub \
                             so the real account runs on its own keys"
                        );
                    });
                    continue;
                }
                None => {}
            }
            // The twin's account, read back from its key by the model's one inverse: its venue is the roster id the
            // key was minted on (`wanted_accounts` names every account over `VENUES`), and a twin has a label.
            let Some((venue, label)) = vike_model::VENUES.iter().find_map(|venue| {
                label_of_route_key(venue, &key).filter(|l| !l.is_default()).map(|l| (*venue, l))
            }) else {
                continue;
            };
            let tier = if self.world.has_exposure(&key) {
                if !self.held_twins.contains_key(&key) {
                    let now = self.world.now();
                    self.held_twins.insert(key.clone(), now);
                    Self::say_twin_is_kept(&key);
                }
                // Kept up AS THE TIER IT RUNS AT, so neither a stop nor a restart nor a "differs" report follows.
                Ok(ran_at.unwrap_or(AccountTier::Demo))
            } else {
                self.held_twins.remove(&key);
                Err(ArmingBlock::AccountInactive)
            };
            wanted.push(AccountFollowTarget {
                venue: venue.to_string(),
                label,
                route_key: key.clone(),
                tier,
                twin: AccountFollowTwin::Vanished,
                id: None,
            });
        }
        self.publish_twin_flags(real);
    }

    /// Tell the door which accounts are an operator's own under a twin's label (`real`) and which twins are kept
    /// up only to be closed (`self.held_twins`), when either changed since it was last told.
    fn publish_twin_flags(&mut self, real: Vec<String>) {
        let closing: Vec<String> = self.held_twins.keys().cloned().collect();
        if self.twin_flags != (real.clone(), closing.clone()) {
            self.world.publish_lane_twin_flags(&real, &closing);
            self.twin_flags = (real, closing);
        }
        self.publish_derived_twins();
    }

    /// Tell the door which RUNNING engines are lane twins the rows derived ([`Self::twins`], a kept-up twin
    /// included), when the set changed since it was last told. Called wherever the set can move.
    fn publish_derived_twins(&mut self) {
        let derived: Vec<String> = self.twins.iter().cloned().collect();
        if self.derived_told != derived {
            self.world.publish_derived_lane_twins(&derived);
            self.derived_told = derived;
        }
    }

    /// **A RUNNING account whose row now carries ANOTHER label is the SAME account, still under the label it
    /// started with**: old route key -> new route key, for every running engine whose row ([`Self::ids`]) the rows
    /// now name differently (`vike-cli account rename`, the observe server's `AccountVerb::Rename`, a hand
    /// edit). Said once per rename, at `warn`.
    ///
    /// The new label must not start a second engine: the gatewayhub keys a session by `(venue, label)`
    /// (`vike_gatewayhub`'s `AccountSessions::session_for`) and files the keys by row, so it would open a second session on
    /// the same keys, and two engines would trade and reconcile one venue account. So [`Self::follow`] judges the
    /// running engine by the answer its row gives under the NEW label (deactivated: it stops; another tier: it
    /// stops and the account starts under the new label at that tier) and starts nothing under the new label while
    /// the old engine runs. The new label takes effect at the account's next stop and start.
    fn follow_renames(&mut self, accounts: &AccountDirectory) -> BTreeMap<String, String> {
        let found: Vec<(String, i64, String)> = self
            .ids
            .iter()
            .filter(|(key, _)| self.running.contains_key(*key))
            .filter_map(|(key, id)| {
                let now = route_key_of_row(accounts, *id)?;
                (now != *key).then(|| (key.clone(), *id, now))
            })
            .collect();
        for (old, id, new) in &found {
            self.say(&format!("renamed:{old}:{new}"), || {
                tracing::warn!(
                    route_key = %old,
                    id,
                    "ARMING: {old} is RUNNING and its row (id {id}) is now named {new}. It is the SAME account, so it \
                     keeps its one engine and its one lease, under {old}: nothing is started under {new} (a second \
                     engine would trade and reconcile the same venue account on the same keys). Orders name it \
                     {old} until then; deactivating the row stops it. The new name takes effect at the account's \
                     next stop and start (its row deactivated and activated, or the next restart of this tradehub)"
                );
            });
        }
        found.into_iter().map(|(old, _, new)| (old, new)).collect()
    }

    /// **A key changed under a RUNNING account is a warning, never a restart** (owner ruling 2026-10-17:
    /// stopping the account cancels its resting orders, so it stays the operator's decision). The process that
    /// holds the key values says which running accounts started on a key the database no longer holds
    /// ([`AccountFollowWorld::key_changed`]); this lists them on the wire (`KEY_CHANGED_WARNING`) and says so once per
    /// change. An account that restarts is judged afresh by the next answer, one that stopped is dropped
    /// at once, and a question nobody could answer changes nothing.
    ///
    /// **The same answer asks a refused account again** ([`Self::ask_again_what_moved`]), so an operator who
    /// corrects a key with `vike-cli secrets set` needs no further step.
    fn follow_key_changes(&mut self, accounts: &AccountDirectory) {
        let asked = if self.running.is_empty() && self.not_running.is_empty() {
            Some(AccountFollowKeyAnswer::default())
        } else {
            self.world.key_changed(accounts)
        };
        let Some(asked) = asked else { return };
        self.ask_again_what_moved(asked.start_inputs);
        let now: BTreeSet<String> =
            asked.changed.into_iter().filter(|key| self.running.contains_key(key)).collect();
        if now == self.key_changed {
            return;
        }
        let raised: Vec<String> = now.difference(&self.key_changed).cloned().collect();
        let cleared: Vec<String> = self.key_changed.difference(&now).cloned().collect();
        self.key_changed = now;
        for key in raised {
            tracing::warn!(
                "ARMING: {key} is RUNNING and its {KEY_CHANGED_WARNING}. It keeps trading with the old \
                 key: the change did not come through a credential verb that reached the running \
                 gatewayhub (`vike-cli secrets set` / `delete` tell it through the datahub, and it \
                 reconnects that account alone, cancelling nothing: decision 0287). To take the new key, \
                 write it again with `vike-cli secrets set` while the datahub and the gatewayhub run, or \
                 take it into the next batched restart (resting orders stay at the venue while \
                 flags.cancel_orders_on_shutdown is off)"
            );
        }
        for key in cleared {
            tracing::info!("ARMING: {key} no longer has a key that differs from the database's");
        }
        self.publish_key_changed();
    }

    /// **An account that does not run is asked again, once, when what its start reads has moved**: its counter
    /// differs from the last answer's. It goes through the retry map, so [`Self::follow`]'s own guards decide
    /// (an account stopped on this very answer stays stopped). An account first seen now moved nothing, and one
    /// that runs keeps running: for it a changed key is the warning.
    fn ask_again_what_moved(&mut self, inputs: BTreeMap<String, u64>) {
        let now = self.world.now();
        for (key, revision) in &inputs {
            if !self.running.contains_key(key)
                && self.start_inputs.get(key).is_some_and(|was| was != revision)
            {
                self.retry.insert(key.clone(), Retry::spent(now));
            }
        }
        self.start_inputs = inputs;
    }

    /// Publish the warning list as it stands.
    fn publish_key_changed(&mut self) {
        let list: Vec<String> = self.key_changed.iter().cloned().collect();
        self.world.publish_key_changed(&list);
    }

    /// `key` stopped or started anew: whatever was said about its old session's key is over.
    fn clear_key_changed(&mut self, key: &str) {
        if self.key_changed.remove(key) {
            self.publish_key_changed();
        }
    }

    /// Ask the world to start `w`, after the one gate that is the watcher's: a `live` account does not
    /// start while `flags.tradehub_live` is off (decision 0169 item 6; the switch stops, it never demotes).
    fn start(&mut self, accounts: &AccountDirectory, w: &AccountFollowTarget) -> HotAccountStart {
        if w.tier == Ok(AccountTier::Live) && !self.live_switch {
            return HotAccountStart::NotRunning(ArmingNotRunning {
                venue: w.venue.clone(),
                account: w.label.clone(),
                tier: Some(AccountTier::Live),
                block: ArmingBlock::LiveSwitchOff,
            });
        }
        self.world.start(accounts, &w.venue, &w.label)
    }

    /// Stop every running account whose row now says it does not run ([`stops`]). One at a time, and a
    /// failure of one never aborts the rest. The account goes on the published NOT RUNNING list BEFORE
    /// the core is told: from that frame the control edge refuses new orders for it with the reason, so
    /// no order is accepted for an account whose lease is about to close.
    ///
    /// Phase one is sent for EVERY account first, then [`AccountFollowWorld::settle`] waits ONCE (bounded) for all their
    /// cancels and detaches them together: ten deactivations in one commit cost one wait, not ten.
    fn stop_what_the_rows_stopped(
        &mut self,
        wanted: &[AccountFollowTarget],
        renamed: &BTreeMap<String, String>,
    ) {
        let mut sent: Vec<ArmingNotRunningView> = Vec::new();
        for (key, ran_at) in self.running.clone() {
            if self.done {
                return;
            }
            // A renamed account answers under its new label ([`Self::follow_renames`]); its engine runs under `key`.
            let answers = renamed.get(&key).unwrap_or(&key);
            let Some(w) = wanted.iter().find(|w| &w.route_key == answers) else { continue };
            let Some(what) = change(ran_at, &w.tier) else { continue };
            let new_tier = w.tier.as_ref().ok().copied();
            let restart_at = if what == Change::Restart { new_tier } else { None };
            let label = vike_model::accounts::account_keys::label_of_route_key(&w.venue, &key)
                .unwrap_or_else(|| w.label.clone());
            // Not "account inactive" for a lane twin whose row went: the account is active, its perp lane is what
            // went. And when the cause is two active tiers on the default account (a demo row and a live row), the
            // sentence says that: the figure is still stated.
            let twin_why = (w.twin == AccountFollowTwin::Vanished).then(|| {
                let conflict = wanted.iter().any(|p| {
                    p.venue == w.venue
                        && p.label.is_default()
                        && p.tier == Err(ArmingBlock::TierConflict)
                });
                lane_twin_stopped_why(&w.venue, ran_at, conflict)
            });
            let stop = match (restart_at, &twin_why) {
                (Some(to), _) => AccountStop::Restart(to),
                (None, Some(why)) => AccountStop::TwinGone(why),
                (None, None) => AccountStop::Inactive,
            };
            let entry = self.world.stopping(&w.venue, &label, &key, new_tier, stop);
            self.set_not_running(entry.clone());
            let outcome = self.world.stop(&entry);
            match outcome {
                HotAccountStop::Stopped | HotAccountStop::NotRunning => {
                    self.engine_gone(&key);
                    // A stop stays stopped on this answer; a restart is started in this very pass.
                    if what == Change::Stop {
                        self.stopped.insert(key, w.tier);
                    } else {
                        self.stopped.remove(&key);
                    }
                    if outcome == HotAccountStop::Stopped {
                        sent.push(entry);
                    }
                }
                HotAccountStop::CoreGone => self.done = true,
            }
        }
        if !sent.is_empty() && !self.done {
            self.world.settle(&sent);
        }
    }

    /// Forget what the watcher kept for the engine of `key`, which has just been stopped, and note when.
    fn engine_gone(&mut self, key: &str) {
        self.running.remove(key);
        self.ids.remove(key);
        self.twins.remove(key);
        self.held_twins.remove(key);
        // The engine is gone and its hold with it; a restart under this key is a NEW engine
        // that is told nothing ([`Self::sync_twin_holds`]).
        self.held_told.remove(key);
        self.restart_held.remove(key);
        self.publish_derived_twins();
        self.retry.remove(key);
        self.clear_key_changed(key);
        // ...and a new engine may be held back again: its warning is said again.
        self.said.remove(&remount_held_key(key));
        let at = self.world.now();
        self.stopped_at.insert(key.to_string(), at);
    }

    /// **Start again the accounts whose lease worker asked** (`venue_backend::gateway::late_currency`): the venue stated the
    /// account's currency AFTER the engine was started without it, so the engine is anchored to an assumed one and
    /// mounted on the pair its row states. The only way to a ledger that follows the venue is what a toggle of `account.active`
    /// does, and this does it for an engine that holds nothing: the NOT RUNNING entry is published first (the door
    /// refuses new orders with [`NotRunningReason::Remount`]), the engine is stopped (`HotAccounts::stop_account`), its session
    /// closed once the stop settled, and the account is started through the code every start runs, whose
    /// `VenueBackendGateway::start_adapter` then finds the stated currency and moves the mount.
    ///
    /// ⚠ **It never touches an engine that holds something.** A position or a live order (or no snapshot to show
    /// there is none: [`AccountFollowWorld::has_exposure`]) is ONE `warn!` and the engine keeps running as it was, because the
    /// stop cancels resting orders and leaves a position with nothing watching it (the toggle's own cost, which is
    /// the operator's to pay). Neither does it stop an engine whose start would be refused for a restart
    /// ([`AccountFollowWorld::remount_refusal`]): it would not come back. An account the rows no longer say runs is the rows' to
    /// follow. The request is consumed either way; the worker that made it is done.
    ///
    /// **Bounded**: an account is started again at most [`REMOUNT_MAX`] times per run and not twice within
    /// [`REMOUNT_WINDOW`] (one `warn!` for the request that is ignored), because a start that meets a transient
    /// login conflict lands on an assumed currency again and would ask for another restart for ever.
    fn remount_asked(&mut self) {
        if self.done {
            return;
        }
        let running: Vec<String> = self.running.keys().cloned().collect();
        let asked: Vec<String> =
            running.into_iter().filter(|key| self.world.take_remount(key)).collect();
        if asked.is_empty() {
            return;
        }
        let accounts = self.world.accounts();
        let wanted = wanted_accounts(&accounts);
        let (mut sent, mut again): (Vec<ArmingNotRunningView>, Vec<AccountFollowTarget>) =
            (Vec::new(), Vec::new());
        for key in asked {
            let Some(w) = wanted.iter().find(|w| w.route_key == key) else { continue };
            let Ok(tier) = w.tier else { continue };
            if let Some(why) = self.world.remount_refusal(&accounts, &w.venue, &w.label) {
                self.say(&remount_held_key(&key), || {
                    tracing::warn!(
                        route_key = %key,
                        "ARMING: the venue now states {key}'s currency, which its running engine was NOT started \
                         on, but the account cannot be started again by itself: {why}. It keeps running as it \
                         is; set the account's `account.active` off and on in your next batched change"
                    );
                });
                continue;
            }
            if self.world.has_exposure(&key) {
                self.say(&remount_held_key(&key), || {
                    tracing::warn!(
                        route_key = %key,
                        "ARMING: the venue now states {key}'s currency, which its running engine was NOT started \
                         on (the ledger is anchored to an assumed one), but the account holds a position or a \
                         live order (or this node has no snapshot to show that it holds none), and starting it \
                         again would cancel the orders and leave the position with nothing watching it. It \
                         keeps running as it is. Close the position and cancel the orders, then set the \
                         account's `account.active` off and on once: the new start follows the venue's currency"
                    );
                });
                continue;
            }
            // BOUNDED: a restart that lands on an assumed currency again would be asked for again, for ever.
            let now = self.world.now();
            let before = self.remounted.get(&key).copied();
            if let Some((at, n)) = before
                && (n >= REMOUNT_MAX || now.saturating_duration_since(at) < REMOUNT_WINDOW)
            {
                self.say(&format!("remount-limited:{key}:{n}"), || {
                    tracing::warn!(
                        route_key = %key,
                        "ARMING: the venue states {key}'s currency again, but this node has already started the \
                         account again {n} time(s) for that (at most {REMOUNT_MAX} per run, {} s apart), and \
                         another start would refuse its orders again for nothing. It keeps running as it is; set \
                         the account's `account.active` off and on in your next batched change",
                        REMOUNT_WINDOW.as_secs()
                    );
                });
                continue;
            }
            self.remounted.insert(key.clone(), (now, before.map_or(1, |(_, n)| n + 1)));
            tracing::warn!(
                route_key = %key,
                "ARMING: {key} is being STOPPED and STARTED AGAIN in the running tradehub: the venue now states \
                 its currency, which the engine was not started on. It holds no position and no live order; \
                 its new start follows the venue's currency (the session starts on the pair that carries it)"
            );
            let mut entry =
                self.world.stopping(&w.venue, &w.label, &key, Some(tier), AccountStop::Inactive);
            let remount = NotRunningReason::Remount;
            entry.reason = remount.to_string();
            entry.refusal_id = Some(remount.shown_id().to_owned());
            entry.block = None;
            self.set_not_running(entry.clone());
            match self.world.stop(&entry) {
                outcome @ (HotAccountStop::Stopped | HotAccountStop::NotRunning) => {
                    self.engine_gone(&key);
                    self.stopped.remove(&key);
                    again.push(w.clone());
                    if outcome == HotAccountStop::Stopped {
                        sent.push(entry);
                    }
                }
                HotAccountStop::CoreGone => {
                    self.done = true;
                    return;
                }
            }
        }
        if !sent.is_empty() {
            self.world.settle(&sent);
        }
        for w in again {
            if self.done {
                return;
            }
            let outcome = self.start(&accounts, &w);
            let ended = self.world.now();
            self.record(&w, outcome, ended);
        }
    }

    /// Decision 0169 item 11: a row that differs from what runs and that the watcher does not act on is
    /// reported, never ignored: a row that stopped naming a running account (a deleted row) leaves it running.
    fn report_running_accounts_that_differ(
        &mut self,
        wanted: &[AccountFollowTarget],
        renamed: &BTreeMap<String, String>,
    ) {
        for (key, ran_at) in self.running.clone() {
            let answers = renamed.get(&key).unwrap_or(&key);
            let now = wanted.iter().find(|w| &w.route_key == answers).map(|w| w.tier);
            // A tier that differs was restarted above ([`change`]); what is left is a row that stopped answering.
            let differs = matches!(now, Some(Err(_)) | None);
            if !differs {
                continue;
            }
            let says = match &now {
                Some(Ok(tier)) => format!("tier `{tier}`"),
                Some(Err(block)) => format!("`{}`", block.as_str()),
                None => "nothing (no row names it)".to_string(),
            };
            let ran = ran_at.map_or_else(|| "its start tier".to_string(), |t| format!("`{t}`"));
            self.say(&format!("differs:{key}:{says}"), || {
                tracing::error!(
                    "ARMING: {key} is RUNNING at {ran} but its row now says {says}. A row that was deleted \
                     or stopped naming the account is not a statement the watcher acts on (an unreadable \
                     table must never stop an account): the account KEEPS RUNNING. Deactivate its row \
                     to stop it, after closing its positions with `close-all`"
                );
            });
        }
    }

    /// Fold one start's outcome (made at `now`) into what runs, what is published and what is asked again.
    fn record(&mut self, w: &AccountFollowTarget, outcome: HotAccountStart, now: Instant) {
        match outcome {
            HotAccountStart::Started { route_key, tier } => {
                self.stopped.remove(&route_key);
                self.stopped_at.remove(&route_key);
                self.retry.remove(&route_key);
                self.clear_key_changed(&route_key);
                self.running.insert(route_key.clone(), Some(tier));
                if let Some(id) = w.id {
                    self.ids.insert(route_key.clone(), id);
                }
                if w.twin == AccountFollowTwin::Derived {
                    self.twins.insert(route_key.clone());
                    self.publish_derived_twins();
                }
                self.not_running.retain(|nr| nr.route_key() != route_key);
                self.world.publish_not_running(&self.not_running);
            }
            HotAccountStart::AlreadyRunning => {
                let tier = w.tier.as_ref().ok().copied();
                self.retry.remove(&w.route_key);
                self.running.insert(w.route_key.clone(), tier);
                if let Some(id) = w.id {
                    self.ids.insert(w.route_key.clone(), id);
                }
                if w.twin == AccountFollowTwin::Derived {
                    self.twins.insert(w.route_key.clone());
                    self.publish_derived_twins();
                }
            }
            HotAccountStart::NotRunning(nr) => {
                let since_stop =
                    self.stopped_at.get(&w.route_key).map(|at| now.saturating_duration_since(*at));
                let transient = is_transient(nr.block, since_stop);
                let fx_closed = nr.block == ArmingBlock::FxMarketClosed;
                let wall = self.world.wall_ms();
                let view = nr.view_at(wall);
                let reason = view.reason.clone();
                self.set_not_running(view);
                if fx_closed {
                    let due = fx_retry_due(&w.venue, now, wall);
                    self.retry.insert(w.route_key.clone(), Retry::spent(due));
                } else if transient {
                    let mut ladder =
                        self.retry.remove(&w.route_key).map_or_else(poll_ladder, |r| r.ladder);
                    let due = now + ladder.failed();
                    self.retry.insert(w.route_key.clone(), Retry { due, ladder });
                } else {
                    self.retry.remove(&w.route_key);
                }
                let again = if fx_closed {
                    "It is started at the FX open, once (no login is made before it)"
                } else if transient {
                    "It is asked again on a backoff and at the next change of the rows"
                } else {
                    "It is tried again at the next change of the rows or of what its start reads \
                     (its keys, its venue's settings, the flags)"
                };
                self.say(&format!("not-running:{}:{reason}", w.route_key), || {
                    tracing::warn!(
                        "ARMING: {} did NOT start in the running tradehub: {reason}. It is NOT RUNNING \
                         for that reason; nothing else was touched. {again}",
                        w.route_key
                    );
                });
            }
            HotAccountStart::RestartRequired(why) => {
                self.retry.remove(&w.route_key);
                let restart = NotRunningReason::RestartRequired;
                self.set_not_running(ArmingNotRunningView {
                    account: AccountNamed::of(&w.venue, w.label.clone()),
                    tier: w.tier.as_ref().ok().copied(),
                    reason: restart.to_string(),
                    block: None,
                    refusal_id: Some(restart.shown_id().to_owned()),
                });
                self.say(&format!("restart:{}:{why}", w.route_key), || {
                    tracing::error!(
                        "ARMING: {} is active but is NOT RUNNING and starts only with a restart of this \
                         tradehub: {why}",
                        w.route_key
                    );
                });
            }
            HotAccountStart::CoreGone => self.done = true,
        }
    }

    /// Put `entry` in the list (replacing the account's older entry) and publish it when it changed.
    fn set_not_running(&mut self, entry: ArmingNotRunningView) {
        let before = self.not_running.clone();
        let key = entry.route_key();
        self.not_running.retain(|nr| nr.route_key() != key);
        self.not_running.push(entry);
        if self.not_running != before {
            self.world.publish_not_running(&self.not_running);
        }
    }
}

/// Why an account of `venue` at `tier` cannot be started in the running tradehub for want of the right
/// Hyperliquid price feed, or `None` when it can. Hyperliquid's price feed is ONE per venue wired for one
/// network at the boot, and nothing rebuilds it: a `live` account (the main network) started beside a feed
/// wired for the TEST network would have its opening orders priced and ceiling-checked off the test book. The
/// reverse is allowed as it always was (a `demo` account beside the main-network feed: mainnet wins, said at
/// the boot). The account is then NOT RUNNING, so the control door refuses its orders (foreign coins
/// included) until the restart.
pub(crate) fn hyperliquid_feed_refusal(
    venue: &str,
    tier: Result<AccountTier, vike_model::ArmingBlock>,
    feed: Option<vike_model::VenueNetwork>,
) -> Option<String> {
    (venue == "hyperliquid"
        && feed == Some(vike_model::VenueNetwork::Testnet)
        && tier.is_ok_and(|t| vike_model::VenueNetwork::for_tier(t) == vike_model::VenueNetwork::Mainnet))
    .then(|| {
        "Hyperliquid's price feed was wired for the TEST network when this tradehub started, and a `live` \
         account trades on the main network, so it would be priced off the wrong book. It starts with the \
         next restart of this tradehub (batch every account change into that one restart)"
            .to_string()
    })
}

/// What the boot wired for the price feeds, which a hot start is checked against BEFORE anything is dialled
/// (`AccountFollowWorldLive::start`). A struct of its own so the decision runs without a core.
struct StartGuards {
    held_venues: HashSet<String>,
    hl_feed_network: Option<vike_model::VenueNetwork>,
}

impl StartGuards {
    /// The restart-only verdict for starting `label`'s account of `venue` now, or `None` when the feeds allow it.
    fn refusal(
        &self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> Option<HotAccountStart> {
        if self.held_venues.contains(venue) {
            return Some(HotAccountStart::RestartRequired(format!(
                "{venue}'s price feed was not wired at boot (the datahub was away or refused it), so an \
                 account started now would trade without prices"
            )));
        }
        hyperliquid_feed_refusal(
            venue,
            account_tier(Some(accounts), venue, label),
            self.hl_feed_network,
        )
        .map(HotAccountStart::RestartRequired)
    }
}

/// The production [`AccountFollowWorld`].
struct AccountFollowWorldLive {
    rows: AccountFollowRows,
    hot: HotAccounts,
    fresh_backend: Option<AccountFollowBackend>,
    data_only: HashSet<String>,
    guards: StartGuards,
    reconcile_legs: Option<Sender<ReconcileLegChange>>,
    publisher: Option<crate::publish::PublisherHandle>,
    /// The process's stop flag: a stop waiting for cancels gives up the wait when it is raised, so the
    /// shutdown join does not sit out [`CANCEL_SETTLE`].
    stop: Arc<AtomicBool>,
    facts: Option<Arc<TradeLeaseRegistry>>,
    policy_cap: bool,
}

impl AccountFollowWorld for AccountFollowWorldLive {
    fn rows_changed(&mut self) -> Result<bool, String> {
        self.rows.watch.changed().map_err(|e| e.to_string())
    }

    fn accounts(&mut self) -> AccountDirectory {
        // The boot's own `data_only` withhold, so a declared venue's exec credentials stay out.
        self.data_only.iter().fold((self.rows.read)(), |dir, venue| {
            crate::venue_arming::withhold_data_only_credentials(&dir, venue).0
        })
    }

    fn start(
        &mut self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> HotAccountStart {
        // The guards that need no network go first: nothing is dialled for an account that cannot start.
        if let Some(early) = self.hot.check(accounts, venue, label) {
            return early;
        }
        if let Some(refused) = self.guards.refusal(accounts, venue, label) {
            return refused;
        }
        // A `paper` account reaches no venue: it starts over the backend of a box that dials nothing, so it needs
        // neither the gatewayhub nor a mount that connected to it at the boot.
        let backend: Arc<dyn VenueBackend> =
            if account_tier(Some(accounts), venue, label) == Ok(AccountTier::Paper) {
                Arc::new(vike_tradehub_core::VenueBackendPaper::new(accounts))
            } else if let Some(fresh) = &self.fresh_backend {
                fresh(accounts.clone(), self.data_only.clone())
            } else {
                return HotAccountStart::RestartRequired(
                    "this mount's venue backend cannot follow the account rows".to_string(),
                );
            };
        let (hot, reconcile_legs) = (&mut self.hot, self.reconcile_legs.as_ref());
        let outcome =
            start_with_cap(self.facts.as_deref(), self.policy_cap, accounts, venue, label, || {
                hot.start_account(&backend, accounts.clone(), venue, label, reconcile_legs)
            });
        if matches!(outcome, HotAccountStart::Started { .. })
            && let Some(facts) = &self.facts
        {
            warm_twin_price(facts, accounts, venue, label);
            // THE ENGINE TAKES THE VENUE'S BALANCE BEFORE THE ACCOUNT IS REPORTED AS RUNNING (decision 0246;
            // COLD): the start returns the moment the engine and its leg are in, and the door would then hold
            // the account's opening orders for the seconds the first pass takes. Wait for it here instead,
            // bounded, so the account shows as running when it can trade; a venue that did not answer in time
            // leaves the door's hold (by name, with the leg's reason) and the account is reported as running.
            await_first_balance(
                facts,
                &route_key_of(venue, label),
                &self.stop,
                vike_tradehub_core::FIRST_BALANCE_BOUND,
            );
        }
        outcome
    }

    fn publish_not_running(&mut self, list: &[ArmingNotRunningView]) {
        if let Some(publisher) = &self.publisher {
            publisher.set_not_running(list.to_vec());
        }
    }

    fn stopping(
        &mut self,
        venue: &str,
        label: &AccountLabel,
        route_key: &str,
        tier: Option<AccountTier>,
        stop: AccountStop<'_>,
    ) -> ArmingNotRunningView {
        // The positions come off the published snapshot (a read, never the fold). With no observe address
        // there is no snapshot here and they are not named; the stop does not depend on them.
        let open = self
            .publisher
            .as_ref()
            .map(|p| open_positions(&p.engine_blocks(), route_key))
            .unwrap_or_default();
        if !open.is_empty() {
            tracing::error!(
                route_key,
                "ARMING: {route_key} is being STOPPED with a position open at the venue ({}). The stop \
                 cancels its resting orders and closes its session; it does NOT close the position and \
                 it does not hide it: the account is listed NOT RUNNING with the position in its \
                 reason. Close it with `close-all` BEFORE deactivating or changing its tier next \
                 time (decision 0169 item 5)",
                open.iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", ")
            );
        }
        let (reason, block) = stop.entry_words(&open);
        ArmingNotRunningView {
            account: AccountNamed::of(venue, label.clone()),
            tier,
            refusal_id: reason.declared_id().map(str::to_owned),
            reason: reason.to_string(),
            block,
        }
    }

    fn stop(&mut self, entry: &ArmingNotRunningView) -> HotAccountStop {
        // Phase one cancels the account's own orders and keeps its client attached; the lease is closed
        // only after the venue has answered them (or the bound has passed), in `settle`. A lease closed at
        // once would drop the venue's `OrderCanceled` and leave the cancelled orders `Accepted` in the core.
        let stopped = self.hot.stop_account(entry.clone(), self.reconcile_legs.as_ref());
        if stopped == HotAccountStop::Stopped
            && let Some(facts) = &self.facts
        {
            facts.engine_stopped(&entry.route_key());
        }
        stopped
    }

    fn settle(&mut self, stopped: &[ArmingNotRunningView]) {
        let owned: Vec<String> = stopped.iter().map(ArmingNotRunningView::route_key).collect();
        let keys: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.await_cancels(&keys);
        for key in keys {
            // A core that is gone ends the watcher at its next command; there is nothing to do about it here.
            let _ = self.hot.detach_account(key);
        }
    }

    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn has_exposure(&mut self, route_key: &str) -> bool {
        holds_something(self.publisher.as_ref(), route_key)
    }

    fn publish_lane_twin_flags(&mut self, real: &[String], closing: &[String]) {
        if let Some(facts) = &self.facts {
            facts.set_lane_twin_flags(real, closing);
        }
    }

    fn publish_derived_lane_twins(&mut self, derived: &[String]) {
        if let Some(facts) = &self.facts {
            facts.set_derived_lane_twins(derived);
        }
    }

    fn publish_configured_accounts(
        &mut self,
        configured: Result<BTreeMap<String, Vec<String>>, String>,
    ) {
        if let Some(facts) = &self.facts {
            facts.set_configured_accounts(configured);
        }
    }

    fn key_changed(&mut self, accounts: &AccountDirectory) -> Option<AccountFollowKeyAnswer> {
        // One dial of the gatewayhub, on this thread, per commit: the gatewayhub holds the key values and
        // fingerprints what each adapter started with, this process reads no value. A mount that cannot
        // follow has no fresh backend and says nothing.
        let fresh = self.fresh_backend.as_ref()?;
        let backend = fresh(accounts.clone(), self.data_only.clone());
        let changed = backend.key_changed()?;
        Some(AccountFollowKeyAnswer {
            changed,
            start_inputs: backend.start_inputs().unwrap_or_default(),
        })
    }

    fn publish_key_changed(&mut self, list: &[String]) {
        if let Some(publisher) = &self.publisher {
            publisher.set_key_changed(list.to_vec());
        }
    }

    fn hold_account(&mut self, route_key: &str, hold: bool) -> bool {
        self.hot.hold_account(route_key, hold)
    }

    fn take_remount(&mut self, route_key: &str) -> bool {
        crate::venue_backend::gateway::late_currency::take_remount(route_key)
    }

    fn remount_refusal(
        &mut self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> Option<String> {
        if self.fresh_backend.is_none() {
            return Some("this mount's venue backend cannot follow the account rows".to_string());
        }
        match self.guards.refusal(accounts, venue, label) {
            Some(HotAccountStart::RestartRequired(why)) => Some(why),
            _ => None,
        }
    }
}

impl AccountFollowWorldLive {
    /// Wait ONCE, at most [`CANCEL_SETTLE`] for the whole batch, until the published snapshot holds no live
    /// order of any of `route_keys`. With no published snapshot (no observe address) there is nothing to read
    /// and the lease's own bounded ack drain is all the wait there is.
    ///
    /// ⚠ This blocks the ONE `account-watch` thread, so for up to [`CANCEL_SETTLE`] no other account's start,
    /// stop or retry is looked at. It ends at once when the process stop flag is raised. (The core refuses
    /// new orders to a stopped account from phase one, so a healthy venue settles well inside the bound.)
    fn await_cancels(&self, route_keys: &[&str]) {
        let Some(publisher) = &self.publisher else { return };
        let started = Instant::now();
        let mut left: Vec<&str> = Vec::new();
        while started.elapsed() < CANCEL_SETTLE && !self.stop.load(Ordering::Relaxed) {
            let orders = publisher.published_orders();
            left = route_keys.iter().copied().filter(|k| live_orders_of(&orders, k) > 0).collect();
            if left.is_empty() {
                return;
            }
            std::thread::sleep(SETTLE_POLL);
        }
        if !left.is_empty() {
            tracing::warn!(
                accounts = ?left,
                "ARMING: {left:?} still have live orders {CANCEL_SETTLE:?} after the stop; their leases \
                 close anyway. The cancels were sent first, so the venue has them; the core's view settles \
                 at the next reconcile or restart"
            );
        }
    }
}

/// Set by [`kick`], cleared by the watcher thread when it acts on it. Process-wide: one tradehub runs one
/// watcher, and the writers that kick it (the observe server's account verbs) hold no handle to it.
static KICK: AtomicBool = AtomicBool::new(false);

/// **Wake the watcher now.** For a writer that has just committed an account row: the thread follows it within
/// one slice (100 ms) instead of at the next poll. It wakes the watcher and nothing else: the row stays the
/// one authority, a kick that finds nothing changed starts and stops nothing, and a hand edit or the CLI is
/// still seen by the poll.
pub fn kick() {
    KICK.store(true, Ordering::Release);
}

/// Whether a [`kick`] is waiting, taking it.
fn take_kick() -> bool {
    KICK.swap(false, Ordering::AcqRel)
}

/// Whether this slice runs a tick: the poll is due, or a writer kicked.
fn tick_due(next: Duration, kicked: bool) -> bool {
    kicked || next.is_zero()
}

/// A running watcher thread.
pub struct AccountFollowHandle {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AccountFollowHandle {
    /// Join the thread (it ends within one slice of `stop`, or after the start it is inside returns).
    pub fn join(mut self) {
        // A panic in the watcher has been reported by its hook; teardown goes on.
        let _ = self.thread.take().map(std::thread::JoinHandle::join);
    }
}

/// Start the `account-watch` thread over `parts`. `publisher` is the observe server's (`None` when no
/// node address is configured), `stop` the process's one stop flag. `None` when the thread cannot be
/// spawned (said at `error`: the accounts then follow the restart path, as before this slice).
pub fn spawn(
    parts: AccountFollowParts,
    publisher: Option<crate::publish::PublisherHandle>,
    stop: Arc<AtomicBool>,
) -> Option<AccountFollowHandle> {
    let AccountFollowParts {
        hot,
        rows,
        fresh_backend,
        data_only,
        held_venues,
        hl_feed_network,
        reconcile_legs,
        not_running,
        live_switch,
        facts,
        policy_cap,
    } = parts;
    let running_keys = hot.running_route_keys();
    let world = AccountFollowWorldLive {
        rows,
        hot,
        fresh_backend,
        data_only,
        guards: StartGuards { held_venues, hl_feed_network },
        reconcile_legs,
        publisher,
        stop: Arc::clone(&stop),
        facts,
        policy_cap,
    };
    let spawned = std::thread::Builder::new().name("account-watch".to_string()).spawn(move || {
        let mut watcher =
            AccountFollowWatcher::new(world, running_keys, not_running).with_live_switch(live_switch);
        tracing::info!(
            "the account rows are followed: an account that becomes active starts in this running \
             tradehub (at its own tier, on any venue), an account that becomes inactive STOPS in it \
             (its resting orders are cancelled, its session and lease close; a position is left where \
             it is), an account whose row states another tier is stopped and started at that tier, \
             and a start the gatewayhub refused for want of an answer is asked again on a backoff"
        );
        // The first poll reports whatever committed while the node was assembling (the watch was
        // opened before the boot read the rows).
        let mut next = Duration::ZERO;
        while !stop.load(Ordering::Relaxed) && !watcher.done() {
            if tick_due(next, take_kick()) {
                // A bug in one tick must not end the following of the rows for the process's life: a
                // second start of an account the core already holds is refused by the core itself, so
                // trying again after a panic is safe.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| watcher.tick()))
                    .is_err()
                {
                    tracing::error!(
                        "ARMING: the account watcher panicked while following the rows; it keeps \
                         following, and an account it was starting is tried again at the next \
                         change of the rows"
                    );
                }
                next = POLL;
            }
            std::thread::sleep(SLICE.min(next));
            next = next.saturating_sub(SLICE);
        }
        // The driver this node started itself (reconciliation turned on by an account that started hot) is
        // ours to end; the boot's own is the teardown's.
        watcher.world.hot.begin_shutdown_hot_reconcile();
    });
    match spawned {
        Ok(thread) => Some(AccountFollowHandle { thread: Some(thread) }),
        Err(e) => {
            tracing::error!(error = %e, "the account-watch thread could not be spawned; accounts start with a restart only");
            None
        }
    }
}

#[path = "account_follow_tests.rs"]
#[cfg(test)]
mod account_follow_tests;
