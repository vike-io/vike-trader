//! The "this name moved" hint (`vike_model::host_build::moved_name`): golden messages for the
//! source shapes a user file can take, the shapes that must draw NO hint, the per-file folder walk,
//! and the name list held against `vike_marketdata`'s root.
//!
//! ⚠ Every old-spelling SOURCE below is built from parts through [`old`]: the market-data codemod
//! rewrites the joined spelling wherever it finds it, string literals included, and a rewritten
//! input would make these tests check the new spelling twice.

use std::fs;

use vike_model::host_build::moved_name::{MOVED_NAMES, folder_hints, moved_name_hint};

use super::scratch;

/// `vike_model` joined to `tail` by `::`, never written as one literal (the module doc says why).
fn old(tail: &str) -> String {
    format!("{}::{}", "vike_model", tail)
}

/// The golden message for ONE moved name.
const BAR_HINT: &str = "`Bar` moved from `vike_model` to `vike_marketdata`: write \
     `use vike_marketdata::Bar;`, keep the rest in `use vike_model::{…}`, and start any inline \
     path to it with `vike_marketdata::`";

#[test]
fn the_documented_strategy_line_draws_the_one_name_hint() {
    let src = format!("use {};\n\npub struct S;\n", old("{Bar, Broker, Strategy}"));
    assert_eq!(moved_name_hint(&src).as_deref(), Some(BAR_HINT));
}

#[test]
fn a_single_name_use_and_an_inline_path_each_draw_it() {
    assert_eq!(moved_name_hint(&format!("use {};", old("Bar"))).as_deref(), Some(BAR_HINT));
    let inline = format!("fn f(b: &{}) -> f64 {{ b.close }}", old("Bar"));
    assert_eq!(moved_name_hint(&inline).as_deref(), Some(BAR_HINT));
    // A renamed import still names the moved item.
    assert_eq!(moved_name_hint(&format!("use {} as B;", old("Bar"))).as_deref(), Some(BAR_HINT));
    // `::` with whitespace around it, and a leading `::`, are still the same path.
    let spaced = format!("use ::{} :: Bar;", "vike_model");
    assert_eq!(moved_name_hint(&spaced).as_deref(), Some(BAR_HINT));
}

#[test]
fn nested_multi_line_groups_list_every_name_in_first_mention_order_once() {
    let src = format!(
        "use {}\n    self,\n    params::{{Params, Bar}},\n    {{TradeTick, strategy::{{Strategy}}}},\n    \
         L2Book as Book,\n    Bar,\n    TradeTick,\n}};\n\nfn g(q: {}) {{}}\n",
        old("{"),
        old("QuoteTick"),
    );
    assert_eq!(
        moved_name_hint(&src).as_deref(),
        Some(
            "`TradeTick`, `L2Book`, `Bar`, `QuoteTick` moved from `vike_model` to \
             `vike_marketdata`: write `use vike_marketdata::{TradeTick, L2Book, Bar, QuoteTick};`, \
             keep the rest in `use vike_model::{…}`, and start any inline path to them with \
             `vike_marketdata::`"
        ),
        "a sub-module item (`params::{{..}}`) names nothing at the root; `self` is not a name"
    );
}

#[test]
fn functions_are_moved_names_too() {
    let src = format!(
        "let bars = {}(&ticks, 60_000);\nlet b = {}(&t);\n",
        old("consolidate_trades"),
        old("trade_tick_to_bar")
    );
    assert_eq!(
        moved_name_hint(&src).as_deref(),
        Some(
            "`consolidate_trades`, `trade_tick_to_bar` moved from `vike_model` to \
             `vike_marketdata`: write `use vike_marketdata::{consolidate_trades, \
             trade_tick_to_bar};`, keep the rest in `use vike_model::{…}`, and start any inline \
             path to them with `vike_marketdata::`"
        )
    );
}

#[test]
fn the_new_spelling_draws_no_hint() {
    let src = format!(
        "use vike_marketdata::Bar;\nuse {};\nfn f(p: &{}) {{}}\n",
        old("{Broker, Strategy, params::Params}"),
        old("params::Params")
    );
    assert_eq!(moved_name_hint(&src), None);
    assert_eq!(moved_name_hint(""), None);
}

#[test]
fn comments_and_literals_supply_no_path() {
    let src = format!(
        "// use {line};\n/// see {doc}\n/* {blk} /* nested {blk} */ still {blk} */\n\
         let s = \"use {line}\";\nlet r = r#\"{line}\"#;\nlet b = b\"{line}\";\n\
         let c = '{{'; let d = '\\''; let e = 'é';\nuse {kept};\n",
        line = old("Bar"),
        doc = old("L2Book"),
        blk = old("TradeTick"),
        kept = old("{Broker, Strategy}"),
    );
    assert_eq!(moved_name_hint(&src), None, "{src}");
}

#[test]
fn a_lifetime_does_not_open_a_char_literal_that_hides_the_path() {
    let src = format!("fn f<'a>(x: &'a str) -> &'a str {{ x }}\nuse {};\n", old("Bar"));
    assert_eq!(moved_name_hint(&src).as_deref(), Some(BAR_HINT));
}

#[test]
fn a_longer_identifier_or_a_glob_is_not_a_moved_name() {
    assert_eq!(moved_name_hint(&format!("use {};", old("Barrel"))), None);
    assert_eq!(moved_name_hint(&format!("use my_{};", old("Bar"))), None);
    assert_eq!(moved_name_hint(&format!("use {};", old("*"))), None, "a glob names nothing");
}

#[test]
fn folder_hints_name_each_offending_file_in_sorted_order() {
    let dir = scratch("moved-name-folder");
    let strat = dir.join("alpha");
    fs::create_dir_all(strat.join("helpers")).unwrap();
    fs::write(strat.join("alpha.rs"), format!("use {};\n", old("{Bar, Broker}"))).unwrap();
    fs::write(strat.join("clean.rs"), "use vike_marketdata::Bar;\n").unwrap();
    fs::write(strat.join("helpers").join("util.rs"), format!("use {};\n", old("Bar"))).unwrap();
    fs::write(strat.join("notes.txt"), format!("{}\n", old("Bar"))).unwrap();
    let got = folder_hints(&strat);
    assert_eq!(
        got,
        [
            format!("{}: {BAR_HINT}", strat.join("alpha.rs").display()),
            format!("{}: {BAR_HINT}", strat.join("helpers").join("util.rs").display()),
        ],
        "only `.rs` files, one line each, sorted by path"
    );
    assert!(folder_hints(&dir.join("absent")).is_empty());
}

/// Each listed name resolves at `vike_marketdata`'s root (the `use` is the check), and the list is
/// exactly the hint's, in its order.
macro_rules! names_at_the_marketdata_root {
    ($($name:ident),+ $(,)?) => {{
        #[expect(unused_imports, reason = "the import IS the check: the name must resolve there")]
        use vike_marketdata::{$($name),+};
        [$(stringify!($name)),+]
    }};
}

#[test]
fn every_moved_name_resolves_at_the_marketdata_root() {
    let resolved = names_at_the_marketdata_root!(
        Bar,
        BarConsolidator,
        BookLevel,
        BookUpdate,
        BookUpdateKind,
        DeltaDecision,
        FillSim,
        L2Book,
        OptionKind,
        QuoteTick,
        SeqPolicy,
        TradeTick,
        book_taker_price,
        consolidate_quotes,
        consolidate_trades,
        quote_tick_to_bar,
        trade_tick_to_bar,
    );
    assert_eq!(MOVED_NAMES, resolved);
}
