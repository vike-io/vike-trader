//! `vike_model::host_build::error_lines` on canned builder output: the text a failed build answers
//! with (cargo's human diagnostics, the hints after them), mapped onto the strategy file's lines.

use vike_model::host_build::error_lines::{BuildErrorLine, build_error_lines};

/// A file whose entry is declared on line 4.
const SOURCE: &str = "//! ```toml\n//! [strategy]\n//! ```\npub fn rule(params: &Params) -> Box<dyn Rule + Send> {\n    todo!()\n}\n";

/// What a failed build of [`SOURCE`] answers: two errors in the file, a warning, a note with its
/// own location, an error in the template, cargo's closing lines and a builder hint.
const DIAGNOSTICS: &str = "\
warning: unused import: `std::fmt`
 --> src/strategy.rs:2:5
  |
2 | use std::fmt;
  |     ^^^^^^^^
error[E0425]: cannot find value `rungz` in this scope
  --> src/strategy.rs:41:9
   |
41 |         rungz + 1
   |         ^^^^^ help: a local variable with a similar name exists: `rungs`
error[E0308]: mismatched types
 --> src/strategy.rs:7:20
  |
7 |     let x: u32 = 1.5;
  |                  ^^^ expected `u32`, found floating-point number
note: the function is defined here
 --> src/strategy.rs:3:4
error[E0061]: this function takes 2 arguments but 1 argument was supplied
  --> src/lib.rs:12:48
   |
12 | strategy::rule(&params)
error: could not compile `strategy_file` (lib) due to 3 previous errors; 1 warning emitted
hint: `X` is now `Y`
";

#[test]
fn each_error_lands_on_its_line_of_the_file_and_nothing_else_does() {
    let line = |line: usize, column: usize, message: &str| BuildErrorLine {
        line,
        column,
        message: message.to_string(),
    };
    assert_eq!(
        build_error_lines(DIAGNOSTICS, SOURCE),
        [
            line(41, 9, "error[E0425]: cannot find value `rungz` in this scope"),
            line(7, 20, "error[E0308]: mismatched types"),
            // The template's error sits on the line that declares the entry it calls.
            line(4, 1, "error[E0061]: this function takes 2 arguments but 1 argument was supplied"),
        ]
    );
}

#[test]
fn a_build_that_failed_outside_the_file_lists_no_line() {
    let elsewhere = "error: failed to run custom build command for `x`\nerror[E0432]: unresolved import\n --> /cargo/registry/x/src/lib.rs:1:5\n";
    assert!(build_error_lines(elsewhere, SOURCE).is_empty());
    assert!(build_error_lines("", SOURCE).is_empty());
}
