//! The "this name moved" hint (`vike_model::host_build::moved_name`): golden messages for the
//! source shapes a user file can take, the shapes that must draw NO hint, the per-file folder walk,
//! and the name list held against `vike_marketdata`'s root.
//!
//! ⚠ Every old-spelling SOURCE below is built from parts through [`old`]: the market-data codemod
//! rewrites the joined spelling wherever it finds it, string literals included, and a rewritten
//! input would make these tests check the new spelling twice.

use std::fs;

use vike_model::host_build::moved_name::{
    MOVED_NAMES, RENAMED_FIELDS, RENAMED_MARKETDATA_NAMES, current_marketdata_name, folder_hints,
    moved_name_hint, renamed_field_hint, renamed_marketdata_hint, renamed_name_hint, source_hints,
};

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
    let src = format!("use {};\n\npub struct S;\n", old("{Bar, OrderBroker, Strategy}"));
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
             `vike_marketdata`: write `use vike_marketdata::{TickTrade, L2Book, Bar, TickQuote};`, \
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
        old("{OrderBroker, Strategy, params::Params}"),
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
        kept = old("{OrderBroker, Strategy}"),
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
    fs::write(strat.join("alpha.rs"), format!("use {};\n", old("{Bar, OrderBroker}"))).unwrap();
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

// ── the renamed-name hint (`RENAMED_NAMES`) ─────────────────────────────────────────────────────
//
// ⚠ These sources name the OLD seam names on purpose; the scripted renames leave this file alone
// (`moved_name.rs`'s module doc).

/// The golden message for the one broker interface's rename.
const HFT_HINT: &str = "`HftBroker` is now `OrderBroker` in `vike_model`: write the new name \
     wherever the file names the old one";

#[test]
fn a_renamed_name_draws_the_renamed_hint_in_first_mention_order() {
    let src = format!("use {};\npub fn build<B: HftBroker>() {{}}\n", old("{HftBroker, Strategy}"));
    assert_eq!(renamed_name_hint(&src).as_deref(), Some(HFT_HINT));
    assert_eq!(moved_name_hint(&src), None, "no market-data name moved here");
    let two = format!("use {};\nfn f(o: {}) {{}}\n", old("{TaggedOrder, Broker}"), old("Broker"));
    assert_eq!(
        renamed_name_hint(&two).as_deref(),
        Some(
            "`TaggedOrder` is now `OrderPlacement`, `Broker` is now `OrderBroker` in `vike_model`: \
             write the new name wherever the file names the old one"
        )
    );
}

/// Phase 4's spelled-out names fire per root, roots in table order: a `vike_model` root name and the
/// market maker reached through its OLD crate name (G14). Every old spelling is built from parts.
#[test]
fn a_spelled_out_name_and_the_old_maker_crate_draw_one_clause_per_root() {
    let share = concat!("share", "_qty");
    let maker = concat!("vike", "_mm");
    let src = format!(
        "use {maker}::{{SpreadMakerPreset, reward}};\nuse {};\nfn f() {{ let _ = {maker}::rearm_fee_floor; }}\n",
        old(share)
    );
    assert_eq!(
        renamed_name_hint(&src),
        Some(format!(
            "`{share}` is now `share_quantity` in `vike_model`; `SpreadMakerPreset` is now \
             `vike_market_making::SpreadMakerPreset`, `reward` is now `vike_market_making::reward`, \
             `rearm_fee_floor` is now `vike_market_making::rearm_fee_floor` in `{maker}`: write the \
             new name wherever the file names the old one"
        ))
    );
    // the new spellings draw nothing
    let new = "use vike_market_making::SpreadMakerPreset;\nuse vike_model::share_quantity;\n";
    assert_eq!(renamed_name_hint(new), None);
}

/// The order client id names the abbreviation pass spelled out (owner 2026-11-11) draw the same hint,
/// a type and a function alike.
#[test]
fn the_order_client_id_renames_draw_the_renamed_hint() {
    let src = format!(
        "use {};\nfn ok(id: &str) -> bool {{ {}(id) }}\n",
        old("{InstanceOrigin, OriginError}"),
        old("is_valid_crypto_coid")
    );
    assert_eq!(
        renamed_name_hint(&src).as_deref(),
        Some(
            "`InstanceOrigin` is now `OrderClientIdOrigin`, `OriginError` is now \
             `OrderClientIdOriginError`, `is_valid_crypto_coid` is now \
             `is_valid_crypto_order_client_id` in `vike_model`: write the new name wherever the file \
             names the old one"
        )
    );
    let new = format!("use {};\n", old("{OrderClientIdOrigin, is_valid_crypto_order_client_id}"));
    assert_eq!(renamed_name_hint(&new), None, "the new names draw nothing");
}

#[test]
fn the_new_names_draw_no_renamed_hint() {
    let src = format!("use {};\n", old("{OrderBroker, Strategy, Fill}"));
    assert_eq!(renamed_name_hint(&src), None);
    assert!(source_hints(&src).is_empty());
    // a longer identifier is not the renamed one
    assert_eq!(renamed_name_hint(&format!("use {};", old("BrokerX"))), None);
}

/// `vike_tactics` joined to `tail` by `::`, never one literal (the order-language rename rewrites a
/// joined old word wherever it finds it).
fn tactics(tail: &str) -> String {
    format!("{}::{}", "vike_tactics", tail)
}

/// An order-language row draws a clause naming its own root; a seam row and a language row in one
/// file give one clause each, in the table's order.
#[test]
fn an_old_order_language_word_draws_its_own_clause_naming_the_crate() {
    let src = format!(
        "use {};\nimpl Rule for S {{ fn header(&self) -> Header {{ Header::PLAIN }} }}\n",
        tactics("{Header, Rule, Side, Tactics}")
    );
    assert_eq!(
        renamed_name_hint(&src).as_deref(),
        Some(
            "`Header` is now `RuleDefaults`, `Side` is now `vike_model::OrderSide` in `vike_tactics`: \
             write the new name wherever the file names the old one"
        )
    );
    let both = format!("use {};\nuse {};\n", tactics("Run"), old("HftBroker"));
    assert_eq!(
        renamed_name_hint(&both).as_deref(),
        Some(
            "`HftBroker` is now `OrderBroker` in `vike_model`; `Run` is now `RuleRun` in \
             `vike_tactics`: write the new name wherever the file names the old one"
        )
    );
    // the old word through ANOTHER crate is another name, and the kept words draw nothing
    assert_eq!(renamed_name_hint(&format!("use {};", old("Side"))), None);
    assert_eq!(renamed_name_hint(&format!("use {};", tactics("{Rule, Tactics}"))), None);
}

/// A row rooted at a crate's MODULE (`vike_indicators::strategy_kit`) draws on that path only.
#[test]
fn a_module_rooted_row_draws_through_its_whole_path() {
    let kit = |tail: &str| format!("{}::{}::{}", "vike_indicators", "strategy_kit", tail);
    assert_eq!(
        renamed_name_hint(&format!("use {};\n", kit("{Candles, Line}"))).as_deref(),
        Some(
            "`Candles` is now `HeikinAshiCandles` in `vike_indicators::strategy_kit`: write the new \
             name wherever the file names the old one"
        )
    );
    assert!(renamed_name_hint(&format!("let c = {}::default();", kit("Candles"))).is_some());
    // the crate's root, or another module, is not that path
    assert_eq!(renamed_name_hint(&format!("use {}::{};", "vike_indicators", "Candles")), None);
    assert_eq!(
        renamed_name_hint(&format!("use {}::{}::{};", "vike_indicators", "frame", "Candles")),
        None
    );
}

/// Both hints, the moved one first, each its own line: what the builder appends and what a folder
/// prints per file.
#[test]
fn a_file_drawing_both_hints_gets_one_line_for_each() {
    let src = format!("use {};\n", old("{Bar, HftBroker}"));
    assert_eq!(source_hints(&src), [BAR_HINT, HFT_HINT]);
    let dir = scratch("renamed-name-folder");
    let strat = dir.join("beta");
    fs::create_dir_all(&strat).unwrap();
    fs::write(strat.join("beta.rs"), &src).unwrap();
    let file = strat.join("beta.rs").display().to_string();
    assert_eq!(
        folder_hints(&strat),
        [format!("{file}: {BAR_HINT}"), format!("{file}: {HFT_HINT}")]
    );
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
        L2BookLevel,
        L2BookUpdate,
        L2BookUpdateKind,
        L2BookDeltaDecision,
        L2BookFillSimulation,
        L2Book,
        OptionKind,
        TickQuote,
        L2BookSequencePolicy,
        TickTrade,
        book_taker_price,
        consolidate_quotes,
        consolidate_trades,
        quote_tick_to_bar,
        trade_tick_to_bar,
    );
    assert_eq!(MOVED_NAMES.map(current_marketdata_name), resolved);
}

/// `vike_marketdata` joined to `tail` by `::`, built from parts like [`old`].
fn market(tail: &str) -> String {
    format!("{}::{}", "vike_marketdata", tail)
}

/// A file naming a market-data name by its OLD spelling at `vike_marketdata`'s root draws the
/// renamed hint, and a moved name's hint writes the name that root spells today.
#[test]
fn a_renamed_market_data_name_draws_its_hint_and_a_moved_one_names_the_new_spelling() {
    let src =
        format!("use {};\nfn f(t: &{}) {{}}\n", market("{QuoteTick, Bar}"), market("TradeTick"));
    assert_eq!(
        renamed_marketdata_hint(&src).as_deref(),
        Some(
            "`QuoteTick` is now `TickQuote`, `TradeTick` is now `TickTrade` in `vike_marketdata`: \
             write the new name wherever the file names the old one"
        )
    );
    assert_eq!(source_hints(&src).len(), 1, "no `vike_model` path here, so only that one line");
    assert_eq!(
        moved_name_hint(&format!("use {};", old("BookUpdate"))).as_deref(),
        Some(
            "`BookUpdate` moved from `vike_model` to `vike_marketdata`: write \
             `use vike_marketdata::L2BookUpdate;`, keep the rest in `use vike_model::{…}`, and start \
             any inline path to it with `vike_marketdata::`"
        )
    );
    assert_eq!(renamed_marketdata_hint(&format!("use {};", market("{TickQuote, Bar}"))), None);
    for (old_name, new_name) in RENAMED_MARKETDATA_NAMES {
        assert!(MOVED_NAMES.contains(&old_name), "`{old_name}` is a moved name");
        assert_eq!(current_marketdata_name(old_name), new_name);
    }
}

// ── the renamed-field hint (`RENAMED_FIELDS`) ───────────────────────────────────────────────────
//
// ⚠ The old field names are built from parts (`RENAMED_FIELDS`' own needles), never spelled.

/// The two old field names, as a file would read them.
fn old_fields() -> (&'static str, &'static str) {
    (RENAMED_FIELDS[0].0, RENAMED_FIELDS[1].0)
}

#[test]
fn a_file_that_reads_a_renamed_field_draws_the_one_line_fix() {
    let (ts, seq) = old_fields();
    let src = format!(
        "fn f(bar: &Bar, u: &BookUpdate) -> i64 {{ bar.{ts} + u.{seq} as i64 + bar.{ts} }}"
    );
    let want = format!(
        "`.{ts}` is now `.timestamp`, `.{seq}` is now `.sequence` on vike's types: write the new name wherever the file reads the old one"
    );
    assert_eq!(renamed_field_hint(&src, None).as_deref(), Some(want.as_str()));
    assert_eq!(source_hints(&src), [want]);
}

#[test]
fn a_call_a_string_a_comment_and_another_receiver_draw_no_field_hint() {
    let (ts, _) = old_fields();
    let src = format!(
        "// bar.{ts}
fn f(x: &X) {{ x.{ts}(); let s = \"bar.{ts}\"; }}"
    );
    assert_eq!(
        renamed_field_hint(&src, None),
        None,
        "a call, a comment and a string read no field"
    );
    let other = format!("fn on_bar(bar) {{ this.{ts} = 1; }}");
    assert!(renamed_field_hint(&other, None).is_some(), "every receiver, unnarrowed");
    assert_eq!(
        renamed_field_hint(&other, Some("bar")),
        None,
        "narrowed to `bar`, `this` is not it"
    );
    let read = format!("fn on_bar(bar) {{ bar.{ts} }}");
    assert!(renamed_field_hint(&read, Some("bar")).is_some(), "narrowed, `bar`'s own read");
}
