//! Unit tests for the import resolver (`imports.rs`).

use super::*;

/// An IMPORTED bare read is the same read, and used to match nothing at all.
///
/// ⚠ The pattern list was exactly the two path-qualified spellings, so `use std::env::var;`
/// followed by `var("VIKE_X")` — one line of import away from the form every gate here keys —
/// was invisible: no row demanded, no allowlist entry, no sighting. Every import shape is
/// covered because they are all one keystroke apart.
#[test]
fn an_imported_bare_read_is_still_a_read() {
    let bare = "use std::env::var;\nfn a() { var(\"VIKE_BARE\"); }\n";
    assert_eq!(
        find_env_reads(bare).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_BARE\""]
    );
    let grouped = "use std::env::{var, var_os};\nfn a() { var_os(\"VIKE_GROUPED\"); }\n";
    assert_eq!(
        find_env_reads(grouped).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_GROUPED\""]
    );
    let renamed = "use std::env::var as getenv;\nfn a() { getenv(\"VIKE_ALIAS\"); }\n";
    assert_eq!(
        find_env_reads(renamed).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_ALIAS\""]
    );
    let mod_alias = "use std::env as e;\nfn a() { e::var(\"VIKE_MODALIAS\"); }\n";
    assert_eq!(
        find_env_reads(mod_alias).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_MODALIAS\""]
    );

    // …and ONE row per call site: with the bare name imported, `var(` also matches the tail of
    // `env::var(`, so the qualified read below must not be reported twice.
    let both = "use std::env::var;\nfn a() { std::env::var(\"VIKE_ONCE\"); }\n";
    assert_eq!(find_env_reads(both).len(), 1, "{:?}", find_env_reads(both));
}

/// The general resolver, driven at every `use` shape a caller can meet.
///
/// The names here are invented (`Widget`, `w`) for the reason [`find_calls`]' doc gives: this
/// file is walked by the gate that calls it, and a real type spelled here as fixture DATA would
/// come back as a `vike-ops` sighting.
#[test]
fn an_imported_type_is_found_under_whatever_name_the_file_gave_it() {
    let of = |src: &str| imported_spellings(src, "Widget");

    assert_eq!(of("use krate::Widget;\n"), vec!["Widget".to_string()]);
    assert_eq!(of("use krate::Widget as W;\n"), vec!["W".to_string()]);
    assert_eq!(of("use krate::{Widget, Other};\n"), vec!["Widget".to_string()]);
    assert_eq!(of("use krate::{Other, Widget as W};\n"), vec!["W".to_string()]);
    assert_eq!(of("pub use krate::Widget as W;\n"), vec!["W".to_string()]);
    // A MODULE alias renames the path, so the item keeps its name behind the new prefix.
    assert_eq!(of("use krate as k;\n"), vec!["k::Widget".to_string()]);
    // rustfmt wraps a long group across lines; the statement is read whole, not per line.
    assert_eq!(of("use krate::{\n    Other,\n    Widget as W,\n};\n"), vec!["W".to_string()]);

    // Nothing is invented from a file that imports nothing of the sort…
    assert!(of("use std::collections::BTreeMap;\nfn f() { let _ = Widget::new(); }\n").is_empty());
    // …and `misuse ` is not a declaration.
    assert!(of("fn misuse Widget;\n").is_empty());

    // The DECLARED residual: a glob brings the name in without spelling it, so there is nothing
    // to harvest — the same refusal `imported_env_read_patterns` makes, and for the same
    // reason. Invert this assertion rather than deleting it if globs are ever resolved.
    assert!(of("use krate::*;\n").is_empty());
}

/// The false positives the import requirement exists to avoid, and the residual it accepts.
///
/// ⚠ The first fixture is `crates/bridges/vike-ibkr/src/config.rs`'s
/// `load_ibkr_config_for_account` in miniature: a local closure NAMED `var`, reading a
/// caller-supplied map — the shape this registry calls correct. Searching for a bare `var(`
/// unconditionally reports eight of these in that one file and, because the inner call's
/// argument is a parameter, would demand a `DYNAMIC_ALLOWLIST` row for a file that reads no
/// environment at all.
#[test]
fn an_unimported_bare_name_is_somebody_elses_function() {
    let local = "fn f(vars: &Map) {\n    let var = |s: &str| vars.get(s);\n    \
                     var(\"ACCOUNT\");\n    var(\"BACKEND\");\n}\n";
    assert!(find_env_reads(local).is_empty(), "{:?}", find_env_reads(local));

    // A definition is not a call, and a method is not a free function — both matter only once
    // the bare name is in play, because a `::` path can be neither.
    let shadowed = "use std::env::var;\nfn var(s: &str) -> Option<String> { None }\n\
                        fn f(c: &C) { c.var(\"VIKE_METHOD\"); }\n";
    assert!(find_env_reads(shadowed).is_empty(), "{:?}", find_env_reads(shadowed));

    // The DECLARED residual: a glob import brings the name in without naming it, so there is
    // nothing to harvest. Handled by NOT falling back to an unconditional bare search — that
    // trades this miss for the eight false demands above. If somebody teaches the scanner to
    // resolve globs, invert this assertion rather than deleting it.
    let globbed = "use std::env::*;\nfn a() { var(\"VIKE_GLOB\"); }\n";
    assert!(find_env_reads(globbed).is_empty(), "{:?}", find_env_reads(globbed));

    // …and the positive control, so a scanner that had stopped matching entirely could not
    // pass every assertion above by answering "nothing" to everything.
    let real = "fn a() { std::env::var(\"VIKE_CONTROL\"); }\n";
    assert_eq!(find_env_reads(real).len(), 1);
}
