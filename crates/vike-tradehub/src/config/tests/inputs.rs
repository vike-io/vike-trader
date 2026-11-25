//! The daemon profile's `[inputs]` remap (decision 0271): the backtest profile's table, in its shape and
//! through its check (`vike_data::store::series::unparsed_input_rows`, in its words), daemon-wide across
//! `[[mounts]]`.

use super::*;

/// **A row in the input grammar loads; a one-part name is the mount's own symbol**, and the table is the
/// default empty map when absent.
#[test]
fn an_inputs_row_in_the_grammar_loads() {
    let p = ProfileDaemon::from_toml_str(&format!(
        r#"
venue = "binance"
symbol = "BTCUSDT"

[inputs]
"binance:BTCUSDT:4h" = "user-mine:binance:BTCUSDT:4h"
"1d" = "binance:ETHUSDT:1d"
{NOTHING_TABLE}"#,
    ))
    .expect("both rows parse");
    assert_eq!(p.inputs.len(), 2);
    assert_eq!(p.inputs["1d"], "binance:ETHUSDT:1d");
    let bare = ProfileDaemon::from_toml_str(&format!(
        "venue = \"binance\"\nsymbol = \"BTCUSDT\"{NOTHING_TABLE}"
    ))
    .unwrap();
    assert!(bare.inputs.is_empty(), "absent is no remap");
}

/// **A row whose name or series does not parse is refused at load, in the backtest's words.**
#[test]
fn an_inputs_row_outside_the_grammar_is_refused_at_load() {
    let e = ProfileDaemon::from_toml_str(
        r#"
venue = "binance"
symbol = "BTCUSDT"

[inputs]
"binance:BTCUSDT:4h" = "binance:BTCUSDT"
"#,
    )
    .expect_err("the series has two parts");
    assert!(e.starts_with("[inputs] \"binance:BTCUSDT:4h\" = \"binance:BTCUSDT\": "), "{e}");
    assert!(e.contains("is not VENUE:SYMBOL:INTERVAL"), "{e}");
    let typo = ProfileDaemon::from_toml_str(
        "venue = \"binance\"\nsymbol = \"BTCUSDT\"\n[inputs]\n\"fortnight\" = \"binance:BTCUSDT:4h\"\n",
    )
    .expect_err("a one-part name must be a bar length");
    assert!(typo.contains("is not one"), "{typo}");
}

/// **`[inputs]` is daemon-wide, beside `[[mounts]]` as `[daemon]` is**: every lowered row carries it, and
/// each row's one-part name is checked against that row's own symbol.
#[test]
fn inputs_beside_mounts_reach_every_row() {
    let p = ProfileDaemon::from_toml_str(
        r#"
[inputs]
"4h" = "binance:BTCUSDT:4h"

[[mounts]]
venue = "binance"
symbol = "BTCUSDT"
interval = "1h"
interval_ms = 3600000

[mounts.strategy]
file = "buy_hold.rhai"

[mounts.strategy.params]
size = 1.0

[[mounts]]
venue = "bybit"
symbol = "BTCUSDT"
interval = "1h"
interval_ms = 3600000

[mounts.strategy]
file = "buy_hold.rhai"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect("[inputs] is not a mount field");
    let rows = p.mount_rows();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.inputs, p.inputs, "every row reads through the one table");
    }
}
