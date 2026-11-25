//! `trade order combo` through the plane's own parser: it builds the `vike_client::verbs::combo_of` combo, a credit net
//! reads as a negative number, its one id is `--order-client-id` or minted, and a malformed line is refused naming what
//! is wrong (the model's words for a mix of net and leg limits).

use vike_client::verbs::{
    ComboLegArg, Verb, combo_of, fill_order_client_id, order_client_id_minter,
};
use vike_model::TimeInForce;
use vike_model::orders::client_id::OrderClientIdSource;
use vike_tradehub_client::wire::{CommandWire, OrderComboWire};

/// `trade order combo <line>`, read by the plane's parser and resolved by the verb's own rules.
fn combo(line: &str) -> Result<OrderComboWire, String> {
    let argv = ["order", "combo"].into_iter().chain(line.split_whitespace()).map(String::from);
    let cli = crate::cmd::trade::parse(argv).map_err(|stop| stop.to_string())?;
    let Some(crate::cmd::trade::Sub::Order { verb: crate::cmd::trade::order::OrderVerb::Combo(c) }) =
        cli.sub
    else {
        panic!("{line}: not read as a combo");
    };
    match super::combo_verb(&c).map_err(|e| e.message)? {
        Verb::Combo(spec) => Ok(spec),
        other => panic!("{line}: not a combo: {other:?}"),
    }
}

fn leg(symbol: &str, ratio: i32, limit: Option<f64>) -> ComboLegArg {
    ComboLegArg { symbol: symbol.into(), ratio, limit }
}

#[test]
fn a_net_combo_and_a_leg_limit_combo_build_the_shared_spec() {
    assert_eq!(
        combo(
            "deribit/ALT buy 2 --leg BTC-27DEC26-100000-C:1 --leg BTC-27DEC26-110000-C:-1 --net -0.03"
        ),
        combo_of(
            "deribit".into(),
            Some("ALT".into()),
            1,
            2.0,
            vec![leg("BTC-27DEC26-100000-C", 1, None), leg("BTC-27DEC26-110000-C", -1, None)],
            Some(-0.03),
            TimeInForce::Gtc,
        ),
        "a credit net is a negative number"
    );
    assert_eq!(
        combo("deribit sell 1 --leg C1:1@0.05 --leg C2:-2@0.02"),
        combo_of(
            "deribit".into(),
            None,
            -1,
            1.0,
            vec![leg("C1", 1, Some(0.05)), leg("C2", -2, Some(0.02))],
            None,
            TimeInForce::Gtc,
        )
    );
    let market = combo("deribit buy 1 --leg C1:1 --leg C2:-1").expect("no net, no limit");
    assert_eq!(market.net_limit, None);
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let CommandWire::Combo(sent) =
        fill_order_client_id(CommandWire::Combo(market.clone()), &mut minter, None, |_| true)
    else {
        panic!("a combo stays one");
    };
    assert!(sent.order_client_id.is_some(), "the client mints the combo's one id");
    assert!(sent.order_client_id.as_deref().is_some_and(|id| id.starts_with('C')), "{sent:?}");
    assert_eq!(OrderComboWire { order_client_id: None, ..sent }, market, "and nothing else");
}

/// **`--order-client-id` pins the combo's one id**, held to `submit --order-client-id`'s charset; the minter never overwrites it.
#[test]
fn order_client_id_pins_the_combos_one_id_in_the_venue_charset() {
    let pinned =
        combo("deribit buy 1 --leg C1:1 --leg C2:-1 --net 0.01 --order-client-id myCombo1")
            .expect("ok");
    assert_eq!(pinned.order_client_id.as_deref(), Some("myCombo1"));
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let sent =
        fill_order_client_id(CommandWire::Combo(pinned.clone()), &mut minter, None, |_| true);
    assert_eq!(sent, CommandWire::Combo(pinned), "a pinned id is never overwritten");
    let why = combo("deribit buy 1 --leg C1:1 --leg C2:-1 --order-client-id MY-PINNED-1")
        .expect_err("charset");
    assert!(
        why.contains("error [order.edge.order_client_id_charset]: \"MY-PINNED-1\" is not a usable order_client_id"),
        "{why}"
    );
}

/// **`--time-in-force` is the combo's lifetime, in `submit --time-in-force`'s words**: absent is `gtc` (the absent
/// wire field), a word rides on a priced combo, and what the door would refuse is refused here: no word, post-only
/// (no book models one; the MCP tool's words too), a `gtd` (a combo carries no expiry), a limit's lifetime on a market
/// combo.
#[test]
fn time_in_force_is_the_combos_lifetime_in_submits_words() {
    let ioc =
        combo("deribit buy 1 --leg C1:1 --leg C2:-1 --net 0.01 --time-in-force IOC").expect("ioc");
    assert_eq!(ioc.time_in_force.as_deref(), Some("ioc"));
    let gtc =
        combo("deribit buy 1 --leg C1:1 --leg C2:-1 --net 0.01 --time-in-force gtc").expect("gtc");
    assert_eq!(gtc.time_in_force, None, "gtc goes out as the absent field");
    for (line, said) in [
        (
            "--net 0.01 --time-in-force forever",
            "--time-in-force \"forever\" is not a time in force",
        ),
        (
            "--net 0.01 --time-in-force post-only",
            "combo: a combo cannot be post-only: no book models a maker-only combo",
        ),
        ("--net 0.01 --time-in-force gtd", "`gtd` needs an expiry, and a combo carries none"),
        ("--time-in-force fok", "belongs to a limit order"),
    ] {
        let line = format!("deribit buy 1 --leg C1:1 --leg C2:-1 {line}");
        let why = combo(&line).expect_err(&line);
        assert!(why.contains(said), "{line}: {said:?} not in {why}");
    }
}

/// **`--tif` is the retired spelling of `--time-in-force`**: refused by name before any other rule of the verb (a
/// combo with no legs at all included), never read as the lifetime it used to set.
#[test]
fn the_retired_tif_flag_is_refused_naming_time_in_force() {
    let want = crate::cmd::trade::plane::renamed("--tif", "--time-in-force");
    assert!(want.starts_with("error [cli.trade.renamed]: "), "{want}");
    assert!(want.contains("`--time-in-force`"), "{want}");
    for line in [
        "deribit buy 1 --leg C1:1 --leg C2:-1 --net 0.01 --tif ioc",
        "deribit buy 1 --leg C1:1 --leg C2:-1 --net 0.01 --tif=ioc",
        "deribit buy 1 --tif ioc",
    ] {
        assert_eq!(combo(line).expect_err(line), want, "{line}");
    }
}

#[test]
fn a_malformed_combo_is_refused_naming_what_is_wrong() {
    for (line, said) in [
        ("deribit buy 1 --leg C1:1@0.05 --leg C2:-1@0.02 --net 0.03", "never both"),
        ("deribit buy 1 --leg C1:1@0.05 --leg C2:-1", "leg 1 states no limit"),
        ("deribit buy 1 --leg C1:1", "needs 2..=N legs"),
        ("deribit buy 1 --leg C1 --leg C2:-1", "--leg is <symbol>:<ratio>[@<limit>]"),
        ("deribit buy 1 --leg C1:x --leg C2:-1", "--leg is <symbol>:<ratio>[@<limit>]"),
        ("deribit buy 1 --leg C1:0 --leg C2:-1", "leg 0 has ratio 0"),
        ("deribit buy 0 --leg C1:1 --leg C2:-1", "qty must be finite and > 0"),
        ("deribit 1 --leg C1:1 --leg C2:-1", "needs <account> <buy|sell> <qty>"),
        (
            "deribit buy 1 --leg C1:1 --leg C2:-1 --reason roll --net 0.03",
            "`--net` appears after --reason",
        ),
    ] {
        let why = combo(line).expect_err(line);
        assert!(why.contains(said), "{line}: {said:?} not in {why}");
    }
}
