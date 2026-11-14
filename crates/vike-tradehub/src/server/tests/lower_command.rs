//! The B4 lowering edge in isolation: the wire `UpdateParams` payload IS the core's own
//! `StrategyParams` serde schema, and an undecodable payload is a REFUSAL, never a silent
//! drop. (The end-to-end proof — the lowered command reaching a mounted strategy through the
//! real server — is `tests/control_roundtrip.rs`'s
//! `update_params_over_the_wire_retunes_the_mounted_strategy`.)

use super::control::{command_kind, lower_command};
use super::refusal::bracket_refusal;
use vike_execution::{Command, OrderIntent};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::StrategyParams;
use vike_tradehub_client::wire::CommandWire;

/// A fully-populated `SpreadMakerParams` payload built from the REAL core type — what proves
/// "the wire schema IS the core schema" rather than a lookalike.
pub(super) fn spread_maker_params(qty: f64, half_spread: f64) -> StrategyParams {
    StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
        qty,
        half_spread,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: vike_model::QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    })
}

#[test]
fn update_params_lowers_into_the_real_core_command() {
    let wire = CommandWire::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: Some("maker-a".into()),
        // The core's own externally-tagged form — proven against the REAL type below.
        params: serde_json::to_value(spread_maker_params(2.0, 1.0)).unwrap(),
    };
    let (cmd, order_client_id) = lower_command(wire).expect("a decodable payload lowers");
    assert_eq!(order_client_id, "", "account-wide-verb convention: nothing order-scoped to echo");
    let Command::UpdateParams(u) = cmd else {
        panic!("must lower to Command::UpdateParams, got a different command");
    };
    assert_eq!(
        (u.venue.as_str(), u.symbol.as_str(), u.interval.as_str()),
        ("binance", "BTCUSDT", "1m")
    );
    assert_eq!(u.mount_id.as_deref(), Some("maker-a"), "the mount id is carried to the core");
    let StrategyParams::SpreadMaker(p) = u.params else {
        panic!("the typed variant survives the wire round-trip");
    };
    assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "the payload's knobs land verbatim");
    assert_eq!(p.half_spread.to_bits(), 1.0_f64.to_bits());
}

/// An update that names no mount lowers with none: the core's refusal of an ambiguous series is
/// its own business, the edge adds no id of its own.
#[test]
fn an_unaddressed_update_params_lowers_with_no_mount_id() {
    let wire = CommandWire::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: None,
        params: serde_json::to_value(spread_maker_params(2.0, 1.0)).unwrap(),
    };
    let (Command::UpdateParams(u), _) = lower_command(wire).expect("lowers") else {
        panic!("must lower to Command::UpdateParams");
    };
    assert_eq!(u.mount_id, None);
}

#[test]
fn an_undecodable_params_payload_is_refused_not_dropped() {
    let wire = CommandWire::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: None,
        params: serde_json::json!({"NoSuchStrategyParams": {"qty": 1.0}}),
    };
    let err = lower_command(wire).expect_err("an unknown variant must refuse");
    assert!(err.contains("update_params"), "names the verb: {err}");
}

#[test]
fn update_params_has_its_own_audit_kind() {
    let wire = CommandWire::UpdateParams {
        venue: "b".into(),
        symbol: "s".into(),
        interval: "1m".into(),
        mount_id: None,
        params: serde_json::json!({}),
    };
    assert_eq!(command_kind(&wire), "update_params");
}

/// The runtime MOUNT's `params`: the node wire keeps JSON, `MountSpec` holds
/// `vike_model::params::Params`, and `lower_command` is the one conversion between them.
///
/// ⚠ This test binary declares no project, so a mount whose `file` passes every pure refusal is
/// refused at the door's file read (`crate::mount_factory`'s `wire_file_refusal`): no lowering here
/// gets as far as a `Command`. `crates/vike-tradehub/tests/wire_script_fence.rs` drives the read on a
/// daemon that has one.
mod mount_params_tests {
    use super::*;

    fn mount(params: serde_json::Value) -> CommandWire {
        CommandWire::MountStrategy {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: Some("rt-a".into()),
            file: Some("grid_dca/grid.rs".into()),
            name: None,
            rhai: None,
            params,
        }
    }

    /// The lowering's conversion writes the wire's `params` JSON back byte for byte — an integer
    /// stays an integer, `2.0` stays a float, a string and a bool are themselves — so what the core
    /// records in its topology sidecar is the text the peer sent. The lowering reads the table with
    /// `Params`' own `Deserialize` (the conversion below, verbatim), and a table it accepts gets past
    /// it to the file read, the refusal that follows the conversion here.
    #[test]
    fn a_mount_lowers_its_params_into_the_same_json() {
        const GOLDEN: &str = r#"{"anchor":"first","bounded01":false,"rungs":2,"size":2.0,"step":1.5,"symbol":"BTCUSDT","tick":0.01}"#;
        let wire_params: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
        let params: vike_model::params::Params =
            serde_json::from_value(wire_params.clone()).expect("a parameter table");
        assert_eq!(serde_json::to_string(&params).unwrap(), GOLDEN);
        assert_eq!(params.get("rungs").and_then(|v| v.as_integer()), Some(2));
        assert_eq!(params.get("size").and_then(|v| v.as_float()), Some(2.0));
        let err = lower_command(mount(wire_params)).expect_err("no project: no file to read");
        assert!(!err.contains("is not a parameter table"), "the table converted: {err}");
        assert!(err.contains("project"), "it stopped at the file read: {err}");
    }

    /// What a parameter table cannot hold is REFUSED at the lowering, naming the verb, with the
    /// sentence `mount_factory`'s `malformed_specs_refuse` asserted while `MountSpec::params` was
    /// still JSON and the refusal happened in its `profile_for`.
    #[test]
    fn a_mount_whose_params_is_not_a_parameter_table_is_refused_naming_the_verb() {
        for params in [
            serde_json::Value::Null,
            serde_json::json!(5),
            serde_json::json!([1]),
            serde_json::json!({"size": null}),
        ] {
            let err =
                lower_command(mount(params.clone())).expect_err("a non-table params must refuse");
            assert!(err.contains("mount_strategy"), "names the verb: {err}");
            assert!(err.contains("is not a parameter table"), "{params}: {err}");
        }
    }

    /// A process that declared no project has no strategies folder to read a file from, so the wire
    /// file is REFUSED rather than opened (this test binary declares none).
    /// `crates/vike-tradehub/tests/wire_script_fence.rs` drives the fence on a daemon that has one.
    #[test]
    fn a_wire_file_is_refused_on_a_process_with_no_project() {
        let err = lower_command(mount(serde_json::json!({})))
            .expect_err("no project: nothing to fence against");
        assert!(err.contains("mount_strategy"), "names the verb: {err}");
        assert!(err.contains("project"), "says what the file must be inside: {err}");
    }

    /// The retired selections are refused at the lowering's pure edge, before any file is looked
    /// for, each with the fix: a strategy is a FILE.
    #[test]
    fn a_retired_name_or_rhai_is_refused_with_the_fix() {
        for (name, rhai) in [(Some("grid"), None), (None, Some("strategies/x.rhai"))] {
            let wire = CommandWire::MountStrategy {
                venue: "binance".into(),
                account: None,
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: None,
                file: None,
                name: name.map(str::to_string),
                rhai: rhai.map(str::to_string),
                params: serde_json::json!({}),
            };
            let err = lower_command(wire).expect_err("a retired selection refuses");
            assert!(err.contains("mount_strategy"), "names the verb: {err}");
            assert!(err.contains("is retired") && err.contains("file"), "the fix: {err}");
        }
    }
}

/// **THE WIRE-TO-CORE HOP (this task).** A wire `Submit` naming an account must reach the
/// core `OrderRequest` carrying it. Before this arm read the field, the `OrderRequest`
/// literal's `..Default::default()` silently dropped it and every order routed to whichever
/// engine happened to be first, regardless of what the client asked for — routing is still
/// blind to it after this task (Task 4's job), but the value now survives the hop for that
/// task to consume.
mod submit_account_tests {
    use super::*;
    use vike_tradehub_client::wire::OrderRequestWire;

    /// Every field `order_client_id`'s non-empty gate and `OrderRequest`'s construction need,
    /// with `account` the one axis under test.
    fn submit(account: Option<&str>) -> CommandWire {
        CommandWire::Submit(OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
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

    fn submitted_order(wire: CommandWire) -> Result<vike_model::OrderRequest, String> {
        let (cmd, _order_client_id) = lower_command(wire)?;
        let Command::Order(OrderIntent::Submit(order)) = cmd else {
            panic!("must lower to Command::Order(OrderIntent::Submit), got a different command");
        };
        Ok(*order)
    }

    #[test]
    fn a_submit_naming_an_account_carries_it_into_the_core_order() {
        let order = submitted_order(submit(Some("ALT"))).expect("a valid label lowers");
        assert_eq!(
            order.account,
            Some(vike_model::accounts::account_keys::AccountLabel::Named("ALT".to_string())),
            "the account named on the wire must reach the core order"
        );
    }

    /// The wire's reserved spelling for "the unlabelled account, deliberately" —
    /// `parse_wire_account`'s own contract, admitted here exactly as `MountStrategy`'s arm
    /// admits it, where `AccountLabel::parse` (an `account` row's label) refuses it.
    #[test]
    fn a_submit_naming_default_on_the_wire_carries_the_default_account() {
        let order = submitted_order(submit(Some("DEFAULT"))).expect("DEFAULT lowers");
        assert_eq!(order.account, Some(vike_model::accounts::account_keys::AccountLabel::Default));
    }

    /// Absence is unchanged: a client naming no account — every pre-existing caller — still
    /// lowers to `None`, byte-identical to before this task.
    #[test]
    fn a_submit_naming_no_account_lowers_to_none() {
        let order = submitted_order(submit(None)).expect("an account-less submit lowers");
        assert_eq!(order.account, None);
    }

    /// ⚠ The failure this task exists to prevent: `None` routes to the venue's DEFAULT book,
    /// so swallowing a malformed label into `None` (`.ok()`, `.unwrap_or_default()`, …) would
    /// silently trade the wrong account for an operator's typo. It must be REFUSED instead —
    /// the same shape `MountStrategy`'s arm already gives for the same parser's error.
    #[test]
    fn a_submit_naming_an_invalid_account_label_is_refused_not_dropped() {
        let err = submitted_order(submit(Some("bad label"))).expect_err("must refuse");
        assert!(err.contains("account"), "names the field: {err}");
    }

    fn with_order_type(order_type: &str) -> CommandWire {
        let CommandWire::Submit(mut req) = submit(None) else { unreachable!() };
        req.order_type = order_type.into();
        CommandWire::Submit(req)
    }

    /// The wire's `order_type` stays a string; the lowering reads it through the INPUT door
    /// (`OrderType::parse`): trimmed and ASCII case-insensitive, so `"LIMIT"` lowers exactly as
    /// `"limit"` does.
    #[test]
    fn a_submit_order_type_lowers_case_insensitively() {
        for word in ["limit", "LIMIT", " Limit "] {
            let order = submitted_order(with_order_type(word)).expect("a known kind lowers");
            assert_eq!(order.order_type, vike_model::OrderType::Limit, "{word:?}");
        }
        let CommandWire::Submit(mut req) = with_order_type("take_profit") else { unreachable!() };
        req.trigger_price = Some(1.0);
        let order = submitted_order(CommandWire::Submit(req)).unwrap();
        assert_eq!(order.order_type, vike_model::OrderType::TakeProfit);
    }

    /// ⚠ A word that is not one of ours — the empty string included — is REFUSED, naming the
    /// accepted words. It used to be lowered verbatim, and alpaca, oanda, ibkr, ig and the
    /// binance perp lane then sent it as a MARKET order.
    #[test]
    fn a_submit_with_an_unknown_order_type_is_refused_naming_the_accepted_words() {
        for word in ["", "garbage", "stop_market"] {
            let err = submitted_order(with_order_type(word)).expect_err("must refuse");
            assert!(err.contains("order_type"), "{word:?}: names the field: {err}");
            assert!(
                err.contains("market, limit, stop, stop_limit, take_profit"),
                "{word:?}: names the accepted words: {err}"
            );
        }
    }
}

/// **THE WIRE-TO-CORE HOP for the three risk-REDUCING verbs** — the arm that used to
/// destructure them as `{ venue, symbol, .. }` and drop the account on the floor, so a
/// `market-exit binance ALT` reached the core as `market-exit binance` and fanned over every
/// account of the exchange. Same parser as the submit arm above (`parse_wire_account`, the ONE
/// reading of `DEFAULT`), same refusal of a malformed label rather than a silent `None`.
mod reduce_account_tests {
    use super::*;
    use vike_model::accounts::account_keys::AccountLabel;

    fn lowered_account(wire: CommandWire) -> Result<Option<AccountLabel>, String> {
        let (cmd, order_client_id) = lower_command(wire)?;
        assert!(order_client_id.is_empty(), "an account-wide verb echoes no order client id");
        match cmd {
            Command::Order(OrderIntent::MassCancel { account, .. })
            | Command::Order(OrderIntent::Flatten { account, .. })
            | Command::Order(OrderIntent::MarketExit { account, .. }) => Ok(account),
            _ => panic!("a reducing verb must lower to its own reducing intent"),
        }
    }

    fn reducers(account: Option<&str>) -> Vec<CommandWire> {
        let account = account.map(str::to_string);
        vec![
            CommandWire::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: account.clone(),
            },
            CommandWire::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: account.clone(),
            },
            CommandWire::MarketExit { venue: Some("binance".into()), account },
        ]
    }

    #[test]
    fn a_reducing_verb_naming_an_account_carries_it_into_the_core_intent() {
        for wire in reducers(Some("ALT")) {
            let label = format!("{wire:?}");
            assert_eq!(
                lowered_account(wire),
                Ok(Some(AccountLabel::Named("ALT".into()))),
                "{label}: the account named on the wire must reach the core"
            );
        }
    }

    #[test]
    fn a_reducing_verb_naming_default_carries_the_default_account() {
        for wire in reducers(Some("DEFAULT")) {
            let label = format!("{wire:?}");
            assert_eq!(lowered_account(wire), Ok(Some(AccountLabel::Default)), "{label}");
        }
    }

    /// Absence is unchanged — and it is the panic button's shape, so it is asserted for the
    /// unscoped exit too: `None` reaches the core as `None`, and the core fans out.
    #[test]
    fn a_reducing_verb_naming_no_account_lowers_to_none() {
        let mut wires = reducers(None);
        wires.push(CommandWire::MarketExit { venue: None, account: None });
        wires.push(CommandWire::MassCancel { venue: None, symbol: None, account: None });
        for wire in wires {
            let label = format!("{wire:?}");
            assert_eq!(lowered_account(wire), Ok(None), "{label}");
        }
    }

    #[test]
    fn a_reducing_verb_naming_an_invalid_account_label_is_refused_not_dropped() {
        for wire in reducers(Some("bad label")) {
            let label = format!("{wire:?}");
            let err = lowered_account(wire).expect_err("must refuse");
            assert!(err.contains("account"), "{label}: names the field: {err}");
        }
    }
}

/// **THE TP/SL BRACKET at the lowering edge** — the move into the core's own
/// `OrderIntent::Bracket`, and the checks that must happen BEFORE the `Ack` because the core
/// examines a bracket's exits only at RELEASE, after the entry has filled.
mod bracket_tests {
    use super::*;
    use vike_tradehub_client::wire::OrderBracketWire;

    /// A well-formed LONG limit bracket on a binance PERP symbol — every check below varies one
    /// field of it, so a refusal can only be that field's.
    fn spec() -> OrderBracketWire {
        OrderBracketWire {
            order_client_id: None,
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 90.0,
            take_profit: 110.0,
            account: None,
        }
    }

    fn lowered(spec: OrderBracketWire) -> Result<(Command, String), String> {
        lower_command(CommandWire::Bracket(spec))
    }

    /// The bracket lowers into the core's own `OrderIntent::Bracket`, field for field. It echoes NO
    /// order client id: the runtime mints all three after this returns (`vike_model::build_bracket`'s doc).
    #[test]
    fn a_bracket_lowers_field_for_field_and_echoes_no_order_client_id() {
        let wire = CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: -1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 110.0,
            take_profit: 90.0,
            account: None,
        });
        let (cmd, order_client_id) = lower_command(wire).expect("a bracket lowers");
        assert_eq!(
            order_client_id, "",
            "the runtime mints the ids after the Ack, so there is none to echo"
        );
        let Command::Order(OrderIntent::Bracket(spec)) = cmd else {
            panic!("must lower to OrderIntent::Bracket");
        };
        assert_eq!(
            *spec,
            vike_model::OrderBracket {
                venue: "binance".into(),
                symbol: "BTCUSDT.P".into(),
                side: -1,
                qty: 2.0,
                entry_price: Some(100.0),
                stop_loss: 110.0,
                take_profit: 90.0,
                account: None,
                order_client_id: None,
            }
        );
    }

    #[test]
    fn a_bracket_has_its_own_audit_kind() {
        let wire = CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 1.0,
            take_profit: 2.0,
            account: None,
        });
        assert_eq!(command_kind(&wire), "bracket");
    }

    /// ⚠ **`side` is +1 or -1 and nothing else.** The adapters read anything `<= 0` as a SELL, and
    /// `vike_model::build_bracket` gives the exits `-side`, so a `0` would make all three legs sells.
    #[test]
    fn a_bracket_with_side_zero_is_refused() {
        for side in [0, 2, -2] {
            let err = lowered(OrderBracketWire { side, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`side`"), "side {side}: names the field: {err}");
        }
    }

    /// ⚠ The node's notional cap cannot see NaN (`NaN > max` is false) and has no answer at all
    /// without a `max_notional_per_order` row, so the finite check is THIS edge's.
    #[test]
    fn a_bracket_with_a_non_positive_or_non_finite_quantity_is_refused() {
        for qty in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = lowered(OrderBracketWire { qty, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`qty`"), "qty {qty}: names the field: {err}");
        }
    }

    /// A priced entry must be a finite price above zero. `None` is a MARKET entry and is admitted.
    #[test]
    fn a_bracket_with_a_bad_entry_price_is_refused() {
        for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = lowered(OrderBracketWire { entry_price: Some(px), ..spec() })
                .expect_err("must refuse");
            assert!(err.contains("`entry_price`"), "entry {px}: names the field: {err}");
        }
        assert!(
            lowered(OrderBracketWire { entry_price: None, ..spec() }).is_ok(),
            "a market entry"
        );
    }

    /// ⚠ **A bad EXIT is the failure this check exists for.** The core holds both exits and first
    /// sends them when the entry FILLS, and a covered reduce bypasses the risk gate's collar and
    /// floors, so a NaN or non-positive exit would be rejected only after the position was open —
    /// leaving it unprotected. A plain `Submit` carrying the same value fails with no position.
    #[test]
    fn a_bracket_with_a_non_finite_or_non_positive_exit_is_refused() {
        for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err =
                lowered(OrderBracketWire { stop_loss: px, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`stop_loss`"), "stop {px}: names the field: {err}");
            let err =
                lowered(OrderBracketWire { take_profit: px, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`take_profit`"), "take profit {px}: names the field: {err}");
        }
    }

    /// ⚠ **An INVERTED long is refused when the entry is priced**: a buy needs
    /// `stop_loss < entry_price < take_profit`, strictly. A stop at or above the entry, or a
    /// take-profit at or below it, would fire the moment the entry fills.
    ///
    /// ⚠ The converse is NOT promised. The check compares against the LIMIT price, not the fill: a
    /// marketable buy limit fills below its price and can leave a passing stop ABOVE the fill
    /// (`check_bracket`'s declared residual). Passing here is a statement about the frame only.
    #[test]
    fn an_inverted_long_bracket_is_refused() {
        for (stop_loss, take_profit) in
            [(110.0, 120.0), (100.0, 110.0), (90.0, 95.0), (90.0, 100.0)]
        {
            let err = lowered(OrderBracketWire { stop_loss, take_profit, ..spec() })
                .expect_err("must refuse");
            assert!(err.contains("inverted"), "sl {stop_loss} tp {take_profit}: {err}");
        }
    }

    /// ...and the mirror for a short: `take_profit < entry_price < stop_loss`.
    #[test]
    fn an_inverted_short_bracket_is_refused() {
        let short = OrderBracketWire { side: -1, stop_loss: 110.0, take_profit: 90.0, ..spec() };
        assert!(lowered(short.clone()).is_ok(), "the well-formed short");
        for (stop_loss, take_profit) in
            [(90.0, 80.0), (100.0, 90.0), (110.0, 105.0), (110.0, 100.0)]
        {
            let err = lowered(OrderBracketWire { stop_loss, take_profit, ..short.clone() })
                .expect_err("must refuse");
            assert!(err.contains("inverted"), "sl {stop_loss} tp {take_profit}: {err}");
        }
    }

    /// ⚠ **A MARKET entry's exits are checked against EACH OTHER**: there is no entry price to
    /// order them around, but a buy whose stop-loss is not below its take-profit (or a sell whose
    /// is not above it) has one exit on the wrong side of ANY fill, so it is refused with no false
    /// positive.
    ///
    /// ⚠ **DECLARED LIMIT, pinned so it cannot change silently: nothing checks a market entry's
    /// exits against where the market IS.** A well-ordered pair below the market is admitted —
    /// this edge holds no book.
    #[test]
    fn a_market_entry_bracket_is_checked_for_its_exits_order_only() {
        let market = OrderBracketWire { entry_price: None, ..spec() };
        assert!(lowered(market.clone()).is_ok(), "a well-ordered market long: sl 90 < tp 110");
        for (stop_loss, take_profit) in [(110.0, 90.0), (100.0, 100.0)] {
            let err = lowered(OrderBracketWire { stop_loss, take_profit, ..market.clone() })
                .expect_err("a market long needs stop_loss < take_profit");
            assert!(
                err.contains("inverted") && err.contains("market entry"),
                "{stop_loss}/{take_profit}: {err}"
            );
        }
        let short = OrderBracketWire { side: -1, stop_loss: 110.0, take_profit: 90.0, ..market };
        assert!(lowered(short.clone()).is_ok(), "a well-ordered market short: tp 90 < sl 110");
        for (stop_loss, take_profit) in [(90.0, 110.0), (100.0, 100.0)] {
            let err = lowered(OrderBracketWire { stop_loss, take_profit, ..short.clone() })
                .expect_err("a market short needs take_profit < stop_loss");
            assert!(
                err.contains("inverted") && err.contains("market entry"),
                "{stop_loss}/{take_profit}: {err}"
            );
        }
        // The pinned limit: exits well-ordered between themselves but nowhere near any plausible
        // market are admitted, because this edge cannot know where the market is.
        assert!(
            lowered(OrderBracketWire { stop_loss: 1.0, take_profit: 2.0, ..spec_market() }).is_ok()
        );
    }

    /// [`spec`] with a MARKET entry.
    fn spec_market() -> OrderBracketWire {
        OrderBracketWire { entry_price: None, ..spec() }
    }

    /// The ONE engine a bracket on `venue` reaches, as the node publishes it: the block keyed by
    /// the bare venue id (the default account — the only shape `account_refusal` admits a bracket
    /// to), mounted on `symbol`.
    fn engine(venue: &str, symbol: &str) -> vike_execution::VenueBlock {
        vike_execution::VenueBlock {
            venue: venue.into(),
            route_key: venue.into(),
            symbol: symbol.into(),
            ..Default::default()
        }
    }

    /// [`spec`]'s binance PERP engine — the roster every value check below is judged against, so
    /// a refusal can only be the field's.
    fn perp_roster() -> Vec<vike_execution::VenueBlock> {
        vec![engine("binance", "BTCUSDT.P")]
    }

    /// `b` judged as the node judges it past the account gate, against a one-engine roster.
    fn judged(b: OrderBracketWire, roster: &[vike_execution::VenueBlock]) -> Option<String> {
        bracket_refusal(&CommandWire::Bracket(b), roster)
    }

    /// ⚠ **The dry run and the command share ONE bracket verdict.** `bracket_refusal` is the call
    /// `accept_command` makes at its step 1d AND a step of the Preview, so
    /// "Preview agrees with Command" is a property of there being one function. This table pins
    /// what that function refuses:
    ///
    /// * the VALUE shapes, with the reason `lower_command`'s own copy of the value check gives — so
    ///   a caller that lowers a frame without the gate is refused in the same words;
    /// * the ENGINE shapes, which `lower_command` cannot see (it holds no roster) and therefore
    ///   LOWERS: the refusal exists only because `accept_command` runs this same function before
    ///   it lowers. Each names what the engine trades.
    ///
    /// The wire-level twin, sending each shape as a Command AND a Preview to one live node and
    /// requiring the same sentence back, is `tests/daemon/venue_routing.rs`'s
    /// `the_dry_run_reports_the_same_bracket_refusals`.
    #[test]
    fn the_preview_and_the_command_share_one_bracket_verdict() {
        let value_shapes = [
            OrderBracketWire { side: 0, ..spec() },
            OrderBracketWire { qty: f64::NAN, ..spec() },
            OrderBracketWire { entry_price: Some(0.0), ..spec() },
            OrderBracketWire { stop_loss: f64::INFINITY, ..spec() },
            OrderBracketWire { stop_loss: 105.0, ..spec() },
            OrderBracketWire { side: -1, ..spec() },
            OrderBracketWire { stop_loss: 110.0, take_profit: 90.0, ..spec_market() },
        ];
        for b in value_shapes {
            let cmd = CommandWire::Bracket(b);
            let verdict = bracket_refusal(&cmd, &perp_roster());
            assert!(verdict.is_some(), "{cmd:?} must refuse");
            assert_eq!(verdict, lower_command(cmd.clone()).err(), "{cmd:?}: the same reason");
        }
        // (bracket, the one engine it reaches, what the refusal must name)
        let engine_shapes = [
            // ⚠ The I-1 scenario: the old refusal TOLD the operator to use `.P`, and a spot-mounted
            // engine then signed the `.P` bracket on the spot lane.
            (spec(), engine("binance", "BTCUSDT"), "`BTCUSDT`"),
            (
                OrderBracketWire { symbol: "BTCUSDT".into(), ..spec() },
                engine("binance", "BTCUSDT"),
                "spot lane",
            ),
            (
                OrderBracketWire { venue: "aster".into(), ..spec() },
                engine("aster", "BTCUSDT"),
                "spot lane",
            ),
            (
                OrderBracketWire { symbol: "ETHUSDT.P".into(), ..spec() },
                engine("binance", "BTCUSDT.P"),
                "`BTCUSDT.P`",
            ),
            (
                OrderBracketWire { venue: "okx".into(), symbol: "BTC-USDT".into(), ..spec() },
                engine("okx", "BTC-USDT-SWAP"),
                "`BTC-USDT-SWAP`",
            ),
        ];
        for (b, engine_instance, names) in engine_shapes {
            let cmd = CommandWire::Bracket(b);
            let verdict = bracket_refusal(&cmd, std::slice::from_ref(&engine_instance))
                .unwrap_or_else(|| {
                    panic!("{cmd:?} on an engine trading `{}` must refuse", engine_instance.symbol)
                });
            assert!(verdict.contains(names), "names what the engine trades: {verdict}");
            assert!(
                lower_command(cmd).is_ok(),
                "the FRAME is well-formed: only the engine refuses it"
            );
        }
        let ok = CommandWire::Bracket(spec());
        assert_eq!(bracket_refusal(&ok, &perp_roster()), None);
        assert!(lower_command(ok).is_ok());
        assert_eq!(
            bracket_refusal(&CommandWire::Cancel("c-1".into()), &[]),
            None,
            "not a bracket, whatever the roster"
        );
    }

    /// A lower-case `.p` is the SPOT lane, exactly as the adapter reads it
    /// (`vike_catalog::split_perp_at` strips `.P` only): an engine MOUNTED on `BTCUSDT.p` is a
    /// spot-lane engine and holds no bracket. And a bracket typed `BTCUSDT.p` against a perp engine
    /// is refused naming the engine's own `BTCUSDT.P` — the refusal suggests nothing it built
    /// itself, so it can never print `BTCUSDT.p.P`.
    #[test]
    fn a_lower_case_perp_suffix_is_the_spot_lane_and_the_refusal_names_the_real_one() {
        let err = judged(spec(), &[engine("binance", "BTCUSDT.p")]).expect("a `.p` engine is spot");
        assert!(err.contains("spot lane"), "{err}");
        let err = judged(OrderBracketWire { symbol: "BTCUSDT.p".into(), ..spec() }, &perp_roster())
            .expect("`.p` is not a symbol the perp engine trades");
        assert!(err.contains("`BTCUSDT.P`"), "names the engine's perp symbol: {err}");
        assert!(!err.contains(".p.P"), "{err}");
    }

    /// ⚠ **On binance and aster a bracket goes only to an engine mounted on the PERP lane — and it
    /// is the ENGINE's lane, never the frame's.** Their `vike_venues::capabilities_for` rows list `stop` as
    /// the spot+perp UNION, while the spot order builder sends `type=STOP` with no stop price: the
    /// core preflight passes the stop-loss leg, the entry fills, and the venue rejects the stop at
    /// release — leaving the take-profit and an UNPROTECTED position the operator believes is
    /// protected. The adapter picks its lane ONCE, from the symbol its engine was mounted on, and
    /// signs every order on that symbol, so a `.P` FRAME against a spot-mounted engine is still a
    /// spot order: this check reads the engine's published block (`bracket_engine_refusal`). This
    /// test used to read "a `.P` symbol picks the perp lane", which was the defect.
    ///
    /// ⚠ A PAPER mount cannot show this: the paper client fills stops on either lane, which is why
    /// the refusal lives at the edge, in front of every mount, rather than in a caps column this cut
    /// does not add.
    #[test]
    fn a_bracket_to_a_binance_or_aster_spot_engine_is_refused_whatever_symbol_it_names() {
        for venue in ["binance", "aster"] {
            let spot = [engine(venue, "BTCUSDT")];
            for symbol in ["BTCUSDT.P", "BTCUSDT"] {
                let err = judged(
                    OrderBracketWire { venue: venue.into(), symbol: symbol.into(), ..spec() },
                    &spot,
                )
                .unwrap_or_else(|| panic!("{venue}/{symbol} on a spot engine must refuse"));
                assert!(err.contains("spot lane"), "{venue}/{symbol}: names the lane: {err}");
                assert!(err.contains("stop-loss"), "{venue}/{symbol}: what it cannot hold: {err}");
                assert!(
                    err.contains("trades `BTCUSDT`"),
                    "{venue}/{symbol}: what it trades: {err}"
                );
                assert!(
                    !err.contains("`BTCUSDT.P`"),
                    "{venue}/{symbol}: no perp symbol is suggested — this engine trades none: {err}"
                );
            }
            let perp = [engine(venue, "BTCUSDT.P")];
            assert_eq!(
                judged(OrderBracketWire { venue: venue.into(), ..spec() }, &perp),
                None,
                "{venue}: a perp engine holds the bracket"
            );
        }
        // A venue with no spot lane to confuse is judged on its symbol alone: bybit's exec is
        // linear-perp only, and hyperliquid's perps are bare coins.
        for (venue, symbol) in [("bybit", "BTCUSDT"), ("hyperliquid", "BTC")] {
            assert_eq!(
                judged(
                    OrderBracketWire { venue: venue.into(), symbol: symbol.into(), ..spec() },
                    &[engine(venue, symbol)]
                ),
                None,
                "{venue}/{symbol}"
            );
        }
    }

    /// ⚠ **A bracket goes to the ONE engine of its venue, so a symbol that engine does not trade is
    /// refused, naming what it does** — the node-side twin of the desktop's `order_dispatch::tradable`.
    /// Without it the adapter places the bracket on its engine's own instrument: okx's client is
    /// bound to one SWAP instrument, so a spot-named bracket would be placed on the SWAP. Every
    /// symbol the engine publishes counts (`VenueBlock::trades` reads the extra symbols too), and the
    /// engine's own symbol is the only perp `BASE.P` a refusal ever suggests.
    #[test]
    fn a_bracket_on_a_symbol_its_engine_does_not_trade_is_refused_naming_the_engines_symbols() {
        for (b, engine_instance, named) in [
            (
                OrderBracketWire { symbol: "ETHUSDT.P".into(), ..spec() },
                engine("binance", "BTCUSDT.P"),
                "`BTCUSDT.P`",
            ),
            (
                OrderBracketWire { symbol: "BTCUSDT".into(), ..spec() },
                engine("binance", "BTCUSDT.P"),
                "`BTCUSDT.P`",
            ),
            (
                OrderBracketWire { venue: "okx".into(), symbol: "BTC-USDT".into(), ..spec() },
                engine("okx", "BTC-USDT-SWAP"),
                "`BTC-USDT-SWAP`",
            ),
            (
                OrderBracketWire { venue: "bybit".into(), symbol: "ETHUSDT".into(), ..spec() },
                engine("bybit", "BTCUSDT"),
                "`BTCUSDT`",
            ),
        ] {
            let asked = format!("`{}`", b.symbol);
            let err = judged(b, &[engine_instance])
                .expect("a symbol the engine does not trade must refuse");
            assert!(err.contains(named), "names the engine's symbol: {err}");
            assert!(err.contains(&asked), "...and the one the bracket asked for: {err}");
        }
        // An extra symbol the engine publishes is traded.
        let mut wide = engine("binance", "BTCUSDT.P");
        wide.extra_symbols = vec!["ETHUSDT.P".to_string()].into();
        assert_eq!(
            judged(OrderBracketWire { symbol: "ETHUSDT.P".into(), ..spec() }, &[wide]),
            None
        );
    }

    /// ⚠ **No engine to judge is a refusal, never a pass.** `accept_command`'s account gate refuses
    /// an empty roster and a labelled-only venue first, in its own words; this pins that the engine
    /// half, called by any OTHER caller with an empty or stale roster, or handed a block that does
    /// not say what it trades, refuses rather than admitting a bracket nobody checked.
    #[test]
    fn a_bracket_with_no_published_engine_to_judge_it_is_refused() {
        let other_venue = [engine("bybit", "BTCUSDT")];
        let labelled = [vike_execution::VenueBlock {
            route_key: "binance#ALT".into(),
            ..engine("binance", "BTCUSDT.P")
        }];
        let silent = [engine("binance", "")];
        for roster in [&[][..], &other_venue[..], &labelled[..], &silent[..]] {
            let err = judged(spec(), roster).unwrap_or_else(|| panic!("{roster:?} must refuse"));
            assert!(err.contains("cannot tell"), "{err}");
        }
    }

    /// The lane twin's PERP engine as the node publishes it: the second engine of binance, keyed
    /// `binance#PERP`, mounted on the perpetual lane.
    fn twin_engine() -> vike_execution::VenueBlock {
        vike_execution::VenueBlock {
            route_key: "binance#PERP".into(),
            account: vike_model::accounts::account_keys::AccountLabel::parse("PERP").ok(),
            ..engine("binance", "BTCUSDT.P")
        }
    }

    /// **A bracket's ACCOUNT is lowered, by the one parser a submit's is.** A label becomes the
    /// spec's `AccountLabel`, `DEFAULT` is the unlabelled account NAMED (`Some(Default)`, which
    /// routes differently from `None`), absence stays `None`, and a malformed label is REFUSED in the
    /// field's name rather than swallowed into `None`, which would route the bracket to the
    /// venue's default book.
    #[test]
    fn a_brackets_account_is_lowered_by_the_one_parser_and_a_malformed_label_is_refused() {
        use vike_model::accounts::account_keys::AccountLabel;
        let account_of = |account: Option<&str>| -> Option<AccountLabel> {
            let b = OrderBracketWire { account: account.map(str::to_string), ..spec() };
            let (Command::Order(OrderIntent::Bracket(spec)), _) = lowered(b).expect("lowers")
            else {
                panic!("must lower to OrderIntent::Bracket");
            };
            spec.account
        };
        assert_eq!(account_of(None), None);
        assert_eq!(account_of(Some("PERP")), AccountLabel::parse("PERP").ok());
        assert_eq!(account_of(Some("DEFAULT")), Some(AccountLabel::Default));
        for bad in ["", "alt", "has space", "binance#ALT"] {
            let err = lowered(OrderBracketWire { account: Some(bad.to_string()), ..spec() })
                .expect_err("a malformed account must refuse");
            assert!(err.starts_with("bracket: `account`"), "{bad:?}: names the field: {err}");
        }
    }

    /// ⚠ **The engine half judges the engine the bracket NAMES.** A `.P` bracket naming `PERP` is
    /// judged by the twin's perp block and passes; the same bracket naming the default account
    /// (or naming nothing) is judged by the SPOT block beside it and refused as the spot lane; a
    /// symbol the twin does not trade is refused naming the TWIN's symbols; and an account with no
    /// published block is refused in words naming its key. Never the other account's block.
    #[test]
    fn a_named_bracket_is_judged_by_the_engine_it_names() {
        let roster = [engine("binance", "BTCUSDT"), twin_engine()];
        let named = |account: Option<&str>| OrderBracketWire {
            account: account.map(str::to_string),
            ..spec()
        };
        assert_eq!(judged(named(Some("PERP")), &roster), None, "the twin's perp engine takes it");
        let err = judged(named(Some("DEFAULT")), &roster).expect("the spot engine refuses");
        assert!(err.contains("spot lane") && err.contains("`binance`"), "{err}");
        let err = judged(named(None), &roster).expect("naming nothing is the default engine");
        assert!(err.contains("spot lane"), "{err}");
        let other = OrderBracketWire { symbol: "ETHUSDT.P".into(), ..named(Some("PERP")) };
        let err = judged(other, &roster).expect("the twin does not trade ETHUSDT.P");
        assert!(err.contains("`binance#PERP`") && err.contains("`BTCUSDT.P`"), "{err}");
        let err = judged(named(Some("NOSUCH")), &roster).expect("no such engine");
        assert!(err.contains("`binance#NOSUCH`") && err.contains("cannot tell"), "{err}");
        let err = judged(named(Some("not a label")), &roster).expect("a bad label");
        assert!(err.starts_with("bracket: `account`"), "{err}");
    }
}

/// **The order's LIFETIME across the door.** A wire `Submit` carries `time_in_force` / `gtd_expiry`
/// into the core `OrderRequest` (absent is the default `gtc` and every frame sent before the field), and
/// the door refuses, naming the rule, a word that is not a lifetime, a lifetime that does not belong to
/// the order's kind, and a `gtd` without a future expiry: never lowered to the default, which would
/// rest an order the operator meant to expire or to keep off the taker side.
mod submit_lifetime_tests {
    use super::*;
    use vike_model::TimeInForce;
    use vike_tradehub_client::wire::OrderRequestWire;

    fn submit(order_type: &str, tif: Option<&str>, gtd_expiry: Option<i64>) -> CommandWire {
        CommandWire::Submit(OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "okx".into(),
            symbol: "BTC-USDT-SWAP".into(),
            side: 1,
            qty: 1.0,
            order_type: order_type.into(),
            price: (order_type == "limit").then_some(1.0),
            // a stop fires at its trigger, which the order-terms rule requires
            trigger_price: (order_type == "stop").then_some(1.0),
            reduce_only: false,
            account: None,
            time_in_force: tif.map(str::to_string),
            gtd_expiry,
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

    fn lowered(wire: CommandWire) -> Result<vike_model::OrderRequest, String> {
        let (cmd, _order_client_id) = lower_command(wire)?;
        let Command::Order(OrderIntent::Submit(order)) = cmd else { panic!("not a submit") };
        Ok(*order)
    }

    #[test]
    fn the_lifetime_reaches_the_core_order() {
        let o = lowered(submit("limit", Some("post-only"), None)).expect("post-only lowers");
        assert_eq!((o.time_in_force, o.gtd_expiry), (TimeInForce::PostOnly, None));
        for (word, want) in
            [("day", TimeInForce::Day), ("IOC", TimeInForce::Ioc), (" fok ", TimeInForce::Fok)]
        {
            let o = lowered(submit("limit", Some(word), None)).expect(word);
            assert_eq!(o.time_in_force, want, "{word}");
        }
        let later = vike_model::now_ms() + 3_600_000;
        let o = lowered(submit("limit", Some("gtd"), Some(later))).expect("gtd with an expiry");
        assert_eq!((o.time_in_force, o.gtd_expiry), (TimeInForce::Gtd, Some(later)));
    }

    /// Absent is the default, for every kind: the frame from before the field lowers as it always did.
    #[test]
    fn an_order_naming_no_lifetime_is_the_default() {
        for kind in ["limit", "market", "stop"] {
            let o = lowered(submit(kind, None, None)).expect(kind);
            assert_eq!((o.time_in_force, o.gtd_expiry), (TimeInForce::Gtc, None), "{kind}");
        }
        let o =
            lowered(submit("market", Some("gtc"), None)).expect("an explicit gtc is the default");
        assert_eq!(o.time_in_force, TimeInForce::Gtc);
    }

    #[test]
    fn a_lifetime_that_is_none_is_refused_in_words() {
        let err = lowered(submit("limit", Some("forever"), None)).expect_err("not a lifetime");
        assert!(err.starts_with("submit: `time_in_force`"), "{err}");
        assert!(
            err.contains("gtc, ioc, fok, gtd, day, post-only"),
            "names the accepted words: {err}"
        );
        for (kind, tif) in [("market", "post-only"), ("stop", "ioc"), ("market", "day")] {
            let err = lowered(submit(kind, Some(tif), None)).expect_err(kind);
            assert!(err.contains("belongs to a limit order"), "{kind}/{tif}: {err}");
        }
        let err = lowered(submit("limit", Some("gtd"), None)).expect_err("gtd needs an expiry");
        assert!(err.contains("needs an expiry"), "{err}");
        let err = lowered(submit("limit", Some("gtd"), Some(1_000))).expect_err("a past expiry");
        assert!(err.contains("not in the future"), "{err}");
        let err = lowered(submit("limit", Some("ioc"), Some(vike_model::now_ms() + 10_000)))
            .expect_err("only gtd takes an expiry");
        assert!(err.contains("belongs to time in force `gtd`"), "{err}");
    }
}

/// **The order's PRICES across the door** (`vike_model::order_terms_refusal`). ⚠ A trigger kind with
/// no trigger used to lower, and hyperliquid then signed a `stop_limit` as a plain resting limit: the
/// door refuses it before the `Ack`, naming the rule, as it does a stated limit at zero or below.
/// What the rule leaves alone still lowers: a market-if-touched and a limit-if-touched `take_profit`,
/// a priced `stop_limit`, a plain limit at zero (a combo net can be negative) and a market order
/// carrying a trigger (the pre-trade gate sizes one, `fixtures/r5/risk.json` case 133).
mod submit_order_terms_tests {
    use super::*;
    use vike_model::OrderType;
    use vike_tradehub_client::wire::OrderRequestWire;

    fn submit(order_type: &str, price: Option<f64>, trigger_price: Option<f64>) -> CommandWire {
        CommandWire::Submit(OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            side: 1,
            qty: 1.0,
            order_type: order_type.into(),
            price,
            trigger_price,
            reduce_only: false,
            account: None,
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

    #[test]
    fn a_trigger_kind_with_no_trigger_is_refused_before_the_ack_naming_the_rule() {
        for (kind, price) in [("stop", None), ("stop_limit", Some(99.0)), ("take_profit", None)] {
            for trigger in [None, Some(0.0), Some(-1.0)] {
                let err = lower_command(submit(kind, price, trigger)).expect_err(kind);
                assert!(err.starts_with("submit: order terms — "), "{kind} {trigger:?}: {err}");
                assert!(err.contains("needs a trigger price above zero"), "{kind}: {err}");
            }
        }
    }

    #[test]
    fn a_stated_limit_at_zero_or_below_on_a_trigger_kind_is_refused() {
        let err = lower_command(submit("stop_limit", None, Some(100.0))).expect_err("no limit");
        assert!(err.contains("a stop_limit order needs a limit price above zero"), "{err}");
        for px in [0.0, -1.0] {
            for kind in ["stop_limit", "take_profit"] {
                let err = lower_command(submit(kind, Some(px), Some(100.0))).expect_err(kind);
                assert!(err.starts_with("submit: order terms — "), "{kind} {px}: {err}");
                assert!(err.contains("limit price"), "{kind} {px}: {err}");
            }
        }
    }

    #[test]
    fn what_the_rule_leaves_alone_still_lowers_with_its_prices() {
        for (kind, price, trigger, want) in [
            ("take_profit", None, Some(105.0), OrderType::TakeProfit),
            ("take_profit", Some(104.0), Some(105.0), OrderType::TakeProfit),
            ("stop_limit", Some(99.0), Some(100.0), OrderType::StopLimit),
            ("stop", None, Some(95.0), OrderType::Stop),
            ("limit", Some(0.0), None, OrderType::Limit),
            ("market", None, Some(65.6), OrderType::Market),
        ] {
            let (cmd, _) = lower_command(submit(kind, price, trigger))
                .unwrap_or_else(|e| panic!("{kind} {price:?} {trigger:?}: {e}"));
            let Command::Order(OrderIntent::Submit(o)) = cmd else { panic!("not a submit") };
            assert_eq!(
                (&o.order_type, o.price, o.trigger_price),
                (&want, price, trigger),
                "{kind}"
            );
        }
    }
}

/// **The wave-2 order fields across the door**: a submit's display (`vike_model::display_refusal`) and release
/// (`vike_model::lifetime_refusal`), an auction lifetime, and a modify's trigger. Each refusal comes before the
/// `Ack`, naming its field; what passes reaches the core with the field it carried, never dropped.
mod wave_two_fields_tests {
    use super::*;
    use crate::server::refusal::modify_price_refusal;
    use vike_model::{
        OrderRelease, OrderReleasePrice, OrderReleasePriceSide, OrderType, TimeInForce,
    };
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire, OrderRequestWire};

    fn submit(order_type: &str, price: Option<f64>) -> OrderRequestWire {
        OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 10.0,
            order_type: order_type.into(),
            price,
            trigger_price: None,
            reduce_only: false,
            account: None,
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
        }
    }

    fn lowered_submit(req: OrderRequestWire) -> Result<vike_model::OrderRequest, String> {
        match lower_command(CommandWire::Submit(req))?.0 {
            Command::Order(OrderIntent::Submit(o)) => Ok(*o),
            other => panic!("not a submit: {other:?}"),
        }
    }

    #[test]
    fn a_display_reaches_the_core_and_a_bad_one_is_refused_naming_the_field() {
        let shown =
            OrderRequestWire { display_quantity: Some(2.0), ..submit("limit", Some(100.0)) };
        assert_eq!(
            lowered_submit(shown).expect("a limit shows a slice").display_quantity,
            Some(2.0)
        );
        // a display of 0 is a hidden order: the venue's `hidden` word judges it at the core edge
        let hidden =
            OrderRequestWire { display_quantity: Some(0.0), ..submit("limit", Some(100.0)) };
        assert_eq!(lowered_submit(hidden).expect("hidden").display_quantity, Some(0.0));
        for (kind, price, display, words) in [
            ("market", None, 2.0, "belongs to a limit order"),
            ("limit", Some(100.0), -1.0, "is not zero or more"),
            ("limit", Some(100.0), 10.0, "hides nothing"),
        ] {
            let req = OrderRequestWire { display_quantity: Some(display), ..submit(kind, price) };
            let err = lowered_submit(req).expect_err(kind);
            assert!(err.starts_with("submit: `display_quantity` — "), "{kind} {display}: {err}");
            assert!(err.contains(words), "{kind} {display}: {err}");
        }
        assert_eq!(
            lowered_submit(submit("limit", Some(100.0))).expect("none").display_quantity,
            None
        );
    }

    #[test]
    fn a_release_reaches_the_core_and_a_past_one_is_refused_naming_the_field() {
        let at = vike_model::now_ms() + 60_000;
        let release = |at| Some(OrderReleaseWire { at, price: None, symbol: None, venue: None });
        let held = OrderRequestWire { release: release(at), ..submit("market", None) };
        assert_eq!(
            lowered_submit(held).expect("a future release").release,
            Some(OrderRelease { at, price: None })
        );
        let past = OrderRequestWire { release: release(1_000), ..submit("market", None) };
        let err = lowered_submit(past).expect_err("a past release");
        assert!(err.starts_with("submit: `release` — "), "{err}");
        assert!(err.contains("not in the future"), "{err}");
        let before = OrderRequestWire {
            time_in_force: Some("gtd".into()),
            gtd_expiry: Some(at - 1_000),
            release: release(at),
            ..submit("limit", Some(100.0))
        };
        let err = lowered_submit(before).expect_err("expires before it is released");
        assert!(err.contains("would expire before it is released"), "{err}");
        // The price condition reaches the core on the ORDER's own symbol; a level at or below zero is refused.
        let priced = |price| OrderRequestWire {
            release: Some(OrderReleaseWire { at, price: Some(price), symbol: None, venue: None }),
            ..submit("market", None)
        };
        let want = OrderReleasePrice {
            symbol: "BTCUSDT".into(),
            level: 100.0,
            side: OrderReleasePriceSide::Above,
            venue: None,
        };
        assert_eq!(
            lowered_submit(priced(OrderReleasePriceWire::Above(100.0)))
                .expect("a conditional")
                .release,
            Some(OrderRelease { at, price: Some(want) })
        );
        let below =
            lowered_submit(priced(OrderReleasePriceWire::Below(90.0))).expect("a conditional");
        assert_eq!(
            below.release.and_then(|r| r.price).map(|c| c.side),
            Some(OrderReleasePriceSide::Below)
        );
        for level in [0.0, -1.0] {
            let err =
                lowered_submit(priced(OrderReleasePriceWire::Below(level))).expect_err("no level");
            assert!(err.starts_with("submit: `release` — "), "{level}: {err}");
            assert!(err.contains("is not a price above zero"), "{level}: {err}");
        }
        // A named symbol reaches the core as the condition's; a symbol with no condition, or a blank one, is
        // refused in words.
        let on = |price, symbol: &str| OrderRequestWire {
            release: Some(OrderReleaseWire { at, price, symbol: Some(symbol.into()), venue: None }),
            ..submit("market", None)
        };
        let eth =
            lowered_submit(on(Some(OrderReleasePriceWire::Above(3500.0)), "ETHUSDT")).expect("eth");
        let symbol = eth.release.and_then(|r| r.price).map(|c| c.symbol.to_string());
        assert_eq!(symbol.as_deref(), Some("ETHUSDT"));
        let err = lowered_submit(on(None, "ETHUSDT")).expect_err("a symbol with no condition");
        assert!(err.contains("belongs to a price condition"), "{err}");
        let err =
            lowered_submit(on(Some(OrderReleasePriceWire::Above(1.0)), "  ")).expect_err("blank");
        assert!(err.contains("symbol is blank"), "{err}");
    }

    /// A price condition on a symbol the order's ENGINE does not trade is refused at the door, naming what it
    /// trades (the order would be held forever); one it trades, the order's own and none pass. An engine the frame
    /// does not say the trades of refuses too.
    #[test]
    fn a_price_condition_on_a_symbol_the_engine_does_not_trade_is_refused() {
        use crate::server::refusal::release_symbol_refusal;
        let at = vike_model::now_ms() + 60_000;
        let on = |symbol: Option<&str>| {
            CommandWire::Submit(OrderRequestWire {
                release: Some(OrderReleaseWire {
                    at,
                    price: Some(OrderReleasePriceWire::Above(3500.0)),
                    symbol: symbol.map(Into::into),
                    venue: None,
                }),
                ..submit("market", None)
            })
        };
        let blocks = [vike_execution::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            symbol: "BTCUSDT".into(),
            extra_symbols: vec!["ETHUSDT".to_string()].into(),
            ..Default::default()
        }];
        for symbol in [None, Some("BTCUSDT"), Some("ETHUSDT")] {
            assert_eq!(release_symbol_refusal(&on(symbol), &blocks), None, "{symbol:?}");
        }
        let why = release_symbol_refusal(&on(Some("SOLUSDT")), &blocks).expect("not traded");
        assert!(why.contains("reads `SOLUSDT`") && why.contains("`BTCUSDT`, `ETHUSDT`"), "{why}");
        let why = release_symbol_refusal(&on(Some("SOLUSDT")), &[]).expect("no engine published");
        assert!(why.contains("publishes no engine for `binance`"), "{why}");
    }

    #[test]
    fn an_auction_lifetime_lowers_on_a_market_and_a_limit_and_is_refused_on_a_stop() {
        for (kind, price) in [("market", None), ("limit", Some(100.0))] {
            for (word, tif) in
                [("at-open", TimeInForce::AtOpen), ("at-close", TimeInForce::AtClose)]
            {
                let req =
                    OrderRequestWire { time_in_force: Some(word.into()), ..submit(kind, price) };
                assert_eq!(lowered_submit(req).expect(word).time_in_force, tif, "{kind} {word}");
            }
        }
        let stop = OrderRequestWire {
            order_type: "stop".into(),
            trigger_price: Some(90.0),
            time_in_force: Some("at-open".into()),
            ..submit("stop", None)
        };
        let err = lowered_submit(stop).expect_err("a stop has no auction");
        assert!(err.contains("belongs to a market or a limit order"), "{err}");
    }

    #[test]
    fn a_modify_trigger_reaches_the_core_and_one_at_or_below_zero_is_refused() {
        let modify = |t| CommandWire::Modify {
            order_client_id: "c-stop".into(),
            new_qty: None,
            new_price: None,
            new_trigger_price: Some(t),
        };
        let (cmd, order_client_id) = lower_command(modify(95.0)).expect("a trigger modify lowers");
        assert_eq!(order_client_id, "c-stop");
        let Command::Order(OrderIntent::Modify { new_trigger_price, .. }) = cmd else {
            panic!("not a modify: {cmd:?}")
        };
        assert_eq!(new_trigger_price, Some(95.0));
        for t in [0.0, -1.0] {
            let err = lower_command(modify(t)).expect_err("no trigger to fire at");
            assert!(err.contains("`new_trigger_price` must be a price above zero"), "{t}: {err}");
        }
    }

    /// A modify's trigger on a published order with NO trigger (a limit, a market) is refused at the door, where it
    /// would move nothing behind an `Ack`; on a trigger kind, on an unknown order client id and with no trigger it passes.
    #[test]
    fn a_trigger_modify_of_an_order_without_a_trigger_is_refused_in_words() {
        let view = |order_client_id: &str, order_type| vike_execution::OrderView {
            data_delay: None,
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type,
            price: Some(100.0),
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
        let book = [
            view("lim", OrderType::Limit),
            view("mkt", OrderType::Market),
            view("stp", OrderType::Stop),
            view("stl", OrderType::StopLimit),
        ];
        let modify = |order_client_id: &str, trigger| CommandWire::Modify {
            order_client_id: order_client_id.into(),
            new_qty: None,
            new_price: None,
            new_trigger_price: trigger,
        };
        for order_client_id in ["lim", "mkt"] {
            let why = modify_price_refusal(&modify(order_client_id, Some(95.0)), &book)
                .expect(order_client_id);
            assert!(why.contains("has no trigger price to move"), "{order_client_id}: {why}");
        }
        for order_client_id in ["stp", "stl", "unknown"] {
            assert_eq!(
                modify_price_refusal(&modify(order_client_id, Some(95.0)), &book),
                None,
                "{order_client_id}"
            );
        }
        assert_eq!(modify_price_refusal(&modify("lim", None), &book), None, "no trigger named");
        // A trailing order is modified in its size only: a new price or a new activation is refused in words,
        // and a size change passes.
        let trailing = [view("trl", OrderType::Trailing), view("tlm", OrderType::TrailingLimit)];
        let change =
            |order_client_id: &str, new_qty, new_price, new_trigger_price| CommandWire::Modify {
                order_client_id: order_client_id.into(),
                new_qty,
                new_price,
                new_trigger_price,
            };
        for order_client_id in ["trl", "tlm"] {
            for (price, trigger, named) in [
                (Some(99.0), None, "`new_price`"),
                (None, Some(95.0), "`new_trigger_price`"),
                (Some(99.0), Some(95.0), "`new_price`"),
            ] {
                let why =
                    modify_price_refusal(&change(order_client_id, None, price, trigger), &trailing)
                        .expect(order_client_id);
                assert!(
                    why.starts_with(&format!("modify: {named} — order {order_client_id}")),
                    "{why}"
                );
                assert!(why.contains("in its size only") && why.contains("cancel it and place"));
            }
            let size = change(order_client_id, Some(2.0), None, None);
            assert_eq!(
                modify_price_refusal(&size, &trailing),
                None,
                "{order_client_id}: a size change"
            );
        }
    }
}

/// **The wave-3 order fields across the door**: a submit's trail and limit offset reach the core on the two
/// trailing kinds, the activation on its trigger price; the order-terms rule refuses a trailing order's bad terms
/// before the `Ack`, and a trail on any other kind is refused there too (the paper book would lower it as a
/// trailing stop, a kind nobody asked for).
mod wave_three_fields_tests {
    use super::*;
    use vike_model::OrderType;
    use vike_tradehub_client::wire::OrderRequestWire;

    fn trailing(kind: &str, trail: Option<f64>, offset: Option<f64>) -> OrderRequestWire {
        OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "okx".into(),
            symbol: "BTC-USDT-SWAP".into(),
            side: -1,
            qty: 1.0,
            order_type: kind.into(),
            price: None,
            trigger_price: None,
            reduce_only: true,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
            display_quantity: None,
            release: None,
            trail,
            limit_offset: offset,
            peg: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
        }
    }

    fn lowered(req: OrderRequestWire) -> Result<vike_model::OrderRequest, String> {
        match lower_command(CommandWire::Submit(req))?.0 {
            Command::Order(OrderIntent::Submit(o)) => Ok(*o),
            other => panic!("not a submit: {other:?}"),
        }
    }

    #[test]
    fn a_trail_and_an_offset_reach_the_core_on_the_trailing_kinds() {
        let stop = lowered(OrderRequestWire {
            trigger_price: Some(110.0),
            ..trailing("trailing", Some(5.0), None)
        })
        .expect("a trailing stop with an activation");
        assert_eq!(
            (stop.order_type, stop.trail, stop.limit_offset, stop.trigger_price),
            (OrderType::Trailing, Some(5.0), None, Some(110.0))
        );
        let limit =
            lowered(trailing("TRAILING_LIMIT", Some(5.0), Some(0.5))).expect("a trailing limit");
        assert_eq!(
            (limit.order_type, limit.trail, limit.limit_offset, limit.trigger_price),
            (OrderType::TrailingLimit, Some(5.0), Some(0.5), None)
        );
    }

    #[test]
    fn a_trailing_orders_bad_terms_are_refused_in_the_rules_words() {
        for (req, words) in [
            (trailing("trailing", None, None), "needs a trail distance above zero"),
            (trailing("trailing", Some(0.0), None), "needs a trail distance above zero"),
            (trailing("trailing_limit", Some(5.0), None), "needs a limit offset of zero or more"),
            (trailing("trailing", Some(5.0), Some(0.5)), "belongs to a trailing_limit order"),
            (
                OrderRequestWire { price: Some(100.0), ..trailing("trailing", Some(5.0), None) },
                "takes no price",
            ),
        ] {
            let err = lowered(req.clone()).expect_err(words);
            assert!(err.starts_with("submit: order terms — "), "{req:?}: {err}");
            assert!(err.contains(words), "{req:?}: {err}");
        }
    }

    #[test]
    fn a_trail_on_any_other_kind_is_refused_naming_the_field() {
        for (kind, price) in [("market", None), ("limit", Some(100.0))] {
            let req = OrderRequestWire { price, ..trailing(kind, Some(5.0), None) };
            let err = lowered(req).expect_err(kind);
            assert!(err.starts_with("submit: order terms — "), "{kind}: {err}");
            assert!(
                err.contains(
                    "a trail belongs to a trailing, trailing_limit or trailing_take_profit order"
                ),
                "{err}"
            );
        }
    }

    /// **The wave-5 peg across the door**: a pegged submit reaches the core with its peg; an anchor that is no
    /// word of ours is refused naming the four (never read as another), and the terms rule refuses a pegged
    /// order with no peg or a price, and a peg on any other kind, before the `Ack`.
    #[test]
    fn a_peg_reaches_the_core_on_a_pegged_order_and_is_refused_elsewhere() {
        use vike_model::{OrderPeg, OrderPegAnchor};
        use vike_tradehub_client::wire::OrderPegWire;
        let peg = |anchor: &str| Some(OrderPegWire { anchor: anchor.into(), offset: -0.05 });
        let pegged =
            |anchor| OrderRequestWire { peg: peg(anchor), ..trailing("pegged", None, None) };
        let o = lowered(pegged("primary")).expect("a pegged order");
        assert_eq!(o.order_type, OrderType::Pegged);
        assert_eq!(o.peg, Some(OrderPeg { anchor: OrderPegAnchor::Primary, offset: -0.05 }));
        let err = lowered(pegged("Mid")).expect_err("an anchor that is no word of ours");
        assert!(err.starts_with("submit: `peg` — the anchor \"Mid\" is not one of"), "{err}");
        for (req, words) in [
            (trailing("pegged", None, None), "a pegged order needs its peg"),
            (OrderRequestWire { price: Some(100.0), ..pegged("mid") }, "takes no price"),
            (
                OrderRequestWire {
                    price: Some(100.0),
                    peg: peg("mid"),
                    ..trailing("limit", None, None)
                },
                "a peg belongs to a pegged order",
            ),
        ] {
            let err = lowered(req.clone()).expect_err(words);
            assert!(err.starts_with("submit: order terms — "), "{req:?}: {err}");
            assert!(err.contains(words), "{req:?}: {err}");
        }
    }
}

/// **The attributes wave across the door** (decision 0267): a limit's attributes, a percent trail, a stop's adjustment
/// and the opening auction's market-to-limit reach the core with what they carried, and each rule refuses in its
/// words before the `Ack`. A release read on ANOTHER venue (2.13) lowers with that venue, the order's own to `None`;
/// the door reads it off an engine of that venue that trades the symbol and asks that engine for the price once.
mod attributes_wave_tests {
    use super::*;
    use std::sync::Mutex;

    use crate::server::control::{ControlAcceptReply, accept_command};
    use crate::server::refusal::{PriceVenueEngine, release_symbol_refusal, release_venue_refusal};
    use crate::server::symbol_admission::{AccountReadiness, SymbolAdmitter};
    use crate::server::tests::admits_any_symbol::Node;
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo, OrderType, TimeInForce};
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire, OrderRequestWire};

    fn submit(order_type: &str, price: Option<f64>) -> OrderRequestWire {
        OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: order_type.into(),
            price,
            trigger_price: None,
            reduce_only: false,
            account: None,
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
        }
    }

    fn lowered(req: OrderRequestWire) -> Result<vike_model::OrderRequest, String> {
        match lower_command(CommandWire::Submit(req))?.0 {
            Command::Order(OrderIntent::Submit(o)) => Ok(*o),
            other => panic!("not a submit: {other:?}"),
        }
    }

    /// `req` is refused before the `Ack`, naming its field (`prefix`), in words that carry `words`.
    fn refused_with(req: OrderRequestWire, prefix: &str, words: &str) {
        let err = lowered(req.clone()).expect_err(words);
        assert!(err.starts_with(prefix), "{req:?}: {err}");
        assert!(err.contains(words), "{req:?}: {err}");
    }

    /// A submit on bybit whose release reads `symbol` on `venue`, above 100000, a minute from now.
    fn read_on(
        venue: &str,
        symbol: Option<&str>,
        price: Option<OrderReleasePriceWire>,
    ) -> OrderRequestWire {
        OrderRequestWire {
            release: Some(OrderReleaseWire {
                at: vike_model::now_ms() + 60_000,
                price,
                symbol: symbol.map(Into::into),
                venue: Some(venue.into()),
            }),
            ..submit("limit", Some(100.0))
        }
    }

    const ABOVE: Option<OrderReleasePriceWire> = Some(OrderReleasePriceWire::Above(100_000.0));

    #[test]
    fn an_attributed_limit_reaches_the_core_and_its_rule_refuses_in_its_words() {
        let attributes = |all_or_none, sweep_to_fill, discretionary_amount| {
            Some(OrderAttributes { all_or_none, sweep_to_fill, discretionary_amount })
        };
        let limit = |attributes, tif: Option<&str>| OrderRequestWire {
            attributes,
            time_in_force: tif.map(Into::into),
            ..submit("limit", Some(100.0))
        };
        assert_eq!(
            lowered(limit(attributes(true, true, Some(0.5)), None)).expect("attributed").attributes,
            Some(OrderAttributes {
                all_or_none: true,
                sweep_to_fill: true,
                discretionary_amount: Some(0.5)
            })
        );
        assert_eq!(lowered(limit(None, None)).expect("a plain limit").attributes, None);
        let prefix = "submit: `attributes` — ";
        let market = OrderRequestWire {
            attributes: attributes(true, false, None),
            ..submit("market", None)
        };
        refused_with(market, prefix, "belongs to a limit order");
        refused_with(
            limit(attributes(true, false, None), Some("ioc")),
            prefix,
            "all-or-none at once is `fok`",
        );
        refused_with(
            limit(attributes(false, true, None), Some("post-only")),
            prefix,
            "takes liquidity at once",
        );
        refused_with(
            limit(attributes(false, false, Some(-1.0)), None),
            prefix,
            "not a finite number above zero",
        );
        refused_with(
            limit(attributes(true, false, None), Some("at-open")),
            prefix,
            "takes no auction lifetime",
        );
    }

    /// The terms rule is asked with the REAL percent: one distance, a percent above 0 and below 100, on a trailing kind
    /// only; and a price trail on a kind that does not trail names every kind that does.
    #[test]
    fn a_percent_trail_reaches_the_core_and_the_terms_rule_judges_it() {
        let trailing = |kind: &str, trail, trail_percent| OrderRequestWire {
            side: -1,
            trail,
            trail_percent,
            ..submit(kind, None)
        };
        let stop = lowered(trailing("trailing", None, Some(1.5))).expect("a percent trailing stop");
        assert_eq!(
            (stop.order_type, stop.trail, stop.trail_percent),
            (OrderType::Trailing, None, Some(1.5))
        );
        let take =
            lowered(trailing("trailing_take_profit", None, Some(2.0))).expect("a take-profit");
        assert_eq!(
            (take.order_type, take.trail_percent),
            (OrderType::TrailingTakeProfit, Some(2.0))
        );
        let prefix = "submit: order terms — ";
        refused_with(
            trailing("trailing", Some(5.0), Some(1.5)),
            prefix,
            "by a distance or by a percent, not both",
        );
        refused_with(
            trailing("trailing", None, Some(100.0)),
            prefix,
            "is not above 0 and below 100",
        );
        let limit = OrderRequestWire { trail_percent: Some(1.5), ..submit("limit", Some(100.0)) };
        refused_with(limit, prefix, "a trail percent belongs to a trailing order");
        let limit = OrderRequestWire { trail: Some(5.0), ..submit("limit", Some(100.0)) };
        refused_with(
            limit,
            prefix,
            "a trail belongs to a trailing, trailing_limit or trailing_take_profit order",
        );
    }

    #[test]
    fn an_adjustment_reaches_the_core_and_its_rule_refuses_in_its_words() {
        // A sell stop at 90 that becomes its target once the price reaches `at`.
        let stop = |at, to| OrderRequestWire {
            side: -1,
            trigger_price: Some(90.0),
            adjust: Some(OrderStopAdjust { at, to }),
            ..submit("stop", None)
        };
        let trailing =
            OrderStopAdjustTo::Trailing { stop: 105.0, trail: None, trail_percent: Some(2.0) };
        assert_eq!(
            lowered(stop(110.0, trailing)).expect("an auto trailing stop").adjust,
            Some(OrderStopAdjust {
                at: 110.0,
                to: OrderStopAdjustTo::Trailing {
                    stop: 105.0,
                    trail: None,
                    trail_percent: Some(2.0)
                }
            })
        );
        let stop_limit = OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 };
        assert_eq!(
            lowered(stop(110.0, stop_limit)).expect("to a stop-limit").adjust.map(|a| a.to),
            Some(OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 })
        );
        let prefix = "submit: `adjust` — ";
        let limit = OrderRequestWire {
            adjust: Some(OrderStopAdjust { at: 110.0, to: stop_limit }),
            ..submit("limit", Some(100.0))
        };
        refused_with(limit, prefix, "belongs to a stop or a stop_limit order");
        refused_with(
            stop(80.0, OrderStopAdjustTo::Stop { stop: 75.0 }),
            prefix,
            "not on the protected side",
        );
        refused_with(
            stop(110.0, OrderStopAdjustTo::Stop { stop: 115.0 }),
            prefix,
            "not on the near side",
        );
        let no_distance =
            OrderStopAdjustTo::Trailing { stop: 105.0, trail: None, trail_percent: None };
        refused_with(
            stop(110.0, no_distance),
            prefix,
            "the adjustment's target: a trailing order needs a trail distance above zero",
        );
    }

    #[test]
    fn a_market_to_limit_at_the_open_lowers_as_the_auction_and_at_the_close_is_refused() {
        let auction = |tif: &str| OrderRequestWire {
            time_in_force: Some(tif.into()),
            ..submit("market_to_limit", None)
        };
        let o = lowered(auction("at-open")).expect("the opening auction's market-to-limit");
        assert_eq!(
            (o.order_type, o.time_in_force),
            (OrderType::MarketToLimit, TimeInForce::AtOpen)
        );
        let err = lowered(auction("at-close")).expect_err("nothing of the session is left");
        assert!(err.starts_with("submit: `time_in_force` — "), "{err}");
        assert!(err.contains("belongs to a market or a limit order"), "{err}");
    }

    /// Another venue lowers with it; the order's own lowers to `None` (today's stored form); an unknown venue, a venue
    /// with no price condition and another venue with no symbol are refused in words.
    #[test]
    fn a_release_read_on_another_venue_lowers_with_it_and_the_orders_own_to_none() {
        let venue_of = |req| {
            let o = lowered(req).expect("lowers");
            o.release.and_then(|r| r.price).and_then(|c| c.venue).map(|v| v.to_string())
        };
        assert_eq!(
            venue_of(read_on("binance", Some("BTCUSDT"), ABOVE)).as_deref(),
            Some("binance")
        );
        assert_eq!(
            venue_of(read_on("bybit", None, ABOVE)),
            None,
            "the order's own, its own symbol"
        );
        assert_eq!(
            venue_of(read_on("bybit", Some("ETHUSDT"), ABOVE)),
            None,
            "the order's own venue"
        );
        let prefix = "submit: `release` — ";
        let unknown = vike_model::price_source_refusal("Binance").expect("no roster id");
        refused_with(read_on("Binance", Some("BTCUSDT"), ABOVE), prefix, &unknown);
        refused_with(
            read_on("binance", None, None),
            prefix,
            "the venue `binance` belongs to a price condition, and this release states none",
        );
        refused_with(
            read_on("binance", None, ABOVE),
            prefix,
            "the price condition read on `binance` names no symbol",
        );
    }

    /// The price venue's engines are the newest published block of each of its route keys, the default account first;
    /// the first that trades the symbol (its own or an admitted one) is the one the door asks. None does: REFUSED, in
    /// the plan's words. A condition on the order's own venue is `release_symbol_refusal`'s and not this rule's, one on
    /// another venue the other way round, and an unknown venue is `lower_command`'s.
    #[test]
    fn a_price_venue_with_no_engine_that_trades_the_symbol_is_refused() {
        let on =
            |venue: &str, symbol: &str| CommandWire::Submit(read_on(venue, Some(symbol), ABOVE));
        let block = |route_key: &str, symbol: &str| vike_execution::VenueBlock {
            venue: route_key.split('#').next().unwrap().into(),
            route_key: route_key.into(),
            symbol: symbol.into(),
            tier: Some(vike_model::AccountTier::Demo),
            ..Default::default()
        };
        let engine = |route_key: &str| PriceVenueEngine {
            route_key: route_key.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
        };
        let bybit = block("bybit", "BTCUSDT");
        let refused = "submit: `release` — the price condition reads BTCUSDT on binance, and no binance account on \
                       this node trades it, so its price is never read here: REFUSED rather than held on a price \
                       nobody reads";
        for blocks in [
            vec![],
            vec![bybit.clone()],
            vec![bybit.clone(), block("binance", "ETHUSDT")],
            // a restarted account's older block is history
            vec![block("binance", "BTCUSDT"), block("binance", "ETHUSDT")],
        ] {
            let why = release_venue_refusal(&on("binance", "BTCUSDT"), &blocks);
            assert_eq!(why, Err(refused.to_string()), "{blocks:?}");
        }
        let labelled =
            vec![bybit.clone(), block("binance", "ETHUSDT"), block("binance#ALT", "BTCUSDT")];
        let read = release_venue_refusal(&on("binance", "BTCUSDT"), &labelled);
        assert_eq!(read, Ok(Some(engine("binance#ALT"))));
        let both = vec![block("binance#ALT", "BTCUSDT"), block("binance", "BTCUSDT")];
        let read = release_venue_refusal(&on("binance", "BTCUSDT"), &both);
        assert_eq!(read, Ok(Some(engine("binance"))), "the default account first");
        let mut admitted = block("binance", "ETHUSDT");
        admitted.extra_symbols = vec!["BTCUSDT".to_string()].into();
        let read = release_venue_refusal(&on("binance", "BTCUSDT"), &[admitted]);
        assert_eq!(read, Ok(Some(engine("binance"))), "an admitted symbol is traded");
        // A paper account feeds no price lane: alone it is refused in its own words, beside a demo one it is passed by.
        let paper = vike_execution::VenueBlock {
            tier: Some(vike_model::AccountTier::Paper),
            ..block("binance", "BTCUSDT")
        };
        let why = release_venue_refusal(&on("binance", "BTCUSDT"), std::slice::from_ref(&paper));
        assert_eq!(
            why,
            Err("submit: `release` — the price condition reads BTCUSDT on binance, and no demo or live \
                 account of binance feeds a price here (only a paper one trades it), so its price is never \
                 read here: REFUSED rather than held on a price nobody reads"
                .to_string())
        );
        let read = release_venue_refusal(
            &on("binance", "BTCUSDT"),
            &[paper, block("binance#ALT", "BTCUSDT")],
        );
        assert_eq!(
            read,
            Ok(Some(engine("binance#ALT"))),
            "the demo account, past the paper default"
        );
        let own = std::slice::from_ref(&bybit);
        assert_eq!(release_venue_refusal(&on("bybit", "SOLUSDT"), own), Ok(None));
        assert!(release_symbol_refusal(&on("bybit", "SOLUSDT"), own).is_some());
        assert_eq!(release_symbol_refusal(&on("binance", "SOLUSDT"), &[bybit]), None);
        assert_eq!(release_venue_refusal(&on("Binance", "BTCUSDT"), &[]), Ok(None));
    }

    /// A facts source that answers every price ask with `answer`, and records the asks and the give-backs.
    struct PriceVenue {
        answer: Result<Option<f64>, String>,
        asked: Mutex<Vec<(String, String, String, bool)>>,
        given_back: Mutex<Vec<(String, String)>>,
    }

    impl PriceVenue {
        fn answering(answer: Result<Option<f64>, String>) -> Self {
            Self { answer, asked: Mutex::default(), given_back: Mutex::default() }
        }

        /// The asks on binance, the price venue.
        fn binance_asks(&self) -> Vec<(String, String, String, bool)> {
            self.asked.lock().unwrap().iter().filter(|a| a.1 == "binance").cloned().collect()
        }
    }

    impl SymbolAdmitter for PriceVenue {
        fn admission(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<vike_execution::SymbolAdmission, SymbolPropertiesRequestError> {
            Err(SymbolPropertiesRequestError::Refused("this source admits nothing".into()))
        }
        fn price(
            &self,
            route_key: &str,
            venue: &str,
            symbol: &str,
            subscribe: bool,
        ) -> Result<Option<f64>, String> {
            let ask = (route_key.into(), venue.into(), symbol.into(), subscribe);
            self.asked.lock().unwrap().push(ask);
            self.answer.clone()
        }
        fn release(&self, route_key: &str, _: &str, symbol: &str) {
            self.given_back.lock().unwrap().push((route_key.into(), symbol.into()));
        }
    }

    impl AccountReadiness for PriceVenue {
        fn generation(&self, _: &str) -> u64 {
            0
        }
    }

    fn accept(
        node: &mut Node,
        blocks: &[vike_execution::VenueBlock],
        cmd: CommandWire,
        facts: Option<&dyn SymbolAdmitter>,
    ) -> Result<ControlAcceptReply, String> {
        let sink = node.core.command_sink();
        accept_command(
            cmd,
            None,
            &mut node.limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            blocks,
            &[],
            &[],
            &[],
            facts,
            None,
            None,
        )
        .map_err(|e| e.message())
    }

    /// **The accept asks the price venue's engine ONCE, committing** (it opens the lane, as an admission's first
    /// committing ask does) and gives the pump back to the idle reaper as it ends (the lane keeper holds it from the
    /// published HELD list on); the facts' `Err` refuses the submit in its own words before the `Ack`, and so does a
    /// surface with no facts source (no price can come).
    #[test]
    fn the_accept_asks_the_price_venues_engine_once_and_refuses_on_its_err() {
        let mut node = Node::start("bybit", "BTCUSDT", vike_model::AccountTier::Demo);
        let mut blocks = node.blocks();
        blocks.push(vike_execution::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            symbol: "BTCUSDT".into(),
            tier: Some(vike_model::AccountTier::Demo),
            ..Default::default()
        });
        let cmd = |order_client_id: &str| {
            CommandWire::Submit(OrderRequestWire {
                order_client_id: order_client_id.into(),
                ..read_on("binance", Some("BTCUSDT"), ABOVE)
            })
        };
        let ask = ("binance".to_string(), "binance".to_string(), "BTCUSDT".to_string(), true);

        let none_yet = PriceVenue::answering(Ok(None));
        accept(&mut node, &blocks, cmd("rv-1"), Some(&none_yet)).expect("held for its price");
        assert_eq!(none_yet.binance_asks(), vec![ask.clone()], "asked once, committing");
        assert_eq!(
            *none_yet.given_back.lock().unwrap(),
            vec![("binance".to_string(), "BTCUSDT".to_string())],
            "the pump goes back to the idle reaper; the keeper holds it"
        );

        let no_feed = PriceVenue::answering(Err("this node runs no price feed for binance".into()));
        assert_eq!(
            accept(&mut node, &blocks, cmd("rv-2"), Some(&no_feed)),
            Err("submit: `release` — this node runs no price feed for binance".to_string())
        );
        assert_eq!(no_feed.binance_asks(), vec![ask]);

        let without = accept(&mut node, &blocks, cmd("rv-3"), None).expect_err("no facts source");
        assert!(
            without.starts_with("submit: `release` — this node has no price source")
                && without.contains("BTCUSDT on binance"),
            "{without}"
        );
        node.core.shutdown_and_join();
    }
}
