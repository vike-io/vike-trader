//! Binance WS-API private-stream session. Port of `exec/binance/user_data.py`:
//! connect → send ONE signed subscribe (never logged — carries apiKey + signature) →
//! loop until the id-matched status==200 ack (bounded by a per-recv poll timeout and an
//! overall handshake deadline, so a half-open handshake can never hang the thread) →
//! hand the socket to the venue-neutral pump, decoding frames with `map_binance_private`
//! and feeding the th-core ingest LOSSLESSLY via `EventSender`.
//!
//! Binance's server sends 20-second protocol PING control frames — answered with Pongs
//! at this layer (no app-level ping, matching Python's `ping=None`).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::websocket::{WebsocketSessionUserStreamTungstenite, send_and_await};
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserOpenOutcome, WebsocketSessionUserResyncHandle,
    WebsocketSessionUserResyncSpec, dial_user_stream, spawn_user_with_resync,
};
use vike_execution::EventSender;
use vike_model::events::Event;

use crate::execution::event_mapper::spot::map_binance_private;
use crate::transport::ws_auth::{build_subscribe_request, match_subscribe_ack};

/// The WS-API session's path under the spot `trade` `ws` `venue_url` row (`wss://ws-api.binance.com` +
/// `/ws-api/v3`; `crate::execution`'s `BinanceUrls::spot_websocket` is the joined address).
pub const PATH_WEBSOCKET_API: &str = "/ws-api/v3";

/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind an
/// open connection, so end the session and let the reconnect + audit-A3 resync repair it.
///
/// The VALUE is `vike_venues::venue_timings::BINANCE_SPOT`'s exec-lane `idle_threshold`, with
/// its reason: 3x the WS-API server-ping interval. There is no app-level ping on this lane — the
/// SERVER pings and tungstenite pongs — so that server cadence is the only liveness an idle account
/// produces. MEASURED (`tests/binance_spot_userdata_soak.rs`, 900s window): 45 inbound frames, every
/// gap 19-20s, worst 20.004s.
///
/// ⚠ **Spot is NOT perp.** The perp lane's server ping is 180s and its threshold 720s
/// (`BINANCE_PERP`'s row, read by `execution/user_data/perp.rs`) — a 9x difference on the same exchange,
/// because spot rides the WS-API host while perp rides a listenKey stream. Neither venue's number
/// may be assumed for the other; both were measured separately for exactly this reason.
pub const IDLE_THRESHOLD: Duration =
    vike_venues::venue_timings::BINANCE_SPOT.exec().idle_threshold();

/// Connect + signed subscribe + ack-await. The socket is closed on ANY handshake exit.
/// `request_id` derives from the clock (unique per attempt, no uuid dependency).
pub fn open_binance_user_data_websocket(
    websocket_url: &str,
    api_key: &str,
    api_secret: &str,
    now_ms: impl Fn() -> i64,
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    let mut socket = match dial_user_stream(websocket_url, CONNECT_10S) {
        Ok(socket) => socket,
        Err(why) => return WebsocketSessionUserOpenOutcome::Transport(why),
    };
    let request_id = format!("vtr{}", now_ms());
    let request = build_subscribe_request(api_key, api_secret, now_ms(), 5000, &request_id);
    // Single-step handshake: the id-matched subscribe ack IS the whole handshake, so on Ok the socket
    // becomes the live stream (bybit/okx run this twice for auth then subscribe). The one signed
    // subscribe+auth frame is WS-gated (spec §A keyed WS gate) and never logged. The bound is the exec
    // lane's `handshake_timeout` in `BINANCE_SPOT`'s row: one deadline for the single subscribe and its ack.
    let deadline =
        Instant::now() + vike_venues::venue_timings::BINANCE_SPOT.exec().handshake_timeout();
    match send_and_await(
        &mut socket,
        Some(gate),
        "subscribe",
        &request.to_string(),
        "subscribe",
        stop,
        deadline,
        "Binance WS subscribe",
        |f| match_subscribe_ack(f, &request_id),
    ) {
        Ok(_) => {
            WebsocketSessionUserOpenOutcome::Ready(WebsocketSessionUserStreamTungstenite(socket))
        }
        Err(outcome) => *outcome,
    }
}

/// Spawn the persistent Binance user-data pump feeding the th-core ingest, with the audit-A3 resync
/// supervisor: on every WS re-open the pump bumps a generation counter, and the supervisor (after a
/// settle delay) replays recent order history via `resync_fetch` so a terminal that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST history
/// source (typically `get_all_orders` + `get_my_trades` →
/// [`map_binance_history`](crate::execution::resync::map_binance_history)) on a SEPARATE `VenueRestBinanceSpot` from the
/// order-submit client; a caller with no replay hands `Vec::new`. Both threads tear down on `shutdown`;
/// the supervisor also self-exits if the pump dies or the core is gone. (Exercised by live smokes.)
///
/// The pump is `vike_bridge_core::websocket_session::user`'s `spawn_user_with_resync` (one spawn for every private WS
/// stream): this function supplies only the venue's open, decoder and idle threshold. `on_reconcile`
/// (reconciliation-activation Task 7) is threaded straight to its resync supervisor — `Some` fires the
/// recon trigger after every reconnect's event-replay settles; `None` (every non-recon caller) replays
/// events only. `fence` is the one the exec thread's client runs its submits through: both producers
/// here pass it (`vike_bridge_core::exec_actor::submit_fence`'s module doc). `clock` stamps the signed
/// subscribe, the offset the exec client's own requests sign on. The WS-API server pings every 20 s and
/// tungstenite pongs, so there is no app-level keepalive. `link_id` is the broker link id the account's
/// orders carry, which the decoder reads every echoed client id with.
pub fn spawn_binance_user_data_with_resync(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    clock: VenueClock,
    link_id: Option<String>,
) -> WebsocketSessionUserResyncHandle {
    let now_ms = clock.ms();
    // Built ONCE per pump; the reconnect loop reuses the open, so the gate persists across reconnects
    // and paces cumulative subscribe sends (spec §A keyed WS gate).
    let websocket_gate = crate::transport::ratelimit::websocket_rate_gate();
    let decode_symbol = symbol.clone();
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: "binance",
        lane: "binance",
        symbol,
        events,
        fence: Some(fence),
        resync_fetch,
        on_reconcile,
        open: move |stop: &AtomicBool| {
            open_binance_user_data_websocket(
                &websocket_url,
                &api_key,
                &api_secret,
                &now_ms,
                stop,
                &websocket_gate,
            )
        },
        decode: move |frame: &serde_json::Value| {
            map_binance_private(frame, "binance", &decode_symbol, link_id.as_deref())
        },
        keepalive: None,
        idle_threshold: Some(IDLE_THRESHOLD),
    })
}

#[cfg(test)]
mod user_data_tests {
    use super::*;
    use tungstenite::Message;
    use vike_bridge_core::websocket_session::user::{
        RESYNC_SETTLE, USER_DATA_MAX_BACKOFF, USER_DATA_POLL,
    };
    use vike_venues::venue_timings::BINANCE_SPOT;

    /// The spot lane's own pump, on a loopback socket that answers the signed subscribe, pushes `inflight`'s fill and
    /// closes once, so the pump re-opens and its replay runs. Reddens on either producer sending straight to the lane.
    #[test]
    fn the_spot_pump_and_its_replay_go_through_the_fence() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (session, stream) in listener.incoming().flatten().enumerate() {
                let mut ws = tungstenite::accept(stream).expect("the upgrade");
                let asked = ws.read().expect("the subscribe");
                let asked: serde_json::Value =
                    serde_json::from_str(asked.to_text().unwrap()).unwrap();
                let ack = serde_json::json!({"id": asked["id"], "status": 200, "result": {}});
                ws.send(Message::Text(ack.to_string().into())).unwrap();
                if session == 0 {
                    let fill = serde_json::json!({
                        "e": "executionReport", "s": "BTCUSDT", "x": "TRADE", "X": "FILLED", "S": "BUY",
                        "c": "inflight", "i": 9, "T": 5, "l": "1", "L": "10", "n": "0", "N": "USDT", "t": 1, "m": false
                    });
                    ws.send(Message::Text(fill.to_string().into())).unwrap();
                    let _ = ws.close(None);
                }
                while ws.read().is_ok() {}
            }
        });
        crate::family::execution::user_data::assert_the_pump_and_its_replay_go_through_the_fence(
            |events, fence, replay| {
                spawn_binance_user_data_with_resync(
                    url,
                    "key".to_string(),
                    "secret".to_string(),
                    "BTCUSDT".to_string(),
                    events,
                    fence,
                    replay,
                    None,
                    VenueClock::default(),
                    None,
                )
            },
        );
    }

    /// The numbers this session ran with before they moved into `venue_timings`, pinned here as
    /// literals: a retune of the row is then a deliberate edit in this file too, beside the matrix
    /// pin in vike-model. `IDLE_THRESHOLD` is what the spawn site hands the pump, so its EFFECTIVE
    /// value is the const; the handshake bound and the pump defaults are read straight from where
    /// `open_binance_user_data_websocket` and the shared spawn read them.
    #[test]
    fn the_session_runs_with_the_values_it_had_before_the_table() {
        assert_eq!(IDLE_THRESHOLD, Duration::from_secs(60), "3x the 20 s WS-API server ping");
        assert_eq!(BINANCE_SPOT.exec().handshake_timeout(), Duration::from_secs(10));
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1));
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1));
    }

    /// The row declares the reconnect policy the shared spawn really runs: the pump's own first
    /// backoff growing to the `USER_DATA_MAX_BACKOFF` it hands the loop.
    #[test]
    fn the_row_declares_the_backoff_the_pump_is_given() {
        let reconnect = BINANCE_SPOT.exec().reconnect();
        assert_eq!(
            reconnect.initial,
            vike_bridge_core::websocket_session::user::USER_DATA_INITIAL_BACKOFF
        );
        assert_eq!(reconnect.max, USER_DATA_MAX_BACKOFF);
    }
}
