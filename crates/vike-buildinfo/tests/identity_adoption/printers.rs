//! The `--version` printer roster the tree gates judge, and the reader of each printed NAME.

use super::scan::{
    ident_byte, names_version, println_args, rel, repo_root, rust_files, strip_comments,
    version_aliases,
};

/// Every `crates/**/src/**.rs` file that answers `--version`, and whether it states its build
/// identity while doing so.
///
/// A file counts when some `println!` names the package version in its own arguments. It STATES its
/// identity when EVERY such call also names `version_line` — per call, not per file, so a crate
/// that converted one arm and left another bare is still reported.
///
/// Two passes, because the alias set is CRATE-wide: the constant a `--version` arm prints routinely
/// sits in a sibling file. The crate key is the path above `/src/`, which is also what makes
/// `crates/bridges/<venue>` one crate rather than a `bridges` heap.
pub(super) fn version_printers() -> Vec<(String, bool)> {
    version_printer_calls()
        .into_iter()
        .map(|(file, calls)| {
            let states_identity = calls.iter().all(|args| args.contains("version_line"));
            (file, states_identity)
        })
        .collect()
}

/// [`version_printers`]' files, each with the ARGUMENT TEXT of its `--version` `println!` calls.
pub(super) fn version_printer_calls() -> Vec<(String, Vec<String>)> {
    let root = repo_root();
    let mut paths = Vec::new();
    rust_files(&root.join("crates"), &mut paths);

    let mut files: Vec<(String, String, String)> = Vec::new();
    let mut aliases: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for path in paths {
        let relative = rel(&path, &root);
        let Some((krate, _)) = relative.split_once("/src/") else { continue };
        let krate = krate.to_string();
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let text = strip_comments(&text);
        aliases.entry(krate.clone()).or_default().extend(version_aliases(&text));
        files.push((relative, krate, text));
    }

    let mut out = Vec::new();
    for (relative, krate, text) in files {
        let crate_aliases = aliases.get(&krate).cloned().unwrap_or_default();
        let calls: Vec<String> = println_args(&text)
            .into_iter()
            .filter(|args| names_version(args, &crate_aliases))
            .collect();
        if !calls.is_empty() {
            out.push((relative, calls));
        }
    }
    out.sort();
    out
}

/// The NAME argument a `--version` line is rendered with: what `env!("CARGO_PKG_NAME")` must be.
pub(super) const OWN_PACKAGE_NAME: &str = "env!(\"CARGO_PKG_NAME\")";

/// The first argument of every `version_line(` call in `args`, whitespace removed. Approximate the
/// way [`println_args`] is: a `,` or an unbalanced `)` inside a string literal would cut it short.
pub(super) fn version_line_names(args: &str) -> Vec<String> {
    const CALL: &str = "version_line(";
    let mut out = Vec::new();
    for (at, _) in args.match_indices(CALL) {
        if at > 0 && ident_byte(args.as_bytes()[at - 1]) {
            continue; // `my_version_line(` is another function
        }
        let mut depth = 0usize;
        let mut first = String::new();
        for c in args[at + CALL.len()..].chars() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => break,
                ')' => depth -= 1,
                ',' if depth == 0 => break,
                _ => {}
            }
            first.push(c);
        }
        out.push(first.chars().filter(|c| !c.is_whitespace()).collect());
    }
    out
}
