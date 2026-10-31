//! A ForexConnect session that failed to open is tried AGAIN, for the life of the mount.
//!
//! A login fails for reasons that heal on their own: FXCM's host discovery (`Hosts.jsp`, `ORA-499`
//! with an `errorCode`) is unreachable for a weekend close or a maintenance window, and measured
//! on the CI box on Fri 2026-10-23 and Sat 2026-10-17 that was enough to kill the account until a manual
//! restart. Two sessions are logged in per mount, and each retries here:
//!
//! * the EXEC session retries inside its own thread (`crate::exec`'s `open_session`), refusing every
//!   command meanwhile; it needs only [`pause_after`] from this module;
//! * the RECONCILE session is a client object the venue's start has already handed away, so the
//!   retry lives in [`Redial`], a [`ReconClient`] that answers every read with an error until its
//!   thread opens the real client, then forwards to it. The thread calls the very same constructor
//!   a one-shot start calls, so registering the session with `crate::reads` and reading the
//!   account's currency are NOT re-implemented here: they happen on the retry that succeeds.
//!
//! ⚠ **Only a session that never opened is retried.** One that opened and later died is the
//! reconcile client's and the exec thread's existing business, unchanged.
//!
//! ⚠ **A REFUSED CREDENTIAL IS NEVER RETRIED** ([`refuses_credentials`]): a host that is down heals, a
//! password that was rotated or mistyped does not, and every retry is one more failed login that FXCM
//! may count towards a lockout (its threshold is unknown; the ig crate documents one on its venue). After
//! one such failure neither session logs in again: the mount keeps refusing every command with the
//! `crate::SESSION_UNAVAILABLE` marker, as it did before any retry existed, and a key change followed
//! by the account's `account.active` off and on (or a restart) is what ends it.
//!
//! ⚠ **Two retries never log in at the same moment** ([`one_retry_at_a_time`]). Before the retries a
//! start logged its sessions in one after the other (the reconcile connect blocks until its login
//! resolves, then the exec thread starts), so two `fc_login_ex` calls overlapped only in a narrow window
//! (a replaced adapter's login still running past the gatewayhub's 10 s join). Both sessions fail
//! within a second of each other and keep identical schedules, so left alone they would overlap at
//! nearly every tick. Whether the ForexConnect SDK is safe under concurrent logins cannot be read
//! from this tree (proprietary), so the retries take turns instead of finding out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

/// The SDK's own words for a login the venue REFUSED over its credentials, as measured against the real
/// host on 2026-09-22 (`crate::sys::LoginFailureClass`'s doc has the failures told apart): a live account
/// with a wrong password. Everything else heals and is retried: `ORA-499` host discovery, a silent wait, a
/// disconnect, and ⚠ **`User or connection doesn't exist.`**, which is NOT a credential verdict: measured
/// 2026-10-24 09:11 UTC, a valid account got it from a login made right after its previous session was torn
/// down (FXCM was still closing it), and the same account logged in fine at 04:00 and 07:33 that day. A
/// first version of this list held it and left a healthy account dead until an `account.active` toggle.
const CREDENTIAL_REFUSALS: [&str; 1] = ["Incorrect user name or password"];

/// Is `why` (a login failure's text) the one measured credential refusal? A deliberately narrow
/// substring match, not a reading of the venue's message set: a failure worded any other way is retried
/// on the schedule, which costs failed logins but never an account that cannot recover.
pub(crate) fn refuses_credentials(why: &str) -> bool {
    let why = why.to_ascii_lowercase();
    CREDENTIAL_REFUSALS.iter().any(|words| why.contains(&words.to_ascii_lowercase()))
}

/// What the `error!` line of a refused credential says to do, shared by both sessions.
pub(crate) const CREDENTIALS_REFUSED: &str = "the venue REFUSED the login credentials, so this session \
     is NOT retried (repeated failed logins can lock the account): change the key, then set the \
     account's `account.active` off and on, or restart";

static RETRY_LOGIN: Mutex<()> = Mutex::new(());

/// The turn a RETRY of a login takes (the first login never does: it behaves as it always did). Held
/// across the `fc_login_ex` call, so it is bounded by the shim's own login wait.
pub(crate) fn one_retry_at_a_time() -> MutexGuard<'static, ()> {
    RETRY_LOGIN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What one attempt of a retry came to.
pub(crate) enum Attempt<C> {
    /// The session opened.
    Opened(C),
    /// It did not; the failure may heal: wait the next pause.
    TryAgain,
    /// It did not and never will without a new key ([`refuses_credentials`]): stop.
    GiveUp,
}

/// The pause after the Nth failed login in a row; the last entry repeats for ever. Short enough that
/// a host back after a blip is found within a minute or two, long enough that a closed weekend costs
/// one login every ten minutes (each attempt holds a thread for the shim's whole login wait).
const PAUSES: [Duration; 5] = [
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
    Duration::from_secs(600),
];

/// How long to wait before login number `failures + 1`, after `failures` (>= 1) failed in a row.
pub(crate) fn pause_after(failures: u32) -> Duration {
    let index = usize::try_from(failures.saturating_sub(1)).unwrap_or(usize::MAX);
    PAUSES[index.min(PAUSES.len() - 1)]
}

/// What every read answers while the session is not open. The login's own words are in the log
/// (`fxcm reconcile: the ForexConnect session did not open`), not repeated here.
const NOT_OPEN: &str = "fxcm reconcile: the ForexConnect session is not open (its login failed and is \
                        retried with a growing pause; the reason is in the log line `the ForexConnect \
                        session did not open`)";

/// ...and once a retry met a REFUSED credential and stopped.
const NOT_OPEN_REFUSED: &str = "fxcm reconcile: the ForexConnect session is not open (the venue refused \
                                the login credentials and the login is not retried: change the key, then \
                                set the account's `account.active` off and on, or restart)";

struct Shared<C> {
    /// The mounted symbol: the client always reads it, so it is never an ADMITTED one.
    symbol: String,
    /// A retry met a refused credential and the thread ended.
    refused: AtomicBool,
    /// The real client, once a retry opened it.
    client: Mutex<Option<Arc<C>>>,
    /// Every pair the engine told this client, kept so the real client is told them when it opens.
    admitted: AdmittedSymbols,
}

impl<C: ReconClient + Sync> Shared<C> {
    fn connected(&self) -> Option<Arc<C>> {
        self.client.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Hand the opened client the pairs told so far, THEN publish it, both under the lock a reader
    /// takes: a fetch never meets a client that lacks a pair this one already claimed, and an
    /// `admit_symbol` racing this waits for the lock and then reaches the client itself.
    fn install(&self, client: C) {
        let mut held = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        for symbol in self.admitted.symbols() {
            client.admit_symbol(&symbol);
        }
        *held = Some(Arc::new(client));
    }
}

/// A reconcile client whose session may not be open yet: see the module doc.
pub(crate) struct Redial<C: ReconClient + Sync + 'static> {
    shared: Arc<Shared<C>>,
    /// Dropped FIRST in `Drop`: the thread sleeps its pause on the other end of this channel, so
    /// closing it wakes the thread at once instead of after up to ten minutes.
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl<C: ReconClient + Sync + 'static> Redial<C> {
    /// Start retrying. The caller's own first attempt has already failed, so the first pause is
    /// [`pause_after`]`(1)`. `open` makes one attempt (and says why it failed, at `warn`, or for a
    /// refused credential at `error`, itself); `pause` is [`pause_after`] in production and a short one
    /// in a test. Each attempt takes its turn behind the other session's retry
    /// ([`one_retry_at_a_time`]).
    pub(crate) fn start(
        symbol: &str,
        mut open: impl FnMut() -> Attempt<C> + Send + 'static,
        pause: impl Fn(u32) -> Duration + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            symbol: symbol.to_string(),
            refused: AtomicBool::new(false),
            client: Mutex::new(None),
            admitted: AdmittedSymbols::default(),
        });
        let (stop, stopped) = mpsc::channel::<()>();
        let theirs = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("fxcm-redial".into())
            .spawn(move || {
                let mut failed = 1u32;
                // Only a whole pause with nobody calling is a tick: a message or a closed channel is "stop".
                while matches!(stopped.recv_timeout(pause(failed)), Err(RecvTimeoutError::Timeout)) {
                    let attempt = {
                        let _turn = one_retry_at_a_time();
                        open()
                    };
                    match attempt {
                        Attempt::Opened(client) => {
                            theirs.install(client);
                            tracing::info!(
                                target: "vike_fxcm",
                                venue = "fxcm",
                                failed_attempts = failed,
                                "fxcm reconcile: the ForexConnect session opened after {failed} failed \
                                 attempt(s); this venue-symbol reconciles as after a normal start"
                            );
                            return;
                        }
                        Attempt::TryAgain => failed += 1,
                        Attempt::GiveUp => {
                            theirs.refused.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }
            })
            .map_err(|e| tracing::error!(venue = "fxcm", error = %e, "fxcm reconcile: the retry thread could not be spawned, so the session is NOT retried"))
            .ok();
        Self { shared, stop: Some(stop), thread }
    }

    fn open(&self) -> Result<Arc<C>, String> {
        self.shared.connected().ok_or_else(|| {
            let words = if self.shared.refused.load(Ordering::SeqCst) {
                NOT_OPEN_REFUSED
            } else {
                NOT_OPEN
            };
            words.to_string()
        })
    }
}

impl<C: ReconClient + Sync + 'static> Drop for Redial<C> {
    fn drop(&mut self) {
        drop(self.stop.take());
        // A thread in the middle of a login finishes it first (the shim bounds it); the real client,
        // if one opened, is then dropped with the last `Arc` and joins its own session thread.
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!(venue = "fxcm", "the fxcm reconcile retry thread panicked");
        }
    }
}

/// ⚠ EVERY method `crate::recon_client::FxcmReconClient` overrides is forwarded here, and no other:
/// a method this wrapper forgot would silently take the trait's default (see
/// `crate::exec::FxcmExecutionClient::confirm` for the same trap).
impl<C: ReconClient + Sync + 'static> ReconClient for Redial<C> {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.open()?.fetch_order_status_reports(since)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.open()?.fetch_fill_reports(since)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.open()?.fetch_position_status_reports()
    }

    /// An error, never `Ok(None)`: `None` says "this venue reports no balance", and a pass that is
    /// told so stops asking and warning.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.open()?.fetch_balance()
    }

    fn admit_symbol(&self, symbol: &str) {
        self.shared.admitted.admit(symbol, &self.shared.symbol);
        if let Some(client) = self.shared.connected() {
            client.admit_symbol(symbol);
        }
    }

    /// The engine gave a pair back (decision 0205): forgotten here AND on the real client, the way `admit_symbol`
    /// tells both, so the claim `admitted_symbols` makes drops it whichever side answers.
    fn forget_symbol(&self, symbol: &str) {
        self.shared.admitted.forget(symbol);
        if let Some(client) = self.shared.connected() {
            client.forget_symbol(symbol);
        }
    }

    /// What the real client reads once it is open, else what it will be told to read: a pass that
    /// meets the error above claims nothing, so the claim is only ever made over a real read.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        match self.shared.connected() {
            Some(client) => client.admitted_symbols(),
            None => Some(self.shared.admitted.symbols()),
        }
    }
}

#[path = "redial_tests.rs"]
#[cfg(test)]
mod redial_tests;
