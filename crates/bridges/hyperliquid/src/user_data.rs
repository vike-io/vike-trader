//! Hyperliquid private-WS pump — `orderUpdates` (FSM lane) + `userFills` (Account lane).
//!
//! Unlike the Bybit/OKX private WS, HL's user streams are **keyless**: there is no auth handshake —
//! a subscription is scoped to a wallet ADDRESS (`{"method":"subscribe","subscription":{"type":…,
//! "user":<MASTER address>}}`). So `open_ws` just connects (via [`vike_bridge_core::ws`]) and sends
//! the two subscribe frames; the shared [`run_user_data_forever_with_idle`] reliability loop then drives
//! recv → [`map_frame_to_events`] → emit with reconnect/backoff (each reconnect re-runs `open_ws`,
//! replaying the subscriptions). Keepalive is `{"method":"ping"}` every ≤30s (research §9 — the server
//! drops a connection idle >60s); it self-skips a non-JSON `pong`.
//!
//! ## Remapping (why the pump needs the registry + symbology)
//! The pure [`crate::event_mapper`] carries the venue `cloid` (`0x…`) and `coin` verbatim; the pump
//! remaps them onto the framework identifiers the core folds on:
//! - **cloid → framework coid** via the shared [`CloidRegistry`] exec populated at submit (HL's
//!   `cloid = keccak(coid)` is one-way, so this table is the only inverse). An unknown cloid is left
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
//! with no trace, recoverable only by `VIKE_RECONCILE`, which is off by default.
//!
//! A replay snapshot is therefore **emitted in full**. Re-folding is not a hazard, because dedup
//! belongs one layer down and always has: `ExecutionEngine::on_event` drops any `Event::Fill` whose
//! `trade_id` is already in `seen_trade_ids` — a set that is only ever inserted into (never pruned,
//! never evicted) for the life of the process, and re-seeded from the journal/reconcile on restart.
//! HL's `tid` is that key, unique per fill. This is the SAME contract alpaca's `since_id` boundary
//! re-delivery and bybit's `execution.fast`/`execution` twin lean on — the bridge deliberately
//! re-pushes a known-duplicate fill and lets the engine collapse it.
//!
//! The `snapshot_seen` latch survives, demoted from a wholesale drop gate to a **first-vs-replay
//! flag**: it tags the `info!` that distinguishes a gap-repair snapshot from a first connect, and it
//! selects the ONE frame the history floor below applies to.
//!
//! ## The FIRST snapshot is account HISTORY, and it gets a floor (the restart law)
//! The paragraph above is about the SECOND and later snapshots. The first one is a different object:
//! HL replays an arbitrary suffix of ACCOUNT history on subscribe, so on a fresh process it is
//! mostly (or entirely) activity from BEFORE this process existed — a previous session's fills, a
//! manual trade in HL's own UI, another bot on the same address. `vike_mount::make_engine` builds
//! ONE `Account::new(1.0, venue, .., BalanceMode::Delta)` with `realized_pnl`/`balance`/`fees_paid`
//! at `0.0` and an EMPTY fill-dedup ledger (`vike_exec::Account`'s `seen_fill_ids`), and
//! `vike_exec::Account`'s `seed_seen_fill_ids` doc says seeding is the CALLER's job and no shipped
//! binary does it (`git grep seed_seen_fill_ids` finds no caller outside vike-exec) — so those rows
//! fold into zero:
//! `balance -= commission` and `fees_paid += commission` fire per admitted row, monotone and
//! uncompensated, and a snapshot that starts mid-round-trip opens a phantom position of arbitrary
//! sign. This is the SAME hazard, on the same venue's money lane, that
//! `vike_bridge_core::exec_actor::run_loop`'s history floor exists for — measured on the CI box as an
//! equity step equal to prior sessions' costs. HL just reaches that lane through a snapshot instead
//! of through a `resync` closure, and reaches neither `run_loop` nor `run_resync_supervisor` at all.
//!
//! ⚠ **The floor is a CONJUNCTION, and the clock half ALONE would be a fill-loss bug.** A row is
//! dropped from the first snapshot only when BOTH hold:
//!   1. `CloidRegistry::resolve` returns `None` — this process did not mint the order; **and**
//!   2. `vike_bridge_core::exec_actor::is_pre_spawn` — a POSITIVE `time` strictly below the floor.
//!
//! A time-only floor is refuted by this pump's own reliability loop: the first SUCCESSFUL connect is
//! unsynchronised with anything. `run_user_data_forever_with_idle`'s `OpenOutcome::Transport` arm
//! sleeps and retries with a backoff doubling to `MAX_BACKOFF`, and `open_ws` returns `Transport`
//! for a failed connect AND for a failed subscribe send — while `crate::exec`'s `run` is already
//! draining `Submit` commands and placing real orders over signed REST. So "nothing this process
//! placed can have filled yet" is FALSE, and the fill of such an order exists in the first snapshot
//! and NOWHERE ELSE (no per-fill catch-up, no `since`/watermark re-request — the same fact that
//! makes a reconnect snapshot admissible above). Clock skew is no defence either: HL's nonce window
//! is `(T − 2 days, T + 1 day)` (`crate::signing::hash`'s `NonceManager`), so skew here is not
//! bounded by a `recvWindow` the way the two in-crate lanes' is.
//!
//! Condition 1 is the correction, and it is exact rather than heuristic: `CloidRegistry::register` is
//! called at submit, `cloid = keccak(coid)` with a `uuid4().hex[:8]` session prefix, and that map is
//! never pruned for the life of the process — so a resolving cloid PROVES the order is ours and the
//! row is admitted whatever the clock says. Only **foreign AND pre-spawn** is dropped: precisely
//! "someone else's, from before we existed". Everything else about the lane is unchanged — a replay
//! snapshot, an incremental frame, and an unstamped (`time == 0`) row are never floored, and
//! `spawn_ms == 0` is no floor at all.
//!
//! The one shape the engine's dedup provably cannot cover — a fill carrying no `tid`, which would
//! re-fold on every reconnect and double-count forever — is no longer this module's problem: the
//! [`vike_model::events::TradeId`] newtype cannot hold `""`, so `crate::event_mapper::map_user_fills`
//! refuses such a row outright (counted and `warn!`ed there). That drop is now unconditional rather
//! than replay-only, which is strictly safer: `seen_trade_ids` cannot hold an id that does not
//! exist, so admitting one on first connect only postponed the double-count to the next reconnect.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::exec_actor::is_pre_spawn;
use vike_bridge_core::user_data::{run_user_data_forever_with_idle, OpenOutcome, UserDataFeed};
use vike_bridge_core::ws::{configure_ws_stream, TungsteniteStream};
use vike_exec::EventSender;
use vike_model::events::{Event, FillEvent};

use crate::consts::{VENUE, WS_PING_SECS};
use crate::event_mapper::{map_order_updates, map_user_fills};
use crate::exec::CloidRegistry;
use crate::instruments::HyperliquidInstruments;
use crate::symbology::Symbology;

/// Read poll timeout — the recv loop wakes this often to check `stop` (matches the Bybit/OKX pumps).
const POLL: Duration = Duration::from_secs(1);
/// App-level keepalive cadence (≤30s; the server drops an idle connection after 60s — research §9).
const PING_EVERY: Duration = Duration::from_secs(WS_PING_SECS);
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind
/// an open connection, so end the session and let the reconnect + audit-A3 resync repair it.
/// 4x [`PING_EVERY`] — HL answers each `{"method":"ping"}` with a pong, so a healthy socket
/// delivers an inbound frame every ~30s even on a totally idle account. Keyed off that
/// server-answer cadence, NOT data cadence: an account with no orders is legitimately silent.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(WS_PING_SECS * 4);
/// Reconnect backoff ceiling.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// The HL app-level keepalive frame (`{"channel":"pong"}` comes back; the pump tolerates it).
const PING_FRAME: &str = r#"{"method":"ping"}"#;

/// Rows the first-`userFills`-snapshot history floor has dropped since process start.
///
/// A drop on the money lane is never silent here — this is the counter half of the local convention
/// `vike_exec::Account`'s `duplicate_fills_refused` / `colliding_fills_refused` set (a `pub` count
/// beside a `tracing` line), in the one shape a free function can hold it: the process-global
/// `static` + accessor pair `vike_polymarket::exec`'s `rate_gate_would_block_count` already uses.
static FIRST_SNAPSHOT_FILLS_DROPPED: AtomicU64 = AtomicU64::new(0);

/// How many rows the first-`userFills`-snapshot floor has dropped since process start — FOREIGN
/// (no cloid this process minted) AND stamped before the pump's spawn. Bounded by construction: the
/// floor is consulted for exactly ONE frame per pump thread, so this can only ever advance once.
///
/// Nonzero is NORMAL on a restart of an account with history and is NOT a fault. What it means is
/// that the platform's books deliberately do not contain that pre-start activity; adopting it is
/// `VIKE_RECONCILE`'s job (off by default) or `vike_exec::Account`'s `seed_seen_fill_ids`'.
pub fn first_snapshot_fills_dropped() -> u64 {
    FIRST_SNAPSHOT_FILLS_DROPPED.load(Ordering::Relaxed)
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
/// subscribe-send failure returns [`OpenOutcome::Transport`] so the reliability loop reconnects.
pub fn open_ws(ws_url: &str, order_sub: &str, fills_sub: &str) -> OpenOutcome<TungsteniteStream> {
    let (socket, _resp) = match tungstenite::connect(ws_url) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_ws_stream(&socket, POLL);
    let mut stream = TungsteniteStream(socket);
    for sub in [order_sub, fills_sub] {
        if stream.send_text(sub).is_err() {
            return OpenOutcome::Transport("subscribe send failed".to_string());
        }
    }
    OpenOutcome::Ready(stream)
}

/// Map ONE inbound WS frame to canonical events, remapping cloid→coid and coin→symbol. Pure over the
/// borrowed frame + the shared symbology/registry + the `snapshot_seen` flag + the history floor —
/// the offline-testable core of the pump (the reliability loop just recv→this→emit). `pub` so the
/// crate's offline reliability test can drive it through the REAL `run_user_data_forever` loop across
/// a scripted reconnect (the aster/binance `*_userdata.rs` shape), rather than re-implementing the
/// decode.
///
/// `spawn_ms` is the PUMP THREAD's spawn instant in epoch ms and applies to the FIRST `userFills`
/// snapshot only — see the module doc's restart-law section for the whole argument, including why a
/// clock-only floor would lose a fill. `0` means **no floor** (an unfloored test or probe), the same
/// hatch `crates/bridges/hyperliquid/src/funding.rs`'s `HlFundingPoller` gives its `floor_ms`.
///
/// ⚠ It must be sampled by the CALLER, once, OUTSIDE the reliability loop
/// ([`spawn_hyperliquid_user_data`] does). A floor re-sampled per session would sit AFTER fills that
/// executed while the first connect was still failing its backoff — and on this venue those fills
/// exist in the first snapshot and nowhere else.
pub fn map_frame_to_events(
    frame: &Value,
    symbology: &Symbology,
    registry: &CloidRegistry,
    snapshot_seen: &AtomicBool,
    spawn_ms: i64,
) -> Vec<Event> {
    match frame.get("channel").and_then(|c| c.as_str()) {
        Some("orderUpdates") => {
            // Drop a stale `canceled` for an oid a native modify just retired (same cloid → a NEW
            // oid); the fast path (nothing retired) borrows the frame untouched.
            let frame = suppress_retired_cancels(frame, registry);
            let mut evs = map_order_updates(&frame, VENUE);
            for ev in &mut evs {
                remap_coid(ev, registry);
                remap_symbol(ev, symbology);
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
            let fills = uf.fills;
            if replay {
                // The untagged-fill drop that used to live HERE now happens one layer up, in
                // `map_user_fills`, and unconditionally: `FillEvent.trade_id` is a
                // `vike_model::events::TradeId`, which cannot hold `""`, so a `tid`-less row can no
                // longer be built into a fill at all — first connect included. That is stricter
                // than the replay-only drop this block performed and strictly safer: the engine's
                // `seen_trade_ids` cannot hold an id that does not exist, so a first-connect
                // admission only deferred the same double-count to the first reconnect. The count
                // and the `warn!` moved with it (the mapper knows the field name).
                tracing::info!(
                    admitted = fills.len(),
                    "userFills reconnect snapshot admitted (gap repair); already-folded fills \
                     dedup downstream on `tid`"
                );
            }
            let mut dropped = 0usize;
            let evs: Vec<Event> = fills
                .into_iter()
                .filter_map(|mut f| {
                    // `Some` PROVES this process minted the order: exec registers `keccak(coid)` at
                    // submit under a per-process `uuid4` prefix, and that map is never pruned for the
                    // life of the process. So a resolving cloid admits the row whatever the clock
                    // says — the half a time-only floor gets wrong.
                    let ours = match registry.resolve(&f.client_order_id) {
                        Some(coid) => {
                            f.client_order_id = coid;
                            true
                        }
                        None => false,
                    };
                    if let Some(sym) = symbology.symbol_for_coin(f.symbol.as_str()) {
                        f.symbol = sym.into();
                    }
                    let ev = Event::Fill(f);
                    // FOREIGN **and** pre-spawn — "someone else's, from before we existed". Either
                    // condition alone admits. `is_pre_spawn` is the SHARED predicate (a second
                    // spelling of "older than my process" is this repo's recurring defect) and it
                    // already gives `spawn_ms == 0` ⇒ no floor and `time == 0` ⇒ admit, so neither
                    // needs a rival check here.
                    if first_snapshot && !ours && is_pre_spawn(&ev, spawn_ms) {
                        dropped += 1;
                        return None;
                    }
                    Some(ev)
                })
                .collect();
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
                     state is VIKE_RECONCILE's job (off by default), not this lane's. A fill of one \
                     of OUR orders is admitted whatever its stamp."
                );
            }
            evs
        }
        // `pong` / `subscriptionResponse` / an unknown channel carry nothing to fold.
        _ => Vec::new(),
    }
}

/// Is this `orderUpdates` entry a cancellation? Matches the same statuses [`crate::event_mapper`]
/// treats as a cancel (`canceled` / any `*Canceled` / `scheduledCancel`).
fn is_cancel_status(upd: &Value) -> bool {
    upd.get("status")
        .and_then(|s| s.as_str())
        .map(|s| s == "canceled" || s.ends_with("Canceled") || s == "scheduledCancel")
        .unwrap_or(false)
}

/// Suppress a stale `canceled` for an oid a native modify retired (see [`CloidRegistry::retire_oid`]):
/// a cancel-replace keeps the cloid on a NEW oid, so a `canceled` streamed for the OLD oid would map
/// (via the shared cloid) to an [`Event::OrderCanceled`] that wrongly terminates the live re-placed
/// order. Scans `data` for cancel entries whose `order.oid` is retired; if any, returns a CLONE with
/// them removed, else borrows the frame unchanged (the steady-state fast path — no clone, no lock
/// contention beyond the empty-set check).
fn suppress_retired_cancels<'a>(frame: &'a Value, registry: &CloidRegistry) -> Cow<'a, Value> {
    let Some(data) = frame.get("data").and_then(|d| d.as_array()) else {
        return Cow::Borrowed(frame);
    };
    let mut drop_idx: Vec<usize> = Vec::new();
    for (i, upd) in data.iter().enumerate() {
        if is_cancel_status(upd) {
            if let Some(oid) = upd.get("order").and_then(|o| o.get("oid")).and_then(Value::as_u64) {
                if registry.take_retired(oid) {
                    drop_idx.push(i);
                }
            }
        }
    }
    if drop_idx.is_empty() {
        return Cow::Borrowed(frame);
    }
    let kept: Vec<Value> = data
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop_idx.contains(i))
        .map(|(_, v)| v.clone())
        .collect();
    let mut owned = frame.clone();
    owned["data"] = Value::Array(kept);
    Cow::Owned(owned)
}

/// Remap a fill-carrying event's `symbol` from the venue `coin` to the unified symbol. Shared with
/// exec's `/exchange` response path so both lanes tag positions by the same symbol.
pub(crate) fn remap_symbol(ev: &mut Event, symbology: &Symbology) {
    if let Some(fill) = fill_mut(ev) {
        if let Some(sym) = symbology.symbol_for_coin(fill.symbol.as_str()) {
            fill.symbol = sym.into();
        }
    }
}

/// Remap an order event's `client_order_id` from the venue cloid to the framework coid (leave it
/// unchanged when the cloid isn't ours — the core then drops it as an unknown order).
fn remap_coid(ev: &mut Event, registry: &CloidRegistry) {
    if let Some(coid) = coid_mut(ev) {
        if let Some(mapped) = registry.resolve(coid) {
            *coid = mapped;
        }
    }
}

/// The mutable `client_order_id` of any coid-carrying event (`None` for position/account/funding).
fn coid_mut(ev: &mut Event) -> Option<&mut String> {
    Some(match ev {
        Event::OrderSubmitted(e) => &mut e.client_order_id,
        Event::OrderAccepted(e) => &mut e.client_order_id,
        Event::OrderRejected(e) => &mut e.client_order_id,
        Event::OrderDenied(e) => &mut e.client_order_id,
        Event::OrderTriggered(e) => &mut e.client_order_id,
        Event::OrderPartiallyFilled(e) => &mut e.client_order_id,
        Event::OrderFilled(e) => &mut e.client_order_id,
        Event::OrderCanceled(e) => &mut e.client_order_id,
        Event::OrderExpired(e) => &mut e.client_order_id,
        Event::OrderLiquidated(e) => &mut e.client_order_id,
        Event::OrderModified(e) => &mut e.client_order_id,
        Event::OrderCancelRejected(e) => &mut e.client_order_id,
        Event::OrderModifyRejected(e) => &mut e.client_order_id,
        Event::Fill(f) => &mut f.client_order_id,
        _ => return None,
    })
}

/// The mutable [`FillEvent`] a fill-carrying event wraps (`None` otherwise).
fn fill_mut(ev: &mut Event) -> Option<&mut FillEvent> {
    match ev {
        Event::Fill(f) => Some(f),
        Event::OrderFilled(e) => Some(&mut e.fill),
        Event::OrderPartiallyFilled(e) => Some(&mut e.fill),
        _ => None,
    }
}

/// Spawn the persistent Hyperliquid private-WS pump feeding the vt-core ingest.
///
/// `master_address` is the MASTER account address the subscriptions are scoped to (agent-wallet
/// queries return empty — research §8). `instruments`/`registry` are shared READ-ONLY with the exec
/// thread for the coin→symbol / cloid→coid remaps. Deterministic teardown via the returned
/// [`UserDataFeed`] (`shutdown()` raises stop + joins).
pub fn spawn_hyperliquid_user_data(
    ws_url: String,
    master_address: String,
    instruments: Arc<HyperliquidInstruments>,
    registry: CloidRegistry,
    events: EventSender,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
) -> UserDataFeed {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("hyperliquid-userdata".to_string())
        .spawn(move || {
            let _span = tracing::info_span!("user_data_pump", venue = VENUE).entered();
            let order_sub = subscribe_frame("orderUpdates", &master_address);
            let fills_sub = subscribe_frame("userFills", &master_address);
            // First-vs-replay flag for the userFills snapshot. Deliberately created HERE, outside
            // the reliability loop, so it survives reconnects — a replay snapshot must be
            // RECOGNIZED (it carries the socket-down gap fills, which are emitted; only untagged
            // fills are dropped from it). It is NOT a drop gate; see the module doc.
            let snapshot_seen = AtomicBool::new(false);
            // The first-snapshot history floor (the restart law), sampled HERE for the same reason
            // and at the same place as the flag above: this call IS the pump's spawn, and the floor
            // must NOT be re-sampled per session. `open_ws` returns `Transport` for a failed connect
            // and for a failed subscribe send alike, so the first SUCCESSFUL connect can be
            // arbitrarily late (backoff doubles to `MAX_BACKOFF`) while `crate::exec`'s `run` is
            // already placing real orders — a floor sampled at that connect would sit AFTER those
            // fills, and on this venue they exist in the first snapshot and NOWHERE else.
            let spawn_ms = vike_model::clock::now_ms();
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text(PING_FRAME);
            };
            run_user_data_forever_with_idle(
                || open_ws(&ws_url, &order_sub, &fills_sub),
                |frame| {
                    map_frame_to_events(
                        frame,
                        instruments.symbology(),
                        &registry,
                        &snapshot_seen,
                        spawn_ms,
                    )
                },
                |event| events.blocking_send(event).is_ok(),
                &stop_thread,
                POLL,
                MAX_BACKOFF,
                Some((PING_EVERY, &mut ping)),
                || {
                    // On (re)connect, poke the reconcile driver (if one is mounted) to run a pass —
                    // it re-fetches order/fill/position truth that may have drifted while the socket
                    // was down. No-op when reconcile is off (`recon_trigger` is `None`), so the
                    // default path is unchanged; a dropped receiver (driver gone) is ignored.
                    if let Some(t) = &recon_trigger {
                        let _ = t.send(());
                    }
                },
                Some(IDLE_THRESHOLD),
            )
        })
        .expect("spawn hyperliquid user-data thread");
    UserDataFeed { stop, handle }
}

#[cfg(test)]
mod tests {
    //! Scripted-stream mapping — canned frames through [`map_frame_to_events`] (NO network) prove the
    //! cloid→coid + coin→symbol remaps, the userFills snapshot latch (first-vs-replay) and the
    //! first-snapshot history floor's four-way matrix.
    use super::*;
    use serde_json::json;

    /// `spawn_ms == 0` — the no-floor hatch. Every test that predates the floor passes this, so it
    /// exercises byte-identical behaviour; the floor's OWN tests pass [`FLOOR`] instead. (A gate every
    /// caller opts out of is the shape this repo treats as a lie, hence the block at the bottom.)
    const NO_FLOOR: i64 = 0;

    /// A real, armed floor. A fixed small epoch-ms value rather than `now_ms()`: the matrix needs a
    /// row on EACH side of it, and a wall-clock floor cannot have a row stamped after it without
    /// sleeping. `1_000_000` is unambiguously positive, so `is_pre_spawn`'s `ts > 0` clause is never
    /// what decides these cases.
    const FLOOR: i64 = 1_000_000;

    fn symbology() -> Symbology {
        let meta = json!({"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]});
        let spot = json!({
            "tokens":[
                {"name":"USDC","szDecimals":8,"index":0},
                {"name":"HYPE","szDecimals":2,"index":150}
            ],
            "universe":[{"name":"@107","tokens":[150,0],"index":107}]
        });
        Symbology::from_meta(&meta, &spot)
    }

    #[test]
    fn order_update_remaps_cloid_to_the_framework_coid() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let cloid = registry.register("vike-1"); // exec would have done this at submit
        let seen = AtomicBool::new(false);

        let frame = json!({"channel":"orderUpdates","data":[{
            "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.0","origSz":"0.1",
                     "oid":42,"cloid":cloid,"timestamp":1},
            "status":"open","statusTimestamp":2
        }]});
        let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            Event::OrderAccepted(a) => {
                assert_eq!(a.client_order_id, "vike-1", "cloid remapped to the framework coid");
                assert_eq!(a.venue_order_id.as_deref(), Some("42"));
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
    }

    #[test]
    fn stale_cancel_of_a_modify_retired_oid_is_suppressed() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let cloid = registry.register("vike-mod");
        let seen = AtomicBool::new(false);

        // A native modify retired oid 42 (exec records this on the shared registry).
        registry.retire_oid(42);

        // HL streams a stale `canceled` for the RETIRED oid 42 (same cloid → the live re-placed
        // order): it must be DROPPED, not folded into an OrderCanceled that terminates the order.
        let stale = json!({"channel":"orderUpdates","data":[{
            "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":42,"cloid":cloid,"timestamp":1},
            "status":"canceled","statusTimestamp":2
        }]});
        assert!(
            map_frame_to_events(&stale, &symbology, &registry, &seen, NO_FLOOR).is_empty(),
            "a canceled for a modify-retired oid is suppressed"
        );

        // A cancel for a DIFFERENT, live oid still folds normally (suppression is one-shot + by oid).
        let real = json!({"channel":"orderUpdates","data":[{
            "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":99,"cloid":cloid,"timestamp":1},
            "status":"canceled","statusTimestamp":2
        }]});
        match &map_frame_to_events(&real, &symbology, &registry, &seen, NO_FLOOR)[..] {
            [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "vike-mod"),
            other => panic!("expected one OrderCanceled, got {other:?}"),
        }
    }

    #[test]
    fn unknown_cloid_is_left_as_is() {
        let symbology = symbology();
        let registry = CloidRegistry::new(); // nothing registered
        let seen = AtomicBool::new(false);
        let frame = json!({"channel":"orderUpdates","data":[{
            "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":7,"cloid":"0xdeadbeef","timestamp":1},
            "status":"open"
        }]});
        match &map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR)[0] {
            Event::OrderAccepted(a) => {
                assert_eq!(a.client_order_id, "0xdeadbeef", "foreign cloid untouched")
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
    }

    #[test]
    fn user_fill_remaps_spot_coin_to_symbol_and_cloid_to_coid() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let cloid = registry.register("vike-9");
        let seen = AtomicBool::new(false);

        // spot fill on the "@107" coin (HYPE/USDC), isSnapshot=false (an incremental fill).
        let frame = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[{
            "coin":"@107","px":"1.5","sz":"2.0","side":"B","oid":1,"cloid":cloid,
            "tid":900,"fee":"0.1","feeToken":"USDC","crossed":true,"time":5
        }]}});
        let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
        match &evs[0] {
            Event::Fill(f) => {
                assert_eq!(f.symbol, "HYPE/USDC", "spot coin @107 remapped to the unified symbol");
                assert_eq!(f.client_order_id, "vike-9", "cloid remapped to the framework coid");
                assert_eq!(f.trade_id, "900"); // tid keys the Account dedup
                assert_eq!(f.last_qty, 2.0);
            }
            other => panic!("expected Fill, got {other:?}"),
        }
    }

    /// One `userFills` snapshot frame carrying `fills` verbatim.
    fn snapshot_frame(fills: Value) -> Value {
        json!({"channel":"userFills","data":{"isSnapshot":true,"fills":fills}})
    }

    /// One BTC perp fill row; `tid` is `Value::Null` to model an untagged fill.
    fn fill_row(tid: Value, sz: &str, time: i64) -> Value {
        json!({"coin":"BTC","px":"50000","sz":sz,"side":"B","oid":1,"tid":tid,
               "fee":"0.5","feeToken":"USDC","crossed":true,"time":time})
    }

    /// THE REGRESSION PIN. HL resends `isSnapshot` on every reconnect, and the fills that executed
    /// while the socket was down exist ONLY there. The old `snapshot_seen` latch returned
    /// `Vec::new()` for every snapshot after the first, so those fills silently vanished. Both
    /// snapshots must now yield their fills; the engine's `seen_trade_ids` dedups the overlap (the
    /// end-to-end proof of that is `tests/hyperliquid_userdata_reconnect.rs`).
    #[test]
    fn reconnect_snapshot_still_yields_its_fills() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(false);

        // Session 1's snapshot: one fill, tid 1.
        let first = map_frame_to_events(
            &snapshot_frame(json!([fill_row(json!(1), "0.1", 1)])),
            &symbology,
            &registry,
            &seen,
            NO_FLOOR,
        );
        assert_eq!(first.len(), 1, "the FIRST snapshot is folded (unchanged)");

        // Session 2 (post-reconnect): HL replays tid 1 AND carries tid 2, which executed while the
        // socket was down. Both are emitted — dropping the frame would have lost tid 2 outright.
        let second = map_frame_to_events(
            &snapshot_frame(json!([fill_row(json!(1), "0.1", 1), fill_row(json!(2), "0.3", 9)])),
            &symbology,
            &registry,
            &seen,
            NO_FLOOR,
        );
        let tids: Vec<String> = second
            .iter()
            .map(|e| match e {
                Event::Fill(f) => f.trade_id.to_string(),
                other => panic!("expected Fill, got {other:?}"),
            })
            .collect();
        assert_eq!(
            tids,
            ["1", "2"],
            "the reconnect snapshot is admitted in full, gap fill included"
        );
    }

    /// An untagged fill is dropped on EVERY frame — first connect, replay snapshot and incremental
    /// alike — because `FillEvent.trade_id` is a `TradeId` and cannot hold `""`, so
    /// `map_user_fills` refuses the row before this module sees it.
    ///
    /// ⚠ This is a deliberate TIGHTENING of the previous behaviour, which admitted an untagged fill
    /// on first connect and dropped it only from a replay. That split bought nothing: the engine's
    /// dedup key would be absent either way, so the first-connect admission merely deferred the
    /// double-count to the first reconnect — and the fill it "saved" was the un-dedupable one. A
    /// tagged fill in the same frame is unaffected, which is what keeps the drop per-row.
    #[test]
    fn an_untagged_fill_is_dropped_on_every_frame_not_just_a_replay() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(false);

        let first = map_frame_to_events(
            &snapshot_frame(json!([fill_row(Value::Null, "0.1", 1)])),
            &symbology,
            &registry,
            &seen,
            NO_FLOOR,
        );
        assert!(
            first.is_empty(),
            "even a FIRST-connect untagged fill is refused — it could never be deduped"
        );

        let second = map_frame_to_events(
            &snapshot_frame(json!([fill_row(Value::Null, "0.1", 1), fill_row(json!(7), "0.2", 5)])),
            &symbology,
            &registry,
            &seen,
            NO_FLOOR,
        );
        match &second[..] {
            [Event::Fill(f)] => assert_eq!(f.trade_id, "7", "only the DEDUPABLE fill survives"),
            other => panic!("expected exactly the tagged fill, got {other:?}"),
        }
    }

    /// An INCREMENTAL (`isSnapshot:false`) frame is never touched by the snapshot flag, before or
    /// after a snapshot has been seen: its TAGGED fills always pass through. Its untagged ones do
    /// not — that refusal is the mapper's and is frame-kind blind (see the test above).
    #[test]
    fn incremental_fills_are_untouched_by_the_snapshot_flag() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(true); // a snapshot has already been seen
        let frame = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(3), "0.1", 4), fill_row(Value::Null, "0.2", 5)]}});
        let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
        match &evs[..] {
            [Event::Fill(f)] => assert_eq!(
                f.trade_id, "3",
                "the tagged incremental fill passes through; the untagged one is refused"
            ),
            other => panic!("expected exactly the tagged fill, got {other:?}"),
        }
    }

    #[test]
    fn non_data_frames_yield_nothing() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(false);
        for frame in
            [json!({"channel":"pong"}), json!({"channel":"subscriptionResponse","data":{}})]
        {
            assert!(map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR).is_empty());
        }
    }

    // ---- the FIRST-snapshot history floor -------------------------------------------------------
    //
    // Every test above passes [`NO_FLOOR`], so none of them enters the branch below — which is
    // exactly why these exist. The floor is a CONJUNCTION (FOREIGN **and** pre-spawn), so pinning it
    // takes the four-way matrix, and each case gets its OWN test with ONE row in the frame: a single
    // row means a missing half of the guard SUCCEEDS at producing the wrong answer instead of being
    // masked by a sibling row or hidden behind a short-circuiting earlier assert. The mixed frame at
    // the end then proves the decision is per-ROW.
    //
    // `fill_row` carries no `cloid`, so `map_one_fill`'s `order_coid` falls back to `oid` and the row
    // is FOREIGN by construction against an empty registry. [`fill_row_cloid`] is its `ours` twin.

    /// One BTC perp fill row carrying `cloid` — what a fill of an order THIS process submitted looks
    /// like on the wire (exec stamps `keccak(coid)` on every order it places).
    fn fill_row_cloid(tid: Value, sz: &str, time: i64, cloid: &str) -> Value {
        json!({"coin":"BTC","px":"50000","sz":sz,"side":"B","oid":1,"tid":tid,"cloid":cloid,
               "fee":"0.5","feeToken":"USDC","crossed":true,"time":time})
    }

    /// Every emitted fill's `tid`, in frame order (panics on any non-fill event).
    fn tids(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(f) => f.trade_id.to_string(),
                other => panic!("userFills must emit only bare fills, got {other:?}"),
            })
            .collect()
    }

    /// (foreign, pre-spawn) ⇒ **DROPPED**. The one combination that drops: someone else's activity
    /// from before this process existed. Folding it would subtract a previous session's commission
    /// from a zero-based `balance` and open a phantom position of whatever sign the history suffix
    /// happens to start on.
    #[test]
    fn a_foreign_pre_spawn_fill_is_dropped_from_the_first_snapshot_and_counted() {
        let symbology = symbology();
        let registry = CloidRegistry::new(); // nothing registered ⇒ every row is FOREIGN
        let seen = AtomicBool::new(false); // ⇒ this frame IS the first snapshot
        let before = first_snapshot_fills_dropped();

        let evs = map_frame_to_events(
            &snapshot_frame(json!([fill_row(json!(11), "0.1", FLOOR - 1)])),
            &symbology,
            &registry,
            &seen,
            FLOOR,
        );

        assert!(evs.is_empty(), "foreign AND pre-spawn is the combination that drops: {evs:?}");
        // A STRICT advance rather than `== before + 1`: the counter is process-global, so a strict
        // inequality holds whatever else a sibling test in this binary is doing concurrently, while
        // still reddening if the increment is deleted (a drop on the money lane is never silent).
        assert!(
            first_snapshot_fills_dropped() > before,
            "the drop must be COUNTED as well as logged"
        );
    }

    /// (foreign, post-spawn) ⇒ admitted. **This is the row that reddens if the timestamp half of the
    /// conjunction is deleted.** A fill on this address that executed while we were running is live
    /// account activity, not history — the floor's subject is WHEN, and authorship alone must never
    /// drop anything (HL's `userFills` is account-wide and the engine gates on SYMBOL, not cloid;
    /// that was true before this floor existed and is deliberately unchanged).
    #[test]
    fn a_foreign_post_spawn_fill_is_admitted_from_the_first_snapshot() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(false);

        let evs = map_frame_to_events(
            &snapshot_frame(json!([fill_row(json!(12), "0.1", FLOOR + 1)])),
            &symbology,
            &registry,
            &seen,
            FLOOR,
        );
        assert_eq!(
            tids(&evs),
            ["12"],
            "a foreign fill AFTER the floor is live activity, not history"
        );
    }

    /// (ours, pre-spawn) ⇒ admitted. **This is the row that reddens if the cloid half of the
    /// conjunction is deleted, and it is the case a clock-only floor gets WRONG.**
    ///
    /// It is reachable: the pump's first SUCCESSFUL connect is unsynchronised with everything —
    /// `open_ws` returns `Transport` for a failed connect AND a failed subscribe send, the backoff
    /// doubles to [`MAX_BACKOFF`], and `crate::exec`'s `run` is meanwhile draining `Submit` and
    /// placing real orders over signed REST. Venue-vs-local skew widens the same window from the
    /// other side, and on this venue skew is NOT bounded by a `recvWindow` (`crate::signing::hash`'s
    /// `NonceManager`: `(T − 2 days, T + 1 day)`). Such a fill exists in this snapshot and NOWHERE
    /// ELSE — HL streams no per-fill catch-up and this pump re-requests no window — so dropping it
    /// would silently lose a fill, the very defect
    /// `tests/hyperliquid_userdata_reconnect.rs`'s
    /// `a_fill_that_executed_while_the_socket_was_down_is_not_lost` exists to prevent.
    #[test]
    fn our_own_pre_spawn_fill_is_admitted_from_the_first_snapshot() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let cloid = registry.register("vike-race"); // exec did this at submit
        let seen = AtomicBool::new(false);

        let evs = map_frame_to_events(
            &snapshot_frame(json!([fill_row_cloid(json!(13), "0.1", FLOOR - 1, &cloid)])),
            &symbology,
            &registry,
            &seen,
            FLOOR,
        );
        assert_eq!(tids(&evs), ["13"], "a resolving cloid PROVES the order is ours — admit it");
        match &evs[0] {
            Event::Fill(f) => assert_eq!(
                f.client_order_id, "vike-race",
                "and it is still remapped to the framework coid, floor or no floor"
            ),
            other => panic!("expected Fill, got {other:?}"),
        }
    }

    /// (ours, post-spawn) ⇒ admitted — the ordinary live case, which neither half of the guard may
    /// touch.
    #[test]
    fn our_own_post_spawn_fill_is_admitted_from_the_first_snapshot() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let cloid = registry.register("vike-live");
        let seen = AtomicBool::new(false);

        let evs = map_frame_to_events(
            &snapshot_frame(json!([fill_row_cloid(json!(14), "0.1", FLOOR + 1, &cloid)])),
            &symbology,
            &registry,
            &seen,
            FLOOR,
        );
        assert_eq!(tids(&evs), ["14"]);
    }

    /// The whole matrix in ONE frame: the floor decides per ROW, so a snapshot mixing a previous
    /// session's history with our own racing fills keeps everything except the history. A per-FRAME
    /// drop (the shape the original `snapshot_seen` latch had) would lose tids 22-24.
    #[test]
    fn the_floor_decides_per_row_not_per_frame() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let cloid = registry.register("vike-mixed");
        let seen = AtomicBool::new(false);

        let evs = map_frame_to_events(
            &snapshot_frame(json!([
                fill_row(json!(21), "0.1", FLOOR - 1), // foreign + pre  → dropped
                fill_row(json!(22), "0.2", FLOOR + 1), // foreign + post → admitted
                fill_row_cloid(json!(23), "0.3", FLOOR - 1, &cloid), // ours + pre     → admitted
                fill_row_cloid(json!(24), "0.4", FLOOR + 1, &cloid), // ours + post    → admitted
            ])),
            &symbology,
            &registry,
            &seen,
            FLOOR,
        );
        assert_eq!(tids(&evs), ["22", "23", "24"], "exactly the history row is dropped");
    }

    /// A REPLAY snapshot is never floored — even a row that is both foreign and pre-spawn. This is
    /// the gap-repair frame: the fills that executed while the socket was down exist ONLY in it, they
    /// can be stamped anywhere relative to a floor sampled at spawn, and "foreign" says nothing about
    /// whether we need them (a fill of an order placed by an EARLIER session of this pump, or an
    /// oid-only liquidation, is foreign). Dedup one layer down (`tid`) is what makes admitting it
    /// safe, and that has not changed.
    #[test]
    fn a_replay_snapshot_is_never_floored() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(true); // a snapshot has ALREADY been seen ⇒ this one is a replay

        let evs = map_frame_to_events(
            &snapshot_frame(json!([fill_row(json!(31), "0.1", FLOOR - 1)])),
            &symbology,
            &registry,
            &seen,
            FLOOR,
        );
        assert_eq!(
            tids(&evs),
            ["31"],
            "the floor applies to the FIRST snapshot only — a replay carries the socket-down gap"
        );
    }

    /// An INCREMENTAL frame is never floored either, and this is not the same statement as the test
    /// above: `replay` is `false` for every `isSnapshot:false` frame (the `&&` short-circuits before
    /// the latch is even read), so dropping the `uf.is_snapshot &&` conjunct would floor EVERY live
    /// fill for the life of the process, forever, not just one frame.
    #[test]
    fn an_incremental_frame_is_never_floored() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(false); // no snapshot seen yet, so `!replay` holds
        let frame = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(41), "0.1", FLOOR - 1)]}});

        let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, FLOOR);
        assert_eq!(tids(&evs), ["41"], "an incremental fill is live, whatever it is stamped");
    }

    /// [`NO_FLOOR`] admits everything — the hatch every pre-floor test above rides, asserted here
    /// rather than assumed, on the exact row an armed floor drops.
    #[test]
    fn spawn_ms_zero_is_no_floor_at_all() {
        let symbology = symbology();
        let registry = CloidRegistry::new();
        let seen = AtomicBool::new(false);

        let evs = map_frame_to_events(
            &snapshot_frame(json!([fill_row(json!(51), "0.1", 1)])),
            &symbology,
            &registry,
            &seen,
            NO_FLOOR,
        );
        assert_eq!(tids(&evs), ["51"], "`spawn_ms == 0` is the unfloored/probe configuration");
    }
}
