//! **The account watcher: an account follows its `active` flag in the RUNNING tradehub** (decision
//! 0169, slice S1 — a `demo` account is STARTED; slice S2 — an account is STOPPED; slice S3 — a `live`
//! account and a venue the node mounted nothing for are STARTED, and a transient refusal is retried).
//!
//! One named thread (`account-watch`, outside `vike-core`, the shape of `gateway-late-arm`) holds a
//! read-only connection to the settings database and asks `PRAGMA data_version` once a second
//! ([`vike_secrets::StoreWatch`]); the number moves exactly when ANOTHER connection committed, so
//! `vike-cli secrets account activate`, a hand edit and a restored database are all seen, and a
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
//!   backoff** ([`FIRST_POLL`] doubling to [`MAX_POLL`], the late arm's own pauses): the gatewayhub
//!   that did not answer (`gatewayhub unavailable`, also an account the node booted without for want
//!   of it: the late-arm restart is not ordered for those any more, decision 0169 item 9), and an
//!   `adapter failed` within [`LEASE_CLOSE_GRACE`] of this watcher's own stop of the same account (its
//!   old lease was still closing, so switching an account off and on within seconds works). Any other
//!   refusal is permanent until the rows change: nothing loops on it;
//! * a RUNNING account whose row now states ANOTHER tier (`demo` to `live`, a `paper` row left alone beside
//!   a deactivated `demo` one, ...) is STOPPED the same way and STARTED again at the tier the row states, in
//!   the same pass (decision 0169 item 11, slice S4): the stop's cancel of its own resting orders and the
//!   close of its session come first, the start crosses every gate a start crosses, and a refusal is that
//!   account NOT RUNNING with its reason, never the old tier and never a stand-in;
//! * what is not followed yet is REPORTED, never ignored (item 11): an account whose price feed was not wired
//!   at boot (that venue still waits for the late-arm restart), and a RUNNING account whose row was deleted
//!   or stopped answering (an unreadable table is never taken for "no accounts") — each an `error` line
//!   saying so, once;
//! * **a key changed under a RUNNING account is a warning, never a restart** (owner ruling 2026-10-17: a
//!   restart cancels the account's resting orders, so it is the operator's decision, in his next batched
//!   restart): the gatewayhub, which alone holds the key values (this tradehub reads none), fingerprints the
//!   key set each adapter started with and names the accounts whose key in the database differs; the
//!   watcher asks on every change of the rows, lists them in the observe frame (`WireSnapshot::key_changed`)
//!   and in one `ARMING:` line, and drops the entry when the account stops or starts anew.
//!
//! A writer that already knows it committed (the account-administration verbs of the observe server) may
//! [`kick`] the thread so the row is followed within one slice instead of one poll; the row stays the one
//! authority and the poll stays the only trigger a hand edit has.
//!
//! The decision (what is wanted, what runs, what to say) is a plain state machine over the `World`
//! trait, so the boundary cases are tests with a scripted world; `LiveWorld` is the production one.
//! The late arm stays the fallback for the price feeds of a venue (`late_arm.rs`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use vike_config::ArmingBlock;
use vike_config::account_arming::{account_tier, known_accounts};
use vike_exec::NotRunningAccount;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub_client::wire::KEY_CHANGED_WARNING;
use vike_tradehub_core::{HotAccounts, HotStart, HotStop, LegChange, NotRunning, VenueBackend};

use crate::late_arm::{FIRST_POLL, MAX_POLL};
use crate::remote_backend::facts::{LeaseFacts, rows_arm_a_cap};

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
/// an account the boot table did not know. `RemoteCtor`'s; `None` for a backend that cannot follow.
pub type FreshBackend =
    Box<dyn Fn(AccountDirectory, HashSet<String>) -> Arc<dyn VenueBackend> + Send + Sync>;

/// The settings database as the watcher sees it: a commit counter and the account rows.
pub struct Rows {
    /// Opened BEFORE the boot read the account rows, so a commit made while the node was assembling is
    /// reported by the first poll instead of being lost between the boot's read and the watch.
    pub watch: vike_secrets::StoreWatch,
    /// The account rows now (the boot's own reader, so the two cannot disagree about what a row is).
    pub read: Box<dyn Fn() -> AccountDirectory + Send>,
}

/// Everything the watcher needs, assembled by the live mount.
pub struct FollowParts {
    /// What starts one more account in the core.
    pub hot: HotAccounts,
    /// The rows to follow.
    pub rows: Rows,
    /// A maker of fresh backends; `None` = this mount cannot follow (an in-process backend), and every
    /// start is reported as a restart.
    pub fresh_backend: Option<FreshBackend>,
    /// The venues the mount withholds exec credentials from (`data_only`): applied to the rows of
    /// every start as the boot applied them.
    pub data_only: HashSet<String>,
    /// Venues whose price feed was NOT wired at boot (held back or capped): an account started there
    /// would trade without prices, so it keeps the restart path.
    pub held_venues: HashSet<String>,
    /// The network Hyperliquid's price feed was wired for at the boot (a mount's feed or the price-only one),
    /// `None` when none was: a `live` account hot-started on a TEST-network feed would be priced and
    /// ceiling-checked off the test book, so it keeps the restart path ([`hyperliquid_feed_refusal`]).
    pub hl_feed_network: Option<crate::HlNetwork>,
    /// The reconcile driver's leg mailbox; `None` when reconcile is off.
    pub recon_legs: Option<Sender<LegChange>>,
    /// The accounts the node did not run at boot, with why: the list the watcher keeps current.
    pub not_running: Vec<NotRunningAccount>,
    /// `flags.tradehub_live`, as the mount was booted with it (a restart-class setting): a `live`
    /// account is not started while it is off.
    pub live_switch: bool,
    /// The control door's facts registry (`None` on a mount without one): told what each hot-started
    /// engine's account ceiling is, and when an engine stops, so the door answers for the engine that
    /// runs and not for the boot's rows (`LeaseFacts::engine_starting`, `LeaseFacts::engine_stopped`).
    pub facts: Option<Arc<LeaseFacts>>,
    /// Whether the box-wide `policy.max_account_exposure` row was set at the boot (the figure every
    /// engine, hot-started ones included, is built under).
    pub policy_cap: bool,
}

/// **Run one start so the door knows the ceiling of the engine it builds BEFORE that engine exists**:
/// the ceiling is read from the rows the start is built from and announced (with a new engine
/// generation) ahead of `start`, so the door never reads the previous engine's answer for the new one;
/// a start that built no engine puts the previous answer back.
pub(crate) fn start_with_cap(
    facts: Option<&LeaseFacts>,
    policy_cap: bool,
    accounts: &AccountDirectory,
    venue: &str,
    label: &AccountLabel,
    start: impl FnOnce() -> HotStart,
) -> HotStart {
    let Some(facts) = facts else { return start() };
    let key = route_key_of(venue, label);
    let before = facts.engine_starting(&key, rows_arm_a_cap(policy_cap, accounts, venue, label));
    let outcome = start();
    if !matches!(outcome, HotStart::Started { .. }) {
        facts.engine_start_refused(&key, before);
    }
    outcome
}

/// One account the rows name, resolved through the one authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub(crate) venue: String,
    pub(crate) label: AccountLabel,
    pub(crate) route_key: String,
    /// The tier it runs at, or the block that says it does not.
    pub(crate) tier: Result<AccountTier, ArmingBlock>,
}

/// Every account the rows know of on any roster venue, resolved by `account_tier`: the function the
/// boot path calls, so a row means the same thing to both.
pub(crate) fn wanted_accounts(accounts: &AccountDirectory) -> Vec<Wanted> {
    let mut out = Vec::new();
    for venue in vike_model::VENUES.iter() {
        for label in known_accounts(venue, Some(accounts)) {
            let tier = account_tier(Some(accounts), venue, &label);
            out.push(Wanted {
                venue: (*venue).to_string(),
                route_key: route_key_of(venue, &label),
                label,
                tier,
            });
        }
    }
    out
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

/// The reason an account is listed with when it stops: the arming word, and each position it still holds
/// at the venue (`SYMBOL size`), because a stop does not close a position and must not hide one.
pub(crate) fn stop_reason(open: &[(String, f64)]) -> String {
    with_positions(ArmingBlock::AccountInactive.as_str().to_string(), open)
}

/// The reason an account is listed with while it restarts at another tier: it is not inactive, it is about
/// to run again. The positions it holds are named for the reason a stop names them.
pub(crate) fn restart_reason(to: AccountTier, open: &[(String, f64)]) -> String {
    with_positions(format!("tier changed, restarting at `{to}`"), open)
}

fn with_positions(word: String, open: &[(String, f64)]) -> String {
    if open.is_empty() {
        return word;
    }
    let held: Vec<String> = open.iter().map(|(symbol, size)| format!("{symbol} {size}")).collect();
    format!("{word}; position open at venue: {}", held.join(", "))
}

/// How many orders of `route_key` are still live in the published `orders`.
pub(crate) fn live_orders_of(orders: &[vike_exec::OrderView], route_key: &str) -> usize {
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
    blocks: &[vike_exec::VenueBlock],
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

/// What the watcher touches. The production one is [`LiveWorld`].
pub(crate) trait World {
    /// Whether another connection committed to the settings database since the last call. `Err` is a
    /// store that exists and cannot be read (said at `error`; what runs does not change).
    fn rows_changed(&mut self) -> Result<bool, String>;
    /// The account rows now.
    fn accounts(&mut self) -> AccountDirectory;
    /// Try to start ONE account. Blocking.
    fn start(&mut self, accounts: &AccountDirectory, venue: &str, label: &AccountLabel)
    -> HotStart;
    /// The accounts that do not run, as they stand: the wire's list.
    fn publish_not_running(&mut self, list: &[NotRunningAccount]);
    /// The entry `route_key` is listed with once stopped: the arming word (or, for a restart, the tier it comes
    /// back at: `restart_at`) and the positions it holds (said at `error` when there are any). Reads only;
    /// stops nothing.
    fn stopping(
        &mut self,
        venue: &str,
        label: &AccountLabel,
        route_key: &str,
        tier: Option<AccountTier>,
        restart_at: Option<AccountTier>,
    ) -> NotRunningAccount;
    /// Stop ONE account, phase one: its reconcile leg is retired and the core cancels its orders and refuses
    /// new ones. Non-blocking; the session is closed by [`Self::settle`].
    fn stop(&mut self, entry: &NotRunningAccount) -> HotStop;
    /// Phase two for every account `stop` answered `Stopped` in one pass: wait ONCE, bounded, for the venue
    /// to answer their cancels, then close all their sessions.
    fn settle(&mut self, stopped: &[NotRunningAccount]);
    /// The clock the retry backoff is read on.
    fn now(&mut self) -> Instant;
    /// **The route keys of accounts that run on a key set the database no longer holds**, as the process that
    /// holds the venue keys (the gatewayhub; this tradehub reads no key value) says now. `None` = it could
    /// not say (no gatewayhub to ask, no answer): what was said before stands. DEFAULT `None`.
    fn key_changed(&mut self, accounts: &AccountDirectory) -> Option<BTreeSet<String>> {
        let _ = accounts;
        None
    }
    /// The route keys of the RUNNING accounts to warn about, as they stand: the wire's list. DEFAULT nothing.
    fn publish_key_changed(&mut self, list: &[String]) {
        let _ = list;
    }
}

/// When a transiently refused account is asked again, and the pause that follows that attempt.
#[derive(Debug, Clone, Copy)]
struct Retry {
    due: Instant,
    pause: Duration,
}

/// **Whether a refusal is only a matter of time** (decision 0169 item 9): the gatewayhub or the price path
/// that did not answer, and an `adapter failed` within [`LEASE_CLOSE_GRACE`] of this watcher's own stop of
/// the same account (`since_stop`: how long ago, `None` when it never stopped it). Every other block is a
/// fact about the account, the venue or the build that only a change of the rows can alter.
///
/// ⚠ **Known limit:** the remote backend maps a held lease, a daemon error and a protocol error to the one
/// `AdapterFailed`, so a start refused for a bad credential right after our own stop is retried like a lease
/// that is still closing. It is bounded: only inside [`LEASE_CLOSE_GRACE`] of the stop, on the backoff
/// ([`FIRST_POLL`] doubling), and never after a change of the rows resets it to a fresh stop. Telling the two
/// apart needs a distinct block from the backend (`ArmingBlock` is a wire and roster type), a change of its
/// own.
pub(crate) fn is_transient(block: ArmingBlock, since_stop: Option<Duration>) -> bool {
    match block {
        ArmingBlock::GatewayUnavailable | ArmingBlock::FeedsUnavailable => true,
        ArmingBlock::AdapterFailed => since_stop.is_some_and(|ago| ago < LEASE_CLOSE_GRACE),
        _ => false,
    }
}

/// The state machine: what runs, what is published as not running, and what was already said.
pub(crate) struct Watcher<W: World> {
    world: W,
    /// Route key -> the tier it runs at (`None` = the rows no longer say, at construction).
    running: BTreeMap<String, Option<AccountTier>>,
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
    not_running: Vec<NotRunningAccount>,
    /// The RUNNING accounts (route keys) whose key in the database is no longer the one their session
    /// started with: published as a warning, never acted on (a restart cancels the account's resting orders,
    /// so the restart stays the operator's, in his next batched one).
    key_changed: BTreeSet<String>,
    /// Lines already said, so a commit that changes nothing about an account does not repeat them.
    said: BTreeSet<String>,
    /// Set when the core is gone: the process is going down and there is nothing left to follow.
    done: bool,
}

impl<W: World> Watcher<W> {
    /// A watcher over `world` for a node that runs `running_keys` (the route keys of the engines the
    /// core was spawned with) and did not run `not_running`. An account the node booted without for want
    /// of the gatewayhub is asked again [`FIRST_POLL`] from now, and on the backoff after that.
    pub(crate) fn new(
        mut world: W,
        running_keys: Vec<String>,
        not_running: Vec<NotRunningAccount>,
    ) -> Self {
        let wanted = wanted_accounts(&world.accounts());
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
        let retry = not_running
            .iter()
            .filter(|nr| nr.reason == ArmingBlock::GatewayUnavailable.as_str())
            .map(|nr| (nr.route_key.clone(), Retry { due: booted + FIRST_POLL, pause: FIRST_POLL }))
            .collect();
        Watcher {
            world,
            running,
            stopped: BTreeMap::new(),
            stopped_at: BTreeMap::new(),
            retry,
            live_switch: true,
            not_running,
            key_changed: BTreeSet::new(),
            said: BTreeSet::new(),
            done: false,
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
    pub(crate) fn tick(&mut self) {
        match self.world.rows_changed() {
            Ok(false) => {
                if self.retry_due() {
                    self.follow(true);
                }
            }
            Ok(true) => self.follow(false),
            Err(e) => self.say(&format!("unreadable:{e}"), || {
                tracing::error!(
                    error = %e,
                    "ARMING: the settings database exists and cannot be read, so the account rows are \
                     NOT being followed (nothing that runs changes; this is NOT \"no accounts\"). The \
                     next poll tries again"
                );
            }),
        }
    }

    /// Say `f` once per distinct `key`.
    fn say(&mut self, key: &str, f: impl FnOnce()) {
        if self.said.insert(key.to_string()) {
            f();
        }
    }

    /// Re-read the rows and act on the difference with what runs. `retry_only`: this is not a change of the
    /// rows but a backoff running out, so only the accounts that are DUE are asked, and nothing is stopped
    /// or reported again (the rows are the ones the last change was followed on).
    fn follow(&mut self, retry_only: bool) {
        let accounts = self.world.accounts();
        let wanted = wanted_accounts(&accounts);
        if !retry_only {
            // Stops first (decision 0169 / the plan's section 2): releasing an account never waits
            // behind a start's gatewayhub round trip.
            self.stop_what_the_rows_stopped(&wanted);
            self.report_running_accounts_that_differ(&wanted);
        }
        let now = self.world.now();
        for w in &wanted {
            if self.done {
                return;
            }
            let key = &w.route_key;
            if self.running.contains_key(key) || w.tier.is_err() {
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

    /// **A key changed under a RUNNING account is a warning, never a restart** (owner ruling 2026-10-17: a
    /// restart cancels the account's resting orders, so it stays the operator's decision). The process that
    /// holds the key values says which running accounts started on a key the database no longer holds
    /// ([`World::key_changed`]); this lists them on the wire (`KEY_CHANGED_WARNING`) and says so once per
    /// change. An account that restarts is judged afresh by the next answer, one that stopped is dropped
    /// at once, and a question nobody could answer changes nothing.
    fn follow_key_changes(&mut self, accounts: &AccountDirectory) {
        let asked = if self.running.is_empty() {
            Some(BTreeSet::new())
        } else {
            self.world.key_changed(accounts)
        };
        let Some(asked) = asked else { return };
        let now: BTreeSet<String> =
            asked.into_iter().filter(|key| self.running.contains_key(key)).collect();
        if now == self.key_changed {
            return;
        }
        let raised: Vec<String> = now.difference(&self.key_changed).cloned().collect();
        let cleared: Vec<String> = self.key_changed.difference(&now).cloned().collect();
        self.key_changed = now;
        for key in raised {
            tracing::warn!(
                "ARMING: {key} is RUNNING and its {KEY_CHANGED_WARNING}. It keeps trading with the old \
                 key and nothing restarts it by itself, because a restart cancels its resting orders. \
                 To take the new key, restart it with the next batched restart, or deactivate its row \
                 and activate it again (close its positions first)"
            );
        }
        for key in cleared {
            tracing::info!("ARMING: {key} no longer has a key that differs from the database's");
        }
        self.publish_key_changed();
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
    fn start(&mut self, accounts: &AccountDirectory, w: &Wanted) -> HotStart {
        if w.tier == Ok(AccountTier::Live) && !self.live_switch {
            return HotStart::NotRunning(NotRunning {
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
    /// Phase one is sent for EVERY account first, then [`World::settle`] waits ONCE (bounded) for all their
    /// cancels and detaches them together: ten deactivations in one commit cost one wait, not ten.
    fn stop_what_the_rows_stopped(&mut self, wanted: &[Wanted]) {
        let mut sent: Vec<NotRunningAccount> = Vec::new();
        for (key, ran_at) in self.running.clone() {
            if self.done {
                return;
            }
            let Some(w) = wanted.iter().find(|w| w.route_key == key) else { continue };
            let Some(what) = change(ran_at, &w.tier) else { continue };
            let new_tier = w.tier.as_ref().ok().copied();
            let restart_at = if what == Change::Restart { new_tier } else { None };
            let entry = self.world.stopping(&w.venue, &w.label, &key, new_tier, restart_at);
            self.set_not_running(entry.clone());
            let outcome = self.world.stop(&entry);
            match outcome {
                HotStop::Stopped | HotStop::NotRunning => {
                    self.running.remove(&key);
                    self.retry.remove(&key);
                    self.clear_key_changed(&key);
                    let at = self.world.now();
                    self.stopped_at.insert(key.clone(), at);
                    // A stop stays stopped on this answer; a restart is started in this very pass.
                    if what == Change::Stop {
                        self.stopped.insert(key, w.tier);
                    } else {
                        self.stopped.remove(&key);
                    }
                    if outcome == HotStop::Stopped {
                        sent.push(entry);
                    }
                }
                HotStop::CoreGone => self.done = true,
            }
        }
        if !sent.is_empty() && !self.done {
            self.world.settle(&sent);
        }
    }

    /// Decision 0169 item 11: a row that differs from what runs and that the watcher does not act on is
    /// reported, never ignored: a row that stopped naming a running account (a deleted row) leaves it running.
    fn report_running_accounts_that_differ(&mut self, wanted: &[Wanted]) {
        for (key, ran_at) in self.running.clone() {
            let now = wanted.iter().find(|w| w.route_key == key).map(|w| w.tier);
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
                     to stop it, after closing its positions with `market-exit`"
                );
            });
        }
    }

    /// Fold one start's outcome (made at `now`) into what runs, what is published and what is asked again.
    fn record(&mut self, w: &Wanted, outcome: HotStart, now: Instant) {
        match outcome {
            HotStart::Started { route_key, tier } => {
                self.stopped.remove(&route_key);
                self.stopped_at.remove(&route_key);
                self.retry.remove(&route_key);
                self.clear_key_changed(&route_key);
                self.running.insert(route_key.clone(), Some(tier));
                self.not_running.retain(|nr| nr.route_key != route_key);
                self.world.publish_not_running(&self.not_running);
            }
            HotStart::AlreadyRunning => {
                let tier = w.tier.as_ref().ok().copied();
                self.retry.remove(&w.route_key);
                self.running.insert(w.route_key.clone(), tier);
            }
            HotStart::NotRunning(nr) => {
                let since_stop =
                    self.stopped_at.get(&w.route_key).map(|at| now.saturating_duration_since(*at));
                let transient = is_transient(nr.block, since_stop);
                let view = nr.view();
                let reason = view.reason.clone();
                self.set_not_running(view);
                if transient {
                    let pause = self
                        .retry
                        .get(&w.route_key)
                        .map_or(FIRST_POLL, |r| (r.pause * 2).min(MAX_POLL));
                    self.retry.insert(w.route_key.clone(), Retry { due: now + pause, pause });
                } else {
                    self.retry.remove(&w.route_key);
                }
                let again = if transient {
                    "It is asked again on a backoff and at the next change of the rows"
                } else {
                    "It is tried again at the next change of the rows"
                };
                self.say(&format!("not-running:{}:{reason}", w.route_key), || {
                    tracing::warn!(
                        "ARMING: {} did NOT start in the running tradehub: {reason}. It is NOT RUNNING \
                         for that reason; nothing else was touched. {again}",
                        w.route_key
                    );
                });
            }
            HotStart::RestartRequired(why) => {
                self.retry.remove(&w.route_key);
                self.set_not_running(NotRunningAccount {
                    venue: w.venue.clone(),
                    account: (!w.label.is_default()).then(|| w.label.clone()),
                    route_key: w.route_key.clone(),
                    tier: w.tier.as_ref().ok().copied(),
                    reason: "restart required to start".to_string(),
                });
                self.say(&format!("restart:{}:{why}", w.route_key), || {
                    tracing::error!(
                        "ARMING: {} is active but is NOT RUNNING and starts only with a restart of this \
                         tradehub: {why}",
                        w.route_key
                    );
                });
            }
            HotStart::CoreGone => self.done = true,
        }
    }

    /// Put `entry` in the list (replacing the account's older entry) and publish it when it changed.
    fn set_not_running(&mut self, entry: NotRunningAccount) {
        let before = self.not_running.clone();
        self.not_running.retain(|nr| nr.route_key != entry.route_key);
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
    tier: Result<AccountTier, vike_config::ArmingBlock>,
    feed: Option<crate::HlNetwork>,
) -> Option<String> {
    (venue == "hyperliquid"
        && feed == Some(crate::HlNetwork::Testnet)
        && tier.is_ok_and(|t| crate::HlNetwork::for_tier(t) == crate::HlNetwork::Mainnet))
    .then(|| {
        "Hyperliquid's price feed was wired for the TEST network when this tradehub started, and a `live` \
         account trades on the main network, so it would be priced off the wrong book. It starts with the \
         next restart of this tradehub (batch every account change into that one restart)"
            .to_string()
    })
}

/// What the boot wired for the price feeds, which a hot start is checked against BEFORE anything is dialled
/// (`LiveWorld::start`). A struct of its own so the decision runs without a core.
struct StartGuards {
    held_venues: HashSet<String>,
    hl_feed_network: Option<crate::HlNetwork>,
}

impl StartGuards {
    /// The restart-only verdict for starting `label`'s account of `venue` now, or `None` when the feeds allow it.
    fn refusal(
        &self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> Option<HotStart> {
        if self.held_venues.contains(venue) {
            return Some(HotStart::RestartRequired(format!(
                "{venue}'s price feed was not wired at boot (the datahub was away or refused it), so an \
                 account started now would trade without prices"
            )));
        }
        hyperliquid_feed_refusal(
            venue,
            account_tier(Some(accounts), venue, label),
            self.hl_feed_network,
        )
        .map(HotStart::RestartRequired)
    }
}

/// The production [`World`].
struct LiveWorld {
    rows: Rows,
    hot: HotAccounts,
    fresh_backend: Option<FreshBackend>,
    data_only: HashSet<String>,
    guards: StartGuards,
    recon_legs: Option<Sender<LegChange>>,
    publisher: Option<crate::publish::PublisherHandle>,
    /// The process's stop flag: a stop waiting for cancels gives up the wait when it is raised, so the
    /// shutdown join does not sit out [`CANCEL_SETTLE`].
    stop: Arc<AtomicBool>,
    facts: Option<Arc<LeaseFacts>>,
    policy_cap: bool,
}

impl World for LiveWorld {
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
    ) -> HotStart {
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
                Arc::new(vike_tradehub_core::PaperBackend::new(accounts))
            } else if let Some(fresh) = &self.fresh_backend {
                fresh(accounts.clone(), self.data_only.clone())
            } else {
                return HotStart::RestartRequired(
                    "this mount's venue backend cannot follow the account rows".to_string(),
                );
            };
        let (hot, recon_legs) = (&mut self.hot, self.recon_legs.as_ref());
        start_with_cap(self.facts.as_deref(), self.policy_cap, accounts, venue, label, || {
            hot.start_account(&backend, accounts.clone(), venue, label, recon_legs)
        })
    }

    fn publish_not_running(&mut self, list: &[NotRunningAccount]) {
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
        restart_at: Option<AccountTier>,
    ) -> NotRunningAccount {
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
                 reason. Close it with `market-exit` BEFORE deactivating or changing its tier next \
                 time (decision 0169 item 5)",
                open.iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", ")
            );
        }
        NotRunningAccount {
            venue: venue.to_string(),
            account: (!label.is_default()).then(|| label.clone()),
            route_key: route_key.to_string(),
            tier,
            reason: restart_at.map_or_else(|| stop_reason(&open), |to| restart_reason(to, &open)),
        }
    }

    fn stop(&mut self, entry: &NotRunningAccount) -> HotStop {
        // Phase one cancels the account's own orders and keeps its client attached; the lease is closed
        // only after the venue has answered them (or the bound has passed), in `settle`. A lease closed at
        // once would drop the venue's `OrderCanceled` and leave the cancelled orders `Accepted` in the core.
        let stopped = self.hot.stop_account(entry.clone(), self.recon_legs.as_ref());
        if stopped == HotStop::Stopped
            && let Some(facts) = &self.facts
        {
            facts.engine_stopped(&entry.route_key);
        }
        stopped
    }

    fn settle(&mut self, stopped: &[NotRunningAccount]) {
        let keys: Vec<&str> = stopped.iter().map(|nr| nr.route_key.as_str()).collect();
        self.await_cancels(&keys);
        for key in keys {
            // A core that is gone ends the watcher at its next command; there is nothing to do about it here.
            let _ = self.hot.detach_account(key);
        }
    }

    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn key_changed(&mut self, accounts: &AccountDirectory) -> Option<BTreeSet<String>> {
        // One dial of the gatewayhub, on this thread, only when a row moved: the gatewayhub holds the key
        // values and fingerprints what each adapter started with, this process reads no value. A mount that
        // cannot follow has no fresh backend and says nothing.
        let fresh = self.fresh_backend.as_ref()?;
        fresh(accounts.clone(), self.data_only.clone()).key_changed()
    }

    fn publish_key_changed(&mut self, list: &[String]) {
        if let Some(publisher) = &self.publisher {
            publisher.set_key_changed(list.to_vec());
        }
    }
}

impl LiveWorld {
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
pub struct FollowHandle {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FollowHandle {
    /// Join the thread (it ends within one slice of `stop`, or after the start it is inside returns).
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            // A panic in the watcher has been reported by its hook; teardown goes on.
            let _ = thread.join();
        }
    }
}

/// Start the `account-watch` thread over `parts`. `publisher` is the observe server's (`None` when no
/// node address is configured), `stop` the process's one stop flag. `None` when the thread cannot be
/// spawned (said at `error`: the accounts then follow the restart path, as before this slice).
pub fn spawn(
    parts: FollowParts,
    publisher: Option<crate::publish::PublisherHandle>,
    stop: Arc<AtomicBool>,
) -> Option<FollowHandle> {
    let FollowParts {
        hot,
        rows,
        fresh_backend,
        data_only,
        held_venues,
        hl_feed_network,
        recon_legs,
        not_running,
        live_switch,
        facts,
        policy_cap,
    } = parts;
    let running_keys = hot.running_route_keys();
    let world = LiveWorld {
        rows,
        hot,
        fresh_backend,
        data_only,
        guards: StartGuards { held_venues, hl_feed_network },
        recon_legs,
        publisher,
        stop: Arc::clone(&stop),
        facts,
        policy_cap,
    };
    let spawned = std::thread::Builder::new().name("account-watch".to_string()).spawn(move || {
        let mut watcher =
            Watcher::new(world, running_keys, not_running).with_live_switch(live_switch);
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
        Ok(thread) => Some(FollowHandle { thread: Some(thread) }),
        Err(e) => {
            tracing::error!(error = %e, "the account-watch thread could not be spawned; accounts start with a restart only");
            None
        }
    }
}

#[path = "account_follow_tests.rs"]
#[cfg(test)]
mod account_follow_tests;
