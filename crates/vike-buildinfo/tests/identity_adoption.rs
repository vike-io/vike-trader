//! Every binary that prints a `--version` line states its BUILD IDENTITY — as a gate, not a habit.
//!
//! # Why this is a gate
//!
//! The claim "our binaries say which commit they came from" is worth nothing the first time one of
//! them does not, and that binary is invariably the one being installed on a box at 2am. It is also
//! exactly the shape this repo has been burned by repeatedly — a roster written in prose, true on
//! the day it was written, silently false a month later (`ci_crates`, `release.yml`'s crate list,
//! the settings registry before `crates/vike-ops/tests/settings_registry.rs`). So the roster is
//! DERIVED here: walk the real `crates/` tree, find every `--version` printer, and require each one
//! to go through `vike_buildinfo::version_line` or to be a declared row in [`WITHOUT_IDENTITY`]
//! with its reason.
//!
//! # How a printer is recognised
//!
//! A `println!` under some `crates/**/src/**.rs` whose own ARGUMENTS name the package version.
//! Text-only, comments stripped — the same mechanism `crates/vike-ops/tests/settings_registry.rs`
//! uses, and the only kind of gate that has ever held in this repo. It deliberately does NOT match
//! every mention of `CARGO_PKG_VERSION`: `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` is an
//! MCP protocol field and `crates/vike-run/src/incident.rs`'s `pkg_version` is a report field;
//! neither is a `--version` answer and neither should be forced to carry one.
//!
//! "Names the package version" resolves ONE indirection, crate-wide: a `const`/`static` bound to
//! `env!("CARGO_PKG_VERSION")` counts as the version wherever that name is printed
//! ([`version_aliases`]). A `--version` arm written `const V: &str = env!("CARGO_PKG_VERSION");
//! println!("{NAME} {V}")` is otherwise INVISIBLE to a literal-only sweep, and the
//! `printers.len() >= 5` floor cannot notice one missing printer. This is the same indirection
//! `settings_registry.rs` resolves for its `const *_ENV: &str` rows and for the same reason —
//! crate-wide rather than file-local, because the constant routinely lives in a sibling file.
//!
//! # Declared blind spots
//!
//! One indirection, and text only. A version reached through a FUNCTION (`fn v() -> &'static str`),
//! assembled by `concat!`/`format!` into a name this sweep never sees, or printed by a macro of our
//! own would still slip through. That is stated rather than tolerated silently: closing it needs a
//! real resolver, and the honest position is that this gate catches the shapes a `--version` arm is
//! actually written in today. `crates/vike-ops/tests/settings_registry.rs` takes the same line — a
//! blind spot has to be declared to exist.
//!
//! # Why it lives HERE and not in vike-ops
//!
//! `vike-ops` owns the repo-walking gates, and this one could have gone there. It did not, for one
//! reason: this gate is about THIS crate's ADOPTION, so the crate that would go stale and the gate
//! that catches it stay in the same directory — and vike-ops would have had to grow a dependency on
//! vike-buildinfo to say anything about it.

use std::path::{Path, PathBuf};

/// `--version` printers that do NOT state their build identity, each with the reason.
///
/// A row here is a REAL gap — a binary whose `--version` cannot answer "which commit is this?" —
/// not a false positive being silenced. [`no_stale_exemptions`] fails when a row's file stops
/// existing OR starts using `version_line`, so closing one is a one-line deletion.
const WITHOUT_IDENTITY: &[(&str, &str)] = &[
    (
        "crates/vike-backtest/src/backtest_cli.rs",
        "vike-backtest is a LIBRARY at layer 50 with vike-datahub, vike-run, vike-studio and \
         vike-report above it. A normal dependency on vike-buildinfo would make every commit \
         rebuild the simulator and everything stacked on it, because this crate's build script \
         reruns whenever HEAD moves — the cost lands on the inner loop, not on the one bin that \
         would gain a line. Every crate wired today is top-of-graph, where the cost is a relink.",
    ),
    (
        "crates/vike-backfill/src/cli.rs",
        "vike-backfill is in `xtask/src/ci/tables.rs`'s EXCLUDE_FROM_CI, so NOTHING in CI compiles \
         it and neither `just windows-check` nor `just the build runner` covers it. Wiring it would be \
         one line that no gate on this box or any other could prove compiles. Its bins are also \
         batch tools run by hand, not daemons installed on a trading box — the incident class this \
         crate exists for.",
    ),
];

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the same idiom
/// `crates/vike-ops/tests/layer_gate.rs` and `crates/vike-ops/tests/citation_gate.rs` use.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `.rs` file under `crates/`, minus the two vendored trees the root manifest EXCLUDES from
/// the workspace (`crates/bridges/vike-ibkr/vendor/ibapi`, `crates/bridges/ctrader/protogen`).
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "vendor" || name == "protogen" || name == "target" {
                continue;
            }
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Repo-relative, forward-slashed — so a row in [`WITHOUT_IDENTITY`] reads the same on Windows and
/// on the Linux runners.
fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

/// Drop `//` line comments so a paragraph ABOUT a `--version` printer is not mistaken for one.
/// Crude on purpose: a `//` inside a string literal is not a case this tree contains, and an
/// approximate matcher with a reasoned exemption table beats a precise one nobody maintains.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `println!` invocation's ARGUMENT TEXT, captured by walking the parentheses from the macro
/// name to its match.
///
/// ⚠ It has to be the macro's arguments and NOT the line, and not a character window either.
/// `rustfmt` splits a wrapped call across four lines — the `println!` on one and the
/// `CARGO_PKG_VERSION` three lines down — so a line-based matcher silently sees NOTHING the moment
/// a call grows past 100 columns, which is exactly what happened to the first cut of this gate: it
/// found 3 of 6 printers and the two that mattered were among the missing. A character window would
/// have found them and would also pair a `println!` with an unrelated `CARGO_PKG_VERSION` further
/// down the file — `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` sits in a file whose stdout
/// is a JSON-RPC protocol, i.e. full of `println!`.
///
/// The paren walk is approximate in one declared way: an unbalanced `(` or `)` inside a string
/// literal would mis-terminate the capture. No `--version` arm in this tree has one, and the floor
/// test below is what notices if the walk ever stops finding anything.
fn println_args(code: &str) -> Vec<String> {
    const MACRO: &str = "println!";
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(hit) = code[cursor..].find(MACRO) {
        let after = cursor + hit + MACRO.len();
        cursor = after;
        let Some(open) = code[after..].find('(').map(|i| after + i) else { break };
        // Only whitespace may sit between the macro name and its delimiter; anything else means
        // this was not a call.
        //
        // ⚠ Nothing here looks at what PRECEDES the match, so `eprintln!` — whose name ends in
        // `println!` and whose `(` follows just as immediately — IS captured. Measured, not
        // assumed. That over-match is the safe direction and is left deliberately: a version number
        // written to stderr is still an answer somebody reads off a box, so it gets demanded to
        // carry identity or to become a declared row, never silently exempted.
        if !code[after..open].trim().is_empty() {
            continue;
        }
        let mut depth = 0usize;
        for (i, c) in code[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(code[open..open + i + 1].to_string());
                        cursor = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The `const`/`static` names this text binds to `env!("CARGO_PKG_VERSION")` — the ONE indirection
/// this gate resolves, so a `--version` arm that prints an alias is not invisible.
///
/// Each declaration is read as `const NAME` … up to the terminating `;`, so a `rustfmt`-wrapped
/// binding is found for the same reason [`println_args`] walks parentheses instead of lines.
/// `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` is the shape in the tree today.
fn version_aliases(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for keyword in ["const ", "static "] {
        let mut cursor = 0usize;
        while let Some(hit) = code[cursor..].find(keyword) {
            let after = cursor + hit + keyword.len();
            cursor = after;
            let Some(end) = code[after..].find(';').map(|i| after + i) else { break };
            let declaration = &code[after..end];
            let Some((name, _)) = declaration.split_once(':') else { continue };
            let name = name.trim();
            // A real identifier, and one bound to the version: `const fn`, a `static mut` and a
            // `const` holding anything else all fall out here.
            if declaration.contains("CARGO_PKG_VERSION")
                && !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether one `println!`'s arguments name the package version — literally, or through a crate's
/// own [`version_aliases`].
fn names_version(args: &str, aliases: &[String]) -> bool {
    contains_ident(args, "CARGO_PKG_VERSION")
        || aliases.iter().any(|alias| contains_ident(args, alias))
}

/// `needle` appears in `haystack` as a WHOLE identifier, not as a substring of a longer one.
///
/// ⚠ A plain `contains` is wrong HERE and only here: an alias name comes from the tree, not from
/// this file, and nothing stops a crate from binding the version to `V`. `args.contains("V")` then
/// matches essentially every `println!` in that crate, and a gate that reports forty printers where
/// there is one gets switched off rather than read. The literal `CARGO_PKG_VERSION` goes through
/// the same check for consistency; its own delimiters are quotes, which are not identifier bytes.
fn contains_ident(haystack: &str, needle: &str) -> bool {
    let ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(start, _)| {
        let end = start + needle.len();
        (start == 0 || !ident_byte(bytes[start - 1]))
            && (end >= bytes.len() || !ident_byte(bytes[end]))
    })
}

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
fn version_printers() -> Vec<(String, bool)> {
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
            out.push((relative, calls.iter().all(|args| args.contains("version_line"))));
        }
    }
    out.sort();
    out
}

#[test]
fn every_version_printer_states_its_build_identity() {
    let bare: Vec<String> = version_printers()
        .into_iter()
        .filter(|(_, uses_identity)| !uses_identity)
        .map(|(file, _)| file)
        .filter(|file| !WITHOUT_IDENTITY.iter().any(|(f, _)| f == file))
        .collect();

    assert!(
        bare.is_empty(),
        "these binaries answer `--version` with a bare name+version and cannot say which commit \
         they were built from:\n{}\n\n\
         A release binary was once built from a bare repo four commits behind `main` and nearly \
         installed on the live recorder; `crates/vike-buildinfo/src/lib.rs` carries that incident. \
         Two responses, in order of preference:\n  \
         1. print `vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\"))` \
         — the name and version stay the first two tokens, so nothing that reads them positionally \
         breaks;\n  \
         2. add a row to WITHOUT_IDENTITY in this file, with the reason it cannot.",
        bare.join("\n  ")
    );
}

#[test]
fn no_stale_exemptions() {
    let root = repo_root();
    let printers = version_printers();
    let mut stale = Vec::new();
    for (file, why) in WITHOUT_IDENTITY {
        if !root.join(file).exists() {
            stale.push(format!("  {file} — no such file; delete the row ({why})"));
            continue;
        }
        match printers.iter().find(|(f, _)| f == file) {
            None => {
                stale.push(format!("  {file} — no longer prints a --version line; delete the row"))
            }
            Some((_, true)) => {
                stale.push(format!("  {file} — now uses version_line; delete the row"))
            }
            Some((_, false)) => {}
        }
    }
    assert!(
        stale.is_empty(),
        "stale WITHOUT_IDENTITY rows — an exemption that has stopped being real is a lie the next \
         reader believes:\n{}",
        stale.join("\n")
    );
}

/// A floor, not a count. This gate is textual, and a walker that quietly stopped matching anything
/// would pass both assertions above by seeing an empty tree. The number is below today's and exists
/// only to make "sees nothing" fail loudly — the first cut of this matcher was line-based, went
/// blind the moment `rustfmt` wrapped a call across four lines, and reported 3 printers where there
/// are 6. That is precisely the failure this floor catches.
#[test]
fn the_gate_actually_sees_the_tree() {
    let printers = version_printers();
    assert!(
        printers.len() >= 5,
        "only {} --version printers found — the walker or the matcher is broken: {printers:?}",
        printers.len()
    );
    assert!(
        printers.iter().filter(|(_, uses_identity)| *uses_identity).count() >= 3,
        "no printer states its identity — `version_line` detection is broken: {printers:?}"
    );
    assert!(
        printers.iter().any(|(f, _)| f == "crates/vike-cli/src/lib.rs"),
        "vike-cli's dispatcher must be seen as a --version printer: {printers:?}"
    );
}

/// The const indirection is RESOLVED, and the proof is that the rule this gate shipped with does
/// not resolve it. Both halves are asserted on one input, so the day somebody simplifies
/// [`names_version`] back to a literal sweep this fails instead of going quietly blind.
#[test]
fn a_version_printed_through_a_const_is_not_invisible() {
    let code =
        "const V: &str = env!(\"CARGO_PKG_VERSION\");\nfn main() { println!(\"{} {}\", NAME, V); }";
    let stripped = strip_comments(code);
    let aliases = version_aliases(&stripped);
    assert_eq!(aliases, vec!["V".to_string()], "the const binding was not harvested");

    let calls = println_args(&stripped);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(names_version(&calls[0], &aliases), "the alias did not resolve: {calls:?}");
    // …and the literal-only rule that shipped first sees nothing here. This is the blind spot.
    assert!(!calls[0].contains("CARGO_PKG_VERSION"), "the fixture stopped being indirect");
}

/// The alias harvest fires on the REAL tree, not only on a fixture: `vike-cli` binds the version to
/// a `const` today. Without a live sighting the resolution above could be dead code and every
/// assertion in this file would still pass.
#[test]
fn the_alias_harvest_finds_the_real_one_in_this_tree() {
    let path = repo_root().join("crates/vike-cli/src/cmd/mcp.rs");
    let text = std::fs::read_to_string(&path).expect("crates/vike-cli/src/cmd/mcp.rs");
    let aliases = version_aliases(&strip_comments(&text));
    assert!(
        aliases.iter().any(|a| a == "SERVER_VERSION"),
        "vike-cli's SERVER_VERSION is the tree's one const-routed version and was not harvested: \
         {aliases:?}"
    );
    // ⚠ …and harvesting it must not INVENT a printer. `SERVER_VERSION` is an MCP `serverInfo`
    // field, never a `--version` answer, so no `println!` in that crate names it and vike-cli's
    // roster row stays the dispatcher's real one.
    assert!(
        !version_printers().iter().any(|(f, _)| f == "crates/vike-cli/src/cmd/mcp.rs"),
        "an MCP protocol field was mistaken for a --version arm"
    );
}

/// A `static` binding is harvested too, and a `const` holding something else is not — the two
/// directions of [`version_aliases`], neither of which the tree exercises today.
#[test]
fn the_alias_harvest_takes_statics_and_leaves_unrelated_constants_alone() {
    let code = "static S: &str = env!(\"CARGO_PKG_VERSION\");\nconst ADDR: &str = \"127.0.0.1\";\n";
    assert_eq!(version_aliases(code), vec!["S".to_string()]);
}

/// A SHORT alias does not smear across the crate. Nothing stops a `--version` arm from binding the
/// version to `V`, and a substring match on one letter would turn every `println!` in that crate
/// into a printer — a gate that over-reports by two orders of magnitude gets disabled, not read.
#[test]
fn a_one_letter_alias_matches_the_identifier_and_not_every_word_containing_it() {
    let aliases = vec!["V".to_string()];
    assert!(names_version("(\"{} {}\", NAME, V)", &aliases), "the real use must still match");
    for unrelated in ["(\"{}\", VENUE)", "(\"{}\", cfg.Verbose)", "(\"starting V2 feed\")"] {
        assert!(!names_version(unrelated, &aliases), "smeared onto {unrelated}");
    }
}
