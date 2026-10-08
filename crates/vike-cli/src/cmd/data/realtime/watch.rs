//! `data realtime watch` — subscribe to one key, stream its frames to the bound, report the end.

use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use vike_datahub_client::market::{
    MD_DEPTH_LEVELS_CEILING, MD_READ_TIMEOUT, MdFrame, MdLane, MdRefusal, MdSpec,
};
use vike_datahub_client::{MdBye, proto::Response, read_frame};
use vike_node_proto::auth::{NodeKeys, Scope};

use super::grammar::default_render;
use super::render::{bye_sentence, jsonl_row, table_line};
use super::status::execute_status;
use super::{Args, Bound, Render, Verb, connect};
use crate::exit::{CliError, CmdResult};

/// Route the parsed line. The two arms share the dial and nothing else.
pub(super) fn execute(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    match args.verb {
        Verb::Watch => execute_watch(args, keys),
        Verb::Status => execute_status(args, keys),
    }
}

// ─── `watch` ─────────────────────────────────────────────────────────────────────────────────────

/// `data realtime watch` — subscribe, stream until the bound, report what happened.
fn execute_watch(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let key = args.key.as_ref().expect("parse guarantees a key on `watch`");
    let lane = args.lane.expect("parse guarantees a lane on `watch`");
    let asked = MdSpec {
        venue: key.venue.clone(),
        symbol: key.symbol.clone(),
        lane,
        depth_levels: args.depth,
    };

    let client = connect(&args.addr, keys, Scope::Read)?;
    // ⚠ The client is CONSUMED on success and handed back INTACT on failure — leg (3) of
    // `FEATURE_MARKET_DATA`'s contract, which is why the error arm carries it. This verb has nothing
    // further to ask a server that said no, so it drops it; the message is the server's own (or the
    // client's local capability refusal, which names the key an operator has to set there).
    let (info, stream) = client.md_subscribe(vec![asked.clone()]).map_err(|(_client, msg)| {
        CliError::failed(format!("the subscription was not opened: {msg}"))
    })?;

    // ⚠ Matched on `MdSpec::key`, which EXCLUDES `depth_levels` by construction — its own doc says
    // so, and it exists precisely so no consumer re-derives the tuple and forgets the exclusion.
    // Comparing whole specs here would fail to find our own subscription the moment a depth was
    // clamped, which is the one case this code is about.
    if let Some((_, why)) = info.refused.iter().find(|(spec, _)| spec.key() == asked.key()) {
        return Err(CliError::failed(refusal_sentence(&asked, why)));
    }
    let Some(accepted) = info.accepted.iter().find(|spec| spec.key() == asked.key()) else {
        return Err(CliError::failed(format!(
            "the server neither accepted nor refused {}:{} on the {} lane — it answered about \
             neither, which is a protocol desync rather than a `no`. Nothing was streamed",
            asked.venue,
            asked.symbol,
            lane.feed_stream_label()
        )));
    };

    // ⚠ EVERY line from here to the summary goes to STDERR: stdout is the stream. See the module
    // doc — a `| jq` must read frames and a `--out` file must hold nothing else.
    for note in subscribe_notes(&asked, accepted, info.heartbeat_ms) {
        eprintln!("{note}");
    }

    let render = args.render.unwrap_or_else(|| {
        default_render(args.verb, args.out.is_some(), io::stdout().is_terminal())
    });
    let mut sink = Sink::open(args.out.as_deref())?;
    let (end, tally) = stream_frames(stream, args.bound, render, &mut sink);
    // ⚠ The summary is emitted BEFORE the close, so a failing flush reports itself INSTEAD of the
    // rung and never instead of the counts: how much arrived is the one thing an operator cannot
    // re-derive from a truncated file.
    for line in summary_lines(&asked, &end, &tally) {
        eprintln!("{line}");
    }
    sink.finish()?;
    if end.was_asked_for(args.bound) {
        return Ok(());
    }
    // A wrapper must be able to tell a finished capture from a broken one, and the exit code is the
    // only channel a pipeline reads — so the two ways of not getting what was asked for are NAMED
    // apart here rather than sharing one sentence. A FAULT is the one that also reaches this line
    // under `--unbounded`, where there is no bound to have missed.
    Err(CliError::failed(if end.is_fault() {
        format!("the stream FAILED: {}", end.sentence())
    } else {
        format!("the stream ended before the bound was reached: {}", end.sentence())
    }))
}

/// What the server said about the subscription, as lines an operator reads BEFORE the first frame.
///
/// ⚠ The depth line is the one that matters and [`MdSpec::depth_levels`]' own doc is why: *"anything
/// above `MD_DEPTH_LEVELS_CEILING` is CLAMPED, and the clamped number comes back in
/// `MdSubscribed.accepted` — a clamp is an acceptance with a smaller number, never a refusal, so a
/// client that asked for 200 LEARNS it got 50."* Rendering the number the operator TYPED is exactly
/// the failure that sentence exists to prevent.
fn subscribe_notes(asked: &MdSpec, accepted: &MdSpec, heartbeat_ms: u64) -> Vec<String> {
    let mut notes = vec![format!(
        "watching {} {} on the {} lane",
        accepted.venue,
        accepted.symbol,
        accepted.lane.feed_stream_label()
    )];
    if let Some(note) = depth_note(asked, accepted) {
        notes.push(note);
    }
    notes.push(format!(
        "the server heartbeats every {heartbeat_ms}ms when it has nothing to say, so silence longer \
         than that is a fault rather than a quiet market"
    ));
    notes
}

/// The depth disclosure, or `None` on a lane that has no levels.
///
/// The comparison is between the RAW request and the SERVER's resolved answer, deliberately: a
/// client-side `resolved_depth()` of the request would already have clamped 5000 to 200, so a note
/// built from it would tell the operator they asked for a number they did not type.
pub(super) fn depth_note(asked: &MdSpec, accepted: &MdSpec) -> Option<String> {
    if accepted.lane == MdLane::Trades {
        return None;
    }
    let served = accepted.resolved_depth();
    Some(match asked.depth_levels {
        Some(n) if n != served => format!(
            "⚠ the depth was CLAMPED: you asked for {n} levels a side and this server serves \
             {served}. That is an ACCEPTANCE with a smaller number, not a refusal — every frame \
             below carries {served}"
        ),
        Some(n) => format!("depth: {n} levels a side, as asked"),
        None => format!(
            "depth: {served} levels a side (the wire's default — `--depth N` asks for more, up to \
             {MD_DEPTH_LEVELS_CEILING})"
        ),
    })
}

/// One refused spec, as a sentence.
///
/// ⚠ The `String` each variant carries is the FAR SIDE's own text
/// (`vike_data::require_live_verb`'s words for a lane, [`validate_md_symbol`]'s for a symbol) and is
/// forwarded verbatim rather than re-worded, for the reason those variants state: one refusal, one
/// wording, wherever it is reached from.
///
/// The match has no `_` arm, so a new [`MdRefusal`] cannot inherit a sentence written for a
/// different one.
pub(super) fn refusal_sentence(asked: &MdSpec, why: &MdRefusal) -> String {
    let what =
        format!("{}:{} on the {} lane", asked.venue, asked.symbol, asked.lane.feed_stream_label());
    let detail = match why {
        MdRefusal::UnknownVenue => format!(
            "`{}` is not a venue this workspace knows at all. `data catalog venues` is the roster",
            asked.venue
        ),
        MdRefusal::VenueNotServed(served) => format!(
            "this server's build links no market-data client for `{}`. It serves: {served}. \
             `data realtime status` is the same list, asked before you type",
            asked.venue
        ),
        MdRefusal::LaneUnsupported(msg) => msg.clone(),
        MdRefusal::SymbolRejected(msg) => msg.clone(),
        MdRefusal::KeyCapTotal { held, cap } => format!(
            "the server's process-wide key budget is full ({held} of {cap} held). Nothing about \
             this spec is wrong"
        ),
        MdRefusal::KeyCapVenue { venue, held, cap } => format!(
            "the server's key budget for `{venue}` is full ({held} of {cap} held) — that cap is \
             what protects the order-signing daemon's share of this box's venue budget"
        ),
        MdRefusal::SpecCapSession { held, cap } => {
            format!("this session already holds its maximum of {held}/{cap} specs")
        }
    };
    // ⚠ `is_permanent` is the WIRE's own classification, not a reading of the text — it is what a
    // retrying client uses to decide whether to keep a spec in its desired set, and an operator
    // deserves the same answer rather than having to infer it.
    let again = if why.is_permanent() {
        "Retrying cannot change this answer on this server."
    } else {
        "This is a CAP and can free up — the same line may work later."
    };
    format!("{what} was REFUSED: {detail}. {again}")
}

// ─── the stream ──────────────────────────────────────────────────────────────────────────────────

/// Why the stream stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum End {
    /// `--events N` was reached.
    Events(usize),
    /// `--for D` elapsed.
    Elapsed,
    /// The server sent [`MdFrame::Bye`] and closed.
    Bye(MdBye),
    /// The read deadline the SERVER's own heartbeat period armed expired with no frame. The link is
    /// dead — `vike_app_core::data::md_session`'s reader draws the same conclusion from the same signal.
    Silent,
    /// A clean end-of-stream: the socket closed without a `Bye`.
    Closed,
    /// The socket carried something that is not an `Md` frame, which after `MdSubscribed` is a
    /// protocol desync by the wire's own §0 invariant.
    Desync(String),
    /// The transport failed.
    Fault(String),
    /// The READER went away — a `| head -3` that got what it wanted, or a closed pager. Normal, and
    /// deliberately not a failure under any bound.
    ReaderGone,
}

impl End {
    /// Did something BREAK, as opposed to the stream simply ending?
    ///
    /// ⚠ **This is the seam the exit code rests on, and it is separate from [`End::was_asked_for`]
    /// because the two questions have different scopes**: an early ENDING is a failure only against
    /// a bound that was missed, while a FAULT is a failure under every bound there is. A dead link
    /// ([`End::Silent`] — silence past the deadline the server's OWN heartbeat period armed), a
    /// transport error and a protocol desync are all things that went wrong with the machinery
    /// rather than answers about the market.
    ///
    /// [`End::Closed`] is deliberately NOT one of them: a socket that reaches EOF without a `Bye`
    /// is the far side hanging up, which is rude rather than broken, and a stream that delivered
    /// everything it was going to deliver has not failed.
    pub(super) fn is_fault(&self) -> bool {
        match self {
            End::Silent | End::Desync(_) | End::Fault(_) => true,
            End::Events(_) | End::Elapsed | End::ReaderGone | End::Bye(_) | End::Closed => false,
        }
    }

    /// Did the stream end the way the operator asked it to?
    ///
    /// Under a bound, only that bound counts: an early `Bye` or a hung-up socket means they did not
    /// get the 30 seconds or the 500 frames they named. Under `--unbounded` there is no bound to
    /// miss, so every ENDING is the end — and a closed reader is always a success, because
    /// `| head -3` is a legitimate way to use a stream.
    ///
    /// ⚠ **A FAULT is never asked for, `--unbounded` included, and this returned `true` for one
    /// until now.** `Bound::Unbounded` folded all five non-bound ends together, so a transport
    /// failure and a protocol desync exited 0 — rendered, in the one channel a pipeline reads,
    /// identically to a clean stop. A wrapper running
    /// `vike-cli data realtime watch … --unbounded --out tape.jsonl` under `set -e` saw a success
    /// and took a half-written tape for a complete one. [`End::is_fault`] is the split, and it is
    /// consulted FIRST: what the bound decides is only whether a clean ENDING was the one asked
    /// for.
    pub(super) fn was_asked_for(&self, bound: Bound) -> bool {
        if self.is_fault() {
            return false;
        }
        match self {
            End::Events(_) | End::Elapsed | End::ReaderGone => true,
            End::Bye(_) | End::Closed => bound == Bound::Unbounded,
            // Unreachable — `is_fault` answered above — and spelled out rather than left to a `_`
            // so a new variant has to classify itself in BOTH functions.
            End::Silent | End::Desync(_) | End::Fault(_) => false,
        }
    }

    /// The reason, in an operator's words.
    pub(super) fn sentence(&self) -> String {
        match self {
            End::Events(n) => format!("the --events bound was reached ({n} data frames)"),
            End::Elapsed => "the --for bound elapsed".to_string(),
            End::Bye(why) => format!("the server ended the stream: {}", bye_sentence(*why)),
            End::Silent => "the link went silent past the server's own heartbeat deadline — that \
                            is a dead link, not a quiet market"
                .to_string(),
            End::Closed => "the server closed the socket without saying why".to_string(),
            End::Desync(what) => format!("protocol desync: {what}"),
            End::Fault(e) => format!("the transport failed: {e}"),
            End::ReaderGone => "the reader closed the pipe".to_string(),
        }
    }
}

/// What the stream carried, counted by CLASS — because "142 frames" answers nothing an operator
/// asked. A quiet key that heartbeated 4 times and a busy one that delivered 138 books are the same
/// number under one counter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Tally {
    /// DATA frames — depth, book, trades. The unit `--events` counts.
    pub(super) events: usize,
    /// Stream-status disclosures.
    pub(super) statuses: usize,
    /// Tape-gap markers.
    pub(super) gaps: usize,
    /// Heartbeats.
    pub(super) heartbeats: usize,
    /// Prints the server told us were LOST, summed across every marker.
    pub(super) dropped: u64,
}

/// Read frames until the bound, a fault or a goodbye, rendering each one.
///
/// ⚠ **The read budget is `min(what is left of --for, the deadline the socket already carries)`, and
/// the second half is READ BACK OFF THE SOCKET rather than recomputed.**
/// `DatahubClient::md_subscribe` arms `max(MD_READ_TIMEOUT, 3 × heartbeat_ms)` — the whole reason the
/// server SENDS its heartbeat period — and a second copy of that expression here could disagree with
/// the deadline actually armed. Without the `--for` half a `--for 5s` would sit in a 45-second read
/// and return forty seconds late; without the deadline half a `--for 2h` would never notice a dead
/// link. Which of the two fired is then the difference between [`End::Elapsed`] and [`End::Silent`],
/// and it is answered by the clock rather than by the error.
pub(super) fn stream_frames(
    mut stream: TcpStream,
    bound: Bound,
    render: Render,
    sink: &mut Sink,
) -> (End, Tally) {
    let started = Instant::now();
    let mut tally = Tally::default();
    // A socket with no deadline armed is not a shape `md_subscribe` produces; falling back to the
    // wire's own floor keeps this total rather than asserting about a value we did not set.
    let deadline = stream.read_timeout().ok().flatten().unwrap_or(MD_READ_TIMEOUT);
    let (max_events, limit) = match bound {
        Bound::First { events, duration } => (events, duration),
        Bound::Unbounded => (None, None),
    };

    let end = loop {
        if matches!(max_events, Some(n) if tally.events >= n) {
            break End::Events(tally.events);
        }
        let left = limit.map(|d| d.saturating_sub(started.elapsed()));
        if left == Some(Duration::ZERO) {
            break End::Elapsed;
        }
        // ⚠ Never zero: a zero read timeout is an `InvalidInput` error on both Unix and Windows
        // rather than a non-blocking read, so the floor is a millisecond and the bound is re-checked
        // at the top of the loop.
        let budget = left.map_or(deadline, |l| l.min(deadline).max(Duration::from_millis(1)));
        if let Err(e) = stream.set_read_timeout(Some(budget)) {
            break End::Fault(e.to_string());
        }
        match read_frame::<_, Response>(&mut stream) {
            Ok(Response::Md(frame)) => {
                let frame = *frame;
                count_frame(&mut tally, &frame);
                let text = match render {
                    Render::Table => table_line(&frame),
                    // `status`'s document form cannot reach this verb — `parse` refuses it — and the
                    // arm is spelled rather than left to a catch-all so a future third rendering
                    // has to answer for itself here.
                    Render::Jsonl | Render::Json => jsonl_row(&frame),
                };
                if let Err(e) = sink.line(&text) {
                    break match e.kind() {
                        io::ErrorKind::BrokenPipe => End::ReaderGone,
                        _ => End::Fault(e.to_string()),
                    };
                }
                if let MdFrame::Bye(why) = frame {
                    break End::Bye(why);
                }
            }
            Ok(other) => break End::Desync(format!("expected an Md frame, got {other:?}")),
            // ⚠ A read TIMEOUT arrives as `WouldBlock` on Unix and `TimedOut` on Windows — one
            // deadline, two errnos — and this is the one place in this verb that has to know it.
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                // Which deadline fired is a question about the CLOCK, not about the error: if the
                // `--for` bound has run out, this is the bound; otherwise the link went silent past
                // the period the server itself declared.
                let spent = limit.is_some_and(|d| started.elapsed() >= d);
                break if spent { End::Elapsed } else { End::Silent };
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break End::Closed,
            Err(e) => break End::Fault(e.to_string()),
        }
    };
    (end, tally)
}

/// Count one frame by CLASS. Only the three DATA variants are events — see [`Tally`] and the
/// `--events` row of [`usage`]: a heartbeat is what the wire says ABOUT the stream, and counting it
/// would let a dead-quiet key satisfy `--events 500` by saying nothing 500 times.
pub(super) fn count_frame(tally: &mut Tally, frame: &MdFrame) {
    match frame {
        MdFrame::Depth(_) | MdFrame::Book(_) | MdFrame::Trades { .. } => tally.events += 1,
        MdFrame::Status { .. } => tally.statuses += 1,
        MdFrame::TapeGap { dropped, .. } => {
            tally.gaps += 1;
            tally.dropped += dropped;
        }
        MdFrame::Heartbeat => tally.heartbeats += 1,
        MdFrame::Bye(_) => {}
    }
}

/// The closing report — stderr, always, under both renderings.
pub(super) fn summary_lines(asked: &MdSpec, end: &End, tally: &Tally) -> Vec<String> {
    let mut lines = vec![
        format!(
            "stream ended — {}:{} {}: {}",
            asked.venue,
            asked.symbol,
            asked.lane.feed_stream_label(),
            end.sentence()
        ),
        format!(
            "frames: {} data, {} status, {} tape-gap, {} heartbeat",
            tally.events, tally.statuses, tally.gaps, tally.heartbeats
        ),
    ];
    if tally.dropped > 0 {
        lines.push(format!(
            "⚠ {} PRINTS WERE LOST inside this stream. Anything folded from it — CVD, delta, \
             footprint volume — is wrong by that much and cannot be repaired from what arrived",
            tally.dropped
        ));
    }
    if tally.events == 0 {
        lines.push(
            "no DATA frame arrived. On a quiet key that is the market and not a fault — the \
             heartbeat count above is what says the link was alive"
                .to_string(),
        );
    }
    lines
}

// ─── where the frames go ─────────────────────────────────────────────────────────────────────────

/// Where a rendered frame is written.
///
/// ⚠ **The file is flushed line by line**, and that is a property of a STREAM rather than a
/// preference: a watch is a thing an operator interrupts, and a buffered tail lost on ^C is data the
/// wire will never send again. The cost is one `write` syscall per frame, against a lane whose
/// publish cadence is measured in tens per second.
pub(super) enum Sink {
    /// stdout, LOCKED ONCE for the stream's whole life.
    ///
    /// ⚠ Not `println!`: that PANICS on a broken pipe, and `| head -3` closes one as a matter of
    /// routine — see [`End::ReaderGone`].
    ///
    /// ⚠ **The doc said "locked once" while [`Sink::line`] took the lock per frame**, which is the
    /// cheaper half of the two claims and the one that was false. Holding it is safe HERE and would
    /// not be everywhere: nothing else in this verb writes stdout at all — every note, disclosure
    /// and summary goes to stderr, by the rule in this module's own doc — so there is no second
    /// writer to deadlock against.
    Stdout(io::StdoutLock<'static>),
    File {
        path: String,
        out: BufWriter<File>,
    },
}

impl Sink {
    /// Open the destination — AFTER the subscription was accepted, deliberately.
    ///
    /// ⚠ `File::create` TRUNCATES, so opening it earlier would destroy a previous capture on a run
    /// that then failed to connect or was refused a spec. The cost of this order is that an
    /// unwritable path is discovered one round trip in rather than at the door, and that is the
    /// cheaper of the two mistakes: a refused subscription costs a connection, and a truncated
    /// capture costs a tape the wire will never send again.
    pub(super) fn open(path: Option<&str>) -> CmdResult<Self> {
        match path {
            None => Ok(Sink::Stdout(io::stdout().lock())),
            Some(p) => {
                let file = File::create(p).map_err(|e| {
                    CliError::failed(format!("--out {p:?} could not be opened for writing: {e}"))
                })?;
                Ok(Sink::File { path: p.to_string(), out: BufWriter::new(file) })
            }
        }
    }

    /// Write one rendered frame. The `io::Error` is returned rather than classified, because only
    /// the caller knows that a broken pipe is a normal end here.
    fn line(&mut self, text: &str) -> io::Result<()> {
        match self {
            Sink::Stdout(out) => {
                writeln!(out, "{text}")?;
                out.flush()
            }
            Sink::File { out, .. } => {
                writeln!(out, "{text}")?;
                out.flush()
            }
        }
    }

    /// Close the destination. A failure HERE is worth a rung of its own: the frames were rendered
    /// and the file may be short, which a caller reading only the exit code would otherwise take for
    /// a complete capture.
    pub(super) fn finish(&mut self) -> CmdResult<()> {
        match self {
            Sink::Stdout(_) => Ok(()),
            Sink::File { path, out } => out
                .flush()
                .map_err(|e| CliError::failed(format!("--out {path:?} could not be flushed: {e}"))),
        }
    }
}
