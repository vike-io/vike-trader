//! Fixture self-tests of the scanner and the NAME reader: each pins a shape the tree gates rely on.

use super::printers::{OWN_PACKAGE_NAME, version_line_names, version_printers};
use super::scan::{names_version, println_args, repo_root, strip_comments, version_aliases};

/// What [`version_line_names`] reads as the NAME, on the shapes a printer is written in.
#[test]
fn the_name_argument_is_read_whole_and_wrapped() {
    let one = "(\"{}\", vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\")))";
    assert_eq!(version_line_names(one), [OWN_PACKAGE_NAME]);
    let wrapped = "(\n  \"{}\",\n  version_line(\n    env!( \"CARGO_PKG_NAME\" ),\n    env!(\"CARGO_PKG_VERSION\"),\n  )\n)";
    assert_eq!(version_line_names(wrapped), [OWN_PACKAGE_NAME]);
    assert_eq!(version_line_names("(\"{}\", version_line(\"vike\", V))"), ["\"vike\""]);
    assert_eq!(version_line_names("(\"{}\", version_line(name, V))"), ["name"]);
    assert!(version_line_names("(\"{}\", my_version_line(x, V))").is_empty());
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
