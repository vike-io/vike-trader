//! **dukascopy's pair quoted in ANOTHER money than the account's, through the control door** (decision 0183,
//! phase 1), driven through the ONE acceptance path into a REAL core (`accept_command` over
//! `admits_any_symbol`'s `Node`), exactly as `symbol_money_door.rs` does for OANDA.
//!
//! The facts are scripted at the seam these tests are about: what the gatewayhub's facts state for a pair
//! (`quote_ccy`, `quote_rate`, `rate_as_of_ms`: account-currency units per ONE unit of the quote currency, the
//! engine's orientation). The gatewayhub half (the rate from the sidecar's ticks, the account's currency from the
//! `ready` line) is `crates/vike-gatewayhub/src/trade/adapters/dukascopy_rate_tests.rs`'s and
//! `crates/bridges/dukascopy/tests/dukascopy_currency.rs`'s; the engine's own contract is
//! `crates/vike-exec/tests/engine/quote_money.rs`'s. What this file pins is the DOOR:
//!
//! * the fence of decision 0170 ("the ledger is one currency") is relaxed ONLY for a `demo` account with an
//!   account-wide ceiling armed, and ONLY with the currency and a usable rate the facts state: a gatewayhub that
//!   states none (an old one, or a jar that states no account currency) never lets a foreign pair through at par;
//! * the money then comes out in dollars: USDJPY closed 0.10 up realizes 66.62, not 10,000, and EURGBP's notional
//!   is pounds times 1.35.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_exec::SymbolAdmission;
use vike_model::{AccountTier, SymbolGrid};
use vike_tradehub_client::wire::WireCommand;

use super::admits_any_symbol::{Node, limit, wait_until};
use crate::server::control::{ControlLimits, ControlLimitsConfig};
use crate::server::symbol_admission::{SymbolFacts, SymbolPlan, plan_submit_symbol};

/// What the scripted gatewayhub states for one symbol: `(symbol, the quote currency it names, the rate it gives)`.
type Stated = (String, Option<&'static str>, Option<f64>);

/// A facts source for the pairs a scripted gatewayhub quotes in another currency, with what it states for each.
struct Gateway {
    /// What it states, symbol by symbol.
    quoted: Mutex<Vec<Stated>>,
    /// The moment the rate was asked, on the engine's clock (the harness core counts up from 0).
    as_of_ms: AtomicI64,
    /// Whether the account runs with an account-wide exposure ceiling.
    capped: bool,
}

impl Gateway {
    fn new() -> Self {
        Gateway { quoted: Mutex::new(Vec::new()), as_of_ms: AtomicI64::new(0), capped: true }
    }

    /// The gatewayhub states `symbol` is quoted in `ccy` at `rate` account units per one unit of it.
    fn quoting(self, symbol: &str, ccy: &'static str, rate: f64) -> Self {
        self.quoted.lock().unwrap().push((symbol.to_string(), Some(ccy), Some(rate)));
        self
    }

    /// The gatewayhub answers `symbol` as an UNCONVERTING lane would for a pair it should have priced: no
    /// currency, no rate (an old gatewayhub, or a jar that states no account currency).
    fn silent_about(self, symbol: &str) -> Self {
        self.quoted.lock().unwrap().push((symbol.to_string(), None, None));
        self
    }

    fn uncapped(mut self) -> Self {
        self.capped = false;
        self
    }

    fn reprice(&self, symbol: &str, rate: f64, as_of_ms: i64) {
        for row in self.quoted.lock().unwrap().iter_mut().filter(|r| r.0 == symbol) {
            row.2 = Some(rate);
        }
        self.as_of_ms.store(as_of_ms, Ordering::SeqCst);
    }

    fn looked_up(&self, route_key: &str, symbol: &str) -> SymbolAdmission {
        let row = self.quoted.lock().unwrap().iter().find(|r| r.0 == symbol).cloned();
        let (ccy, rate) = row.map_or((None, None), |(_, c, r)| (c, r));
        SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            // dukascopy's grid: the SDK's price step, the sidecar's 1000-unit floor
            grid: SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(1000.0),
                min_qty: Some(1000.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: ccy.map(str::to_string),
            quote_rate: rate,
            rate_as_of_ms: rate.map(|_| self.as_of_ms.load(Ordering::SeqCst)),
            im_requirement: None,
        }
    }
}

impl SymbolFacts for Gateway {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        Ok(self.looked_up(route_key, symbol))
    }
    fn price(&self, _: &str, _: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(match symbol {
            "USDJPY" => 150.0,
            "EURGBP" => 0.85,
            _ => 1.1,
        }))
    }
    fn refresh(&self, route_key: &str, _: &str, symbol: &str) -> Option<SymbolAdmission> {
        Some(self.looked_up(route_key, symbol))
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

/// The one published block of the node's engine.
fn block(node: &Node) -> vike_exec::VenueBlock {
    node.blocks().remove(0)
}

/// A block of a published roster: dukascopy mounted on `symbol`, at `tier`.
fn engine_at(symbol: &str, tier: AccountTier) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: "dukascopy".into(),
        route_key: "dukascopy".into(),
        symbol: symbol.into(),
        tier: Some(tier),
        ..Default::default()
    }
}

fn sell_to_close(coid: &str, symbol: &str, qty: f64, price: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, "dukascopy", symbol, qty, price) else {
        unreachable!()
    };
    req.side = -1;
    req.reduce_only = true;
    WireCommand::Submit(req)
}

fn refused(plan: SymbolPlan) -> String {
    match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn plan(cmd: &WireCommand, block: vike_exec::VenueBlock, facts: &Gateway) -> SymbolPlan {
    plan_submit_symbol(cmd, &[block], Some(facts), true)
}

/// **USDJPY on a USD account is booked in DOLLARS.** A yen is a FRACTION of a dollar (`1/150`, never `150`):
/// the position opens at 100,000 dollars of notional (not 15,000,000), and the close, which re-reads the rate
/// (`1/150.10`) before it is judged, books `10,000 / 150.10` at the rate in force when it folds.
#[test]
fn a_usdjpy_unit_closed_ten_pips_up_realizes_66_dollars_not_10000() {
    let facts = Gateway::new().quoting("USDJPY", "JPY", 1.0 / 150.0);
    let mut node = Node::start_anchored("dukascopy", "EURUSD", AccountTier::Demo, "USD");
    node.send(limit("jpy-open", "dukascopy", "USDJPY", 100_000.0, 150.0), Some(&facts))
        .expect("opened");
    node.venue_saw("jpy-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == "USDJPY" && p.size == 100_000.0)
    });
    let notional = block(&node).contract_value_of("USDJPY").order_notional(100_000.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: dollars, not 15,000,000 yen");
    let [row] = &block(&node).quote_rates[..] else { panic!("one rate") };
    assert_eq!(row.ccy, "JPY");
    assert!(
        (row.rate - 1.0 / 150.0).abs() < 1e-15,
        "a yen is a fraction of a dollar: {}",
        row.rate
    );

    facts.reprice("USDJPY", 1.0 / 150.10, 10);
    node.send(sell_to_close("jpy-close", "USDJPY", 100_000.0, 150.10), Some(&facts))
        .expect("closed");
    node.venue_saw("jpy-close");
    let want = 10_000.0 / 150.10;
    wait_until("the realized PnL", || (block(&node).realized_pnl - want).abs() < 1e-6);
    let realized = block(&node).realized_pnl;
    assert!((realized - 66.622).abs() < 1e-3, "{realized}");
    assert!(realized < 100.0, "nowhere near the 10,000 the unconverted engine books");
    node.core.shutdown_and_join();
}

/// **EURGBP on a USD account is booked in DOLLARS**: a pound is worth MORE than a dollar (`1.35`), so 85,000
/// pounds are 114,750 dollars (the single-currency engine read 85,000 "dollars", 1.35x LOW).
#[test]
fn a_cross_is_booked_in_the_accounts_currency_not_the_quote_currency() {
    let facts = Gateway::new().quoting("EURGBP", "GBP", 1.35);
    let mut node = Node::start_anchored("dukascopy", "EURUSD", AccountTier::Demo, "USD");
    node.send(limit("fx-1", "dukascopy", "EURGBP", 100_000.0, 0.85), Some(&facts))
        .expect("accepted");
    node.venue_saw("fx-1");
    wait_until("the admission to publish", || node.blocks().iter().any(|b| b.trades("EURGBP")));
    let published = block(&node);
    let notional = published.contract_value_of("EURGBP").order_notional(100_000.0, 0.85);
    assert!((notional - 114_750.0).abs() < 1e-6, "{notional}: 85,000 pounds at 1.35 dollars each");
    assert!(
        published.contract_value_of("EURUSD").is_default(),
        "the mounted pair, quoted in dollars, is untouched"
    );
    node.core.shutdown_and_join();
}

/// **The ceiling counts the cross in the ACCOUNT's currency** (the door's own number, before the order is
/// admitted): 100,000 euros at 0.85 is 114,750 dollars, refused by a ceiling of 100,000 and accepted by one of
/// 120,000, and the refusal admits nothing.
#[test]
fn the_ceiling_counts_a_cross_in_the_accounts_currency() {
    let facts = Gateway::new().quoting("EURGBP", "GBP", 1.35);
    let mut node = Node::start_anchored("dukascopy", "EURUSD", AccountTier::Demo, "USD");
    let order = limit("fx-cap", "dukascopy", "EURGBP", 100_000.0, 0.85);
    node.limits = ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(100_000.0),
        rate_per_sec: 1e9,
    });
    let over = node.send(order.clone(), Some(&facts)).expect_err("114,750 is over 100,000");
    assert!(over.contains("order notional 114750.00 exceeds"), "{over}");
    node.limits = ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(120_000.0),
        rate_per_sec: 1e9,
    });
    node.send(order, Some(&facts)).expect("114,750 is under 120,000");
    node.venue_saw("fx-cap");
    node.core.shutdown_and_join();
}

/// **The fence still refuses where it must, in words**: a `live` account, a demo account with no account-wide
/// ceiling, and a gatewayhub that states no currency (an old one, a jar that states no account currency), a
/// different currency than the pair's, or a rate that is not a number. Each leaves the pair NOT admitted, where
/// the same pair on an account that qualifies, with the facts of a gatewayhub that converts, is a placement
/// carrying the currency and the rate.
#[test]
fn the_door_admits_a_foreign_pair_only_for_an_account_that_converts_and_has_a_rate() {
    let cross = limit("c-1", "dukascopy", "EURGBP", 100_000.0, 0.85);
    let facts = Gateway::new().quoting("EURGBP", "GBP", 1.35);
    match plan(&cross, engine_at("EURUSD", AccountTier::Demo), &facts) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("GBP"), Some(1.35)));
        }
        other => panic!("a demo account with a ceiling takes it: {other:?}"),
    }

    // a live account: the old fence's own words
    let live = refused(plan(&cross, engine_at("EURUSD", AccountTier::Live), &facts));
    assert!(live.contains("quoted in GBP") && live.contains("nothing converts"), "{live}");
    // a paper account too (the door's earlier rule: a paper account trades only the symbol it was started on)
    let paper = refused(plan(&cross, engine_at("EURUSD", AccountTier::Paper), &facts));
    assert!(paper.contains("trades only the symbol it was started on"), "{paper}");
    // a demo account with no account-wide ceiling armed: the same words (and no lookup was spent)
    let uncapped = Gateway::new().quoting("EURGBP", "GBP", 1.35).uncapped();
    let why = refused(plan(&cross, engine_at("EURUSD", AccountTier::Demo), &uncapped));
    assert!(why.contains("nothing converts") && why.contains("quoted in GBP"), "{why}");

    // a gatewayhub that states no currency: the pair would be booked at par, so it is NOT admitted
    let silent = Gateway::new().silent_about("EURGBP");
    let why = refused(plan(&cross, engine_at("EURUSD", AccountTier::Demo), &silent));
    assert!(why.contains("quoted in GBP") && why.contains("state no currency"), "{why}");
    assert!(why.contains("NOT admitted") && why.contains("never valued at par"), "{why}");
    // ...or another currency than the pair's
    let wrong = Gateway::new().quoting("EURGBP", "JPY", 0.0067);
    let why = refused(plan(&cross, engine_at("EURUSD", AccountTier::Demo), &wrong));
    assert!(why.contains("quoted in GBP") && why.contains("state JPY"), "{why}");

    // a rate that is not a number
    for bad in [0.0, -1.35, f64::NAN, f64::INFINITY] {
        let unpriced = Gateway::new().quoting("EURGBP", "GBP", bad);
        let why = refused(plan(&cross, engine_at("EURUSD", AccountTier::Demo), &unpriced));
        assert!(
            why.contains("no rate, no admission") && why.contains("NOT admitted"),
            "{bad}: {why}"
        );
    }

    // an engine whose own symbol is not a pair is admitted nothing, however the account is set up
    let why = refused(plan(&cross, engine_at("AAPLUSUSD", AccountTier::Demo), &facts));
    assert!(why.contains("an unknown currency") && why.contains("nothing converts"), "{why}");

    // a pair quoted in the engine's own money: no currency, no rate, admitted as before
    let plain = limit("c-3", "dukascopy", "GBPUSD", 100_000.0, 1.3);
    match plan(&plain, engine_at("EURUSD", AccountTier::Demo), &Gateway::new()) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy, a.quote_rate, a.rate_as_of_ms), (None, None, None));
        }
        other => panic!("{other:?}"),
    }
}

/// OANDA's rule is unchanged by dukascopy joining the venues that convert, and a venue with no rate source
/// still says so, naming every venue that has one (binance stands for it: ctrader has a rate source now).
#[test]
fn a_venue_without_a_rate_source_is_refused_naming_the_venues_that_have_one() {
    let facts = Gateway::new().quoting("GBPJPY", "JPY", 1.0 / 150.0);
    let jpy = limit("c-2", "binance", "GBPJPY", 1000.0, 190.0);
    let block = vike_exec::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        symbol: "EURUSD".into(),
        tier: Some(AccountTier::Demo),
        ..Default::default()
    };
    let why = refused(plan_submit_symbol(&jpy, &[block], Some(&facts), true));
    assert!(
        why.contains("practice accounts of oanda, ctrader, ibkr, dukascopy, ig, fxcm only"),
        "{why}"
    );
    assert!(why.contains("binance has no rate source"), "{why}");
}
