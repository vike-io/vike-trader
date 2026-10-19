//! **An IBKR stock quoted in another currency than the account's, through the control door** (decision 0183,
//! phase 1 for ibkr), driven into a REAL core with `accept_command` over `admits_any_symbol`'s `Node`, as
//! `symbol_money_door` does for OANDA.
//!
//! The facts source is scripted at the seam these tests are about: the quote currency and the rate the
//! gatewayhub states (account-currency units per ONE unit of it: a euro is MORE than a dollar, a yen a FRACTION
//! of one). DOCS-SHAPED: the rates are illustrative, no quote was captured from a gateway; what the gatewayhub
//! really states is `vike_ibkr::fx_rate`'s and `ibkr_rate`'s tests' to pin.
//!
//! The rule under test: only a `demo` account with an account-wide ceiling, whose ledger the engine's assembly
//! anchored to the base currency IBKR states (and whose mounted contract counts money in it), takes a stock of
//! another currency, and only with the conversion in the facts. A forex pair is a CASH position and is refused.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use vike_exec::SymbolAdmission;
use vike_model::{AccountTier, SymbolGrid};
use vike_tradehub_client::wire::WireCommand;

use super::admits_any_symbol::{Node, limit, wait_until};
use crate::server::symbol_admission::{
    SymbolFacts, SymbolPlan, anchor_with_journal, plan_submit_symbol, quote_lease_converts,
    quote_lease_reads_currency, unanchorable_mount,
};

/// A facts source that quotes the stocks it is told to in another currency.
struct Source {
    /// Stock name -> (currency, rate: account units per ONE unit of it).
    quoted: Mutex<HashMap<String, (&'static str, f64)>>,
    /// The moment the rate was asked, on the engine's clock (the harness core counts up from 0).
    as_of_ms: AtomicI64,
    /// Whether the next `refresh` hands out a fresh answer.
    refresh_due: AtomicBool,
    /// Whether the account runs with an account-wide exposure ceiling.
    capped: bool,
}

impl Source {
    fn quoting(name: &str, ccy: &'static str, rate: f64) -> Self {
        Source {
            quoted: Mutex::new(HashMap::from([(name.to_string(), (ccy, rate))])),
            as_of_ms: AtomicI64::new(0),
            refresh_due: AtomicBool::new(false),
            capped: true,
        }
    }

    /// A source whose gatewayhub states NO conversion for anything (an older one, or a lease that asked the old
    /// way).
    fn plain() -> Self {
        Source {
            quoted: Mutex::new(HashMap::new()),
            as_of_ms: AtomicI64::new(0),
            refresh_due: AtomicBool::new(false),
            capped: true,
        }
    }

    fn uncapped(mut self) -> Self {
        self.capped = false;
        self
    }

    fn reprice(&self, name: &str, rate: f64, as_of_ms: i64) {
        if let Some(row) = self.quoted.lock().unwrap().get_mut(name) {
            row.1 = rate;
        }
        self.as_of_ms.store(as_of_ms, Ordering::SeqCst);
        self.refresh_due.store(true, Ordering::SeqCst);
    }

    fn looked_up(&self, route_key: &str, symbol: &str) -> SymbolAdmission {
        let quoted = self.quoted.lock().unwrap().get(symbol).copied();
        SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(1.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: quoted.map(|(ccy, _)| ccy.to_string()),
            quote_rate: quoted.map(|(_, rate)| rate),
            rate_as_of_ms: quoted.map(|_| self.as_of_ms.load(Ordering::SeqCst)),
            im_requirement: None,
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
        Ok(Some(100.0))
    }
    fn refresh(&self, route_key: &str, _: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.refresh_due.swap(false, Ordering::SeqCst).then(|| self.looked_up(route_key, symbol))
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

fn block(node: &Node) -> vike_exec::VenueBlock {
    node.blocks().remove(0)
}

fn engine_at(mounted: &str, tier: AccountTier) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: "ibkr".into(),
        route_key: "ibkr".into(),
        symbol: mounted.into(),
        tier: Some(tier),
        ..Default::default()
    }
}

fn plan(cmd: &WireCommand, block: vike_exec::VenueBlock, facts: &Source) -> SymbolPlan {
    plan_submit_symbol(cmd, &[block], Some(facts), true)
}

fn refused(plan: SymbolPlan) -> String {
    match plan {
        SymbolPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn sell_to_close(coid: &str, symbol: &str, qty: f64, price: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, "ibkr", symbol, qty, price) else {
        unreachable!()
    };
    req.side = -1;
    req.reduce_only = true;
    WireCommand::Submit(req)
}

/// **A EUR stock on a USD account is booked in DOLLARS.** A euro is worth MORE than a dollar (`1.10`): 100
/// shares at 50 euros are 5,000 euros, 5,500 dollars, where the single-currency engine read 5,000 "dollars". The
/// door admits it with the quote currency and the rate; the published contract value is the unit times the rate,
/// and the rate table says which rate that was.
#[test]
fn a_euro_stock_is_booked_in_dollars_not_euros() {
    let facts = Source::quoting("SAP.SMART.EUR", "EUR", 1.10);
    let mut node = Node::start_anchored("ibkr", "AAPL.SMART.USD", AccountTier::Demo, "USD");
    node.send(limit("eu-1", "ibkr", "SAP.SMART.EUR", 100.0, 50.0), Some(&facts)).expect("accepted");
    node.venue_saw("eu-1");
    wait_until("the admission to publish", || {
        node.blocks().iter().any(|b| b.trades("SAP.SMART.EUR"))
    });
    let published = block(&node);
    let notional = published.contract_value_of("SAP.SMART.EUR").order_notional(100.0, 50.0);
    assert!((notional - 5_500.0).abs() < 1e-6, "{notional}: 5,000 euros at 1.10 dollars each");
    let [row] = &published.quote_rates[..] else { panic!("one rate: {:?}", published.quote_rates) };
    assert_eq!(row.ccy, "EUR");
    assert_eq!((row.rate, row.stale), (1.10, false));
    assert!(
        published.contract_value_of("AAPL.SMART.USD").is_default(),
        "the mounted stock, in dollars, is untouched"
    );
    node.core.shutdown_and_join();
}

/// **A yen stock closed 10,000 yen above its entry realizes 66.62 dollars, not 10,000.** A yen is a FRACTION of a
/// dollar (`1/150`, never `150`): 100 shares opened at 3,000 yen are 300,000 yen, 2,000 dollars; the close,
/// which re-reads the rate (`1/150.10`) before it is judged, books `10,000 / 150.10` at the rate in force when it
/// folds. The same fill in the single-currency engine books 10,000 "dollars".
#[test]
fn a_yen_stock_closed_ten_thousand_yen_up_realizes_66_dollars_not_10000() {
    let name = "7203.SMART.JPY";
    let facts = Source::quoting(name, "JPY", 1.0 / 150.0);
    let mut node = Node::start_anchored("ibkr", "AAPL.SMART.USD", AccountTier::Demo, "USD");
    node.send(limit("jp-open", "ibkr", name, 100.0, 3_000.0), Some(&facts)).expect("opened");
    node.venue_saw("jp-open");
    wait_until("the position", || {
        block(&node).positions.iter().any(|p| p.symbol == name && p.size == 100.0)
    });
    let notional = block(&node).contract_value_of(name).order_notional(100.0, 3_000.0);
    assert!((notional - 2_000.0).abs() < 1e-6, "{notional}: dollars, not 300,000 yen");

    facts.reprice(name, 1.0 / 150.10, 10);
    node.send(sell_to_close("jp-close", name, 100.0, 3_100.0), Some(&facts)).expect("closed");
    node.venue_saw("jp-close");
    let want = 10_000.0 / 150.10;
    wait_until("the realized PnL", || (block(&node).realized_pnl - want).abs() < 1e-6);
    let realized = block(&node).realized_pnl;
    assert!((realized - 66.622).abs() < 1e-3, "{realized}");
    assert!(realized < 100.0, "nowhere near the 10,000 the unconverted engine books");
    node.core.shutdown_and_join();
}

/// **A stock of another currency is admitted only for an account that converts, in words**: a `live` account, a
/// demo account with no account-wide ceiling and a paper one are each refused, and the refusal says why; the same
/// order on an account that qualifies is a placement carrying the currency and the rate.
#[test]
fn a_stock_of_another_currency_needs_a_demo_account_with_a_ceiling() {
    let cmd = limit("c-1", "ibkr", "SAP.SMART.EUR", 10.0, 50.0);
    let facts = Source::quoting("SAP.SMART.EUR", "EUR", 1.10);
    match plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Demo), &facts) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("EUR"), Some(1.10)));
        }
        other => panic!("a demo account with a ceiling takes it: {other:?}"),
    }
    let live = refused(plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Live), &facts));
    assert!(live.contains("nothing converts") && live.contains("`demo` account only"), "{live}");
    assert!(live.contains("REFUSED") && live.contains("SAP.SMART.EUR"), "{live}");

    let uncapped = Source::quoting("SAP.SMART.EUR", "EUR", 1.10).uncapped();
    let why = refused(plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Demo), &uncapped));
    assert!(why.contains("nothing converts") && why.contains("exposure ceiling"), "{why}");

    let paper = refused(plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Paper), &facts));
    assert!(paper.contains("paper account"), "{paper}");
}

/// **The facts without a conversion are not admitted**: a gatewayhub that did not convert (an older one, or a
/// lease that asked the old way) answers a EUR stock with no currency and no rate, which the engine would read as
/// the ledger's own money. The door refuses it, in words, instead of valuing euros at par.
#[test]
fn a_stock_of_another_currency_whose_facts_carry_no_conversion_is_not_admitted() {
    let cmd = limit("c-2", "ibkr", "SAP.SMART.EUR", 10.0, 50.0);
    let why = refused(plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Demo), &Source::plain()));
    assert!(why.contains("no conversion of EUR") && why.contains("never valued at par"), "{why}");
    // a rate that is no rate: the shared quote check, unchanged
    for bad in [0.0, -1.10, f64::NAN, f64::INFINITY] {
        let unpriced = Source::quoting("SAP.SMART.EUR", "EUR", bad);
        let why = refused(plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Demo), &unpriced));
        assert!(
            why.contains("no rate, no admission") && why.contains("NOT admitted"),
            "{bad}: {why}"
        );
    }
    // the facts name ANOTHER currency than the stock's: not a conversion of this stock
    let wrong = Source::quoting("SAP.SMART.EUR", "GBP", 1.35);
    let why = refused(plan(&cmd, engine_at("AAPL.SMART.USD", AccountTier::Demo), &wrong));
    assert!(why.contains("no conversion of EUR"), "{why}");
    // a stock in the engine's own money needs none, and carries none
    let own = limit("c-3", "ibkr", "MSFT.SMART.USD", 10.0, 50.0);
    match plan(&own, engine_at("AAPL.SMART.USD", AccountTier::Demo), &Source::plain()) {
        SymbolPlan::Place { admit: Some(a), .. } => {
            assert_eq!((a.quote_ccy, a.quote_rate, a.rate_as_of_ms), (None, None, None));
        }
        other => panic!("{other:?}"),
    }
}

/// **A forex pair is a CASH position, refused in words** by every spelling, converting account or not; an engine
/// that holds none of the money of the stock (a mounted forex pair, a contract with no currency) is refused
/// before any lookup.
#[test]
fn a_forex_pair_is_refused_as_a_cash_position_and_nothing_is_looked_up() {
    let facts = Source::quoting("EUR.IDEALPRO.USD", "USD", 1.0);
    for forex in ["EUR.IDEALPRO.USD", "eur.usd.idealpro", "EUR.IDEALPRO.GBP"] {
        let why = refused(plan(
            &limit("f-1", "ibkr", forex, 10.0, 1.1),
            engine_at("AAPL.SMART.USD", AccountTier::Demo),
            &facts,
        ));
        assert!(
            why.contains("not an IBKR stock name") && why.contains("TICKER.SMART.CCY"),
            "{forex}: {why}"
        );
        assert!(why.contains("CASH position") && why.contains("not converted"), "{forex}: {why}");
    }
    for mounted in ["EUR.IDEALPRO.USD", "AAPL"] {
        let why = refused(plan(
            &limit("f-2", "ibkr", "SAP.SMART.EUR", 10.0, 50.0),
            engine_at(mounted, AccountTier::Demo),
            &Source::quoting("SAP.SMART.EUR", "EUR", 1.10),
        ));
        assert!(why.contains("nothing converts"), "{mounted}: {why}");
        // The account HAS its ceiling (the source arms one): the mount is the reason, and the refusal does not
        // tell a demo account with a ceiling that it "needs a ceiling" (it passed the account's real bit).
        assert!(!why.contains("exposure ceiling"), "{mounted}: a ceiling is armed: {why}");
        // The same mount on an account WITHOUT a ceiling is told about the ceiling, as before.
        let uncapped = refused(plan(
            &limit("f-3", "ibkr", "SAP.SMART.EUR", 10.0, 50.0),
            engine_at(mounted, AccountTier::Demo),
            &Source::quoting("SAP.SMART.EUR", "EUR", 1.10).uncapped(),
        ));
        assert!(uncapped.contains("exposure ceiling"), "{mounted}: {uncapped}");
    }
}

/// **The ledger is anchored to the base currency only when the mounted contract counts money in it**: IBKR's
/// balance seed is the cash of the mounted contract's currency, so a base currency that is another one would
/// label that cash and value the mounted stock in the wrong currency. A mounted forex pair is a CASH position.
/// Other venues are untouched.
#[test]
fn the_ledger_is_anchored_only_to_the_mounted_contracts_own_currency() {
    for (mounted, ccy) in [
        ("AAPL.SMART.USD", "USD"),
        ("AAPL.SMART.USD", "usd"),
        ("SAP.SMART.EUR", "EUR"),
        ("ES.GLOBEX.USD.FUT.20251219", "USD"),
    ] {
        assert_eq!(unanchorable_mount("ibkr", mounted, ccy), None, "{mounted} / {ccy}");
    }
    let why = unanchorable_mount("ibkr", "AAPL.SMART.USD", "EUR").expect("another currency");
    assert!(
        why.contains("counts its money in USD") && why.contains("base currency is EUR"),
        "{why}"
    );
    for forex in ["EUR.IDEALPRO.USD", "EUR.USD.IDEALPRO"] {
        let why = unanchorable_mount("ibkr", forex, "USD").expect(forex);
        assert!(why.contains("CASH position"), "{forex}: {why}");
    }
    let why = unanchorable_mount("ibkr", "AAPL", "USD").expect("no currency");
    assert!(why.contains("states no currency"), "{why}");
    for venue in ["oanda", "ctrader", "ig", "binance"] {
        assert_eq!(unanchorable_mount(venue, "EURUSD", "EUR"), None, "{venue} is not judged here");
    }
}

/// **A lease asks the gatewayhub to convert** for oanda as it always did (whatever its ceiling), and for ibkr
/// only when the account has an exposure ceiling armed, the venue stated the account's currency and the mounted
/// contract agrees (`anchored`): otherwise it asks the old way, which changes nothing for an ibkr account that
/// never converted (the plain lookup, one dial, no currency read).
#[test]
fn an_ibkr_lease_converts_only_with_a_ceiling_and_an_anchored_ledger() {
    for (venue, tier, anchored, ceiling, want) in [
        ("oanda", AccountTier::Demo, false, false, true),
        ("oanda", AccountTier::Demo, true, false, true),
        ("oanda", AccountTier::Demo, true, true, true),
        ("oanda", AccountTier::Live, true, true, false),
        ("ibkr", AccountTier::Demo, true, true, true),
        ("ibkr", AccountTier::Demo, true, false, false),
        ("ibkr", AccountTier::Demo, false, true, false),
        ("ibkr", AccountTier::Live, true, true, false),
        ("ibkr", AccountTier::Paper, true, true, false),
        ("binance", AccountTier::Demo, true, true, false),
    ] {
        assert_eq!(
            quote_lease_converts(venue, tier, anchored, ceiling),
            want,
            "{venue} {tier:?} anchored {anchored} ceiling {ceiling}"
        );
    }
}

/// **The start reads the account's currency only for an ibkr account that will convert**: ibkr, demo, a ceiling
/// armed. An ibkr account with no ceiling reads nothing at its start (it spent no dial on a currency before the
/// conversion existed, and does not now); every other venue's statement is free or absent and is not asked for.
#[test]
fn only_a_converting_ibkr_account_asks_its_start_to_read_the_currency() {
    for (venue, tier, ceiling, want) in [
        ("ibkr", AccountTier::Demo, true, true),
        ("ibkr", AccountTier::Demo, false, false),
        ("ibkr", AccountTier::Live, true, false),
        ("ibkr", AccountTier::Paper, true, false),
        ("oanda", AccountTier::Demo, true, false),
        ("binance", AccountTier::Demo, true, false),
    ] {
        assert_eq!(
            quote_lease_reads_currency(venue, tier, ceiling),
            want,
            "{venue} {tier:?} ceiling {ceiling}"
        );
    }
}

/// **A restart whose gatewayhub could not state the currency anchors to the one its journal carries**, for an
/// ibkr account that converts: the restored engine's rates are expressed in it, and an engine assembled without
/// it could not take a fresh rate for a held position. A stated currency always wins; an account that does not
/// read the currency, and every other venue, take nothing from the journal.
#[test]
fn a_restart_that_cannot_read_the_currency_anchors_to_the_journals() {
    let stated = || Some("EUR".to_string());
    // the venue's word wins over the journal's
    assert_eq!(anchor_with_journal("ibkr", true, stated(), Some("USD")).as_deref(), Some("EUR"));
    // none stated: the journal's, trimmed and upper-cased
    assert_eq!(anchor_with_journal("ibkr", true, None, Some(" usd ")).as_deref(), Some("USD"));
    // nothing in the journal, or a blank: no anchor (a stock of another currency stays refused)
    assert_eq!(anchor_with_journal("ibkr", true, None, None), None);
    assert_eq!(anchor_with_journal("ibkr", true, None, Some("  ")), None);
    // an account that does not convert, and another venue, are left as they were
    assert_eq!(anchor_with_journal("ibkr", false, None, Some("USD")), None);
    assert_eq!(anchor_with_journal("oanda", true, None, Some("USD")), None);
    assert_eq!(anchor_with_journal("oanda", true, stated(), Some("USD")).as_deref(), Some("EUR"));
}

/// **A mount whose money IBKR may quote in a minor unit is not anchored**: a London stock's contract says
/// `GBP` and its price may be in pence, so a GBP ledger anchored to it would book pence as pounds. The guard is
/// on the anchor (the new feature), not on any account that never converted.
#[test]
fn a_mount_in_a_minor_unit_currency_is_not_anchored() {
    for (mounted, ccy) in [
        ("LLOY.SMART.GBP", "GBP"),
        ("LLOY.SMART.GBX", "GBX"),
        ("TEVA.SMART.ILS", "ILS"),
        ("NPN.SMART.ZAR", "ZAR"),
        ("NPN.SMART.ZAc", "ZAR"),
    ] {
        let why = unanchorable_mount("ibkr", mounted, ccy).expect(mounted);
        assert!(
            why.contains("MINOR unit") && why.contains("100 times too high"),
            "{mounted}: {why}"
        );
    }
    // a major currency with no such doubt is anchored as before
    assert_eq!(unanchorable_mount("ibkr", "SAP.SMART.EUR", "EUR"), None);
    assert_eq!(unanchorable_mount("ibkr", "AAPL.SMART.USD", "USD"), None);
}

/// **A restart keeps an IBKR stock's rate row, and the boot renews it** (the restore half of decision 0183, for
/// the stock the engine was admitted): the journal restores the foreign-quoted symbol with its currency and the
/// rate it was last priced at (the engine's own restore); the symbol is a re-admission target; the boot's pass
/// re-reads the rate and the rebuilt engine takes the fresh one. A boot that cannot reach the rate source leaves
/// the old row, which the engine calls stale at 120 s.
#[test]
fn a_restart_keeps_an_ibkr_stocks_rate_row_and_the_boot_renews_it() {
    use crate::readmit::{Pace, Target, readmit, targets};
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, AdmitOutcome, BalanceMode, Command, ExecutionEngine, RiskGate};
    use vike_model::RiskLimits;

    let engine = || {
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "ibkr", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "ibkr",
            "AAPL.SMART.USD",
        );
        engine.set_account_ccy("USD").expect("a currency code");
        engine
    };
    let name = "SAP.SMART.EUR";
    let facts = Source::quoting(name, "EUR", 1.10);
    facts.as_of_ms.store(1_000, Ordering::SeqCst);

    let mut live = engine();
    live.now_ms = 1_000;
    assert!(!matches!(live.admit_symbol(&facts.looked_up("ibkr", name)), AdmitOutcome::Refused(_)));
    let snap = live.snapshot_state();

    let mut rebuilt = engine();
    rebuilt.restore_runtime_state(&snap);
    assert!(rebuilt.accepts_symbol(name), "the restore put the admission back");
    assert_eq!(rebuilt.account.quote_ccy_of(name), Some("EUR"), "the currency survived");
    assert_eq!(rebuilt.account.quote_rate_of(name), 1.10, "and so did the rate row");

    // No rate source reachable: the old row stands, and goes stale after the engine's window.
    let window = vike_exec::DEFAULT_QUOTE_RATE_TTL_MS;
    assert_eq!(rebuilt.account.quote_leg(name, 1_000 + window), (1.10, false));
    assert_eq!(rebuilt.account.quote_leg(name, 1_000 + window + 1), (1.10, true));

    // The boot's pass: the symbol is a target and the re-admission carries a fresh rate.
    let wanted = targets(std::slice::from_ref(&snap));
    assert_eq!(
        wanted,
        vec![Target { route_key: "ibkr".into(), venue: "ibkr".into(), symbol: name.into() }]
    );
    let fresh = Source::quoting(name, "EUR", 1.12);
    fresh.as_of_ms.store(500_000, Ordering::SeqCst);
    let mut sent = Vec::new();
    let done = readmit(&wanted, &fresh, &Pace::default(), |c| sent.push(c));
    assert_eq!(done.sent, 1);
    rebuilt.now_ms = 500_000;
    for command in sent {
        let Command::AdmitSymbol(a) = command else { unreachable!() };
        assert!(!matches!(rebuilt.admit_symbol(&a), AdmitOutcome::Refused(_)), "{a:?}");
    }
    assert_eq!(rebuilt.account.quote_rate_of(name), 1.12);
    assert_eq!(rebuilt.account.quote_leg(name, 500_000), (1.12, false), "fresh again");
}

/// **A rate that is not usable is refused by the ENGINE too, in words**, whatever the door let through: a
/// zero, negative or non-finite rate, a rate from the future and a ledger that was never told its currency.
/// The engine is the last judge, not the node (`account-ccy-unset`, `quote-rate-future`).
#[test]
fn the_engine_refuses_an_unusable_ibkr_rate_and_an_unanchored_ledger() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, AdmitOutcome, BalanceMode, ExecutionEngine, RiskGate};
    use vike_model::RiskLimits;

    let build = |anchored: bool| {
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "ibkr", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "ibkr",
            "AAPL.SMART.USD",
        );
        if anchored {
            engine.set_account_ccy("USD").expect("a currency code");
        }
        engine.now_ms = 10_000;
        engine
    };
    let name = "SAP.SMART.EUR";
    let admission = |rate: f64, as_of: i64| {
        let facts = Source::quoting(name, "EUR", rate);
        facts.as_of_ms.store(as_of, Ordering::SeqCst);
        facts.looked_up("ibkr", name)
    };
    // the ledger was never told its currency: the rate is in a unit nobody agreed to
    assert_eq!(
        build(false).admit_symbol(&admission(1.10, 10_000)),
        AdmitOutcome::Refused("account-ccy-unset")
    );
    for bad in [0.0, -1.10, f64::NAN, f64::INFINITY] {
        assert!(
            matches!(build(true).admit_symbol(&admission(bad, 10_000)), AdmitOutcome::Refused(_)),
            "{bad}"
        );
    }
    // a stamp more than five seconds ahead of the engine's clock
    assert!(matches!(
        build(true).admit_symbol(&admission(1.10, 10_000 + 6_000)),
        AdmitOutcome::Refused("quote-rate-future")
    ));
    // the usable one is admitted
    assert_eq!(build(true).admit_symbol(&admission(1.10, 10_000)), AdmitOutcome::Admitted);
}
