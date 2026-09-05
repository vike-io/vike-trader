//! `publish` — the CoreSnapshot -> wire projection and the ONE publisher thread (headless two-layer
//! plan, Layer 2, PR-11; steal S9).
//!
//! ## The hot-path guarantee (the latency-gate merge condition)
//!
//! The publisher NEVER touches the vike-core hot fold. Its ONLY interaction with the core is a single
//! `snapshot_cell().load_full()` — an atomic arc-swap load of the already-published, coalesced
//! [`CoreSnapshot`] — done on ITS OWN thread. It then serializes ONCE per change and fans the frame
//! bytes out to subscribers. The core thread never serializes, never blocks on a client, and is not
//! aware a publisher exists. This is why `cargo test -p vike-core --release --test runtime_latency
//! -- --ignored` (the p99 < 10µs core-hop gate) is unaffected: no work is added to the fold, only a
//! reader of the arc-swap cell is added alongside it. Do NOT ever move serialization onto the core
//! thread or read anything but the cell here.
//!
//! ## Fan-out and lossiness (steal S9 / S6)
//!
//! On each change (the coalesced snapshot's `seq` advanced) the publisher [`project`]s the snapshot
//! to a [`WireSnapshot`], frames it ONCE into a shared `Arc<Vec<u8>>` (the "serialize once" win), and
//! pushes that Arc into every subscriber's [`Mailbox`] — a per-connection BOUNDED, DROP-OLDEST queue.
//! A slow client's mailbox drops its OLDEST queued frame rather than back-pressuring the publisher, so
//! one stalled observer can never stall the fan-out or any other observer — the observer is lossy by
//! contract, exactly like the GUI's arc-swap read keeps only the latest. The publisher only ever
//! enqueues (never writes a socket), so a subscriber whose own writer thread is blocked on a full OS
//! send buffer is completely isolated from the publisher and from every other subscriber.
//!
//! ## vike-app is a deferred follow-up
//!
//! This module and the [`crate::server`] it feeds are the CI-testable core of PR-11. The
//! `vike-app --observe` GUI mode (a `RemoteCoreHandle` driving the panels) is a separate,
//! local-verify follow-up and is intentionally NOT wired here.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
use vike_core::snapshot::{CoreSnapshot, HeldOrderView, OrderView, PositionView, VenueBlock};
use vike_exec::TradingState;
use vike_model::Bar;
use vike_tradehub_client::proto::{Response, write_frame};
use vike_tradehub_client::wire::{
    WireBar, WireBarSeries, WireHeldOrderView, WireNodeIdentity, WireOrderView, WirePositionView,
    WireSnapshot, WireTradingState, WireVenueBlock,
};

/// Per-subscriber mailbox depth. A handful of frames is ample slack for a briefly-busy but live
/// consumer; once exceeded, the OLDEST frame is dropped (a stale snapshot a live consumer would have
/// skipped past anyway). Small on purpose — a large buffer would only delay a slow client's frames,
/// never help, since the observer wants the LATEST state, not a backlog.
const MAILBOX_CAP: usize = 8;

/// How often the publisher samples the arc-swap cell for a `seq` change. The core publishes coalesced
/// at ≥16 ms, so a ~15 ms poll never misses a distinct publish for long and adds no measurable load
/// (one atomic Arc load per tick). It is a plain sleep loop — no timer, no core interaction.
const POLL_INTERVAL: Duration = Duration::from_millis(15);

// ---------------------------------------------------------------------------------------------
// Projection: CoreSnapshot -> WireSnapshot (the rendered display subset the GUI needs).
// ---------------------------------------------------------------------------------------------

/// Map the account trading state onto its standalone wire mirror (same three variants).
fn project_trading_state(ts: TradingState) -> WireTradingState {
    match ts {
        TradingState::Active => WireTradingState::Active,
        TradingState::Reducing => WireTradingState::Reducing,
        TradingState::Halted => WireTradingState::Halted,
    }
}

/// Project one [`PositionView`] to its wire mirror (the display-relevant subset; the core's
/// `mark_source`/`margin_mode`/`isolated_margin` stay core-side).
fn project_position(p: &PositionView) -> WirePositionView {
    WirePositionView {
        venue: p.venue.clone(),
        symbol: p.symbol.clone(),
        position_side: p.position_side.clone(),
        size: p.size,
        avg_px: p.avg_px,
        unrealized: p.unrealized,
        leverage: p.leverage,
        liq_price: p.liq_price,
    }
}

/// Project one [`OrderView`] to its wire mirror. `status` is rendered to its `{:?}` string at this
/// edge so the wire crate need not mirror the `OrderStatus` enum (per [`WireOrderView`]'s contract).
/// The Debug-spelling contract is pinned by `vike-app-core`'s exhaustive `observe_bridge` round-trip
/// test (every `OrderStatus` variant's `{:?}` string must decode back identically on the GUI side).
fn project_order(o: &OrderView) -> WireOrderView {
    WireOrderView {
        client_order_id: o.client_order_id.clone(),
        venue: o.venue.clone(),
        symbol: o.symbol.clone(),
        side: o.side,
        qty: o.qty,
        order_type: o.order_type.clone(),
        price: o.price,
        trigger_price: o.trigger_price,
        status: format!("{:?}", o.status),
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        avg_fill_px: o.avg_fill_px,
    }
}

/// Project one held bracket exit to its wire mirror.
fn project_held(h: &HeldOrderView) -> WireHeldOrderView {
    WireHeldOrderView {
        client_order_id: h.client_order_id.clone(),
        venue: h.venue.clone(),
        symbol: h.symbol.clone(),
        side: h.side,
        qty: h.qty,
        order_type: h.order_type.clone(),
        price: h.price,
        trigger_price: h.trigger_price,
        parent_order_id: h.parent_order_id.clone(),
    }
}

/// Project one per-venue [`VenueBlock`] to its wire mirror (numbers a GUI renders; the heavy Arc
/// multiplier grid / fee schedule / margin math stay core-side).
fn project_venue(v: &VenueBlock) -> WireVenueBlock {
    WireVenueBlock {
        venue: v.venue.clone(),
        balance: v.balance,
        realized_pnl: v.realized_pnl,
        fees_paid: v.fees_paid,
        funding_paid: v.funding_paid,
        equity: v.equity,
        unrealized: v.unrealized,
        missing_prices: v.missing_prices,
        margin_used: v.margin_used,
        free_bp: v.free_bp,
        trading_state: project_trading_state(v.trading_state),
        positions: v.positions.iter().map(project_position).collect(),
    }
}

/// The bounded per-series closed-bar tail the node ships to observers. The core's own `closed` vec is
/// UNBOUNDED (it grows for the whole session — the fold never trims it), so the cap MUST be applied
/// HERE at the projection edge: `project` runs on every `seq` bump (far more frequent than bar closes),
/// and `frame_snapshot` `.expect()`s the body fits `MAX_FRAME_LEN` — an unbounded history would both
/// re-serialize the whole growing vec many times/sec AND eventually panic the framer. ~300 = a chart's
/// worth.
const CHART_BARS_CAP: usize = 300;

/// One [`Bar`] → its wire OHLCV mirror.
fn project_bar(b: &Bar) -> WireBar {
    WireBar { ts: b.ts, o: b.open, h: b.high, l: b.low, c: b.close, v: b.volume }
}

/// Project the core's bar cache to bounded wire series for the observer chart. The snapshot's `bars`
/// map spans EVERY mounted `(venue, symbol, interval)`, so filter to the PRIMARY mounted
/// `(venue, symbol)` the observer renders, and tail-slice each series' `closed` to the last
/// [`CHART_BARS_CAP`] plus the forming candle. Read-only over the Arc-shared cache — no fold, no clone
/// of the whole history.
fn project_bar_series(snap: &CoreSnapshot) -> Vec<WireBarSeries> {
    snap.bars
        .iter()
        .filter(|(k, _)| k.0 == snap.venue && k.1 == snap.symbol)
        .map(|(k, series)| {
            let closed = &series.closed;
            let start = closed.len().saturating_sub(CHART_BARS_CAP);
            WireBarSeries {
                venue: k.0.clone(),
                symbol: k.1.clone(),
                interval: k.2.clone(),
                closed: closed[start..].iter().map(project_bar).collect(),
                forming: series.forming.as_ref().map(project_bar),
            }
        })
        .collect()
}

/// Project a rendered [`CoreSnapshot`] to the standalone [`WireSnapshot`] the node publishes. Pure
/// over its arguments — a read-only mapping of the display fields, never a fold. This is the ONE
/// place the core snapshot shape crosses into the versioned wire schema. `identity` is the node's
/// process-static identity block (split-plane B3), threaded from [`spawn`]'s caller; `None`
/// publishes the pre-identity shape byte-identically.
pub fn project(snap: &CoreSnapshot, identity: Option<&WireNodeIdentity>) -> WireSnapshot {
    WireSnapshot {
        identity: identity.cloned(),
        seq: snap.seq,
        venue: snap.venue.clone(),
        symbol: snap.symbol.clone(),
        trading_state: project_trading_state(snap.trading_state),
        balance: snap.balance,
        equity_total: snap.portfolio.equity_total,
        venues: snap.portfolio.venues.iter().map(project_venue).collect(),
        orders: snap.orders.iter().map(project_order).collect(),
        positions: snap.positions.iter().map(project_position).collect(),
        held_exits: snap.held_exits.iter().map(project_held).collect(),
        // Arc<str> in-process -> owned String on the wire (schema unchanged)
        recent_events: snap.recent_events.iter().map(|s| s.to_string()).collect(),
        fault: snap.fault.clone(),
        bars: project_bar_series(snap),
    }
}

/// Project + frame the snapshot ONCE into shareable wire bytes: a full `[u32 len][JSON]`
/// [`Response::SnapshotFrame`] frame every subscriber's writer can `write_all` verbatim (the
/// serialize-once fan-out, steal S9). `write_frame` into a `Vec` only fails on a serialize error or
/// an over-`MAX_FRAME_LEN` body; a coalesced snapshot is kilobytes, so this cannot fail in practice.
fn frame_snapshot(snap: &CoreSnapshot, identity: Option<&WireNodeIdentity>) -> Arc<Vec<u8>> {
    let wire = project(snap, identity);
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Response::SnapshotFrame(Box::new(wire)))
        .expect("frame a coalesced snapshot (serialize cannot fail on a bounded snapshot)");
    Arc::new(buf)
}

// ---------------------------------------------------------------------------------------------
// Mailbox: a bounded, drop-oldest, single-producer/single-consumer frame queue.
// ---------------------------------------------------------------------------------------------

/// The outcome of a [`Mailbox`] receive.
pub enum Recv {
    /// One framed snapshot to write to the socket.
    Frame(Arc<Vec<u8>>),
    /// The wait elapsed with no frame (loop and re-check).
    Timeout,
    /// The mailbox was closed (publisher shutting down, or the subscriber deregistered) — stop.
    Closed,
}

struct MailboxInner {
    buf: VecDeque<Arc<Vec<u8>>>,
    closed: bool,
}

/// A per-connection BOUNDED, DROP-OLDEST frame queue. The publisher [`push`](Mailbox::push)es (never
/// blocking); the connection's writer thread [`recv_timeout`](Mailbox::recv_timeout)s. When full, the
/// OLDEST frame is discarded — the lossy-observer contract.
pub struct Mailbox {
    inner: Mutex<MailboxInner>,
    cv: Condvar,
    cap: usize,
}

impl Mailbox {
    fn new(cap: usize) -> Arc<Self> {
        Arc::new(Mailbox {
            inner: Mutex::new(MailboxInner { buf: VecDeque::new(), closed: false }),
            cv: Condvar::new(),
            cap,
        })
    }

    /// Producer side (the publisher thread): enqueue `frame`, DROPPING the oldest frame(s) if the
    /// bounded buffer is full. NEVER blocks and never fails — a closed mailbox silently ignores the
    /// push (the subscriber is gone). This is the property that keeps one slow client from ever
    /// stalling the publisher.
    fn push(&self, frame: Arc<Vec<u8>>) {
        let mut g = self.inner.lock().expect("mailbox poisoned");
        if g.closed {
            return;
        }
        while g.buf.len() >= self.cap {
            g.buf.pop_front();
        }
        g.buf.push_back(frame);
        drop(g);
        self.cv.notify_one();
    }

    /// Consumer side (the connection's writer thread): pop the oldest queued frame, or block up to
    /// `timeout` for one. Returns [`Recv::Closed`] once the mailbox is closed and drained,
    /// [`Recv::Timeout`] if the wait elapsed with nothing queued.
    pub fn recv_timeout(&self, timeout: Duration) -> Recv {
        let mut g = self.inner.lock().expect("mailbox poisoned");
        if let Some(f) = g.buf.pop_front() {
            return Recv::Frame(f);
        }
        if g.closed {
            return Recv::Closed;
        }
        // Wait for a push or the timeout; a spurious wakeup with nothing queued simply reports
        // `Timeout`, and the caller loops. The `WaitTimeoutResult` is not needed — the buffer/closed
        // re-check below is authoritative.
        let (mut g2, _) = self.cv.wait_timeout(g, timeout).expect("mailbox poisoned");
        if let Some(f) = g2.buf.pop_front() {
            return Recv::Frame(f);
        }
        if g2.closed {
            return Recv::Closed;
        }
        Recv::Timeout
    }

    /// Close the mailbox: no more frames accepted, waiting consumers wake to [`Recv::Closed`].
    fn close(&self) {
        let mut g = self.inner.lock().expect("mailbox poisoned");
        g.closed = true;
        drop(g);
        self.cv.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.inner.lock().expect("mailbox poisoned").closed
    }
}

// ---------------------------------------------------------------------------------------------
// Publisher: the poll+fan-out thread and its registry of subscribers.
// ---------------------------------------------------------------------------------------------

struct Subscriber {
    id: u64,
    mailbox: Arc<Mailbox>,
}

struct RegistryInner {
    subs: Vec<Subscriber>,
    /// The most recently framed snapshot, handed to a NEW subscriber immediately so it renders
    /// current state without waiting for the next change.
    last_frame: Option<Arc<Vec<u8>>>,
}

struct PublisherShared {
    /// The core's published snapshot cell — read (never written) via `load_full`. This is the ONLY
    /// coupling to the core, and it is off the hot fold by construction.
    cell: Arc<ArcSwap<CoreSnapshot>>,
    /// The node's process-static identity block (split-plane B3), stamped into every published
    /// frame. `None` = an identity-less node (publishes the pre-identity wire shape verbatim).
    identity: Option<WireNodeIdentity>,
    /// The node's process-static per-mount rows (split-plane I10) — one `WireMountRow` per mounted
    /// strategy, in mount order; the server's source for the `StrategyStatus` mounts `Vec`. Empty
    /// = the pre-I10 caller shape, where the server derives the one row from `identity` instead
    /// (see `server.rs`'s `Request::StrategyStatus` arm). NOT stamped into pushed frames — the
    /// wire snapshot carries only the identity block, exactly as before.
    mounts: Vec<vike_tradehub_client::wire::WireMountRow>,
    reg: Mutex<RegistryInner>,
    next_id: AtomicU64,
    stop: AtomicBool,
}

/// A cheap, cloneable handle to the running publisher: [`subscribe`](PublisherHandle::subscribe) to
/// join the fan-out, [`snapshot_frame_now`](PublisherHandle::snapshot_frame_now) for a point-in-time
/// frame, and [`shutdown`](PublisherHandle::shutdown) to stop the thread and release subscribers.
#[derive(Clone)]
pub struct PublisherHandle {
    shared: Arc<PublisherShared>,
    /// The poll thread's join handle, shared so any clone may [`shutdown`](Self::shutdown) once.
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// A live subscription: holds the receive [`Mailbox`] and DEREGISTERS from the publisher on drop, so
/// a connection's writer thread ending cleans up its registry slot automatically.
pub struct Subscription {
    shared: Arc<PublisherShared>,
    id: u64,
    mailbox: Arc<Mailbox>,
}

impl Subscription {
    /// Block up to `timeout` for the next framed snapshot (see [`Mailbox::recv_timeout`]).
    pub fn recv_timeout(&self, timeout: Duration) -> Recv {
        self.mailbox.recv_timeout(timeout)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.mailbox.close();
        let mut g = self.shared.reg.lock().expect("registry poisoned");
        g.subs.retain(|s| s.id != self.id);
    }
}

/// Spawn the publisher thread over the core's snapshot `cell` (from `CoreHandle::snapshot_cell()`).
/// The thread polls the cell for a `seq` change, projects+frames each new snapshot ONCE, and fans the
/// bytes to every subscriber. Returns a [`PublisherHandle`]; the server clones it to register
/// connections.
pub fn spawn(
    cell: Arc<ArcSwap<CoreSnapshot>>,
    identity: Option<WireNodeIdentity>,
) -> PublisherHandle {
    spawn_with_mounts(cell, identity, Vec::new())
}

/// [`spawn`] with the node's per-mount rows (split-plane I10): one
/// [`vike_tradehub_client::wire::WireMountRow`] per mounted strategy, in mount order — what the
/// server's `StrategyStatus` read verb answers with. [`spawn`] passes an EMPTY vec, under which
/// the server falls back to deriving the one row from `identity` (the pre-I10 behaviour,
/// byte-identical for every existing caller).
pub fn spawn_with_mounts(
    cell: Arc<ArcSwap<CoreSnapshot>>,
    identity: Option<WireNodeIdentity>,
    mounts: Vec<vike_tradehub_client::wire::WireMountRow>,
) -> PublisherHandle {
    let shared = Arc::new(PublisherShared {
        cell,
        identity,
        mounts,
        reg: Mutex::new(RegistryInner { subs: Vec::new(), last_frame: None }),
        next_id: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });

    let thread_shared = Arc::clone(&shared);
    let join = std::thread::Builder::new()
        .name("vt-tradehub-publish".into())
        .spawn(move || run_publisher(thread_shared))
        .expect("spawn vt-tradehub-publish thread");

    PublisherHandle { shared, join: Arc::new(Mutex::new(Some(join))) }
}

/// The publisher poll+fan-out loop. Reads the arc-swap cell; on a `seq` change, frames once and
/// broadcasts. Exits when `stop` is set, closing every subscriber's mailbox so writer threads wake.
fn run_publisher(shared: Arc<PublisherShared>) {
    // `None` until the first observed snapshot, so even a `seq: 0` placeholder is published once and a
    // brand-new subscriber gets a current frame.
    let mut last_seq: Option<u64> = None;
    while !shared.stop.load(Ordering::Acquire) {
        let snap = shared.cell.load_full();
        if last_seq != Some(snap.seq) {
            last_seq = Some(snap.seq);
            let frame = frame_snapshot(&snap, shared.identity.as_ref());
            broadcast(&shared, frame);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    // Teardown: close + clear every subscriber so their writer threads exit promptly.
    let mut g = shared.reg.lock().expect("registry poisoned");
    for s in &g.subs {
        s.mailbox.close();
    }
    g.subs.clear();
    g.last_frame = None;
}

/// Record `frame` as the latest, prune any closed subscribers, and push the shared Arc into every
/// remaining subscriber's mailbox (drop-oldest per mailbox). Runs on the publisher thread only.
fn broadcast(shared: &PublisherShared, frame: Arc<Vec<u8>>) {
    let mut g = shared.reg.lock().expect("registry poisoned");
    g.last_frame = Some(Arc::clone(&frame));
    g.subs.retain(|s| !s.mailbox.is_closed());
    for s in &g.subs {
        s.mailbox.push(Arc::clone(&frame));
    }
}

impl PublisherHandle {
    /// Register a new subscriber and return its [`Subscription`]. The current snapshot frame (if any)
    /// is enqueued immediately, so a fresh observer renders live state without waiting for the next
    /// change. If the publisher has already stopped, the returned subscription is pre-closed (its
    /// first `recv_timeout` yields [`Recv::Closed`]).
    pub fn subscribe(&self) -> Subscription {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let mailbox = Mailbox::new(MAILBOX_CAP);
        if self.shared.stop.load(Ordering::Acquire) {
            mailbox.close();
            return Subscription { shared: Arc::clone(&self.shared), id, mailbox };
        }
        let mut g = self.shared.reg.lock().expect("registry poisoned");
        if let Some(f) = &g.last_frame {
            mailbox.push(Arc::clone(f));
        }
        g.subs.push(Subscriber { id, mailbox: Arc::clone(&mailbox) });
        drop(g);
        Subscription { shared: Arc::clone(&self.shared), id, mailbox }
    }

    /// A point-in-time framed snapshot of the CURRENT cell — the answer to a `Request::Snapshot`.
    /// Projects+frames on demand off the arc-swap cell (a read, never the fold).
    pub fn snapshot_frame_now(&self) -> Arc<Vec<u8>> {
        frame_snapshot(&self.shared.cell.load_full(), self.shared.identity.as_ref())
    }

    /// The node's process-static identity block (split-plane B3), or `None` on an identity-less
    /// node — the server's source for the `StrategyStatus` read verb (split-plane B4). Cloned per
    /// call: an OCCASIONAL request/response verb, never per-publish work.
    pub fn identity(&self) -> Option<WireNodeIdentity> {
        self.shared.identity.clone()
    }

    /// The node's process-static per-mount rows (split-plane I10) — one row per mounted strategy,
    /// in mount order; EMPTY on a publisher spawned through the mount-less [`spawn`]. Cloned per
    /// call, same as [`Self::identity`]: an occasional request/response verb, never per-publish
    /// work.
    pub fn mounts(&self) -> Vec<vike_tradehub_client::wire::WireMountRow> {
        self.shared.mounts.clone()
    }

    /// The number of currently-registered subscribers (test/diagnostics).
    pub fn subscriber_count(&self) -> usize {
        self.shared.reg.lock().expect("registry poisoned").subs.len()
    }

    /// Stop the publisher thread and release every subscriber. Idempotent: only the first call joins
    /// the thread; later calls (or calls on another clone) are no-ops.
    pub fn shutdown(&self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.lock().expect("join lock poisoned").take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Response`/`write_frame`/`project` come in via `super::*`; only `read_frame` is extra here.
    use vike_tradehub_client::proto::read_frame;

    /// The identity block is threaded through the projection verbatim (split-plane B3), and its
    /// absence stays absent — an identity-less node publishes exactly the old shape.
    #[test]
    fn project_threads_the_identity_through() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let id = WireNodeIdentity {
            name: "n".into(),
            strategy: "s".into(),
            params: "p".into(),
            live: false,
            build: "b".into(),
        };
        let wire = project(&snap, Some(&id));
        assert_eq!(wire.identity.as_ref().unwrap().strategy, "s");
        assert!(project(&snap, None).identity.is_none());
    }

    /// The projection carries the rendered fields the GUI needs: seq, venue/symbol, trading state,
    /// equity, and each order (with its status rendered to a string).
    #[test]
    fn project_maps_the_rendered_fields() {
        let mut snap = CoreSnapshot::empty("polymarket", "TOK");
        snap.seq = 7;
        snap.trading_state = TradingState::Reducing;
        snap.portfolio.equity_total = 1234.5;
        snap.orders.push(OrderView {
            client_order_id: "c-1".into(),
            venue: "polymarket".into(),
            symbol: "TOK".into(),
            side: 1,
            qty: 20.0,
            order_type: "limit".into(),
            price: Some(0.4),
            trigger_price: None,
            status: vike_exec::OrderStatus::Accepted,
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        });

        let wire = project(&snap, None);
        assert_eq!(wire.seq, 7);
        assert_eq!(wire.venue, "polymarket");
        assert_eq!(wire.symbol, "TOK");
        assert_eq!(wire.trading_state, WireTradingState::Reducing);
        assert_eq!(wire.equity_total.to_bits(), 1234.5_f64.to_bits());
        assert_eq!(wire.orders.len(), 1);
        assert_eq!(wire.orders[0].client_order_id, "c-1");
        assert_eq!(wire.orders[0].status, "Accepted", "status is rendered to its {{:?}} string");
    }

    /// A framed snapshot decodes back into a `Response::SnapshotFrame` whose wire matches the direct
    /// projection — the "serialize once" bytes are a valid frame.
    #[test]
    fn frame_snapshot_round_trips_as_a_snapshot_frame() {
        let mut snap = CoreSnapshot::empty("sim", "BTCUSDT");
        snap.seq = 3;
        let bytes = frame_snapshot(&snap, None);
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        match read_frame::<_, Response>(&mut cur).expect("decode frame") {
            Response::SnapshotFrame(w) => {
                assert_eq!(w.seq, 3);
                assert_eq!(*w, project(&snap, None));
            }
            other => panic!("expected SnapshotFrame, got {other:?}"),
        }
    }

    /// The mailbox is bounded and drops the OLDEST frame when full — never the newest, never blocking.
    #[test]
    fn mailbox_drops_oldest_when_full() {
        let mb = Mailbox::new(2);
        let f = |n: u8| Arc::new(vec![n]);
        mb.push(f(1));
        mb.push(f(2));
        mb.push(f(3)); // drops the oldest (1)
        match mb.recv_timeout(Duration::from_millis(0)) {
            Recv::Frame(b) => assert_eq!(*b, vec![2], "oldest (1) was dropped, 2 is next"),
            _ => panic!("expected a frame"),
        }
        match mb.recv_timeout(Duration::from_millis(0)) {
            Recv::Frame(b) => assert_eq!(*b, vec![3]),
            _ => panic!("expected a frame"),
        }
        // empty now -> a zero timeout reports Timeout, not Closed.
        assert!(matches!(mb.recv_timeout(Duration::from_millis(0)), Recv::Timeout));
        mb.close();
        assert!(matches!(mb.recv_timeout(Duration::from_millis(0)), Recv::Closed));
    }
}
