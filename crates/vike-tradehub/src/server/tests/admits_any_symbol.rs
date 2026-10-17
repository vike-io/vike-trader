//! The control door lets an order name ANY symbol of its exchange (decision 0173): bybit, binance, aster
//! and okx admit the symbol into the account's engine and place the order on it; deribit keeps the refusal,
//! with the table's reason; every refusal names its cause.
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
    SYMBOL_RULES, SymbolFacts, SymbolPlan, SymbolRule, plan_submit_symbol, submit_symbol_refusal,
};
use vike_exec::{
    Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, SymbolAdmission,
};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, RiskLimits, SymbolGrid};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

/// The venues whose adapter signs on the ORDER's symbol.
const ADMITS: [&str; 5] = ["binance", "bybit", "aster", "okx", "hyperliquid"];
/// The venues whose adapter still signs the MOUNTED symbol whatever the order names.
const MOUNTED_ONLY: [&str; 1] = ["deribit"];
/// The venues whose adapter reads the order's own symbol and whose engine judges it soundly enough
/// (live accounts of polymarket run on this row: it must not refuse a symbol).
const PER_ORDER: [&str; 2] = ["oanda", "polymarket"];
/// The venues whose adapter reads the order's own symbol but whose engine cannot judge a foreign one.
const PER_ORDER_UNSAFE: [&str; 6] = ["ctrader", "ig", "fxcm", "ibkr", "alpaca", "dukascopy"];

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

/// **An `Admits` venue's lease claims what its daemon client reads, and okx's reads nothing.** The
/// node mirrors the symbols it told the daemon (`RemoteReconClient::admitted_symbols`) only for the
/// venues in `RECON_READS_ADMITTED`; any other claims none, so the restore planner holds its orders on
/// an admitted symbol. A new `Admits` venue fails here until someone decides which side it is on.
#[test]
fn an_admits_venue_is_either_read_by_its_recon_client_or_the_known_okx_gap() {
    use vike_gatewayhub_client::trade::RECON_READS_ADMITTED;
    let mut admits: Vec<&str> = ADMITS.to_vec();
    admits.retain(|v| !RECON_READS_ADMITTED.contains(v));
    assert_eq!(admits, ["okx"], "okx's recon client reads the mounted instrument only");
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
    waits: bool,
}

impl Table {
    fn eth() -> Self {
        Table {
            rows: vec![
                ("bybit", "ETHUSDT", Some(0.01), Some(0.01)),
                ("binance", "ETHUSDT", Some(0.01), Some(0.0001)),
                ("binance", "ETHUSDT.P", Some(0.01), Some(0.001)),
                ("aster", "ETHUSDT.P", Some(0.01), Some(0.001)),
                ("aster", "ETHUSDT", Some(0.01), Some(0.0001)),
                ("bybit", "NOTICKUSDT", None, Some(0.01)),
                ("bybit", "NOLOTUSDT", Some(0.01), None),
                ("okx", "ETH-USDT-SWAP", Some(0.01), Some(0.001)),
                ("hyperliquid", "kPEPE", Some(0.001), Some(0.01)),
            ],
            asked: Mutex::new(Vec::new()),
            price: Price::Ready,
            capped: true,
            events: Mutex::new(Vec::new()),
            refresh: Mutex::new(None),
            waits: true,
        }
    }
    fn with_price(mut self, price: Price) -> Self {
        self.price = price;
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
}

impl SymbolFacts for Table {
    fn generation(&self, _: &str) -> u64 {
        0
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
        Ok(SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid { tick_size: *tick, lot_size: *lot, ..SymbolGrid::default() },
            contract_value: None,
            initial_mark: None,
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

/// **A paper account is ONE book on every per-order venue**: `PaperExecutionClient` stamps its own
/// symbol on every fill, so a foreign symbol would fill on the mounted symbol's bars and be booked
/// on it. The door refuses it with the words the `Admits` venues use.
#[test]
fn a_paper_account_refuses_a_foreign_symbol_on_every_per_order_venue() {
    let paper = |venue: &str| vike_exec::VenueBlock {
        tier: Some(AccountTier::Paper),
        ..engine(venue, "MOUNTED")
    };
    for venue in PER_ORDER.iter().filter(|v| **v != "polymarket").chain(PER_ORDER_UNSAFE.iter()) {
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

/// **No price source, no opening order on a foreign symbol**: none of these venues has a price
/// watcher, so the engine would judge a priceless order at notional 0 and pass every ceiling. A
/// limit price (the order's own) or a reduce-only order is not refused.
#[test]
fn a_priceless_opening_order_on_a_foreign_symbol_is_refused_where_nothing_prices_it() {
    for venue in PER_ORDER.iter().filter(|v| **v != "polymarket") {
        for tier in [AccountTier::Demo, AccountTier::Live] {
            let block = vike_exec::VenueBlock { tier: Some(tier), ..engine(venue, "MOUNTED") };
            let roster = std::slice::from_ref(&block);
            let why =
                refused(plan_submit_symbol(&market(venue, "FOREIGN", false), roster, None, true));
            assert!(why.contains("limit price") && why.contains("REFUSED"), "{venue}: {why}");
            assert_eq!(
                submit_symbol_refusal(&market(venue, "FOREIGN", false), roster, None),
                Some(why)
            );
            // A reduce-only order closes what exists: no price needed.
            assert_eq!(
                plan_submit_symbol(&market(venue, "FOREIGN", true), roster, None, true),
                SymbolPlan::Pass,
                "{venue} {tier:?}: reduce-only"
            );
            // A priced order is judged at its own price: exactly as before.
            assert_eq!(
                plan_submit_symbol(&submit(venue, "FOREIGN", None), roster, None, true),
                SymbolPlan::Pass,
                "{venue} {tier:?}: a limit price"
            );
        }
    }
}

/// **The unsafe per-order venues refuse a foreign symbol with their own reason**, on any tier and
/// for any order (reduce-only included: the engine rounds it on the wrong grid too).
#[test]
fn an_unsafe_per_order_venue_refuses_a_foreign_symbol_with_its_reason() {
    let reasons = [
        ("ibkr", "multiplier and lot of a foreign instrument are unknown to the engine"),
        ("ctrader", "rounds the order on the mounted symbol's grid until admission supports"),
        ("alpaca", "rounds the order on the mounted symbol's grid until admission supports"),
        ("ig", "no price source for a symbol the engine was not started on"),
        ("fxcm", "no price source for a symbol the engine was not started on"),
        ("dukascopy", "no price source for a symbol the engine was not started on"),
    ];
    assert_eq!(reasons.len(), PER_ORDER_UNSAFE.len());
    for (venue, reason) in reasons {
        assert!(PER_ORDER_UNSAFE.contains(&venue), "{venue}");
        let block = engine(venue, "MOUNTED");
        for cmd in [
            submit(venue, "FOREIGN", None),
            market(venue, "FOREIGN", false),
            market(venue, "FOREIGN", true),
        ] {
            let why = refused(plan_submit_symbol(&cmd, std::slice::from_ref(&block), None, true));
            assert!(why.contains(reason), "{venue}: {why}");
            assert!(why.contains("`FOREIGN`") && why.contains("`MOUNTED`"), "{venue}: {why}");
            assert!(why.contains(&format!("`{venue}`")) && why.contains("REFUSED"), "{why}");
            assert_eq!(
                submit_symbol_refusal(&cmd, std::slice::from_ref(&block), None),
                Some(why),
                "{venue}: the Preview prints the same"
            );
        }
        // The same engine on its own symbol is placed as before.
        assert_eq!(
            plan_submit_symbol(&submit(venue, "MOUNTED", None), &[block], None, true),
            SymbolPlan::Pass
        );
    }
}

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

/// **deribit keeps the refusal**, naming the symbol, the engine, what it trades, the table's reason,
/// and that the order was refused. A real account does not change that.
#[test]
fn deribit_still_refuses_a_foreign_symbol_with_the_tables_reason() {
    let facts = Table::eth();
    let (venue, mounted) = ("deribit", "BTC-PERPETUAL");
    let why = refused(plan_submit_symbol(
        &submit(venue, "ETH-PERPETUAL", None),
        &[engine(venue, mounted)],
        Some(&facts),
        true,
    ));
    assert!(why.contains("`ETH-PERPETUAL`"), "names the symbol asked for: {why}");
    assert!(why.contains(&format!("`{mounted}`")), "names what it trades: {why}");
    assert!(why.contains(&format!("`{venue}`")), "names the engine: {why}");
    assert!(why.contains("places every order on the symbol the engine was mounted on"), "{why}");
    assert!(why.contains("REFUSED"), "says what happened: {why}");
    // The Preview surfaces print exactly this.
    assert_eq!(
        submit_symbol_refusal(
            &submit(venue, "ETH-PERPETUAL", None),
            &[engine(venue, mounted)],
            Some(&facts)
        ),
        Some(why)
    );
    // A combo leg in `extra_symbols` is still a symbol the adapter signs on the MOUNTED instrument.
    let combo = vike_exec::VenueBlock {
        extra_symbols: vec!["ETH-PERPETUAL".into()].into(),
        ..engine(venue, mounted)
    };
    assert!(
        submit_symbol_refusal(&submit(venue, "ETH-PERPETUAL", None), &[combo], Some(&facts))
            .is_some(),
        "a combo leg is not a symbol a deribit adapter can place"
    );
    assert!(facts.asked().is_empty(), "a refused venue never looks anything up");
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
    use crate::server::symbol_admission::{PriceWait, await_price};
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

struct Node {
    core: vike_core::CoreHandle,
    submitted: Arc<Mutex<Vec<OrderRequest>>>,
    limits: ControlLimits,
}

impl Node {
    /// A `demo` engine for `venue` mounted on `symbol`, on BTC's coarse grid (tick 0.1, lot 0.001).
    fn start(venue: &'static str, symbol: &str, tier: AccountTier) -> Self {
        Self::start_with(venue, symbol, tier, &[])
    }

    /// [`Self::start`] plus one LABELLED account of the same venue per `labels` entry, each on its own
    /// scripted client (all recording into the one `submitted`), as `make_engine_accounts` builds them:
    /// the venue's canonical id on the `Account`, `venue#LABEL` on the route key.
    fn start_with(venue: &'static str, symbol: &str, tier: AccountTier, labels: &[&str]) -> Self {
        let submitted: Arc<Mutex<Vec<OrderRequest>>> = Arc::default();
        let scripted = |route_key: String| {
            let client = ScriptedVenue {
                venue,
                submitted: Arc::clone(&submitted),
                pending: VecDeque::new(),
                n: 0,
            };
            let limits =
                RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
            let mut engine = ExecutionEngine::new(
                Account::new(1.0, venue, None, BalanceMode::Delta),
                RiskGate::new(limits),
                client,
                venue,
                symbol,
            );
            engine.tier = tier;
            engine.route_key = route_key;
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

    fn blocks(&self) -> Vec<vike_exec::VenueBlock> {
        self.core.snapshot_cell().load().portfolio.venues.clone()
    }

    /// One command through the ONE acceptance path, as the TCP surface calls it.
    fn send(
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
            facts,
            None,
            None,
        )
        .map(super::control::Accepted::into_coid)
        .map_err(|e| e.message())
    }

    fn venue_saw(&self, coid: &str) -> OrderRequest {
        let found =
            || self.submitted.lock().unwrap().iter().find(|o| o.client_order_id == coid).cloned();
        wait_until("the order to reach the venue client", || found().is_some());
        found().unwrap()
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn limit(coid: &str, venue: &str, symbol: &str, qty: f64, price: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.client_order_id = coid.into();
    req.qty = qty;
    req.price = Some(price);
    WireCommand::Submit(req)
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

/// A paper engine, an engine of an unfinished venue (deribit) and a surface with no source each refuse
/// at the door, through `accept_command`, before anything is lowered.
#[test]
fn paper_deribit_and_a_surface_without_a_source_refuse_through_the_door() {
    let facts = Table::eth();
    let mut paper = Node::start("bybit", "BTCUSDT", AccountTier::Paper);
    let err = paper.send(limit("p", "bybit", "ETHUSDT", 1.0, 5.0), Some(&facts)).unwrap_err();
    assert!(err.contains("paper account"), "{err}");
    paper.core.shutdown_and_join();

    let mut deribit = Node::start("deribit", "BTC-PERPETUAL", AccountTier::Demo);
    let err =
        deribit.send(limit("o", "deribit", "ETH-PERPETUAL", 1.0, 5.0), Some(&facts)).unwrap_err();
    assert!(err.contains("places every order on the symbol the engine was mounted on"), "{err}");
    deribit.core.shutdown_and_join();

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
    let blocks = node.blocks();
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
    use crate::server::symbol_admission::{FailFast, PriceWait, await_price};
    let wait = PriceWait { route_key: "bybit".into(), venue: "bybit".into() };
    let inner: std::sync::Arc<dyn SymbolFacts> =
        std::sync::Arc::new(Table::eth().with_price(Price::Pending));
    let fast = FailFast(inner);
    let started = Instant::now();
    let err = await_price(&fast, &wait, "ETHUSDT").expect_err("no price, no wait");
    assert!(err.contains("REFUSED") && err.contains("retry"), "{err}");
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
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
    assert_eq!(advertised, admits, "bybit, binance, aster, okx and hyperliquid, and nothing else");
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
    assert!(
        !needs_lookup(&submit("deribit", "ETH-PERPETUAL", None), &roster),
        "not an admitting venue"
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
    let blocks = node.blocks();
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
