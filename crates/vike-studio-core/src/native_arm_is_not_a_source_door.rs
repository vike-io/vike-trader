use super::*;

/// **The finding `docs/decisions/0064` recorded, closed for good.** The arm LABELLED native once
/// reached the compiler (`name = "rhai"` with a `src` param). A registry name resolves nothing now,
/// so every `Native` frame is refused, whatever it names or carries, with the fix.
#[test]
fn a_retired_native_spec_is_refused_for_every_name_with_the_fix() {
    for (name, params_toml) in [
        ("rhai", "src = \"fn on_bar() {}\""),
        ("  rhai  ", ""),
        ("buy_hold", "size = 1.0\nsrc = \"fn on_bar() {}\""),
        ("buy_hold", "size = 1.0\nsymbol = \"BTCUSDT\""),
        ("buy_hold", "   "),
    ] {
        let err = to_strategy_spec(&StrategySpecWire::Native {
            name: name.to_string(),
            params_toml: params_toml.to_string(),
        })
        .expect_err("a registry name resolves nothing");
        let msg = format!("{err:?}");
        assert!(msg.contains("StrategySpecWire::File"), "{name}: names the variant to send: {msg}");
    }
}

/// ⚠ The complement: a FILE spec resolves, params and all, and an empty params text is the empty
/// table.
#[test]
fn a_file_spec_resolves_with_its_params() {
    for params_toml in ["size = 1.0\nsymbol = \"BTCUSDT\"", "   "] {
        let spec = to_strategy_spec(&StrategySpecWire::File {
            file: "trend/buy_hold.rs".to_string(),
            params_toml: params_toml.to_string(),
            sdk: None,
        })
        .expect("a file spec resolves");
        assert!(format!("{spec:?}").contains("trend/buy_hold.rs"), "{spec:?}");
    }
}
