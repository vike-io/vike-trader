//! **The price-only protections END TO END, through the REAL assembly** (`live_runtime_with`): the
//! wiring that carries a price-only venue (`crate::feeds::price_only_venues`) into the link dead-man's
//! venue set, into `CoreConfig::price_only`, and from a disclosed link loss to the halt of THAT venue.
//!
//! The pieces are tested alone elsewhere, on fake clocks and hand-built cores: the core's sweeps and the
//! holdings gate in `crates/vike-core/src/runtime/tests/link_deadman.rs` and
//! `crates/vike-core/src/runtime/tests/safe_state/drawdown_latch.rs`, the config folds in
//! `crates/vike-tradehub/src/tradehub_cli/tests/deadman.rs`, the hub links in
//! `crates/vike-tradehub/tests/hop_kill_proofs.rs`. None of them can notice a join that went missing between
//! `price_only_venues`, `link_deadman_config_from_policy`, `price_only_config` and the core those build.
//! These drive the assembled core and read what an operator reads: the published snapshot.
//!
//! # What runs REAL
//!
//! `live_runtime_with` (plan gate, `assemble_runtime`, the paper engines of two venues, the live core thread
//! on its wall clock), `wire_venue_feeds` for the mounted venue and `wire_price_only_feeds` for the other,
//! the `CoreLaneSink` chain that carries a feed's `stream_status` onto the tick lane, the link dead-man's
//! dispatch arm and sweep timer, the holdings gate, the venue-scoped cancel and halt.
//!
//! # What is SCRIPTED
//!
//! The datahub. [`PriceOnlyCtors`] hands each feed arm a [`QuietFeed`] and keeps the sink the arm built, so
//! the test discloses exactly what a `StratFeed` discloses when its session drops: a `GapStart` under
//! `STRAT_LINK_STREAM` per held symbol, and the `Live` that closes it. The venue is the paper exchange (no
//! network). Nothing dials.
//!
//! # What this cannot show
//!
//! The grace is a policy row, so it is made short here (the cadence of the dead-man's sweep follows it).
//! `PRICE_ONLY_SWEEP`, the margin-call and drawdown cadence, is a production constant, so the sweep-driven
//! scenarios wait for its first firing in real time.

use super::*;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use vike_exec::{Command, ExecutionClient, OrderIntent, SymbolAdmission};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled, OrderSubmitted};
use vike_model::events::{PositionSide, TradeId};
use vike_model::{AccountTier, OrderRequest, OrderStatus, OrderType, StreamStatus, SymbolGrid};
use vike_model::{TradingState, now_ms};
use vike_venues::link_deadman::{STRAT_LINK_STREAM, TRADE_LINK_STREAM};

/// The link dead-man's grace in these tests. Short, because the sweep runs at half of it; long enough that
/// a loaded box does not make "inside the grace" a race (the tests assert a lower bound, never an upper one).
const GRACE_MS: u64 = 600;
/// How long a state the core must reach is waited for, seconds.
const WAIT_SECS: u64 = 15;
/// How long a state the core must NOT reach is watched: five graces.
const QUIET: Duration = Duration::from_millis(GRACE_MS * 5);

/// The wired market of each venue (`crates/vike-venues/src/venue_wiring.rs`), the symbol its paper engine runs.
const HL: &str = "hyperliquid";
const HL_SYMBOL: &str = "BTC";
const DERIBIT: &str = "deribit";
const DERIBIT_SYMBOL: &str = "BTC-PERPETUAL";

/// Every engine's seed (the first mount's `seed_cash`, see [`seeded_buy_hold_mount`]).
const SEED: f64 = 1_000.0;
/// The loss the sweep tests book: a long of 0.9 coin at 1,000, marked at 300, is 630 down, 31.5 % of the two
/// seeds the drawdown latch measures against its 25 % trip.
const LONG_QTY: f64 = 0.9;
const ENTRY_PX: f64 = 1_000.0;
const CRASH_PX: f64 = 300.0;
/// The first firing of the price-only sweep is `PRICE_ONLY_SWEEP` (30 s) after the core starts, the second
/// 30 s later; the budget covers a loaded box.
const SWEEP_WAIT_SECS: u64 = 100;

/// The sinks the feed arms built, by venue: what a `StratFeed` writes its disclosures into.
type Sinks = Arc<Mutex<HashMap<String, Arc<dyn LiveDataSink>>>>;

/// A feed that subscribes everything and delivers nothing: the arm's `subscribe_*` calls succeed, no frame
/// ever arrives, so no strategy acts and no paper order fills. The same registry bookkeeping as
/// `ScriptedFeed`, so the arm's teardown is the production path.
struct QuietFeed {
    registry: FeedRegistry,
}

impl QuietFeed {
    fn idle(&mut self, name: String) -> Result<SubscriptionId, LiveDataError> {
        self.registry.spawn(name, |_stop| {}).map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }
}

impl DataClient for QuietFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.idle(format!("quiet-{symbol}@{interval}"))
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.idle(format!("quiet-{symbol}-quotes"))
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.idle(format!("quiet-{symbol}-trades"))
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.idle(format!("quiet-{symbol}-book"))
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

/// The test's [`FeedCtors`]: every `client` dial is a [`QuietFeed`] whose sink is kept, and the CEX arm's
/// concrete `strat` dial is refused (a refusal only leaves that price-only venue without a feed, said at
/// `warn`; none of these tests trades a CEX venue).
struct PriceOnlyCtors {
    sinks: Sinks,
}

impl FeedCtors for PriceOnlyCtors {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(format!("{venue}: this scripted constructor builds client() feeds only"))
    }

    fn client(
        &self,
        venue: &str,
        _network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        self.sinks.lock().expect("the sink table").insert(venue.to_string(), sink);
        Ok(Box::new(QuietFeed { registry: FeedRegistry::new() }))
    }
}

/// `super::buy_hold_mount`'s profile with a SEED of [`SEED`] in the toml (its default is 100,000, which would
/// need a loss of 50,000 to trip the latch): every engine of the node is seeded from the first mount's.
/// The strategy never acts, its feed delivers no bar.
fn seeded_buy_hold_mount(venue: &str) -> ResolvedAdapter {
    let symbol =
        super::wired_symbol(venue).unwrap_or_else(|| panic!("assemble_runtime mounts {venue}"));
    let toml = format!(
        "venue = \"{venue}\"\nsymbol = \"{symbol}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         seed_cash = {SEED:?}\n\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("the {venue} profile parses and validates: {e:?}"));
    row.validate_for_live().expect("…and passes the pure live gate");
    let cfg = row.to_maker_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    ResolvedAdapter { row, cfg, spec, strategy }
}

/// A live core assembled by the real `live_runtime_with` over two paper accounts: `mounted` (a strategy
/// mount drives it) and `price_only` (no mount names it, so `wire_price_only_feeds` feeds it and the core
/// sweeps it).
struct Rig {
    handle: vike_core::CoreHandle,
    teardown: crate::feeds::LiveTeardown,
    sinks: Sinks,
    /// The next trade id [`Self::fill`] mints: one per fill, so the engine's duplicate guard never folds
    /// one away.
    trades: AtomicU64,
    _state: tempfile::TempDir,
}

impl Rig {
    fn start(mounted: &str, price_only: &str, grace_ms: u64) -> Rig {
        let mut seam = super::SeamPolicy::armed(&[
            (mounted, AccountTier::Paper),
            (price_only, AccountTier::Paper),
        ]);
        seam.policy.link_deadman_grace_ms = Some(grace_ms);
        Rig::start_over(mounted, price_only, &seam, None, &super::LocalCtor)
    }

    /// [`Self::start`] over a seam and a backend of the test's choosing.
    fn start_over(
        mounted: &str,
        price_only: &str,
        seam: &super::SeamPolicy,
        budget: Option<vike_model::ProfileRisk>,
        backend: &dyn BackendCtor,
    ) -> Rig {
        // A HALT sentinel on this box would refuse every opening order and read as the dead-man's doing.
        let halt = vike_boot::halt::halt_path_from_env();
        assert!(!halt.exists(), "a HALT sentinel exists at {}: remove it first", halt.display());

        let sinks: Sinks = Arc::default();
        let state = tempfile::tempdir().expect("a throwaway state dir");
        let (handle, teardown, _live, _late_arm, _not_running) = super::seam_live_runtime_over(
            vec![seeded_buy_hold_mount(mounted)],
            budget,
            seam,
            HashMap::new(),
            state.path(),
            &PriceOnlyCtors { sinks: Arc::clone(&sinks) },
            backend,
        )
        .expect("the live mount stands up over the two accounts");
        let rig = Rig { handle, teardown, sinks, trades: AtomicU64::new(1), _state: state };
        for venue in [mounted, price_only] {
            // The core publishes its first real snapshot when it first folds something; a `Live` of a link
            // that was never down is the harmless something (it opens and closes no window).
            let symbol = super::wired_symbol(venue).expect("a wired market");
            rig.strat_link_up(venue, symbol);
            assert!(
                wait_until(WAIT_SECS, || {
                    rig.handle.snapshot().portfolio.venues.iter().any(|b| b.route_key == venue)
                }),
                "{venue} has an engine: {}",
                rig.dump()
            );
            assert!(
                rig.sinks.lock().expect("the sink table").contains_key(venue),
                "{venue} has a feed arm wired by the assembly"
            );
        }
        rig
    }

    fn finish(self) {
        super::tear_down(self.handle, self.teardown);
    }

    /// What a failed assertion needs: every order, every venue's state, the ring.
    fn dump(&self) -> String {
        let s = self.handle.snapshot();
        format!(
            "capital_base {} equity {}; orders {:?}; states {:?}; positions {:?}; events {:?}",
            s.portfolio.capital_base,
            s.portfolio.equity,
            s.orders.iter().map(|o| (&o.client_order_id, &o.venue, o.status)).collect::<Vec<_>>(),
            s.portfolio.venues.iter().map(|b| (&b.route_key, b.trading_state)).collect::<Vec<_>>(),
            s.portfolio
                .venues
                .iter()
                .flat_map(|b| b.positions.iter())
                .map(|p| (p.venue.as_str(), p.size, p.unrealized, p.mark_source))
                .collect::<Vec<_>>(),
            s.recent_events
        )
    }

    /// The trading state the published snapshot carries for `venue`'s engine.
    fn state_of(&self, venue: &str) -> TradingState {
        let snap = self.handle.snapshot();
        let block = snap.portfolio.venues.iter().find(|b| b.route_key == venue);
        block.unwrap_or_else(|| panic!("no engine for {venue}: {}", self.dump())).trading_state
    }

    fn status(&self, coid: &str) -> Option<OrderStatus> {
        self.handle.snapshot().orders.iter().find(|o| o.client_order_id == coid).map(|o| o.status)
    }

    fn resting(&self, coid: &str) -> bool {
        matches!(self.status(coid), Some(OrderStatus::Submitted | OrderStatus::Accepted))
    }

    /// Send a limit buy far below any market, so it rests: the dead-man's holding.
    fn rest(&self, venue: &str, symbol: &str, coid: &str) {
        // 0.05 coin at 1,000 is 50 of a 1,000 seed: inside the margin gate, never filled.
        self.limit_buy(venue, symbol, coid, 0.05, 1_000.0);
    }

    fn limit_buy(&self, venue: &str, symbol: &str, coid: &str, qty: f64, price: f64) {
        self.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: venue.into(),
            symbol: symbol.into(),
            side: 1,
            qty,
            order_type: OrderType::Limit,
            price: Some(price),
            ..Default::default()
        }))));
    }

    /// The sink `venue`'s feed arm was built on: what its feed writes into.
    fn sink_of(&self, venue: &str) -> Arc<dyn LiveDataSink> {
        let sinks = self.sinks.lock().expect("the sink table");
        Arc::clone(sinks.get(venue).unwrap_or_else(|| panic!("no feed was wired for {venue}")))
    }

    /// Disclose `status` of `venue`'s `symbol` under the link word `stream`, through the sink the venue's
    /// feed arm was built on: the path a `StratFeed` or the gatewayhub client takes into the core.
    fn disclose(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.sink_of(venue).stream_status(venue, symbol, stream, status);
    }

    /// A quote of `venue`'s `symbol` around `px`, stamped `ts` (epoch ms), delivered as the price tap's
    /// feed delivers it: the number the price-only sweeps mark the engine's position with.
    fn quote(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.sink_of(venue).quote(
            venue,
            symbol,
            vike_marketdata::QuoteTick {
                ts,
                local_ts: now_ms(),
                bid: px - 0.5,
                ask: px + 0.5,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: symbol.to_string(),
            },
        );
    }

    /// The venue fills `coid` (a long of `qty` at `px`, stamped `ts`) through the core's event door, as a
    /// live client's user-data stream would: the engine books the position. The paper exchange fills on
    /// bars and a price-only venue has none.
    fn fill(&self, venue: &str, symbol: &str, coid: &str, qty: f64, px: f64, ts: i64) {
        let fill = FillEvent {
            trade_id: TradeId::prefixed("e2e", self.trades.fetch_add(1, Ordering::Relaxed)),
            client_order_id: coid.into(),
            venue: venue.into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        let door = self.handle.event_sender().routed(venue);
        door.blocking_send(Event::Fill(fill)).expect("the core takes the fill");
    }

    /// Is `needle` in a line of the recent-events ring?
    fn noted(&self, needle: &str) -> bool {
        self.handle.snapshot().recent_events.iter().any(|line| line.contains(needle))
    }

    /// The datahub session of `venue`'s feed dropped.
    fn strat_link_down(&self, venue: &str, symbol: &str) {
        self.disclose(
            venue,
            symbol,
            STRAT_LINK_STREAM,
            StreamStatus::GapStart { at_ts_ms: now_ms() },
        );
    }

    /// …and came back.
    fn strat_link_up(&self, venue: &str, symbol: &str) {
        self.disclose(
            venue,
            symbol,
            STRAT_LINK_STREAM,
            StreamStatus::Live { gap_started_ts_ms: None },
        );
    }

    /// Rest an order on `venue` and wait until the core holds it live.
    fn rest_and_wait(&self, venue: &str, symbol: &str, coid: &str) {
        self.rest(venue, symbol, coid);
        assert!(
            wait_until(WAIT_SECS, || self.resting(coid)),
            "{coid} on {venue} never rested: {}",
            self.dump()
        );
    }

    /// Does `venue`'s engine hold a non-flat position?
    fn holds(&self, venue: &str) -> bool {
        let snap = self.handle.snapshot();
        let block = snap.portfolio.venues.iter().find(|b| b.route_key == venue);
        block.is_some_and(|b| b.positions.iter().any(|p| p.size.abs() > 0.0))
    }

    /// `venue`'s engine, as the snapshot publishes it.
    fn block_of(&self, venue: &str) -> vike_exec::VenueBlock {
        let snap = self.handle.snapshot();
        let block = snap.portfolio.venues.iter().find(|b| b.route_key == venue);
        block.unwrap_or_else(|| panic!("no engine for {venue}: {}", self.dump())).clone()
    }

    /// A long of [`LONG_QTY`] at [`ENTRY_PX`] on `venue`'s own symbol: a resting order, then the venue's fill
    /// (stamped `fill_ts`).
    fn go_long(&self, venue: &str, symbol: &str, fill_ts: i64) {
        let coid = format!("pos-{venue}");
        self.limit_buy(venue, symbol, &coid, LONG_QTY, ENTRY_PX);
        assert!(
            wait_until(WAIT_SECS, || self.resting(&coid)),
            "the entry order on {venue} never rested: {}",
            self.dump()
        );
        self.fill(venue, symbol, &coid, LONG_QTY, ENTRY_PX, fill_ts);
        assert!(
            wait_until(WAIT_SECS, || self.holds(venue)),
            "the fill on {venue} opened no position: {}",
            self.dump()
        );
    }
}

/// **A price-only venue whose datahub session drops with an order resting is cancelled and halted after the
/// grace, and it alone** (the whole chain: `price_only_venues` -> the link dead-man's set -> the core's
/// holdings gate -> the venue-scoped trip). Deribit has no mount here; hyperliquid does and is the PRIMARY
/// engine. The mounted venue's resting order survives, its engine stays `Active`, it keeps accepting
/// orders, the halted venue refuses an opening order, and no HALT sentinel is written.
#[test]
fn a_dead_datahub_session_halts_the_price_only_venue_alone_and_cancels_only_its_orders() {
    let rig = Rig::start(HL, DERIBIT, GRACE_MS);
    rig.rest_and_wait(DERIBIT, DERIBIT_SYMBOL, "po1");
    rig.rest_and_wait(HL, HL_SYMBOL, "mo1");

    let down_at = Instant::now();
    rig.strat_link_down(DERIBIT, DERIBIT_SYMBOL);
    assert!(
        wait_until(WAIT_SECS, || rig.state_of(DERIBIT) == TradingState::Halted),
        "the price-only venue was never halted: {}",
        rig.dump()
    );
    // …after the grace, never before it (a lower bound, so a slow box cannot fail it).
    assert!(
        down_at.elapsed() >= Duration::from_millis(GRACE_MS - 50),
        "halted {:?} after the link dropped, inside the {GRACE_MS} ms grace",
        down_at.elapsed()
    );
    assert!(
        wait_until(WAIT_SECS, || rig.status("po1") == Some(OrderStatus::Canceled)),
        "the dead venue's resting order was not cancelled: {}",
        rig.dump()
    );

    // The scope of the trip: the other venue's order, state and door are untouched.
    assert!(rig.resting("mo1"), "the mounted venue's order was pulled: {}", rig.dump());
    assert_eq!(rig.state_of(HL), TradingState::Active, "the mounted venue was halted");
    // No sentinel: a venue-scoped halt is in-process (decision 0180), so no other venue's boundary can see it.
    let halt = vike_boot::halt::halt_path_from_env();
    assert!(!halt.exists(), "the link dead-man wrote the HALT sentinel at {}", halt.display());

    // The halted venue refuses an opening order; the mounted one still takes one. Sent in this order on the
    // one ingest lane, so once the second rests the first has been judged.
    rig.rest(DERIBIT, DERIBIT_SYMBOL, "po2");
    rig.rest_and_wait(HL, HL_SYMBOL, "mo2");
    assert!(!rig.resting("po2"), "the halted venue took an opening order: {}", rig.dump());
    rig.finish();
}

/// **The halted venue is re-armed when its link is Live again**, to the state it held before (here `Active`),
/// and takes orders again; the order the trip cancelled stays cancelled. The price-only venue here is
/// hyperliquid, the PRIMARY engine, and the mounted deribit is untouched at every step.
#[test]
fn a_price_only_venue_halted_by_a_dead_link_is_released_when_the_link_is_live_again() {
    let rig = Rig::start(DERIBIT, HL, GRACE_MS);
    rig.rest_and_wait(HL, HL_SYMBOL, "po1");
    rig.rest_and_wait(DERIBIT, DERIBIT_SYMBOL, "mo1");

    rig.strat_link_down(HL, HL_SYMBOL);
    assert!(
        wait_until(WAIT_SECS, || rig.state_of(HL) == TradingState::Halted),
        "the price-only venue was never halted: {}",
        rig.dump()
    );
    assert_eq!(rig.state_of(DERIBIT), TradingState::Active, "the mounted venue was halted");
    assert!(rig.resting("mo1"), "the mounted venue's order was pulled: {}", rig.dump());

    rig.strat_link_up(HL, HL_SYMBOL);
    assert!(
        wait_until(WAIT_SECS, || rig.state_of(HL) == TradingState::Active),
        "the venue was not released by the Live that closed its outage: {}",
        rig.dump()
    );
    assert_eq!(rig.status("po1"), Some(OrderStatus::Canceled), "the cancelled order came back");
    rig.rest_and_wait(HL, HL_SYMBOL, "po2");
    assert_eq!(rig.state_of(DERIBIT), TradingState::Active);
    assert!(rig.resting("mo1"), "the mounted venue's order was pulled: {}", rig.dump());
    rig.finish();
}

/// **An admitted symbol alone does not arm the dead-man** (no order, no position): admissions are never
/// released, so counting one would halt a venue over a pump nobody uses. Through the assembly: the symbol is
/// admitted into the price-only engine, then BOTH hub links of the venue drop (the datahub session and the
/// account's trade link), on the symbol it started on and on the admitted one, and a window five graces long
/// halts nothing. The control, on the same core: the moment an order rests, the same disclosure halts the
/// venue (so the quiet was the holdings gate, not a dead wire).
#[test]
fn an_admission_alone_does_not_arm_the_price_only_dead_man_but_a_resting_order_does() {
    const ADMITTED: &str = "ETH-PERPETUAL";
    let rig = Rig::start(HL, DERIBIT, GRACE_MS);
    rig.handle.send_command(Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: DERIBIT.into(),
        symbol: ADMITTED.into(),
        grid: SymbolGrid { tick_size: Some(0.05), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    })));
    assert!(
        wait_until(WAIT_SECS, || {
            let snap = rig.handle.snapshot();
            snap.portfolio.venues.iter().any(|b| b.route_key == DERIBIT && b.trades(ADMITTED))
        }),
        "the symbol was not admitted into the price-only engine: {}",
        rig.dump()
    );

    for symbol in [DERIBIT_SYMBOL, ADMITTED] {
        rig.strat_link_down(DERIBIT, symbol);
        rig.disclose(
            DERIBIT,
            symbol,
            TRADE_LINK_STREAM,
            StreamStatus::GapStart { at_ts_ms: now_ms() },
        );
    }
    assert!(
        !wait_until_for(QUIET, || rig.state_of(DERIBIT) != TradingState::Active),
        "an admitted symbol with nothing held halted the venue: {}",
        rig.dump()
    );

    // The control: a resting order, and a link that drops while it rests (an outage already in progress
    // when a venue first holds something is unwatched by design, so the loss is disclosed again).
    rig.rest_and_wait(DERIBIT, DERIBIT_SYMBOL, "po1");
    rig.strat_link_down(DERIBIT, DERIBIT_SYMBOL);
    assert!(
        wait_until(WAIT_SECS, || rig.state_of(DERIBIT) == TradingState::Halted),
        "a resting order did not arm the dead-man: {}",
        rig.dump()
    );
    assert!(
        wait_until(WAIT_SECS, || rig.status("po1") == Some(OrderStatus::Canceled)),
        "{}",
        rig.dump()
    );
    assert_eq!(rig.state_of(HL), TradingState::Active, "the mounted venue was halted");
    rig.finish();
}

// ---- the clock-driven sweeps: the margin call and the drawdown latch of an engine no bar ever reaches ----

/// **The control: a price-only engine's loss on a FRESH mark trips the account-wide drawdown latch.** No
/// strategy is mounted on hyperliquid and no bar of its series ever closes, so before `PriceOnlyConfig` this
/// loss was invisible to the latch. The first firing of `TimerKind::PriceOnlySweep` (30 s after the core
/// starts) measures it and latches `Reducing` on EVERY engine, the mounted venue's included (the latch is
/// account-wide by measure, decision 0180's inventory). The other two tests here are this one with the mark
/// made unusable.
#[test]
fn a_price_only_engines_loss_on_a_fresh_mark_trips_the_account_wide_drawdown_latch() {
    let rig = Rig::start(DERIBIT, HL, GRACE_MS);
    rig.go_long(HL, HL_SYMBOL, now_ms());
    rig.quote(HL, HL_SYMBOL, CRASH_PX, now_ms());
    assert!(
        wait_until(WAIT_SECS, || rig.block_of(HL).unrealized < -500.0),
        "the engine is not underwater on the fresh quote, so the latch has nothing to see: {}",
        rig.dump()
    );

    assert!(
        wait_until(SWEEP_WAIT_SECS, || rig.noted("DRAWDOWN LATCH")),
        "the price-only sweep never latched a 31 % loss: {}",
        rig.dump()
    );
    assert_eq!(rig.state_of(HL), TradingState::Reducing);
    assert_eq!(rig.state_of(DERIBIT), TradingState::Reducing, "the latch is account-wide");
    assert!(!rig.noted("price-only risk sweeps SKIP"), "a fresh mark was skipped: {}", rig.dump());
    rig.finish();
}

/// **A price-only engine with a STALE mark does not feed the latch** (the stale-skip): the same loss, but the
/// only price the engine has is older than `MAX_PRICE_AGE`, so the sweep SKIPS the engine (said once, in the
/// recent events) and the latch neither trips nor moves on a number nobody can vouch for. The snapshot's own
/// permissive pricing still shows the loss, so the quiet is the skip and not an absent loss.
#[test]
fn a_price_only_engine_with_a_stale_mark_does_not_feed_the_drawdown_latch() {
    let rig = Rig::start(DERIBIT, HL, GRACE_MS);
    let old = now_ms() - 120_000;
    rig.go_long(HL, HL_SYMBOL, old);
    rig.quote(HL, HL_SYMBOL, CRASH_PX, old);

    assert!(
        wait_until(SWEEP_WAIT_SECS, || rig.noted("price-only risk sweeps SKIP the hyperliquid")),
        "the sweep never skipped the engine whose mark is stale: {}",
        rig.dump()
    );
    assert!(
        rig.noted("older than the price-only bound"),
        "the skip names its reason: {}",
        rig.dump()
    );
    assert!(!rig.noted("DRAWDOWN LATCH"), "the latch acted on a stale mark: {}", rig.dump());
    assert_eq!(rig.state_of(HL), TradingState::Active);
    assert_eq!(rig.state_of(DERIBIT), TradingState::Active);
    assert!(
        rig.block_of(HL).unrealized < -500.0,
        "the loss the sweep ignored is real: {}",
        rig.dump()
    );
    rig.finish();
}

/// A venue client that accepts and fills EVERY order at once, at its limit price, through `poll_events`
/// (the in-process client shape, as `server/tests/admits_any_symbol.rs`'s `ScriptedVenue`): the demo
/// account's position without a venue.
struct FillingClient {
    pending: VecDeque<Event>,
    n: u64,
}

impl ExecutionClient for FillingClient {
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }

    fn submit(&mut self, request: &OrderRequest) {
        self.n += 1;
        let px = request.price.unwrap_or(1.0);
        let coid = request.client_order_id.clone();
        let fill = FillEvent {
            trade_id: TradeId::prefixed("demo", self.n),
            client_order_id: coid.clone(),
            venue: request.venue.as_str().into(),
            symbol: request.symbol.as_str().into(),
            side: request.side,
            last_qty: request.qty,
            last_px: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: request.ts,
            // No venue mark: a mark rung would outrank the quote the test marks the position with.
            mark_price: None,
            position_side: PositionSide::Both,
        };
        self.pending.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: coid.clone(),
            ts: request.ts,
        }));
        self.pending.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.clone(),
            venue_order_id: Some(format!("v{}", self.n).into()),
            ts: request.ts,
        }));
        self.pending.push_back(Event::Fill(fill.clone()));
        self.pending.push_back(Event::OrderFilled(OrderFilled {
            client_order_id: coid,
            fill,
            ts: request.ts,
        }));
    }

    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The backend of the network test: the generic scripted table, with deribit's `demo` start answered by a
/// [`FillingClient`] bound at `demo`. Asked twice per mount (the plan loop, then the mount), each time a fresh
/// script, of which only the mount's is consumed.
struct DemoDeribit;

impl BackendCtor for DemoDeribit {
    fn backend(
        &self,
        _vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        _data_only: &HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend> {
        let bind = vike_tradehub_core::AdapterBind {
            client: Box::new(FillingClient { pending: VecDeque::new(), n: 0 }),
            recon: None,
            bound_tier: AccountTier::Demo,
            grid: None,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
            confirmed_book: None,
            leverage_cap: None,
            account_ccy: None,
            account_ccy_assumed: false,
        };
        Arc::new(
            vike_tradehub_core::ScriptedBackend::credentialed(
                policy,
                &crate::scripted_arming_tests::absent_venues(),
            )
            .script_start(DERIBIT, Ok(bind)),
        )
    }
}

/// **A price-only engine whose feed dials another network is kept out of the latch, seed and PnL.** Deribit's
/// price feed is the public MAINNET host (`price_source_tiers`: `Live`), so a `demo` deribit account is priced
/// off a book that is not its own: its loss is phantom basis, and counting it could latch every venue. The
/// demo engine here holds a loss that WOULD trip the latch (31.5 % of both seeds) on a perfectly fresh mark;
/// the first sweep skips it by network (no `DRAWDOWN LATCH`). The control, on the same core: a real loss on the
/// mounted paper venue at the second sweep latches, and the capital base that note reports is the mounted
/// seed alone, 1,000, so the excluded engine's seed left the curve with its PnL.
#[test]
fn a_price_only_engine_on_another_network_than_its_feed_is_kept_out_of_the_latch() {
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(2, "API_KEY", "fake-deribit-key");
    let seam = super::SeamPolicy::armed(&[(HL, AccountTier::Paper), (DERIBIT, AccountTier::Demo)])
        .holding(book);
    let rig =
        Rig::start_over(HL, DERIBIT, &seam, Some(super::oanda::fill_sized_budget()), &DemoDeribit);
    assert_eq!(rig.block_of(DERIBIT).tier, Some(AccountTier::Demo), "{}", rig.dump());

    rig.limit_buy(DERIBIT, DERIBIT_SYMBOL, "pos-deribit", LONG_QTY, ENTRY_PX);
    assert!(wait_until(WAIT_SECS, || rig.holds(DERIBIT)), "the demo fill: {}", rig.dump());
    rig.quote(DERIBIT, DERIBIT_SYMBOL, CRASH_PX, now_ms());
    assert!(
        wait_until(WAIT_SECS, || rig.block_of(DERIBIT).unrealized < -500.0),
        "the demo engine is not underwater, so the test would prove nothing: {}",
        rig.dump()
    );

    assert!(
        wait_until(SWEEP_WAIT_SECS, || rig.noted("price-only risk sweeps SKIP the deribit")),
        "the sweep never skipped the engine on the other network: {}",
        rig.dump()
    );
    assert!(rig.noted("dials another network"), "the skip names its reason: {}", rig.dump());
    assert!(!rig.noted("DRAWDOWN LATCH"), "a phantom basis tripped the latch: {}", rig.dump());
    assert_eq!(rig.state_of(HL), TradingState::Active);

    // The control: the latch is alive, and measures the mounted engine alone.
    rig.go_long(HL, HL_SYMBOL, now_ms());
    rig.quote(HL, HL_SYMBOL, CRASH_PX, now_ms());
    assert!(
        wait_until(SWEEP_WAIT_SECS, || rig.noted("DRAWDOWN LATCH")),
        "the latch never acted on a real loss: {}",
        rig.dump()
    );
    assert!(
        rig.noted("capital_base=1000.00"),
        "the excluded engine's seed stayed in the curve: {}",
        rig.dump()
    );
    rig.finish();
}

/// [`wait_until`] over a [`Duration`] rather than whole seconds, for the "must NOT happen within" side:
/// `true` as soon as `cond` holds, `false` after `within`.
fn wait_until_for(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
