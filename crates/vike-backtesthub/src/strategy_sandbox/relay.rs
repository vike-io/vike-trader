//! The PARENT of a sandboxed backtest: spawn the child, hand it the request, answer its history reads
//! from this daemon's own store, and take its answer. A cancel is a SIGKILL of the child. An ANSWERED run does not
//! wait for its child to exit (it frees its memory first, 10–20 ms): the child is reaped on a thread of its own, which
//! the next run on the calling thread waits for ([`reap_later`]); a failed or cancelled run reaps at once, its exit
//! being part of its answer.
//!
//! The child is started by `vike_sandbox::spawn_strategy_process` (an empty environment, working
//! directory `/`, three pipes): its stdin and stdout are the protocol, its stderr the reason this side
//! drains into a bounded tail (and where a strategy's own `println!` lands: the child points its fd 1 at
//! its stderr before it loads the file, `crates/vike-strategy-sandbox/src/lib.rs`'s module doc).

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use vike_backtest::search::paramscan::threads::paramscan_threads;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::summary::{ParamscanRowSummaryWire, RunSummaryWire};
use vike_data::{HistoryStore, HistoryStoreError, SharedColumns};
use vike_datahub_client::HistoryStoreDatahub;
use vike_datahub_client::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, ColumnRow, ColumnsMagic, DEPTH_COLUMNS, QUOTE_COLUMNS,
    TRADE_COLUMNS,
};
use vike_hub_protocol::child_pipe;
use vike_model::StoreKindId;
use vike_model::Trade;
use vike_runs::RunEquityCurve;

use crate::backtesthub_server::saved_run::{ParamscanSave, StudioRun};
use crate::run_summary::HeldLog;

use super::RustRun;
use super::wire::{
    BARS_PER_FRAME, ChildFrame, ChildRequest, KeptAnswer, ROWS_PER_FRAME, ReadAnswerFrame,
    StoreRead, TICKS_PER_FRAME, columns_part_into, write_kept_frame,
};
use super::witness::{ReadWitnesses, Seen};

/// How much of the child's reason pipe a failure quotes: its LAST bytes.
const REASON_TAIL_BYTES: usize = 4096;

/// How often a watched run looks at its client's peer-gone flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// How long a child that answered (or closed its channel) gets to exit before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(1);

/// The fetch workers of one relay, so also K, the most answers it holds fetched and not yet written (each worker
/// writes its own answer in its turn): a run's peak is that many reads' answers, not its whole plan, and that many
/// fetches at once is all the daemon's datahub handle serves together ([`HistoryStoreDatahub::POOL_CONNECTIONS`], 4),
/// so one more would only wait there holding nothing.
pub(super) const READ_WORKERS: usize = HistoryStoreDatahub::POOL_CONNECTIONS;

/// The most reads a child may have asked and not yet been answered. A run names a few reads per series ahead (its
/// price bars and inputs, a tick replay's quotes, trades and book), so an honest run of a thousand symbols stays far
/// under it; a child past it is stopped and its run refused in words naming it.
pub(super) const MAX_UNANSWERED_READS: u64 = 8192;

/// The most bytes of read frames a child may have waiting for a worker: an honest read is well under a KiB, and a
/// frame may be up to `vike_hub_protocol::frame::MAX_FRAME_LEN`, so the count alone bounds no memory.
const MAX_QUEUED_READ_BYTES: usize = 16 << 20;

/// A child's answer, with what it streamed of a SUMMARIZED run for the parent to hold (none for any
/// other run).
pub(super) struct Answered {
    /// The `Response` the in-process daemon would have written (a summarized run's own answer).
    pub(super) response: Response,
    /// The points a summarized scan closed, each with its trades, in point order.
    pub(super) points: Vec<(RunSummaryWire, Vec<Trade>)>,
    /// The trades sent after the last point: a summarized single run's.
    pub(super) trades: Vec<Trade>,
    /// A summarized single run's whole equity curve, its `ChildFrame::HeldCurve` parts joined in order (empty when
    /// its `log` came up in their place).
    pub(super) curve: RunEquityCurve,
    /// A summarized single bar run's event log, sent in its curve's place (`ChildFrame::HeldLog`).
    pub(super) log: Option<Box<HeldLog>>,
    /// The saved run a Studio single run left (`ChildFrame::StudioRun`), for this process's saver.
    pub(super) studio_run: Option<StudioRun>,
    /// A summarized profile search's rows (`ChildFrame::ParamscanRow`), best first.
    pub(super) rows: Vec<ParamscanRowSummaryWire>,
}

/// How a sandboxed run ended.
pub(super) enum Outcome {
    /// The child's answer.
    Answer(Box<Answered>),
    /// The client left, and the child was killed.
    Cancelled,
    /// The child could not start, died, or broke the protocol: the reason, its exit and its last words.
    Failed(String),
}

/// Run `request` in ONE child of `program` (`<program> strategy-sandbox backtest <builds> <artifact>...`) that
/// loads every mount's artifact (`runs`, in mount order: each the artifact its text was built into, its path under
/// `builds`, the builder's answer), its history read from `store` and each mount's text and model keys' texts handed
/// over in the start message. `client_gone` set kills the child. A summarized search's rows are written by `save` as
/// they come up, and each read is recorded with its witness in `witness` when given ([`relay_witnessed`]).
pub(super) fn run(
    program: &Path,
    build: &str,
    request: &Request,
    builds: &Path,
    runs: &[RustRun],
    store: &(dyn HistoryStore + Send + Sync),
    client_gone: Option<&AtomicBool>,
    save: Option<&mut ParamscanSave<'_>>,
    witness: Option<&ReadWitnesses>,
) -> Outcome {
    wait_for_the_last_reap();
    let args = [Path::new("strategy-sandbox"), Path::new("backtest"), builds]
        .into_iter()
        .chain(runs.iter().map(|run| Path::new(&run.artifact)));
    let mut child = match vike_sandbox::spawn_strategy_process(program, args) {
        Ok(child) => child,
        Err(e) => {
            return Outcome::Failed(format!("the strategy sandbox could not be started: {e}"));
        }
    };
    let (Some(mut to_child), Some(mut from_child), Some(reason)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Outcome::Failed("the strategy sandbox started without its three pipes".to_string());
    };
    // Not scoped: an answered run's reaper takes it along ([`reap_later`]).
    let tail = match thread::Builder::new()
        .name("sandbox-reason".into())
        .spawn(move || reason_tail(reason))
    {
        Ok(tail) => tail,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Failed(format!(
                "the strategy sandbox's reason pipe could not be read (no thread: {e})"
            ));
        }
    };
    // The history streams DOWN in bar parts of ~1 MB; the answers up are small. Only this pipe is widened, so a
    // child holds 1 MiB more of the user's `pipe-user-pages-soft`, not 2.
    vike_sandbox::widen_pipe(&to_child);
    let child = Arc::new(Mutex::new(child));
    let done = AtomicBool::new(false);
    let (child_ref, done_ref) = (&*child, &done);
    let relayed = thread::scope(|scope| {
        let watcher = client_gone.map(|gone| scope.spawn(move || watch(gone, done_ref, child_ref)));
        // An answer still going down when the relay ends may be blocked on a child that reads no more: killed.
        let stop = || {
            let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
        };
        let mounts = runs.iter().map(|run| (run.source.text.as_str(), &run.models)).collect();
        let relayed = relay_witnessed(
            &mut to_child,
            &mut from_child,
            build,
            request,
            mounts,
            store,
            &stop,
            save,
            witness,
        );
        // Closed before the reap, so a child still reading sees end-of-file rather than a wait.
        drop(to_child);
        done.store(true, Ordering::Relaxed);
        if let Some(watcher) = &watcher {
            watcher.thread().unpark();
        }
        relayed
    });
    match relayed {
        // The child exits AFTER its answer, freeing its memory first: the answer does not wait for that.
        Ok(answered) => {
            reap_later(child, tail);
            Outcome::Answer(Box::new(answered))
        }
        Err(e) => {
            let status = end(&child, e.kill);
            let tail = tail.join().unwrap_or_default();
            if client_gone.is_some_and(|g| g.load(Ordering::Relaxed)) {
                return Outcome::Cancelled;
            }
            let tail =
                if tail.trim().is_empty() { String::new() } else { format!(": {}", tail.trim()) };
            Outcome::Failed(format!("{} ({status}){tail}", e.why))
        }
    }
}

thread_local! {
    /// The reaper of the child this thread's last answered run left exiting ([`reap_later`]).
    static REAPING: RefCell<Option<thread::JoinHandle<()>>> = const { RefCell::new(None) };
}

/// Reap an ANSWERED run's `child` on a thread of its own, [`end`]'s way (up to [`EXIT_GRACE`] to exit, then a
/// SIGKILL), its reason pipe read to the end (`tail`). The next [`run`] on this thread waits for it
/// ([`wait_for_the_last_reap`]), so a thread never has two children alive, as when every run waited here; a reaper
/// that cannot start reaps here.
fn reap_later(child: Arc<Mutex<Child>>, tail: thread::JoinHandle<String>) {
    let reaping = Arc::clone(&child);
    let reaper = thread::Builder::new().name("sandbox-reap".into()).spawn(move || {
        end(&reaping, false);
        let _ = tail.join();
    });
    match reaper {
        Ok(reaper) => REAPING.with_borrow_mut(|last| *last = Some(reaper)),
        Err(_) => {
            end(&child, false);
        }
    }
}

/// Wait until the child this thread's last answered run left is reaped ([`reap_later`]).
pub(super) fn wait_for_the_last_reap() {
    if let Some(reaper) = REAPING.with_borrow_mut(Option::take) {
        let _ = reaper.join();
    }
}

/// Why the relay stopped without an answer, and whether the child must be killed (it is still running
/// and broke the protocol) rather than given [`EXIT_GRACE`].
pub(super) struct Stopped {
    pub(super) why: String,
    kill: bool,
}

/// The protocol, parent side: this build and the request down (the request NAMED: `wire`'s module doc), the
/// child's build checked, then every read answered and every held part kept until the answer comes up. A child
/// of another build is stopped, its refusal in words.
///
/// ⚠ **Several reads in flight** (decision 0281's amendment of 2026-11-19): this thread only READS the child's
/// frames and queues each read, so a child writing reads while answers are still going down is never blocked (a
/// child's prefetch reads its answers only once its whole batch is written: a relay that paused its reading would
/// deadlock with it). [`READ_WORKERS`] workers fetch the queued reads from `store` in request order and write each
/// answer in REQUEST order, whole before the next ([`InTurn`]), so the frames need no tag and at most
/// [`READ_WORKERS`] answers are held. A child past [`MAX_UNANSWERED_READS`] unanswered reads (or
/// [`MAX_QUEUED_READ_BYTES`] of queued ones) is stopped. When the relay ends, the reads no worker took are dropped
/// and a fetch in flight finishes unwritten; with an answer still unwritten, a writer may be blocked on a child that
/// reads no more: `stop` (the caller's kill) runs first, so it fails rather than waits. A summarized search's rows
/// are kept and their whole JSON handed to `save` as they come up.
///
/// The tests' entry, ONE mount (`text`, `models`), every read unwitnessed; [`run`] relays through [`relay_witnessed`].
#[cfg(any(test, feature = "test-support"))]
pub(super) fn relay(
    to_child: &mut (impl Write + Send),
    from_child: &mut impl Read,
    build: &str,
    request: &Request,
    text: &str,
    models: &BTreeMap<String, String>,
    store: &(dyn HistoryStore + Send + Sync),
    stop: &dyn Fn(),
    save: Option<&mut ParamscanSave<'_>>,
) -> Result<Answered, Stopped> {
    relay_witnessed(
        to_child,
        from_child,
        build,
        request,
        vec![(text, models)],
        store,
        stop,
        save,
        None,
    )
}

/// [`relay`] over every mount's text and model texts (`mounts`, in mount order), each read the child makes recorded
/// with its WITNESS in `witness` when given (decision 0078's amendment of 2026-11-20: `super::witness`'s module doc),
/// taken by the worker that answers it, before the answer is fetched.
pub(super) fn relay_witnessed(
    to_child: &mut (impl Write + Send),
    from_child: &mut impl Read,
    build: &str,
    request: &Request,
    mounts: Vec<(&str, &BTreeMap<String, String>)>,
    store: &(dyn HistoryStore + Send + Sync),
    stop: &dyn Fn(),
    save: Option<&mut ParamscanSave<'_>>,
    witness: Option<&ReadWitnesses>,
) -> Result<Answered, Stopped> {
    let paramscan_threads = paramscan_threads();
    let first = ChildRequest { request, paramscan_threads, mounts };
    child_pipe::send_build(to_child, build)
        .and_then(|()| child_pipe::write_message_named(to_child, &first))
        .map_err(ended)?;
    match child_pipe::check_build(from_child, build, "the strategy sandbox") {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            return Err(Stopped { why: e.to_string(), kill: true });
        }
        Err(e) => return Err(ended(e)),
    }
    let answers = InTurn::new(to_child, witness);
    thread::scope(|scope| {
        let workers = (0..READ_WORKERS)
            .filter(|_| {
                let answers = &answers;
                thread::Builder::new()
                    .name("relay-read".into())
                    .spawn_scoped(scope, move || answers.work(store))
                    .is_ok()
            })
            .count();
        let relayed = match workers {
            0 => Err(Stopped {
                why: "the strategy sandbox's history reads could not be answered: no thread could \
                      start to fetch them"
                    .to_string(),
                kill: true,
            }),
            _ => read_frames(from_child, save, |read, bytes| answers.ask(read, bytes)),
        };
        answers.close(stop);
        relayed
    })
}

/// The relay's stop when the channel ended or faulted under it.
fn ended(e: io::Error) -> Stopped {
    Stopped { why: format!("the strategy sandbox ended without an answer ({e})"), kill: false }
}

/// The bytes read through a reader, counted: a queued read's cost is its frame's.
struct Counted<'r, R> {
    inner: &'r mut R,
    bytes: usize,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes += n;
        Ok(n)
    }
}

/// [`relay`]'s loop over the child's frames: each read handed to `ask` with its frame's length (a refusal stops the
/// relay), every held part kept until the answer, a summarized search's rows written by `save`.
fn read_frames(
    from_child: &mut impl Read,
    mut save: Option<&mut ParamscanSave<'_>>,
    mut ask: impl FnMut(StoreRead, usize) -> Result<(), Stopped>,
) -> Result<Answered, Stopped> {
    let (mut points, mut trades, mut studio_run, mut rows) =
        (Vec::new(), Vec::new(), None, Vec::new());
    let (mut curve, mut log) = (RunEquityCurve::default(), None);
    let mut from_child = Counted { inner: from_child, bytes: 0 };
    loop {
        from_child.bytes = 0;
        match child_pipe::read_message::<_, ChildFrame>(&mut from_child) {
            Ok(ChildFrame::Read(read)) => ask(read, from_child.bytes)?,
            Ok(ChildFrame::HeldTrades(part)) => trades.extend(part),
            Ok(ChildFrame::HeldPoint(point)) => points.push((*point, std::mem::take(&mut trades))),
            Ok(ChildFrame::HeldCurve(part)) => {
                curve.equity.extend(part.equity);
                curve.timestamp.extend(part.timestamp);
            }
            Ok(ChildFrame::HeldLog(held)) => log = Some(held),
            Ok(ChildFrame::StudioRun(run)) => studio_run = Some(*run),
            Ok(ChildFrame::ParamscanRow(row, json)) => {
                if let Some(save) = save.as_deref_mut() {
                    save.row(json.as_deref());
                }
                rows.push(*row);
            }
            Ok(ChildFrame::ParamscanTail(tail)) => {
                if let Some(save) = save.as_deref_mut() {
                    save.tail(tail);
                }
            }
            Ok(ChildFrame::Answer(response)) => {
                let response = *response;
                return Ok(Answered { response, points, trades, curve, log, studio_run, rows });
            }
            Err(e) if e.kind() != io::ErrorKind::InvalidData => return Err(ended(e)),
            Err(e) => {
                return Err(Stopped {
                    why: format!(
                        "the strategy sandbox sent a frame this backtesthub cannot read ({e}), so it \
                         was stopped"
                    ),
                    kill: true,
                });
            }
        }
    }
}

/// The answers going down to the child, written in REQUEST order: answer `n` waits for every answer before it to be
/// written whole, so the frames of two answers never interleave and the child reads them positionally. The reads
/// wait in a [`Queue`] for the relay's workers ([`Self::work`]), who take them in request order.
struct InTurn<'w, W> {
    writer: Mutex<Writer<'w, W>>,
    /// Signalled when an answer is written whole, or the relay closes.
    turn: Condvar,
    /// How many answers were written whole (or given up).
    done: AtomicU64,
    queue: Mutex<Queue>,
    /// Signalled when a read is queued, or the relay closes.
    queued: Condvar,
    /// Where each read's witness is recorded, for a run that may be answered from its saved run later.
    witness: Option<&'w ReadWitnesses>,
}

/// The reads no worker took yet, in request order, each with its frame's length; their bytes; how many reads the
/// child asked; and whether the relay ended.
struct Queue {
    reads: VecDeque<(u64, StoreRead, usize)>,
    bytes: usize,
    asked: u64,
    closed: bool,
}

/// The pipe down, the number of the answer whose turn it is, and whether the relay ended.
struct Writer<'w, W> {
    out: &'w mut W,
    next: u64,
    closed: bool,
}

impl<'w, W: Write> InTurn<'w, W> {
    fn new(out: &'w mut W, witness: Option<&'w ReadWitnesses>) -> Self {
        Self {
            writer: Mutex::new(Writer { out, next: 0, closed: false }),
            turn: Condvar::new(),
            done: AtomicU64::new(0),
            queue: Mutex::new(Queue { reads: VecDeque::new(), bytes: 0, asked: 0, closed: false }),
            queued: Condvar::new(),
            witness,
        }
    }

    /// Queue the child's next read (its frame `bytes` long) for a worker, numbered in request order; refused, the
    /// child to be killed, past [`MAX_UNANSWERED_READS`] unanswered reads or [`MAX_QUEUED_READ_BYTES`] queued.
    fn ask(&self, read: StoreRead, bytes: usize) -> Result<(), Stopped> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let unanswered = queue.asked - self.done.load(Ordering::Acquire);
        if unanswered >= MAX_UNANSWERED_READS || queue.bytes + bytes > MAX_QUEUED_READ_BYTES {
            return Err(Stopped {
                why: format!(
                    "the strategy sandbox asked more history reads than a run may leave unanswered (at most \
                     {MAX_UNANSWERED_READS} reads, and {} MiB of them waiting), so it was stopped",
                    MAX_QUEUED_READ_BYTES >> 20
                ),
                kill: true,
            });
        }
        let number = queue.asked;
        queue.asked += 1;
        queue.bytes += bytes;
        queue.reads.push_back((number, read, bytes));
        drop(queue);
        self.queued.notify_one();
        Ok(())
    }

    /// One fetch worker: the next queued read, in request order, fetched from `store` and written in its turn
    /// ([`Self::answer_in_turn`]), until the relay closes.
    fn work(&self, store: &(dyn HistoryStore + Send + Sync)) {
        loop {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            let (number, read, bytes) = loop {
                if queue.closed {
                    return;
                }
                match queue.reads.pop_front() {
                    Some(next) => break next,
                    None => {
                        queue = self.queued.wait(queue).unwrap_or_else(PoisonError::into_inner);
                    }
                }
            };
            queue.bytes -= bytes;
            drop(queue);
            self.answer_in_turn(number, store, read);
        }
    }

    /// The writer once it is answer `number`'s turn, or the relay closed.
    fn turn_of(&self, number: u64) -> MutexGuard<'_, Writer<'w, W>> {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        while writer.next != number && !writer.closed {
            writer = self.turn.wait(writer).unwrap_or_else(PoisonError::into_inner);
        }
        writer
    }

    /// Read number `number`, fetched from `store` now and written in its turn ([`answer`] through a [`Turn`]).
    fn answer_in_turn(
        &self,
        number: u64,
        store: &(dyn HistoryStore + Send + Sync),
        read: StoreRead,
    ) {
        let mut turn = Turn { answers: self, number, writer: None };
        // A witnessed read is stamped BEFORE its answer is fetched, and its answer seen as it goes down
        // (`super::witness`'s module doc).
        let witnessed =
            self.witness.map(|witness| (witness, witness.before(store, &read), read.clone()));
        let mut seen = witnessed
            .as_ref()
            .map_or_else(|| Seen::new(false), |(witness, before, _)| witness.seen(before));
        // A broken pipe means the child is gone (or the relay ended); its next frame says so, with its exit. Every
        // other fault is this read's refusal in words, written in its turn after whole frames only (the child reads a
        // `Failed` after any parts): a store read that PANICS (as `crate::study_sandbox` catches a parent's answer;
        // unwinding here would leave every later answer waiting for this turn, and `thread::scope` would raise it
        // again at the relay's end), an answer the frame cap or its encoding refuses. A turn that moved on without
        // its `Failed` would hand the NEXT answer's frames to this ask, and every later ask one answer late.
        let answered =
            catch_unwind(AssertUnwindSafe(|| answer(&mut seen.over(&mut turn), store, read)));
        let refusal = match answered {
            Ok(Ok(())) => None,
            Ok(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => None,
            Ok(Err(e)) => Some(format!("the backtesthub could not answer this read: {e}")),
            Err(panic) => Some(format!(
                "the backtesthub's store panicked answering this read: {}",
                vike_utility::panic::payload_text(panic.as_ref())
            )),
        };
        if let Some((witness, before, read)) = witnessed {
            witness.record(number, read, before, &seen, refusal.is_some());
        }
        if let Some(refusal) = refusal {
            let failed = ReadAnswerFrame::<()>::Failed(HistoryStoreError::Io(refusal));
            let _ = child_pipe::write_message(&mut turn, &failed);
        }
        let mut writer = turn.writer.take().unwrap_or_else(|| self.turn_of(number));
        writer.next = writer.next.max(number + 1);
        drop(writer);
        self.done.fetch_add(1, Ordering::Release);
        self.turn.notify_all();
    }

    /// End the answers: the reads no worker took are dropped and every idle worker leaves; with an answer still
    /// unwritten, `stop` runs (a writer may be blocked on a child that reads no more), then every writer still
    /// waiting for its turn gives up. A fetch in flight finishes and is not written.
    fn close(&self, stop: &dyn Fn()) {
        let asked = {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.closed = true;
            queue.reads.clear();
            queue.asked
        };
        self.queued.notify_all();
        if self.done.load(Ordering::Acquire) < asked {
            stop();
        }
        self.writer.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
        self.turn.notify_all();
    }
}

/// One answer's way down: it waits for its turn at its FIRST write (so its fetch runs before), then holds the pipe
/// until it is written whole.
struct Turn<'a, 'w, W> {
    answers: &'a InTurn<'w, W>,
    number: u64,
    writer: Option<MutexGuard<'a, Writer<'w, W>>>,
}

impl<W: Write> Turn<'_, '_, W> {
    fn out(&mut self) -> io::Result<&mut W> {
        let (answers, number) = (self.answers, self.number);
        let writer = self.writer.get_or_insert_with(|| answers.turn_of(number));
        if writer.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the relay ended before this answer's turn",
            ));
        }
        Ok(&mut *writer.out)
    }
}

impl<W: Write> Write for Turn<'_, '_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out()?.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.out()?.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out()?.flush()
    }
}

/// Answer one history read from `store`, the store's own answer variant for variant.
pub(super) fn answer(
    out: &mut impl Write,
    store: &(dyn HistoryStore + Send + Sync),
    read: StoreRead,
) -> io::Result<()> {
    match read {
        // A bar or tick read is answered from the store's KEPT columns where it keeps them
        // (`HistoryStore::read_columns`: the daemon's cache keeps its days as the bodies the parts are made of), each
        // part spliced from them with no row decoded or encoded; any other answer is encoded once.
        StoreRead::LoadBars { venue, symbol, interval, range } => {
            let kept =
                store.read_columns(StoreKindId::Bar, &venue, None, &symbol, Some(&interval), range);
            send_columns(out, &BAR_COLUMNS, BARS_PER_FRAME, kept, || {
                store.load_bars(&venue, &symbol, &interval, range)
            })
        }
        StoreRead::LoadBarsSourcedCapped { venue, source, symbol, interval, range, budget } => {
            let kept = match budget {
                None => store.read_columns(
                    StoreKindId::Bar,
                    &venue,
                    Some(&source),
                    &symbol,
                    Some(&interval),
                    range,
                ),
                Some(_) => None,
            };
            send_columns(out, &BAR_COLUMNS, BARS_PER_FRAME, kept, || {
                store.load_bars_sourced_capped(&venue, &source, &symbol, &interval, range, budget)
            })
        }
        StoreRead::ScanQuotes { venue, symbol, range } => {
            let kept = store.read_columns(StoreKindId::Quote, &venue, None, &symbol, None, range);
            send_columns(out, &QUOTE_COLUMNS, TICKS_PER_FRAME, kept, || {
                store.scan_quotes(&venue, &symbol, range)
            })
        }
        StoreRead::ScanTrades { venue, symbol, range } => {
            let kept = store.read_columns(StoreKindId::Trade, &venue, None, &symbol, None, range);
            send_columns(out, &TRADE_COLUMNS, TICKS_PER_FRAME, kept, || {
                store.scan_trades(&venue, &symbol, range)
            })
        }
        StoreRead::ScanBookUpdates { venue, symbol, range } => {
            let kept = store.read_columns(StoreKindId::Book, &venue, None, &symbol, None, range);
            send_columns(out, &BOOK_UPDATE_COLUMNS, TICKS_PER_FRAME, kept, || {
                store.scan_book_updates(&venue, &symbol, range)
            })
        }
        StoreRead::ScanDepth { venue, symbol, range } => {
            send_columns(out, &DEPTH_COLUMNS, TICKS_PER_FRAME, None, || {
                store.scan_depth(&venue, &symbol, range)
            })
        }
        StoreRead::ScanCohort { venue, asset, range } => {
            send_rows(out, store.scan_cohort(&venue, &asset, range))
        }
        StoreRead::ScanPerpMetrics { venue, symbol, range } => {
            send_rows(out, store.scan_perp_metrics(&venue, &symbol, range))
        }
        StoreRead::PropertiesAsOf { venue, symbol, timestamp: ts } => {
            send_value(out, store.properties_as_of(&venue, &symbol, ts))
        }
        StoreRead::ListSeries => send_rows(out, store.list_series()),
        StoreRead::Inventory => send_rows(out, store.inventory()),
        StoreRead::SeriesGaps(id) => send_rows(out, store.series_gaps(&id)),
        StoreRead::SeriesFacts(id) => send_value(out, store.series_facts(&id)),
        StoreRead::ScanChain { venue, underlying, range } => {
            send_rows(out, store.scan_chain(&venue, &underlying, range))
        }
        StoreRead::BarDays { venue, source, symbol, interval, range } => send_value(
            out,
            store.series_days(
                StoreKindId::Bar,
                &venue,
                source.as_deref(),
                &symbol,
                Some(&interval),
                range,
            ),
        ),
        StoreRead::KeptInstruments { venues } => {
            // The store's ENCODED form where it holds one (the datahub handle's stamped cache): the frame the typed
            // answer below would write, its rows' bytes copied in as they are held, no row decoded or encoded here.
            match venues.as_deref().and_then(|v| store.kept_instruments_and_splits_encoded_of(v)) {
                Some(Ok(kept)) => {
                    write_kept_frame(out, &kept)?;
                    return child_pipe::write_message(out, &ReadAnswerFrame::<()>::Done);
                }
                Some(Err(e)) => {
                    let kept: KeptAnswer = (Err(e.clone()), Err(e));
                    return send_value(out, Ok(kept));
                }
                None => {}
            }
            let kept: KeptAnswer = match &venues {
                Some(venues) => store.kept_instruments_and_splits_of(venues),
                None => store.kept_instruments_and_splits(),
            };
            send_value(out, Ok(kept))
        }
    }
}

/// A row answer in parts of at most [`ROWS_PER_FRAME`] rows, then `Done`.
pub(super) fn send_rows<W: Write, T: Serialize>(
    out: &mut W,
    answer: Result<Vec<T>, HistoryStoreError>,
) -> io::Result<()> {
    match answer {
        Ok(rows) => send_parts(
            out,
            rows.len(),
            ROWS_PER_FRAME,
            |at, end| &rows[at..end],
            |out, part| child_pipe::write_message(out, &ReadAnswerFrame::Rows(part)),
        ),
        Err(e) => child_pipe::write_message(out, &ReadAnswerFrame::<()>::Failed(e)),
    }
}

/// A BAR or TICK answer as its count (`ReadAnswerFrame::Count`, so the child sizes its rows once), then `magic`
/// columns parts of at most `per_part` rows (`super::wire::columns_part_into`: the datahub's own binary body of that
/// read, a frame in place of a `Rows`: the data relay's bulk frame, decision 0281's amendments of 2026-11-18 and -19),
/// then `Done`. The answer is the store's `kept` columns where it keeps them (`HistoryStore::read_columns`), else
/// `owned`'s rows encoded once (`crate::bar_cache::encoded`, a body at a time, each body's rows freed as it is made);
/// every part is spliced from its bodies, byte for byte the part its rows encode to, through ONE frame buffer, and the
/// child decodes a part while this side splices the next. A part over the frame cap (a book part of deep levels) is
/// halved before a byte of it is written; a bar part is a fixed-width column set, far under the cap, so none is.
pub(super) fn send_columns<W: Write, R: ColumnRow>(
    out: &mut W,
    magic: &ColumnsMagic,
    per_part: usize,
    kept: Option<Result<SharedColumns, HistoryStoreError>>,
    owned: impl FnOnce() -> Result<Vec<R>, HistoryStoreError>,
) -> io::Result<()> {
    let answer = kept.unwrap_or_else(|| owned().map(|rows| crate::bar_cache::encoded(magic, rows)));
    let columns = match answer {
        Ok(columns) => columns,
        Err(e) => {
            return child_pipe::write_message(out, &ReadAnswerFrame::<()>::Failed(e));
        }
    };
    child_pipe::write_message(out, &ReadAnswerFrame::<()>::Count(columns.len()))?;
    let mut frame = Vec::new();
    send_parts(
        out,
        columns.len(),
        per_part,
        |at, end| crate::bar_cache::runs_of(&columns, at, end),
        |out, runs| {
            columns_part_into::<R>(&mut frame, magic, &runs)?;
            out.write_all(&frame)
        },
    )
}

/// `len` rows in parts of at most `per_part` rows, each the rows `part(at, end)` and written by `write_part`, then
/// `Done`. A part over the frame cap is halved: `child_pipe::write_message` and `super::wire::columns_part_into`
/// refuse it before writing a byte, so nothing of it reached the child.
fn send_parts<W: Write, P>(
    out: &mut W,
    len: usize,
    per_part: usize,
    part: impl Fn(usize, usize) -> P,
    mut write_part: impl FnMut(&mut W, P) -> io::Result<()>,
) -> io::Result<()> {
    let (mut at, mut size) = (0, per_part);
    while at < len {
        let end = (at + size).min(len);
        match write_part(out, part(at, end)) {
            Ok(()) => at = end,
            Err(e) if e.kind() == io::ErrorKind::InvalidData && end - at > 1 => {
                size = (end - at) / 2;
            }
            Err(e) => return Err(e),
        }
    }
    child_pipe::write_message(out, &ReadAnswerFrame::<()>::Done)
}

/// A single-value answer: one part, NAMED (its types leave fields out before their last: `wire`'s module doc),
/// then `Done`.
fn send_value<V: Serialize>(
    out: &mut impl Write,
    answer: Result<V, HistoryStoreError>,
) -> io::Result<()> {
    match answer {
        Ok(value) => {
            child_pipe::write_message_named(out, &ReadAnswerFrame::Rows(&value))?;
            child_pipe::write_message(out, &ReadAnswerFrame::<()>::Done)
        }
        Err(e) => child_pipe::write_message(out, &ReadAnswerFrame::<()>::Failed(e)),
    }
}

/// The last [`REASON_TAIL_BYTES`] the child wrote to its reason pipe, read until it closes.
pub(crate) fn reason_tail(mut reason: impl Read) -> String {
    let (mut kept, mut buffer) = (Vec::new(), [0u8; 4096]);
    while let Ok(n @ 1..) = reason.read(&mut buffer) {
        kept.extend_from_slice(&buffer[..n]);
        if kept.len() > 2 * REASON_TAIL_BYTES {
            kept.drain(..kept.len() - REASON_TAIL_BYTES);
        }
    }
    kept.drain(..kept.len().saturating_sub(REASON_TAIL_BYTES));
    String::from_utf8_lossy(&kept).into_owned()
}

/// Kill the child the moment `gone` is set, until the relay is `done`.
fn watch(gone: &AtomicBool, done: &AtomicBool, child: &Mutex<Child>) {
    while !done.load(Ordering::Relaxed) {
        if gone.load(Ordering::Relaxed) {
            let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
            return;
        }
        thread::park_timeout(CANCEL_POLL);
    }
}

/// Reap the child: at once with a SIGKILL when `kill`, else after up to [`EXIT_GRACE`] for it to exit
/// by itself (it answered, or closed its channel) and a SIGKILL after that. Its exit, in words.
pub(crate) fn end(child: &Mutex<Child>, kill: bool) -> String {
    let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
    if !kill {
        // Counted sleeps, not a clock read: this crate reads no wall clock (`clock_pin`'s ratchet).
        for _ in 0..EXIT_GRACE.as_millis() / 10 {
            if let Ok(Some(status)) = child.try_wait() {
                return status.to_string();
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = child.kill();
    child.wait().map_or_else(|e| format!("not reaped: {e}"), |status| status.to_string())
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod relay_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "relay_reap_tests.rs"]
mod relay_reap_tests;
