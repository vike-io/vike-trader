//! Fixture self-tests of the scanner and the NAME reader: each pins a shape the tree gates rely on.

use super::printers::{
    OWN_PACKAGE_NAME, is_version_text, version_line_names, version_printer_calls, version_printers,
};
use super::scan::{
    names_version, println_args, program_version_texts, repo_root, strip_comments, version_aliases,
};

/// The `Program::new` version texts `code` resolves to, as a printer of its own crate sees them.
fn program_texts(code: &str) -> Vec<String> {
    let code = strip_comments(code);
    program_version_texts(&code, std::slice::from_ref(&code))
}

/// The binding shape every group of the migration wrote: the version line bound with `let`, then
/// handed to `Program::new` by reference. The resolved text carries the `version_line` call (so the
/// printer states its identity) and the NAME it was given (so the name check can read it).
#[test]
fn a_program_new_through_a_let_binding_is_a_printer_with_a_readable_name() {
    let code = "fn read(argv: &[String]) {\n    let version = vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\"));\n    let program = Program::new(BIN, &version);\n}";
    let texts = program_texts(code);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(is_version_text(&texts[0], &[]), "not seen as a printer: {texts:?}");
    assert!(texts[0].contains("version_line"), "identity not seen: {texts:?}");
    assert_eq!(version_line_names(&texts[0]), [OWN_PACKAGE_NAME]);
}

/// The proof the name check bites on THIS shape: a literal name in the bound `version_line` is
/// read as that literal, which is not the package's own, so
/// `every_version_printer_prints_its_own_package_name` fails it (`name != OWN_PACKAGE_NAME`).
#[test]
fn a_wrong_name_in_a_program_new_binding_is_read_as_wrong() {
    let literal = "let v = vike_buildinfo::version_line(\"vike\", env!(\"CARGO_PKG_VERSION\"));\nlet p = Program::new(BIN, &v);";
    let texts = program_texts(literal);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(is_version_text(&texts[0], &[]), "a wrong-named printer must still be SEEN: {texts:?}");
    let names = version_line_names(&texts[0]);
    assert_eq!(names, ["\"vike\""]);
    assert!(names.iter().all(|n| n != OWN_PACKAGE_NAME));

    let constant = "let v = vike_buildinfo::version_line(NAME, env!(\"CARGO_PKG_VERSION\"));\nlet p = Program::new(BIN, &v);";
    let names = version_line_names(&program_texts(constant)[0]);
    assert_eq!(names, ["NAME"], "a name bound through a const fails SAFE, as a println! does");
}

/// The other spellings of the same printer: the call wrapped across lines under a path
/// (`vike_util::cli::Program::new(` as `rustfmt` lays it out), the line built inline, and the line
/// reached through a local function whose body is the `version_line` call (`vike-cli`'s shape,
/// braces in a string and a closure included).
#[test]
fn a_program_new_is_resolved_wrapped_inline_and_through_a_function() {
    let wrapped = "let a: Args = vike_util::cli::Program::new(\n    \"tool\",\n    &vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\")),\n)\n.parse();";
    let texts = program_texts(wrapped);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert_eq!(version_line_names(&texts[0]), [OWN_PACKAGE_NAME]);

    let through_fn = "fn version_line() -> &'static str {\n    static LINE: OnceLock<String> = OnceLock::new();\n    LINE.get_or_init(|| {\n        vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\"))\n    })\n}\nfn program(bin: &str) -> Program<'_> {\n    Program::new(bin, version_line())\n}";
    let texts = program_texts(through_fn);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert_eq!(version_line_names(&texts[0]), [OWN_PACKAGE_NAME]);
}

/// A BARE printer is seen as one and does not state its identity: through `concat!`, through a
/// `format!` inside a sibling file's function (the shape `vike-backtesthub`'s bins and `vike-backfill`
/// use), and through a `let` that calls a function named like itself.
#[test]
fn a_bare_program_new_is_seen_and_does_not_state_an_identity() {
    let concat = "Program::new(\"tool\", concat!(\"tool \", env!(\"CARGO_PKG_VERSION\"))).parse();";
    let texts = program_texts(concat);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(is_version_text(&texts[0], &[]));
    assert!(!texts[0].contains("version_line"), "{texts:?}");

    let main = "fn main() {\n    let version = binutil::bin_version_line(\"tool\");\n    let args: Args = Program::new(\"tool\", &version).parse();\n}";
    let sibling = "pub fn bin_version_line(bin: &str) -> String {\n    format!(\"{bin} {}\", env!(\"CARGO_PKG_VERSION\"))\n}";
    let main = strip_comments(main);
    let texts = program_version_texts(&main, &[main.clone(), strip_comments(sibling)]);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        is_version_text(&texts[0], &[]),
        "a sibling file's function was not followed: {texts:?}"
    );
    assert!(!texts[0].contains("version_line"), "{texts:?}");

    let backfill = "pub fn parse(bin: &str) {\n    let version_line = version_line(bin);\n    Program::new(bin, &version_line).parse()\n}\nfn version_line(bin: &str) -> String {\n    format!(\"{bin} {}\", env!(\"CARGO_PKG_VERSION\"))\n}";
    let texts = program_texts(backfill);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(is_version_text(&texts[0], &[]), "{texts:?}");
    assert!(!texts[0].contains("version_line("), "{texts:?}");
}

/// What is NOT a printer: the `--help` rendering call that passes an empty version, another type's
/// `new`, and a version that is a function PARAMETER (declared blind spot: nothing here can follow
/// a value out of a caller, so a printer written that way fails open).
#[test]
fn a_program_new_that_does_not_print_a_version_is_not_a_printer() {
    for code in [
        "let usage = Program::new(BIN, \"\").command::<Args>().render_help();",
        "let p = MyProgram::new(BIN, &version);",
        "fn program(version: &str) -> Program<'_> {\n    Program::new(BIN, version)\n}",
        "let p = Program::new(BIN);",
    ] {
        let texts = program_texts(code);
        assert!(
            !texts.iter().any(|t| is_version_text(t, &[])),
            "{code:?} was taken for a --version printer: {texts:?}"
        );
    }
}

/// The new shape fires on the REAL tree, not only on fixtures: `vike-cli` (the `./vike-cli` the
/// release asserts) and `vike-backfill` (a bare printer resolved through a `let` and a function) are
/// seen, one with an identity and one without. Without a live sighting the resolver could be dead
/// code and every fixture above would still pass.
#[test]
fn the_program_new_shape_is_seen_in_the_real_tree() {
    let printers = version_printers();
    let identity_of = |file: &str| printers.iter().find(|(f, _)| f == file).map(|(_, id)| *id);
    assert_eq!(identity_of("crates/vike-cli/src/cmd/args.rs"), Some(true), "{printers:?}");
    assert_eq!(identity_of("crates/vike-backfill/src/cli.rs"), Some(false), "{printers:?}");
    let calls = version_printer_calls();
    let (_, cli_calls) = calls
        .iter()
        .find(|(f, _)| f == "crates/vike-cli/src/cmd/args.rs")
        .expect("vike-cli's args.rs is a printer");
    assert!(
        cli_calls.iter().flat_map(|c| version_line_names(c)).all(|n| n == OWN_PACKAGE_NAME),
        "{cli_calls:?}"
    );
}

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
