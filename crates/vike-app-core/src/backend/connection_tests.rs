use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use vike_data::{DataClient, DataClientError, DataClientSubscriptionId};
use vike_marketdata::L2BookLevel;
use vike_model::feed_status::DataClientConnectionState::{Connected, Disconnected};

/// The observe poll every connection below is dialed with — the row's default
/// (`preferences.observe_poll_ms`), which these tests do not vary.
const POLL: Duration = Duration::from_millis(50);

fn rec(name: &str, control: bool) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        address: "127.0.0.1:1".to_string(), // closed port: dials fail fast, nothing listens
        observe_key: String::new(),
        control_key: None,
        control,
    }
}

fn status() -> Arc<Mutex<DataClientStatus>> {
    Arc::default()
}

/// The window every dial had before the patience rows existed: what a box with neither row
/// resolves, and what these tests dial with.
fn patience() -> LinkPatience {
    LinkPatience::default()
}

/// The `--observe ADDR` synthetic record is unnamed (never persisted), names no key, and is ARMED
/// so the process-level master gate and the device key stay the only gates on the CLI path.
#[test]
fn the_synthetic_observe_record_is_unnamed_and_armed() {
    let r = cli_observe_record("the CI box.example:9040");
    assert_eq!(r.address, "the CI box.example:9040");
    assert_eq!(r.name, "", "synthetic record is unnamed — never persisted");
    assert_eq!((r.observe_key.as_str(), r.control_key.as_deref()), ("", None), "names no key");
    assert!(r.control, "CLI record must be ARMED: the master gate and the device key decide");
}

/// `connect_backend` refuses the control channel when ANY gate is down: an unarmed record under
/// the master gate, an armed record without it, and (this test binary holds no device key) both up.
/// (No refuse-path dials a control socket. The bridge thread holds no device key either, so it
/// sends nothing; dropping the conn stops-and-joins it.)
#[test]
fn connect_backend_refuses_control_when_any_gate_is_down() {
    // Per-backend gate down (record unarmed), master up: read-only.
    let conn =
        connect_backend(&rec("unarmed", false), true, POLL, patience(), status(), |_| {}, || {});
    assert!(conn.control.is_none(), "unarmed record must never mount control");
    assert_eq!(conn.record, rec("unarmed", false));
    drop(conn); // stop + join the bridge thread

    // Master gate down, record armed: read-only.
    let conn =
        connect_backend(&rec("armed", true), false, POLL, patience(), status(), |_| {}, || {});
    assert!(conn.control.is_none(), "master gate off must refuse control even for an armed record");
    drop(conn);

    // Both gates up but no device key: nothing is sent and the observer stays read-only.
    let conn =
        connect_backend(&rec("armed", true), true, POLL, patience(), status(), |_| {}, || {});
    assert!(conn.control.is_none(), "no device key degrades to read-only, never Some");
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
    fn subscribe_bars(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        Ok(DataClientSubscriptionId(1))
    }
    fn subscribe_quotes(&mut self, _: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        Ok(DataClientSubscriptionId(2))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        Ok(DataClientSubscriptionId(3))
    }
    fn subscribe_book(&mut self, _: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        Ok(DataClientSubscriptionId(4))
    }
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
        self.log.lock().unwrap().push(format!("{}:unsub {}", self.venue, id.0));
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push(format!("{}:shutdown", self.venue));
    }
}

/// Owns every field of [`BackendSwitchSlots`], POPULATED — the CoreSyncState-shaped fixture the
/// brief asks for, widened to the whole clear surface.
struct Fixture {
    charts: HashMap<String, state::ChartState>,
    tick_volume_aggregators: HashMap<String, (String, String, TickVolAgg)>,
    orderflow_aggregators: HashMap<String, (String, String, OrderflowAgg)>,
    published: Vec<crate::ui::series_follow::PublishedSeries>,
    last_sequence: u64,
    status_line: DataClientStatus,
    feeds: DataClientMap,
    subscriptions: HashMap<String, DataClientSubscriptionId>,
    spawned: HashSet<String>,
    unroutable: HashSet<String>,
    feed_retries: DatahubFeedRetries,
    trades: TickStore,
    books: L2BookStore,
    direct_bars: StoredBarStore,
    trade_depth: HashMap<(String, String), L2BookSubscriptions>,
    polymarket_subscriptions: HashMap<String, L2BookSubscriptions>,
    hidden: HashSet<String>,
    feed_status: Arc<Mutex<DataClientStatus>>,
    log: Log,
}

fn tick(symbol: &str) -> vike_marketdata::TickTrade {
    vike_marketdata::TickTrade {
        timestamp: 1,
        local_timestamp: 1,
        price: 100.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: symbol.into(),
    }
}

fn populated() -> Fixture {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut feeds: DataClientMap = HashMap::new();
    feeds.insert("binance", Box::new(RecClient { venue: "binance", log: Arc::clone(&log) }));
    feeds.insert("okx", Box::new(RecClient { venue: "okx", log: Arc::clone(&log) }));
    feeds.insert("polymarket", Box::new(RecClient { venue: "polymarket", log: Arc::clone(&log) }));

    let mut charts = HashMap::new();
    // The fixture is the shipped desktop's shape (feeds + backend + the bar store under
    // `BackendBarPlane::BackendStore`), so the session/plane SPLIT has two observable chart families:
    // - the klines — `deribit:BTC-PERPETUAL@1m` and `BTCUSDT@1m`: backend A's streamed tail or
    //   a read of A's hist store, either way backend-session state the switch clear must drop;
    // - `BTCUSDT@100t` — tape-rendered, not A's session — kept, as since B2.
    charts.insert("deribit:BTC-PERPETUAL@1m".to_string(), state::ChartState::default());
    charts.insert("binance:BTCUSDT@1m".to_string(), state::ChartState::default());
    charts.insert("binance:BTCUSDT@100t".to_string(), state::ChartState::default());
    let mut tick_volume_aggregators = HashMap::new();
    tick_volume_aggregators.insert(
        "binance:BTCUSDT@100t".to_string(),
        (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            TickVolAgg::new(&vike_orderflow::tickvol::BarKind::Tick(100)).expect("tick agg"),
        ),
    );
    let mut orderflow_aggregators = HashMap::new();
    orderflow_aggregators.insert(
        "binance:BTCUSDT@1m".to_string(),
        ("binance".to_string(), "BTCUSDT".to_string(), OrderflowAgg::new(0.5)),
    );
    let mut subscriptions = HashMap::new();
    subscriptions.insert("binance:BTCUSDT@1m".to_string(), DataClientSubscriptionId(101));
    subscriptions.insert("okx:ETHUSDT@1m".to_string(), DataClientSubscriptionId(201));

    let mut feed_retries = DatahubFeedRetries::default();
    feed_retries.note_missing(&crate::data::datahub::feed_retry::DatahubFeedRetryKey::series(
        "deribit:X@1m",
    ));

    let trades = TickStore::default();
    trades.push("binance", &tick("BTCUSDT"));
    let books = L2BookStore::default();
    books.update(
        "binance",
        "BTCUSDT",
        0.1,
        vec![L2BookLevel::new(100.0, 1.0)],
        vec![L2BookLevel::new(100.1, 1.0)],
        1,
    );
    let direct_bars = StoredBarStore::default();
    direct_bars.seed(
        "binance",
        "BTCUSDT",
        "1m",
        vec![vike_marketdata::Bar {
            timestamp: 60_000,
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
        tick_volume_aggregators,
        orderflow_aggregators,
        // A's published series — backend-session state `clear_session_state` must drop, or the
        // symbol picker offers B a series A had and `follow_backend` can retarget onto it.
        published: vec![crate::ui::series_follow::PublishedSeries::new(
            "deribit",
            "BTC-PERPETUAL",
            "1m",
        )],
        last_sequence: 42,
        status_line: DataClientStatus::new(Connected, "OBSERVING the CI box"),
        feeds,
        subscriptions,
        spawned: ["binance:BTCUSDT@1m".to_string(), "okx:ETHUSDT@1m".to_string()].into(),
        unroutable: ["deribit:X@1m".to_string()].into(),
        feed_retries,
        trades,
        books,
        direct_bars,
        // One Binance and one OKX depth stream, so the clear must stop depth streams across two
        // venues; one cockpit token with both legs live. Ids are disjoint from `subscriptions`' so the log
        // lines are unambiguous.
        trade_depth: [
            (
                ("binance".to_string(), "BTCUSDT".to_string()),
                L2BookSubscriptions { book: DataClientSubscriptionId(150), trades: None },
            ),
            (
                ("okx".to_string(), "ETH-USDT-SWAP".to_string()),
                L2BookSubscriptions { book: DataClientSubscriptionId(250), trades: None },
            ),
        ]
        .into(),
        polymarket_subscriptions: [(
            "1071".to_string(),
            L2BookSubscriptions {
                book: DataClientSubscriptionId(450),
                trades: Some(DataClientSubscriptionId(451)),
            },
        )]
        .into(),
        hidden: ["binance:ETHUSDT@1m".to_string()].into(),
        feed_status: Arc::new(Mutex::new(DataClientStatus::new(Connected, "OBSERVING the CI box"))),
        log,
    }
}

impl Fixture {
    fn slots(&mut self) -> BackendSwitchSlots<'_> {
        BackendSwitchSlots {
            charts: &mut self.charts,
            tick_volume_aggregators: &mut self.tick_volume_aggregators,
            orderflow_aggregators: &mut self.orderflow_aggregators,
            published: &mut self.published,
            last_sequence: &mut self.last_sequence,
            status: &mut self.status_line,
            feeds: &mut self.feeds,
            subscriptions: &mut self.subscriptions,
            spawned: &mut self.spawned,
            unroutable: &mut self.unroutable,
            feed_retries: &mut self.feed_retries,
            trades: &self.trades,
            books: &self.books,
            direct_bars: Some(&self.direct_bars),
            // The shipped desktop's plane: the store holds reads of the backend's hist store.
            bar_plane: split_plane::BackendBarPlane::BackendStore,
            trade_depth: &mut self.trade_depth,
            polymarket_subscriptions: &mut self.polymarket_subscriptions,
            hidden: &mut self.hidden,
            feed_status: &self.feed_status,
        }
    }

    /// The backend session is gone (snapshot-rendered charts, seq gate, status, hidden) —
    /// [`clear_session_state`]'s whole contract on the populated fixture.
    fn assert_backend_session_cleared(&self) {
        assert!(
            !self.charts.contains_key("deribit:BTC-PERPETUAL@1m"),
            "the snapshot-rendered (backend-tail kline) chart is backend-session state and \
                 must clear"
        );
        assert!(
            !self.charts.contains_key("binance:BTCUSDT@1m"),
            "under BackendStore a kline chart's bars came from THIS backend — it must clear"
        );
        assert!(
            self.direct_bars.keys().is_empty(),
            "…and so must the store itself: A's history may not paint under B"
        );
        assert_eq!(self.last_sequence, 0, "last_sequence");
        assert!(self.published.is_empty(), "published (A's series may not be offered under B)");
        assert_eq!(self.status_line, DataClientStatus::default(), "status");
        assert!(self.hidden.is_empty(), "hidden");
    }

    /// The local feed plane is UNTOUCHED — every ledger, fold, cache and in-flight batch
    /// still present. ⚠ Consuming reads (`try_recv`, `TickStore::drain`) — call this once,
    /// as a test's final assertion block.
    fn assert_feed_plane_intact(&self) {
        assert!(
            self.charts.contains_key("binance:BTCUSDT@100t"),
            "the tape-rendered chart is venue truth and survives a switch"
        );
        assert_eq!(self.subscriptions.len(), 2, "subs — the venue streams keep flowing");
        assert_eq!(self.spawned.len(), 2, "spawned — still the fold filter for B's bars");
        assert_eq!(self.unroutable.len(), 1, "unroutable");
        assert!(!self.feed_retries.is_empty(), "feed_retries");
        assert!(
            self.tick_volume_aggregators.contains_key("binance:BTCUSDT@100t"),
            "tick_volume_aggregators"
        );
        assert!(
            self.orderflow_aggregators.contains_key("binance:BTCUSDT@1m"),
            "orderflow_aggregators"
        );
        assert_eq!(self.trade_depth.len(), 2, "trade_depth — the Trade windows keep painting");
        assert_eq!(
            self.polymarket_subscriptions.len(),
            1,
            "poly_subs — the cockpit keeps painting"
        );
        assert_eq!(self.trades.drain("binance", "BTCUSDT").len(), 1, "trade tape survives");
        assert!(self.books.get("binance", "BTCUSDT").is_some(), "books survive");
    }
}

/// THE B2 SWITCH CONTRACT — feeds are NOT backend-session state. A switch clears the backend
/// session (the kline charts and the bar store — both are reads of THIS backend, so backend A's
/// history may not paint under B — the seq gate, status, hidden) and performs **ZERO feed
/// unsubscribes and zero shutdowns**: the recording clients must observe nothing at all, and
/// every local-plane slot — ledgers, tape folds, caches — stays intact.
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

/// `switch_backend(target: None)` — the Disconnect path: old conn stopped and dropped, the
/// empty snapshot published (so the render loop never keeps painting A's data), the backend
/// session cleared — the feed plane still running (a disconnect stays IN observe mode, so the
/// ladder/tape keep painting) — and the feed-status line says so.
#[test]
fn a_disconnect_stops_the_old_backend_and_blanks_the_published_snapshot() {
    let mut fx = populated();
    let mut active = Some(connect_backend(
        &rec("the CI box", false),
        false,
        POLL,
        patience(),
        status(),
        |_| {},
        || {},
    ));

    let published: Arc<Mutex<Option<Arc<vike_execution::CoreSnapshot>>>> =
        Arc::new(Mutex::new(None));
    let repainted = Arc::new(AtomicBool::new(false));
    let (p, r) = (Arc::clone(&published), Arc::clone(&repainted));
    switch_backend(
        &mut active,
        None,
        false,
        POLL,
        patience(),
        fx.slots(),
        move |snapshot| *p.lock().unwrap() = Some(snapshot),
        move || r.store(true, Ordering::Relaxed),
    );

    assert!(active.is_none(), "disconnect leaves no active backend");
    fx.assert_backend_session_cleared();
    let snapshot = published.lock().unwrap().clone().expect("an empty snapshot must be published");
    assert!(snapshot.bars.is_empty() && snapshot.orders.is_empty(), "published snapshot is EMPTY");
    assert!(repainted.load(Ordering::Relaxed), "a repaint is requested so the blank shows");
    assert_eq!(
        *fx.feed_status.lock().unwrap(),
        DataClientStatus::new(Disconnected, "disconnected")
    );
    assert!(fx.log.lock().unwrap().is_empty(), "a disconnect touches no local feed stream");
    fx.assert_feed_plane_intact();
}

/// `switch_backend(target: Some)` — the Connect path: the old conn is replaced by one dialed
/// from the NEW record, over a cleared backend session.
#[test]
fn a_switch_replaces_the_active_backend_with_the_target_record() {
    let mut fx = populated();
    let mut active =
        Some(connect_backend(&rec("old", false), false, POLL, patience(), status(), |_| {}, || {}));

    let target = rec("new", false);
    switch_backend(&mut active, Some(&target), false, POLL, patience(), fx.slots(), |_| {}, || {});

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
    let file = BackendRegistry {
        backends: vec![rec("a", false), rec("b", true)],
        active: None,
        refused: None,
    };

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
    assert_eq!(rows[0].record.address, "cli.example:9040");
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
    let file = BackendRegistry {
        backends: vec![rec("a", false), rec("b", true)],
        active: None,
        refused: None,
    };

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
    let empty = BackendRegistry::default();
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

/// The `--observe` connection is UNLISTED, so it can never be named: `active_record` resolves a
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
    assert_eq!(r.address, "other.example:9040");
    assert_eq!(r.name, "", "the flag's record is unnamed — never persisted");
    assert!(r.control, "the synthetic record is armed");
}

/// A blank or whitespace-only flag is NOT a rung: a bare `--observe` means "the configured
/// node", so it falls through to the registry rather than dialling an empty address.
#[test]
fn a_blank_flag_falls_through_to_the_registry() {
    let mut active = rec("the CI box", false);
    active.address = "<host>:9097".to_string();
    assert_eq!(startup_backend_from(Some("   "), Some(active.clone())).address, "<host>:9097");
    assert_eq!(startup_backend_from(None, Some(active)).address, "<host>:9097");
}

/// The registry rung answers with the record ITSELF — name and arming intact, never a synthetic
/// record rebuilt around its address.
#[test]
fn the_registry_rung_answers_with_the_record_itself() {
    let active = rec("the CI box", false);
    let r = startup_backend_from(None, Some(active.clone()));
    assert_eq!(r, active, "the record rides through unchanged");
    assert_ne!(r, cli_observe_record(&active.address), "NOT the synthetic record");
}

/// Nothing configured at all still yields a record — the viewer opens on the local default
/// instead of refusing to start.
#[test]
fn nothing_configured_still_yields_the_local_default() {
    let r = startup_backend_from(None, None);
    assert_eq!(r, cli_observe_record(vike_config::config::DEFAULT_TRADEHUB_ADDRESS));
}

// ── the MODE question: `BackendStartup::requested` (the 2026-09-06 P1) ──────────────────────

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// ⚠ THE REGRESSION GATE, half one. An ordinary desktop launch — no `--observe` anywhere in
/// argv, no active registry record — REQUESTS NOTHING. When this was written that made `vike-app`
/// compose its local-core arm (`BackendMode::LocalCore`, deleted in 2026-10): a local trading core,
/// live exec engines, and an order path that could actually place an order. ⚠ Since the `fat` build was
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
        s.record.address,
        vike_config::config::DEFAULT_TRADEHUB_ADDRESS,
        "the address ladder is unchanged — it always answers"
    );
    assert!(
        !crate::backend::split_plane::observes(true, s.requested),
        "a fat launch that requested nothing does not observe: it was the local-core arm"
    );
}

/// ⚠ THE REGRESSION GATE, half two — the reverse direction, so the fix cannot regress the other
/// way: `--observe ADDR` still takes the observer arm, on the synthetic record, with the local
/// core gone.
#[test]
fn the_observe_flag_requests_observing() {
    let s = startup_observe_from(argv(&["vike-app", "--observe", "the CI box.example:9040"]), None);
    assert!(s.requested, "--observe ADDR is a request to observe");
    assert_eq!(s.record.address, "the CI box.example:9040");
    assert_eq!(s.record, cli_observe_record("the CI box.example:9040"), "synthetic record");
    assert!(
        crate::backend::split_plane::observes(true, s.requested),
        "fat + --observe observes: no local core"
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
    assert_eq!(s.record.address, vike_config::config::DEFAULT_TRADEHUB_ADDRESS);
    // …and a blank argument behaves the same way (the ladder already trims it).
    let blank = startup_observe_from(argv(&["vike-app", "--observe", "   "]), None);
    assert!(blank.requested);
    assert_eq!(blank.record.address, vike_config::config::DEFAULT_TRADEHUB_ADDRESS);
}

/// An ACTIVE registry record requests observing on its own — "configure once, launch bare
/// afterwards" (`active_after`'s half of the same story) — and rides through as ITSELF, name
/// and arming intact, which is #1611's fix and must survive this one.
#[test]
fn an_active_registry_record_requests_observing_and_rides_through_unchanged() {
    let mut active = rec("the CI box", true);
    active.address = "<host>:9097".to_string();
    let s = startup_observe_from(argv(&["vike-app"]), Some(active.clone()));
    assert!(s.requested, "a configured active node is a request to observe");
    assert_eq!(s.record, active, "the record rides through unchanged — NOT re-synthesized");
    // The flag still OVERRIDES it, synthetic, exactly as the ladder's rung 1 says.
    let flagged =
        startup_observe_from(argv(&["vike-app", "--observe", "other:9040"]), Some(active));
    assert!(flagged.requested);
    assert_eq!(flagged.record.address, "other:9040");
    assert_eq!(flagged.record.name, "", "the flag's record is unnamed — never persisted");
}

/// The scan reads the word ANYWHERE in argv (after the program name, before or after other
/// flags) and takes the FIRST occurrence's argument — the shape `std::env::args()` hands it.
#[test]
fn the_scan_finds_the_flag_anywhere_in_argv() {
    let s = startup_observe_from(argv(&["vike-app", "--style", "dark", "--observe", "a:1"]), None);
    assert!(s.requested);
    assert_eq!(s.record.address, "a:1");
    let none = startup_observe_from(argv(&["vike-app", "--observed", "a:1"]), None);
    assert!(!none.requested, "a LONGER flag that merely starts with the word is not a match");
}
