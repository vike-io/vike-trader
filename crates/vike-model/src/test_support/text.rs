//! Reading Rust source as TEXT — what a test that pins its own crate's code against a declared
//! table does when the language offers no reflection to ask instead.

/// The lines of the top-level fn `name` in `text`: everything after its definition line up to the
/// first line that is a lone `}` at column zero, both of those lines excluded, each line returned
/// `\n`-terminated. An absent `name` returns `""`, which is why every caller asserts a floor on
/// what it harvested.
///
/// Only a TOP-LEVEL fn ends where this looks: a method or a nested fn closes at its own indentation,
/// so its harvest runs on to the next column-zero `}`.
pub fn fn_body(text: &str, name: &str) -> String {
    // ⚠ `fn <name>` then `(` OR `<`. A needle carrying the paren matches NOTHING on a GENERIC
    // function, and the harvest then returns an empty body — a gate that has silently gone blind
    // rather than one that fails. `vike_model::runs`'s writer `write_run_with<R>` is one; the first
    // spelling of the reserved-roster test reported `left: []`.
    let needle = format!("fn {name}");
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if !inside {
            // ⚠ The line must BE a definition, not merely mention one. A caller's floor catches a
            // ZERO harvest; it cannot catch a MIS-ANCHORED one, and a source that quotes
            // `fn write_run_with(` inside a comment would otherwise anchor there and sweep a region
            // that happens to contain every name the caller looks for — an assertion that passes
            // while measuring nothing.
            let def = line.trim_start();
            let is_definition = def.starts_with("fn ")
                || def.starts_with("pub fn ")
                || def.starts_with("pub(crate) fn ");
            // ONE condition, not three nested `if`s: clippy's `collapsible_if` refuses the nested
            // spelling at `-D warnings`, which is the merge gate. Edition 2024 let-chains are what
            // make it one expression.
            if is_definition
                && let Some(i) = line.find(&needle)
                && matches!(line[i + needle.len()..].chars().next(), Some('(') | Some('<'))
            {
                inside = true;
            }
            continue;
        }
        if line == "}" {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
