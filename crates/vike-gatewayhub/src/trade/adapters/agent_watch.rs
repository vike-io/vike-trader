//! **`AgentWatch` — tells the operator that a Hyperliquid API wallet's approval is ending or gone.**
//!
//! An agent wallet is approved by its master for at most 180 days. When the approval lapses the venue
//! rejects every order with `User or API Wallet 0x... does not exist` and nothing else changes, so the
//! only symptom used to be those rejections (the testnet approval lapsed 2026-10-10; the first rejection
//! was seen 2026-10-18). The reads are the bridge's (`vike_hyperliquid::agent_expiry`, two PUBLIC `/info`
//! requests); this module is the SCHEDULE and the VOICE:
//!
//! * a successful `hyperliquid` start registers the account ([`AgentWatch::watch`], from
//!   `BridgeTrade::start`), and one thread checks it at once and then every [`CHECK_EVERY`];
//! * a status that needs the operator (`ExpiresSoon` at 14 and 3 days, `Expired`, `Missing`) is a loud
//!   `warn!` and ONE operator alert through the workspace alerting path ([`store_notifier`]: the Telegram
//!   and webhook targets the credential store configures), at most once per [`REALERT_AFTER_MS`] per
//!   account, and at once again when the severity RISES;
//! * a venue that did not answer is `info!`/`debug!` ("could not check"), never an alarm.
//!
//! # ⚠ A warning, and nothing else
//!
//! It runs off every path: a separate thread, started after the adapter is up, so it never delays a boot
//! or a start; it hands nothing to the adapter, the session or the core, so it changes no trading
//! decision; and it halts nothing. A panic in a check is caught and read as "could not check".
//!
//! # No credential here
//!
//! The registered [`AgentSubject`] holds addresses only (the key is read once, for its address, and not
//! kept), the alert and the log name the master and the signer ADDRESS and nothing else, and the
//! notifier reads only the alerting scope of the store (`vike_alerting::WEBHOOK_KEYS`).
//!
//! # What it does not do
//!
//! The state (when each account was last alerted) is in memory: a daemon restart may repeat an alert once.
//! The surface is the log and the pager; there is no field for it in the engine block or the `trade status`
//! line (those are the tradehub's snapshot, which this daemon does not write).

use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_alerting::{AlertTargets, FiredAlert, UreqTransport, WebhookSink};
use vike_hyperliquid::agent_expiry::{AgentStatus, AgentSubject, DAY_MS, cure};
use vike_secrets::{CredentialStore, KeyScope};

/// How often a registered account is checked again.
pub const CHECK_EVERY: Duration = Duration::from_secs(3600);
/// A status that needs the operator is told at most this often per account, unless it gets WORSE.
pub const REALERT_AFTER_MS: i64 = DAY_MS;

/// Where a fired alert goes (production: [`store_notifier`]).
pub type Notify = Arc<dyn Fn(&FiredAlert) + Send + Sync>;
/// The venue read (production: `vike_hyperliquid::agent_expiry::check`); `now_ms` is the clock.
pub type Checker = Arc<dyn Fn(&AgentSubject, i64) -> AgentStatus + Send + Sync>;
/// The clock, epoch ms.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
/// Does the account still run at the tier it was registered at? Asked before every check, so a deactivated
/// account is dropped from the watch instead of alerting for an account the operator turned off.
pub type StillRuns = Box<dyn Fn() -> bool + Send + Sync>;

/// One account to watch.
pub struct Watched {
    /// `venue/account/tier`: a second registration of the same key replaces the first.
    pub key: String,
    /// The account as the operator names it (`default` for the default account).
    pub account: String,
    /// What to ask about: addresses only.
    pub subject: AgentSubject,
    /// See [`StillRuns`].
    pub still_runs: StillRuns,
}

/// What the watcher remembers of one account, to say a thing once and not every hour.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Seen {
    /// The severity class last told (`AgentStatus::rank`).
    rank: u8,
    /// When the operator was last told, epoch ms.
    told_ms: Option<i64>,
    /// Has any check answered yet (the first answer is logged at `info`)?
    answered: bool,
}

/// **Is it time to speak about `status`?** PURE: updates `seen` and answers `true` when the status needs the
/// operator and either has not been told yet, got worse, or was last told [`REALERT_AFTER_MS`] ago.
/// A status of rank 0 resets the memory (so a lapse after a re-approval is told at once); a check that did
/// not answer changes nothing.
fn due(seen: &mut Seen, status: &AgentStatus, now_ms: i64) -> bool {
    if matches!(status, AgentStatus::CouldNotCheck(_)) {
        return false;
    }
    seen.answered = true;
    let rank = status.rank();
    if rank == 0 {
        seen.rank = 0;
        seen.told_ms = None;
        return false;
    }
    let speak = rank > seen.rank || seen.told_ms.is_none_or(|t| now_ms - t >= REALERT_AFTER_MS);
    seen.rank = rank;
    if speak {
        seen.told_ms = Some(now_ms);
    }
    speak
}

/// The alert for `status` on `w`, as the pager shows it: the title and the body, addresses only.
fn alert_of(w: &Watched, status: &AgentStatus, now_ms: i64) -> FiredAlert {
    let network = format!("{:?}", w.subject.network).to_ascii_lowercase();
    let what = if status.cannot_sign() { "CANNOT SIGN" } else { "EXPIRES SOON" };
    let master = w.subject.master.as_deref().unwrap_or("(not configured)");
    FiredAlert {
        rule_id: "hl-agent-approval".to_string(),
        title: format!("Hyperliquid API wallet {what}: account {} ({network})", w.account),
        body: format!(
            "{}\naccount: {} ({network})\nmaster:  {master}\nsigner:  {}\nfix:     {}\n\
             This is a warning only: nothing was halted or changed.",
            status.headline(now_ms),
            w.account,
            w.subject.signer,
            cure(w.subject.network)
        ),
        ts_ms: now_ms,
        targets: AlertTargets { in_process: false, webhooks: Vec::new() },
    }
}

/// Say what a check found: the log line every time something needs the operator (the first time in a
/// window loud, the repeats quiet), the alert when [`due`], `info!`/`debug!` otherwise.
fn report(
    seen: &mut Seen,
    w: &Watched,
    status: &AgentStatus,
    now_ms: i64,
    notify: &(dyn Fn(&FiredAlert) + Send + Sync),
) {
    let first_answer = !seen.answered;
    let speak = due(seen, status, now_ms);
    let (account, signer) = (w.account.as_str(), w.subject.signer.as_str());
    let master = w.subject.master.as_deref().unwrap_or("-");
    if speak {
        let alert = alert_of(w, status, now_ms);
        tracing::warn!(
            account,
            master,
            signer,
            "TRADE-AGENT: hyperliquid account {account}: {}. To fix: {}. (A warning only: nothing \
             was halted or changed.)",
            status.headline(now_ms),
            cure(w.subject.network)
        );
        notify(&alert);
    } else if status.needs_attention() {
        tracing::debug!(
            account,
            "TRADE-AGENT: still needs the operator: {}",
            status.headline(now_ms)
        );
    } else if matches!(status, AgentStatus::CouldNotCheck(_)) || first_answer {
        tracing::info!(
            account,
            signer,
            "TRADE-AGENT: hyperliquid account {account}: {}",
            status.headline(now_ms)
        );
    } else {
        tracing::debug!(account, "TRADE-AGENT: {}", status.headline(now_ms));
    }
}

/// How the watcher reads, tells and times (swapped by the tests).
struct Parts {
    checker: Checker,
    notify: Notify,
    clock: Clock,
    every: Duration,
}

struct Entry {
    watched: Watched,
    seen: Seen,
}

/// Check one entry now. `false` = the account no longer runs and the entry is to be dropped.
fn check_one(parts: &Parts, entry: &mut Entry) -> bool {
    if !(entry.watched.still_runs)() {
        return false;
    }
    let now_ms = (parts.clock)();
    let status = std::panic::catch_unwind(AssertUnwindSafe(|| {
        (parts.checker)(&entry.watched.subject, now_ms)
    }))
    .unwrap_or_else(|_| AgentStatus::CouldNotCheck("the check panicked".to_string()));
    report(&mut entry.seen, &entry.watched, &status, now_ms, parts.notify.as_ref());
    true
}

/// The watcher thread: a new registration is checked at once, everything again every `parts.every`.
fn run(rx: &Receiver<Watched>, parts: &Parts) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut next_pass = Instant::now() + parts.every;
    loop {
        match rx.recv_timeout(next_pass.saturating_duration_since(Instant::now())) {
            Ok(watched) => {
                // The same account started again: the newer registration replaces the older, and its
                // memory of what was told is kept (a restart is not a new incident).
                let seen = entries
                    .iter()
                    .position(|e| e.watched.key == watched.key)
                    .map(|i| entries.remove(i).seen)
                    .unwrap_or_default();
                let mut entry = Entry { watched, seen };
                if check_one(parts, &mut entry) {
                    entries.push(entry);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                entries.retain_mut(|entry| check_one(parts, entry));
                next_pass = Instant::now() + parts.every;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// See the module doc. Dropping it ends the thread.
pub struct AgentWatch {
    tx: Mutex<Sender<Watched>>,
}

impl AgentWatch {
    /// The production watch: the real venue read, the wall clock and [`CHECK_EVERY`], telling through `notify`.
    #[must_use]
    pub fn new(notify: Notify) -> Self {
        Self::with_parts(
            Arc::new(|subject: &AgentSubject, now_ms: i64| {
                vike_hyperliquid::agent_expiry::check(subject, now_ms)
            }),
            notify,
            Arc::new(vike_model::now_ms),
            CHECK_EVERY,
        )
    }

    /// A watch over the given read, pager, clock and period (the tests').
    #[must_use]
    pub fn with_parts(checker: Checker, notify: Notify, clock: Clock, every: Duration) -> Self {
        let (tx, rx) = channel();
        let parts = Parts { checker, notify, clock, every };
        if let Err(e) =
            std::thread::Builder::new().name("gwh-hl-agent".into()).spawn(move || run(&rx, &parts))
        {
            tracing::warn!(error = %e, "TRADE-AGENT: the approval watch could not start; no approval is checked");
        }
        AgentWatch { tx: Mutex::new(tx) }
    }

    /// Watch `watched`: checked now (on the watcher's thread, never the caller's) and then every period.
    pub fn watch(&self, watched: Watched) {
        let _ = self.tx.lock().unwrap_or_else(PoisonError::into_inner).send(watched);
    }
}

/// **The production pager**: every webhook target the credential store configures (Telegram, the generic
/// URL), read at the moment of the alert from the alerting scope of the store alone. No target configured
/// is a log line (the `warn!` above is the record); a failed delivery logs the target name and a coarse
/// category, never a URL or token (`WebhookSink::try_deliver`).
#[must_use]
pub fn store_notifier(store: CredentialStore) -> Notify {
    Arc::new(move |alert: &FiredAlert| {
        let vars = match store.read(&KeyScope::of(vike_alerting::WEBHOOK_KEYS)) {
            Ok(read) => read.into_map(),
            Err(e) => {
                tracing::warn!(error = %e, "TRADE-AGENT: the credential store could not be read for the alert targets; the alert is in this log only");
                return;
            }
        };
        let targets = vike_alerting::webhook_configs_from_env(&vars);
        if targets.is_empty() {
            tracing::info!(
                "TRADE-AGENT: no alert target is configured; the alert is in this log only"
            );
        }
        for target in targets {
            let name = target.name.clone();
            match WebhookSink::new(target, UreqTransport::new()).try_deliver(alert) {
                Ok(()) => {
                    tracing::info!(target_name = %name, "TRADE-AGENT: the alert was delivered")
                }
                Err(category) => tracing::warn!(
                    target_name = %name, error = %category,
                    "TRADE-AGENT: the alert could not be delivered"
                ),
            }
        }
    })
}

#[path = "agent_watch_tests.rs"]
#[cfg(test)]
mod agent_watch_tests;
