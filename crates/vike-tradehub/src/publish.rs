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
//! ## The observer on the other end of the wire
//!
//! This module and the [`crate::server`] it feeds are the CI-testable core of PR-11. The GUI half
//! landed separately in #733 as `vike-app --observe`, and since the desktop lost its local core it
//! is the ONLY mode `vike-desktop` has: `crates/vike-app-core/src/backend/observe_bridge.rs`'s
//! `wire_to_core` turns each [`WireSnapshot`] this module projects back into a `CoreSnapshot` for
//! the panels, received over a `vike_tradehub_client::RemoteCoreHandle`. Nothing on that side is
//! wired here. ⚠ This section was headed "vike-app is a deferred follow-up" until 2026-09-28, two months
//! after the follow-up landed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
use vike_exec::{CoreSnapshot, HeldOrderView, OrderView, PositionView, TradingState, VenueBlock};
use vike_model::Bar;
use vike_tradehub_client::proto::{Response, write_frame};
use vike_tradehub_client::wire::{
    WireBar, WireBarSeries, WireEngineMode, WireHeldOrderView, WireNodeIdentity, WireOrderView,
    WirePositionView, WireSnapshot, WireTradingState, WireVenueBlock,
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
        // Rendered through `AccountLabel`'s own `Display`, like `project_venue`'s account.
        account: o.account.as_ref().map(ToString::to_string),
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
        // ⚠ Rendered through `AccountLabel`'s own `Display`, never by reaching into the enum: that
        // impl answers `DEFAULT` for the unlabelled account, which is the WIRE's spelling
        // (`vike_model::accounts::account_keys::parse_wire_account`) and precisely the one the
        // label `AccountLabel::parse` (an `account` row's label) refuses. Here the block already says which account it is by carrying `None`, so the
        // label text is only ever a real label — but rendering it any other way would be a second
        // place that decides how an account is spelled.
        account: v.account.as_ref().map(ToString::to_string),
        // The core's own key, copied rather than re-derived. `route_key_of(venue, account)` would
        // answer the same today and would be a second derivation of a routing identity — the
        // `venue#LABEL` trap `vike_exec::ExecutionEngine`'s `route_key` doc warns against.
        route_key: v.route_key.clone(),
        // The engine's symbols, primary first, and what stands behind it (the Trade window design,
        // §4.3). An empty primary (a block that names none) publishes an empty list: "not said".
        symbols: if v.symbol.is_empty() {
            Vec::new()
        } else {
            std::iter::once(v.symbol.clone()).chain(v.extra_symbols.iter().cloned()).collect()
        },
        mode: v.mode.map(|m| match m {
            vike_exec::EngineMode::Paper => WireEngineMode::Paper,
            vike_exec::EngineMode::Demo => WireEngineMode::Demo,
            vike_exec::EngineMode::Live => WireEngineMode::Live,
        }),
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
/// the primary engine is a HARDCODED CONSTANT: the first row of
/// `crates/vike-tradehub/src/wired_markets.rs`'s `WIRED_MARKETS` (`BINANCE_MARKET`), which
/// `build_node` mounts first and holds as its primary engine
/// unconditionally, so `snap.venue` is `"binance"` on every node it builds regardless of what is
/// mounted. A daemon mounting bybit wires exactly one kline feed, the core's cache holds exactly
/// `("bybit", …)`, and the filter compared it against `"binance"`.
/// (`crates/vike-core/src/snapshot/snapshot_tests.rs`'s
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
/// [`MAX_BAR_SERIES`] keeps the `(venue, symbol)` pairs a [`vike_exec::MountRowKind::Mount`] row
/// names FIRST, so if the cap ever bites, what survives is what a strategy trades rather than
/// whatever a feed happened to subscribe first. Using it as a filter would fail CLOSED — a core
/// with feeds and no mount (a bare data node, a harness) would publish nothing, which is this
/// defect in a new disguise.
fn project_bar_series(snap: &CoreSnapshot) -> Vec<WireBarSeries> {
    let mounted: std::collections::HashSet<(&str, &str)> = snap
        .mounts
        .iter()
        .filter(|m| m.kind == vike_exec::MountRowKind::Mount)
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
        // Copied, never recomputed here. `accounts_epoch_of` is the ONE digest and the core already
        // ran it over the engines themselves; a second call at the projection edge would be a
        // second answer to "which accounts does this node mount", computed from a different vantage
        // point at a different moment.
        accounts_epoch: snap.accounts_epoch,
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
#[derive(Debug)]
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

/// One mount as the LIVE core reports it: the id an `UpdateParams` addresses it by, its series key
/// and its typed params. What [`PublisherHandle::live_mount_params`] returns, in the core's mount
/// order.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveMountParams {
    /// `vike_exec::MountView::mount_id`: the stored, sanitized mount id.
    pub mount_id: String,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// The mount's own answer (`vike_model::Strategy::params`' default is `None`), as the core's
    /// serde JSON.
    pub typed_params: Option<serde_json::Value>,
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

/// The OPEN orders in a published snapshot's `orders` that `cmd` names — at most one, and only for a
/// `Modify`, the one verb that names a client order id and nothing else. Every other command gets an
/// empty `Vec`, which allocates nothing.
///
/// Open means NOT terminal (`vike_exec::CoreSnapshot::open_order`'s question over the same rows).
/// Filtering here, before the clone, is the point of the signature: `orders` is the whole registry
/// of every engine — nothing ever prunes a terminal order out of it — so cloning it per command
/// would grow with the node's age, while the one order a `Modify` names is a single small clone.
///
/// ONE function behind [`PublisherHandle::open_orders_named_by`] and the Telegram surface's twin
/// (`crate::telegram::deps`), so the two surfaces cannot disagree about which order a command names.
pub(crate) fn named_open_orders(
    cmd: &vike_tradehub_client::wire::WireCommand,
    orders: &[OrderView],
) -> Vec<OrderView> {
    let vike_tradehub_client::wire::WireCommand::Modify { client_order_id, .. } = cmd else {
        return Vec::new();
    };
    orders
        .iter()
        .filter(|o| o.client_order_id == *client_order_id && !o.status.is_terminal())
        .cloned()
        .collect()
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
    /// lowered ([`crate::server::refusal::venue_refusal`]), read off the arc-swap snapshot cell PER CALL
    /// exactly like [`Self::snapshot_frame_now`] (a read, never the fold).
    ///
    /// One entry per ENGINE, in the core's own registration order (primary first, then extras) —
    /// `vike_exec::Portfolio::venues` is documented as "one block per engine". ⚠ Two engines of
    /// ONE exchange (a box with a labelled account) therefore contribute the SAME string twice, and that is
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

    /// **The ROUTING KEYS this node runs an engine under** — one per ENGINE, same cell, same
    /// per-call read and same registration order as [`Self::engine_venues`], differing in exactly
    /// one thing: the field it reads. `vike_exec::VenueBlock::route_key` is
    /// `route_key_of(venue, account)` — the bare venue id for the DEFAULT account, `venue#LABEL`
    /// for a labelled one — and its own doc is the authority for why a gate compares against it
    /// and never against `venue`.
    ///
    /// ⚠ **This is a SIBLING of [`Self::engine_venues`], not a replacement for it**, and the two
    /// answer genuinely different questions rather than one question at two resolutions. That one
    /// asks *does this node run this EXCHANGE at all* — the only question a venue-keyed address can
    /// pose, and the one `crate::server::refusal::venue_refusal`'s exactness argument is written against.
    /// This one asks *does this node run THIS BOOK*, which is the question a two-account node makes
    /// answerable and a one-account node answers identically (the keys ARE the venues there, by
    /// construction — see `vike_model::accounts::account_keys::AccountRef::route_key`). Re-pointing the first
    /// at these keys would change what an existing refusal MEANS, so the gates are added beside
    /// each other: `crate::server::refusal::account_refusal` reads this one.
    ///
    /// ⚠ Duplicates are NOT possible here the way they are in [`Self::engine_venues`] — two engines
    /// of one exchange contribute the same `venue` twice and two DIFFERENT route keys, which is the
    /// whole of what this accessor buys. That is a property of the mount rather than of this
    /// method, so the refusal built on it still sorts and dedups what it PRINTS.
    ///
    /// **EMPTY means "this core has not published yet", NOT "this node runs no engines"** — the
    /// identical rule [`Self::engine_venues`] states at length, inherited here rather than
    /// re-argued, and inherited STRUCTURALLY: both read the same `portfolio.venues`, so they are
    /// empty together and no caller can see one roster populated while the other is not.
    ///
    /// Command cadence, never the per-message fold: one `Vec` per operator action.
    pub fn engine_route_keys(&self) -> Vec<String> {
        self.shared.cell.load().portfolio.venues.iter().map(|v| v.route_key.clone()).collect()
    }

    /// **The engine BLOCKS this node runs** — one per ENGINE, same cell, same per-call read and same
    /// registration order as [`Self::engine_route_keys`], for the one gate that needs more than a
    /// key: a bracket's (`crate::server::refusal::bracket_refusal`), which must know what the ONE engine a
    /// bracket reaches TRADES (`vike_exec::VenueBlock::trades`) and which lane that engine is
    /// mounted on, because a venue adapter places every order on its engine's own instrument.
    ///
    /// Whole blocks rather than a third projection, so the "does it trade this symbol" answer is
    /// `VenueBlock::trades` itself and not a second spelling of it. EMPTY on the same terms as the
    /// two siblings, and empty TOGETHER with them: all three read one `portfolio.venues`.
    ///
    /// Command cadence, never the per-message fold: one clone per operator action.
    pub fn engine_blocks(&self) -> Vec<vike_exec::VenueBlock> {
        self.shared.cell.load().portfolio.venues.clone()
    }

    /// **The OPEN order a `Modify` names**, off the same snapshot cell as [`Self::engine_blocks`] and
    /// per call like it — for the one gate that needs to know which instrument a bare client order
    /// id rests on: the notional ceiling (`crate::server::control::ControlLimits::vet`), which sizes
    /// a `Modify` with that instrument's contract multiplier. Empty for every other command, and
    /// empty for an order this snapshot does not hold (not published yet, terminal, never existed),
    /// which the ceiling reads as "unknown" and sizes at 1.0 exactly as it always did.
    ///
    /// ⚠ **No new read of the core**: `CoreSnapshot::orders` is built by the same coalesced publish
    /// as `portfolio.venues`, through the same arc-swap load. Reading the two in separate calls can
    /// straddle a publish, and that is harmless: the roster has exactly one transition (empty to
    /// published) and a multiplier grid is immutable after construction, so the worst a straddle
    /// yields is the order missing from an older snapshot — the unknown case above.
    ///
    /// Command cadence, never the per-message fold: one scan of the registry and at most one clone.
    pub fn open_orders_named_by(
        &self,
        cmd: &vike_tradehub_client::wire::WireCommand,
    ) -> Vec<vike_exec::OrderView> {
        named_open_orders(cmd, &self.shared.cell.load().orders)
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

    /// The mount rows the LIVE core reports — [`LiveMountParams`] (id, series key, typed params) per
    /// mount, in the core's own mount order — read off the arc-swap snapshot cell PER CALL, exactly like
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
    pub fn live_mount_params(&self) -> Vec<LiveMountParams> {
        self.shared
            .cell
            .load_full()
            .mounts
            .iter()
            .filter(|m| m.kind == vike_exec::MountRowKind::Mount)
            .map(|m| LiveMountParams {
                mount_id: m.mount_id.clone(),
                venue: m.venue.clone(),
                symbol: m.symbol.clone(),
                interval: m.interval.clone(),
                // `StrategyParams` is a plain serde union; a value that somehow could not be
                // serialized is reported as "no typed params" rather than failing the whole
                // status read — the same degrade the `None` default already means, and the
                // capability string is what tells a client the node CAN answer at all.
                typed_params: m.params.as_ref().and_then(|p| serde_json::to_value(p).ok()),
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
            // A `join` Err is a publisher panic its hook already reported; nothing to recover.
            let _ = join.join();
        }
    }
}

#[path = "publish_tests.rs"]
#[cfg(test)]
mod publish_tests;
