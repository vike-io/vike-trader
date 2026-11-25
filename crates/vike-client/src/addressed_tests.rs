use serde_json::json;
use vike_model::TradingState;
use vike_model::{AccountTier, ContractValue};
use vike_tradehub_client::wire::{
    ArmingNotRunningViewWire, CommandWire, ContractValueWire, CoreSnapshotWire, OrderRequestWire,
    VenueBlockWire,
};

use super::{contract_value_of, guardrail_of, tier_of};
use crate::verbs::GuardrailCapabilities;

/// A published block under `route_key`, its venue and label read back off the key.
fn block(route_key: &str, tier: Option<&str>, symbols: &[&str]) -> VenueBlockWire {
    let (venue, account) =
        route_key.split_once('#').map_or((route_key, None), |(v, a)| (v, Some(a)));
    let mut block: VenueBlockWire = serde_json::from_value(json!({
        "venue": venue, "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0, "funding_paid": 0.0,
        "equity": 0.0, "unrealized": 0.0, "missing_prices": 0, "margin_used": 0.0, "free_buying_power": 0.0,
        "trading_state": "Active", "positions": [],
    }))
    .expect("a block");
    block.account = account.map(str::to_string);
    block.route_key = route_key.to_string();
    block.tier = tier.map(str::to_string);
    block.symbols = symbols.iter().map(|s| (*s).to_string()).collect();
    block
}

fn row(symbol: &str, kind: &str, per_unit: f64) -> ContractValueWire {
    ContractValueWire { symbol: symbol.into(), kind: kind.into(), per_unit }
}

/// A node shaped like a real one: the rows are what each venue's own facts state (decision 0170 for
/// deribit and ibkr, decision 0183 for a converted pair).
fn node() -> CoreSnapshotWire {
    let mut deribit = block("deribit", Some("demo"), &["BTC-PERPETUAL", "ETH-PERPETUAL"]);
    deribit.contract_values =
        vec![row("BTC-PERPETUAL", "inverse", 1.0), row("ETH-PERPETUAL", "inverse", 1.0)];
    let mut ibkr = block("ibkr", Some("demo"), &["ESZ6"]);
    ibkr.contract_values = vec![row("ESZ6", "linear", 50.0)];
    let mut ig = block("ig", Some("demo"), &["CS.D.EURUSD.MINI.IP"]);
    ig.contract_values = vec![row("CS.D.EURUSD.MINI.IP", "linear", 10_000.0)];
    let mut oanda = block("oanda", Some("demo"), &["EURUSD", "USDJPY"]);
    // A rate of 1/128, exact in binary so the row below can compare the product.
    oanda.contract_values = vec![row("USDJPY", "linear", 0.0078125)];
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues = vec![
        deribit,
        ibkr,
        ig,
        oanda,
        block("okx", Some("demo"), &["BTC-USDT-SWAP"]),
        block("hyperliquid", Some("demo"), &["BTC"]),
        block("hyperliquid#LIVE", Some("live"), &["BTC"]),
        block("bybit#DEMO", Some("demo"), &["BTCUSDT"]),
        block("bybit#LIVE", Some("live"), &["BTCUSDT"]),
        block("polymarket", Some("live"), &[]),
        block("binance", Some("demo"), &["BTCUSDT"]),
        block("binance#PERP", Some("live"), &["BTCUSDT.P", "ETHUSDT.P"]),
    ];
    frame
}

fn order(
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    qty: f64,
    price: Option<f64>,
) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: if price.is_some() { "limit".into() } else { "market".into() },
        price,
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// **Both directions, venue by venue, against a ceiling of 100,000.** What `qty x price` read each
/// one as is in the row's comment; the deribit, bybit, okx, hyperliquid and polymarket answers of that
/// formula are the ones measured against the CI box's node on 2026-11-07. `UNDER` is the dangerous way
/// round: it read within a ceiling the node refuses the order under.
#[test]
fn the_guardrail_values_each_venues_contract_as_the_node_does() {
    let frame = node();
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(100_000.0) };
    let cases: [(&str, CommandWire, Option<f64>, Option<bool>); 12] = [
        // OVER: was 381,235 and OVER LIMIT, for a true $10 the node accepts.
        (
            "deribit $10",
            order("deribit", None, "BTC-PERPETUAL", 10.0, Some(38_123.5)),
            Some(10.0),
            Some(true),
        ),
        // UNDER: was 20,000 and within limits; the node refuses 200,000.
        (
            "deribit low price",
            order("deribit", None, "BTC-PERPETUAL", 200_000.0, Some(0.1)),
            Some(200_000.0),
            Some(false),
        ),
        // UNDER: was no notional and within limits; the node refuses 200,000.
        (
            "deribit market",
            order("deribit", None, "BTC-PERPETUAL", 200_000.0, None),
            Some(200_000.0),
            Some(false),
        ),
        // UNDER: was 15,000 and within limits; an E-mini's multiplier is 50.
        (
            "ibkr future",
            order("ibkr", None, "ESZ6", 3.0, Some(5_000.0)),
            Some(750_000.0),
            Some(false),
        ),
        // UNDER: was 15 and within limits, by the epic's 10,000 per unit.
        (
            "ig epic",
            order("ig", None, "CS.D.EURUSD.MINI.IP", 10.0, Some(1.5)),
            Some(150_000.0),
            Some(false),
        ),
        // OVER: was 128,000 and OVER LIMIT; the yen quote converts into the account's dollars.
        (
            "oanda cross",
            order("oanda", None, "USDJPY", 1_000.0, Some(128.0)),
            Some(1_000.0),
            Some(true),
        ),
        // The linear ones were right, and are.
        (
            "okx swap",
            order("okx", None, "BTC-USDT-SWAP", 1.0, Some(50_000.0)),
            Some(50_000.0),
            Some(true),
        ),
        (
            "hyperliquid",
            order("hyperliquid", Some("DEFAULT"), "BTC", 3.0, Some(50_000.0)),
            Some(150_000.0),
            Some(false),
        ),
        // NOT PRICED, never within limits: a coin-margined bybit symbol the engine does not trade
        // (the door refuses it), a symbol whose facts the frame lacks, the account-wide polymarket
        // engine, and a market order on a linear contract (was no notional and within limits).
        (
            "bybit coin-margined",
            order("bybit", Some("DEMO"), "BTCUSD", 10.0, Some(100_000.0)),
            None,
            None,
        ),
        ("okx foreign", order("okx", None, "SOL-USDT-SWAP", 1.0, Some(150.0)), None, None),
        ("polymarket", order("polymarket", None, "81360344", 1_000.0, Some(0.5)), None, None),
        ("okx market", order("okx", None, "BTC-USDT-SWAP", 1.0, None), None, None),
    ];
    for (name, cmd, notional, within) in cases {
        let guard = guardrail_of(&cmd, capabilities, Ok(&frame)).to_json();
        assert_eq!(guard["notional"], json!(notional), "{name}: {guard}");
        assert_eq!(guard["within_limits"], json!(within), "{name}: {guard}");
        assert_eq!(guard["unpriced"].is_string(), notional.is_none(), "{name}: {guard}");
    }
}

/// **A tier change lists the account under its NEW tier while the stopped engine's block keeps the OLD one**
/// (`crates/vike-tradehub/src/account_follow.rs`'s `stop_what_the_rows_stopped`, and it stays that way when the new
/// tier's start is refused). A match on key and tier reads that frame as an engine that runs: the door would pass an
/// order to a tombstone, and `trade health` would say RUNNING. So the route key alone says who is stopped.
#[test]
fn an_account_restarting_at_another_tier_is_stopped_whatever_tier_its_old_block_says() {
    let mut frame = node();
    frame.venues.push(block("okx#ALT", Some("demo"), &["BTC-USDT-SWAP"]));
    frame.not_running.push(ArmingNotRunningViewWire {
        venue: "okx".into(),
        account: Some("ALT".into()),
        route_key: "okx#ALT".into(),
        tier: Some("live".into()),
        reason: "tier changed, restarting at `live`".into(),
        detail: None,
        block_word: None,
        refusal_id: None,
    });
    let cmd = order("okx", Some("ALT"), "BTC-USDT-SWAP", 1.0, Some(1.0));
    let why = tier_of(&cmd, Ok(&frame)).expect("it names one account").unwrap_err();
    assert!(why.contains("NOT RUNNING"), "{why}");
    let old_block = frame.venues.last().expect("the block just pushed");
    assert!(
        !frame.not_running.iter().any(|n| {
            n.route_key == old_block.route_key
                && n.tier.as_ref().is_none_or(|tier| old_block.tier.as_ref() == Some(tier))
        }),
        "the key-and-tier rule reads this stopped engine as running: that is why it is not the rule here"
    );
}

#[test]
fn a_modify_is_sized_by_the_order_it_names_and_is_not_priced_when_the_frame_lacks_it() {
    let mut frame = node();
    let resting: serde_json::Value = json!({
        "order_client_id": "c-9", "venue": "deribit", "symbol": "BTC-PERPETUAL", "side": 1, "qty": 10.0,
        "order_type": "limit", "price": 50_000.0, "trigger_price": null, "status": "ACCEPTED",
        "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
    });
    frame.orders = vec![serde_json::from_value(resting).expect("an order")];
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(100_000.0) };
    let modify = |order_client_id: &str, new_price| CommandWire::Modify {
        order_client_id: order_client_id.into(),
        new_qty: Some(200_000.0),
        new_price,
        new_trigger_price: None,
    };
    // An inverse contract needs no price: the door refuses this at 200,000.
    let g = guardrail_of(&modify("c-9", None), capabilities, Ok(&frame));
    assert_eq!(g.to_json()["within_limits"], json!(false), "{}", g.to_json());
    let unknown = modify("nosuch", Some(1.0));
    let g = guardrail_of(&unknown, capabilities, Ok(&frame)).to_json();
    assert_eq!(g["within_limits"], json!(null), "{g}");
    assert!(g["unpriced"].as_str().is_some_and(|why| why.contains("nosuch")), "{g}");
}

/// **A modify of an OLDER node's `Accepted` order is judged against the position, as the door judges it**: the
/// status word reads through the shared `vike_tradehub_client::observe_decode::order_status_of`, so a sell that
/// covers the long is a CLOSE the door never sizes, whichever spelling the node sent. Before the shared reading the
/// Debug word was unknown, the order was no live one, and the modify was sized against the cap. A word no spelling
/// reads is still no live order (the CONTROL).
#[test]
fn a_modify_of_an_older_nodes_accepted_order_is_judged_as_the_close_it_is() {
    let mut frame = node();
    let okx = frame.venues.iter_mut().find(|b| b.route_key == "okx").expect("the okx block");
    okx.positions = vec![vike_tradehub_client::wire::PositionViewWire {
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        position_side: "BOTH".into(),
        size: 5.0,
        average_price: 100.0,
        unrealized: 0.0,
        leverage: 1.0,
        liquidation_price: 0.0,
        mark_source: None,
    }];
    let resting = |status: &str| -> vike_tradehub_client::wire::OrderViewWire {
        serde_json::from_value(json!({
            "order_client_id": "c-9", "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": -1, "qty": 1.0,
            "order_type": "limit", "price": 100.0, "trigger_price": null, "status": status,
            "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
        }))
        .expect("an order")
    };
    let modify = CommandWire::Modify {
        order_client_id: "c-9".into(),
        new_qty: Some(2.0),
        new_price: Some(100.0),
        new_trigger_price: None,
    };
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1.0) };
    for word in ["Accepted", "ACCEPTED"] {
        frame.orders = vec![resting(word)];
        let g = guardrail_of(&modify, capabilities, Ok(&frame)).to_json();
        assert_eq!(g["within_limits"], json!(true), "{word}: a close is not sized: {g}");
        assert!(g["note"].is_string(), "{word}: {g}");
    }
    frame.orders = vec![resting("SOMETHING_NEW")];
    let g = guardrail_of(&modify, capabilities, Ok(&frame)).to_json();
    assert_eq!(g["within_limits"], json!(false), "an unread word is no live order: sized, {g}");
}

#[test]
fn the_tier_is_the_one_of_the_engine_the_door_addresses() {
    let frame = node();
    for (name, cmd, want) in [
        (
            "two accounts: the named one",
            order("hyperliquid", Some("LIVE"), "BTC", 1.0, None),
            AccountTier::Live,
        ),
        (
            "two accounts: DEFAULT",
            order("hyperliquid", Some("DEFAULT"), "BTC", 1.0, None),
            AccountTier::Demo,
        ),
        ("a label", order("bybit", Some("DEMO"), "BTCUSDT", 1.0, None), AccountTier::Demo),
        // The lane twin is `live` here and its default `demo`, so the two cannot be mistaken.
        (
            "a perpetual naming no account",
            order("binance", None, "BTCUSDT.P", 1.0, None),
            AccountTier::Live,
        ),
        (
            "a perpetual naming DEFAULT",
            order("binance", Some("DEFAULT"), "ethusdt.p", 1.0, None),
            AccountTier::Live,
        ),
        ("spot on a twin's venue", order("binance", None, "BTCUSDT", 1.0, None), AccountTier::Demo),
    ] {
        assert_eq!(tier_of(&cmd, Ok(&frame)), Some(Ok(want)), "{name}");
    }
    // A label the twin table does not name is that account's own, `.P` or not.
    let mut alt = node();
    alt.venues.push(block("binance#ALT", Some("demo"), &["BTCUSDT.P"]));
    let cmd = order("binance", Some("ALT"), "BTCUSDT.P", 1.0, None);
    assert_eq!(tier_of(&cmd, Ok(&alt)), Some(Ok(AccountTier::Demo)));
    // A block under the twin's label off the twin's lane (a spot symbol) is not the twin: the default keeps the order.
    let mut no_twin = node();
    no_twin.venues.retain(|b| b.route_key != "binance#PERP");
    no_twin.venues.push(block("binance#PERP", Some("live"), &["ETHUSDT"]));
    let cmd = order("binance", None, "BTCUSDT.P", 1.0, None);
    assert_eq!(tier_of(&cmd, Ok(&no_twin)), Some(Ok(AccountTier::Demo)));
    // …while on ANOTHER symbol of the twin's lane it is the twin (its symbol is its parent row's own, owner
    // 2026-11-13: the table names the lane, never a symbol).
    let mut other_lane_symbol = node();
    other_lane_symbol.venues.retain(|b| b.route_key != "binance#PERP");
    other_lane_symbol.venues.push(block("binance#PERP", Some("live"), &["ETHUSDT.P"]));
    assert_eq!(tier_of(&cmd, Ok(&other_lane_symbol)), Some(Ok(AccountTier::Live)));
}

/// **A bare venue reads as the door reads it**, so the preview and the send cannot disagree: the one account of the
/// venue whatever its label, and no tier at all where it has several, a stopped one among them.
#[test]
fn a_bare_venue_is_the_tier_of_the_one_account_and_of_none_where_it_has_several() {
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues = vec![
        block("okx#DEMO", Some("demo"), &["BTC-USDT-SWAP"]),
        block("bybit#LIVE", Some("live"), &["BTCUSDT"]),
    ];
    frame.not_running = vec![ArmingNotRunningViewWire {
        venue: "bybit".into(),
        account: Some("DEMO".into()),
        route_key: "bybit#DEMO".into(),
        tier: Some("demo".into()),
        reason: "account inactive".into(),
        detail: None,
        block_word: None,
        refusal_id: None,
    }];
    let okx = order("okx", None, "BTC-USDT-SWAP", 1.0, Some(1.0));
    assert_eq!(tier_of(&okx, Ok(&frame)), Some(Ok(AccountTier::Demo)));

    let bybit = order("bybit", None, "BTCUSDT", 1.0, Some(1.0));
    let why = tier_of(&bybit, Ok(&frame)).expect("an order names one account").unwrap_err();
    assert!(why.contains("bybit/DEMO") && why.contains("bybit/LIVE"), "{why}");
    let why = contract_value_of(&bybit, Ok(&frame)).unwrap_err();
    assert!(why.contains("bybit/DEMO") && why.contains("bybit/LIVE"), "{why}");
}

#[test]
fn an_order_named_by_id_is_the_tier_of_the_account_it_rests_in() {
    let mut frame = node();
    let resting = |order_client_id: &str, account: Option<&str>| {
        serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "hyperliquid", "account": account, "symbol": "BTC",
            "side": 1, "qty": 1.0, "order_type": "limit", "price": 1.0, "trigger_price": null,
            "status": "ACCEPTED", "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
        }))
        .expect("an order")
    };
    frame.orders = vec![resting("c-live", Some("LIVE")), resting("c-demo", None)];
    let cancel = |order_client_id: &str| CommandWire::Cancel(order_client_id.into());
    assert_eq!(tier_of(&cancel("c-live"), Ok(&frame)), Some(Ok(AccountTier::Live)));
    assert_eq!(tier_of(&cancel("c-demo"), Ok(&frame)), Some(Ok(AccountTier::Demo)));
    let unknown = tier_of(&cancel("nosuch"), Ok(&frame)).expect("a cancel names one account");
    assert!(unknown.is_err_and(|why| why.contains("nosuch")));
}

/// A verb that names no one account has no tier to state, and says so by carrying no answer at all,
/// whether or not a frame could be read: `Err` would claim there was a tier the node did not name.
#[test]
fn a_verb_that_names_no_one_account_has_no_tier_to_state() {
    let frame = node();
    for cmd in [
        CommandWire::SetTradingState(TradingState::Halted),
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
        CommandWire::UnmountStrategy { controller_id: "m".into() },
        CommandWire::SetSetting {
            file: "policy".into(),
            key: "policy.x".into(),
            value: "1".into(),
            confirm: None,
        },
    ] {
        assert_eq!(tier_of(&cmd, Ok(&frame)), None, "{cmd:?}");
        assert_eq!(tier_of(&cmd, Err("no node")), None, "{cmd:?}");
    }
}

/// Every way of not knowing is an `Err` that says which, never a tier or a default value: the
/// preview's "absent verdict never reads as an approving one".
#[test]
fn what_the_frame_cannot_say_is_said_never_guessed() {
    let cmd = order("okx", None, "BTC-USDT-SWAP", 1.0, Some(1.0));
    let said = |frame: Result<&CoreSnapshotWire, &str>, needle: &str| {
        let tier = tier_of(&cmd, frame).expect("an order names one account").unwrap_err();
        assert!(tier.contains(needle), "{tier}");
        let value = contract_value_of(&cmd, frame).unwrap_err();
        assert!(value.contains(needle), "{value}");
    };
    said(Err("could not be asked"), "could not be asked");
    said(Ok(&CoreSnapshotWire::empty()), "has not folded");
    let mut stopped = node();
    stopped.not_running = vec![ArmingNotRunningViewWire {
        venue: "okx".into(),
        account: None,
        route_key: "okx".into(),
        tier: Some("demo".into()),
        reason: "adapter failed".into(),
        detail: None,
        block_word: None,
        refusal_id: None,
    }];
    said(Ok(&stopped), "NOT RUNNING");
    let mut other = node();
    other.venues.retain(|b| b.venue != "okx");
    said(Ok(&other), "no engine under `okx`");
    // Bare `bybit`: its two accounts are both labelled, and the door refuses that.
    let bare = order("bybit", None, "BTCUSDT", 1.0, Some(1.0));
    assert!(tier_of(&bare, Ok(&node())).expect("an account").is_err());
    let bad = order("okx", Some("not a label!"), "BTC-USDT-SWAP", 1.0, Some(1.0));
    assert!(tier_of(&bad, Ok(&node())).expect("an account").is_err());

    // The tier alone: unnamed by an older node, or a word no parser takes.
    for (word, needle) in [(None, "does not name"), (Some("quantum"), "no tier")] {
        let mut frame = node();
        frame.venues.iter_mut().find(|b| b.venue == "okx").expect("okx").tier =
            word.map(str::to_string);
        let why = tier_of(&cmd, Ok(&frame)).expect("an account").unwrap_err();
        assert!(why.contains(needle), "{why}");
    }

    // The value alone: a row a client cannot read is no value.
    for bad in
        [row("ESZ6", "quanto", 1.0), row("ESZ6", "linear", 0.0), row("ESZ6", "linear", f64::NAN)]
    {
        let mut frame = node();
        frame.venues.iter_mut().find(|b| b.venue == "ibkr").expect("ibkr").contract_values =
            vec![bad];
        let cmd = order("ibkr", None, "ESZ6", 1.0, Some(1.0));
        assert!(contract_value_of(&cmd, Ok(&frame)).is_err());
    }
}

/// A symbol with no row is the default only because the engine says it trades it.
#[test]
fn no_row_reads_as_the_default_only_for_a_symbol_the_engine_trades() {
    let frame = node();
    let traded = order("okx", None, "BTC-USDT-SWAP", 1.0, None);
    assert_eq!(contract_value_of(&traded, Ok(&frame)), Ok(ContractValue::default()));
    let inverse = order("deribit", None, "BTC-PERPETUAL", 1.0, None);
    assert_eq!(contract_value_of(&inverse, Ok(&frame)), Ok(ContractValue::inverse(1.0)));
}
