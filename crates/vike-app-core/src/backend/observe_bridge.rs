//! `observe_bridge` — the `--observe` thin-client's `WireSnapshot` → `CoreSnapshot` REVERSE bridge.
//!
//! The read-only observer — the desktop shell, in EVERY launch since it lost its local core
//! (`crates/vike-desktop/src/main.rs`'s `App::new`; it was `vike-app --observe <ADDR>`, one mode of
//! three, when this was written) — has NO local trading core. It connects to a headless
//! `vike-tradehub` daemon's observe server under `Scope::Read` and republishes each pushed
//! [`vike_tradehub_client::WireSnapshot`] into a GUI-owned arc-swap cell as a
//! [`vike_core::CoreSnapshot`], so every existing DOM/Trade/Portfolio panel renders the remote
//! daemon's live trading state unchanged. Charts render too: the daemon ships a bounded bar tail
//! (`WireSnapshot::bars`, ~300 closed + the forming candle per mounted series), which
//! [`wire_to_core`] rebuilds into the core's bar cache so `sync_from_core` drives the candlestick
//! engine exactly like the local GUI.
//!
//! Moved here VERBATIM from the GUI shell's `main.rs` (`vike-app` then — the CI-excluded GUI
//! binary) so the mapping finally runs in a gate — the FORWARD projection (`CoreSnapshot` →
//! `WireSnapshot`, `vike-tradehub/src/publish.rs`) was CI-tested while this inverse was not, and
//! [`map_order_status`]'s hand-maintained Debug-spelling fallback meant a newly added
//! `vike_exec::OrderStatus` variant silently rendered as terminal `Rejected` in the observer. The
//! exhaustive round-trip test below (no-wildcard `match` over every variant) turns that silent
//! drift into a compile/test failure. The shell reaches this module through
//! [`crate::backend::backend_conn`], which spawns [`spawn_bridge`] (and [`connect_control`]) for the
//! backend it connects to. ⚠ This ended "`vike-app` re-imports [`wire_to_core`] under its original
//! name (the same pattern as `crate::reconcile_config`)" until 2026-09-28: the desktop imports
//! nothing from here directly any more, and `reconcile_config` left this crate long ago — it is
//! `crates/vike-tradehub/src/reconcile_config.rs`.

/// Map one wire trading-state to the core enum (same three variants, distinct types).
pub fn map_ts(ts: vike_tradehub_client::WireTradingState) -> vike_exec::TradingState {
    match ts {
        vike_tradehub_client::WireTradingState::Active => vike_exec::TradingState::Active,
        vike_tradehub_client::WireTradingState::Reducing => vike_exec::TradingState::Reducing,
        vike_tradehub_client::WireTradingState::Halted => vike_exec::TradingState::Halted,
    }
}

/// Decode a wire order-status STRING back to `vike_exec::OrderStatus`. The wire carries the status
/// as a rendered string; `OrderStatus::parse` decodes the SCREAMING_SNAKE `as_str` spelling
/// ("ACCEPTED"), while `wire.rs` documents the projection as `format!("{:?}", status)` — the
/// Debug/PascalCase spelling ("Accepted"). So try the canonical parser first, fall back to the
/// documented Debug spelling, and only then warn and treat an unknown status as `Rejected` (a
/// terminal, so an unknown status never renders as live/actionable in the GUI).
pub fn map_order_status(s: &str) -> vike_exec::OrderStatus {
    use vike_exec::OrderStatus;
    if let Some(st) = OrderStatus::parse(s) {
        return st;
    }
    match s {
        "Initialized" => OrderStatus::Initialized,
        "Submitted" => OrderStatus::Submitted,
        "Accepted" => OrderStatus::Accepted,
        "Triggered" => OrderStatus::Triggered,
        "PartiallyFilled" => OrderStatus::PartiallyFilled,
        "Filled" => OrderStatus::Filled,
        "Canceled" => OrderStatus::Canceled,
        "Rejected" => OrderStatus::Rejected,
        "Denied" => OrderStatus::Denied,
        "Expired" => OrderStatus::Expired,
        "PendingCancel" => OrderStatus::PendingCancel,
        "Liquidated" => OrderStatus::Liquidated,
        "Emulated" => OrderStatus::Emulated,
        "Released" => OrderStatus::Released,
        other => {
            tracing::warn!(
                status = other,
                "observe: unknown wire order status; treating as Rejected"
            );
            OrderStatus::Rejected
        }
    }
}

/// Wire position → core `PositionView`. The wire carries the display-relevant subset; the
/// resolver-only fields (`mark_source`) and the inert margin-mode carriers (`margin_mode`/
/// `isolated_margin`) default to their off-path values (`None`/`Cross`/`None`).
pub fn map_pos(
    p: &vike_tradehub_client::wire::WirePositionView,
) -> vike_core::snapshot::PositionView {
    vike_core::snapshot::PositionView {
        venue: p.venue.clone(),
        symbol: p.symbol.clone(),
        position_side: p.position_side.clone(),
        size: p.size,
        avg_px: p.avg_px,
        unrealized: p.unrealized,
        mark_source: None,
        leverage: p.leverage,
        liq_price: p.liq_price,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// Wire order → core `OrderView`.
pub fn map_order(o: &vike_tradehub_client::wire::WireOrderView) -> vike_core::snapshot::OrderView {
    vike_core::snapshot::OrderView {
        client_order_id: o.client_order_id.clone(),
        venue: o.venue.clone(),
        symbol: o.symbol.clone(),
        side: o.side,
        qty: o.qty,
        order_type: o.order_type.clone(),
        price: o.price,
        trigger_price: o.trigger_price,
        status: map_order_status(&o.status),
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        avg_fill_px: o.avg_fill_px,
    }
}

/// Wire held bracket exit → core `HeldOrderView` (NOT re-exported at the crate root — reach into
/// `vike_core::snapshot`).
pub fn map_held(
    h: &vike_tradehub_client::wire::WireHeldOrderView,
) -> vike_core::snapshot::HeldOrderView {
    vike_core::snapshot::HeldOrderView {
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

/// Wire per-venue block → core `VenueBlock`. The heavy internals stay core-side, so the
/// GUI-irrelevant fields default: `balance_mode` = `Delta` (matching `CoreSnapshot::empty`),
/// `fee_schedule` = `None`, an empty multiplier grid with a 1.0 default; `margin_ratio` is
/// recomputed from the wired `margin_used`/`equity`.
pub fn map_venueblock(
    v: &vike_tradehub_client::wire::WireVenueBlock,
) -> vike_core::snapshot::VenueBlock {
    vike_core::snapshot::VenueBlock {
        venue: v.venue.clone(),
        // ⚠ **THE WIRE CARRIES NO ACCOUNT, so this bridge cannot say which one a block is.**
        // `vike_core::snapshot::VenueBlock` gained `account`/`route_key`
        // (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1)
        // and `vike_tradehub_client::wire::WireVenueBlock` deliberately did NOT — that mirror is
        // §6.2's, in the stage that gives the MCP gate a route key to compare. So the only honest
        // answer here is the absent/bare-venue shape, i.e. "this reader was not told".
        //
        // It cannot misroute anything: this bridge builds a READ-ONLY observer view and routes no
        // order — the core that signs is the remote one, and its own refusal is what protects the
        // books. What it CAN do is mislead a READER watching a two-account node, who sees two
        // blocks claiming to be their venue's sole account; that is the half §6.2 closes, and it is
        // named here rather than left to be discovered.
        account: None,
        route_key: v.venue.clone(),
        balance: v.balance,
        realized_pnl: v.realized_pnl,
        fees_paid: v.fees_paid,
        funding_paid: v.funding_paid,
        balance_mode: vike_exec::BalanceMode::Delta,
        equity: v.equity,
        unrealized: v.unrealized,
        missing_prices: v.missing_prices,
        margin_used: v.margin_used,
        free_bp: v.free_bp,
        margin_ratio: if v.equity > 0.0 { v.margin_used / v.equity } else { 0.0 },
        fee_schedule: None,
        trading_state: map_ts(v.trading_state),
        multipliers: std::sync::Arc::new(indexmap::IndexMap::new()),
        multiplier_default: 1.0,
        positions: v.positions.iter().map(map_pos).collect(),
    }
}

/// Wire portfolio slice → core `Portfolio`. `equity_total` is the wire's cross-venue aggregate;
/// the scalar fields (`equity`/`realized_pnl`/`fees_paid`/`funding_paid`) mirror the PRIMARY venue
/// (`venues[0]`), exactly as `CoreSnapshot::build` populates them (NOT summed); `margin_used_total`
/// and `missing_prices_total` ARE cross-venue sums.
pub fn build_portfolio(w: &vike_tradehub_client::WireSnapshot) -> vike_core::Portfolio {
    let primary = w.venues.first();
    vike_core::Portfolio {
        equity: primary.map(|v| v.equity).unwrap_or(0.0),
        equity_total: w.equity_total,
        realized_pnl: primary.map(|v| v.realized_pnl).unwrap_or(0.0),
        fees_paid: primary.map(|v| v.fees_paid).unwrap_or(0.0),
        funding_paid: primary.map(|v| v.funding_paid).unwrap_or(0.0),
        // Naive fold, NOT `.sum()`: std's float `Sum` empty identity is `-0.0`, so an EMPTY
        // venues list (the pre-first-frame `WireSnapshot::empty()` "connecting" state) would
        // yield a `-0.0` here where `CoreSnapshot::empty`'s `Portfolio::default()` holds `+0.0`.
        // The fold starts at `+0.0` (bit-identical to `.sum()` for every non-empty list —
        // `margin_used` is never `-0.0`), keeping the observer's placeholder bit-identical to
        // the local core's. (`CoreSnapshot::build`'s own `.sum()` site never sees an empty list:
        // it always has at least the primary engine's venue.)
        margin_used_total: w.venues.iter().fold(0.0, |acc, v| acc + v.margin_used),
        // 0.0 = UNKNOWN, deliberately. `WireVenueBlock` carries no per-engine `seed_cash` and
        // `WireSnapshot` carries no total, so there is nothing honest to map here; inventing one
        // would let `Portfolio::drawdown_curve` read as a real capital base on the observe path.
        // Nothing downstream of this bridge divides by it: the drawdown latch runs in the REMOTE
        // core (against its own seeds) and `vike_alerting`'s `RuleTrigger::Drawdown` is mounted
        // only by `vike-tradehub`, which builds its snapshots locally via `CoreSnapshot::build`.
        // The observe client is a read-only GUI. If a wire seed is ever added, map it here.
        capital_base: 0.0,
        missing_prices_total: w.venues.iter().map(|v| v.missing_prices).sum(),
        balances_by_asset: Vec::new(),
        venues: w.venues.iter().map(map_venueblock).collect(),
    }
}

/// Bridge one remote `WireSnapshot` into the `CoreSnapshot` the GUI panels already render (see the
/// module comment above). READ-ONLY: no local core; `bars` are rebuilt from the wire's bounded tail
/// (charts render), `marks`/`mounts`/`recon` empty, counters zero.
pub fn wire_to_core(w: &vike_tradehub_client::WireSnapshot) -> vike_core::CoreSnapshot {
    vike_core::CoreSnapshot {
        seq: w.seq,
        venue: w.venue.clone(),
        symbol: w.symbol.clone(),
        trading_state: map_ts(w.trading_state),
        balance: w.balance,
        balance_mode: vike_exec::BalanceMode::Delta,
        portfolio: build_portfolio(w),
        positions: w.positions.iter().map(map_pos).collect(),
        marks: Vec::new(),
        orders: w.orders.iter().map(map_order).collect(),
        held_exits: w.held_exits.iter().map(map_held).collect(),
        // Rebuild the bar cache from the wire so the observer chart renders (the node ships a bounded
        // last-K tail + the forming candle per mounted series). `sync_from_core` consumes this exactly
        // like the local GUI's live bars — no observer-specific render code.
        bars: w
            .bars
            .iter()
            .map(|s| {
                let to_bar = |b: &vike_tradehub_client::wire::WireBar| vike_model::Bar {
                    ts: b.ts,
                    open: b.o,
                    high: b.h,
                    low: b.l,
                    close: b.c,
                    volume: b.v,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some(s.symbol.clone()),
                };
                (
                    (s.venue.clone(), s.symbol.clone(), s.interval.clone()),
                    vike_exec::BarSeries {
                        closed: std::sync::Arc::new(s.closed.iter().map(&to_bar).collect()),
                        forming: s.forming.as_ref().map(&to_bar),
                    },
                )
            })
            .collect(),
        mounts: Vec::new(),
        // wire stays Vec<String>; the in-process snapshot shares Arc<str> (see RecentNote::render_once)
        recent_events: w.recent_events.iter().map(|s| s.as_str().into()).collect(),
        fault: w.fault.clone(),
        conflated_market_drops: 0,
        rejected_commands: 0,
        recon: vike_core::snapshot::ReconBlock::default(),
        recon_coin_deltas: indexmap::IndexMap::new(),
        // `0` = "the node has not said", the same meaning `CoreSnapshot::empty` gives it. The
        // account-set digest is a fact about the REMOTE core's mount set and the wire does not
        // carry it (see `map_venueblock` above); inventing one here would let a Stage-3 staleness
        // check believe it had an answer from an observer that has none.
        accounts_epoch: 0,
    }
}

/// Render a daemon identity as the short display tag every status surface leads with:
/// `name [LIVE]` for a live daemon (uppercase by design — it must be impossible to miss) and
/// `name [paper]` otherwise (split-plane I3: "which daemon is the armed control channel pointing
/// at" is the spec's named most-dangerous ambiguity). One authority for the spelling — the control
/// line ([`crate::backend::tradehub_control::control_status_line`]) and the observe status
/// ([`observing_status`]) both call this, so the two strips can never disagree about a daemon.
pub fn identity_label(id: &vike_tradehub_client::wire::WireNodeIdentity) -> String {
    format!("{} [{}]", id.name, if id.live { "LIVE" } else { "paper" })
}

/// The connected observe status line — `"OBSERVING <addr> (connected)"` until a frame carries the
/// daemon's [`WireNodeIdentity`](vike_tradehub_client::wire::WireNodeIdentity), then led by
/// [`identity_label`]: `"the build runner [LIVE] — OBSERVING <addr> (connected)"`. Pure so it is
/// unit-tested below; [`spawn_bridge`]'s thread is the one caller.
///
/// # ⚠ The `(connected)` tail is LOAD-BEARING, not decoration
///
/// Every status line this loop writes is read back by
/// `crates/vike-model/src/feed_status.rs`'s `parse_feed_status` — the ONE classifier behind the
/// status strip's dot (`crates/vike-app-core/src/ui/status_dot.rs`'s `feed_dot_color`), the
/// Connections tool's Status column, and the headless daemon's health gate. Three of the four lines
/// [`spawn_bridge`] sets already match a token in it (the dial reads `Connecting`, the drop reads
/// `Disconnected`, the dial failure reads `Error`); this one matched nothing, so a HEALTHY observe
/// link classified `Unknown` — grey in the strip, the word "Unknown" in Connections.
///
/// ⚠ **And the identity-bearing variant did not read `Unknown`. It read `Connected` BY ACCIDENT.**
/// [`identity_label`] renders a live daemon as `name [LIVE]`, and `live` is in that parser's
/// connected family — so `"the build runner [LIVE] — OBSERVING …"` went green while
/// `"sim-box [paper] — OBSERVING …"` stayed grey. The dot's colour tracked whether the observed
/// daemon was ARMED rather than whether the link was up: two unrelated questions, and a coincidence
/// that no test could see because the pinned one only ever fed it the identity-less string.
///
/// The token is added HERE rather than to the parser deliberately. `parse_feed_status` is shared by
/// three crates, and teaching it `observing` would put a word only THIS producer writes into a
/// vocabulary the daemon's reconcile health gate also reads, changing how every venue's feed string
/// is classified to fix one line in the GUI. The producer is also the side that KNOWS: this function
/// is called from the `Ok(remote)` arm of the reconnect loop and from nowhere else, so the claim it
/// is making is one it is entitled to make.
pub fn observing_status(
    backend: Option<&str>,
    addr: &str,
    identity: Option<&vike_tradehub_client::wire::WireNodeIdentity>,
) -> String {
    // ⚠ WHICH address this line names is `backend_identity`'s decision, not this function's — the
    // status bar and the Connections foot strip must not name two different boxes for one
    // connection. A daemon that reported its own address wins; `addr` (the DIAL address, and
    // through a tunnel always the tunnel mouth) is what is left when it reported none.
    let addr = crate::backend::backend_identity::shown_address(
        addr,
        identity.map(|id| id.advertise_addr.as_str()),
    );
    let bare = format!("OBSERVING {addr} (connected)");
    // The daemon's OWN self-report wins: it carries the arming tag as well as a name, and it is
    // the backend answering for itself rather than the registry answering about it. The registry
    // NAME is the rung below — the only identity this side has before the first frame lands, and
    // the whole of what an operator gets on a link that never carries one.
    let lead = match identity {
        Some(id) => Some(identity_label(id)),
        None => backend.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
    };
    led_by(lead.as_deref(), &bare)
}

/// Lead `bare` with `label` — UNLESS the lead would change how
/// [`vike_model::feed_status::parse_feed_status`] classifies the line.
///
/// ⚠ **A name is operator-chosen text going into a string a CLASSIFIER reads**, and that parser
/// substring-matches a fixed vocabulary over the whole line with `disconnected`/`error`/`failed`
/// winning outright. So a backend named `error-box`, or a daemon that calls itself `failed-over`,
/// turns a HEALTHY link's dot red — the status dot then reports the NAME instead of the link, which
/// is the same class of accident [`observing_status`]'s doc records for `[LIVE]` (a live daemon's
/// tag made the line classify `Connected` for a reason that had nothing to do with the connection).
///
/// The dot's correctness outranks the label: the label is shown at full length in the Connections
/// foot strip either way, whereas a red dot on a working link is a fact nobody can check. The
/// PRODUCER is also the side entitled to decide this — teaching the shared parser about names
/// would change how every venue's feed string is classified to fix one line in the GUI.
fn led_by(label: Option<&str>, bare: &str) -> String {
    let Some(label) = label.map(str::trim).filter(|l| !l.is_empty()) else {
        return bare.to_string();
    };
    let led = format!("{label} — {bare}");
    if vike_model::feed_status::parse_feed_status(&led)
        == vike_model::feed_status::parse_feed_status(bare)
    {
        led
    } else {
        bare.to_string()
    }
}

/// Stop handle for the [`spawn_bridge`] thread: raise the flag, then JOIN.
///
/// Exists for the multi-backend GUI (split-plane B1): switching backends at runtime replaces the
/// bridge, and the reconnect loop never exits on its own — without a join, every switch would leak
/// a thread that keeps dialling the OLD address forever. [`BridgeHandle::stop`] is idempotent and
/// [`Drop`] calls it, so letting the handle fall out of scope IS a full stop-and-join.
#[must_use = "dropping the handle stops the bridge thread — hold it for the bridge's lifetime"]
pub struct BridgeHandle {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `None` once [`BridgeHandle::stop`] has joined (taken on the first call — the idempotence),
    /// or when the spawn itself failed and there is no thread to join.
    join: Option<std::thread::JoinHandle<()>>,
    /// The daemon identity the CURRENT connection last reported (split-plane I3) — written by the
    /// bridge thread on every frame whose `identity` changed, cleared on a link drop (a stale name
    /// must not outlive its connection: the next daemon at the same address may be a different —
    /// or an identity-less — node). Read by the GUI via [`BridgeHandle::identity`] so the control
    /// status line can name the daemon without `CoreSnapshot` (latency-gated vike-core) growing a
    /// display-only field.
    identity:
        std::sync::Arc<std::sync::Mutex<Option<vike_tradehub_client::wire::WireNodeIdentity>>>,
    /// The datahub dial address the CURRENT connection's `Welcome.features` advertised
    /// (split-plane REQ-2, `datahub=<addr>`) — written at connect (the handshake is where the
    /// advertisement lives, so unlike `identity` it needs no frame), cleared on a link drop under
    /// the SAME discipline: the next daemon at this address may front a different datahub, or
    /// none, and a stale advertisement must not outlive its connection. Read by the GUI via
    /// [`BridgeHandle::advertised_datahub`] into the one resolution point
    /// ([`crate::data::datahub_resolve::resolve_datahub_addr`]), where an explicit
    /// `config.datahub_addr` still always wins.
    advertised_datahub: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl BridgeHandle {
    /// The daemon identity the current connection last reported — `None` before the first
    /// identity-carrying frame, from an older (pre-B3) node, or between a link drop and the next
    /// connection's first frame. A clone (five short strings) read once per painted frame.
    pub fn identity(&self) -> Option<vike_tradehub_client::wire::WireNodeIdentity> {
        self.identity.lock().ok().and_then(|g| g.clone())
    }
    /// The datahub dial address the current connection's Welcome advertised — `None` before the
    /// first successful connect, from a daemon with no `datahub_advertise_addr` configured, or
    /// between a link drop and the next successful handshake (the advertisement is only as live
    /// as its connection). Feed it to [`crate::data::datahub_resolve::resolve_datahub_addr`] beside the
    /// explicit `config.datahub_addr` — never adopt it directly.
    pub fn advertised_datahub(&self) -> Option<String> {
        self.advertised_datahub.lock().ok().and_then(|g| g.clone())
    }
    /// Raise the stop flag and join the bridge thread. Idempotent: the join handle is taken on
    /// the first call, so a second call (or the [`Drop`] after an explicit `stop`) is a no-op.
    /// Returns promptly — the thread re-checks the flag every [`STOP_POLL`] both in the connected
    /// snapshot poll and inside the dial backoff, so a stop parks for ~one slice, never the full
    /// [`DIAL_BACKOFF`] window.
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Backoff between dial attempts — long enough not to hammer a down daemon.
const DIAL_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
/// How finely both waits are sliced so a [`BridgeHandle::stop`] lands in ~one slice: the same 50ms
/// the connected snapshot poll always used, now also the granularity of the backoff sleep.
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Spawn the `--observe` push→`CoreSnapshot` bridge thread; the returned [`BridgeHandle`] is the
/// ONLY way to stop it before process exit — hold it for exactly the bridge's intended lifetime.
///
/// The connect lives INSIDE the thread so it (a) never blocks window startup and (b) RE-DIALS on a
/// dropped link: when the daemon restarts or the tunnel blips, `is_connected()` flips false, we drop
/// the handle, back off, and reconnect from scratch (fresh handshake + subscribe) — the observer
/// heals itself instead of freezing on the last frame. The status line tracks each phase.
///
/// Moved down out of `vike-app`'s `App::new` with the rest of the wiring: this loop is the only
/// consumer of three `vike_tradehub_client::RemoteCoreHandle` APIs (`connect` / `is_connected` /
/// `snapshot`) plus [`wire_to_core`], and it lived in a file no gate compiles — so a signature
/// change in that CI-tested crate broke the observer silently.
///
/// The GUI-owned arc-swap cell and the egui repaint stay in the binary as two closures: `publish`
/// receives the already-`Arc`ed snapshot exactly as `ArcSwap::store` wants it (so nothing is copied
/// that was not copied before), and `repaint` is the same `ctx.request_repaint()` wake. Keeping the
/// cell out of the signature is what lets this crate stay free of an `arc_swap` dependency — the
/// same choice [`crate::ui::core_sync`] made.
/// `backend` is the record's NAME (`crate::backend::backend_identity::status_label`) — the one thing this
/// side knows that IDENTIFIES the box, and `None` for a record that carries none. It is an
/// `Option<String>` rather than a second `String` deliberately: two adjacent `String` parameters
/// are a swap that compiles.
pub fn spawn_bridge<P, R>(
    addr: String,
    backend: Option<String>,
    observe_key: String,
    status: std::sync::Arc<std::sync::Mutex<String>>,
    publish: P,
    repaint: R,
) -> BridgeHandle
where
    P: Fn(std::sync::Arc<vike_core::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = stop.clone();
    let identity = std::sync::Arc::new(std::sync::Mutex::new(None));
    let identity_thread = identity.clone();
    let advertised_datahub = std::sync::Arc::new(std::sync::Mutex::new(None));
    let advertised_thread = advertised_datahub.clone();
    let addr_thread = addr;
    let backend_thread = backend;
    let key_thread = observe_key;
    let join = std::thread::Builder::new()
        .name("vike-app-observe-bridge".into())
        .spawn(move || {
            let stopped = || stop_thread.load(std::sync::atomic::Ordering::Relaxed);
            let set_status = |s: &str| {
                if let Ok(mut g) = status.lock() {
                    *g = s.to_string();
                }
            };
            let set_identity = |id: &Option<vike_tradehub_client::wire::WireNodeIdentity>| {
                if let Ok(mut g) = identity_thread.lock() {
                    g.clone_from(id);
                }
            };
            let set_advertised = |a: Option<String>| {
                if let Ok(mut g) = advertised_thread.lock() {
                    *g = a;
                }
            };
            let mut last_seq = u64::MAX;
            // The identity currently RENDERED in the status line + shared cell — per connection,
            // reset on a drop (a stale daemon name must not outlive its connection).
            let mut shown_identity: Option<vike_tradehub_client::wire::WireNodeIdentity> = None;
            while !stopped() {
                // The NAME leads here too: "connecting to 127.0.0.1:7879…" is the same string on
                // every box, and this is the line an operator stares at while nothing works.
                set_status(&led_by(
                    backend_thread.as_deref(),
                    &format!("connecting to {addr_thread}…"),
                ));
                match vike_tradehub_client::RemoteCoreHandle::connect(
                    addr_thread.as_str(),
                    key_thread.as_bytes(),
                ) {
                    Ok(remote) => {
                        set_status(&observing_status(
                            backend_thread.as_deref(),
                            &addr_thread,
                            None,
                        ));
                        // The REQ-2 advertisement lives in the handshake this connect just ran —
                        // publish it for the GUI's datahub resolution (explicit key still wins,
                        // see `crate::data::datahub_resolve`). Cleared again on a link drop below.
                        set_advertised(remote.advertised_datahub().map(str::to_string));
                        tracing::info!(
                            addr = %addr_thread,
                            advertised_datahub = remote.advertised_datahub().unwrap_or("(none)"),
                            "observe: connected"
                        );
                        // Poll the LOCAL push-fed cell until the link drops (or we are told to
                        // stop) — a 50ms poll never blocks the node.
                        while remote.is_connected() && !stopped() {
                            let wire = remote.snapshot();
                            if wire.seq != last_seq {
                                last_seq = wire.seq;
                                // I3: the frame names its daemon — surface it in the observe
                                // status and the shared cell the GUI reads (only on change; the
                                // usual case is a cheap None==None / eq check per frame).
                                if wire.identity != shown_identity {
                                    shown_identity.clone_from(&wire.identity);
                                    set_identity(&wire.identity);
                                    set_status(&observing_status(
                                        backend_thread.as_deref(),
                                        &addr_thread,
                                        wire.identity.as_ref(),
                                    ));
                                }
                                publish(std::sync::Arc::new(wire_to_core(&wire)));
                                repaint();
                            }
                            std::thread::sleep(STOP_POLL);
                        }
                        if stopped() {
                            // Told to stop, not a dropped link: no "reconnecting…" status/log.
                            break;
                        }
                        // Link dropped. Force a re-store on the NEW connection's first
                        // frame (its `seq` may restart at 0), forget the dropped connection's
                        // identity AND its datahub advertisement (the next daemon may be a
                        // different node fronting a different — or no — datahub), and flag the
                        // outage.
                        last_seq = u64::MAX;
                        shown_identity = None;
                        set_identity(&None);
                        set_advertised(None);
                        tracing::warn!(addr = %addr_thread, "observe: link dropped; reconnecting");
                        set_status(&format!("disconnected from {addr_thread}, reconnecting…"));
                        repaint();
                    }
                    Err(e) => {
                        tracing::warn!(addr = %addr_thread, error = %e, "observe: reconnect failed; retrying");
                        set_status(&format!(
                            "observe connect to {addr_thread} failed ({e}); retrying…"
                        ));
                    }
                }
                // Backoff before the next dial, SLICED: the flag is re-checked every STOP_POLL so
                // a `stop()` mid-backoff returns in ~one slice, never the full window (the old
                // unsliced 2s sleep is why this thread could only die with the process — B6).
                let mut waited = std::time::Duration::ZERO;
                while waited < DIAL_BACKOFF && !stopped() {
                    std::thread::sleep(STOP_POLL);
                    waited += STOP_POLL;
                }
            }
            tracing::info!(addr = %addr_thread, "observe: bridge stopped");
        })
        .ok();
    BridgeHandle { stop, join, identity, advertised_datahub }
}

/// Build the `--observe` **`Scope::Write` WRITE path** — opt-in, default OFF.
///
/// A SECOND, dedicated connection under `Scope::Write`, built ONLY when `enabled`
/// (`VIKE_TRADEHUB_CONTROL=1`, via [`crate::backend::tradehub_control::control_enabled`] — the CALLER reads
/// that flag and the `.env` key, so this stays a pure function of its arguments) AND `key` is
/// present and non-blank. When it connects, the GUI's order buttons drive this observer's REMOTE
/// core (see `App.remote_ctrl` + the command-dispatch choke point): ⚠ **this can place/cancel REAL
/// orders on the daemon.** A missing key or a failed connect leaves it `None` and the observer
/// stays read-only.
///
/// `connect` may block briefly at startup (fine — same one-shot handshake cost as
/// [`RemoteCoreHandle::connect`](vike_tradehub_client::RemoteCoreHandle::connect)).
///
/// Moved down out of `App::new` because it is a **safety gate guarding real order placement** whose
/// refuse-paths lived in a file no test could reach; the two that need no network are pinned below.
pub fn connect_control(
    enabled: bool,
    addr: &str,
    key: Option<&str>,
) -> Option<vike_tradehub_client::RemoteControlHandle> {
    if !enabled {
        return None;
    }
    match key.filter(|k| !k.trim().is_empty()) {
        Some(key) => {
            match vike_tradehub_client::RemoteControlHandle::connect(addr, key.as_bytes()) {
                Ok(h) => {
                    tracing::warn!(
                        %addr,
                        "CONTROL channel connected — this observer can place/cancel REAL \
                         orders on the remote daemon"
                    );
                    Some(h)
                }
                Err(e) => {
                    tracing::error!(%addr, error = %e, "control connect failed; observer stays read-only");
                    None
                }
            }
        }
        None => {
            tracing::warn!(
                "VIKE_TRADEHUB_CONTROL=1 but no VIKE_TRADEHUB_CONTROL_KEY in .env; \
                 observer stays read-only"
            );
            None
        }
    }
}

#[path = "observe_bridge_tests.rs"]
#[cfg(test)]
mod observe_bridge_tests;
