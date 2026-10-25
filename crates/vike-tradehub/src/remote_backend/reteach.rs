//! **After the gatewayhub restarts under a live lease: re-teach it, then reconcile at once** (wave 2,
//! W13; plan B.7, "orders across the hop").
//!
//! A new `gw_epoch` means everything the gatewayhub held in memory for the account is gone: the venue
//! adapter, the bridge's client-order-id to venue-order-id map (polymarket's CLOB ids, ig's deal ids,
//! ctrader's `OrderIdMap`, hyperliquid's cloid registry), the dedup set, the watermark. Resting orders
//! are still at the venue (decision D6: nothing cancels on a link loss), but a CANCEL of one fails with
//! "no venue order id for ..." until something teaches the map again. The link client announces the
//! change ([`LinkSignal::epoch_changed`], on the link thread) and this module is what the tradehub does
//! about it, on a thread of its own because the link thread cannot wait for its own answers:
//!
//! 1. **`AdapterStart` again** (`TradeSession::restart_adapter`), with the parameters of the first (the gatewayhub has no adapter, so
//!    nothing else works). Its answer releases the boot teaching the client held across the restart, and
//!    NOT the risk-off cancels it held: those wait for step 2's `TradeSession::release_held` (the
//!    teaching goes first, or a cancel fails with "no venue order id" on a venue whose map the restart
//!    emptied). A refused start (`NotRunning`) is said at `error`: the account no longer arms, and the
//!    held cancels become visible advisories.
//! 2. **`AdoptRestored` for EVERY resting order of the account**, read off the core's published
//!    snapshot ([`SnapshotCell`], filled by the composition root once the core is up): the same
//!    `(coid, unified symbol)` pairs the boot restore announces. A hyperliquid cancel works again. Then
//!    `release_held`, which sends the held cancels behind that teaching.
//! 3. **`ResolveOrders` for the submits that stayed UNKNOWN across the restart** (the client asked once
//!    at the reconnect, before any adapter existed, and was always answered "no resolver").
//! 4. **The admitted symbols, told again** (`TradeSession::retell_symbols`, decision 0174): the new daemon's
//!    reconcile client reads the mounted symbol only until it is told which others the engine admitted, and
//!    the facts cache would answer the next admission without asking it. One committing `SymbolFacts` request per
//!    symbol the old daemon was told (at most `MAX_EXTRA_SYMBOLS`), BEFORE the pass below, so that pass reads
//!    them; a symbol it refuses is asked again on a capped backoff, and until it is told the core HOLDS
//!    (never retires) the orders resting on it.
//! 5. **An immediate reconcile pass** (the manager's trigger): a bridge map that is only re-taught by a
//!    reconcile fetch (polymarket, ig, ctrader) is re-taught within one pass instead of one interval,
//!    and anything that happened while the gatewayhub was down is found (`MissingFill`, held under
//!    `quarantine`).
//!
//! Failure is retried, but not for ever and not into a teardown: while the lease is open an unanswered
//! `AdapterStart` (the link flapped again) is asked again on a capped backoff, and the held risk-off
//! cancels that wait on it are released after `HELD_CANCEL_PATIENCE` to be answered. A refusal asking
//! again cannot change (`Unsupported`, `Protocol`) ends the worker at once; a client the daemon bound at
//! another tier ends the LEASE, as the boot path does; and nothing is started on a lease whose teardown
//! has begun (`TradeSession::is_stopping`). Every way out of the worker releases the held risk-off cancels: after the teaching,
//! `release_held_anyway` when it gives up or when a teardown begins (before or after the adapter answered), so the shutdown
//! sweep's cancels go out ahead of the final `Detach` instead of becoming advisories while the adapter is up. The tier mismatch
//! alone discards them, by design (the lease ends and the account does not run).
//!
//! **The same thread carries the lease's late-currency watch** ([`super::late_currency`]): an fxcm lease started while
//! the venue stated no currency asks the same start again on a slow cadence, and the reply of step 1 is judged by the
//! same rule, so a currency the venue states late (a login that succeeded after the start, or a restarted daemon that
//! could log in) starts the account again on the pair that carries it, through the account watcher.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use vike_exec::{CoreSnapshot, ExecutionClient};
use vike_gatewayhub_client::proto::{AdapterReply, AdapterStart};
use vike_gatewayhub_client::trade::{
    CoreLinkSignal, LinkSignal, LinkState, TradeError, TradeSession,
};
use vike_model::AccountTier;

use super::late_currency::{self, Heard, Late};

/// The core's published snapshot, late-bound: the leases are opened while the core is being assembled
/// and the snapshot exists only after it spawned. Filled once by the composition root.
pub type SnapshotCell = Arc<OnceLock<Arc<ArcSwap<CoreSnapshot>>>>;

/// How long the worker waits on its inbox before it looks at the lease again.
const POLL: Duration = Duration::from_millis(250);
/// The first pause after an unanswered `AdapterStart`, doubling to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_millis(500);
/// The longest pause between asks.
const RETRY_MAX: Duration = Duration::from_secs(5);
/// How long the held risk-off cancels wait on an adapter that will not start before they are released to
/// be answered (an advisory each): a cancel left waiting for ever on a daemon that never comes back is a
/// resting order nobody is told about.
const HELD_CANCEL_PATIENCE: Duration = Duration::from_secs(30);
/// How long the worker keeps asking the restarted daemon for the symbols it refused to be told.
const RETELL_PATIENCE: Duration = Duration::from_secs(60);

/// The link signal the tradehub installs: everything [`CoreLinkSignal`] says, plus the epoch change
/// handed to the worker (the signal is called on the link thread, which must not wait for answers).
pub(super) struct ReteachSignal {
    core: CoreLinkSignal,
    tx: Sender<(u64, u64)>,
}

impl ReteachSignal {
    pub(super) fn new(core: CoreLinkSignal, tx: Sender<(u64, u64)>) -> Self {
        Self { core, tx }
    }
}

impl LinkSignal for ReteachSignal {
    fn link_lost(&self, why: &str) {
        self.core.link_lost(why);
    }

    fn link_restored(&self) {
        self.core.link_restored();
    }

    fn epoch_changed(&self, old: u64, new: u64) {
        // A closed channel is a worker that has ended with its lease: nothing left to re-teach.
        let _ = self.tx.send((old, new));
    }
}

/// Everything the worker needs to answer one epoch change.
pub(super) struct Reteach {
    pub(super) session: TradeSession,
    pub(super) start: AdapterStart,
    pub(super) venue: String,
    pub(super) label: Option<String>,
    pub(super) tier: AccountTier,
    pub(super) trigger: Option<Sender<()>>,
    pub(super) cell: SnapshotCell,
    /// The lease's watch for a currency the venue did not state at its start ([`late_currency`]); `None` for
    /// every lease that needs none.
    pub(super) late: Option<Late>,
}

impl Reteach {
    /// Run the worker on a thread named `gateway-reteach` until the lease closes.
    pub(super) fn spawn(self, rx: Receiver<(u64, u64)>) -> std::io::Result<()> {
        std::thread::Builder::new()
            .name("gateway-reteach".to_string())
            .spawn(move || self.run(&rx))
            .map(drop)
    }

    fn run(mut self, rx: &Receiver<(u64, u64)>) {
        let mut late = self.late.take();
        loop {
            match rx.recv_timeout(POLL) {
                Ok((old, new)) => {
                    // A burst of changes (a restart that flapped) is one re-teaching of the last.
                    let mut new = new;
                    while let Ok((_, n)) = rx.try_recv() {
                        new = n;
                    }
                    // A change that arrives while the re-teaching is still asking the daemon for
                    // symbols is answered next, at once, not after the patience runs out.
                    let mut change = Some((old, new));
                    while let Some((o, n)) = change {
                        change = self.reteach(rx, o, n, &mut late);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.session.link_state() == LinkState::Closed {
                        return;
                    }
                    if let Some(watch) = late.as_mut().filter(|w| w.due(Instant::now())) {
                        self.ask_currency(watch);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if late.as_ref().is_some_and(Late::is_over) {
                late = None;
            }
        }
    }

    /// **One ask of the late-currency watch** ([`late_currency`]): while the daemon calls the adapter up, the SAME
    /// `AdapterStart` is sent again (a plain start, not `restart_adapter`: nothing is taught, nothing is held), and
    /// the reply is judged ([`Self::hear_currency`]). Not asked of a lease that is closing, nor of an adapter the
    /// daemon does not call up (a question must not revive a dead one); a failed ask is the next pause's.
    fn ask_currency(&self, watch: &mut Late) {
        watch.asked(Instant::now());
        if self.gone() || !matches!(self.session.status(), Ok(s) if s.adapter_up) {
            return;
        }
        if let Ok(reply) = self.session.start_adapter(self.start.clone()) {
            self.hear_currency(watch, &reply);
        }
    }

    /// What a reply of the start says about the account's money, for a lease the venue gave no word on: a currency
    /// the venue states now and a pair follows asks the account watcher to start the account again (it decides
    /// whether it may, [`late_currency`]); one no pair follows is said once and left as it is.
    fn hear_currency(&self, watch: &mut Late, reply: &AdapterReply) {
        match watch.hear(&self.venue, self.tier, &self.start.symbol, reply) {
            Heard::Nothing => {}
            Heard::Moves { ccy, pair } => {
                tracing::warn!(
                    venue = %self.venue, route_key = watch.route_key(),
                    "{}: the venue now states the account's money is {ccy}, which this account's engine was NOT \
                     started on (the venue stated none at its start, so the ledger is anchored to an assumed \
                     currency on the wired mount `{}`). Asking the account watcher to start the account again, \
                     which starts the session on `{pair}`: it does so only while the account holds no position \
                     and no live order",
                    self.venue, self.start.symbol
                );
                late_currency::request_remount(watch.route_key());
            }
            Heard::Stays { ccy } => tracing::info!(
                venue = %self.venue, route_key = watch.route_key(),
                "{}: the venue now states the account's money is {ccy}; no pair carries that ledger for this \
                 venue, so the engine stays as it was (anchored to the currency it was assumed in, balance \
                 held) until the account is started again (`account.active` off and on)",
                self.venue
            ),
        }
    }

    /// The resting orders of this account, as `(coid, unified symbol)`. Empty before the core is up.
    pub(super) fn resting(&self) -> Vec<(String, String)> {
        resting_orders(
            self.cell.get().map(|c| c.load_full()).as_deref(),
            &self.venue,
            self.label.as_deref(),
        )
    }

    /// The lease is over or going over: nothing new may be started on it.
    fn gone(&self) -> bool {
        self.session.link_state() == LinkState::Closed || self.session.is_stopping()
    }

    fn reteach(
        &self,
        rx: &Receiver<(u64, u64)>,
        old: u64,
        new: u64,
        late: &mut Option<Late>,
    ) -> Option<(u64, u64)> {
        tracing::warn!(
            venue = %self.venue, old_epoch = old, new_epoch = new,
            "the gatewayhub restarted under this account's lease: starting the adapter again, \
             re-teaching its resting orders and reconciling at once"
        );
        let mut pause = RETRY_FIRST;
        let asked_since = Instant::now();
        let mut said_patience = false;
        loop {
            // Not during a teardown either: the lease's final `Detach` would retire an adapter started
            // in the ack drain, so the start would be spent on nothing.
            if self.gone() {
                // ...and the held risk-off cancels are not left held for a re-teaching that will not happen
                // (W13c): they go out ahead of the final `Detach`, the adapter's answer or not.
                self.session.release_held_anyway();
                return None;
            }
            match self.session.restart_adapter(self.start.clone()) {
                Ok(reply @ AdapterReply::Live { bound_tier, .. }) => {
                    if bound_tier != self.tier {
                        // As strict as the boot path (`RemoteBackend::start_adapter`): a client the
                        // daemon bound at another tier is a live order path the operator did not ask
                        // for (or a downgrade the account never agreed to). The lease ends; the held
                        // cancels get their advisory at the close, and the account takes no more orders.
                        tracing::error!(
                            venue = %self.venue,
                            "the restarted gatewayhub bound a client at tier {bound_tier} but the \
                             account's row states {}: refused, the lease is detached and the account \
                             does NOT RUN",
                            self.tier
                        );
                        self.session.detach();
                        return None;
                    }
                    // The restarted daemon may have logged in where the first one could not: a currency it states
                    // now, for a lease the venue gave no word on, is the late-currency watch's to judge.
                    if let Some(watch) = late.as_mut() {
                        self.hear_currency(watch, &reply);
                    }
                    break;
                }
                Ok(AdapterReply::NotRunning { block_word, refusal_text }) => {
                    tracing::error!(
                        venue = %self.venue, block_word = %block_word,
                        "the restarted gatewayhub did NOT start this account's adapter ({refusal_text}): \
                         the account no longer trades; its held cancels are answered as advisories and \
                         its resting orders stay at the venue for the operator"
                    );
                    self.session.release_held();
                    return None;
                }
                // A refusal that asking again cannot change: the daemon lacks the verb, or answered it
                // with something it cannot (a desync). Said once; the held cancels are released to be
                // answered, not left waiting on an adapter that will never come.
                Err(e @ (TradeError::Unsupported(_) | TradeError::Protocol(_))) => {
                    tracing::error!(
                        venue = %self.venue,
                        "re-starting the adapter after a gatewayhub restart was refused ({e}): not \
                         asking again; the account's orders fail until the tradehub or the gatewayhub \
                         is fixed"
                    );
                    self.session.release_held_anyway();
                    return None;
                }
                // A link that came back mid-ask, a silent daemon: asked again for as long as the lease
                // lives. But the held risk-off cancels do not wait on it for ever: past
                // `HELD_CANCEL_PATIENCE` they are released, to be answered (as advisories) by a daemon
                // with no adapter, and the failure is said at `error`.
                Err(e) => {
                    if !said_patience && asked_since.elapsed() >= HELD_CANCEL_PATIENCE {
                        said_patience = true;
                        tracing::error!(
                            venue = %self.venue,
                            "the adapter could not be restarted for {HELD_CANCEL_PATIENCE:?} after a \
                             gatewayhub restart ({e}): releasing the held risk-off cancels to be \
                             answered; the adapter is still asked for"
                        );
                        self.session.release_held_anyway();
                    }
                    tracing::warn!(
                        venue = %self.venue, retry_in_ms = pause.as_millis(),
                        "re-starting the adapter after a gatewayhub restart did not land ({e}); asking again"
                    );
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(RETRY_MAX);
                }
            }
        }
        if self.gone() {
            // The adapter ANSWERED and the shutdown sweep's risk-off cancels are held for the teaching this
            // worker will no longer queue: send them (W13c). Left held, `close_for_good` turns them into
            // advisories while the adapter is up and the orders keep resting.
            self.session.release_held_anyway();
            return None;
        }

        let resting = self.resting();
        if !resting.is_empty() {
            tracing::info!(
                venue = %self.venue, orders = resting.len(),
                "re-teaching the restarted gatewayhub's adapter {} resting order(s)", resting.len()
            );
            self.session.exec_client().adopt_restored(&resting);
        }
        // The held risk-off cancels follow the teaching (the boot teaching went out with the adapter's
        // answer, the orders minted since boot just now): a cancel ahead of it fails with "no venue
        // order id" on a venue whose map the restart emptied. FIFO behind the frame above.
        self.session.release_held();

        let unknown = self.session.unknown_orders();
        if !unknown.is_empty() {
            match self.session.resolve_orders(&unknown) {
                Ok(results) => tracing::warn!(
                    venue = %self.venue, asked = unknown.len(), answered = results.len(),
                    "{} submit(s) were Written across the gatewayhub restart; the venue's own event, \
                     the reconcile pass below and the stuck-order watchdog settle them",
                    unknown.len()
                ),
                Err(e) => tracing::error!(
                    venue = %self.venue, coids = ?unknown,
                    "submits Written across the gatewayhub restart stay UNKNOWN and could not be \
                     resolved ({e}); the reconcile pass and the watchdog are the backstop"
                ),
            }
        }

        // 3b. The admitted symbols (decision 0174): the new daemon's reconcile client reads the mounted
        // symbol only until it is told which others the engine admitted, and the node's facts cache
        // would answer the next admission without ever asking. Ask for every one the old daemon was
        // told, BEFORE the pass below, so that pass reads them.
        let mut refused = self.retell();
        if let Some(trigger) = &self.trigger {
            // A closed trigger is a manager that has stopped (teardown): nothing to reconcile for.
            let _ = trigger.send(());
        }
        self.retell_until_told(rx, &mut refused)
    }

    /// Tell the new daemon the symbols the old one was told; the ones it refused, with its reason.
    /// Each refusal is said at `error`: the core HOLDS (never retires) the orders on a symbol the
    /// client was not told, and an operator should know there are some.
    fn retell(&self) -> Vec<(String, String)> {
        let refused = self.session.retell_symbols();
        for (symbol, why) in &refused {
            tracing::error!(
                venue = %self.venue, %symbol,
                "the restarted gatewayhub's reconcile client could not be told `{symbol}` ({why}): \
                 orders resting on it are HELD, not adopted, until it is told; asking again"
            );
        }
        refused
    }

    /// Ask again, on the capped backoff and for at most [`RETELL_PATIENCE`], for the symbols the new
    /// daemon refused; one more reconcile pass is requested when the last of them is told. Whatever is
    /// still refused afterwards stays owed (`TradeSession::retell_symbols` asks again) and the next
    /// order on the symbol asks the daemon itself (`LeaseFacts::admission` does not trust its cache
    /// for an untold symbol).
    ///
    /// Returns a gatewayhub restart that arrived meanwhile (the wait listens on the epoch channel), for
    /// the caller to answer next.
    fn retell_until_told(
        &self,
        rx: &Receiver<(u64, u64)>,
        refused: &mut Vec<(String, String)>,
    ) -> Option<(u64, u64)> {
        let mut pause = RETRY_FIRST;
        let began = Instant::now();
        while !refused.is_empty() && !self.gone() && began.elapsed() < RETELL_PATIENCE {
            match rx.recv_timeout(pause) {
                Ok((old, mut new)) => {
                    while let Ok((_, n)) = rx.try_recv() {
                        new = n;
                    }
                    return Some((old, new));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
            pause = (pause * 2).min(RETRY_MAX);
            *refused = self.session.retell_symbols();
            if refused.is_empty()
                && let Some(trigger) = &self.trigger
            {
                let _ = trigger.send(());
            }
        }
        if !refused.is_empty() {
            tracing::error!(
                venue = %self.venue, symbols = ?refused.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(),
                "gave up telling the restarted gatewayhub's reconcile client these symbols after \
                 {RETELL_PATIENCE:?}; their orders stay HELD until an order on the symbol (or the next \
                 gatewayhub restart) tells it"
            );
        }
        None
    }
}

/// The live orders of `venue`'s `label` account in `snapshot`, as `(coid, unified symbol)` pairs: the
/// shape `ExecutionClient::adopt_restored` takes. `None` (no core yet) is none.
pub fn resting_orders(
    snapshot: Option<&CoreSnapshot>,
    venue: &str,
    label: Option<&str>,
) -> Vec<(String, String)> {
    let Some(snapshot) = snapshot else { return Vec::new() };
    snapshot
        .orders
        .iter()
        .filter(|o| {
            o.venue == venue
                && o.account
                    .as_ref()
                    .and_then(vike_model::accounts::account_keys::AccountLabel::text)
                    == label
                && o.status.is_live()
        })
        .map(|o| (o.client_order_id.clone(), o.symbol.clone()))
        .collect()
}
