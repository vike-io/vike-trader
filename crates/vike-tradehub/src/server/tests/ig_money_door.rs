//! **IG's foreign-quoted epics through the control door into a REAL core** (decision 0183): the `symbol_money_door`
//! tests' IG twin. IG is the first converting venue whose contract value is NOT the default: an IG contract is
//! 10,000 units of the base currency (`contractSize`), so the engine's effective value for a yen epic is
//! `10,000 x (1/150)` dollars per contract per yen of price, and the goldens are the decision's own at IG's size:
//! ten contracts of USDJPY at 150.00 are 100,000 dollars of notional (not 15,000,000), and a 0.10 yen move on
//! them realizes `10,000 / 150.10 = 66.62` dollars (not 10,000).
//!
//! The facts are scripted at the seam the node reads (`SymbolFacts`), shaped as the gatewayhub's IG answer is
//! (`quote_ccy`, the rate in account-per-quote orientation, the moment it was asked, IG's contract value);
//! nothing here is IG's own figure. The rule that opens the venue is the node's single list.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use vike_exec::SymbolAdmission;
use vike_model::{AccountTier, ContractValue, SymbolGrid};
use vike_tradehub_client::wire::WireCommand;

use super::admits_any_symbol::{Node, limit, wait_until};
use crate::server::symbol_admission::{
    SymbolFacts, SymbolPlan, foreign_contract_value, plan_submit_symbol, quote_converting_venues,
};

const MOUNTED: &str = "CS.D.EURUSD.MINI.IP";
const USDJPY: &str = "CS.D.USDJPY.MINI.IP";

/// A scripted gatewayhub answer for IG: every epic it is asked about is quoted in `ccy` at `rate` account
/// dollars per unit of it (a yen epic at 1/150, a pound epic at 1.35), on a pip grid of `tick`.
struct IgFacts {
    ccy: &'static str,
    tick: f64,
    ref_price: f64,
    rate: std::sync::Mutex<f64>,
    as_of_ms: AtomicI64,
    refresh_due: AtomicBool,
    capped: bool,
}

impl IgFacts {
    fn quoting(ccy: &'static str, tick: f64, ref_price: f64, rate: f64) -> Self {
        Self {
            ccy,
            tick,
            ref_price,
            rate: std::sync::Mutex::new(rate),
            as_of_ms: AtomicI64::new(0),
            refresh_due: AtomicBool::new(false),
            capped: true,
        }
    }

    fn yen(rate: f64) -> Self {
        Self::quoting("JPY", 0.01, 150.0, rate)
    }

    fn pound(rate: f64) -> Self {
        Self::quoting("GBP", 0.0001, 0.85, rate)
    }

    fn uncapped(mut self) -> Self {
        self.capped = false;
        self
    }

    /// The daemon now states `rate`, asked at `as_of_ms` on the engine's clock (the harness counts up from 0).
    fn reprice(&self, rate: f64, as_of_ms: i64) {
        *self.rate.lock().unwrap() = rate;
        self.as_of_ms.store(as_of_ms, Ordering::SeqCst);
        self.refresh_due.store(true, Ordering::SeqCst);
    }

    fn looked_up(&self, route_key: &str, symbol: &str) -> SymbolAdmission {
        SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(self.tick),
                lot_size: Some(0.1),
                min_qty: Some(0.1),
                ..SymbolGrid::default()
            },
            contract_value: Some(ContractValue::linear(10_000.0)),
            initial_mark: None,
            quote_ccy: Some(self.ccy.to_string()),
            quote_rate: Some(*self.rate.lock().unwrap()),
            rate_as_of_ms: Some(self.as_of_ms.load(Ordering::SeqCst)),
            im_requirement: None,
        }
    }
}

impl SymbolFacts for IgFacts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        Ok(self.looked_up(route_key, symbol))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(self.ref_price))
    }
    fn refresh(&self, route_key: &str, _: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.refresh_due.swap(false, Ordering::SeqCst).then(|| self.looked_up(route_key, symbol))
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

/// The one published block of the node's engine.
fn block(node: &Node) -> vike_exec::VenueBlock {
    node.blocks().remove(0)
}

fn engine_at(tier: AccountTier) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: "ig".into(),
        route_key: "ig".into(),
        symbol: MOUNTED.into(),
        tier: Some(tier),
        ..Default::default()
    }
}

fn sell_to_close(coid: &str, epic: &str, qty: f64, price: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, "ig", epic, qty, price) else { unreachable!() };
    req.side = -1;
    req.reduce_only = true;
    WireCommand::Submit(req)
}

/// **The list that opens a venue names IG beside OANDA**, and fxcm (its currency half; its door row is still
/// `PerOrderUnsafe`, so it keeps its fence for a symbol the engine was not started on).
#[test]
fn the_converting_venues_name_ig_beside_oanda() {
    let venues = quote_converting_venues();
    assert!(venues.iter().any(|v| v == "oanda") && venues.iter().any(|v| v == "ig"), "{venues:?}");
    assert!(venues.iter().any(|v| v == "fxcm"), "{venues:?}");
}

/// **An IG lease asks the gatewayhub to convert (and its start to read the account's currency) only for an
/// account that can use the answer**: demo, an exposure ceiling armed and, for the lease, the currency stated
/// (`anchored`). A demo IG account with NO ceiling used to send `quoted` on every lease, so the gatewayhub did a
/// login, `GET /accounts` and one to three market reads per epic and the node then refused the answer for want of
/// a ceiling. Another venue's rule is untouched: oanda converts whatever its ceiling, fxcm (on the list since its
/// currency half landed) converts for a demo account with a ceiling and reads nothing at its start.
#[test]
fn an_ig_lease_converts_and_reads_the_currency_only_with_a_ceiling_and_a_stated_currency() {
    use crate::server::symbol_admission::{quote_lease_converts, quote_lease_reads_currency};
    for (venue, tier, anchored, ceiling, converts, reads) in [
        ("ig", AccountTier::Demo, true, true, true, true),
        ("ig", AccountTier::Demo, true, false, false, false),
        ("ig", AccountTier::Demo, false, true, false, true),
        ("ig", AccountTier::Demo, false, false, false, false),
        ("ig", AccountTier::Live, true, true, false, false),
        ("ig", AccountTier::Paper, true, true, false, false),
        ("oanda", AccountTier::Demo, false, false, true, false),
        ("fxcm", AccountTier::Demo, true, true, true, false),
    ] {
        assert_eq!(
            quote_lease_converts(venue, tier, anchored, ceiling),
            converts,
            "{venue} {tier:?} anchored {anchored} ceiling {ceiling}: converts"
        );
        assert_eq!(
            quote_lease_reads_currency(venue, tier, ceiling),
            reads,
            "{venue} {tier:?} ceiling {ceiling}: reads the currency"
        );
    }
}

/// **Ten contracts of USDJPY on a USD account are booked in DOLLARS, at IG's contract size.** The door admits
/// the epic with its currency and rate, the engine's published value for it is `10,000 x (1/150)`, the position
/// opens at 100,000 dollars of notional, and the close, which re-reads the rate before it is judged, books
/// `10,000 / 150.10` at the rate in force when it folds. The mounted EURUSD epic is untouched.
#[test]
fn ten_contracts_of_usdjpy_are_a_hundred_thousand_dollars_and_a_move_of_ten_pips_is_66() {
    let facts = IgFacts::yen(1.0 / 150.0);
    let mut node = Node::start_anchored("ig", MOUNTED, AccountTier::Demo, "USD");
    node.send(limit("ig-open", "ig", USDJPY, 10.0, 150.0), Some(&facts)).expect("opened");
    node.venue_saw("ig-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == USDJPY && p.size == 10.0)
    });
    let notional = block(&node).contract_value_of(USDJPY).order_notional(10.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: dollars, not 15,000,000 yen");
    let [row] = &block(&node).quote_rates[..] else { panic!("{:?}", block(&node).quote_rates) };
    assert_eq!((row.ccy.as_str(), row.rate, row.stale), ("JPY", 1.0 / 150.0, false));
    assert!(block(&node).contract_value_of(MOUNTED).is_default(), "the mounted epic is untouched");

    facts.reprice(1.0 / 150.10, 10);
    node.send(sell_to_close("ig-close", USDJPY, 10.0, 150.10), Some(&facts)).expect("closed");
    node.venue_saw("ig-close");
    let want = 10.0 * 10_000.0 * 0.10 / 150.10;
    wait_until("the realized PnL", || (block(&node).realized_pnl - want).abs() < 1e-6);
    assert!((block(&node).realized_pnl - 66.622).abs() < 1e-3);
    assert!(
        block(&node).realized_pnl < 100.0,
        "nowhere near the 10,000 an unconverted engine books"
    );
    node.core.shutdown_and_join();
}

/// **One contract of EURGBP on a USD account is booked in DOLLARS, and a pound is MORE than a dollar.** 10,000
/// euros at 0.8500 are 8,500 pounds = 11,475 dollars (raw, 8,500 "dollars" would be 1.35x LOW), and a move of
/// 0.0010 up realizes 10 pounds = 13.50 dollars, not 10.
#[test]
fn one_contract_of_eurgbp_is_booked_in_dollars_and_a_pound_is_more_than_one() {
    const EURGBP: &str = "CS.D.EURGBP.MINI.IP";
    let facts = IgFacts::pound(1.35);
    let mut node = Node::start_anchored("ig", MOUNTED, AccountTier::Demo, "USD");
    node.send(limit("gb-open", "ig", EURGBP, 1.0, 0.8500), Some(&facts)).expect("opened");
    node.venue_saw("gb-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == EURGBP && p.size == 1.0)
    });
    let notional = block(&node).contract_value_of(EURGBP).order_notional(1.0, 0.85);
    assert!((notional - 11_475.0).abs() < 1e-6, "{notional}: 8,500 pounds at 1.35 dollars each");
    let [row] = &block(&node).quote_rates[..] else { panic!("{:?}", block(&node).quote_rates) };
    assert_eq!((row.ccy.as_str(), row.rate, row.stale), ("GBP", 1.35, false));

    facts.reprice(1.35, 10);
    node.send(sell_to_close("gb-close", EURGBP, 1.0, 0.8510), Some(&facts)).expect("closed");
    node.venue_saw("gb-close");
    wait_until("the realized PnL", || (block(&node).realized_pnl - 13.5).abs() < 1e-6);
    assert!(block(&node).realized_pnl > 10.0, "more than the 10 an unconverted engine books");
    node.core.shutdown_and_join();
}

/// **The ceiling counts the epic in the ACCOUNT's currency before it is admitted**: the door sizes it with the
/// facts' contract value times the rate (`foreign_contract_value`), where it used to read the contract alone.
#[test]
fn the_door_sizes_an_unadmitted_yen_epic_with_the_contract_times_the_rate() {
    let facts = IgFacts::yen(1.0 / 150.0);
    let node = Node::start_anchored("ig", MOUNTED, AccountTier::Demo, "USD");
    let order = limit("ig-cap", "ig", USDJPY, 10.0, 150.0);
    let value = foreign_contract_value(&order, &node.blocks(), Some(&facts)).expect("sized");
    let notional = value.order_notional(10.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: dollars, not 15,000,000 yen");
    node.core.shutdown_and_join();
}

/// **Only a `demo` IG account with an account-wide ceiling converts**: a `live` account, an uncapped demo
/// account and a rate that is not a number each leave the epic NOT admitted, in words, where the same epic on
/// an account that qualifies is a placement carrying the currency and the rate.
#[test]
fn the_door_admits_a_yen_epic_only_for_a_demo_ig_account_with_a_ceiling_and_a_rate() {
    let plan = |block: vike_exec::VenueBlock, facts: &IgFacts| {
        plan_submit_symbol(&limit("ig-1", "ig", USDJPY, 10.0, 150.0), &[block], Some(facts), true)
    };
    let refused = |plan: SymbolPlan| match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    match plan(engine_at(AccountTier::Demo), &IgFacts::yen(1.0 / 150.0)) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("JPY"), Some(1.0 / 150.0)));
            assert_eq!(a.contract_value, Some(ContractValue::linear(10_000.0)));
        }
        other => panic!("a demo account with a ceiling takes it: {other:?}"),
    }
    let live = refused(plan(engine_at(AccountTier::Live), &IgFacts::yen(1.0 / 150.0)));
    assert!(live.contains("`demo` account only") && live.contains("`live`"), "{live}");
    // a paper account never gets as far as the conversion rule: it trades only the symbol it started on
    let paper = refused(plan(engine_at(AccountTier::Paper), &IgFacts::yen(1.0 / 150.0)));
    assert!(paper.contains("a paper account trades only the symbol it was started on"), "{paper}");
    let uncapped =
        refused(plan(engine_at(AccountTier::Demo), &IgFacts::yen(1.0 / 150.0).uncapped()));
    assert!(
        uncapped.contains("exposure ceiling") && uncapped.contains("quoted in JPY"),
        "{uncapped}"
    );
    for bad in [0.0, -1.0 / 150.0, f64::NAN, f64::INFINITY] {
        let why = refused(plan(engine_at(AccountTier::Demo), &IgFacts::yen(bad)));
        assert!(
            why.contains("no rate, no admission") && why.contains("NOT admitted"),
            "{bad}: {why}"
        );
    }
}
