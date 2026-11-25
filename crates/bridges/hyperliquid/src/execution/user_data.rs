//! Hyperliquid private-WS pump — `orderUpdates` (FSM lane) + `userFills` (Account lane).
//!
//! Unlike the Bybit/OKX private WS, HL's user streams are **keyless**: there is no auth handshake —
//! a subscription is scoped to a wallet ADDRESS (`{"method":"subscribe","subscription":{"type":…,
//! "user":<MASTER address>}}`). So `open_websocket` just connects (via [`vike_bridge_core::websocket`]) and sends
//! the two subscribe frames; the shared reliability loop (`vike_bridge_core::websocket_session::user`'s `spawn_user`) then drives
//! recv → [`map_frame_to_events`] → emit with reconnect/backoff (each reconnect re-runs `open_websocket`,
//! replaying the subscriptions). Keepalive is `{"method":"ping"}` every ≤30s (research §9 — the server
//! drops a connection idle >60s); it self-skips a non-JSON `pong`.
//!
//! ## Remapping (why the pump needs the registry + symbology)
//! The pure [`crate::execution::event_mapper`] carries the venue `cloid` (`0x…`) and `coin` verbatim; the pump
//! remaps them onto the framework identifiers the core folds on:
//! - **cloid → framework order client id** via the shared [`OrderClientIdVenueRegistry`] exec populated at submit (HL's
//!   `cloid = keccak(order client id)` is one-way, so this table is the only inverse). An unknown cloid is left
//!   as-is → the core drops it as not-ours.
//! - **coin → unified symbol** via the shared [`Symbology`] (perp `coin == symbol`; spot `"@107" →
//!   "HYPE/USDC"`), so the Account fold keys positions by the unified symbol.
//!
//! ## userFills snapshot — why a RECONNECT snapshot is admitted, not dropped
//! HL replays recent fills as an `isSnapshot` frame on subscribe, **and again on every reconnect**.
//! Those two facts are not symmetric: the fills that executed **while the socket was down** exist
//! ONLY in the reconnect snapshot — HL streams no per-fill catch-up, and this pump carries no
//! `since`/watermark re-request. So dropping a later snapshot wholesale (what the original
//! `snapshot_seen` latch did — one latch per pump thread, never reset) silently lost every fill of
//! any reconnect-with-activity: platform position and realized PnL then disagree with the venue,
//! with no trace, recoverable only by the reconcile driver — which is ON by default for a live
//! start since 2026-09-06, but under `quarantine`, so recovery still needs an operator confirm.
//!
//! A replay snapshot is therefore **admitted**, minus the rows this pump already saw. Re-folding is
//! not a hazard, because dedup belongs one layer down and always has: `Engine::on_event`
//! drops any `Event::Fill` whose `trade_id` is already in `seen_trade_ids` — a set that is only ever
//! inserted into (never pruned, never evicted) for the life of the process, and re-seeded from the
//! journal/reconcile on restart. HL's `tid` is that key, unique per fill. This is the SAME contract
//! alpaca's `since_id` boundary re-delivery and bybit's `execution.fast`/`execution` twin lean on.
//!
//! **The pump keeps its own ledger of the `tid`s it has decoded, and a replay row already in it is
//! left out** (`OrderClientIdVenueRegistry::first_sight_of_fill`). The engine's dedup cannot be the only one: a
//! FOREIGN fill (the account's history, a manual trade) on a symbol two of the daemon's accounts
//! both claim is dropped by the core's router as UNATTRIBUTED, at `warn!`, and never reaches
//! `seen_trade_ids` — so every reconnect re-announced it (the CI box, 2026-10-18: two testnet fills, every
//! ~11 minutes). The ledger records floor-dropped rows too, so history the first snapshot refused is
//! not smuggled in by the first reconnect. A gap fill is by definition unseen and passes.
//!
//! The `snapshot_seen` latch survives, demoted from a wholesale drop gate to a **first-vs-replay
//! flag**: it tags the `info!` that distinguishes a gap-repair snapshot from a first connect, and it
//! selects the ONE frame the history floor below applies to.
//!
//! ## The FIRST snapshot is account HISTORY, and it gets a floor (the restart law)
//! The paragraph above is about the SECOND and later snapshots. The first one is a different object:
//! HL replays an arbitrary suffix of ACCOUNT history on subscribe, so on a fresh process it is
//! mostly (or entirely) activity from BEFORE this process existed — a previous session's fills, a
//! manual trade in HL's own UI, another bot on the same address. `vike_tradehub_core::make_engine` builds
//! ONE `Account::new(1.0, venue, .., BalanceMode::Delta)` with `realized_pnl`/`balance`/`fees_paid`
//! at `0.0` and an EMPTY fill-dedup ledger (`vike_execution::Account`'s `seen_fill_ids`), and
//! `vike_execution::Account`'s `seed_seen_fill_ids` doc says seeding is the CALLER's job and no shipped
//! binary does it (`git grep seed_seen_fill_ids` finds no caller outside vike-execution) — so those rows
//! fold into zero:
//! `balance -= commission` and `fees_paid += commission` fire per admitted row, monotone and
//! uncompensated, and a snapshot that starts mid-round-trip opens a phantom position of arbitrary
//! sign. This is the SAME hazard, on the same venue's money lane, that
//! `vike_bridge_core::exec_actor::run_loop`'s history floor exists for — measured on the CI box as an
//! equity step equal to prior sessions' costs. HL just reaches that lane through a snapshot instead
//! of through a `resync` closure, and reaches neither `run_loop` nor `run_user_resync` at all.
//!
//! ⚠ **The floor is a CONJUNCTION, and the clock half ALONE would be a fill-loss bug.** A row is
//! dropped from the first snapshot only when BOTH hold:
//!   1. `OrderClientIdVenueRegistry::resolve` returns `None` — this process did not mint the order; **and**
//!   2. `vike_bridge_core::exec_actor::is_pre_spawn` — a POSITIVE `time` strictly below the floor.
//!
//! A time-only floor is refuted by this pump's own reliability loop: the first SUCCESSFUL connect is
//! unsynchronised with anything. `run_user`'s `WebsocketSessionUserOpenOutcome::Transport` arm
//! sleeps and retries with a backoff doubling to the exec lane's reconnect ceiling, and `open_websocket` returns
//! `Transport` for a failed connect AND for a failed subscribe send — while `crate::execution`'s `run`
//! is already draining `Submit` commands and placing real orders over signed REST. So "nothing
//! this process placed can have filled yet" is FALSE, and the fill of such an order exists in the
//! first snapshot and NOWHERE ELSE (no per-fill catch-up, no `since`/watermark re-request — the
//! same fact that makes a reconnect snapshot admissible above). Clock skew is no defence either:
//! HL's nonce window is `(T − 2 days, T + 1 day)` (`crate::signing::hash`'s `NonceManager`), so
//! skew here is not bounded by a `recvWindow` the way the two in-crate lanes' is.
//!
//! Condition 1 is the correction, and it is exact rather than heuristic: `OrderClientIdVenueRegistry::register` is
//! called at submit, `cloid = keccak(order client id)` with a `uuid4().hex[:8]` session prefix, and that map is
//! never pruned for the life of the process — so a resolving cloid PROVES the order is ours and the
//! row is admitted whatever the clock says. Only **foreign AND pre-spawn** is dropped: precisely
//! "someone else's, from before we existed". Everything else about the lane is unchanged — a replay
//! snapshot, an incremental frame, and an unstamped (`time == 0`) row are never floored, and
//! `spawn_ms == 0` is no floor at all.
//!
//! ⚠ **Restored orders are remapped but NOT "ours" for the floor** (decision 0121). After a restart,
//! `adopt_restored` seeds [`OrderClientIdVenueRegistry::register_restored`] with the cloids of orders a PREVIOUS
//! session left resting, so their WS events and fills resolve to the real order client id. The adoption runs at
//! the first reconcile pass, which may land BEFORE this pump's first snapshot: were such a cloid read
//! as authorship, the snapshot would admit the previous session's pre-spawn fills of that order into
//! a fresh `Account`, the phantom position and charged-twice commission this floor exists to stop.
//! So condition 1 reads "resolves AND was not restored". Only the first snapshot is floored, so a
//! restored order's fills from now on (incremental frames) and any reconnect snapshot are admitted;
//! what filled while the process was down is the reconcile pass's (`MissingFill`, under the policy).
//!
//! The one shape the engine's dedup provably cannot cover — a fill carrying no `tid`, which would
//! re-fold on every reconnect and double-count forever — is no longer this module's problem: the
//! [`vike_model::events::TradeId`] newtype cannot hold `""`, so `crate::execution::event_mapper::map_user_fills`
//! refuses such a row outright (counted and `warn!`ed there). That drop is now unconditional rather
//! than replay-only, which is strictly safer: `seen_trade_ids` cannot hold an id that does not
//! exist, so admitting one on first connect only postponed the double-count to the next reconnect.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::Value;

use vike_bridge_core::exec_actor::is_pre_spawn;
use vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite;
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserHandle, WebsocketSessionUserOpenOutcome, WebsocketSessionUserSpec,
    dial_user_stream, spawn_user,
};
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings;

use crate::execution::OrderClientIdVenueRegistry;
use crate::execution::event_mapper::{is_cancel, map_order_updates, map_user_fills};
use crate::instrument::properties::SharedInstruments;
use crate::instrument::symbology::Symbology;
use crate::transport::consts::VENUE;

// No cadence is declared in this file. The venue's own — the app-level ping and the silent-stall
// watchdog — are the exec lane of `venue_timings::HYPERLIQUID`, read where each is used in
// `spawn_hyperliquid_user_data`; the row carries their reasons (the watchdog is keyed off HL's
// pong-per-ping cadence, NOT data cadence: an account with no orders is legitimately silent). The
// venue-free one is `vike_bridge_core::websocket_session::user`'s `USER_DATA_POLL` (the recv loop wakes this
// often to check `stop`); the reconnect ladder is the same lane's `reconnect` row (1 s doubling to 30 s).

/// The HL app-level keepalive frame (`{"channel":"pong"}` comes back; the pump tolerates it). Public for the link
/// harness (`crates/vike-bridge-core/tests/link_conformance.rs`), which sends it at the row's cadence.
pub const PING_FRAME: &str = r#"{"method":"ping"}"#;

/// Rows the first-`userFills`-snapshot history floor has dropped since process start.
///
/// A drop on the money lane is never silent here — this is the counter half of the local convention
/// `vike_execution::Account`'s `duplicate_fills_refused` / `colliding_fills_refused` set (a `pub` count
/// beside a `tracing` line), in the one shape a free function can hold it: the process-global
/// `static` + accessor pair `vike_polymarket::execution::rest`'s `rate_gate_would_block_count` already uses.
static FIRST_SNAPSHOT_FILLS_DROPPED: AtomicU64 = AtomicU64::new(0);

/// How many rows the first-`userFills`-snapshot floor has dropped since process start — FOREIGN
/// (no cloid this process minted) AND stamped before the pump's spawn. Bounded by construction: the
/// floor is consulted for exactly ONE frame per pump thread, so this can only ever advance once.
///
/// Nonzero is NORMAL on a restart of an account with history and is NOT a fault. What it means is
/// that the platform's books deliberately do not contain that pre-start activity; adopting it is
/// the reconcile driver's job (ON by default for a live start, but `quarantine` HOLDS what it
/// finds, so adoption still needs an operator confirm) or `vike_execution::Account`'s
/// `seed_seen_fill_ids`'.
pub fn first_snapshot_fills_dropped() -> u64 {
    FIRST_SNAPSHOT_FILLS_DROPPED.load(Ordering::Relaxed)
}

/// The reconcile nudge of a reopen (`vike_venues::venue_connection::HYPERLIQUID`'s `nudges_reconcile`): sent once the
/// NEW session's first `userFills` snapshot is decoded, never at the re-open itself. That snapshot is how this stream
/// reads a gap back (the module doc), so a pass poked at the re-open reads the venue against a book the gap's fills
/// have not reached yet. Never on the first connect (no gap); a `None` trigger is reconcile off.
///
/// shortcut: sent as the snapshot is decoded, just before the pump hands its rows on (same thread, at once: the pump's
/// decode seam has no point after the emit) while the pass it starts reads the venue over REST first; upgrade if the
/// shared pump grows an after-emit hook.
pub struct ReconnectNudge {
    trigger: Option<std::sync::mpsc::Sender<()>>,
    /// A re-open waits for its session's snapshot.
    due: AtomicBool,
}

impl ReconnectNudge {
    pub fn new(trigger: Option<std::sync::mpsc::Sender<()>>) -> Self {
        Self { trigger, due: AtomicBool::new(false) }
    }

    /// The pump re-opened the session (`run_user`'s `on_reconnect`): the next `userFills` snapshot carries the gap.
    pub fn reopened(&self) {
        self.due.store(true, Ordering::Relaxed);
    }

    /// One decoded frame: the first `userFills` snapshot after a re-open sends the nudge (a dropped receiver, the
    /// reconcile driver gone, is ignored).
    pub fn decoded(&self, frame: &Value) {
        let snapshot = frame.get("channel").and_then(Value::as_str) == Some("userFills")
            && frame.pointer("/data/isSnapshot").and_then(Value::as_bool) == Some(true);
        if snapshot
            && self.due.swap(false, Ordering::Relaxed)
            && let Some(trigger) = &self.trigger
        {
            let _ = trigger.send(());
        }
    }
}

/// Build a `{"method":"subscribe","subscription":{"type":<sub_type>,"user":<user>}}` frame.
pub fn subscribe_frame(sub_type: &str, user: &str) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": sub_type, "user": user }
    })
    .to_string()
}

/// Connect + configure + send the `orderUpdates` and `userFills` subscribe frames. No auth handshake
/// (HL user streams are address-scoped/keyless). The socket is left open on `Ready`; a connect or
/// subscribe-send failure returns [`WebsocketSessionUserOpenOutcome::Transport`] so the reliability loop reconnects.
pub fn open_websocket(
    websocket_url: &str,
    order_sub: &str,
    fills_sub: &str,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    let socket = match dial_user_stream(websocket_url, CONNECT_10S) {
        Ok(socket) => socket,
        Err(words) => return WebsocketSessionUserOpenOutcome::Transport(words),
    };
    let mut stream = WebsocketSessionUserStreamTungstenite(socket);
    for sub in [order_sub, fills_sub] {
        if stream.send_text(sub).is_err() {
            return WebsocketSessionUserOpenOutcome::Transport("subscribe send failed".to_string());
        }
    }
    WebsocketSessionUserOpenOutcome::Ready(stream)
}

/// Map ONE inbound WS frame to canonical events, remapping cloid→order client id and coin→symbol. Pure over the
/// borrowed frame + the shared symbology/registry + the `snapshot_seen` flag + the history floor —
/// the offline-testable core of the pump (the reliability loop just recv→this→emit). `pub` so the
/// crate's offline reliability test can drive it through the REAL `run_user` loop across
/// a scripted reconnect (the aster/binance `*_userdata.rs` shape), rather than re-implementing the
/// decode.
///
/// `spawn_ms` is the PUMP THREAD's spawn instant in epoch ms and applies to the FIRST `userFills`
/// snapshot only — see the module doc's restart-law section for the whole argument, including why a
/// clock-only floor would lose a fill. `0` means **no floor** (an unfloored test or probe), the same
/// hatch `crates/bridges/hyperliquid/src/history/funding.rs`'s `HyperliquidFundingPoller` gives its `floor_ms`.
///
/// ⚠ It must be sampled by the CALLER, once, OUTSIDE the reliability loop
/// ([`spawn_hyperliquid_user_data`] does). A floor re-sampled per session would sit AFTER fills that
/// executed while the first connect was still failing its backoff — and on this venue those fills
/// exist in the first snapshot and nowhere else.
pub fn map_frame_to_events(
    frame: &Value,
    symbology: &Symbology,
    registry: &OrderClientIdVenueRegistry,
    snapshot_seen: &AtomicBool,
    spawn_ms: i64,
) -> Vec<Event> {
    match frame.get("channel").and_then(|c| c.as_str()) {
        Some("orderUpdates") => {
            // Drop a stale `canceled` for an oid a native modify just retired (same cloid → a NEW
            // oid); the fast path (nothing retired) borrows the frame untouched.
            let frame = suppress_retired_cancels(frame, registry);
            learn_live_order_ids(&frame, registry);
            let mut evs = map_order_updates(&frame, VENUE);
            for event in &mut evs {
                remap_order_client_id(event, registry);
                remap_symbol(event, symbology);
            }
            evs
        }
        Some("userFills") => {
            let uf = map_user_fills(frame, VENUE);
            // A snapshot arrives on EVERY (re)connect, and the fills that executed while the socket
            // was down exist ONLY in it — so a replay snapshot is ADMITTED, not dropped. The
            // engine's never-pruned `seen_trade_ids` collapses the ones already folded; the flag
            // only distinguishes first-connect from replay. See the module doc for the full why.
            let replay = uf.is_snapshot && snapshot_seen.swap(true, Ordering::Relaxed);
            // The FIRST snapshot is the only frame the history floor applies to: it is an arbitrary
            // suffix of ACCOUNT history rather than this session's activity. `uf.is_snapshot &&` is
            // load-bearing — without it `!replay` is true for EVERY incremental frame forever, and
            // the floor would silently eat live fills.
            let first_snapshot = uf.is_snapshot && !replay;
            // The untagged-fill drop that used to live HERE now happens one layer up, in
            // `map_user_fills`, and unconditionally: `FillEvent.trade_id` is a
            // `vike_model::events::TradeId`, which cannot hold `""`, so a `tid`-less row can no
            // longer be built into a fill at all — first connect included. That is stricter than
            // the replay-only drop this block performed and strictly safer: the engine's
            // `seen_trade_ids` cannot hold an id that does not exist, so a first-connect admission
            // only deferred the same double-count to the first reconnect. The count and the
            // `warn!` moved with it (the mapper knows the field name).
            let fills = uf.fills;
            let mut dropped = 0usize;
            let mut replayed = 0usize;
            let evs: Vec<Event> = fills
                .into_iter()
                .filter_map(|mut f| {
                    // EVERY row's id is recorded here, floor-dropped rows included, so a replay can
                    // tell a row this process already saw from a gap fill. A REPLAY row seen before
                    // is left out: it is history this pump already handed on (or deliberately
                    // floored), and re-emitting it only made the core drop it again — at WARN, on
                    // every reconnect, when the fill is foreign and several accounts claim its
                    // symbol. Only a replay is filtered: a first-time row (a gap fill) and every
                    // incremental row pass exactly as before.
                    if !registry.first_sight_of_fill(&f.trade_id) && replay {
                        replayed += 1;
                        return None;
                    }
                    // `Some` PROVES this process minted the order: exec registers `keccak(order client id)` at
                    // submit under a per-process `uuid4` prefix, and that map is never pruned for the
                    // life of the process. So a resolving cloid admits the row whatever the clock
                    // says — the half a time-only floor gets wrong.
                    //
                    // ⚠ Except a RESTORED cloid (`OrderClientIdVenueRegistry::register_restored`, seeded by
                    // `adopt_restored`): it resolves, but a previous session minted it, so it is not
                    // "ours" for the floor (module doc, "restored orders").
                    let resolved = registry.resolve(&f.order_client_id);
                    // No cloid at all (the mapper's `oid` fallback): an order placed by hand in the
                    // venue's app, never one of ours (every order we sign carries a cloid).
                    let no_cloid = resolved.is_none() && !f.order_client_id.starts_with("0x");
                    let ours = match resolved {
                        Some(order_client_id) => {
                            let restored = registry.is_restored(&f.order_client_id);
                            f.order_client_id = order_client_id;
                            !restored
                        }
                        None => false,
                    };
                    if let Some(instrument) = symbology.by_coin(f.symbol.as_str()) {
                        f.symbol = instrument.symbol.as_str().into();
                        // A spot fee paid in the base token comes out of the quantity (decision 0218).
                        super::spot::net_fill_event(&mut f, instrument);
                        // ...and a SPOT fill of an order placed by hand carries no client id, so the
                        // fold keeps it out of the strategy's position (the spot rule). A perpetual's
                        // keeps its `oid` as ever; a cloid this process does not know keeps its hash.
                        if no_cloid && instrument.asset_class == vike_model::AssetClass::CryptoSpot
                        {
                            f.order_client_id.clear();
                        }
                    }
                    let event = Event::Fill(f);
                    // FOREIGN **and** pre-spawn — "someone else's, from before we existed". Either
                    // condition alone admits. `is_pre_spawn` is the SHARED predicate (a second
                    // spelling of "older than my process" is this repo's recurring defect) and it
                    // already gives `spawn_ms == 0` ⇒ no floor and `time == 0` ⇒ admit, so neither
                    // needs a rival check here.
                    if first_snapshot && !ours && is_pre_spawn(&event, spawn_ms) {
                        dropped += 1;
                        return None;
                    }
                    Some(event)
                })
                .collect();
            if replay {
                tracing::info!(
                    admitted = evs.len(),
                    already_seen = replayed,
                    "userFills reconnect snapshot (gap repair): rows not seen before are admitted, \
                     replayed rows are left out; the engine dedups on `tid` as well"
                );
            }
            if dropped > 0 {
                FIRST_SNAPSHOT_FILLS_DROPPED.fetch_add(dropped as u64, Ordering::Relaxed);
                // WARN, not INFO and not ERROR. Not `info!`: `exec_actor::run_loop`'s twin is `info!`
                // because it is a 5s-cadence background lane that would spam, whereas this fires at
                // most ONCE per process and its subject is money that an operator comparing platform
                // equity against the venue's must be able to find. Not `error!`: it is the EXPECTED
                // outcome of restarting against an account with history, not a fault.
                tracing::warn!(
                    venue = VENUE,
                    dropped,
                    admitted = evs.len(),
                    spawn_ms,
                    "first `userFills` snapshot: dropped rows that are FOREIGN (no cloid this \
                     process minted) and stamped before this pump's spawn — HL replays a suffix of \
                     ACCOUNT history on subscribe, and this Account starts flat with an empty \
                     fill-dedup ledger, so folding them would subtract a previous session's \
                     commissions from a zero-based balance and open a phantom position. Pre-start \
                     state is the reconcile driver's job (on by default for a live mount, and \
                     quarantine-HELD there), not this lane's. A fill of one \
                     of OUR orders is admitted whatever its stamp."
                );
            }
            evs
        }
        // `pong` / `subscriptionResponse` / an unknown channel carry nothing to fold.
        _ => Vec::new(),
    }
}

/// Is this `orderUpdates` entry a cancellation, by [`is_cancel`]?
fn is_cancel_status(upd: &Value) -> bool {
    upd.get("status").and_then(|s| s.as_str()).is_some_and(is_cancel)
}

/// Record, per registered cloid, the oid of each `open`/`resting` row: a cancel-replace keeps the
/// cloid on a NEW oid, and exec reads it back ([`OrderClientIdVenueRegistry::take_live_oid`]) when a `modify`
/// reply left the new oid unknown. Rows of other statuses or without a cloid/oid are ignored.
fn learn_live_order_ids(frame: &Value, registry: &OrderClientIdVenueRegistry) {
    let Some(data) = frame.get("data").and_then(|d| d.as_array()) else { return };
    for upd in data {
        if matches!(upd.get("status").and_then(Value::as_str), Some("open" | "resting"))
            && let Some(order) = upd.get("order")
            && let (Some(cloid), Some(oid)) = (
                order.get("cloid").and_then(Value::as_str),
                order.get("oid").and_then(Value::as_u64),
            )
        {
            registry.learn_live_oid(cloid, oid);
        }
    }
}

/// Suppress a stale `canceled` for an oid a native modify retired (see [`OrderClientIdVenueRegistry::retire_oid`]):
/// a cancel-replace keeps the cloid on a NEW oid, so a `canceled` streamed for the OLD oid would map
/// (via the shared cloid) to an [`Event::OrderCanceled`] that wrongly terminates the live re-placed
/// order. Scans `data` for cancel entries whose `order.oid` is retired; if any, returns a CLONE with
/// them removed, else borrows the frame unchanged (the steady-state fast path — no clone, no lock
/// contention beyond the empty-set check).
fn suppress_retired_cancels<'a>(
    frame: &'a Value,
    registry: &OrderClientIdVenueRegistry,
) -> Cow<'a, Value> {
    let Some(data) = frame.get("data").and_then(|d| d.as_array()) else {
        return Cow::Borrowed(frame);
    };
    let mut drop_index: Vec<usize> = Vec::new();
    for (i, upd) in data.iter().enumerate() {
        if is_cancel_status(upd)
            && let Some(oid) = upd.get("order").and_then(|o| o.get("oid")).and_then(Value::as_u64)
            && registry.take_retired(oid)
        {
            drop_index.push(i);
        }
    }
    if drop_index.is_empty() {
        return Cow::Borrowed(frame);
    }
    let kept: Vec<Value> = data
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop_index.contains(i))
        .map(|(_, v)| v.clone())
        .collect();
    let mut owned = frame.clone();
    owned["data"] = Value::Array(kept);
    Cow::Owned(owned)
}

/// Remap a fill-carrying event's `symbol` from the venue `coin` to the unified symbol. Shared with
/// exec's `/exchange` response path so both lanes tag positions by the same symbol.
pub(crate) fn remap_symbol(event: &mut Event, symbology: &Symbology) {
    if let Some(fill) = event.fill_mut()
        && let Some(instrument) = symbology.by_coin(fill.symbol.as_str())
    {
        fill.symbol = instrument.symbol.as_str().into();
        super::spot::net_fill_event(fill, instrument);
    }
}

/// Remap an order event's `order_client_id` from the venue cloid to the framework order client id (leave it
/// unchanged when the cloid isn't ours — the core then drops it as an unknown order).
fn remap_order_client_id(event: &mut Event, registry: &OrderClientIdVenueRegistry) {
    if let Some(order_client_id) = event.order_client_id_mut()
        && let Some(mapped) = registry.resolve(order_client_id)
    {
        *order_client_id = mapped;
    }
}

/// Spawn the persistent Hyperliquid private-WS pump feeding the th-core ingest.
///
/// `master_address` is the MASTER account address the subscriptions are scoped to (agent-wallet
/// queries return empty — research §8). `instruments`/`registry` are shared with the exec thread for
/// the coin→symbol / cloid→order client id remaps (the pump only reads; the exec thread replaces the universe
/// when an order names a coin listed after the start, and the next frame is remapped through it).
/// Deterministic teardown via the returned [`WebsocketSessionUserHandle`] (`shutdown()` raises stop + joins).
///
/// The pump is the shared private stream with no REST replay (`spawn_user`): HL re-sends its
/// `userFills` snapshot on every subscribe, so a reconnect needs no history fetch, only the
/// reconcile nudge, sent once that snapshot is decoded ([`ReconnectNudge`]).
pub fn spawn_hyperliquid_user_data(
    websocket_url: String,
    master_address: String,
    instruments: SharedInstruments,
    registry: OrderClientIdVenueRegistry,
    events: EventSender,
    reconcile_trigger: Option<std::sync::mpsc::Sender<()>>,
) -> WebsocketSessionUserHandle {
    let order_sub = subscribe_frame("orderUpdates", &master_address);
    let fills_sub = subscribe_frame("userFills", &master_address);
    // First-vs-replay flag for the userFills snapshot. Owned by the decoder, which outlives every
    // session, so it survives reconnects — a replay snapshot must be RECOGNIZED (it carries the
    // socket-down gap fills, which are emitted; only untagged fills are dropped from it). It is NOT a
    // drop gate; see the module doc.
    let snapshot_seen = AtomicBool::new(false);
    // The first-snapshot history floor (the restart law), sampled ONCE, here, at the pump's spawn and
    // before its thread exists, and NEVER per session: `open_websocket` returns `Transport` for a failed
    // connect and for a failed subscribe send alike, so the first SUCCESSFUL connect can be arbitrarily
    // late (backoff doubles to the lane's 30 s ceiling) while `crate::execution`'s `run` is already placing
    // real orders — a floor sampled at that connect would sit AFTER those fills, and on this venue they
    // exist in the first snapshot and NOWHERE else.
    let spawn_ms = vike_model::now_ms();
    // The pump arms the nudge at a re-open; the decoder sends it on that session's snapshot.
    let nudge = Arc::new(ReconnectNudge::new(reconcile_trigger));
    let reopened = Arc::clone(&nudge);
    let lane = venue_timings::HYPERLIQUID.exec();
    spawn_user(WebsocketSessionUserSpec {
        venue: VENUE,
        lane: VENUE,
        // One stream of the whole account: the thread is `hyperliquid-userdata`.
        symbol: String::new(),
        events,
        // `OrderSubmitted` goes out before the place, so no fill can precede it (no fence).
        fence: None,
        open: move |_stop: &AtomicBool| open_websocket(&websocket_url, &order_sub, &fills_sub),
        decode: move |frame: &Value| {
            let universe = instruments.current();
            let events = map_frame_to_events(
                frame,
                universe.symbology(),
                &registry,
                &snapshot_seen,
                spawn_ms,
            );
            nudge.decoded(frame);
            events
        },
        keepalive: Some((
            lane.keepalive_every(),
            Box::new(|ws: &mut WebsocketSessionUserStreamTungstenite| {
                let _ = ws.send_text(PING_FRAME);
            }),
        )),
        idle_threshold: Some(lane.idle_threshold()),
        backoff: ReconnectBackoff::from_row(&lane.reconnect()),
        on_reconnect: move || reopened.reopened(),
    })
}

#[path = "user_data_tests.rs"]
#[cfg(test)]
mod user_data_tests;
