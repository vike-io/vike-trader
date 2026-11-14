//! LIVE demo MEASUREMENT (2026-10-26): what does ONE UNIT OF VOLUME mean for a cTrader CFD (a metal, an index, an
//! energy, a crypto CFD), against what the engine assumes for a forex pair (`qty = volume / 100`, notional
//! `qty x price`, realized PnL `qty x price move`, all in the quote currency)? The bridge refused every CFD by
//! name ("what one unit of volume means for a CFD has not been measured"); this file is the measurement that
//! lifts that refusal, and `crates/bridges/ctrader/CLAUDE.md`'s "A CFD's unit" quotes its numbers.
//!
//! Five tests, run ONE at a time (they share the demo account, so `--test-threads=1`) on a the CI box lane, where the
//! credential store is (`docs/ops/live-smoke-lane.md`). The symbols and options are POSITIONAL arguments after the
//! test filter (an environment read would be a settings row; see `positional`):
//!
//!     MSYS_NO_PATHCONV=1 LANE=<pinned lane> just the CI box <branch> \
//!         env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!         cargo test -p vike-ctrader --test ctrader_cfd_units_smoke -- --ignored --nocapture \
//!         --test-threads=1 cfd_round_trip XAUUSD US500 sell steps=2 wait=150
//!
//! - `pending_order_ids_read_smoke` (READ-ONLY: no order, nothing changed): the `clientOrderId`, `label` and `comment`
//!   the venue states for each pending order. It is here for the raw client, and has nothing to do with a CFD's unit.
//! - `hand_stop_left_alone_smoke` (PLACES REAL DEMO ORDERS, EURUSD; also here for the raw client): two far stops bound to a
//!   position the way a platform sends them, a real bridge exec start that must not adopt them, and a close the bridge must
//!   not answer by cancelling them. It ends with this run's orders gone and touches nothing else on the account. Name it
//!   after `--test-threads=1`: the bare command runs this file's other `#[ignore]`d tests too.
//! - `ended_session_read_smoke` (READ-ONLY: no order; it logs its OWN socket's account session out): what the venue answers
//!   on an ended session. Here for the raw client too. One `CTSESSION_` line per frame.
//! - `cfd_classes_read_smoke` (READ-ONLY: no order, nothing changed): every ENABLED symbol by the broker's asset
//!   class (counts, the volume-grid shapes of the class with their symbol names, up to three examples with the
//!   whole facts row the bridge reads and the rest of `ProtoOASymbol` it does not), and, for those examples, the
//!   venue's own `ProtoOAExpectedMarginReq` answer at the minimum volume and at one lot beside the live quote.
//!   Symbol names after the filter replace the examples with those names.
//! - `cfd_round_trip_smoke` (PLACES REAL DEMO ORDERS): for each symbol named after the filter (required), a MARKET
//!   BUY of the symbol's minimum volume (times `steps=N`, default 1; the word `sell` opens a short instead;
//!   `wait=N` widens the wait for a thin feed's two quotes to N seconds), the position and its margin read back, the
//!   venue's own unrealized PnL polled beside the live bid, then a CLOSE BY POSITION ID. It prints one `CTCFD_`
//!   line per fact and the IMPLIED MULTIPLIER `grossProfit / ((exit - entry) x filledVolume / 100)`: 1.0 means
//!   "volume / 100 is the quantity the PnL is paid on", as for a forex pair. It ends FLAT (a clean-up on a fresh
//!   session cancels every order and closes every position by id) and fails when it cannot.
//! - `cfd_bridge_round_trip_smoke` (PLACES REAL DEMO ORDERS): the same round trip through the BRIDGE
//!   (`live_runtime_for_account`, started on EURUSD, each named symbol traded as a symbol the engine was
//!   not started on): a minimum-quantity market BUY (the first symbol also an off-grid 1.4 minimums, which the
//!   bridge must snap to one) and a `reduce_only` market SELL that the bridge turns into a close by position id.
//!
//! ⚠ DEMO ONLY: the host is asserted to be the demo host and the account must be a non-live one of the token.
//! The round trip refuses (touching nothing) unless the account is FLAT at the start, a symbol the broker does not
//! enable, and a market that is not ticking. If a run is ever interrupted, `ctrader_close_all_smoke` is the
//! repo's remedy (it closes EURUSD only: close another symbol's leftover by hand).
//!
//! ⚠ The running node's own sockets see these events: while a probe position exists it shows a
//! `PositionOnlyExternal` recon row; it clears at the next pass once the book is flat.
//!
//! A RAW wire client on purpose (its own TLS socket and the crate's public `framing` / `proto` only), so every
//! number is what the venue said, with no bridge logic between. Credentials: the DEFAULT demo account's through
//! `vike_secrets::smoke_directory` and `CtraderConfig::from_credentials` (no rotation home: nothing here refreshes
//! or writes the grant). Nothing secret is ever printed. Absent creds -> the tests SKIP.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use prost::Message;
use rustls::{ClientConnection, StreamOwned};
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::instrument::symbols::{SymbolLists, SymbolMap};
use vike_ctrader::test_support::{NoopSink, demo_endpoints};
use vike_ctrader::transport::connection::{
    ConnectionConfig, connect_and_auth_execution, fetch_symbol_lists,
};
use vike_ctrader::transport::framing::{FrameReader, encode};
use vike_ctrader::transport::proto::*;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// Spot prices on the wire are integers in 1/100000 for every symbol (`event_mapper::RELATIVE_PRICE_SCALE`); the
/// round trip prints the scaled quote beside the fill so a class on another scale shows at once.
const SCALE: f64 = 100_000.0;
/// `ProtoOAExpectedMarginReq` / `Res`: the venue's own margin for a volume, no order needed (not in the
/// bridge's `pt` table, which sends neither).
const PT_EXPECTED_MARGIN_REQ: u32 = 2139;
const PT_EXPECTED_MARGIN_RES: u32 = 2140;

type Tls = StreamOwned<ClientConnection, TcpStream>;

fn utc() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock");
    let s = d.as_secs() % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, (s / 60) % 60, s % 60, d.subsec_millis())
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_millis() as i64
}

fn money(raw: i64, digits: u32) -> f64 {
    raw as f64 / 10f64.powi(digits as i32)
}

fn body(m: &ProtoMessage) -> &[u8] {
    m.payload.as_deref().unwrap_or(&[])
}

fn is_open(p: &ProtoOaPosition) -> bool {
    p.position_status == ProtoOaPositionStatus::PositionStatusOpen as i32
}

/// The venue's refusal in `frames`, if any: an `ERROR_RES`, an `ORDER_ERROR_EVENT`, or a rejected execution.
fn failure_of(frames: &[ProtoMessage]) -> Option<String> {
    frames.iter().find_map(|m| match m.payload_type {
        pt::ERROR_RES => ProtoOaErrorRes::decode(body(m)).ok().map(|e| format!("{e:?}")),
        pt::ORDER_ERROR_EVENT => {
            ProtoOaOrderErrorEvent::decode(body(m)).ok().map(|e| format!("{e:?}"))
        }
        pt::EXECUTION_EVENT => ProtoOaExecutionEvent::decode(body(m))
            .ok()
            .filter(|e| {
                e.execution_type == ProtoOaExecutionType::OrderRejected as i32
                    || e.error_code.is_some()
            })
            .map(|e| format!("{e:?}")),
        _ => None,
    })
}

fn execution_events(frames: &[ProtoMessage]) -> Vec<ProtoOaExecutionEvent> {
    frames
        .iter()
        .filter(|m| m.payload_type == pt::EXECUTION_EVENT)
        .filter_map(|m| ProtoOaExecutionEvent::decode(body(m)).ok())
        .collect()
}

// ================================ the symbol rows ================================

/// One listed symbol: its names, its broker class, whether this account may trade it, and the full entity.
struct Row {
    id: i64,
    name: String,
    class: String,
    base: String,
    quote: String,
    /// `tradingMode` word, or `ENABLED` when none is stated.
    mode: String,
    /// `enabled` is not `false` AND the mode is `ENABLED`: the only symbols a measurement may touch.
    tradable: bool,
    /// The light symbol's description (the broker's words: a dated contract names its month here).
    description: String,
    full: ProtoOaSymbol,
}

fn rows_of(lists: &SymbolLists) -> Vec<Row> {
    let map = SymbolMap::from_lists(lists);
    lists
        .light
        .iter()
        .filter_map(|l| {
            let name = l.symbol_name.clone()?;
            let full = lists.full.iter().find(|f| f.symbol_id == l.symbol_id)?.clone();
            let mode = full
                .trading_mode
                .and_then(|m| ProtoOaTradingMode::try_from(m).ok())
                .map_or_else(|| "ENABLED".to_string(), |m| m.as_str_name().to_string());
            let tradable = l.enabled != Some(false) && mode == "ENABLED";
            Some(Row {
                id: l.symbol_id,
                name,
                class: map
                    .category(l.symbol_id)
                    .and_then(|c| c.asset_class)
                    .unwrap_or("(no class)")
                    .to_string(),
                base: map.base_asset(l.symbol_id).unwrap_or("?").to_string(),
                quote: map.quote_asset(l.symbol_id).unwrap_or("?").to_string(),
                mode,
                tradable,
                description: l.description.clone().unwrap_or_default(),
                full,
            })
        })
        .collect()
}

/// The facts of one row, one greppable line: everything the bridge reads and the fields it does not.
fn facts_line(r: &Row) -> String {
    let f = &r.full;
    format!(
        "CTCFD_FACTS sym={} id={} class=\"{}\" base={} quote={} digits={} pipPosition={} lotSize={:?} minVolume={:?} \
         stepVolume={:?} maxVolume={:?} measurementUnits={:?} shortSelling={:?} swapType={:?} swapLong={:?} \
         swapShort={:?} commissionType={:?} preciseCommission={:?} minCommission={:?} minCommissionAsset={:?} \
         leverageId={:?} maxExposure={:?} scheduleZone={:?} slDistance={:?} distanceIn={:?} tradingMode={}",
        r.name,
        r.id,
        r.class,
        r.base,
        r.quote,
        f.digits,
        f.pip_position,
        f.lot_size,
        f.min_volume,
        f.step_volume,
        f.max_volume,
        f.measurement_units,
        f.enable_short_selling,
        f.swap_calculation_type,
        f.swap_long,
        f.swap_short,
        f.commission_type,
        f.precise_trading_commission_rate,
        f.precise_min_commission,
        f.min_commission_asset,
        f.leverage_id,
        f.max_exposure,
        f.schedule_time_zone,
        f.sl_distance,
        f.distance_set_in,
        r.mode
    )
}

fn config() -> Option<CtraderConfig> {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let config_value = CtraderConfig::from_credentials(&demo_endpoints(), &credentials);
    if config_value.is_none() {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
    }
    config_value
}

/// The smoke's parameters are POSITIONAL test arguments, not environment variables (every environment read
/// is a settings row, and a smoke's knobs are not settings): the libtest filters after `--`, which only select
/// tests by substring, so `cfd_round_trip XAUUSD US500 sell steps=2 wait=150` runs `cfd_round_trip_smoke`
/// on those two names. A SYMBOL is a word starting with a capital letter or a digit; `sell`, `steps=N` and
/// `wait=N` are the options.
fn positional() -> Vec<String> {
    std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect()
}

fn wanted_names() -> Vec<String> {
    positional()
        .into_iter()
        .filter(|a| a.chars().next().is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
        .filter(|a| !a.contains('='))
        .collect()
}

/// The value of an `key=N` option among the positional arguments.
fn option_number(key: &str) -> Option<u64> {
    positional().iter().find_map(|a| a.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

// ================================ the raw session ================================

#[derive(Clone, Copy, Default)]
struct Spot {
    bid: i64,
    ask: i64,
    ts: i64,
    seen: u32,
}

struct Wire {
    rd: FrameReader<Tls>,
    ctid: i64,
    money_digits: u32,
    last_beat: Instant,
    spots: HashMap<i64, Spot>,
    seq: u32,
}

fn open_tls(host: &str, port: u16) -> FrameReader<Tls> {
    let tcp = TcpStream::connect((host, port)).expect("tcp connect to the demo host");
    tcp.set_read_timeout(Some(Duration::from_millis(500))).expect("read timeout");
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("tls versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_owned()).expect("server name");
    let conn = ClientConnection::new(Arc::new(config), name).expect("tls client");
    FrameReader::new(StreamOwned::new(conn, tcp))
}

impl Wire {
    /// Connect and authorize; refuses anything but a non-live account on the demo host.
    fn connect(config: &CtraderConfig) -> Wire {
        assert_eq!(
            config.host,
            demo_endpoints().host,
            "REFUSING: this smoke dials the demo host only"
        );
        let mut w = Wire {
            rd: open_tls(&config.host, config.port),
            ctid: 0,
            money_digits: 2,
            last_beat: Instant::now(),
            spots: HashMap::new(),
            seq: 0,
        };
        let app = ProtoOaApplicationAuthReq {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.expose().clone(),
            ..Default::default()
        };
        w.send(pt::APPLICATION_AUTH_REQ, &app, "app");
        w.await_pt(pt::APPLICATION_AUTH_RES, "app", 15).expect("application auth");

        let list_req = ProtoOaGetAccountListByAccessTokenReq {
            access_token: config.access_token.expose().clone(),
            ..Default::default()
        };
        w.send(pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ, &list_req, "accts");
        let m =
            w.await_pt(pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES, "accts", 15).expect("account list");
        let list = ProtoOaGetAccountListByAccessTokenRes::decode(body(&m)).expect("decode list");
        let ctid = match config.account_id {
            Some(id) => id,
            None => {
                list.ctid_trader_account
                    .iter()
                    .find(|a| !a.is_live.unwrap_or(false))
                    .expect("a non-live account in the token's list")
                    .ctid_trader_account_id as i64
            }
        };
        let account = list
            .ctid_trader_account
            .iter()
            .find(|a| a.ctid_trader_account_id as i64 == ctid)
            .expect("the account is in the token's list");
        assert!(!account.is_live.unwrap_or(false), "REFUSING: that account is LIVE");
        w.ctid = ctid;

        let auth = ProtoOaAccountAuthReq {
            ctid_trader_account_id: ctid,
            access_token: config.access_token.expose().clone(),
            ..Default::default()
        };
        w.send(pt::ACCOUNT_AUTH_REQ, &auth, "aa");
        w.await_pt(pt::ACCOUNT_AUTH_RES, "aa", 15).expect("account auth");
        let t = w.trader();
        w.money_digits = t.money_digits.unwrap_or(2);
        let kind = t.account_type.map(ProtoOaAccountType::try_from);
        println!(
            "[{}] CTCFD_SESSION demo host, non-live account (id withheld); accountType={kind:?} moneyDigits={} \
             depositAssetId={} leverageInCents={:?} maxLeverage={:?}",
            utc(),
            w.money_digits,
            t.deposit_asset_id,
            t.leverage_in_cents,
            t.max_leverage
        );
        w
    }

    fn order_client_id(&mut self, tag: &str) -> String {
        self.seq += 1;
        format!("ctcfd-{tag}-{}-{}", now_ms() % 10_000_000, self.seq)
    }

    fn send<M: Message>(&mut self, payload_type: u32, msg: &M, id: &str) {
        let frame = encode(payload_type, &msg.encode_to_vec(), id);
        let stream = self.rd.get_mut();
        stream.write_all(&frame).expect("socket write");
        stream.flush().expect("socket flush");
    }

    /// Read for up to `wait`; spots and heartbeats are absorbed, the first other frame is returned.
    fn read_one(&mut self, wait: Duration) -> Option<ProtoMessage> {
        let end = Instant::now() + wait;
        loop {
            if self.last_beat.elapsed() >= Duration::from_secs(10) {
                self.send(pt::HEARTBEAT_EVENT, &ProtoHeartbeatEvent::default(), "");
                self.last_beat = Instant::now();
            }
            let started = Instant::now();
            match self.rd.next_frame() {
                Ok(Some(m)) => match m.payload_type {
                    pt::SPOT_EVENT => {
                        if let Ok(s) = ProtoOaSpotEvent::decode(body(&m)) {
                            let e = self.spots.entry(s.symbol_id).or_default();
                            if let Some(b) = s.bid {
                                e.bid = b as i64;
                            }
                            if let Some(a) = s.ask {
                                e.ask = a as i64;
                            }
                            e.ts = s.timestamp.unwrap_or(0);
                            e.seen += 1;
                        }
                    }
                    pt::HEARTBEAT_EVENT => {}
                    _ => return Some(m),
                },
                Ok(None) => {
                    if started.elapsed() < Duration::from_millis(50) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Err(e) => panic!("socket read failed: {e}"),
            }
            if Instant::now() >= end {
                return None;
            }
        }
    }

    fn pump(&mut self, wait: Duration) {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            let left = end.saturating_duration_since(Instant::now());
            let _ = self.read_one(left);
        }
    }

    /// Wait for the response of payload type `want` to request `id` (an `ERROR_RES` echoing `id` is its error).
    fn await_pt(&mut self, want: u32, id: &str, secs: u64) -> Result<ProtoMessage, String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            let Some(m) = self.read_one(Duration::from_millis(500)) else { continue };
            if m.payload_type == want {
                return Ok(m);
            }
            if m.payload_type == pt::ERROR_RES && m.client_msg_id.as_deref() == Some(id) {
                return Err(format!("{:?}", ProtoOaErrorRes::decode(body(&m))));
            }
        }
        Err(format!("timeout waiting for payloadType {want}"))
    }

    /// The frames that follow a request: wait up to `first` for the first, then until `quiet` of silence.
    fn outcome(&mut self, first: Duration, quiet: Duration) -> Vec<ProtoMessage> {
        let mut out = Vec::new();
        let Some(m) = self.read_one(first) else { return out };
        out.push(m);
        while let Some(m) = self.read_one(quiet) {
            out.push(m);
        }
        out
    }

    fn trader(&mut self) -> ProtoOaTrader {
        let req = ProtoOaTraderReq { ctid_trader_account_id: self.ctid, ..Default::default() };
        self.send(pt::TRADER_REQ, &req, "tr");
        let m = self.await_pt(pt::TRADER_RES, "tr", 15).expect("trader");
        ProtoOaTraderRes::decode(body(&m)).expect("decode trader").trader
    }

    /// The account balance, descaled.
    fn balance(&mut self) -> f64 {
        let t = self.trader();
        money(t.balance, t.money_digits.unwrap_or(self.money_digits))
    }

    fn subscribe(&mut self, ids: &[i64]) {
        let req = ProtoOaSubscribeSpotsReq {
            ctid_trader_account_id: self.ctid,
            symbol_id: ids.to_vec(),
            subscribe_to_spot_timestamp: Some(true),
            ..Default::default()
        };
        self.send(pt::SUBSCRIBE_SPOTS_REQ, &req, "spots");
        self.pump(Duration::from_secs(2));
    }

    /// A quote that is TICKING: two spot events within `secs`, the last stamped within 30 s of now. A closed
    /// market sends the last price once and then nothing; this refuses that.
    fn live_quote(&mut self, id: i64, secs: u64) -> Option<Spot> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let first = self.spots.get(&id).map_or(0, |s| s.seen);
        while Instant::now() < deadline {
            self.pump(Duration::from_millis(500));
            if let Some(s) = self.spots.get(&id).copied()
                && s.bid > 0
                && s.ask > 0
                && s.seen >= first + 2
            {
                return Some(s);
            }
        }
        None
    }

    fn quote(&mut self, id: i64) -> Spot {
        self.pump(Duration::from_millis(300));
        self.spots.get(&id).copied().unwrap_or_default()
    }

    /// The venue's own expected margin of `volumes` (centi-units) on symbol `id`: `(volume, buy, sell)` descaled.
    fn expected_margin(&mut self, id: i64, volumes: &[i64]) -> Vec<(i64, f64, f64)> {
        let req = ProtoOaExpectedMarginReq {
            ctid_trader_account_id: self.ctid,
            symbol_id: id,
            volume: volumes.to_vec(),
            ..Default::default()
        };
        self.send(PT_EXPECTED_MARGIN_REQ, &req, "em");
        match self.await_pt(PT_EXPECTED_MARGIN_RES, "em", 15) {
            Ok(m) => {
                let res = ProtoOaExpectedMarginRes::decode(body(&m)).expect("decode margin");
                let digits = res.money_digits.unwrap_or(self.money_digits);
                res.margin
                    .iter()
                    .map(|e| (e.volume, money(e.buy_margin, digits), money(e.sell_margin, digits)))
                    .collect()
            }
            Err(why) => {
                println!("CTCFD_MARGIN_ERR id={id} {why}");
                Vec::new()
            }
        }
    }

    fn reconcile_try(&mut self) -> Result<(Vec<ProtoOaPosition>, Vec<ProtoOaOrder>), String> {
        let req = ProtoOaReconcileReq { ctid_trader_account_id: self.ctid, ..Default::default() };
        self.send(pt::RECONCILE_REQ, &req, "rec");
        let m = self.await_pt(pt::RECONCILE_RES, "rec", 20)?;
        let res = ProtoOaReconcileRes::decode(body(&m)).map_err(|e| e.to_string())?;
        Ok((res.position, res.order))
    }

    /// The venue's own unrealized PnL of position `pid`: `(gross, net)` descaled.
    fn unrealized(&mut self, pid: i64) -> Option<(f64, f64)> {
        let req = ProtoOaGetPositionUnrealizedPnLReq {
            ctid_trader_account_id: self.ctid,
            ..Default::default()
        };
        self.send(pt::GET_POSITION_UNREALIZED_PNL_REQ, &req, "upnl");
        let m = self.await_pt(pt::GET_POSITION_UNREALIZED_PNL_RES, "upnl", 15).ok()?;
        let res = ProtoOaGetPositionUnrealizedPnLRes::decode(body(&m)).ok()?;
        let p = res.position_unrealized_pn_l.iter().find(|p| p.position_id == pid)?;
        Some((
            money(p.gross_unrealized_pn_l, res.money_digits),
            money(p.net_unrealized_pn_l, res.money_digits),
        ))
    }

    fn market(&mut self, id: i64, side: ProtoOaTradeSide, volume: i64) -> Vec<ProtoMessage> {
        let order_client_id = self.order_client_id("mkt");
        let req = ProtoOaNewOrderReq {
            ctid_trader_account_id: self.ctid,
            symbol_id: id,
            order_type: ProtoOaOrderType::Market as i32,
            trade_side: side as i32,
            volume,
            client_order_id: Some(order_client_id.clone()),
            label: Some(order_client_id.clone()),
            ..Default::default()
        };
        self.send(pt::NEW_ORDER_REQ, &req, &order_client_id);
        self.outcome(Duration::from_secs(10), Duration::from_millis(1500))
    }

    fn close(&mut self, pid: i64, volume: i64) -> Vec<ProtoMessage> {
        let req = ProtoOaClosePositionReq {
            ctid_trader_account_id: self.ctid,
            position_id: pid,
            volume,
            ..Default::default()
        };
        let id = self.order_client_id("close");
        self.send(pt::CLOSE_POSITION_REQ, &req, &id);
        self.outcome(Duration::from_secs(8), Duration::from_millis(1500))
    }

    fn cancel(&mut self, order_id: i64) {
        let req = ProtoOaCancelOrderReq {
            ctid_trader_account_id: self.ctid,
            order_id,
            ..Default::default()
        };
        let id = self.order_client_id("cxl");
        self.send(pt::CANCEL_ORDER_REQ, &req, &id);
        let _ = self.outcome(Duration::from_secs(6), Duration::from_millis(1000));
    }

    /// Cancel every resting order, close every open position by id, read the book back; true iff flat.
    /// Never panics (it is the clean-up).
    fn flatten(&mut self) -> bool {
        for round in 0..4 {
            let Ok((positions, orders)) = self.reconcile_try() else { return false };
            let open: Vec<&ProtoOaPosition> = positions.iter().filter(|p| is_open(p)).collect();
            if open.is_empty() && orders.is_empty() {
                println!(
                    "[{}] CTCFD_CLEANUP flat (round {round}): 0 open positions, 0 resting orders",
                    utc()
                );
                return true;
            }
            println!(
                "[{}] CTCFD_CLEANUP round {round}: {} open position(s), {} resting order(s)",
                utc(),
                open.len(),
                orders.len()
            );
            for o in &orders {
                self.cancel(o.order_id);
            }
            for p in &open {
                self.close(p.position_id, p.trade_data.volume);
            }
        }
        match self.reconcile_try() {
            Ok((positions, orders)) => orders.is_empty() && !positions.iter().any(is_open),
            Err(_) => false,
        }
    }
}

/// Flattens the account on a FRESH session when it drops after being armed (a run that panicked).
struct Guard {
    config: CtraderConfig,
    armed: Cell<bool>,
}

impl Guard {
    fn new(config: &CtraderConfig) -> Guard {
        Guard { config: config.clone(), armed: Cell::new(false) }
    }

    fn finish(self) -> bool {
        self.armed.set(false);
        Wire::connect(&self.config).flatten()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.armed.get() {
            let flat = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Wire::connect(&self.config).flatten()
            }));
            println!("[{}] CTCFD_GUARD clean-up after a failed run: flat={flat:?}", utc());
        }
    }
}

// ================================ the ids of a pending order, read-only ============================

/// READ-ONLY, nothing placed: what `ProtoOAReconcileRes` says of each PENDING order in the three fields that could name
/// it. Run with a node order resting it answered the order client id in both `clientOrderId` and `label` (2026-11-06), which is what
/// `transport/connection/reconcile.rs`'s `node_placed_order_client_id` relies on (`crates/bridges/ctrader/CLAUDE.md`'s "A bound exit that outlived its
/// position is flagged at connect, never cancelled").
#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn pending_order_ids_read_smoke() {
    let Some(config_value) = config() else { return };
    let mut w = Wire::connect(&config_value);
    let req = ProtoOaReconcileReq { ctid_trader_account_id: w.ctid, ..Default::default() };
    w.send(pt::RECONCILE_REQ, &req, "rc");
    let m = w.await_pt(pt::RECONCILE_RES, "rc", 15).expect("the reconcile answer");
    let res = ProtoOaReconcileRes::decode(body(&m)).expect("decode the reconcile answer");
    for o in &res.order {
        println!("CTCFD_PENDING {}", pending_line(o));
    }
    println!("CTCFD_PENDING_END rows={}", res.order.len());
}

/// One pending order in the fields that could name it.
fn pending_line(o: &ProtoOaOrder) -> String {
    format!(
        "order={} type={} side={} positionId={:?} clientOrderId={:?} label={:?} comment={:?}",
        o.order_id,
        o.order_type,
        o.trade_data.trade_side,
        o.position_id,
        o.client_order_id,
        o.trade_data.label,
        o.trade_data.comment
    )
}

// ================================ what an ended session answers, read-only ==========================

/// One `CTSESSION_<step>` line per frame the venue sent after a request: its payload type, and for a refusal its words.
fn print_answer(step: &str, frames: &[ProtoMessage]) {
    for m in frames {
        match m.payload_type {
            pt::ERROR_RES => println!("CTSESSION_{step} {:?}", ProtoOaErrorRes::decode(body(m))),
            other => println!("CTSESSION_{step} payloadType={other}"),
        }
    }
    println!("CTSESSION_{step}_END frames={}", frames.len());
}

/// READ-ONLY, nothing placed: **what the venue answers on a socket whose account session has ended.** It ends one by
/// `ProtoOAAccountLogoutReq` on its OWN socket, then sends the actor's probe (`ProtoOATraderReq`) and a reconcile read. Its
/// run of 2026-11-06 is `crates/bridges/ctrader/CLAUDE.md`'s "A lapsed account session"; the probe's rule rests on it.
#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn ended_session_read_smoke() {
    let Some(config_value) = config() else { return };
    let mut w = Wire::connect(&config_value);
    let trader = ProtoOaTraderReq { ctid_trader_account_id: w.ctid, ..Default::default() };
    let read = ProtoOaReconcileReq { ctid_trader_account_id: w.ctid, ..Default::default() };
    let logout = ProtoOaAccountLogoutReq { ctid_trader_account_id: w.ctid, ..Default::default() };
    let (first, quiet) = (Duration::from_secs(10), Duration::from_secs(2));

    w.send(pt::TRADER_REQ, &trader, "");
    print_answer("PROBE_BEFORE", &w.outcome(first, quiet));
    w.send(ProtoOaPayloadType::ProtoOaAccountLogoutReq as u32, &logout, "");
    print_answer("LOGOUT", &w.outcome(first, quiet));
    w.send(pt::TRADER_REQ, &trader, "");
    print_answer("PROBE_AFTER", &w.outcome(first, quiet));
    w.send(pt::RECONCILE_REQ, &read, "");
    print_answer("RECONCILE_AFTER", &w.outcome(first, quiet));
}

// ================================ test 1: the classes, read-only ================================

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn cfd_classes_read_smoke() {
    let Some(config_value) = config() else { return };
    let lists = fetch_symbol_lists(config_value.to_connection_config())
        .expect("handshake + symbol lists against the real cTrader demo endpoint");
    let rows = rows_of(&lists);
    assert!(!rows.is_empty(), "the demo account lists no symbol");
    let names = wanted_names();

    let mut by_class: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in &rows {
        by_class.entry(r.class.as_str()).or_default().push(r);
    }
    println!("CTCFD_CLASSES_BEGIN");
    println!("listed={} tradable={}", rows.len(), rows.iter().filter(|r| r.tradable).count());
    let mut examples: Vec<&Row> = Vec::new();
    for (class, members) in &by_class {
        let mut tradable: Vec<&Row> = members.iter().copied().filter(|r| r.tradable).collect();
        tradable.sort_by(|a, b| a.name.cmp(&b.name));
        println!(
            "CTCFD_CLASS class=\"{class}\" listed={} tradable={}",
            members.len(),
            tradable.len()
        );
        // The volume-grid shapes inside the class, with their names: a class is one rule only if one shape.
        let mut shapes: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for r in &tradable {
            let f = &r.full;
            let shape = format!(
                "lot={:?} min={:?} step={:?} max={:?} digits={} pip={} quote={} units={:?}",
                f.lot_size,
                f.min_volume,
                f.step_volume,
                f.max_volume,
                f.digits,
                f.pip_position,
                r.quote,
                f.measurement_units
            );
            shapes.entry(shape).or_default().push(&r.name);
        }
        for (shape, who) in &shapes {
            let shown: Vec<&&str> = who.iter().take(6).collect();
            println!("  CTCFD_SHAPE {shape} :: {} symbol(s) {shown:?}", who.len());
        }
        for r in tradable.iter().take(3) {
            println!("  {}", facts_line(r));
            examples.push(*r);
        }
        if !class.to_ascii_lowercase().contains("forex") {
            for r in &tradable {
                println!(
                    "  CTCFD_DESC sym={} quote={} description=\"{}\"",
                    r.name, r.quote, r.description
                );
            }
        }
    }
    if !names.is_empty() {
        examples = names
            .iter()
            .map(|n| rows.iter().find(|r| &r.name == n).unwrap_or_else(|| panic!("no symbol {n}")))
            .collect();
    }

    // The venue's own margin at the minimum volume and at one lot, beside the live quote.
    let mut w = Wire::connect(&config_value);
    let ids: Vec<i64> = examples.iter().map(|r| r.id).collect();
    w.subscribe(&ids);
    for r in &examples {
        let f = &r.full;
        let (min, lot) = (f.min_volume.unwrap_or(0), f.lot_size.unwrap_or(0));
        let margins = w.expected_margin(r.id, &[min, lot]);
        let q = w.quote(r.id);
        let mid = (q.bid + q.ask) as f64 / 2.0 / SCALE;
        let units_min = min as f64 / 100.0;
        let notional_min = units_min * mid;
        println!(
            "CTCFD_MARGIN sym={} class=\"{}\" quote={} bid={} ask={} minUnits={units_min} notionalAtMin(quote ccy)={notional_min:.4} \
             marginAtMin(buy,sell)={:?} marginAtLot(buy,sell)={:?}",
            r.name,
            r.class,
            r.quote,
            q.bid as f64 / SCALE,
            q.ask as f64 / SCALE,
            margins.iter().find(|m| m.0 == min).map(|m| (m.1, m.2)),
            margins.iter().find(|m| m.0 == lot).map(|m| (m.1, m.2)),
        );
    }
    println!("CTCFD_CLASSES_END");
}

// ================================ test 2: the round trip ================================

#[test]
#[ignore = "network + REAL cTrader demo endpoint + places REAL demo orders — run manually (see module doc)"]
fn cfd_round_trip_smoke() {
    let Some(config_value) = config() else { return };
    let names = wanted_names();
    assert!(
        !names.is_empty(),
        "name the symbols after the test filter (XAUUSD US500 ...): nothing is traded unnamed"
    );
    let steps = option_number("steps").unwrap_or(1) as i64;

    let lists = fetch_symbol_lists(config_value.to_connection_config())
        .expect("handshake + symbol lists against the real cTrader demo endpoint");
    let rows = rows_of(&lists);
    let guard = Guard::new(&config_value);
    let mut w = Wire::connect(&config_value);
    let (positions, orders) = w.reconcile_try().expect("start-of-run state");
    assert!(
        !positions.iter().any(is_open) && orders.is_empty(),
        "ABORT: the account is not flat at the start; nothing was touched"
    );
    guard.armed.set(true);

    for name in &names {
        let row = rows
            .iter()
            .find(|r| &r.name == name)
            .unwrap_or_else(|| panic!("ABORT: {name} is not in the account's symbol list"));
        assert!(row.tradable, "ABORT: {name} is not enabled for trading ({})", row.mode);
        println!("{}", facts_line(row));
        round_trip(&mut w, row, steps);
    }
    drop(w);
    assert!(
        guard.finish(),
        "the account is NOT flat after the run: run ctrader_close_all_smoke / close by hand"
    );
}

fn round_trip(w: &mut Wire, row: &Row, steps: i64) {
    let min = row.full.min_volume.expect("the symbol states a minimum volume");
    let volume = min * steps;
    let id = row.id;
    w.subscribe(&[id]);
    // A thin feed (an alt-coin CFD, natural gas) ticks every minute or so: `wait=150` widens the wait.
    let secs = option_number("wait").unwrap_or(40);
    let Some(q0) = w.live_quote(id, secs) else {
        panic!("ABORT: no ticking quote for {} within {secs} s: the market is closed", row.name);
    };
    println!(
        "[{}] CTCFD_QUOTE sym={} bid={} ask={} (wire {}/{}, scale {SCALE})",
        utc(),
        row.name,
        q0.bid as f64 / SCALE,
        q0.ask as f64 / SCALE,
        q0.bid,
        q0.ask
    );
    let margins = w.expected_margin(id, &[volume]);
    println!("CTCFD_EXPECTED_MARGIN sym={} volume={volume} buy/sell={:?}", row.name, margins);
    let balance0 = w.balance();

    // ---- entry: a market BUY of the minimum volume (the argument `sell`: a SELL, a short).
    let sell = positional().iter().any(|a| a.eq_ignore_ascii_case("sell"));
    let dir = if sell { -1.0 } else { 1.0 };
    let side = if sell { ProtoOaTradeSide::Sell } else { ProtoOaTradeSide::Buy };
    let frames = w.market(id, side, volume);
    if let Some(why) = failure_of(&frames) {
        println!("CTCFD_ENTRY_REFUSED sym={} {why}", row.name);
        panic!("the venue refused the entry: {why}");
    }
    let events = execution_events(&frames);
    let fill = events
        .iter()
        .find(|e| e.execution_type == ProtoOaExecutionType::OrderFilled as i32)
        .unwrap_or_else(|| {
            panic!("no fill event for the entry of {}: {} frame(s)", row.name, frames.len())
        });
    let deal = fill.deal.as_ref().expect("the fill carries a deal");
    let entry_px = deal.execution_price.expect("the fill states a price");
    println!(
        "[{}] CTCFD_ENTRY sym={} side={side:?} volume={volume} filledVolume={} execPrice={entry_px} commissionRaw={:?} \
         marginRate={:?} baseToUsd={:?} dealMoneyDigits={:?} quoteAtOrder bid/ask={}/{}",
        utc(),
        row.name,
        deal.filled_volume,
        deal.commission,
        deal.margin_rate,
        deal.base_to_usd_conversion_rate,
        deal.money_digits,
        q0.bid as f64 / SCALE,
        q0.ask as f64 / SCALE
    );

    // ---- the position and its margin.
    let (positions, _) = w.reconcile_try().expect("read the position");
    let pos = positions
        .iter()
        .find(|p| is_open(p) && p.trade_data.symbol_id == id)
        .unwrap_or_else(|| panic!("no open {} position after the fill", row.name));
    let pid = pos.position_id;
    let pos_digits = pos.money_digits.unwrap_or(w.money_digits);
    println!(
        "CTCFD_POSITION sym={} positionId={pid} volume={} entryPrice={:?} usedMargin={:?} (descaled {:?}) marginRate={:?} \
         measurementUnits={:?} swapRaw={} commissionRaw={:?} moneyDigits={pos_digits}",
        row.name,
        pos.trade_data.volume,
        pos.price,
        pos.used_margin,
        pos.used_margin.map(|m| money(m as i64, pos_digits)),
        pos.margin_rate,
        pos.trade_data.measurement_units,
        pos.swap,
        pos.commission
    );
    let entry = pos.price.unwrap_or(entry_px);
    let units = pos.trade_data.volume as f64 / 100.0;

    // ---- the venue's own unrealized PnL beside the live bid: gross / ((bid - entry) x units).
    for poll in 0..4 {
        w.pump(Duration::from_secs(2));
        let q = w.quote(id);
        // A long is marked at the bid, a short at the ask; `dir` is the side's sign.
        let bid = q.bid as f64 / SCALE;
        let mark = if sell { q.ask as f64 / SCALE } else { bid };
        let Some((gross, net)) = w.unrealized(pid) else {
            println!("CTCFD_UPNL sym={} poll={poll} not answered", row.name);
            continue;
        };
        let denominator = dir * (mark - entry) * units;
        let implied = if denominator.abs() > 1e-12 { gross / denominator } else { f64::NAN };
        println!(
            "CTCFD_UPNL sym={} poll={poll} bid={bid} ask={} entry={entry} units={units} grossUnrealized={gross} \
             netUnrealized={net} impliedMultiplier(gross/(dir*(mark-entry)*units))={implied:.6}",
            row.name,
            q.ask as f64 / SCALE
        );
    }

    // ---- the close, by position id.
    let frames = w.close(pid, pos.trade_data.volume);
    if let Some(why) = failure_of(&frames) {
        println!("CTCFD_CLOSE_REFUSED sym={} {why}", row.name);
        panic!("the venue refused the close: {why}");
    }
    let closing = execution_events(&frames)
        .into_iter()
        .find(|e| {
            e.execution_type == ProtoOaExecutionType::OrderFilled as i32
                && e.deal.as_ref().is_some_and(|d| d.close_position_detail.is_some())
        })
        .unwrap_or_else(|| panic!("no closing fill for {}: {} frame(s)", row.name, frames.len()));
    let d = closing.deal.expect("closing deal");
    let det = d.close_position_detail.expect("closing detail");
    let digits = det.money_digits.or(d.money_digits).unwrap_or(w.money_digits);
    let exit_price = d.execution_price.expect("the closing fill states a price");
    let closed_units = det.closed_volume.unwrap_or(d.filled_volume) as f64 / 100.0;
    let gross = money(det.gross_profit, digits);
    let mv = dir * (exit_price - det.entry_price) * closed_units;
    let implied = if mv.abs() > 1e-12 { gross / mv } else { f64::NAN };
    println!(
        "[{}] CTCFD_CLOSE sym={} positionId={pid} filledVolume={} closedVolume={:?} exitPrice={exit_price} detailEntryPrice={} \
         grossProfit={gross} swap={} commission={} balanceAfter={} quoteToDepositRate={:?} pnlConversionFee={:?} \
         moneyDigits={digits}",
        utc(),
        row.name,
        d.filled_volume,
        det.closed_volume,
        det.entry_price,
        money(det.swap, digits),
        money(det.commission, digits),
        money(det.balance, digits),
        det.quote_to_deposit_conversion_rate,
        det.pnl_conversion_fee.map(|f| money(f, digits)),
    );
    println!(
        "CTCFD_UNITS sym={} class=\"{}\" quote={} units={closed_units} entry={} exit={exit_price} priceMove={} \
         moveTimesUnits={mv} grossProfit(deposit ccy)={gross} impliedMultiplier(gross/(move*units))={implied:.6} \
         (1.0 = the PnL is volume/100 x price move in the QUOTE currency; a quote that is not the deposit \
         currency shows the quote->deposit rate instead)",
        row.name,
        row.class,
        row.quote,
        det.entry_price,
        exit_price - det.entry_price
    );
    let balance1 = w.balance();
    println!(
        "CTCFD_BALANCE sym={} before={balance0} after={balance1} diff={:.6} (grossProfit + the closing detail's commission, \
         which is the position's TOTAL, + swap = {:.6}; the opening deal's own commission {:?} is inside it)",
        row.name,
        balance1 - balance0,
        gross + money(det.commission, digits) + money(det.swap, digits),
        deal.commission
    );
    assert!(w.flatten(), "{} left the account NOT flat", row.name);
}

// ================================ test 3: the same round trip through the bridge ================================

/// Drain the ingest lane into `events` (kept for the whole run).
fn drain(
    rx: &mut tokio::sync::mpsc::Receiver<vike_execution::Ingest>,
    events: &mut Vec<vike_model::events::Event>,
) {
    while let Ok(ing) = rx.try_recv() {
        if let vike_execution::Ingest::Event(e) = ing {
            events.push(e);
        }
    }
}

/// Poll until the fill (`OrderFilled`) of `order_client_id` is in `events`; a rejection or `secs` without a fill panics.
fn await_fill(
    rx: &mut tokio::sync::mpsc::Receiver<vike_execution::Ingest>,
    events: &mut Vec<vike_model::events::Event>,
    order_client_id: &str,
    secs: u64,
) -> vike_model::events::FillEvent {
    use vike_model::events::Event;
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        drain(rx, events);
        for e in events.iter() {
            match e {
                Event::OrderFilled(f) if f.order_client_id == order_client_id => {
                    return f.fill.clone();
                }
                Event::OrderRejected(r) if r.order_client_id == order_client_id => {
                    panic!("the venue or the bridge REFUSED {order_client_id}: {}", r.reason)
                }
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "no fill for {order_client_id} within {secs} s");
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// **The bridge itself on a CFD, as a symbol the engine was NOT started on** (the shape the door admits): the exec
/// client is started on EURUSD (`live_runtime_for_account`, the seam production starts it through) and each
/// named symbol is traded through it: a MARKET BUY of the symbol's minimum quantity (and, for the first
/// symbol, an off-grid quantity of 1.4 minimums that the bridge must snap to the grid), then a `reduce_only` MARKET
/// SELL that the bridge turns into a close BY POSITION ID. Prints the fills' quantity, price and commission, and
/// ends with a raw-wire read that the account is flat. The numbers to read: `qty` equals the minimum in UNITS
/// (`volume / 100`), and the realized `move x qty` has the sign and size the raw round trip showed.
#[test]
#[ignore = "network + REAL cTrader demo endpoint + places REAL demo orders — run manually (see module doc)"]
fn cfd_bridge_round_trip_smoke() {
    use vike_ctrader::adapter::live_runtime_for_account;
    use vike_execution::{ExecutionClient, event_channel};
    use vike_model::{HaltAdmit, OrderRequest, OrderType};

    let Some(config_value) = config() else { return };
    let names = wanted_names();
    assert!(
        !names.is_empty(),
        "name the symbols after the test filter (XAUUSD US500 ...): nothing is traded unnamed"
    );
    let lists = fetch_symbol_lists(config_value.to_connection_config())
        .expect("handshake + symbol lists against the real cTrader demo endpoint");
    let rows = rows_of(&lists);
    let guard = Guard::new(&config_value);
    let (positions, orders) =
        Wire::connect(&config_value).reconcile_try().expect("start-of-run state");
    assert!(
        !positions.iter().any(is_open) && orders.is_empty(),
        "ABORT: the account is not flat at the start; nothing was touched"
    );
    guard.armed.set(true);

    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let (tx, mut rx) = event_channel(1024);
    let mut runtime = live_runtime_for_account(
        &AccountLabel::Default,
        &credentials,
        &demo_endpoints(),
        None,
        "EURUSD",
        true,
        &tx,
        HaltAdmit::default(),
        std::path::Path::new("ctrader-cfd-smoke-never-engaged/HALT"),
    )
    .expect("creds present: the exec connect and handshake must succeed");
    let mut events = Vec::new();
    let market = |order_client_id: &str, symbol: &str, side: i32, qty: f64, reduce_only: bool| {
        OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "ctrader".into(),
            symbol: symbol.into(),
            side,
            qty,
            order_type: OrderType::Market,
            reduce_only,
            timestamp: now_ms(),
            ..Default::default()
        }
    };

    for (i, name) in names.iter().enumerate() {
        let row = rows
            .iter()
            .find(|r| &r.name == name)
            .unwrap_or_else(|| panic!("ABORT: {name} is not in the account's symbol list"));
        assert!(row.tradable, "ABORT: {name} is not enabled for trading ({})", row.mode);
        let min = row.full.min_volume.expect("a minimum volume") as f64 / 100.0;
        println!("{}", facts_line(row));
        // The first symbol also takes an off-grid quantity: 1.4 minimums must snap to ONE minimum.
        let quantities = if i == 0 { vec![min, min * 1.4] } else { vec![min] };
        for (j, qty) in quantities.into_iter().enumerate() {
            let tag = format!("{i}{j}");
            let (open, close) = (
                format!("ctcfdb{tag}o{}", now_ms() % 1_000_000),
                format!("ctcfdb{tag}c{}", now_ms() % 1_000_000),
            );
            runtime.client.submit(&market(&open, name, 1, qty, false));
            let entry = await_fill(&mut rx, &mut events, &open, 30);
            println!(
                "CTCFD_BRIDGE_ENTRY sym={name} asked qty={qty} filled qty={} px={} commission={}",
                entry.last_qty, entry.last_price, entry.commission
            );
            assert_eq!(entry.symbol.as_str(), name.as_str());
            assert_eq!(
                entry.last_qty, min,
                "{name}: the bridge sends the grid quantity: one minimum"
            );
            runtime.client.submit(&market(&close, name, -1, entry.last_qty, true));
            let exit = await_fill(&mut rx, &mut events, &close, 30);
            println!(
                "CTCFD_BRIDGE_EXIT sym={name} qty={} px={} commission={} move x qty = {:.6}",
                exit.last_qty,
                exit.last_price,
                exit.commission,
                (exit.last_price - entry.last_price) * entry.last_qty
            );
            assert_eq!(exit.last_qty, entry.last_qty, "{name}: the close is the whole position");
            let (positions, orders) =
                Wire::connect(&config_value).reconcile_try().expect("read the book");
            assert!(
                !positions.iter().any(is_open) && orders.is_empty(),
                "{name}: the account is not flat after the close by position id"
            );
        }
    }
    drop(runtime);
    assert!(guard.finish(), "the account is NOT flat after the run");
}

// ================================ test 5: a stop the node did not place ================================

/// How long after the bridge saw the position close a cancel has to show up. The orphan rule writes it on the actor's next
/// pass and the broker listed such a cancel within the second (`crates/bridges/ctrader/CLAUDE.md`'s "What the broker
/// answered on the hedged demo account, 2026-10-27 and 2026-10-28 (v0.1.96)").
const ORPHAN_RULE_WAIT: Duration = Duration::from_secs(10);

/// Everything THIS run put on the account, cancelled and closed on a FRESH session on every way out of the test.
/// ⚠ Not [`Guard`]: `Wire::flatten` cancels every order and closes every position, and the running node's are not ours.
struct HandGuard {
    config: CtraderConfig,
    /// Every order and position of this run carries a label starting with this.
    tag: String,
    symbol_id: i64,
    position: Cell<Option<i64>>,
    stops: RefCell<Vec<(&'static str, i64)>>,
    armed: Cell<bool>,
}

impl HandGuard {
    fn tagged(&self, label: Option<&str>) -> bool {
        label.is_some_and(|l| l.starts_with(&self.tag))
    }

    fn holds_order(&self, o: &ProtoOaOrder) -> bool {
        self.stops.borrow().iter().any(|(_, id)| *id == o.order_id)
            || self.tagged(o.trade_data.label.as_deref())
    }

    fn holds_position(&self, p: &ProtoOaPosition) -> bool {
        is_open(p)
            && (self.position.get() == Some(p.position_id)
                || self.tagged(p.trade_data.label.as_deref()))
    }

    /// Cancel this run's stops, close its position, read the venue back. `true` when none of it is left and no EURUSD long
    /// is open; whatever is left is named here with its id.
    fn clean(&self) -> bool {
        // A rotation by the node during the run ended the pair `self.cfg` holds, and the store has the new one.
        let config_under_test = config().unwrap_or_else(|| self.config.clone());
        let mut w = Wire::connect(&config_under_test);
        for _ in 0..4 {
            let Ok((positions, orders)) = w.reconcile_try() else { return false };
            let stops: Vec<i64> =
                orders.iter().filter(|o| self.holds_order(o)).map(|o| o.order_id).collect();
            let open: Vec<(i64, i64)> = positions
                .iter()
                .filter(|p| self.holds_position(p))
                .map(|p| (p.position_id, p.trade_data.volume))
                .collect();
            if stops.is_empty() && open.is_empty() {
                break;
            }
            for order_id in stops {
                w.cancel(order_id);
            }
            for (position_id, volume) in open {
                w.close(position_id, volume);
            }
        }
        let Ok((positions, orders)) = w.reconcile_try() else { return false };
        let long = ProtoOaTradeSide::Buy as i32;
        let (mut clean, now) = (true, utc());
        for o in orders.iter().filter(|o| self.holds_order(o)) {
            clean = false;
            println!(
                "[{now}] CTHAND_LEFTOVER order STILL RESTS, cancel it by hand: {}",
                pending_line(o)
            );
        }
        for p in positions.iter().filter(|p| {
            is_open(p)
                && p.trade_data.symbol_id == self.symbol_id
                && p.trade_data.trade_side == long
        }) {
            clean = false;
            println!(
                "[{now}] CTHAND_LEFTOVER EURUSD long STILL OPEN: positionId={} volume={} label={:?}",
                p.position_id, p.trade_data.volume, p.trade_data.label
            );
        }
        if clean {
            println!(
                "[{now}] CTHAND_CLEANUP no order of this run rests and no EURUSD long is open"
            );
        }
        clean
    }

    /// Never panics: `false` when something of this run may still be on the account, which it then names.
    fn finish(&self) -> bool {
        self.armed.set(false);
        let clean = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.clean()))
            .unwrap_or(false);
        if !clean {
            println!(
                "[{}] CTHAND_LEFTOVER ⚠ NOT clean. This run's position {:?}, stops (variant, orderId) {:?}, labels `{}-*`: \
                 close and cancel them by hand in the cTrader platform. A stop left without its position OPENS one when \
                 the market reaches it",
                utc(),
                self.position.get(),
                self.stops.borrow(),
                self.tag
            );
        }
        clean
    }
}

impl Drop for HandGuard {
    fn drop(&mut self) {
        if self.armed.get() {
            self.finish();
        }
    }
}

/// Send a far protective SELL STOP bound to position `pid` the way a platform does: a typed `label` and `comment`, and the
/// request's own id (when it has one) for a `clientOrderId`. The order id of the venue's accept, or the venue's words.
fn send_hand_stop(
    w: &mut Wire,
    symbol_id: i64,
    pid: i64,
    volume: i64,
    stop_price: f64,
    label: &str,
    request_id: Option<String>,
) -> Result<i64, String> {
    let req = ProtoOaNewOrderReq {
        ctid_trader_account_id: w.ctid,
        symbol_id,
        order_type: ProtoOaOrderType::Stop as i32,
        trade_side: ProtoOaTradeSide::Sell as i32,
        volume,
        stop_price: Some(stop_price),
        position_id: Some(pid),
        label: Some(label.to_string()),
        comment: Some("typed by hand".to_string()),
        client_order_id: request_id.clone(),
        ..Default::default()
    };
    // The venue refuses an empty envelope (`Invalid clientRequestId length: 0`, 2026-11-06) and makes a raw order's
    // `clientOrderId` of the envelope's id, so a request naming none still needs one, and it is not the label.
    let envelope = request_id.unwrap_or_else(|| format!("{label}-envelope"));
    w.send(pt::NEW_ORDER_REQ, &req, &envelope);
    let frames = w.outcome(Duration::from_secs(10), Duration::from_millis(1500));
    execution_events(&frames)
        .iter()
        .filter(|e| e.execution_type == ProtoOaExecutionType::OrderAccepted as i32)
        .filter_map(|e| e.order.as_ref())
        .find(|o| o.trade_data.label.as_deref() == Some(label))
        .map(|o| o.order_id)
        .ok_or_else(|| {
            failure_of(&frames)
                .unwrap_or_else(|| format!("no accept among {} frame(s)", frames.len()))
        })
}

/// **A stop the node did not place is left alone by the bridge: never adopted, and not cancelled when its position closes**
/// (`crates/bridges/ctrader/CLAUDE.md`'s "A bound exit that outlived its position is flagged at connect, never cancelled").
///
/// What a run prints, and what it means. `CTHAND_ROW`: what `ProtoOAReconcileRes` says of each stop. `CTHAND_OUTCOME`: what
/// the venue answered at each step. `CTHAND_LANE`: every event the bridge handed its lane. A variant the venue REFUSED is
/// not measured, and the test fails naming it.
///
/// ⚠ **This bridge cannot rotate the grant, which is why it is not `live_runtime_for_account`.** A refused access token makes
/// that start spend the refresh token ([`CtraderConfig::to_connection_config`] hands it over), and with no rotation home the store
/// keeps the spent one: the running node's session ends and its own refresh is refused until someone runs `ctrader_authorize`.
/// [`ConnectionConfig::new`] carries no refresh token and no grant cell, so the actor's every road to a refresh answers
/// `NoRefreshToken`. It is otherwise the start production runs: [`connect_and_auth_execution`] seeds the book, runs
/// `adopt_bound_exits` and runs the orphan rule. A token refused here fails the run and spends nothing.
#[test]
#[ignore = "network + REAL cTrader demo endpoint + places REAL demo orders — run manually (see module doc)"]
fn hand_stop_left_alone_smoke() {
    use vike_execution::event_channel;

    let Some(config_value) = config() else { return };
    let lists = fetch_symbol_lists(config_value.to_connection_config())
        .expect("handshake + symbol lists against the real cTrader demo endpoint");
    let symbols = rows_of(&lists);
    let row = symbols.iter().find(|r| r.name == "EURUSD").expect("EURUSD is listed");
    let (id, min) = (row.id, row.full.min_volume.expect("a minimum volume"));
    let (long, sell) = (ProtoOaTradeSide::Buy as i32, ProtoOaTradeSide::Sell as i32);
    let is_our_side = |p: &ProtoOaPosition| {
        is_open(p) && p.trade_data.symbol_id == id && p.trade_data.trade_side == long
    };

    // 1. Flat on the symbol and the side the scenario uses: the stops face a long, and the bridge must see only ours.
    let mut w = Wire::connect(&config_value);
    let (positions, orders) = w.reconcile_try().expect("start-of-run state");
    assert!(
        !positions.iter().any(is_our_side)
            && !orders
                .iter()
                .any(|o| o.trade_data.symbol_id == id && o.trade_data.trade_side == sell),
        "ABORT: a EURUSD long or a resting EURUSD sell is on the account; nothing was touched"
    );
    w.subscribe(&[id]);
    let Some(quote) = w.live_quote(id, 40) else {
        panic!(
            "ABORT: no ticking EURUSD quote within 40 s: the market is closed; nothing was touched"
        );
    };
    let far = |below: f64| ((quote.bid as f64 / SCALE - below) * SCALE).round() / SCALE;

    let tag = format!("cthand-{}", now_ms() % 10_000_000);
    let guard = HandGuard {
        config: config_value.clone(),
        tag: tag.clone(),
        symbol_id: id,
        position: Cell::new(None),
        stops: RefCell::new(Vec::new()),
        armed: Cell::new(true),
    };

    // 2. The position, then the two stops, with nothing of the node's order client id path.
    let entry = ProtoOaNewOrderReq {
        ctid_trader_account_id: w.ctid,
        symbol_id: id,
        order_type: ProtoOaOrderType::Market as i32,
        trade_side: long,
        volume: min,
        label: Some(format!("{tag}-entry")),
        comment: Some("typed by hand".to_string()),
        ..Default::default()
    };
    w.send(pt::NEW_ORDER_REQ, &entry, &format!("{tag}-entry"));
    let frames = w.outcome(Duration::from_secs(10), Duration::from_millis(1500));
    if let Some(why) = failure_of(&frames) {
        panic!("the venue refused the entry: {why}");
    }
    let (positions, _) = w.reconcile_try().expect("read the position");
    let position = positions
        .iter()
        .find(|p| is_our_side(p))
        .unwrap_or_else(|| panic!("no EURUSD long after the entry: {} frame(s)", frames.len()));
    let (pid, volume) = (position.position_id, position.trade_data.volume);
    guard.position.set(Some(pid));
    println!(
        "[{}] CTHAND_POSITION positionId={pid} volume={volume} label={:?} comment={:?}",
        utc(),
        position.trade_data.label,
        position.trade_data.comment
    );

    // Far under the market by the distances `ctrader_bound_exit_smoke.rs` rests its stops at.
    let variants =
        [("A", far(0.0100), None), ("B", far(0.0150), Some(format!("{tag}-platform-request")))];
    for (variant, stop_price, request_id) in variants {
        let label = format!("{tag}-typed-{variant}");
        match send_hand_stop(&mut w, id, pid, volume, stop_price, &label, request_id) {
            Ok(order_id) => {
                guard.stops.borrow_mut().push((variant, order_id));
                println!(
                    "[{}] CTHAND_OUTCOME variant={variant} ACCEPTED order={order_id} stopPrice={stop_price}",
                    utc()
                );
            }
            Err(why) => {
                println!("[{}] CTHAND_OUTCOME variant={variant} REFUSED by the venue: {why}", utc())
            }
        }
    }
    let measured = guard.stops.borrow().clone();

    // 3. What the venue's own list says of them.
    let (_, orders) = w.reconcile_try().expect("read the stops");
    let stops: Vec<&ProtoOaOrder> = orders.iter().filter(|o| guard.holds_order(o)).collect();
    for o in &stops {
        println!("[{}] CTHAND_ROW {}", utc(), pending_line(o));
    }

    // 4. A real exec start: its connect-time look must read the position and adopt neither stop.
    let (tx, mut lane) = event_channel(1024);
    let mut conn = ConnectionConfig::new(
        config_value.host.clone(),
        config_value.port,
        config_value.client_id.clone(),
        config_value.client_secret.expose().clone(),
        config_value.access_token.expose().clone(),
    );
    conn.account_id = config_value.account_id;
    let bridge =
        connect_and_auth_execution(conn, Arc::new(NoopSink), tx).expect("the bridge's exec start");
    assert!(bridge.ctid == w.ctid, "the bridge authorized another account than the raw wire");
    assert!(
        bridge.symbols.account_may_hold_a_hedge(),
        "a NETTED account adopts nothing: this run measures nothing"
    );
    let (book_read, adopted) = {
        let book = bridge.positions.lock().expect("the bridge's position book");
        let adopted: Vec<String> = stops
            .iter()
            .flat_map(|o| [o.client_order_id.as_deref(), o.trade_data.label.as_deref()])
            .flatten()
            .filter(|key| book.bound_exit(key).is_some())
            .map(str::to_string)
            .collect();
        (book.is_fetched() && book.position(pid).is_some(), adopted)
    };
    println!(
        "[{}] CTHAND_OUTCOME connect: the bridge's book read the position={book_read}; stops it took as bound exits: {adopted:?}",
        utc()
    );
    assert!(
        book_read,
        "the connect-time look did not read the position: this run measures nothing"
    );
    assert!(adopted.is_empty(), "the bridge ADOPTED a stop it did not place: {adopted:?}");

    // 5. Close the position past the bridge. The handshake can leave the first socket unread for half a minute.
    w = Wire::connect(&config_value);
    let mut frames = w.close(pid, volume);
    if let Some(why) = failure_of(&frames) {
        panic!("the venue refused the close: {why}");
    }
    let deadline = Instant::now() + Duration::from_secs(25);
    while bridge.positions.lock().expect("the bridge's position book").position(pid).is_some() {
        assert!(
            Instant::now() < deadline,
            "the bridge never saw the close: this run measures nothing"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "[{}] CTHAND_OUTCOME close: the venue closed it and the bridge's book dropped it",
        utc()
    );
    frames.extend(w.outcome(ORPHAN_RULE_WAIT, ORPHAN_RULE_WAIT));
    for e in execution_events(&frames) {
        let Some(order) = &e.order else { continue };
        if measured.iter().any(|(_, id)| *id == order.order_id) {
            println!(
                "[{}] CTHAND_OUTCOME venue event on order={}: {:?}",
                utc(),
                order.order_id,
                ProtoOaExecutionType::try_from(e.execution_type)
            );
        }
    }
    let (positions, orders) = w.reconcile_try().expect("read the stops after the close");
    let position_gone = !positions.iter().any(|p| is_open(p) && p.position_id == pid);
    let mut gone = Vec::new();
    for (variant, order_id) in &measured {
        match orders.iter().find(|o| o.order_id == *order_id) {
            Some(o) => println!(
                "[{}] CTHAND_OUTCOME variant={variant} after the close: STILL PENDING {}",
                utc(),
                pending_line(o)
            ),
            None => {
                gone.push(*variant);
                println!(
                    "[{}] CTHAND_OUTCOME variant={variant} after the close: order={order_id} is NOT on the venue's list",
                    utc()
                );
            }
        }
    }
    let mut events = Vec::new();
    drain(&mut lane, &mut events);
    for e in &events {
        println!("CTHAND_LANE {e:?}");
    }

    // 6. Clean up first, so a failed verdict leaves nothing behind either.
    drop(bridge);
    let flat = guard.finish();
    assert!(position_gone, "the venue still lists the position after its close");
    assert!(
        gone.is_empty(),
        "the stops of variant {gone:?} left the venue's list: something cancelled them"
    );
    assert_eq!(measured.len(), 2, "a variant was refused and is not measured: see CTHAND_OUTCOME");
    assert!(flat, "the account is NOT flat: see CTHAND_LEFTOVER");
    println!(
        "[{}] CTHAND_OUTCOME verdict: both stops LEFT ALONE, and the clean-up took them",
        utc()
    );
}
