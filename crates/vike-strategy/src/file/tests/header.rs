//! The header: where it is found, what it declares, what it refuses, and the params checks over
//! it that the deleted registry had no twin of (no `[params]`, a range).

use vike_model::params::{ParamValue, Params};

use crate::file::scan::StrategyFileLanguage;
use crate::file::{
    ParamType, RouteKind, StrategyFileFill, StrategyFileHeader, StrategyFileHeaderRun,
    StrategyFileNeed, StrategyFileRoutes, StrategyFileRunsOn,
};

fn header(text: &str) -> StrategyFileHeader {
    StrategyFileHeader::from_toml(text).unwrap_or_else(|e| panic!("{e}\n---\n{text}"))
}

fn refused(text: &str) -> String {
    match StrategyFileHeader::from_toml(text) {
        Ok(h) => panic!("expected a refusal, parsed {h:?}\n---\n{text}"),
        Err(e) => e,
    }
}

fn params(text: &str) -> Params {
    toml::from_str(text).expect("params")
}

/// The module doc's example, as a Rust file writes it.
const GRID_FILE: &str = "//! Grid: a ladder of resting limits around an anchor.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"rests only post-only limits inside the band\"
//! routes = \"single_leg\"
//! optimistic_quotes = false
//!
//! [params.size]
//! type = \"number\"
//! default = 1.0
//! [params.anchor]
//! type = \"text\"
//! default = \"first\"
//! [params.anchor_price]
//! type = \"number\"
//! needs = { anchor = [\"fixed\"] }
//! [params.rungs]
//! type = \"integer\"
//! range = [1, 500]
//! [params.symbol]
//! type = \"text\"
//! route = \"symbol\"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_model::params::Params;
";

#[test]
fn the_example_file_parses_into_its_declarations() {
    let h = StrategyFileHeader::parse(GRID_FILE).expect("the example parses");
    assert_eq!(h.strategy.runs_on, StrategyFileRunsOn::Live);
    assert_eq!(h.strategy.why.as_deref(), Some("rests only post-only limits inside the band"));
    assert_eq!(h.strategy.routes, StrategyFileRoutes::SingleLeg);
    assert!(!h.strategy.optimistic_quotes);
    // The header's order, not the alphabet's: `size` was written first.
    let keys: Vec<&str> = h.params.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["size", "anchor", "anchor_price", "rungs", "symbol"]);
    let size = h.param("size").expect("size");
    assert_eq!(size.r#type, ParamType::Number);
    assert_eq!(size.default, Some(ParamValue::Float(1.0)));
    assert_eq!(h.param("rungs").expect("rungs").range, Some([1.0, 500.0]));
    assert_eq!(
        h.param("anchor_price").expect("anchor_price").needs.get("anchor"),
        Some(&StrategyFileNeed::OneOf(vec!["fixed".to_string()]))
    );
    assert_eq!(h.param("symbol").expect("symbol").route, Some(RouteKind::Symbol));
    assert_eq!(h.run, Some(StrategyFileHeaderRun { bound_exits: true, ..Default::default() }));
    assert_eq!(
        h.readable_keys(),
        "size (a number (integer or float)), anchor (a string), anchor_price (a number (integer \
         or float)), rungs (an integer), symbol (a string)"
    );
}

#[test]
fn an_absent_key_takes_its_default() {
    let h = header("[strategy]\n");
    assert_eq!(h.strategy.runs_on, StrategyFileRunsOn::Paper);
    assert_eq!(h.strategy.why, None);
    assert_eq!(h.strategy.routes, StrategyFileRoutes::SingleLeg);
    assert!(!h.strategy.optimistic_quotes);
    assert_eq!(h.strategy.fill, StrategyFileFill::NextOpen);
    assert!(h.params.is_empty());
    assert_eq!(h.run, None);
    let run = header("[strategy]\n[run]\nfills_a_day = 13\n").run.expect("[run]");
    assert_eq!(run, StrategyFileHeaderRun { fills_a_day: Some(13), ..Default::default() });
}

/// `fill` takes its two words, and one strategy file states it for the backtest and the paper book alike.
#[test]
fn the_fill_word_is_next_open_or_close() {
    assert_eq!(
        header(
            "[strategy]
fill = \"close\"
"
        )
        .strategy
        .fill,
        StrategyFileFill::Close
    );
    assert_eq!(
        header(
            "[strategy]
fill = \"next_open\"
"
        )
        .strategy
        .fill,
        StrategyFileFill::NextOpen
    );
    let e = refused(
        "[strategy]
fill = \"open\"
",
    );
    assert!(e.contains("next_open") && e.contains("close"), "the refusal names the two words: {e}");
}

#[test]
fn a_rhai_file_writes_the_same_header_behind_plain_comments() {
    let source = "\u{feff}// A Rhai strategy.\r\n//\r\n// ```toml\r\n// [strategy]\r\n// runs_on = \
                  \"backtest\"\r\n// ```\r\n\r\nfn on_bar(bar) {}\r\n";
    let h = StrategyFileHeader::parse(source).expect("a Rhai header parses");
    assert_eq!(h.strategy.runs_on, StrategyFileRunsOn::Backtest);
}

#[test]
fn the_header_is_found_only_in_the_leading_comment_lines() {
    let no_header = "fn main() {}\n//! ```toml\n//! [strategy]\n//! ```\n";
    assert!(StrategyFileHeader::parse(no_header).expect_err("no header").starts_with("no header"));
    assert!(StrategyFileHeader::parse("").expect_err("empty").starts_with("no header"));
    let unclosed = "//! ```toml\n//! [strategy]\nfn main() {}\n";
    assert!(StrategyFileHeader::parse(unclosed).expect_err("unclosed").contains("not closed"));
    // The FIRST toml block is the header; a later one in the same comment is prose.
    let two = "//! ```toml\n//! [strategy]\n//! runs_on = \"live\"\n//! ```\n//! ```toml\n//! nonsense\n//! ```\n";
    assert_eq!(
        StrategyFileHeader::parse(two).expect("first block").strategy.runs_on,
        StrategyFileRunsOn::Live
    );
}

#[test]
fn every_table_refuses_a_key_it_does_not_know() {
    for text in [
        "[strategy]\nrunz_on = \"live\"\n",
        "[strategy]\n[params.qty]\ntype = \"number\"\ndefualt = 1\n",
        "[strategy]\n[run]\nbound_exit = true\n",
        "[strategy]\n[parms.qty]\ntype = \"number\"\n",
    ] {
        let e = refused(text);
        assert!(e.contains("unknown field"), "{e}\n---\n{text}");
    }
}

#[test]
fn a_word_outside_the_format_is_refused() {
    for text in [
        "[strategy]\nruns_on = \"everywhere\"\n",
        "[strategy]\nroutes = \"two_legs\"\n",
        "[strategy]\n[params.qty]\ntype = \"str\"\n",
        "[strategy]\n[params.qty]\n",
        "[strategy]\n[params.symbol]\ntype = \"text\"\nroute = \"market\"\n",
        "[strategy]\n[params.a]\ntype = \"text\"\n[params.b]\ntype = \"number\"\nneeds = { a = \"some\" }\n",
        "[strategy]\n[params.a]\ntype = \"text\"\n[params.b]\ntype = \"number\"\nneeds = { a = [] }\n",
        "runs_on = \"live\"\n",
    ] {
        refused(text);
    }
}

#[test]
fn a_declaration_that_contradicts_itself_is_refused() {
    for (text, words) in [
        (
            "[params.qty]\ntype = \"number\"\ndefault = \"1\"\n",
            "`qty`'s default wants a number (integer or float), got string",
        ),
        (
            "[params.side]\ntype = \"text\"\nrange = [0, 1]\n",
            "`side` has a range but is neither a number nor an integer",
        ),
        (
            "[params.rungs]\ntype = \"integer\"\nrange = [5, 1]\n",
            "`rungs`'s range [5, 1] holds no value",
        ),
        (
            "[params.rungs]\ntype = \"integer\"\nrange = [1, 5]\ndefault = 9\n",
            "`rungs`'s default 9 is outside its range [1, 5]",
        ),
        (
            "[params.tick]\ntype = \"number\"\nneeds = { bounded = [\"true\"] }\n",
            "`tick` needs `bounded`, which the header does not declare",
        ),
        (
            "[params.tick]\ntype = \"number\"\nneeds = { tick = \"positive\" }\n",
            "`tick` needs itself",
        ),
        (
            "[params.symbol]\ntype = \"number\"\nroute = \"symbol\"\n",
            "`symbol` is a route, so its type is a string",
        ),
        (
            "[params.venues]\ntype = \"text\"\nroute = \"venue_map\"\n",
            "`venues` is a route, so its type is a table",
        ),
    ] {
        let e = refused(&format!("[strategy]\n{text}"));
        assert!(
            e.starts_with("the header is refused: ") && e.contains(words),
            "{e}\nwanted: {words}"
        );
    }
}

#[test]
fn a_file_with_no_params_accepts_no_key() {
    let h = header("[strategy]\n");
    let handed = params("qty = 1\nsymbol = \"BTCUSDT\"\n");
    assert_eq!(h.unknown_params(&handed), ["qty", "symbol"]);
    assert_eq!(h.readable_keys(), "nothing: declare it in the file's header");
    assert!(h.unknown_params(&Params::new()).is_empty());
}

/// The cost parameters every file has (the owner's ruling of 2026-11-18) are never declared: one
/// meaning each.
#[test]
fn a_header_declaring_a_cost_param_is_refused() {
    for key in vike_model::COST_PARAMS {
        let declared = refused(&format!("[strategy]\n[params.{key}]\ntype = \"number\"\n"));
        assert!(
            declared.contains(&format!("`{key}` is a cost parameter every strategy file has")),
            "{declared}"
        );
    }
}

#[test]
fn a_value_outside_its_range_is_refused_in_the_type_refusals_words() {
    let h = header(
        "[strategy]\n[params.rungs]\ntype = \"integer\"\nrange = [1, 500]\n[params.band]\ntype = \
         \"number\"\nrange = [0.0, 0.5]\n",
    );
    let rendered = |text: &str| -> Vec<String> {
        h.out_of_range_params(&params(text)).iter().map(ToString::to_string).collect()
    };
    assert_eq!(
        rendered("rungs = 900\nband = 0.75\n"),
        [
            "`band` wants a value in [0, 0.5], got 0.75",
            "`rungs` wants a value in [1, 500], got 900",
        ]
    );
    // Both ends are inside; a value of the wrong type is the type check's, not this one's.
    assert!(rendered("rungs = 1\nband = 0.5\n").is_empty());
    assert!(rendered("rungs = 900.0\n").is_empty());
    assert_eq!(h.mistyped_params(&params("rungs = 900.0\n")).len(), 1);
}

#[test]
fn a_multi_leg_header_judges_no_route() {
    let h = header(
        "[strategy]\nroutes = \"multi_leg\"\nmulti_leg_why = \"two legs\"\n[params.symbol_a]\ntype = \
         \"text\"\nroute = \"symbol\"\n",
    );
    assert!(h.misrouted_params(&params("symbol_a = \"ETHUSDT\"\n"), "bybit", "BTCUSDT").is_empty());
}

#[test]
fn a_need_is_judged_against_the_resolved_rows() {
    let h = header(
        "[strategy]\n[params.anchor]\ntype = \"text\"\n[params.anchor_price]\ntype = \
         \"number\"\nneeds = { anchor = [\"fixed\"] }\n[params.open]\ntype = \"integer\"\n\
         [params.delay]\ntype = \"integer\"\nneeds = { open = \"positive\" }\n",
    );
    let rows = |pairs: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    };
    assert!(h.unmet_needs("anchor_price", &rows(&[("anchor", "fixed")])).is_empty());
    assert_eq!(h.unmet_needs("anchor_price", &rows(&[("anchor", "first")])), ["anchor=first"]);
    assert_eq!(h.unmet_needs("anchor_price", &rows(&[])), ["anchor=(absent)"]);
    assert!(h.unmet_needs("delay", &rows(&[("open", "1.5")])).is_empty());
    assert_eq!(h.unmet_needs("delay", &rows(&[("open", "0")])), ["open=0"]);
    // A key with no needs, or no declaration, is consumed unconditionally.
    assert!(h.unmet_needs("anchor", &rows(&[])).is_empty());
    assert!(h.unmet_needs("undeclared", &rows(&[])).is_empty());
}

#[test]
fn a_multi_leg_header_says_why_and_only_a_multi_leg_one_does() {
    let h = header("[strategy]\nroutes = \"multi_leg\"\nmulti_leg_why = \"TWO-LEG: one spread\"\n");
    assert_eq!(h.strategy.multi_leg_why.as_deref(), Some("TWO-LEG: one spread"));
    for (text, words) in [
        ("routes = \"multi_leg\"\n", "needs `multi_leg_why`"),
        ("routes = \"multi_leg\"\nmulti_leg_why = \"  \"\n", "needs `multi_leg_why`"),
        ("multi_leg_why = \"two legs\"\n", "`routes` is not \"multi_leg\""),
    ] {
        let e = refused(&format!("[strategy]\n{text}"));
        assert!(
            e.starts_with("the header is refused: ") && e.contains(words),
            "{e}\nwanted: {words}"
        );
    }
}

#[test]
fn an_array_key_takes_a_list_and_refuses_anything_else() {
    let h = header("[strategy]\n[params.wallets]\ntype = \"array\"\ndefault = [\"0xaaa\"]\n");
    assert_eq!(h.param("wallets").expect("wallets").r#type, ParamType::Array);
    assert!(h.mistyped_params(&params("wallets = [\"0xaaa\", \"0xbbb\"]\n")).is_empty());
    assert!(h.mistyped_params(&params("wallets = []\n")).is_empty());
    let wrong: Vec<String> = h
        .mistyped_params(&params("wallets = \"0xaaa\"\n"))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(wrong, ["`wallets` wants an array, got string"]);
    assert_eq!(h.readable_keys(), "wallets (an array)");
    refused("[strategy]\n[params.wallets]\ntype = \"array\"\ndefault = \"0xaaa\"\n");
    refused("[strategy]\n[params.wallets]\ntype = \"array\"\nrange = [0, 1]\n");
}

/// `[strategy] sdk = N` pins a Rust file's SDK (decision 0296, verdict 2): an integer, absent by
/// default, anything else refused; a `.rhai` file naming it is refused in words.
#[test]
fn the_sdk_pin_is_an_integer_a_rust_file_may_name_and_a_rhai_file_may_not() {
    assert_eq!(header("[strategy]\n").strategy.sdk, None);
    assert_eq!(header("[strategy]\nsdk = 3\n").strategy.sdk, Some(3));
    for wrong in ["sdk = \"3\"\n", "sdk = 3.5\n", "sdk = -1\n", "sdk = [3]\n"] {
        refused(&format!("[strategy]\n{wrong}"));
    }
    let file = "//! ```toml\n//! [strategy]\n//! sdk = 7\n//! ```\n";
    let rust = StrategyFileHeader::parse_file(file, StrategyFileLanguage::Rust).expect("rust");
    assert_eq!(rust.strategy.sdk, Some(7));
    let rhai = StrategyFileHeader::parse_file(file, StrategyFileLanguage::Rhai).expect_err("rhai");
    assert!(
        rhai.starts_with("the header is refused: ")
            && rhai.contains("a Rhai file runs on this vike's own code"),
        "{rhai}"
    );
    let unpinned = "//! ```toml\n//! [strategy]\n//! ```\n";
    assert!(StrategyFileHeader::parse_file(unpinned, StrategyFileLanguage::Rhai).is_ok());
}

/// `ParamType::word()` is the header's word: each member's serde tag (the header's `type = "..."`), so the CLI's
/// help names the type a header writes.
#[test]
fn every_param_type_word_is_its_serde_tag() {
    for ty in ParamType::ALL {
        let tagged: ParamType = serde_json::from_value(serde_json::Value::from(ty.word()))
            .unwrap_or_else(|e| panic!("{ty:?}: {e}"));
        assert_eq!(tagged, ty, "{}", ty.word());
    }
}
