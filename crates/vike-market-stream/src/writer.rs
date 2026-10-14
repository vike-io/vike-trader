//! The stream session's WRITER thread: the only thread that writes to the session's socket after the
//! `DataOpened` reply, and the only one that serializes a frame.
//!
//! It drains `crate::sink::WireSink::next`: frames in emission order, the `Gap`s the queue owes
//! in front of them, a `Heartbeat` after the stated period of silence or on a client `Ping`, and the
//! closing `Bye`. A write that blocks past the socket's write timeout (a peer that has stopped reading)
//! or fails ends the session; nothing on this thread is ever waited for by a venue pump.
//!
//! # Lapses
//!
//! Every `Gap` written is a LAPSE: the reader lost frames. More than `lapse_budget` of them inside
//! `lapse_window` ends the session with `ByeReason::TooSlow` (after the gap that crossed the budget),
//! and the datahub's client reconnects and re-seeds. A session that keeps losing frames is being lied
//! to more slowly, so it is cut off rather than served an ever-staler stream.
//!
//! # A frame the writer cannot send
//!
//! A frame is serialized HERE, so this is where a frame over `GW_DOWN_FRAME_CEILING_BYTES` is found.
//! The client would treat a longer frame as a framing fault that kills the session, so it is not sent:
//! a lossless frame is taken back from the delivery books and becomes a hole (`Queue::note_unsendable`),
//! disclosed like any other; the incident is logged at `error`.

use std::io;
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use crate::frame::write_capped;
use crate::queue::Taken;
use crate::sink::{Next, WireSink};
use crate::wire::{ByeReason, StreamDown};

/// What the writer is configured with.
#[derive(Debug, Clone, Copy)]
pub struct WriterConfig {
    /// The heartbeat period stated in `DataOpened`: a `Heartbeat` after this much silence.
    pub heartbeat: Duration,
    /// `Gap`s allowed inside `lapse_window`.
    pub lapse_budget: u64,
    /// The window the budget is counted in.
    pub lapse_window: Duration,
    /// The longest frame the peer reads (the gatewayhub's `GW_DOWN_FRAME_CEILING_BYTES`): a frame over it
    /// is not sent.
    pub down_frame_ceiling_bytes: u32,
}

/// Write one frame under the client's ceiling.
fn emit(stream: &mut TcpStream, frame: &StreamDown, ceiling: u32) -> io::Result<()> {
    write_capped(stream, frame, ceiling)
}

/// The writer loop. Returns when the session is over; the socket is shut down on the way out, which is
/// what wakes the reader thread parked on it.
pub fn run_writer(mut stream: TcpStream, sink: &WireSink, cfg: WriterConfig, session: u64) {
    let mut last_write = Instant::now();
    let mut window_start = Instant::now();
    let mut lapses: u64 = 0;
    'session: loop {
        let wait = cfg.heartbeat.saturating_sub(last_write.elapsed());
        match sink.next(wait) {
            Next::Frame(taken) => {
                let Taken { gaps, item, undo } = *taken;
                if !gaps.is_empty() {
                    if !lapse(&mut stream, &gaps, &mut lapses, &mut window_start, &cfg) {
                        break 'session;
                    }
                    last_write = Instant::now();
                }
                let frame = item.into_frame();
                match emit(&mut stream, &frame, cfg.down_frame_ceiling_bytes) {
                    Ok(()) => last_write = Instant::now(),
                    Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                        // Refused before its first byte: the stream is in step, the frame is a hole.
                        tracing::error!(
                            session,
                            error = %e,
                            "vike-gatewayhub stream: a frame could not be sent and was dropped; \
                             a lossless one is disclosed as a Gap"
                        );
                        if let Some(undo) = undo {
                            sink.note_unsendable(undo);
                        }
                    }
                    Err(e) => {
                        tracing::info!(session, error = %e, "vike-gatewayhub stream: write fault; closing");
                        break 'session;
                    }
                }
            }
            Next::Gaps(gaps) => {
                if !lapse(&mut stream, &gaps, &mut lapses, &mut window_start, &cfg) {
                    break 'session;
                }
                last_write = Instant::now();
            }
            Next::Heartbeat => {
                if emit(&mut stream, &StreamDown::Heartbeat, cfg.down_frame_ceiling_bytes).is_err()
                {
                    break 'session;
                }
                last_write = Instant::now();
            }
            Next::Idle => {
                if last_write.elapsed() >= cfg.heartbeat {
                    if emit(&mut stream, &StreamDown::Heartbeat, cfg.down_frame_ceiling_bytes)
                        .is_err()
                    {
                        break 'session;
                    }
                    last_write = Instant::now();
                }
            }
            Next::End(reason) => {
                if let Some(reason) = reason {
                    let _ =
                        emit(&mut stream, &StreamDown::Bye(reason), cfg.down_frame_ceiling_bytes);
                }
                break 'session;
            }
        }
    }
    // However the loop ended, nothing more is queued and the reader must wake.
    sink.finish(None);
    let _ = stream.shutdown(Shutdown::Both);
}

/// Write the gaps of one disclosure and count it as ONE lapse. `false` when the session must end: a
/// write fault, or the lapse budget crossed (the `TooSlow` bye is written here).
fn lapse(
    stream: &mut TcpStream,
    gaps: &[StreamDown],
    lapses: &mut u64,
    window_start: &mut Instant,
    cfg: &WriterConfig,
) -> bool {
    if window_start.elapsed() >= cfg.lapse_window {
        *lapses = 0;
        *window_start = Instant::now();
    }
    *lapses = lapses.saturating_add(1);
    for gap in gaps {
        if emit(stream, gap, cfg.down_frame_ceiling_bytes).is_err() {
            return false;
        }
    }
    if *lapses > cfg.lapse_budget {
        let _ = emit(
            stream,
            &StreamDown::Bye(ByeReason::TooSlow { lapses: *lapses }),
            cfg.down_frame_ceiling_bytes,
        );
        tracing::info!(
            lapses = *lapses,
            "vike-gatewayhub stream: closed (too slow - gap disclosures over budget)"
        );
        return false;
    }
    true
}
