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

/// Cap on the NUMBER of series ONE published frame carries — [`CHART_BARS_CAP`]'s other half.
///
/// The bar lane costs `series × CHART_BARS_CAP` candles re-serialized on EVERY `seq` bump (~38/s on
/// the live node), so the frame needs a bound in BOTH dimensions. The venue/symbol filter this
/// replaced was supplying the second one by accident — while being wrong about which series it
/// kept, see [`project_bar_series`] — so removing it without putting a real bound in its place
/// would trade a silent emptiness for an unbounded frame.
///
/// 16 × 300 candles is ~0.4 MB of JSON per frame: far under `MAX_FRAME_LEN` (64 MiB, which
/// [`frame_snapshot`] `.expect()`s), and far above any mount table this daemon wires. It is a
/// CEILING over a set already bounded by the feeds a mount subscribed, not an expected working
/// limit — a node that reaches it holds more series than a human is charting.
const MAX_BAR_SERIES: usize = 16;

/// One [`Bar`] → its wire OHLCV mirror.
fn project_bar(b: &Bar) -> WireBar {
    WireBar { ts: b.ts, o: b.open, h: b.high, l: b.low, c: b.close, v: b.volume }
}

/// Warn ONCE that a frame dropped series past [`MAX_BAR_SERIES`]. The publisher re-projects at the
/// publish cadence, so an unconditional warn would be a log flood — and a dropped series is a
/// standing configuration fact, not an event, so the first line says everything a later one would.
/// It warns AT ALL because a silently-missing bar series is precisely the failure this function was
/// fixed for.
fn warn_series_truncated(total: usize) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        tracing::warn!(
            series = total,
            cap = MAX_BAR_SERIES,
            "this node holds more bar series than one published frame carries; the excess is \
             dropped from the observer chart lane (mounted series are kept first)"
        );
    }
}

/// Project the core's bar cache to bounded wire series for the observer chart: each series'
/// `closed` tail-sliced to the last [`CHART_BARS_CAP`] plus the forming candle. Read-only over the
/// Arc-shared cache — no fold, no clone of the whole history.
///
/// ⚠ **This filtered on `snap.venue`/`snap.symbol` and its own doc called that "the PRIMARY mounted
/// `(venue, symbol)`". Both halves were false, and together they published `bars: []` on every live
/// daemon, every frame, forever.** Those two scalars mirror the primary ENGINE, not a mount, and
/// the primary engine is a HARDCODED CONSTANT: `build_node` passes
/// `crates/vike-run/src/node.rs`'s `BINANCE_MARKET` to its first `mount_accounts_of` call
/// unconditionally, so `snap.venue` is `"binance"` on every node it builds regardless of what is
/// mounted. A daemon mounting bybit wires exactly one kline feed, the core's cache holds exactly
/// `("bybit", …)`, and the filter compared it against `"binance"`.
/// (`crates/vike-core/src/snapshot.rs`'s
/// `the_primary_mirroring_scalars_cannot_see_a_non_primary_engines_fills` machine-checks the same
/// asymmetry for the LEDGER scalars — nobody had carried it to the bar lane.)
///
/// **So there is no `(venue, symbol)` filter any more, and its absence is the fix rather than a
/// relaxation of one.** The core's bar cache holds one entry per series a FEED subscribed, and the
/// feeds are wired from the mount table (`crates/vike-tradehub/src/feeds.rs`'s `wire_venue_feeds`)
/// — the key set is already "the bars this node actually has". Publishing it whole is also the
/// only shape a MULTI-mount daemon can be right about: any filter that can emit only ONE
/// `(venue, symbol)` reproduces this
/// same defect one mount later, so replacing the primary-engine pair with, say, `mounts[0]` would
/// have fixed the live box and left the bug in the design.
///
/// `snap.mounts` is still read — but for ORDERING, never as a filter. Truncation to
/// [`MAX_BAR_SERIES`] keeps the `(venue, symbol)` pairs a [`vike_core::MountRowKind::Mount`] row
/// names FIRST, so if the cap ever bites, what survives is what a strategy trades rather than
/// whatever a feed happened to subscribe first. Using it as a filter would fail CLOSED — a core
/// with feeds and no mount (a bare data node, a harness) would publish nothing, which is this
/// defect in a new disguise.
fn project_bar_series(snap: &CoreSnapshot) -> Vec<WireBarSeries> {
    let mounted: std::collections::HashSet<(&str, &str)> = snap
        .mounts
        .iter()
        .filter(|m| m.kind == vike_core::MountRowKind::Mount)
        .map(|m| (m.venue.as_str(), m.symbol.as_str()))
        .collect();
    let mut rows: Vec<_> = snap.bars.iter().collect();
    // `sort_by_key` is STABLE, so the cache's own insertion order survives inside each group and a
    // frame's series order is unchanged for every node that never reaches the cap.
    rows.sort_by_key(|(k, _)| !mounted.contains(&(k.0.as_str(), k.1.as_str())));
    if rows.len() > MAX_BAR_SERIES {
        warn_series_truncated(rows.len());
        rows.truncate(MAX_BAR_SERIES);
    }
    rows.into_iter()
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

    /// **The VENUES this node runs an ENGINE for** — the set a wire command's addressed venue
    /// (`vike_tradehub_client::wire::WireCommand::addressed_venue`) is checked against before it is
    /// lowered ([`crate::server::venue_refusal`]), read off the arc-swap snapshot cell PER CALL
    /// exactly like [`Self::snapshot_frame_now`] (a read, never the fold).
    ///
    /// One entry per ENGINE, in the core's own registration order (primary first, then extras) —
    /// `vike_core::Portfolio::venues` is documented as "one block per engine". ⚠ Two engines of
    /// ONE exchange (a `[accounts]` box) therefore contribute the SAME string twice, and that is
    /// left as it is rather than deduplicated: this answers "does this node run `v` at all", which
    /// is the only question a venue-keyed address can pose, and collapsing it would quietly imply
    /// the wire could tell the two accounts apart. It cannot: naming ONE ACCOUNT of a venue is a
    /// separate, designed-elsewhere change to what the wire carries, and this method is
    /// deliberately not a down payment on it.
    ///
    /// **EMPTY means "this core has not published yet", NOT "this node runs no engines"** — every
    /// built snapshot carries at least the primary block, so the empty answer is reachable only
    /// before the first `publish` (the core publishes when its state goes dirty, so a feed-less
    /// paper daemon can sit there for a while). A caller must therefore treat empty as UNKNOWN and
    /// never as a refusal set: refusing against it would deadlock such a daemon permanently, since
    /// a refused command never reaches the core and so never makes it dirty. `venue_refusal` says
    /// the same thing from the other side.
    ///
    /// Command cadence, never the per-message fold: one `Vec` per operator action.
    pub fn engine_venues(&self) -> Vec<String> {
        self.shared.cell.load().portfolio.venues.iter().map(|v| v.venue.clone()).collect()
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

    /// The mount rows the LIVE core reports — `(venue, symbol, interval, typed params)` per mount,
    /// in the core's own mount order — read off the arc-swap snapshot cell PER CALL, exactly like
    /// [`Self::snapshot_frame_now`] (a read, never the fold).
    ///
    /// ⚠ Distinct from [`Self::mounts`], and the distinction is the whole defect being fixed: that
    /// one is the PROCESS-STATIC boot block, cloned off a value captured before the first tick, so
    /// its rendered params string is stale the moment an `UpdateParams` lands. This one is what each
    /// mount holds NOW, which is the only thing a read-modify-write may be built on.
    ///
    /// The `Option` is the mount's own answer (`vike_model::Strategy::params`' default), NOT a
    /// failure: a mount that publishes no typed params is a mount `UpdateParams` cannot address
    /// either. The RESIDUAL row is filtered out here — it describes no mount, carries an empty key,
    /// and would otherwise misalign an order-matched overlay.
    ///
    /// An OCCASIONAL request/response verb like the two above: `StrategyStatus` is not the push
    /// stream, so serializing N mounts' params per call is not the constraint.
    pub fn live_mount_params(&self) -> Vec<(String, String, String, Option<serde_json::Value>)> {
        self.shared
            .cell
            .load_full()
            .mounts
            .iter()
            .filter(|m| m.kind == vike_core::MountRowKind::Mount)
            .map(|m| {
                (
                    m.venue.clone(),
                    m.symbol.clone(),
                    m.interval.clone(),
                    // `StrategyParams` is a plain serde union; a value that somehow could not be
                    // serialized is reported as "no typed params" rather than failing the whole
                    // status read — the same degrade the `None` default already means, and the
                    // capability string is what tells a client the node CAN answer at all.
                    m.params.as_ref().and_then(|p| serde_json::to_value(p).ok()),
                )
            })
            .collect()
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
            advertise_addr: String::new(),
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

    // -----------------------------------------------------------------------------------------
    // The bar lane (`project_bar_series`). ⚠ Until these landed, NOTHING in this workspace built a
    // `CoreSnapshot` carrying bars and asserted what the projection emits: the only other
    // `WireBarSeries` sites are client-side hand-built round-trips, which never reach this filter.
    // That is exactly how a structurally empty bar lane shipped with every test green.
    // -----------------------------------------------------------------------------------------

    fn bar(ts: i64, close: f64) -> Bar {
        Bar {
            ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn series(closed: Vec<Bar>, forming: Option<Bar>) -> vike_exec::BarSeries {
        vike_exec::BarSeries { closed: Arc::new(closed), forming }
    }

    fn mount_row(venue: &str, symbol: &str, interval: &str) -> vike_core::MountView {
        vike_core::MountView {
            kind: vike_core::MountRowKind::Mount,
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            ready: true,
            position: 0.0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            notional: 0.0,
            budget: None,
            latched: false,
            params: None,
        }
    }

    /// ⚠ **THE LIVE CONFIGURATION, and the case nothing covered.** The daemon mounts bybit;
    /// `vike_run::build_node` makes its hardcoded binance paper engine the PRIMARY, so
    /// `CoreSnapshot::venue` reads `"binance"` while the only series the core's bar cache holds is
    /// the bybit feed's. The old projection compared the two and published `bars: []` — every
    /// frame, forever, on a daemon that was otherwise healthy and publishing ~38 snapshots/second.
    #[test]
    fn a_bybit_mount_under_a_binance_primary_publishes_its_bars() {
        // `empty`'s arguments ARE the primary engine's (venue, symbol) — the untraded binance one.
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.mounts.push(mount_row("bybit", "BTCUSDT", "1m"));
        snap.bars.insert(
            ("bybit".into(), "BTCUSDT".into(), "1m".into()),
            series(vec![bar(1, 100.0), bar(2, 101.0)], Some(bar(3, 102.0))),
        );

        let wire = project(&snap, None);
        assert_eq!(wire.venue, "binance", "the primary-mirroring scalars are untouched by the fix");
        assert_eq!(
            wire.bars.len(),
            1,
            "the MOUNTED series must be published — this is the whole defect: a bybit-mounted, \
             binance-primary daemon published an empty bar lane"
        );
        let s = &wire.bars[0];
        assert_eq!(
            (s.venue.as_str(), s.symbol.as_str(), s.interval.as_str()),
            ("bybit", "BTCUSDT", "1m")
        );
        assert_eq!(s.closed.len(), 2);
        assert_eq!(
            s.forming.as_ref().expect("the forming candle rides along").c.to_bits(),
            102.0_f64.to_bits()
        );
    }

    /// A MULTI-mount daemon publishes EVERY series it holds, not one. This is the half that a
    /// "filter to `mounts[0]`" repair would have left broken — it would have made the live box work
    /// and kept the defect one mount later.
    #[test]
    fn every_mounted_series_is_published_not_just_one() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.mounts.push(mount_row("bybit", "BTCUSDT", "1m"));
        snap.mounts.push(mount_row("okx", "BTC-USDT", "5m"));
        snap.bars.insert(
            ("bybit".into(), "BTCUSDT".into(), "1m".into()),
            series(vec![bar(1, 1.0)], None),
        );
        snap.bars.insert(
            ("okx".into(), "BTC-USDT".into(), "5m".into()),
            series(vec![bar(1, 2.0)], None),
        );

        let wire = project(&snap, None);
        let mut got: Vec<(&str, &str)> =
            wire.bars.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())).collect();
        got.sort_unstable();
        assert_eq!(got, vec![("bybit", "BTCUSDT"), ("okx", "BTC-USDT")]);
    }

    /// `mounts` ORDERS the frame, it never FILTERS it: a core with feeds and no strategy mount
    /// still publishes its bars. A mount-gated projection would be the same empty chart wearing a
    /// new argument.
    #[test]
    fn a_mount_less_core_still_publishes_its_bars() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.bars.insert(
            ("bybit".into(), "BTCUSDT".into(), "1m".into()),
            series(vec![bar(1, 1.0)], None),
        );
        assert!(snap.mounts.is_empty(), "no mount rows at all");
        assert_eq!(project(&snap, None).bars.len(), 1);
    }

    /// The frame is bounded in BOTH dimensions, and a truncated one keeps the MOUNTED series. The
    /// cap replaces the bounding the removed venue filter was doing by accident; the mounted-first
    /// ordering is what makes a truncated frame still the useful one. Every unmounted series is
    /// inserted FIRST here, so insertion order alone would have evicted the traded one.
    #[test]
    fn the_series_cap_bounds_the_frame_and_keeps_the_mounted_series_first() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        for i in 0..(MAX_BAR_SERIES + 4) {
            snap.bars.insert(
                ("binance".into(), format!("ALT{i}"), "1m".into()),
                series(vec![bar(1, 1.0)], None),
            );
        }
        snap.mounts.push(mount_row("bybit", "BTCUSDT", "1m"));
        snap.bars.insert(
            ("bybit".into(), "BTCUSDT".into(), "1m".into()),
            series(vec![bar(1, 9.0)], None),
        );

        let wire = project(&snap, None);
        assert_eq!(wire.bars.len(), MAX_BAR_SERIES, "the frame carries at most the series cap");
        assert_eq!(
            (wire.bars[0].venue.as_str(), wire.bars[0].symbol.as_str()),
            ("bybit", "BTCUSDT"),
            "the mounted series sorts FIRST, so a capped frame still carries what is traded"
        );
    }

    /// Each series is still tail-sliced to [`CHART_BARS_CAP`] — the per-series bound is unchanged
    /// by the filter's removal, and it keeps the TAIL (the recent candles), not the head.
    #[test]
    fn each_series_is_tail_sliced_to_the_chart_bars_cap() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let bars: Vec<Bar> = (0..(CHART_BARS_CAP as i64 + 10)).map(|i| bar(i, i as f64)).collect();
        snap.bars.insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(bars, None));

        let wire = project(&snap, None);
        assert_eq!(wire.bars[0].closed.len(), CHART_BARS_CAP);
        assert_eq!(wire.bars[0].closed[0].ts, 10, "the TAIL survives, not the head");
    }

    /// ⚠ **THE COMMAND-PLANE WIRE DID NOT MOVE, and that is Stage 1's whole compatibility
    /// claim.** `vike_core::snapshot::VenueBlock` gained `account` and `route_key`
    /// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1)
    /// and this projection deliberately forwards NEITHER — the snapshot mirror is §6.2's work,
    /// in the stage that gives the MCP gate a route key to compare against.
    ///
    /// So this test is a FENCE rather than a description: it fails the day somebody adds either
    /// field to `WireVenueBlock` without doing §6.2's consumer half, which is exactly the shape
    /// of change that would let a client believe it can address an account it cannot.
    #[test]
    fn the_published_venue_block_carries_no_account_field_yet() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![vike_core::VenueBlock {
            venue: "binance".into(),
            account: vike_model::account_keys::AccountLabel::parse("ALT").ok(),
            route_key: "binance#ALT".into(),
            ..Default::default()
        }];
        let json = serde_json::to_string(&project(&snap, None)).expect("the wire serializes");
        assert!(!json.contains("\"account\""), "no account key on the wire yet: {json}");
        assert!(!json.contains("route_key"), "…and no route key either: {json}");
        assert!(json.contains("\"binance\""), "the venue is published exactly as before: {json}");
    }
}
