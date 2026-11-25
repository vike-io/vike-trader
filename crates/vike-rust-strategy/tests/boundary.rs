//! The strategy BOUNDARY's text shapes (decision 0296): every type that crosses between a host and a
//! strategy file as TEXT, one canonical value each, held both ways to a committed golden under
//! `tests/boundary/` (the value encodes to the golden's shape, and the golden decodes to the value).
//! Those goldens are part of the boundary hash (`build/source_hash.rs`'s `BOUNDARY_FOLDERS`), so a serde
//! shape change fails here until its golden is updated, and the update moves `sdk::BOUNDARY_HASH`: an SDK
//! cut before it is then unusable on this host (`sdk::usable`). JSON is compared as a parsed document,
//! never as bytes: key order and spacing are not the shape.
//!
//! The types: the mount facts (`vike_model::params::MountFacts`), the params document
//! (`vike_model::params::Params`, as TOML), the stated fee (`vike_model::FeeSchedule`) and slippage
//! (`vike_model::ProfileEngineSlippage`), a forecast (`vike_model::Forecast`), the state
//! (`vike_model::StrategyState`'s msgpack bytes, pinned as hex since `ABI_VERSION` 15), the live params
//! (`vike_model::StrategyParams`), and the two string lists: the names `inputs` and `symbols` lend, and
//! the `echo` pairs. The order list crosses as `repr(C)` rows since 15 (`src/abi.rs`'s `OrderListLegAbi`,
//! under `abi::layout`), no text, so it has no golden here.
//!
//! And the TYPE of every field the two sides read in place (`lent_fields.txt`): `abi::layout` hashes sizes,
//! alignments and offsets, which a same-size change keeps (`String` for `Option<String>`, `i64` for `f64`
//! or `usize`), so each field's `std::any::type_name` is pinned here and moves the boundary with its golden.

use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use vike_model::params::{MountFacts, ParamValue, Params};
use vike_model::{FeeSchedule, Forecast, ProfileEngineSlippage, StrategyParams};
use vike_rust_strategy::guest;

/// Every golden this file holds a type to; `tests/boundary/` holds exactly these.
const GOLDENS: [&str; 10] = [
    "echo.json",
    "fee_schedule.json",
    "forecast.json",
    "lent_fields.txt",
    "mount_facts.json",
    "names.json",
    "params.toml",
    "slippage.json",
    "state.msgpack.hex",
    "strategy_params.json",
];

fn golden(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/boundary").join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `encoded` (the boundary's own encoding of `value`) has the golden's shape, and the golden decodes back
/// to `value` through `decode` (the boundary's own decoder).
fn holds<T: PartialEq + Debug>(
    name: &str,
    value: &T,
    encoded: Value,
    decode: impl Fn(&str) -> Option<T>,
) {
    let text = golden(name);
    let pinned: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        encoded,
        pinned,
        "{name}: the shape that crosses the strategy boundary moved. A shape change IS a boundary change: \
         update the golden (which moves BOUNDARY_HASH and retires the older SDKs on this host). Now:\n{}",
        serde_json::to_string_pretty(&encoded).unwrap_or_default()
    );
    assert_eq!(decode(&text).as_ref(), Some(value), "{name} decodes to the canonical value");
}

fn plain<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("a canonical value encodes")
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).expect("the boundary's encoder writes JSON")
}

fn decoded<T: DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_str(text).ok()
}

#[test]
fn the_boundary_folder_holds_exactly_the_goldens_this_file_checks() {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/boundary");
    let mut found: Vec<String> = fs::read_dir(&folder)
        .expect("tests/boundary")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    assert_eq!(found, GOLDENS, "a golden nothing checks, or a check with no golden");
}

#[test]
fn the_mount_facts_keep_their_shape() {
    let facts = MountFacts {
        symbol: "BTCUSDT".to_string(),
        venue: "bybit".to_string(),
        interval: "1m".to_string(),
        tick_size: Some(0.5),
        static_round_trip_fee_rate: Some(0.25),
        account_round_trip_fee_rate: None,
    };
    holds("mount_facts.json", &facts, plain(&facts), guest::facts_from_json);
}

#[test]
fn the_params_document_keeps_its_shape() {
    let mut route = Params::new();
    route.insert("venue", "bybit");
    let mut params = Params::new();
    params.insert("fast", 12_i64);
    params.insert(
        "levels",
        vec![ParamValue::Int(1), ParamValue::Float(2.5), ParamValue::Str("three".to_string())],
    );
    params.insert("on", true);
    params.insert("ratio", 0.5);
    params.insert("symbol", "BTCUSDT");
    params.insert("route", route);
    let text = golden("params.toml");
    let pinned: toml::Table = toml::from_str(&text).expect("params.toml");
    let encoded: toml::Table =
        toml::from_str(&toml::to_string(&params).expect("params encode as TOML")).expect("TOML");
    assert_eq!(
        encoded, pinned,
        "params.toml: the params document's shape moved (a boundary change: update the golden)"
    );
    // The template's own parse (`template/lib.rs.in`'s `toml::from_str`).
    assert_eq!(toml::from_str::<Params>(&text).expect("the template's parse"), params);
}

#[test]
fn the_stated_fee_keeps_its_shape() {
    let fees = vec![
        FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 },
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.5 },
        FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 12.5 },
        FeeSchedule::ProbabilityScaled {
            taker_rate: 0.25,
            maker_rate: 0.0,
            maker_rebate_share: 0.25,
        },
        FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 },
        FeeSchedule::Free,
    ];
    let encoded = Value::Array(fees.iter().map(|fee| parsed(&guest::fee_json(fee))).collect());
    holds("fee_schedule.json", &fees, encoded, decoded);
}

#[test]
fn the_stated_slippage_keeps_its_shape() {
    let slippage = ProfileEngineSlippage {
        model: "ticks".to_string(),
        value: None,
        ticks: Some(2.0),
        tick: Some(0.5),
        base: None,
        exec_time: None,
        window: None,
        gamma: None,
        eta: None,
    };
    holds("slippage.json", &slippage, parsed(&guest::slippage_json(&slippage)), decoded);
}

#[test]
fn a_forecast_keeps_its_shape() {
    let forecast = Forecast::new(0.5, 60_000, Some(0.25)).expect("a forecast the rule accepts");
    holds("forecast.json", &forecast, plain(&forecast), decoded);
}

/// The state (`vike_model::StrategyState`, `ABI_VERSION` 15): msgpack WITH FIELD NAMES of the strategy's
/// own types, pinned as bytes (hex, whitespace ignored), so a change to how a state encodes (a field
/// order, a float's width, how a `None` is written) moves the boundary; the golden reads back to the value
/// with its floats' bits, and crosses the guest's door as it is.
#[test]
fn the_state_keeps_its_bytes() {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Saved {
        armed: bool,
        levels: Vec<i64>,
        note: String,
        position: f64,
        small: f64,
        nothing: Option<f64>,
    }
    let value = Saved {
        armed: true,
        levels: vec![1, 2],
        note: "a".to_string(),
        position: 0.1 + 0.2,
        small: 1e-7,
        nothing: None,
    };
    let text = golden("state.msgpack.hex");
    let digits: String = text.split_whitespace().collect();
    let pinned: Vec<u8> = (0..digits.len() / 2)
        .map(|i| u8::from_str_radix(&digits[2 * i..2 * i + 2], 16).expect("hex"))
        .collect();
    let state = vike_model::StrategyState::of(&value).expect("encodes");
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
    assert_eq!(
        state.bytes(),
        pinned,
        "state.msgpack.hex: how a state encodes moved (a boundary change: update the golden). Now:\n{}",
        hex(state.bytes())
    );
    let back: Saved = vike_model::StrategyState::from_bytes(pinned.clone()).read().expect("reads");
    assert_eq!(
        (back.position.to_bits(), back.small.to_bits()),
        (value.position.to_bits(), 1e-7_f64.to_bits())
    );
    assert_eq!(back, value);
    let lent = vike_rust_strategy::abi::SliceAbi::of(pinned.as_slice());
    assert_eq!(guest::state_from_bytes(lent), Some(state), "the guest's door takes it as it is");
}

/// The live params (`Strategy::params`, `on_params_updated`): the externally tagged enum, its key/value
/// bag arm the one every file can use.
#[test]
fn the_live_params_keep_their_shape() {
    let mut route = Params::new();
    route.insert("venue", "bybit");
    let mut bag = Params::new();
    bag.insert("fast", 12_i64);
    bag.insert("ratio", 0.5);
    bag.insert("symbol", "BTCUSDT");
    bag.insert("levels", vec![ParamValue::Int(1), ParamValue::Float(2.5)]);
    bag.insert("route", route);
    let params = StrategyParams::Params(bag);
    holds(
        "strategy_params.json",
        &params,
        parsed(&guest::params_json(&params)),
        guest::strategy_params_from_json,
    );
}

/// The names `inputs` and `symbols` lend: a JSON list of strings (a name is any text, so no separator).
#[test]
fn the_names_keep_their_shape() {
    let names: Vec<String> =
        ["BTCUSDT", "ETHUSDT.P", "a name, with \"quotes\""].map(str::to_string).to_vec();
    holds("names.json", &names, parsed(&guest::names_json(&names)), decoded);
}

/// The `echo` export's pairs: a JSON list of `[key, value]` lists, in the file's order.
#[test]
fn the_echo_pairs_keep_their_shape() {
    let pairs = [("fast", "12".to_string()), ("mode", "a value, with \"quotes\"".to_string())];
    let owned: Vec<(String, String)> =
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    holds("echo.json", &owned, parsed(&guest::echo_json(&pairs)), decoded);
}

/// The type of the field `_read` reads: `std::any::type_name`, named through a field access because a
/// field has no type of its own to name.
fn field<S, T>(_read: fn(&S) -> &T) -> &'static str {
    std::any::type_name::<T>()
}

/// The TYPE, the OFFSET and the SIZE of every field a strategy file and its host read in place (`ABI_VERSION`
/// 15): the host's bars, closed rows and ticks, and the order list's rows the file lends. `abi::layout` hashes
/// their sizes, alignments and offsets, which a same-size change keeps (`String` for `Option<String>`, `i64`
/// for `f64` or `usize`): such a change fails here until `lent_fields.txt` is updated, and the update moves the
/// boundary hash, so a file built against the old types is refused at load, never misread. The golden is
/// also an SDK's own record of its rows (`crates/vike-rust-strategy/src/sdk.rs`'s `LENT_FIELDS`): an older
/// SDK whose rows differ from this host's is retired, never adapted (decision 0296's verdict 7).
#[test]
fn the_lent_fields_keep_their_types() {
    use std::mem::{align_of, offset_of, size_of};
    use vike_marketdata::{Bar, TickQuote, TickTrade};
    use vike_model::TradeClosedRow;
    use vike_rust_strategy::abi::OrderListLegAbi;
    // One type's lines: its size and alignment, then every field's type and offset. The destructure names
    // every field and no `..`, so a field added to the type does not compile here until it is listed, and the
    // offsets move with any reorder: a real layout change moves this golden by itself.
    macro_rules! lent {
        ($owner:ident: $($field:ident),+) => {{
            let _every = |row: &$owner| {
                let $owner { $($field: _),+ } = row;
            };
            let name = stringify!($owner);
            let mut lines =
                vec![format!("{name}: {} bytes, align {}", size_of::<$owner>(), align_of::<$owner>())];
            $(lines.push(format!(
                "{name}.{}: {} @ {}",
                stringify!($field),
                field(|row: &$owner| &row.$field),
                offset_of!($owner, $field)
            ));)+
            lines
        }};
    }
    let lines: Vec<String> = [
        lent!(Bar: timestamp, open, high, low, close, volume, funding, bid, ask, symbol),
        lent!(TradeClosedRow: symbol, tag, side, entry_timestamp, entry_bar, entry_price,
            exit_timestamp, exit_bar, exit_price, qty, pnl, commission, net),
        lent!(TickQuote: timestamp, local_timestamp, bid, ask, bid_size, ask_size, symbol),
        lent!(TickTrade: timestamp, local_timestamp, price, size, is_buyer_maker, symbol),
        lent!(OrderListLegAbi: tag, side, qty, order_type, order_type_word, price, trigger_price, trail,
            reduce_only, time_in_force, parent, contingency, linked, order_client_id),
    ]
    .concat();
    let now: String = lines.iter().map(|line| format!("{line}\n")).collect();
    assert_eq!(
        now,
        golden("lent_fields.txt"),
        "lent_fields.txt: a type, an offset or a size of a row read in place moved (a boundary change: update \
         the golden, which moves BOUNDARY_HASH, and an older SDK whose rows differ is retired). Now:\n{now}"
    );
}
