//! `balance_gate` — **the NODE holds the gate on a venue's reconcile balance**, because only the node knows what
//! the engine it feeds was bound with (third review of `fix/usd-default-currency`, HIGH-A and HIGH-B).
//!
//! The reconcile balance of oanda, ig and fxcm is the account's cash in ITS currency, with no currency beside it
//! (the account summary, the account row and the Accounts table carry none), and the core adopts it into a ledger
//! LABELLED with whatever currency the engine was bound with at its start. Three bindings make that wrong, and in
//! each the balance is not handed to the engine:
//!
//! * the engine's currency is only ASSUMED (`account_ccy_assumed`: the venue said nothing, or the read failed):
//!   the real currency's cash would be labelled USD;
//! * the venue STATED a currency and the mount fence DECLINED to anchor the ledger to it (EUR on the USD-quoted
//!   `EURUSD` mount): the engine books its mounted symbol in another money than the cash is in;
//! * the venue stated a currency other than USD and this account does not convert (no exposure ceiling), so the
//!   engine anchors to nothing and keeps its dollar label.
//!
//! The gatewayhub keeps a gate of its own (`TradeLane::balance_withheld`), but it follows the last START the
//! gatewayhub SAW, not what the node's engine was bound with: the node's re-teach after a gatewayhub restart
//! re-sends the `AdapterStart`, the gatewayhub reads the currency again and may now get a statement, and the node
//! throws that reply away because its engine is not rebuilt. So the gateway's gate is a belt, and this decorator,
//! built in `RemoteBackend::start_adapter` from the values that bind the engine, is the authority: nothing a
//! gatewayhub later reads can open it.
//!
//! A second, dynamic hold covers ibkr's boot window: ibkr's balance is the cash of the MOUNTED contract's
//! currency, which its daemon-side client withholds itself once told the engine holds a stock in another currency;
//! a tradehub that boots with such a stock in its journal has not yet told the new client (the boot re-admission
//! runs after the start), so the balance is not asked until every journaled foreign symbol was told.
//!
//! # A hold is VISIBLE, and said again (batch 4; reviews 3 MEDIUM-A and the batch-3 integration M-4)
//!
//! A held balance leaves the engine on its own book (seed + PnL) with nothing in the equity to say so, so the gate
//! reports what it holds, in short words, to the lease registry (`GateSpec::shown` ->
//! `LeaseFacts::set_balance_held`), from which the publisher stamps every frame's `balance_held` and the
//! operator surfaces (`trade equity`, `trade status`, `trade doctor`, the Telegram `/equity`) print
//! `balance held: <reason>`. The first sight of a hold is a `warn` (at once); one that goes on is said again at
//! `warn` every [`REMIND_EVERY`] per account, from the reconcile passes that ask for the balance (so a quiet
//! node is not nagged between passes), naming the account, the venue, the reason and the operator step
//! (`account.active` off and then on). The cadence is this object's own state on the monotonic clock, no global.
//!
//! ⚠ **The one automatic lift is the venue's own confirmation of the very currency the engine assumed.** The hold
//! of an ASSUMED binding lasts the engine's life, except when the venue later NAMES the assumed currency: the
//! rate stamps (`WireSymbolFacts::account_ccy`) alone prove nothing, because the lister fills in the same assumed
//! USD a failed read gave the start, unflagged, so an assumed USD stamps `USD` exactly as a stated one does. The
//! proof is the flag the gatewayhub adds beside the stamp (`WireSymbolFacts::account_ccy_stated`: the venue
//! named that currency to some reader, `assumed_ccy::AssumedSaid::has_stated`, or fxcm's session did not assume
//! it); `LeaseFacts` raises [`GateSpec::lift`] when a facts answer states the engine's anchor as the venue's word,
//! and this gate hands the venue's cash on from the next pass (the assumption was right: the cash is in the
//! currency the ledger is labelled with). A venue that names ANOTHER currency lifts nothing (the ledger is
//! mislabelled: the symbol is refused in words and the account must be started again); every other binding has
//! no lift, and the hold stays what a start that reads the currency again ends, the operator step above.
//!
//! Every other verb is forwarded as it is, and venues outside [`crate::server::symbol_admission`]'s
//! `balance_carries_no_currency` (ctrader fences itself, dukascopy has no balance leg, ibkr's client withholds
//! itself) pass through untouched.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use crate::server::symbol_admission::balance_carries_no_currency;

/// How often a hold that goes on is said again at `warn` (the first sight of a hold is said at once).
pub(super) const REMIND_EVERY: Duration = Duration::from_secs(10 * 60);

/// The short words of ibkr's boot-window hold, shown beside the account like a binding's.
pub(super) const UNTOLD_REASON: &str = "untold foreign stock";

/// How the engine of an account of a no-currency-balance venue was bound, as far as its balance is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Binding<'a> {
    /// The ledger is anchored to the currency the venue stated (or the account does not convert and the venue
    /// stated nothing that could disagree): the balance is in the ledger's money.
    Anchored,
    /// The ledger's currency is only the ruling's USD ASSUMED.
    Assumed,
    /// A currency (`ccy`: the venue's word when `stated`, else an assumed one) the engine's mount fence declined to
    /// anchor the ledger to (`why` is the fence's own sentence).
    Declined { ccy: &'a str, stated: bool, why: &'a str },
    /// The venue stated `ccy`, other than USD, and this account does not convert: the engine anchors to nothing.
    Unanchored { ccy: &'a str },
}

/// **Why `venue`'s reconcile balance must not be handed to an engine bound like this**, or `None`.
pub(super) fn held_reason(venue: &str, binding: &Binding<'_>) -> Option<String> {
    if !balance_carries_no_currency(venue) {
        return None;
    }
    match binding {
        Binding::Anchored => None,
        Binding::Assumed => Some(format!(
            "{venue}: the engine's currency is only ASSUMED (USD) and the venue's cash balance carries no \
             currency, so it is not adopted into this ledger (the account's real currency would be labelled \
             USD); set the account's `account.active` off and on to start it again and read the currency"
        )),
        Binding::Declined { why, .. } => Some(format!(
            "{venue}: {why}; the venue's cash balance is in the account's own currency, which is not this \
             ledger's, so it is not adopted into it"
        )),
        Binding::Unanchored { ccy } => Some(format!(
            "{venue}: the venue states the account's currency is {ccy} and this account does not convert, so \
             its engine is anchored to nothing and keeps a dollar ledger; the venue's cash balance, in {ccy}, \
             is not adopted into it"
        )),
    }
}

/// What a gate holds back and why, said twice: the sentence for the log and the short words for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hold {
    /// The sentence the log carries ([`held_reason`]).
    pub(super) why: String,
    /// The short words the operator surfaces show beside the account: `assumed USD`, `stated EUR not anchored`.
    pub(super) reason: String,
}

/// The [`Hold`] of an engine bound like `binding`, or `None` when its balance is handed on.
pub(super) fn hold(venue: &str, binding: &Binding<'_>) -> Option<Hold> {
    let why = held_reason(venue, binding)?;
    let reason = match binding {
        Binding::Anchored => return None,
        Binding::Assumed => "assumed USD".to_string(),
        Binding::Declined { ccy, stated, .. } => {
            format!("{} {ccy} not anchored", if *stated { "stated" } else { "assumed" })
        }
        Binding::Unanchored { ccy } => format!("stated {ccy} not anchored"),
    };
    Some(Hold { why, reason })
}

/// **The line a hold that goes on is said again with** (`warn`, every [`REMIND_EVERY`] per account).
pub(super) fn held_reminder(venue: &str, account: &str, reason: &str) -> String {
    format!(
        "{venue} account {account}: balance held: {reason}. The venue's cash balance is still NOT adopted, so \
         this engine's equity is its own book (seed + PnL), not the venue's wallet. To read the account's \
         currency again and end the hold, set the account's `account.active` off and then on (batch every \
         affected account into one restart of it; an untold foreign stock normally ends by itself once the \
         boot re-admission tells the daemon). Said again every {} minutes while it lasts",
        REMIND_EVERY.as_secs() / 60
    )
}

/// The symbols still owed to the daemon's reconcile client, asked on every balance fetch (empty = none).
pub(super) type Untold = Box<dyn Fn() -> Vec<String> + Send>;

/// Where a gate says what it holds, for the operator surfaces: called with the short reason while the balance is
/// held back and with `None` once it is not. Production writes `LeaseFacts::set_balance_held`.
pub(super) type Shown = Box<dyn Fn(Option<&str>) + Send>;

/// Everything [`gate`] builds a [`BalanceGate`] from.
pub(super) struct GateSpec {
    /// The roster venue id.
    pub(super) venue: String,
    /// The account as the operator spells it (`DEFAULT` for the unlabelled one), for the log lines.
    pub(super) account: String,
    /// What the engine's binding holds back, fixed when the engine was bound; `None` = the balance is handed on.
    pub(super) hold: Option<Hold>,
    /// ibkr: the journaled foreign symbols the daemon's client has not been told yet.
    pub(super) untold: Option<Untold>,
    /// The flag `LeaseFacts` raises when the venue NAMES the currency the engine only assumed
    /// (`LeaseFacts::note_assumed_hold`): set for an `Assumed` binding's hold and for no other. Once raised the
    /// hold is over: the venue's cash is in the ledger's currency after all.
    pub(super) lift: Option<Arc<AtomicBool>>,
    /// Where the gate says what it holds.
    pub(super) shown: Shown,
}

/// The bound reconcile client with the balance gate in front of it: see the module doc.
pub(super) struct BalanceGate {
    inner: Box<dyn ReconClient>,
    venue: String,
    account: String,
    hold: Option<Hold>,
    untold: Option<Untold>,
    lift: Option<Arc<AtomicBool>>,
    shown: Shown,
    /// The cadence of the reminders and of the dynamic hold's display: this gate's own state.
    said: Mutex<Said>,
}

/// What a gate has already said.
#[derive(Default)]
struct Said {
    /// When the hold was last said at `warn`; `None` = not yet (or no hold stands).
    at: Option<Instant>,
    /// Whether the dynamic (ibkr) hold is shown right now.
    untold_shown: bool,
    /// Whether the binding's hold was lifted by the venue's confirmation (shown withdrawn, said once).
    lifted: bool,
}

/// `inner` with the balance gate in front of it, or `inner` itself when nothing is held and nothing is owed.
pub(super) fn gate(inner: Box<dyn ReconClient>, spec: GateSpec) -> Box<dyn ReconClient> {
    if spec.hold.is_none() && spec.untold.is_none() {
        return inner;
    }
    Box::new(BalanceGate::new(inner, spec, Instant::now()))
}

impl BalanceGate {
    /// The gate over `inner`, built at `now`: a hold fixed by the binding is said at `warn` and shown at once.
    fn new(inner: Box<dyn ReconClient>, spec: GateSpec, now: Instant) -> Self {
        let GateSpec { venue, account, hold, untold, lift, shown } = spec;
        let mut said = Said::default();
        if let Some(hold) = &hold {
            tracing::warn!(
                venue = %venue, account = %account,
                "RECONCILE BALANCE HELD BACK for this engine's life: {}", hold.why
            );
            shown(Some(hold.reason.as_str()));
            said.at = Some(now);
        }
        BalanceGate { inner, venue, account, hold, untold, lift, shown, said: Mutex::new(said) }
    }

    /// Whether the binding's hold has been lifted by the venue's confirmation of the assumed currency
    /// ([`GateSpec::lift`]). The first time it is seen raised the hold is withdrawn from the operator surfaces and
    /// said once at `info`; a gate with no lift never lifts.
    fn hold_lifted(&self) -> bool {
        let Some(lift) = &self.lift else { return false };
        if !lift.load(Ordering::SeqCst) {
            return false;
        }
        let first = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            !std::mem::replace(&mut said.lifted, true)
        };
        if first {
            (self.shown)(None);
            tracing::info!(
                venue = %self.venue, account = %self.account,
                "balance hold ended: the venue named the account's currency and it is the one this engine's \
                 ledger only assumed, so the venue's cash balance is in the ledger's currency and is adopted \
                 from the next pass"
            );
        }
        true
    }

    /// [`ReconClient::fetch_balance`] as of `now` (monotonic): the clock is a parameter so a test steps it.
    fn fetch_balance_at(&self, now: Instant) -> Result<Option<f64>, String> {
        if let Some(hold) = &self.hold
            && !self.hold_lifted()
        {
            tracing::debug!(venue = %self.venue, "balance not reported: {}", hold.why);
            self.remind(now, &hold.reason);
            return Ok(None);
        }
        if let Some(untold) = &self.untold {
            let owed = untold();
            if owed.is_empty() {
                self.end_untold_hold();
            } else {
                tracing::debug!(
                    venue = %self.venue, symbols = ?owed,
                    "balance not reported: the daemon's reconcile client was not told these foreign symbols yet"
                );
                self.show_untold_hold();
                self.remind(now, UNTOLD_REASON);
                return Ok(None);
            }
        }
        self.inner.fetch_balance()
    }

    /// Say the hold again at `warn` when it was never said, or last [`REMIND_EVERY`] ago or more.
    fn remind(&self, now: Instant, reason: &str) {
        let due = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            let due = said.at.is_none_or(|at| now.saturating_duration_since(at) >= REMIND_EVERY);
            if due {
                said.at = Some(now);
            }
            due
        };
        if due {
            tracing::warn!(
                venue = %self.venue, account = %self.account,
                "{}", held_reminder(&self.venue, &self.account, reason)
            );
        }
    }

    /// The dynamic hold began (or goes on): shown once.
    fn show_untold_hold(&self) {
        let first = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            !std::mem::replace(&mut said.untold_shown, true)
        };
        if first {
            (self.shown)(Some(UNTOLD_REASON));
        }
    }

    /// Every owed symbol was told: a dynamic hold that was shown is withdrawn, and the next one is said at once.
    fn end_untold_hold(&self) {
        let was_shown = {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            let was_shown = std::mem::replace(&mut said.untold_shown, false);
            if was_shown {
                said.at = None;
            }
            was_shown
        };
        if was_shown {
            (self.shown)(None);
            tracing::info!(
                venue = %self.venue, account = %self.account,
                "balance hold ended: the daemon's reconcile client was told every journaled foreign symbol"
            );
        }
    }
}

impl ReconClient for BalanceGate {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.inner.fetch_order_status_reports(since)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.inner.fetch_fill_reports(since)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.inner.fetch_position_status_reports()
    }

    /// `Ok(None)` ("not reported": no balance folded, no drift raised, the venue not asked) while the binding
    /// holds it or the daemon's client is owed a journaled foreign symbol; else the inner client's answer. A hold
    /// that persists is said again every [`REMIND_EVERY`] (see the module doc).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.fetch_balance_at(Instant::now())
    }

    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.inner.fetch_fee_rates()
    }

    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.inner.fetch_account_identity()
    }

    fn admit_symbol(&self, symbol: &str) {
        self.inner.admit_symbol(symbol);
    }

    fn forget_symbol(&self, symbol: &str) {
        self.inner.forget_symbol(symbol);
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        self.inner.admitted_symbols()
    }

    fn lists_trigger_orders(&self) -> bool {
        self.inner.lists_trigger_orders()
    }
    // `fetch_mass_status` is deliberately NOT overridden: the trait's default composes the three forwarded
    // fetches above in the order the driver runs them, exactly as it does over the inner client.
}

#[path = "balance_gate_tests.rs"]
#[cfg(test)]
mod balance_gate_tests;
