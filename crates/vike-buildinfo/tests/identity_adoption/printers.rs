//! The `--version` printer roster the tree gates judge, and the reader of each printed NAME.

use super::scan::{
    ident_byte, names_version, println_args, program_version_texts, rel, repo_root, rust_files,
    strip_comments, version_aliases,
};

/// Every `crates/**/src/**.rs` file that answers `--version`, and whether it states its build
/// identity while doing so.
///
/// A file counts when it has a printer of either shape (see [`version_printer_calls`]). It STATES
/// its identity when EVERY such printer also names `version_line` — per call, not per file, so a
/// crate that converted one arm and left another bare is still reported.
///
/// Two passes, because the alias set and the function table are CRATE-wide: the constant a
/// `--version` arm prints, and the function a `Program::new` takes its version line from, routinely
/// sit in a sibling file. The crate key is the path above `/src/`, which is also what makes
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

/// [`version_printers`]' files, each with the text of its `--version` printers. There are two
/// shapes, and a file's list holds one entry per printer of either:
///
///   * a `println!` whose own ARGUMENTS name the package version — the argument text;
///   * a `Program::new(<bin>, <version>)`, `vike_util::cli::Program` being what prints `-V` for
///     every program that reads its command line with `clap` (decision 0155; `CliExit::emit` prints
///     the version argument verbatim) — the version argument RESOLVED through the `let` it names and
///     the function it calls ([`program_version_texts`]), counted when that text names the package
///     version or `version_line`.
pub(super) fn version_printer_calls() -> Vec<(String, Vec<String>)> {
    let root = repo_root();
    let mut paths = Vec::new();
    rust_files(&root.join("crates"), &mut paths);

    let mut files: Vec<(String, String, String)> = Vec::new();
    let mut aliases: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut siblings: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for path in paths {
        let relative = rel(&path, &root);
        let Some((krate, _)) = relative.split_once("/src/") else { continue };
        let krate = krate.to_string();
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let text = strip_comments(&text);
        aliases.entry(krate.clone()).or_default().extend(version_aliases(&text));
        siblings.entry(krate.clone()).or_default().push(text.clone());
        files.push((relative, krate, text));
    }

    let mut out = Vec::new();
    for (relative, krate, text) in files {
        let crate_aliases = aliases.get(&krate).cloned().unwrap_or_default();
        let crate_files = siblings.get(&krate).map(Vec::as_slice).unwrap_or_default();
        let mut calls: Vec<String> = println_args(&text)
            .into_iter()
            .filter(|args| names_version(args, &crate_aliases))
            .collect();
        calls.extend(
            program_version_texts(&text, crate_files)
                .into_iter()
                .filter(|resolved| is_version_text(resolved, &crate_aliases)),
        );
        if !calls.is_empty() {
            out.push((relative, calls));
        }
    }
    out.sort();
    out
}

/// Whether a RESOLVED `Program::new` version argument is a `--version` answer: it names the package
/// version (literally or through a crate's [`version_aliases`]) or calls `version_line`. A literal
/// (`""`, which `--help` rendering passes) or an unresolved parameter is neither.
pub(super) fn is_version_text(resolved: &str, aliases: &[String]) -> bool {
    names_version(resolved, aliases) || resolved.contains("version_line(")
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
