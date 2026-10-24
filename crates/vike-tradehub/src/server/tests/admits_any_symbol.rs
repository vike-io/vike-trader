//! The control door lets an order name ANY symbol of its exchange (decision 0173): bybit, binance, aster,
//! okx, hyperliquid, deribit, alpaca, ctrader, oanda, ig, ibkr and dukascopy admit the symbol into the
//! account's engine and place the order on it (deribit: a perpetual of the engine's own money model only;
//! alpaca: an equity or a crypto pair; ctrader: the broker's name, case kept, and only a forex pair quoted
//! in the deposit currency passes the gatewayhub's facts; oanda: an instrument spelled as the engine spells
//! it, `GBPUSD` or `SPX500_USD`, whose quote currency is the account's, which the gatewayhub's facts
//! enforce; ig: an epic, spelled upper case, whose facts the gatewayhub admits for CURRENCIES only;
//! dukascopy: a currency or metal pair quoted in the engine's money); every refusal names its cause.
//!
//! Two layers. The PLAN (`plan_submit_symbol`) is a pure function of the published block and a
//! facts source, so each rule has a row. The END-TO-END tests drive `accept_command` into a REAL
//! core over a scripted venue client: the order reaches the venue client on the symbol the operator
//! named, snapped on THAT symbol's grid, and its fill folds into THAT symbol's position.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::control::{ControlLimits, ControlLimitsConfig, accept_command};
use crate::server::symbol_admission::{
    PriceWait, SYMBOL_RULES, SymbolFacts, SymbolPlan, SymbolRule, lane_addressed,
    plan_submit_symbol, submit_symbol_refusal,
};
use vike_exec::{
    Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, SymbolAdmission,
};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, RiskLimits, SymbolGrid};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

/// The venues whose adapter signs on the ORDER's symbol.
const ADMITS: [&str; 13] = [
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
];
/// The venues whose adapter still signs the MOUNTED symbol whatever the order names (none today:
/// deribit's was the last row).
const MOUNTED_ONLY: [&str; 0] = [];
/// The venues whose adapter reads the order's own symbol and whose engine judges it soundly enough
/// (live accounts of polymarket run on this row: it must not refuse a symbol; oanda's was the other one
/// until it became `Admits`).
const PER_ORDER: [&str; 1] = ["polymarket"];
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
    let of = |want: fn(&SymbolRule) -> bool| {
        let mut v: Vec<&str> =
            SYMBOL_RULES.iter().filter(|(_, r)| want(r)).map(|(venue, _)| *venue).collect();
        v.sort_unstable();
        v
    };
    let sorted = |mut v: Vec<&'static str>| {
        v.sort_unstable();
        v
    };
    assert_eq!(of(|r| matches!(r, SymbolRule::Admits(_))), sorted(ADMITS.to_vec()));
    assert_eq!(of(|r| matches!(r, SymbolRule::MountedOnly(_))), sorted(MOUNTED_ONLY.to_vec()));
    assert_eq!(of(|r| matches!(r, SymbolRule::PerOrder)), sorted(PER_ORDER.to_vec()));
    assert_eq!(
        of(|r| matches!(r, SymbolRule::PerOrderUnsafe(_))),
        sorted(PER_ORDER_UNSAFE.to_vec())
    );
    assert_eq!(SYMBOL_RULES.len(), roster.len(), "one row per roster venue");
}

/// **An `Admits` venue's lease claims what its daemon client reads, and okx's and dukascopy's read
/// nothing.** (fxcm's reconcile client was the third until its price lane made an admitted pair reachable: it now
/// parses its whole-account Orders and Trades snapshots once per told pair, so a lost-reply market order or a
/// restart with a position open on an admitted pair is booked by reconcile, not left to the engine's flat view.)
/// The node mirrors the symbols it told the daemon (`RemoteReconClient::admitted_symbols`) only
/// for the venues in `RECON_READS_ADMITTED`; any other claims none, so the restore planner holds its orders
/// on an admitted symbol. A new `Admits` venue fails here until someone decides which side it is on.
/// dukascopy's gap is not a filter but an absence: its positions snapshot already spans every symbol the
/// sidecar reports, yet the venue hands the node NO reconcile client at all (`Wiring::SidecarExecNoRecon`).
#[test]
fn an_admits_venue_is_either_read_by_its_recon_client_or_a_known_gap() {
    use vike_gatewayhub_client::trade::RECON_READS_ADMITTED;
    let mut admits: Vec<&str> = ADMITS.to_vec();
    admits.retain(|v| !RECON_READS_ADMITTED.contains(v));
    assert_eq!(
        admits,
        ["okx", "dukascopy"],
        "okx's recon client reads the mounted instrument only; dukascopy has no reconcile client (fxcm's reads \
         every admitted pair since its price lane: `recon_client.rs`'s `every_pass_reads_the_admitted_pairs_...`)"
    );
    assert!(RECON_READS_ADMITTED.iter().all(|v| ADMITS.contains(v)));
}

fn submit(venue: &str, symbol: &str, account: Option<&str>) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c-1".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

fn market(venue: &str, symbol: &str, reduce_only: bool) -> WireCommand {
    let WireCommand::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.order_type = "market".into();
    req.price = None;
    req.reduce_only = reduce_only;
    WireCommand::Submit(req)
}

/// A published engine block: a real account (`demo`) unless the test says otherwise.
fn engine(route_key: &str, symbol: &str) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: route_key.split('#').next().unwrap_or(route_key).into(),
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
}

impl Table {
    fn eth() -> Self {
        Table {
            inverse: vec!["ETH-PERPETUAL", "SOL-PERPETUAL"],
            rows: vec![
                ("deribit", "ETH-PERPETUAL", Some(0.05), Some(1.0)),
                ("deribit", "SOL-PERPETUAL", Some(0.01), Some(1.0)),
                ("deribit", "ETH_USDC-PERPETUAL", Some(0.01), Some(0.0001)),
                ("deribit", "BTC_USDC-PERPETUAL", Some(0.1), Some(0.0001)),
                ("bybit", "ETHUSDT", Some(0.01), Some(0.01)),
                ("binance", "ETHUSDT", Some(0.01), Some(0.0001)),
                ("binance", "ETHUSDT.P", Some(0.01), Some(0.001)),
                ("aster", "ETHUSDT.P", Some(0.01), Some(0.001)),
                ("aster", "ETHUSDT", Some(0.01), Some(0.0001)),
                ("bybit", "NOTICKUSDT", None, Some(0.01)),
                ("bybit", "NOLOTUSDT", Some(0.01), None),
                ("okx", "ETH-USDT-SWAP", Some(0.01), Some(0.001)),
                ("hyperliquid", "kPEPE", Some(0.001), Some(0.01)),
                ("alpaca", "MSFT", Some(0.01), Some(1.0)),
                ("alpaca", "BRK.B", Some(0.01), Some(1.0)),
                ("alpaca", "BTC/USD", Some(1.0), Some(0.000000001)),
                ("ctrader", "GBPUSD", Some(0.00001), Some(0.01)),
                ("ctrader", "Wheat_H6", Some(0.01), Some(1.0)),
                ("oanda", "GBPUSD", Some(0.00001), Some(1.0)),
                ("oanda", "AUDUSD", Some(0.00001), Some(1.0)),
                ("oanda", "SPX500_USD", Some(0.1), Some(0.1)),
                ("ig", "CS.D.GBPUSD.MINI.IP", Some(0.0001), Some(0.1)),
                ("ig", "CS.D.AUDUSD.MINI.IP", Some(0.0001), Some(0.1)),
                ("ibkr", "MSFT.SMART.USD", Some(0.01), Some(1.0)),
                ("ibkr", "TSLA.SMART.USD", Some(0.01), Some(1.0)),
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

impl SymbolFacts for Table {
    fn generation(&self, _: &str) -> u64 {
        0
    }
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
    ) -> Result<SymbolAdmission, String> {
        self.asked.lock().unwrap().push(format!("{route_key}/{symbol}"));
        let Some((_, _, tick, lot)) =
            self.rows.iter().find(|(v, s, _, _)| *v == venue && *s == symbol)
        else {
            return Err(format!("{venue}: no instrument {symbol} in the venue's instrument list"));
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
            grid: SymbolGrid { tick_size: *tick, lot_size: *lot, ..SymbolGrid::default() },
            contract_value,
            initial_mark: None,
            quote_ccy: None,
            quote_rate: None,
            rate_as_of_ms: None,
            im_requirement: None,
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
            Price::Pending => Ok(None),
            Price::Failing => Err(format!("{venue} has no price feed for {symbol}")),
        }
    }

    fn admitted(&self, _: &str, _: &str, symbol: &str) {
        self.events.lock().unwrap().push(format!("admitted:{symbol}"));
    }

    fn release(&self, _: &str, _: &str, symbol: &str) {
        self.events.lock().unwrap().push(format!("release:{symbol}"));
    }

    fn refresh(&self, _: &str, _: &str, _: &str) -> Option<SymbolAdmission> {
        self.refresh.lock().unwrap().take()
    }

    fn waits(&self) -> bool {
        self.waits
    }

    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }

    fn derived_lane_twin(&self, route_key: &str) -> bool {
        self.derived.contains(&route_key)
    }

    fn confirmed_leverage(&self, _: &str, _: &str) -> Option<f64> {
        self.confirm
    }
}

fn placed(plan: SymbolPlan) -> (String, Option<Box<SymbolAdmission>>) {
    match plan {
        SymbolPlan::Place { symbol, admit, .. } => (symbol, admit),
        other => panic!("expected a placement, got {other:?}"),
    }
}

fn refused(plan: SymbolPlan) -> String {
    match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// **The headline, on every finished venue**: a symbol the engine was not started on is spelled the
/// venue's way, its facts are looked up, and the plan is the admission followed by the order.
#[test]
fn a_foreign_symbol_is_admitted_in_the_venues_spelling_with_its_own_grid() {
    // (venue, the engine's symbol, what the operator typed, the canonical spelling, tick, lot)
    let rows = [
        ("bybit", "BTCUSDT", "ethusdt", "ETHUSDT", 0.01, 0.01),
        ("bybit", "BTCUSDT", "ETHUSDT.P", "ETHUSDT", 0.01, 0.01),
        ("binance", "BTCUSDT", "ethusdt", "ETHUSDT", 0.01, 0.0001),
        ("binance", "BTCUSDT.P", "ethusdt.p", "ETHUSDT.P", 0.01, 0.001),
        ("aster", "BTCUSDT.P", "ETHUSDT.P", "ETHUSDT.P", 0.01, 0.001),
        ("okx", "BTC-USDT-SWAP", "eth-usdt-swap", "ETH-USDT-SWAP", 0.01, 0.001),
        ("hyperliquid", "BTC", "kPEPE", "kPEPE", 0.001, 0.01),
        ("alpaca", "AAPL", "msft", "MSFT", 0.01, 1.0),
        ("alpaca", "AAPL", " brk.b ", "BRK.B", 0.01, 1.0),
        ("alpaca", "AAPL", "btc/usd", "BTC/USD", 1.0, 0.000000001),
        ("alpaca", "BTC/USD", "MSFT", "MSFT", 0.01, 1.0),
        // The broker's name keeps its case, and the door never upper-cases it.
        ("ctrader", "EURUSD", "GBPUSD", "GBPUSD", 0.00001, 0.01),
        ("ctrader", "EURUSD", " Wheat_H6 ", "Wheat_H6", 0.01, 1.0),
        ("oanda", "EURUSD", "gbpusd", "GBPUSD", 0.00001, 1.0),
        ("oanda", "EURUSD", " GBP_USD ", "GBPUSD", 0.00001, 1.0),
        ("oanda", "EURUSD", "spx500_usd", "SPX500_USD", 0.1, 0.1),
        ("oanda", "SPX500_USD", "audusd", "AUDUSD", 0.00001, 1.0),
        ("ig", "CS.D.EURUSD.MINI.IP", "cs.d.gbpusd.mini.ip", "CS.D.GBPUSD.MINI.IP", 0.0001, 0.1),
        ("ig", "CS.D.EURUSD.MINI.IP", " CS.D.AUDUSD.MINI.IP ", "CS.D.AUDUSD.MINI.IP", 0.0001, 0.1),
        ("ibkr", "AAPL.SMART.USD", "msft", "MSFT.SMART.USD", 0.01, 1.0),
        ("ibkr", "AAPL.SMART.USD", " tsla.smart.usd ", "TSLA.SMART.USD", 0.01, 1.0),
        ("ibkr", "EUR.IDEALPRO.USD", "MSFT", "MSFT.SMART.USD", 0.01, 1.0),
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
        let leg = vike_exec::VenueBlock {
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

/// **An order on the engine's OWN symbol asks the source to hold its price lane, and the plan does not change**
/// (decision 0197): on every `Admits` venue, for a `demo` and a `live` engine, a limit order and a market order
/// (a first MARKET order is judged as it always was), the plan is still `Pass`; no quote is asked, no wait is
/// planned, nothing is refused when the lane cannot be had, and the source is asked once per order.
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
                let block = vike_exec::VenueBlock { tier: Some(tier), ..engine(venue, mounted) };
                for cmd in [submit(venue, mounted, None), market(venue, mounted, false)] {
                    assert_eq!(
                        plan_submit_symbol(&cmd, std::slice::from_ref(&block), Some(&facts), true),
                        SymbolPlan::Pass,
                        "{venue} {tier:?}: the plan is as it was whatever the quote"
                    );
                }
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

/// **Only a committing surface for a real engine asks**: a Preview reads and subscribes nothing, a `paper`
/// engine has no venue to price, an engine that names no symbol has nothing to hold, a surface with no source
/// asks nobody, and a DECLARED leg or a foreign symbol is the old price rule's, not this one.
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
        vike_exec::VenueBlock { tier: Some(AccountTier::Paper), ..engine("bybit", "BTCUSDT") };
    assert_eq!(plan_submit_symbol(&own, &[paper], Some(&facts), true), SymbolPlan::Pass);
    let unknown = vike_exec::VenueBlock { tier: None, ..engine("bybit", "BTCUSDT") };
    assert_eq!(plan_submit_symbol(&own, &[unknown], Some(&facts), true), SymbolPlan::Pass);
    let unmounted = engine("bybit", "");
    assert_eq!(plan_submit_symbol(&own, &[unmounted], Some(&facts), true), SymbolPlan::Pass);
    assert_eq!(
        plan_submit_symbol(&own, &[demo], None, true),
        SymbolPlan::Pass,
        "no source, no ask"
    );
    assert!(facts.holds().is_empty(), "paper, no tier, no symbol: nobody is asked");

    // A foreign symbol is admitted and priced by the old rule; it holds no OWN lane.
    let foreign = submit("bybit", "ETHUSDT", None);
    let block = engine("bybit", "BTCUSDT");
    let _ = plan_submit_symbol(&foreign, &[block], Some(&facts), true);
    assert!(facts.holds().is_empty(), "the engine's own symbol only");
}

/// The Telegram channel's source forwards the ask (it never waits: the wrapped source opens the lane off the
/// poller's thread), so a chat order on the engine's own symbol keeps its lane like the control door's.
#[test]
fn the_telegram_source_forwards_the_lane_ask() {
    let table = Arc::new(Table::eth());
    let source = crate::server::symbol_admission::FailFast(table.clone());
    assert_eq!(
        plan_submit_symbol(
            &submit("bybit", "BTCUSDT", None),
            &[engine("bybit", "BTCUSDT")],
            Some(&source),
            true
        ),
        SymbolPlan::Pass
    );
    assert_eq!(table.holds(), ["bybit/BTCUSDT".to_string()]);
}

/// **A limit order on the engine's own symbol is accepted with no quote at all, end to end** (a market the
/// venue keeps closed still takes a resting limit order), through the ONE acceptance path into a real core: it
/// reaches the venue client, the source is asked to hold the lane, and no price was asked or awaited.
#[test]
fn a_limit_order_on_the_own_symbol_is_accepted_with_no_quote_and_asks_for_the_lane() {
    let facts = Table::eth().with_price(Price::Failing);
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let coid = node
        .send(limit("own-limit", "bybit", "BTCUSDT", 0.01, 60_000.0), Some(&facts))
        .expect("a limit order on the engine's own symbol is accepted without a quote");
    assert_eq!(coid, "own-limit");
    node.venue_saw(&coid);
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
            let leg = vike_exec::VenueBlock {
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
            let unmounted = vike_exec::VenueBlock { tier: Some(tier), ..engine(venue, "") };
            assert_eq!(
                plan_submit_symbol(&market(venue, "ANY", false), &[unmounted], None, true),
                SymbolPlan::Pass,
                "{venue} {tier:?}: an engine mounted on no symbol (polymarket) judges every token"
            );
        }
    }
}

/// **A paper account is ONE book on every unsafe per-order venue** (and on oanda, an `Admits` venue
/// since the any-symbol step): `PaperExecutionClient` stamps its own symbol on every fill, so a foreign
/// symbol would fill on the mounted symbol's bars and be booked on it. The door refuses it with the
/// words the `Admits` venues use.
#[test]
fn a_paper_account_refuses_a_foreign_symbol_on_every_per_order_venue() {
    let paper = |venue: &str| vike_exec::VenueBlock {
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
            submit_symbol_refusal(&submit(venue, "FOREIGN", None), &[paper(venue)], None),
            Some(why),
            "{venue}: the Preview prints the same"
        );
    }
    // Polymarket is exempt (its engine judges every token; live accounts run on this row): a paper
    // engine with a mounted token and a priceless market order on another token passes, as before.
    let poly = vike_exec::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine("polymarket", "MOUNTED_TOKEN")
    };
    for cmd in
        [submit("polymarket", "OTHER_TOKEN", None), market("polymarket", "OTHER_TOKEN", false)]
    {
        assert_eq!(
            plan_submit_symbol(&cmd, std::slice::from_ref(&poly), None, true),
            SymbolPlan::Pass
        );
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

/// **oanda's spelling**: an instrument is a six-letter FX pair (with or without its underscore) or a CFD
/// name with its single underscore; anything else is refused by name before any lookup, because the
/// symbol reaches a URL path and a JSON body downstream. And one instrument is one symbol in an engine:
/// `GBP_USD` is `GBPUSD`, so an engine mounted on either is never admitted the other.
#[test]
fn oanda_refuses_what_is_not_an_instrument_and_the_other_spelling_of_one_the_engine_trades() {
    let facts = Table::eth();
    for bad in [
        "",
        "  ",
        "GBP",
        "GBP-USD",
        "GBP USD",
        "GBP_USD_X",
        "_USD",
        "GBP_",
        "GBPUS1",
        "A\"}",
        "GBP/USD",
    ] {
        let why = refused(plan_submit_symbol(
            &submit("oanda", bad, None),
            &[engine("oanda", "EURUSD")],
            Some(&facts),
            true,
        ));
        assert!(
            why.contains("is not an OANDA instrument") && why.contains("GBPUSD"),
            "{bad:?}: {why}"
        );
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");

    let leg = vike_exec::VenueBlock {
        extra_symbols: vec!["AUDUSD".into()].into(),
        ..engine("oanda", "EURUSD")
    };
    for (block, typed, own) in [
        (engine("oanda", "EUR_USD"), "eurusd", "EUR_USD"),
        (engine("oanda", "EURUSD"), "EUR_USD", "EURUSD"),
        (leg, "aud_usd", "AUDUSD"),
    ] {
        // `EUR_USD` typed against an engine mounted on `EURUSD` IS that engine's symbol (canonicalised),
        // so only the mounted-on-underscore and the leg rows are refusals.
        let plan = plan_submit_symbol(&submit("oanda", typed, None), &[block], Some(&facts), true);
        match own {
            "EURUSD" => assert_eq!(
                plan,
                SymbolPlan::Place { symbol: "EURUSD".into(), admit: None, wait: None },
                "{typed}: the engine's own instrument, in its own spelling"
            ),
            "AUDUSD" => assert!(
                matches!(&plan, SymbolPlan::Place { symbol, admit: None, .. } if symbol == "AUDUSD"),
                "{typed}: a declared leg is traded already: {plan:?}"
            ),
            _ => {
                let why = refused(plan);
                assert!(why.contains(&format!("already trades as `{own}`")), "{typed}: {why}");
            }
        }
    }
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
        WireCommand::Cancel("c-1".into()),
        WireCommand::Flatten { venue: "bybit".into(), symbol: "ETHUSDT".into(), account: None },
        WireCommand::MassCancel {
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
fn deribit_engine(symbol: &str, inverse: bool) -> vike_exec::VenueBlock {
    let mut block = engine("deribit", symbol);
    if inverse {
        block.contract_values = Arc::new(indexmap::IndexMap::from([(
            symbol.to_string(),
            vike_model::ContractValue::inverse(1.0),
        )]));
    }
    block
}

/// **deribit admits a perpetual of the engine's own money model**, in the venue's own spelling and on
/// its own grid: an INVERSE engine takes another inverse perpetual (the admission carries the inverse
/// contract value), a LINEAR one another linear perpetual (the default value).
#[test]
fn deribit_admits_a_perpetual_of_the_engines_money_model() {
    let facts = Table::eth();
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("deribit", "eth-perpetual", None),
        &[deribit_engine("BTC-PERPETUAL", true)],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETH-PERPETUAL");
    let admit = admit.expect("admitted first");
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(0.05), Some(1.0)));
    assert_eq!(admit.contract_value, Some(vike_model::ContractValue::inverse(1.0)));

    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("deribit", "eth_usdc-perpetual", None),
        &[deribit_engine("BTC_USDC-PERPETUAL", false)],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "ETH_USDC-PERPETUAL");
    let admit = admit.expect("admitted first");
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(0.01), Some(0.0001)));
    assert_eq!(admit.contract_value, None, "linear is the default");
    assert_eq!(facts.asked(), ["deribit/ETH-PERPETUAL", "deribit/ETH_USDC-PERPETUAL"]);
}

/// **The other money model is refused BY NAME**, naming both instruments and both models, and a
/// Preview prints exactly that. Nothing is admitted.
#[test]
fn deribit_refuses_the_other_money_model_by_name() {
    let facts = Table::eth();
    let inverse_engine = [deribit_engine("BTC-PERPETUAL", true)];
    let why = refused(plan_submit_symbol(
        &submit("deribit", "ETH_USDC-PERPETUAL", None),
        &inverse_engine,
        Some(&facts),
        true,
    ));
    assert!(why.contains("`ETH_USDC-PERPETUAL`") && why.contains("`BTC-PERPETUAL`"), "{why}");
    assert!(why.contains("LINEAR") && why.contains("INVERSE"), "{why}");
    assert!(why.contains("one account trades one money model"), "{why}");
    assert!(why.contains("REFUSED"), "says what happened: {why}");
    assert_eq!(
        submit_symbol_refusal(
            &submit("deribit", "ETH_USDC-PERPETUAL", None),
            &inverse_engine,
            Some(&facts)
        ),
        Some(why)
    );
    let linear_engine = [deribit_engine("BTC_USDC-PERPETUAL", false)];
    let why = refused(plan_submit_symbol(
        &submit("deribit", "ETH-PERPETUAL", None),
        &linear_engine,
        Some(&facts),
        true,
    ));
    assert!(why.contains("INVERSE") && why.contains("mounted on `BTC_USDC-PERPETUAL`"), "{why}");
}

/// **Only a PERPETUAL is a symbol of this adapter**: a dated future, an option, a spot pair, a bare
/// symbol and anything with a character no instrument has are each refused by name BEFORE any lookup
/// (the name reaches a JSON-RPC frame downstream).
#[test]
fn deribit_refuses_anything_but_a_perpetual_by_name_before_any_lookup() {
    let facts = Table::eth();
    let roster = [deribit_engine("BTC-PERPETUAL", true)];
    for (typed, what) in [
        ("BTC-27MAR26", "dated future"),
        ("BTC-27MAR26-60000-C", "option"),
        ("BTC_USDC", "spot pair"),
        ("ETHUSDT", "spot pair or a bare symbol"),
        ("ETH-PERPETUAL\"}", "not a deribit instrument name"),
        ("ETH PERP", "not a deribit instrument name"),
        ("A-B-C-D-E-F", "not a deribit perpetual"),
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

/// An engine mounted on anything but a perpetual (an option, a dated future) adds no instrument.
#[test]
fn a_deribit_engine_not_mounted_on_a_perpetual_adds_nothing() {
    let facts = Table::eth();
    let roster = [deribit_engine("BTC-27MAR26-60000-C", false)];
    let why = refused(plan_submit_symbol(
        &submit("deribit", "ETH-PERPETUAL", None),
        &roster,
        Some(&facts),
        true,
    ));
    assert!(why.contains("not a perpetual") && why.contains("BTC-27MAR26-60000-C"), "{why}");
}

/// **okx takes the venue's own instId and nothing else.** Only `[A-Z0-9]+-[A-Z0-9]+-SWAP` is a
/// symbol of the SWAP lane this adapter trades; what else a trader may type is refused BY NAME at the
/// door, before any lookup: a spot id, a dated future, an option, and the other venues' spellings
/// (okx's engine symbol is the instId, and `BTCUSDT` / `BTCUSDT.P` are not okx names: nothing is
/// translated).
#[test]
fn okx_refuses_by_name_what_is_not_a_swap_instid() {
    let facts = Table::eth();
    let on = |symbol: &str| {
        refused(plan_submit_symbol(
            &submit("okx", symbol, None),
            &[engine("okx", "BTC-USDT-SWAP")],
            Some(&facts),
            true,
        ))
    };
    for (typed, word) in [
        ("BTC-USDT", "spot"),
        ("btc-usdt", "spot"),
        ("BTC-USDT-250926", "dated"),
        ("BTC-USD-250926-60000-C", "option"),
        ("BTCUSDT.P", "not an okx"),
        ("BTCUSDT", "not an okx"),
        ("ETH-USDT-SWAP-X", "not an okx"),
        ("-USDT-SWAP", "not an okx"),
        ("ETH_USDT-SWAP", "not an okx"),
        ("", "not an okx"),
    ] {
        let why = on(typed);
        assert!(why.contains(word), "{typed:?}: names {word:?}: {why}");
        assert!(why.contains("SWAP"), "{typed:?}: says what an okx symbol looks like: {why}");
        assert!(why.contains("REFUSED"), "{typed:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a name the door refuses is never looked up");
    // The native id, in any case, is admitted.
    for typed in ["ETH-USDT-SWAP", "eth-usdt-swap", " Eth-Usdt-Swap "] {
        let (symbol, admit) = placed(plan_submit_symbol(
            &submit("okx", typed, None),
            &[engine("okx", "BTC-USDT-SWAP")],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, "ETH-USDT-SWAP", "{typed:?}");
        assert_eq!(admit.expect("admitted first").symbol, "ETH-USDT-SWAP");
    }
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
    // An unknown symbol: the lookup's own words.
    let why = on("bybit", "BTCUSDT", "NOSUCHUSDT");
    assert!(why.contains("cannot admit `NOSUCHUSDT`") && why.contains("no instrument"), "{why}");
    // A grid without a tick, or without a lot: named, never dressed as resolved.
    let why = on("bybit", "BTCUSDT", "NOTICKUSDT");
    assert!(why.contains("tick size") && !why.contains("lot size"), "{why}");
    let why = on("bybit", "BTCUSDT", "NOLOTUSDT");
    assert!(why.contains("lot size") && !why.contains("tick size"), "{why}");
    // The other lane of a one-lane engine, both ways, naming the lane.
    let why = on("binance", "BTCUSDT", "ETHUSDT.P");
    assert!(why.contains("perp symbol") && why.contains("spot lane"), "{why}");
    let why = on("aster", "BTCUSDT.P", "ETHUSDT");
    assert!(why.contains("spot symbol") && why.contains("perp lane"), "{why}");
    // Not a symbol at all: it would reach a signed query string.
    for bad in ["ETH USDT", "ETH/USDT", "ETH&x=1", "", "  ", ".P", "ETH-USDT"] {
        let why = on("bybit", "BTCUSDT", bad);
        assert!(why.contains("is not an exchange symbol"), "{bad:?}: {why}");
    }
    // A paper engine is one book; a block that names no tier is never wider than the old gate.
    for (tier, word) in [(Some(AccountTier::Paper), "paper account"), (None, "does not say")] {
        let block = vike_exec::VenueBlock { tier, ..engine("bybit", "BTCUSDT") };
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
    let full = vike_exec::VenueBlock {
        extra_symbols: (0..vike_exec::MAX_EXTRA_SYMBOLS).map(|i| format!("S{i}USDT")).collect(),
        ..engine("bybit", "BTCUSDT")
    };
    let why =
        refused(plan_submit_symbol(&submit("bybit", "ETHUSDT", None), &[full], Some(&facts), true));
    assert!(why.contains("at most 16 symbols"), "{why}");
}

// ── the lane twin: ONE account row, TWO engines (binance spot, and its perpetual lane) ─────────────────
//
// A binance account whose row states a margin leverage runs a second engine, `binance#PERP`, over the
// same row and keys (`vike_secrets::Accounts::with_lane_twins`). The door puts each order on the engine
// of its lane; the plan then admits the symbol into THAT engine exactly as it does for any account.

/// The account an order's address names, as the wire spells it.
fn account_of(cmd: &WireCommand) -> Option<&str> {
    match cmd {
        WireCommand::Submit(req) => req.account.as_deref(),
        other => panic!("not a submit: {other:?}"),
    }
}

/// The two engines of one account row: the default one on the spot lane, the twin on the perpetual one.
fn two_lanes() -> [vike_exec::VenueBlock; 2] {
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
        assert_eq!(to("ETHUSDT", account).as_deref(), Some("DEFAULT"), "{account:?}");
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
    let cancel = WireCommand::Cancel("c-1".into());
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
        WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
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
    let account_of = |cmd: WireCommand| match cmd {
        WireCommand::Bracket(b) => b.account,
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
    let mount = |venue: &str, account: Option<&str>| WireCommand::MountStrategy {
        venue: venue.to_string(),
        account: account.map(str::to_string),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        controller_id: None,
        name: Some("spread_maker".to_string()),
        rhai: None,
        params: serde_json::json!({}),
    };
    let account_of = |cmd: WireCommand| match cmd {
        WireCommand::MountStrategy { account, .. } => account,
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
fn live(block: vike_exec::VenueBlock) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock { tier: Some(AccountTier::Live), ..block }
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
        WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
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
        |facts: &Table, cmd: WireCommand| plan_submit_symbol(&cmd, &roster, Some(facts), true);

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
        plan(&failing, market("bybit", "BTCUSDT", false)),
        SymbolPlan::Pass,
        "the mounted symbol's price is the mount's feed, not this gate's"
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
        .expect_err("no price ever arrives");
    assert!(err.contains("no price on this node yet") && err.contains("REFUSED"), "{err}");
    assert!(started.elapsed() >= Duration::from_secs(3), "it waited: {:?}", started.elapsed());
    let err = await_price(&Table::eth().with_price(Price::Failing), &wait, "ETHUSDT")
        .expect_err("the feed is gone");
    assert!(err.contains("no price feed"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// End to end: accept_command -> command lane -> a REAL core -> a scripted venue client
// ---------------------------------------------------------------------------------------------

/// A venue double that records every order it is handed and answers each with accept + a full fill
/// at the request's price (the shape `vike_exec::testing::TestExecutionClient` has, but with its
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
            client_order_id: request.client_order_id.clone(),
            venue: self.venue.into(),
            symbol: request.symbol.as_str().into(),
            side: request.side,
            last_qty: request.qty,
            last_px: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: request.ts,
            mark_price: Some(px),
            position_side: vike_model::events::PositionSide::Both,
        };
        self.pending.push_back(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.pending.push_back(Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: Some(format!("v{}", self.n).into()),
            ts: request.ts,
        }));
        self.pending.push_back(Event::Fill(fill.clone()));
        self.pending.push_back(Event::OrderFilled(vike_model::events::OrderFilled {
            client_order_id: request.client_order_id.clone(),
            fill,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
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
        Self::start_valued(venue, symbol, tier, labels, false, None, None)
    }

    /// [`Self::start`] for an account the node has TOLD its currency (`ExecutionEngine::set_account_ccy`, which
    /// the assembly does for an account that converts a quote currency): the engine admits a symbol quoted in
    /// another currency only then (`account-ccy-unset` otherwise).
    pub(super) fn start_anchored(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        ccy: &'static str,
    ) -> Self {
        Self::start_valued(venue, symbol, tier, &[], false, None, Some(ccy))
    }

    /// A deribit-style engine whose mounted symbol is INVERSE (an amount of US dollars), as the mount
    /// reports `BTC-PERPETUAL`'s contract value.
    fn start_inverse(venue: &'static str, symbol: &str) -> Self {
        Self::start_valued(venue, symbol, AccountTier::Demo, &[], true, None, None)
    }

    /// An engine armed with the buying-power gate at the engine-wide initial margin `im` (the mount's
    /// `1 / leverage`; `1.0` is its "no leverage unless asked" default), so a position's published
    /// leverage is the margin rate its symbol is held at.
    pub(super) fn start_margined(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        im: f64,
    ) -> Self {
        Self::start_valued(venue, symbol, tier, &[], false, Some(im), None)
    }

    fn start_valued(
        venue: &'static str,
        symbol: &str,
        tier: AccountTier,
        labels: &[&str],
        inverse: bool,
        im: Option<f64>,
        account_ccy: Option<&'static str>,
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
                im_requirement: im,
                ..RiskLimits::new()
            };
            let mut engine = ExecutionEngine::new(
                Account::new(1.0, venue, None, BalanceMode::Delta),
                RiskGate::new(limits),
                client,
                venue,
                symbol,
            );
            engine.tier = tier;
            engine.route_key = route_key;
            if let Some(ccy) = account_ccy {
                engine.set_account_ccy(ccy).expect("a currency code");
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
                ts: 1,
                route_key: None,
            }))
            .expect("the core is alive");
        let cell = self.core.snapshot_cell();
        wait_until("the engine to publish", || !cell.load().portfolio.venues.is_empty());
    }

    pub(super) fn blocks(&self) -> Vec<vike_exec::VenueBlock> {
        self.core.snapshot_cell().load().portfolio.venues.clone()
    }

    /// One command through the ONE acceptance path, as the TCP surface calls it.
    pub(super) fn send(
        &mut self,
        cmd: WireCommand,
        facts: Option<&dyn SymbolFacts>,
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
            facts,
            None,
            None,
        )
        .map(super::control::Accepted::into_coid)
        .map_err(|e| e.message())
    }

    /// [`Self::send`] with the published engines and route keys as the roster the TCP door judges from. The
    /// plain `send` hands the gates an EMPTY roster, which every verb but a BRACKET reads as "not published
    /// yet" and lets through; a bracket is refused on an empty roster, so a test of one needs this.
    pub(super) fn send_rostered(
        &mut self,
        cmd: WireCommand,
        facts: Option<&dyn SymbolFacts>,
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
            &[],
            &[],
            facts,
            None,
            None,
        )
        .map(super::control::Accepted::into_coid)
        .map_err(|e| e.message())
    }

    /// The published roster once `route_key`'s block trades `symbol`. The venue client saw the order
    /// ON the core thread, which publishes AFTER the fold (coalesced), so `venue_saw` returning says
    /// nothing about the snapshot: wait for the admission itself, then judge the roster.
    fn blocks_once_trading(&self, route_key: &str, symbol: &str) -> Vec<vike_exec::VenueBlock> {
        wait_until(&format!("{route_key} to publish {symbol} as traded"), || {
            self.blocks().iter().any(|b| b.route_key == route_key && b.trades(symbol))
        });
        self.blocks()
    }

    pub(super) fn venue_saw(&self, coid: &str) -> OrderRequest {
        let found =
            || self.submitted.lock().unwrap().iter().find(|o| o.client_order_id == coid).cloned();
        wait_until("the order to reach the venue client", || found().is_some());
        found().unwrap()
    }
}

pub(super) fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn limit(coid: &str, venue: &str, symbol: &str, qty: f64, price: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.client_order_id = coid.into();
    req.qty = qty;
    req.price = Some(price);
    WireCommand::Submit(req)
}

// ── the lane twin's first order, end to end ─────────────────────────────────────────────────────────
//
// v0.1.78 on the CI box (drill `n078-B1`): the first `.P` order after the twin's hot start printed "sent, but the
// node has not answered within 2s" and left no row, no denial and no audit line. The pump that was to price
// the twin's own symbol was binance's SPOT one, which never delivered a tick for `BTCUSDT.P`
// (`vike_binance::market_data`'s `pump_wire`), so the door's hold for the first price could only run out, and
// it ran out after the CLI had stopped listening. These pin the door's half: the hold is bounded and ends in
// a sentence, and the first tick releases the order.

/// The twin's price lane as the node sees it: nothing until the pump's first tick, then that price. Every
/// other answer is the table's. `waits` is the surface's: the TCP door holds an order, Telegram does not.
struct FirstTick {
    table: Table,
    px: Mutex<Option<f64>>,
}

impl FirstTick {
    fn new(waits: bool) -> Self {
        FirstTick { table: Table { waits, ..Table::eth() }, px: Mutex::new(None) }
    }

    /// The pump's first frame lands: the tap now holds a price.
    fn tick(&self, px: f64) {
        *self.px.lock().unwrap() = Some(px);
    }
}

impl SymbolFacts for FirstTick {
    fn generation(&self, route_key: &str) -> u64 {
        self.table.generation(route_key)
    }

    fn price_source(&self, venue: &str) -> Result<(), String> {
        self.table.price_source(venue)
    }

    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
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
                quote_ccy: None,
                quote_rate: None,
                rate_as_of_ms: None,
                im_requirement: None,
            });
        }
        self.table.admission(route_key, venue, symbol)
    }

    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(*self.px.lock().unwrap())
    }

    fn waits(&self) -> bool {
        self.table.waits
    }

    fn exposure_cap_armed(&self, route_key: &str) -> bool {
        self.table.exposure_cap_armed(route_key)
    }
}

/// The ceilings a lane-twin test arms on BOTH engines of the account.
#[derive(Clone, Copy, Default)]
pub(super) struct TwinCaps {
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
            TwinCaps { max_total_exposure, ..TwinCaps::default() },
        )
    }

    /// [`Self::start_lane_twin_capped`] with BOTH engines at `tier` and the three ceilings an operator arms for a
    /// real account (`caps`): what a MAINNET twin runs under (`assemble_engine` refuses a live engine without
    /// the first two, and `policy.max_account_exposure` is the third).
    pub(super) fn start_lane_twin_tiered(tier: AccountTier, caps: TwinCaps) -> Self {
        Self::start_lane_twin_with(tier, caps, false)
    }

    /// [`Self::start_lane_twin_tiered`] whose engines went through the boot restore (`restore_runtime_state` of
    /// the snapshot a previous run journaled): the folded money comes back, the MARKS never do (market data is
    /// not journaled), so until the first tick a restored engine holds no price for its own symbol.
    pub(super) fn start_lane_twin_restored(tier: AccountTier, caps: TwinCaps) -> Self {
        Self::start_lane_twin_with(tier, caps, true)
    }

    fn start_lane_twin_with(tier: AccountTier, caps: TwinCaps, restored: bool) -> Self {
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
                max_total_exposure: caps.max_total_exposure,
                max_notional_per_order: caps.max_notional_per_order,
                max_account_exposure: caps.max_account_exposure,
                ..RiskLimits::new()
            };
            let mut engine = ExecutionEngine::new(
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
            vec![(10_000.0, scripted("binance#PERP", "BTCUSDT.P"))],
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

    /// The pump's first frame for `symbol`, into the core's tick lane (what `TapSink` forwards, BEFORE it
    /// records the price the door reads).
    pub(super) fn tick(&self, symbol: &str, bid: f64, ask: f64) {
        self.core
            .tick_sender()
            .quote(vike_exec::QuoteUpdate {
                venue: "binance".into(),
                symbol: symbol.into(),
                quote: vike_marketdata::QuoteTick {
                    ts: 1,
                    local_ts: 0,
                    bid,
                    ask,
                    bid_size: 1.0,
                    ask_size: 1.0,
                    symbol: String::new(),
                },
            })
            .expect("the core is alive");
    }
}

/// **A surface that must not wait (Telegram's `FailFast`) is answered at once**, in the door's words, for a
/// twin whose pump has not ticked; nothing reaches the venue; and the order is taken the moment the first
/// tick is in. The sentence is the one a held order ends in, so an operator reads the same thing on every
/// surface.
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

    // The first frame: into the core's tick lane FIRST, then into the tap, as `TapSink` orders them.
    node.tick("BTCUSDT.P", 74_299.9, 74_300.1);
    facts.tick(74_300.0);
    let coid = node.send(order(), Some(&facts)).expect("the first tick released the order");
    assert_eq!(coid, "twin-1");
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
        .find(|e| e.level == tracing::Level::WARN && e.field("coid") == Some("twin-2"))
        .unwrap_or_else(|| {
            panic!("a held-then-refused order leaves a WARN naming its coid: {lines:?}")
        });
    assert_eq!(warned.field("symbol"), Some("BTCUSDT.P"));
    assert_eq!(warned.field("venue"), Some("binance"));
    let held_ms: u64 = warned.field("held_ms").and_then(|ms| ms.parse().ok()).expect("held_ms");
    assert!(held_ms >= 3_000, "the line says how long it held: {warned:?}");
    assert!(warned.message.contains("REFUSED") && warned.message.contains(&why), "{warned:?}");

    node.tick("BTCUSDT.P", 74_299.9, 74_300.1);
    let started = Instant::now();
    let coid = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            facts.tick(74_300.0);
        });
        node.send(limit("twin-3", "binance", "BTCUSDT.P", 0.002, 74_288.4), Some(&facts))
    })
    .expect("a tick mid-hold releases the order");
    assert_eq!(coid, "twin-3");
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
        ("okx", "BTC-USDT-SWAP", "eth-usdt-swap", "ETH-USDT-SWAP", 0.01, 0.001),
        ("hyperliquid", "BTC", "kPEPE", "kPEPE", 0.001, 0.01),
        ("alpaca", "AAPL", "btc/usd", "BTC/USD", 1.0, 0.000000001),
    ] {
        let facts = Table::eth();
        let mut node = Node::start(venue, mounted, AccountTier::Demo);
        let coid = format!("e2e-{venue}");
        // 3000.456 @ 0.12346: off both grids, so the snap is visible.
        let acked = node
            .send(limit(&coid, venue, typed, 0.12346, 3000.456), Some(&facts))
            .unwrap_or_else(|e| panic!("{venue}: the order must be accepted, got: {e}"));
        assert_eq!(acked, coid);

        let seen = node.venue_saw(&coid);
        assert_eq!(seen.symbol, canonical, "{venue}: signed on the symbol the operator named");
        let snapped_px = (3000.456_f64 / tick).round() * tick;
        let snapped_qty = (0.12346_f64 / lot).round() * lot;
        assert!((seen.price.unwrap() - snapped_px).abs() < 1e-6, "{venue}: tick {tick}: {seen:?}");
        assert!((seen.qty - snapped_qty).abs() < 1e-9, "{venue}: lot {lot}: {seen:?}");

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

/// **oanda, end to end**: a pair the account was not started on (typed with an underscore, any case) is
/// admitted, reaches the venue client as the engine's own spelling `GBPUSD` snapped on the pair's grid
/// (five decimals, whole units: the mounted symbol's 0.1 / 0.001 grid is not used), its fill folds into
/// its own position, and the engine FAILS CLOSED for oanda now (an order on a symbol it was never told is
/// denied, where it used to be signed on the mounted grid).
#[test]
fn oanda_places_a_foreign_pair_on_its_own_grid_and_folds_into_its_own_position() {
    let facts = Table::eth();
    let mut node = Node::start("oanda", "EURUSD", AccountTier::Demo);
    // 100.4 units @ 3000.4567891: off both grids, so the snap is visible (the table's price is the mark).
    let acked = node
        .send(limit("e2e-oanda", "oanda", "gbp_usd", 100.4, 3_000.456_789_1), Some(&facts))
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
/// ceiling before the symbol is admitted, when the engine's grid has no row for it: an INVERSE perpetual
/// (an amount of US dollars) used to read as `qty · price` there, so 10 dollars of `ETH-PERPETUAL` at 4000
/// was "40000", refused by a ceiling of 100, and the symbol was never admitted. Accepted when the real
/// notional is under the ceiling, refused when over; a LINEAR one is `qty · price` as ever; and the
/// Preview agrees with the send.
#[test]
fn the_ceiling_sizes_a_foreign_symbol_with_its_own_contract_value() {
    use crate::server::symbol_admission::foreign_contract_value;
    let ceiling =
        || ControlLimits::new(ControlLimitsConfig { max_notional: Some(100.0), rate_per_sec: 1e9 });

    // INVERSE: an amount of dollars, the price never enters.
    let facts = Table::eth();
    let mut node = Node::start_inverse("deribit", "BTC-PERPETUAL");
    node.limits = ceiling();
    let blocks = node.blocks();
    let inverse_under = limit("inv-ok", "deribit", "ETH-PERPETUAL", 10.0, 4000.0);
    assert_eq!(
        foreign_contract_value(&inverse_under, &blocks, Some(&facts)),
        Some(vike_model::ContractValue::inverse(1.0))
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
            foreign_contract_value(&inverse_under, &blocks, Some(&facts))
        ),
        None,
        "the preview with the facts' value agrees with the send"
    );
    node.send(inverse_under, Some(&facts)).expect("10 dollars is under a ceiling of 100");
    node.venue_saw("inv-ok");
    let over = node
        .send(limit("inv-over", "deribit", "SOL-PERPETUAL", 150.0, 150.0), Some(&facts))
        .expect_err("150 dollars is over a ceiling of 100");
    assert!(over.contains("order notional 150.00 exceeds"), "{over}");
    assert!(
        !facts.events().iter().any(|e| e == "admitted:SOL-PERPETUAL"),
        "a refused order admits nothing: {:?}",
        facts.events()
    );
    node.core.shutdown_and_join();

    // LINEAR: `qty · price`.
    let facts = Table::eth();
    let mut node = Node::start("deribit", "BTC_USDC-PERPETUAL", AccountTier::Demo);
    node.limits = ceiling();
    node.send(limit("lin-ok", "deribit", "ETH_USDC-PERPETUAL", 0.01, 4000.0), Some(&facts))
        .expect("40 is under a ceiling of 100");
    node.venue_saw("lin-ok");
    let over = node
        .send(limit("lin-over", "deribit", "ETH_USDC-PERPETUAL", 0.05, 4000.0), Some(&facts))
        .expect_err("200 is over a ceiling of 100");
    assert!(over.contains("order notional 200.00 exceeds"), "{over}");
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
    assert!(err.contains("NOSUCHUSDT") && err.contains("no instrument"), "{err}");
    node.send(limit("own", "bybit", "BTCUSDT", 1.0, 60_000.04), Some(&facts)).expect("mounted");
    let own = node.venue_saw("own");
    assert_eq!(own.symbol, "BTCUSDT");
    assert!((own.price.unwrap() - 60_000.0).abs() < 1e-6, "still the mounted 0.1 tick: {own:?}");
    assert!(
        node.submitted.lock().unwrap().iter().all(|o| o.client_order_id != "nope"),
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
        .filter(|(_, r)| matches!(r, SymbolRule::Admits(_)))
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
            client_order_id: "lost".into(),
            venue: venue.into(),
            symbol: foreign.into(),
            side: 1,
            qty: 0.12346,
            order_type: vike_model::OrderType::Limit,
            price: Some(3000.456),
            ..Default::default()
        };
        node.core.send_command(vike_exec::Command::Order(vike_exec::OrderIntent::Submit(
            Box::new(order),
        )));
        node.send(limit("after", venue, mounted, 1.0, 60_000.0), None).expect("mounted");
        node.venue_saw("after");
        assert!(
            node.submitted.lock().unwrap().iter().all(|o| o.client_order_id != "lost"),
            "{venue}: the unadmitted order never reached the venue client"
        );
        node.core.shutdown_and_join();
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 2: labelled accounts, previews, refresh, release, fail-fast
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
    if let WireCommand::Submit(req) = &mut cmd {
        req.client_order_id = "alt-mkt".into();
        req.account = Some("ALT".into());
    }
    let acked = node.send(cmd, Some(&facts)).expect("accepted");
    assert_eq!(acked, "alt-mkt");
    let seen = node.venue_saw("alt-mkt");
    assert_eq!((seen.symbol.as_str(), seen.client_order_id.as_str()), ("ETHUSDT", "alt-mkt"));
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
        let account = key.split_once('#').map(|(_, label)| label);
        let plan =
            plan_submit_symbol(&submit("bybit", "ETHUSDT", account), &blocks, Some(&facts), true);
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
            .is_some_and(|why| why.contains("no price feed"))
    );
}

/// A symbol the engine already trades is refreshed when the source has fresher facts, as an ordinary
/// `AdmitSymbol` ahead of the order; a preview does not.
#[test]
fn a_stale_admission_is_refreshed_ahead_of_the_next_order() {
    let roster = [vike_exec::VenueBlock {
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
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
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
    let trading = |tier| vike_exec::VenueBlock {
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
    assert_eq!(close.events(), ["price:ETHUSDT:subscribe"], "but it opens the lane");

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

/// The Telegram channel's source never makes the poller wait: one look, then a prompt refusal.
#[test]
fn a_source_that_does_not_wait_is_refused_at_once() {
    use crate::server::symbol_admission::{FailFast, await_price};
    let wait = PriceWait { route_key: "bybit".into(), venue: "bybit".into() };
    let inner: std::sync::Arc<dyn SymbolFacts> =
        std::sync::Arc::new(Table::eth().with_price(Price::Pending));
    let fast = FailFast(inner);
    let started = Instant::now();
    let err = await_price(&fast, &wait, "ETHUSDT").expect_err("no price, no wait");
    assert!(err.contains("REFUSED") && err.contains("retry"), "{err}");
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
}

/// **The Telegram poller never waits for the daemon's commit.** On a venue that posts a leverage the commit
/// takes seconds, and the poller runs the whole acceptance inline (a `/panic` queues behind it): `FailFast`
/// returns from `admitted` at once while the wrapped source's commit is still blocked, and the commit still
/// runs, once, on its own thread, when it is let go (as the DETACHED variant, which states a figure it
/// brings at the symbol's next refresh).
#[test]
fn a_chat_orders_commit_runs_off_the_pollers_thread() {
    use crate::server::symbol_admission::FailFast;
    use std::sync::mpsc;

    struct Gated {
        gate: Mutex<mpsc::Receiver<()>>,
        told: Mutex<Vec<String>>,
    }
    impl SymbolFacts for Gated {
        fn admission(&self, _: &str, _: &str, _: &str) -> Result<SymbolAdmission, String> {
            Err("not asked".into())
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(None)
        }
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn admitted(&self, route_key: &str, _: &str, symbol: &str) {
            let _ = self.gate.lock().unwrap().recv_timeout(Duration::from_secs(30));
            self.told.lock().unwrap().push(format!("{route_key}:{symbol}"));
        }
    }

    let (open, gate) = mpsc::channel::<()>();
    let inner = Arc::new(Gated { gate: Mutex::new(gate), told: Mutex::new(Vec::new()) });
    let fast = FailFast(Arc::clone(&inner) as Arc<dyn SymbolFacts>);

    let started = Instant::now();
    fast.admitted("bybit", "bybit", "ETHUSDT");
    assert!(started.elapsed() < Duration::from_millis(500), "waited {:?}", started.elapsed());
    assert!(inner.told.lock().unwrap().is_empty(), "the commit is still blocked");

    open.send(()).unwrap();
    wait_until("the commit ran on its own thread", || {
        *inner.told.lock().unwrap() == ["bybit:ETHUSDT".to_string()]
    });
}

/// An empty symbol is refused on an admitting venue (it was refused by the interim gate too; the
/// adapters would read it as the mounted symbol).
#[test]
fn an_empty_symbol_is_refused_on_an_admitting_venue() {
    let facts = Table::eth();
    let roster = [engine("bybit", "BTCUSDT")];
    let why = refused(plan_submit_symbol(&submit("bybit", "", None), &roster, Some(&facts), true));
    assert!(why.contains("is not an exchange symbol"), "{why}");
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
        .filter(|(_, rule)| matches!(rule, SymbolRule::Admits(_)))
        .map(|(venue, _)| *venue)
        .collect();
    door.sort_unstable();
    assert_eq!(advertised, door);
    let mut admits = ADMITS.to_vec();
    admits.sort_unstable();
    assert_eq!(
        advertised, admits,
        "bybit, binance, aster, okx, hyperliquid, deribit, alpaca, ctrader, oanda, ig, ibkr, dukascopy and fxcm, and nothing else"
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
    assert_eq!(admit.expect("admitted").initial_mark, Some(3000.0));
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
    let WireCommand::Submit(mut req) = limit("mkt", "bybit", "ETHUSDT", 1.0, 3000.0) else {
        unreachable!()
    };
    req.order_type = "market".into();
    req.price = None;
    node.send(WireCommand::Submit(req), Some(&facts)).expect("accepted");
    node.venue_saw("mkt");
    node.core.shutdown_and_join();
}

/// A LIVE account takes a new symbol only with an account-wide exposure ceiling armed; a demo account
/// does not need one. The refusal names the setting.
#[test]
fn a_live_account_without_an_account_wide_ceiling_is_refused_a_new_symbol() {
    let facts = Table::eth().uncapped();
    let live =
        vike_exec::VenueBlock { tier: Some(AccountTier::Live), ..engine("bybit", "BTCUSDT") };
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

/// An order that cannot be lowered (an empty client order id) spends nothing: no lookup, no price
/// subscription, nothing to give back.
#[test]
fn an_order_that_cannot_be_lowered_opens_nothing() {
    let facts = Table::eth();
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let err = node.send(limit("", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).unwrap_err();
    assert!(err.contains("client_order_id"), "{err}");
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
    let WireCommand::Submit(req) = &mut tiny else { unreachable!() };
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
    assert!(!needs_lookup(&WireCommand::Cancel("c".into()), &roster), "not a submit");
    let declared = vike_exec::VenueBlock {
        extra_symbols: vec!["ETHUSDT".into()].into(),
        ..engine("bybit", "BTCUSDT")
    };
    assert!(!needs_lookup(&submit("bybit", "ETHUSDT", None), &[declared]), "already traded");
}
// ---------------------------------------------------------------------------------------------
// hyperliquid: the coin name is the key, case and all
// ---------------------------------------------------------------------------------------------

/// **A Hyperliquid coin keeps its case at the door, and what is not a core perp is refused before any
/// lookup.** `kPEPE` is asked as `kPEPE`; the same letters upper-cased are another (unlisted) coin and
/// are refused by the facts source's own words; the mounted coin passes untouched; a spot pair, `@N`,
/// a builder-dex coin and a lane suffix are not exchange symbols here and ask nothing.
#[test]
fn hyperliquid_keeps_a_coins_case_and_refuses_what_is_not_a_core_perp_before_any_lookup() {
    let facts = Table::eth();
    let roster = [engine("hyperliquid", "BTC")];
    let on = |typed: &str| {
        plan_submit_symbol(&submit("hyperliquid", typed, None), &roster, Some(&facts), true)
    };

    let (symbol, admit) = placed(on("kPEPE"));
    assert_eq!(symbol, "kPEPE");
    let admit = admit.expect("admitted first");
    assert_eq!((admit.route_key.as_str(), admit.symbol.as_str()), ("hyperliquid", "kPEPE"));
    assert_eq!(facts.asked(), vec!["hyperliquid/kPEPE".to_string()]);

    // Another case is another coin: asked as typed, refused with the lookup's words.
    for wrong in ["KPEPE", "kpepe", "btc"] {
        let why = refused(on(wrong));
        assert!(why.contains(&format!("cannot admit `{wrong}`")), "{wrong}: {why}");
    }
    assert_eq!(facts.asked().len(), 4, "each wrong case was asked exactly as typed");

    // Not a core perp's name: refused at the door, nothing asked.
    let before = facts.asked().len();
    for bad in ["HYPE/USDC", "@107", "test:ABC", "BTC.P"] {
        let why = refused(on(bad));
        assert!(why.contains("is not an exchange symbol") && why.contains("REFUSED"), "{why}");
    }
    assert_eq!(facts.asked().len(), before);

    // The mounted coin is not this gate's, and a paper account is one book.
    assert_eq!(on("BTC"), SymbolPlan::Pass);
    let paper =
        vike_exec::VenueBlock { tier: Some(AccountTier::Paper), ..engine("hyperliquid", "BTC") };
    let why = refused(plan_submit_symbol(
        &submit("hyperliquid", "kPEPE", None),
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
    let roster = [engine("hyperliquid", "BTC")];
    let pending = Table::eth().with_price(Price::Pending);
    assert!(matches!(
        plan_submit_symbol(&submit("hyperliquid", "kPEPE", None), &roster, Some(&pending), true),
        SymbolPlan::Place { wait: Some(_), admit: Some(_), .. }
    ));
    let why = refused(plan_submit_symbol(
        &market("hyperliquid", "kPEPE", true),
        &roster,
        Some(&pending),
        true,
    ));
    assert!(why.contains("reduce-only"), "{why}");
    let held = vike_exec::VenueBlock {
        extra_symbols: vec!["kPEPE".into()].into(),
        ..engine("hyperliquid", "BTC")
    };
    assert!(matches!(
        plan_submit_symbol(&market("hyperliquid", "kPEPE", true), &[held], Some(&pending), true),
        SymbolPlan::Place { admit: None, wait: None, .. }
    ));
}

/// **A LIVE hyperliquid account takes a new coin only with the account-wide exposure ceiling armed**
/// (the rule every `Admits` venue shares: `max_total_exposure` bounds ONE symbol), and an order on a coin
/// the facts source cannot answer is refused with nothing admitted.
#[test]
fn a_live_hyperliquid_account_needs_the_ceiling_and_an_unlisted_coin_is_refused() {
    let live =
        vike_exec::VenueBlock { tier: Some(AccountTier::Live), ..engine("hyperliquid", "BTC") };
    let uncapped = Table::eth().uncapped();
    let why = refused(plan_submit_symbol(
        &submit("hyperliquid", "kPEPE", None),
        std::slice::from_ref(&live),
        Some(&uncapped),
        true,
    ));
    assert!(why.contains("max_account_exposure") && why.contains("LIVE"), "{why}");
    assert!(uncapped.asked().is_empty(), "refused before any lookup");
    // Armed: the same order is placed; a coin the listing lacks is refused, nothing admitted.
    let armed = Table::eth();
    placed(plan_submit_symbol(
        &submit("hyperliquid", "kPEPE", None),
        std::slice::from_ref(&live),
        Some(&armed),
        true,
    ));
    let why = refused(plan_submit_symbol(
        &submit("hyperliquid", "NOSUCH", None),
        std::slice::from_ref(&live),
        Some(&armed),
        true,
    ));
    assert!(why.contains("cannot admit `NOSUCH`"), "{why}");
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
    let roster = [engine("hyperliquid", "BTC"), engine("hyperliquid#ALT", "BTC")];
    let (_, admit) = placed(plan_submit_symbol(
        &submit("hyperliquid", "kPEPE", Some("ALT")),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(admit.expect("ALT is admitted").route_key, "hyperliquid#ALT");
    let (_, admit) = placed(plan_submit_symbol(
        &submit("hyperliquid", "kPEPE", None),
        &roster,
        Some(&facts),
        true,
    ));
    assert_eq!(admit.expect("the default is admitted").route_key, "hyperliquid");
    assert_eq!(
        facts.asked(),
        vec!["hyperliquid#ALT/kPEPE".to_string(), "hyperliquid/kPEPE".to_string()]
    );

    let mut node = Node::start_with("hyperliquid", "BTC", AccountTier::Demo, &["ALT"]);
    let facts = Table::eth();
    let mut cmd = market("hyperliquid", "kPEPE", false);
    if let WireCommand::Submit(req) = &mut cmd {
        req.client_order_id = "hl-alt".into();
        req.account = Some("ALT".into());
    }
    node.send(cmd, Some(&facts)).expect("ALT is admitted the coin");
    let seen = node.venue_saw("hl-alt");
    assert_eq!(seen.symbol, "kPEPE", "signed on the coin as the venue spells it");
    let blocks = node.blocks_once_trading("hyperliquid#ALT", "kPEPE");
    let block = |key: &str| blocks.iter().find(|b| b.route_key == key).expect(key);
    assert!(block("hyperliquid#ALT").trades("kPEPE"));
    assert!(!block("hyperliquid").trades("kPEPE"), "the other account was not told");
    assert!(block("hyperliquid").trades("BTC"), "and keeps its own coin");
    node.core.shutdown_and_join();
}

/// A hyperliquid engine mounted on a SPOT pair or a builder-dex coin runs that product, so a core
/// perp is not admitted into it (its balance, fills channel and asset ids differ); nothing is asked.
#[test]
fn a_hyperliquid_spot_or_dex_engine_is_not_admitted_a_core_perp() {
    let facts = Table::eth();
    for mounted in ["HYPE/USDC", "@107", "test:ABC"] {
        let roster = [engine("hyperliquid", mounted)];
        let why = refused(plan_submit_symbol(
            &submit("hyperliquid", "kPEPE", None),
            &roster,
            Some(&facts),
            true,
        ));
        assert!(why.contains("spot or builder-dex market"), "{mounted}: {why}");
    }
    assert!(facts.asked().is_empty(), "refused before any lookup");
}

/// **alpaca takes the venue's own asset symbol and nothing else**: an equity ticker (a share-class dot
/// allowed) or a crypto pair WITH its slash. What else a trader may type is refused BY NAME before any
/// lookup, because the symbol reaches a query string and a JSON body downstream.
#[test]
fn alpaca_refuses_what_is_not_an_asset_symbol_before_any_lookup() {
    let facts = Table::eth();
    for bad in
        ["", "  ", "BTC USD", "BTC-USD", "BTC/USD/X", "A.B.C", "/USD", "BTC/", "AAPL&x=1", "A\"}"]
    {
        let why = refused(plan_submit_symbol(
            &submit("alpaca", bad, None),
            &[engine("alpaca", "AAPL")],
            Some(&facts),
            true,
        ));
        assert!(
            why.contains("is not an alpaca symbol") && why.contains("BTC/USD"),
            "{bad:?}: {why}"
        );
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
    // A bare crypto pair is NOT guessed into a slashed one: it is read as the ticker it looks like and
    // the venue's own list refuses it by name.
    let why = refused(plan_submit_symbol(
        &submit("alpaca", "btcusd", None),
        &[engine("alpaca", "AAPL")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("cannot admit `BTCUSD`") && why.contains("no instrument"), "{why}");
}

/// **One instrument is one symbol in an engine**: alpaca writes a crypto pair with or without its slash
/// (`BTCUSD` and `BTC/USD` are one pair on the wire), so an engine trading one is not admitted the other,
/// whichever of the engine's symbols (mounted, a declared leg or an earlier admission) spells it.
#[test]
fn alpaca_refuses_the_other_spelling_of_an_instrument_the_engine_trades() {
    let facts = Table::eth();
    let leg = vike_exec::VenueBlock {
        extra_symbols: vec!["BTC/USD".into()].into(),
        ..engine("alpaca", "AAPL")
    };
    for (block, typed, own) in [
        (engine("alpaca", "BTCUSD"), "btc/usd", "BTCUSD"),
        (engine("alpaca", "BTC/USD"), "BTCUSD", "BTC/USD"),
        (leg, "btcusd", "BTC/USD"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("alpaca", typed, None),
            &[block],
            Some(&facts),
            true,
        ));
        assert!(why.contains(&format!("already trades as `{own}`")), "{typed}: {why}");
        assert!(why.contains("REFUSED"), "{typed}: {why}");
    }
    assert!(facts.asked().is_empty(), "refused before any lookup, no slot spent");
}

/// **A restarted account is judged on its NEW engine.** The core's list only grows, so the published engine
/// blocks hold the stopped engine's block (which had traded ETHUSDT) before the new one's (which has not): the
/// door must plan the admission the new engine needs, or the RiskGate denies `symbol-not-admitted`. The first
/// assertion pins the raw list's wrong answer (the first match), so the second is the fix and not a tautology.
#[test]
fn a_symbol_only_the_stale_block_traded_is_admitted_on_the_new_engine() {
    let facts = Table::eth();
    let stale = vike_exec::VenueBlock {
        extra_symbols: vec!["ETHUSDT".to_string()].into(),
        ..engine("bybit#ALT", "BTCUSDT")
    };
    let raw = vec![stale, engine("bybit#ALT", "BTCUSDT")];
    let cmd = submit("bybit", "ETHUSDT", Some("ALT"));
    let (_, admit) = placed(plan_submit_symbol(&cmd, &raw, Some(&facts), true));
    assert!(admit.is_none(), "the raw list's first match is the stale engine: no admission");

    let mut snap = vike_exec::CoreSnapshot::empty("bybit", "BTCUSDT");
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
    for (mounted, typed, canonical) in [("AAPL", "BTC/USD", "BTC/USD"), ("BTC/USD", "msft", "MSFT")]
    {
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

/// **ctrader takes the broker's name, case kept, and nothing else**: letters, digits and `_` only, so a
/// name reaches a log line and a protobuf field clean. What else a trader may type is refused BY NAME
/// before any lookup.
#[test]
fn ctrader_refuses_what_is_not_a_broker_name_before_any_lookup() {
    let facts = Table::eth();
    let too_long = "A".repeat(33);
    let bad_names = [
        "",
        "  ",
        "EUR USD",
        "EUR/USD",
        "EUR-USD",
        "EURUSD.x",
        "A\"}",
        "EUR\nUSD",
        too_long.as_str(),
    ];
    for bad in bad_names {
        let why = refused(plan_submit_symbol(
            &submit("ctrader", bad, None),
            &[engine("ctrader", "EURUSD")],
            Some(&facts),
            true,
        ));
        assert!(
            why.contains("is not a ctrader symbol") && why.contains("case kept"),
            "{bad:?}: {why}"
        );
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **ig takes an EPIC and nothing else**: three or more dot-separated segments of letters, digits, `_` and
/// `-`. A market NAME (the adapter itself refuses `EURUSD`), a pair with a slash and anything that could
/// not stand in a URL path is refused BY NAME before any lookup.
#[test]
fn ig_refuses_what_is_not_an_epic_before_any_lookup() {
    let facts = Table::eth();
    let long = format!("CS.D.{}.IP", "X".repeat(64));
    for bad in [
        "",
        "  ",
        "EURUSD",
        "EUR/USD",
        "CS.D",
        "CS..EURUSD.IP",
        ".D.EURUSD.IP",
        "CS.D.EURUSD.",
        "CS.D.EUR USD.IP",
        "CS.D.EURUSD.MINI.IP?x=1",
        "CS.D.EURUSD/../x",
        "CS.D.EUR\"}.IP",
        long.as_str(),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("ig", bad, None),
            &[engine("ig", "CS.D.EURUSD.MINI.IP")],
            Some(&facts),
            true,
        ));
        assert!(
            why.contains("is not an IG epic") && why.contains("CS.D.EURUSD.MINI.IP"),
            "{bad:?}: {why}"
        );
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// A wrong case is ANOTHER name (the broker's list is case-sensitive): the door does not upper-case it
/// into the mounted symbol or into a listed one, it asks the list, and the list refuses it by name. The
/// mounted symbol itself, as the engine spells it, passes untouched.
#[test]
fn ctrader_keeps_the_case_it_is_given_and_leaves_the_refusal_to_the_list() {
    let facts = Table::eth();
    let why = refused(plan_submit_symbol(
        &submit("ctrader", "gbpusd", None),
        &[engine("ctrader", "EURUSD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("cannot admit `gbpusd`") && why.contains("no instrument"), "{why}");
    assert_eq!(facts.asked(), ["ctrader/gbpusd"], "asked as typed, not as `GBPUSD`");
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
    let paper =
        vike_exec::VenueBlock { tier: Some(AccountTier::Paper), ..engine("ctrader", "EURUSD") };
    let why = refused(plan_submit_symbol(
        &submit("ctrader", "GBPUSD", None),
        &[paper],
        Some(&Table::eth()),
        true,
    ));
    assert!(why.contains("a paper account trades only the symbol it was started on"), "{why}");
}

/// **One epic is one symbol in an engine**: the door spells an epic upper case (the reconcile client holds
/// it so), so an engine whose own symbol or declared leg is the same epic in another case is not admitted
/// it a second time, and an engine on the upper-case epic takes the lower-case one as its own.
#[test]
fn ig_refuses_a_case_variant_of_an_epic_the_engine_trades() {
    let facts = Table::eth();
    let leg = vike_exec::VenueBlock {
        extra_symbols: vec!["cs.d.gbpusd.mini.ip".into()].into(),
        ..engine("ig", "CS.D.EURUSD.MINI.IP")
    };
    for (block, typed, own) in [
        (engine("ig", "cs.d.eurusd.mini.ip"), "CS.D.EURUSD.MINI.IP", "cs.d.eurusd.mini.ip"),
        (leg, "CS.D.GBPUSD.MINI.IP", "cs.d.gbpusd.mini.ip"),
    ] {
        let why =
            refused(plan_submit_symbol(&submit("ig", typed, None), &[block], Some(&facts), true));
        assert!(why.contains(&format!("already trades as `{own}`")), "{typed}: {why}");
        assert!(why.contains("REFUSED"), "{typed}: {why}");
    }
    assert!(facts.asked().is_empty(), "refused before any lookup, no slot spent");
    // The same epic in a lower case, against an engine on the canonical one, IS that engine's symbol.
    assert_eq!(
        plan_submit_symbol(
            &submit("ig", "cs.d.eurusd.mini.ip", None),
            &[engine("ig", "CS.D.EURUSD.MINI.IP")],
            Some(&facts),
            true
        ),
        SymbolPlan::Place { symbol: "CS.D.EURUSD.MINI.IP".into(), admit: None, wait: None }
    );
}

/// **ig's admission carries the epic's CONTRACT VALUE**, which the mounted epic's start does not (the
/// plan's "no grid and no contract value even for the mounted epic"): the facts' linear 10,000 rides the
/// admission, so the engine and the policy ceiling size an order on the epic as contracts x units.
#[test]
fn ig_admission_carries_the_epics_contract_value() {
    let facts = Table::eth();
    let (symbol, admit) = placed(plan_submit_symbol(
        &submit("ig", "cs.d.gbpusd.mini.ip", None),
        &[engine("ig", "CS.D.EURUSD.MINI.IP")],
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, "CS.D.GBPUSD.MINI.IP");
    let admit = admit.expect("admitted first");
    assert_eq!(admit.contract_value, Some(vike_model::ContractValue::linear(10_000.0)));
    assert_eq!((admit.grid.tick_size, admit.grid.lot_size), (Some(0.0001), Some(0.1)));
    // Unlisted by the facts (a refused class, an unknown epic): refused in the facts' words, no slot spent.
    let why = refused(plan_submit_symbol(
        &submit("ig", "IX.D.SPTRD.DAILY.IP", None),
        &[engine("ig", "CS.D.EURUSD.MINI.IP")],
        Some(&facts),
        true,
    ));
    assert!(
        why.contains("cannot admit `IX.D.SPTRD.DAILY.IP`") && why.contains("no instrument"),
        "{why}"
    );
}

/// **ig, end to end**: an epic the account was not started on is admitted, reaches the venue client as the
/// upper-case epic snapped on ITS grid (the pip, the minimum deal size: not the mounted 0.1 / 0.001), its
/// fill folds into its own position, the engine holds its contract value, and the engine now FAILS CLOSED
/// for ig (an order on an epic it was never told is denied, where it used to be signed on the mounted grid).
#[test]
fn ig_places_a_foreign_epic_on_its_own_grid_and_folds_into_its_own_position() {
    let facts = Table::eth().with_price(Price::At(1.2345));
    let mut node = Node::start("ig", "CS.D.EURUSD.MINI.IP", AccountTier::Demo);
    // 0.12346 contracts @ 1.234567: off both grids, so the snap is visible (the table's price is the mark).
    let acked = node
        .send(limit("e2e-ig", "ig", "cs.d.gbpusd.mini.ip", 0.12346, 1.234_567), Some(&facts))
        .unwrap_or_else(|e| panic!("ig: the order must be accepted, got: {e}"));
    assert_eq!(acked, "e2e-ig");

    let seen = node.venue_saw("e2e-ig");
    assert_eq!(seen.symbol, "CS.D.GBPUSD.MINI.IP", "signed on the upper-case epic");
    assert!((seen.price.unwrap() - 1.2346).abs() < 1e-9, "the pip grid: {seen:?}");
    assert!((seen.qty - 0.1).abs() < 1e-9, "the minimum deal size is the step: {seen:?}");

    let cell = node.core.snapshot_cell();
    wait_until("the foreign position", || {
        cell.load().positions.iter().any(|p| p.symbol == "CS.D.GBPUSD.MINI.IP")
    });
    let snap = cell.load_full();
    let block = &snap.portfolio.venues[0];
    assert!(block.trades("CS.D.GBPUSD.MINI.IP"), "published as tradable");
    assert!(block.admits_symbols, "advertised: the desktop may send a symbol");
    assert_eq!(
        block.contract_value_of("CS.D.GBPUSD.MINI.IP"),
        vike_model::ContractValue::linear(10_000.0),
        "the engine sizes the epic as contracts x units"
    );
    let sizes: Vec<(&str, bool)> =
        snap.positions.iter().map(|p| (p.symbol.as_str(), p.size > 0.0)).collect();
    assert_eq!(sizes, vec![("CS.D.GBPUSD.MINI.IP", true)], "the fill is the new epic's");

    // Fail closed: the mount hands ig's engine the deny flag (`admitting_venues`), and an order the door
    // did not admit (no facts source) never reaches the venue client.
    assert!(crate::server::symbol_admission::admitting_venues().iter().any(|v| v == "ig"));
    let denied = node.send(limit("never", "ig", "CS.D.AUDUSD.MINI.IP", 1.0, 0.7), None);
    assert!(denied.is_err(), "no facts source, no admission: {denied:?}");
    assert!(node.submitted.lock().unwrap().iter().all(|o| o.client_order_id != "never"));
    node.core.shutdown_and_join();
}

/// **ibkr takes a US stock by NAME and nothing else**: `TICKER.SMART.CCY`, or a bare ticker read as the USD
/// stock. Forex, a future, an option, an explicit exchange and anything with a character a ticker does not
/// have are refused BY NAME before any lookup (the symbol reaches the gateway's symbol search downstream).
#[test]
fn ibkr_refuses_what_is_not_a_stock_name_before_any_lookup() {
    let facts = Table::eth();
    for bad in [
        "",
        "  ",
        "EUR.IDEALPRO.USD",
        "EUR.USD.IDEALPRO",
        "AAPL.NASDAQ.USD",
        "ES.GLOBEX.USD.FUT.20251219",
        "SPY.SMART.USD.OPT.20251219.500.C",
        "AAPL.SMART.USD.STK",
        "BRK.B.SMART.USD",
        "AAPL.SMART.US",
        "AAPL&x=1",
        "A B",
        "ABCDEFGHIJKLM",
    ] {
        let why = refused(plan_submit_symbol(
            &submit("ibkr", bad, None),
            &[engine("ibkr", "AAPL.SMART.USD")],
            Some(&facts),
            true,
        ));
        assert!(
            why.contains("is not an IBKR stock name") && why.contains("TICKER.SMART.CCY"),
            "{bad:?}: {why}"
        );
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
}

/// **One stock is one symbol in an engine**: a stock spelled with a named exchange beside the same ticker on
/// SMART is the same position twice, so the engine's own spelling is the one to use, whichever of the engine's
/// symbols (mounted, a declared leg or an earlier admission) spells it. A forex pair or a future sharing the
/// ticker is another instrument and is not refused for it.
#[test]
fn ibkr_refuses_another_spelling_of_a_stock_the_engine_trades() {
    let facts = Table::eth();
    let leg = vike_exec::VenueBlock {
        extra_symbols: vec!["MSFT.NASDAQ.USD".into()].into(),
        ..engine("ibkr", "AAPL.SMART.USD")
    };
    for (block, typed, own) in [
        (engine("ibkr", "MSFT.NASDAQ.USD"), "msft", "MSFT.NASDAQ.USD"),
        (engine("ibkr", "VOD.LSE.USD.STK"), "vod", "VOD.LSE.USD.STK"),
        (leg, "MSFT", "MSFT.NASDAQ.USD"),
    ] {
        let why =
            refused(plan_submit_symbol(&submit("ibkr", typed, None), &[block], Some(&facts), true));
        assert!(why.contains(&format!("already trades as `{own}`")), "{typed}: {why}");
        assert!(why.contains("REFUSED"), "{typed}: {why}");
    }
    assert!(facts.asked().is_empty(), "refused before any lookup, no slot spent");
    // TSLA beside a forex pair or a future of the same root is a different instrument.
    for mounted in ["TSLA.IDEALPRO.USD", "TSLA.GLOBEX.USD.FUT.20251219"] {
        let (symbol, _) = placed(plan_submit_symbol(
            &submit("ibkr", "tsla", None),
            &[engine("ibkr", mounted)],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, "TSLA.SMART.USD", "{mounted}");
    }
}

/// **The ledger is one currency**: a stock is admitted only into an engine whose own contract counts money in
/// the stock's currency (a GBP engine is not admitted a USD stock; a USD one is not admitted a GBP stock), and
/// an engine whose symbol states no currency is not admitted anything. Here the account has NO account-wide
/// exposure ceiling (`uncapped`), so it does not convert (decision 0183: an account that converts, a `demo`
/// one with a ceiling, is `ibkr_money_door`'s: the same rows are then looked up, and admitted only with the
/// conversion in the facts).
#[test]
fn ibkr_admits_a_stock_only_in_the_engines_own_currency() {
    let facts = Table::eth().uncapped();
    for (mounted, typed) in [
        ("VOD.SMART.GBP", "AAPL"),
        ("AAPL.SMART.USD", "VOD.SMART.GBP"),
        ("EUR.IDEALPRO.EUR", "AAPL"),
        ("AAPL", "MSFT"),
    ] {
        let why = refused(plan_submit_symbol(
            &submit("ibkr", typed, None),
            &[engine("ibkr", mounted)],
            Some(&facts),
            true,
        ));
        assert!(why.contains("nothing converts"), "{mounted} <- {typed}: {why}");
    }
    assert!(facts.asked().is_empty());
    // A forex engine counts money in its quote currency, in either field order.
    for mounted in ["EUR.IDEALPRO.USD", "EUR.USD.IDEALPRO"] {
        let (symbol, _) = placed(plan_submit_symbol(
            &submit("ibkr", "msft", None),
            &[engine("ibkr", mounted)],
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, "MSFT.SMART.USD", "{mounted}");
    }
}

/// The engine's own symbol in another case is the engine's own spelling on every tier (this venue answered
/// it before it admitted): placed on the engine's spelling, no lookup.
#[test]
fn ibkr_own_symbols_in_another_case_are_placed_on_the_engines_spelling() {
    let facts = Table::eth();
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        let block = vike_exec::VenueBlock {
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
        &[engine("ibkr", "AAPL.SMART.USD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("no price feed for `MSFT.SMART.USD`"), "{why}");
    let why = refused(plan_submit_symbol(
        &market("ibkr", "msft", true),
        &[engine("ibkr", "AAPL.SMART.USD")],
        Some(&facts),
        true,
    ));
    assert!(why.contains("reduce-only"), "{why}");
    let paper = vike_exec::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine("ibkr", "AAPL.SMART.USD")
    };
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
                &[engine("ibkr", "AAPL.SMART.USD")],
                Some(&facts),
                commit,
            ));
            assert!(
                why.contains(&format!(
                    "no price feed for `{}.SMART.USD`: this node runs no price feed for ibkr",
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
        &[engine("ibkr", "AAPL.SMART.USD")],
        Some(&facts),
        true,
    ));
    assert_eq!(facts.asked(), ["ibkr/MSFT.SMART.USD"]);
}

/// A dukascopy order of the sidecar's smallest size (1000 units), priced, so the grid check passes.
fn dukascopy_order(symbol: &str) -> WireCommand {
    let WireCommand::Submit(mut req) = submit("dukascopy", symbol, None) else { unreachable!() };
    req.qty = 1000.0;
    WireCommand::Submit(req)
}

/// **dukascopy takes a currency or metal pair, spelled with or without its slash, in the engine's own
/// money**: the admission carries the table's grid for THAT pair, one lookup, on the canonical six letters.
#[test]
fn dukascopy_admits_a_pair_in_the_engines_money_on_its_own_grid() {
    for (mounted, typed, canonical, tick) in [
        ("EURUSD", "gbpusd", "GBPUSD", 0.00001),
        ("EURUSD", " xau/usd ", "XAUUSD", 0.001),
        ("EUR/USD", "GBPUSD", "GBPUSD", 0.00001),
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

/// A name with a character no SDK instrument has is refused BY NAME before any lookup (it reaches the
/// sidecar's JSON line); an SDK-shaped name that is not a pair is passed to the gatewayhub's table in upper
/// case, which refuses a CFD and an unknown name in its own words.
#[test]
fn dukascopy_refuses_what_is_not_an_instrument_name_and_leaves_the_rest_to_the_table() {
    let facts = Table::eth();
    let roster = [engine("dukascopy", "EURUSD")];
    let long = "A".repeat(33);
    for bad in ["", "  ", "EUR USD", "EURUSD&x=1", "EUR_USD", "EUR\"USD", long.as_str()] {
        let why = refused(plan_submit_symbol(&dukascopy_order(bad), &roster, Some(&facts), true));
        assert!(why.contains("is not an instrument name"), "{bad:?}: {why}");
        assert!(why.contains("REFUSED"), "{bad:?}: {why}");
    }
    assert!(facts.asked().is_empty(), "a refused spelling never looks anything up");
    let why = refused(plan_submit_symbol(
        &dukascopy_order("copper.cmd/usd"),
        &roster,
        Some(&facts),
        true,
    ));
    assert!(why.contains("cannot admit `COPPER.CMD/USD`"), "{why}");
    assert_eq!(facts.asked(), ["dukascopy/COPPER.CMD/USD"], "the table is asked, upper-cased");
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

/// **The engine's own pair under another spelling stays the engine's own, on every tier**: this venue took
/// `EUR/USD` for `EURUSD` before it admitted anything (the sidecar reads both as one instrument), so the
/// order is placed on the engine's spelling with no lookup and no second symbol, paper included. An admitted
/// leg is the same.
#[test]
fn dukascopy_own_pair_in_another_spelling_is_the_engines_own_on_every_tier() {
    let facts = Table::eth();
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        for (mounted, typed, want) in [
            ("EURUSD", "eur/usd", "EURUSD"),
            ("EURUSD", "eurusd", "EURUSD"),
            ("EUR/USD", "EURUSD", "EUR/USD"),
        ] {
            let block = vike_exec::VenueBlock { tier: Some(tier), ..engine("dukascopy", mounted) };
            assert_eq!(
                plan_submit_symbol(&dukascopy_order(typed), &[block], Some(&facts), true),
                SymbolPlan::Place { symbol: want.into(), admit: None, wait: None },
                "{tier:?}: {mounted} <- {typed}"
            );
        }
    }
    let leg = vike_exec::VenueBlock {
        extra_symbols: vec!["GBPUSD".into()].into(),
        ..engine("dukascopy", "EURUSD")
    };
    assert_eq!(
        plan_submit_symbol(&dukascopy_order("gbp/usd"), &[leg], Some(&facts), true),
        SymbolPlan::Place { symbol: "GBPUSD".into(), admit: None, wait: None },
        "an admitted leg is placed as the engine spells it"
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
    let WireCommand::Submit(mut close) = dukascopy_order("gbpusd") else { unreachable!() };
    close.reduce_only = true;
    let why = refused(plan_submit_symbol(&WireCommand::Submit(close), &roster, Some(&facts), true));
    assert!(why.contains("reduce-only"), "{why}");
    let paper =
        vike_exec::VenueBlock { tier: Some(AccountTier::Paper), ..engine("dukascopy", "EURUSD") };
    let why = refused(plan_submit_symbol(&dukascopy_order("gbpusd"), &[paper], Some(&facts), true));
    assert!(why.contains("paper account trades only the symbol it was started on"), "{why}");
}

/// A quantity under the sidecar's 1000-unit floor is refused from the facts' grid with no slot spent.
#[test]
fn dukascopy_refuses_a_quantity_under_the_sidecars_floor_before_a_slot_is_spent() {
    let facts = Table::eth();
    let WireCommand::Submit(mut small) = dukascopy_order("gbpusd") else { unreachable!() };
    small.qty = 999.0;
    let why = refused(plan_submit_symbol(
        &WireCommand::Submit(small),
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
