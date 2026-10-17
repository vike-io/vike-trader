//! **The late arm: a tradehub that started while the gatewayhub was down arms its demo/live accounts
//! when the gatewayhub comes up** (decision 0164, owner ruling D7; decision H1 of the wave-2 plan).
//!
//! The owner's ruling is that arming is AUTOMATIC when the gatewayhub comes up. The core cannot add
//! an engine at run time (`vike-core` fixes its engines at `spawn_core_multi`, and adding one is a
//! measured-crate change with journal and restore consequences), so the mechanism is a bounded
//! restart, the default the plan names:
//!
//! * a named thread (`gateway-late-arm`) polls the gatewayhub — **only** while at least one account
//!   is NOT RUNNING with `ArmingBlock::GatewayUnavailable` ([`LateArmPlan::waiting`]) — and the
//!   datahub's strategy lane too when the PRICE FEEDS could not be wired for want of it
//!   (`feed_gate.rs`: a feed that is unavailable is waited for exactly like a gatewayhub that is);
//! * when it answers, the tradehub logs an `ARMING:` line, raises the process's one stop flag so the
//!   ordinary graceful teardown runs (its resting-order cancel sweep only with
//!   `flags.cancel_orders_on_shutdown`), and exits with [`EXIT_LATE_ARM`], non-zero, so
//!   `Restart=on-failure` brings it back armed — but only for a gatewayhub that was AWAY: a refusal no
//!   retry changes (a deploy version skew) leaves the account NOT RUNNING `adapter failed` and arms
//!   nothing, and an operator's stop, or a teardown that did not complete, is never turned into 75
//!   ([`claim_stop`], [`exit_status`]);
//! * **at most once per process start, and never in the first [`MIN_UPTIME`]**: a gatewayhub that
//!   flaps, or a unit that restarts the tradehub in a loop, must not turn into a restart storm, and
//!   a fresh start has just proven the gatewayhub was down a minute ago.
//!
//! * **a restart BUDGET for the feed-driven wait** ([`FEED_RESTARTS_PER_WINDOW`] per [`FEED_RESTART_WINDOW`]).
//!   The late arm's checks are the two things that already passed (the datahub's dial and the tradehub's own
//!   link); a cause they cannot see (the datahub's OWN link to the gatewayhub broken for good, a key rotated
//!   there) would pass both at the 90 s mark, fire, restart the whole process (every engine torn down, a
//!   cancel sweep with `flags.cancel_orders_on_shutdown`), meet the same refusal and repeat every ~95 s for as
//!   long as the cause lasts. A [`RestartLedger`] in the project state directory counts the feed-driven
//!   restarts across process starts; once the budget is spent the plan stops waiting for the feeds, the process
//!   stays up without prices with a banner that says the budget is spent ([`RESTARTS_SPENT_MARK`]), and an
//!   operator restarts it by hand once the cause is fixed. A restart for an account waiting on the gatewayhub
//!   alone is not counted (the restarted process finds the gatewayhub up and waits for nothing).
//!
//! The decision is a pure state machine ([`LateArm`]) over an injected clock and probe, so the
//! boundary cases (the 90 s guard, the backoff, the once-only latch) are tests on a virtual clock
//! (`late_arm_tests.rs`); the thread ([`spawn`]) only supplies the real ones.
//!
//! # A box with NO account that runs
//!
//! A tradehub whose every account needs the gatewayhub has no engine to run on while it is down
//! (`RuntimeError::NoAccountRuns`): it exits at start, non-zero, and the supervisor's own restart is
//! the arming. This module is for the mixed box — at least one `paper` account keeps the process
//! (and its strategies on paper) alive while the others wait.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::trade::rpc;

/// The entry in [`LateArmPlan::waiting`] that stands for the price feeds.
pub const FEEDS_WAIT_ENTRY: &str = "price feeds (datahub)";

/// How many feed-driven late-arm restarts one [`FEED_RESTART_WINDOW`] allows. Three: a datahub deploy or a
/// gatewayhub restart that outlasts the first start needs one restart (a second when the datahub and the
/// gatewayhub come back apart), and a cause the late arm cannot see would otherwise restart every ~95 s.
pub const FEED_RESTARTS_PER_WINDOW: usize = 3;

/// The window of [`FEED_RESTARTS_PER_WINDOW`]: an hour.
pub const FEED_RESTART_WINDOW: Duration = Duration::from_secs(3600);

/// The file in the project's state directory holding the feed-driven restarts' timestamps.
pub const RESTART_LEDGER_FILE: &str = "late-arm-feed-restarts";

/// What leads the banner's cause when the restart budget is spent (`feeds::short_cause` reads it).
pub const RESTARTS_SPENT_MARK: &str = "late-arm restart budget spent";

/// The most timestamps the ledger file keeps.
const LEDGER_KEPT: usize = 16;

/// **The count of feed-driven late-arm restarts**, kept across process starts as one line of comma-separated
/// millisecond timestamps in the project's state directory (a derived counter, not configuration: losing it only
/// resets the budget). Append-only in spirit: each write keeps the entries still inside the window and adds the
/// new one. A ledger with no path ([`RestartLedger::none`], a test's) counts nothing and never refuses.
#[derive(Debug, Clone, Default)]
pub struct RestartLedger {
    path: Option<PathBuf>,
}

impl RestartLedger {
    /// A ledger that keeps nothing: no budget.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The ledger in `state_dir`.
    #[must_use]
    pub fn in_dir(state_dir: &Path) -> Self {
        Self { path: Some(state_dir.join(RESTART_LEDGER_FILE)) }
    }

    /// The stamps in the file. A file that is absent is empty; a file that EXISTS and cannot be read is an
    /// error (not "no restarts yet"): the caller must not read a broken counter as a clean one.
    fn read(&self) -> Result<Vec<i64>, String> {
        let Some(path) = &self.path else { return Ok(Vec::new()) };
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(text.split(',').filter_map(|t| t.trim().parse().ok()).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn in_window(stamp: i64, now_ms: i64) -> bool {
        let window = i64::try_from(FEED_RESTART_WINDOW.as_millis()).unwrap_or(i64::MAX);
        // A stamp from the future (the clock stepped back) still counts: the conservative reading.
        now_ms.saturating_sub(stamp) < window
    }

    /// How many feed-driven restarts the window holds at `now_ms`.
    ///
    /// # Errors
    /// The ledger file exists and cannot be read.
    pub fn recent(&self, now_ms: i64) -> Result<usize, String> {
        Ok(self.read()?.into_iter().filter(|&t| Self::in_window(t, now_ms)).count())
    }

    /// Count a restart at `now_ms`.
    ///
    /// # Errors
    /// The file cannot be read or written. The caller must then NOT restart: an uncounted restart is how the
    /// budget would be bypassed.
    pub fn record(&self, now_ms: i64) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let mut kept: Vec<i64> =
            self.read()?.into_iter().filter(|&t| Self::in_window(t, now_ms)).collect();
        kept.push(now_ms);
        let skip = kept.len().saturating_sub(LEDGER_KEPT);
        let line = kept[skip..].iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        std::fs::write(path, line).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The exit status of a late-arm stop: `EX_TEMPFAIL` ("try again"). Non-zero on purpose, so a
/// supervisor that restarts on failure restarts it.
pub const EXIT_LATE_ARM: u8 = 75;

/// The exit status of a start that ends on a condition ONLY AN OPERATOR mends (a CAP refusal of a venue with a
/// running live account: `feed_gate::failed_start_exit`): `EX_CONFIG`. It is NOT restarted: the unit carries
/// `RestartPreventExitStatus=` for it (`deploy/vike-tradehub.service`), so a persistent cap ends in a `failed`
/// unit (and the `OnFailure=` page) instead of a restart every `RestartSec=` that tears the whole process down each
/// time. Without that directive the status is an ordinary failure and the supervisor restarts on it.
pub const EXIT_OPERATOR_NEEDED: u8 = 78;

/// The process must have been up this long before a late arm may fire.
pub const MIN_UPTIME: Duration = Duration::from_secs(90);

/// The first pause between polls, doubling to [`MAX_POLL`].
pub const FIRST_POLL: Duration = Duration::from_secs(5);

/// The longest pause between polls.
pub const MAX_POLL: Duration = Duration::from_secs(30);

/// How finely the thread watches the stop flag between polls.
const WATCH_SLICE: Duration = Duration::from_millis(250);

/// What a live mount hands the root when some account is waiting for the gatewayhub, or the price feeds
/// are waiting for the datahub.
#[derive(Debug, Clone)]
pub struct LateArmPlan {
    /// Where to ask (and the Trade key to ask with).
    pub link: GatewayLink,
    /// The route keys of the accounts NOT RUNNING with `gatewayhub unavailable`, for the line (and a
    /// `price feeds (datahub)` entry when the feeds wait).
    pub waiting: Vec<String>,
    /// Whether the gatewayhub is part of the wait: some account is NOT RUNNING `gatewayhub unavailable`, or a
    /// `demo`/`live` account of a venue whose feed was held back exists (it arms through the gatewayhub once
    /// the feed is back, so a restart that finds the gatewayhub away would arm nothing). `false` for a box whose
    /// only wait is the datahub and that has no such account (every account `paper`): such a box may have no
    /// gatewayhub key at all, its own link to the gatewayhub can never answer, and it must not wait for one.
    pub gateway: bool,
    /// `Some` when the price feeds could not be wired for want of the datahub (`feed_gate.rs`): the late arm
    /// fires only once this answers too. Without it a datahub that stays away while the gatewayhub is up
    /// would fire the restart at the first poll, and the restarted process would wait again: a restart
    /// every ~95 s.
    pub datahub: Option<crate::feeds::DatahubProbe>,
    /// Why the price feeds are unavailable, in words: the probe's refusal (or the transient refusal a feed met at
    /// wiring time). `None` when the feeds are not what waits. The ready banner leads with it
    /// (`PRICE FEEDS UNAVAILABLE: no strategy-lane key - ...`) so a process that is up but has no prices says so
    /// at the place the operator looks.
    pub feeds_why: Option<String>,
    /// Where the feed-driven restarts are counted ([`RestartLedger`]); [`RestartLedger::none`] for a plan with no
    /// budget. Only a plan that waits for the feeds ([`Self::datahub`]) is counted.
    pub restarts: RestartLedger,
}

impl LateArmPlan {
    /// The plan for what a live mount left waiting, or `None` when nothing waits. `accounts` are the route
    /// keys of the accounts NOT RUNNING with `gatewayhub unavailable` (the feed gate holds its accounts
    /// with the same word); `datahub` is the probe of the price feeds when they could not be wired.
    #[must_use]
    pub fn of(
        link: GatewayLink,
        accounts: Vec<String>,
        datahub: Option<crate::feeds::DatahubProbe>,
    ) -> Option<Self> {
        let gateway = !accounts.is_empty();
        let mut waiting = accounts;
        if datahub.is_some() {
            waiting.push(FEEDS_WAIT_ENTRY.to_string());
        }
        (!waiting.is_empty()).then_some(LateArmPlan {
            link,
            waiting,
            gateway,
            datahub,
            feeds_why: None,
            restarts: RestartLedger::none(),
        })
    }

    /// A plan that waits for NOTHING and exists to carry the banner's cause (`feeds_why`): a process that is up
    /// without prices for a reason no restart mends (a CAP, [`crate::feeds::FEEDS_CAPPED_MARK`]). [`spawn`] starts
    /// no thread for it (its `waiting` is empty), so it orders no restart, ever.
    #[must_use]
    pub fn notice(link: GatewayLink, feeds_why: String) -> Self {
        LateArmPlan {
            link,
            waiting: Vec::new(),
            gateway: false,
            datahub: None,
            feeds_why: Some(feeds_why),
            restarts: RestartLedger::none(),
        }
    }

    /// **Apply the restart budget** at `now_ms`: a plan that waits for the feeds while the ledger already holds
    /// [`FEED_RESTARTS_PER_WINDOW`] restarts in the window stops waiting for them. The process then stays up
    /// without prices (it would only restart into the same refusal), says so at `error`, and its banner cause
    /// leads with [`RESTARTS_SPENT_MARK`]; an account still waiting for the gatewayhub alone keeps its wait. An
    /// unreadable ledger is read as spent: a counter that cannot be trusted must not license a restart.
    #[must_use]
    pub fn within_budget(mut self, now_ms: i64) -> Self {
        if self.datahub.is_none() {
            return self;
        }
        let spent = match self.restarts.recent(now_ms) {
            Ok(used) if used < FEED_RESTARTS_PER_WINDOW => return self,
            Ok(used) => format!(
                "{used} feed-driven late-arm restarts in the last {} min (the budget is \
                 {FEED_RESTARTS_PER_WINDOW})",
                FEED_RESTART_WINDOW.as_secs() / 60
            ),
            Err(e) => format!("the restart ledger cannot be read ({e})"),
        };
        tracing::error!(
            reason = %spent,
            "{}: {spent}. The cause is not one this process can see or mend (the price path answers the \
             late arm's checks and still refuses the feeds), so it stays up WITHOUT prices instead of \
             restarting again: `paper` accounts run priceless and strategies on a held venue see nothing. \
             Fix the cause (the datahub's own link to the gatewayhub, its keys, a cap) and restart this \
             tradehub by hand",
            crate::feed_gate::PRICE_FEEDS_UNAVAILABLE
        );
        self.datahub = None;
        self.waiting.retain(|w| w != FEEDS_WAIT_ENTRY);
        self.feeds_why = Some(format!(
            "{RESTARTS_SPENT_MARK}: {}",
            self.feeds_why.as_deref().unwrap_or("the price feeds were refused")
        ));
        self
    }

    /// What the wait is for, in words, for the log: the gatewayhub, the datahub's strategy lane, or both.
    #[must_use]
    pub fn answers_words(&self) -> String {
        let hub = format!("the gatewayhub at {}", self.link.addr());
        match (self.gateway, self.datahub.is_some()) {
            (true, true) => format!("{hub} and the datahub's strategy lane answer"),
            (true, false) => format!("{hub} answers"),
            (false, true) => "the datahub's strategy lane answers".to_string(),
            (false, false) => "everything waited for answers".to_string(),
        }
    }

    /// Whether everything this plan waits for answers now: the gatewayhub when an account waits for it,
    /// the datahub when the feeds do. The thread's probe, kept apart so a test asks it without a thread.
    #[must_use]
    pub fn answers(&self) -> bool {
        (!self.gateway || rpc::arming_table(&self.link).is_ok())
            && self.datahub.as_ref().is_none_or(|probe| probe.check().is_none())
    }
}

/// One decision of the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing is due; look again at the next watch slice.
    Idle,
    /// The probe was made and the gatewayhub did not answer.
    StillDown,
    /// The probe was made and the gatewayhub answered: stop gracefully, exit non-zero. Returned
    /// at most once.
    Fire,
}

/// The pure machine. `now` is the time since the process started, on whatever clock the caller has.
#[derive(Debug)]
pub struct LateArm {
    next_poll: Duration,
    pause: Duration,
    fired: bool,
}

impl Default for LateArm {
    fn default() -> Self {
        LateArm { next_poll: MIN_UPTIME, pause: FIRST_POLL, fired: false }
    }
}

impl LateArm {
    /// A machine that polls for the first time at [`MIN_UPTIME`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance to `now`, probing at most once. `probe` answers whether the gatewayhub answers.
    pub fn step(&mut self, now: Duration, probe: &mut dyn FnMut() -> bool) -> Step {
        if self.fired || now < self.next_poll {
            return Step::Idle;
        }
        if probe() {
            self.fired = true;
            return Step::Fire;
        }
        self.next_poll = now + self.pause;
        self.pause = (self.pause * 2).min(MAX_POLL);
        Step::StillDown
    }

    /// Whether it has fired.
    #[must_use]
    pub fn fired(&self) -> bool {
        self.fired
    }
}

/// What a root keeps of a running late arm.
#[derive(Debug)]
pub struct LateArmHandle {
    /// Set when the late arm fired: the root returns [`EXIT_LATE_ARM`] after its teardown.
    pub fired: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LateArmHandle {
    /// Join the thread (it ends within one watch slice of `stop`). Teardown calls this.
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            // A panic in the poll thread has been reported by its hook; teardown goes on.
            let _ = thread.join();
        }
    }
}

/// **Claim the process stop for the late arm, unless somebody already holds it.** `fired` is raised BEFORE
/// the stop (the root reads it only after the teardown the stop starts), and taken back when the stop was
/// already up: a SIGTERM that latched while the probe was in flight is the OPERATOR's stop, and it wins —
/// the process exits with the teardown's status, never 75 (a `systemctl stop` must not read as a failure,
/// and must not restart). `true` when the late arm owns the stop.
#[must_use]
pub fn claim_stop(stop: &AtomicBool, fired: &AtomicBool) -> bool {
    fired.store(true, Ordering::SeqCst);
    if stop.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        return true;
    }
    fired.store(false, Ordering::SeqCst);
    false
}

/// **The status the late arm adds to a finished teardown**: `Some(`[`EXIT_LATE_ARM`]`)` only when the late arm
/// fired AND the teardown was graceful. A teardown that was hard-capped or aborted keeps ITS status and
/// says so at `error`: resting orders may not all have been cancelled, and a clean-looking restart on top
/// of that is not the late arm's to order. The accounts then wait for the next start, as without a late arm.
#[must_use]
pub fn exit_status(fired: bool, teardown_graceful: bool) -> Option<u8> {
    match (fired, teardown_graceful) {
        (true, true) => Some(EXIT_LATE_ARM),
        (true, false) => {
            tracing::error!(
                "the late arm fired but the teardown did not complete gracefully: the exit status is \
                 the teardown's, not {EXIT_LATE_ARM} (no restart is ordered over a teardown that \
                 left work undone)"
            );
            None
        }
        (false, _) => None,
    }
}

/// Start the `gateway-late-arm` thread. `started` is when the PROCESS started (so the 90 s guard is
/// the process's age), `stop` the process's one stop flag. Returns `None` when the plan is empty
/// (nothing waits) or the thread cannot be spawned (logged: the supervisor's restart is then the
/// only arm, which is what a start with no late arm already is).
pub fn spawn(plan: LateArmPlan, started: Instant, stop: Arc<AtomicBool>) -> Option<LateArmHandle> {
    if plan.waiting.is_empty() {
        return None;
    }
    let fired = Arc::new(AtomicBool::new(false));
    let thread_fired = Arc::clone(&fired);
    tracing::warn!(
        accounts = plan.waiting.join(", "),
        "{} thing(s) wait: the tradehub polls until {} (after {} s of uptime, every {}-{} s) and restarts \
         itself armed, once, when they do. A missing strategy-lane key is read from the key store again \
         at every poll, so minting it later arms this process too{}",
        plan.waiting.len(),
        plan.answers_words(),
        MIN_UPTIME.as_secs(),
        FIRST_POLL.as_secs(),
        MAX_POLL.as_secs(),
        if plan.datahub.is_some() {
            format!(
                "; a restart for the price feeds is counted, at most {FEED_RESTARTS_PER_WINDOW} per {} \
                 min",
                FEED_RESTART_WINDOW.as_secs() / 60
            )
        } else {
            String::new()
        }
    );
    let spawned = std::thread::Builder::new().name("gateway-late-arm".to_string()).spawn(move || {
        let mut machine = LateArm::new();
        let mut probe = || plan.answers();
        while !stop.load(Ordering::Relaxed) {
            match machine.step(started.elapsed(), &mut probe) {
                Step::Fire => {
                    // A feed-driven restart is COUNTED before it is ordered; a count that cannot be written is
                    // a restart that cannot be limited, so it is not ordered (the process stays up).
                    if plan.datahub.is_some()
                        && let Err(e) = plan.restarts.record(vike_model::now_ms())
                    {
                        tracing::error!(
                            error = %e,
                            "the late arm would restart this tradehub for its price feeds but could not \
                             write its restart ledger: no restart is ordered, because an uncounted restart \
                             is how a restart loop would go unbounded. The feeds stay unwired until an \
                             operator restarts this process"
                        );
                        return;
                    }
                    tracing::warn!(
                        "ARMING: {}; {} wait(s) were NOT RUNNING (`gatewayhub unavailable`) or without \
                         their price feeds ({}). Stopping gracefully (the cancel sweep runs only with \
                         `flags.cancel_orders_on_shutdown`) and exiting {EXIT_LATE_ARM} so the \
                         supervisor restarts this tradehub with them armed — once per process start",
                        plan.answers_words(),
                        plan.waiting.len(),
                        plan.waiting.join(", ")
                    );
                    if !claim_stop(&stop, &thread_fired) {
                        tracing::warn!(
                            "late arm: what it waited for answered while the process was already \
                             stopping; the stop stands and the exit status is the teardown's, not \
                             {EXIT_LATE_ARM}"
                        );
                    }
                    return;
                }
                Step::StillDown => tracing::debug!(
                    "late arm: not everything it waits for answers yet ({})",
                    plan.waiting.join(", ")
                ),
                Step::Idle => {}
            }
            std::thread::sleep(WATCH_SLICE);
        }
    });
    match spawned {
        Ok(thread) => Some(LateArmHandle { fired, thread: Some(thread) }),
        Err(e) => {
            tracing::error!(error = %e, "the late-arm thread could not be spawned; the accounts wait for a restart");
            None
        }
    }
}

#[path = "late_arm_tests.rs"]
#[cfg(test)]
mod late_arm_tests;
