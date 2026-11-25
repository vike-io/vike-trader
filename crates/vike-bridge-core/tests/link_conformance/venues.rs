//! The covered rows: one [`LinkVenue`] per exec lane on the shared private-stream pump, each handing over its venue's
//! REAL production pieces (the decoder its pump runs, the history replay or subscribe snapshot that reads a gap back,
//! the payload its keepalive sends, its open, and the words it refuses a key in) and nothing else. Frames and rows are
//! venue-native JSON, each built after the bridge test it names. No duration is here: every wait is the row's
//! (`vike_venues::venue_timings`), read by the scenarios.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use vike_binance::family::execution::user_data::{
    ListenKeyAuthentication, WebsocketSessionUserStreamKeyed, open_user_data_websocket,
};
use vike_bridge_core::exec_actor::DEFAULT_FILL_SENTINEL;
use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::silent_peer::{SilentDial, open_silent_peer};
use vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserOpenOutcome;
use vike_log::loopback::serve_replies;
use vike_model::events::Event;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_rate_limits::VenueRateLimitsMarket;
use vike_venues::venue_timings::VenueTimingsReconnect;

use super::Scenario;
use super::scenarios::Cell;
use super::sessions::Proof;

/// One fill of a scenario: `n` names it (each venue spells its order and trade ids from it), `ts` is the venue's
/// execution stamp in epoch ms.
#[derive(Clone, Copy, Debug)]
pub(super) struct Fill {
    pub n: u32,
    pub ts: i64,
}

/// A venue's decoder: one frame to its events.
pub(super) type Decoder = Box<dyn Fn(&Value) -> Vec<Event> + Send>;

/// A venue's history replay over the rows of the fills its history lists now.
pub(super) type HistoryReplay = Box<dyn Fn(&[Fill]) -> Vec<Event> + Send>;

/// A venue's subscribe snapshot frame carrying the fills it lists now.
pub(super) type SubscribeSnapshot = Box<dyn Fn(&[Fill]) -> String + Send>;

/// A snapshot venue's REAL reconcile nudge over the trigger a start hands it (`None`: its row nudges nothing): the hook
/// its pump's `on_reconnect` runs, and the one its decoder runs on each frame.
pub(super) type SnapshotNudge =
    fn(Option<mpsc::Sender<()>>) -> (Box<dyn FnMut()>, Box<dyn Fn(&Value)>);

/// The frames a cursor venue re-sends at an open, from the cursor its REAL open asked from, of the fills it lists now.
pub(super) type CursorResend = Box<dyn Fn(&[Fill]) -> Vec<String> + Send>;

/// One run's pieces of a venue's exec lane, with whatever state the venue keeps across them (a registry the exec
/// thread filled at accept, a first-snapshot flag).
pub(super) struct LinkLane {
    /// The decoder the venue's production pump runs.
    pub decode: Decoder,
    /// The venue-native stream frame of one full fill.
    pub fill_frame: Box<dyn Fn(Fill) -> String + Send>,
    /// How the venue reads a gap back; the row's `recovery` says which.
    pub recovery: Recovery,
}

/// How a lane reads back what a gap could have lost.
pub(super) enum Recovery {
    /// `HistoryWindow`: the REAL history replay over venue-native rows of the fills the venue lists now.
    History(HistoryReplay),
    /// `SnapshotOnSubscribe`: the frame the venue sends on every subscribe, carrying the fills it lists now, and the
    /// venue's own reconcile nudge.
    Snapshot { frame: SubscribeSnapshot, nudge: SnapshotNudge },
    /// `Cursor`: every open brings back what lies past the lane's cursor before its first frame, the venue's way.
    Cursor(CursorRead),
}

/// How a cursor venue's open brings back what lies past the lane's cursor.
pub(super) enum CursorRead {
    /// The open's REAL read past the cursor (oanda's `transactions/sinceid`), over venue-native rows of the fills the
    /// venue lists now: its events go out before the first frame.
    Read(HistoryReplay),
    /// The venue re-sends on the stream from the cursor the lane's REAL open asked from (alpaca's `since_id`), and the
    /// stream frame of a fill's order event that comes before its fill (such a cursor needs one event to resume from).
    Resend { resend: CursorResend, accepted: Box<dyn Fn(Fill) -> String + Send> },
}

/// What keeps the lane's socket alive from our side.
pub(super) enum Keepalive {
    /// The pump sends the venue's frame at the row's `keepalive_every`: its n-th send, from 1.
    Sends(fn(u64) -> String),
    /// The server pings and the pump answers each with a pong; it sends nothing of its own.
    ServerPings,
    /// The keepalive runs where no scripted stream reaches it, and the venue's own suite proves it on its seam: the
    /// tests `by`, and what they pin.
    OnItsSeam { by: &'static [Proof], pins: &'static str },
    /// The keepalive lives where no scripted stream reaches it, with where.
    Deferred(&'static str),
}

/// What ends a session for its key, and what the venue's code does then.
pub(super) enum Expiry {
    /// The venue refuses the key at the (re)login: the REAL open meets the venue's refusal reply and the pump gives up.
    RefusedLogin {
        replies: &'static [&'static str],
        open: fn(
            &str,
            &AtomicBool,
        ) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>,
    },
    /// The key is a listenKey: the venue says on the stream that it expired, and the REAL re-open mints another. The
    /// open is handed the REST origin and the socket host.
    ListenKey {
        open: fn(
            &str,
            &str,
            &AtomicBool,
        ) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamKeyed>,
    },
    /// No login answer to refuse: a key the venue no longer takes ends each session at once.
    ClosedAtOnce,
    /// Nothing on this stream can expire, with why (a refusal these scenarios cannot script is a `LinkVenue::cell`
    /// answer, never this).
    NotApplicable(&'static str),
}

/// A lane's announced close: the bridge's REAL notice hook (what its pump spec hands over) and a venue-native frame of the
/// notice, after the venue's documentation.
pub(super) struct Notice {
    pub hook: fn(&Value) -> Option<String>,
    pub frame: &'static str,
}

/// The exec thread's gap-sentinel.
pub(super) enum Sentinel {
    /// `vike_bridge_core::exec_actor`'s `run_loop`, with this window.
    RunLoop(Duration),
    /// The venue's own exec loop, with where and why it needs none.
    OwnLoop(&'static str),
}

/// A covered row: its key and its venue's real pieces.
pub(super) trait LinkVenue: Sync {
    fn venue(&self) -> &'static str;
    /// The market, only where the table splits the venue.
    fn market(&self) -> Option<VenueRateLimitsMarket> {
        None
    }
    /// A fresh lane for one run.
    fn lane(&self) -> LinkLane;
    fn keepalive(&self) -> Keepalive;
    fn expiry(&self) -> Expiry;
    fn sentinel(&self) -> Sentinel;
    /// The venue's REAL open dialled at `vike_bridge_core::silent_peer`'s peer (never called where [`Self::cell`] answers
    /// S12).
    fn dial_silent_peer(&self) -> SilentDial;
    /// The lane's announced close, where its bridge reads one (the row's `Handled`).
    fn notice(&self) -> Option<Notice> {
        None
    }
    /// The ladder the venue's spec hands its pump, over the row's `reconnect` (the default: the row's, no mode). A venue
    /// whose spec adds a mode hands its REAL builder, so S5 reads a silence the way production does.
    fn ladder(&self, row: &VenueTimingsReconnect) -> ReconnectBackoff {
        ReconnectBackoff::from_row(row)
    }
    /// A cell the lane's shape answers itself, with why; `None` (the default) leaves it to the row and the answers
    /// above. It is the ONE way an impl defers a cell (a refusal or a dial these scenarios cannot script).
    fn cell(&self, _scenario: Scenario) -> Option<Cell> {
        None
    }
}

/// The covered rows. Extending: a new venue on the shared pump implements [`LinkVenue`] and joins here.
pub(super) fn covered() -> Vec<Box<dyn LinkVenue>> {
    vec![
        Box::new(BinanceSpot),
        Box::new(BinancePerp),
        Box::new(Aster),
        Box::new(Bybit),
        Box::new(Okx),
        Box::new(Deribit),
        Box::new(Polymarket),
        Box::new(Hyperliquid),
        Box::new(Oanda),
        Box::new(Alpaca),
    ]
}

/// The family's pump: its private `keepalive_hook` sends aster's socket ping and the listenKey renewal (a REST PUT) over
/// `WebsocketSessionUserStreamKeyed`, a real socket, and judges the PUT due on the key's wall clock (its `renewed`), so
/// no scripted stream or virtual clock reaches it. Its loopback suite runs the hook at narrowed cadences, and the pins
/// beside it hold the production ones to the rows.
const FAMILY_PUMP: &str = "crates/bridges/binance/src/family/execution/user_data.rs";
/// The production spawn on a loopback venue: a socket keepalive on its cadence, and the venue's own pings keeping a
/// session, each answered.
const FAMILY_PIN: Proof = Proof {
    file: "crates/bridges/binance/tests/listenkey_stream_pin.rs",
    test: "the_listen_key_stream_keeps_its_steps_timings_lines_and_rows",
};
/// The hook's wake (60 s on both venues) and the family's one renewal cadence (every row's 1800 s).
const FAMILY_WAKE: Proof =
    Proof { file: FAMILY_PUMP, test: "the_hook_wakes_often_enough_for_a_retry_to_save_the_key" };
const FAMILY_RENEWAL: Proof =
    Proof { file: FAMILY_PUMP, test: "the_renewal_cadence_is_one_fact_for_the_family" };

/// Binance perp's S11, on the family's seam.
const BINANCE_PERP_KEEPALIVE: Keepalive = Keepalive::OnItsSeam {
    by: &[
        Proof {
            file: FAMILY_PUMP,
            test: "a_venue_that_does_not_ask_for_it_sends_the_socket_nothing",
        },
        Proof {
            file: "crates/bridges/binance/src/execution/user_data/perp.rs",
            test: "spec_pins_binance_perp_wire_shape",
        },
        FAMILY_WAKE,
        FAMILY_RENEWAL,
        FAMILY_PIN,
    ],
    pins: "the socket hears nothing of ours (the spec's `socket_keepalive` is `None`, as the row declares no \
           `keepalive_every`) and every server ping is answered, on one socket; the listenKey PUT runs at the row's \
           `session_renew_every` (1800 s), the hook waking every 60 s to retry a failed one. The loopback suite runs \
           the hook at narrowed cadences; the row's are held by the pins",
};

/// Aster's S11, on the family's seam.
const ASTER_KEEPALIVE: Keepalive = Keepalive::OnItsSeam {
    by: &[
        Proof {
            file: FAMILY_PUMP,
            test: "a_venue_that_asks_for_it_hears_from_the_socket_and_the_key_keeps_its_own_cadence",
        },
        Proof {
            file: "crates/bridges/aster/src/execution/user_data/perp.rs",
            test: "spec_keeps_the_socket_talking_inside_the_venues_timer",
        },
        FAMILY_WAKE,
        FAMILY_RENEWAL,
        FAMILY_PIN,
    ],
    pins: "a WS ping and a pong on the socket at the row's `keepalive_every` (60 s: the spec reads the row, the pin \
           holds the literal), the listenKey PUT on its own 1800 s, on one socket with no reconnect. The loopback \
           suite runs the hook at narrowed cadences; the row's are held by the pins",
};

/// The family's order history and trade rows of `fills`, after `crates/bridges/binance/src/execution/resync.rs`'s
/// `replays_gap_fill_cancel_expire_and_skips_open` (spot) and `perp_replays_fill_with_position_side_and_cancel` (perp).
fn family_history(fills: &[Fill], perp: bool) -> (Value, Value) {
    let orders = fills
        .iter()
        .map(|f| {
            json!({"orderId": 900 + f.n, "clientOrderId": format!("c{}", f.n), "status": "FILLED",
                   "origQty": "1", "updateTime": f.ts})
        })
        .collect();
    let trades = fills
        .iter()
        .map(|f| {
            if perp {
                json!({"id": f.n, "orderId": 900 + f.n, "price": "50000", "qty": "1", "commission": "0",
                       "commissionAsset": "USDT", "maker": false, "time": f.ts, "side": "BUY",
                       "positionSide": "BOTH"})
            } else {
                json!({"id": f.n, "orderId": 900 + f.n, "price": "50000", "qty": "1", "commission": "0",
                       "commissionAsset": "USDT", "time": f.ts, "isBuyer": true, "isMaker": false})
            }
        })
        .collect();
    (Value::Array(orders), Value::Array(trades))
}

/// The family's perp `ORDER_TRADE_UPDATE` of one full fill, after `crates/bridges/binance/src/family/fill_trade_id_tests.rs`'s
/// `perp_update`.
fn family_perp_fill(f: Fill) -> String {
    json!({"e": "ORDER_TRADE_UPDATE", "T": f.ts, "o": {"s": "BTCUSDT", "c": format!("c{}", f.n), "x": "TRADE",
           "X": "FILLED", "S": "BUY", "l": "1", "L": "50000", "n": "0", "N": "USDT", "t": f.n, "m": false,
           "ps": "BOTH"}})
    .to_string()
}

/// A loopback REST origin that mints one listenKey per POST.
fn listen_key_origin() -> String {
    serve_replies("", vec![(200, r#"{"listenKey":"the-listen-key"}"#.to_string())]).0
}

/// Binance's listenKey auth is a header, the test key's.
struct BinanceKeyHeader;

impl ListenKeyAuthentication for BinanceKeyHeader {
    fn query(&self, _method: &str) -> String {
        String::new()
    }
    fn header(&self) -> Option<(&'static str, String)> {
        Some(("X-MBX-APIKEY", "key".to_string()))
    }
}

/// Binance spot: the WS-API stream (`crates/bridges/binance/src/execution/user_data/spot.rs`'s
/// `spawn_binance_user_data_with_resync`).
struct BinanceSpot;

impl LinkVenue for BinanceSpot {
    fn venue(&self) -> &'static str {
        "binance"
    }
    fn market(&self) -> Option<VenueRateLimitsMarket> {
        Some(VenueRateLimitsMarket::Spot)
    }
    fn lane(&self) -> LinkLane {
        LinkLane {
            decode: Box::new(|frame| {
                vike_binance::instrument::names::speak_user_names(
                    vike_binance::family::execution::event_mapper::spot::map_private(
                        frame, "binance", "BTCUSDT", None,
                    ),
                )
            }),
            // After `crates/bridges/binance/src/family/fill_trade_id_tests.rs`'s `spot_fill`.
            fill_frame: Box::new(|f| {
                json!({"e": "executionReport", "s": "BTCUSDT", "x": "TRADE", "X": "FILLED", "S": "BUY",
                       "c": format!("c{}", f.n), "i": 900 + f.n, "T": f.ts, "l": "1", "L": "50000", "n": "0",
                       "N": "USDT", "t": f.n, "m": false})
                .to_string()
            }),
            recovery: Recovery::History(Box::new(|fills| {
                let (orders, trades) = family_history(fills, false);
                vike_binance::family::execution::resync::map_history(
                    &orders, &trades, "binance", "BTCUSDT", None,
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::ServerPings
    }
    fn expiry(&self) -> Expiry {
        Expiry::RefusedLogin {
            // `crates/bridges/binance/src/transport/ws_auth.rs`'s `match_subscribe_ack` reads the reply to the request id
            // `vtr{now_ms}`, and the open below stamps 0.
            replies: &[
                r#"{"id":"vtr0","status":401,"error":{"code":-2015,"msg":"Invalid API-key, IP, or permissions for action."}}"#,
            ],
            open: |url, stop| {
                vike_binance::execution::user_data::spot::open_binance_user_data_websocket(
                    url,
                    "key",
                    "secret",
                    || 0,
                    stop,
                    &vike_binance::transport::ratelimit::websocket_rate_gate(),
                )
            },
        }
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/binance/src/family/execution/mod.rs`'s `run_spot`.
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        open_silent_peer(|url| {
            vike_binance::execution::user_data::spot::open_binance_user_data_websocket(
                &url,
                "key",
                "secret",
                || 0,
                &AtomicBool::new(false),
                &vike_binance::transport::ratelimit::websocket_rate_gate(),
            )
        })
    }
    fn notice(&self) -> Option<Notice> {
        Some(Notice {
            hook: vike_binance::execution::user_data::spot::server_shutdown_notice,
            frame: r#"{"event":{"e":"serverShutdown","E":1770123456789,"T":1770124000000}}"#,
        })
    }
}

/// Binance USDⓈ-M: the listenKey stream (`crates/bridges/binance/src/execution/user_data/perp.rs`'s
/// `spawn_binance_perp_user_data_with_resync`).
struct BinancePerp;

impl LinkVenue for BinancePerp {
    fn venue(&self) -> &'static str {
        "binance"
    }
    fn market(&self) -> Option<VenueRateLimitsMarket> {
        Some(VenueRateLimitsMarket::Perp)
    }
    fn lane(&self) -> LinkLane {
        LinkLane {
            decode: Box::new(|frame| {
                vike_binance::instrument::names::speak_user_names(
                    vike_binance::execution::user_data::perp::coin_m_stream(
                        vike_binance::execution::event_mapper::perp::map_binance_perp_stream(
                            frame,
                            "binance",
                            "BTCUSDT.P",
                            false,
                            None,
                        ),
                        false,
                    ),
                )
            }),
            fill_frame: Box::new(family_perp_fill),
            recovery: Recovery::History(Box::new(|fills| {
                let (orders, trades) = family_history(fills, true);
                vike_binance::family::execution::resync::map_perp_history(
                    &orders,
                    &trades,
                    "binance",
                    "BTCUSDT.P",
                    None,
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        BINANCE_PERP_KEEPALIVE
    }
    fn expiry(&self) -> Expiry {
        Expiry::ListenKey {
            open: |rest, websocket, stop| {
                open_user_data_websocket(
                    rest,
                    vike_binance::execution::user_data::perp::LISTENKEY_PATH,
                    websocket,
                    "",
                    &BinanceKeyHeader,
                    stop,
                )
            },
        }
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/binance/src/family/execution/mod.rs`'s `run_perp`.
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        let rest = listen_key_origin();
        open_silent_peer(move |url| {
            open_user_data_websocket(
                &rest,
                vike_binance::execution::user_data::perp::LISTENKEY_PATH,
                &url,
                "",
                &BinanceKeyHeader,
                &AtomicBool::new(false),
            )
        })
    }
}

/// Aster on its perp lane, the one its adapter mounts (`crates/bridges/aster/src/execution/user_data/perp.rs`'s
/// `spawn_aster_perp_user_data_with_resync`); its spot lane is the same family pump on the same timing row.
struct Aster;

/// Aster's listenKey auth: a signed query from a test key.
fn aster_auth() -> vike_aster::transport::listenkey_auth::ListenKeyAuthenticationAster {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "0x000000000000000000000000000000000000aa".to_string().into(),
        api_secret: "0x0123456789012345678901234567890123456789012345678901234567890a"
            .to_string()
            .into(),
        passphrase: None,
    };
    vike_aster::transport::listenkey_auth::ListenKeyAuthenticationAster::new(
        &creds,
        vike_aster::execution::user_data::perp::LISTENKEY_PATH,
        &vike_bridge_core::signer::VenueClock::default(),
    )
}

impl LinkVenue for Aster {
    fn venue(&self) -> &'static str {
        "aster"
    }
    fn lane(&self) -> LinkLane {
        LinkLane {
            decode: Box::new(|frame| {
                vike_binance::family::execution::event_mapper::perp::map_perp_stream(
                    frame,
                    "aster",
                    "BTCUSDT.P",
                    None,
                )
            }),
            fill_frame: Box::new(family_perp_fill),
            recovery: Recovery::History(Box::new(|fills| {
                let (orders, trades) = family_history(fills, true);
                vike_aster::execution::resync::map_aster_perp_history(
                    &orders,
                    &trades,
                    "aster",
                    "BTCUSDT.P",
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        ASTER_KEEPALIVE
    }
    fn expiry(&self) -> Expiry {
        Expiry::ListenKey {
            open: |rest, websocket, stop| {
                open_user_data_websocket(
                    rest,
                    vike_aster::execution::user_data::perp::LISTENKEY_PATH,
                    websocket,
                    "/ws",
                    &aster_auth(),
                    stop,
                )
            },
        }
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/binance/src/family/execution/mod.rs`'s `run_perp`, which aster's `run` calls.
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        let rest = listen_key_origin();
        open_silent_peer(move |url| {
            open_user_data_websocket(
                &rest,
                vike_aster::execution::user_data::perp::LISTENKEY_PATH,
                &url,
                "/ws",
                &aster_auth(),
                &AtomicBool::new(false),
            )
        })
    }
}

/// Bybit V5 linear perp (`crates/bridges/bybit/src/execution/user_data.rs`'s `spawn_perp`), mounted on its user name
/// with the kept list held, as production names it (decision 0290).
struct Bybit;

impl LinkVenue for Bybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn lane(&self) -> LinkLane {
        vike_bybit::history::kept_list_for_tests();
        LinkLane {
            decode: Box::new(vike_bybit::execution::user_data::perp_frame_decoder("BTCUSDT.P")),
            // After `crates/bridges/bybit/tests/offline/trade_id_gate.rs`'s `an_execution_with_a_real_id_still_dual_publishes`.
            fill_frame: Box::new(|f| {
                json!({"topic": "execution", "creationTime": 1, "data": [{"category": "linear", "symbol": "BTCUSDT",
                       "orderId": format!("o{}", f.n), "orderLinkId": format!("c{}", f.n), "execType": "Trade",
                       "execId": format!("t{}", f.n), "side": "Buy", "execPrice": "50000", "execQty": "1",
                       "execFee": "0.1", "feeCurrency": "USDT", "isMaker": false, "cumExecQty": "1",
                       "orderQty": "1", "leavesQty": "0", "markPrice": "50000", "positionIdx": 0,
                       "execTime": f.ts.to_string()}]})
                .to_string()
            }),
            // After `crates/bridges/bybit/src/execution/resync.rs`'s `replays_gap_fill_cancel_and_open_partial`.
            recovery: Recovery::History(Box::new(|fills| {
                let orders: Vec<Value> = fills
                    .iter()
                    .map(|f| {
                        json!({"orderId": format!("o{}", f.n), "orderLinkId": format!("c{}", f.n),
                               "orderStatus": "Filled", "updatedTime": f.ts.to_string()})
                    })
                    .collect();
                let executions: Vec<Value> = fills
                    .iter()
                    .map(|f| {
                        json!({"execType": "Trade", "orderId": format!("o{}", f.n),
                               "orderLinkId": format!("c{}", f.n), "execId": format!("t{}", f.n),
                               "execPrice": "50000", "execQty": "1", "side": "Buy", "execFee": "0.1",
                               "feeCurrency": "USDT", "isMaker": false, "markPrice": "50000",
                               "execTime": f.ts.to_string()})
                    })
                    .collect();
                vike_bybit::execution::resync::map_bybit_history(
                    &Value::Array(orders),
                    &Value::Array(executions),
                    "bybit",
                    "BTCUSDT.P",
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Sends(|_| vike_bybit::transport::ws_auth::ping_frame())
    }
    fn expiry(&self) -> Expiry {
        Expiry::RefusedLogin {
            // `crates/bridges/bybit/src/transport/ws_auth.rs`'s `classifies_transient_vs_auth`.
            replies: &[
                r#"{"op":"auth","success":false,"ret_msg":"API key is invalid.","retCode":10003}"#,
            ],
            open: |url, stop| {
                vike_bybit::execution::user_data::open_bybit_user_data_websocket(
                    url,
                    "key",
                    "secret",
                    || 0,
                    &["execution", "order"],
                    stop,
                    &vike_bybit::transport::ratelimit::websocket_rate_gate(),
                )
            },
        }
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/bybit/src/execution/mod.rs`'s `run`.
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        open_silent_peer(|url| {
            vike_bybit::execution::user_data::open_bybit_user_data_websocket(
                &url,
                "key",
                "secret",
                || 0,
                &["execution", "order"],
                &AtomicBool::new(false),
                &vike_bybit::transport::ratelimit::websocket_rate_gate(),
            )
        })
    }
}

/// OKX's `orders` socket on the mounted SWAP (`crates/bridges/okx/src/execution/user_data.rs`'s
/// `spawn_okx_perp_user_data_with_resync`); its algo socket is the same pump with another decoder. Named with the kept
/// list held, as production names it (decision 0290), and as the okx market row holds it in this process.
struct Okx;

impl LinkVenue for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn lane(&self) -> LinkLane {
        vike_okx::instrument::kept_list_for_tests();
        LinkLane {
            decode: Box::new(|frame| {
                vike_okx::instrument::speak_user_names(
                    vike_okx::execution::event_mapper::map_okx_perp_by_instrument(
                        frame,
                        "okx",
                        "BTC-USDT-SWAP",
                        |_| Some(1.0),
                    ),
                )
            }),
            // After `crates/vike-bridge-core/tests/bridge_conformance/venues.rs`'s `Okx::frame_fill`.
            fill_frame: Box::new(|f| {
                json!({"arg": {"channel": "orders"}, "data": [{"clOrdId": format!("c{}", f.n), "state": "filled",
                       "tradeId": format!("t{}", f.n), "fillSz": "1", "fillPx": "50000", "fillFee": "-0.1",
                       "fillFeeCcy": "USDT", "side": "buy", "execType": "T", "accFillSz": "1", "sz": "1",
                       "fillTime": f.ts.to_string()}]})
                .to_string()
            }),
            // After `crates/bridges/okx/src/execution/resync.rs`'s `replays_gap_fill_and_cancel_with_contracts_to_base`.
            recovery: Recovery::History(Box::new(|fills| {
                let orders: Vec<Value> = fills
                    .iter()
                    .map(|f| {
                        json!({"ordId": format!("o{}", f.n), "clOrdId": format!("c{}", f.n), "state": "filled",
                               "uTime": f.ts.to_string()})
                    })
                    .collect();
                let trades: Vec<Value> = fills
                    .iter()
                    .map(|f| {
                        json!({"tradeId": format!("t{}", f.n), "ordId": format!("o{}", f.n), "clOrdId": "0",
                               "fillPx": "50000", "fillSz": "1", "side": "buy", "fee": "-0.1", "feeCcy": "USDT",
                               "execType": "T", "subType": "1", "ts": f.ts.to_string()})
                    })
                    .collect();
                vike_okx::instrument::speak_user_names(
                    vike_okx::execution::resync::map_okx_history(
                        &Value::Array(orders),
                        &Value::Array(trades),
                        "okx",
                        "BTC-USDT-SWAP",
                        1.0,
                    ),
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Sends(|_| vike_okx::transport::ws_auth::PING_TEXT.to_string())
    }
    fn expiry(&self) -> Expiry {
        Expiry::RefusedLogin {
            // `crates/bridges/okx/src/transport/ws_auth.rs`'s `classifies_transient_vs_auth`.
            replies: &[r#"{"event":"error","code":"60009","msg":"Login failed."}"#],
            open: |url, stop| {
                vike_okx::execution::user_data::open_okx_user_data_websocket(
                    url,
                    "key",
                    "secret",
                    "passphrase",
                    || 0,
                    "orders",
                    "ANY",
                    stop,
                    &vike_okx::transport::ratelimit::websocket_rate_gate(),
                )
            },
        }
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/okx/src/execution/mod.rs`'s `run`.
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        open_silent_peer(|url| {
            vike_okx::execution::user_data::open_okx_user_data_websocket(
                &url,
                "key",
                "secret",
                "passphrase",
                || 0,
                "orders",
                "ANY",
                &AtomicBool::new(false),
                &vike_okx::transport::ratelimit::websocket_rate_gate(),
            )
        })
    }
    fn notice(&self) -> Option<Notice> {
        Some(Notice {
            hook: vike_okx::execution::user_data::service_upgrade_notice,
            frame: r#"{"event":"notice","code":"64008","msg":"The connection will soon be closed for a service upgrade. Please reconnect.","connId":"a4d3ae55"}"#,
        })
    }
}

/// Deribit's private stream (`crates/bridges/deribit/src/execution/user_data.rs`'s
/// `spawn_deribit_user_data_with_resync`) on the venue name the exec thread mounts, with the kept list held as
/// production holds it: the market row holds it in this process too, and a fill must not change its name mid-run.
struct Deribit;

/// A deribit `user.trades` row of one full fill, after `crates/vike-bridge-core/tests/bridge_conformance/venues.rs`'s
/// `Deribit::frame_fill`.
fn deribit_trade(f: Fill) -> Value {
    json!({"trade_id": format!("t{}", f.n), "label": format!("c{}", f.n), "instrument_name": "BTC-PERPETUAL",
           "direction": "buy", "amount": 1.0, "price": 50000.0, "fee": 0.0, "fee_currency": "BTC",
           "liquidity": "T", "timestamp": f.ts, "state": "filled"})
}

/// The channel the deribit pump subscribes fills on.
fn deribit_channels() -> [String; 1] {
    ["user.trades.any.any.raw".to_string()]
}

impl LinkVenue for Deribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }
    fn lane(&self) -> LinkLane {
        vike_deribit::names::hold_deribit_kept_rows();
        LinkLane {
            decode: Box::new(|frame| {
                vike_deribit::names::speak_user_names(
                    vike_deribit::execution::event_mapper::map_deribit_private(
                        frame,
                        "deribit",
                        "BTC-PERPETUAL",
                    ),
                )
            }),
            fill_frame: Box::new(|f| {
                json!({"jsonrpc": "2.0", "method": "subscription",
                       "params": {"channel": "user.trades.any.any.raw", "data": [deribit_trade(f)]}})
                .to_string()
            }),
            // After `crates/bridges/deribit/src/execution/resync.rs`'s `replays_fills_then_cancel_and_reject_terminals`.
            recovery: Recovery::History(Box::new(|fills| {
                let trades = fills.iter().copied().map(deribit_trade).collect();
                vike_deribit::names::speak_user_names(
                    vike_deribit::execution::resync::map_deribit_history(
                        &json!([]),
                        &Value::Array(trades),
                        "deribit",
                        "BTC-PERPETUAL",
                    ),
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Sends(|n| {
            vike_deribit::transport::ws_auth::build_public_test(
                i64::try_from(n).expect("a small send count"),
            )
            .to_string()
        })
    }
    fn expiry(&self) -> Expiry {
        Expiry::RefusedLogin {
            // A fresh builder numbers the auth 1; 13009 is no transient code
            // (`crates/bridges/deribit/src/execution/user_data.rs`'s `deribit_ack_is_transient`).
            replies: &[
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":13009,"message":"unauthorized"}}"#,
            ],
            open: |url, stop| {
                vike_deribit::execution::user_data::open_deribit_user_data_websocket(
                    url,
                    "client",
                    "secret",
                    &deribit_channels(),
                    &vike_deribit::transport::rpc::JsonRpcBuilder::new(),
                    &Mutex::new(String::new()),
                    stop,
                    &vike_deribit::transport::ratelimit::websocket_rate_gate(),
                )
            },
        }
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/deribit/src/execution/mod.rs`'s `run`.
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        open_silent_peer(|url| {
            vike_deribit::execution::user_data::open_deribit_user_data_websocket(
                &url,
                "client",
                "secret",
                &deribit_channels(),
                &vike_deribit::transport::rpc::JsonRpcBuilder::new(),
                &Mutex::new(String::new()),
                &AtomicBool::new(false),
                &vike_deribit::transport::ratelimit::websocket_rate_gate(),
            )
        })
    }
    fn notice(&self) -> Option<Notice> {
        Some(Notice {
            hook: vike_deribit::execution::user_data::maintenance_notice,
            frame: r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"platform_state","data":{"maintenance":true}}}"#,
        })
    }
}

/// Polymarket's user channel (`crates/bridges/polymarket/src/execution/user_data/mod.rs`'s
/// `spawn_polymarket_user_data_with_resync_tracked`). Its fills name the CLOB's order id, so the lane's registry
/// learns each scenario order at accept first, as the exec thread does.
struct Polymarket;

/// One trade of fill `f` as a `/data/trades` row (no `timestamp`: the REST rows carry none), after
/// `crates/bridges/polymarket/src/execution/resync.rs`'s `replays_taker_and_cancel_rekeyed`; the user channel's frame is
/// the same trade with its event words and stamp.
fn polymarket_trade(f: Fill, status: &str) -> Value {
    json!({"id": format!("h{}", f.n), "status": status, "asset_id": "111", "side": "BUY", "size": "10",
           "price": "0.5", "taker_order_id": format!("0xT{}", f.n), "maker_orders": []})
}

impl LinkVenue for Polymarket {
    fn venue(&self) -> &'static str {
        "polymarket"
    }
    fn lane(&self) -> LinkLane {
        let registry = vike_polymarket::OrderVenueIdTracker::new();
        for n in 1..=4 {
            let _ = registry.on_accept(&format!("c{n}"), &format!("0xT{n}"), 1);
        }
        let replayed = registry.clone();
        LinkLane {
            decode: Box::new(move |frame| {
                vike_polymarket::decode_user_with_tracker(frame, &registry, None)
            }),
            fill_frame: Box::new(|f| {
                let mut frame = polymarket_trade(f, "MATCHED");
                frame["event_type"] = json!("trade");
                frame["type"] = json!("TRADE");
                frame["timestamp"] = json!(f.ts.to_string());
                frame.to_string()
            }),
            recovery: Recovery::History(Box::new(move |fills| {
                let trades = fills.iter().map(|f| polymarket_trade(*f, "CONFIRMED")).collect();
                vike_polymarket::map_polymarket_history(
                    &Value::Array(trades),
                    &json!([]),
                    &replayed,
                )
            })),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Sends(|_| vike_polymarket::USER_CHANNEL_PING.to_string())
    }
    fn expiry(&self) -> Expiry {
        Expiry::ClosedAtOnce
    }
    fn sentinel(&self) -> Sentinel {
        Sentinel::OwnLoop(
            "its exec thread runs its own loop (`crates/bridges/polymarket/src/execution/mod.rs`'s `run`) with no \
             gap-sentinel: an event that beats its order's accept is parked and replayed by that thread",
        )
    }
    fn dial_silent_peer(&self) -> SilentDial {
        super::declare_polymarket_direct_egress();
        open_silent_peer(|url| {
            vike_polymarket::open_polymarket_user_data_websocket(
                &url,
                &vike_polymarket::PolymarketConfig::default(),
                &[],
                &AtomicBool::new(false),
                &RateGatePerKey::new("polymarket", Vec::new(), None),
            )
        })
    }
}

/// Hyperliquid's `userFills` stream (`crates/bridges/hyperliquid/src/execution/user_data.rs`'s
/// `spawn_hyperliquid_user_data`): no REST replay, every subscribe re-sends the snapshot.
struct Hyperliquid;

/// A `userFills` row of one full fill under the order's `cloid`, after
/// `crates/bridges/hyperliquid/src/execution/user_data_tests.rs`'s `fill_row_cloid`.
fn hyperliquid_fill(f: Fill, cloid: &str) -> Value {
    json!({"coin": "BTC", "px": "50000", "sz": "1", "side": "B", "oid": f.n, "cloid": cloid, "tid": f.n,
           "fee": "0.5", "feeToken": "USDC", "crossed": true, "time": f.ts})
}

/// The `cloid` the exec thread registered for fill `f`'s order.
fn hyperliquid_cloid(cloids: &[String], f: Fill) -> &str {
    &cloids[usize::try_from(f.n).expect("a small fill number") - 1]
}

impl LinkVenue for Hyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }
    fn lane(&self) -> LinkLane {
        let registry = vike_hyperliquid::execution::OrderClientIdVenueRegistry::new();
        let cloids: Vec<String> = (1..=4).map(|n| registry.register(&format!("oc-{n}"))).collect();
        let symbology = vike_hyperliquid::instrument::symbology::Symbology::from_meta(
            &json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]}),
            &json!({"tokens": [], "universe": []}),
        );
        let snapshot_seen = AtomicBool::new(false);
        let spawn_ms = vike_model::now_ms();
        let (streamed, snapshotted) = (cloids.clone(), cloids);
        LinkLane {
            decode: Box::new(move |frame| {
                vike_hyperliquid::execution::user_data::map_frame_to_events(
                    frame,
                    &symbology,
                    &registry,
                    &snapshot_seen,
                    spawn_ms,
                )
            }),
            fill_frame: Box::new(move |f| {
                json!({"channel": "userFills", "data": {"isSnapshot": false,
                       "fills": [hyperliquid_fill(f, hyperliquid_cloid(&streamed, f))]}})
                .to_string()
            }),
            recovery: Recovery::Snapshot {
                frame: Box::new(move |fills| {
                    let rows: Vec<Value> = fills
                        .iter()
                        .map(|f| hyperliquid_fill(*f, hyperliquid_cloid(&snapshotted, *f)))
                        .collect();
                    json!({"channel": "userFills", "data": {"isSnapshot": true, "fills": rows}})
                        .to_string()
                }),
                // `spawn_hyperliquid_user_data`'s wiring of it.
                nudge: |trigger| {
                    use vike_hyperliquid::execution::user_data::ReconnectNudge;
                    let nudge = Arc::new(ReconnectNudge::new(trigger));
                    let seen = Arc::clone(&nudge);
                    let reopened: Box<dyn FnMut()> = Box::new(move || nudge.reopened());
                    let decoded: Box<dyn Fn(&Value)> =
                        Box::new(move |frame: &Value| seen.decoded(frame));
                    (reopened, decoded)
                },
            },
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Sends(|_| vike_hyperliquid::execution::user_data::PING_FRAME.to_string())
    }
    fn expiry(&self) -> Expiry {
        Expiry::NotApplicable(
            "the stream carries no key or token: it subscribes by address \
             (`crates/bridges/hyperliquid/src/execution/user_data.rs`'s `open_websocket`)",
        )
    }
    fn sentinel(&self) -> Sentinel {
        Sentinel::OwnLoop(
            "its exec thread runs its own loop (`crates/bridges/hyperliquid/src/execution/actor.rs`'s `run`) with no \
             gap-sentinel: a gap comes back in the next subscribe's `userFills` snapshot",
        )
    }
    fn dial_silent_peer(&self) -> SilentDial {
        open_silent_peer(|url| {
            vike_hyperliquid::execution::user_data::open_websocket(&url, "{}", "{}")
        })
    }
}

/// OANDA's transactions stream (`crates/bridges/oanda/src/execution/user_data.rs`'s `stream_transactions`): a chunked
/// HTTP body read as lines on the shared pump, whose every open reads `transactions/sinceid` past the lane's floor before
/// the first line (the row's `Cursor`). Its fills name the symbol the exec thread's submit hook recorded for the order.
struct Oanda;

/// One `ORDER_FILL` transaction of fill `f`, the same row on the stream and in a `sinceid` read, after
/// `crates/bridges/oanda/src/execution/user_data_tests.rs`'s `stream_fill`.
fn oanda_fill(f: Fill) -> Value {
    json!({"type": "ORDER_FILL", "id": f.n.to_string(), "time": (f.ts / 1000).to_string(),
           "orderID": format!("9{}", f.n), "instrument": "EUR_USD", "units": "1", "price": "1.1",
           "clientExtensions": {"id": format!("c{}", f.n)}})
}

impl LinkVenue for Oanda {
    fn venue(&self) -> &'static str {
        "oanda"
    }
    fn lane(&self) -> LinkLane {
        let symbols = vike_oanda::submitted_symbols();
        for n in 1..=4 {
            symbols.record(&format!("c{n}"), "EURUSD");
        }
        let read = std::sync::Arc::clone(&symbols);
        LinkLane {
            decode: Box::new(move |frame| vike_oanda::decode_transaction_events(frame, &symbols)),
            fill_frame: Box::new(|f| oanda_fill(f).to_string()),
            // After `crates/bridges/oanda/src/execution/user_data_tests.rs`'s `gap_of`.
            recovery: Recovery::Cursor(CursorRead::Read(Box::new(move |fills| {
                let rows: Vec<Value> = fills.iter().copied().map(oanda_fill).collect();
                vike_oanda::map_transactions_since(&json!({"transactions": rows}), &read)
            }))),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Deferred(
            "we send nothing and HTTP has no ping: the venue's HEARTBEAT line every ~5 s is a text line, and the bound \
             that judges its silence is the agent's per-read timeout (`crates/bridges/oanda/src/execution/user_data.rs`'s \
             `transactions_agent`), held by `crates/bridges/oanda/src/execution/user_data_tests.rs`'s \
             `a_heartbeating_stream_survives_past_the_response_head_bound`",
        )
    }
    fn expiry(&self) -> Expiry {
        Expiry::NotApplicable("answered by `cell`: S4 is DEFERRED")
    }
    fn sentinel(&self) -> Sentinel {
        Sentinel::OwnLoop(
            "its exec thread runs its own loop (`crates/bridges/oanda/src/execution/mod.rs`'s `run_on`) with no \
             gap-sentinel: every stream open reads the exact gap back past the lane's floor",
        )
    }
    fn dial_silent_peer(&self) -> SilentDial {
        unreachable!("S12 is DEFERRED for oanda (`Oanda::cell`)")
    }
    fn cell(&self, scenario: Scenario) -> Option<Cell> {
        let deferred = |why: &str| Some(Cell::Deferred(why.to_string()));
        match scenario {
            Scenario::Expiry => deferred(
                "a refused token is an HTTP status on the stream open, not a login reply on a socket: a 401 keeps the lane \
                 on the slow clock and three 403s end it (`crates/bridges/oanda/src/execution/user_data.rs`'s \
                 `MAX_TERMINAL_OPENS`), held by the loopback suite beside it \
                 (`crates/bridges/oanda/src/execution/user_data_tests.rs`'s \
                 `a_401_past_the_budget_keeps_the_lane_and_the_stream_comes_back` and \
                 `a_permanently_rejected_open_gives_up_after_the_bounded_budget`)",
            ),
            Scenario::DialBound => deferred(
                "a chunked HTTP GET, not a websocket upgrade: a host that accepts and says nothing is cut at the \
                 response-head wait (the row's `response_timeout`), held by \
                 `crates/bridges/oanda/src/execution/user_data_tests.rs`'s \
                 `a_host_that_accepts_and_never_answers_reads_as_no_response_head`",
            ),
            _ => None,
        }
    }
}

/// Alpaca's trade-events SSE (`crates/bridges/alpaca/src/execution/user_data.rs`'s `spawn_trade_events`): no REST
/// replay, its open resumes from the last event id it saw (`vike_alpaca::TradeEventsCursor`).
struct Alpaca;

/// The sortable event id (a ULID on the venue) of fill `f`'s order event `kind`: `a` its acceptance, `f` its fill.
fn alpaca_event_id(f: Fill, kind: char) -> String {
    format!("01LINK{:010}{kind}", f.n)
}

/// One `/v2/events/trades` event of fill `f`'s order on the share `AAPL`, after
/// `crates/bridges/alpaca/tests/engine_fill.rs`'s `sse`: its acceptance (`a`) or its full fill (`f`).
fn alpaca_event(f: Fill, kind: char) -> String {
    let order = json!({"client_order_id": format!("c{}", f.n), "id": format!("v{}", f.n), "symbol": "AAPL",
                       "side": "buy"});
    let id = alpaca_event_id(f, kind);
    match kind {
        'f' => json!({"event": "fill", "event_id": id, "execution_id": format!("e{}", f.n), "price": "100",
                      "qty": "1", "timestamp": f.ts.to_string(), "order": order}),
        _ => json!({"event": "new", "event_id": id, "timestamp": f.ts.to_string(), "order": order}),
    }
    .to_string()
}

impl LinkVenue for Alpaca {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn lane(&self) -> LinkLane {
        // The ONE cursor the lane's open and decode share, as `spawn_trade_events` builds it.
        let cursor = Arc::new(Mutex::new(vike_alpaca::TradeEventsCursor::default()));
        let (decoded, opened) = (Arc::clone(&cursor), cursor);
        let symbols = vike_alpaca::submitted_symbols();
        LinkLane {
            decode: Box::new(move |frame| {
                let admitted = decoded.lock().unwrap_or_else(|e| e.into_inner()).admit(frame);
                if admitted { vike_alpaca::decode_trade_event(frame, &symbols) } else { Vec::new() }
            }),
            fill_frame: Box::new(|f| alpaca_event(f, 'f')),
            recovery: Recovery::Cursor(CursorRead::Resend {
                // The REAL open asks from the cursor (and arms its skip of the boundary event); the venue re-sends every
                // event from that id on, the boundary INCLUDED (live-verified, the bridge's module doc). A bare open is
                // live from now.
                resend: Box::new(move |fills| {
                    let url =
                        opened.lock().unwrap_or_else(|e| e.into_inner()).open_url("https://broker");
                    let Some((_, since)) = url.split_once("since_id=") else { return Vec::new() };
                    fills
                        .iter()
                        .flat_map(|f| {
                            ['a', 'f']
                                .map(|kind| (alpaca_event_id(*f, kind), alpaca_event(*f, kind)))
                        })
                        .filter(|(id, _)| id.as_str() >= since)
                        .map(|(_, frame)| frame)
                        .collect()
                }),
                accepted: Box::new(|f| alpaca_event(f, 'a')),
            }),
        }
    }
    fn keepalive(&self) -> Keepalive {
        Keepalive::Deferred(
            "an SSE body over HTTP: there is no ping to send or answer, and the venue's `:heartbeat` comment lines \
             have no documented cadence (unmeasured); the row's 30 s read-idle bound is the only liveness check, and \
             a quiet session it ends is a reopen (`vike_alpaca::trade_events_backoff`'s silence mode, S5)",
        )
    }
    fn ladder(&self, _row: &VenueTimingsReconnect) -> ReconnectBackoff {
        // The REAL builder over the same row (`crates/bridges/alpaca/src/execution/user_data.rs`'s `RECONNECT`).
        vike_alpaca::trade_events_backoff()
    }
    fn expiry(&self) -> Expiry {
        Expiry::NotApplicable("answered by `cell`: S4 is DEFERRED")
    }
    fn sentinel(&self) -> Sentinel {
        // `crates/bridges/alpaca/src/execution/mod.rs`'s `run`, with an empty replay (S8 does not apply, `cell`).
        Sentinel::RunLoop(DEFAULT_FILL_SENTINEL)
    }
    fn dial_silent_peer(&self) -> SilentDial {
        unreachable!("S12 is DEFERRED for alpaca (`Alpaca::cell`)")
    }
    fn cell(&self, scenario: Scenario) -> Option<Cell> {
        let deferred = |why: &str| Some(Cell::Deferred(why.to_string()));
        match scenario {
            Scenario::Expiry => deferred(
                "a lapsed Bearer is answered 401 at the OPEN, which force-refreshes the token and is a refused open \
                 the pump waits out on the row's ladder (`crates/bridges/alpaca/src/execution/user_data_tests.rs`'s \
                 `a_refused_open_refreshes_the_token_and_another_answer_is_a_transport_failure`); this rig's scripted \
                 opens never meet a 401",
            ),
            Scenario::BusyCommands => Some(Cell::NotApplicable(
                "its gap-sentinel replays nothing (`crates/bridges/alpaca/src/execution/mod.rs`'s `run` hands \
                 `run_loop` an empty replay): the stream's cursor re-sends a gap at the open"
                    .to_string(),
            )),
            Scenario::DialBound => deferred(
                "an HTTP open over `ureq`: the row's `dial_timeout` bounds the TCP and TLS connect, and a peer that \
                 accepts and never answers is cut by the 30 s read-idle bound instead \
                 (`crates/bridges/alpaca/src/execution/user_data_tests.rs`'s \
                 `the_sse_agent_gives_up_on_a_host_that_never_answers`), not the dial window; `silent_peer` speaks \
                 for a WebSocket handshake",
            ),
            _ => None,
        }
    }
}
