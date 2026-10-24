//! The fetch family: `Fetch` and `FetchCredentialed`, whose reply is a PROGRESS / PAGE sequence ending
//! in exactly one terminal frame.
//!
//! # The shape of one fetch
//!
//! 1. **Refuse without asking the venue.** A lane the daemon does not serve is a positional
//!    `Response::Error` naming the feature string (decision 0112's server leg); a venue that lane does
//!    not serve is a terminal `FetchFailed { Refused }`, which is the in-process `SourceError::Refused`:
//!    the request cannot be expressed, retrying cannot help. A credentialed fetch is `Klines` for a
//!    `cd_venue=` venue or nothing.
//! 2. **An immediate `FetchProgress`**, before any venue is touched, so a fetch waiting on a venue
//!    budget is distinguishable from no daemon at all. Its `next_in_ms` is the cadence the daemon
//!    PROMISES: another frame within that long, and the client's idle bound is
//!    `max(floor, 3 x next_in_ms)` per frame (drift register R8: a remote fetch never times out sooner
//!    than the in-process one).
//! 3. **The lane runs on a named worker thread** (`gwh-fetch`) and returns the WHOLE answer. The
//!    workers are counted against one daemon-wide cap (`ServerLimits::max_fetches`), the slot taken
//!    before the thread is spawned and returned when the WORKER returns, so one that outlives its
//!    dropped connection (a venue call has no probe) still holds its place; a fetch past the cap is a
//!    transient positional `Error`.
//! 4. **The connection thread MONITORS**, and is the fetch's timer: every `next_in` of silence it writes
//!    a progress frame (a single venue call can block 30 s, so the cadence is a timer and not a
//!    property of the page loop), it polls the socket for a `FetchCancel` and for the client going
//!    away, and it watches the daemon's stop. A `FetchCancel` for this fetch, a dead link and a stop
//!    each raise the fetch's cancel flag (`FetchCtx::is_cancelled`); **a link drop cancels every fetch
//!    the connection started**, so no venue budget is spent for nobody.
//! 5. **The worker writes the answer**: pages from `FetchRows::into_pages` (each under
//!    `FETCH_PAGE_CEILING_BYTES`, so no reply is one frame; drift register R9), then `FetchDone` with the
//!    totals the client verifies its reassembly against, or `FetchFailed` with the failure's variant
//!    and words intact, or `FetchCancelled`. The pages are MADE as they are written, one at a time:
//!    the answer is already in memory whole (a lane returns it whole), and splitting it up front into
//!    a second set of `Vec`s held twice the answer at the peak.
//!
//! # Why the monitor stashes a frame instead of refusing it
//!
//! The client is strictly sequential, so the only frame it can legitimately send during a fetch is
//! `FetchCancel`. A frame that is anything else is a PIPELINED request: the monitor must read it (the
//! alternative, leaving it unread, would hide a following `FetchCancel` behind it), so it is kept on
//! `Conn::stash` and answered, in order, after this fetch's terminal frame. A peer that pipelines past
//! `MAX_STASH` frames is closed.
//!
//! # Writes
//!
//! Two threads write the socket during a fetch (the worker's pages and the monitor's progress), so
//! every write goes through one `Out` lock; each frame is one `write_all`, so frames never
//! interleave.
//!
//! ⚠ **Nothing is written after the terminal frame.** The monitor decides to write a progress frame
//! when the fetch has been silent for a cadence, and the worker's terminal write can land between that
//! decision and the monitor's write. The terminal write sets a flag under the same lock the progress
//! write takes, and a progress write that finds it set writes nothing: a stray `FetchProgress` after
//! `FetchDone` would be read by the client's NEXT fetch on the same connection as a desync.

use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{
    FEATURE_GW_FETCH_CREDENTIALED, FetchFailure, FetchProgress, FetchRows, FetchState, FetchWhat,
    GW_DOWN_FRAME_CEILING_BYTES, Request, Response,
};
use vike_market_stream::frame::write_capped;

use super::features::{
    fetch_gate, fetch_lane_armed, fetch_roster, lane_not_armed, not_served, served_features,
};
use super::frame::{Ready, read_client_body, wait_readable};
use super::shared::{After, Conn, Shared};
use super::stats::Slot;
use crate::rest::{FetchCtx, RestFacts};
use crate::session::ConnectionScope;

/// How often the monitor looks at the socket and the flags between progress frames.
const SLICE: Duration = Duration::from_millis(50);

/// The most pipelined frames the monitor keeps for after the fetch.
const MAX_STASH: usize = 16;

/// A refusal made before the venue is asked.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A positional `Response::Error`: the verb is not served at all.
    Error(String),
    /// A terminal `FetchFailed`: the lane is served but not for this request.
    Failed(FetchFailure),
}

/// The row kind a lane answers with.
fn expected_kind(what: &FetchWhat) -> &'static str {
    match what {
        FetchWhat::Klines { .. } => "Bars",
        FetchWhat::Funding { .. } => "Funding",
        FetchWhat::Ticks { .. } => "Quotes",
        FetchWhat::CatalogList { .. }
        | FetchWhat::CatalogSearch { .. }
        | FetchWhat::OptionIndex { .. } => "Instruments",
    }
}

/// Whether the daemon must refuse `what` before it reaches the lane.
pub fn pre_check(rest: &RestFacts, what: &FetchWhat, credentialed: bool) -> Option<Refusal> {
    if credentialed && !matches!(what, FetchWhat::Klines { .. }) {
        return Some(Refusal::Error(format!(
            "gatewayhub: FetchCredentialed serves {FEATURE_GW_FETCH_CREDENTIALED:?} klines only (a \
             cd_venue= venue); a {} fetch is not credentialed; nothing was asked of any venue",
            what.lane()
        )));
    }
    if !fetch_lane_armed(rest, what, credentialed) {
        let verb = if credentialed {
            format!("a credentialed {} fetch", what.lane())
        } else {
            format!("a {} fetch", what.lane())
        };
        let gate = fetch_gate(what, credentialed);
        let advertised = served_features(&[], rest).iter().any(|f| f == gate);
        let text = if advertised { lane_not_armed(&verb, gate) } else { not_served(&verb, gate) };
        return Some(Refusal::Error(text));
    }
    if let Some((venue, roster)) = fetch_roster(rest, what, credentialed)
        && !roster.iter().any(|v| v == venue)
    {
        return Some(Refusal::Failed(FetchFailure::Refused(format!(
            "gatewayhub: `{venue}` is not served by the {}{} lane (served: [{}]); nothing was asked \
             of any venue",
            if credentialed { "credentialed " } else { "" },
            what.lane(),
            roster.join(", ")
        ))));
    }
    None
}

/// The socket as the fetch's two writers share it.
struct Out {
    rid: u64,
    started: Instant,
    every: Duration,
    inner: Mutex<OutInner>,
}

struct OutInner {
    stream: TcpStream,
    last_write: Instant,
    pages: u32,
    rows: u64,
    /// Whether the fetch's terminal frame has been written. Set, and read, only under the lock.
    terminal: bool,
}

impl Out {
    fn lock(&self) -> std::sync::MutexGuard<'_, OutInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Write the fetch's ONE terminal frame (`FetchDone`, `FetchFailed` or `FetchCancelled`) and close
    /// the door behind it: no progress frame is written after this starts.
    fn terminal(&self, response: &Response) -> io::Result<()> {
        let mut g = self.lock();
        g.terminal = true;
        write_capped(&mut g.stream, response, GW_DOWN_FRAME_CEILING_BYTES)?;
        g.last_write = Instant::now();
        Ok(())
    }

    fn since_last_write(&self) -> Duration {
        self.lock().last_write.elapsed()
    }

    /// A progress frame, unless the terminal frame is already out (then nothing is written and the
    /// answer is `Ok`: there is no fault). The check is under the lock the terminal write holds, so
    /// the monitor's earlier look at the silence cannot outlive the worker's last word.
    fn progress(&self, state: FetchState) -> io::Result<()> {
        let mut g = self.lock();
        if g.terminal {
            return Ok(());
        }
        let frame = Response::FetchProgress(FetchProgress {
            rid: self.rid,
            state,
            pages: g.pages,
            rows: g.rows,
            elapsed_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            next_in_ms: u64::try_from(self.every.as_millis()).unwrap_or(u64::MAX),
        });
        write_capped(&mut g.stream, &frame, GW_DOWN_FRAME_CEILING_BYTES)?;
        g.last_write = Instant::now();
        Ok(())
    }

    /// Write one page and count it.
    fn page(&self, page: u32, rows: FetchRows) -> io::Result<()> {
        let n = rows.len() as u64;
        let mut g = self.lock();
        let frame = Response::FetchPage { rid: self.rid, page, rows };
        write_capped(&mut g.stream, &frame, GW_DOWN_FRAME_CEILING_BYTES)?;
        g.last_write = Instant::now();
        g.pages += 1;
        g.rows += n;
        Ok(())
    }

    fn totals(&self) -> (u32, u64) {
        let g = self.lock();
        (g.pages, g.rows)
    }
}

/// Marks the worker finished however it ends, a panic included, so the monitor never waits for a
/// thread that is gone.
struct DoneGuard(Arc<AtomicBool>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The worker: run the lane, then write the answer. `_running` is the worker's place under the
/// daemon-wide cap, taken by [`serve`] BEFORE the thread exists and given back only when this function
/// returns, so a worker that outlives a dropped connection (a venue call has no probe) still counts.
fn run_worker(
    shared: &Shared,
    memo: &ConnectionScope,
    what: &FetchWhat,
    credentialed: bool,
    ctx: &FetchCtx,
    out: &Out,
    _running: Slot,
) {
    let result = shared.lanes.rest.fetch(memo, what, credentialed, ctx);
    let rid = out.rid;
    if ctx.is_cancelled() {
        let _ = out.terminal(&Response::FetchCancelled { rid });
        return;
    }
    let rows = match result {
        Ok(rows) => rows,
        Err(failure) => {
            let _ = out.terminal(&Response::FetchFailed { rid, failure });
            return;
        }
    };
    if rows.kind() != expected_kind(what) {
        let failure = FetchFailure::Fetch(format!(
            "gatewayhub: the {} lane answered with {} rows; nothing was sent",
            what.lane(),
            rows.kind()
        ));
        let _ = out.terminal(&Response::FetchFailed { rid, failure });
        return;
    }
    // LAZY: each page is made as the loop pulls it, so the answer is never held twice.
    let pages = rows.into_pages(shared.config.limits.fetch_page_ceiling);
    for (index, page) in pages.enumerate() {
        if ctx.is_cancelled() {
            let _ = out.terminal(&Response::FetchCancelled { rid });
            return;
        }
        let number = u32::try_from(index).unwrap_or(u32::MAX);
        match out.page(number, page) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                // A row larger than the frame ceiling: refused before its first byte, so the stream is
                // still in step and the client is told, in words, rather than left to time out.
                let failure = FetchFailure::Fetch(format!(
                    "gatewayhub: a page of the {} answer cannot be framed ({e}); nothing further was \
                     sent",
                    what.lane()
                ));
                let _ = out.terminal(&Response::FetchFailed { rid, failure });
                return;
            }
            Err(_) => {
                // The peer is gone; the monitor sees it too.
                ctx.cancel();
                return;
            }
        }
    }
    let (pages, rows) = out.totals();
    let _ = out.terminal(&Response::FetchDone { rid, pages, rows });
}

/// Wait for the worker: at once when it is done, up to `bound` otherwise. `false` when it panicked or
/// was left behind (a thread blocked in a venue call outlives the wait; it ends when the call returns).
fn reap(handle: JoinHandle<()>, done: &AtomicBool, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while !done.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if done.load(Ordering::Acquire) {
        handle.join().is_ok()
    } else {
        tracing::warn!(
            "vike-gatewayhub fetch: a worker is still inside its venue call; left to finish"
        );
        false
    }
}

/// `Fetch` / `FetchCredentialed`: refuse, or run the fetch to its terminal frame.
pub fn serve(conn: &mut Conn, rid: u64, what: FetchWhat, credentialed: bool) -> After {
    let shared = Arc::clone(&conn.shared);
    match pre_check(&shared.rest, &what, credentialed) {
        Some(Refusal::Error(text)) => return reply(conn, &Response::Error(text)),
        Some(Refusal::Failed(failure)) => {
            return reply(conn, &Response::FetchFailed { rid, failure });
        }
        None => {}
    }
    // The daemon-wide cap on fetch WORKERS, taken before anything is spawned and handed to the worker,
    // which gives it back when its venue call returns, not when its connection goes: a client that
    // starts a long fetch and drops, repeatedly, finds the cap and not an ever-longer list of threads
    // inside venue calls. A refused fetch is a positional, transient `Error` (the client reads it as a
    // retryable fetch failure) and asks no venue.
    let cap = shared.config.limits.max_fetches;
    let Some(slot) = shared.stats.fetches_running.try_enter(cap) else {
        return reply(
            conn,
            &Response::Error(format!(
                "gatewayhub: already running {cap} fetches (the daemon's cap); transient, retried \
                 on the caller's next pass; nothing was asked of any venue"
            )),
        );
    };
    let writer = match conn.stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            return reply(
                conn,
                &Response::Error(format!("gatewayhub: could not start the fetch: {e}")),
            );
        }
    };
    let every = shared.config.timings.fetch_progress_every;
    let out = Arc::new(Out {
        rid,
        started: Instant::now(),
        every,
        inner: Mutex::new(OutInner {
            stream: writer,
            last_write: Instant::now(),
            pages: 0,
            rows: 0,
            terminal: false,
        }),
    });
    let ctx = FetchCtx::new();
    // The immediate progress frame: accepted, before any venue is touched.
    if out.progress(ctx.state()).is_err() {
        return After::Close;
    }

    let done = Arc::new(AtomicBool::new(false));
    let handle = {
        let shared = Arc::clone(&shared);
        let memo = Arc::clone(&conn.memo);
        let out = Arc::clone(&out);
        let ctx = ctx.clone();
        let done = Arc::clone(&done);
        let what = what.clone();
        std::thread::Builder::new().name("gwh-fetch".into()).spawn(move || {
            let _done = DoneGuard(done);
            run_worker(&shared, &memo, &what, credentialed, &ctx, &out, slot);
        })
    };
    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            let failure =
                FetchFailure::Fetch(format!("gatewayhub: could not start the fetch: {e}"));
            return reply(conn, &Response::FetchFailed { rid, failure });
        }
    };

    let frame_timeout = shared.config.timings.frame_read_timeout;
    let mut link_gone = false;
    loop {
        if done.load(Ordering::Acquire) {
            break;
        }
        if conn.ctl.is_stopping() {
            ctx.cancel();
        }
        let silent = out.since_last_write();
        if silent >= every {
            if out.progress(ctx.state()).is_err() {
                ctx.cancel();
                link_gone = true;
                break;
            }
            continue;
        }
        let wait = (every - silent).min(SLICE);
        match wait_readable(&conn.stream, wait) {
            Ok(Ready::Timeout) => {}
            Ok(Ready::Data) => match read_client_body(&mut conn.stream, frame_timeout) {
                Ok(body) => match serde_json::from_slice::<Request>(&body) {
                    Ok(Request::FetchCancel { rid: cancelled }) => {
                        if cancelled == rid {
                            ctx.cancel();
                        }
                    }
                    _ if conn.stash.len() < MAX_STASH => conn.stash.push_back(body),
                    _ => {
                        tracing::warn!(
                            peer = ?conn.peer,
                            "vike-gatewayhub fetch: the peer pipelined past the stash; closing"
                        );
                        ctx.cancel();
                        link_gone = true;
                        break;
                    }
                },
                Err(_) => {
                    ctx.cancel();
                    link_gone = true;
                    break;
                }
            },
            Ok(Ready::Eof) | Err(_) => {
                // A link drop cancels every fetch the connection started.
                ctx.cancel();
                link_gone = true;
                break;
            }
        }
    }
    let bound = if link_gone || conn.ctl.is_stopping() {
        shared.config.timings.stop_drain
    } else {
        // `done` is set: the join is immediate.
        Duration::from_secs(1)
    };
    let clean = reap(handle, &done, bound);
    if link_gone || !clean || conn.ctl.is_stopping() { After::Close } else { After::Continue }
}

fn reply(conn: &mut Conn, response: &Response) -> After {
    match conn.reply(response) {
        Ok(()) => After::Continue,
        Err(_) => After::Close,
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod fetch_tests;
