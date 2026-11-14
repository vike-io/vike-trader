//! Each maker file's header against its reader: the spread maker's declared keys are its own
//! `SpreadMaker::PARAM_KEYS` (moved into the file with the maker) with their kinds (its `why` and `optimistic_quotes` were
//! the registry's rows, held to them until the registry was deleted); the cross-exchange maker
//! declares exactly the keys its `from_params` reads.

use vike_model::SpreadModel;
use vike_model::params::Params;

use crate::spread_maker::SpreadMaker;

const SPREAD_MAKER: &str = include_str!("../../../../strategies/market_making/spread_maker.rs");
const CROSS_EXCHANGE_MAKER: &str =
    include_str!("../../../../strategies/market_making/cross_exchange_maker.rs");

/// The file's header: the first fenced `toml` block of its leading `//!` lines.
fn header(file: &str) -> toml::Table {
    let block: Vec<&str> = file
        .lines()
        .map_while(|l| l.strip_prefix("//!"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .skip_while(|l| *l != "```toml")
        .skip(1)
        .take_while(|l| *l != "```")
        .collect();
    assert!(!block.is_empty(), "no ```toml block in the file's leading doc");
    block.join("\n").parse().expect("the header is TOML")
}

/// `[params]`, as `(key, type)` sorted by key.
fn declared(header: &toml::Table) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = header["params"]
        .as_table()
        .expect("[params]")
        .iter()
        .map(|(k, v)| (k.clone(), v["type"].as_str().expect("type").to_string()))
        .collect();
    keys.sort();
    keys
}

#[test]
fn the_spread_maker_declares_exactly_the_keys_its_reader_reads() {
    let mut want: Vec<(String, String)> = SpreadMaker::PARAM_KEYS
        .iter()
        .map(|(k, kinds)| {
            let t = match *kinds {
                ["float", "integer"] => "number",
                ["string"] => "text",
                other => panic!("`{k}`: no header type for the kinds {other:?}"),
            };
            (k.to_string(), t.to_string())
        })
        .collect();
    want.sort();
    assert_eq!(declared(&header(SPREAD_MAKER)), want);
}

#[test]
fn the_spread_maker_strategy_table_runs_live_with_a_reason_and_optimistic_quotes() {
    let h = header(SPREAD_MAKER);
    let s = h["strategy"].as_table().expect("[strategy]");
    assert!(s["why"].as_str().is_some_and(|why| !why.is_empty()), "a live file states its `why`");
    assert_eq!(s["runs_on"].as_str(), Some("live"));
    assert_eq!(s["optimistic_quotes"].as_bool(), Some(true));
    assert_eq!(s.get("routes"), None, "single_leg, the default: no key names a market");
}

/// `gueant_maker` became the `spread_model` key: its declared default is the model an empty table
/// prices with, and `"gueant"` is the model the alias forced.
#[test]
fn the_spread_model_default_is_what_an_empty_table_prices_with() {
    let h = header(SPREAD_MAKER);
    let default = h["params"]["spread_model"]["default"].as_str().expect("a default");
    let model = |p: &Params| {
        SpreadMaker::from_params(p)
            .expect("reads")
            .params()
            .avellaneda_stoikov
            .expect("A-S")
            .spread_model
    };
    let mut named = Params::new();
    named.insert("spread_model", default);
    assert_eq!(model(&Params::new()), SpreadModel::AvellanedaStoikov);
    assert_eq!(model(&named), model(&Params::new()));
    let mut gueant = Params::new();
    gueant.insert("spread_model", "gueant");
    assert_eq!(model(&gueant), SpreadModel::Gueant);
}

#[test]
fn the_cross_exchange_maker_declares_exactly_the_keys_its_reader_reads() {
    let body = CROSS_EXCHANGE_MAKER
        .split("    pub fn from_params(params: &Params) -> Self {")
        .nth(1)
        .expect("from_params")
        .split("\n    }\n")
        .next()
        .expect("its body");
    let mut read: Vec<(String, String)> = Vec::new();
    for (call, t) in [("get_i(\"", "integer"), ("get(\"", "number")] {
        for chunk in body.split(call).skip(1) {
            let key = chunk.split('"').next().expect("a key");
            // `params.get("maker_symbol")` is the leg read, a string; `get("…")` is the f64 closure.
            let t = if key.ends_with("_symbol") { "text" } else { t };
            if !read.iter().any(|(k, _)| k == key) {
                read.push((key.to_string(), t.to_string()));
            }
        }
    }
    read.sort();
    let h = header(CROSS_EXCHANGE_MAKER);
    assert_eq!(declared(&h), read);
    assert_eq!(
        h["strategy"]["runs_on"].as_str(),
        Some("paper"),
        "decision 0033: mounted by nothing live"
    );
    assert_eq!(h["strategy"]["routes"].as_str(), Some("multi_leg"));
}
