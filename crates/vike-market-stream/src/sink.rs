//! The WIRE SINK: a `vike_data::LiveDataSink` whose every verb becomes one frame of ONE stream session's
//! bounded queue (`crate::queue`), and the writer-side API the session's writer thread drains.
//!
//! This is the object that makes "a venue pump never blocks" true. A feed thread calling `quote` takes
//! the session's lock for a few hash operations, assigns the frame its `seq`, queues it (or evicts, or
//! supersedes, per the queue's policy) and returns; nothing on that path serializes a frame, touches a
//! socket or waits for the consumer. The writer thread owns all of that, so a consumer parked in the
//! middle of a frame costs the pump nothing but a growing disclosed `Gap`.
//!
//! # Keys
//!
//! Frames carry a session-assigned key id `k` instead of the venue and symbol (a polymarket token id is
//! 78 digits). One `k` per SYMBOL per session; every frame of every subscription on the symbol shares it,
//! and the `Gap` names the lane. The reader thread binds a key when it starts a subscription
//! ([`WireSink::open_sub`]) and announces it in the `SubOk`; a feed that emits a symbol it was not asked
//! for gets a key too, announced by a `Key` frame ahead of the first frame that needs it (at most
//! [`SinkLimits::max_unsolicited`] of them; the rest are dropped and COUNTED). Keys are never reused
//! within a session, and a released key's numbers are freed once its queued frames have drained.
//!
//! # `SubOk` before any frame carrying `k`
//!
//! A venue client may emit a seed from INSIDE `subscribe_*`, before the call returns and the `SubOk` is
//! queued. A key a `Sub` is still starting is therefore HELD ([`WireSink::open_sub`] holds it): its
//! frames are queued and counted like any other, but the writer does not deliver them until it has
//! dequeued that key's `SubOk`. If the subscription is refused instead ([`WireSink::sub_failed`]) the
//! held frames are purged, because the client never learned the key.
//!
//! # The class of a subscription
//!
//! A subscriber may raise the delivery class of a key's lane ([`WireSink::set_delivery`]); the policy
//! that makes a raised lane lossless, and why, is `crate::queue`'s module doc. The sink only routes the
//! change in: it applies the floor to the queue under the same lock a push takes and queues the
//! announcement ([`StreamDown::Delivery`]) in the control lane's emission order.
//!
//! # What the sink is not
//!
//! It is not a broker: it makes no decision about WHO wants a frame. The datahub's `RoutingSink` fans
//! frames to its holders after the hop; this sink carries everything the venue client emits for the
//! symbols the session holds.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_data::delivery::Delivery;
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

use crate::queue::{Control, Payload, Queue, QueueLimits, QueueStats, Taken, Undo, Verb};
use crate::wire::{ByeReason, StreamDown, SubErrKind, WireDelivery, WireLane, WireStream};

/// The caps of one sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkLimits {
    /// The queue's bounds.
    pub queue: QueueLimits,
    /// The most symbols the session binds at once (subscriptions plus unsolicited keys count against
    /// the first; see `max_unsolicited` for the second).
    pub max_keys: usize,
    /// The most symbols bound WITHOUT a subscription.
    pub max_unsolicited: usize,
}

/// How a key came to be bound for a `Sub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenKind {
    /// A new key, held until its `SubOk` is dequeued.
    Fresh,
    /// The symbol was already bound by a live subscription.
    Existing,
    /// The symbol was bound by a `Key` frame (a feed emitted it unasked); the subscription adopts it.
    Adopted,
}

/// A key bound for one `Sub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    /// The key.
    pub k: u32,
    /// How it came to be.
    pub kind: OpenKind,
}

/// What the writer thread gets.
#[derive(Debug)]
pub enum Next {
    /// A frame to write, with the `Gap`s that precede it.
    Frame(Box<Taken>),
    /// `Gap`s for holes no queued frame will carry.
    Gaps(Vec<StreamDown>),
    /// Write a `Heartbeat` now (a client `Ping`).
    Heartbeat,
    /// The session is over: write the `Bye` (if any) and stop.
    End(Option<ByeReason>),
    /// The wait elapsed with nothing to write.
    Idle,
}

/// What a session did, for its closing log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkSummary {
    /// The queue's own counters.
    pub queue: QueueStats,
    /// Frames for a symbol the session could not bind (past the unsolicited cap), dropped.
    pub unbound_dropped: u64,
}

struct KeyEntry {
    k: u32,
    /// Bound by a subscription (as opposed to a `Key` frame for an unasked symbol).
    solicited: bool,
    /// Live subscriptions on the symbol.
    subs: usize,
}

struct State {
    queue: Queue,
    keys: HashMap<String, KeyEntry>,
    next_k: u32,
    unsolicited: usize,
    ended: Option<Option<ByeReason>>,
    heartbeat_requested: bool,
    writer_waiting: bool,
    unbound_dropped: u64,
}

impl State {
    fn end(&mut self, reason: Option<ByeReason>) {
        if self.ended.is_none() {
            self.ended = Some(reason);
        }
    }

    fn alloc_k(&mut self) -> Option<u32> {
        let k = self.next_k;
        self.next_k = self.next_k.checked_add(1)?;
        Some(k)
    }

    /// Queue a control frame, ending the session if the reserved lane is full.
    fn push_ctrl(&mut self, control: Control) {
        if !self.queue.push_ctrl(control) {
            self.end(Some(ByeReason::ControlLaneOverflow));
        }
    }
}

struct Shared {
    venue: String,
    limits: SinkLimits,
    state: Mutex<State>,
    cv: Condvar,
}

/// The wire sink. Cheap to clone (an `Arc`); the clones are one sink.
#[derive(Clone)]
pub struct WireSink {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for WireSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireSink").field("venue", &self.shared.venue).finish_non_exhaustive()
    }
}

impl WireSink {
    /// A sink for one session of `venue`.
    pub fn new(venue: &str, limits: SinkLimits) -> Self {
        Self {
            shared: Arc::new(Shared {
                venue: venue.to_string(),
                limits,
                state: Mutex::new(State {
                    queue: Queue::new(limits.queue),
                    keys: HashMap::new(),
                    next_k: 1,
                    unsolicited: 0,
                    ended: None,
                    heartbeat_requested: false,
                    writer_waiting: false,
                    unbound_dropped: 0,
                }),
                cv: Condvar::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wake the writer, if it is waiting; drops the guard first.
    fn wake(&self, guard: MutexGuard<'_, State>) {
        let waiting = guard.writer_waiting;
        drop(guard);
        if waiting {
            self.shared.cv.notify_one();
        }
    }

    // ---- The reader side: a subscription's life -------------------------------------------------

    /// Bind (or find) the key for `symbol` ahead of a `subscribe_*` call. A fresh key is HELD.
    pub fn open_sub(&self, symbol: &str) -> Result<Opened, SubErrKind> {
        let mut g = self.lock();
        let st = &mut *g;
        if st.ended.is_some() {
            return Err(SubErrKind::Draining);
        }
        if let Some(entry) = st.keys.get_mut(symbol) {
            let kind = if entry.solicited {
                OpenKind::Existing
            } else {
                entry.solicited = true;
                st.unsolicited = st.unsolicited.saturating_sub(1);
                OpenKind::Adopted
            };
            return Ok(Opened { k: entry.k, kind });
        }
        let cap = self.shared.limits.max_keys;
        if st.keys.len() >= cap {
            let held = u32::try_from(st.keys.len()).unwrap_or(u32::MAX);
            return Err(SubErrKind::Budget {
                scope: "symbols per session".to_string(),
                held,
                cap: u32::try_from(cap).unwrap_or(u32::MAX),
            });
        }
        let Some(k) = st.alloc_k() else {
            return Err(SubErrKind::Budget {
                scope: "key ids per session".to_string(),
                held: u32::MAX,
                cap: u32::MAX,
            });
        };
        st.keys.insert(symbol.to_string(), KeyEntry { k, solicited: true, subs: 0 });
        st.queue.hold(k);
        Ok(Opened { k, kind: OpenKind::Fresh })
    }

    /// The in-process `subscribe_*` returned `Ok`: queue the `SubOk`. Dequeuing it releases a held key.
    pub fn sub_ok(&self, sid: u64, symbol: &str, opened: Opened) {
        let mut g = self.lock();
        let st = &mut *g;
        if let Some(entry) = st.keys.get_mut(symbol)
            && entry.k == opened.k
        {
            entry.subs += 1;
        }
        st.push_ctrl(Control::SubOk { sid, k: opened.k });
        self.wake(g);
    }

    /// The in-process `subscribe_*` returned `Err`: queue the `SubErr`, and forget a key this call
    /// created (its held frames are purged: the client never learned it).
    pub fn sub_failed(&self, sid: u64, symbol: &str, opened: Opened, err: SubErrKind) {
        let mut g = self.lock();
        let st = &mut *g;
        match opened.kind {
            OpenKind::Fresh => {
                st.keys.remove(symbol);
                st.queue.purge(opened.k);
            }
            OpenKind::Adopted => {
                if let Some(entry) = st.keys.get_mut(symbol)
                    && entry.k == opened.k
                    && entry.subs == 0
                {
                    entry.solicited = false;
                    st.unsolicited += 1;
                }
            }
            OpenKind::Existing => {}
        }
        st.push_ctrl(Control::SubErr { sid, err });
        self.wake(g);
    }

    /// Queue the refusal of a `Sub` that never got a key (a spelling the daemon will not carry, a
    /// subscription cap).
    pub fn refuse(&self, sid: u64, err: SubErrKind) {
        let mut g = self.lock();
        g.push_ctrl(Control::SubErr { sid, err });
        self.wake(g);
    }

    /// Answer a repeated `Sub` with the key it already has: no new subscription is counted.
    pub fn re_ack(&self, sid: u64, k: u32) {
        let mut g = self.lock();
        g.push_ctrl(Control::SubOk { sid, k });
        self.wake(g);
    }

    /// A subscription on `symbol` ended (its `unsubscribe` returned, so the feed emits nothing more for
    /// it). When it was the symbol's last, the key is released.
    pub fn sub_ended(&self, symbol: &str) {
        let mut g = self.lock();
        let st = &mut *g;
        let release = match st.keys.get_mut(symbol) {
            Some(entry) => {
                entry.subs = entry.subs.saturating_sub(1);
                (entry.subs == 0 && entry.solicited).then_some(entry.k)
            }
            None => None,
        };
        if let Some(k) = release {
            st.keys.remove(symbol);
            st.queue.release_key(k);
        }
    }

    /// How many BOOK frames (an anchor or a delta) of `symbol`'s key this session has lost since the key
    /// was bound: evicted from the bounded queue, refused as non-finite or taken back by the writer as
    /// unsendable. A figure that only grows while the key lives (a key given back and bound again starts
    /// from zero, so compare for DIFFERENCE, not for more), `0` for a symbol with no key.
    ///
    /// A lost book frame is disclosed to the receiver as a `Gap`, but the receiver has no way to ask for a
    /// resync (the tube has no such frame). This is the SENDER's half of the cure, for a sender that
    /// diffs (`vike_gatewayhub`'s `BookFeed`): when the figure moved, the receiver's book is no longer
    /// the one the sender last encoded, so the next frame must be an anchor, not another delta.
    pub fn book_losses(&self, symbol: &str) -> u64 {
        let g = self.lock();
        let Some(entry) = g.keys.get(symbol) else { return 0 };
        g.queue.lost(entry.k, Verb::Book) + g.queue.lost(entry.k, Verb::BookDelta)
    }

    /// Set the delivery class FLOOR of `lane` on key `k` (a subscriber's `Sub.class` or `Delivery` frame;
    /// [`Delivery::Conflatable`] clears it). Applies to the frames pushed from here on; the ones already
    /// queued keep the rule they entered under. A change of the lane's class is announced to the
    /// receiver in frame order ([`StreamDown::Delivery`]); a floor that changes nothing (a lane of verbs
    /// that are already lossless, a bar lane) says nothing. Returns the lane's previous floor, so a
    /// caller whose subscription then failed can put it back.
    pub fn set_delivery(&self, k: u32, lane: WireLane, floor: Delivery) -> Delivery {
        let mut g = self.lock();
        let st = &mut *g;
        let previous = st.queue.floor_of(k, lane);
        if st.ended.is_some() {
            return previous;
        }
        if let Some(class) = st.queue.set_floor(k, lane, floor) {
            st.push_ctrl(Control::Delivery { k, lane, class: WireDelivery::from_delivery(class) });
        }
        self.wake(g);
        previous
    }

    /// Answer a client `Ping`: the writer writes a `Heartbeat` at once.
    pub fn request_heartbeat(&self) {
        let mut g = self.lock();
        g.heartbeat_requested = true;
        self.wake(g);
    }

    /// End the session. `Some(reason)` is written as the last frame (`Bye`); `None` ends it silently
    /// (the socket is already gone). Idempotent: the first reason stands. The backlog is discarded.
    pub fn finish(&self, reason: Option<ByeReason>) {
        let mut g = self.lock();
        g.end(reason);
        drop(g);
        self.shared.cv.notify_all();
    }

    /// Whether the session has been ended.
    pub fn is_ended(&self) -> bool {
        self.lock().ended.is_some()
    }

    /// What the session did.
    pub fn summary(&self) -> SinkSummary {
        let g = self.lock();
        SinkSummary { queue: g.queue.stats(), unbound_dropped: g.unbound_dropped }
    }

    /// How many data frames and bytes are queued right now (the tests' view of the bounds).
    pub fn queued(&self) -> (usize, usize) {
        let g = self.lock();
        (g.queue.data_frames(), g.queue.data_bytes())
    }

    // ---- The writer side ------------------------------------------------------------------------

    /// The next thing the writer should do, waiting up to `wait` for one.
    pub fn next(&self, wait: Duration) -> Next {
        let deadline = Instant::now() + wait;
        let mut g = self.lock();
        loop {
            if let Some(end) = g.ended {
                return Next::End(end);
            }
            if g.heartbeat_requested {
                g.heartbeat_requested = false;
                return Next::Heartbeat;
            }
            if let Some(taken) = g.queue.take() {
                return Next::Frame(Box::new(taken));
            }
            let gaps = g.queue.tail_gaps();
            if !gaps.is_empty() {
                return Next::Gaps(gaps);
            }
            let now = Instant::now();
            if now >= deadline {
                return Next::Idle;
            }
            g.writer_waiting = true;
            let (guard, _) = self
                .shared
                .cv
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            g = guard;
            g.writer_waiting = false;
        }
    }

    /// The writer found a lossless frame it could not send (over the frame ceiling): it is a hole.
    pub fn note_unsendable(&self, undo: Undo) {
        self.lock().queue.note_unsendable(undo);
    }

    // ---- The pump side --------------------------------------------------------------------------

    /// The key for `symbol`, binding an unasked one (and queuing its `Key` frame) if there is room.
    fn bind(&self, st: &mut State, symbol: &str) -> Option<u32> {
        if let Some(entry) = st.keys.get(symbol) {
            return Some(entry.k);
        }
        if st.unsolicited >= self.shared.limits.max_unsolicited {
            return None;
        }
        let k = st.alloc_k()?;
        st.keys.insert(symbol.to_string(), KeyEntry { k, solicited: false, subs: 0 });
        st.unsolicited += 1;
        st.push_ctrl(Control::Key { k, symbol: symbol.to_string() });
        Some(k)
    }

    fn push_data(&self, symbol: &str, payload: Payload) {
        // The finite scan of a folded `Book` walks every level: done here, outside the session lock the
        // pump and the writer share.
        let finite = payload.is_finite();
        let mut g = self.lock();
        let st = &mut *g;
        if st.ended.is_some() {
            return;
        }
        let Some(k) = self.bind(st, symbol) else {
            st.unbound_dropped += 1;
            return;
        };
        st.queue.push_data_checked(k, payload, finite);
        self.wake(g);
    }

    fn push_status(&self, symbol: &str, control: impl FnOnce(u32) -> Control) {
        let mut g = self.lock();
        let st = &mut *g;
        if st.ended.is_some() {
            return;
        }
        let Some(k) = self.bind(st, symbol) else {
            st.unbound_dropped += 1;
            return;
        };
        st.push_ctrl(control(k));
        self.wake(g);
    }
}

impl LiveDataSink for WireSink {
    fn seed_bars(&self, _venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push_data(symbol, Payload::Seed { interval: interval.to_string(), bars });
    }

    fn close_bar(&self, _venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push_data(symbol, Payload::BarClose { interval: interval.to_string(), bar });
    }

    fn forming_bar(&self, _venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push_data(symbol, Payload::Forming { interval: interval.to_string(), bar });
    }

    fn mark_tick(&self, _venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push_data(symbol, Payload::Mark { px, ts });
    }

    fn bar_close_tick(&self, _venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push_data(symbol, Payload::BarCloseTick { px, ts });
    }

    fn l2_snapshot(
        &self,
        _venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.push_data(symbol, Payload::Depth { tick_size, bids, asks, ts });
    }

    fn quote(&self, _venue: &str, symbol: &str, quote: QuoteTick) {
        self.push_data(symbol, Payload::Quote(quote));
    }

    fn trade(&self, _venue: &str, symbol: &str, trade: TradeTick) {
        self.push_data(symbol, Payload::Trade(trade));
    }

    fn book(&self, _venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.push_data(symbol, Payload::Book(book));
    }

    fn book_update(&self, _venue: &str, symbol: &str, update: BookUpdate) {
        self.push_data(symbol, Payload::BookDelta(update));
    }

    fn stream_status(&self, _venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        let stream = WireStream::from_word(stream);
        self.push_status(symbol, move |k| Control::Status { k, stream, status });
    }

    fn data_delay(&self, _venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        let stream = WireStream::from_word(stream);
        self.push_status(symbol, move |k| Control::Delay { k, stream, delay });
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod sink_tests;
