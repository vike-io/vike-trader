use super::*;

/// **The finding `docs/decisions/0064` recorded, as a test.** The arm is LABELLED native; the
/// registry's `"rhai"` arm COMPILES. Before this refusal the two met and the label lost.
#[test]
fn a_native_spec_may_not_name_the_compiling_arm() {
    let err = to_strategy_spec(&WireSpec::Native {
        name: "rhai".to_string(),
        params_toml: "src = \"fn on_bar() {}\"".to_string(),
    })
    .expect_err("`native` naming the compiling arm is a source door");
    let msg = format!("{err:?}");
    assert!(msg.contains("may not name"), "{msg}");
    assert!(msg.contains("WireSpec::Rhai"), "…and names the arm that says what it is: {msg}");
}

/// Whitespace is not a bypass: the registry lookup trims nothing, but a caller might.
#[test]
fn padding_the_compiling_name_does_not_get_past_it() {
    assert!(
        to_strategy_spec(&WireSpec::Native {
            name: "  rhai  ".to_string(),
            params_toml: String::new(),
        })
        .is_err(),
        "a padded name must be refused too"
    );
}

/// The SECOND half, and it is not redundant: a future registry arm that reads `src` would be
/// reachable under a name this refusal does not know.
#[test]
fn a_native_spec_may_not_carry_a_source_param_under_any_name() {
    let err = to_strategy_spec(&WireSpec::Native {
        name: "buy_hold".to_string(),
        params_toml: "size = 1.0\nsrc = \"fn on_bar() {}\"".to_string(),
    })
    .expect_err("`src` is the key the compiling arm reads");
    assert!(format!("{err:?}").contains("may not carry"), "{err:?}");
}

/// ⚠ The complement, and the half that stops this refusal from being a capability regression:
/// an ORDINARY native spec still resolves, params and all.
#[test]
fn an_ordinary_native_spec_still_resolves() {
    let spec = to_strategy_spec(&WireSpec::Native {
        name: "buy_hold".to_string(),
        params_toml: "size = 1.0\nsymbol = \"BTCUSDT\"".to_string(),
    })
    .expect("a real native strategy must still run");
    assert!(format!("{spec:?}").contains("buy_hold"), "{spec:?}");
}

/// ...including with no params at all, which every registry `from_params` tolerates.
#[test]
fn an_empty_params_table_still_resolves() {
    assert!(
        to_strategy_spec(&WireSpec::Native {
            name: "buy_hold".to_string(),
            params_toml: "   ".to_string(),
        })
        .is_ok(),
        "empty/whitespace params are the documented empty table"
    );
}
