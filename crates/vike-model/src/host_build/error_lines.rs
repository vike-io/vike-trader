//! Where a failed build's errors point in the strategy FILE itself, for a list a user can click.
//!
//! The builder answers a failed build with cargo's own HUMAN text, verbatim, the hints after it
//! (`crates/vike-strategy-builder/src/render.rs`'s `compile_diagnostics`; it passes no
//! `--message-format`, so there is no JSON to read), and it writes the file as the scratch crate's
//! `src/strategy.rs` byte for byte, no prefix (`render.rs`'s `build_strategy_file`). So rustc's
//! ` --> src/strategy.rs:L:C` under an error IS line L, column C of the file. An error in
//! `src/lib.rs` is the template's, which calls the file's entry (a `build` or `rule` whose
//! signature does not fit): it is placed on the line that declares that entry. Any other location
//! (a dependency's source) is no line of the file and is not listed; the whole text still is.
//!
//! Only an error's PRIMARY location is taken (the first `-->` under its headline); a warning, a
//! `note:` and cargo's closing `could not compile` line are not errors of a line.
//!
//! Pure: no file, no environment and no clock.

/// The scratch crate's copy of the strategy file (`render.rs`'s `build_strategy_file`).
const STRATEGY_FILE: &str = "src/strategy.rs";

/// The template's half, which calls the file's entry.
const TEMPLATE_FILE: &str = "src/lib.rs";

/// One error of a failed build, on its line of the strategy file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildErrorLine {
    /// 1-based, as rustc and the editor's gutter count.
    pub line: usize,
    /// 1-based, in characters as rustc counts them.
    pub column: usize,
    /// rustc's headline, code included (`error[E0425]: cannot find value `x` in this scope`).
    pub message: String,
}

/// Every error of `diagnostics` (a failed build's text) placed on a line of `source` (the file
/// that was built), in the order rustc printed them.
pub fn build_error_lines(diagnostics: &str, source: &str) -> Vec<BuildErrorLine> {
    let mut found = Vec::new();
    let mut headline: Option<&str> = None;
    for text in diagnostics.lines() {
        if text.starts_with("error") {
            headline = Some(text.trim_end());
            continue;
        }
        if !text.starts_with(' ') {
            // A warning, a note, a hint: whatever `-->` follows is not an error's.
            headline = None;
            continue;
        }
        let Some(location) = text.trim_start().strip_prefix("--> ") else { continue };
        let Some(message) = headline.take() else { continue };
        let mut parts = location.trim_end().rsplitn(3, ':');
        let (column, line, path) = (parts.next(), parts.next(), parts.next());
        let (Some(column), Some(line)) =
            (column.and_then(|c| c.parse().ok()), line.and_then(|l| l.parse().ok()))
        else {
            continue;
        };
        let (line, column) = match path {
            Some(STRATEGY_FILE) => (line, column),
            Some(TEMPLATE_FILE) => (entry_line(source), 1),
            _ => continue,
        };
        found.push(BuildErrorLine { line, column, message: message.to_string() });
    }
    found
}

/// The 1-based line declaring the file's entry, else 1: a line that STARTS with `pub fn build` or
/// `pub fn rule` then `(` or `<`, the builder's own reading of an export (`render.rs`'s `Entry`).
fn entry_line(source: &str) -> usize {
    let declares = |text: &str| {
        text.strip_prefix("pub fn ").is_some_and(|rest| {
            ["build", "rule"]
                .iter()
                .any(|name| rest.strip_prefix(name).is_some_and(|t| t.starts_with(['<', '('])))
        })
    };
    source.lines().position(declares).map_or(1, |i| i + 1)
}
