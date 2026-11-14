//! The PARENT of a sandboxed backtest: spawn the child, hand it the request, answer its history reads
//! from this daemon's own store, and take its answer. A cancel is a SIGKILL of the child.
//!
//! The child is started by `vike_sandbox::spawn_strategy_process` (an empty environment, working
//! directory `/`, three pipes): its stdin and stdout are the protocol, its stderr the reason this side
//! drains into a bounded tail (and where a strategy's own `println!` lands: the child points its fd 1 at
//! its stderr before it loads the file, `crates/vike-strategy-sandbox/src/lib.rs`'s module doc).

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use vike_backtest::harness::StrategySource;
use vike_backtest::harness::paramscan::threads::paramscan_threads;
use vike_backtesthub_client::proto::{Request, Response};
use vike_data::{HistoryStore, HistoryStoreError};

use vike_marketdata::Bar;

use super::wire::{
    BARS_PER_FRAME, BarColumns, ChildFrame, ChildRequest, ROWS_PER_FRAME, ReadAnswerFrame,
    StoreRead, frame_into, read_message, write_message,
};

/// How much of the child's reason pipe a failure quotes: its LAST bytes.
const REASON_TAIL_BYTES: usize = 4096;

/// How often a watched run looks at its client's peer-gone flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// How long a child that answered (or closed its channel) gets to exit before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(1);

/// How a sandboxed run ended.
pub(super) enum Outcome {
    /// The child's answer, the `Response` the in-process daemon would have written.
    Answer(Box<Response>),
    /// The client left, and the child was killed.
    Cancelled,
    /// The child could not start, died, or broke the protocol: the reason, its exit and its last words.
    Failed(String),
}

/// Run `request` in a child of `program` (`<program> strategy-sandbox backtest <builds> <sha256>`) that
/// loads the artifact `source` was built into, its history read from `store`. `client_gone` set kills
/// the child.
pub(super) fn run(
    program: &Path,
    request: &Request,
    builds: &Path,
    source: &StrategySource,
    store: &(dyn HistoryStore + Send + Sync),
    client_gone: Option<&AtomicBool>,
) -> Outcome {
    let sha = Path::new(&source.sha256);
    let args = [Path::new("strategy-sandbox"), Path::new("backtest"), builds, sha];
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
    let child = Mutex::new(child);
    let done = AtomicBool::new(false);
    let (child_ref, done_ref) = (&child, &done);
    thread::scope(|scope| {
        let tail = scope.spawn(move || reason_tail(reason));
        let watcher = client_gone.map(|gone| scope.spawn(move || watch(gone, done_ref, child_ref)));
        let relayed = relay(&mut to_child, &mut from_child, request, &source.text, store);
        // Closed before the reap, so a child still reading sees end-of-file rather than a wait.
        drop(to_child);
        done.store(true, Ordering::Relaxed);
        if let Some(watcher) = &watcher {
            watcher.thread().unpark();
        }
        let status = end(&child, relayed.as_ref().is_err_and(|e| e.kill));
        let tail = tail.join().unwrap_or_default();
        match relayed {
            Ok(response) => Outcome::Answer(Box::new(response)),
            Err(_) if client_gone.is_some_and(|g| g.load(Ordering::Relaxed)) => Outcome::Cancelled,
            Err(e) => {
                let tail = if tail.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", tail.trim())
                };
                Outcome::Failed(format!("{} ({status}){tail}", e.why))
            }
        }
    })
}

/// Why the relay stopped without an answer, and whether the child must be killed (it is still running
/// and broke the protocol) rather than given [`EXIT_GRACE`].
struct Stopped {
    why: String,
    kill: bool,
}

/// The protocol, parent side: the request down, then every read answered until the answer comes up.
fn relay(
    to_child: &mut ChildStdin,
    from_child: &mut ChildStdout,
    request: &Request,
    text: &str,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<Response, Stopped> {
    let ended = |e: io::Error| Stopped {
        why: format!("the strategy sandbox ended without an answer ({e})"),
        kill: false,
    };
    let kept = super::kept_rows_of(request, store).map_err(|e| (&e).into());
    let first = ChildRequest { request, paramscan_threads: paramscan_threads(), text, kept };
    write_message(to_child, &first).map_err(ended)?;
    loop {
        match read_message::<_, ChildFrame>(from_child) {
            Ok(ChildFrame::Read(read)) => {
                // A write fault means the child is gone; the next read says so, with its exit.
                let _ = answer(to_child, store, read);
            }
            Ok(ChildFrame::Answer(response)) => return Ok(*response),
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

/// Answer one history read from `store`, the store's own answer variant for variant.
pub(super) fn answer(
    out: &mut impl Write,
    store: &(dyn HistoryStore + Send + Sync),
    read: StoreRead,
) -> io::Result<()> {
    match read {
        StoreRead::LoadBars { venue, symbol, interval, range } => {
            send_bars(out, store.load_bars(&venue, &symbol, &interval, range.into()))
        }
        StoreRead::LoadBarsSourcedCapped { venue, source, symbol, interval, range, budget } => {
            send_bars(
                out,
                store.load_bars_sourced_capped(
                    &venue,
                    &source,
                    &symbol,
                    &interval,
                    range.into(),
                    budget,
                ),
            )
        }
        StoreRead::ScanQuotes { venue, symbol, range } => {
            send_rows(out, store.scan_quotes(&venue, &symbol, range.into()))
        }
        StoreRead::ScanTrades { venue, symbol, range } => {
            send_rows(out, store.scan_trades(&venue, &symbol, range.into()))
        }
        StoreRead::ScanBookUpdates { venue, symbol, range } => {
            send_rows(out, store.scan_book_updates(&venue, &symbol, range.into()))
        }
        StoreRead::ScanDepth { venue, symbol, range } => {
            send_rows(out, store.scan_depth(&venue, &symbol, range.into()))
        }
        StoreRead::ScanCohort { venue, asset, range } => {
            send_rows(out, store.scan_cohort(&venue, &asset, range.into()))
        }
        StoreRead::ScanPerpMetrics { venue, symbol, range } => {
            send_rows(out, store.scan_perp_metrics(&venue, &symbol, range.into()))
        }
        StoreRead::PropertiesAsOf { venue, symbol, timestamp: ts } => {
            send_value(out, store.properties_as_of(&venue, &symbol, ts))
        }
        StoreRead::ListSeries => send_rows(out, store.list_series()),
        StoreRead::Inventory => send_rows(out, store.inventory()),
        StoreRead::SeriesGaps(id) => send_rows(out, store.series_gaps(&id)),
        StoreRead::SeriesFacts(id) => send_value(out, store.series_facts(&id)),
    }
}

/// A row answer in parts of at most [`ROWS_PER_FRAME`] rows, then `Done`.
pub(super) fn send_rows<W: Write, T: Serialize>(
    out: &mut W,
    answer: Result<Vec<T>, HistoryStoreError>,
) -> io::Result<()> {
    send_parts(out, answer, ROWS_PER_FRAME, |out, part| {
        write_message(out, &ReadAnswerFrame::Rows(part))
    })
}

/// A BAR answer as [`BarColumns`] parts of at most [`BARS_PER_FRAME`] bars, then `Done`: the data relay's
/// bulk frame (MEASURED: per-row named msgpack was most of a sandboxed run's data cost). Every part is
/// encoded through ONE `BarColumns` and ONE frame buffer, so after the first part this side touches no
/// fresh memory, and the child decodes a part while this side encodes the next; the rows are freed on a
/// thread of their own, which the run does not wait for. A part is a fixed-width column set, far under
/// the frame cap, so none is ever halved.
pub(super) fn send_bars<W: Write>(
    out: &mut W,
    answer: Result<Vec<Bar>, HistoryStoreError>,
) -> io::Result<()> {
    let rows = match answer {
        Ok(rows) => rows,
        Err(e) => return write_message(out, &ReadAnswerFrame::<()>::Failed((&e).into())),
    };
    let (mut columns, mut frame) = (BarColumns::default(), Vec::new());
    for part in rows.chunks(BARS_PER_FRAME) {
        columns.fill(part);
        frame_into(&mut frame, &ReadAnswerFrame::Rows(&columns))?;
        out.write_all(&frame)?;
    }
    // A million bars' frees are not the run's: off this thread (a fire-and-forget drop). A thread that
    // cannot start hands its closure back to be dropped here, so the rows are freed either way.
    let _ = thread::Builder::new().name("relay-free".into()).spawn(move || drop(rows));
    write_message(out, &ReadAnswerFrame::<()>::Done)
}

/// `answer` in parts of at most `per_part` rows, each written by `write_part`, then `Done`; a store's
/// refusal as one `Failed`. A part over the frame cap is halved: `write_message` refuses it before writing
/// a byte, so nothing of it reached the child.
fn send_parts<W: Write, T>(
    out: &mut W,
    answer: Result<Vec<T>, HistoryStoreError>,
    per_part: usize,
    write_part: impl Fn(&mut W, &[T]) -> io::Result<()>,
) -> io::Result<()> {
    let rows = match answer {
        Ok(rows) => rows,
        Err(e) => return write_message(out, &ReadAnswerFrame::<()>::Failed((&e).into())),
    };
    let (mut at, mut size) = (0, per_part);
    while at < rows.len() {
        let end = (at + size).min(rows.len());
        match write_part(out, &rows[at..end]) {
            Ok(()) => at = end,
            Err(e) if e.kind() == io::ErrorKind::InvalidData && end - at > 1 => {
                size = (end - at) / 2;
            }
            Err(e) => return Err(e),
        }
    }
    write_message(out, &ReadAnswerFrame::<()>::Done)
}

/// A single-value answer: one part, then `Done`.
fn send_value<V: Serialize>(
    out: &mut impl Write,
    answer: Result<V, HistoryStoreError>,
) -> io::Result<()> {
    match answer {
        Ok(value) => {
            write_message(out, &ReadAnswerFrame::Rows(&value))?;
            write_message(out, &ReadAnswerFrame::<()>::Done)
        }
        Err(e) => write_message(out, &ReadAnswerFrame::<()>::Failed((&e).into())),
    }
}

/// The last [`REASON_TAIL_BYTES`] the child wrote to its reason pipe, read until it closes.
fn reason_tail(mut reason: impl Read) -> String {
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
fn end(child: &Mutex<Child>, kill: bool) -> String {
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
