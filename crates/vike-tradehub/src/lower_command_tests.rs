//! The B4 lowering edge in isolation: the wire `UpdateParams` payload IS the core's own
//! `StrategyParams` serde schema, and an undecodable payload is a REFUSAL, never a silent
//! drop. (The end-to-end proof — the lowered command reaching a mounted strategy through the
//! real server — is `tests/control_roundtrip.rs`'s
//! `update_params_over_the_wire_retunes_the_mounted_strategy`.)

use super::*;

/// A fully-populated `SpreadMakerParams` payload built from the REAL core type — what proves
/// "the wire schema IS the core schema" rather than a lookalike.
fn spread_maker_params(qty: f64, half_spread: f64) -> StrategyParams {
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
    let wire = WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        // The core's own externally-tagged form — proven against the REAL type below.
        params: serde_json::to_value(spread_maker_params(2.0, 1.0)).unwrap(),
    };
    let (cmd, coid) = lower_command(wire).expect("a decodable payload lowers");
    assert_eq!(coid, "", "account-wide-verb convention: nothing order-scoped to echo");
    let Command::UpdateParams(u) = cmd else {
        panic!("must lower to Command::UpdateParams, got a different command");
    };
    assert_eq!(
        (u.venue.as_str(), u.symbol.as_str(), u.interval.as_str()),
        ("binance", "BTCUSDT", "1m")
    );
    let StrategyParams::SpreadMaker(p) = u.params else {
        panic!("the typed variant survives the wire round-trip");
    };
    assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "the payload's knobs land verbatim");
    assert_eq!(p.half_spread.to_bits(), 1.0_f64.to_bits());
}

#[test]
fn an_undecodable_params_payload_is_refused_not_dropped() {
    let wire = WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params: serde_json::json!({"NoSuchStrategyParams": {"qty": 1.0}}),
    };
    let err = lower_command(wire).expect_err("an unknown variant must refuse");
    assert!(err.contains("update_params"), "names the verb: {err}");
}

#[test]
fn update_params_has_its_own_audit_kind() {
    let wire = WireCommand::UpdateParams {
        venue: "b".into(),
        symbol: "s".into(),
        interval: "1m".into(),
        params: serde_json::json!({}),
    };
    assert_eq!(command_kind(&wire), "update_params");
}

/// **THE WIRE-TO-CORE HOP (this task).** A wire `Submit` naming an account must reach the
/// core `OrderRequest` carrying it. Before this arm read the field, the `OrderRequest`
/// literal's `..Default::default()` silently dropped it and every order routed to whichever
/// engine happened to be first, regardless of what the client asked for — routing is still
/// blind to it after this task (Task 4's job), but the value now survives the hop for that
/// task to consume.
mod submit_account_tests {
    use super::*;
    use vike_tradehub_client::wire::WireOrderRequest;

    /// Every field `client_order_id`'s non-empty gate and `OrderRequest`'s construction need,
    /// with `account` the one axis under test.
    fn submit(account: Option<&str>) -> WireCommand {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: account.map(str::to_string),
        })
    }

    fn submitted_order(wire: WireCommand) -> Result<vike_model::OrderRequest, String> {
        let (cmd, _coid) = lower_command(wire)?;
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
            Some(vike_model::account_keys::AccountLabel::Named("ALT".to_string())),
            "the account named on the wire must reach the core order"
        );
    }

    /// The wire's reserved spelling for "the unlabelled account, deliberately" —
    /// `parse_wire_account`'s own contract, admitted here exactly as `MountStrategy`'s arm
    /// admits it, where `policy.toml` refuses it.
    #[test]
    fn a_submit_naming_default_on_the_wire_carries_the_default_account() {
        let order = submitted_order(submit(Some("DEFAULT"))).expect("DEFAULT lowers");
        assert_eq!(order.account, Some(vike_model::account_keys::AccountLabel::Default));
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
}

/// **THE WIRE-TO-CORE HOP for the three risk-REDUCING verbs** — the arm that used to
/// destructure them as `{ venue, symbol, .. }` and drop the account on the floor, so a
/// `market-exit binance ALT` reached the core as `market-exit binance` and fanned over every
/// account of the exchange. Same parser as the submit arm above (`parse_wire_account`, the ONE
/// reading of `DEFAULT`), same refusal of a malformed label rather than a silent `None`.
mod reduce_account_tests {
    use super::*;
    use vike_model::account_keys::AccountLabel;

    fn lowered_account(wire: WireCommand) -> Result<Option<AccountLabel>, String> {
        let (cmd, coid) = lower_command(wire)?;
        assert!(coid.is_empty(), "an account-wide verb echoes no coid");
        match cmd {
            Command::Order(OrderIntent::MassCancel { account, .. })
            | Command::Order(OrderIntent::Flatten { account, .. })
            | Command::Order(OrderIntent::MarketExit { account, .. }) => Ok(account),
            _ => panic!("a reducing verb must lower to its own reducing intent"),
        }
    }

    fn reducers(account: Option<&str>) -> Vec<WireCommand> {
        let account = account.map(str::to_string);
        vec![
            WireCommand::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: account.clone(),
            },
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: account.clone(),
            },
            WireCommand::MarketExit { venue: Some("binance".into()), account },
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
        wires.push(WireCommand::MarketExit { venue: None, account: None });
        wires.push(WireCommand::MassCancel { venue: None, symbol: None, account: None });
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
