//! `vike_model::host_build::source_edits` on planted files: a rename, a removed item with a
//! replacement, a removed item with none, a no-op, and the pre-SDK tables as data; then the same for
//! a `.rhai` script, with its idempotence rule.
//!
//! ⚠ The SDK-table cases use invented names, so no codemod can rewrite them. The pre-SDK case builds
//! its old spelling from parts (`crates/vike-model/src/host_build/moved_name.rs`'s module doc says why).

use vike_model::host_build::moved_name::commission_hint;
use vike_model::host_build::source_edits::{
    RhaiRename, SdkRename, apply_edits, builtin_renames, builtin_rhai_renames, rhai_edits,
    source_edits,
};

fn row(old: &str, new: Option<&str>, hint: Option<&str>) -> SdkRename {
    SdkRename { old: old.into(), new: new.map(Into::into), hint: hint.map(Into::into) }
}

/// The file `renames` turns `source` into.
fn migrated(source: &str, renames: &[SdkRename]) -> String {
    apply_edits(source, &source_edits(source, renames).edits)
}

#[test]
fn a_rename_edits_the_paths_the_group_item_and_every_bare_use_and_nothing_else() {
    let table = [row("vike_indicators::OldMean", Some("vike_indicators::NewMean"), None)];
    let source = "use vike_indicators::OldMean;\n\
                  use vike_indicators::{Other, OldMean as Mean};\n\
                  // OldMean in a comment stays\n\
                  fn f(m: &OldMean) -> f64 {\n\
                  \x20   let s = \"OldMean\";\n\
                  \x20   m.OldMean + other::OldMean + vike_indicators::OldMean::DEFAULT + Mean::ZERO\n\
                  }\n";
    let expected = "use vike_indicators::NewMean;\n\
                    use vike_indicators::{Other, NewMean as Mean};\n\
                    // OldMean in a comment stays\n\
                    fn f(m: &NewMean) -> f64 {\n\
                    \x20   let s = \"OldMean\";\n\
                    \x20   m.OldMean + other::OldMean + vike_indicators::NewMean::DEFAULT + Mean::ZERO\n\
                    }\n";
    assert_eq!(migrated(source, &table), expected);
    let found = source_edits(source, &table);
    assert!(found.hints.is_empty(), "{:?}", found.hints);
    assert!(
        found
            .edits
            .iter()
            .all(|e| e.hint == "`vike_indicators::OldMean` is now `vike_indicators::NewMean`"),
        "{:?}",
        found.edits
    );
}

#[test]
fn a_removed_item_with_a_replacement_moves_the_path_and_renames_the_bare_uses() {
    let table = [row(
        "vike_tactics::GoneSizer",
        Some("vike_model::QuantitySizer"),
        Some("`GoneSizer` is gone: `vike_model::QuantitySizer` replaces it"),
    )];
    let source = "use vike_tactics::GoneSizer;\nfn s() -> GoneSizer { GoneSizer::default() }\n";
    assert_eq!(
        migrated(source, &table),
        "use vike_model::QuantitySizer;\nfn s() -> QuantitySizer { QuantitySizer::default() }\n"
    );
    let found = source_edits(source, &table);
    assert_eq!(found.edits.len(), 3);
    assert_eq!(found.edits[0].hint, "`GoneSizer` is gone: `vike_model::QuantitySizer` replaces it");

    // In a group whose other names stay, the root cannot move: a hint, and the file untouched.
    let grouped = "use vike_tactics::{GoneSizer, RuleRun};\nfn s() -> GoneSizer { todo!() }\n";
    let found = source_edits(grouped, &table);
    assert!(found.edits.is_empty(), "{:?}", found.edits);
    assert_eq!(
        found.hints,
        [
            "`GoneSizer` is gone: `vike_model::QuantitySizer` replaces it: move `GoneSizer` out of its \
          `use vike_tactics::{…}` group into `use vike_model::QuantitySizer;`"
        ]
    );
}

#[test]
fn a_removed_item_with_no_replacement_is_a_hint_and_no_edit() {
    let table =
        [row("vike_tactics::Retired", None, Some("`Retired` is gone: write the rule by hand"))];
    let source = "use vike_tactics::Retired;\n";
    let found = source_edits(source, &table);
    assert!(found.edits.is_empty());
    assert_eq!(found.hints, ["`Retired` is gone: write the rule by hand"]);
}

#[test]
fn a_file_that_reaches_no_row_draws_nothing_and_comes_back_byte_identical() {
    let table = [
        row("vike_indicators::OldMean", Some("vike_indicators::NewMean"), None),
        row("vike_tactics::Retired", None, None),
    ];
    // Every mention is a comment, a literal, a field, another root's item or a glob.
    let source = "//! OldMean\nuse vike_indicators::*;\nuse other::OldMean;\n\
                  fn f(x: X) -> &str { let _ = x.Retired; \"vike_tactics::Retired\" }\n";
    let found = source_edits(source, &table);
    assert!(found.edits.is_empty(), "{:?}", found.edits);
    assert!(found.hints.is_empty(), "{:?}", found.hints);
    assert_eq!(migrated(source, &table), source);
    // An empty table changes nothing either.
    assert_eq!(migrated(source, &[]), source);
}

#[test]
fn the_pre_sdk_tables_ride_as_data() {
    let rows = builtin_renames();
    let moved = format!("use {}::{};\nfn f(b: &Bar) {{}}\n", "vike_model", "Bar");
    assert_eq!(migrated(&moved, &rows), "use vike_marketdata::Bar;\nfn f(b: &Bar) {}\n");
    // The order language's old commission is a hint only, in `commission_hint`'s own words.
    let cost = format!("use {}::{};\n", "vike_tactics", "Commission");
    let found = source_edits(&cost, &rows);
    assert!(found.edits.is_empty(), "{:?}", found.edits);
    assert_eq!(found.hints, [commission_hint(&cost).expect("the hint draws")]);
}

// ================================ a `.rhai` script (`rhai_edits`) ================================

fn rhai_row(kind: &str, old: &str, new: Option<&str>) -> RhaiRename {
    RhaiRename { kind: kind.into(), old: old.into(), new: new.map(Into::into) }
}

/// The script `rows` turn `source` into, `bound` being the names this vike binds.
fn rhai_migrated(source: &str, rows: &[RhaiRename], bound: &[&str]) -> String {
    apply_edits(source, &rhai_edits(source, rows, &|name| bound.contains(&name)).edits)
}

#[test]
fn a_rhai_function_and_an_indicator_are_edited_and_no_comment_or_string_is() {
    let rows = [
        rhai_row("fn", "old_signal", Some("new_signal")),
        rhai_row("indicator", "oldosc", Some("new_osc")),
    ];
    let source = "// old_signal()\nlet a = old_signal(1) + x.old_signal(2);\n\
                  let d = oldosc(3) + oldosc_k(3) + oldoscillate(3);\nlet e = \"old_signal(1)\" + `old_signal`;\n";
    assert_eq!(
        rhai_migrated(source, &rows, &[]),
        "// old_signal()\nlet a = new_signal(1) + x.new_signal(2);\n\
         let d = new_osc(3) + new_osc_k(3) + oldoscillate(3);\nlet e = \"old_signal(1)\" + `old_signal`;\n"
    );
}

/// **A property or a type is a hint, never an edit**: no list says whether this vike still has one,
/// so the script's own map field (`state.size`), range (`0..size`) or variable of that name would be
/// rewritten with it. Each place is named by its line.
#[test]
fn a_rhai_property_or_type_row_is_a_hint_per_line_and_the_script_is_untouched() {
    let rows = [rhai_row("get", "size", Some("quantity")), rhai_row("type", "OldSide", None)];
    let source = "let state = #{ size: 1 };\nlet a = state.size;\nfor i in 0..size {}\nlet c = OldSide::Long;\n";
    let found = rhai_edits(source, &rows, &|_| false);
    assert!(found.edits.is_empty(), "{:?}", found.edits);
    assert_eq!(
        found.hints,
        [
            "line 2: `size` (a get) is now `quantity` in this vike: rewrite it by hand where it is \
             vike's, not where it is the script's own",
            "line 3: `size` (a get) is now `quantity` in this vike: rewrite it by hand where it is \
             vike's, not where it is the script's own",
            "line 4: `OldSide` (a type) is gone in this vike: rewrite it by hand where it is vike's, \
             not where it is the script's own",
        ]
    );
}

/// **The idempotence rule**: an old name is rewritten only while it resolves to nothing, so a name
/// the engine binds, or one the script defines, is left alone, and a second pass finds nothing.
#[test]
fn a_rhai_name_that_still_resolves_is_left_alone_and_a_second_pass_changes_nothing() {
    let rows = [
        rhai_row("fn", "old_signal", Some("new_signal")),
        rhai_row("fn", "close", Some("flatten")),
    ];
    let source = "let a = old_signal(1) + close();\n";
    let once = rhai_migrated(source, &rows, &["close"]);
    assert_eq!(once, "let a = new_signal(1) + close();\n");
    assert_eq!(rhai_migrated(&once, &rows, &["close"]), once, "a second pass changes nothing");
    let defines = "fn old_signal(x) { x }\nlet a = old_signal(1);\n";
    assert_eq!(rhai_migrated(defines, &rows, &[]), defines, "the script's own function stays");
}

#[test]
fn a_removed_rhai_name_with_no_replacement_is_a_hint_and_no_edit() {
    let rows = [rhai_row("fn", "retired_fn", None)];
    let found = rhai_edits("retired_fn(1);\n", &rows, &|_| false);
    assert!(found.edits.is_empty());
    assert_eq!(found.hints, ["`retired_fn` is gone in this vike: rewrite what calls it by hand"]);
}

/// The pre-SDK Rhai rows are the renamed FIELDS, read as properties; the old spelling is built from
/// parts (the scripted rename rewrites it wherever it is whole).
#[test]
fn the_pre_sdk_rhai_rows_are_the_renamed_fields_as_hints() {
    let old = concat!("t", "s");
    let source = format!("let t = bar.{old};\n");
    let found = rhai_edits(&source, &builtin_rhai_renames(), &|_| false);
    assert!(found.edits.is_empty(), "{:?}", found.edits);
    assert_eq!(found.hints.len(), 1, "{:?}", found.hints);
    assert!(found.hints[0].starts_with("line 1: `") && found.hints[0].contains("`timestamp`"));
}
