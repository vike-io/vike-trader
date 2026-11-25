//! `server::observer_push` — the SHAPED observe stream: a subscriber's writer that builds its own frames from the
//! publisher's newest instead of writing the shared bytes in order. The owner's ruling of 2026-11-18 put both of its
//! settings rows in the database, OFF by default ("add this to settings but by default turned off").
//!
//! ## Who is shaped
//!
//! A subscriber of this build's clients, which sends `Request::SubscribeWith` (to a node advertising
//! `vike_tradehub_client::proto::FEATURE_OBSERVE_COMPRESSED`), and EVERY subscriber while the cap is on. A `Subscribe` /
//! `SubscribeNewest` with the cap off keeps [`super::run_push_writer`]'s plain path, byte for byte as before.
//!
//! ## What a shaped writer does
//!
//! - **The cap** (`config.observer_frames_per_second`, [`crate::publish::ObserverStream::frame_gap`]): at most that many
//!   frames a second. The writer is woken by each push, but it never writes the queue: when a frame is due it builds it
//!   from the publisher's NEWEST ([`PublisherHandle::latest_frame`]), so the subscriber gets the latest state, never a
//!   backlog. Each frame is a whole snapshot (every open order and every one that ended within the hour, the positions, the
//!   trading state, the HALT sentinel, the fault), so a frame it skipped holds no fact the newest does not, BUT the
//!   recent-event lines: the core's ring is 64 lines, and an order's fill, ack, refusal or a halt is a line that can roll
//!   off between two capped frames. So the lines of the frames this subscriber was not sent RIDE THE NEXT ONE, ahead of
//!   its own ring ([`ObserverPush::frame`], from the publisher's numbered log), and none is lost while the log holds it.
//!   Without the cap nothing is carried, as before.
//! - **The form** (`vike_hub_protocol::frame::BodyCodec`): every frame and heartbeat goes in the form the subscription
//!   arrived in, msgpack for a client of this build, JSON for one from before it. A msgpack subscriber's untouched
//!   frame is the publisher's own msgpack bytes, framed once for all of them (`crate::publish::PublishedFrame::bytes`).
//! - **Compression** (`config.observer_frame_compression`, for a subscriber that asked): DEFLATE, one compressor for
//!   both forms. A msgpack subscriber's body is deflated behind
//!   `vike_tradehub_client::observe_stream::MSGPACK_DEFLATE_BODY_MAGIC` (an untouched frame is the publisher's one
//!   deflated frame of the publish, `crate::publish::PublishedFrame::deflated_msgpack`); a JSON subscriber, a desktop
//!   from before msgpack, keeps the deflate it reads (behind
//!   `vike_tradehub_client::observe_stream::COMPRESSED_BODY_MARKER`). Not the codec's LZ4 form on this stream: deflate
//!   sends fewer bytes, whole frames and patches alike (`measure_bytes_and_time_per_publish`), over the desktop's tunnel.
//! - **The newest** (a reader that says what it read): one frame on its way, the next when the reader has read it, as
//!   the plain path's newest mode.
//! - **Patches** (a subscriber that asked, by `vike_tradehub_client::proto::FEATURE_OBSERVE_PATCHES`; ON whenever it
//!   is asked: it changes how the state is sent, never what the reader rebuilds): a whole snapshot first, then only
//!   what changed since the frame this writer sent last (`vike_tradehub_client::observe_patch`), a new or edited bar
//!   per series instead of 300, the changed fields of an order, a position, an account. Each frame is numbered and
//!   names the one it applies to, so a reader that missed one asks again (`Request::ObserveResync`) and the next frame
//!   is whole. The diff is against what was SENT, so the cap and the newest skip frames freely. It is typed
//!   (`vike_tradehub_client::observe_patch::diff_snapshots`, the tree diff's very bytes): the writer keeps the snapshot
//!   it sent, by pointer when it sent the publisher's own, and an order or bar that did not change costs a comparison.
//!
//! With neither row on and no patches, a shaped subscriber is written the publisher's own bytes, unchanged.
//!
//! ## What it does not touch
//!
//! The trading core, its strategy lanes and every control path: this runs on the subscriber's own connection thread,
//! after the publisher's one arc-swap load of the core's published snapshot, and the publisher's push never blocks on it
//! (`crate::publish`'s module doc). A `Request::Snapshot` is answered as before.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_hub_protocol::frame::{
    BodyCodec, decode_body, encode_reply, read_frame_raw, write_reply_as,
};
use vike_tradehub_client::observe_patch;
use vike_tradehub_client::observe_stream::{compress_body, deflate_msgpack_body, frame_body};
use vike_tradehub_client::proto::{
    ObserveSubscriptionWire, Request, Response, SeriesKeyWire, SnapshotPatchWire,
};
use vike_tradehub_client::wire::BarSeriesWire;

use crate::publish::{PublishedFrame, PublisherHandle, PublisherSubscription, ShapedWake};

use super::{LinkPolicy, WRITER_RECV_TIMEOUT, log_push_write_end, write_all_flush};

/// What a subscriber asked of its stream ([`super::run_push_writer`]'s argument).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PushShape {
    /// It subscribed with `Request::SubscribeWith`: a reader of this build, which may speak after subscribing.
    pub(super) shaped: bool,
    /// It says what it has read and is sent the newest next.
    pub(super) newest: bool,
    /// It reads a compressed body.
    pub(super) compressed: bool,
    /// It rebuilds the snapshot from patches (`Response::SnapshotPatch`).
    pub(super) patches: bool,
    /// The bar series it shows (`None`: every series), until it says otherwise.
    pub(super) series: Option<Vec<SeriesKeyWire>>,
    /// The form its frames go in: the one its subscription arrived in. A plain subscription is an old client's word
    /// and keeps JSON, byte for byte.
    pub(super) codec: BodyCodec,
}

impl PushShape {
    /// A `Subscribe` (`newest` false) or a `SubscribeNewest`.
    pub(super) fn plain(newest: bool) -> Self {
        PushShape { newest, ..PushShape::default() }
    }

    /// A `SubscribeWith`, which arrived in `codec`'s form.
    pub(super) fn asked(subscription: &ObserveSubscriptionWire, codec: BodyCodec) -> Self {
        PushShape {
            shaped: true,
            newest: subscription.newest,
            compressed: subscription.compressed,
            patches: subscription.patches,
            series: subscription.series.clone(),
            codec,
        }
    }
}

/// **One subscriber's frame builder**: what the node's rows and the subscriber's asks make of the publisher's newest.
pub(super) struct ObserverPush {
    /// Carry the lines of the frames not sent (the cap is on).
    carry: bool,
    /// The form the frames are written in: the subscription's ([`PushShape::codec`]), JSON or msgpack.
    codec: BodyCodec,
    /// Deflate the body: the row is on AND the subscriber asked for compression.
    deflate: bool,
    /// The serial after the last recent-event line this subscriber was sent.
    sent_events_end: Option<u64>,
    /// Send patches: what this subscriber was sent last ([`Patching`]); `None` sends whole frames.
    patching: Option<Patching>,
    /// The bar series this subscriber shows; `None` is every series.
    series: Option<HashSet<SeriesKeyWire>>,
}

/// A patched subscription's side of the stream: the snapshot it was sent last, and the number of that frame.
#[derive(Default)]
struct Patching {
    /// The snapshot the subscriber holds, as this writer sent it (the publisher's own frame when it was sent unchanged,
    /// so no copy); `None` before the first frame and after a resync, so the next frame is whole.
    sent: Option<Arc<PublishedFrame>>,
    /// The number of the last frame sent (0 before the first).
    frame: u64,
}

impl ObserverPush {
    pub(super) fn new(shape: &PushShape, publisher: &PublisherHandle) -> Self {
        let stream = publisher.observer_stream();
        let (codec, deflate) = Self::form(shape.codec, shape.compressed && stream.compression);
        ObserverPush {
            carry: stream.frame_gap().is_some(),
            codec,
            deflate,
            sent_events_end: None,
            patching: shape.patches.then(Patching::default),
            series: shape.series.clone().map(HashSet::from_iter),
        }
    }

    /// What a subscription in `connection`'s form is written in, and whether it is deflated, when it is `compress`ed:
    /// JSON or msgpack (a subscription in the codec's LZ4 form is written msgpack: this stream's one compressor is
    /// deflate), deflated when compressed.
    fn form(connection: BodyCodec, compress: bool) -> (BodyCodec, bool) {
        match connection {
            BodyCodec::Json => (BodyCodec::Json, compress),
            BodyCodec::Msgpack | BodyCodec::MsgpackLz4 => (BodyCodec::Msgpack, compress),
        }
    }

    /// The subscriber shows `series` now (`None`: every series).
    pub(super) fn show(&mut self, series: Option<Vec<SeriesKeyWire>>) {
        self.series = series.map(HashSet::from_iter);
    }

    /// The reader missed a frame: the next one is whole.
    pub(super) fn resync(&mut self) {
        if let Some(patching) = &mut self.patching {
            patching.sent = None;
        }
    }

    /// The whole frame to write for `latest`. `missed` answers the recent-event lines numbered `from..to`
    /// ([`PublisherHandle::events_between`]).
    pub(super) fn frame(
        &mut self,
        latest: &Arc<PublishedFrame>,
        missed: impl FnOnce(u64, u64) -> Vec<String>,
    ) -> io::Result<Arc<Vec<u8>>> {
        let ring = latest.wire.recent_events.len() as u64;
        let ring_start = latest.events_end.saturating_sub(ring);
        let carried = match self.sent_events_end {
            Some(sent) if self.carry && sent < ring_start => missed(sent, ring_start),
            _ => Vec::new(),
        };
        self.sent_events_end = Some(latest.events_end);
        // A series the subscriber does not show keeps its key and sends no bar, so the reader still knows it is there.
        let hidden = |key: &BarSeriesWire| {
            self.series.as_ref().is_some_and(|shown| {
                !shown.contains(&SeriesKeyWire {
                    venue: key.venue.clone(),
                    symbol: key.symbol.clone(),
                    interval: key.interval.clone(),
                })
            })
        };
        let filters = latest
            .wire
            .bars
            .iter()
            .any(|s| hidden(s) && (!s.closed.is_empty() || s.forming.is_some()));
        if carried.is_empty() && !filters && self.patching.is_none() {
            match (self.deflate, self.codec) {
                (false, codec) => return Ok(Arc::clone(latest.bytes(codec))),
                (true, BodyCodec::Msgpack) => return Ok(Arc::clone(latest.deflated_msgpack())),
                // A JSON subscriber's deflate runs on its own writer, as it always did.
                (true, _) => {}
            }
        }
        let wire = if carried.is_empty() && !filters {
            Cow::Borrowed(&latest.wire)
        } else {
            let mut wire = latest.wire.clone();
            wire.recent_events.splice(0..0, carried);
            for series in wire.bars.iter_mut().filter(|s| hidden(s)) {
                series.closed = Vec::new();
                series.forming = None;
            }
            Cow::Owned(wire)
        };
        let frame = match (&mut self.patching, wire) {
            (Some(patching), wire) => {
                let shown = match wire {
                    Cow::Borrowed(_) => Arc::clone(latest),
                    Cow::Owned(wire) => Arc::new(PublishedFrame::new(wire, latest.events_end)),
                };
                framed(self.codec, self.deflate, &patching.next(shown)?)?
            }
            // Only a deflating JSON writer comes here with the publisher's wire unchanged (the returns above).
            (None, Cow::Borrowed(_)) => {
                frame_body(&compress_body(&latest.bytes(BodyCodec::Json)[4..]))?
            }
            (None, Cow::Owned(wire)) => {
                framed(self.codec, self.deflate, &Response::SnapshotFrame(Box::new(wire)))?
            }
        };
        Ok(Arc::new(frame))
    }
}

/// `response` as the whole frame a shaped writer sends: in `codec`'s form, deflated when `deflate`.
fn framed(codec: BodyCodec, deflate: bool, response: &Response) -> io::Result<Vec<u8>> {
    match (deflate, codec) {
        (true, BodyCodec::Json) => {
            let json = serde_json::to_vec(response)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            frame_body(&compress_body(&json))
        }
        (true, _) => frame_body(&deflate_msgpack_body(&encode_reply(codec, response)?[4..])),
        (false, _) => {
            let mut frame = Vec::new();
            write_reply_as(&mut frame, codec, response)?;
            Ok(frame)
        }
    }
}

impl Patching {
    /// The next `Response::SnapshotPatch`: what changed since the frame sent last, or the whole of `shown` when nothing
    /// was sent or the reader resynced. The same patch in every form (`observe_patch`'s one tree grammar).
    fn next(&mut self, shown: Arc<PublishedFrame>) -> io::Result<Response> {
        let patch = match &self.sent {
            Some(sent) => observe_patch::diff_snapshots(&sent.wire, &shown.wire)
                .map(|patch| patch.unwrap_or_else(|| serde_json::json!({}))),
            None => serde_json::to_value(&shown.wire),
        }
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let base = self.sent.is_some().then_some(self.frame);
        self.frame += 1;
        self.sent = Some(shown);
        Ok(Response::SnapshotPatch(Box::new(SnapshotPatchWire { frame: self.frame, base, patch })))
    }
}

/// [`super::run_push_writer`] for a shaped subscriber: its writer here, and a thread hearing its reader.
pub(super) fn run_shaped(
    stream: &mut TcpStream,
    publisher: &PublisherHandle,
    peer: Option<SocketAddr>,
    policy: LinkPolicy,
    shape: PushShape,
) {
    let subscription = publisher.subscribe_shaped();
    let reads = if shape.shaped || shape.newest {
        match spawn_reader(stream, &subscription) {
            Ok(reads) => Some(reads),
            Err(e) => {
                tracing::warn!(?peer, error = %e, "vike-tradehub observe: no reader of the subscriber's words, closing");
                return;
            }
        }
    } else {
        None
    };
    write_shaped(stream, publisher, &subscription, peer, policy, &shape);
    if let Some(reads) = reads {
        // A read parked on this socket ends only here.
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let _ = reads.join();
    }
}

/// The thread that hears a shaped subscriber's reader until the socket ends; nothing else reads this connection now. A
/// frame it cannot decode is passed over, as the session passes one over.
fn spawn_reader(
    stream: &TcpStream,
    subscription: &PublisherSubscription,
) -> io::Result<thread::JoinHandle<()>> {
    let mut read_half = stream.try_clone()?;
    read_half.set_read_timeout(None)?;
    let words = subscription.reader_words();
    thread::Builder::new().name("th-observer-words".into()).spawn(move || {
        while let Ok(body) = read_frame_raw(&mut read_half) {
            match decode_body(&body) {
                Ok(Request::FrameRead) => words.say(|w| w.read = true),
                Ok(Request::ObserveResync) => words.say(|w| w.resync = true),
                Ok(Request::ObserveSeries { series }) => words.say(|w| w.series = Some(series)),
                _ => {}
            }
        }
    })
}

/// The shaped writer's loop: a frame when one is due and something is new, a heartbeat when the socket has been quiet for
/// one, until a write fails or the publisher stops.
fn write_shaped(
    stream: &mut TcpStream,
    publisher: &PublisherHandle,
    subscription: &PublisherSubscription,
    peer: Option<SocketAddr>,
    policy: LinkPolicy,
    shape: &PushShape,
) {
    let gap = publisher.observer_stream().frame_gap();
    let mut push = ObserverPush::new(shape, publisher);
    // Something the subscriber has not been sent: a push (the subscribe's own counts), or a word that changes the frame.
    let mut pending = false;
    // A frame is on its way to a reader that says what it read, and unread.
    let mut on_its_way = false;
    let mut last_frame: Option<Instant> = None;
    let mut last_write = Instant::now();
    loop {
        // With a frame held back by the cap, wake when it is due rather than at the next push; one waiting for the
        // reader's word is woken by the word itself.
        let until_due = gap
            .zip(last_frame)
            .map(|(gap, at)| gap.saturating_sub(at.elapsed()))
            .filter(|_| pending && !(shape.newest && on_its_way))
            .unwrap_or(WRITER_RECV_TIMEOUT);
        match subscription
            .wait_shaped(until_due.clamp(Duration::from_millis(1), WRITER_RECV_TIMEOUT))
        {
            ShapedWake::Closed => {
                tracing::info!(?peer, "vike-tradehub observe: publisher stopped, closing push");
                return;
            }
            ShapedWake::Woke { framed, words } => {
                pending |= framed;
                if words.read {
                    on_its_way = false;
                }
                if words.resync {
                    push.resync();
                    pending = true;
                }
                if let Some(series) = words.series {
                    push.show(series);
                    pending = true;
                }
            }
        }
        let cap_allows = gap.zip(last_frame).is_none_or(|(gap, at)| at.elapsed() >= gap);
        if pending
            && cap_allows
            && !(shape.newest && on_its_way)
            && let Some(latest) = publisher.latest_frame()
        {
            let written = push
                .frame(&latest, |from, to| publisher.events_between(from, to))
                .and_then(|frame| write_all_flush(stream, &frame));
            if let Err(e) = written {
                log_push_write_end(&e, peer, "push write", policy.push_write_timeout);
                return;
            }
            pending = false;
            on_its_way = shape.newest;
            last_frame = Some(Instant::now());
            last_write = Instant::now();
        } else if last_write.elapsed() >= policy.observe_heartbeat {
            if let Err(e) = write_reply_as(stream, shape.codec, &Response::Pong) {
                log_push_write_end(&e, peer, "heartbeat write", policy.push_write_timeout);
                return;
            }
            last_write = Instant::now();
        }
    }
}

#[path = "observer_push_tests.rs"]
#[cfg(test)]
mod observer_push_tests;
