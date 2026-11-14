//! **The late-currency heal: an engine started while the venue stated no currency is started again once it
//! does** (FXCM's login that only succeeds late; decision 0183, the node half).
//!
//! # The situation
//!
//! A currency travels from the venue to the node only INSIDE an `AdapterStart` reply. When the venue cannot
//! state it at the start (FXCM's ForexConnect login failed, so the reconcile session that reads `BASE_CRNCY` is
//! not open), the gatewayhub tells an ASSUMED USD, and the node anchors the engine's ledger to that (or, for a
//! journal that names another money, to the journal's, which the mount fence then declines): the engine is
//! mounted on the pair its row states (`EURUSD`). The login later succeeds on the gatewayhub's schedule, the gatewayhub then
//! KNOWS the account is in pounds, and nothing told the node: no start is sent again to a running adapter, except
//! by the reteach worker after a GATEWAYHUB restart (which used to throw the reply's currency away). So the
//! engine kept an assumed USD ledger for a GBP account, and `account.active` off and on was the only cure. A push
//! from the gatewayhub needs a new frame (an event, or a new epoch that would make the node believe the daemon
//! restarted): this module asks instead, from the node, with the verb it already has.
//!
//! # What it does
//!
//! 1. **Armed at the start, for the one account that can follow** (`Late::armed`): the account converts a quote
//!    currency, the venue did NOT state its currency in the start reply (nothing at all, or the gatewayhub's own
//!    assumed USD), the lease runs on the symbol the account's row states, and that row states a pair to move to
//!    (its `currency_symbol` fact: fxcm's GBP account's `EURGBP`). Any other lease asks nothing, ever. A currency the venue STATED at the
//!    start is never watched: a real change of a stated currency stays what it was, refused at the door.
//! 2. **The ask** (`reteach.rs`'s `ask_currency`, on the worker thread the lease already has): at the cadence of
//!    FXCM's own login retry ([`PAUSES`]: 30 s, 1, 2, 5 minutes, then every 10), when the daemon says the adapter
//!    is up, the SAME `AdapterStart` is sent again. A running adapter answers the same start from memory and
//!    reads the account's currency afresh, throttled on its side (`vike-gatewayhub`'s `retry_account_currency`),
//!    so an ask costs no login. The reteach worker judges the reply of a gatewayhub restart by the same rule.
//! 3. **The verdict** (`Late::hear`, pure): a reply that states a currency as the venue's WORD ends the watch.
//!    If `moved_mount` names a pair for it (fxcm GBP: `EURGBP`), the account has to be started again on that pair
//!    (`Heard::Moves`); if it names none, there is nothing the node can move (`Heard::Stays`, said once).
//! 4. **The request** ([`request_remount`]): a process-wide set of route keys, taken by the account watcher
//!    (`account_follow.rs`'s `Watcher::remount_asked`), which stops the account's engine and starts it again
//!    through the code a start runs, so `VenueBackendGateway::start_adapter` finds the stated currency and moves the
//!    mount ("Starting the session again on `EURGBP`") exactly as a fresh boot does. The WATCHER decides whether
//!    it may: never while the account holds a position or a live order (one `warn!`, the operator toggles), never
//!    when the start would be refused for a restart, and the engine keeps running in both cases. It also bounds
//!    the restarts (`REMOUNT_WINDOW`, `REMOUNT_MAX`): a start that lands on an assumed currency again re-arms the
//!    watch, and without the bound the next ask that hears the pound would restart the account again, for ever.
//!
//! # What it never does
//!
//! It sends no order, closes nothing and cancels nothing by itself, and moves no ledger in place (the engine is
//! rebuilt, as the toggle rebuilds it). It asks nothing of a lease whose adapter the daemon does not call up (a
//! dead adapter is not revived by a question).

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::AdapterReply;
use vike_model::AccountTier;

/// The pauses between two asks, the last one repeating: the cadence of FXCM's own login retry
/// (`crates/bridges/fxcm/src/reconcile/redial.rs`'s `pause_after`), so a login that succeeds late is heard about in the same
/// minutes. The gatewayhub's own throttle (`TradeConfig::account_currency_retry_after`, 30 s) is the floor.
pub const PAUSES: &[Duration] = &[
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
    Duration::from_secs(600),
];

/// What a reply said about the account's money, for a lease started without the venue's word on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Heard {
    /// Nothing the venue said: no currency, or the gatewayhub's own assumed one (or a client at another tier).
    Nothing,
    /// The venue states `currency`, which the mount cannot carry and `pair` can: the account is to be started
    /// again, and its start moves the mount to `pair`.
    Moves { currency: String, pair: String },
    /// The venue states `currency` and no pair moves the mount for it: nothing the node can change.
    Stays { currency: String },
}

/// The watch of one lease: armed at its start, asked on [`PAUSES`], over once the venue has stated a currency.
#[derive(Debug)]
pub(super) struct Late {
    route_key: String,
    converts: bool,
    /// The pair the account's row states (its `currency_symbol` fact), which a stated currency may move the mount to.
    pair: Option<String>,
    pauses: &'static [Duration],
    asked: usize,
    next: Instant,
    over: bool,
}

/// The pause after `asked` asks; the last pause repeats, and an empty list is ten minutes.
fn pause(pauses: &[Duration], asked: usize) -> Duration {
    pauses.get(asked).or(pauses.last()).copied().unwrap_or(Duration::from_secs(600))
}

impl Late {
    /// The watch for a lease, or `None` when it needs none: the account does not convert, the venue itself stated
    /// its currency (`venue_said`), or the account's row states no pair to move to (`pair`: its `currency_symbol`
    /// fact, which the start hands over only for a lease on the symbol that row states; a mount somebody chose is
    /// their word).
    pub(super) fn armed(
        converts: bool,
        venue_said: bool,
        route_key: &str,
        pauses: &'static [Duration],
        pair: Option<&str>,
    ) -> Option<Self> {
        (converts && !venue_said && pair.is_some()).then(|| Late {
            route_key: route_key.to_string(),
            converts,
            pair: pair.map(str::to_string),
            pauses,
            asked: 0,
            next: Instant::now() + pause(pauses, 0),
            over: false,
        })
    }

    /// The account the watch is about, as the account watcher spells it.
    pub(super) fn route_key(&self) -> &str {
        &self.route_key
    }

    /// Whether the venue has stated a currency, so there is nothing left to ask.
    pub(super) fn is_over(&self) -> bool {
        self.over
    }

    /// Whether an ask is due at `now`.
    pub(super) fn due(&self, now: Instant) -> bool {
        !self.over && now >= self.next
    }

    /// An ask was made at `now`: the next one is a pause later (the last pause repeats).
    pub(super) fn asked(&mut self, now: Instant) {
        self.asked += 1;
        self.next = now + pause(self.pauses, self.asked);
    }

    /// **Judge a reply of the same start.** Only a `Live` reply at the account's own tier that states a currency as
    /// the VENUE'S word counts (a blank statement is none, the gatewayhub's assumed USD is its silence): it ends the
    /// watch, whatever it names. Whether a pair follows it is `moved_mount`'s rule, the one a fresh start applies.
    pub(super) fn hear(
        &mut self,
        venue: &str,
        tier: AccountTier,
        mount: &str,
        reply: &AdapterReply,
    ) -> Heard {
        let AdapterReply::Running {
            bound_tier,
            account_currency: Some(currency_value),
            account_currency_assumed: false,
            ..
        } = reply
        else {
            return Heard::Nothing;
        };
        let currency_value = currency_value.trim().to_ascii_uppercase();
        if currency_value.is_empty() || *bound_tier != tier {
            return Heard::Nothing;
        }
        self.over = true;
        match super::moved_mount(venue, tier, self.converts, mount, reply, self.pair.as_deref()) {
            Some(pair) => Heard::Moves { currency: currency_value, pair },
            None => Heard::Stays { currency: currency_value },
        }
    }
}

/// The accounts a lease worker asked to have started again. Process-wide, like `account_follow::kick`: the worker
/// and the account watcher hold no handle to each other.
static REMOUNTS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn remounts() -> MutexGuard<'static, BTreeSet<String>> {
    REMOUNTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// **Ask the account watcher to stop `route_key`'s engine and start it again** (and wake it). The watcher alone
/// decides whether it may (the module doc, item 4).
pub fn request_remount(route_key: &str) {
    remounts().insert(route_key.to_string());
    crate::account_follow::kick();
}

/// Whether `route_key` was asked for, taking the request.
pub fn take_remount(route_key: &str) -> bool {
    remounts().remove(route_key)
}

/// Drop a request left by an earlier lease of `route_key`: a request belongs to the engine whose worker made it,
/// and a start that begins now builds another.
pub(super) fn forget_remount(route_key: &str) {
    remounts().remove(route_key);
}

#[path = "late_currency_tests.rs"]
#[cfg(test)]
mod late_currency_tests;
