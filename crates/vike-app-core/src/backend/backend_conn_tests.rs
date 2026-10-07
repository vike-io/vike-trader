use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use vike_data::{DataClient, LiveDataError, SubscriptionId};
use vike_model::BookLevel;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn rec(name: &str, control: bool) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        addr: "127.0.0.1:1".to_string(), // closed port: dials fail fast, nothing listens
        observe_key: "PROD2_OBSERVE_KEY".to_string(),
        control_key: Some("PROD2_CONTROL_KEY".to_string()),
        control,
        datahub_observe_key: String::new(),
    }
}

fn status() -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(String::new()))
}

/// The `--observe ADDR` synthetic record reproduces today's env-key behavior exactly: observe
/// key from `VIKE_TRADEHUB_OBSERVE_KEY`, control key from `VIKE_TRADEHUB_CONTROL_KEY`, and
/// the record ARMED so the process-level master gate stays the ONLY gate on the CLI path.
#[test]
fn the_synthetic_observe_record_reproduces_todays_env_key_behavior() {
    let r = cli_observe_record("the CI box.example:9040");
    assert_eq!(r.addr, "the CI box.example:9040");
    assert_eq!(r.name, "", "synthetic record is unnamed — never persisted");
    assert_eq!(r.observe_key, backend_registry::OBSERVE_KEY_NAME);
    assert_eq!(r.control_key.as_deref(), Some(backend_registry::CONTROL_KEY_NAME));
    assert!(
        r.control,
        "CLI record must be ARMED: pre-B1, VIKE_TRADEHUB_CONTROL=1 + the key's presence decided alone"
    );

    // Resolution over the same map `App::new` always read: both present -> both resolve;
    // control key absent -> read-only (exactly `vars.get(..)` before B1); empty map -> none.
    let both = vars(&[
        (backend_registry::OBSERVE_KEY_NAME, "obs"),
        (backend_registry::CONTROL_KEY_NAME, "ctl"),
    ]);
    assert_eq!(backend_registry::resolve_keys(&r, &both), (Some("obs"), Some("ctl")));
    let observe_only = vars(&[(backend_registry::OBSERVE_KEY_NAME, "obs")]);
    assert_eq!(backend_registry::resolve_keys(&r, &observe_only), (Some("obs"), None));
    assert_eq!(backend_registry::resolve_keys(&r, &vars(&[])), (None, None));
}

/// `connect_backend` resolves keys per record and refuses the control channel when EITHER
/// gate is down: an unarmed record under the master gate, and an armed record without it.
/// (Both refuse-paths need no network — nothing dials a control socket. The bridge thread
/// does dial the dead observe addr in the background; dropping the conn stops-and-joins it.)
#[test]
fn connect_backend_refuses_control_when_either_gate_is_down() {
    let m = vars(&[("PROD2_OBSERVE_KEY", "obs"), ("PROD2_CONTROL_KEY", "ctl")]);

    // Per-backend gate down (record unarmed), master up: read-only.
    let conn = connect_backend(&rec("unarmed", false), &m, true, status(), |_| {}, || {});
    assert!(conn.ctrl.is_none(), "unarmed record must never mount control");
    assert_eq!(conn.record, rec("unarmed", false));
    drop(conn); // stop + join the bridge thread

    // Master gate down, record armed: read-only.
    let conn = connect_backend(&rec("armed", true), &m, false, status(), |_| {}, || {});
    assert!(conn.ctrl.is_none(), "master gate off must refuse control even for an armed record");
    drop(conn);

    // Both gates up but no daemon at the addr: the connect fails and the observer DEGRADES to
    // read-only rather than erroring — the same contract `connect_control` always had.
    let conn = connect_backend(&rec("armed", true), &m, true, status(), |_| {}, || {});
    assert!(conn.ctrl.is_none(), "a dead daemon degrades to read-only, never Some");
    drop(conn);
}

// ── the switch routine ──────────────────────────────────────────────────────────────────

type Log = Arc<Mutex<Vec<String>>>;

/// A recording `DataClient` double — enough of `feed_lifecycle`'s `FakeFeed` to prove the
/// unsubscribe routing (that module's double is `#[cfg(test)]`-private to it).
struct RecClient {
    venue: &'static str,
    log: Log,
}

impl DataClient for RecClient {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(1))
    }
    fn subscribe_quotes(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(2))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(3))
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(4))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.lock().unwrap().push(format!("{}:unsub {}", self.venue, id.0));
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push(format!("{}:shutdown", self.venue));
    }
}

/// Owns every field of [`SwitchSlots`], POPULATED — the CoreSyncState-shaped fixture the
/// brief asks for, widened to the whole clear surface.
struct Fixture {
    charts: HashMap<String, model::ChartState>,
    aggs: HashMap<String, (String, String, TickVolAgg)>,
    of_aggs: HashMap<String, (String, String, OrderflowAgg)>,
    bf_pending: HashMap<String, Vec<vike_model::TradeTick>>,
    published: Vec<crate::ui::series_follow::PublishedSeries>,
    last_seq: u64,
    status_line: String,
    feeds: FeedMap,
    subs: HashMap<String, SubscriptionId>,
    spawned: HashSet<String>,
    unroutable: HashSet<String>,
    feed_retries: FeedRetries,
    bf_spawned: HashSet<String>,
    bf_retries: BackfillRetries,
    earliest_live_ids: Mutex<HashMap<String, u64>>,
    /// Held so the backfill channel stays CONNECTED and the drain sees Empty after the queued
    /// batch. ⚠ This used to say "exactly like the live App, which owns its sender for the
    /// process life" — no longer: the desktop shell's producers left with its local market-data
    /// plane, and `crates/vike-desktop/src/main.rs`'s `dead_receiver` drops the sender on the
    /// spot, so the LIVE drain sees Disconnected on every frame. Both answers end the drain
    /// loop identically; this fixture keeps a sender only so a batch can be queued for the
    /// switch to prove it survives.
    _bf_tx: std::sync::mpsc::Sender<(String, Vec<vike_model::TradeTick>)>,
    bf_rx: Receiver<(String, Vec<vike_model::TradeTick>)>,
    bf_done_rx: Receiver<BackfillReport>,
    trades: TradeStore,
    books: BookStore,
    direct_bars: DirectBarStore,
    trade_depth: HashMap<(String, String), TradeDepthSubs>,
    poly_subs: HashMap<String, PolyBookSubs>,
    hidden: HashSet<String>,
    feed_status: Arc<Mutex<String>>,
    log: Log,
}

fn tick(symbol: &str) -> vike_model::TradeTick {
    vike_model::TradeTick {
        ts: 1,
        local_ts: 1,
        price: 100.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: symbol.to_string(),
    }
}

fn populated() -> Fixture {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut feeds: FeedMap = HashMap::new();
    feeds.insert("binance", Box::new(RecClient { venue: "binance", log: Arc::clone(&log) }));
    feeds.insert("okx", Box::new(RecClient { venue: "okx", log: Arc::clone(&log) }));
    feeds.insert("polymarket", Box::new(RecClient { venue: "polymarket", log: Arc::clone(&log) }));

    let mut charts = HashMap::new();
    // The fixture is THIRD-MODE shaped (feeds + backend + the direct-bar store), so the
    // session/plane SPLIT has three observable chart families:
    // - `deribit:BTC-PERPETUAL@1m` — a kline on a venue with NO local bar feed:
    //   snapshot-rendered, backend A's streamed tail — the switch clear must drop it;
    // - `BTCUSDT@1m` — a kline on a DIRECT_BAR_VENUES venue: rendered from the venue-fed
    //   DirectBarStore (B2's direct-bar follow-up), venue truth — a switch must KEEP it;
    // - `BTCUSDT@100t` — tape-rendered, venue truth — kept, as since B2.
    charts.insert("deribit:BTC-PERPETUAL@1m".to_string(), model::ChartState::default());
    charts.insert("BTCUSDT@1m".to_string(), model::ChartState::default());
    charts.insert("BTCUSDT@100t".to_string(), model::ChartState::default());
    let mut aggs = HashMap::new();
    aggs.insert(
        "BTCUSDT@100t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            TickVolAgg::new(&vike_orderflow::tickvol::BarKind::Tick(100)).expect("tick agg"),
        ),
    );
    let mut of_aggs = HashMap::new();
    of_aggs.insert(
        "BTCUSDT@1m".to_string(),
        ("binance".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
    );
    let mut bf_pending = HashMap::new();
    bf_pending.insert("BTCUSDT".to_string(), vec![tick("BTCUSDT")]);

    let mut subs = HashMap::new();
    subs.insert("BTCUSDT@1m".to_string(), SubscriptionId(101));
    subs.insert("okx:ETHUSDT@1m".to_string(), SubscriptionId(201));

    let mut feed_retries = FeedRetries::default();
    feed_retries.note_missing(&crate::ui::feed_lifecycle::RetryKey::series("deribit:X@1m"));
    let mut bf_retries = BackfillRetries::default();
    bf_retries.note_report(&BackfillReport::failed("BTCUSDT", 0, "boom"));

    let (bf_tx, bf_rx) = std::sync::mpsc::channel();
    bf_tx.send(("BTCUSDT".to_string(), vec![tick("BTCUSDT")])).unwrap();
    let (bf_done_tx, bf_done_rx) = std::sync::mpsc::channel();
    bf_done_tx.send(BackfillReport::finished("BTCUSDT", 3)).unwrap();

    let trades = TradeStore::default();
    trades.push("binance", &tick("BTCUSDT"));
    let books = BookStore::default();
    books.update(
        "binance",
        "BTCUSDT",
        0.1,
        vec![BookLevel::new(100.0, 1.0)],
        vec![BookLevel::new(100.1, 1.0)],
        1,
    );
    let direct_bars = DirectBarStore::default();
    direct_bars.seed(
        "binance",
        "BTCUSDT",
        "1m",
        vec![vike_model::Bar {
            ts: 60_000,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }],
    );

    Fixture {
        charts,
        aggs,
        of_aggs,
        bf_pending,
        // A's published series — backend-session state `clear_session_state` must drop, or the
        // symbol picker offers B a series A had and `follow_backend` can retarget onto it.
        published: vec![crate::ui::series_follow::PublishedSeries::new(
            "deribit",
            "BTC-PERPETUAL",
            "1m",
        )],
        last_seq: 42,
        status_line: "OBSERVING the CI box".to_string(),
        feeds,
        subs,
        spawned: ["BTCUSDT@1m".to_string(), "okx:ETHUSDT@1m".to_string()].into(),
        unroutable: ["deribit:X@1m".to_string()].into(),
        feed_retries,
        bf_spawned: ["BTCUSDT".to_string()].into(),
        bf_retries,
        earliest_live_ids: Mutex::new([("BTCUSDT".to_string(), 7_u64)].into()),
        _bf_tx: bf_tx,
        bf_rx,
        bf_done_rx,
        trades,
        books,
        direct_bars,
        // One Binance and one OKX depth stream, so the clear must stop depth streams across two
        // venues; one cockpit token with both legs live. Ids are disjoint from `subs`' so the log
        // lines are unambiguous.
        trade_depth: [
            (
                ("binance".to_string(), "BTCUSDT".to_string()),
                TradeDepthSubs { depth: SubscriptionId(150), trades: None },
            ),
            (
                ("okx".to_string(), "ETH-USDT-SWAP".to_string()),
                TradeDepthSubs { depth: SubscriptionId(250), trades: None },
            ),
        ]
        .into(),
        poly_subs: [(
            "1071".to_string(),
            PolyBookSubs { book: SubscriptionId(450), trades: Some(SubscriptionId(451)) },
        )]
        .into(),
        hidden: ["ETHUSDT@1m".to_string()].into(),
        feed_status: Arc::new(Mutex::new("OBSERVING the CI box".to_string())),
        log,
    }
}

impl Fixture {
    fn slots(&mut self) -> SwitchSlots<'_> {
        SwitchSlots {
            charts: &mut self.charts,
            aggs: &mut self.aggs,
            of_aggs: &mut self.of_aggs,
            bf_pending: &mut self.bf_pending,
            published: &mut self.published,
            last_seq: &mut self.last_seq,
            status: &mut self.status_line,
            feeds: &mut self.feeds,
            subs: &mut self.subs,
            spawned: &mut self.spawned,
            unroutable: &mut self.unroutable,
            feed_retries: &mut self.feed_retries,
            bf_spawned: &mut self.bf_spawned,
            bf_retries: &mut self.bf_retries,
            earliest_live_ids: &self.earliest_live_ids,
            bf_rx: &self.bf_rx,
            bf_done_rx: &self.bf_done_rx,
            trades: &self.trades,
            books: &self.books,
            direct_bars: Some(&self.direct_bars),
            // The fixture is THIRD-MODE shaped, so the plane is the one that fills the store
            // from LOCAL venue feeds. `the_backend_store_plane_clears_its_own_store` below
            // drives the same fixture under the desktop's plane instead.
            bar_plane: split_plane::BarPlane::VenueFeeds,
            trade_depth: &mut self.trade_depth,
            poly_subs: &mut self.poly_subs,
            hidden: &mut self.hidden,
            feed_status: &self.feed_status,
        }
    }

    /// [`Self::slots`] with the **BACKEND-STORE** plane — the shipped desktop's shape, where
    /// the store's content is a read of THIS backend's hist store rather than venue truth.
    fn backend_store_slots(&mut self) -> SwitchSlots<'_> {
        SwitchSlots { bar_plane: split_plane::BarPlane::BackendStore, ..self.slots() }
    }

    /// The backend session is gone (snapshot-rendered charts, seq gate, status, hidden) —
    /// [`clear_session_state`]'s whole contract on the populated fixture.
    fn assert_backend_session_cleared(&self) {
        assert!(
            !self.charts.contains_key("deribit:BTC-PERPETUAL@1m"),
            "the snapshot-rendered (backend-tail kline) chart is backend-session state and \
                 must clear"
        );
        assert_eq!(self.last_seq, 0, "last_seq");
        assert!(self.published.is_empty(), "published (A's series may not be offered under B)");
        assert!(self.status_line.is_empty(), "status");
        assert!(self.hidden.is_empty(), "hidden");
    }

    /// The local feed plane is UNTOUCHED — every ledger, fold, cache and in-flight batch
    /// still present. ⚠ Consuming reads (`try_recv`, `TradeStore::drain`) — call this once,
    /// as a test's final assertion block.
    fn assert_feed_plane_intact(&self) {
        assert!(
            self.charts.contains_key("BTCUSDT@100t"),
            "the tape-rendered chart is venue truth and survives a switch"
        );
        assert!(
            self.charts.contains_key("BTCUSDT@1m"),
            "the DIRECT-BAR kline chart paints the venue's own bars and survives a switch"
        );
        assert!(
            self.direct_bars.series("binance", "BTCUSDT", "1m").is_some(),
            "the direct-bar store is venue truth and survives a switch"
        );
        assert_eq!(self.subs.len(), 2, "subs — the venue streams keep flowing");
        assert_eq!(self.spawned.len(), 2, "spawned — still the fold filter for B's bars");
        assert_eq!(self.unroutable.len(), 1, "unroutable");
        assert!(!self.feed_retries.is_empty(), "feed_retries");
        assert!(self.aggs.contains_key("BTCUSDT@100t"), "aggs");
        assert!(self.of_aggs.contains_key("BTCUSDT@1m"), "of_aggs");
        assert!(self.bf_pending.contains_key("BTCUSDT"), "bf_pending stays deliverable");
        assert_eq!(self.bf_spawned.len(), 1, "bf_spawned");
        assert!(!self.bf_retries.is_empty(), "bf_retries");
        assert!(!self.earliest_live_ids.lock().unwrap().is_empty(), "earliest_live_ids");
        assert_eq!(self.trade_depth.len(), 2, "trade_depth — the Trade windows keep painting");
        assert_eq!(self.poly_subs.len(), 1, "poly_subs — the cockpit keeps painting");
        assert!(self.bf_rx.try_recv().is_ok(), "in-flight backfill batch survives");
        assert!(self.bf_done_rx.try_recv().is_ok(), "in-flight backfill report survives");
        assert_eq!(self.trades.drain("binance", "BTCUSDT").len(), 1, "trade tape survives");
        assert!(self.books.get("binance", "BTCUSDT").is_some(), "books survive");
    }

    /// Every named field is empty/reset — B1's original TOTAL-clear contract, now the
    /// composition of [`clear_session_state`] and [`teardown_feed_plane`].
    fn assert_cleared(&self) {
        assert!(self.charts.is_empty(), "charts");
        assert!(self.aggs.is_empty(), "aggs");
        assert!(self.of_aggs.is_empty(), "of_aggs");
        assert!(self.bf_pending.is_empty(), "bf_pending");
        assert_eq!(self.last_seq, 0, "last_seq");
        assert!(self.published.is_empty(), "published");
        assert!(self.status_line.is_empty(), "status");
        assert!(self.subs.is_empty(), "subs");
        assert!(self.spawned.is_empty(), "spawned");
        assert!(self.unroutable.is_empty(), "unroutable");
        assert!(self.feed_retries.is_empty(), "feed_retries");
        assert!(self.bf_spawned.is_empty(), "bf_spawned");
        assert!(self.bf_retries.is_empty(), "bf_retries");
        assert!(self.earliest_live_ids.lock().unwrap().is_empty(), "earliest_live_ids");
        assert!(self.bf_rx.try_recv().is_err(), "bf_rx drained");
        assert!(self.bf_done_rx.try_recv().is_err(), "bf_done_rx drained");
        assert!(self.trades.drain("binance", "BTCUSDT").is_empty(), "trades cleared");
        assert!(self.books.get("binance", "BTCUSDT").is_none(), "books cleared");
        assert!(self.direct_bars.keys().is_empty(), "direct-bar store cleared");
        assert!(self.trade_depth.is_empty(), "trade_depth");
        assert!(self.poly_subs.is_empty(), "poly_subs");
        assert!(self.hidden.is_empty(), "hidden");
    }
}

/// The full unsubscribe fan-out `teardown_feed_plane` must produce on the populated fixture:
/// every `subs` id, both depth ledger entries and both cockpit legs, each against the venue client
/// that issued it.
const FULL_TEARDOWN_LOG: [&str; 6] = [
    "binance:unsub 101",    // subs: BTCUSDT@1m
    "binance:unsub 150",    // trade_depth: (binance, BTCUSDT) depth leg
    "okx:unsub 201",        // subs: okx:ETHUSDT@1m
    "okx:unsub 250",        // trade_depth: (okx, ETH-USDT-SWAP) depth leg
    "polymarket:unsub 450", // poly_subs: 1071 book leg
    "polymarket:unsub 451", // poly_subs: 1071 trade leg
];

/// THE B2 SWITCH CONTRACT — feeds are NOT backend-session state. A switch clears the backend
/// session (snapshot-rendered charts, the seq gate, status, hidden) and performs **ZERO feed
/// unsubscribes and zero shutdowns**: the recording clients must observe nothing at all,
/// and every local-plane slot — ledgers, folds, caches, in-flight backfill — stays intact.
#[test]
fn a_switch_clears_the_backend_session_and_never_touches_the_feed_plane() {
    let mut fx = populated();
    clear_session_state(&mut fx.slots());

    fx.assert_backend_session_cleared();
    assert!(
        fx.log.lock().unwrap().is_empty(),
        "a backend switch must not unsubscribe or shut down ANY local feed stream"
    );
    assert_eq!(fx.feeds.len(), 3, "feeds map untouched");
    fx.assert_feed_plane_intact();
}

/// ⚠ THE SAME SWITCH UNDER THE **BACKEND-STORE** PLANE — the shipped desktop's shape, and the
/// opposite verdict on the same two slots. There the store holds bars read out of THIS
/// backend's hist store (a 5m chart the daemon does not stream), so they are backend-session
/// state: both the kline chart entries and the store's own content must go, or backend A's
/// history repaints under backend B's connection. The tape stays venue truth under every
/// plane, which is what makes this a narrowing of the clear rather than a return to B1's
/// total one.
#[test]
fn the_backend_store_plane_clears_its_own_store_on_a_switch() {
    let mut fx = populated();
    assert!(fx.direct_bars.series("binance", "BTCUSDT", "1m").is_some(), "seeded by `populated`");
    clear_session_state(&mut fx.backend_store_slots());

    fx.assert_backend_session_cleared();
    assert!(
        !fx.charts.contains_key("BTCUSDT@1m"),
        "under BackendStore a kline chart's bars came from THIS backend — it must clear"
    );
    assert!(
        fx.direct_bars.keys().is_empty(),
        "…and so must the store itself: A's history may not paint under B"
    );
    assert!(
        fx.charts.contains_key("BTCUSDT@100t"),
        "the tape is venue truth under every plane and still survives"
    );
    assert!(
        fx.log.lock().unwrap().is_empty(),
        "still ZERO unsubscribes — this is a session clear, not a feed teardown"
    );
}

/// THE MODE-EXIT TEARDOWN — [`teardown_feed_plane`] stops every local stream through the
/// ledgers: each id unsubscribed against the venue client that issued it (the #1379
/// discipline — depth/bar and cockpit book/trade legs included, which pre-#1379 were
/// dropped without any unsubscribe), the local plane cleared, and the BACKEND session left
/// alone (it is `clear_session_state`'s, not this function's).
#[test]
fn the_feed_plane_teardown_unsubscribes_every_stream_through_the_ledgers() {
    let mut fx = populated();
    teardown_feed_plane(&mut fx.slots());

    let mut log = fx.log.lock().unwrap().clone();
    log.sort();
    assert_eq!(
        log,
        FULL_TEARDOWN_LOG.map(str::to_string).to_vec(),
        "each id must be unsubscribed against the venue that issued it — never handed onward"
    );
    // The feed CLIENTS survive even here: shutdown is `App::on_exit`'s (bounded, parallel).
    assert_eq!(fx.feeds.len(), 3, "feeds map is not torn down — clients are on_exit's");
    assert!(fx.subs.is_empty() && fx.trade_depth.is_empty() && fx.poly_subs.is_empty());
    assert!(fx.aggs.is_empty() && fx.of_aggs.is_empty() && fx.bf_pending.is_empty());
    assert!(fx.spawned.is_empty() && fx.unroutable.is_empty());
    assert!(fx.feed_retries.is_empty() && fx.bf_retries.is_empty());
    assert!(fx.trades.drain("binance", "BTCUSDT").is_empty(), "tape cleared");
    assert!(fx.books.get("binance", "BTCUSDT").is_none(), "books cleared");
    assert!(fx.direct_bars.keys().is_empty(), "direct-bar store is feed-plane state: cleared");
    // The backend session is deliberately NOT this function's: still folded, still painted.
    assert!(
        fx.charts.contains_key("deribit:BTC-PERPETUAL@1m"),
        "the backend-tail kline chart is the session clear's"
    );
    assert!(!fx.charts.contains_key("BTCUSDT@100t"), "tape chart is the feed plane's");
    assert!(
        !fx.charts.contains_key("BTCUSDT@1m"),
        "the direct-bar kline chart is the feed plane's (its bars are the venue's, not A's)"
    );
    assert_eq!(fx.last_seq, 42, "seq gate untouched");
    assert_eq!(fx.status_line, "OBSERVING the CI box", "status untouched");
}

/// The two clears COMPOSE to B1's original total clear, field for field — so no
/// [`SwitchSlots`] slot can silently fall between the session half and the feed-plane half,
/// and the full unsubscribe fan-out still happens exactly once.
#[test]
fn the_session_clear_and_the_feed_plane_teardown_compose_to_the_b1_total_clear() {
    let mut fx = populated();
    clear_session_state(&mut fx.slots());
    teardown_feed_plane(&mut fx.slots());

    fx.assert_cleared();
    let mut log = fx.log.lock().unwrap().clone();
    log.sort();
    assert_eq!(log, FULL_TEARDOWN_LOG.map(str::to_string).to_vec());
}

/// `switch_backend(target: None)` — the Disconnect path: old conn stopped and dropped, the
/// empty snapshot published (so the render loop never keeps painting A's data), the backend
/// session cleared — the feed plane still running (a disconnect stays IN observe mode, so the
/// ladder/tape keep painting) — and the feed-status line says so.
#[test]
fn a_disconnect_stops_the_old_backend_and_blanks_the_published_snapshot() {
    let mut fx = populated();
    let m = vars(&[("PROD2_OBSERVE_KEY", "obs")]);
    let mut active =
        Some(connect_backend(&rec("the CI box", false), &m, false, status(), |_| {}, || {}));

    let published: Arc<Mutex<Option<Arc<vike_core::CoreSnapshot>>>> = Arc::new(Mutex::new(None));
    let repainted = Arc::new(AtomicBool::new(false));
    let (p, r) = (Arc::clone(&published), Arc::clone(&repainted));
    switch_backend(
        &mut active,
        None,
        &m,
        false,
        fx.slots(),
        move |snap| *p.lock().unwrap() = Some(snap),
        move || r.store(true, Ordering::Relaxed),
    );

    assert!(active.is_none(), "disconnect leaves no active backend");
    fx.assert_backend_session_cleared();
    let snap = published.lock().unwrap().clone().expect("an empty snapshot must be published");
    assert!(snap.bars.is_empty() && snap.orders.is_empty(), "published snapshot is EMPTY");
    assert!(repainted.load(Ordering::Relaxed), "a repaint is requested so the blank shows");
    assert_eq!(*fx.feed_status.lock().unwrap(), "disconnected");
    assert!(fx.log.lock().unwrap().is_empty(), "a disconnect touches no local feed stream");
    fx.assert_feed_plane_intact();
}

/// `switch_backend(target: Some)` — the Connect path: the old conn is replaced by one dialed
/// from the NEW record (key resolution per that record), over a cleared backend session.
#[test]
fn a_switch_replaces_the_active_backend_with_the_target_record() {
    let mut fx = populated();
    let m = vars(&[("PROD2_OBSERVE_KEY", "obs")]);
    let mut active = Some(connect_backend(&rec("old", false), &m, false, status(), |_| {}, || {}));

    let target = rec("new", false);
    switch_backend(&mut active, Some(&target), &m, false, fx.slots(), |_| {}, || {});

    assert_eq!(
        active.as_ref().map(|c| c.record.name.as_str()),
        Some("new"),
        "the active backend is now the target record"
    );
    fx.assert_backend_session_cleared();
    assert!(fx.log.lock().unwrap().is_empty(), "a switch touches no local feed stream");
    drop(active); // stop + join the new bridge
}

// ── the pure UI decisions ───────────────────────────────────────────────────────────────

/// Which entry is active, and how an unlisted (CLI-synthetic) connection is shown: it leads
/// the list, marked active; registry rows keep file order; a listed active row is marked in
/// place with no extra row.
#[test]
fn picker_rows_mark_the_active_record_and_surface_an_unlisted_connection() {
    let file = BackendsFile { backends: vec![rec("a", false), rec("b", true)], active: None };

    // Active is a registry record: marked in place, no extra row.
    let listed_active = rec("b", true);
    let rows = picker_rows(&file, Some(&listed_active));
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.listed));
    assert_eq!(
        rows.iter().map(|r| r.is_active).collect::<Vec<_>>(),
        vec![false, true],
        "the matching registry row is the active one"
    );

    // Active is the CLI synthetic record: surfaced as an extra, unlisted first row.
    let synthetic = cli_observe_record("cli.example:9040");
    let rows = picker_rows(&file, Some(&synthetic));
    assert_eq!(rows.len(), 3);
    assert!(rows[0].is_active && !rows[0].listed, "unlisted active row leads");
    assert_eq!(rows[0].record.addr, "cli.example:9040");
    assert!(rows[1..].iter().all(|r| r.listed && !r.is_active));

    // No active connection: plain registry list.
    let rows = picker_rows(&file, None);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| !r.is_active));
}

/// ⚠ **The REGISTRY's rows carry no unlisted head row, in any of the three states** — which is
/// what stops the Connections tool's Backend tab from restating the ambient foot strip's five
/// facts a second time on the same screen. The paired `active_is_unlisted` is how the tab knows
/// to say so in a sentence instead.
///
/// Reddens on `backend_tab` going back to `picker_rows`, and on a filter that also drops the
/// listed record the connection matches (which would leave nothing to Edit or Delete).
#[test]
fn the_registry_rows_never_carry_the_unlisted_live_connection() {
    let file = BackendsFile { backends: vec![rec("a", false), rec("b", true)], active: None };

    // Active is a registry record: the row is there, marked active, and nothing is unlisted.
    let listed_active = rec("b", true);
    let rows = registry_rows(&file, Some(&listed_active));
    assert_eq!(rows.len(), 2, "every registry record still has its row");
    assert_eq!(rows.iter().map(|r| r.is_active).collect::<Vec<_>>(), vec![false, true]);
    assert!(!active_is_unlisted(&file, Some(&listed_active)));

    // Active is the CLI synthetic record: the registry list is UNCHANGED and the connection is
    // reported as unlisted rather than appended as a row.
    let synthetic = cli_observe_record("cli.example:9040");
    let rows = registry_rows(&file, Some(&synthetic));
    assert_eq!(rows.len(), 2, "the synthetic connection is not a record");
    assert!(rows.iter().all(|r| r.listed && !r.is_active));
    assert!(active_is_unlisted(&file, Some(&synthetic)));

    // Nothing connected: plain registry list, nothing unlisted.
    let rows = registry_rows(&file, None);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| !r.is_active));
    assert!(!active_is_unlisted(&file, None));

    // An EMPTY registry with a synthetic connection is the `--observe` box: no rows at all,
    // and the connection still reported. That is the state whose row was the duplication.
    let empty = BackendsFile::default();
    assert!(registry_rows(&empty, Some(&synthetic)).is_empty());
    assert!(active_is_unlisted(&empty, Some(&synthetic)));
}

/// What a click does: the active row disconnects, any other row connects (implicit
/// disconnect-then-connect inside `switch_backend`).
#[test]
fn clicking_the_active_backend_disconnects_and_any_other_connects() {
    let a = rec("a", false);
    let b = rec("b", false);
    assert_eq!(click_action(&a, Some(&a)), BackendAction::Disconnect);
    assert_eq!(click_action(&b, Some(&a)), BackendAction::Connect(b.clone()));
    assert_eq!(click_action(&a, None), BackendAction::Connect(a.clone()));
}

/// The B2 fence: no switching while a local core runs.
#[test]
fn switching_is_unavailable_while_a_local_core_runs() {
    assert!(switching_available(false));
    assert!(!switching_available(true));
}

/// Connecting to a LISTED record names it active, so a bare relaunch dials the node the
/// operator last picked instead of the local default.
#[test]
fn connecting_to_a_listed_record_names_it_active() {
    let listed = vec![rec("the latency box", false), rec("the CI box", false)];
    let action = BackendAction::Connect(rec("the CI box", false));
    assert_eq!(active_after(&action, &listed), Some("the CI box".to_string()));
}

/// Disconnecting clears the pointer: staying down is a decision, and a launch that redialled
/// the node the operator just left would be the app overruling them.
#[test]
fn disconnecting_clears_the_active_pointer() {
    let listed = vec![rec("the latency box", false)];
    assert_eq!(active_after(&BackendAction::Disconnect, &listed), None);
}

/// The `--observe` connection is UNLISTED, so it can never be named: `active_addr` resolves a
/// pointer through `backends`, and one naming no record would answer nothing anyway.
#[test]
fn an_unlisted_connection_is_never_named_active() {
    let listed = vec![rec("the latency box", false)];
    let action = BackendAction::Connect(rec("cli-observe", false));
    assert_eq!(active_after(&action, &listed), None);
}

/// An empty registry names nothing — the first-run state, where the picker has no rows at all.
#[test]
fn an_empty_registry_names_nothing_active() {
    let action = BackendAction::Connect(rec("the CI box", false));
    assert_eq!(active_after(&action, &[]), None);
}

/// The flag OVERRIDES the registry and stays synthetic, so `--observe ADDR` keeps its
/// pre-registry meaning even when an active record exists.
#[test]
fn the_observe_flag_overrides_the_registry_and_stays_synthetic() {
    let active = rec("the CI box", false);
    let r = startup_backend_from(Some("other.example:9040"), Some(active));
    assert_eq!(r.addr, "other.example:9040");
    assert_eq!(r.name, "", "the flag's record is unnamed — never persisted");
    assert_eq!(r.observe_key, backend_registry::OBSERVE_KEY_NAME);
}

/// A blank or whitespace-only flag is NOT a rung: a bare `--observe` means "the configured
/// node", so it falls through to the registry rather than dialling an empty address.
#[test]
fn a_blank_flag_falls_through_to_the_registry() {
    let mut active = rec("the CI box", false);
    active.addr = "<host>:9097".to_string();
    assert_eq!(startup_backend_from(Some("   "), Some(active.clone())).addr, "<host>:9097");
    assert_eq!(startup_backend_from(None, Some(active)).addr, "<host>:9097");
}

/// The registry rung answers with the record ITSELF — key names and arming intact. Rebuilding
/// a synthetic record here is what made a configured launch sign with the wrong key.
#[test]
fn the_registry_rung_answers_with_the_record_itself() {
    let mut active = rec("the CI box", true);
    active.observe_key = "PROD2_OBSERVE_KEY".to_string();
    let r = startup_backend_from(None, Some(active.clone()));
    assert_eq!(r, active, "the record rides through unchanged");
    assert_ne!(r.observe_key, backend_registry::OBSERVE_KEY_NAME, "NOT the synthetic key name");
}

/// Nothing configured at all still yields a record — the viewer opens on the local default
/// instead of refusing to start.
#[test]
fn nothing_configured_still_yields_the_local_default() {
    let r = startup_backend_from(None, None);
    assert_eq!(r.addr, crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR);
    assert_eq!(r.observe_key, backend_registry::OBSERVE_KEY_NAME);
}

// ── the MODE question: `StartupObserve::requested` (the 2026-09-06 P1) ──────────────────────

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// ⚠ THE REGRESSION GATE, half one. An ordinary desktop launch — no `--observe` anywhere in
/// argv, no active registry record — REQUESTS NOTHING. When this was written that made `vike-app`
/// compose [`crate::backend::split_plane::AppMode::LocalCore`]: a local trading core, live exec
/// engines, and an order path that could actually place an order. ⚠ Since the `fat` build was
/// deleted (2026-09-09) the shell observes on every launch (`observes(false, _)` is `true`), so
/// what `requested` decides today is what a first-run state reads, not the mode. It still resolves
/// an ADDRESS (the ladder has no `None` rung, and the Connections UI needs one), and reading THAT
/// as "requested" is exactly the bug — hence both assertions in one test, so a future edit cannot
/// satisfy one by breaking the other.
#[test]
fn a_normal_launch_requests_nothing_and_still_resolves_an_address() {
    let s = startup_observe_from(argv(&["vike-app"]), None);
    assert!(
        !s.requested,
        "no --observe and no active record: observing was never requested, so the app must \
             keep its LOCAL CORE"
    );
    assert_eq!(
        s.record.addr,
        crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR,
        "the address ladder is unchanged — it always answers"
    );
    assert_eq!(
        crate::backend::split_plane::app_mode(
            true,
            crate::backend::split_plane::observes(true, s.requested)
        ),
        Some(crate::backend::split_plane::AppMode::LocalCore),
        "a fat launch that requested nothing is the local-core arm"
    );
}

/// ⚠ THE REGRESSION GATE, half two — the reverse direction, so the fix cannot regress the other
/// way: `--observe ADDR` still takes the observer arm, on the synthetic record, with the local
/// core gone.
#[test]
fn the_observe_flag_requests_observing() {
    let s = startup_observe_from(argv(&["vike-app", "--observe", "the CI box.example:9040"]), None);
    assert!(s.requested, "--observe ADDR is a request to observe");
    assert_eq!(s.record.addr, "the CI box.example:9040");
    assert_eq!(s.record.observe_key, backend_registry::OBSERVE_KEY_NAME, "synthetic record");
    assert_eq!(
        crate::backend::split_plane::app_mode(
            true,
            crate::backend::split_plane::observes(true, s.requested)
        ),
        Some(crate::backend::split_plane::AppMode::ObserveWithFeeds),
        "fat + --observe is the third mode: no local core"
    );
}

/// A BARE `--observe` (the word last on the line, no address after it) is a request, and it is
/// the case the scan had to leave the CI-excluded shell to be tested at all: `args.next()`
/// yields `None`, so the word contributes NO address rung while still meaning "observe the
/// configured node". Reading presence off the ARGUMENT would have made this launch local-core.
#[test]
fn a_bare_observe_flag_is_a_request_with_no_address_rung() {
    let s = startup_observe_from(argv(&["vike-app", "--observe"]), None);
    assert!(s.requested, "the word alone means the configured node");
    assert_eq!(s.record.addr, crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR);
    // …and a blank argument behaves the same way (the ladder already trims it).
    let blank = startup_observe_from(argv(&["vike-app", "--observe", "   "]), None);
    assert!(blank.requested);
    assert_eq!(blank.record.addr, crate::backend::backend_registry::DEFAULT_OBSERVE_ADDR);
}

/// An ACTIVE registry record requests observing on its own — "configure once, launch bare
/// afterwards" (`active_after`'s half of the same story) — and rides through as ITSELF, key
/// names intact, which is #1611's fix and must survive this one.
#[test]
fn an_active_registry_record_requests_observing_and_rides_through_unchanged() {
    let mut active = rec("the CI box", true);
    active.addr = "<host>:9097".to_string();
    let s = startup_observe_from(argv(&["vike-app"]), Some(active.clone()));
    assert!(s.requested, "a configured active node is a request to observe");
    assert_eq!(s.record, active, "the record rides through unchanged — NOT re-synthesized");
    // The flag still OVERRIDES it, synthetic, exactly as the ladder's rung 1 says.
    let flagged =
        startup_observe_from(argv(&["vike-app", "--observe", "other:9040"]), Some(active));
    assert!(flagged.requested);
    assert_eq!(flagged.record.addr, "other:9040");
    assert_eq!(flagged.record.name, "", "the flag's record is unnamed — never persisted");
}

/// The scan reads the word ANYWHERE in argv (after the program name, before or after other
/// flags) and takes the FIRST occurrence's argument — the shape `std::env::args()` hands it.
#[test]
fn the_scan_finds_the_flag_anywhere_in_argv() {
    let s = startup_observe_from(argv(&["vike-app", "--style", "dark", "--observe", "a:1"]), None);
    assert!(s.requested);
    assert_eq!(s.record.addr, "a:1");
    let none = startup_observe_from(argv(&["vike-app", "--observed", "a:1"]), None);
    assert!(!none.requested, "a LONGER flag that merely starts with the word is not a match");
}
