//! **Which engine IS the lane twin, and what a twin kept up only to be closed takes** (v0.1.80 independent
//! review b, findings 2 and 6).
//!
//! A published block carries a label, a mount symbol and a tier, and nothing that says whether the engine is the
//! twin the node DERIVED or an operator's own account that spells the label (`derive_lane_twins` stands down for
//! one). The door judged by the label and the symbol alone, so a real `live` account mounted on `BTCUSDT.P`
//! received the default demo account's perpetual orders. The table opens the twin at the demo tier only, and the
//! account watcher tells the door about the real accounts it read under a twin's label
//! ([`SymbolFacts::real_account_under_twin_label`]).
//!
//! A twin whose row is gone while it holds something is kept up to be CLOSED. It took opening orders; it takes
//! none now ([`lane_twin_closing_refusal`]), and every reducing verb still goes through.

use super::*;
use crate::server::refusal::lane_twin_closing_refusal;

/// A source that knows what the account watcher told the door about the lane twins.
struct Flagged {
    table: Table,
    real: Vec<&'static str>,
    closing: Vec<&'static str>,
    derived: Vec<&'static str>,
}

impl Flagged {
    fn new(real: &[&'static str], closing: &[&'static str]) -> Self {
        Flagged {
            table: Table::eth(),
            real: real.to_vec(),
            closing: closing.to_vec(),
            derived: Vec::new(),
        }
    }

    /// This watcher also says these route keys are the lane twins it DERIVED and runs.
    fn deriving(mut self, derived: &[&'static str]) -> Self {
        self.derived = derived.to_vec();
        self
    }
}

impl SymbolFacts for Flagged {
    fn generation(&self, route_key: &str) -> u64 {
        self.table.generation(route_key)
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.table.admission(route_key, venue, symbol)
    }
    fn price(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        self.table.price(route_key, venue, symbol, subscribe)
    }
    fn real_account_under_twin_label(&self, route_key: &str) -> bool {
        self.real.contains(&route_key)
    }
    fn derived_lane_twin(&self, route_key: &str) -> bool {
        self.derived.contains(&route_key)
    }
    fn lane_twin_closing_only(&self, route_key: &str) -> bool {
        self.closing.contains(&route_key)
    }
}

fn mount(account: Option<&str>) -> WireCommand {
    WireCommand::MountStrategy {
        venue: "binance".to_string(),
        account: account.map(str::to_string),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        controller_id: None,
        name: Some("spread_maker".to_string()),
        rhai: None,
        params: serde_json::json!({}),
    }
}

fn mount_account(cmd: WireCommand) -> Option<String> {
    match cmd {
        WireCommand::MountStrategy { account, .. } => account,
        other => panic!("not a mount: {other:?}"),
    }
}

/// **A `live` account under the twin's label is not the twin WHEN THE NODE SAYS SO.** The owner opened the twin
/// at the `live` tier too (2026-10-19), so the table's tiers no longer tell a real live `PERP` account from the
/// derived live twin: the watcher's word ([`SymbolFacts::real_account_under_twin_label`]) does. Flagged, the
/// default account's perpetual order is left as it came (and turned back by the plan, naming the lane), a spot
/// order is not written out to `DEFAULT`, an account-less mount is not renamed, and the live account's own
/// symbol passes untouched like any other account's. Unflagged, a DEMO engine of the twin's shape IS the twin
/// (the absence rule); a LIVE one needs the positive word (the next test).
#[test]
fn a_real_live_account_the_node_names_under_the_twins_label_is_not_the_lane_twin() {
    let live = |block: vike_exec::VenueBlock| vike_exec::VenueBlock {
        tier: Some(AccountTier::Live),
        ..block
    };
    let roster = [live(engine("binance", "BTCUSDT")), live(engine("binance#PERP", "BTCUSDT.P"))];
    let facts = Flagged::new(&["binance#PERP"], &[]);
    for symbol in ["BTCUSDT.P", "ETHUSDT.P", "ethusdt.p", "ETHUSDT"] {
        assert_eq!(
            account_of(&lane_addressed(submit("binance", symbol, None), &roster, Some(&facts))),
            None,
            "{symbol}: the order is as it came"
        );
    }
    assert_eq!(mount_account(lane_addressed(mount(None), &roster, Some(&facts))), None);

    let why = refused(plan_submit_symbol(
        &submit("binance", "ETHUSDT.P", None),
        &[live(engine("binance", "BTCUSDT"))],
        Some(&facts),
        true,
    ));
    assert!(why.contains("perp symbol") && why.contains("spot lane"), "{why}");
    assert_eq!(
        plan_submit_symbol(
            &submit("binance", "BTCUSDT.P", Some("PERP")),
            &roster,
            Some(&facts),
            true
        ),
        SymbolPlan::Pass,
        "the live account's own symbol is nothing the twin's price lane has a say in"
    );
    assert!(facts.table.events().is_empty(), "…so its price lane is not asked");

    // Not flagged either way: a DEMO engine of the twin's shape is the twin (the absence rule it always had),
    // a LIVE one is not until the watcher says it derived it (next test).
    assert_eq!(
        account_of(&lane_addressed(submit("binance", "ETHUSDT.P", None), &two_lanes(), None)),
        Some("PERP")
    );
}

/// **A `live` engine is the lane twin only on the watcher's POSITIVE word** (review r2, finding 7). The block of
/// an older real `live` account labelled `PERP` and mounted on `BTCUSDT.P` is the derived twin's to the letter,
/// and "nobody reported a real account" is an absence (a surface with no source, a watcher that has not yet
/// said, a node that predates the flag). The default account's perpetual order is rewritten onto the twin's
/// keys only for an engine the node DERIVED; for every other live engine of that shape it is left as it came
/// (and turned back by the plan), a mount is not renamed, and the demo tier keeps the absence rule.
#[test]
fn a_live_engine_is_the_lane_twin_only_when_the_node_says_it_derived_it() {
    let live = |block: vike_exec::VenueBlock| vike_exec::VenueBlock {
        tier: Some(AccountTier::Live),
        ..block
    };
    let roster = [live(engine("binance", "BTCUSDT")), live(engine("binance#PERP", "BTCUSDT.P"))];
    let to = |facts: Option<&dyn SymbolFacts>| {
        account_of(&lane_addressed(submit("binance", "BTCUSDT.P", None), &roster, facts))
            .map(str::to_string)
    };
    let silent = Flagged::new(&[], &[]);
    let elsewhere = Flagged::new(&[], &[]).deriving(&["binance#ALT"]);
    let derived = Flagged::new(&[], &[]).deriving(&["binance#PERP"]);
    let derived_but_real = Flagged::new(&["binance#PERP"], &[]).deriving(&["binance#PERP"]);
    assert_eq!(to(None), None, "no source at all: no evidence");
    assert_eq!(to(Some(&silent)), None, "a source that says nothing: no evidence");
    assert_eq!(to(Some(&elsewhere)), None, "another key derived: no evidence for this one");
    assert_eq!(to(Some(&derived)).as_deref(), Some("PERP"), "the watcher derived it");
    assert_eq!(to(Some(&derived_but_real)), None, "reported as a real account: never the twin");
    for facts in [None, Some(&silent as &dyn SymbolFacts)] {
        assert_eq!(mount_account(lane_addressed(mount(None), &roster, facts)), None);
    }
    assert_eq!(
        mount_account(lane_addressed(mount(None), &roster, Some(&derived))).as_deref(),
        Some("DEFAULT"),
        "a derived twin is up: an account-less mount is written out to the default account"
    );

    // The shape alone still ADDS the twin's price rule (it fails closed): an opening order on the live
    // engine's own symbol asks the price lane, and with no source at all it is refused in words.
    let own = submit("binance", "BTCUSDT.P", Some("PERP"));
    let asked = Flagged::new(&[], &[]);
    assert!(matches!(
        plan_submit_symbol(&own, &roster, Some(&asked), true),
        SymbolPlan::Place { wait: None, admit: None, .. }
    ));
    assert_eq!(asked.table.events(), ["price:BTCUSDT.P:subscribe"], "the price lane is asked");
    let why = refused(plan_submit_symbol(&own, &roster, None, true));
    assert!(why.contains("perp lane") && why.contains("REFUSED"), "{why}");
}

/// **A real account under the label is not the twin when the node says so**, at the same tier and on the same
/// symbol as the derived twin would be: the watcher read the rows and found the label held by a row of the
/// table. The source that says nothing leaves the judgement to the block (the twin), and a source that names
/// ANOTHER key leaves it too.
#[test]
fn a_real_account_the_node_names_under_the_twins_label_is_not_the_lane_twin() {
    let roster = two_lanes();
    let to = |facts: Option<&dyn SymbolFacts>| {
        account_of(&lane_addressed(submit("binance", "BTCUSDT.P", None), &roster, facts))
            .map(str::to_string)
    };
    // (a DEMO roster: `two_lanes()`; the live tier needs the positive word, the test above)
    assert_eq!(to(None).as_deref(), Some("PERP"));
    assert_eq!(to(Some(&Flagged::new(&[], &[]))).as_deref(), Some("PERP"));
    assert_eq!(to(Some(&Flagged::new(&["binance#ALT"], &[]))).as_deref(), Some("PERP"));
    let real = Flagged::new(&["binance#PERP"], &[]);
    assert_eq!(to(Some(&real)), None, "the order is left as it came");
    assert_eq!(mount_account(lane_addressed(mount(None), &roster, Some(&real))), None);
    // …and the plan does not hold its price lane for the twin's own symbol either.
    let untouched = Flagged::new(&["binance#PERP"], &[]);
    assert_eq!(
        plan_submit_symbol(
            &submit("binance", "BTCUSDT.P", Some("PERP")),
            &roster,
            Some(&untouched),
            true
        ),
        SymbolPlan::Pass
    );
}

/// **A twin kept up only to be closed takes no opening order and every reducing one**, verb by verb. Only the
/// risk-increasing plane is refused (an order that is not reduce-only, a mount, a bracket); the refusal names
/// the lane and the way out, and an order of another account, a spot symbol or a surface with no source is
/// untouched.
#[test]
fn a_twin_kept_up_only_to_be_closed_takes_no_opening_order_and_every_reducing_one() {
    let facts = Flagged::new(&[], &["binance#PERP"]);
    let refused_for = |cmd: &WireCommand| lane_twin_closing_refusal(cmd, &[], &[], Some(&facts));

    let opening = submit("binance", "BTCUSDT.P", Some("PERP"));
    let why = refused_for(&opening).expect("an opening order is refused");
    for want in [
        "perp lane `binance#PERP`",
        "kept up only to be closed",
        "REFUSED",
        "state the leverage again",
    ] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(refused_for(&mount(Some("PERP"))).is_some(), "a mount trades the lane");
    // A BRACKET's entry opens a position on the lane it names, a limit and a market entry alike; the default
    // account's bracket (named or not) is the spot engine's and is not this twin's to refuse.
    let bracket = |account: Option<&str>, entry_price: Option<f64>| {
        WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 0.002,
            entry_price,
            stop_loss: 70_000.0,
            take_profit: 80_000.0,
            account: account.map(str::to_string),
        })
    };
    for entry_price in [Some(74_000.0), None] {
        let why = refused_for(&bracket(Some("PERP"), entry_price)).unwrap_or_else(|| {
            panic!("a bracket on the kept-up twin ({entry_price:?}) is refused")
        });
        assert!(why.contains("kept up only to be closed") && why.contains("The bracket"), "{why}");
        for account in [None, Some("DEFAULT"), Some("ALT")] {
            assert_eq!(
                refused_for(&bracket(account, entry_price)),
                None,
                "{account:?}: not the twin's bracket"
            );
        }
    }

    let WireCommand::Submit(mut reducing) = opening.clone() else { unreachable!() };
    reducing.reduce_only = true;
    for (name, cmd) in [
        ("a reduce-only order", WireCommand::Submit(reducing)),
        ("a cancel", WireCommand::Cancel("c-1".into())),
        (
            "a modify",
            WireCommand::Modify {
                client_order_id: "c-1".into(),
                new_qty: None,
                new_price: Some(1.0),
            },
        ),
        (
            "a mass-cancel",
            WireCommand::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: Some("PERP".into()),
            },
        ),
        (
            "a flatten",
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT.P".into(),
                account: Some("PERP".into()),
            },
        ),
        (
            "a market exit",
            WireCommand::MarketExit { venue: Some("binance".into()), account: Some("PERP".into()) },
        ),
    ] {
        assert_eq!(refused_for(&cmd), None, "{name} is never held");
    }
    for (name, cmd) in [
        ("the default account", submit("binance", "ETHUSDT", Some("DEFAULT"))),
        ("another account", submit("binance", "BTCUSDT.P", Some("ALT"))),
        ("another venue", submit("bybit", "BTCUSDT", Some("PERP"))),
    ] {
        assert_eq!(refused_for(&cmd), None, "{name} is not the twin");
    }
    assert_eq!(
        lane_twin_closing_refusal(&opening, &[], &[], None),
        None,
        "a surface with no source holds nothing"
    );
}

/// **Through the acceptance path, into a real core**: the default account's unnamed perpetual order is addressed
/// to the twin and refused in words, nothing reaches the venue, and a reduce-only order for the same twin is
/// taken. The Preview says what the order says.
#[test]
fn the_door_refuses_an_opening_order_for_a_kept_up_twin_and_takes_its_close() {
    let mut node = Node::start_lane_twin();
    let facts = Flagged::new(&[], &["binance#PERP"]);

    let why = node
        .send(limit("open-1", "binance", "BTCUSDT.P", 0.002, 74_000.0), Some(&facts))
        .expect_err("an opening order for the kept-up twin");
    assert!(why.contains("kept up only to be closed") && why.contains("REFUSED"), "{why}");
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing reached the venue");

    // The same for the default account's `.P` BRACKET: it is addressed to the twin (`lane_addressed`) and
    // judged as the twin's, so it cannot open a position on a lane kept up only to be closed.
    let bracket = WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 0.002,
        entry_price: Some(74_000.0),
        stop_loss: 70_000.0,
        take_profit: 80_000.0,
        account: None,
    });
    let why = node.send(bracket, Some(&facts)).expect_err("a bracket for the kept-up twin");
    assert!(why.contains("kept up only to be closed") && why.contains("REFUSED"), "{why}");
    assert!(node.submitted.lock().unwrap().is_empty(), "no bracket leg reached the venue");

    let WireCommand::Submit(mut close) = limit("close-1", "binance", "BTCUSDT.P", 0.002, 74_000.0)
    else {
        unreachable!()
    };
    (close.side, close.reduce_only) = (-1, true);
    node.send(WireCommand::Submit(close), Some(&facts)).expect("the close is taken");
    let seen = node.venue_saw("close-1");
    assert_eq!(seen.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
    node.core.shutdown_and_join();
}

/// The Telegram channel's wrapper (`FailFast`) passes both answers through: a wrapper that forgot them would
/// answer the trait's default (`false`) and silently switch the rules off for chat orders.
#[test]
fn the_fail_fast_wrapper_passes_the_twin_flags_through() {
    use crate::server::symbol_admission::FailFast;
    let wrapped = FailFast(std::sync::Arc::new(
        Flagged::new(&["binance#PERP"], &["binance#ALT"]).deriving(&["binance#X"]),
    ));
    assert!(wrapped.real_account_under_twin_label("binance#PERP"));
    assert!(wrapped.lane_twin_closing_only("binance#ALT"));
    assert!(!wrapped.real_account_under_twin_label("binance#ALT"));
    assert!(!wrapped.lane_twin_closing_only("binance#PERP"));
    assert!(wrapped.derived_lane_twin("binance#X") && !wrapped.derived_lane_twin("binance#PERP"));
}

/// A resting order of the twin `binance#PERP` on `BTCUSDT.P`.
fn resting(coid: &str, side: i32, qty: f64, price: f64) -> vike_exec::OrderView {
    vike_exec::OrderView {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        account: vike_model::accounts::account_keys::AccountLabel::parse("PERP").ok(),
        symbol: "BTCUSDT.P".to_string(),
        side,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        data_delay: None,
    }
}

/// **A `Modify` of a closing-only twin's order is a reducing verb only while it stays one** (re-review). It names
/// a client order id, so the published open order says whose it is and the published block the position it
/// closes: an OPENING order made larger or more aggressive (a buy priced up, a sell priced down), a closing
/// order made larger than the position, and the same on a flat lane are refused like the opening order they
/// extend; a smaller order, an eased price, a moved stop, an unknown id, and a twin that is not closing-only are
/// not.
#[test]
fn a_modify_that_adds_risk_to_a_closing_only_twin_is_refused_and_one_that_reduces_it_is_not() {
    let modify = |coid: &str, qty: Option<f64>, price: Option<f64>| WireCommand::Modify {
        client_order_id: coid.to_string(),
        new_qty: qty,
        new_price: price,
    };
    let long = |size: f64| vike_exec::VenueBlock {
        positions: vec![vike_exec::PositionView {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            position_side: vike_model::events::PositionSide::Both,
            size,
            avg_px: 100.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 1.0,
            liq_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }],
        ..engine("binance#PERP", "BTCUSDT.P")
    };
    let orders = [
        resting("buy", 1, 1.0, 100.0),
        resting("stop", -1, 0.5, 90.0),
        resting("sell-open", -1, 1.0, 110.0),
    ];
    let closing = Flagged::new(&[], &["binance#PERP"]);
    let open_twin = Flagged::new(&[], &[]);
    let refused_for = |blocks: &[vike_exec::VenueBlock], facts: &Flagged, cmd: &WireCommand| {
        lane_twin_closing_refusal(cmd, &orders, blocks, Some(facts))
    };

    // Long 0.5: a BUY adds to it, a SELL closes it.
    let held = [long(0.5)];
    for (what, cmd) in [
        ("a buy made bigger", modify("buy", Some(2.0), None)),
        ("a buy priced up", modify("buy", None, Some(101.0))),
        ("a closing stop larger than the position", modify("stop", Some(0.6), None)),
    ] {
        let why = refused_for(&held, &closing, &cmd).unwrap_or_else(|| panic!("{what} is refused"));
        assert!(why.contains("kept up only to be closed") && why.contains("The modify"), "{why}");
    }
    for (what, cmd) in [
        ("a buy made smaller", modify("buy", Some(0.5), None)),
        ("a buy priced down", modify("buy", None, Some(99.0))),
        ("a buy left alone", modify("buy", Some(1.0), Some(100.0))),
        ("a stop moved up", modify("stop", None, Some(95.0))),
        ("a stop shrunk", modify("stop", Some(0.25), None)),
        ("a stop at the position's size", modify("stop", Some(0.5), None)),
        ("an unknown id", modify("nobody", Some(9.0), Some(1.0))),
    ] {
        assert_eq!(refused_for(&held, &closing, &cmd), None, "{what} is not refused");
    }

    // A SELL resting on a lane that is long the other way... and a flat lane: every resting order is opening.
    let flat = [engine("binance#PERP", "BTCUSDT.P")];
    assert!(refused_for(&flat, &closing, &modify("sell-open", Some(2.0), None)).is_some());
    assert!(refused_for(&flat, &closing, &modify("sell-open", None, Some(109.0))).is_some());
    assert!(refused_for(&flat, &closing, &modify("sell-open", None, Some(111.0))).is_none());

    // A twin that is not closing-only is untouched by this gate, and so is no source at all.
    assert_eq!(refused_for(&held, &open_twin, &modify("buy", Some(2.0), None)), None);
    assert_eq!(
        lane_twin_closing_refusal(&modify("buy", Some(2.0), None), &orders, &held, None),
        None
    );
}
