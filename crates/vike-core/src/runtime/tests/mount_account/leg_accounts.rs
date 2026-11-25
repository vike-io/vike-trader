//! **A leg names its ACCOUNT, and its orders, reads and fills go there** (decision 0293).
//!
//! The headline shape: a maker on binance `BTCUSDT.P` hedges on bybit's `BTCUSDT.P`, ONE user name on both
//! venues, and bybit runs TWO accounts (`DEMO`, `LIVE`). The hedge leg is named `BTCUSDT.P@bybit/DEMO`: its
//! order reaches `bybit#DEMO`'s client and no other book, its fill comes back under that name, and the
//! mount's reads of the leg read that account. A bare `@bybit` there, and a plain leg spelled like the
//! mount's own symbol, are refused at the start in words.

use super::*;
use vike_model::events::{FillEvent, PositionSide, TradeId};
use vike_model::{Fill, OrderBroker, OrderPlacement};

const MAKER: &str = "binance";
const TAKER: &str = "bybit";
const PERP: &str = "BTCUSDT.P";
const HEDGE: &str = "BTCUSDT.P@bybit/DEMO";

/// One engine of `venue` on [`PERP`] under the routing key `route_key`.
fn book(venue: &str, route_key: &str, seed: f64) -> Engine<RecordingClient> {
    let mut e = Engine::new(
        Account::new(seed, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        PERP,
    );
    e.route_key = route_key.to_string();
    e.collect_applied_fills = true;
    e
}

/// What the strategy heard on each fill: the fill's symbol, and its reads of the hedge leg and of its own
/// symbol at that moment.
type Heard = Arc<TestMutex<Vec<(String, f64, f64)>>>;

/// Quotes its own symbol on `""` and hedges on `hedge` on the feed lane; records each fill.
struct Hedger {
    hedge: String,
    heard: Heard,
}

impl Strategy<LiveBroker> for Hedger {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        let limit = OrderPlacement::Limit { price: 100.0 };
        OrderBroker::place(broker, "", "bid", 1, 1.0, limit, false);
        OrderBroker::place(broker, &self.hedge, "", -1, 1.0, limit, false);
    }

    fn on_fill(&mut self, broker: &mut LiveBroker, fill: &Fill) {
        let row = (fill.symbol.clone(), broker.position(&self.hedge), broker.position(PERP));
        self.heard.lock().unwrap().push(row);
    }
}

/// The maker's mount on binance `PERP`, its one leg `leg`.
fn maker(leg: &str, heard: &Heard) -> Mount {
    Mount {
        order_client_id_code: None,
        account: None,
        symbols: vec![MountLeg::named(leg, MAKER).expect("a leg name the rule reads")],
        controller_id: None,
        underlying_symbol: None,
        venue: MAKER.into(),
        symbol: PERP.into(),
        interval: INTERVAL.into(),
        strategy: Box::new(Hedger { hedge: leg.to_string(), heard: Arc::clone(heard) }),
    }
}

/// binance (one account) and bybit `DEMO` + `LIVE` (two), the maker mounted with leg `leg`.
fn cross_core(leg: &str, heard: &Heard) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(maker(leg, heard)),
        ..CoreConfig::default()
    };
    core_of(
        book(MAKER, MAKER, 1_000.0),
        vec![
            (1_000.0, book(TAKER, "bybit#DEMO", 1_000.0)),
            (1_000.0, book(TAKER, "bybit#LIVE", 1_000.0)),
        ],
        config,
    )
}

/// A venue fill of `order_client_id`, `qty` sold at 100 on `venue`'s `PERP`.
fn sold(venue: &str, order_client_id: &str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from("leg-1"),
        order_client_id: order_client_id.to_string(),
        venue: venue.into(),
        symbol: PERP.into(),
        side: -1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

/// ⚠ **THE HEADLINE.** The hedge named `BTCUSDT.P@bybit/DEMO` reaches `bybit#DEMO` alone, sent by its symbol
/// and carrying its account; the maker's own quote stays on binance; the fill comes back under the leg's
/// NAME with the DEMO book's position; and `LIVE`, the other bybit account, sees nothing.
#[test]
fn a_leg_named_by_its_account_trades_reads_and_hears_that_account() {
    let heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = cross_core(HEDGE, &heard);
    assert!(core.mounts[0].is_some(), "the mount runs: {:?}", refusal_note(&core));

    core.drive_strategy_feed_status(MAKER, PERP, FeedStatus::Disconnected);

    let quote = &core.engine(0).client.submissions;
    assert_eq!(quote.len(), 1, "the maker's own quote stays on binance: {quote:?}");
    assert_eq!((quote[0].venue.as_str(), quote[0].symbol.as_str()), (MAKER, PERP));
    let hedge = &core.engine(1).client.submissions;
    assert_eq!(hedge.len(), 1, "the hedge reaches bybit#DEMO: {hedge:?}");
    assert_eq!((hedge[0].venue.as_str(), hedge[0].symbol.as_str()), (TAKER, PERP));
    assert_eq!(
        hedge[0].account,
        Some(AccountLabel::Named("DEMO".into())),
        "sent by its symbol, carrying the account its name binds"
    );
    assert!(core.engine(2).client.submissions.is_empty(), "bybit#LIVE is never reached");

    let order_client_id = hedge[0].order_client_id.clone();
    let _ = core.handle(Ingest::Event(sold(TAKER, &order_client_id, 1.0)));
    let heard = heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 1, "the strategy hears its hedge fill: {heard:?}");
    let (symbol, hedge_position, own_position) = &heard[0];
    assert_eq!(
        symbol, HEDGE,
        "the fill carries the leg's NAME, the one the strategy sent it under"
    );
    assert_eq!(*hedge_position, -1.0, "the leg reads the DEMO book after its fill");
    assert_eq!(*own_position, 0.0, "the maker's own symbol reads binance's book, not bybit's");
}

/// A BARE venue naming no account is refused at the start when that venue runs two accounts, in the mount
/// rule's words naming the leg and the fix; the strategy never runs, so nothing is sent anywhere.
#[test]
fn a_bare_venue_leg_on_a_venue_of_two_accounts_is_refused_at_the_start() {
    let heard = Arc::new(TestMutex::new(Vec::new()));
    let core = cross_core("BTCUSDT.P@bybit", &heard);
    assert!(core.mounts[0].is_none(), "the mount does not run");
    let note = refusal_note(&core).expect("a refusal in the ring");
    assert!(note.contains("its leg `BTCUSDT.P@bybit`"), "it names the leg: {note}");
    assert!(note.contains("2 engines"), "and why it is ambiguous: {note}");
    assert!(note.contains("`BTCUSDT.P@bybit/<LABEL>`"), "and the fix: {note}");
}

/// A PLAIN leg on bybit spelled like the mount's own `BTCUSDT.P` is two legs under one name: its orders
/// would resolve to the maker's own book. Refused at the start, naming the fix.
#[test]
fn a_plain_leg_named_like_the_mounts_own_symbol_on_another_venue_is_refused_at_the_start() {
    let heard = Arc::new(TestMutex::new(Vec::new()));
    let mut mount = maker(HEDGE, &heard);
    mount.symbols = vec![MountLeg::at(PERP, TAKER)];
    let config = CoreConfig { seed_cash: 1_000.0, strategy: Some(mount), ..CoreConfig::default() };
    let core =
        core_of(book(MAKER, MAKER, 1_000.0), vec![(1_000.0, book(TAKER, TAKER, 1_000.0))], config);
    assert!(core.mounts[0].is_none(), "the mount does not run");
    let note = refusal_note(&core).expect("a refusal in the ring");
    assert!(note.contains("is named like the mount's own instrument"), "{note}");
    assert!(note.contains("`BTCUSDT.P@bybit/<LABEL>`"), "it names the fix: {note}");
}

/// A bare `SYMBOL@venue` of the mount's OWN venue names no account and is refused by the leg rule; one naming
/// an account of it is a leg on that account, judged at the start.
#[test]
fn a_leg_on_the_mounts_own_venue_must_name_its_account() {
    let why = MountLeg::named("ETHUSDT.P@binance", MAKER).expect_err("no account named");
    assert!(why.contains("names the mount's own venue `binance` and no account"), "{why}");
    let leg =
        MountLeg::named("ETHUSDT.P@binance/ALT", MAKER).expect("another account of the venue");
    assert_eq!((leg.venue.as_deref(), leg.account), (Some(MAKER), Some(alt())));
    assert_eq!(MountLeg::named("ETHUSDT.P", MAKER), Ok(MountLeg::same_venue("ETHUSDT.P")));
}

/// A fill of `order_client_id`: `side` `qty` at 100 on `venue`'s `PERP`, trade `trade`.
fn filled(venue: &str, order_client_id: &str, side: i32, qty: f64, trade: &'static str) -> Event {
    let Event::Fill(mut f) = sold(venue, order_client_id, qty) else { unreachable!("a fill") };
    f.side = side;
    f.trade_id = TradeId::from(trade);
    Event::Fill(f)
}

/// A maker on bybit `DEMO` `BTCUSDT.P` with its leg `leg` on the same venue, bybit running `DEMO` and `LIVE`.
fn own_venue_core(leg: &str, heard: &Heard) -> CoreThread<RecordingClient> {
    let mut mount = maker(leg, heard);
    mount.venue = TAKER.into();
    mount.account = Some(AccountLabel::Named("DEMO".into()));
    mount.symbols = vec![MountLeg::named(leg, TAKER).expect("a leg name the rule reads")];
    let config = CoreConfig { seed_cash: 1_000.0, strategy: Some(mount), ..CoreConfig::default() };
    core_of(
        book(TAKER, "bybit#DEMO", 1_000.0),
        vec![(1_000.0, book(TAKER, "bybit#LIVE", 1_000.0))],
        config,
    )
}

/// **A leg on ANOTHER account of the mount's OWN venue routes to that account** (decision 0293): the maker on
/// bybit `DEMO` quotes `BTCUSDT.P` there and hedges the same symbol on bybit `LIVE`. The hedge reaches LIVE's
/// client alone, its fill comes back under the leg's name, and the mount's LEDGER keeps the two accounts' rows
/// apart: the own row (DEMO) and the leg's row (LIVE) on one venue and one symbol never merge.
#[test]
fn a_leg_on_another_account_of_the_mounts_own_venue_trades_there_and_keeps_its_own_ledger_row() {
    const LIVE_LEG: &str = "BTCUSDT.P@bybit/LIVE";
    let heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = own_venue_core(LIVE_LEG, &heard);
    assert!(core.mounts[0].is_some(), "the mount runs: {:?}", refusal_note(&core));

    core.drive_strategy_feed_status(TAKER, PERP, FeedStatus::Disconnected);
    let quote = core.engine(0).client.submissions.clone();
    let hedge = core.engine(1).client.submissions.clone();
    assert_eq!(quote.len(), 1, "the maker's quote stays on DEMO: {quote:?}");
    assert_eq!(quote[0].account, None, "the mount's own order names no account, as before");
    assert_eq!(hedge.len(), 1, "the hedge reaches LIVE: {hedge:?}");
    assert_eq!(hedge[0].account, Some(AccountLabel::Named("LIVE".into())));
    assert_eq!(hedge[0].symbol, PERP, "sent by its symbol");

    let _ = core.handle(Ingest::Event(filled(TAKER, &hedge[0].order_client_id, -1, 1.0, "live-1")));
    let _ = core.handle(Ingest::Event(filled(TAKER, &quote[0].order_client_id, 1, 1.0, "demo-1")));
    let heard = heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 2, "both fills heard: {heard:?}");
    assert_eq!(heard[0].0, LIVE_LEG, "the LIVE fill carries the leg's name");
    assert_eq!(heard[0].1, -1.0, "the leg reads LIVE's book after its fill");
    assert_eq!(heard[1].0, PERP, "the DEMO fill is the mount's own");

    let ledger = &core.mount_attribution[0];
    assert_eq!(ledger.size, 1.0, "the own row is DEMO's alone");
    let rows: Vec<(&str, Option<&AccountLabel>, &str, f64)> = ledger
        .legs
        .iter()
        .map(|r| (r.venue.as_str(), r.account.as_ref(), r.symbol.as_str(), r.attr.size))
        .collect();
    let live = AccountLabel::Named("LIVE".into());
    assert_eq!(rows, [(TAKER, Some(&live), PERP, -1.0)], "LIVE's row stands apart, unmerged");
}

/// A leg naming the mount's OWN account is the mount's own instrument under a second name: refused at the start.
#[test]
fn a_leg_naming_the_mounts_own_account_is_refused_at_the_start() {
    let heard = Arc::new(TestMutex::new(Vec::new()));
    let core = own_venue_core("ETHUSDT.P@bybit/DEMO", &heard);
    assert!(core.mounts[0].is_none(), "the mount does not run");
    let note = refusal_note(&core).expect("a refusal in the ring");
    assert!(note.contains("names the mount's own account"), "{note}");
}

/// Places one reduce-only stop on `leg` on the feed lane.
struct StopOnLeg {
    leg: String,
}

impl Strategy<LiveBroker> for StopOnLeg {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        let stop = OrderPlacement::Stop { trigger: 90.0 };
        OrderBroker::place(broker, &self.leg, "", -1, 1.0, stop, true);
    }
}

/// **A core-held stop on a leg named by its account is ARMED on that account and FIRES there** (decision
/// 0293). polymarket holds no stop, so the core holds a strategy's reduce-only one. The mount trades
/// polymarket's unlabelled account and its leg `X@polymarket/LIVE` the other: the stop the strategy places on
/// the leg is armed on LIVE's engine as the mount's own, and its fire, a reduce-only market, reaches LIVE's
/// client and names LIVE (so a replay lowers it there too), never the mount's book.
#[test]
fn a_core_held_stop_on_a_leg_named_by_its_account_arms_and_fires_on_that_account() {
    const POLY: &str = "polymarket";
    const LEG: &str = "X@polymarket/LIVE";
    let engine = |route_key: &str| {
        let mut e = vike_execution::testing::engine_on(POLY, "X", RecordingClient::default());
        e.route_key = route_key.to_string();
        e.collect_applied_fills = true;
        e
    };
    let mount = Mount {
        order_client_id_code: None,
        account: Some(AccountLabel::Default),
        symbols: vec![MountLeg::named(LEG, POLY).expect("a leg on another account of the venue")],
        controller_id: None,
        underlying_symbol: None,
        venue: POLY.into(),
        symbol: "X".into(),
        interval: INTERVAL.into(),
        strategy: Box::new(StopOnLeg { leg: LEG.into() }),
    };
    let config = CoreConfig { strategy: Some(mount), ..CoreConfig::default() };
    let mut core = core_of(engine(POLY), vec![(0.0, engine("polymarket#LIVE"))], config);
    assert!(core.mounts[0].is_some(), "the mount runs: {:?}", refusal_note(&core));
    // LIVE holds the long the stop reduces.
    let key = (POLY.into(), "X".into(), PositionSide::Both);
    let held =
        vike_execution::PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() };
    core.engine_mut(1).account.positions.insert(key, held);

    core.drive_strategy_feed_status(POLY, "X", FeedStatus::Disconnected);
    let arms: Vec<_> = core.emulated.cond_engine.values().copied().collect();
    assert_eq!(arms.len(), 1, "the stop is held by the core: {:?}", core.recent);
    assert_eq!(arms[0].engine, 1, "armed on LIVE's engine");
    assert_eq!(arms[0].mount, Some(0), "as the mount's own stop");
    assert!(
        core.engine(0).client.submissions.is_empty()
            && core.engine(1).client.submissions.is_empty()
    );

    core.fire_conditionals_at_price(POLY, "X", 89.0, 10);
    let fired = &core.engine(1).client.submissions;
    assert_eq!(fired.len(), 1, "the fire reaches LIVE's client: {:?}", core.recent);
    assert!(fired[0].reduce_only && fired[0].side == -1, "{:?}", fired[0]);
    assert_eq!(fired[0].account, Some(AccountLabel::Named("LIVE".into())), "and names LIVE");
    assert!(core.engine(0).client.submissions.is_empty(), "the mount's own book is never reached");
}
