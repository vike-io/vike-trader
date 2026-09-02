//! `observe_bridge` — the `--observe` thin-client's `WireSnapshot` → `CoreSnapshot` REVERSE bridge.
//!
//! The read-only observer (`vike-app --observe <ADDR>`, see `App::new`'s observe branch in
//! `vike-app`'s `main.rs`) has NO local trading core. It connects to a headless `vike-tradehub`
//! daemon's observe server under `Scope::Observe` and republishes each pushed
//! [`vike_tradehub_client::WireSnapshot`] into a GUI-owned arc-swap cell as a
//! [`vike_core::CoreSnapshot`], so every existing DOM/Trade/Portfolio panel renders the remote
//! daemon's live trading state unchanged. Charts render too: the daemon ships a bounded bar tail
//! (`WireSnapshot::bars`, ~300 closed + the forming candle per mounted series), which
//! [`wire_to_core`] rebuilds into the core's bar cache so `sync_from_core` drives the candlestick
//! engine exactly like the local GUI.
//!
//! Moved here VERBATIM from `vike-app`'s `main.rs` (the CI-excluded GUI binary) so the mapping
//! finally runs in a gate — the FORWARD projection (`CoreSnapshot` → `WireSnapshot`,
//! `vike-tradehub/src/publish.rs`) was CI-tested while this inverse was not, and
//! [`map_order_status`]'s hand-maintained Debug-spelling fallback meant a newly added
//! `vike_exec::OrderStatus` variant silently rendered as terminal `Rejected` in the observer. The
//! exhaustive round-trip test below (no-wildcard `match` over every variant) turns that silent
//! drift into a compile/test failure. `vike-app` re-imports [`wire_to_core`] under its original
//! name (the same pattern as [`crate::reconcile_config`]).

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
    }
}

/// Render a daemon identity as the short display tag every status surface leads with:
/// `name [LIVE]` for a live daemon (uppercase by design — it must be impossible to miss) and
/// `name [paper]` otherwise (split-plane I3: "which daemon is the armed control channel pointing
/// at" is the spec's named most-dangerous ambiguity). One authority for the spelling — the control
/// line ([`crate::tradehub_control::control_status_line`]) and the observe status
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
/// status strip's dot (`crates/vike-app-core/src/status_dot.rs`'s `feed_dot_color`), the
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
    addr: &str,
    identity: Option<&vike_tradehub_client::wire::WireNodeIdentity>,
) -> String {
    match identity {
        Some(id) => format!("{} — OBSERVING {addr} (connected)", identity_label(id)),
        None => format!("OBSERVING {addr} (connected)"),
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
    /// ([`crate::datahub_resolve::resolve_datahub_addr`]), where an explicit
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
    /// as its connection). Feed it to [`crate::datahub_resolve::resolve_datahub_addr`] beside the
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
/// same choice [`crate::core_sync`] made.
pub fn spawn_bridge<P, R>(
    addr: String,
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
                set_status(&format!("connecting to {addr_thread}…"));
                match vike_tradehub_client::RemoteCoreHandle::connect(
                    addr_thread.as_str(),
                    key_thread.as_bytes(),
                ) {
                    Ok(remote) => {
                        set_status(&observing_status(&addr_thread, None));
                        // The REQ-2 advertisement lives in the handshake this connect just ran —
                        // publish it for the GUI's datahub resolution (explicit key still wins,
                        // see `crate::datahub_resolve`). Cleared again on a link drop below.
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

/// Build the `--observe` **`Scope::Control` WRITE path** — opt-in, default OFF.
///
/// A SECOND, dedicated connection under `Scope::Control`, built ONLY when `enabled`
/// (`VIKE_TRADEHUB_CONTROL=1`, via [`crate::tradehub_control::control_enabled`] — the CALLER reads
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::OrderStatus;
    use vike_tradehub_client::wire::{
        WireBar, WireBarSeries, WireHeldOrderView, WireNodeIdentity, WireOrderView,
        WirePositionView, WireSnapshot, WireTradingState, WireVenueBlock,
    };

    /// A wire identity for the I3 observe-status tests — only `name` and `live` render.
    fn ident(name: &str, live: bool) -> WireNodeIdentity {
        WireNodeIdentity {
            name: name.into(),
            strategy: "spread_maker".into(),
            params: "{}".into(),
            live,
            build: "vike-tradehub 0.1.0 (abc1234)".into(),
        }
    }

    /// B3 (`identity` is `None` from an older node, or before the first frame): the identity-less
    /// observe status.
    ///
    /// ⚠ It is no longer BYTE-IDENTICAL to the pre-I3 string, and this test used to assert that it
    /// was. The `(connected)` tail is what makes a healthy observe link classify — see
    /// [`observing_status`]'s doc — and byte-identity with a string that classified as `Unknown` was
    /// never the property worth keeping; the LEADING `OBSERVING <addr>`, which is what an operator
    /// reads, is unchanged.
    #[test]
    fn observing_status_without_identity_names_the_addr_and_carries_the_token() {
        assert_eq!(
            observing_status("127.0.0.1:9301", None),
            "OBSERVING 127.0.0.1:9301 (connected)"
        );
    }

    /// I3 (split-plane): once a frame carries identity, the observe status LEADS with the daemon
    /// name and the loud uppercase `[LIVE]` tag — the read-only twin of the control line's rule.
    #[test]
    fn observing_status_with_live_identity_leads_with_name_and_uppercase_live() {
        let id = ident("the build runner", true);
        assert_eq!(
            observing_status("127.0.0.1:9301", Some(&id)),
            "the build runner [LIVE] — OBSERVING 127.0.0.1:9301 (connected)"
        );
    }

    /// A paper daemon renders the lowercase `[paper]` tag and the line carries no uppercase
    /// "LIVE" anywhere, so a glance can never read a paper daemon as live.
    #[test]
    fn observing_status_with_paper_identity_has_no_uppercase_live() {
        let id = ident("sim-box", false);
        let line = observing_status("127.0.0.1:9301", Some(&id));
        assert_eq!(line, "sim-box [paper] — OBSERVING 127.0.0.1:9301 (connected)");
        assert!(!line.contains("LIVE"), "a paper daemon must never render LIVE: {line}");
    }

    /// ⚠ THE PAPERCUT, and the accident hiding underneath it.
    ///
    /// A healthy observe link used to classify `Unknown` (grey dot, the word "Unknown" in the
    /// Connections tool) — EXCEPT when the observed daemon was live, because `identity_label`'s
    /// `[LIVE]` tag happens to contain a token in the shared parser's connected family. So the dot
    /// answered "is the daemon armed", not "is the link up". All three spellings now classify
    /// `Connected`, and the paper one is the case that proves the token rather than the coincidence
    /// is doing the work.
    #[test]
    fn every_observe_connected_spelling_now_classifies_as_connected() {
        use vike_model::feed_status::{parse_feed_status, ConnectionState};
        for line in [
            observing_status("127.0.0.1:9301", None),
            observing_status("127.0.0.1:9301", Some(&ident("the build runner", true))),
            observing_status("127.0.0.1:9301", Some(&ident("sim-box", false))),
        ] {
            assert_eq!(parse_feed_status(&line), ConnectionState::Connected, "`{line}`");
        }
        // The coincidence, demonstrated on the strings as they were: the live one classified and
        // the paper one did not, from the same healthy link.
        assert_eq!(
            parse_feed_status("the build runner [LIVE] — OBSERVING 127.0.0.1:9301"),
            ConnectionState::Connected,
            "the pre-fix live spelling classified — on the word LIVE, not on any observe token"
        );
        assert_eq!(
            parse_feed_status("sim-box [paper] — OBSERVING 127.0.0.1:9301"),
            ConnectionState::Unknown,
            "…and the pre-fix paper spelling did not: same link, different colour"
        );
    }

    /// The other three lines [`spawn_bridge`]'s loop sets, classified through the same parser — so
    /// the observe lane is now fully covered rather than covered in its healthy state only. These
    /// are spelled from the loop's own `format!`s; a change there that broke one would leave this
    /// red.
    #[test]
    fn the_other_observe_lines_still_classify_as_they_did() {
        use vike_model::feed_status::{parse_feed_status, ConnectionState};
        assert_eq!(parse_feed_status("connecting to 127.0.0.1:9301…"), ConnectionState::Connecting);
        assert_eq!(
            parse_feed_status("disconnected from 127.0.0.1:9301, reconnecting…"),
            ConnectionState::Disconnected
        );
        assert_eq!(
            parse_feed_status(
                "observe connect to 127.0.0.1:9301 failed (connection refused); retrying…"
            ),
            ConnectionState::Error
        );
    }

    /// Every `OrderStatus` variant. The no-wildcard `match` below is the COMPILE-TIME
    /// exhaustiveness pin: adding a variant to `vike_exec::OrderStatus` fails to compile right
    /// here until it is added to BOTH the list and the arm — which then feeds the round-trip
    /// assertions below. This is the audit's ask: a new variant can no longer silently fall
    /// through `map_order_status`'s unknown arm and render as terminal `Rejected` in the observer
    /// (the forward projection in vike-tradehub `publish.rs` renders `format!("{:?}", status)`,
    /// so the Debug spelling IS the wire contract).
    fn all_order_statuses() -> Vec<OrderStatus> {
        use OrderStatus as S;
        let all = vec![
            S::Initialized,
            S::Submitted,
            S::Accepted,
            S::Triggered,
            S::PartiallyFilled,
            S::Filled,
            S::Canceled,
            S::Rejected,
            S::Denied,
            S::Expired,
            S::PendingCancel,
            S::Liquidated,
            S::Emulated,
            S::Released,
        ];
        for s in &all {
            match s {
                S::Initialized
                | S::Submitted
                | S::Accepted
                | S::Triggered
                | S::PartiallyFilled
                | S::Filled
                | S::Canceled
                | S::Rejected
                | S::Denied
                | S::Expired
                | S::PendingCancel
                | S::Liquidated
                | S::Emulated
                | S::Released => {}
            }
        }
        all
    }

    /// The projection's spelling (`format!("{:?}", status)`, per vike-tradehub `publish.rs`'s
    /// `project_order`) round-trips IDENTICALLY for every variant — the Debug-spelling fallback
    /// arm of `map_order_status`.
    #[test]
    fn every_status_round_trips_through_the_debug_spelling() {
        for s in all_order_statuses() {
            assert_eq!(map_order_status(&format!("{s:?}")), s, "Debug spelling of {s:?}");
        }
    }

    /// The canonical SCREAMING_SNAKE `as_str` spelling (`OrderStatus::parse`'s vocabulary) also
    /// decodes for every variant — the first branch of `map_order_status`.
    #[test]
    fn every_status_round_trips_through_the_screaming_snake_spelling() {
        for s in all_order_statuses() {
            assert_eq!(map_order_status(s.as_str()), s, "as_str spelling of {s:?}");
        }
    }

    /// An unknown wire status decodes to terminal `Rejected` — never a live/actionable state.
    /// Wrong-case spellings of real variants are unknown too (both real vocabularies are
    /// exact-match).
    #[test]
    fn unknown_status_falls_back_to_rejected() {
        assert_eq!(map_order_status("BOGUS_STATUS"), OrderStatus::Rejected);
        assert_eq!(map_order_status(""), OrderStatus::Rejected);
        assert_eq!(map_order_status("accepted"), OrderStatus::Rejected);
        assert_eq!(map_order_status("FILLED "), OrderStatus::Rejected);
    }

    /// A fully-populated two-venue wire snapshot: venue "binance" is the PRIMARY (scalar mirror),
    /// venue "bybit" exists to prove the summed-vs-mirrored split and the `equity <= 0` ratio arm.
    fn full_wire_snapshot() -> WireSnapshot {
        let pos = WirePositionView {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: "BOTH".into(),
            size: 0.5,
            avg_px: 60_000.0,
            unrealized: 2_345.67,
            leverage: 5.0,
            liq_price: 48_000.0,
        };
        WireSnapshot {
            seq: 42,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trading_state: WireTradingState::Reducing,
            balance: 10_000.0,
            equity_total: 12_400.0,
            venues: vec![
                WireVenueBlock {
                    venue: "binance".into(),
                    balance: 10_000.0,
                    realized_pnl: 250.5,
                    fees_paid: 3.25,
                    funding_paid: -1.5,
                    equity: 12_345.67,
                    unrealized: 2_345.67,
                    missing_prices: 1,
                    margin_used: 500.0,
                    free_bp: 11_845.67,
                    trading_state: WireTradingState::Reducing,
                    positions: vec![pos.clone()],
                },
                WireVenueBlock {
                    venue: "bybit".into(),
                    balance: 100.0,
                    realized_pnl: -20.0,
                    fees_paid: 1.0,
                    funding_paid: 0.5,
                    equity: 0.0, // exercises the `equity <= 0 -> margin_ratio 0.0` arm
                    unrealized: 0.0,
                    missing_prices: 2,
                    margin_used: 400.0,
                    free_bp: 0.0,
                    trading_state: WireTradingState::Halted,
                    positions: Vec::new(),
                },
            ],
            orders: vec![WireOrderView {
                client_order_id: "c-1".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 0.5,
                order_type: "limit".into(),
                price: Some(59_000.0),
                trigger_price: None,
                status: "Accepted".into(),
                venue_order_id: Some("v-9".into()),
                filled_qty: 0.25,
                avg_fill_px: 59_100.0,
            }],
            positions: vec![pos],
            held_exits: vec![WireHeldOrderView {
                client_order_id: "c-2".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 0.5,
                order_type: "stop".into(),
                price: None,
                trigger_price: Some(55_000.0),
                parent_order_id: Some("c-1".into()),
            }],
            recent_events: vec!["OrderAccepted c-1".into()],
            fault: Some("boom".into()),
            bars: vec![WireBarSeries {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                closed: vec![WireBar {
                    ts: 1_000,
                    o: 60_000.0,
                    h: 60_100.0,
                    l: 59_900.0,
                    c: 60_050.0,
                    v: 12.5,
                }],
                forming: Some(WireBar {
                    ts: 61_000,
                    o: 60_050.0,
                    h: 60_080.0,
                    l: 60_020.0,
                    c: 60_070.0,
                    v: 3.2,
                }),
            }],
            identity: None,
        }
    }

    /// The top-level scalars, order fields, position fields (with the documented off-path
    /// defaults), and held-exit fields all map through `wire_to_core`; the observer-only fields
    /// (`marks`/`mounts`/`recon`/counters) stay empty/zero.
    #[test]
    fn wire_to_core_maps_orders_positions_and_held_exits() {
        let snap = wire_to_core(&full_wire_snapshot());

        assert_eq!(snap.seq, 42);
        assert_eq!(snap.venue, "binance");
        assert_eq!(snap.symbol, "BTCUSDT");
        assert_eq!(snap.trading_state, vike_exec::TradingState::Reducing);
        assert_eq!(snap.balance.to_bits(), 10_000.0_f64.to_bits());
        assert_eq!(snap.balance_mode, vike_exec::BalanceMode::Delta);
        let recent: Vec<&str> = snap.recent_events.iter().map(|s| &**s).collect();
        assert_eq!(recent, vec!["OrderAccepted c-1"]);
        assert_eq!(snap.fault.as_deref(), Some("boom"));

        // Whole-struct comparisons (derived PartialEq) pin every carried field at once.
        assert_eq!(
            snap.orders,
            vec![vike_core::snapshot::OrderView {
                client_order_id: "c-1".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 0.5,
                order_type: "limit".into(),
                price: Some(59_000.0),
                trigger_price: None,
                status: OrderStatus::Accepted,
                venue_order_id: Some("v-9".into()),
                filled_qty: 0.25,
                avg_fill_px: 59_100.0,
            }]
        );
        assert_eq!(
            snap.positions,
            vec![vike_core::snapshot::PositionView {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: "BOTH".into(),
                size: 0.5,
                avg_px: 60_000.0,
                unrealized: 2_345.67,
                mark_source: None, // off-path default (resolver-only)
                leverage: 5.0,
                liq_price: 48_000.0,
                margin_mode: vike_model::MarginMode::Cross, // off-path default
                isolated_margin: None,                      // off-path default
            }]
        );
        assert_eq!(
            snap.held_exits,
            vec![vike_core::snapshot::HeldOrderView {
                client_order_id: "c-2".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 0.5,
                order_type: "stop".into(),
                price: None,
                trigger_price: Some(55_000.0),
                parent_order_id: Some("c-1".into()),
            }]
        );

        // Observer-only voids: no local core state.
        assert!(snap.marks.is_empty());
        assert!(snap.mounts.is_empty());
        assert_eq!(snap.conflated_market_drops, 0);
        assert_eq!(snap.rejected_commands, 0);
        assert_eq!(snap.recon, vike_core::snapshot::ReconBlock::default());
        assert!(snap.recon_coin_deltas.is_empty());
    }

    /// `build_portfolio`'s primary-vs-summed derivation: the scalar fields MIRROR `venues[0]`
    /// (never summed — exactly as `CoreSnapshot::build` populates them), while
    /// `margin_used_total`/`missing_prices_total` ARE cross-venue sums and `equity_total` is
    /// carried from the wire aggregate.
    #[test]
    fn build_portfolio_mirrors_primary_and_sums_the_totals() {
        let wire = full_wire_snapshot();
        let p = build_portfolio(&wire);

        assert_eq!(p.equity.to_bits(), 12_345.67_f64.to_bits(), "mirrors venues[0], not summed");
        assert_eq!(p.equity_total.to_bits(), 12_400.0_f64.to_bits(), "the wire aggregate");
        assert_eq!(p.realized_pnl.to_bits(), 250.5_f64.to_bits());
        assert_eq!(p.fees_paid.to_bits(), 3.25_f64.to_bits());
        assert_eq!(p.funding_paid.to_bits(), (-1.5_f64).to_bits());
        assert_eq!(p.margin_used_total.to_bits(), 900.0_f64.to_bits(), "500 + 400 summed");
        assert_eq!(p.missing_prices_total, 3, "1 + 2 summed");
        assert!(p.balances_by_asset.is_empty(), "not on the wire");
        assert_eq!(p.venues.len(), 2);
    }

    /// An empty wire snapshot (no venues) yields the 0.0 primary-mirror fields — the `unwrap_or`
    /// arms, never a panic. The `to_bits` comparisons pin POSITIVE zero: `margin_used_total`
    /// must come from the naive fold (std's float `Sum` empty identity is `-0.0`, which would
    /// diverge bitwise from `CoreSnapshot::empty`'s `Portfolio::default()` placeholder).
    #[test]
    fn build_portfolio_with_no_venues_zeroes_the_primary_mirror() {
        let p = build_portfolio(&WireSnapshot::empty());
        assert_eq!(p.equity.to_bits(), 0.0_f64.to_bits());
        assert_eq!(p.realized_pnl.to_bits(), 0.0_f64.to_bits());
        assert_eq!(p.margin_used_total.to_bits(), 0.0_f64.to_bits());
        assert_eq!(p.missing_prices_total, 0);
        assert!(p.venues.is_empty());
    }

    /// `map_venueblock` recomputes `margin_ratio` from the wired `margin_used`/`equity` (the wire
    /// does not carry it), guards the `equity <= 0` arm to 0.0, and defaults the GUI-irrelevant
    /// core internals (`balance_mode` Delta / `fee_schedule` None / empty 1.0-default multiplier
    /// grid).
    #[test]
    fn map_venueblock_recomputes_margin_ratio_and_defaults_the_internals() {
        let wire = full_wire_snapshot();
        let primary = map_venueblock(&wire.venues[0]);
        assert_eq!(
            primary.margin_ratio.to_bits(),
            (500.0_f64 / 12_345.67_f64).to_bits(),
            "recomputed margin_used / equity"
        );
        assert_eq!(primary.balance_mode, vike_exec::BalanceMode::Delta);
        assert_eq!(primary.fee_schedule, None);
        assert!(primary.multipliers.is_empty());
        assert_eq!(primary.multiplier_default.to_bits(), 1.0_f64.to_bits());
        assert_eq!(primary.trading_state, vike_exec::TradingState::Reducing);
        assert_eq!(primary.positions.len(), 1);

        let zero_equity = map_venueblock(&wire.venues[1]);
        assert_eq!(
            zero_equity.margin_ratio.to_bits(),
            0.0_f64.to_bits(),
            "equity <= 0 guards the division to 0.0"
        );
        assert_eq!(zero_equity.trading_state, vike_exec::TradingState::Halted);
    }

    /// The bounded wire bar tail is rebuilt into the core's `(venue, symbol, interval)` bar cache:
    /// closed bars + the forming candle, with the symbol stamped and the wire-absent `Bar` fields
    /// (`funding`/`bid`/`ask`) `None`.
    #[test]
    fn wire_to_core_rebuilds_the_bar_cache() {
        let snap = wire_to_core(&full_wire_snapshot());
        let key = ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string());
        let series = snap.bars.get(&key).expect("wire series lands under its (v, s, i) key");
        assert_eq!(
            *series.closed,
            vec![vike_model::Bar {
                ts: 1_000,
                open: 60_000.0,
                high: 60_100.0,
                low: 59_900.0,
                close: 60_050.0,
                volume: 12.5,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }]
        );
        assert_eq!(
            series.forming,
            Some(vike_model::Bar {
                ts: 61_000,
                open: 60_050.0,
                high: 60_080.0,
                low: 60_020.0,
                close: 60_070.0,
                volume: 3.2,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            })
        );
    }

    // ── connect_control: the refuse-paths of a gate that guards REAL order placement ─────────
    //
    // Every case below must return `None` WITHOUT dialling, so the address is a deliberately
    // unroutable placeholder: if a refactor ever lets one of these reach `RemoteControlHandle::
    // connect`, the test hangs/fails instead of silently arming a write path. The connecting
    // path itself needs a live daemon and stays uncovered here.
    const NEVER_DIALLED: &str = "127.0.0.1:0";

    /// Default OFF. `VIKE_TRADEHUB_CONTROL` unset ⇒ read-only observer, whatever key is present —
    /// a key in `.env` must never be sufficient on its own.
    #[test]
    fn control_disabled_never_connects_even_with_a_valid_looking_key() {
        assert!(connect_control(false, NEVER_DIALLED, Some("a-real-looking-key")).is_none());
        assert!(connect_control(false, NEVER_DIALLED, None).is_none());
    }

    /// Enabled but no key ⇒ read-only. The observer must degrade, not dial with an empty secret.
    #[test]
    fn control_enabled_without_a_key_stays_read_only() {
        assert!(connect_control(true, NEVER_DIALLED, None).is_none());
    }

    /// A present-but-blank key (an empty or whitespace-only `.env` line — the realistic
    /// mis-configuration) is treated as ABSENT, not as a zero-length secret to authenticate with.
    #[test]
    fn a_blank_or_whitespace_only_key_counts_as_absent() {
        for blank in ["", " ", "\t", "  \n "] {
            assert!(
                connect_control(true, NEVER_DIALLED, Some(blank)).is_none(),
                "{blank:?} must not arm the control channel"
            );
        }
    }

    // ── spawn_bridge: the stop handle (split-plane B6) ───────────────────────────────────────
    //
    // Both tests dial a DEAD loopback address (bind an ephemeral port, then drop the listener),
    // so every connect attempt is refused instantly and the thread spends its life in the dial
    // backoff — exactly where an unsliced 2s sleep would park a `stop()` for the full window.

    /// Bind-then-drop an ephemeral loopback port: nothing listens there afterwards, so
    /// `RemoteCoreHandle::connect` fails FAST (refused) instead of hanging the test on a dial.
    fn dead_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("ephemeral local_addr").to_string();
        drop(listener);
        addr
    }

    fn spawn_dead_bridge() -> BridgeHandle {
        spawn_bridge(
            dead_addr(),
            "test-key".into(),
            std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            |_snap| {},
            || {},
        )
    }

    /// `stop()` returns well under the 2s dial backoff: the backoff sleep is SLICED with the
    /// flag re-checked between slices, so a mid-backoff stop parks for ~one slice, never the
    /// whole window. The 1s bound is 20x the slice and half the backoff — a regression to one
    /// unsliced sleep fails it deterministically.
    #[test]
    fn stop_returns_well_under_the_dial_backoff() {
        let mut handle = spawn_dead_bridge();
        // Let the thread through its first (refused) dial and into the backoff sleep.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let t0 = std::time::Instant::now();
        handle.stop();
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1000),
            "stop() took {elapsed:?} — the backoff sleep is not sliced"
        );
        handle.stop(); // idempotent: the join handle is already taken; returns immediately
    }

    /// Dropping the handle stops the thread too (`Drop` = `stop()`), so a caller that simply
    /// lets it fall out of scope — the B1 runtime backend switch — leaks no dialling thread.
    #[test]
    fn drop_stops_the_bridge_thread() {
        let handle = spawn_dead_bridge();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let stop_flag = handle.stop.clone(); // same-module test: reach the private flag
        let t0 = std::time::Instant::now();
        drop(handle);
        let elapsed = t0.elapsed();
        assert!(
            stop_flag.load(std::sync::atomic::Ordering::Relaxed),
            "drop must raise the stop flag"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1000),
            "drop took {elapsed:?} — Drop must stop-and-join promptly"
        );
    }
}
