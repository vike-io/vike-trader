use super::*;
use crate::Indicator;
use crate::test_support::{Scratch, bar};

const GOOD: &str = "fn on_bar(bar) { bar.close }";

#[test]
fn loads_a_flat_directory_and_the_prototype_actually_computes() {
    let s = Scratch::new("flat");
    s.write("passthrough.rhai", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.is_clean(), "{:?}", r.diagnostics);
    assert_eq!(r.indicators.len(), 1);
    assert_eq!(r.indicators[0].name, "passthrough");

    let mut proto = r.prototypes().remove(0);
    assert_eq!(proto.on_bar(&bar(7.5))[0], 7.5);
}

/// An absent directory is the ordinary unconfigured state — see `load_user_indicators`' doc.
#[test]
fn an_absent_directory_is_a_clean_empty_report_not_an_error() {
    let s = Scratch::new("absent");
    let r = load_user_indicators(&s.path().join("nope"));
    assert!(r.is_clean() && r.indicators.is_empty());
}

#[test]
fn an_empty_directory_is_also_clean() {
    let s = Scratch::new("empty");
    let r = load_user_indicators(s.path());
    assert!(r.is_clean() && r.indicators.is_empty());
}

#[test]
fn a_compile_error_is_reported_against_its_file_and_the_rest_still_load() {
    let s = Scratch::new("broken");
    s.write("fine.rhai", GOOD);
    s.write("broken.rhai", "fn on_bar(bar) { this. }");
    let r = load_user_indicators(s.path());
    assert_eq!(r.indicators.len(), 1, "the good one still loads");
    assert_eq!(r.indicators[0].name, "fine");
    assert!(matches!(r.diagnostics[0], IndicatorDiagnostic::CompileFailed { .. }));
    assert!(r.diagnostics[0].message().contains("broken.rhai"));
}

/// The most likely real mistake: a strategy hook copied into an indicator file.
#[test]
fn a_file_with_no_on_bar_is_a_compile_error_not_a_silent_skip() {
    let s = Scratch::new("noonbar");
    s.write("empty_thing.rhai", "fn init() { #{} }");
    let r = load_user_indicators(s.path());
    assert!(r.indicators.is_empty());
    assert!(r.diagnostics[0].message().contains("on_bar"), "{:?}", r.diagnostics[0]);
}

#[test]
fn a_builtin_name_is_refused_by_name_before_its_body_is_even_compiled() {
    let s = Scratch::new("shadow");
    // Deliberately UNCOMPILABLE: if the conflict check ran second, the reported diagnostic
    // would be the compile error and would send the author to fix the wrong thing.
    s.write("sma.rhai", "this is not rhai at all (");
    let r = load_user_indicators(s.path());
    assert!(r.indicators.is_empty());
    match &r.diagnostics[0] {
        IndicatorDiagnostic::NameConflict { name, reason, .. } => {
            assert_eq!(name, "sma");
            assert!(reason.contains("built-in indicator"), "{reason}");
        }
        d => panic!("expected a NameConflict, got {d:?}"),
    }
}

#[test]
fn a_host_verb_name_is_refused_too() {
    let s = Scratch::new("verb");
    s.write("close.rhai", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.indicators.is_empty());
    assert!(r.diagnostics[0].message().contains("host function"), "{:?}", r.diagnostics[0]);
}

/// `var` is a rhai reserved word, so a file named for it could never be CALLED — the refusal
/// has to happen here, since nothing downstream could report it.
#[test]
fn a_name_rhai_cannot_parse_is_refused_at_load() {
    let s = Scratch::new("reserved");
    s.write("var.rhai", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.indicators.is_empty());
    assert!(!r.is_clean());
}

/// Flat means flat — but the file must be MENTIONED, or the folder silently does nothing.
#[test]
fn a_rhai_file_in_a_subdirectory_is_reported_rather_than_silently_ignored() {
    let s = Scratch::new("nested");
    s.write("mine/deep.rhai", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.indicators.is_empty(), "a nested file is NOT loaded");
    match &r.diagnostics[0] {
        IndicatorDiagnostic::InSubdirectory { path } => {
            assert!(path.ends_with("deep.rhai"));
        }
        d => panic!("expected InSubdirectory, got {d:?}"),
    }
    assert!(r.diagnostics[0].message().contains("FLAT"));
}

#[test]
fn a_non_rhai_file_is_ignored_without_a_diagnostic() {
    let s = Scratch::new("readme");
    s.write("README.md", "notes");
    s.write("thing.rhai", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.is_clean());
    assert_eq!(r.indicators.len(), 1);
}

#[test]
fn the_extension_is_matched_case_insensitively() {
    let s = Scratch::new("case");
    s.write("Shouty.RHAI", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.is_clean(), "{:?}", r.diagnostics);
    assert_eq!(r.indicators[0].name, "Shouty");
}

/// Order must not decide which of two colliding files wins, so NEITHER does.
#[test]
fn two_files_claiming_one_name_load_neither_and_say_so() {
    let s = Scratch::new("dupe");
    s.write("dup.rhai", GOOD);
    let second = s.write("dup.RHAI", GOOD);
    // A case-insensitive filesystem (Windows/macOS) collapses these into ONE file, so the
    // collision cannot be constructed there — skip rather than assert a platform's behaviour.
    if std::fs::read_dir(s.path()).unwrap().filter_map(Result::ok).count() < 2 {
        assert!(second.exists());
        return;
    }
    let r = load_user_indicators(s.path());
    assert!(r.indicators.is_empty(), "neither is loaded");
    match &r.diagnostics[0] {
        IndicatorDiagnostic::DuplicateName { name, paths } => {
            assert_eq!(name, "dup");
            assert_eq!(paths.len(), 2);
        }
        d => panic!("expected DuplicateName, got {d:?}"),
    }
}

/// `read_dir` order is unspecified; a report that reshuffles between machines cannot be diffed.
#[test]
fn indicators_come_back_sorted_by_name() {
    let s = Scratch::new("sorted");
    for n in ["zulu", "alpha", "mike"] {
        s.write(&format!("{n}.rhai"), GOOD);
    }
    let r = load_user_indicators(s.path());
    let names: Vec<&str> = r.indicators.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "mike", "zulu"]);
}

/// A `fn outputs()` declaration survives the LOAD path, so the prototype the loader hands on
/// carries the lines the accessors are generated from. Without this the declaration would be
/// provable only through `compile_indicator` directly, which no binary calls.
#[test]
fn a_declared_output_list_reaches_the_loaded_prototype() {
    let s = Scratch::new("outputs");
    s.write(
        "my_bands.rhai",
        "fn outputs() { [\"upper\", \"mid\", \"lower\"] } \
             fn on_bar(bar) { [bar.close + 1.0, bar.close, bar.close - 1.0] }",
    );
    s.write("plain.rhai", GOOD);
    let r = load_user_indicators(s.path());
    assert!(r.is_clean(), "{:?}", r.diagnostics);
    let protos = r.prototypes();
    // Sorted by name: my_bands, plain.
    assert_eq!(protos[0].outputs(), ["upper".to_string(), "mid".into(), "lower".into()]);
    assert_eq!(
        crate::user_line_accessors(&protos[0]).into_iter().map(|(_, f)| f).collect::<Vec<_>>(),
        vec!["my_bands_upper", "my_bands_mid", "my_bands_lower"]
    );
    // ...and the file next to it, which declares nothing, is untouched: one line, no accessors.
    assert_eq!(protos[1].outputs(), ["plain".to_string()]);
    assert!(crate::user_line_accessors(&protos[1]).is_empty());
}

/// A bad `outputs()` is a COMPILE diagnostic against its own file, so the author is sent to the
/// line to rename rather than to a function-not-found in some strategy.
///
/// ⚠ The file name and the line are DERIVED — `crates/vike-script/src/engine/mod.rs`'s
/// `registry_name_a_user_line_could_spell` picks a registry name containing a `_` whose stem is
/// itself a free indicator name, so the loader reaches `compile_indicator` instead of refusing
/// the file for its own NAME first. Hand-writing the pair is how this test came to claim that
/// `pos.rhai` declaring a line `ition` shadows the host read `position`: `line_fn_name` joins
/// with `_`, so it spells `pos_ition`, the file loaded clean, and the test failed.
#[test]
fn a_bad_output_declaration_is_reported_against_its_file() {
    let (stem, line) = crate::engine::registry_name_a_user_line_could_spell();
    let taken = crate::line_fn_name(stem, line);
    let s = Scratch::new("badoutputs");
    s.write("fine.rhai", GOOD);
    s.write(
        &format!("{stem}.rhai"),
        &format!("fn outputs() {{ [\"{line}\", \"x\"] }} fn on_bar(bar) {{ [1.0, 2.0] }}"),
    );
    let r = load_user_indicators(s.path());
    assert_eq!(r.indicators.len(), 1, "the good one still loads: {:?}", r.indicators);
    assert_eq!(r.indicators[0].name, "fine");
    let msg = r.diagnostics[0].message();
    assert!(msg.contains(&format!("{stem}.rhai")), "{msg}");
    assert!(msg.contains(&taken), "the message names the accessor it would shadow: {msg}");
}

/// Two prototypes from one load must not share state — they are separate indicators.
#[test]
fn two_loaded_indicators_stream_independently() {
    let s = Scratch::new("indep");
    s.write("count_a.rhai", "fn init() { #{ n: 0 } } fn on_bar(bar) { this.n += 1; this.n }");
    s.write("count_b.rhai", "fn init() { #{ n: 0 } } fn on_bar(bar) { this.n += 10; this.n }");
    let r = load_user_indicators(s.path());
    let mut protos = r.prototypes();
    assert_eq!(protos.len(), 2);
    for _ in 0..3 {
        protos[0].on_bar(&bar(1.0));
    }
    protos[1].on_bar(&bar(1.0));
    assert_eq!(protos[0].value()[0], 3.0);
    assert_eq!(protos[1].value()[0], 10.0);
}
