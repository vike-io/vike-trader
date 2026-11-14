//! The COMBO at the node's door (wave 6 of the order kinds): `combo` is advertised beside the arm that lowers it, the
//! lowering is the model's `OrderCombo` field for field (its account, leg limits, lifetime and the ONE id its client
//! minted, which the `Ack` echoes), `combo_refusal` refuses each value shape in the model's words and a combo with no
//! id, a retry under a used id is refused whole, the ceiling sizes a combo leg by leg, and the account gates treat a
//! combo as an opening order on ONE account (decision 0254).

use super::control::{ControlLimits, ControlLimitsConfig, command_kind, lower_command};
use super::handshake::served_features;
use super::refusal::{
    account_refusal, combo_refusal, not_running_refusal, one_account_addressed,
    order_client_id_reuse_refusal,
};
use super::symbol_admission::{AccountReadiness, SymbolAdmitter};
use vike_execution::{Command, OrderIntent};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::OrderComboLeg;
use vike_tradehub_client::wire::{CommandWire, OrderComboWire};

/// A deribit call spread: buy the 100k call, sell the 110k call, priced by its net or (with `legs`) leg by leg.
fn spread(
    net_limit: Option<f64>,
    legs: Option<(f64, f64)>,
    account: Option<&str>,
) -> OrderComboWire {
    let leg = |symbol: &str, ratio, limit| OrderComboLeg { symbol: symbol.into(), ratio, limit };
    OrderComboWire {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            leg("BTC-27DEC26-100000-C", 1, legs.map(|l| l.0)),
            leg("BTC-27DEC26-110000-C", -1, legs.map(|l| l.1)),
        ],
        net_limit,
        account: account.map(str::to_string),
        time_in_force: None,
        order_client_id: Some("c1combo".into()),
    }
}

fn engine(route_key: &str, symbol: &str) -> vike_execution::VenueBlock {
    vike_execution::VenueBlock {
        venue: route_key.split('#').next().unwrap_or_default().into(),
        route_key: route_key.into(),
        symbol: symbol.into(),
        ..Default::default()
    }
}

/// The word ships TOGETHER with the arm that lowers it: a combo lowers onto `OrderIntent::Combo` as the very spec the
/// model holds (account, legs, ratios, leg limits and the client's id unchanged), and the `Ack` echoes that id.
#[test]
fn the_node_advertises_the_combo_beside_the_arm_that_lowers_it() {
    let word = vike_tradehub_client::proto::FEATURE_COMBO;
    assert_eq!(word, "combo");
    assert!(served_features(None, false).iter().any(|f| f == word), "the word is advertised");
    let cmd = CommandWire::Combo(spread(None, Some((0.05, 0.02)), Some("ALT")));
    assert_eq!(command_kind(&cmd), "combo");
    let (lowered, order_client_id) = lower_command(cmd).expect("the arm lowers a combo");
    let Command::Order(OrderIntent::Combo(spec)) = lowered else {
        panic!("not lowered to a combo: {lowered:?}");
    };
    let want = vike_model::OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            vike_model::OrderComboLeg {
                symbol: "BTC-27DEC26-100000-C".into(),
                ratio: 1,
                limit: Some(0.05),
            },
            vike_model::OrderComboLeg {
                symbol: "BTC-27DEC26-110000-C".into(),
                ratio: -1,
                limit: Some(0.02),
            },
        ],
        net_limit: None,
        time_in_force: vike_model::TimeInForce::default(),
        account: vike_model::accounts::account_keys::parse_wire_account("ALT").ok(),
        order_client_id: Some("c1combo".into()),
    };
    assert_eq!(*spec, want);
    assert_eq!(order_client_id, "c1combo", "the Ack echoes the combo's one id");
    let (net, _) = lower_command(CommandWire::Combo(spread(Some(-0.03), None, None))).expect("net");
    assert!(
        matches!(&net, Command::Order(OrderIntent::Combo(s)) if s.net_limit == Some(-0.03) && s.account.is_none()),
        "a credit net is carried signed, an absent account stays absent: {net:?}"
    );
    assert_eq!(
        vike_tradehub_client::proto::NODE_PROTO_VERSION,
        5,
        "no protocol bump (5 is the phase-4 renames', not this feature's)"
    );
}

/// **The combo's lifetime rides the wire** (doubt 2 of the wave): absent is `gtc`, a word is read as a submit's
/// (`TimeInForce::parse`), and the rule a plain order meets (`lifetime_refusal`) refuses what the combo's order
/// cannot carry: a word that is none, a `gtd` (this wire names no expiry), and a limit's lifetime on a market combo.
/// A post-only combo, priced or not, is refused before the `Ack` in the client doors' words (no book models one).
/// Which lifetimes a book or a venue then takes is theirs.
#[test]
fn the_combo_lifetime_rides_the_wire_and_meets_the_plain_orders_rule() {
    const POST_ONLY: &str = "a combo cannot be post-only: no book models a maker-only combo";
    let with_time_in_force = |net: Option<f64>, tif: &str| {
        let mut c = spread(net, None, None);
        c.time_in_force = Some(tif.into());
        CommandWire::Combo(c)
    };
    let (ioc, _) = lower_command(with_time_in_force(Some(0.03), "ioc")).expect("an ioc net combo");
    assert!(
        matches!(&ioc, Command::Order(OrderIntent::Combo(s)) if s.time_in_force == vike_model::TimeInForce::Ioc),
        "{ioc:?}"
    );
    let (gtc, _) = lower_command(CommandWire::Combo(spread(Some(0.03), None, None))).expect("gtc");
    assert!(
        matches!(&gtc, Command::Order(OrderIntent::Combo(s)) if s.time_in_force == vike_model::TimeInForce::Gtc),
        "absent is gtc: {gtc:?}"
    );
    for (bad, said) in [
        (with_time_in_force(Some(0.03), "forever"), "combo: `time_in_force`"),
        (with_time_in_force(Some(0.03), "gtd"), "needs an expiry"),
        (with_time_in_force(None, "fok"), "belongs to a limit order"),
        // a raw wire client's post-only combo, priced or not, in the words the client doors give
        (with_time_in_force(Some(0.03), "post_only"), POST_ONLY),
        (with_time_in_force(None, "post-only"), POST_ONLY),
    ] {
        let lowered = lower_command(bad.clone()).expect_err(said);
        assert!(lowered.contains(said), "{said:?} not in {lowered}");
        assert_eq!(combo_refusal(&bad), Some(lowered), "the door and the lowering agree");
    }
}

/// **A remote combo names its own id** (a remote submit's rule): none, or a blank one, is refused before the `Ack`;
/// a retry under an id an order of this node already carries is refused WHOLE, naming the id; a new id is a new combo.
#[test]
fn a_combo_names_its_own_id_and_a_retry_is_refused_whole() {
    for id in [None, Some("  ")] {
        let mut c = spread(Some(0.03), None, None);
        c.order_client_id = id.map(str::to_string);
        let said = lower_command(CommandWire::Combo(c.clone())).expect_err("no id");
        assert!(said.contains("requires a pre-minted order_client_id"), "{said}");
        assert_eq!(combo_refusal(&CommandWire::Combo(c)), Some(said));
    }
    let placed = vike_execution::OrderView {
        data_delay: None,
        order_client_id: "c1combo".into(),
        venue: "deribit".into(),
        account: None,
        symbol: String::new(),
        side: 1,
        qty: 2.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.03),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let retry = CommandWire::Combo(spread(Some(0.03), None, None));
    let said =
        order_client_id_reuse_refusal(&retry, std::slice::from_ref(&placed), &Default::default())
            .expect("refused");
    assert!(
        said.starts_with("combo: order client id c1combo is already used")
            && said.contains("REFUSED whole"),
        "{said}"
    );
    let mut fresh = spread(Some(0.03), None, None);
    fresh.order_client_id = Some("c2combo".into());
    assert_eq!(
        order_client_id_reuse_refusal(&CommandWire::Combo(fresh), &[placed], &Default::default()),
        None,
        "a new combo"
    );
}

/// Each VALUE shape is refused by `lower_command` and by the door's verdict in the same words, the model's; a valid
/// combo of each pricing passes, and every other verb is not this verdict's.
#[test]
fn the_combo_values_are_refused_in_the_models_words() {
    for ok in [
        spread(Some(0.03), None, None),
        spread(None, Some((0.05, 0.02)), None),
        spread(None, None, None),
    ] {
        assert_eq!(combo_refusal(&CommandWire::Combo(ok)), None);
    }
    let with = |edit: fn(&mut OrderComboWire)| {
        let mut c = spread(None, Some((0.05, 0.02)), None);
        edit(&mut c);
        c
    };
    for (bad, said) in [
        (with(|c| c.side = 0), "side must be +1 or -1"),
        (with(|c| c.qty = 0.0), "qty must be finite and > 0"),
        (with(|c| c.legs.truncate(1)), "needs 2..=N legs, got 1"),
        (with(|c| c.legs[1].ratio = 0), "leg 1 has ratio 0"),
        (with(|c| c.legs[0].symbol = String::new()), "leg 0 has an empty symbol"),
        (with(|c| c.net_limit = Some(0.03)), "a net limit or a limit per leg, never both"),
        (with(|c| c.legs[1].limit = None), "leg 1 states no limit while another leg does"),
        (with(|c| c.legs[0].limit = Some(f64::NAN)), "leg 0's limit must be finite"),
        (with(|c| c.account = Some("bad label!".into())), "combo: `account`"),
    ] {
        let lowered = lower_command(CommandWire::Combo(bad.clone())).expect_err(said);
        assert!(lowered.contains(said), "lower_command: {said:?} not in {lowered}");
        let door = combo_refusal(&CommandWire::Combo(bad)).expect("refused");
        assert_eq!(door, lowered, "the door and the lowering say one sentence");
    }
    assert_eq!(combo_refusal(&CommandWire::Cancel("c".into())), None, "other verbs");
}

/// A facts source that holds one price for every symbol it is asked.
struct Priced(f64);

impl SymbolAdmitter for Priced {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<vike_execution::SymbolAdmission, SymbolPropertiesRequestError> {
        Err(SymbolPropertiesRequestError::Refused("no facts here".to_string()))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(self.0))
    }
}

impl AccountReadiness for Priced {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// **Every leg is sized, never the signed net**: a combo priced by its legs is capped leg by leg at `|ratio| × qty` at
/// each leg's own limit (the frame's numbers), naming the leg; one priced by its net or at market is sized leg by leg
/// at the price the node holds, and with no price held it is not sized at the door (the core's gate crosses each leg).
#[test]
fn a_combos_legs_are_sized_and_its_net_never_is() {
    let blocks = [engine("deribit", "BTC-PERPETUAL")];
    let limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(100.0), rate_per_sec: 1e9 });
    // Leg 0 is 2 × 60 = 120 over a ceiling of 100; leg 1 is 2 × 40 = 80 under it.
    let by_legs = CommandWire::Combo(spread(None, Some((60.0, 40.0)), None));
    let said = limits.preview_vet(&by_legs, &blocks, &[]).expect("leg 0 is over the ceiling");
    assert!(said.contains("the combo's leg 0") && said.contains("120.00"), "{said}");
    let under = CommandWire::Combo(spread(None, Some((40.0, 30.0)), None));
    assert_eq!(limits.preview_vet(&under, &blocks, &[]), None);
    assert_eq!(limits.market_reason(&under, &blocks, &[], None, Some(&Priced(1e6)), None), None);

    // A net of 0.5 a unit: never a size. Its legs at the held 60 are 120 each, over the ceiling.
    let net = CommandWire::Combo(spread(Some(0.5), None, None));
    assert_eq!(limits.preview_vet(&net, &blocks, &[]), None, "the signed net is no size");
    let said = limits
        .market_reason(&net, &blocks, &[], None, Some(&Priced(60.0)), Some(1.0))
        .expect("the held price sizes each leg");
    assert!(said.contains("the combo's leg 0") && said.contains("120.00"), "{said}");
    assert_eq!(limits.market_reason(&net, &blocks, &[], None, Some(&Priced(40.0)), None), None);
    assert_eq!(
        limits.market_reason(&net, &blocks, &[], None, None, Some(1e6)),
        None,
        "no price held"
    );
}

/// The ACCOUNT gates treat a combo as an opening order (decision 0254): a named account this node does not run is
/// refused naming the ones it does, a bare venue of two accounts is refused, a bare venue of ONE labelled account has
/// the label written onto the combo, and an account that does not run says why.
#[test]
fn the_combo_names_one_account() {
    let roster = ["deribit".to_string(), "deribit#ALT".to_string()];
    let unknown = CommandWire::Combo(spread(Some(0.03), None, Some("NOPE")));
    let said = account_refusal(&unknown, &roster).expect("refused before the Ack");
    assert!(said.contains("NOPE") && said.contains("ALT") && said.contains("The combo"), "{said}");
    let named = CommandWire::Combo(spread(Some(0.03), None, Some("ALT")));
    assert_eq!(account_refusal(&named, &roster), None);

    let bare = CommandWire::Combo(spread(Some(0.03), None, None));
    let blocks = [engine("deribit", "BTC-PERPETUAL"), engine("deribit#ALT", "BTC-PERPETUAL")];
    let refused =
        one_account_addressed(bare.clone(), &roster, &[], &blocks, None).expect_err("two");
    assert!(refused.contains("the combo"), "{refused}");
    let one = ["deribit#ALT".to_string()];
    let Ok(CommandWire::Combo(addressed)) =
        one_account_addressed(bare, &one, &[], &blocks[1..], None)
    else {
        panic!("a venue of one account takes a bare combo");
    };
    assert_eq!(addressed.account.as_deref(), Some("ALT"), "the one label is written out");

    let stopped = vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::of(
            "deribit",
            vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a label"),
        ),
        tier: Some(vike_model::AccountTier::Demo),
        reason: "account inactive".into(),
        block: Some(vike_model::ArmingBlock::AccountInactive),
    };
    let why = not_running_refusal(&named, &[stopped]).expect("it does not run");
    assert!(why.contains("does NOT RUN") && why.contains("The combo"), "{why}");
}
