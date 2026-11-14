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
//! to a [`CoreSnapshotWire`], frames it ONCE into a shared `Arc<Vec<u8>>` (the "serialize once" win), and
//! pushes that Arc into every subscriber's [`PublisherMailbox`] — a per-connection BOUNDED, DROP-OLDEST queue.
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
//! `wire_to_core` turns each [`CoreSnapshotWire`] this module projects back into a `CoreSnapshot` for
//! the panels, received over a `vike_tradehub_client::TradehubCoreHandle`. Nothing on that side is
//! wired here. ⚠ This section was headed "vike-app is a deferred follow-up" until 2026-09-28, two months
//! after the follow-up landed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use vike_execution::{
    ArmingNotRunningView, CoreSnapshot, OrderEmulationStopView, OrderEmulationView, OrderHeldView,
    OrderView, PositionView, ReconcileAlertView, VenueBlock,
};
use vike_marketdata::Bar;
use vike_tradehub_client::proto::{Response, write_frame};
use vike_tradehub_client::wire::{
    ArmingNotRunningViewWire, BalanceWithheldWire, BarSeriesWire, BarWire, CoreSnapshotWire,
    HubIdentityWire, OrderEmulationStopViewWire, OrderEmulationViewWire, OrderHeldViewWire,
    OrderViewWire, PositionViewWire, PriceLinkDownWire, ReconcileAlertViewWire, TradeLinkDownWire,
    VenueBlockWire, VenueLinkDownWire,
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

/// How often the publisher looks whether a hub link moved ([`NodeReadings::link_mark`]). A look takes a lock per lease and
/// per price dial, so not at [`POLL_INTERVAL`]; a quarter of a second is nothing against an outage.
const LINK_CHECK_EVERY: Duration = Duration::from_millis(250);

/// While a link stays down, a quiet node still pushes a frame this often, so a pushed frame's `down_ms` does not stand
/// still at what the first frame of the outage said (a subscriber reads the last frame it got, not the node's clock).
const LINK_AGE_STEP_MS: u64 = 5_000;

/// [`NodeReadings::link_mark`]: the trade links `(route key, closed, age step)`, the price links `(venue, age step)`, the
/// venue paths down `(route key, why, failed restarts)` and those said to be up.
type LinkMark =
    (Vec<(String, bool, u64)>, Vec<(String, u64)>, Vec<(String, String, u32)>, Vec<String>);

/// **One reading of the hub links.** A frame carries one and the publisher keeps its mark, so what the next look is
/// compared with is what the last frame SAID ([`run_publisher`]), however that frame came to be framed.
#[derive(Default)]
struct LinkReading {
    trade: Vec<TradeLinkDownWire>,
    price: Vec<PriceLinkDownWire>,
    path: Vec<VenueLinkDownWire>,
    path_up: Vec<String>,
}

impl LinkReading {
    fn mark(&self) -> LinkMark {
        (
            self.trade
                .iter()
                .map(|l| (l.route_key.clone(), l.closed, l.down_ms / LINK_AGE_STEP_MS))
                .collect(),
            self.price.iter().map(|l| (l.venue.clone(), l.down_ms / LINK_AGE_STEP_MS)).collect(),
            self.path
                .iter()
                .map(|l| (l.route_key.clone(), l.why.clone(), l.failed_restarts))
                .collect(),
            self.path_up.clone(),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Projection: CoreSnapshot -> CoreSnapshotWire (the rendered display subset the GUI needs).
// ---------------------------------------------------------------------------------------------

/// Project one [`PositionView`] to its wire mirror (the display-relevant subset; the core's
/// `mark_source`/`margin_mode`/`isolated_margin` stay core-side).
fn project_position(p: &PositionView) -> PositionViewWire {
    PositionViewWire {
        venue: p.venue.as_str().to_owned(),
        symbol: p.symbol.as_str().to_owned(),
        position_side: p.position_side.to_string(),
        size: p.size,
        average_price: p.average_price,
        unrealized: p.unrealized,
        leverage: p.leverage,
        liquidation_price: p.liquidation_price,
    }
}

/// Project one [`OrderView`] to its wire mirror. `status` is rendered to the FSM word
/// (`OrderStatus::as_str`, e.g. `FILLED`) at this edge so the wire crate need not mirror the
/// `OrderStatus` enum (per [`OrderViewWire`]'s contract). Nodes before this one sent the Debug
/// spelling (`Filled`), so every reader of the field accepts BOTH: `vike-app-core`'s
/// `observe_bridge` round-trip tests pin each spelling decoding back identically for every variant.
fn project_order(o: &OrderView) -> OrderViewWire {
    OrderViewWire {
        order_client_id: o.order_client_id.clone(),
        venue: o.venue.clone(),
        // Rendered through `AccountLabel`'s own `Display`, like `project_venue`'s account.
        account: o.account.as_ref().map(ToString::to_string),
        symbol: o.symbol.clone(),
        side: o.side,
        qty: o.qty,
        order_type: o.order_type.as_str().to_owned(),
        price: o.price,
        trigger_price: o.trigger_price,
        status: o.status.as_str().to_owned(),
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        average_fill_price: o.average_fill_price,
        data_delay: o.data_delay.map(|d| d.word().to_owned()),
        reason: o.reason.clone(),
        // Stamped by `NodeReadings::stamp` in `frame_snapshot`: the core holds no clock for an order.
        age_ms: None,
        // A trailing order's terms; on any other kind a `trail` keeps its backtest-only meaning and is not
        // shown, so every non-trailing row is the frame from before.
        trail: o.trail.filter(|_| o.order_type.is_trailing()),
        limit_offset: o.limit_offset.filter(|_| o.order_type.is_trailing()),
        // The attributes wave's readings, each absent when `None`, so an order without them is the frame from before.
        attributes: o.attributes,
        trail_percent: o.trail_percent.filter(|_| o.order_type.is_trailing()),
        adjust: o.adjust,
        release: o.release.map(|r| project_release(r, &o.symbol)),
        display_quantity: o.display_quantity,
    }
}

/// A release as its wire mirror: the price condition as a side and a level, and its symbol only when it reads another
/// than `order_symbol`. ⚠ NEVER the condition's price venue: `OrderReleaseWire::venue` is a SUBMIT's key, and an older
/// client's `deny_unknown_fields` there would fail its whole frame (a held row says it as `release_venue`).
fn project_release(
    r: vike_model::OrderRelease,
    order_symbol: &str,
) -> vike_tradehub_client::wire::OrderReleaseWire {
    use vike_tradehub_client::wire::OrderReleasePriceWire;
    vike_tradehub_client::wire::OrderReleaseWire {
        at: r.at,
        price: r.price.map(|c| match c.side {
            vike_model::OrderReleasePriceSide::Above => OrderReleasePriceWire::Above(c.level),
            vike_model::OrderReleasePriceSide::Below => OrderReleasePriceWire::Below(c.level),
        }),
        symbol: r.price.map(|c| c.symbol.to_string()).filter(|s| s != order_symbol),
        venue: None,
    }
}

/// Project one held order (a bracket exit, or an order held until its release) to its wire mirror: the account as
/// an order row's, the release with its price condition, the condition's symbol when it reads another than the
/// order's own (a strategy's order held on another symbol's price shows that symbol), and, as READINGS beside the
/// release, the venue it reads when that is not the order's own (`release_venue`) and why it still waits; its terms
/// as [`project_order`] projects a resting order's.
fn project_held(h: &OrderHeldView) -> OrderHeldViewWire {
    let release = h.release.map(|r| project_release(r, &h.symbol));
    OrderHeldViewWire {
        order_client_id: h.order_client_id.clone(),
        venue: h.venue.clone(),
        symbol: h.symbol.clone(),
        side: h.side,
        qty: h.qty,
        order_type: h.order_type.as_str().to_owned(),
        price: h.price,
        trigger_price: h.trigger_price,
        parent_order_id: h.parent_order_id.clone(),
        // Rendered through `AccountLabel`'s own `Display`, like `project_order`'s account.
        account: h.account.as_ref().map(ToString::to_string),
        release,
        waiting: h.waiting.clone(),
        release_venue: h.release.and_then(|r| r.price?.venue).map(|v| v.to_string()),
        attributes: h.attributes,
        trail_percent: h.trail_percent.filter(|_| h.order_type.is_trailing()),
        adjust: h.adjust,
        display_quantity: h.display_quantity,
    }
}

/// Project one armed core-held reduce-only stop to its wire mirror.
fn project_core_held_stop(s: &OrderEmulationStopView) -> OrderEmulationStopViewWire {
    OrderEmulationStopViewWire {
        arm_id: s.arm_id.clone(),
        venue: s.venue.clone(),
        symbol: s.symbol.clone(),
        account: s.route_key.clone(),
        side: s.side,
        qty: s.qty,
        trigger_price: s.trigger_price,
        held: s.held,
        rearms: s.rearms,
    }
}

/// Project one emulated order (decision 0275) to its wire mirror, field for field; the account through
/// `AccountLabel`'s own `Display`, like [`project_order`]'s.
fn project_emulated(e: &OrderEmulationView) -> OrderEmulationViewWire {
    OrderEmulationViewWire {
        id: e.id.clone(),
        venue: e.venue.clone(),
        account: e.account.as_ref().map(ToString::to_string),
        symbol: e.symbol.clone(),
        side: e.side,
        qty: e.qty,
        kind: e.kind.to_string(),
        reduce_only: e.reduce_only,
        trigger: e.trigger,
        price: e.price,
        trail: e.trail,
        peg: e.peg.map(|p| vike_tradehub_client::wire::OrderPegWire {
            anchor: p.anchor.as_word().to_string(),
            offset: p.offset,
        }),
        display_quantity: e.display_quantity,
        filled: e.filled,
        average_price: e.average_price,
        child: e.child.clone(),
        waiting: e.waiting.clone(),
        rearms: e.rearms,
        held: e.held,
    }
}

/// Project one per-venue [`VenueBlock`] to its wire mirror (numbers a GUI renders; the heavy Arc
/// multiplier grid / fee schedule / margin math stay core-side).
fn project_venue(v: &VenueBlock) -> VenueBlockWire {
    VenueBlockWire {
        // What a client stamps into an id for this account (decision 0270), copied: absent while not said.
        account_code: v.account_code.map(|c| c.as_str().to_string()),
        order_client_id_shape: v.order_client_id_shape,
        venue: v.venue.clone(),
        balance: v.balance,
        realized_pnl: v.realized_pnl,
        fees_paid: v.fees_paid,
        funding_paid: v.funding_paid,
        equity: v.equity,
        unrealized: v.unrealized,
        missing_prices: v.missing_prices,
        margin_used: v.margin_used,
        free_buying_power: v.free_buying_power,
        trading_state: v.trading_state,
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
        // `venue#LABEL` trap `vike_execution::Engine`'s `route_key` doc warns against.
        route_key: v.route_key.clone(),
        // The engine's symbols, primary first, and its account's tier (the Trade window design,
        // §4.3). An empty primary (a block that names none) publishes an empty list: "not said".
        symbols: if v.symbol.is_empty() {
            Vec::new()
        } else {
            std::iter::once(v.symbol.clone()).chain(v.extra_symbols.iter().cloned()).collect()
        },
        // The tier's WORD, through its one spelling (`AccountTier::as_str`): the wire links no
        // `vike-model` (decision 0091 §3), so no second enum of the same three words stands here.
        tier: v.tier.map(|t| t.as_str().to_owned()),
        // The grid's non-default rows (the grid holds only those): the kind as its serde word, so
        // this crate names no `vike-model` type on the wire. Empty for every venue but deribit's
        // inverse perpetuals and ibkr's multiplier futures.
        contract_values: v
            .contract_values
            .iter()
            .map(|(symbol, value)| vike_tradehub_client::wire::ContractValueWire {
                symbol: symbol.clone(),
                kind: match value.kind {
                    vike_model::NotionalKind::Linear => "linear",
                    vike_model::NotionalKind::Inverse => "inverse",
                }
                .to_owned(),
                per_unit: value.per_unit,
            })
            .collect(),
        // The capability, copied: the engine's flag was set by the mount from the door's own
        // table (`vike_catalog::SYMBOL_RULES`), so the door and this frame agree by
        // construction (`admits_any_symbol`'s `the_published_capability_is_the_doors_admit_set`).
        admits_symbols: v.admits_symbols,
        // The conversion rates of the currencies the account holds a symbol quoted in (decision 0183,
        // phase 1), with their age and staleness: empty for every account on a venue quoted in its own
        // currency, which writes no key. `contract_values` above already carries the rate multiplied in.
        quote_rates: v
            .quote_rates
            .iter()
            .map(|r| vike_tradehub_client::wire::QuoteRateViewWire {
                currency: r.currency.to_string(),
                rate: r.rate,
                as_of_ms: r.as_of_ms,
                stale: r.stale,
                held: r.held,
            })
            .collect(),
        quote_rates_in: v.quote_rates_in.to_string(),
        // The currency group of the block's figures (decision 0183, item 13): empty for the dollar
        // group, which writes no key, so a node of today publishes the block it always did.
        ledger_currency: v.ledger_currency.to_string(),
        // Whether that currency is only the owner's assumed USD (decision 0189), absent when false.
        currency_assumed: v.currency_assumed,
        balance_is_own_book: v.balance_mode == vike_execution::BalanceMode::Delta,
    }
}

/// The engine blocks a frame carries. The core's list only grows (decision 0169 item 7), so an account that
/// stopped and started again (a deactivation and an activation, a tier change) has TWO blocks under one route
/// key: the stopped engine's, and the new one's after it. Every consumer resolves a block by route key and takes
/// the first, which would be the stale one, so the frame carries the NEWEST block of each key. A block that
/// names no route key is never judged against another. A stopped account that did NOT start again keeps its
/// block: its position is still valued and still shown (item 5).
pub(crate) fn current_venue_blocks(blocks: &[VenueBlock]) -> impl Iterator<Item = &VenueBlock> {
    blocks.iter().enumerate().filter_map(move |(i, block)| {
        let superseded = !block.route_key.is_empty()
            && blocks[i + 1..].iter().any(|later| later.route_key == block.route_key);
        (!superseded).then_some(block)
    })
}

/// The NOT RUNNING row of the account under `route_key`: the node-side twin of
/// `vike_tradehub_client::wire::CoreSnapshotWire::stopped_row`, whose doc says why the key alone decides.
pub(crate) fn stopped_row<'a>(
    not_running: &'a [ArmingNotRunningView],
    route_key: &str,
) -> Option<&'a ArmingNotRunningView> {
    not_running.iter().find(|row| row.route_key() == route_key)
}

/// The route keys of the engines that RUN: the newest block of each key ([`current_venue_blocks`]), minus the
/// accounts in `not_running` (their stopped engine's block stays published, decision 0169 item 5).
/// [`PublisherHandle::engine_route_keys`] reads it.
pub(crate) fn running_route_keys(
    blocks: &[VenueBlock],
    not_running: &[vike_execution::ArmingNotRunningView],
) -> Vec<String> {
    current_venue_blocks(blocks)
        .filter(|block| stopped_row(not_running, &block.route_key).is_none())
        .map(|block| block.route_key.clone())
        .collect()
}

/// The cross-venue equity of the blocks a frame carries ([`current_venue_blocks`]), which is NOT always the
/// core's `Portfolio::equity_total`. The core sums EVERY engine it ever held (a stop is "a zero step" for its
/// drawdown curve, `vike_core`'s `stop_engine_hot`), so an account that stopped and started again adds its stopped
/// engine's last equity to the new engine's: the venue wallet both adopted is counted twice, and the figure
/// stops being the sum of the rows beside it. Measured on the CI box (drill `n078-B1`, 2026-10-19): one deactivate and
/// activate of binance's demo account moved `equity_total` by the spot wallet's 4984.80 on top of the twin's own
/// 4996.01 (`wallet_venues` named `binance` three times). With nothing superseded this is the core's figure
/// bit for bit, so a node that never restarted an account publishes the number it always did. A stopped account
/// that did not start again keeps its block, as the rows do (decision 0169 item 5).
///
/// ONE currency, as the core's figure is: the PRIMARY block's group (`Portfolio::primary_currency`), because the
/// blocks of an engine in another currency are not in the primary's (decision 0183, item 13). A frame's other
/// groups are read off its blocks' `ledger_currency`.
pub(crate) fn current_equity_total(portfolio: &vike_execution::Portfolio) -> f64 {
    if current_venue_blocks(&portfolio.venues).count() == portfolio.venues.len() {
        portfolio.equity_total
    } else {
        let primary = portfolio.primary_currency();
        vike_model::py_sum(
            current_venue_blocks(&portfolio.venues)
                .filter(|v| v.ledger_currency == primary)
                .map(|v| v.equity),
        )
    }
}

/// Project one account that does NOT run (decision 0156) to its wire mirror: the reason as the node
/// words it, the block as its KEY (`ArmingBlock::word`), the tier its row's word (each absent when
/// none is stated). The wire still carries the venue, the label and the route key as text: same
/// bytes as when the view held them loose (`publish_tests.rs`'s
/// `pinned_a_healthy_frame_is_byte_identical` and
/// `pinned_a_labelled_not_running_row_is_byte_identical`).
fn project_not_running(n: &ArmingNotRunningView) -> ArmingNotRunningViewWire {
    ArmingNotRunningViewWire {
        venue: n.account.venue().to_string(),
        // The label's text, absent for the default account (the view never held its `DEFAULT`).
        account: n.account.label_text().map(str::to_string),
        route_key: n.route_key(),
        tier: n.tier.map(|t| t.as_str().to_owned()),
        reason: n.reason.clone(),
        block_word: n.block,
        // Stamped by `NodeReadings::stamp`: the core's view of the account holds only the word.
        detail: None,
    }
}

/// Project one HELD reconcile divergence to its wire mirror (`trade recon ls`). The proposed events
/// themselves never leave the core: only their count does.
fn project_reconcile_alert(a: &ReconcileAlertView) -> ReconcileAlertViewWire {
    ReconcileAlertViewWire {
        id: a.id,
        account: a.account.clone(),
        kind: a.kind.clone(),
        detail: a.detail.clone(),
        proposed_event_count: a.proposed_event_count,
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
fn project_bar(b: &Bar) -> BarWire {
    BarWire { timestamp: b.timestamp, o: b.open, h: b.high, l: b.low, c: b.close, v: b.volume }
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
/// the primary engine is a HARDCODED CONSTANT: the first engine row of
/// `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` (the binance row), which
/// `assemble_runtime` mounts first and holds as its primary engine
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
/// [`MAX_BAR_SERIES`] keeps the `(venue, symbol)` pairs a [`vike_execution::MountViewKind::Mount`] row
/// names FIRST, so if the cap ever bites, what survives is what a strategy trades rather than
/// whatever a feed happened to subscribe first. Using it as a filter would fail CLOSED — a core
/// with feeds and no mount (a bare data node, a harness) would publish nothing, which is this
/// defect in a new disguise.
fn project_bar_series(snap: &CoreSnapshot) -> Vec<BarSeriesWire> {
    let mounted: std::collections::HashSet<(&str, &str)> = snap
        .mounts
        .iter()
        .filter(|m| m.kind == vike_execution::MountViewKind::Mount)
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
            BarSeriesWire {
                venue: k.0.clone(),
                symbol: k.1.clone(),
                interval: k.2.clone(),
                closed: closed[start..].iter().map(project_bar).collect(),
                forming: series.forming.as_ref().map(project_bar),
            }
        })
        .collect()
}

/// Project a rendered [`CoreSnapshot`] to the standalone [`CoreSnapshotWire`] the node publishes. Pure
/// over its arguments — a read-only mapping of the display fields, never a fold. This is the ONE
/// place the core snapshot shape crosses into the versioned wire schema. `identity` is the node's
/// process-static identity block (split-plane B3), threaded from [`spawn`]'s caller; `None`
/// publishes the pre-identity shape byte-identically.
///
/// `not_running` is the node's process-static list of the accounts it does NOT run (decision 0156;
/// `vike_tradehub_core::RuntimeAssembly::not_running`), attached HERE, off the fold: the core publishes none of them
/// (`vike_execution::CoreSnapshot::not_running`'s doc says why), so the frame's list is this one. Empty
/// writes no key, byte-identically to a node with every account running.
pub fn project(
    snap: &CoreSnapshot,
    identity: Option<&HubIdentityWire>,
    not_running: &[ArmingNotRunningView],
) -> CoreSnapshotWire {
    CoreSnapshotWire {
        identity: identity.cloned(),
        not_running: not_running.iter().map(project_not_running).collect(),
        // Stamped by `frame_snapshot`, off the fold, from the account watcher's list.
        key_changed: Vec::new(),
        // Stamped by `NodeReadings::stamp` from the lease registry's balance gates: the core holds none.
        balance_withheld: Vec::new(),
        // ...and the venue paths that are down or said to be up, stamped beside it from the lease registry.
        venue_link_down: Vec::new(),
        venue_path_up: Vec::new(),
        // ...and this node's own links to the other two hubs, from the same registry.
        trade_link_down: Vec::new(),
        price_link_down: Vec::new(),
        price_dials_up: None,
        // The operator's armed core-held reduce-only stops, which only the core knows: copied as the
        // core published them (an empty list is not on the wire).
        core_held_stops: snap.core_held_stops.iter().map(project_core_held_stop).collect(),
        // Every order the core emulates, one list (decision 0275), copied as published (none writes no key).
        emulated_orders: snap.emulated_orders.iter().map(project_emulated).collect(),
        reconcile: snap.reconcile.alerts.iter().map(project_reconcile_alert).collect(),
        // Copied, never recomputed here. `accounts_epoch_of` is the ONE digest and the core already
        // ran it over the engines themselves; a second call at the projection edge would be a
        // second answer to "which accounts does this node mount", computed from a different vantage
        // point at a different moment.
        accounts_epoch: snap.accounts_epoch,
        sequence: snap.sequence,
        venue: snap.venue.clone(),
        symbol: snap.symbol.clone(),
        trading_state: snap.trading_state,
        balance: snap.balance,
        equity_total: current_equity_total(&snap.portfolio),
        venues: current_venue_blocks(&snap.portfolio.venues).map(project_venue).collect(),
        orders: snap.orders.iter().map(project_order).collect(),
        positions: snap.positions.iter().map(project_position).collect(),
        held_exits: snap.held_exits.iter().map(project_held).collect(),
        // Arc<str> in-process -> owned String on the wire (schema unchanged)
        recent_events: snap.recent_events.iter().map(|s| s.to_string()).collect(),
        fault: snap.fault.clone(),
        bars: project_bar_series(snap),
        // The publisher's own readings of the node, stamped by `frame_snapshot` (the doc of
        // `NodeReadings`): this projection is pure over the core snapshot and carries none of them.
        uptime_ms: None,
        halt_sentinel: None,
        // The currency `equity_total` and every other total of this frame are in, STATED because the frame's
        // first block need not be the primary's (the newest block of each account is carried, so a primary
        // that restarted moves to the end). Empty, so absent from the frame, for the dollar group.
        primary_currency: snap.portfolio.primary_currency().to_string(),
        // Stamped by `frame_snapshot_reading`, when it is framed.
        published_ms: 0,
    }
}

/// Project + frame the snapshot ONCE into shareable wire bytes: a full `[u32 len][JSON]`
/// [`Response::SnapshotFrame`] frame every subscriber's writer can `write_all` verbatim (the
/// serialize-once fan-out, steal S9). `write_frame` into a `Vec` only fails on a serialize error or
/// an over-`MAX_FRAME_LEN` body; a coalesced snapshot is kilobytes, so this cannot fail in practice.
fn frame_snapshot(
    snap: &CoreSnapshot,
    identity: Option<&HubIdentityWire>,
    not_running: &[ArmingNotRunningView],
    key_changed: &[String],
    facts: &NodeReadings,
) -> Arc<Vec<u8>> {
    frame_snapshot_reading(snap, identity, not_running, key_changed, facts, facts.read_links())
}

/// [`frame_snapshot`] carrying `links`, a reading the caller also keeps the mark of.
fn frame_snapshot_reading(
    snap: &CoreSnapshot,
    identity: Option<&HubIdentityWire>,
    not_running: &[ArmingNotRunningView],
    key_changed: &[String],
    facts: &NodeReadings,
    links: LinkReading,
) -> Arc<Vec<u8>> {
    let mut wire = project(snap, identity, not_running);
    // Attached HERE, off the fold, like `not_running`: the account watcher's list of RUNNING accounts whose
    // key in the database is no longer the one their session started with (route keys, words only).
    wire.key_changed = key_changed.to_vec();
    // ...and the publisher's own readings of the node (uptime, the HALT sentinel, order ages), which the
    // core does not hold and `project` therefore cannot copy.
    facts.stamp_reading(&mut wire, snap, Instant::now(), links);
    // What a reader measures this frame's age by: the moment it was framed, on the publisher thread.
    wire.published_ms = vike_model::now_ms();
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Response::SnapshotFrame(Box::new(wire)))
        .expect("frame a coalesced snapshot (serialize cannot fail on a bounded snapshot)");
    Arc::new(buffer)
}

// ---------------------------------------------------------------------------------------------
// What the PUBLISHER knows about the node and the core does not.
// ---------------------------------------------------------------------------------------------

/// When this process started, as far as the daemon can tell: the FIRST call fixes it.
///
/// `tradehub_cli.rs`'s `run` asks for it before it reads its arguments, so the clock is the process's
/// own and not the publisher's: the publisher is spawned only once the accounts are up, which can be
/// minutes after the process started, and an uptime counted from there would call a node that
/// restarted two minutes ago "up for ten seconds". A process that never asked early (a test, a tool)
/// gets the instant of its first frame, which is the age of its publisher.
pub fn process_started() -> Instant {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

/// A span as whole milliseconds, saturating (a `u64` of them is half a billion years).
fn millis(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

/// **The readings the publisher itself takes of the node and stamps onto every frame it builds**
/// ([`CoreSnapshotWire::uptime_ms`], [`CoreSnapshotWire::halt_sentinel`], [`OrderViewWire::age_ms`]), after
/// [`project`] and off the fold: the core holds none of them, and a field of `vike_execution::CoreSnapshot`
/// would put a clock read, or a file stat, on the measured hot path's side of the line. `project`
/// stays pure over its arguments and its golden JSON is untouched; a frame is `project` plus
/// [`Self::stamp`].
///
/// One value per publisher, shared by its thread (push frames) and by every connection thread that
/// answers a `Request::Snapshot` ([`PublisherHandle::snapshot_frame_now`]), which is why the state is
/// behind a `Mutex`/`OnceLock`: each reader of the same node must see the same first-seen instants.
#[derive(Default)]
struct NodeReadings {
    /// The first instant this process saw each order that was not terminal, by order client id. An
    /// order that ended (or left the registry) is dropped by the next [`Self::stamp`], so the map
    /// holds only what rests, however long the registry (which never prunes) has grown.
    first_seen: Mutex<HashMap<String, Instant>>,
    /// The HALT sentinel's path, handed over by the daemon's composition root
    /// ([`PublisherHandle::with_halt_sentinel_path`]). Unset = this publisher was never told where the
    /// switch is, and the frame says nothing about it (`halt_sentinel: None`), which is not "clear".
    halt_path: OnceLock<PathBuf>,
    /// The lease registry the control edge also reads ([`PublisherHandle::with_symbol_sources`]), asked for the
    /// accounts whose venue balance the node holds back
    /// ([`crate::server::symbol_admission::AccountReadiness::balance_withheld`]) and why a start was refused. Unset = a
    /// node with no gatewayhub lease: the frame lists none, as an older node's does.
    readiness: OnceLock<Arc<dyn crate::server::symbol_admission::AccountReadiness>>,
    /// The same registry, asked for the node's links ([`crate::server::symbol_admission::LinkHealth`]). Unset = none
    /// listed.
    link_health: OnceLock<Arc<dyn crate::server::symbol_admission::LinkHealth>>,
}

impl NodeReadings {
    /// The accounts whose venue balance the node holds back (`venue_backend::gateway::balance_gate`) among the route keys
    /// `running` accepts, in route-key order: the hold of an account whose engine does not run is history.
    fn balance_held(&self, running: impl Fn(&str) -> bool) -> Vec<BalanceWithheldWire> {
        let Some(source) = self.readiness.get() else { return Vec::new() };
        source
            .balance_held()
            .into_iter()
            .filter(|(route_key, _)| running(route_key))
            .map(|(route_key, reason)| BalanceWithheldWire { route_key, reason })
            .collect()
    }

    /// The hub links as they stand ([`LinkReading`]).
    fn read_links(&self) -> LinkReading {
        let Some(source) = self.link_health.get() else { return LinkReading::default() };
        LinkReading {
            trade: source.trade_link_down(),
            price: source.price_link_down(),
            path: source.venue_link_down(),
            path_up: source.venue_path_up(),
        }
    }

    /// **What a subscriber must hear about as soon as it moves**, though nothing folded: who is down, whether for good,
    /// and how long in steps of [`LINK_AGE_STEP_MS`]. Compared by the publisher thread with the last frame's.
    fn link_mark(&self) -> LinkMark {
        self.read_links().mark()
    }

    /// Stamp each `adapter failed` row with the cause the lease registry kept: the word covers several, and the one
    /// behind this row is the last refused start's. A row with another word is not that start's.
    fn stamp_start_refusals(&self, wire: &mut CoreSnapshotWire) {
        let Some(source) = self.readiness.get() else { return };
        let adapter_failed = vike_model::ArmingBlock::AdapterFailed.as_str();
        for row in wire.not_running.iter_mut().filter(|row| row.reason == adapter_failed) {
            row.detail = source.start_refusal(&row.route_key);
        }
    }

    /// Stamp `wire` (just projected from `snap`) as of `now`, with the hub links as they stand.
    #[cfg(test)]
    fn stamp(&self, wire: &mut CoreSnapshotWire, snap: &CoreSnapshot, now: Instant) {
        self.stamp_reading(wire, snap, now, self.read_links());
    }

    /// Stamp `wire` (just projected from `snap`) as of `now`, carrying `links`.
    fn stamp_reading(
        &self,
        wire: &mut CoreSnapshotWire,
        snap: &CoreSnapshot,
        now: Instant,
        links: LinkReading,
    ) {
        wire.uptime_ms = Some(millis(now.saturating_duration_since(process_started())));
        let running = |key: &str| wire.venues.iter().any(|block| block.route_key == key);
        let held = self.balance_held(running);
        let LinkReading { mut trade, price, path: mut down, path_up: mut up } = links;
        // The outage of an account whose engine does not run is history.
        down.retain(|link| running(&link.route_key));
        // A stopped account keeps its block, and its lease is over on purpose: no link of its is a loss.
        let leased = |key: &str| running(key) && wire.stopped_row(key).is_none();
        trade.retain(|link| leased(&link.route_key));
        up.retain(|key| leased(key));
        wire.balance_withheld = held;
        wire.venue_link_down = down;
        wire.venue_path_up = up;
        wire.trade_link_down = trade;
        wire.price_link_down = price;
        wire.price_dials_up = self.link_health.get().and_then(|source| source.price_dials_up());
        self.stamp_start_refusals(wire);
        // The ONE `exists()` every enforcing client's submit seam asks, over the path they were
        // handed: `trade health`'s reading and an adapter's refusal cannot disagree about the file. A
        // stat per frame (~40/s at most) on the publisher's own thread; never the fold.
        let sentinel = self.halt_path.get().map(PathBuf::as_path);
        wire.halt_sentinel =
            sentinel.map(|path| vike_execution::halt::sentinel_engaged(Some(path)));
        self.stamp_order_ages(wire, snap, now);
    }

    /// Stamp `age_ms` on every order that is not terminal. `wire.orders` is `snap.orders` projected
    /// one for one and in order ([`project`]), which is what lets the two be walked together.
    fn stamp_order_ages(&self, wire: &mut CoreSnapshotWire, snap: &CoreSnapshot, now: Instant) {
        let mut first_seen = self.first_seen.lock().unwrap_or_else(PoisonError::into_inner);
        let mut resting: HashSet<&str> = HashSet::new();
        for (view, order) in snap.orders.iter().zip(wire.orders.iter_mut()) {
            if view.status.is_terminal() {
                continue;
            }
            let order_client_id = view.order_client_id.as_str();
            resting.insert(order_client_id);
            let first = match first_seen.get(order_client_id).copied() {
                Some(first) => first,
                None => {
                    first_seen.insert(order_client_id.to_owned(), now);
                    now
                }
            };
            order.age_ms = Some(millis(now.saturating_duration_since(first)));
        }
        first_seen.retain(|order_client_id, _| resting.contains(order_client_id.as_str()));
    }
}

// ---------------------------------------------------------------------------------------------
// Mailbox: a bounded, drop-oldest, single-producer/single-consumer frame queue.
// ---------------------------------------------------------------------------------------------

/// The outcome of a [`PublisherMailbox`] receive.
#[derive(Debug)]
pub enum PublisherMailboxReceive {
    /// One framed snapshot to write to the socket.
    Frame(Arc<Vec<u8>>),
    /// The wait elapsed with no frame (loop and re-check).
    Timeout,
    /// The mailbox was closed (publisher shutting down, or the subscriber deregistered) — stop.
    Closed,
}

struct MailboxInner {
    buffer: VecDeque<Arc<Vec<u8>>>,
    closed: bool,
    /// `None` until a reader says it reports what it has read ([`PublisherSubscription::newest_only`]). `Some(true)`: a frame is on
    /// its way and unread, so nothing more goes out.
    on_its_way: Option<bool>,
}

impl MailboxInner {
    /// The frame the writer sends now: the oldest queued, or the NEWEST for a reader that reports what it read, the
    /// others dropped, and only once it has read the last.
    fn next_frame(&mut self) -> Option<Arc<Vec<u8>>> {
        match self.on_its_way {
            None => self.buffer.pop_front(),
            Some(true) => None,
            Some(false) => {
                let newest = self.buffer.pop_back()?;
                self.buffer.clear();
                self.on_its_way = Some(true);
                Some(newest)
            }
        }
    }
}

/// A per-connection BOUNDED, DROP-OLDEST frame queue. The publisher [`push`](PublisherMailbox::push)es (never
/// blocking); the connection's writer thread [`recv_timeout`](PublisherMailbox::recv_timeout)s. When full, the
/// OLDEST frame is discarded — the lossy-observer contract.
pub struct PublisherMailbox {
    inner: Mutex<MailboxInner>,
    cv: Condvar,
    cap: usize,
}

impl PublisherMailbox {
    fn new(cap: usize) -> Arc<Self> {
        Arc::new(PublisherMailbox {
            inner: Mutex::new(MailboxInner {
                buffer: VecDeque::new(),
                closed: false,
                on_its_way: None,
            }),
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
        while g.buffer.len() >= self.cap {
            g.buffer.pop_front();
        }
        g.buffer.push_back(frame);
        drop(g);
        self.cv.notify_one();
    }

    /// Consumer side (the connection's writer thread): pop the next frame ([`MailboxInner::next_frame`]), or block up to
    /// `timeout` for one. Returns [`PublisherMailboxReceive::Closed`] once the mailbox is closed and drained,
    /// [`PublisherMailboxReceive::Timeout`] if the wait elapsed with nothing to send.
    pub fn recv_timeout(&self, timeout: Duration) -> PublisherMailboxReceive {
        let mut g = self.inner.lock().expect("mailbox poisoned");
        if let Some(f) = g.next_frame() {
            return PublisherMailboxReceive::Frame(f);
        }
        if g.closed {
            return PublisherMailboxReceive::Closed;
        }
        // Wait for a push, a reader's word or the timeout; a spurious wakeup with nothing to send
        // simply reports `Timeout`, and the caller loops. The `WaitTimeoutResult` is not needed — the
        // buffer/closed re-check below is authoritative.
        let (mut g2, _) = self.cv.wait_timeout(g, timeout).expect("mailbox poisoned");
        if let Some(f) = g2.next_frame() {
            return PublisherMailboxReceive::Frame(f);
        }
        if g2.closed {
            return PublisherMailboxReceive::Closed;
        }
        PublisherMailboxReceive::Timeout
    }

    /// The reader has read the frame on its way: the writer may send the newest.
    fn frame_read(&self) {
        let mut g = self.inner.lock().expect("mailbox poisoned");
        if g.on_its_way.is_some() {
            g.on_its_way = Some(false);
        }
        drop(g);
        self.cv.notify_one();
    }

    /// Close the mailbox: no more frames accepted, waiting consumers wake to [`PublisherMailboxReceive::Closed`].
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
    mailbox: Arc<PublisherMailbox>,
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
    identity: Option<HubIdentityWire>,
    /// The node's process-static per-mount rows (split-plane I10) — one `MountViewWire` per mounted
    /// strategy, in mount order; the server's source for the `StrategyStatus` mounts `Vec`. Empty
    /// = the pre-I10 caller shape, where the server derives the one row from `identity` instead
    /// (see `server.rs`'s `Request::StrategyStatus` arm). NOT stamped into pushed frames — the
    /// wire snapshot carries only the identity block, exactly as before.
    mounts: Vec<vike_tradehub_client::wire::MountViewWire>,
    /// The node's list of the accounts it does NOT run, each with why (decision 0156;
    /// `vike_tradehub_core::RuntimeAssembly::not_running`): stamped into every published frame
    /// ([`project`]), and read by the control edge so a command addressed to one is refused with
    /// its reason. Empty = every account the node knows runs (or a caller with no node). Static from
    /// start until decision 0169: an account started in the running node leaves it
    /// ([`PublisherHandle::set_not_running`]), so it is a cell, not a field.
    not_running: ArcSwap<Vec<ArmingNotRunningView>>,
    /// The route keys of the RUNNING accounts whose key in the database differs from the one their session
    /// started with ([`PublisherHandle::set_key_changed`]); stamped into every frame beside `not_running`.
    key_changed: ArcSwap<Vec<String>>,
    /// What the publisher reads off the node itself and stamps onto every frame ([`NodeReadings`]).
    facts: NodeReadings,
    /// Where the core records its runtime mounts ([`PublisherHandle::with_mount_topology_dir`]).
    topology: OnceLock<MountTopology>,
    /// Raised by [`PublisherHandle::set_not_running`]: the publisher thread frames the current snapshot once
    /// more even though its `seq` did not move, so a changed list reaches subscribers without waiting for the
    /// core's next publish.
    reframe: AtomicBool,
    reg: Mutex<RegistryInner>,
    next_id: AtomicU64,
    stop: AtomicBool,
}

/// One mount as the LIVE core reports it: the id an `UpdateParams` addresses it by, its series key
/// and its typed params. What [`PublisherHandle::live_mount_params`] returns, in the core's mount
/// order.
#[derive(Debug, Clone, PartialEq)]
pub struct MountLiveParams {
    /// `vike_execution::MountView::mount_id`: the stored, sanitized mount id.
    pub mount_id: String,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// The mount's own answer (`vike_model::Strategy::params`' default is `None`), as the core's
    /// serde JSON.
    pub typed_params: Option<serde_json::Value>,
    /// `vike_execution::MountView::latched`: the mount's budget latch is spent (decision 0239).
    pub latched: bool,
}

/// The real mounts of `snap`, in the core's order: [`PublisherHandle::live_mount_params`].
fn live_mount_params_of(snap: &CoreSnapshot) -> Vec<MountLiveParams> {
    snap.mounts
        .iter()
        .filter(|m| m.kind == vike_execution::MountViewKind::Mount)
        .map(|m| MountLiveParams {
            mount_id: m.mount_id.clone(),
            venue: m.venue.clone(),
            symbol: m.symbol.clone(),
            interval: m.interval.clone(),
            // `StrategyParams` is a plain serde union; a value that somehow could not be
            // serialized is reported as "no typed params" rather than failing the whole
            // status read — the same degrade the `None` default already means, and the
            // capability string is what tells a client the node CAN answer at all.
            typed_params: m.params.as_ref().and_then(|p| serde_json::to_value(p).ok()),
            latched: m.latched,
        })
        .collect()
}

/// Write what the core publishes about `live` onto `row`, and what a mount the core recorded says about itself.
fn write_live_mount(
    row: &mut vike_tradehub_client::wire::MountViewWire,
    live: MountLiveParams,
    records: &[vike_core::mount_topology::MountRecord],
) {
    if let Some(spec) = records.iter().find(|r| r.mount_id() == live.mount_id).map(|r| &r.spec) {
        row.strategy = crate::mount_factory::order_client_id_strategy_name(spec).to_string();
        row.params = format!(
            "venue={} symbol={} interval={} :: {}",
            spec.venue, spec.symbol, spec.interval, spec.params
        );
        row.route_key = vike_model::accounts::account_keys::route_key_of(
            &spec.venue,
            spec.account
                .as_ref()
                .unwrap_or(&vike_model::accounts::account_keys::AccountLabel::Default),
        );
        // A mount command names no product.
        row.asset_class = None;
        // The strategy code the door stamped on the mount (decision 0270), absent for `0000`.
        row.order_client_id_code = spec.order_client_id_code.map(|c| c.as_str().to_string());
    }
    row.mount_id = live.mount_id;
    row.venue = live.venue;
    row.symbol = live.symbol;
    row.interval = live.interval;
    row.typed_params = live.typed_params;
    // The budget latch is the core's own state (decision 0239), never a boot-time fact.
    row.latched = live.latched;
}

/// The core's runtime-mount record as [`PublisherHandle::mount_rows_now`] reads it (`vike_core::mount_topology`): the
/// only place a runtime mount's account and strategy are written, because the core's published row names neither.
struct MountTopology {
    /// `CoreConfig::state_dir`, the directory the core writes the record in.
    dir: PathBuf,
    /// The records that held a BOOT mount's id when the node started, with their stamps. The start's resurrect re-sends
    /// such a record, the core refuses it as a duplicate of the boot mount, and it stays in the file describing a
    /// mount that does not run. A re-mount of that id writes a new stamp, and then the record is the truth.
    refused: Vec<(String, u64)>,
}

/// A cheap, cloneable handle to the running publisher: [`subscribe`](PublisherHandle::subscribe) to
/// join the fan-out, [`snapshot_frame_now`](PublisherHandle::snapshot_frame_now) for a point-in-time
/// frame, and [`shutdown`](PublisherHandle::shutdown) to stop the thread and release subscribers.
#[derive(Clone)]
pub struct PublisherHandle {
    shared: Arc<PublisherShared>,
    /// The poll thread's join handle, shared so any clone may [`shutdown`](Self::shutdown) once.
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Where the control edge looks up the facts of a symbol an engine was not started on
    /// (`crate::server::symbol_admission`); `None` on a node with no gatewayhub lease to ask.
    /// Process-static like `not_running`, set once by [`Self::with_symbol_sources`] before the handle
    /// is cloned into the server.
    admitter: Option<Arc<dyn crate::server::symbol_admission::SymbolAdmitter>>,
    /// Where an operator's venue read is asked (`crate::server::venue_read`), set with [`Self::admitter`].
    venue_reads: Option<Arc<dyn crate::server::symbol_admission::VenueReads>>,
    /// The node's LATE-RETRY memory (decision 0270, D7): the order client ids its orders used before the last
    /// restart, which the control door refuses (`ControlLimits::with_boot_order_client_ids`). Process-static like
    /// [`Self::admitter`], set once by [`Self::with_boot_order_client_ids`] before the handle is cloned into the server;
    /// EMPTY on a node with no journal.
    boot_order_client_ids: Arc<std::collections::HashSet<String>>,
    /// What each `.rs` mount the start deactivated for its BUILD says now (the owner's ruling of 2026-11-14),
    /// read by [`Self::mount_rows_now`]; set once by [`Self::with_mount_builds`] before the handle is cloned into
    /// the server. Empty on a node whose every mount ran at the start.
    mount_builds: crate::config::MountBuilds,
}

/// A live subscription: holds the receive [`PublisherMailbox`] and DEREGISTERS from the publisher on drop, so
/// a connection's writer thread ending cleans up its registry slot automatically.
pub struct PublisherSubscription {
    shared: Arc<PublisherShared>,
    id: u64,
    mailbox: Arc<PublisherMailbox>,
}

impl PublisherSubscription {
    /// Block up to `timeout` for the next framed snapshot (see [`PublisherMailbox::recv_timeout`]).
    pub fn recv_timeout(&self, timeout: Duration) -> PublisherMailboxReceive {
        self.mailbox.recv_timeout(timeout)
    }

    /// Hold this subscription to one frame on its way, and the newest when the reader has read the last
    /// (`vike_tradehub_client::proto::FEATURE_OBSERVE_NEWEST`). Only this writer waits: the publisher still pushes
    /// without blocking, and every other subscriber has its own mailbox. What the reader's word goes through is the answer.
    pub(crate) fn newest_only(&self) -> PublisherFrameReads {
        self.mailbox.inner.lock().expect("mailbox poisoned").on_its_way = Some(false);
        PublisherFrameReads(Arc::clone(&self.mailbox))
    }
}

/// Where a [`PublisherSubscription::newest_only`] reader's word lands.
pub(crate) struct PublisherFrameReads(Arc<PublisherMailbox>);

impl PublisherFrameReads {
    /// The reader has read the frame on its way.
    pub(crate) fn frame_read(&self) {
        self.0.frame_read();
    }
}

impl Drop for PublisherSubscription {
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
    identity: Option<HubIdentityWire>,
) -> PublisherHandle {
    spawn_with_mounts(cell, identity, Vec::new(), Vec::new())
}

/// [`spawn`] with the node's per-mount rows (split-plane I10): one
/// [`vike_tradehub_client::wire::MountViewWire`] per mounted strategy, in mount order — what the
/// server's `StrategyStatus` read verb answers with. [`spawn`] passes an EMPTY vec, under which
/// the server falls back to deriving the one row from `identity` (the pre-I10 behaviour,
/// byte-identical for every existing caller).
///
/// `not_running` is the node's list of the accounts it does NOT run (decision 0156,
/// `vike_tradehub_core::RuntimeAssembly::not_running`), published with every frame; [`spawn`] passes none.
pub fn spawn_with_mounts(
    cell: Arc<ArcSwap<CoreSnapshot>>,
    identity: Option<HubIdentityWire>,
    mounts: Vec<vike_tradehub_client::wire::MountViewWire>,
    not_running: Vec<ArmingNotRunningView>,
) -> PublisherHandle {
    let shared = Arc::new(PublisherShared {
        cell,
        identity,
        mounts,
        not_running: ArcSwap::from_pointee(not_running),
        key_changed: ArcSwap::from_pointee(Vec::new()),
        facts: NodeReadings::default(),
        topology: OnceLock::new(),
        reframe: AtomicBool::new(false),
        reg: Mutex::new(RegistryInner { subs: Vec::new(), last_frame: None }),
        next_id: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });

    let thread_shared = Arc::clone(&shared);
    let join = std::thread::Builder::new()
        .name("th-publish".into())
        .spawn(move || run_publisher(thread_shared))
        .expect("spawn th-publish thread");

    PublisherHandle {
        shared,
        join: Arc::new(Mutex::new(Some(join))),
        admitter: None,
        venue_reads: None,
        boot_order_client_ids: Arc::default(),
        mount_builds: crate::config::MountBuilds::default(),
    }
}

/// The publisher poll+fan-out loop. Reads the arc-swap cell; on a `seq` change, frames once and
/// broadcasts. Exits when `stop` is set, closing every subscriber's mailbox so writer threads wake.
fn run_publisher(shared: Arc<PublisherShared>) {
    // `None` until the first observed snapshot, so even a `seq: 0` placeholder is published once and a
    // brand-new subscriber gets a current frame.
    let mut last_sequence: Option<u64> = None;
    let mut links = LinkMark::default();
    let mut links_read = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        let snap = shared.cell.load_full();
        let mut reframe = shared.reframe.swap(false, Ordering::AcqRel);
        // A link moves no `seq`, and a dead datahub stops the very ticks that keep frames coming, so a subscriber
        // (the GUI, MCP's `node_snapshot`) would hear of it only at the core's next publish.
        if links_read.elapsed() >= LINK_CHECK_EVERY {
            links_read = Instant::now();
            reframe |= shared.facts.link_mark() != links;
        }
        if reframe || last_sequence != Some(snap.sequence) {
            last_sequence = Some(snap.sequence);
            // The mark of what THIS frame says, whatever asked for it: a frame for a fold can carry a loss no look
            // has seen, and if the link comes back before the next look, that look must still read a move.
            let reading = shared.facts.read_links();
            links = reading.mark();
            let frame = frame_snapshot_reading(
                &snap,
                shared.identity.as_ref(),
                &shared.not_running.load_full(),
                &shared.key_changed.load_full(),
                &shared.facts,
                reading,
            );
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

/// The orders in a published snapshot's `orders` that `cmd` names, for the two verbs that carry a
/// order client id of their own and the give-back of a symbol. Every other command gets an empty
/// `Vec`, which allocates nothing.
///
/// * **A `Modify` names the OPEN order of its id** (open means NOT terminal,
///   `vike_execution::CoreSnapshot::open_order`'s question over the same rows): the notional ceiling's way
///   to the instrument that order rests on.
/// * **A `Submit` names the order that already carries its id, in ANY status**, FILLED included:
///   `crate::server::refusal::order_client_id_reuse_refusal`'s input. A terminal order is exactly the case that
///   gate exists for.
/// * **An `UnadmitSymbol` names every order in its symbol that has not ended**, any account (case
///   ignored, the engine's question: `Engine::unadmit_symbol` refuses on a status that is not
///   terminal): `crate::server::unadmit::plan_unadmit`'s input, which keeps the ones of the addressed
///   account by their route key.
///
/// Filtering here, before the clone, is the point of the signature: `orders` is the whole registry
/// of every engine — nothing ever prunes a terminal order out of it — so cloning it per command
/// would grow with the node's age, while the one order a command names is a single small clone.
///
/// ONE function behind [`PublisherHandle::orders_named_by`], so the send and the Preview cannot
/// disagree about which order a command names.
pub(crate) fn named_orders(
    cmd: &vike_tradehub_client::wire::CommandWire,
    orders: &[OrderView],
) -> Vec<OrderView> {
    use vike_tradehub_client::wire::CommandWire;
    match cmd {
        CommandWire::Modify { order_client_id, .. } => orders
            .iter()
            .filter(|o| o.order_client_id == *order_client_id && !o.status.is_terminal())
            .cloned()
            .collect(),
        CommandWire::Submit(req) => {
            orders.iter().filter(|o| o.order_client_id == req.order_client_id).cloned().collect()
        }
        // An order list, a combo and a bracket name ids of their own too (every leg's, the combo's, every leg a
        // bracket's stem can name: `command_order_client_ids`), and the orders that already carry one, any status, are what the
        // door's `order_client_id_reuse_refusal` refuses them WHOLE by. Until these arms the door was handed none for them.
        CommandWire::OrderList(_) | CommandWire::Combo(_) | CommandWire::Bracket(_) => {
            let ids = crate::server::refusal::command_order_client_ids(cmd)
                .map(|(ids, _)| ids)
                .unwrap_or_default();
            orders.iter().filter(|o| ids.contains(&o.order_client_id)).cloned().collect()
        }
        // A give-back names the orders of its SYMBOL that have not ended, of any account: the door's
        // `server::unadmit::plan_unadmit` keeps the ones of its own route key. Until this arm the verb was
        // handed none, so the door never refused a symbol with an order in it, on any engine.
        CommandWire::UnadmitSymbol { symbol, .. } => {
            let typed = symbol.trim();
            orders
                .iter()
                .filter(|o| !o.status.is_terminal() && o.symbol.eq_ignore_ascii_case(typed))
                .cloned()
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The HELD reconcile divergence a `ReconcileConfirm` or a `ReconcileCancel` names, out of a snapshot's
/// `recon.alerts` (empty for every other command, and for an id the snapshot does not hold). Read by the
/// audit record only (`crate::server::control::accept_command`'s `recon` parameter); ONE function behind
/// [`PublisherHandle::reconcile_alerts_named_by`], as [`named_orders`] is for orders.
pub(crate) fn named_reconcile_alerts(
    cmd: &vike_tradehub_client::wire::CommandWire,
    alerts: &[ReconcileAlertView],
) -> Vec<ReconcileAlertView> {
    use vike_tradehub_client::wire::CommandWire;
    let (CommandWire::ReconcileConfirm { id } | CommandWire::ReconcileCancel { id }) = cmd else {
        return Vec::new();
    };
    alerts.iter().filter(|a| a.id == *id).cloned().collect()
}

/// The REAL mount rows of a published snapshot's `mounts` for an `UnlatchMount` (decision 0239), and an empty `Vec`
/// for every other command (allocating nothing): the residual row describes no mount and is left out. The door's
/// `server::unlatch::plan_unlatch` judges the command against them. ONE function behind
/// [`PublisherHandle::mounts_named_by`], as [`named_orders`] is for orders.
pub(crate) fn named_mounts(
    cmd: &vike_tradehub_client::wire::CommandWire,
    mounts: &[vike_execution::MountView],
) -> Vec<vike_execution::MountView> {
    if !matches!(cmd, vike_tradehub_client::wire::CommandWire::UnlatchMount { .. }) {
        return Vec::new();
    }
    mounts.iter().filter(|m| m.kind == vike_execution::MountViewKind::Mount).cloned().collect()
}

impl PublisherHandle {
    /// Register a new subscriber and return its [`PublisherSubscription`]. The current snapshot frame (if any)
    /// is enqueued immediately, so a fresh observer renders live state without waiting for the next
    /// change. If the publisher has already stopped, the returned subscription is pre-closed (its
    /// first `recv_timeout` yields [`PublisherMailboxReceive::Closed`]).
    pub fn subscribe(&self) -> PublisherSubscription {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let mailbox = PublisherMailbox::new(MAILBOX_CAP);
        if self.shared.stop.load(Ordering::Acquire) {
            mailbox.close();
            return PublisherSubscription { shared: Arc::clone(&self.shared), id, mailbox };
        }
        let mut g = self.shared.reg.lock().expect("registry poisoned");
        if let Some(f) = &g.last_frame {
            mailbox.push(Arc::clone(f));
        }
        g.subs.push(Subscriber { id, mailbox: Arc::clone(&mailbox) });
        drop(g);
        PublisherSubscription { shared: Arc::clone(&self.shared), id, mailbox }
    }

    /// A point-in-time framed snapshot of the CURRENT cell — the answer to a `Request::Snapshot`.
    /// Projects+frames on demand off the arc-swap cell (a read, never the fold).
    pub fn snapshot_frame_now(&self) -> Arc<Vec<u8>> {
        frame_snapshot(
            &self.shared.cell.load_full(),
            self.shared.identity.as_ref(),
            &self.shared.not_running.load_full(),
            &self.shared.key_changed.load_full(),
            &self.shared.facts,
        )
    }

    /// **The VENUES this node runs an ENGINE for** — the set a wire command's addressed venue
    /// (`vike_tradehub_client::wire::CommandWire::addressed_venue`) is checked against before it is
    /// lowered ([`crate::server::refusal::venue_refusal`]), read off the arc-swap snapshot cell PER CALL
    /// exactly like [`Self::snapshot_frame_now`] (a read, never the fold).
    ///
    /// One entry per ENGINE, in the core's own registration order (primary first, then extras) —
    /// `vike_execution::Portfolio::venues` is documented as "one block per engine". ⚠ Two engines of
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
    /// one thing: the field it reads. `vike_execution::VenueBlock::route_key` is
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
    /// **One key per account that RUNS** (v0.1.80 review b): the NEWEST block of each key ([`current_venue_blocks`])
    /// minus the accounts the node lists as not running ([`Self::not_running`]). The core's list only grows and a
    /// stopped account keeps its block, but the core counts only live engines when it asks whether a venue has
    /// several ("a STOPPED account is no longer an account of the exchange"); the bracket gate
    /// (`crate::server::refusal::bracket_book_refusal`) counts these keys against it, and a raw list refused every
    /// bracket of a venue while a restarted account's old block (a duplicate key) or a stopped lane twin's was in
    /// the roster. Empty together with [`Self::engine_venues`] except when EVERY published engine is stopped, where
    /// every command naming an account is refused by the not-running gate first.
    ///
    /// Command cadence, never the per-message fold: one `Vec` per operator action.
    pub fn engine_route_keys(&self) -> Vec<String> {
        running_route_keys(&self.shared.cell.load().portfolio.venues, &self.not_running())
    }

    /// **The engine BLOCKS this node runs** — one per ENGINE, same cell, same per-call read and same
    /// registration order as [`Self::engine_route_keys`], for the one gate that needs more than a
    /// key: a bracket's (`crate::server::refusal::bracket_refusal`), which must know what the ONE engine a
    /// bracket reaches TRADES (`vike_execution::VenueBlock::trades`) and which lane that engine is
    /// mounted on, because a venue adapter places every order on its engine's own instrument.
    ///
    /// Whole blocks rather than a third projection, so the "does it trade this symbol" answer is
    /// `VenueBlock::trades` itself and not a second spelling of it. EMPTY on the same terms as the
    /// two siblings, and empty TOGETHER with them: all three read one `portfolio.venues`.
    ///
    /// **The NEWEST block of each route key**, like the frame ([`current_venue_blocks`]): the core's list only grows, so
    /// an account that stopped and started again has its stopped engine's block BEFORE the new one, and every
    /// consumer here resolves a block by route key, first match (`plan_submit_symbol`, `bracket_refusal`, the
    /// notional ceiling), which would judge the order against an engine the core no longer routes to.
    ///
    /// Command cadence, never the per-message fold: one clone per operator action.
    pub fn engine_blocks(&self) -> Vec<vike_execution::VenueBlock> {
        current_venue_blocks(&self.shared.cell.load().portfolio.venues).cloned().collect()
    }

    /// **Every order the published snapshot holds**, off the same cell and per call like
    /// [`Self::engine_blocks`]: what the account watcher waits on after it cancels a stopped account's book
    /// (decision 0169). Command cadence, never the per-message fold: one clone.
    pub fn published_orders(&self) -> Vec<vike_execution::OrderView> {
        self.shared.cell.load().orders.clone()
    }

    /// **The order a command names**, off the same snapshot cell as [`Self::engine_blocks`] and
    /// per call like it ([`named_orders`] says which: the OPEN order a `Modify` names, the order of
    /// ANY status a `Submit`'s id already belongs to).
    ///
    /// The `Modify` half is for the one gate that needs to know which instrument a bare client order
    /// id rests on: the notional ceiling (`crate::server::control::ControlLimits::vet`), which sizes
    /// a `Modify` with that instrument's contract multiplier. Empty for an order this snapshot does
    /// not hold (not published yet, terminal, never existed), which the ceiling reads as "unknown"
    /// and sizes at 1.0 exactly as it always did. The `Submit` half is
    /// `crate::server::refusal::order_client_id_reuse_refusal`'s: an empty answer there is "the id is free as
    /// far as this snapshot can see" (a publish is coalesced, so an order placed a moment ago may
    /// not be in it yet; the gate's doc states that residual).
    ///
    /// ⚠ **No new read of the core**: `CoreSnapshot::orders` is built by the same coalesced publish
    /// as `portfolio.venues`, through the same arc-swap load. Reading the two in separate calls can
    /// straddle a publish, and that is harmless: the roster has exactly one transition (empty to
    /// published) and a multiplier grid is immutable after construction, so the worst a straddle
    /// yields is the order missing from an older snapshot — the unknown case above.
    ///
    /// Command cadence, never the per-message fold: one scan of the registry and at most one clone.
    pub fn orders_named_by(
        &self,
        cmd: &vike_tradehub_client::wire::CommandWire,
    ) -> Vec<vike_execution::OrderView> {
        named_orders(cmd, &self.shared.cell.load().orders)
    }

    /// **The HELD divergence a `ReconcileConfirm` names**, off the same snapshot cell and per call like
    /// [`Self::orders_named_by`]: the audit record's source for WHAT was confirmed
    /// ([`named_reconcile_alerts`]). Command cadence, never the per-message fold: one scan and at most
    /// one clone.
    pub fn reconcile_alerts_named_by(
        &self,
        cmd: &vike_tradehub_client::wire::CommandWire,
    ) -> Vec<ReconcileAlertView> {
        named_reconcile_alerts(cmd, &self.shared.cell.load().reconcile.alerts)
    }

    /// The node's process-static identity block (split-plane B3), or `None` on an identity-less
    /// node — the server's source for the `StrategyStatus` read verb (split-plane B4). Cloned per
    /// call: an OCCASIONAL request/response verb, never per-publish work.
    pub fn identity(&self) -> Option<HubIdentityWire> {
        self.shared.identity.clone()
    }

    /// The node's process-static per-mount rows (split-plane I10) — one row per mount the BOOT made,
    /// in mount order; EMPTY on a publisher spawned through the mount-less [`spawn`]. Cloned per
    /// call, same as [`Self::identity`]: an occasional request/response verb, never per-publish
    /// work. What the node runs NOW is [`Self::mount_rows_now`] over these.
    pub fn mounts(&self) -> Vec<vike_tradehub_client::wire::MountViewWire> {
        self.shared.mounts.clone()
    }

    /// **Tell the publisher where the core records its runtime mounts** (`CoreConfig::state_dir`), so a row can name a
    /// runtime mount's account and strategy. The daemon's composition root passes the directory it gave the core;
    /// `None` (no project) leaves a runtime mount with an unknown account, as the core then records nothing. Called
    /// after the boot's mounts are known; the first path wins.
    #[must_use]
    pub fn with_mount_topology_dir(self, dir: Option<PathBuf>) -> Self {
        if let Some(dir) = dir {
            let refused = vike_core::mount_topology::read(&dir)
                .into_iter()
                .map(|record| (record.mount_id(), record.mounted_at_ms))
                .filter(|(id, _)| self.shared.mounts.iter().any(|m| m.mount_id == *id))
                .collect();
            let _ = self.shared.topology.set(MountTopology { dir, refused });
        }
        self
    }

    /// The records that describe a mount this node really took: the file, minus [`MountTopology::refused`].
    fn runtime_mount_records(&self) -> Vec<vike_core::mount_topology::MountRecord> {
        let Some(topology) = self.shared.topology.get() else { return Vec::new() };
        vike_core::mount_topology::read(&topology.dir)
            .into_iter()
            .filter(|r| !topology.refused.contains(&(r.mount_id(), r.mounted_at_ms)))
            .collect()
    }

    /// **The mount rows as they stand NOW**: the `boot` rows laid over what the core publishes, so a strategy mounted
    /// while the node runs has a row and one unmounted has none. Read off one snapshot, per call, off the fold.
    ///
    /// - A boot row is matched to the core's row by [`MountViewWire::mount_id`](vike_tradehub_client::wire::MountViewWire::mount_id),
    ///   so one mount's text is never written over another's. A row with no id (the identity fallback) takes the next core
    ///   row by position, the only thing it has.
    /// - A boot row with an id the core does not list is GONE, and leaves the answer, but only when the core has said so:
    ///   a `seq` 0 placeholder lists nothing because nothing is built, and a fault frame may list nothing because the
    ///   build panicked. Those keep the row, because an omission reads as "that mount is gone", a claim about a live book.
    /// - A core row with no boot row is a runtime mount, boot rows first and these after, in the core's order. A mount
    ///   recorded by the core ([`Self::with_mount_topology_dir`], a boot id mounted again included) takes its strategy,
    ///   its params text and its account from that record, the core's published row naming neither.
    /// - With no record (no project, or the write failed) the row names no account and its strategy is `unknown`, and
    ///   `live` says whether any account of the venue trades at one, because it must not read paper while its orders may
    ///   reach a venue. This is the one way a row stays vague.
    ///
    /// ⚠ What a runtime mount's account does afterwards (`tier`, `not_running`) is the caller's overlay, by `route_key`.
    #[must_use]
    pub fn mount_rows_now(
        &self,
        boot: Vec<vike_tradehub_client::wire::MountViewWire>,
    ) -> Vec<vike_tradehub_client::wire::MountViewWire> {
        let snap = self.shared.cell.load_full();
        let mut live = live_mount_params_of(&snap);
        let records = self.runtime_mount_records();
        let core_has_said = snap.sequence > 0 && snap.fault.is_none();
        // A boot mount the core does not hold while its account is NOT RUNNING is one the start DEACTIVATED (its
        // venue not connected, owner 2026-11-14): kept on the list, so the caller's overlay shows it not running.
        let not_running = self.not_running();
        let deactivated = |row: &vike_tradehub_client::wire::MountViewWire| {
            !row.route_key.is_empty()
                && not_running.iter().any(|nr| nr.route_key() == row.route_key)
        };
        let mut rows = Vec::with_capacity(boot.len() + live.len());
        for mut row in boot {
            let at = if row.mount_id.is_empty() {
                (!live.is_empty()).then_some(0)
            } else {
                live.iter().position(|mount| mount.mount_id == row.mount_id)
            };
            // A `.rs` mount the start deactivated for its BUILD (owner 2026-11-14): while it does not run its row
            // stays, not running, its params the reason; once it runs, its params are the line its file reported.
            let building = match self.mount_builds.get(&row.mount_id) {
                Some(Ok(params)) => {
                    row.params = params;
                    false
                }
                Some(Err(why)) => {
                    row.params = why;
                    row.not_running = true;
                    true
                }
                None => false,
            };
            match at {
                Some(at) => {
                    write_live_mount(&mut row, live.remove(at), &records);
                    rows.push(row);
                }
                None if row.mount_id.is_empty()
                    || !core_has_said
                    || deactivated(&row)
                    || building =>
                {
                    rows.push(row);
                }
                None => {}
            }
        }
        for mount in live {
            let mut row = vike_tradehub_client::wire::MountViewWire {
                order_client_id_code: None,
                strategy: "unknown".to_string(),
                params: format!(
                    "venue={} symbol={} interval={}",
                    mount.venue, mount.symbol, mount.interval
                ),
                live: current_venue_blocks(&snap.portfolio.venues).any(|block| {
                    block.venue == mount.venue && block.tier != Some(vike_model::AccountTier::Paper)
                }),
                venue: String::new(),
                symbol: String::new(),
                interval: String::new(),
                mount_id: String::new(),
                typed_params: None,
                asset_class: None,
                latched: false,
                route_key: String::new(),
                tier: None,
                not_running: false,
            };
            write_live_mount(&mut row, mount, &records);
            rows.push(row);
        }
        rows
    }

    /// **The accounts this node does NOT run, each with why** (decision 0156) — the list every frame
    /// carries. The control edge reads it so a command addressed to one of them is refused WITH its
    /// reason (`crate::server::refusal::account_refusal`, `crate::server::refusal::venue_refusal`).
    /// A snapshot of the list as it stands: command cadence, never per-publish work.
    pub fn not_running(&self) -> Arc<Vec<ArmingNotRunningView>> {
        self.shared.not_running.load_full()
    }

    /// **Replace the list of accounts this node does not run** (decision 0169): an account started in the
    /// running node leaves it. Takes effect on the next frame (the core publishes right after it adds
    /// the engine) and on the next control command.
    pub fn set_not_running(&self, not_running: Vec<ArmingNotRunningView>) {
        self.shared.not_running.store(Arc::new(not_running));
        self.shared.reframe.store(true, Ordering::Release);
    }

    /// **Replace the list of RUNNING accounts whose key in the database differs from the one their session
    /// started with** (route keys): takes effect on the next frame. A warning on an account that keeps
    /// running, never a stop: the key is replaced by the account's next restart.
    pub fn set_key_changed(&self, key_changed: Vec<String>) {
        self.shared.key_changed.store(Arc::new(key_changed));
        self.shared.reframe.store(true, Ordering::Release);
    }

    /// Attach the sources the control edge resolves a new symbol's facts from and reads a venue through, and the
    /// ones the frame's readings come from. Call it on the handle BEFORE it is cloned into the server: a clone made
    /// earlier keeps the sources it had.
    #[must_use]
    pub fn with_symbol_sources(
        mut self,
        sources: crate::server::symbol_admission::SymbolSources,
    ) -> Self {
        // The publisher's own thread reads the readings' sources for the frame's `balance_withheld` and links (the
        // handle's fields are not visible to it); the first source wins, like the HALT sentinel's path.
        let crate::server::symbol_admission::SymbolSources {
            admitter,
            readiness,
            link_health,
            venue_reads,
        } = sources;
        let mut reframe = false;
        if let Some(source) = readiness {
            let _ = self.shared.facts.readiness.set(source);
            reframe = true;
        }
        if let Some(source) = link_health {
            let _ = self.shared.facts.link_health.set(source);
            reframe = true;
        }
        if reframe {
            self.shared.reframe.store(true, Ordering::Release);
        }
        self.admitter = admitter;
        self.venue_reads = venue_reads;
        self
    }

    /// **Tell the publisher where the HALT sentinel is**, so every frame reports whether the file exists
    /// ([`CoreSnapshotWire::halt_sentinel`]). The path is the process's ONE resolved sentinel, which the
    /// daemon's composition root passes in (`crate::hub_server::start_observe_server`): the publisher
    /// resolves nothing itself, because `vike_boot::halt::halt_path_from_env` memoizes on its first
    /// call and a publisher spawned before the root declared its project (every test) would fix the
    /// kill switch's path for the process, and probe the directory, as a side effect of publishing.
    /// A publisher never handed a path reports `None`, which a reader must not take for "clear".
    /// The first path wins (one process has one sentinel); the next frame carries the answer.
    #[must_use]
    pub fn with_halt_sentinel_path(self, path: PathBuf) -> Self {
        // `set` refuses only a second path; keeping the first is the contract above.
        let _ = self.shared.facts.halt_path.set(path);
        self.shared.reframe.store(true, Ordering::Release);
        self
    }

    /// The source of a new symbol's facts, when this node has one
    /// (`crate::server::symbol_admission::SymbolAdmitter`).
    pub fn symbol_admitter(&self) -> Option<&dyn crate::server::symbol_admission::SymbolAdmitter> {
        self.admitter.as_deref()
    }

    /// Where an operator's venue read is asked, when this node has a lease registry
    /// (`crate::server::symbol_admission::VenueReads`).
    pub fn venue_reads(&self) -> Option<&dyn crate::server::symbol_admission::VenueReads> {
        self.venue_reads.as_deref()
    }

    /// Attach the node's late-retry memory (decision 0270, D7), built once at boot. Call it on the handle BEFORE it is
    /// cloned into the server, like [`Self::with_symbol_sources`]: a clone made earlier keeps the memory it had.
    #[must_use]
    pub fn with_boot_order_client_ids(
        mut self,
        ids: Arc<std::collections::HashSet<String>>,
    ) -> Self {
        self.boot_order_client_ids = ids;
        self
    }

    /// **Tell the publisher what each `.rs` mount waiting for its build says** (`crate::config::MountBuilds`): its
    /// boot row is kept while the core does not hold it, marked not running with the reason as its params, and
    /// takes the params line its file reported once it runs.
    #[must_use]
    pub fn with_mount_builds(mut self, builds: crate::config::MountBuilds) -> Self {
        self.mount_builds = builds;
        self
    }

    /// The late-retry memory every control surface of this node is handed ([`Self::with_boot_order_client_ids`]).
    pub fn boot_order_client_ids(&self) -> &Arc<std::collections::HashSet<String>> {
        &self.boot_order_client_ids
    }

    /// The mount rows the LIVE core reports — [`MountLiveParams`] (id, series key, typed params) per
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
    pub fn live_mount_params(&self) -> Vec<MountLiveParams> {
        live_mount_params_of(&self.shared.cell.load_full())
    }

    /// **The published mount rows an `UnlatchMount` judges** (decision 0239), off the same snapshot cell and per
    /// call like [`Self::orders_named_by`] ([`named_mounts`] says which): every REAL mount row (never the residual)
    /// for that verb, and nothing for any other, so a command that names no mount clones nothing. Command cadence,
    /// never the per-message fold.
    pub fn mounts_named_by(
        &self,
        cmd: &vike_tradehub_client::wire::CommandWire,
    ) -> Vec<vike_execution::MountView> {
        named_mounts(cmd, &self.shared.cell.load().mounts)
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
