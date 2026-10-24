//! The two per-symbol MONEY facts the control door carries into an admission, driven through the ONE
//! acceptance path into a REAL core (`accept_command` over `admits_any_symbol`'s `Node`):
//!
//! * **the symbol's own leverage** (decision 0182's per-symbol twin). The daemon's commit makes the venue
//!   hold the account's figure for the symbol and reports the figure it SAW; the door restates the
//!   admission with `im_requirement = 1 / figure` ahead of the order, so the engine margins the symbol at
//!   the leverage the venue holds. Nothing confirmed, nothing restated.
//!
//! The observable is `PositionView::leverage` (`1 / im_for(symbol)`) of the position the order's fill opens.
//!
//! * **a symbol quoted in another currency than the account's** (decision 0183, phase 1). The facts carry the
//!   quote currency and a rate (account-currency units per ONE unit of it); the door admits the symbol only for
//!   an account the rule opens (OANDA, `demo`, an account-wide ceiling armed) and only with a usable rate, and
//!   the engine then books it in the account's currency: EURGBP's notional is pounds times 1.35, and a USDJPY
//!   unit closed 0.10 above its entry realizes `10,000 / 150.10`, not 10,000. The observables are the published
//!   block's contract value, rate table and realized PnL.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use vike_exec::SymbolAdmission;
use vike_model::{AccountTier, SymbolGrid};
use vike_tradehub_client::wire::WireCommand;

use super::admits_any_symbol::{Node, limit, wait_until};
use crate::server::control::{ControlLimits, ControlLimitsConfig};
use crate::server::symbol_admission::{
    SymbolFacts, SymbolPlan, foreign_contract_value, plan_submit_symbol,
};

/// A facts source scripted at the seam these tests are about: what the daemon's COMMIT confirms, symbol
/// by symbol. It keeps the last commit's figure and stamps it on every answer, as `LeaseFacts` does.
struct Source {
    /// The leverage the scripted daemon confirms at a commit, by symbol (absent: confirms nothing).
    confirms: Mutex<HashMap<String, f64>>,
    /// The figure the last commit left, by symbol.
    kept: Mutex<HashMap<String, f64>>,
    /// Every commit, in order.
    commits: Mutex<Vec<String>>,
    /// Whether `refresh` hands out a fresh answer (a stale admission) on the next ask.
    refresh_due: AtomicBool,
    /// The symbols the daemon quotes in another currency than the account's: `(currency, rate)` where the
    /// rate is account-currency units per ONE unit of that currency.
    quoted: Mutex<HashMap<String, (&'static str, f64)>>,
    /// The moment the rate was asked, on the engine's clock (the harness core counts up from 0).
    as_of_ms: AtomicI64,
    /// Whether the account runs with an account-wide exposure ceiling.
    capped: bool,
}

impl Source {
    fn confirming(figures: &[(&str, f64)]) -> Self {
        Source {
            confirms: Mutex::new(figures.iter().map(|(s, x)| ((*s).to_string(), *x)).collect()),
            kept: Mutex::new(HashMap::new()),
            commits: Mutex::new(Vec::new()),
            refresh_due: AtomicBool::new(false),
            quoted: Mutex::new(HashMap::new()),
            as_of_ms: AtomicI64::new(0),
            capped: true,
        }
    }

    /// A source whose daemon quotes `symbol` in `ccy` at `rate` account units per one unit of it.
    fn quoting(self, symbol: &str, ccy: &'static str, rate: f64) -> Self {
        self.quoted.lock().unwrap().insert(symbol.to_string(), (ccy, rate));
        self
    }

    fn uncapped(mut self) -> Self {
        self.capped = false;
        self
    }

    /// The daemon now states `rate` for `symbol`, asked at `as_of_ms` on the engine's clock.
    fn reprice(&self, symbol: &str, rate: f64, as_of_ms: i64) {
        if let Some(row) = self.quoted.lock().unwrap().get_mut(symbol) {
            row.1 = rate;
        }
        self.as_of_ms.store(as_of_ms, Ordering::SeqCst);
        self.refresh_due.store(true, Ordering::SeqCst);
    }

    fn commits(&self) -> Vec<String> {
        self.commits.lock().unwrap().clone()
    }

    fn looked_up(&self, route_key: &str, symbol: &str) -> SymbolAdmission {
        let quoted = self.quoted.lock().unwrap().get(symbol).copied();
        SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(0.001),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: quoted.map(|(ccy, _)| ccy.to_string()),
            quote_rate: quoted.map(|(_, rate)| rate),
            rate_as_of_ms: quoted.map(|_| self.as_of_ms.load(Ordering::SeqCst)),
            im_requirement: self.kept.lock().unwrap().get(symbol).map(|x| 1.0 / x),
        }
    }
}

impl SymbolFacts for Source {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        Ok(self.looked_up(route_key, symbol))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(3000.0))
    }
    fn refresh(&self, route_key: &str, _: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.refresh_due.swap(false, Ordering::SeqCst).then(|| self.looked_up(route_key, symbol))
    }
    fn admitted(&self, _: &str, _: &str, symbol: &str) {
        self.commits.lock().unwrap().push(symbol.to_string());
        let figure = self.confirms.lock().unwrap().get(symbol).copied();
        let mut kept = self.kept.lock().unwrap();
        match figure {
            Some(x) => {
                kept.insert(symbol.to_string(), x);
            }
            None => {
                kept.remove(symbol);
            }
        }
    }
    fn confirmed_leverage(&self, _: &str, symbol: &str) -> Option<f64> {
        self.kept.lock().unwrap().get(symbol).copied()
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

/// The published leverage (`1 / im`) of the position in `symbol`, once there is one.
fn leverage_of(node: &Node, symbol: &str) -> Option<f64> {
    node.core
        .snapshot_cell()
        .load()
        .portfolio
        .venues
        .first()?
        .positions
        .iter()
        .find(|p| p.symbol == symbol)
        .map(|p| p.leverage)
}

/// **The headline.** The account's engine margins at 1x (the mount's "no leverage unless asked"); the
/// venue holds ETHUSDT at 10x for the account, which the daemon's commit confirmed: the position the
/// order opens is held at 10x, and the mounted symbol is untouched.
#[test]
fn a_symbol_the_venue_holds_at_the_accounts_leverage_is_margined_at_it() {
    let facts = Source::confirming(&[("ETHUSDT", 10.0)]);
    let mut node = Node::start_margined("bybit", "BTCUSDT", AccountTier::Demo, 1.0);
    node.send(limit("lev-1", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("accepted");
    wait_until("ETHUSDT's position at its own margin rate", || {
        leverage_of(&node, "ETHUSDT") == Some(10.0)
    });
    assert_eq!(facts.commits(), ["ETHUSDT"], "one commit: the admission the order rode");
    node.core.shutdown_and_join();
}

/// **Nothing confirmed, nothing restated**: the daemon confirmed no figure (a spot symbol, a refusal at the
/// venue, an older daemon), so the symbol is admitted exactly as before and margined at the engine-wide
/// rate. Here the engine-wide rate is 1/2, so the position reads 2x, not any perp figure.
#[test]
fn a_symbol_with_no_confirmed_leverage_keeps_the_engine_wide_rate() {
    let facts = Source::confirming(&[]);
    let mut node = Node::start_margined("bybit", "BTCUSDT", AccountTier::Demo, 0.5);
    node.send(limit("lev-2", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("accepted");
    wait_until("ETHUSDT's position", || leverage_of(&node, "ETHUSDT").is_some());
    assert_eq!(leverage_of(&node, "ETHUSDT"), Some(2.0), "1 / the engine-wide 0.5");
    assert_eq!(facts.commits(), ["ETHUSDT"]);
    node.core.shutdown_and_join();
}

/// **A later commit that confirms another figure replaces the symbol's row.** The first order admits the
/// symbol at 10x; the venue is later read holding 5x, and the next order's refresh (which carries the figure
/// the earlier commit left, 10x) is followed by the restatement of what the new commit confirmed: 5x.
#[test]
fn a_later_commit_with_another_figure_replaces_the_symbols_margin_rate() {
    let facts = Source::confirming(&[("ETHUSDT", 10.0)]);
    let mut node = Node::start_margined("bybit", "BTCUSDT", AccountTier::Demo, 1.0);
    node.send(limit("lev-3a", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("first");
    wait_until("the 10x position", || leverage_of(&node, "ETHUSDT") == Some(10.0));
    let cell = node.core.snapshot_cell();
    wait_until("the admission to publish", || {
        cell.load().portfolio.venues.first().is_some_and(|b| b.trades("ETHUSDT"))
    });

    facts.confirms.lock().unwrap().insert("ETHUSDT".into(), 5.0);
    facts.refresh_due.store(true, Ordering::SeqCst);
    node.send(limit("lev-3b", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("second");
    wait_until("the position at the new figure", || leverage_of(&node, "ETHUSDT") == Some(5.0));
    assert_eq!(facts.commits(), ["ETHUSDT", "ETHUSDT"], "the refresh was committed again");
    node.core.shutdown_and_join();
}

/// **A symbol the engine already trades, with nothing stale, sends nothing and commits nothing**: the
/// second order on an admitted symbol does not restate (no admission rides it), so the row stays.
#[test]
fn an_order_on_an_admitted_symbol_with_nothing_due_restates_nothing() {
    let facts = Source::confirming(&[("ETHUSDT", 10.0)]);
    let mut node = Node::start_margined("bybit", "BTCUSDT", AccountTier::Demo, 1.0);
    node.send(limit("lev-4a", "bybit", "ETHUSDT", 1.0, 3000.0), Some(&facts)).expect("first");
    wait_until("the 10x position", || leverage_of(&node, "ETHUSDT") == Some(10.0));
    let cell = node.core.snapshot_cell();
    wait_until("the admission to publish", || {
        cell.load().portfolio.venues.first().is_some_and(|b| b.trades("ETHUSDT"))
    });
    node.send(limit("lev-4b", "bybit", "ETHUSDT", 1.0, 3001.0), Some(&facts)).expect("second");
    node.venue_saw("lev-4b");
    assert_eq!(facts.commits(), ["ETHUSDT"], "no second commit");
    assert_eq!(leverage_of(&node, "ETHUSDT"), Some(10.0));
    node.core.shutdown_and_join();
}

// ---- a symbol quoted in another currency than the account's ----------------------------------------------

/// The one published block of the node's engine.
fn block(node: &Node) -> vike_exec::VenueBlock {
    node.blocks().remove(0)
}

/// A block of a published roster: `venue` mounted on `symbol`, at `tier`.
fn engine_at(venue: &str, symbol: &str, tier: AccountTier) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: venue.into(),
        route_key: venue.into(),
        symbol: symbol.into(),
        tier: Some(tier),
        ..Default::default()
    }
}

fn sell_to_close(coid: &str, venue: &str, symbol: &str, qty: f64, price: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, venue, symbol, qty, price) else {
        unreachable!()
    };
    req.side = -1;
    req.reduce_only = true;
    WireCommand::Submit(req)
}

/// **EURGBP on a USD account is booked in DOLLARS.** The venue quotes it in pounds, and a pound is worth MORE
/// than a dollar (`1.35`): 100,000 euros at 0.85 is 85,000 pounds, 114,750 dollars, where the single-currency
/// engine read 85,000 "dollars" (1.35x LOW). The door admits it with the quote currency and the rate, the
/// engine's published contract value is the unit times the rate, and the rate table says which rate that was.
#[test]
fn a_cross_is_booked_in_the_accounts_currency_not_the_quote_currency() {
    let facts = Source::confirming(&[]).quoting("EURGBP", "GBP", 1.35);
    let mut node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    node.send(limit("fx-1", "oanda", "EURGBP", 100_000.0, 0.85), Some(&facts)).expect("accepted");
    node.venue_saw("fx-1");
    wait_until("the admission to publish", || node.blocks().iter().any(|b| b.trades("EURGBP")));
    let published = block(&node);
    let notional = published.contract_value_of("EURGBP").order_notional(100_000.0, 0.85);
    assert!((notional - 114_750.0).abs() < 1e-6, "{notional}: 85,000 pounds at 1.35 dollars each");
    let [row] = &published.quote_rates[..] else { panic!("one rate: {:?}", published.quote_rates) };
    assert_eq!(row.ccy, "GBP");
    assert_eq!((row.rate, row.stale), (1.35, false));
    assert!(!published.quote_rates_stale());
    assert!(
        published.contract_value_of("EURUSD").is_default(),
        "the mounted pair, quoted in dollars, is untouched"
    );
    node.core.shutdown_and_join();
}

/// **A USDJPY unit closed 0.10 above its entry realizes 66.62 dollars, not 10,000.** A yen is a FRACTION of a
/// dollar (`1/150`, never `150`): the position opens at 100,000 dollars of notional (not 15,000,000), and the
/// close, which re-reads the rate (`1/150.10`) before it is judged, books `10,000 / 150.10` at the rate in force
/// when it folds. The same fill in the single-currency engine books 10,000 "dollars".
#[test]
fn a_usdjpy_unit_closed_ten_pips_up_realizes_66_dollars_not_10000() {
    let facts = Source::confirming(&[]).quoting("USDJPY", "JPY", 1.0 / 150.0);
    let mut node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    node.send(limit("jpy-open", "oanda", "USDJPY", 100_000.0, 150.0), Some(&facts))
        .expect("opened");
    node.venue_saw("jpy-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == "USDJPY" && p.size == 100_000.0)
    });
    let notional = block(&node).contract_value_of("USDJPY").order_notional(100_000.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: dollars, not 15,000,000 yen");

    // The close: the rate is asked again before the order is judged, and rides the order's lane.
    facts.reprice("USDJPY", 1.0 / 150.10, 10);
    node.send(sell_to_close("jpy-close", "oanda", "USDJPY", 100_000.0, 150.10), Some(&facts))
        .expect("closed");
    node.venue_saw("jpy-close");
    let want = 10_000.0 / 150.10;
    wait_until("the realized PnL", || (block(&node).realized_pnl - want).abs() < 1e-6);
    let realized = block(&node).realized_pnl;
    assert!((realized - 66.622).abs() < 1e-3, "{realized}");
    assert!(realized < 100.0, "nowhere near the 10,000 the unconverted engine books");
    node.core.shutdown_and_join();
}

/// **cTrader books the same two goldens through the same core** (decision 0183, phase 1; the facts are scripted
/// docs-shaped, as the gatewayhub's cTrader rate would state them, `crate::quote_rate`): EURGBP on a USD
/// account is 114,750 dollars of notional (a pound is worth MORE than a dollar), and a USDJPY unit closed 0.10
/// above its entry realizes `10,000 / 150.10` = 66.62 dollars (a yen is a FRACTION of one), not 10,000.
#[test]
fn ctrader_books_a_pound_cross_and_a_yen_pair_in_the_accounts_currency() {
    let facts = Source::confirming(&[]).quoting("EURGBP", "GBP", 1.35).quoting(
        "USDJPY",
        "JPY",
        1.0 / 150.0,
    );
    let mut node = Node::start_anchored("ctrader", "EURUSD", AccountTier::Demo, "USD");
    node.send(limit("ct-gbp", "ctrader", "EURGBP", 100_000.0, 0.85), Some(&facts))
        .expect("accepted");
    node.venue_saw("ct-gbp");
    wait_until("the admission to publish", || node.blocks().iter().any(|b| b.trades("EURGBP")));
    let notional = block(&node).contract_value_of("EURGBP").order_notional(100_000.0, 0.85);
    assert!((notional - 114_750.0).abs() < 1e-6, "{notional}: 85,000 pounds at 1.35 dollars each");

    node.send(limit("ct-jpy-open", "ctrader", "USDJPY", 100_000.0, 150.0), Some(&facts))
        .expect("opened");
    node.venue_saw("ct-jpy-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == "USDJPY" && p.size == 100_000.0)
    });
    let jpy = block(&node).contract_value_of("USDJPY").order_notional(100_000.0, 150.0);
    assert!((jpy - 100_000.0).abs() < 1e-6, "{jpy}: dollars, not 15,000,000 yen");
    facts.reprice("USDJPY", 1.0 / 150.10, 10);
    node.send(sell_to_close("ct-jpy-close", "ctrader", "USDJPY", 100_000.0, 150.10), Some(&facts))
        .expect("closed");
    node.venue_saw("ct-jpy-close");
    let want = 10_000.0 / 150.10;
    wait_until("the realized PnL", || (block(&node).realized_pnl - want).abs() < 1e-6);
    assert!(
        block(&node).realized_pnl < 100.0,
        "nowhere near the 10,000 an unconverted engine books"
    );
    node.core.shutdown_and_join();
}

/// **The notional ceiling counts the cross in the ACCOUNT's currency**, for a symbol not admitted yet too: the
/// door sizes it with the facts' unit times the rate (`foreign_contract_value`), where it used to read the
/// unit alone. 100,000 euros at 0.85 is 114,750 dollars: refused by a ceiling of 100,000 (raw, 85,000 would
/// have passed), accepted by one of 120,000, and the refusal admits nothing.
#[test]
fn the_ceiling_counts_a_cross_in_the_accounts_currency() {
    let facts = Source::confirming(&[]).quoting("EURGBP", "GBP", 1.35);
    let mut node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    let order = limit("fx-cap", "oanda", "EURGBP", 100_000.0, 0.85);
    assert_eq!(
        foreign_contract_value(&order, &node.blocks(), Some(&facts)),
        Some(vike_model::ContractValue::linear(1.35)),
        "the unit times the rate"
    );
    node.limits = ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(100_000.0),
        rate_per_sec: 1e9,
    });
    let over = node.send(order.clone(), Some(&facts)).expect_err("114,750 is over 100,000");
    assert!(over.contains("order notional 114750.00 exceeds"), "{over}");
    assert!(facts.commits().is_empty(), "a refused order admits nothing");
    node.limits = ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(120_000.0),
        rate_per_sec: 1e9,
    });
    node.send(order, Some(&facts)).expect("114,750 is under 120,000");
    node.venue_saw("fx-cap");
    node.core.shutdown_and_join();
}

/// **The door refuses a cross it must not admit, in words**: a `live` account (the conversion is `demo`
/// only), a demo account with no account-wide ceiling, a venue with no rate source, and a rate that is not a
/// number each leave the symbol NOT admitted, where the same cross on an account that qualifies is a placement
/// carrying the currency and the rate. The interim fence is the daemon's: a source that answers a
/// plain pair carries no currency and is admitted as before.
#[test]
fn the_door_admits_a_cross_only_for_an_account_that_converts_and_has_a_rate() {
    let plan = |cmd: &WireCommand, block: vike_exec::VenueBlock, facts: &Source| {
        plan_submit_symbol(cmd, &[block], Some(facts), true)
    };
    let refused = |plan: SymbolPlan| match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let cross = limit("c-1", "oanda", "EURGBP", 1000.0, 0.85);

    let facts = Source::confirming(&[]).quoting("EURGBP", "GBP", 1.35);
    match plan(&cross, engine_at("oanda", "EURUSD", AccountTier::Demo), &facts) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("GBP"), Some(1.35)));
        }
        other => panic!("a demo account with a ceiling takes it: {other:?}"),
    }
    // cTrader has a rate source too (the mid of the conversion pair): a JPY-quoted pair on a demo account with a
    // ceiling is a placement carrying the currency and the rate (a yen is a FRACTION of a dollar), and the same
    // pair on a live ctrader account or one with no ceiling is refused like OANDA's.
    let yen = limit("c-4", "ctrader", "GBPJPY", 1000.0, 190.0);
    let yen_facts = Source::confirming(&[]).quoting("GBPJPY", "JPY", 1.0 / 150.0);
    match plan(&yen, engine_at("ctrader", "EURUSD", AccountTier::Demo), &yen_facts) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("JPY"), Some(1.0 / 150.0)));
        }
        other => panic!("a ctrader demo account with a ceiling takes it: {other:?}"),
    }
    let live_yen =
        refused(plan(&yen, engine_at("ctrader", "EURUSD", AccountTier::Live), &yen_facts));
    assert!(live_yen.contains("`demo` account only") && live_yen.contains("GBPJPY"), "{live_yen}");
    let capless = Source::confirming(&[]).quoting("GBPJPY", "JPY", 1.0 / 150.0).uncapped();
    let why = refused(plan(&yen, engine_at("ctrader", "EURUSD", AccountTier::Demo), &capless));
    assert!(why.contains("exposure ceiling") && why.contains("quoted in JPY"), "{why}");

    let live = refused(plan(&cross, engine_at("oanda", "EURUSD", AccountTier::Live), &facts));
    assert!(live.contains("`demo` account only") && live.contains("`live`"), "{live}");
    assert!(live.contains("REFUSED") && live.contains("EURGBP"), "{live}");

    let uncapped = Source::confirming(&[]).quoting("EURGBP", "GBP", 1.35).uncapped();
    let why = refused(plan(&cross, engine_at("oanda", "EURUSD", AccountTier::Demo), &uncapped));
    assert!(why.contains("exposure ceiling") && why.contains("quoted in GBP"), "{why}");

    // A venue with no rate source: the daemon never quotes it, and a facts source that did is refused.
    let wandering = Source::confirming(&[]).quoting("GBPJPY", "JPY", 1.0 / 150.0);
    let jpy = limit("c-2", "binance", "GBPJPY", 1000.0, 190.0);
    let why = refused(plan(&jpy, engine_at("binance", "EURUSD", AccountTier::Demo), &wandering));
    assert!(
        why.contains("practice accounts of oanda, ctrader, ibkr, dukascopy, ig, fxcm only")
            && why.contains("binance has no rate source"),
        "{why}"
    );

    for bad in [0.0, -1.35, f64::NAN, f64::INFINITY] {
        let unpriced = Source::confirming(&[]).quoting("EURGBP", "GBP", bad);
        let why = refused(plan(&cross, engine_at("oanda", "EURUSD", AccountTier::Demo), &unpriced));
        assert!(
            why.contains("no rate, no admission") && why.contains("NOT admitted"),
            "{bad}: {why}"
        );
    }

    // A pair quoted in the account's own currency: no currency, no rate, admitted as before.
    let plain = limit("c-3", "oanda", "GBPUSD", 1000.0, 1.3);
    match plan(&plain, engine_at("oanda", "EURUSD", AccountTier::Demo), &Source::confirming(&[])) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy, a.quote_rate, a.rate_as_of_ms), (None, None, None));
        }
        other => panic!("{other:?}"),
    }
}

/// **fxcm converts a quote currency on a capped `demo` account and nowhere else** (decision 0183, FXCM): the
/// venue is on the converting list, so the engine's ledger is anchored to the account's currency (the venue's
/// word or the ruling's assumed USD) under the same opt-in as OANDA, and since its price lane the door is
/// `Admits`: the rule itself, and each tier's answer for a foreign-quoted pair.
#[test]
fn fxcm_converts_only_on_a_capped_demo_account() {
    use crate::server::symbol_admission::{quote_conversion_refusal, quote_converting_venues};
    assert!(quote_converting_venues().iter().any(|v| v == "fxcm"));
    assert_eq!(quote_conversion_refusal("fxcm", Some(AccountTier::Demo), true), None);
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = quote_conversion_refusal("fxcm", Some(tier), true).expect("refused");
        assert!(why.contains("`demo` account only"), "{tier:?}: {why}");
    }
    let why = quote_conversion_refusal("fxcm", Some(AccountTier::Demo), false).expect("refused");
    assert!(why.contains("exposure ceiling"), "{why}");
}

/// **fxcm's door admits a pair its engine was not started on**, with the facts and the price the venue's price
/// lane answers: a pair in the account's own money on a `demo` account as it is; a foreign-quoted pair (USDJPY on
/// a USD account) with its currency and its rate, on a capped `demo` account only; never on a `paper` account, a
/// `live` one or a `demo` one with no account-wide ceiling. Each refusal names its cause and admits nothing, and
/// the slash spelling is the same pair (`USD/JPY` is placed as `USDJPY`).
#[test]
fn fxcms_door_admits_a_pair_in_the_accounts_money_and_a_foreign_one_only_at_its_rate() {
    let plan = |cmd: &WireCommand, block: vike_exec::VenueBlock, facts: &Source| {
        plan_submit_symbol(cmd, &[block], Some(facts), true)
    };
    let refused = |plan: SymbolPlan| match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let yen = limit("c-fx", "fxcm", "USDJPY", 1000.0, 150.10);
    let facts = Source::confirming(&[]).quoting("USDJPY", "JPY", 1.0 / 150.10);

    // demo + a ceiling: admitted, carrying the currency and the rate.
    match plan(&yen, engine_at("fxcm", "EURUSD", AccountTier::Demo), &facts) {
        SymbolPlan::Place { symbol, admit: Some(a), .. } => {
            assert_eq!(symbol, "USDJPY");
            assert_eq!(a.quote_ccy.as_deref(), Some("JPY"));
            assert_eq!(a.quote_rate, Some(1.0 / 150.10));
        }
        other => panic!("a capped demo account takes it: {other:?}"),
    }
    // the slash is the same pair
    let slashed = limit("c-fx2", "fxcm", "usd/jpy", 1000.0, 150.10);
    assert!(matches!(
        plan(&slashed, engine_at("fxcm", "EURUSD", AccountTier::Demo), &facts),
        SymbolPlan::Place { symbol, admit: Some(_), .. } if symbol == "USDJPY"
    ));
    // the engine's own pair under the other spelling is the engine's own symbol, with no lookup
    let own = limit("c-own", "fxcm", "EUR/USD", 1000.0, 1.1);
    assert!(matches!(
        plan(&own, engine_at("fxcm", "EURUSD", AccountTier::Demo), &facts),
        SymbolPlan::Place { symbol, admit: None, .. } if symbol == "EURUSD"
    ));

    // a live account and a demo one with no ceiling do not convert, so the pair-money fence is the answer
    let live = refused(plan(&yen, engine_at("fxcm", "EURUSD", AccountTier::Live), &facts));
    assert!(live.contains("quoted in JPY") && live.contains("nothing converts"), "{live}");
    let paper = refused(plan(&yen, engine_at("fxcm", "EURUSD", AccountTier::Paper), &facts));
    assert!(paper.contains("paper account trades only"), "{paper}");
    let uncapped = Source::confirming(&[]).quoting("USDJPY", "JPY", 1.0 / 150.10).uncapped();
    let why = refused(plan(&yen, engine_at("fxcm", "EURUSD", AccountTier::Demo), &uncapped));
    assert!(why.contains("quoted in JPY") && why.contains("nothing converts"), "{why}");
    // a gatewayhub that states no currency for a pair in another money: it would be booked at par
    let silent = Source::confirming(&[]);
    let why = refused(plan(&yen, engine_at("fxcm", "EURUSD", AccountTier::Demo), &silent));
    assert!(why.contains("quoted in JPY") && why.contains("no currency"), "{why}");
    assert!(why.contains("NOT admitted") && why.contains("never valued at par"), "{why}");
    let unrated = Source::confirming(&[]).quoting("USDJPY", "JPY", 0.0);
    let why = refused(plan(&yen, engine_at("fxcm", "EURUSD", AccountTier::Demo), &unrated));
    assert!(why.contains("no rate, no admission"), "{why}");

    // a pair in the account's own money: no currency, no rate, no ceiling needed
    let plain = limit("c-gbp", "fxcm", "GBPUSD", 1000.0, 1.35);
    match plan(
        &plain,
        engine_at("fxcm", "EURUSD", AccountTier::Demo),
        &Source::confirming(&[]).uncapped(),
    ) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy, a.quote_rate, a.rate_as_of_ms), (None, None, None));
        }
        other => panic!("a pair in the account's own money is admitted as before: {other:?}"),
    }
    // a name that is not a pair is refused by the door itself, in words that name no other venue
    let junk = limit("c-bad", "fxcm", "EUR USD!", 1000.0, 1.1);
    let why = refused(plan(&junk, engine_at("fxcm", "EURUSD", AccountTier::Demo), &facts));
    assert!(why.contains("not an instrument name") && !why.contains("Dukascopy"), "{why}");
}

/// **Only fxcm's POUND account moves off the venue's wired mount** (`currency_mount`): the wired `EURUSD` is quoted
/// in dollars, which its ledger books at par, so a GBP account could not anchor its ledger on it (the node held the
/// balance back and refused every foreign-quoted pair: measured on the demo account, 2026-10-20). The pound pair
/// `EURGBP` can. Every other case keeps the symbol it has, which is today's behaviour: a USD account (the wired
/// mount already carries it) and an UNSTATED one (the node assumes USD), any other money (held and refused, as
/// before: the table has no row), a mount somebody chose, and every other venue.
#[test]
fn only_fxcms_pound_account_moves_off_the_wired_mount() {
    use crate::server::symbol_admission::{CURRENCY_MOUNTS, currency_mount, unanchorable_mount};
    let wired = vike_venues::venue_wiring::wired_symbol("fxcm").expect("fxcm has an engine row");
    assert_eq!(wired, "EURUSD", "PREMISE: the row this rule exists for");
    assert_eq!(currency_mount("fxcm", wired, "GBP"), Some("EURGBP"));
    assert_eq!(
        currency_mount("fxcm", wired, "gbp"),
        Some("EURGBP"),
        "the code's case is no matter"
    );
    // The wired mount carries a dollar ledger already: nothing moves (a USD account is byte-identical).
    assert_eq!(currency_mount("fxcm", wired, "USD"), None);
    // Any other money keeps today's behaviour: the table has no row for it, so the ledger stays unanchored.
    for ccy in ["JPY", "EUR", "CHF", "AUD", "CAD", "NZD", "USDT", ""] {
        assert_eq!(currency_mount("fxcm", wired, ccy), None, "`{ccy}` has no row");
    }
    // A mount somebody chose is never replaced, and a pound pair needs no replacement.
    for chosen in ["EURGBP", "USDJPY", "GBPUSD", "eurusd"] {
        assert_eq!(
            currency_mount("fxcm", chosen, "GBP"),
            None,
            "`{chosen}` is not the wired mount"
        );
    }
    // Every other venue is untouched, whatever it is mounted on.
    for venue in ["oanda", "ctrader", "dukascopy", "ig", "ibkr", "binance", "bybit", "okx"] {
        let mount = vike_venues::venue_wiring::wired_symbol(venue).unwrap_or("EURUSD");
        assert_eq!(currency_mount(venue, mount, "GBP"), None, "{venue}");
    }
    // The table is honest: each row is the venue's wired mount replaced by a pair that DOES anchor `ccy`, and the
    // wired mount it replaces does not (a row that moved a mount which already anchors would change a working
    // account).
    for (venue, ccy, mount) in CURRENCY_MOUNTS {
        let wired = vike_venues::venue_wiring::wired_symbol(venue).expect("a wired venue");
        assert!(
            unanchorable_mount(venue, wired, ccy).is_some(),
            "{venue}/{ccy}: {wired} anchors it"
        );
        assert_eq!(
            unanchorable_mount(venue, mount, ccy),
            None,
            "{venue}/{ccy}: {mount} must anchor"
        );
        assert_eq!(currency_mount(venue, wired, ccy), Some(*mount));
    }
}

/// **A POUND fxcm account, mounted on `EURGBP`, takes the dollar pair `EURUSD` as a foreign-quoted symbol at the
/// GBPUSD rate** (the measured demo account, 2026-10-20: its currency is GBP, so the engine is mounted on a pound
/// pair, `AdapterBind::mount_symbol`): the facts state USD and the pounds-per-dollar rate (`1 / 1.35`), and the door
/// admits it on a capped `demo` account only; the engine's own pound pair passes untouched; never on a `live` or
/// `paper` account or one with no account-wide ceiling. ⚠ A PIN, not a red test: the door's rule is generic and was
/// right before the move existed, so this holds the claim the move relies on (what the node does with the engine it
/// is now handed); the tests that fail without the move are in `remote_backend_wire.rs` and the engine's own.
#[test]
fn a_pound_fxcm_engine_admits_the_dollar_pair_at_the_gbpusd_rate_on_a_capped_demo_account() {
    let plan = |cmd: &WireCommand, block: vike_exec::VenueBlock, facts: &Source| {
        plan_submit_symbol(cmd, &[block], Some(facts), true)
    };
    let refused = |plan: SymbolPlan| match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let eurusd = limit("c-usd", "fxcm", "EURUSD", 1000.0, 1.1);
    let pounds_a_dollar = 1.0 / 1.35;
    let facts = Source::confirming(&[]).quoting("EURUSD", "USD", pounds_a_dollar);

    match plan(&eurusd, engine_at("fxcm", "EURGBP", AccountTier::Demo), &facts) {
        SymbolPlan::Place { symbol, admit: Some(a), .. } => {
            assert_eq!(symbol, "EURUSD");
            assert_eq!(a.quote_ccy.as_deref(), Some("USD"));
            assert_eq!(a.quote_rate, Some(pounds_a_dollar));
        }
        other => panic!("a capped demo pound account takes the dollar pair: {other:?}"),
    }
    // the engine's own pair, under either spelling, is placed as it is with no lookup
    let own = limit("c-own", "fxcm", "EUR/GBP", 1000.0, 0.85);
    assert!(matches!(
        plan(&own, engine_at("fxcm", "EURGBP", AccountTier::Demo), &facts),
        SymbolPlan::Place { symbol, admit: None, .. } if symbol == "EURGBP"
    ));
    // a live account and a demo one with no ceiling do not convert: the dollar pair is a foreign money for them
    for (tier, source) in [
        (AccountTier::Live, Source::confirming(&[]).quoting("EURUSD", "USD", pounds_a_dollar)),
        (
            AccountTier::Demo,
            Source::confirming(&[]).quoting("EURUSD", "USD", pounds_a_dollar).uncapped(),
        ),
    ] {
        let why = refused(plan(&eurusd, engine_at("fxcm", "EURGBP", tier), &source));
        assert!(
            why.contains("quoted in USD") && why.contains("nothing converts"),
            "{tier:?}: {why}"
        );
    }
    let paper = refused(plan(&eurusd, engine_at("fxcm", "EURGBP", AccountTier::Paper), &facts));
    assert!(paper.contains("paper account trades only"), "{paper}");
    // a gatewayhub that states no currency for the dollar pair: it would be booked at par, so it is not admitted
    let silent = Source::confirming(&[]);
    let why = refused(plan(&eurusd, engine_at("fxcm", "EURGBP", AccountTier::Demo), &silent));
    assert!(why.contains("quoted in USD") && why.contains("no currency"), "{why}");
    assert!(why.contains("NOT admitted") && why.contains("never valued at par"), "{why}");
}

/// **An engine mounted on a pair whose money is NOT the account's currency is admitted no pair in the account's
/// currency** (the review's F1/F2): a labelled FXCM account on its own row's symbol, `USDJPY` (money JPY) on a USD
/// account, is not anchored to USD (`fxcm_rate::anchorable`: its mounted fills are booked at par in yen). A pair
/// quoted in the account's currency (`EURUSD`) carries no currency in the facts, so under the single-currency path
/// it would book dollars and yen at par in one ledger: the door refuses it, naming the money the engine counts
/// in. A pair in the MOUNT's own money (`EURJPY`) is the facts' to judge as for any engine: the door takes it
/// with its currency and rate, and the engine (not anchored, so `account-ccy-unset`) is what refuses it next
/// (below, through a real core). Every other door test of this file mounts `EURUSD`, so this is the one that
/// holds the mismatched mount.
#[test]
fn fxcm_refuses_the_accounts_currency_pair_into_an_engine_mounted_on_another_money() {
    let plan = |cmd: &WireCommand, block: vike_exec::VenueBlock, facts: &Source| {
        plan_submit_symbol(cmd, &[block], Some(facts), true)
    };
    let refused = |plan: SymbolPlan| match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let usd_pair = limit("c-eurusd", "fxcm", "EURUSD", 1000.0, 1.1);
    // the facts of a USD account for EURUSD: quoted in its own currency, so no currency, no rate
    let plain = Source::confirming(&[]);
    let why = refused(plan(&usd_pair, engine_at("fxcm", "USDJPY", AccountTier::Demo), &plain));
    assert!(why.contains("quoted in USD") && why.contains("no currency"), "{why}");
    assert!(why.contains("NOT admitted") && why.contains("never valued at par"), "{why}");
    // not converting (no ceiling): the old words, naming the engine's money
    let uncapped = Source::confirming(&[]).uncapped();
    let why = refused(plan(&usd_pair, engine_at("fxcm", "USDJPY", AccountTier::Demo), &uncapped));
    assert!(why.contains("counts its money in JPY") && why.contains("nothing converts"), "{why}");
    // the mount's own money: the facts are asked, and they state JPY against an account currency that is another
    // one (USD). An anchored engine has mount money == account money, so a stated currency here means THIS engine
    // is not anchored: it would refuse the admission (`account-ccy-unset`) after the door had made the price
    // lane's place permanent (`LeaseFacts::admitted`). The door refuses it first (the review's F-1).
    let yen_cross = limit("c-eurjpy", "fxcm", "EURJPY", 1000.0, 165.0);
    let quoting = Source::confirming(&[]).quoting("EURJPY", "JPY", 1.0 / 150.0);
    let why = refused(plan(&yen_cross, engine_at("fxcm", "USDJPY", AccountTier::Demo), &quoting));
    assert!(why.contains("not anchored") && why.contains("account-ccy-unset"), "{why}");
    assert!(why.contains("quoted in JPY") && why.contains("`USDJPY`"), "{why}");
    // ...and the same pair into an engine mounted on a pair in the ACCOUNT's money (EURUSD) is the ordinary
    // foreign-quoted admission: money USD != JPY, so the facts' currency and rate are demanded and given
    match plan(&yen_cross, engine_at("fxcm", "EURUSD", AccountTier::Demo), &quoting) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("JPY"), Some(1.0 / 150.0)));
        }
        other => panic!("an anchored engine takes a foreign-quoted pair at its rate: {other:?}"),
    }
}

/// The other half of the one above, through a REAL core: the engine mounted on `USDJPY` that has not been told the
/// account's currency (an unanchored ledger) takes neither pair. The dollar pair is refused at the door; the yen
/// cross passes the door and is refused by the engine (`account-ccy-unset`), so it is never traded and never
/// reaches the venue. A later order on the mounted pair is the proof that both earlier commands were folded.
#[test]
fn an_unanchored_usdjpy_engine_trades_neither_the_dollar_pair_nor_the_yen_cross() {
    let facts = Source::confirming(&[]).quoting("EURJPY", "JPY", 1.0 / 150.0);
    let mut node = Node::start("fxcm", "USDJPY", AccountTier::Demo);
    let why = node
        .send(limit("dollar", "fxcm", "EURUSD", 1000.0, 1.1), Some(&facts))
        .expect_err("a dollar pair into a yen engine");
    assert!(why.contains("no currency") || why.contains("nothing converts"), "{why}");
    // the yen cross is refused at the DOOR now (the engine is not anchored, so it would have refused the admission
    // after `LeaseFacts::admitted` had made the price lane's place permanent): no command was enqueued and the
    // facts source was never told to commit the symbol (its lane place stays the door's to reap)
    let why = node
        .send(limit("yen", "fxcm", "EURJPY", 1000.0, 165.0), Some(&facts))
        .expect_err("a yen cross into an unanchored yen engine");
    assert!(why.contains("not anchored") && why.contains("account-ccy-unset"), "{why}");
    assert!(facts.commits().is_empty(), "no symbol was committed: {:?}", facts.commits());
    node.send(limit("own", "fxcm", "USDJPY", 1000.0, 150.0), None).expect("the mounted pair");
    node.venue_saw("own");
    assert!(
        !node.blocks().iter().any(|b| b.trades("EURUSD") || b.trades("EURJPY")),
        "neither pair is traded: {:?}",
        node.blocks()
    );
    let seen: Vec<String> =
        node.submitted.lock().unwrap().iter().map(|o| o.symbol.clone()).collect();
    assert_eq!(seen, ["USDJPY"], "only the mounted pair reached the venue");
    node.core.shutdown_and_join();
}

/// **A USDJPY unit closed 0.10 above its entry realizes 66.62 dollars on FXCM too**, through the real core: the
/// door admits the pair at the rate FXCM's conversion pair states (`1/150`, then `1/150.10`), the position opens at
/// 100,000 dollars of notional and the close books `10,000 / 150.10`, never the 10,000 "dollars" an unconverted
/// engine would. (The rate is scripted here; its derivation from a docs-shaped Offers table is
/// `crates/bridges/fxcm/tests/fxcm_price_lane_money.rs`'s.)
#[test]
fn a_usdjpy_unit_closed_ten_pips_up_realizes_66_dollars_on_fxcm_too() {
    let facts = Source::confirming(&[]).quoting("USDJPY", "JPY", 1.0 / 150.0);
    let mut node = Node::start_anchored("fxcm", "EURUSD", AccountTier::Demo, "USD");
    node.send(limit("jpy-open", "fxcm", "USDJPY", 100_000.0, 150.0), Some(&facts)).expect("opened");
    node.venue_saw("jpy-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == "USDJPY" && p.size == 100_000.0)
    });
    let notional = block(&node).contract_value_of("USDJPY").order_notional(100_000.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: dollars, not 15,000,000 yen");

    facts.reprice("USDJPY", 1.0 / 150.10, 10);
    node.send(sell_to_close("jpy-close", "fxcm", "USDJPY", 100_000.0, 150.10), Some(&facts))
        .expect("closed");
    node.venue_saw("jpy-close");
    let want = 10_000.0 / 150.10;
    wait_until("the realized PnL", || (block(&node).realized_pnl - want).abs() < 1e-6);
    let realized = block(&node).realized_pnl;
    assert!((realized - 66.62).abs() < 1e-2, "{realized}");
    assert!(realized < 100.0, "nowhere near the 10,000 the unconverted engine books");
    node.core.shutdown_and_join();
}
