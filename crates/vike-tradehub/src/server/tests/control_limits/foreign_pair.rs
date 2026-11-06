//! A pair quoted in another money than the account's, at the notional ceiling (decision 0183 item 10: a
//! ceiling is stated in the ACCOUNT's currency).
//!
//! `1000 EURJPY @168.49` is 168,490 YEN and about 1,000 euros. An engine's published grid lists no row for a
//! pair it was never told about, so the ceiling sizes it with the value the symbol door's look-up carries
//! (`foreign_contract_value`: the facts' unit times the rate, in the account's money) and, when the door
//! REFUSES the pair, with nothing: it used to read the engine's default value there, `qty · price`, the
//! pair's own quote money, and the operator was told the order was over a 100,000 ceiling it was a hundredth
//! of, in a sentence that hid the reason the door would have given.
//!
//! The rows run the real acceptance path over a real core (`admits_any_symbol`'s `Node`) and the real TCP
//! dry-run, with a facts source that quotes every symbol in yen.
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_exec::SymbolAdmission;
use vike_model::{AccountTier, SymbolGrid};

use super::tcp_preview::{control_stream, preview};
use super::*;
use crate::server::symbol_admission::{SymbolFacts, foreign_contract_value};
use crate::server::tests::admits_any_symbol::{Node, limit, wait_until};

/// The node's `policy.max_notional_per_order`, in the account's money.
const CEILING: f64 = 100_000.0;

/// A facts source that quotes EVERY symbol in yen at `rate` account units per yen (`None`: the facts state the
/// currency and no rate), for an account whose exposure ceiling is armed or not.
struct Yen {
    rate: Option<f64>,
    capped: bool,
    /// The first look-up fails and every later one answers: the facts changed between two plans of one order.
    fails_first: bool,
    asked: AtomicUsize,
}

impl Yen {
    fn at(rate: Option<f64>) -> Self {
        Yen { rate, capped: true, fails_first: false, asked: AtomicUsize::new(0) }
    }

    fn uncapped(mut self) -> Self {
        self.capped = false;
        self
    }

    fn failing_first(mut self) -> Self {
        self.fails_first = true;
        self
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl SymbolFacts for Yen {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        if self.asked.fetch_add(1, Ordering::SeqCst) == 0 && self.fails_first {
            return Err("the gatewayhub did not answer".into());
        }
        Ok(SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(1.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: Some("JPY".into()),
            quote_rate: self.rate,
            rate_as_of_ms: self.rate.map(|_| 0),
            im_requirement: None,
            release: false,
            margin_mode: None,
        })
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(150.0))
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

fn set_ceiling(node: &mut Node) {
    node.limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(CEILING), rate_per_sec: 1e9 });
}

/// **A pair the door refuses is refused in the DOOR's words, never sized in its quote money.** The account has
/// no account-wide exposure ceiling armed, so the door will not admit a pair quoted in another money: the order
/// is refused with that reason whether or not a notional ceiling is set, on every venue that converts a quote
/// currency. It used to be refused with `order notional 168490.00 exceeds ... 100000.00` (a yen figure against
/// a ceiling in the ledger's money) whenever `qty · price` was over the ceiling, and that figure is not the
/// order's: 1,000 EURJPY is about a thousand euros.
///
/// ⚠ KILL PROOF: have `foreign_contract_value` answer `Ok(None)` for a refused plan again and every row
/// below flips to the `order notional` sentence.
#[test]
fn a_pair_the_door_refuses_is_refused_in_its_words_never_sized_in_its_quote_money() {
    // (venue, the mounted symbol, the pair asked for, its price): 1,000 units are over the ceiling in yen.
    let rows = [
        ("fxcm", "EURUSD", "EURJPY", 168.49),
        ("dukascopy", "EURUSD", "USDJPY", 157.0),
        ("oanda", "EURUSD", "USDJPY", 157.0),
        ("ctrader", "EURUSD", "USDJPY", 157.0),
        ("ig", "CS.D.EURUSD.MINI.IP", "CS.D.USDJPY.MINI.IP", 157.0),
    ];
    for (venue, mounted, pair, px) in rows {
        let facts = Yen::at(Some(1.0 / 150.0)).uncapped();
        let mut node = Node::start(venue, mounted, AccountTier::Demo);
        let order = limit("jpy-1", venue, pair, 1000.0, px);
        assert!(1000.0 * px > CEILING, "{venue}: the row is over the ceiling in yen");

        // The reference: the door's own refusal, with no notional ceiling set at all.
        let door = node
            .send(order.clone(), Some(&facts))
            .expect_err(&format!("{venue}: the door refuses"));
        assert!(door.contains("quoted in JPY"), "{venue}: {door}");

        set_ceiling(&mut node);
        let refused = node.send(order, Some(&facts)).expect_err(&format!("{venue}: refused"));
        assert_eq!(refused, door, "{venue}: the same sentence with a ceiling set");
        assert!(!refused.contains("order notional"), "{venue}: {refused}");
        assert!(node.submitted.lock().unwrap().is_empty(), "{venue}: nothing reached the venue");
        node.core.shutdown_and_join();
    }
}

/// **What the ceiling is handed** (`foreign_contract_value`): the symbol's value in the account's money when the
/// door places it, the door's refusal when it does not, and nothing for a symbol the engine's grid answers.
#[test]
fn the_ceiling_is_handed_a_value_or_the_doors_refusal() {
    let order = limit("jpy-v", "oanda", "EURJPY", 1000.0, 168.49);
    let node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    assert_eq!(
        foreign_contract_value(&order, &node.blocks(), Some(&Yen::at(Some(1.0 / 150.0)))),
        Ok(Some(vike_model::ContractValue::linear(1.0 / 150.0))),
        "a yen is a fraction of a dollar"
    );
    let refusal = foreign_contract_value(&order, &node.blocks(), Some(&Yen::at(None)))
        .expect_err("no rate, no value");
    assert!(refusal.contains("usable conversion rate"), "{refusal}");
    let own = limit("own", "oanda", "EURUSD", 1000.0, 1.1);
    assert_eq!(
        foreign_contract_value(&own, &node.blocks(), Some(&Yen::at(None))),
        Ok(None),
        "the mounted symbol is the grid's"
    );
    node.core.shutdown_and_join();
}

/// **A pair the door admits is counted in the ACCOUNT's money** (a USD account on a converting venue, its ledger
/// anchored and its account-wide ceiling armed), before it is admitted and after: a yen is a fraction of a
/// dollar, so 1,000 EURJPY at 168.49 passes a 100,000 ceiling and 100,000 of them (112,326.67 dollars) does
/// not, with the multiplier named; the mounted USD pair keeps the sentence it always had, byte for byte.
#[test]
fn a_pair_the_door_admits_is_counted_in_the_accounts_money_before_and_after_its_admission() {
    let facts = Yen::at(Some(1.0 / 150.0));
    let mut node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    set_ceiling(&mut node);

    // Not admitted yet: sized with the facts' value.
    let over = node
        .send(limit("usdjpy-big", "oanda", "USDJPY", 1_000_000.0, 157.0), Some(&facts))
        .expect_err("a million dollars of it is over the ceiling");
    assert!(over.contains("order notional 1046666.67 exceeds"), "{over}");
    assert!(over.contains("contract multiplier 0.0066"), "{over}");
    assert!(!node.blocks()[0].trades("USDJPY"), "a refused order admits nothing");
    node.send(limit("usdjpy-1", "oanda", "USDJPY", 1000.0, 157.0), Some(&facts))
        .expect("1,000 dollars of it is about 1,047 dollars, not 157,000 yen over the ceiling");
    node.venue_saw("usdjpy-1");
    node.send(limit("eurjpy-1", "oanda", "EURJPY", 1000.0, 168.49), Some(&facts))
        .expect("1,000 euros of it is about 1,123 dollars");
    node.venue_saw("eurjpy-1");

    // Admitted: the engine's grid row is the same effective value.
    wait_until("the admission to publish", || node.blocks()[0].trades("EURJPY"));
    let over = node
        .send(limit("eurjpy-2", "oanda", "EURJPY", 100_000.0, 168.49), Some(&facts))
        .expect_err("112,326.67 dollars is over the ceiling");
    assert!(over.contains("order notional 112326.67 exceeds"), "{over}");
    node.send(limit("eurjpy-3", "oanda", "EURJPY", 50_000.0, 168.49), Some(&facts))
        .expect("56,163.33 dollars is under it");

    // The mounted pair, quoted in dollars, is unchanged: no value clause.
    let over = node
        .send(limit("eurusd-1", "oanda", "EURUSD", 200_000.0, 1.0), Some(&facts))
        .expect_err("200,000 is over the ceiling");
    assert_eq!(
        over,
        "order notional 200000.00 exceeds the node's policy ceiling max_notional_per_order 100000.00"
    );
    node.core.shutdown_and_join();
}

/// **No rate, a named refusal**: the facts state the currency and carry no rate, so the pair is not admitted
/// ("never valued at par") and the order is refused with THAT, where it used to be refused as 168,490 over the
/// ceiling.
#[test]
fn a_pair_whose_facts_carry_no_rate_is_refused_by_name_not_valued_at_par() {
    let facts = Yen::at(None);
    let mut node = Node::start("fxcm", "EURUSD", AccountTier::Demo);
    set_ceiling(&mut node);
    let order = limit("jpy-norate", "fxcm", "EURJPY", 1000.0, 168.49);
    let refused = node.send(order, Some(&facts)).expect_err("no rate, no admission");
    assert!(
        refused.contains("usable conversion rate") && refused.contains("never valued at par"),
        "{refused}"
    );
    assert!(!refused.contains("order notional"), "{refused}");
    assert!(!node.blocks()[0].trades("EURJPY"));
    node.core.shutdown_and_join();
}

/// **An order the ceiling could not size is never let through by a later answer.** The look-up that sized it
/// failed; the committing plan, asked again, would have answered and admitted a symbol the ceiling never
/// counted (1 EURJPY is under any ceiling in yen, so nothing else would have stopped it). The refusal of the
/// first answer stands, and the committing plan is never run.
#[test]
fn a_symbol_the_ceiling_could_not_size_is_not_admitted_by_a_later_answer() {
    let facts = Yen::at(Some(1.0 / 150.0)).failing_first();
    let mut node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    set_ceiling(&mut node);
    let refused = node
        .send(limit("jpy-race", "oanda", "EURJPY", 1.0, 168.49), Some(&facts))
        .expect_err("the first look-up failed");
    assert!(refused.contains("the gatewayhub did not answer"), "{refused}");
    assert_eq!(facts.asked(), 1, "the committing plan was not asked");
    assert!(!node.blocks()[0].trades("EURJPY"));
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing reached the venue");
    node.core.shutdown_and_join();
}

/// A TCP dry-run over `node`'s own published cell, with `facts` as the node's symbol source.
fn preview_stream(node: &Node, facts: &std::sync::Arc<Yen>) -> TcpStream {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-foreign-pair-ceiling-preview";
    let publisher = crate::publish::spawn(node.core.snapshot_cell(), None)
        .with_symbol_facts(Some(std::sync::Arc::clone(facts) as std::sync::Arc<dyn SymbolFacts>));
    wait_until("the publisher's roster", || !publisher.engine_blocks().is_empty());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = serve(
            listener,
            publisher,
            HubKeys::new(
                b"observe-key-for-the-foreign-pair-preview".to_vec(),
                CONTROL_KEY.to_vec(),
            ),
            None,
            ControlLimitsConfig { max_notional: Some(CEILING), rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    control_stream(addr, CONTROL_KEY)
}

/// **The dry-run says what the send says**, for a pair the door refuses and for one it admits: the TCP Preview
/// arm (the same chain `accept_command` runs, over the node's published roster and the node's facts) never
/// reports accepted what the send refuses, and gives the very sentence.
#[test]
fn the_preview_agrees_with_the_send_for_a_foreign_pair() {
    // Refused by the door (no account-wide ceiling armed).
    let facts = std::sync::Arc::new(Yen::at(Some(1.0 / 150.0)).uncapped());
    let mut node = Node::start("fxcm", "EURUSD", AccountTier::Demo);
    set_ceiling(&mut node);
    let mut ctl = preview_stream(&node, &facts);
    let order = limit("pv-1", "fxcm", "EURJPY", 1000.0, 168.49);
    let (accepted, reason) = preview(&mut ctl, order.clone());
    let reason = reason.expect("a refusal carries its reason");
    assert!(!accepted && reason.contains("quoted in JPY"), "{reason}");
    assert!(!reason.contains("order notional"), "{reason}");
    let sent = node.send(order, Some(&*facts)).expect_err("the send refuses it too");
    assert_eq!(sent, reason, "one sentence on both surfaces");
    node.core.shutdown_and_join();

    // Admitted by the door (a USD account that converts): under the ceiling both accept, over it both refuse
    // with the same sentence, counted in dollars.
    let facts = std::sync::Arc::new(Yen::at(Some(1.0 / 150.0)));
    let mut node = Node::start_anchored("oanda", "EURUSD", AccountTier::Demo, "USD");
    set_ceiling(&mut node);
    let mut ctl = preview_stream(&node, &facts);
    let (accepted, reason) = preview(&mut ctl, limit("pv-2", "oanda", "EURJPY", 1000.0, 168.49));
    assert!(accepted, "about 1,123 dollars is under the ceiling: {reason:?}");
    let big = limit("pv-3", "oanda", "EURJPY", 100_000.0, 168.49);
    let (accepted, reason) = preview(&mut ctl, big.clone());
    let reason = reason.expect("a refusal carries its reason");
    assert!(!accepted && reason.contains("order notional 112326.67 exceeds"), "{reason}");
    let sent = node.send(big, Some(&*facts)).expect_err("the send refuses it too");
    assert_eq!(sent, reason, "one sentence on both surfaces");
    node.core.shutdown_and_join();
}
