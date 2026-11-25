//! The control door lets an order name ANY symbol of its exchange (decision 0173): bybit, binance, aster,
//! okx, hyperliquid, deribit, alpaca, ctrader, oanda, ig, ibkr, dukascopy and fxcm admit the symbol into the
//! account's engine and place the order on it. Every venue but polymarket reads the symbol by its USER name through
//! the venue's kept row (decision 0290: `EURUSD`, `BTCUSDT.P`, `AAPL`, `US500.CFD`, in any case; the venue's own
//! spelling is its bridge's), and then (deribit: a perpetual, dated future or USDC option of the engine's own money model only;
//! alpaca: an equity or a crypto pair; ctrader: only a forex pair or a
//! CFD of a measured class (metals, indices, oil and commodities, crypto) quoted in the deposit currency
//! passes the gatewayhub's facts; oanda: an instrument whose quote currency is the account's, which the gatewayhub's facts
//! enforce; ig: an epic whose facts the gatewayhub admits for a measured type only;
//! dukascopy: a currency or metal pair quoted in the engine's money; fxcm: a pair, or another instrument its kept
//! list carries, admitted by its class; polymarket: an outcome TOKEN id or a market
//! name the facts source resolves to one, on an engine mounted on no symbol, whose rows are
//! `server/tests/outcome_token_door.rs`'s); every refusal names its cause.
//!
//! Two layers. The PLAN (`plan_submit_symbol`) is a pure function of the published block and a
//! facts source, so each rule has a row. The END-TO-END tests drive `accept_command` into a REAL
//! core over a scripted venue client: the order reaches the venue client on the symbol the operator
//! named, snapped on THAT symbol's grid, and its fill folds into THAT symbol's position.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;

use super::control::{ControlLimits, ControlLimitsConfig, accept_command};
use crate::server::symbol_admission::{
    AccountReadiness, PriceWait, SymbolAdmitter, SymbolPlan, lane_addressed, plan_submit_symbol,
    submit_symbol_refusal,
};
use vike_catalog::{SYMBOL_RULES, VenueSymbolRule, door_symbol, door_user_symbol};
use vike_execution::{Account, BalanceMode, Engine, ExecutionClient, RiskGate, SymbolAdmission};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, RiskLimits, SymbolGrid};
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};

/// The venues whose adapter signs on the ORDER's symbol.
const ADMITS: [&str; 14] = [
    "binance",
    "bybit",
    "aster",
    "okx",
    "hyperliquid",
    "deribit",
    "alpaca",
    "ctrader",
    "oanda",
    "ig",
    "ibkr",
    "dukascopy",
    "fxcm",
    "polymarket",
];
/// The venues whose adapter still signs the MOUNTED symbol whatever the order names (none today:
/// deribit's was the last row).
const MOUNTED_ONLY: [&str; 0] = [];
/// The venues whose adapter reads the order's own symbol and whose engine judges it soundly enough (none today:
/// oanda's was one until it became `Admits`, and polymarket's, the last, with the outcome-token door).
const PER_ORDER: [&str; 0] = [];
/// The venues whose adapter reads the order's own symbol but whose engine cannot judge a foreign one (none
/// today: fxcm's was the last, until its price lane).
const PER_ORDER_UNSAFE: [&str; 0] = [];

/// **Every roster venue is classified.** A new venue lands in none of the four and this fails, so
/// whoever adds it reads its adapter's order builder and decides, instead of the venue silently
/// defaulting to "not refused".
#[test]
fn every_roster_venue_has_a_symbol_rule() {
    let mut classified: Vec<&str> = ADMITS
        .iter()
        .chain(MOUNTED_ONLY.iter())
        .chain(PER_ORDER.iter())
        .chain(PER_ORDER_UNSAFE.iter())
        .copied()
        .collect();
    classified.sort_unstable();
    let mut roster: Vec<&str> = vike_model::VENUES.to_vec();
    roster.sort_unstable();
    assert_eq!(classified, roster);
    let of = |want: fn(&VenueSymbolRule) -> bool| {
        let mut v: Vec<&str> =
            SYMBOL_RULES.iter().filter(|(_, r)| want(r)).map(|(venue, _)| *venue).collect();
        v.sort_unstable();
        v
    };
    let sorted = |mut v: Vec<&'static str>| {
        v.sort_unstable();
        v
    };
    assert_eq!(of(|r| matches!(r, VenueSymbolRule::Admits(_))), sorted(ADMITS.to_vec()));
    assert_eq!(of(|r| matches!(r, VenueSymbolRule::MountedOnly(_))), sorted(MOUNTED_ONLY.to_vec()));
    assert_eq!(of(|r| matches!(r, VenueSymbolRule::PerOrder)), sorted(PER_ORDER.to_vec()));
    assert_eq!(
        of(|r| matches!(r, VenueSymbolRule::PerOrderUnsafe(_))),
        sorted(PER_ORDER_UNSAFE.to_vec())
    );
    assert_eq!(SYMBOL_RULES.len(), roster.len(), "one row per roster venue");
}

/// **An `Admits` venue's lease claims what its daemon client reads, and dukascopy's is not listed.**
/// (fxcm's reconcile client was a gap until its price lane made an admitted pair reachable: it now
/// parses its whole-account Orders and Trades snapshots once per told pair, so a lost-reply market order or a
/// restart with a position open on an admitted pair is booked by reconcile, not left to the engine's flat view.
/// okx's was the other until it read each told instrument with that instrument's own `ctVal`:
/// `crates/bridges/okx/tests/offline/reconcile_admitted.rs`.)
/// The node mirrors the symbols it told the daemon (`ReconcileClientGateway::admitted_symbols`) only
/// for the venues in `RECONCILE_READS_ADMITTED`; any other claims none, so the restore planner holds its orders
/// on an admitted symbol. A new `Admits` venue fails here until someone decides which side it is on.
/// dukascopy's is a CHOICE, not an absence: its reconcile client (`Wiring::SidecarExecutionReconcile`) pulls the whole account's
/// orders and positions from the sidecar, so its reads span every symbol, but it keeps no admitted list to name them and
/// it cannot name a restored take-profit across a gatewayhub restart, so the restore planner is left to HOLD its orders on an
/// admitted symbol instead of retiring them (`crates/bridges/dukascopy/CLAUDE.md`'s reconcile leg).
#[test]
fn an_admits_venue_is_either_read_by_its_reconcile_client_or_a_known_gap() {
    use vike_gatewayhub_client::trade::RECONCILE_READS_ADMITTED;
    let mut admits: Vec<&str> = ADMITS.to_vec();
    admits.retain(|v| !RECONCILE_READS_ADMITTED.contains(v));
    assert_eq!(
        admits,
        ["dukascopy"],
        "dukascopy's reconcile client names no admitted symbols, on purpose (okx's and fxcm's read every admitted \
         instrument: `crates/bridges/okx/tests/offline/reconcile_admitted.rs`, fxcm's `recon_client.rs`'s \
         `every_pass_reads_the_admitted_pairs_...`)"
    );
    assert!(RECONCILE_READS_ADMITTED.iter().all(|v| ADMITS.contains(v)));
}

fn submit(venue: &str, symbol: &str, account: Option<&str>) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// [`submit`] of `qty` (a lot of 10 dollars, of 1,000 contracts, ... is more than one).
fn submit_quantity(venue: &str, symbol: &str, qty: f64) -> CommandWire {
    let CommandWire::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.qty = qty;
    CommandWire::Submit(req)
}

fn market(venue: &str, symbol: &str, reduce_only: bool) -> CommandWire {
    let CommandWire::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.order_type = "market".into();
    req.price = None;
    req.reduce_only = reduce_only;
    CommandWire::Submit(req)
}

/// A published engine block: a real account (`demo`) unless the test says otherwise.
fn engine(route_key: &str, symbol: &str) -> vike_execution::VenueBlock {
    vike_execution::VenueBlock {
        venue: vike_model::accounts::account_keys::venue_of_route_key(route_key).into(),
        route_key: route_key.into(),
        symbol: symbol.into(),
        tier: Some(AccountTier::Demo),
        ..Default::default()
    }
}

/// What the table answers a price ask with.
#[derive(Clone, Copy)]
enum Price {
    /// A price is already here.
    Ready,
    /// A price is already here, at this value (an FX epic is not near `Ready`'s 3000).
    At(f64),
    /// Subscribed, nothing yet.
    Pending,
    /// Subscribed, nothing yet, and the lane says its venue stream is in a gap (`lane_verdict`).
    Down,
    /// No feed can be had.
    Failing,
}

/// A facts source over a fixed table `(venue, symbol) -> (tick, lot)`; records what it was asked.
struct Table {
    rows: Vec<(&'static str, &'static str, Option<f64>, Option<f64>)>,
    asked: Mutex<Vec<String>>,
    price: Price,
    /// Whether the account runs with an account-wide exposure ceiling.
    capped: bool,
    /// Every price ask, release and admitted note, in order: `price:SYM:subscribe|peek`,
    /// `release:SYM`, `admitted:SYM`.
    events: Mutex<Vec<String>>,
    /// What `refresh` answers (a stale admission, once).
    refresh: Mutex<Option<SymbolAdmission>>,
    /// Every `hold_lane` ask, as `route key/symbol` (decision 0197: the engine's own symbol is being ordered in).
    holds: Mutex<Vec<String>>,
    waits: bool,
    /// The symbols whose facts say INVERSE (an amount counted in US dollars): deribit's coin-margined
    /// perpetuals. Every other row answers the default, linear.
    inverse: Vec<&'static str>,
    /// The venues this node holds NO price watcher for (`price_source` refuses them).
    unpriced: Vec<&'static str>,
    /// The route keys the watcher says are DERIVED lane twins that run (the evidence a live engine needs).
    derived: Vec<&'static str>,
    /// The leverage the exchange was SEEN holding for every admitted symbol (`confirmed_leverage`); `None` =
    /// nothing confirmed (a refusal at the venue, an older gatewayhub).
    confirm: Option<f64>,
    /// The minimum notional every row's grid carries.
    min_notional: Option<f64>,
    /// When the price the table answers was made (`price_made_ms`); `None` = not known.
    made: Option<i64>,
}

impl Table {
    fn eth() -> Self {
        Table {
            inverse: vec!["ETHUSD.P", "SOLUSD.P", "BTCUSD_260327", "BTCUSD.P", "BTCUSD_261225"],
            rows: vec![
                ("deribit", "ETHUSD.P", Some(0.05), Some(1.0)),
                ("deribit", "SOLUSD.P", Some(0.01), Some(1.0)),
                ("deribit", "BTCUSD_260327", Some(2.5), Some(10.0)),
                ("deribit", "BTCUSDC_260327", Some(2.5), Some(0.0001)),
                ("deribit", "ETH_USDC-27MAR26-2500-C", Some(0.2), Some(0.1)),
                ("deribit", "XRP_USDC-27MAR26-2d5-C", Some(0.0005), Some(1000.0)),
                ("deribit", "ETHUSDC.P", Some(0.01), Some(0.0001)),
                ("deribit", "BTCUSDC.P", Some(0.1), Some(0.0001)),
                ("bybit", "ETHUSDT", Some(0.01), Some(0.01)),
                ("bybit", "ETHUSDT.P", Some(0.01), Some(0.01)),
                // decision 0231's classes: a dated linear future, a spot pair and two coin-margined contracts
                ("bybit", "ETHUSDT_261127", Some(0.05), Some(0.01)),
                ("bybit", "DOGEUSDT", Some(0.00001), Some(0.1)),
                ("bybit", "BTCUSD.P", Some(0.5), Some(1.0)),
                ("bybit", "BTCUSD_261225", Some(0.5), Some(1.0)),
                // a coin-margined contract of ANOTHER coin (decision 0258's one settle coin per engine)
                ("bybit", "ETHUSD.P", Some(0.05), Some(1.0)),
                ("binance", "ETHUSDT", Some(0.01), Some(0.0001)),
                ("binance", "ETHUSDT.P", Some(0.01), Some(0.001)),
                // a USDC-settled linear perpetual (decision 0258's one settle coin, linear engines included)
                ("binance", "BTCUSDC.P", Some(0.1), Some(0.001)),
                ("aster", "ETHUSDT.P", Some(0.01), Some(0.001)),
                ("aster", "ETHUSDT", Some(0.01), Some(0.0001)),
                // two listed instruments whose facts state no tick, and no lot
                ("bybit", "XRPUSDT.P", None, Some(0.01)),
                ("bybit", "DOGEUSDT.P", Some(0.01), None),
                ("okx", "ETHUSDT.P", Some(0.01), Some(0.001)),
                ("okx", "ETHUSDT", Some(0.01), Some(0.00000001)),
                ("okx", "ETHUSDT_261225", Some(0.01), Some(0.01)),
                ("hyperliquid", "KPEPEUSDC.P", Some(0.001), Some(0.01)),
                ("hyperliquid", "HYPEUSDC", Some(0.001), Some(0.01)),
                ("hyperliquid", "XYZ:TSLAUSDC.P", Some(0.01), Some(0.001)),
                ("alpaca", "MSFT", Some(0.01), Some(1.0)),
                ("alpaca", "BRK.B", Some(0.01), Some(1.0)),
                ("alpaca", "BTCUSD", Some(1.0), Some(0.000000001)),
                ("ctrader", "GBPUSD", Some(0.00001), Some(0.01)),
                ("ctrader", "US500.CFD", Some(0.01), Some(1.0)),
                ("oanda", "GBPUSD", Some(0.00001), Some(1.0)),
                ("oanda", "AUDUSD", Some(0.00001), Some(1.0)),
                ("oanda", "US500.CFD", Some(0.1), Some(0.1)),
                ("ig", "GBPUSD", Some(0.0001), Some(0.1)),
                ("ig", "AUDUSD", Some(0.0001), Some(0.1)),
                ("ibkr", "MSFT", Some(0.01), Some(1.0)),
                ("ibkr", "TSLA", Some(0.01), Some(1.0)),
                ("dukascopy", "GBPUSD", Some(0.00001), Some(1000.0)),
                ("dukascopy", "XAUUSD", Some(0.001), Some(1000.0)),
                ("dukascopy", "EURGBP", Some(0.00001), Some(1000.0)),
                ("fxcm", "GBPUSD", Some(0.00001), Some(1000.0)),
                ("fxcm", "AUDUSD", Some(0.00001), Some(1000.0)),
            ],
            asked: Mutex::new(Vec::new()),
            price: Price::Ready,
            capped: true,
            events: Mutex::new(Vec::new()),
            refresh: Mutex::new(None),
            holds: Mutex::new(Vec::new()),
            waits: true,
            unpriced: Vec::new(),
            derived: Vec::new(),
            confirm: None,
            min_notional: None,
            made: None,
        }
    }
    /// The exchange confirms it holds `leverage` for every symbol admitted here.
    fn confirming(mut self, leverage: f64) -> Self {
        self.confirm = Some(leverage);
        self
    }
    fn with_price(mut self, price: Price) -> Self {
        self.price = price;
        self
    }
    /// This source's watcher says these route keys are the lane twins it derived and runs.
    fn deriving(mut self, route_keys: &[&'static str]) -> Self {
        self.derived = route_keys.to_vec();
        self
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
    fn uncapped(mut self) -> Self {
        self.capped = false;
        self
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
    fn holds(&self) -> Vec<String> {
        self.holds.lock().unwrap().clone()
    }
}

impl SymbolAdmitter for Table {
    fn hold_lane(&self, route_key: &str, _: &str, symbol: &str) {
        self.holds.lock().unwrap().push(format!("{route_key}/{symbol}"));
    }

    fn price_source(&self, venue: &str) -> Result<(), String> {
        if self.unpriced.contains(&venue) {
            return Err(format!("this node runs no price feed for {venue}"));
        }
        Ok(())
    }

    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        self.asked.lock().unwrap().push(format!("{route_key}/{symbol}"));
        let Some((_, _, tick, lot)) =
            self.rows.iter().find(|(v, s, _, _)| *v == venue && *s == symbol)
        else {
            return Err(SymbolPropertiesRequestError::Refused(format!(
                "{venue}: no instrument {symbol} in the venue's instrument list"
            )));
        };
        let contract_value = if self.inverse.contains(&symbol) {
            Some(vike_model::ContractValue::inverse(1.0))
        } else if venue == "ig" {
            // ig's facts always carry the epic's contract size (an FX mini contract is 10,000 units).
            Some(vike_model::ContractValue::linear(10_000.0))
        } else {
            None
        };
        Ok(SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: *tick,
                lot_size: *lot,
                min_notional: self.min_notional,
                ..SymbolGrid::default()
            },
            contract_value,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        })
    }

    fn price(
        &self,
        _: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        let mode = if subscribe { "subscribe" } else { "peek" };
        self.events.lock().unwrap().push(format!("price:{symbol}:{mode}"));
        match self.price {
            Price::Ready => Ok(Some(3000.0)),
            Price::At(px) => Ok(Some(px)),
            Price::Pending | Price::Down => Ok(None),
            Price::Failing => Err(format!("{venue} has no price feed for {symbol}")),
        }
    }

    fn lane_verdict(&self, _: &str, venue: &str, _: &str) -> Option<String> {
        matches!(self.price, Price::Down)
            .then(|| format!("its price stream is DOWN ({venue}'s stream is in a gap)"))
    }

    fn price_made_ms(&self, _: &str, _: &str, _: f64) -> Option<i64> {
        self.made
    }

    fn admitted(&self, _: &str, _: &str, symbol: &str) {
        self.events.lock().unwrap().push(format!("admitted:{symbol}"));
    }

    fn release(&self, _: &str, _: &str, symbol: &str) {
        self.events.lock().unwrap().push(format!("release:{symbol}"));
    }

    fn release_requested(&self, _: &str, _: &str, symbol: &str) {
        self.events.lock().unwrap().push(format!("release_requested:{symbol}"));
    }

    fn refresh(&self, _: &str, _: &str, _: &str) -> Option<SymbolAdmission> {
        self.refresh.lock().unwrap().take()
    }

    fn waits(&self) -> bool {
        self.waits
    }

    fn confirmed_leverage(&self, _: &str, _: &str) -> Option<f64> {
        self.confirm
    }
}

impl AccountReadiness for Table {
    fn generation(&self, _: &str) -> u64 {
        0
    }

    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }

    fn derived_lane_twin(&self, route_key: &str) -> bool {
        self.derived.contains(&route_key)
    }
}

fn placed(plan: SymbolPlan) -> (String, Option<Box<SymbolAdmission>>) {
    match plan {
        SymbolPlan::Place { symbol, admit, .. } => (symbol, admit),
        other => panic!("expected a placement, got {other:?}"),
    }
}

pub(super) fn refused(plan: SymbolPlan) -> String {
    match plan {
        SymbolPlan::Refuse(why) => why.text,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// **The headline, on every finished venue**: a symbol the engine was not started on is spelled the
/// venue's way, its facts are looked up, and the plan is the admission followed by the order.
#[test]
fn a_foreign_symbol_is_admitted_in_the_venues_spelling_with_its_own_grid() {
    // (venue, the engine's symbol, what the operator typed, its user name, tick, lot)
    let rows = [
        ("bybit", "BTCUSDT", "ethusdt", "ETHUSDT", 0.01, 0.01),
        ("bybit", "BTCUSDT.P", "ethusdt.p", "ETHUSDT.P", 0.01, 0.01),
        ("binance", "BTCUSDT", "ethusdt", "ETHUSDT", 0.01, 0.0001),
        ("binance", "BTCUSDT.P", "ethusdt.p", "ETHUSDT.P", 0.01, 0.001),
        ("aster", "BTCUSDT.P", "ETHUSDT.P", "ETHUSDT.P", 0.01, 0.001),
        ("okx", "BTCUSDT.P", "ethusdt.p", "ETHUSDT.P", 0.01, 0.001),
        ("hyperliquid", "BTCUSDC.P", "kPEPEUSDC.P", "KPEPEUSDC.P", 0.001, 0.01),
        ("alpaca", "AAPL", "msft", "MSFT", 0.01, 1.0),
        ("alpaca", "AAPL", " brk.b ", "BRK.B", 0.01, 1.0),
        ("alpaca", "AAPL", "btcusd", "BTCUSD", 1.0, 0.000000001),
        ("alpaca", "BTCUSD", "MSFT", "MSFT", 0.01, 1.0),
        ("ctrader", "EURUSD", "GBPUSD", "GBPUSD", 0.00001, 0.01),
        ("ctrader", "EURUSD", " us500.cfd ", "US500.CFD", 0.01, 1.0),
        ("oanda", "EURUSD", "gbpusd", "GBPUSD", 0.00001, 1.0),
        ("oanda", "EURUSD", " GbpUsd ", "GBPUSD", 0.00001, 1.0),
        ("oanda", "EURUSD", "us500.cfd", "US500.CFD", 0.1, 0.1),
        ("oanda", "US500.CFD", "audusd", "AUDUSD", 0.00001, 1.0),
        ("ig", "EURUSD", "gbpusd", "GBPUSD", 0.0001, 0.1),
        ("ig", "EURUSD", " AUDUSD ", "AUDUSD", 0.0001, 0.1),
        ("ibkr", "AAPL", "msft", "MSFT", 0.01, 1.0),
        ("ibkr", "AAPL", " tsla ", "TSLA", 0.01, 1.0),
        ("ibkr", "EURUSD", "MSFT", "MSFT", 0.01, 1.0),
    ];
    for (venue, mounted, typed, canonical, tick, lot) in rows {
        let facts = Table::eth();
        let (symbol, admit) = placed(plan_submit_symbol(
            &submit(venue, typed, None),
            &[engine(venue, mounted)],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, canonical, "{venue}: {typed}");
        let admit = admit.unwrap_or_else(|| panic!("{venue}: {typed} must be admitted first"));
        assert_eq!((admit.route_key.as_str(), admit.symbol.as_str()), (venue, canonical));
        assert_eq!(admit.grid.tick_size, Some(tick), "{venue}: its OWN tick");
        assert_eq!(admit.grid.lot_size, Some(lot), "{venue}: its OWN lot");
        assert_eq!(facts.asked(), vec![format!("{venue}/{canonical}")], "{venue}: asked once");
    }
}

/// **A price named on a market order is not its value at the symbol's minimum notional**: a limit at 1 is worth 1
/// and is refused under a floor of 5; a market order that names the same 1 is judged as one that names none, on
/// its quantity (the engine judges its value at the mark).
#[test]
fn a_market_order_is_not_held_to_the_minimum_notional_at_the_price_it_names() {
    let facts = Table { min_notional: Some(5.0), ..Table::eth() };
    let roster = [engine("bybit", "BTCUSDT")];
    let at_one = submit("bybit", "ETHUSDT", None);
    match plan_submit_symbol(&at_one, &roster, Some(&facts), true) {
        SymbolPlan::Refuse(why) => assert!(why.text.contains("minimum notional"), "{why}"),
        other => panic!("a limit worth 1 is under the floor: {other:?}"),
    }
    let CommandWire::Submit(mut req) = at_one else { unreachable!() };
    req.order_type = "market".into();
    placed(plan_submit_symbol(&CommandWire::Submit(req), &roster, Some(&facts), true));
}

/// A ceiling of `max` with no rate limit in the way.
fn ceiling_of(max: f64) -> ControlLimits {
    ControlLimits::new(ControlLimitsConfig { max_notional: Some(max), rate_per_sec: 1e9 })
}

/// A MARKET BUY of `qty` as a wire client writes it, naming `price` or none.
fn market_of(
    order_client_id: &str,
    venue: &str,
    symbol: &str,
    qty: f64,
    price: Option<f64>,
) -> CommandWire {
    let CommandWire::Submit(mut req) = market(venue, symbol, false) else { unreachable!() };
    (req.order_client_id, req.qty, req.price) = (order_client_id.into(), qty, price);
    CommandWire::Submit(req)
}

/// **`policy.max_notional_per_order` binds a MARKET order on every engine, at the price the door holds** (it bound
/// one only on a LIVE lane twin: a market order reached every other engine's core unsized, and the row reaches no
/// `RiskLimits`). Whatever number the order names: 0.1 BTC "at 1" is worth 7,430. A close is never sized.
#[test]
fn the_ceiling_binds_a_market_order_at_the_price_the_door_holds() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let mut node = Node::start("bybit", "BTCUSDT", tier);
        node.limits = ceiling_of(500.0);
        let facts = FirstTick::new(false);
        facts.tick(74_300.0);

        for (order_client_id, named) in [("plain", None), ("at-one", Some(1.0))] {
            let why = node
                .send(market_of(order_client_id, "bybit", "BTCUSDT", 0.1, named), Some(&facts))
                .expect_err("7,430 over a 500 ceiling");
            assert!(
                why.contains("max_notional_per_order") && why.contains("7430"),
                "{tier:?} {order_client_id}: {why}"
            );
        }
        assert!(node.submitted.lock().unwrap().is_empty(), "nothing was sent");

        node.send(market_of("small", "bybit", "BTCUSDT", 0.005, Some(1e9)), Some(&facts))
            .expect("371.5 at the market, whatever it names");
        node.venue_saw("small");

        let CommandWire::Submit(mut close) = market_of("close", "bybit", "BTCUSDT", 0.1, None)
        else {
            unreachable!()
        };
        (close.side, close.reduce_only) = (-1, true);
        node.send(CommandWire::Submit(close), Some(&facts)).expect("a close is never sized");
        node.venue_saw("close");
        node.core.shutdown_and_join();
    }
}

/// **A market order with a `trigger_price` is judged at the higher of the trigger and the price the door holds**,
/// so neither number can size it under the ceiling on its own: a far trigger can't hide behind a low mark, and a
/// trigger of 1 can't hide a position worth the mark.
#[test]
fn a_market_order_with_a_trigger_is_judged_at_the_higher_of_the_trigger_and_the_node_price() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.limits = ceiling_of(500.0);
    let facts = FirstTick::new(false);
    facts.tick(74_300.0);
    let triggered = |order_client_id: &str, qty: f64, trigger: f64| {
        let CommandWire::Submit(mut req) =
            market_of(order_client_id, "bybit", "BTCUSDT", qty, None)
        else {
            unreachable!()
        };
        req.trigger_price = Some(trigger);
        CommandWire::Submit(req)
    };

    // 371.5 at the mark, 1,000 at the trigger: the trigger binds.
    let why = node
        .send(triggered("over-trigger", 0.005, 200_000.0), Some(&facts))
        .expect_err("1,000 at the trigger, over a 500 ceiling");
    assert!(why.contains("max_notional_per_order") && why.contains("1000"), "{why}");
    // 0.1 at the trigger, 7,430 at the mark: the mark binds.
    let why = node
        .send(triggered("over-mark", 0.1, 1.0), Some(&facts))
        .expect_err("7,430 at the mark, over a 500 ceiling");
    assert!(why.contains("max_notional_per_order") && why.contains("7430"), "{why}");
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing was sent");
    node.core.shutdown_and_join();
}

/// **No trading without a price** (the owner's ruling of 2026-10-30, "we cant trade until we get price"): an OPENING
/// market order on the engine's own symbol of a real account with no strategy mounted there used to go out at notional
/// zero, every cap passing, because no quote had reached the tap or the engine. This test pinned that as "the door
/// invents no price" (a market order must not be stopped for want of a number at the door, and the number it names must
/// not size it looser): it was protecting the plain pass of decision 0197, whose `no-reference-price` and floors it
/// took for a floor they are not on a grid with no `min_notional` (ctrader). Now the order is held for the first price,
/// sized and marked at it (`lane_twin_pricing`'s labelled-engine test has the mark: this venue's fill moves it), and
/// refused by name if none comes; a surface with no source has nothing to ask, so there the number it names is still
/// all that can bind it, no more loosely than when the door sized at that number.
#[test]
fn no_trading_without_a_price() {
    let mut node = Node::start("ctrader", "EURUSD", AccountTier::Demo);
    node.limits = ceiling_of(5_000.0);
    let facts = FirstTick::new(false);

    let why = node
        .send(market_of("early", "ctrader", "EURUSD", 0.1, None), Some(&facts))
        .expect_err("no price yet");
    for want in ["`EURUSD`", "no price", "REFUSED"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing was sent");

    facts.tick(1.25);
    node.send(market_of("priced", "ctrader", "EURUSD", 100.0, None), Some(&facts))
        .expect("125 under the ceiling, at the first price");
    node.venue_saw("priced");
    let why = node
        .send(market_of("over", "ctrader", "EURUSD", 10_000.0, Some(1.0)), Some(&facts))
        .expect_err("12,500 at the first price, whatever it names");
    assert!(why.contains("max_notional_per_order") && why.contains("12500"), "{why}");

    // No source to ask: the number it names is all that binds it.
    node.send(market_of("no-source", "ctrader", "EURUSD", 0.1, None), None)
        .expect("a surface with no price source");
    node.venue_saw("no-source");
    let why = node
        .send(market_of("named", "ctrader", "EURUSD", 10_000.0, Some(1.0)), None)
        .expect_err("10,000 at the number it names, over a 5,000 ceiling");
    assert!(why.contains("max_notional_per_order") && why.contains("10000"), "{why}");
    node.core.shutdown_and_join();
}

/// **A source that may wait holds the order for the first tick.** A surface that may not is refused at once, and the
/// ask has opened the lane (`no_trading_without_a_price` above).
#[test]
fn an_opening_market_order_is_held_for_the_first_tick_of_its_own_symbol() {
    let mut node = Node::start("ctrader", "EURUSD", AccountTier::Demo);
    let facts = Arc::new(FirstTick::new(true));
    let later = Arc::clone(&facts);
    let ticks = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        later.tick(1.25);
    });
    node.send(market_of("held", "ctrader", "EURUSD", 100.0, None), Some(&*facts))
        .expect("the first tick released it");
    assert_eq!(
        facts.price("ctrader", "ctrader", "EURUSD", false),
        Ok(Some(1.25)),
        "the send came back only once the tick had landed"
    );
    ticks.join().expect("the tick");
    node.venue_saw("held");
    node.core.shutdown_and_join();
}

/// **A strategy mount of this boot prices its own symbol, on a lane the tap may never see** (a mount that runs on candle
/// closes alone): the order goes on as it always did.
#[test]
fn a_symbol_a_strategy_mount_prices_owes_no_wait() {
    struct MountFed;
    impl SymbolAdmitter for MountFed {
        fn admission(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
            panic!("a mounted symbol asks no facts")
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            panic!("a mounted symbol asks no quote")
        }
        fn priced_by_mount(&self, venue: &str, symbol: &str) -> bool {
            (venue, symbol) == ("oanda", "EURUSD")
        }
    }

    impl AccountReadiness for MountFed {
        fn generation(&self, _: &str) -> u64 {
            0
        }
    }
    let order = market("oanda", "EURUSD", false);
    let roster = [engine("oanda", "EURUSD")];
    assert_eq!(plan_submit_symbol(&order, &roster, Some(&MountFed), true), SymbolPlan::Pass);
}

/// **A close is never held**, flagged or not, and neither is an order with a price of its own or a `paper` engine's. A
/// feed that cannot be had refuses an open at the Preview as at the send (a Preview reads and subscribes nothing).
#[test]
fn a_close_a_priced_order_and_a_paper_engine_owe_no_wait() {
    let venue = "bybit";
    let mounted = "BTCUSDT";
    let pending = Table::eth().with_price(Price::Pending);
    let own = engine(venue, mounted);
    let long = vike_execution::VenueBlock {
        positions: vec![vike_execution::PositionView {
            venue: venue.into(),
            symbol: mounted.into(),
            position_side: vike_model::events::PositionSide::Both,
            size: 1.0,
            average_price: 100.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liquidation_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }],
        ..own.clone()
    };
    let plain_sell = |qty: f64| {
        let CommandWire::Submit(mut req) = market(venue, mounted, false) else { unreachable!() };
        (req.side, req.qty) = (-1, qty);
        CommandWire::Submit(req)
    };
    let plan = |cmd: &CommandWire, block: &vike_execution::VenueBlock, commit| {
        plan_submit_symbol(cmd, std::slice::from_ref(block), Some(&pending), commit)
    };
    assert_eq!(plan(&market(venue, mounted, true), &own, true), SymbolPlan::Pass, "flagged");
    assert_eq!(plan(&plain_sell(1.0), &long, true), SymbolPlan::Pass, "implicit");
    assert!(
        matches!(plan(&plain_sell(2.0), &long, true), SymbolPlan::Place { wait: Some(_), .. }),
        "past the position, an open: the core judges all of it"
    );
    assert_eq!(plan(&submit(venue, mounted, None), &own, true), SymbolPlan::Pass, "priced");
    // A paper engine waits for its own symbol's CURRENT price as a demo one does (the owner, 2026-11-16).
    let paper = vike_execution::VenueBlock { tier: Some(AccountTier::Paper), ..own.clone() };
    assert!(
        matches!(
            plan(&market(venue, mounted, false), &paper, true),
            SymbolPlan::Place { wait: Some(_), .. }
        ),
        "paper"
    );
    assert_eq!(plan(&market(venue, mounted, false), &own, false), SymbolPlan::Pass, "a Preview");

    let failing = Table::eth().with_price(Price::Failing);
    let why = refused(plan_submit_symbol(
        &market(venue, mounted, false),
        std::slice::from_ref(&own),
        Some(&failing),
        false,
    ));
    assert!(why.contains("no price feed for `BTCUSDT`"), "{why}");
}

/// **A symbol the engine does not trade yet is valued by its own facts**, not at the engine's default: an IG epic
/// is 10,000 units, so 0.1 of one at 1.25 is worth 1,250, where the default value would read 0.125.
#[test]
fn a_market_order_on_a_new_symbol_is_judged_with_the_value_of_its_facts() {
    let mut node = Node::start("ig", "EURUSD", AccountTier::Demo);
    node.limits = ceiling_of(500.0);
    let facts = FirstTick::new(false);
    facts.tick(1.25);
    let why = node
        .send(market_of("epic", "ig", "GBPUSD", 0.1, None), Some(&facts))
        .expect_err("1,250 over a 500 ceiling");
    assert!(why.contains("max_notional_per_order") && why.contains("1250"), "{why}");
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing was sent");
    node.core.shutdown_and_join();
}

/// An order as the snapshot publishes it, on the plain bybit engine.
fn published(
    order_client_id: &str,
    order_type: vike_model::OrderType,
    price: Option<f64>,
) -> vike_execution::OrderView {
    vike_execution::OrderView {
        data_delay: None,
        order_client_id: order_client_id.into(),
        venue: "bybit".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.001,
        order_type,
        price,
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

/// **A `Modify` of a resting MARKET order is sized as the core sizes it**: at the market, never at the number it
/// names (the core's `gate_modify` judges the projected order, and a market order's price is no price). With no
/// price at the door the number it names still binds, as it did. A limit order is sized at the price named.
#[test]
fn a_modify_of_a_resting_market_order_is_sized_as_the_core_sizes_it() {
    use vike_model::OrderType;
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.limits = ceiling_of(500.0);
    let facts = FirstTick::new(false);
    facts.tick(74_300.0);
    let no_price = FirstTick::new(false);
    let modify = |qty: f64, price: Option<f64>| CommandWire::Modify {
        order_client_id: "m".into(),
        new_qty: Some(qty),
        new_price: price,
        new_trigger_price: None,
    };
    let market_order = [published("m", OrderType::Market, None)];

    let why = node
        .send_rostered_reading(modify(0.1, Some(1.0)), Some(&facts), &market_order)
        .expect_err("7,430 at the market, over a 500 ceiling");
    assert!(why.contains("max_notional_per_order") && why.contains("7430"), "{why}");
    node.send_rostered_reading(modify(0.005, Some(1e9)), Some(&facts), &market_order)
        .expect("371.5 at the market, whatever it names");

    let why = node
        .send_rostered_reading(modify(0.1, Some(74_300.0)), Some(&no_price), &market_order)
        .expect_err("7,430 at the number it names, the door holds no other");
    assert!(why.contains("7430"), "{why}");
    node.send_rostered_reading(modify(0.1, Some(1.0)), Some(&no_price), &market_order)
        .expect("0.1 at the number it names");

    // A number left on the resting order is no standing price either: the words of a market order with none.
    let stale = [published("m", OrderType::Market, Some(1.0))];
    let why = node
        .send_rostered_reading(modify(0.1, None), Some(&facts), &stale)
        .expect_err("no standing price to size it at");
    assert!(why.contains("no standing price"), "{why}");

    let limit_order = [published("m", OrderType::Limit, Some(1.0))];
    node.send_rostered_reading(modify(0.1, Some(1.0)), Some(&facts), &limit_order)
        .expect("a limit order is worth the price it names");
    node.send_rostered_reading(modify(0.1, Some(74_300.0)), Some(&facts), &limit_order)
        .expect_err("7,430 at the price it names");
    node.core.shutdown_and_join();
}

/// An order with no account is the venue's default account's: it is judged on THAT engine (not a
/// labelled one's) and admitted under the bare venue key. (A labelled account is admitted under its own: see
/// `a_labelled_account_is_admitted_a_symbol_under_its_own_route_key`.)
#[test]
fn the_admission_names_the_account_the_order_reaches() {
    let facts = Table::eth();
    let roster = [engine("bybit", "BTCUSDT"), engine("bybit#ALT", "SOLUSDT")];
    let (_, admit) =
        placed(plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&facts), true));
    assert_eq!(admit.expect("admitted").route_key, "bybit", "an absent account is the default one");
    let (_, admit) = placed(plan_submit_symbol(
        &submit("bybit", "ETHUSDT", Some("DEFAULT")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(admit.expect("admitted").route_key, "bybit", "DEFAULT is the same account");
}

/// **What must keep working, byte for byte**: the engine's own symbol (and a declared leg) passes
/// untouched with no lookup and no extra command; a per-order venue is not this gate's.
#[test]
fn the_engines_own_symbols_pass_untouched_and_ask_for_nothing() {
    let facts = Table::eth();
    for venue in ADMITS {
        let (mounted, other) = match venue {
            "aster" => ("BTCUSDT.P", "ETHUSDT"),
            "okx" => ("BTC-USDT-SWAP", "ETH-USDT-SWAP"),
            "deribit" => ("BTC-PERPETUAL", "ETH-PERPETUAL"),
            "alpaca" => ("AAPL", "MSFT"),
            "ctrader" | "oanda" | "dukascopy" | "fxcm" => ("EURUSD", "GBPUSD"),
            "ig" => ("CS.D.EURUSD.MINI.IP", "CS.D.GBPUSD.MINI.IP"),
            "ibkr" => ("AAPL.SMART.USD", "MSFT.SMART.USD"),
            _ => ("BTCUSDT", "ETHUSDT"),
        };
        let leg = vike_execution::VenueBlock {
            extra_symbols: vec![other.into()].into(),
            ..engine(venue, mounted)
        };
        assert_eq!(
            plan_submit_symbol(
                &submit(venue, mounted, None),
                std::slice::from_ref(&leg),
                Some(&facts),
                true
            ),
            SymbolPlan::Pass,
            "{venue}: the mounted symbol"
        );
        // A declared leg is traded already: no admission, no lookup, no price wait.
        assert_eq!(
            plan_submit_symbol(
                &submit(venue, other, None),
                std::slice::from_ref(&leg),
                Some(&facts),
                true
            ),
            SymbolPlan::Place { symbol: other.into(), admit: None, wait: None },
            "{venue}: a declared leg"
        );
    }
    assert!(facts.asked().is_empty(), "no lookup for a symbol the engine already trades");
    for venue in PER_ORDER {
        assert_eq!(
            plan_submit_symbol(
                &submit(venue, "ANYTHING", None),
                &[engine(venue, "OTHER")],
                Some(&facts),
                true
            ),
            SymbolPlan::Pass,
            "{venue}: its adapter reads the order's symbol (a priced order on a real account)"
        );
    }
}

/// **An order on the engine's OWN symbol asks the source to hold its price lane** (decision 0197): on every `Admits`
/// venue, for a `demo` and a `live` engine, the source is asked once per order and the plan asks no quote. A limit
/// order's plan is still `Pass`, whatever the quote; nothing is refused when the lane cannot be had. An OPENING
/// market order owes a wait for its first price (`no_trading_without_a_price` below has the rest).
#[test]
fn an_order_on_the_engines_own_symbol_asks_for_its_lane_and_nothing_else_changes() {
    for venue in ADMITS {
        let mounted = match venue {
            "aster" => "BTCUSDT.P",
            "okx" => "BTC-USDT-SWAP",
            "deribit" => "BTC-PERPETUAL",
            "alpaca" => "AAPL",
            "ctrader" | "oanda" | "dukascopy" | "fxcm" => "EURUSD",
            "ig" => "CS.D.EURUSD.MINI.IP",
            "ibkr" => "AAPL.SMART.USD",
            _ => "BTCUSDT",
        };
        for tier in [AccountTier::Demo, AccountTier::Live] {
            for price in [Price::Ready, Price::Pending, Price::Failing] {
                let facts = Table::eth().with_price(price);
                let block =
                    vike_execution::VenueBlock { tier: Some(tier), ..engine(venue, mounted) };
                assert_eq!(
                    plan_submit_symbol(
                        &submit(venue, mounted, None),
                        std::slice::from_ref(&block),
                        Some(&facts),
                        true
                    ),
                    SymbolPlan::Pass,
                    "{venue} {tier:?}: a limit order's plan is as it was whatever the quote"
                );
                assert_eq!(
                    plan_submit_symbol(
                        &market(venue, mounted, false),
                        std::slice::from_ref(&block),
                        Some(&facts),
                        true
                    ),
                    SymbolPlan::Place {
                        symbol: mounted.into(),
                        admit: None,
                        wait: Some(PriceWait { route_key: venue.into(), venue: venue.into() }),
                    },
                    "{venue} {tier:?}: an opening market order waits for its first price"
                );
                assert_eq!(
                    facts.holds(),
                    [format!("{venue}/{mounted}"), format!("{venue}/{mounted}")],
                    "{venue} {tier:?}: the source is asked to hold the lane, once per order"
                );
                assert!(facts.events().is_empty(), "{venue}: no quote is asked and none awaited");
                assert!(facts.asked().is_empty(), "{venue}: no lookup either");
            }
        }
    }
}

/// **A LABELLED account's order on its own symbol asks for the lane under ITS route key** (decision 0198), `demo`
/// or `live`, and the plan is still `Pass` with no quote asked: the account the order names, not the venue's
/// default engine, is the one that holds the lane.
#[test]
fn a_labelled_engines_order_on_its_own_symbol_asks_for_the_lane_under_its_own_route_key() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let facts = Table::eth().with_price(Price::Failing);
        let roster = [
            vike_execution::VenueBlock { tier: Some(tier), ..engine("hyperliquid#LIVE", "BTC") },
            engine("hyperliquid", "BTC"),
        ];
        assert_eq!(
            plan_submit_symbol(
                &submit("hyperliquid", "BTC", Some("LIVE")),
                &roster,
                Some(&facts),
                true
            ),
            SymbolPlan::Pass,
            "{tier:?}: the plan is as it was"
        );
        assert_eq!(
            facts.holds(),
            ["hyperliquid#LIVE/BTC".to_string()],
            "{tier:?}: its own route key"
        );
        assert!(
            facts.events().is_empty() && facts.asked().is_empty(),
            "{tier:?}: nothing else asked"
        );
    }
}

/// **Only a committing surface asks**: a Preview reads and subscribes nothing, an engine that states no tier or names no
/// symbol has nothing to hold, a surface with no source asks nobody, and a DECLARED leg or a foreign symbol is the old
/// price rule's, not this one. A `paper` engine holds its own lane like a demo one (the owner, 2026-11-16).
#[test]
fn only_a_committing_surface_for_a_real_engine_asks_for_the_own_lane() {
    let facts = Table::eth();
    let demo = engine("bybit", "BTCUSDT");
    let own = submit("bybit", "BTCUSDT", None);
    assert_eq!(
        plan_submit_symbol(&own, std::slice::from_ref(&demo), Some(&facts), false),
        SymbolPlan::Pass
    );
    assert!(facts.holds().is_empty(), "a Preview subscribes nothing");

    let paper =
        vike_execution::VenueBlock { tier: Some(AccountTier::Paper), ..engine("bybit", "BTCUSDT") };
    assert_eq!(plan_submit_symbol(&own, &[paper], Some(&facts), true), SymbolPlan::Pass);
    assert_eq!(facts.holds().len(), 1, "a paper engine holds its own lane");
    let unknown = vike_execution::VenueBlock { tier: None, ..engine("bybit", "BTCUSDT") };
    assert_eq!(plan_submit_symbol(&own, &[unknown], Some(&facts), true), SymbolPlan::Pass);
    let unmounted = engine("bybit", "");
    assert_eq!(plan_submit_symbol(&own, &[unmounted], Some(&facts), true), SymbolPlan::Pass);
    assert_eq!(
        plan_submit_symbol(&own, &[demo], None, true),
        SymbolPlan::Pass,
        "no source, no ask"
    );
    assert_eq!(facts.holds().len(), 1, "no tier, no symbol: nobody else is asked");

    // A foreign symbol is admitted and priced by the old rule; it holds no OWN lane.
    let foreign = submit("bybit", "ETHUSDT", None);
    let block = engine("bybit", "BTCUSDT");
    let _ = plan_submit_symbol(&foreign, &[block], Some(&facts), true);
    assert_eq!(facts.holds().len(), 1, "the engine's own symbol only");
}

/// **A limit order on the engine's own symbol is accepted with no quote at all, end to end** (a market the
/// venue keeps closed still takes a resting limit order), through the ONE acceptance path into a real core: it
/// reaches the venue client, the source is asked to hold the lane, and no price was asked or awaited.
#[test]
fn a_limit_order_on_the_own_symbol_is_accepted_with_no_quote_and_asks_for_the_lane() {
    let facts = Table::eth().with_price(Price::Failing);
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let order_client_id = node
        .send(limit("own-limit", "bybit", "BTCUSDT", 0.01, 60_000.0), Some(&facts))
        .expect("a limit order on the engine's own symbol is accepted without a quote");
    assert_eq!(order_client_id, "own-limit");
    node.venue_saw(&order_client_id);
    assert_eq!(facts.holds(), ["bybit/BTCUSDT".to_string()]);
    assert!(facts.events().is_empty(), "no price was asked, nothing awaited");
    node.core.shutdown_and_join();
}

/// **The mounted symbol, another case of it, a declared leg, no symbol and no mounted symbol behave
/// exactly as before on EVERY per-order venue**, paper included: nothing here is a foreign symbol.
#[test]
fn a_per_order_venues_own_symbol_passes_untouched_on_every_tier() {
    for venue in PER_ORDER.iter().chain(PER_ORDER_UNSAFE.iter()) {
        for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
            let leg = vike_execution::VenueBlock {
                tier: Some(tier),
                extra_symbols: vec!["LEG".into()].into(),
                ..engine(venue, "Mounted")
            };
            for (cmd, what) in [
                (submit(venue, "Mounted", None), "the mounted symbol"),
                (submit(venue, "MOUNTED", None), "the mounted symbol, another case"),
                (market(venue, "Mounted", false), "a priceless market order on it"),
                (submit(venue, "LEG", None), "a declared leg"),
                (submit(venue, "", None), "no symbol"),
            ] {
                assert_eq!(
                    plan_submit_symbol(&cmd, std::slice::from_ref(&leg), None, true),
                    SymbolPlan::Pass,
                    "{venue} {tier:?}: {what}"
                );
            }
            let unmounted = vike_execution::VenueBlock { tier: Some(tier), ..engine(venue, "") };
            assert_eq!(
                plan_submit_symbol(&market(venue, "ANY", false), &[unmounted], None, true),
                SymbolPlan::Pass,
                "{venue} {tier:?}: an engine mounted on no symbol (polymarket) judges every token"
            );
        }
    }
}

/// **A paper account is ONE book on every unsafe per-order venue** (and on oanda, an `Admits` venue
/// since the any-symbol step): `ExecutionClientPaper` stamps its own symbol on every fill, so a foreign
/// symbol would fill on the mounted symbol's bars and be booked on it. The door refuses it with the
/// words the `Admits` venues use.
#[test]
fn a_paper_account_refuses_a_foreign_symbol_on_every_per_order_venue() {
    let paper = |venue: &str| vike_execution::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine(venue, "MOUNTED")
    };
    for venue in PER_ORDER_UNSAFE.iter().chain(["oanda"].iter()) {
        let why = refused(plan_submit_symbol(
            &submit(venue, "FOREIGN", None),
            &[paper(venue)],
            None,
            true,
        ));
        assert!(why.contains("trades only the symbol it was started on"), "{venue}: {why}");
        assert!(why.contains("`FOREIGN`") && why.contains("REFUSED"), "{venue}: {why}");
        assert_eq!(
            submit_symbol_refusal(&submit(venue, "FOREIGN", None), &[paper(venue)], None)
                .map(|r| r.text),
            Some(why),
            "{venue}: the Preview prints the same"
        );
    }
    // Polymarket was exempt while its row was `PerOrder` (its engine judged every token). It is `Admits` now
    // (the outcome-token door), and a paper engine is one book there like anywhere: a mounted token's engine
    // and an engine mounted on no symbol both refuse another token, in the words the `Admits` venues use, on a
    // surface with no facts source (with one, the engine mounted on no symbol admits it: `outcome_token_door.rs`).
    for mounted in ["MOUNTED_TOKEN", ""] {
        let polymarket = vike_execution::VenueBlock {
            tier: Some(AccountTier::Paper),
            ..engine("polymarket", mounted)
        };
        for cmd in
            [submit("polymarket", "OTHER_TOKEN", None), market("polymarket", "OTHER_TOKEN", false)]
        {
            let why =
                refused(plan_submit_symbol(&cmd, std::slice::from_ref(&polymarket), None, true));
            assert!(why.contains("trades only the symbol it was started on"), "{mounted:?}: {why}");
            assert!(why.contains("`OTHER_TOKEN`") && why.contains("REFUSED"), "{mounted:?}: {why}");
        }
    }
}

/// **oanda used to be a per-order venue whose priceless opening order on a foreign symbol was refused for
/// want of any price source; it has one now, so it takes the `Admits` path.** A market order on a foreign
/// pair is HELD for the first quote of the pair (the watcher subscribes it), refused naming the cause when
/// the node has no feed, and a reduce-only order on a pair the engine holds nothing in is refused (it can
/// only fail). The old limit-price fence is gone with the row.
#[test]
fn oanda_prices_a_foreign_pair_through_the_door_not_through_a_limit_price_fence() {
    let block = engine("oanda", "EURUSD");
    let roster = std::slice::from_ref(&block);

    let pending = Table::eth().with_price(Price::Pending);
    match plan_submit_symbol(&market("oanda", "gbpusd", false), roster, Some(&pending), true) {
        SymbolPlan::Place { symbol, admit, wait } => {
            assert_eq!(symbol, "GBPUSD");
            assert!(admit.is_some(), "the pair is admitted first");
            assert_eq!(
                wait,
                Some(PriceWait { route_key: "oanda".into(), venue: "oanda".into() }),
                "held for the first quote"
            );
        }
        other => panic!("expected a held placement, got {other:?}"),
    }
    assert!(pending.events().contains(&"price:GBPUSD:subscribe".to_string()));

    let failing = Table::eth().with_price(Price::Failing);
    let why = refused(plan_submit_symbol(
        &market("oanda", "gbpusd", false),
        roster,
        Some(&failing),
        true,
    ));
    assert!(why.contains("no price feed for `GBPUSD`") && why.contains("REFUSED"), "{why}");

    let facts = Table::eth();
    let why =
        refused(plan_submit_symbol(&market("oanda", "gbpusd", true), roster, Some(&facts), true));
    assert!(why.contains("reduce-only") && why.contains("REFUSED"), "{why}");
    assert_eq!(
        facts.asked(),
        ["oanda/GBPUSD"],
        "looked up once, then refused before a slot is spent"
    );
}

/// **oanda reads a user name** (decision 0290): what is not a symbol is refused by its shape, and what no kept row
/// names (the venue's own `GBP_USD` among them) by the list, both before any lookup, because the symbol reaches a URL
/// path and a JSON body downstream. And one instrument is one symbol in an engine: the engine's own pair or a leg
/// typed in another case is that symbol, and the venue's `EUR_USD` is no second name of it.
#[test]
fn oanda_refuses_what_is_not_an_instrument_and_the_other_spelling_of_one_the_engine_trades() {
    let facts = Table::eth();
    for (bad, word) in [
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("GBP USD", "is not a symbol"),
        ("A\"}", "is not a symbol"),
        ("GBP", "carries no row"),
        ("GBP-USD", "carries no row"),
        ("GBP_USD", "carries no row"),
        ("GBP_USD_X", "carries no row"),
        ("_USD", "carries no row"),
        ("GBP_", "carries no row"),
        ("GBPUS1", "carries no row"),
        ("GBP/USD", "carries no row"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("oanda", bad, None),
            &[engine("oanda", "EURUSD")],
            Some(&facts),
            true,
        ));
        assert!(why.contains(word), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");

    let leg = vike_execution::VenueBlock {
        extra_symbols: vec!["AUDUSD".into()].into(),
        ..engine("oanda", "EURUSD")
    };
    assert_eq!(
        plan_submit_symbol(
            &submit("oanda", "eurusd", None),
            &[engine("oanda", "EURUSD")],
            Some(&facts),
            true
        ),
        SymbolPlan::Place { symbol: "EURUSD".into(), admit: None, wait: None },
        "the engine's own instrument, in its own spelling"
    );
    let plan = plan_submit_symbol(&submit("oanda", "audusd", None), &[leg], Some(&facts), true);
    assert!(
        matches!(&plan, SymbolPlan::Place { symbol, admit: None, .. } if symbol == "AUDUSD"),
        "a declared leg is traded already: {plan:?}"
    );
    let why = refused(plan_submit_symbol(
        &submit("oanda", "EUR_USD", None),
        &[engine("oanda", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("carries no row") && why.contains("REFUSED"), "{why}");
    assert!(facts.asked().is_empty(), "no lookup, no slot spent");
}

// The `PerOrderUnsafe` rule (a foreign symbol refused with the row's reason, on any tier and for any order)
// has no roster venue since fxcm's price lane (`PER_ORDER_UNSAFE` is empty), so its test went with the last
// row; the rule's code stays for `just new-venue`'s classification hint, like `MountedOnly`'s.

/// The mounted symbol typed in another case is the same symbol: placed on the canonical spelling,
/// with nothing to admit.
#[test]
fn the_mounted_symbol_in_another_case_is_placed_without_an_admission() {
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("bybit", "btcusdt", None),
        &[engine("bybit", "BTCUSDT")],
        None,
        true,
    ));
    assert_eq!((symbol.as_str(), admit), ("BTCUSDT", None));
}

/// Only what is certain is refused, and only a Submit: an engine that did not say what it trades, no
/// engine at all, another venue's roster and another verb are not this gate's.
#[test]
fn it_judges_only_a_submit_against_a_published_engine() {
    let ok = submit("bybit", "ETHUSDT", None);
    let facts = Table::eth();
    for roster in [
        vec![engine("bybit", "")],
        vec![],
        vec![engine("okx", "BTC-USDT-SWAP")],
        vec![engine("bybit#ALT", "BTCUSDT")],
    ] {
        assert_eq!(
            plan_submit_symbol(&ok, &roster, Some(&facts), true),
            SymbolPlan::Pass,
            "{roster:?}"
        );
    }
    let roster = [engine("bybit", "BTCUSDT")];
    for cmd in [
        CommandWire::Cancel("c-1".into()),
        CommandWire::Flatten { venue: "bybit".into(), symbol: "ETHUSDT".into(), account: None },
        CommandWire::MassCancel {
            venue: Some("bybit".into()),
            symbol: Some("ETHUSDT".into()),
            account: None,
        },
    ] {
        assert_eq!(
            plan_submit_symbol(&cmd, &roster, Some(&facts), true),
            SymbolPlan::Pass,
            "{cmd:?}"
        );
    }
    assert!(facts.asked().is_empty());
}

/// A published deribit block: the engine mounted on `symbol`, valued INVERSE (an amount of US dollars)
/// when it is a coin-margined perpetual, the default (linear) otherwise.
fn deribit_engine(symbol: &str, inverse: bool) -> vike_execution::VenueBlock {
    valued_engine("deribit", symbol, inverse)
}

/// [`engine`] for `route_key` on `symbol`, its contract valued INVERSE (one unit of the quote money per
/// contract) when `inverse`, the default (linear) otherwise.
fn valued_engine(route_key: &str, symbol: &str, inverse: bool) -> vike_execution::VenueBlock {
    let mut block = engine(route_key, symbol);
    if inverse {
        block.contract_values = Arc::new(indexmap::IndexMap::from([(
            symbol.to_string(),
            vike_model::ContractValue::inverse(1.0),
        )]));
    }
    block
}

/// **deribit admits a perpetual of the engine's own money model**, by its user name and on
/// its own grid: an INVERSE engine takes another inverse instrument of its coin (the admission carries the
/// inverse contract value), a LINEAR one another linear perpetual (the default value).
#[test]
fn deribit_admits_a_perpetual_of_the_engines_money_model() {
    let facts = Table::eth();
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("deribit", "ethusd.p", None),
        &[deribit_engine("ETHUSD_260327", true)],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETHUSD.P");
    let admit = admit.expect("admitted first");
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(0.05), Some(1.0)));
    assert_eq!(admit.contract_value, Some(vike_model::ContractValue::inverse(1.0)));

    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("deribit", "ethusdc.p", None),
        &[deribit_engine("BTCUSDC.P", false)],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETHUSDC.P");
    let admit = admit.expect("admitted first");
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(0.01), Some(0.0001)));
    assert_eq!(admit.contract_value, None, "linear is the default");
    assert_eq!(facts.asked(), ["deribit/ETHUSD.P", "deribit/ETHUSDC.P"]);
}

/// **An inverse contract of another coin is refused BY NAME at the door** (decision 0258: an inverse engine keeps
/// its books in its settle coin, one per engine), naming both instruments and both coins, before any lookup is
/// committed and with nothing admitted; the Preview prints the same words. A contract of the engine's own coin is
/// admitted.
#[test]
fn deribit_refuses_an_inverse_contract_of_another_coin_by_name() {
    let facts = Table::eth();
    let btc = [deribit_engine("BTCUSD.P", true)];
    for (other, coin) in [("ETHUSD.P", "ETH"), ("SOLUSD.P", "SOL")] {
        let why =
            refused(plan_submit_symbol(&submit("deribit", other, None), &btc, Some(&facts), true));
        assert!(why.contains(&format!("`{other}` settles in {coin}")), "{why}");
        assert!(why.contains("mounted on `BTCUSD.P`, keeps its books in BTC"), "{why}");
        assert!(why.contains("one settle coin"), "{why}");
        assert_eq!(
            submit_symbol_refusal(&submit("deribit", other, None), &btc, Some(&facts))
                .map(|r| r.text),
            Some(why)
        );
    }
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit_quantity("deribit", "BTCUSD_260327", 10.0),
        &btc,
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "BTCUSD_260327", "the engine's own coin is admitted");
    assert!(admit.is_some());
}

/// **The other money model is refused BY NAME**, naming both instruments and both models, and a
/// Preview prints exactly that. Nothing is admitted.
#[test]
fn deribit_refuses_the_other_money_model_by_name() {
    let facts = Table::eth();
    let inverse_engine = [deribit_engine("BTCUSD.P", true)];
    let why = refused(plan_submit_symbol(
        &submit("deribit", "ETHUSDC.P", None),
        &inverse_engine,
        Some(&facts),
        true,
    ));
    assert!(why.contains("`ETHUSDC.P`") && why.contains("`BTCUSD.P`"), "{why}");
    assert!(why.contains("LINEAR") && why.contains("INVERSE"), "{why}");
    assert!(why.contains("one account trades one money model"), "{why}");
    assert!(why.contains("REFUSED"), "says what happened: {why}");
    assert_eq!(
        submit_symbol_refusal(&submit("deribit", "ETHUSDC.P", None), &inverse_engine, Some(&facts))
            .map(|r| r.text),
        Some(why)
    );
    let linear_engine = [deribit_engine("BTCUSDC.P", false)];
    let why = refused(plan_submit_symbol(
        &submit("deribit", "ETHUSD.P", None),
        &linear_engine,
        Some(&facts),
        true,
    ));
    assert!(why.contains("INVERSE") && why.contains("mounted on `BTCUSDC.P`"), "{why}");
}

/// **Only a perpetual, a dated future and a USDC option are symbols of this adapter** (decision 0233): a
/// coin-settled option and a spot pair are refused by name, and a combo, a bare symbol of another venue, an old
/// instrument name and anything with a character no instrument has by the door's reading of a user name, each BEFORE
/// any lookup (the name reaches a JSON-RPC frame downstream).
#[test]
fn deribit_refuses_a_coin_option_a_spot_pair_and_a_combo_by_name_before_any_lookup() {
    let facts = Table::eth();
    let roster = [deribit_engine("BTCUSD.P", true)];
    for (typed, what) in [
        ("BTC-27MAR26-60000-C", "coin-settled option"),
        ("BTCUSDC", "spot pair"),
        ("ETHUSDT", "carries no row"),
        ("BTC-FS-31OCT26_PERP", "carries no row"),
        ("BTC-27FOO26", "carries no row"),
        ("BTC-PERPETUAL", "carries no row"),
        ("ETH_USDC-27MAR26-D5-C", "not a deribit option name"),
        ("ETH-PERPETUAL\"}", "is not a symbol"),
        ("ETH PERP", "is not a symbol"),
        ("A-B-C-D-E-F", "carries no row"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("deribit", typed, None),
            &roster,
            Some(&facts),
            true,
        ));
        assert!(why.contains(what), "{typed}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **A dated future and a USDC option are admitted** by the engine of their money model, by their user names
/// (an option's is the venue's own name: the decimal point of a strike keeps its lower-case `d`), on their own
/// grids. An inverse engine takes an inverse dated future and refuses the linear option; a linear one, mounted on a
/// perpetual OR on a dated future, takes a linear dated future and a USDC option.
#[test]
fn deribit_admits_a_dated_future_and_a_usdc_option_of_the_engines_money_model() {
    let facts = Table::eth();
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit_quantity("deribit", "btcusd_260327", 10.0),
        &[deribit_engine("BTCUSD.P", true)],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "BTCUSD_260327");
    let admit = admit.expect("admitted first");
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(2.5), Some(10.0)));
    assert_eq!(admit.contract_value, Some(vike_model::ContractValue::inverse(1.0)));

    // an inverse engine mounted on a DATED future takes another inverse instrument of its coin too
    let (symbol, _) = placed(plan_submit_symbol(
        &submit("deribit", "ETHUSD.P", None),
        &[deribit_engine("ETHUSD_260327", true)],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETHUSD.P");

    for mounted in ["BTCUSDC.P", "BTCUSDC_260327"] {
        let engine = [deribit_engine(mounted, false)];
        for (typed, canonical) in [
            ("btcusdc_260327", "BTCUSDC_260327"),
            ("eth_usdc-27mar26-2500-c", "ETH_USDC-27MAR26-2500-C"),
            ("XRP_USDC-27MAR26-2D5-C", "XRP_USDC-27MAR26-2d5-C"),
        ] {
            if mounted == canonical {
                continue;
            }
            let (symbol, admit) = placed(plan_submit_symbol(
                &submit_quantity("deribit", typed, 1000.0),
                &engine,
                Some(&facts),
                true,
            ));
            assert_eq!(symbol, canonical, "mounted on {mounted}");
            assert_eq!(
                admit.expect("admitted first").contract_value,
                None,
                "linear is the default"
            );
        }
    }

    let why = refused(plan_submit_symbol(
        &submit_quantity("deribit", "ETH_USDC-27MAR26-2500-C", 1000.0),
        &[deribit_engine("BTCUSD.P", true)],
        Some(&facts),
        true,
    ));
    assert!(why.contains("LINEAR") && why.contains("INVERSE"), "{why}");
    assert!(why.contains("one account trades one money model"), "{why}");
}

/// A published bybit block (the demo account `DEMO`) mounted on `symbol`, valued INVERSE when `inverse`.
fn bybit_engine(symbol: &str, inverse: bool) -> vike_execution::VenueBlock {
    valued_engine("bybit#DEMO", symbol, inverse)
}

/// **A linear engine refuses a coin-margined contract BY NAME** (the bybit demo proof of the classes,
/// 2026-10-31: the inverse perpetual and the inverse dated future, `BTCUSD.P` and `BTCUSD_261225` by their user names,
/// were admitted into the linear `bybit#DEMO` engine, after which the
/// engine DROPPED the fill of every linear symbol, a dated future and a spot pair among them, while the venue
/// filled them). The sentence names both kinds, both symbols, the consequence and the way out, the preview
/// prints exactly it, and nothing is asked of the price lane or admitted.
#[test]
fn a_linear_bybit_engine_refuses_a_coin_margined_contract_by_name() {
    let facts = Table::eth();
    let roster = [bybit_engine("BTCUSDT.P", false)];
    for typed in ["btcusd.p", "BTCUSD_261225"] {
        let canonical = typed.to_ascii_uppercase();
        let why = refused(plan_submit_symbol(
            &submit("bybit", typed, Some("DEMO")),
            &roster,
            Some(&facts),
            true,
        ));
        assert!(why.contains(&format!("`{canonical}` is INVERSE")), "{why}");
        assert!(why.contains("mounted on `BTCUSDT.P`, is LINEAR"), "{why}");
        assert!(
            why.contains(
                "DROP the fills of every linear symbol it trades, its mounted `BTCUSDT.P` included"
            ),
            "{why}"
        );
        assert!(why.contains("NOT admitted") && why.contains("decision 0233"), "{why}");
        assert!(why.contains("Use a separate account mounted on an inverse contract"), "{why}");
        assert!(why.contains("The order was REFUSED"), "says what happened: {why}");
        assert_eq!(
            submit_symbol_refusal(&submit("bybit", typed, Some("DEMO")), &roster, Some(&facts))
                .map(|r| r.text),
            Some(why),
            "the preview prints the same words"
        );
    }
    assert!(facts.events().is_empty(), "a refusal admits and prices nothing: {:?}", facts.events());
}

/// **Everything a linear bybit engine took before, it still takes**: a perpetual, a dated linear future and a
/// spot pair, each with the linear default value (no row in the engine's grid).
#[test]
fn a_linear_bybit_engine_still_admits_every_linear_class() {
    let facts = Table::eth();
    let roster = [bybit_engine("BTCUSDT.P", false)];
    for (typed, canonical) in
        [("ethusdt.p", "ETHUSDT.P"), ("ETHUSDT_261127", "ETHUSDT_261127"), ("dogeusdt", "DOGEUSDT")]
    {
        let (symbol, admit) = placed(plan_submit_symbol(
            &submit("bybit", typed, Some("DEMO")),
            &roster,
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, canonical);
        assert_eq!(admit.expect("admitted first").contract_value, None, "{typed}: linear");
    }
}

/// **The rule is the ENGINE's kind, not the venue's name**: an engine mounted on an inverse contract takes
/// another inverse one and refuses a linear one, naming the symbol it would lose (the fills of a linear
/// symbol in an inverse engine are dropped from the first one). Bybit mounts no inverse contract today; the
/// row is for the day it does, and for any other venue whose facts state an inverse symbol.
#[test]
fn an_inverse_engine_takes_an_inverse_contract_and_refuses_a_linear_one() {
    let facts = Table::eth();
    let roster = [bybit_engine("BTCUSD.P", true)];
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("bybit", "btcusd_261225", Some("DEMO")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "BTCUSD_261225");
    assert_eq!(
        admit.expect("admitted first").contract_value,
        Some(vike_model::ContractValue::inverse(1.0))
    );
    let why = refused(plan_submit_symbol(
        &submit("bybit", "ETHUSDT_261127", Some("DEMO")),
        &roster,
        Some(&facts),
        true,
    ));
    assert!(why.contains("`ETHUSDT_261127` is LINEAR"), "{why}");
    assert!(why.contains("mounted on `BTCUSD.P`, is INVERSE"), "{why}");
    assert!(why.contains("DROP the fills of `ETHUSDT_261127`"), "{why}");
    assert!(why.contains("Use a separate account mounted on a linear contract"), "{why}");
}

/// **One settle coin per inverse engine, for bybit as for deribit** (decision 0258, item 7): a bybit engine mounted
/// on `BTCUSD.P` keeps its books in BTC, so the coin-margined `ETHUSD.P` is refused at the door in the shared rule's
/// words (both coins are their kept rows' settle coins), the preview prints them, and a contract of its own coin is
/// admitted.
///
/// Base: the coin fence was deribit's alone, so `ETHUSD.P` was ADMITTED into the `BTCUSD.P` engine.
#[test]
fn a_bybit_inverse_engine_refuses_an_inverse_contract_of_another_coin_by_name() {
    let facts = Table::eth();
    let roster = [bybit_engine("BTCUSD.P", true)];
    let order = submit("bybit", "ethusd.p", Some("DEMO"));
    let why = refused(plan_submit_symbol(&order, &roster, Some(&facts), true));
    assert!(why.contains("`ETHUSD.P` settles in ETH"), "{why}");
    assert!(why.contains("mounted on `BTCUSD.P`, keeps its books in BTC"), "{why}");
    assert!(why.contains("one settle coin"), "{why}");
    assert_eq!(submit_symbol_refusal(&order, &roster, Some(&facts)).map(|r| r.text), Some(why));
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("bybit", "btcusd_261225", Some("DEMO")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "BTCUSD_261225", "the engine's own coin is admitted");
    assert!(admit.is_some());
}

/// **One settle coin per LINEAR engine too** (decision 0258, item 7; the owner's ruling of 2026-11-22): a binance engine
/// mounted on the USDT-settled `BTCUSDT.P` keeps its books in the USDT wallet, so the USDC-settled linear `BTCUSDC.P`
/// is refused at the door in the shared rule's words (both coins are the kept rows' settle coins, `vike_model::MoneyModel`),
/// the preview prints them, and a linear contract of its own coin is admitted.
///
/// Base: the door judged a settle coin on an inverse engine only, so `BTCUSDC.P` was ADMITTED into the `BTCUSDT.P` engine.
#[test]
fn a_linear_engine_refuses_a_linear_contract_of_another_settle_coin_by_name() {
    let facts = Table::eth();
    let roster = [engine("binance#DEMO", "BTCUSDT.P")];
    let order = submit("binance", "btcusdc.p", Some("DEMO"));
    let why = refused(plan_submit_symbol(&order, &roster, Some(&facts), true));
    // The shared rule's sentence, byte for byte (inside the door's account wrapper).
    assert!(
        why.contains(
            "`BTCUSDC.P` settles in USDC but this engine, mounted on `BTCUSDT.P`, keeps its books in USDT, and one \
             engine holds one settle coin (the venue keeps one wallet per coin). Use an account mounted on a USDC \
             contract"
        ),
        "{why}"
    );
    assert_eq!(submit_symbol_refusal(&order, &roster, Some(&facts)).map(|r| r.text), Some(why));
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("binance", "ethusdt.p", Some("DEMO")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETHUSDT.P", "a linear contract of the engine's own coin is admitted");
    assert!(admit.is_some());
}

/// **A symbol the engine already trades is not judged again**: a restart restores an admission the engine
/// holds (the boot's re-admission does not pass the door at all), and an order on it is placed, refreshed, as
/// before, whatever kind it is. Deribit's own rule (above) is untouched: its words fire first.
#[test]
fn a_restored_admission_of_the_other_kind_is_placed_not_refused() {
    let facts = Table::eth();
    let mut restored = bybit_engine("BTCUSDT", false);
    restored.extra_symbols = vec!["BTCUSD".to_string()].into();
    restored.contract_values = Arc::new(indexmap::IndexMap::from([(
        "BTCUSD".to_string(),
        vike_model::ContractValue::inverse(1.0),
    )]));
    let (symbol, _) = placed(plan_submit_symbol(
        &submit("bybit", "btcusd", Some("DEMO")),
        &[restored],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "BTCUSD");
}

/// **End to end, through a real core**: the coin-margined order is refused and reaches no venue, so the
/// engine's grid never gets an inverse row, and the dated linear future and the mounted perpetual both fill
/// and BOOK. (Before the fence the same sequence admitted `BTCUSD.P`, and the next fill of every linear symbol
/// was dropped by `Account::kind_is_unknown_for`: `vike-execution`'s `admit_symbol` suite pins that half.)
#[test]
fn a_refused_coin_margined_admission_leaves_every_linear_fill_booked() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT.P", AccountTier::Demo);
    let err = node
        .send(limit("inv", "bybit", "BTCUSD.P", 12.0, 3000.0), Some(&facts))
        .expect_err("a coin-margined contract is refused by a linear engine");
    assert!(err.contains("INVERSE") && err.contains("DROP"), "{err}");
    assert!(!node.blocks()[0].trades("BTCUSD.P"), "nothing was admitted");

    node.send(limit("dated", "bybit", "ETHUSDT_261127", 0.1, 3000.0), Some(&facts))
        .expect("a dated linear future is admitted");
    node.venue_saw("dated");
    let cell = node.core.snapshot_cell();
    wait_until("the dated future's position", || {
        cell.load().positions.iter().any(|p| p.symbol == "ETHUSDT_261127" && p.size > 0.0)
    });
    node.send(limit("own", "bybit", "BTCUSDT.P", 0.01, 60_000.0), Some(&facts))
        .expect("the mounted perpetual");
    node.venue_saw("own");
    wait_until("the mounted perpetual's position", || {
        cell.load().positions.iter().any(|p| p.symbol == "BTCUSDT.P" && p.size > 0.0)
    });
    assert!(
        node.submitted.lock().unwrap().iter().all(|o| o.order_client_id != "inv"),
        "the refused order never reached the venue"
    );
    node.core.shutdown_and_join();
}

/// **A deribit option's user name is the venue's own name** (decision 0290's O-3): the door reads it by its shape,
/// idempotently, a decimal strike keeping the venue's lower-case `d`; a perpetual and a dated future are their user
/// names (`BTCUSD.P`, `BTCUSDC.P`, `BTCUSD_261225`), and the old instrument names name no row.
#[test]
fn deribit_reads_each_instrument_class_by_its_user_name() {
    for (typed, canonical) in [
        ("btc_usdc-6nov26-77000-c", "BTC_USDC-6NOV26-77000-C"),
        ("TRX_USDC-6NOV26-0D275-P", "TRX_USDC-6NOV26-0d275-P"),
    ] {
        let got = door_symbol("deribit", typed).expect("a rule").expect(typed);
        assert_eq!(got, canonical, "{typed}");
        assert_eq!(door_symbol("deribit", &got), Some(Ok(canonical.to_string())));
    }
    let rows = crate::testutil::door_rows();
    for (typed, user) in
        [("btcusd.p", "BTCUSD.P"), (" btcusdc.p ", "BTCUSDC.P"), ("btcusd_261225", "BTCUSD_261225")]
    {
        assert_eq!(door_user_symbol("deribit", typed, &rows).as_deref(), Ok(user), "{typed}");
    }
    for old in ["ETH-PERPETUAL", "BTC_USDC-PERPETUAL", "BTC-25DEC26"] {
        let why = door_user_symbol("deribit", old, &rows).expect_err(old);
        assert!(why.contains("carries no row"), "{old}: {why}");
    }
}

/// **Every expiry the calendar has is a deribit option name**, with a one- or two-digit day, in every year a `YY`
/// spells (2000 to 2099). A real day is one the civil round trip keeps.
#[test]
fn deribit_admits_every_real_expiry() {
    use vike_utility::calendar::{civil_from_days, days_from_civil};
    const MONTHS: [&str; 12] =
        ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    for yy in 0..100_i64 {
        for (month, word) in (1_u32..).zip(MONTHS) {
            for day in 1..=31_u32 {
                let date = (2000 + yy, month, day);
                if civil_from_days(days_from_civil(date.0, month, day)) != date {
                    continue;
                }
                for expiry in [format!("{day}{word}{yy:02}"), format!("{day:02}{word}{yy:02}")] {
                    let name = format!("ETH_USDC-{expiry}-2500-C");
                    assert_eq!(door_symbol("deribit", &name), Some(Ok(name.clone())), "{name}");
                }
            }
        }
    }
}

/// **A day the calendar does not have is no deribit option name** (owner, 2026-11-13: refuse at once): a 31
/// February, a 29 February outside a leap year, a 31st in a 30-day month and a day 0 are refused at the door by the
/// option's shape. The day the venue would refuse never reaches it; every real day stays admitted
/// (`deribit_admits_every_real_expiry`).
#[test]
fn deribit_refuses_an_expiry_the_calendar_does_not_have() {
    use vike_utility::calendar::{civil_from_days, days_from_civil};
    const MONTHS: [&str; 12] =
        ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    let mut refused = 0;
    for yy in 0..100_i64 {
        for (month, word) in (1_u32..).zip(MONTHS) {
            for day in 0..=31_u32 {
                let date = (2000 + yy, month, day);
                if civil_from_days(days_from_civil(date.0, month, day)) == date {
                    continue;
                }
                for expiry in [format!("{day}{word}{yy:02}"), format!("{day:02}{word}{yy:02}")] {
                    let option = format!("ETH_USDC-{expiry}-2500-C");
                    let why = door_symbol("deribit", &option).expect("a rule").expect_err(&option);
                    assert!(
                        why.starts_with(&format!("`{option}` is not a deribit option name (")),
                        "{why}"
                    );
                    refused += 1;
                }
            }
        }
    }
    // Per year twelve day-0s and the 29-31 February, 31 April, June, September and November that do not exist.
    assert_eq!(refused, 2 * (100 * 12 + 100 * 7 - 25), "every impossible day was asked");
}

/// An engine mounted on anything but a perpetual or a dated future (an option) adds no instrument.
#[test]
fn a_deribit_engine_not_mounted_on_a_perpetual_adds_nothing() {
    let facts = Table::eth();
    let roster = [deribit_engine("ETH_USDC-27MAR26-2500-C", false)];
    let why = refused(plan_submit_symbol(
        &submit("deribit", "ETHUSDC.P", None),
        &roster,
        Some(&facts),
        true,
    ));
    assert!(why.contains("not a perpetual") && why.contains("ETH_USDC-27MAR26-2500-C"), "{why}");
}

/// **okx takes a user name and nothing else** (decisions 0232 and 0290). Three classes are okx's: a spot pair
/// `BTCUSDT`, a perpetual `BTCUSDT.P` and a dated future `BTCUSDT_261225`, in any case; what else a trader may type is
/// refused BY NAME at the door, before any lookup: an option by its shape, and okx's own instrument ids (its bridge's
/// wire, `ETH-USDT-SWAP`) or a name of no symbol's shape by the door's reading of a user name.
#[test]
fn okx_refuses_by_name_what_is_not_an_okx_user_name() {
    let facts = Table::eth();
    let on = |symbol: &str| {
        refused(plan_submit_symbol(
            &submit("okx", symbol, None),
            &[engine("okx", "BTCUSDT.P")],
            Some(&facts),
            true,
        ))
    };
    for (typed, word) in [
        ("BTC-USD-250926-60000-C", "is an option"),
        ("btc-usd-250926-60000-p", "is an option"),
        ("ETH-USDT-SWAP", "carries no row"),
        ("ETH-USDT", "carries no row"),
        ("ETH-USDT-261225", "carries no row"),
        ("ETH-USDT-SWAP-X", "carries no row"),
        ("-USDT-SWAP", "carries no row"),
        ("ETH_USDT-SWAP", "carries no row"),
        ("ETH-SWAP", "carries no row"),
        ("ETH-USDT-26122", "carries no row"),
        // six digits that are no date: the bridge signed these as a SWAP (owner ruling F-8)
        ("ETH-USDT-261399", "carries no row"),
        ("ETH-USDT-261200", "carries no row"),
        ("", "is not a symbol"),
    ] {
        let why = on(typed);
        assert!(why.contains(word), "{typed:?}: names {word:?}: {why}");
        assert!(why.contains("REFUSED"), "{typed:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a name the door refuses is never looked up");
    // The user name of each class, in any case, is admitted.
    for (typed, canonical) in [
        ("ETHUSDT.P", "ETHUSDT.P"),
        ("ethusdt.p", "ETHUSDT.P"),
        (" EthUsdt.P ", "ETHUSDT.P"),
        ("ETHUSDT", "ETHUSDT"),
        ("ethusdt", "ETHUSDT"),
        ("ETHUSDT_261225", "ETHUSDT_261225"),
        (" ethusdt_261225", "ETHUSDT_261225"),
    ] {
        let (symbol, admit) = placed(plan_submit_symbol(
            &submit("okx", typed, None),
            &[engine("okx", "BTCUSDT.P")],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, canonical, "{typed:?}");
        assert_eq!(admit.expect("admitted first").symbol, canonical);
    }
}

/// **okx's own instrument ids are its bridge's wire, no user name** (decision 0290): `ETH-USDT-SWAP`, a USD-margined
/// `HYPE-USD_UM-SWAP` and an option's id name no row at the door; an option is refused by its shape.
#[test]
fn okx_reads_no_instrument_id_at_the_door() {
    let rows = crate::testutil::door_rows();
    for old in ["ETH-USDT-SWAP", "HYPE-USD_UM-SWAP", "sol-usd_um-261127"] {
        let why = door_user_symbol("okx", old, &rows).expect_err(old);
        assert!(why.contains("carries no row"), "{old}: {why}");
    }
    let why = door_user_symbol("okx", "BTC-USD-261106-76000-C", &rows).expect_err("an option");
    assert!(why.contains("is an option"), "{why}");
}
/// Each way an `Admits` venue still says no, and the cause it names.
#[test]
fn every_refusal_names_its_cause() {
    let facts = Table::eth();
    let on = |venue: &str, mounted: &str, symbol: &str| {
        refused(plan_submit_symbol(
            &submit(venue, symbol, None),
            &[engine(venue, mounted)],
            Some(&facts),
            true,
        ))
    };
    // A name no kept row carries: the list's own words.
    let why = on("bybit", "BTCUSDT", "NOSUCHUSDT");
    assert!(
        why.contains("`NOSUCHUSDT` is no user name") && why.contains("carries no row"),
        "{why}"
    );
    // A listed symbol the venue's lookup does not answer: the lookup's own words.
    let why = on("bybit", "BTCUSDT", "SOLUSDT");
    assert!(why.contains("cannot admit `SOLUSDT`") && why.contains("no instrument"), "{why}");
    // A grid without a tick, or without a lot: named, never dressed as resolved.
    let why = on("bybit", "BTCUSDT", "XRPUSDT.P");
    assert!(why.contains("publishes no tick size for"), "{why}");
    let why = on("bybit", "BTCUSDT", "DOGEUSDT.P");
    assert!(why.contains("publishes no lot size for"), "{why}");
    // The other lane of a one-lane engine, both ways, naming the lane.
    let why = on("binance", "BTCUSDT", "ETHUSDT.P");
    assert!(why.contains("perp symbol") && why.contains("spot lane"), "{why}");
    let why = on("aster", "BTCUSDT.P", "ETHUSDT");
    assert!(why.contains("spot symbol") && why.contains("perp lane"), "{why}");
    // Not a symbol at all (it would reach a signed query string), or a name no kept row carries.
    for (bad, word) in [
        ("ETH USDT", "is not a symbol"),
        ("ETH&x=1", "is not a symbol"),
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("ETH/USDT", "carries no row"),
        (".P", "carries no row"),
        ("ETH-USDT", "carries no row"),
    ] {
        let why = on("bybit", "BTCUSDT", bad);
        assert!(why.contains(word), "{bad:?}: {why}");
    }
    // A paper engine is one book; a block that names no tier is never wider than the old gate.
    for (tier, word) in [(Some(AccountTier::Paper), "paper account"), (None, "does not say")] {
        let block = vike_execution::VenueBlock { tier, ..engine("bybit", "BTCUSDT") };
        let why = refused(plan_submit_symbol(
            &submit("bybit", "ETHUSDT", None),
            &[block],
            Some(&facts),
            true,
        ));
        assert!(why.contains(word) && why.contains("REFUSED"), "{tier:?}: {why}");
    }
    // A surface with no source cannot admit.
    let why = refused(plan_submit_symbol(
        &submit("bybit", "ETHUSDT", None),
        &[engine("bybit", "BTCUSDT")],
        None,
        true,
    ));
    assert!(why.contains("cannot look up a symbol's facts"), "{why}");
    // The 17th extra symbol.
    let full = vike_execution::VenueBlock {
        extra_symbols: (0..vike_execution::MAX_EXTRA_SYMBOLS)
            .map(|i| format!("S{i}USDT"))
            .collect(),
        ..engine("bybit", "BTCUSDT")
    };
    let why =
        refused(plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &[full], Some(&facts), true));
    assert!(why.contains("at most 16 symbols"), "{why}");
}

// ── the lane twin: ONE account row, TWO engines (binance spot, and its perpetual lane) ─────────────────
//
// A binance account whose row states a margin leverage runs a second engine, `binance#PERP`, over the
// same row and keys (`vike_secrets::AccountRows::with_lane_twins`). The door puts each order on the engine
// of its lane; the plan then admits the symbol into THAT engine exactly as it does for any account.

/// The account an order's address names, as the wire spells it.
fn account_of(cmd: &CommandWire) -> Option<&str> {
    match cmd {
        CommandWire::Submit(req) => req.account.as_deref(),
        other => panic!("not a submit: {other:?}"),
    }
}

/// The two engines of one account row: the default one on the spot lane, the twin on the perpetual one.
fn two_lanes() -> [vike_execution::VenueBlock; 2] {
    [engine("binance", "BTCUSDT"), engine("binance#PERP", "BTCUSDT.P")]
}

/// **A perpetual order of the default account goes to the lane twin, every other order to the default
/// engine, and the address is written out.** Written out because the core refuses an order that names no
/// account on a venue it runs two engines of: a client that has always left the default account unnamed
/// (the CLI, the Trade window) would otherwise have every binance order refused after its `Ack` the
/// moment a twin starts.
#[test]
fn a_perp_order_of_the_default_account_goes_to_the_lane_twin_and_the_rest_to_the_default_engine() {
    let roster = two_lanes();
    let to = |symbol: &str, account: Option<&str>| {
        account_of(&lane_addressed(submit("binance", symbol, account), &roster, None))
            .map(str::to_string)
    };
    for account in [None, Some("DEFAULT")] {
        assert_eq!(to("ETHUSDT.P", account).as_deref(), Some("PERP"), "{account:?}");
        assert_eq!(to("ethusdt.p", account).as_deref(), Some("PERP"), "{account:?}: lower case");
        // decision 0290: a dated USDⓈ-M contract carries no `.P` and is the futures lane's all the same
        assert_eq!(to("BTCUSDT_261225", account).as_deref(), Some("PERP"), "{account:?}: dated");
        assert_eq!(to("ETHUSDT", account).as_deref(), Some("DEFAULT"), "{account:?}");
    }
    // A twin MOUNTED on a dated contract is the twin too (`vike_secrets`' lane), and takes the lane's orders.
    let dated_twin = [engine("binance", "BTCUSDT"), engine("binance#PERP", "BTCUSDT_261225")];
    for symbol in ["ETHUSDT.P", "BTCUSDT_261225"] {
        let cmd = lane_addressed(submit("binance", symbol, None), &dated_twin, None);
        assert_eq!(account_of(&cmd), Some("PERP"), "{symbol}");
    }
    // Not the door's to move: another account's order, an address that already names the twin (a spot
    // symbol sent there is the PLAN's refusal), and another venue's order.
    assert_eq!(to("ETHUSDT.P", Some("ALT")).as_deref(), Some("ALT"));
    assert_eq!(to("ETHUSDT.P", Some("PERP")).as_deref(), Some("PERP"));
    assert_eq!(to("ETHUSDT", Some("PERP")).as_deref(), Some("PERP"));
    assert_eq!(account_of(&lane_addressed(submit("bybit", "ETHUSDT", None), &roster, None)), None);
    assert_eq!(
        account_of(&lane_addressed(submit("aster", "ETHUSDT.P", None), &roster, None)),
        None
    );
    // Only a Submit is addressed.
    let cancel = CommandWire::Cancel("c-1".into());
    assert_eq!(lane_addressed(cancel.clone(), &roster, None), cancel);

    // The plan then admits each symbol into the engine of ITS lane, spelled the venue's way.
    let facts = Table::eth();
    let lands = |symbol: &str| {
        let cmd = lane_addressed(submit("binance", symbol, None), &roster, None);
        let (symbol, admit) = placed(plan_submit_symbol(&cmd, &roster, Some(&facts), true));
        (symbol, admit.expect("admitted first").route_key)
    };
    assert_eq!(lands("ethusdt.p"), ("ETHUSDT.P".to_string(), "binance#PERP".to_string()));
    assert_eq!(lands("ethusdt"), ("ETHUSDT".to_string(), "binance".to_string()));
}

/// **A BRACKET is addressed as a plain order is**: a `.P` bracket of the default account goes to the lane
/// twin (the only engine of the venue that can hold its stop-loss), every other symbol's names the
/// default engine, and the core is never handed an account-less bracket on a venue it runs two engines of.
/// A bracket that names an account, a venue without a twin and a twin that is not up are left as they came.
#[test]
fn a_bracket_is_addressed_to_the_lane_twin_like_a_plain_order() {
    let bracket = |venue: &str, symbol: &str, account: Option<&str>| {
        CommandWire::Bracket(vike_tradehub_client::wire::OrderBracketWire {
            order_client_id: None,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 90.0,
            take_profit: 110.0,
            account: account.map(str::to_string),
        })
    };
    let account_of = |cmd: CommandWire| match cmd {
        CommandWire::Bracket(b) => b.account,
        other => panic!("not a bracket: {other:?}"),
    };
    let up = two_lanes();
    for account in [None, Some("DEFAULT")] {
        let to = |symbol: &str| {
            account_of(lane_addressed(bracket("binance", symbol, account), &up, None))
        };
        assert_eq!(to("BTCUSDT.P").as_deref(), Some("PERP"), "{account:?}");
        assert_eq!(to("btcusdt.p").as_deref(), Some("PERP"), "{account:?}: lower case");
        assert_eq!(to("BTCUSDT").as_deref(), Some("DEFAULT"), "{account:?}");
    }
    for named in ["ALT", "PERP"] {
        let cmd = lane_addressed(bracket("binance", "BTCUSDT.P", Some(named)), &up, None);
        assert_eq!(account_of(cmd).as_deref(), Some(named), "an account already named is left");
    }
    assert_eq!(account_of(lane_addressed(bracket("bybit", "BTCUSDT", None), &up, None)), None);
    let spot_only = [engine("binance", "BTCUSDT")];
    assert_eq!(
        account_of(lane_addressed(bracket("binance", "BTCUSDT.P", None), &spot_only, None)),
        None
    );
    let look_alike = [engine("binance", "BTCUSDT"), engine("binance#PERP", "ETHUSDT")];
    assert_eq!(
        account_of(lane_addressed(bracket("binance", "BTCUSDT.P", None), &look_alike, None)),
        None
    );
}

/// **F4 of the twin review: a strategy mount that names no account names the DEFAULT where a twin is up.**
/// The core refuses an account-less mount on a venue it runs two engines of, so without this the run-time
/// `MountStrategy` of every shipped strategy would be refused the moment a leverage is stated. A mount that
/// names an account, a venue without a twin and a look-alike engine are as they were.
#[test]
fn a_strategy_mount_that_names_no_account_names_the_default_where_a_lane_twin_is_up() {
    let mount = |venue: &str, account: Option<&str>| CommandWire::MountStrategy {
        venue: venue.to_string(),
        account: account.map(str::to_string),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        controller_id: None,
        file: Some("market_making/spread_maker.rs".to_string()),
        name: None,
        rhai: None,
        params: serde_json::json!({}),
    };
    let account_of = |cmd: CommandWire| match cmd {
        CommandWire::MountStrategy { account, .. } => account,
        other => panic!("not a mount: {other:?}"),
    };
    let up = two_lanes();
    assert_eq!(
        account_of(lane_addressed(mount("binance", None), &up, None)).as_deref(),
        Some("DEFAULT"),
        "the mount reaches the same spot engine it reached before the twin"
    );
    for named in ["DEFAULT", "ALT", "PERP"] {
        assert_eq!(
            account_of(lane_addressed(mount("binance", Some(named)), &up, None)).as_deref(),
            Some(named)
        );
    }
    let spot_only = [engine("binance", "BTCUSDT")];
    assert_eq!(account_of(lane_addressed(mount("binance", None), &spot_only, None)), None);
    assert_eq!(account_of(lane_addressed(mount("bybit", None), &up, None)), None);
    let look_alike = [engine("binance", "BTCUSDT"), engine("binance#PERP", "ETHUSDT")];
    assert_eq!(account_of(lane_addressed(mount("binance", None), &look_alike, None)), None);
}

/// **Without a twin nothing moves, and the refusal says how to get one.** A spot-only account (no stated
/// leverage, so no twin) sees its orders byte-identical to before, and a perpetual symbol is refused in
/// the door's words with the way out: the one existing setting that runs the lane.
#[test]
fn without_a_lane_twin_nothing_moves_and_the_refusal_says_how_to_get_one() {
    let roster = [engine("binance", "BTCUSDT")];
    for symbol in ["ETHUSDT", "ETHUSDT.P", "ethusdt.p"] {
        let cmd = lane_addressed(submit("binance", symbol, None), &roster, None);
        assert_eq!(account_of(&cmd), None, "{symbol}: no twin is up, so the order is as it came");
    }
    let facts = Table::eth();
    let cmd = submit("binance", "ETHUSDT.P", None);
    let why = refused(plan_submit_symbol(&cmd, &roster, Some(&facts), true));
    for want in ["perp symbol", "spot lane", "set-leverage", "binance#PERP", "no restart"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(facts.asked().is_empty(), "the lane is refused before any lookup spends a round trip");
    // A twin that is NOT UP (a block the table does not recognise) is no twin either.
    let roster = [engine("binance", "BTCUSDT"), engine("binance#PERP", "ETHUSDT")];
    assert_eq!(
        account_of(&lane_addressed(submit("binance", "SOLUSDT.P", None), &roster, None)),
        None
    );
}

/// **The twin's own symbol asks its price lane.** Nothing wires a boot feed for it (it is mounted on no
/// strategy), so it is treated like an admitted symbol that is already trading: asked once, an opening
/// order held for the first price, a Preview reading and subscribing nothing. Every other engine's own
/// symbol still passes untouched.
#[test]
fn the_lane_twins_own_symbol_asks_its_price_lane() {
    let roster = two_lanes();
    for typed in ["BTCUSDT.P", "btcusdt.p"] {
        let cmd = submit("binance", typed, Some("PERP"));
        let ready = Table::eth();
        match plan_submit_symbol(&cmd, &roster, Some(&ready), true) {
            SymbolPlan::Place { symbol, admit: None, wait: None } => {
                assert_eq!(symbol, "BTCUSDT.P", "{typed}");
            }
            other => panic!("{typed}: {other:?}"),
        }
        assert_eq!(ready.events(), ["price:BTCUSDT.P:subscribe"], "{typed}: the lane is asked");
        assert!(ready.asked().is_empty(), "{typed}: its grid is the mount's, no lookup");

        let pending = Table::eth().with_price(Price::Pending);
        match plan_submit_symbol(&cmd, &roster, Some(&pending), true) {
            SymbolPlan::Place { wait: Some(wait), .. } => {
                assert_eq!(
                    (wait.route_key.as_str(), wait.venue.as_str()),
                    ("binance#PERP", "binance")
                );
            }
            other => panic!("{typed}: an opening order waits for the first price: {other:?}"),
        }
        let peek = Table::eth().with_price(Price::Pending);
        assert!(matches!(
            plan_submit_symbol(&cmd, &roster, Some(&peek), false),
            SymbolPlan::Place { wait: None, admit: None, .. }
        ));
        assert!(peek.events().is_empty(), "{typed}: a Preview subscribes nothing");
    }
    let untouched = Table::eth();
    let own = submit("binance", "BTCUSDT", Some("DEFAULT"));
    assert_eq!(plan_submit_symbol(&own, &roster, Some(&untouched), true), SymbolPlan::Pass);
    assert!(untouched.events().is_empty(), "the default engine's own symbol is as before");
}

/// The same engine as a LIVE (mainnet) account's.
fn live(block: vike_execution::VenueBlock) -> vike_execution::VenueBlock {
    vike_execution::VenueBlock { tier: Some(AccountTier::Live), ..block }
}

/// **A MAINNET lane twin is addressed, priced and capped by the door as a demo one is** (the owner opened
/// binance's `live` tier for it, 2026-10-19). A perpetual of the default account goes to the twin and the
/// rest to the spot engine; the twin's own symbol asks its price lane and an opening order is held for the
/// first price; a NEW symbol admitted into it needs the account-wide exposure ceiling armed (a live
/// account's rule), is refused without one before any lookup, and with one is admitted into the twin's own
/// engine. The refusal that sends a spot-only account to the twin names the tiers the table opens.
#[test]
fn a_live_lane_twin_is_addressed_priced_and_capped_like_a_demo_one() {
    let roster = [live(engine("binance", "BTCUSDT")), live(engine("binance#PERP", "BTCUSDT.P"))];
    // A live engine is the twin only on the watcher's positive word (review r2, finding 7).
    let derived = Table::eth().deriving(&["binance#PERP"]);
    let to = |symbol: &str, account: Option<&str>| {
        account_of(&lane_addressed(submit("binance", symbol, account), &roster, Some(&derived)))
            .map(str::to_string)
    };
    for account in [None, Some("DEFAULT")] {
        assert_eq!(to("ETHUSDT.P", account).as_deref(), Some("PERP"), "{account:?}");
        assert_eq!(to("ETHUSDT", account).as_deref(), Some("DEFAULT"), "{account:?}");
    }

    // The twin's own symbol asks its price lane; an opening order waits for the first price.
    let cmd = submit("binance", "BTCUSDT.P", Some("PERP"));
    let ready = Table::eth();
    match plan_submit_symbol(&cmd, &roster, Some(&ready), true) {
        SymbolPlan::Place { symbol, admit: None, wait: None } => assert_eq!(symbol, "BTCUSDT.P"),
        other => panic!("{other:?}"),
    }
    assert_eq!(ready.events(), ["price:BTCUSDT.P:subscribe"], "the lane is asked");
    let pending = Table::eth().with_price(Price::Pending);
    match plan_submit_symbol(&cmd, &roster, Some(&pending), true) {
        SymbolPlan::Place { wait: Some(wait), .. } => {
            assert_eq!((wait.route_key.as_str(), wait.venue.as_str()), ("binance#PERP", "binance"));
        }
        other => panic!("an opening order waits for the first price: {other:?}"),
    }

    // A new symbol into the live twin: the account-wide ceiling first (refused before any lookup)…
    let new_symbol = lane_addressed(submit("binance", "ethusdt.p", None), &roster, Some(&derived));
    let uncapped = Table::eth().uncapped();
    let why = refused(plan_submit_symbol(&new_symbol, &roster, Some(&uncapped), true));
    for want in ["`binance#PERP`", "LIVE account", "account-wide exposure ceiling"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(uncapped.asked().is_empty(), "refused before any lookup spends a round trip");
    // …and with one armed, admitted into the twin's own engine.
    let capped = Table::eth();
    let (symbol, admit) = placed(plan_submit_symbol(&new_symbol, &roster, Some(&capped), true));
    assert_eq!(symbol, "ETHUSDT.P");
    assert_eq!(admit.expect("admitted first").route_key, "binance#PERP");

    // A live spot-only account is sent to the twin by words that name the tiers the table opens.
    let spot_only = [live(engine("binance", "BTCUSDT"))];
    let why = refused(plan_submit_symbol(
        &submit("binance", "ETHUSDT.P", None),
        &spot_only,
        Some(&Table::eth()),
        true,
    ));
    for want in ["perp symbol", "set-leverage", "binance#PERP", "demo or live accounts only"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
}

/// **A bracket's ENTRY obeys the same price rule as a plain opening order on the twin's own symbol**: the
/// price lane is asked once, and with no price yet the bracket WAITS for the first one (a market entry
/// judged with no mark would vacate the exposure caps, the very reason the plain order waits). A Preview
/// reads and subscribes nothing, and the default engine's own symbol is as it was.
#[test]
fn a_brackets_entry_on_the_lane_twins_own_symbol_waits_for_its_price_like_a_plain_order() {
    let roster = two_lanes();
    let bracket = |venue: &str, account: &str, entry_price: Option<f64>| {
        CommandWire::Bracket(vike_tradehub_client::wire::OrderBracketWire {
            order_client_id: None,
            venue: venue.to_string(),
            symbol: if account == "PERP" { "BTCUSDT.P" } else { "BTCUSDT" }.to_string(),
            side: 1,
            qty: 1.0,
            entry_price,
            stop_loss: 90.0,
            take_profit: 110.0,
            account: Some(account.to_string()),
        })
    };
    for entry_price in [Some(100.0), None] {
        let cmd = bracket("binance", "PERP", entry_price);
        let ready = Table::eth();
        assert!(
            matches!(
                plan_submit_symbol(&cmd, &roster, Some(&ready), true),
                SymbolPlan::Place { wait: None, .. }
            ),
            "{entry_price:?}: a price is here, so it is placed"
        );
        assert_eq!(
            ready.events(),
            ["price:BTCUSDT.P:subscribe"],
            "{entry_price:?}: the lane is asked"
        );

        let pending = Table::eth().with_price(Price::Pending);
        match plan_submit_symbol(&cmd, &roster, Some(&pending), true) {
            SymbolPlan::Place { wait: Some(wait), .. } => {
                assert_eq!(
                    (wait.route_key.as_str(), wait.venue.as_str()),
                    ("binance#PERP", "binance"),
                    "{entry_price:?}"
                );
            }
            other => panic!("{entry_price:?}: the entry waits for the first price: {other:?}"),
        }
        let peek = Table::eth().with_price(Price::Pending);
        assert!(matches!(
            plan_submit_symbol(&cmd, &roster, Some(&peek), false),
            SymbolPlan::Place { wait: None, admit: None, .. }
        ));
        assert!(peek.events().is_empty(), "{entry_price:?}: a Preview subscribes nothing");
    }
    let untouched = Table::eth();
    let own = bracket("binance", "DEFAULT", Some(100.0));
    assert_eq!(plan_submit_symbol(&own, &roster, Some(&untouched), true), SymbolPlan::Pass);
    assert!(untouched.events().is_empty(), "the default engine's own symbol is as before");
}

/// **An account an operator labelled `PERP` and mounted elsewhere is not the twin**: its own symbol still
/// passes untouched, and it is not handed the default account's perpetual orders.
#[test]
fn an_account_an_operator_labelled_perp_is_not_mistaken_for_the_lane_twin() {
    let roster = [engine("binance", "BTCUSDT"), engine("binance#PERP", "ETHUSDT")];
    let facts = Table::eth();
    let own = submit("binance", "ETHUSDT", Some("PERP"));
    assert_eq!(plan_submit_symbol(&own, &roster, Some(&facts), true), SymbolPlan::Pass);
    assert!(facts.events().is_empty());
}

/// A spot symbol sent to the twin is refused in words that send it to the account's own spot engine.
#[test]
fn a_spot_symbol_sent_to_the_lane_twin_is_sent_back_to_the_spot_engine() {
    let facts = Table::eth();
    let cmd = submit("binance", "ETHUSDT", Some("PERP"));
    let why = refused(plan_submit_symbol(&cmd, &two_lanes(), Some(&facts), true));
    for want in ["spot symbol", "perp lane", "own spot engine", "binance#PERP"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
}

/// An opening order on an admitted symbol needs a PRICE (the collar, the caps and the margin are
/// judged at the mark): a price already here is a plain placement; none yet is a placement that
/// WAITS for one (market orders included: once priced they are ordinary); a feed that cannot be
/// had is a refusal naming it; a reduce-only order is never held up; the mounted symbol is not
/// asked at all.
#[test]
fn an_opening_order_on_an_admitted_symbol_waits_for_its_price() {
    let roster = [engine("bybit", "BTCUSDT")];
    let plan =
        |facts: &Table, cmd: CommandWire| plan_submit_symbol(&cmd, &roster, Some(facts), true);

    let ready = Table::eth();
    assert!(matches!(
        plan(&ready, market("bybit", "ETHUSDT", false)),
        SymbolPlan::Place { wait: None, admit: Some(_), .. }
    ));

    let pending = Table::eth().with_price(Price::Pending);
    match plan(&pending, submit("bybit", "ETHUSDT", None)) {
        SymbolPlan::Place { wait: Some(w), .. } => {
            assert_eq!((w.route_key.as_str(), w.venue.as_str()), ("bybit", "bybit"));
        }
        other => panic!("expected a held placement, got {other:?}"),
    }
    let why = refused(plan(&pending, market("bybit", "ETHUSDT", true)));
    assert!(why.contains("reduce-only") && why.contains("holds none"), "{why}");

    let failing = Table::eth().with_price(Price::Failing);
    let why = refused(plan(&failing, submit("bybit", "ETHUSDT", None)));
    assert!(why.contains("no price feed for `ETHUSDT`") && why.contains("REFUSED"), "{why}");

    assert_eq!(
        plan(&failing, submit("bybit", "BTCUSDT", None)),
        SymbolPlan::Pass,
        "a priced order on the mounted symbol is not this gate's"
    );
}

/// The wait itself: it returns as soon as a price shows, and refuses with a cause when none does.
#[test]
fn the_door_holds_an_order_for_its_first_price_and_then_refuses() {
    use crate::server::symbol_admission::await_price;
    let wait = PriceWait { route_key: "bybit".into(), venue: "bybit".into() };
    await_price(&Table::eth(), &wait, "ETHUSDT").expect("a price is here");
    let started = Instant::now();
    let err = await_price(&Table::eth().with_price(Price::Pending), &wait, "ETHUSDT")
        .expect_err("no price ever arrives")
        .text;
    assert!(err.contains("no price on this node yet") && err.contains("REFUSED"), "{err}");
    assert!(started.elapsed() >= Duration::from_secs(3), "it waited: {:?}", started.elapsed());
    let err = await_price(&Table::eth().with_price(Price::Failing), &wait, "ETHUSDT")
        .expect_err("the feed is gone")
        .text;
    assert!(err.contains("no price feed"), "{err}");
}

/// **A refusal after the hold carries the lane's own verdict instead of "retry in a moment"** (A54): when the price
/// lane's venue stream is in a gap (IG's `REQERR` for the item, a refused login, a dropped socket) a retry cannot
/// help, and the old words said it could. The words stay byte for byte when the lane has nothing to add (no gap),
/// for a source that does not wait too, and the verdict is the same one a non-waiting source gives at once.
#[test]
fn a_refusal_after_the_hold_carries_the_lanes_verdict_not_retry() {
    use crate::server::symbol_admission::await_price;
    let wait = PriceWait { route_key: "ig".into(), venue: "ig".into() };

    let down = Table::eth().with_price(Price::Down);
    let started = Instant::now();
    let err =
        await_price(&down, &wait, "CS.D.GBPUSD.MINI.IP").expect_err("the stream is down").text;
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "the hold ran first: {:?}",
        started.elapsed()
    );
    assert!(
        err.contains("has no price on this node after 4 s: its price stream is DOWN (ig's stream is in a gap)"),
        "{err}"
    );
    assert!(err.contains("REFUSED") && err.contains("a retry cannot help"), "{err}");
    assert_eq!(
        err,
        "submit: `CS.D.GBPUSD.MINI.IP` has no price on this node after 4 s: its price stream \
         is DOWN (ig's stream is in a gap). The order was REFUSED: a retry cannot help until \
         that stream is back",
        "the lane's verdict, never the old \"retry in a moment\" words"
    );

    // A source that does not wait says it at once, without the hold's seconds.
    let mut instant = Table::eth().with_price(Price::Down);
    instant.waits = false;
    let err = await_price(&instant, &wait, "CS.D.GBPUSD.MINI.IP").expect_err("no hold").text;
    assert!(err.contains("has no price on this node: its price stream is DOWN"), "{err}");

    // No verdict: the old words, unchanged.
    let mut slow = Table::eth().with_price(Price::Pending);
    slow.waits = false;
    let err = await_price(&slow, &wait, "CS.D.GBPUSD.MINI.IP").expect_err("no quote yet").text;
    assert!(
        err.contains("no price on this node yet") && err.contains("retry in a moment"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------------
// End to end: accept_command -> command lane -> a REAL core -> a scripted venue client
// ---------------------------------------------------------------------------------------------

/// A venue double that records every order it is handed and answers each with accept + a full fill
/// at the request's price (the shape `vike_execution::testing::TestExecutionClient` has, but with its
/// record shared so the test can read it after the engine owns the client).
struct ScriptedVenue {
    venue: &'static str,
    submitted: Arc<Mutex<Vec<OrderRequest>>>,
    pending: VecDeque<Event>,
    n: u64,
}

impl ExecutionClient for ScriptedVenue {
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }
    fn submit(&mut self, request: &OrderRequest) {
        self.submitted.lock().unwrap().push(request.clone());
        self.n += 1;
        let px = request.price.unwrap_or(1.0);
        let fill = vike_model::events::FillEvent {
            // The fill carries the symbol the ORDER named, as the per-order bridges now do.
            trade_id: vike_model::events::TradeId::prefixed("scripted", self.n),
            order_client_id: request.order_client_id.clone(),
            venue: self.venue.into(),
            symbol: request.symbol.as_str().into(),
            side: request.side,
            last_qty: request.qty,
            last_price: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            timestamp: request.timestamp,
            mark_price: Some(px),
            position_side: vike_model::events::PositionSide::Both,
        };
        self.pending.push_back(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        }));
        self.pending.push_back(Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: request.order_client_id.clone(),
            venue_order_id: Some(format!("v{}", self.n).into()),
            timestamp: request.timestamp,
        }));
        self.pending.push_back(Event::Fill(fill.clone()));
        self.pending.push_back(Event::OrderFilled(vike_model::events::OrderFilled {
            order_client_id: request.order_client_id.clone(),
            fill,
            timestamp: request.timestamp,
        }));
    }
    fn cancel(&mut self, _order_client_id: &str) {}
    fn detach(&mut self) {}
}

pub(super) struct Node {
    pub(super) core: vike_core::CoreHandle,
    pub(super) submitted: Arc<Mutex<Vec<OrderRequest>>>,
    pub(super) limits: ControlLimits,
}

impl Node {
    /// A `demo` engine for `venue` mounted on `symbol`, on BTC's coarse grid (tick 0.1, lot 0.001).
    pub(super) fn start(venue: &'static str, symbol: &str, tier: AccountTier) -> Self {
        Self::start_with(venue, symbol, tier, &[])
    }

    /// [`Self::start`] plus one LABELLED account of the same venue per `labels` entry, each on its own
    /// scripted client (all recording into the one `submitted`), as `make_engine_accounts` builds them:
    /// the venue's canonical id on the `Account`, `venue#LABEL` on the route key.
    fn start_with(venue: &'static str, symbol: &str, tier: AccountTier, labels: &[&str]) -> Self {
        Self::start_valued(venue, symbol, tier, labels, false, None, None, None)
    }

    /// [`Self::start`] for an account the node has TOLD its currency (`Engine::set_account_currency`, which
    /// the assembly does for an account that converts a quote currency): the engine admits a symbol quoted in
    /// another currency only then (`account-currency-unset` otherwise).
    pub(super) fn start_anchored(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        currency: &'static str,
    ) -> Self {
        Self::start_valued(venue, symbol, tier, &[], false, None, Some(currency), None)
    }

    /// [`Self::start_anchored`] for a ledger in `currency` over a MOUNT quoted in another currency, `mount_currency` (a EUR
    /// account on `EURUSD`, quoted in `USD`): the assembly's declaration of it
    /// (`Engine::declare_mount_quote_currency`, decision 0183's mounted-symbol amendment), no rate yet.
    pub(super) fn start_foreign_mount(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        currency: &'static str,
        mount_currency: &'static str,
    ) -> Self {
        Self::start_valued(
            venue,
            symbol,
            tier,
            &[],
            false,
            None,
            Some(currency),
            Some(mount_currency),
        )
    }

    /// A deribit-style engine whose mounted symbol is INVERSE (an amount of US dollars), as the mount
    /// reports `BTC-PERPETUAL`'s contract value.
    fn start_inverse(venue: &'static str, symbol: &str) -> Self {
        Self::start_valued(venue, symbol, AccountTier::Demo, &[], true, None, None, None)
    }

    /// An engine armed with the buying-power gate at the engine-wide initial margin `initial_margin` (the mount's
    /// `1 / leverage`; `1.0` is its "no leverage unless asked" default), so a position's published
    /// leverage is the margin rate its symbol is held at.
    pub(super) fn start_margined(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        initial_margin: f64,
    ) -> Self {
        Self::start_valued(venue, symbol, tier, &[], false, Some(initial_margin), None, None)
    }

    fn start_valued(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        labels: &[&str],
        inverse: bool,
        initial_margin: Option<f64>,
        account_ccy: Option<&'static str>,
        mount_currency: Option<&'static str>,
    ) -> Self {
        let submitted: Arc<Mutex<Vec<OrderRequest>>> = Arc::default();
        let scripted = |route_key: String| {
            let client = ScriptedVenue {
                venue,
                submitted: Arc::clone(&submitted),
                pending: VecDeque::new(),
                n: 0,
            };
            let limits = RiskLimits {
                tick_size: Some(0.1),
                lot_size: Some(0.001),
                initial_margin_requirement: initial_margin,
                ..RiskLimits::new()
            };
            let mut engine = Engine::new(
                Account::new(1.0, venue, None, BalanceMode::Delta),
                RiskGate::new(limits),
                client,
                venue,
                symbol,
            );
            engine.tier = tier;
            engine.route_key = route_key;
            if let Some(currency) = account_ccy {
                engine.set_account_currency(currency).expect("a currency code");
            }
            if let Some(money) = mount_currency {
                engine.declare_mount_quote_currency(money).expect("the mount is declared");
            }
            if inverse {
                engine
                    .account
                    .admit_contract_value(symbol, vike_model::ContractValue::inverse(1.0));
            }
            // As the mount sets it (`assemble_engine`): the venues the door admits fail closed.
            engine.gate.limits.deny_unadmitted_symbols =
                crate::server::symbol_admission::admitting_venues().iter().any(|v| v == venue);
            engine
        };
        let engine = scripted(venue.to_string());
        let extras: Vec<_> =
            labels.iter().map(|l| (10_000.0, scripted(format!("{venue}#{l}")))).collect();
        let clock = Arc::new(AtomicI64::new(0));
        let core = vike_core::spawn_core_multi(
            engine,
            extras,
            vike_core::CoreConfig {
                seed_cash: 10_000.0,
                clock: Box::new(move || clock.fetch_add(1, Ordering::Relaxed)),
                ..vike_core::CoreConfig::default()
            },
        );
        let node = Node {
            core,
            submitted,
            limits: ControlLimits::new(ControlLimitsConfig {
                max_notional: None,
                rate_per_sec: 1e9,
            }),
        };
        node.publish(venue);
        node
    }

    /// Wait until the core has published the engine block (the roster the door judges from). A core
    /// with no feed publishes nothing until something happens to it, so a balance report (and a
    /// balance that clears every margin check below) is the something.
    fn publish(&self, venue: &str) {
        self.core
            .event_sender()
            .blocking_send(Event::AccountState(vike_model::events::AccountState {
                venue: venue.into(),
                balances: vec![("USDT".to_string(), 1_000_000.0)],
                timestamp: 1,
                route_key: None,
            }))
            .expect("the core is alive");
        let cell = self.core.snapshot_cell();
        wait_until("the engine to publish", || !cell.load().portfolio.venues.is_empty());
    }

    pub(super) fn blocks(&self) -> Vec<vike_execution::VenueBlock> {
        self.core.snapshot_cell().load().portfolio.venues.clone()
    }

    /// One command through the ONE acceptance path, as the TCP surface calls it.
    pub(super) fn send(
        &mut self,
        cmd: CommandWire,
        facts: Option<&dyn SymbolAdmitter>,
    ) -> Result<String, String> {
        let blocks = self.blocks();
        let sink = self.core.command_sink();
        accept_command(
            cmd,
            None,
            &mut self.limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            &blocks,
            &[],
            &[],
            &[],
            facts,
            None,
            None,
        )
        .map(super::control::ControlAcceptReply::into_order_client_id)
        .map_err(|e| e.message())
    }

    /// [`Self::send`] with the published engines and route keys as the roster the TCP door judges from. The
    /// plain `send` hands the gates an EMPTY roster, which every verb but a BRACKET reads as "not published
    /// yet" and lets through; a bracket is refused on an empty roster, so a test of one needs this.
    pub(super) fn send_rostered(
        &mut self,
        cmd: CommandWire,
        facts: Option<&dyn SymbolAdmitter>,
    ) -> Result<String, String> {
        self.send_rostered_reading(cmd, facts, &[])
    }

    /// [`Self::send_rostered`] with the published orders the command names, as the TCP session hands them to the door
    /// (`PublisherHandle::orders_named_by`, through `crate::publish::named_orders`). The plain form hands the gates
    /// NONE, which hid that the give-back of a symbol was never given the orders it judges.
    pub(super) fn send_rostered_published(
        &mut self,
        cmd: CommandWire,
        facts: Option<&dyn SymbolAdmitter>,
    ) -> Result<String, String> {
        let orders = crate::publish::named_orders(&cmd, &self.core.snapshot_cell().load().orders);
        self.send_rostered_reading(cmd, facts, &orders)
    }

    fn send_rostered_reading(
        &mut self,
        cmd: CommandWire,
        facts: Option<&dyn SymbolAdmitter>,
        orders: &[vike_execution::OrderView],
    ) -> Result<String, String> {
        let blocks = self.blocks();
        let route_keys: Vec<String> = blocks.iter().map(|b| b.route_key.clone()).collect();
        let mut engines: Vec<String> = blocks.iter().map(|b| b.venue.clone()).collect();
        engines.dedup();
        let sink = self.core.command_sink();
        accept_command(
            cmd,
            None,
            &mut self.limits,
            &sink,
            None,
            &engines,
            &route_keys,
            &[],
            &blocks,
            orders,
            &[],
            &[],
            facts,
            None,
            None,
        )
        .map(super::control::ControlAcceptReply::into_order_client_id)
        .map_err(|e| e.message())
    }

    /// The published roster once `route_key`'s block trades `symbol`. The venue client saw the order
    /// ON the core thread, which publishes AFTER the fold (coalesced), so `venue_saw` returning says
    /// nothing about the snapshot: wait for the admission itself, then judge the roster.
    fn blocks_once_trading(
        &self,
        route_key: &str,
        symbol: &str,
    ) -> Vec<vike_execution::VenueBlock> {
        wait_until(&format!("{route_key} to publish {symbol} as traded"), || {
            self.blocks().iter().any(|b| b.route_key == route_key && b.trades(symbol))
        });
        self.blocks()
    }

    pub(super) fn venue_saw(&self, order_client_id: &str) -> OrderRequest {
        let found = || {
            self.submitted
                .lock()
                .unwrap()
                .iter()
                .find(|o| o.order_client_id == order_client_id)
                .cloned()
        };
        wait_until("the order to reach the venue client", || found().is_some());
        found().unwrap()
    }
}

/// The node's production sink chain for a venue's price feed, over `node`'s core: the core's three lanes
/// behind the price tap, as the feeds wire them (`ibkr_delayed_price_door`, `lane_twin_pricing`).
pub(super) fn production_sink(
    node: &Node,
    tap: &Arc<crate::feeds::price_tap::PriceTap>,
) -> crate::feeds::price_tap::DataClientSinkPriceTap {
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        node.core.bar_sender(),
        node.core.market_sender(),
        node.core.tick_sender(),
    )) as Arc<dyn vike_data::DataClientSink>;
    crate::feeds::price_tap::DataClientSinkPriceTap::new(core, Arc::clone(tap))
}

/// Wait up to 10 s for `cond` (the money-door suites' bound); the failure names `what`.
pub(super) fn wait_until(what: &str, cond: impl FnMut() -> bool) {
    vike_log::poll::wait_for(Duration::from_secs(10), what, cond);
}

pub(super) fn limit(
    order_client_id: &str,
    venue: &str,
    symbol: &str,
    qty: f64,
    price: f64,
) -> CommandWire {
    let CommandWire::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.order_client_id = order_client_id.into();
    req.qty = qty;
    req.price = Some(price);
    CommandWire::Submit(req)
}

/// [`limit`] turned into the reduce-only SELL that closes a long (the money doors' closing leg).
pub(super) fn sell_to_close(
    order_client_id: &str,
    venue: &str,
    symbol: &str,
    qty: f64,
    price: f64,
) -> CommandWire {
    let CommandWire::Submit(mut req) = limit(order_client_id, venue, symbol, qty, price) else {
        unreachable!()
    };
    req.side = -1;
    req.reduce_only = true;
    CommandWire::Submit(req)
}

// ── the lane twin's first order, end to end ─────────────────────────────────────────────────────────
//
// v0.1.78 on the CI box (drill `n078-B1`): the first `.P` order after the twin's hot start printed "sent, but the
// node has not answered within 2s" and left no row, no denial and no audit line. The pump that was to price
// the twin's own symbol was binance's SPOT one, which never delivered a tick for `BTCUSDT.P`
// (`vike_binance::feed::tick`'s `pump_wire`), so the door's hold for the first price could only run out, and
// it ran out after the CLI had stopped listening. These pin the door's half: the hold is bounded and ends in
// a sentence, and the first tick releases the order.

/// The twin's price lane as the node sees it: nothing until the pump's first tick, then that price. Every
/// other answer is the table's. `waits` is whether the door holds an order for the first price (`false` skips the
/// hold).
struct FirstTick {
    table: Table,
    price: Mutex<Option<f64>>,
}

impl FirstTick {
    fn new(waits: bool) -> Self {
        FirstTick { table: Table { waits, ..Table::eth() }, price: Mutex::new(None) }
    }

    /// The pump's first frame lands: the tap now holds a price.
    fn tick(&self, px: f64) {
        *self.price.lock().unwrap() = Some(px);
    }
}

impl SymbolAdmitter for FirstTick {
    fn price_source(&self, venue: &str) -> Result<(), String> {
        self.table.price_source(venue)
    }

    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        // The twin's own symbol: the grid the gatewayhub's look-up answers on its lease, which the door
        // hands the engine as an `AdmitSymbol` that the engine ignores for its mounted symbol.
        if (venue, symbol) == ("binance", "BTCUSDT.P") {
            return Ok(SymbolAdmission {
                route_key: route_key.into(),
                symbol: symbol.into(),
                grid: SymbolGrid {
                    tick_size: Some(0.1),
                    lot_size: Some(0.001),
                    ..SymbolGrid::default()
                },
                contract_value: None,
                initial_mark: None,
                initial_mark_ms: None,
                quote_currency: None,
                quote_rate: None,
                rate_as_of_ms: None,
                initial_margin_requirement: None,
                release: false,
                margin_mode: None,
            });
        }
        self.table.admission(route_key, venue, symbol)
    }

    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(*self.price.lock().unwrap())
    }

    fn waits(&self) -> bool {
        self.table.waits
    }
}

impl AccountReadiness for FirstTick {
    fn generation(&self, route_key: &str) -> u64 {
        self.table.generation(route_key)
    }

    fn exposure_cap_armed(&self, route_key: &str) -> bool {
        self.table.exposure_cap_armed(route_key)
    }
}

/// The ceilings a lane-twin test arms on BOTH engines of the account.
#[derive(Clone, Copy, Default)]
pub(super) struct TwinCapabilities {
    pub(super) max_total_exposure: Option<f64>,
    pub(super) max_notional_per_order: Option<f64>,
    pub(super) max_account_exposure: Option<f64>,
}

impl Node {
    /// binance's ONE account running both lanes, as a hot start of the twin leaves it: the default engine on
    /// the spot `BTCUSDT` and the lane twin `binance#PERP` on `BTCUSDT.P`, each on its own scripted client
    /// (all recording into the one `submitted`).
    ///
    /// ⚠ Every `start_lane_twin*` builds its engines by hand with `RiskGate::new`: UNARMED, with no mark floor,
    /// which is NOT the twin production builds. `lane_twin_pricing`'s `assembled::start` is.
    fn start_lane_twin() -> Self {
        Self::start_lane_twin_capped(None)
    }

    /// [`Self::start_lane_twin`] with a per-symbol exposure ceiling on both engines: an order the ceiling
    /// would refuse AT A MARK passes at notional zero where an engine holds none, which is how a test sees
    /// whether the engine is priced.
    pub(super) fn start_lane_twin_capped(max_total_exposure: Option<f64>) -> Self {
        Self::start_lane_twin_tiered(
            AccountTier::Demo,
            TwinCapabilities { max_total_exposure, ..TwinCapabilities::default() },
        )
    }

    /// [`Self::start_lane_twin_capped`] with BOTH engines at `tier` and the three ceilings an operator arms for a
    /// real account (`capabilities`): what a MAINNET twin runs under (`assemble_engine` refuses a live engine without
    /// the first two, and `policy.max_account_exposure` is the third).
    pub(super) fn start_lane_twin_tiered(
        tier: AccountTier,
        capabilities: TwinCapabilities,
    ) -> Self {
        Self::start_lane_twin_with(tier, capabilities, false)
    }

    /// [`Self::start_lane_twin_tiered`] whose engines went through the boot restore (`restore_runtime_state` of
    /// the snapshot a previous run journaled): the folded money comes back, the MARKS never do (market data is
    /// not journaled), so until the first tick a restored engine holds no price for its own symbol.
    pub(super) fn start_lane_twin_restored(
        tier: AccountTier,
        capabilities: TwinCapabilities,
    ) -> Self {
        Self::start_lane_twin_with(tier, capabilities, true)
    }

    fn start_lane_twin_with(
        tier: AccountTier,
        capabilities: TwinCapabilities,
        restored: bool,
    ) -> Self {
        Self::start_binance_pair(tier, capabilities, restored, "binance#PERP", "BTCUSDT.P")
    }

    /// The default engine on the spot `BTCUSDT` and ONE other engine of the same exchange, `labelled_key` on
    /// `labelled_symbol` (the lane twin above, or any labelled account), each on its own scripted client.
    fn start_binance_pair(
        tier: AccountTier,
        capabilities: TwinCapabilities,
        restored: bool,
        labelled_key: &str,
        labelled_symbol: &str,
    ) -> Self {
        let submitted: Arc<Mutex<Vec<OrderRequest>>> = Arc::default();
        let scripted = |route_key: &str, symbol: &str| {
            let client = ScriptedVenue {
                venue: "binance",
                submitted: Arc::clone(&submitted),
                pending: VecDeque::new(),
                n: 0,
            };
            let limits = RiskLimits {
                tick_size: Some(0.1),
                lot_size: Some(0.001),
                max_total_exposure: capabilities.max_total_exposure,
                max_notional_per_order: capabilities.max_notional_per_order,
                max_account_exposure: capabilities.max_account_exposure,
                ..RiskLimits::new()
            };
            let mut engine = Engine::new(
                Account::new(1.0, "binance", None, BalanceMode::Delta),
                RiskGate::new(limits),
                client,
                "binance",
                symbol,
            );
            engine.tier = tier;
            engine.route_key = route_key.to_string();
            engine.gate.limits.deny_unadmitted_symbols = true;
            if restored {
                let journaled = engine.snapshot_state();
                engine.restore_runtime_state(&journaled);
            }
            engine
        };
        let clock = Arc::new(AtomicI64::new(0));
        let core = vike_core::spawn_core_multi(
            scripted("binance", "BTCUSDT"),
            vec![(10_000.0, scripted(labelled_key, labelled_symbol))],
            vike_core::CoreConfig {
                seed_cash: 10_000.0,
                clock: Box::new(move || clock.fetch_add(1, Ordering::Relaxed)),
                ..vike_core::CoreConfig::default()
            },
        );
        let node = Node {
            core,
            submitted,
            limits: ControlLimits::new(ControlLimitsConfig {
                max_notional: None,
                rate_per_sec: 1e9,
            }),
        };
        node.publish("binance");
        wait_until("both lanes to publish", || node.blocks().len() == 2);
        node
    }

    /// The pump's first frame for `symbol`, into the core's tick lane (what `DataClientSinkPriceTap` forwards, BEFORE it
    /// records the price the door reads).
    pub(super) fn tick(&self, symbol: &str, bid: f64, ask: f64) {
        self.core
            .tick_sender()
            .quote(vike_execution::QuoteUpdate {
                venue: "binance".into(),
                symbol: symbol.into(),
                quote: vike_marketdata::TickQuote {
                    timestamp: 1,
                    local_timestamp: 0,
                    bid,
                    ask,
                    bid_size: 1.0,
                    ask_size: 1.0,
                    symbol: Default::default(),
                },
            })
            .expect("the core is alive");
    }
}

/// **A source that does not wait (`SymbolAdmitter::waits`) is answered at once**, in the door's words, for a
/// twin whose pump has not ticked; nothing reaches the venue; and the order is taken the moment the first
/// tick is in. The sentence is the one a held order ends in.
#[test]
fn a_lane_twin_order_with_no_price_yet_is_refused_at_once_and_taken_after_the_first_tick() {
    let facts = FirstTick::new(false);
    let mut node = Node::start_lane_twin();
    let order = || limit("twin-1", "binance", "BTCUSDT.P", 0.002, 74_288.4);

    let started = Instant::now();
    let why = node.send(order(), Some(&facts)).expect_err("no price yet: the hold refuses");
    assert!(started.elapsed() < Duration::from_secs(2), "no wait: {:?}", started.elapsed());
    for want in ["`BTCUSDT.P`", "no price on this node yet", "REFUSED", "retry in a moment"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty(), "a refusal sends the venue nothing");

    // The first frame: into the core's tick lane FIRST, then into the tap, as `DataClientSinkPriceTap` orders them.
    node.tick("BTCUSDT.P", 74_299.9, 74_300.1);
    facts.tick(74_300.0);
    let order_client_id =
        node.send(order(), Some(&facts)).expect("the first tick released the order");
    assert_eq!(order_client_id, "twin-1");
    let seen = node.venue_saw("twin-1");
    assert_eq!(seen.symbol, "BTCUSDT.P");
    assert_eq!(
        seen.account.as_ref().map(ToString::to_string).as_deref(),
        Some("PERP"),
        "the order is the twin's, not the spot engine's"
    );
    node.core.shutdown_and_join();
}

/// **The TCP door holds an opening order for the first price, but only for a bound, and ends in a sentence**
/// (the drill's order was held past the CLI's 2 s, so the CLI printed "NOT yet known" while the door was
/// still counting). A tick that lands mid-hold releases the order at once, well inside the bound.
///
/// ⚠ Takes the whole `PRICE_WAIT` once, like `the_door_holds_an_order_for_its_first_price_and_then_refuses`.
#[test]
fn a_lane_twin_order_is_held_for_a_bounded_time_and_a_tick_mid_hold_releases_it() {
    use crate::server::symbol_admission::PRICE_WAIT;
    let facts = FirstTick::new(true);
    let mut node = Node::start_lane_twin();

    let started = Instant::now();
    let (sent, lines) = vike_log::capture::captured(|| {
        node.send(limit("twin-2", "binance", "BTCUSDT.P", 0.002, 74_288.4), Some(&facts))
    });
    let why = sent.expect_err("the pump never ticked: the hold runs out");
    let held = started.elapsed();
    assert!(held >= PRICE_WAIT - Duration::from_secs(1), "it held for the first price: {held:?}");
    assert!(held < PRICE_WAIT + Duration::from_secs(3), "and it is bounded: {held:?}");
    for want in ["`BTCUSDT.P`", "nothing has arrived in 4 s", "REFUSED", "retry in a moment"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty(), "a refusal sends the venue nothing");
    // The v0.1.81 IG drill: the CLI's 2 s wait ended inside this 4 s hold, so the sentence above went to a
    // socket nobody read, and a refusal is audited nowhere: the order left no row, no audit line, no log
    // line. The node's own log is the one place a hung-up client's refusal can still be read.
    let warned = lines
        .iter()
        .find(|e| e.level == tracing::Level::WARN && e.field("order_client_id") == Some("twin-2"))
        .unwrap_or_else(|| {
            panic!("a held-then-refused order leaves a WARN naming its order client id: {lines:?}")
        });
    assert_eq!(warned.field("symbol"), Some("BTCUSDT.P"));
    assert_eq!(warned.field("venue"), Some("binance"));
    let held_ms: u64 = warned.field("held_ms").and_then(|ms| ms.parse().ok()).expect("held_ms");
    assert!(held_ms >= 3_000, "the line says how long it held: {warned:?}");
    assert!(warned.message.contains("REFUSED") && warned.message.contains(&why), "{warned:?}");

    node.tick("BTCUSDT.P", 74_299.9, 74_300.1);
    let started = Instant::now();
    let order_client_id = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            facts.tick(74_300.0);
        });
        node.send(limit("twin-3", "binance", "BTCUSDT.P", 0.002, 74_288.4), Some(&facts))
    })
    .expect("a tick mid-hold releases the order");
    assert_eq!(order_client_id, "twin-3");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "released by the tick: {:?}",
        started.elapsed()
    );
    assert_eq!(node.venue_saw("twin-3").symbol, "BTCUSDT.P");
    node.core.shutdown_and_join();
}

/// **The client's default ack wait outlasts the door's price hold, so a refusal after the hold is read.**
/// The two sides sit in crates that cannot see each other (`vike-cli` waits `config.node_ack_wait_ms`,
/// `vike-config`'s `DEFAULT_NODE_ACK_WAIT_MS`; the door holds `PRICE_WAIT`), and this crate sees both. The
/// v0.1.81 IG drill had 2 s against 4 s: the CLI hung up inside the hold and never read the refusal. At least
/// a second over the hold is the least a tunnelled round trip and the pre-hold facts look-up can need.
#[test]
fn the_default_ack_wait_outlasts_the_doors_price_hold() {
    use crate::server::symbol_admission::PRICE_WAIT;
    let ack = Duration::from_millis(u64::from(vike_config::config::DEFAULT_NODE_ACK_WAIT_MS));
    assert!(
        ack >= PRICE_WAIT + Duration::from_secs(1),
        "a client waiting {ack:?} hangs up before the door's {PRICE_WAIT:?} hold ends and never reads its refusal"
    );
}

/// **The whole path**, on each finished venue: an order for a symbol the account was not started on
/// is Acked, reaches the venue client on THAT symbol (the venue's spelling), snapped on THAT
/// symbol's grid (not the mounted symbol's coarse 0.1/0.001), the published block says the account
/// trades it now, and the venue's fill lands in THAT symbol's position, never the mounted one's.
#[test]
fn a_foreign_symbol_is_placed_on_its_own_grid_and_folds_into_its_own_position() {
    for (venue, mounted, typed, canonical, tick, lot) in [
        ("bybit", "BTCUSDT", "ethusdt", "ETHUSDT", 0.01, 0.01),
        ("binance", "BTCUSDT", "ethusdt", "ETHUSDT", 0.01, 0.0001),
        ("aster", "BTCUSDT.P", "ETHUSDT.P", "ETHUSDT.P", 0.01, 0.001),
        ("okx", "BTCUSDT.P", "ethusdt.p", "ETHUSDT.P", 0.01, 0.001),
        ("hyperliquid", "BTCUSDC.P", "kPEPEUSDC.P", "KPEPEUSDC.P", 0.001, 0.01),
        ("alpaca", "AAPL", "btcusd", "BTCUSD", 1.0, 0.000000001),
    ] {
        let facts = Table::eth();
        let mut node = Node::start(venue, mounted, AccountTier::Demo);
        let order_client_id = format!("e2e-{venue}");
        // 3000.456 @ 0.12346: off both grids, so the snap is visible.
        let acked = node
            .send(limit(&order_client_id, venue, typed, 0.12346, 3000.456), Some(&facts))
            .unwrap_or_else(|e| panic!("{venue}: the order must be accepted, got: {e}"));
        assert_eq!(acked, order_client_id);

        let seen = node.venue_saw(&order_client_id);
        assert_eq!(seen.symbol, canonical, "{venue}: signed on the symbol the operator named");
        let snapped_price = (3000.456_f64 / tick).round() * tick;
        let snapped_quantity = (0.12346_f64 / lot).round() * lot;
        assert!(
            (seen.price.unwrap() - snapped_price).abs() < 1e-6,
            "{venue}: tick {tick}: {seen:?}"
        );
        assert!((seen.qty - snapped_quantity).abs() < 1e-9, "{venue}: lot {lot}: {seen:?}");

        let cell = node.core.snapshot_cell();
        wait_until("the foreign position", || {
            cell.load().positions.iter().any(|p| p.symbol == canonical)
        });
        let snap = cell.load_full();
        assert!(snap.portfolio.venues[0].trades(canonical), "{venue}: published as tradable");
        let sizes: Vec<(&str, bool)> =
            snap.positions.iter().map(|p| (p.symbol.as_str(), p.size > 0.0)).collect();
        assert_eq!(sizes, vec![(canonical, true)], "{venue}: the fill is ETH's, not BTC's");
        node.core.shutdown_and_join();
    }
}

/// **oanda, end to end**: a pair the account was not started on (typed in any case) is
/// admitted, reaches the venue client as its user name `GBPUSD` snapped on the pair's grid
/// (five decimals, whole units: the mounted symbol's 0.1 / 0.001 grid is not used), its fill folds into
/// its own position, and the engine FAILS CLOSED for oanda now (an order on a symbol it was never told is
/// denied, where it used to be signed on the mounted grid).
#[test]
fn oanda_places_a_foreign_pair_on_its_own_grid_and_folds_into_its_own_position() {
    let facts = Table::eth();
    let mut node = Node::start("oanda", "EURUSD", AccountTier::Demo);
    // 100.4 units @ 3000.4567891: off both grids, so the snap is visible (the table's price is the mark).
    let acked = node
        .send(limit("e2e-oanda", "oanda", "gbpusd", 100.4, 3_000.456_789_1), Some(&facts))
        .unwrap_or_else(|e| panic!("oanda: the order must be accepted, got: {e}"));
    assert_eq!(acked, "e2e-oanda");

    let seen = node.venue_saw("e2e-oanda");
    assert_eq!(seen.symbol, "GBPUSD", "signed on the engine's own spelling of the pair");
    assert!((seen.price.unwrap() - 3000.45679).abs() < 1e-6, "five decimals: {seen:?}");
    assert!((seen.qty - 100.0).abs() < 1e-9, "whole units: {seen:?}");

    let cell = node.core.snapshot_cell();
    wait_until("the foreign position", || {
        cell.load().positions.iter().any(|p| p.symbol == "GBPUSD")
    });
    let snap = cell.load_full();
    assert!(snap.portfolio.venues[0].trades("GBPUSD"), "published as tradable");
    assert!(snap.portfolio.venues[0].admits_symbols, "advertised: the desktop may send a symbol");
    let sizes: Vec<(&str, bool)> =
        snap.positions.iter().map(|p| (p.symbol.as_str(), p.size > 0.0)).collect();
    assert_eq!(sizes, vec![("GBPUSD", true)], "the fill is GBPUSD's, not EURUSD's");
    node.core.shutdown_and_join();
}

/// A second order on the now-admitted symbol needs no lookup: the plan passes it straight through.
#[test]
fn a_second_order_on_an_admitted_symbol_asks_for_nothing() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.send(limit("one", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("first");
    let cell = node.core.snapshot_cell();
    wait_until("the admission to publish", || {
        cell.load().portfolio.venues.first().is_some_and(|b| b.trades("ETHUSDT"))
    });
    node.send(limit("two", "bybit", "ETHUSDT", 1.0, 3001.0), Some(&facts)).expect("second");
    node.venue_saw("two");
    assert_eq!(facts.asked().len(), 1, "one lookup for two orders: {:?}", facts.asked());
    node.core.shutdown_and_join();
}

/// **The policy ceiling sizes a foreign symbol with ITS OWN contract value.** The door judges the
/// ceiling before the symbol is admitted, when the engine's grid has no row for it: an INVERSE contract
/// (an amount of US dollars) used to read as `qty · price` there, so 10 dollars of `BTCUSD_260327` at 4000
/// was "40000", refused by a ceiling of 100, and the symbol was never admitted. Refused when over, accepted
/// when the real notional is under the ceiling; a LINEAR one is `qty · price` as ever; and the Preview agrees
/// with the send. (The inverse instrument is the engine's own coin's: another coin's is refused first.)
#[test]
fn the_ceiling_sizes_a_foreign_symbol_with_its_own_contract_value() {
    use crate::server::symbol_admission::foreign_contract_value;
    let ceiling =
        || ControlLimits::new(ControlLimitsConfig { max_notional: Some(100.0), rate_per_sec: 1e9 });

    // INVERSE: an amount of dollars, the price never enters.
    let facts = Table::eth();
    let mut node = Node::start_inverse("deribit", "BTCUSD.P");
    node.limits = ceiling();
    let over = node
        .send(limit("inv-over", "deribit", "BTCUSD_260327", 150.0, 150.0), Some(&facts))
        .expect_err("150 dollars is over a ceiling of 100");
    assert!(over.contains("order notional 150.00 exceeds"), "{over}");
    assert!(
        !facts.events().iter().any(|e| e == "admitted:BTCUSD_260327"),
        "a refused order admits nothing: {:?}",
        facts.events()
    );
    let blocks = node.blocks();
    let inverse_under = limit("inv-ok", "deribit", "BTCUSD_260327", 10.0, 4000.0);
    assert_eq!(
        foreign_contract_value(&inverse_under, &blocks, Some(&facts)),
        Ok(Some(vike_model::ContractValue::inverse(1.0)))
    );
    assert!(
        node.limits.preview_vet(&inverse_under, &blocks, &[]).is_some(),
        "the plain preview, which knows no facts, still sizes it as linear"
    );
    assert_eq!(
        node.limits.preview_vet_foreign(
            &inverse_under,
            &blocks,
            &[],
            foreign_contract_value(&inverse_under, &blocks, Some(&facts)).unwrap()
        ),
        None,
        "the preview with the facts' value agrees with the send"
    );
    node.send(inverse_under, Some(&facts)).expect("10 dollars is under a ceiling of 100");
    node.venue_saw("inv-ok");
    node.core.shutdown_and_join();

    // LINEAR: `qty · price`.
    let facts = Table::eth();
    let mut node = Node::start("deribit", "BTCUSDC.P", AccountTier::Demo);
    node.limits = ceiling();
    node.send(limit("lin-ok", "deribit", "ETHUSDC.P", 0.01, 4000.0), Some(&facts))
        .expect("40 is under a ceiling of 100");
    node.venue_saw("lin-ok");
    let over = node
        .send(limit("lin-over", "deribit", "ETHUSDC.P", 0.05, 4000.0), Some(&facts))
        .expect_err("200 is over a ceiling of 100");
    assert!(over.contains("order notional 200.00 exceeds"), "{over}");
    node.core.shutdown_and_join();
}

/// **The engine's OWN symbol typed in another spelling is sized from the engine's grid row** (A54): `btc-perpetual`
/// is the mounted `BTC-PERPETUAL`, an INVERSE contract (an amount of US dollars, the price never enters). The
/// plan places it on the engine's spelling, and the ceiling used to look the TYPED name up in the grid, find no
/// row and size 10 dollars at 4000 as `qty · price`: 40 000, refused by a ceiling of 100 it is far under.
#[test]
fn the_engines_own_symbol_in_another_spelling_is_sized_from_its_grid_row() {
    use crate::server::symbol_admission::foreign_contract_value;
    let facts = Table::eth();
    let mut node = Node::start_inverse("deribit", "BTC-PERPETUAL");
    node.limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(100.0), rate_per_sec: 1e9 });
    let blocks = node.blocks();
    let lower = limit("own-lower", "deribit", "btc-perpetual", 10.0, 4000.0);
    assert_eq!(
        foreign_contract_value(&lower, &blocks, Some(&facts)),
        Ok(Some(vike_model::ContractValue::inverse(1.0))),
        "the grid's row under the engine's own name, not the default"
    );
    node.send(lower, Some(&facts)).expect("10 dollars is under a ceiling of 100");
    assert_eq!(
        node.venue_saw("own-lower").symbol,
        "BTC-PERPETUAL",
        "placed on the engine's spelling"
    );
    assert!(
        facts.asked().is_empty(),
        "the engine's own symbol asks the facts nothing: {:?}",
        facts.asked()
    );
    let over = node
        .send(limit("own-over", "deribit", "btc-perpetual", 150.0, 4000.0), Some(&facts))
        .expect_err("150 dollars is over a ceiling of 100");
    assert!(over.contains("order notional 150.00 exceeds"), "{over}");
    node.core.shutdown_and_join();
}

/// **Refusals leave the node as it was**: an unknown symbol is an error naming the cause, nothing
/// reaches the venue, and the engine admitted nothing. A mounted-symbol order is unchanged.
#[test]
fn an_unknown_symbol_is_refused_with_its_cause_and_reaches_no_venue() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let err = node
        .send(limit("nope", "bybit", "NOSUCHUSDT", 1.0, 5.0), Some(&facts))
        .expect_err("an unlisted symbol is refused");
    assert!(
        err.contains("NOSUCHUSDT") && err.contains("no user name of a bybit instrument"),
        "{err}"
    );
    node.send(limit("own", "bybit", "BTCUSDT", 1.0, 60_000.04), Some(&facts)).expect("mounted");
    let own = node.venue_saw("own");
    assert_eq!(own.symbol, "BTCUSDT");
    assert!((own.price.unwrap() - 60_000.0).abs() < 1e-6, "still the mounted 0.1 tick: {own:?}");
    assert!(
        node.submitted.lock().unwrap().iter().all(|o| o.order_client_id != "nope"),
        "the refused order never reached the venue"
    );
    assert!(!node.blocks()[0].trades("NOSUCHUSDT"));
    node.core.shutdown_and_join();
}

/// A paper engine and a surface with no source each refuse at the door, through `accept_command`,
/// before anything is lowered.
#[test]
fn paper_and_a_surface_without_a_source_refuse_through_the_door() {
    let facts = Table::eth();
    let mut paper = Node::start("bybit", "BTCUSDT", AccountTier::Paper);
    let err = paper.send(limit("p", "bybit", "ETHUSDT", 1.0, 5.0), Some(&facts)).unwrap_err();
    assert!(err.contains("paper account"), "{err}");
    paper.core.shutdown_and_join();

    let mut bare = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let err = bare.send(limit("b", "bybit", "ETHUSDT", 1.0, 5.0), None).unwrap_err();
    assert!(err.contains("cannot look up a symbol's facts"), "{err}");
    assert!(bare.submitted.lock().unwrap().is_empty());
    bare.core.shutdown_and_join();
}

/// The table the door admits by and the list the mount fails closed by are the same venues:
/// a venue the door admits but the engine does not fail closed on would sign an unadmitted order on
/// the mounted grid after a lost admission; one the engine fails closed on but the door does not
/// admit could never place a foreign order.
#[test]
fn the_venues_the_door_admits_are_the_ones_the_engine_fails_closed_on() {
    let mut admits: Vec<&str> = SYMBOL_RULES
        .iter()
        .filter(|(_, r)| matches!(r, VenueSymbolRule::Admits(_)))
        .map(|(v, _)| *v)
        .collect();
    admits.sort_unstable();
    let mut closed: Vec<String> = crate::server::symbol_admission::admitting_venues();
    closed.sort_unstable();
    assert_eq!(admits, closed);
}

/// **A lost admission stops the order**: sent straight to the core with no `AdmitSymbol` (as a
/// refused or dropped one would leave it), an order on a symbol the engine was never told about is
/// DENIED by the fail-closed engine, not rounded onto the mounted grid and signed.
#[test]
fn an_order_with_no_admission_is_denied_by_the_engine_not_signed_on_the_mounted_grid() {
    for (venue, mounted, foreign) in [
        ("bybit", "BTCUSDT", "ETHUSDT"),
        ("okx", "BTC-USDT-SWAP", "ETH-USDT-SWAP"),
        ("hyperliquid", "BTC", "kPEPE"),
    ] {
        let mut node = Node::start(venue, mounted, AccountTier::Demo);
        let order = vike_model::OrderRequest {
            order_client_id: "lost".into(),
            venue: venue.into(),
            symbol: foreign.into(),
            side: 1,
            qty: 0.12346,
            order_type: vike_model::OrderType::Limit,
            price: Some(3000.456),
            ..Default::default()
        };
        node.core.send_command(vike_execution::Command::Order(
            vike_execution::OrderIntent::Submit(Box::new(order)),
        ));
        node.send(limit("after", venue, mounted, 1.0, 60_000.0), None).expect("mounted");
        node.venue_saw("after");
        assert!(
            node.submitted.lock().unwrap().iter().all(|o| o.order_client_id != "lost"),
            "{venue}: the unadmitted order never reached the venue client"
        );
        node.core.shutdown_and_join();
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 2: labelled accounts, previews, refresh, release
// ---------------------------------------------------------------------------------------------

/// A LABELLED account is admitted a new symbol like the default one, under its OWN route key: the
/// core fans the venue's quotes out to every engine admitted the symbol, so it is priced. Its own
/// symbol passes untouched, and the default account of the same venue is admitted separately.
#[test]
fn a_labelled_account_is_admitted_a_symbol_under_its_own_route_key() {
    let facts = Table::eth();
    let roster = [engine("binance", "BTCUSDT"), engine("binance#ALT", "BTCUSDT")];
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("binance", "ETHUSDT", Some("ALT")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETHUSDT");
    let admit = admit.expect("ALT is admitted ETH");
    assert_eq!(admit.route_key, "binance#ALT", "ALT's engine, not the default's");
    assert_eq!(facts.asked(), vec!["binance#ALT/ETHUSDT".to_string()]);
    // The default account of the same venue is admitted under the bare venue key.
    let (_, admit) = placed(plan_submit_symbol(
        &submit("binance", "ETHUSDT", None),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(admit.expect("admitted").route_key, "binance");
    // The labelled account's OWN symbol passes untouched.
    assert_eq!(
        plan_submit_symbol(&submit("binance", "BTCUSDT", Some("ALT")), &roster, Some(&facts), true),
        SymbolPlan::Pass
    );
}

/// **The labelled account's whole path through the door and a REAL two-account core**: a MARKET order
/// (no price of its own) on a symbol `bybit#ALT` was never started on is admitted into ALT's engine
/// with the door's price as its first mark and placed on ALT's client, not the default account's. (That later quotes keep ALT's book priced is
/// `vike-core`'s `admitted_fanout`, pinned in its own suite.)
#[test]
fn a_labelled_account_places_a_market_order_on_a_new_symbol() {
    let mut node = Node::start_with("bybit", "BTCUSDT", AccountTier::Demo, &["ALT"]);
    let facts = Table::eth();
    let mut cmd = market("bybit", "ETHUSDT", false);
    if let CommandWire::Submit(req) = &mut cmd {
        req.order_client_id = "alt-mkt".into();
        req.account = Some("ALT".into());
    }
    let acked = node.send(cmd, Some(&facts)).expect("accepted");
    assert_eq!(acked, "alt-mkt");
    let seen = node.venue_saw("alt-mkt");
    assert_eq!((seen.symbol.as_str(), seen.order_client_id.as_str()), ("ETHUSDT", "alt-mkt"));
    let blocks = node.blocks_once_trading("bybit#ALT", "ETHUSDT");
    let alt = blocks.iter().find(|b| b.route_key == "bybit#ALT").expect("ALT's block");
    let default = blocks.iter().find(|b| b.route_key == "bybit").expect("the default block");
    assert!(alt.trades("ETHUSDT"), "ALT was admitted ETH");
    assert!(!default.trades("ETHUSDT"), "the default account was not");
    node.core.shutdown_and_join();
}

/// What the publishing side says of a labelled account must agree with the door: a labelled engine of
/// an admitting venue fails closed for an unadmitted symbol (the flag the mount sets), exactly as the
/// default one does, and `VenueBlock::admits_symbols` (and its wire twin) advertises it for BOTH; the
/// door admits both.
#[test]
fn the_engine_flag_the_door_and_the_labelled_account_agree() {
    let node = Node::start_with("bybit", "BTCUSDT", AccountTier::Demo, &["ALT"]);
    let blocks = node.blocks();
    let roster: Vec<_> = blocks.iter().map(|b| b.route_key.as_str()).collect();
    assert_eq!(roster, ["bybit", "bybit#ALT"]);
    assert!(
        blocks.iter().all(|b| b.admits_symbols),
        "the default and the labelled engine both advertise it: {:?}",
        blocks.iter().map(|b| (&b.route_key, b.admits_symbols)).collect::<Vec<_>>()
    );
    let wire = {
        let cell = node.core.snapshot_cell();
        let snap = cell.load();
        crate::publish::project(&snap, None, &[])
    };
    assert!(wire.venues.iter().all(|v| v.admits_symbols), "and so does the wire frame");
    for key in roster {
        let facts = Table::eth();
        let label = vike_model::accounts::account_keys::label_of_route_key("bybit", key)
            .expect("every roster key is a bybit account's");
        let plan = plan_submit_symbol(
            &submit("bybit", "ETHUSDT", label.text()),
            &blocks,
            Some(&facts),
            true,
        );
        let (_, admit) = placed(plan);
        assert_eq!(admit.expect("admitted").route_key, key, "the door admits {key}");
    }
    node.core.shutdown_and_join();
}

/// A PREVIEW reads: it asks for no subscription (a control peer must not be able to open price pumps
/// by previewing), refreshes nothing and never waits; a COMMAND subscribes.
#[test]
fn a_preview_never_subscribes_a_price_feed_and_a_command_does() {
    let roster = [engine("bybit", "BTCUSDT")];
    let preview = Table::eth().with_price(Price::Pending);
    assert_eq!(
        submit_symbol_refusal(&submit("bybit", "ETHUSDT", None), &roster, Some(&preview)),
        None,
        "a pending price is not a refusal in a preview"
    );
    assert_eq!(preview.events(), ["price:ETHUSDT:peek"], "a read, not a subscription");

    let command = Table::eth().with_price(Price::Pending);
    let plan = plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&command), true);
    assert!(matches!(plan, SymbolPlan::Place { wait: Some(_), .. }), "{plan:?}");
    assert_eq!(command.events(), ["price:ETHUSDT:subscribe"]);

    // A preview whose feed cannot exist is refused with the cause, like the command.
    let failing = Table::eth().with_price(Price::Failing);
    assert!(
        submit_symbol_refusal(&submit("bybit", "ETHUSDT", None), &roster, Some(&failing))
            .is_some_and(|why| why.text.contains("no price feed"))
    );
}

/// A symbol the engine already trades is refreshed when the source has fresher facts, as an ordinary
/// `AdmitSymbol` ahead of the order; a preview does not.
#[test]
fn a_stale_admission_is_refreshed_ahead_of_the_next_order() {
    let roster = [vike_execution::VenueBlock {
        extra_symbols: vec!["ETHUSDT".into()].into(),
        ..engine("bybit", "BTCUSDT")
    }];
    let facts = Table::eth();
    let fresh = SymbolAdmission {
        route_key: "bybit".into(),
        symbol: "ETHUSDT".into(),
        grid: SymbolGrid { tick_size: Some(0.05), lot_size: Some(0.1), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    };
    *facts.refresh.lock().unwrap() = Some(fresh.clone());
    assert!(
        matches!(
            plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&facts), false),
            SymbolPlan::Place { admit: None, .. }
        ),
        "a preview refreshes nothing"
    );
    let (_, admit) =
        placed(plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&facts), true));
    assert_eq!(admit.as_deref(), Some(&fresh), "the fresh grid goes ahead of the order");
    let (_, again) =
        placed(plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&facts), true));
    assert!(again.is_none(), "and only once");
}

/// **A symbol the engine already trades still asks the price lane** (v0.1.66 on the CI box: a symbol restored
/// from the journal trades but has no mark, and no ask ever subscribed its price, so every order was denied
/// `no-reference-price`). An opening order with no price yet is held for one; a close is not; a Preview, a
/// paper engine and a failing lane change nothing (the engine's own `no-reference-price` stays the answer).
#[test]
fn a_symbol_the_engine_already_trades_asks_the_price_lane() {
    let trading = |tier| vike_execution::VenueBlock {
        extra_symbols: vec!["ETHUSDT".into()].into(),
        tier: Some(tier),
        ..engine("bybit", "BTCUSDT")
    };
    let roster = [trading(AccountTier::Demo)];
    let wait = Some(PriceWait { route_key: "bybit".into(), venue: "bybit".into() });
    let place = |wait| SymbolPlan::Place { symbol: "ETHUSDT".into(), admit: None, wait };

    let pending = Table::eth().with_price(Price::Pending);
    assert_eq!(
        plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&pending), true),
        place(wait),
        "no price yet: held for the first one"
    );
    assert_eq!(pending.events(), ["price:ETHUSDT:subscribe"], "the lane was asked, and opened");

    let close = Table::eth().with_price(Price::Pending);
    assert_eq!(
        plan_submit_symbol(&market("bybit", "ETHUSDT", true), &roster, Some(&close), true),
        place(None),
        "a close is never held up"
    );
    assert!(close.events().is_empty(), "and asks nothing of a gatewayhub: {:?}", close.events());

    let ready = Table::eth();
    assert_eq!(
        plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&ready), true),
        place(None),
        "a price is already here"
    );

    let preview = Table::eth().with_price(Price::Pending);
    assert_eq!(
        plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&preview), false),
        place(None)
    );
    assert!(preview.events().is_empty(), "a Preview asks nothing: {:?}", preview.events());

    let paper = Table::eth().with_price(Price::Pending);
    assert_eq!(
        plan_submit_symbol(
            &submit("bybit", "ETHUSDT", None),
            &[trading(AccountTier::Paper)],
            Some(&paper),
            true
        ),
        place(None)
    );
    assert!(paper.events().is_empty(), "a paper engine is priced by its own feed");

    let failing = Table::eth().with_price(Price::Failing);
    assert_eq!(
        plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&failing), true),
        place(None),
        "the engine's own no-reference-price is the fail-closed answer"
    );
}

/// An order held for a price that never comes releases the feed it opened, and nothing is sent.
#[test]
fn a_timed_out_price_wait_releases_the_feed_and_sends_nothing() {
    let facts = Table::eth().with_price(Price::Pending);
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let err = node
        .send(limit("slow", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts))
        .expect_err("no price within the wait");
    assert!(err.contains("no price on this node yet"), "{err}");
    let events = facts.events();
    assert_eq!(events.last().map(String::as_str), Some("release:ETHUSDT"), "{events:?}");
    assert!(!events.iter().any(|e| e.starts_with("admitted")), "{events:?}");
    assert!(node.submitted.lock().unwrap().is_empty());
    assert!(!node.blocks()[0].trades("ETHUSDT"), "never admitted");
    node.core.shutdown_and_join();
}

/// A sent admission is announced to the source (it keeps the feed and may refresh later).
#[test]
fn a_sent_admission_is_announced_to_the_source() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.send(limit("one", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("accepted");
    assert!(facts.events().contains(&"admitted:ETHUSDT".to_string()), "{:?}", facts.events());
    node.core.shutdown_and_join();
}

/// An empty symbol is refused on an admitting venue (it was refused by the interim gate too; the
/// adapters would read it as the mounted symbol).
#[test]
fn an_empty_symbol_is_refused_on_an_admitting_venue() {
    let facts = Table::eth();
    let roster = [engine("bybit", "BTCUSDT")];
    let why = refused(plan_submit_symbol(&submit("bybit", "", None), &roster, Some(&facts), true));
    assert!(why.contains("is not a symbol"), "{why}");
}

/// **The capability a node PUBLISHES is the door's admit set, row for row (one table).** Every roster
/// venue gets a real engine built the way the mount builds it (the fail-closed flag from
/// `admitting_venues`, as `Node::start` does), a real core publishes its block, and the wire projection
/// a desktop reads says `admits_symbols` for exactly the venues whose `SYMBOL_RULES` row is `Admits`:
/// the venue the door would admit a foreign symbol for is the venue the desktop is told it may send one
/// to, and a `MountedOnly` or `PerOrder` venue is never advertised. The key is written only when true,
/// so an unadvertised venue's frame is the one an older node writes.
#[test]
fn the_published_capability_is_the_doors_admit_set() {
    let mut advertised: Vec<&str> = Vec::new();
    for venue in vike_model::VENUES.iter().copied() {
        let node = Node::start(venue, "BTCUSDT", AccountTier::Demo);
        let wire = {
            let cell = node.core.snapshot_cell();
            let snap = cell.load();
            crate::publish::project(&snap, None, &[])
        };
        node.core.shutdown_and_join();
        let [block] = wire.venues.as_slice() else { panic!("{venue}: one engine, one block") };
        let json = serde_json::to_string(block).expect("serializes");
        assert_eq!(json.contains("admits_symbols"), block.admits_symbols, "{venue}: {json}");
        if block.admits_symbols {
            advertised.push(venue);
        }
    }
    advertised.sort_unstable();
    let mut door: Vec<&str> = SYMBOL_RULES
        .iter()
        .filter(|(_, rule)| matches!(rule, VenueSymbolRule::Admits(_)))
        .map(|(venue, _)| *venue)
        .collect();
    door.sort_unstable();
    assert_eq!(advertised, door);
    let mut admits = ADMITS.to_vec();
    admits.sort_unstable();
    assert_eq!(
        advertised, admits,
        "bybit, binance, aster, okx, hyperliquid, deribit, alpaca, ctrader, oanda, ig, ibkr, dukascopy, fxcm and polymarket, and nothing else"
    );
}

// ---------------------------------------------------------------------------------------------
// Review round 3: the first mark, subscriptions that cannot leak, slots that cannot be burned,
// a live account's ceiling
// ---------------------------------------------------------------------------------------------

/// The price the door saw rides the admission as the engine's first mark for the symbol.
#[test]
fn the_price_the_door_saw_is_seeded_into_the_admission() {
    let roster = [engine("bybit", "BTCUSDT")];
    let (_, admit) = placed(plan_submit_symbol(
        &submit("bybit", "ETHUSDT", None),
        &roster,
        Some(&Table::eth()),
        true,
    ));
    let admit = admit.expect("admitted");
    assert_eq!((admit.initial_mark, admit.initial_mark_ms), (Some(3000.0), None), "no time known");
    // ...with the time it was MADE when the source knows it, so the engine's first mark keeps it.
    let dated = Table { made: Some(1_762_000_000_000), ..Table::eth() };
    let (_, admit) =
        placed(plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &roster, Some(&dated), true));
    let admit = admit.expect("admitted");
    assert_eq!(
        (admit.initial_mark, admit.initial_mark_ms),
        (Some(3000.0), Some(1_762_000_000_000))
    );
}

/// End to end: after the first order admits the symbol with the door's price as its first mark, a
/// MARKET order on it reaches the venue (the gate had a reference price); with no seed the engine
/// would deny it `no-reference-price` until a quote arrived.
#[test]
fn the_seeded_mark_lets_a_market_order_through_right_after_the_admission() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.send(limit("first", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("accepted");
    node.venue_saw("first");
    let CommandWire::Submit(mut req) = limit("mkt", "bybit", "ETHUSDT", 1.0, 3000.0) else {
        unreachable!()
    };
    req.order_type = "market".into();
    req.price = None;
    node.send(CommandWire::Submit(req), Some(&facts)).expect("accepted");
    node.venue_saw("mkt");
    node.core.shutdown_and_join();
}

/// A LIVE account takes a new symbol only with an account-wide exposure ceiling armed; a demo account
/// does not need one. The refusal names the setting.
#[test]
fn a_live_account_without_an_account_wide_ceiling_is_refused_a_new_symbol() {
    let facts = Table::eth().uncapped();
    let live =
        vike_execution::VenueBlock { tier: Some(AccountTier::Live), ..engine("bybit", "BTCUSDT") };
    let why = refused(plan_submit_symbol(
        &submit("bybit", "ETHUSDT", None),
        std::slice::from_ref(&live),
        Some(&facts),
        true,
    ));
    assert!(why.contains("max_account_exposure") && why.contains("LIVE"), "{why}");
    assert!(facts.asked().is_empty(), "refused before any lookup");
    placed(plan_submit_symbol(
        &submit("bybit", "ETHUSDT", None),
        std::slice::from_ref(&live),
        Some(&Table::eth()),
        true,
    ));
    placed(plan_submit_symbol(
        &submit("bybit", "ETHUSDT", None),
        &[engine("bybit", "BTCUSDT")],
        Some(&facts),
        true,
    ));
}

/// An order that cannot be lowered (an empty order client id) spends nothing: no lookup, no price
/// subscription, nothing to give back.
#[test]
fn an_order_that_cannot_be_lowered_opens_nothing() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let err = node.send(limit("", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).unwrap_err();
    assert!(err.contains("order_client_id"), "{err}");
    assert!(facts.events().is_empty() && facts.asked().is_empty(), "{:?}", facts.events());
    node.core.shutdown_and_join();
}

/// A reduce-only order cannot burn a symbol slot; nor can an order the venue's own grid rejects.
#[test]
fn an_order_the_venue_would_reject_cannot_spend_a_symbol_slot() {
    let facts = Table::eth();
    let roster = [engine("bybit", "BTCUSDT")];
    let why =
        refused(plan_submit_symbol(&market("bybit", "ETHUSDT", true), &roster, Some(&facts), true));
    assert!(why.contains("reduce-only"), "{why}");
    // bybit ETHUSDT: lot 0.01
    let mut tiny = limit("tiny", "bybit", "ETHUSDT", 0.001, 3000.0);
    let why = refused(plan_submit_symbol(&tiny, &roster, Some(&facts), true));
    assert!(why.contains("below one lot") && why.contains("no symbol slot was spent"), "{why}");
    let CommandWire::Submit(req) = &mut tiny else { unreachable!() };
    req.qty = 1.0;
    placed(plan_submit_symbol(&tiny, &roster, Some(&facts), true));
    assert!(
        !facts.events().iter().any(|e| e.starts_with("admitted")),
        "planning never announces an admission"
    );
}

/// What a Preview of a foreign symbol costs the source is a lookup, and only for such a symbol.
#[test]
fn a_preview_takes_a_rate_token_only_when_it_would_cost_a_lookup() {
    use crate::server::symbol_admission::needs_lookup;
    // Every venue has a block, so each verdict below is the RULE's and not an absent roster row.
    let roster = [
        engine("bybit", "BTCUSDT"),
        engine("okx", "BTC-USDT-SWAP"),
        engine("deribit", "BTC-PERPETUAL"),
    ];
    assert!(needs_lookup(&submit("bybit", "ETHUSDT", None), &roster));
    assert!(needs_lookup(&submit("okx", "ETH-USDT-SWAP", None), &roster), "okx admits");
    assert!(!needs_lookup(&submit("bybit", "BTCUSDT", None), &roster), "the engine's own symbol");
    assert!(needs_lookup(&submit("deribit", "ETH-PERPETUAL", None), &roster), "deribit admits");
    assert!(
        !needs_lookup(&submit("deribit", "BTC-PERPETUAL", None), &roster),
        "the engine's own symbol"
    );
    assert!(!needs_lookup(&CommandWire::Cancel("c".into()), &roster), "not a submit");
    let declared = vike_execution::VenueBlock {
        extra_symbols: vec!["ETHUSDT".into()].into(),
        ..engine("bybit", "BTCUSDT")
    };
    assert!(!needs_lookup(&submit("bybit", "ETHUSDT", None), &[declared]), "already traded");
}
// ---------------------------------------------------------------------------------------------
// hyperliquid: the coin name is the key, case and all
// ---------------------------------------------------------------------------------------------

/// **A Hyperliquid coin is its user name in any case, and what no kept row names is refused before any
/// lookup** (decision 0290). `kPEPEUSDC.P` and `kpepeusdc.p` are the kept row's `KPEPEUSDC.P`, asked as such; the
/// venue's own coin names (`kPEPE`, `BTC`) and a lane suffix on a bare coin name no row, and the wire name of a
/// spot pair (`@107`) is refused by its shape, each asking nothing; the mounted coin passes untouched.
#[test]
fn hyperliquid_reads_a_coin_by_its_user_name_in_any_case_and_refuses_the_rest_before_any_lookup() {
    let facts = Table::eth();
    let roster = [engine("hyperliquid", "BTCUSDC.P")];
    let on = |typed: &str| {
        plan_submit_symbol(&submit("hyperliquid", typed, None), &roster, Some(&facts), true)
    };

    for typed in ["kPEPEUSDC.P", "kpepeusdc.p"] {
        let (symbol, admit) = placed(on(typed));
        assert_eq!(symbol, "KPEPEUSDC.P", "{typed}");
        let admit = admit.expect("admitted first");
        assert_eq!(
            (admit.route_key.as_str(), admit.symbol.as_str()),
            ("hyperliquid", "KPEPEUSDC.P"),
            "{typed}"
        );
    }
    assert_eq!(facts.asked(), ["hyperliquid/KPEPEUSDC.P", "hyperliquid/KPEPEUSDC.P"]);

    // Not a name a kept row answers (the venue's own coin names, a lane suffix on a bare coin), and the wire
    // name of a spot pair: refused at the door, nothing asked.
    let before = facts.asked().len();
    for (bad, word) in [
        ("kPEPE", "carries no row"),
        ("BTC", "carries no row"),
        ("BTC.P", "carries no row"),
        ("@107", "wire name of a spot pair"),
    ] {
        let why = refused(on(bad));
        assert!(why.contains(word) && why.contains("REFUSED"), "{bad}: {why}");
    }
    assert_eq!(facts.asked().len(), before);

    // The mounted coin is not this gate's, and a paper account is one book.
    assert_eq!(on("BTCUSDC.P"), SymbolPlan::Pass);
    let paper = vike_execution::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine("hyperliquid", "BTCUSDC.P")
    };
    let why = refused(plan_submit_symbol(
        &submit("hyperliquid", "KPEPEUSDC.P", None),
        &[paper],
        Some(&facts),
        true,
    ));
    assert!(why.contains("paper account"), "{why}");
}

/// An OPENING order on an admitted coin needs its price; a reduce-only first order on a coin the
/// engine was never told about holds nothing to close and burns no slot (the shared rule, decision
/// 0174 item 10), and an already admitted coin's close never waits.
#[test]
fn an_opening_hyperliquid_order_waits_for_its_quote_and_a_close_does_not() {
    let roster = [engine("hyperliquid", "BTCUSDC.P")];
    let pending = Table::eth().with_price(Price::Pending);
    assert!(matches!(
        plan_submit_symbol(
            &submit("hyperliquid", "KPEPEUSDC.P", None),
            &roster,
            Some(&pending),
            true
        ),
        SymbolPlan::Place { wait: Some(_), admit: Some(_), .. }
    ));
    let why = refused(plan_submit_symbol(
        &market("hyperliquid", "KPEPEUSDC.P", true),
        &roster,
        Some(&pending),
        true,
    ));
    assert!(why.contains("reduce-only"), "{why}");
    let held = vike_execution::VenueBlock {
        extra_symbols: vec!["KPEPEUSDC.P".into()].into(),
        ..engine("hyperliquid", "BTCUSDC.P")
    };
    assert!(matches!(
        plan_submit_symbol(
            &market("hyperliquid", "KPEPEUSDC.P", true),
            &[held],
            Some(&pending),
            true
        ),
        SymbolPlan::Place { admit: None, wait: None, .. }
    ));
}

/// **A LIVE hyperliquid account takes a new coin only with the account-wide exposure ceiling armed**
/// (the rule every `Admits` venue shares: `max_total_exposure` bounds ONE symbol), and an order on a coin
/// the facts source cannot answer is refused with nothing admitted.
#[test]
fn a_live_hyperliquid_account_needs_the_ceiling_and_an_unlisted_coin_is_refused() {
    let live = vike_execution::VenueBlock {
        tier: Some(AccountTier::Live),
        ..engine("hyperliquid", "BTCUSDC.P")
    };
    let uncapped = Table::eth().uncapped();
    let why = refused(plan_submit_symbol(
        &submit("hyperliquid", "KPEPEUSDC.P", None),
        std::slice::from_ref(&live),
        Some(&uncapped),
        true,
    ));
    assert!(why.contains("max_account_exposure") && why.contains("LIVE"), "{why}");
    assert!(uncapped.asked().is_empty(), "refused before any lookup");
    // Armed: the same order is placed; a kept coin the facts source does not list is refused, nothing admitted.
    let armed = Table::eth();
    placed(plan_submit_symbol(
        &submit("hyperliquid", "KPEPEUSDC.P", None),
        std::slice::from_ref(&live),
        Some(&armed),
        true,
    ));
    let why = refused(plan_submit_symbol(
        &submit("hyperliquid", "ETHUSDC.P", None),
        std::slice::from_ref(&live),
        Some(&armed),
        true,
    ));
    assert!(why.contains("cannot admit `ETHUSDC.P`"), "{why}");
}

/// **Two hyperliquid accounts of one exchange admit a coin independently.** An account's admission is
/// under its own route key: planned for the default account and for `ALT` they are two admissions,
/// each asked of its own lease. Through a REAL two-account core, a market order on `ALT` admits the
/// coin into ALT's engine only and reaches the venue client once, on the coin's own spelling; the
/// default engine still does not trade it (and keeps failing closed for it). (A book shared by two
/// accounts of ONE network is the shared-book rule's warning, not this table's.)
#[test]
fn two_hyperliquid_accounts_admit_a_coin_independently() {
    let facts = Table::eth();
    let roster = [engine("hyperliquid", "BTCUSDC.P"), engine("hyperliquid#ALT", "BTCUSDC.P")];
    let (_, admit) = placed(plan_submit_symbol(
        &submit("hyperliquid", "KPEPEUSDC.P", Some("ALT")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(admit.expect("ALT is admitted").route_key, "hyperliquid#ALT");
    let (_, admit) = placed(plan_submit_symbol(
        &submit("hyperliquid", "KPEPEUSDC.P", None),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(admit.expect("the default is admitted").route_key, "hyperliquid");
    assert_eq!(
        facts.asked(),
        vec!["hyperliquid#ALT/KPEPEUSDC.P".to_string(), "hyperliquid/KPEPEUSDC.P".to_string()]
    );

    let mut node = Node::start_with("hyperliquid", "BTCUSDC.P", AccountTier::Demo, &["ALT"]);
    let facts = Table::eth();
    let mut cmd = market("hyperliquid", "KPEPEUSDC.P", false);
    if let CommandWire::Submit(req) = &mut cmd {
        req.order_client_id = "hl-alt".into();
        req.account = Some("ALT".into());
    }
    node.send(cmd, Some(&facts)).expect("ALT is admitted the coin");
    let seen = node.venue_saw("hl-alt");
    assert_eq!(seen.symbol, "KPEPEUSDC.P", "signed on the coin's user name");
    let blocks = node.blocks_once_trading("hyperliquid#ALT", "KPEPEUSDC.P");
    let block = |key: &str| blocks.iter().find(|b| b.route_key == key).expect(key);
    assert!(block("hyperliquid#ALT").trades("KPEPEUSDC.P"));
    assert!(!block("hyperliquid").trades("KPEPEUSDC.P"), "the other account was not told");
    assert!(block("hyperliquid").trades("BTCUSDC.P"), "and keeps its own coin");
    node.core.shutdown_and_join();
}

/// **A hyperliquid engine of ANY mounted product takes a perpetual, a spot pair or a builder-dex coin as an
/// admitted symbol** (decision 0218; before it an engine mounted on a spot pair or a dex coin was refused a core
/// perp for its "one product"). The adapter signs each order by the ORDER's symbol on its own asset id, grid and
/// product, so the door asks the facts and places; the mounted symbol itself still passes untouched.
#[test]
fn a_hyperliquid_engine_of_any_mounted_product_takes_another_product_as_an_admitted_symbol() {
    for mounted in ["HYPEUSDC", "XYZ:TSLAUSDC.P", "BTCUSDC.P"] {
        let facts = Table::eth();
        let roster = [engine("hyperliquid", mounted)];
        for typed in
            ["KPEPEUSDC.P", "HYPEUSDC", "XYZ:TSLAUSDC.P"].into_iter().filter(|t| *t != mounted)
        {
            let (symbol, admit) = placed(plan_submit_symbol(
                &submit("hyperliquid", typed, None),
                &roster,
                Some(&facts),
                true,
            ));
            assert_eq!(symbol, typed, "{mounted}: {typed}");
            assert_eq!(admit.expect("admitted first").symbol, typed, "{mounted}: {typed}");
        }
        assert_eq!(
            plan_submit_symbol(&submit("hyperliquid", mounted, None), &roster, Some(&facts), true),
            SymbolPlan::Pass,
            "{mounted}: the mounted symbol is not this gate's"
        );
    }
}

/// **alpaca takes a user name and nothing else**: an equity ticker (a share-class dot allowed) or a crypto pair
/// without a slash (decision 0290). What is not a symbol is refused by its shape, and what no kept row names (the
/// venue's own slashed `BTC/USD` among them) by the list, both BEFORE any lookup, because the symbol reaches a query
/// string and a JSON body downstream.
#[test]
fn alpaca_refuses_what_is_not_a_user_name_before_any_lookup() {
    let facts = Table::eth();
    for (bad, word) in [
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("BTC USD", "is not a symbol"),
        ("AAPL&x=1", "is not a symbol"),
        ("A\"}", "is not a symbol"),
        ("BTC/USD", "carries no row"),
        ("BTC-USD", "carries no row"),
        ("BTC/USD/X", "carries no row"),
        ("A.B.C", "carries no row"),
        ("/USD", "carries no row"),
        ("BTC/", "carries no row"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("alpaca", bad, None),
            &[engine("alpaca", "AAPL")],
            Some(&facts),
            true,
        ));
        assert!(why.contains(word), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **One instrument is one symbol in an engine**: a crypto pair has one user name, `BTCUSD` (decision 0290), so the
/// pair an engine trades, whichever of its symbols (mounted, a declared leg or an earlier admission) it is, typed in
/// another case is that symbol, with no lookup; the venue's slashed `BTC/USD` is no second name of it.
#[test]
fn alpaca_refuses_the_other_spelling_of_an_instrument_the_engine_trades() {
    let facts = Table::eth();
    let leg = || vike_execution::VenueBlock {
        extra_symbols: vec!["BTCUSD".into()].into(),
        ..engine("alpaca", "AAPL")
    };
    for (block, typed) in [(engine("alpaca", "BTCUSD"), "btc/usd"), (leg(), "BTC/USD")] {
        let why = refused(plan_submit_symbol(
            &submit("alpaca", typed, None),
            &[block],
            Some(&facts),
            true,
        ));
        assert!(why.contains("carries no row"), "{typed}: {why}");
        assert!(why.contains("REFUSED"), "{typed}: {why}");
    }
    for (block, typed) in [(engine("alpaca", "BTCUSD"), "btcusd"), (leg(), " BtcUsd ")] {
        assert_eq!(
            plan_submit_symbol(&submit("alpaca", typed, None), &[block], Some(&facts), true),
            SymbolPlan::Place { symbol: "BTCUSD".into(), admit: None, wait: None },
            "{typed}"
        );
    }
    assert!(facts.asked().is_empty(), "no lookup, no slot spent");
}

/// **A restarted account is judged on its NEW engine.** The core's list only grows, so the published engine
/// blocks hold the stopped engine's block (which had traded ETHUSDT) before the new one's (which has not): the
/// door must plan the admission the new engine needs, or the RiskGate denies `symbol-not-admitted`. The first
/// assertion pins the raw list's wrong answer (the first match), so the second is the fix and not a tautology.
#[test]
fn a_symbol_only_the_stale_block_traded_is_admitted_on_the_new_engine() {
    let facts = Table::eth();
    let stale = vike_execution::VenueBlock {
        extra_symbols: vec!["ETHUSDT".to_string()].into(),
        ..engine("bybit#ALT", "BTCUSDT")
    };
    let raw = vec![stale, engine("bybit#ALT", "BTCUSDT")];
    let cmd = submit("bybit", "ETHUSDT", Some("ALT"));
    let (_, admit) = placed(plan_submit_symbol(&cmd, &raw, Some(&facts), true));
    assert!(admit.is_none(), "the raw list's first match is the stale engine: no admission");

    let mut snap = vike_execution::CoreSnapshot::empty("bybit", "BTCUSDT");
    snap.portfolio.venues = raw;
    let publisher = crate::publish::spawn(Arc::new(arc_swap::ArcSwap::from_pointee(snap)), None);
    let (_, admit) =
        placed(plan_submit_symbol(&cmd, &publisher.engine_blocks(), Some(&facts), true));
    assert_eq!(admit.expect("admitted first").route_key, "bybit#ALT");
    publisher.shutdown();
}

/// An alpaca engine is one real USD account whatever it is mounted on: an equity engine is admitted a
/// crypto pair and a crypto engine an equity (both are USD-quoted with a multiplier of 1, so the
/// engine's `qty x price` is the real exposure), and the admission carries the DEFAULT contract value.
#[test]
fn alpaca_mixes_equities_and_crypto_pairs_on_one_account() {
    let facts = Table::eth();
    for (mounted, typed, canonical) in [("AAPL", "btcusd", "BTCUSD"), ("BTCUSD", "msft", "MSFT")] {
        let (symbol, admit) = placed(plan_submit_symbol(
            &submit("alpaca", typed, None),
            &[engine("alpaca", mounted)],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, canonical);
        assert_eq!(admit.expect("admitted first").contract_value, None, "linear, multiplier 1");
    }
}

/// **ctrader takes a user name and nothing else** (decision 0290: the broker's own name is the kept row's wire, its
/// bridge's alone): what is not a symbol (it would reach a log line and a protobuf field) is refused by its shape,
/// and what no kept row names by the list, both BEFORE any lookup.
#[test]
fn ctrader_refuses_what_is_not_a_user_name_before_any_lookup() {
    let facts = Table::eth();
    let too_long = "A".repeat(33);
    let bad_names = [
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("EUR USD", "is not a symbol"),
        ("A\"}", "is not a symbol"),
        ("EUR\nUSD", "is not a symbol"),
        ("EUR/USD", "carries no row"),
        ("EUR-USD", "carries no row"),
        ("EURUSD.x", "carries no row"),
        (too_long.as_str(), "carries no row"),
    ];
    for (bad, word) in bad_names {
        let why = refused(plan_submit_symbol(
            &submit("ctrader", bad, None),
            &[engine("ctrader", "EURUSD")],
            Some(&facts),
            true,
        ));
        assert!(why.contains(word), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **ig takes a user name and nothing else** (decision 0290: the epic is the kept row's wire, its bridge's alone):
/// what could not stand in a URL path is refused by its shape, and a name no kept row carries (an epic of any number of
/// segments, IG-2's five-segment shape being the bridge's, a pair with a slash) by the list, both BEFORE any lookup.
#[test]
fn ig_refuses_what_is_not_a_user_name_before_any_lookup() {
    let facts = Table::eth();
    let long = format!("CS.D.{}.IP", "X".repeat(64));
    for (bad, word) in [
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("CS.D.EUR USD.IP", "is not a symbol"),
        ("CS.D.EURUSD.MINI.IP?x=1", "is not a symbol"),
        ("CS.D.EUR\"}.IP", "is not a symbol"),
        (long.as_str(), "is not a symbol"),
        ("CS.D.EURUSD.MINI.IP", "carries no row"),
        ("CS.D.EURUSD", "carries no row"),
        ("CS.D.EURUSD.MINI", "carries no row"),
        ("CS.D.EURUSD.MINI.IP.X", "carries no row"),
        ("EUR/USD", "carries no row"),
        ("CS.D", "carries no row"),
        ("CS..EURUSD.IP", "carries no row"),
        (".D.EURUSD.IP", "carries no row"),
        ("CS.D.EURUSD.", "carries no row"),
        ("CS.D.EURUSD/../x", "carries no row"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("ig", bad, None),
            &[engine("ig", "EURUSD")],
            Some(&facts),
            true,
        ));
        assert!(why.contains(word), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **A user name is one name in any case** (decision 0290): `gbpusd` is the kept row's `GBPUSD`, which the door asks
/// the facts for (the broker's own name is the row's wire), and a name no kept row carries (the broker's `Wheat_H6`)
/// is refused by the list before any lookup. The mounted symbol itself, as the engine spells it, passes untouched.
#[test]
fn ctrader_reads_a_user_name_in_any_case_and_leaves_the_refusal_to_the_list() {
    let facts = Table::eth();
    let (symbol, _) = placed(plan_submit_symbol(
        &submit("ctrader", "gbpusd", None),
        &[engine("ctrader", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "GBPUSD");
    assert_eq!(facts.asked(), ["ctrader/GBPUSD"], "asked as the row's user name");
    let why = refused(plan_submit_symbol(
        &submit("ctrader", "Wheat_H6", None),
        &[engine("ctrader", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("carries no row") && why.contains("REFUSED"), "{why}");
    assert_eq!(facts.asked().len(), 1, "a name no row carries is never looked up");
    assert_eq!(
        plan_submit_symbol(
            &submit("ctrader", "EURUSD", None),
            &[engine("ctrader", "EURUSD")],
            Some(&facts),
            true
        ),
        SymbolPlan::Pass
    );
}

/// A `paper` ctrader account is one simulated book and keeps its one symbol, like every `Admits` venue.
#[test]
fn a_paper_ctrader_account_keeps_its_one_symbol() {
    let paper = vike_execution::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine("ctrader", "EURUSD")
    };
    let why = refused(plan_submit_symbol(
        &submit("ctrader", "GBPUSD", None),
        &[paper],
        Some(&Table::eth()),
        true,
    ));
    assert!(why.contains("a paper account trades only the symbol it was started on"), "{why}");
}

/// **A symbol refusal travels under its id** (`crate::refusals::SymbolRefusal`): the plan's frame carries
/// `tradehub.symbol.paper_one_book` and the words the door always said, and a person reads `error [<id>]: <text>`.
#[test]
fn a_symbol_refusal_carries_its_id_and_renders_it() {
    use crate::refusals::SymbolRefusal;
    let paper = vike_execution::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine("ctrader", "EURUSD")
    };
    let SymbolPlan::Refuse(frame) =
        plan_submit_symbol(&submit("ctrader", "GBPUSD", None), &[paper], Some(&Table::eth()), true)
    else {
        panic!("a paper account keeps its one symbol")
    };
    assert_eq!(frame.id, "tradehub.symbol.paper_one_book");
    let refusal = SymbolRefusal::PaperOneBook {
        engine: "this node's `ctrader` engine trades `EURUSD` only, not `GBPUSD`".into(),
    };
    assert_eq!(frame, vike_hub_protocol::refusal::HubRefusal::from(refusal.clone()));
    assert_eq!(
        refusal.rendered(),
        "error [tradehub.symbol.paper_one_book]: submit: this node's `ctrader` engine trades `EURUSD` only, not \
         `GBPUSD`: a paper account trades only the symbol it was started on. The order was REFUSED"
    );
}

/// **One instrument is one symbol in an engine** (decision 0290: one user name in any case): the engine's own pair
/// or a declared leg typed in another case is that symbol, placed on the engine's spelling with no lookup and no
/// second admission.
#[test]
fn ig_places_another_case_of_a_symbol_the_engine_trades_on_the_engines_spelling() {
    let facts = Table::eth();
    let leg = vike_execution::VenueBlock {
        extra_symbols: vec!["GBPUSD".into()].into(),
        ..engine("ig", "EURUSD")
    };
    for (block, typed, own) in
        [(engine("ig", "EURUSD"), "eurusd", "EURUSD"), (leg, " GbpUsd ", "GBPUSD")]
    {
        assert_eq!(
            plan_submit_symbol(&submit("ig", typed, None), &[block], Some(&facts), true),
            SymbolPlan::Place { symbol: own.into(), admit: None, wait: None },
            "{typed}"
        );
    }
    assert!(facts.asked().is_empty(), "no lookup, no slot spent");
}

/// **ig's admission carries the epic's CONTRACT VALUE**, which the mounted epic's start does not (the
/// plan's "no grid and no contract value even for the mounted epic"): the facts' linear 10,000 rides the
/// admission, so the engine and the policy ceiling size an order on the epic as contracts x units.
#[test]
fn ig_admission_carries_the_epics_contract_value() {
    let facts = Table::eth();
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("ig", "gbpusd", None),
        &[engine("ig", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "GBPUSD");
    let admit = admit.expect("admitted first");
    assert_eq!(admit.contract_value, Some(vike_model::ContractValue::linear(10_000.0)));
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(0.0001), Some(0.1)));
    // Kept but unlisted by the facts (a refused class, an epic the account cannot trade): refused in the facts'
    // words, no slot spent.
    let why = refused(plan_submit_symbol(
        &submit("ig", "EURJPY", None),
        &[engine("ig", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("cannot admit `EURJPY`") && why.contains("no instrument"), "{why}");
}

/// **The admission door values a floor with the gate's function at the venue's unit** (decision 0170, item 8):
/// one ig GBPUSD mini contract at 1 is worth 10,000, not 1, so a floor of 5 does not refuse it; one dollar of the
/// inverse ETH perpetual at 3,000 is worth 1, not 3,000, so the same floor refuses it before a slot is spent. The
/// door read `qty · price` for both.
#[test]
fn the_admission_floor_values_the_order_at_the_venues_unit() {
    let facts = Table { min_notional: Some(5.0), ..Table::eth() };
    let (symbol, _) = placed(plan_submit_symbol(
        &submit("ig", "GBPUSD", None),
        &[engine("ig", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "GBPUSD");
    let CommandWire::Submit(mut req) = submit("deribit", "ETHUSD.P", None) else { unreachable!() };
    req.price = Some(3_000.0);
    let why = refused(plan_submit_symbol(
        &CommandWire::Submit(req),
        &[deribit_engine("ETHUSD_260327", true)],
        Some(&facts),
        true,
    ));
    assert!(why.contains("the order's value 1 is below the symbol's minimum notional 5"), "{why}");
}

/// **ig, end to end**: an epic the account was not started on is admitted, reaches the venue client as its
/// user name snapped on ITS grid (the pip, the minimum deal size: not the mounted 0.1 / 0.001), its
/// fill folds into its own position, the engine holds its contract value, and the engine now FAILS CLOSED
/// for ig (an order on an epic it was never told is denied, where it used to be signed on the mounted grid).
#[test]
fn ig_places_a_foreign_epic_on_its_own_grid_and_folds_into_its_own_position() {
    let facts = Table::eth().with_price(Price::At(1.2345));
    let mut node = Node::start("ig", "EURUSD", AccountTier::Demo);
    // 0.12346 contracts @ 1.234567: off both grids, so the snap is visible (the table's price is the mark).
    let acked = node
        .send(limit("e2e-ig", "ig", "gbpusd", 0.12346, 1.234_567), Some(&facts))
        .unwrap_or_else(|e| panic!("ig: the order must be accepted, got: {e}"));
    assert_eq!(acked, "e2e-ig");

    let seen = node.venue_saw("e2e-ig");
    assert_eq!(seen.symbol, "GBPUSD", "signed on the user name");
    assert!((seen.price.unwrap() - 1.2346).abs() < 1e-9, "the pip grid: {seen:?}");
    assert!((seen.qty - 0.1).abs() < 1e-9, "the minimum deal size is the step: {seen:?}");

    let cell = node.core.snapshot_cell();
    wait_until("the foreign position", || {
        cell.load().positions.iter().any(|p| p.symbol == "GBPUSD")
    });
    let snap = cell.load_full();
    let block = &snap.portfolio.venues[0];
    assert!(block.trades("GBPUSD"), "published as tradable");
    assert!(block.admits_symbols, "advertised: the desktop may send a symbol");
    assert_eq!(
        block.contract_value_of("GBPUSD"),
        vike_model::ContractValue::linear(10_000.0),
        "the engine sizes the epic as contracts x units"
    );
    let sizes: Vec<(&str, bool)> =
        snap.positions.iter().map(|p| (p.symbol.as_str(), p.size > 0.0)).collect();
    assert_eq!(sizes, vec![("GBPUSD", true)], "the fill is the new epic's");

    // Fail closed: the mount hands ig's engine the deny flag (`admitting_venues`), and an order the door
    // did not admit (no facts source) never reaches the venue client.
    assert!(crate::server::symbol_admission::admitting_venues().iter().any(|v| v == "ig"));
    let denied = node.send(limit("never", "ig", "AUDUSD", 1.0, 0.7), None);
    assert!(denied.is_err(), "no facts source, no admission: {denied:?}");
    assert!(node.submitted.lock().unwrap().iter().all(|o| o.order_client_id != "never"));
    node.core.shutdown_and_join();
}

/// **ibkr takes a stock, a forex pair or a dated future by its user name (decisions 0234 and 0290), and nothing
/// else**: `AAPL`, `EURUSD`, `ES_261218`, each the operator's kept row. An option is refused by its shape, anything
/// with a character a symbol does not have by the door's reading, and an exchange-qualified or pinned name (the old
/// spellings) and anything else no kept row carries by the list, all BEFORE any lookup (the symbol reaches the
/// gateway's symbol search downstream).
#[test]
fn ibkr_refuses_what_is_not_a_user_name_before_any_lookup() {
    let facts = Table::eth();
    for (bad, word) in [
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("AAPL&x=1", "is not a symbol"),
        ("SPY.SMART.USD.OPT.20251219.500.C", "is an option"),
        ("AAPL.SMART.USD", "carries no row"),
        ("AAPL.NASDAQ.USD", "carries no row"),
        // The overnight session is reached with SMART plus the account's `extended_hours` switch
        // (`crates/bridges/vike-ibkr/CLAUDE.md`), never by naming the exchange: it would be a second
        // spelling of the stock an engine already trades.
        ("AAPL.OVERNIGHT.USD", "carries no row"),
        ("AAPL.SMART.USD.STK", "carries no row"),
        ("AAPL.SMART.USD.STK.0", "carries no row"),
        ("AAPL.SMART.USD.STK.99999999999", "carries no row"),
        ("AAPL.SMART.USD.STK.265598.X", "carries no row"),
        ("A.B.C.SMART.USD", "carries no row"),
        ("A.B.C", "carries no row"),
        ("BRK.", "carries no row"),
        (".B", "carries no row"),
        ("BRK__B", "carries no row"),
        ("AAPL.SMART.US", "carries no row"),
        ("ABCDEFGHIJKLM", "carries no row"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("ibkr", bad, None),
            &[engine("ibkr", "AAPL")],
            Some(&facts),
            true,
        ));
        assert!(why.contains(word), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **An ibkr instrument is its user name, found by the operator's kept row** (decision 0290's O-10): a stock its
/// ticker (`AAPL`), a pair `EURUSD`, a dated future `ES_261218`, in any case; the old names (`AAPL.SMART.USD`,
/// `EUR.USD.IDEALPRO`, `ES.CME.USD.FUT.20261218`, a pinned `AAPL.SMART.USD.STK.265598`) name no row, and an option is
/// refused by its shape.
#[test]
fn ibkr_reads_a_stock_a_pair_and_a_future_by_their_user_names_and_refuses_an_option() {
    let rows = crate::testutil::door_rows();
    let door = |typed: &str| door_user_symbol("ibkr", typed, &rows);
    for (typed, user) in
        [("aapl", "AAPL"), (" MSFT ", "MSFT"), ("eurusd", "EURUSD"), ("es_261218", "ES_261218")]
    {
        assert_eq!(door(typed).as_deref(), Ok(user), "{typed}");
    }
    for old in [
        "AAPL.SMART.USD",
        "EUR.USD.IDEALPRO",
        "ES.CME.USD.FUT.20261218",
        "AAPL.SMART.USD.STK.265598",
    ] {
        let why = door(old).expect_err(old);
        assert!(why.contains("carries no row"), "{old}: {why}");
    }
    let option = door("SPY.SMART.USD.OPT.20261218.500.C").expect_err("an option");
    assert!(option.contains("is an option"), "{option}");
}

/// **One stock is one symbol in an engine**: the engine's own stock, or a leg, typed in another case is that symbol,
/// placed on the engine's spelling with no lookup (decision 0290: one user name in any case).
#[test]
fn ibkr_places_another_case_of_a_stock_the_engine_trades_on_the_engines_spelling() {
    let facts = Table::eth();
    let leg = vike_execution::VenueBlock {
        extra_symbols: vec!["MSFT".into()].into(),
        ..engine("ibkr", "AAPL")
    };
    for (block, typed, own) in [(engine("ibkr", "MSFT"), "msft", "MSFT"), (leg, " msft ", "MSFT")] {
        let (symbol, _) =
            placed(plan_submit_symbol(&submit("ibkr", typed, None), &[block], Some(&facts), true));
        assert_eq!(symbol, own, "{typed}");
    }
    assert!(facts.asked().is_empty(), "no lookup, no slot spent");
}

/// **The ledger is one currency**: a stock is admitted only into an engine whose own contract counts money in
/// the stock's currency, its kept row's (a GBP engine is not admitted a USD stock; a USD one is not admitted a GBP
/// stock; a EUR one not a USD stock), and an engine whose symbol no kept row names, so states no currency, is not
/// admitted anything. Here the account has NO account-wide
/// exposure ceiling (`uncapped`), so it does not convert (decision 0183: an account that converts, a `demo`
/// one with a ceiling, is `ibkr_money_door`'s: the same rows are then looked up, and admitted only with the
/// conversion in the facts).
#[test]
fn ibkr_admits_a_stock_only_in_the_engines_own_currency() {
    let facts = Table::eth().uncapped();
    for (mounted, typed) in
        [("VOD", "AAPL"), ("AAPL", "VOD"), ("SAP", "AAPL"), ("UNLISTED", "MSFT")]
    {
        let why = refused(plan_submit_symbol(
            &submit("ibkr", typed, None),
            &[engine("ibkr", mounted)],
            Some(&facts),
            true,
        ));
        assert!(why.contains("nothing converts"), "{mounted} <- {typed}: {why}");
    }
    assert!(facts.asked().is_empty());
    // A forex engine counts money in its quote currency, its kept row's.
    let (symbol, _) = placed(plan_submit_symbol(
        &submit("ibkr", "msft", None),
        &[engine("ibkr", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "MSFT");
}

/// The engine's own symbol in another case is the engine's own spelling on every tier (this venue answered
/// it before it admitted): placed on the engine's spelling, no lookup.
#[test]
fn ibkr_own_symbols_in_another_case_are_placed_on_the_engines_spelling() {
    let facts = Table::eth();
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        let block = vike_execution::VenueBlock {
            tier: Some(tier),
            ..engine("ibkr", "ES.GLOBEX.USD.FUT.20251219")
        };
        assert_eq!(
            plan_submit_symbol(
                &submit("ibkr", "es.globex.usd.fut.20251219", None),
                &[block],
                Some(&facts),
                true
            ),
            SymbolPlan::Place {
                symbol: "ES.GLOBEX.USD.FUT.20251219".into(),
                admit: None,
                wait: None
            },
            "{tier:?}"
        );
    }
    assert!(facts.asked().is_empty());
}

/// **Without a price lane nothing opens**: a node whose datahub serves no ibkr lane (or whose stream is
/// delayed) has no ibkr price, so an opening order on a new stock is refused naming the feed (the slot is not
/// spent) and a reduce-only order is refused for holding nothing. A paper engine is one book.
#[test]
fn ibkr_without_a_price_feed_opens_nothing_and_a_paper_account_admits_nothing() {
    let mut facts = Table::eth();
    facts.price = Price::Failing;
    let why = refused(plan_submit_symbol(
        &submit("ibkr", "msft", None),
        &[engine("ibkr", "AAPL")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("no price feed for `MSFT`"), "{why}");
    let why = refused(plan_submit_symbol(
        &market("ibkr", "msft", true),
        &[engine("ibkr", "AAPL")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("reduce-only"), "{why}");
    let paper =
        vike_execution::VenueBlock { tier: Some(AccountTier::Paper), ..engine("ibkr", "AAPL") };
    let why =
        refused(plan_submit_symbol(&submit("ibkr", "msft", None), &[paper], Some(&facts), true));
    assert!(why.contains("paper account trades only the symbol it was started on"), "{why}");
}

/// **A venue with no price handle costs no lookup**: ibkr's lookup is a real data connect (budgeted per hour,
/// shared with the data feed), so an opening order on a new stock is refused for the missing price source
/// BEFORE the facts source is asked, in the same words, on the commit and on the Preview alike.
#[test]
fn a_venue_with_no_price_handle_refuses_before_any_facts_lookup() {
    let mut facts = Table::eth();
    facts.unpriced = vec!["ibkr"];
    for commit in [true, false] {
        for typed in ["msft", "tsla", "nvda"] {
            let why = refused(plan_submit_symbol(
                &submit("ibkr", typed, None),
                &[engine("ibkr", "AAPL")],
                Some(&facts),
                commit,
            ));
            assert!(
                why.contains(&format!(
                    "no price feed for `{}`: this node runs no price feed for ibkr",
                    typed.to_uppercase()
                )),
                "{why}"
            );
        }
    }
    assert!(facts.asked().is_empty(), "no lookup may be spent: {:?}", facts.asked());
    // A venue WITH a handle still looks up (the guard is the missing handle, not the venue).
    facts.unpriced = vec!["bybit"];
    placed(plan_submit_symbol(
        &submit("ibkr", "msft", None),
        &[engine("ibkr", "AAPL")],
        Some(&facts),
        true,
    ));
    assert_eq!(facts.asked(), ["ibkr/MSFT"]);
}

/// A dukascopy order of the sidecar's smallest size (1000 units), priced, so the grid check passes.
fn dukascopy_order(symbol: &str) -> CommandWire {
    let CommandWire::Submit(mut req) = submit("dukascopy", symbol, None) else { unreachable!() };
    req.qty = 1000.0;
    CommandWire::Submit(req)
}

/// **dukascopy takes a currency or metal pair by its user name, in any case, in the engine's own
/// money** (an engine mounted on a pair or on a metal): the admission carries the table's grid for THAT pair, one
/// lookup, on its user name.
#[test]
fn dukascopy_admits_a_pair_in_the_engines_money_on_its_own_grid() {
    for (mounted, typed, canonical, tick) in [
        ("EURUSD", "gbpusd", "GBPUSD", 0.00001),
        ("EURUSD", " xauusd ", "XAUUSD", 0.001),
        ("XAUUSD", "GBPUSD", "GBPUSD", 0.00001),
    ] {
        let facts = Table::eth();
        let (symbol, admit) = placed(plan_submit_symbol(
            &dukascopy_order(typed),
            &[engine("dukascopy", mounted)],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, canonical, "{typed}");
        let admit = admit.expect("admitted first");
        assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(tick), Some(1000.0)));
        assert_eq!(admit.contract_value, None, "linear: a unit of quantity is a unit of the base");
        assert_eq!(facts.asked(), vec![format!("dukascopy/{canonical}")], "asked once, bare");
    }
}

/// A name with a character no symbol has is refused BY NAME before any lookup (it reaches the sidecar's JSON
/// line), and so is a name no kept row carries (the SDK's own `EUR/USD` or `COPPER.CMD/USD`); a kept instrument that
/// is not a pair (the share CFD `AAPL.CFD`) is passed to the gatewayhub's table by its user name, which refuses a CFD
/// and an unknown name in its own words.
#[test]
fn dukascopy_refuses_what_is_not_an_instrument_name_and_leaves_the_rest_to_the_table() {
    let facts = Table::eth();
    let roster = [engine("dukascopy", "EURUSD")];
    let long = "A".repeat(33);
    for (bad, word) in [
        ("", "is not a symbol"),
        ("  ", "is not a symbol"),
        ("EUR USD", "is not a symbol"),
        ("EURUSD&x=1", "is not a symbol"),
        ("EUR\"USD", "is not a symbol"),
        ("EUR_USD", "carries no row"),
        ("EUR/USD", "carries no row"),
        ("copper.cmd/usd", "carries no row"),
        (long.as_str(), "carries no row"),
    ] {
        let why = refused(plan_submit_symbol(&dukascopy_order(bad), &roster, Some(&facts), true));
        assert!(why.contains(word), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
    let why =
        refused(plan_submit_symbol(&dukascopy_order("aapl.cfd"), &roster, Some(&facts), true));
    assert!(why.contains("cannot admit `AAPL.CFD`"), "{why}");
    assert_eq!(facts.asked(), ["dukascopy/AAPL.CFD"], "the table is asked, by the user name");
}

/// **The ledger is one currency** for an account that does not convert: a pair is admitted only into an engine
/// whose own pair is quoted in the same money, and an engine whose symbol is not a pair is admitted nothing.
/// (An account that converts a quote currency, decision 0183, is `dukascopy_money_door.rs`'s: this fence is
/// what it relaxes, only for a `demo` account with an account-wide ceiling armed, and only with the facts'
/// currency and rate; the table here is `uncapped`, which is the other half of that rule.)
#[test]
fn dukascopy_admits_a_pair_only_in_the_engines_own_money() {
    let facts = Table::eth().uncapped();
    for (mounted, typed, cause) in [
        ("EURGBP", "gbpusd", "counts its money in GBP"),
        ("EURUSD", "eurgbp", "quoted in GBP"),
        ("AAPLUSUSD", "gbpusd", "an unknown currency"),
    ] {
        let why = refused(plan_submit_symbol(
            &dukascopy_order(typed),
            &[engine("dukascopy", mounted)],
            Some(&facts),
            true,
        ));
        assert!(why.contains("nothing converts") && why.contains(cause), "{mounted}: {why}");
    }
    assert!(facts.asked().is_empty(), "refused before any lookup, no slot spent");
}

/// **The engine's own pair in another case stays the engine's own, on every tier** (decision 0290: one user name
/// in any case), so the order is placed on the engine's spelling with no lookup and no second symbol, paper
/// included. An admitted leg is the same.
#[test]
fn dukascopy_own_pair_in_another_spelling_is_the_engines_own_on_every_tier() {
    let facts = Table::eth();
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        for (mounted, typed, want) in
            [("EURUSD", "eurusd", "EURUSD"), ("EURUSD", " EurUsd ", "EURUSD")]
        {
            let block =
                vike_execution::VenueBlock { tier: Some(tier), ..engine("dukascopy", mounted) };
            assert_eq!(
                plan_submit_symbol(&dukascopy_order(typed), &[block], Some(&facts), true),
                SymbolPlan::Place { symbol: want.into(), admit: None, wait: None },
                "{tier:?}: {mounted} <- {typed}"
            );
        }
    }
    let leg = vike_execution::VenueBlock {
        extra_symbols: vec!["GBPUSD".into()].into(),
        ..engine("dukascopy", "EURUSD")
    };
    assert_eq!(
        plan_submit_symbol(&dukascopy_order("gbpusd"), &[leg], Some(&facts), true),
        SymbolPlan::Place { symbol: "GBPUSD".into(), admit: None, wait: None },
        "an admitted leg is placed as the engine spells it"
    );
    assert!(facts.asked().is_empty());
}

/// **A pair is its user name on both pair venues** (decision 0290): the venue's slashed name is its bridge's wire, so
/// `EUR/USD` names no row at the door, and an admitted leg typed in another case is that leg, no second symbol.
#[test]
fn a_pair_is_its_user_name_and_its_slashed_wire_name_names_no_row() {
    let rows = crate::testutil::door_rows();
    for venue in ["dukascopy", "fxcm"] {
        assert_eq!(door_user_symbol(venue, " eurusd ", &rows).as_deref(), Ok("EURUSD"), "{venue}");
        let why = door_user_symbol(venue, "EUR/USD", &rows).expect_err("the wire name");
        assert!(why.contains("carries no row"), "{venue}: {why}");
    }
    let facts = Table::eth();
    let leg = vike_execution::VenueBlock {
        extra_symbols: vec!["GBPUSD".into()].into(),
        ..engine("fxcm", "EURUSD")
    };
    let CommandWire::Submit(mut req) = submit("fxcm", "gbpusd", None) else { unreachable!() };
    req.qty = 1000.0;
    assert_eq!(
        plan_submit_symbol(&CommandWire::Submit(req), &[leg], Some(&facts), true),
        SymbolPlan::Place { symbol: "GBPUSD".into(), admit: None, wait: None },
        "an admitted leg typed in another case is that leg, no second symbol"
    );
    assert!(facts.asked().is_empty());
}

/// **Without a price nothing opens**: while the door has no dukascopy price (the datahub advertises no dukascopy
/// lane, no armed `demo` sidecar holds the ticks, or its jar predates the tick stream; the tradehub otherwise
/// prices a new pair from the sidecar's ticks), an opening order on a new pair is refused naming the feed with
/// the slot unspent, a reduce-only order for holding nothing, and a paper engine is one book.
#[test]
fn dukascopy_without_a_price_feed_opens_nothing_and_a_paper_account_admits_nothing() {
    let mut facts = Table::eth();
    facts.price = Price::Failing;
    let roster = [engine("dukascopy", "EURUSD")];
    let why = refused(plan_submit_symbol(&dukascopy_order("gbpusd"), &roster, Some(&facts), true));
    assert!(why.contains("no price feed for `GBPUSD`"), "{why}");
    let CommandWire::Submit(mut close) = dukascopy_order("gbpusd") else { unreachable!() };
    close.reduce_only = true;
    let why = refused(plan_submit_symbol(&CommandWire::Submit(close), &roster, Some(&facts), true));
    assert!(why.contains("reduce-only"), "{why}");
    let paper = vike_execution::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine("dukascopy", "EURUSD")
    };
    let why = refused(plan_submit_symbol(&dukascopy_order("gbpusd"), &[paper], Some(&facts), true));
    assert!(why.contains("paper account trades only the symbol it was started on"), "{why}");
}

/// A quantity that rounds to nothing on the sidecar's 1000-unit lot is refused from the facts' grid with no slot spent
/// (999 goes out as 1000, as the gate rounds it).
#[test]
fn dukascopy_refuses_a_quantity_under_the_sidecars_floor_before_a_slot_is_spent() {
    let facts = Table::eth();
    let CommandWire::Submit(mut small) = dukascopy_order("gbpusd") else { unreachable!() };
    small.qty = 400.0;
    let why = refused(plan_submit_symbol(
        &CommandWire::Submit(small),
        &[engine("dukascopy", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("below one lot (1000)") && why.contains("no symbol slot"), "{why}");
}

// The lane twin's own symbol is priced in the twin ENGINE: the production sink chain into a real core.
#[cfg(test)]
#[path = "lane_twin_pricing.rs"]
mod lane_twin_pricing;

// A node that is not Active admits no symbol (its commit is a write on the exchange's account).
#[cfg(test)]
#[path = "admission_while_halted.rs"]
mod admission_while_halted;

// Which engine IS the lane twin (tier, the node's word on a real account), and a twin kept up only to be closed.
#[cfg(test)]
#[path = "lane_twin_identity.rs"]
mod lane_twin_identity;

// A venue-scoped command that names no account reaches exactly one account or does not go (decision 0254).
#[cfg(test)]
#[path = "one_account_door.rs"]
mod one_account_door;
#[path = "side_door.rs"]
mod side_door;

// A preview answers what the send will answer, over every wire verb and the shapes of the one-account table.
#[cfg(test)]
#[path = "preview_door.rs"]
mod preview_door;

// What a client's preview says of an order, held to what the door does with it: its guardrail and its lane.
#[cfg(test)]
#[path = "client_door.rs"]
mod client_door;

// The give-back of an admitted symbol (decision 0205): the plan against a published block, and the verb through a real core.
#[cfg(test)]
#[path = "unadmit_door.rs"]
mod unadmit_door;
