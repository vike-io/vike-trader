//! **The EXECUTION STREAM's tradehub half: the FIX drop copy's sender.** The OMS journals every execution and
//! order-lifecycle event as a self-contained record (`vike_journal::JournalRecord::ExecutionReport`); this module
//! reads those records back off the journal's FILES and sends them to the datahub, which stores each one
//! idempotently and keeps one cursor per journal (`vike_datahub_client::execution_stream`'s module doc is the
//! grammar). Neither end holds per-order state.
//!
//! # It never blocks or slows trading
//!
//! - One thread, `execution-stream`, started by `crate::tradehub_cli`'s `run` once the core is up, on either arm,
//!   whenever `config.journal_dir` names a journal. The core never names it: no channel, no fence, no lock. It reads
//!   files and the journal sync thread's atomics (`vike_journal::JournalDurableWatch`), nothing else.
//! - It reads only what is DURABLE (`vike_journal::read_durable_since`), so a power cut can never take back a record it
//!   sent and hand its sequence to another. While it waits past the durable mark it asks the sync thread to sync, at
//!   most once a second (`JournalDurableWatch::request_sync`: one unbounded send, never a wait).
//! - A datahub that is down, refuses, or accepts and never reads costs only this thread: every write is bounded
//!   ([`ExecutionStreamTimings::write_timeout`]), a failure redials after [`ExecutionStreamTimings::retry_first`]
//!   doubling to [`ExecutionStreamTimings::retry_max`], and the journal keeps every record the datahub has not
//!   acknowledged (its prune floor is the stream file's `acknowledged_sequence`, up to `EXECUTION_STREAM_HOLD_MS`).
//!   The lag is said ([`lag`]), never acted on.
//! - At the stop it gets one bounded final send ([`ExecutionStreamTimings::final_send`]) and is joined as one of the
//!   teardown's bounded tasks.
//!
//! # The datahub's cursor is the authority
//!
//! The publisher keeps no resume point of its own: every connection opens with `Request::ExecutionStreamOpen`, and the
//! answer's `stored_through` is where it sends from (`None` is first contact: from the journal's first record, sequence
//! 0 included, chained from `None`). `ExecutionStreamDown::Stored` raises the journal's floor in `<journal_dir>/execution_stream.json`
//! (`vike_journal::JournalExecutionStream`, at most once a second); `ExecutionStreamDown::Resend` rewinds. Before it
//! sends, the open is checked ([`link`]): a datahub ahead of the journal, or one whose record at its cursor was recorded
//! at another time, follows a COPY of this journal, and the journal takes a new identity; a cursor the journal has
//! pruned past is said as an `ExecutionStreamUp::Gap`.
//!
//! # What is sent
//!
//! One exhaustive match over the journal's records ([`read::body_of`]): every `ExecutionReport` (a removed mount's
//! strategy code named from its durable `strategy_code` row, `vike_secrets::strategy_code::strategy_name_for`), every
//! `Forecast`, every `ExecutionReportsUnverified`, every `PositionSnapshot` (each boot's accounts' positions, what an
//! account's window opens from), and a venue fill or fee correction that names NO order (a foreign execution the OMS
//! journals no report for), built from its own `Command`. Nothing else.
//!
//! # A moved-aside journal is drained FIRST
//!
//! A boot that moves a broken journal aside lists it as the fresh journal's predecessor in the fresh journal's stream
//! file (`vike_core`'s boot restore). Before the fresh journal opens a stream, every predecessor not yet `done` is
//! streamed under its OWN journal id: its readable records past the datahub's cursor, then one
//! `ExecutionReportsUnverified` for whatever lies past the place its read halted; once the datahub has stored all of it
//! the predecessor is marked `done`. The datahub orders one order's rows across a move by the order in which it first
//! heard from each journal, so a successor must never open before its predecessor.
//!
//! A moved journal can hold a whole uptime's records, so it is never read whole (`read::MovedAside`): each connection
//! DIALS first (a datahub that is down costs no read), the journal is walked once a page at a time for its ends, and
//! then streamed a page at a time like the open journal. It owes the stream no more than the open journal does: a
//! leading run older than `EXECUTION_STREAM_HOLD_MS` is one unverified window, said at ERROR on both ends.
//!
//! # This install's order client id sessions
//!
//! A session's `vike.db` row (`order_client_id_session`, `vike_secrets::order_client_id_sessions`) is written BEFORE
//! the core mints under it: the boot's recorder (`tradehub_cli`'s `order_client_id_session_recorder`, the core's
//! `CoreConfig::order_client_id_session_recorder`) raises it to the session's starting sequence plus
//! [`ORDER_CLIENT_ID_SESSION_LEASE`]. The row is a BOUND, never behind what the session minted: an id minted a moment
//! before a crash is still this install's after its journal is moved aside, a journal that names its sessions nowhere a
//! boot can read. Every journal `Snapshot` the publisher reads then raises the lease ahead of how far the session has
//! minted, at most once a minute per session ([`read::SessionRows`]), so a session that mints past its first lease
//! stays bounded too.

/// **How far ahead of its minted sequence a session's `vike.db` row is raised**: the ids a session may mint between
/// two raises (the boot's, then one a minute from the journal's `Snapshot`s) without one of them reading as foreign
/// after a journal moved aside. 2^20: no venue's rate limits come near a million orders a minute, and the bound costs
/// little against another install's id falling inside it, about 3·10⁻¹⁰ an id at the narrowest (Binance's compact
/// 10-digit) width, the order the ownership check already accepts.
pub(crate) const ORDER_CLIENT_ID_SESSION_LEASE: u64 = 1 << 20;

mod lag;
mod link;
mod read;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_hub_protocol::auth::HubKeys;
use vike_journal::{
    JournalDurableWatch, JournalExecutionStream, JournalExecutionStreamPredecessor,
};

use self::lag::ExecutionStreamLag;
use self::link::{Ended, Pause};
use self::read::{JournalRecords, MovedAside, SessionRows, StrategyCodeMounts};

/// What the publisher streams, where to, and under which key.
pub struct ExecutionStreamSource {
    /// The OPEN journal's directory (`config.journal_dir`).
    pub journal_dir: PathBuf,
    /// How far that journal is durable (`vike_core::CoreHandle::journal_durable_watch`).
    pub watch: JournalDurableWatch,
    /// The datahub (`config.datahub_addr`, the datahub's loopback default when unset).
    pub datahub_addr: String,
    /// The daemon's identity line, sent with each open for the operator.
    pub tradehub: String,
    /// The datahub TRADE key, read again at every dial (a key minted later is found); `None` dials without one,
    /// which only a key-less datahub serves.
    pub keys: Arc<dyn Fn() -> Option<HubKeys> + Send + Sync>,
    /// The project's settings directory: where a removed mount's strategy code is named and the order client id
    /// sessions are kept. `None` (no project) names no removed mount and keeps no session.
    pub settings_dir: Option<PathBuf>,
    /// How long it waits for what.
    pub timings: ExecutionStreamTimings,
}

/// The publisher's waits. [`Default`] is production's; a test shortens them.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionStreamTimings {
    /// The first wait before a failed connection is dialled again.
    pub retry_first: Duration,
    /// The longest such wait (it doubles per failure up to this).
    pub retry_max: Duration,
    /// The wait before a datahub that does not advertise the stream is asked again.
    pub unsupported_wait: Duration,
    /// How long one look for the datahub's acknowledgements waits while there is nothing to send.
    pub ack_poll: Duration,
    /// The bound on every write to the datahub (a datahub that stops reading must not hold the thread).
    pub write_timeout: Duration,
    /// How long the final send at the stop may take.
    pub final_send: Duration,
}

impl ExecutionStreamTimings {
    /// The redial ladder: [`Self::retry_first`] doubling to [`Self::retry_max`], the one ladder
    /// (`vike_venues::reconnect::ReconnectBackoff`).
    fn redial(&self) -> vike_venues::reconnect::ReconnectBackoff {
        vike_venues::reconnect::ReconnectBackoff::new(self.retry_first, self.retry_max)
    }
}

impl Default for ExecutionStreamTimings {
    fn default() -> Self {
        ExecutionStreamTimings {
            retry_first: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            unsupported_wait: Duration::from_secs(5 * 60),
            ack_poll: Duration::from_millis(200),
            write_timeout: Duration::from_secs(60),
            final_send: Duration::from_secs(2),
        }
    }
}

/// The running publisher: its thread, joined at the stop.
pub struct ExecutionStreamPublisher {
    join: JoinHandle<()>,
}

impl ExecutionStreamPublisher {
    /// Start the `execution-stream` thread. It runs until `stop` is raised.
    ///
    /// # Errors
    /// The thread could not be spawned (nothing is streamed; trading is untouched).
    pub fn spawn(source: ExecutionStreamSource, stop: Arc<AtomicBool>) -> io::Result<Self> {
        let join = std::thread::Builder::new()
            .name("execution-stream".into())
            .spawn(move || Publisher::new(&source, &stop).run())?;
        Ok(ExecutionStreamPublisher { join })
    }

    /// Wait for the thread, once the caller raised its stop flag: bounded by its final send.
    pub fn join(self) {
        // A `join` Err is a panic its hook already reported; the teardown goes on.
        let _ = self.join.join();
    }
}

/// The thread's state across connections.
struct Publisher<'a> {
    source: &'a ExecutionStreamSource,
    stop: &'a AtomicBool,
    lag: ExecutionStreamLag,
    /// The newest durable sequence of the OPEN journal this thread has read, and how far the datahub holds it.
    head: Option<u64>,
    position: Option<u64>,
    sessions: SessionRows,
    mounts: StrategyCodeMounts,
    /// The moved-aside journal being drained, as its one walk found it (kept across connections until it is done).
    moved: Option<MovedAside>,
    /// The last failure said at `warn`: a repeat of the same words is said at `debug`.
    last_failure: Option<String>,
}

impl<'a> Publisher<'a> {
    fn new(source: &'a ExecutionStreamSource, stop: &'a AtomicBool) -> Self {
        Publisher {
            source,
            stop,
            lag: ExecutionStreamLag::default(),
            head: None,
            position: None,
            sessions: SessionRows::new(source.settings_dir.clone()),
            mounts: StrategyCodeMounts::new(source.settings_dir.clone()),
            moved: None,
            last_failure: None,
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn run(mut self) {
        let timings = self.source.timings;
        let mut redial = timings.redial();
        while !self.stopped() {
            match self.pass() {
                Ok(()) => return,
                Err(Pause::Diverged) => continue,
                Err(Pause::Unsupported(why)) => {
                    self.say_failure(&why);
                    self.sleep(timings.unsupported_wait);
                }
                Err(Pause::Failed { why, sent }) => {
                    if sent {
                        redial.reset();
                    }
                    self.say_failure(&why);
                    self.sleep(redial.failed());
                }
            }
        }
    }

    /// One connection's worth: the predecessors first, then the open journal until the stop. `Ok` only at the stop.
    fn pass(&mut self) -> Result<(), Pause> {
        let source = self.source;
        let dir = source.journal_dir.as_path();
        let mut file =
            JournalExecutionStream::load_or_create(dir, JournalExecutionStream::mint_journal_id)
                .map_err(|e| {
                    failed(format!("the journal's stream file in {}: {e}", dir.display()))
                })?;
        while let Some(i) = file.predecessors.iter().position(|p| !p.done) {
            let predecessor = file.predecessors[i].clone();
            if !predecessor.path.exists() {
                tracing::error!(
                    journal_id = %predecessor.journal_id,
                    path = %predecessor.path.display(),
                    "execution stream: a journal moved aside is gone before its executions were sent; the datahub \
                     will never hold what it did not acknowledge of it"
                );
            } else {
                // dial first: a datahub that is down costs no read of the moved journal
                let client = self.dial()?;
                let moved = match self.moved.take() {
                    Some(moved) if moved.journal_id == predecessor.journal_id => moved,
                    _ => scan_moved_aside(&predecessor)?,
                };
                let ended = self.stream(
                    client,
                    &JournalRecords::MovedAside(&moved),
                    &predecessor.journal_id,
                    None,
                );
                // a closed journal does not change: the next connection streams it without walking it again
                self.moved = Some(moved);
                match ended? {
                    Ended::Stopped => return Ok(()),
                    Ended::Drained => tracing::info!(
                        journal_id = %predecessor.journal_id,
                        path = %predecessor.path.display(),
                        "execution stream: the journal moved aside is drained"
                    ),
                }
            }
            file.predecessors[i].done = true;
            self.moved = None;
            file.store(dir).map_err(|e| {
                failed(format!("the journal's stream file in {}: {e}", dir.display()))
            })?;
        }
        let client = self.dial()?;
        let journal = JournalRecords::Live { dir, watch: &source.watch };
        let journal_id = file.journal_id.clone();
        self.stream(client, &journal, &journal_id, Some(&mut file)).map(drop)
    }

    /// A failure in words: at `warn` the first time, at `debug` while the same words repeat.
    fn say_failure(&mut self, why: &str) {
        if self.last_failure.as_deref() == Some(why) {
            tracing::debug!(datahub = %self.source.datahub_addr, "execution stream: {why}");
        } else {
            tracing::warn!(
                datahub = %self.source.datahub_addr,
                "execution stream: {why}; the journal keeps every execution the datahub has not stored, and it is \
                 asked again"
            );
            self.last_failure = Some(why.to_string());
        }
        self.observe_lag_while_away();
    }

    /// While no datahub is reached the head still moves: read it on (cheap: the reader resumes where it stopped)
    /// and say the lag.
    fn observe_lag_while_away(&mut self) {
        let source = self.source;
        let dir = source.journal_dir.as_path();
        let journal = JournalRecords::Live { dir, watch: &source.watch };
        if let Ok(head) = journal.head(self.head, &mut self.sessions) {
            self.head = head;
        }
        if self.position.is_none() {
            self.position = JournalExecutionStream::load(dir)
                .ok()
                .flatten()
                .and_then(|f| f.acknowledged_sequence);
        }
        self.observe_lag();
    }

    fn observe_lag(&mut self) {
        if let Some(event) = self.lag.observe(self.head, self.position, Instant::now()) {
            lag::log_lag(&self.source.journal_dir, event);
        }
    }

    /// Sleep `wait`, waking at once when the stop is raised.
    fn sleep(&self, wait: Duration) {
        let until = Instant::now() + wait;
        while !self.stopped() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
    }
}

fn failed(why: String) -> Pause {
    Pause::Failed { why, sent: false }
}

/// The one walk of a moved-aside journal ([`MovedAside::scan`]), its hold judged at this moment.
fn scan_moved_aside(predecessor: &JournalExecutionStreamPredecessor) -> Result<MovedAside, Pause> {
    let path = &predecessor.path;
    MovedAside::scan(&predecessor.journal_id, path, vike_model::now_ms())
        .map_err(|e| failed(format!("the journal moved aside to {}: {e}", path.display())))
}

#[cfg(test)]
#[path = "execution_stream/tests.rs"]
mod tests;
