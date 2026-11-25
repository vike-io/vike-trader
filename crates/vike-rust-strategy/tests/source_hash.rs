//! The source hash a build folder is named by (`build/source_hash.rs`, run by `build.rs`), held both
//! ways: a change no artifact compiles keeps it (a deploy then reuses every artifact), a comment or layout
//! edit of a source or a manifest among them (`build/tokens.rs`), and a change to any linked source's code
//! or data moves it (a deploy then never serves an artifact built against other sources). The miniature
//! trees live under `CARGO_TARGET_TMPDIR`, one per test process; the tests that name it read this checkout.

use std::fs;
use std::path::{Path, PathBuf};

#[expect(
    clippy::module_inception,
    reason = "the build script's walk keeps its name inside this file's module"
)]
#[path = "../build/source_hash.rs"]
pub(crate) mod source_hash;

use source_hash::tokens::{rust_tokens, toml_tokens};
use source_hash::{
    FNV_OFFSET, HashForm, RENDERER, TEMPLATE_MANIFESTS, boundary_hash, closure, fnv1a64_fold,
    source_hash,
};
use vike_rust_strategy::sdk::{SdkMarker, SdkSourceHashes};

/// A template naming one crate by placeholder, as the real two do.
const TEMPLATE: &str = "[package]\nname = \"{{NAME}}\"\n\n[dependencies]\n\
    # a comment naming path = \"{{NOT_A_SEED}}\"\n\
    seed = { path = \"{{SEED_PATH}}\" }\nlibm = \"0.2\"\n";

/// A miniature workspace: `seed` (the template's) links `linked` normally and `tested` as a
/// dev-dependency only; `unrelated` is linked by nothing; the renderer sits at its real path.
fn tree(tag: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("source-hash-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.dependencies]\nserde = \"1\"\n\n[profile.release]\nlto = \"thin\"\n",
        ),
        (
            "crates/seed/Cargo.toml",
            "[package]\nname = \"seed\"\n\n[dependencies]\nlinked = { path = \"../linked\", features = [\"x\"] }\n\n[dev-dependencies]\ntested = { path = \"../tested\" }\n",
        ),
        ("crates/seed/src/lib.rs", "pub fn seed() {}\n"),
        ("crates/linked/Cargo.toml", "[package]\nname = \"linked\"\n"),
        ("crates/linked/src/lib.rs", "pub fn linked() -> u32 { 1 }\n"),
        ("crates/linked/src/deep/data.txt", "a table the crate includes\n"),
        ("crates/linked/tests/linked.rs", "#[test]\nfn t() {}\n"),
        ("crates/linked/README.md", "prose\n"),
        ("crates/tested/Cargo.toml", "[package]\nname = \"tested\"\n"),
        ("crates/tested/src/lib.rs", "pub fn tested() {}\n"),
        ("crates/unrelated/Cargo.toml", "[package]\nname = \"unrelated\"\n"),
        ("crates/unrelated/src/lib.rs", "pub fn unrelated() {}\n"),
        (RENDERER, "// the renderer\n"),
    ];
    for (file, text) in files {
        write(&root, file, text);
    }
    root
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the folder");
    fs::write(path, text).expect("the file");
}

fn hash(root: &Path) -> String {
    source_hash(root, &[TEMPLATE], HashForm::Tokens).hash
}

/// The hash is the workspace's frozen FNV-1a 64 over a spelled-out encoding, so a tree hashes the same under
/// every toolchain (decision 0296's step 0): the module's fold is held to `vike_utility::fnv`'s, and the
/// miniature tree's hash is pinned as a value in both forms. A changed digit here moves every build folder;
/// the BYTES form's value is the one every hash had before 2026-11-20, which an SDK cut then (SDK 1) is
/// still checked in, so it may never move.
#[test]
fn the_hash_is_the_frozen_fnv_and_no_toolchain_moves_it() {
    let every_byte: Vec<u8> = (0..=255).collect();
    let inputs: [&[u8]; 4] = [b"", b"a", b"foobar", &every_byte];
    for bytes in inputs {
        assert_eq!(fnv1a64_fold(FNV_OFFSET, bytes), vike_utility::fnv::fnv1a64(bytes), "{bytes:?}");
    }
    let root = tree("golden");
    assert_eq!(source_hash(&root, &[TEMPLATE], HashForm::Bytes).hash, "2b216bb40f58545a");
    assert_eq!(hash(&root), "8c0a23e651559a1f");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_closure_follows_normal_path_dependencies_and_never_a_dev_dependency() {
    let root = tree("closure");
    assert_eq!(closure(&root, &[TEMPLATE]), ["crates/linked", "crates/seed"]);
    let _ = fs::remove_dir_all(&root);
}

/// A change no artifact compiles keeps the hash: another crate's source, a dev-dependency, a linked
/// crate's tests and prose, the root manifest outside its dependency table.
#[test]
fn a_change_outside_the_closure_keeps_the_hash() {
    let root = tree("keeps");
    let before = hash(&root);
    assert_eq!(before.len(), 16, "{before}");
    for (file, text) in [
        ("crates/unrelated/src/lib.rs", "pub fn unrelated() { /* edited */ }\n"),
        ("crates/tested/src/lib.rs", "pub fn tested() { /* edited */ }\n"),
        ("crates/linked/tests/linked.rs", "#[test]\nfn t() { /* edited */ }\n"),
        ("crates/linked/README.md", "other prose\n"),
        ("crates/unrelated/src/new.rs", "pub fn new() {}\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/seed\"]\n\n[workspace.dependencies]\nserde = \"1\"\n\n[profile.release]\nlto = true\n",
        ),
    ] {
        write(&root, file, text);
        assert_eq!(hash(&root), before, "an edit of {file} must keep the folder");
    }
    let _ = fs::remove_dir_all(&root);
}

/// A change to anything a linked crate compiles moves the hash, and undoing it moves it back: a
/// transitively linked source, a file beside the sources, a new source file, a manifest, the
/// renderer, the root's dependency table.
#[test]
fn a_change_to_a_linked_source_moves_the_hash() {
    let root = tree("moves");
    let before = hash(&root);
    for (file, edited, original) in [
        (
            "crates/linked/src/lib.rs",
            "pub fn linked() -> u32 { 2 }\n",
            Some("pub fn linked() -> u32 { 1 }\n"),
        ),
        (
            "crates/linked/src/deep/data.txt",
            "another table\n",
            Some("a table the crate includes\n"),
        ),
        ("crates/seed/src/added.rs", "pub fn added() {}\n", None),
        (
            "crates/linked/Cargo.toml",
            "[package]\nname = \"linked\"\nedition = \"2024\"\n",
            Some("[package]\nname = \"linked\"\n"),
        ),
        ("crates/seed/build.rs", "fn main() {}\n", None),
        (RENDERER, "pub fn render() {}\n", Some("// the renderer\n")),
    ] {
        write(&root, file, edited);
        assert_ne!(hash(&root), before, "an edit of {file} must move the folder");
        match original {
            Some(text) => write(&root, file, text),
            None => fs::remove_file(root.join(file)).expect("remove the added file"),
        }
        assert_eq!(hash(&root), before, "undoing the edit of {file} must give the folder back");
    }
    write(
        &root,
        "Cargo.toml",
        "[workspace]\nmembers = []\n\n[workspace.dependencies]\nserde = \"1.0.200\"\n\n[profile.release]\nlto = \"thin\"\n",
    );
    assert_ne!(hash(&root), before, "the root's dependency table must move the folder");
    let _ = fs::remove_dir_all(&root);
}

/// A file a linked source includes is hashed wherever it lives, outside `src/` too, and watched; one
/// that only a dev-dependency or an unrelated crate includes is not.
#[test]
fn an_included_file_outside_the_sources_moves_the_hash() {
    let root = tree("includes");
    write(
        &root,
        "crates/linked/src/lib.rs",
        "pub const T: &str = include_str!(\"../data/table.txt\");\n",
    );
    write(&root, "crates/linked/data/table.txt", "one\n");
    write(
        &root,
        "crates/unrelated/src/lib.rs",
        "pub const T: &[u8] = include_bytes!(\"../blob.bin\");\n",
    );
    write(&root, "crates/unrelated/blob.bin", "one\n");
    let before = source_hash(&root, &[TEMPLATE], HashForm::Tokens);
    assert!(
        before.watched.contains(&root.join("crates/linked/data/table.txt")),
        "{:?}",
        before.watched
    );
    write(&root, "crates/unrelated/blob.bin", "two\n");
    assert_eq!(hash(&root), before.hash, "an unrelated crate's include must keep the folder");
    write(&root, "crates/linked/data/table.txt", "two\n");
    assert_ne!(hash(&root), before.hash, "a linked source's include must move the folder");
    assert_eq!(
        source_hash::include_targets(
            "crates/k/src/a/b.rs",
            "x!(); include_bytes! ( \"../c.bin\" ); include!(concat!(\"x\"));"
        ),
        ["crates/k/src/c.bin"]
    );
    let _ = fs::remove_dir_all(&root);
}

/// A lock entry of a third-party package the closure compiles moves the hash (review L1): the template's
/// `libm` and what it lists, transitively; and the lock is watched. A lock change no artifact compiles
/// keeps it: a package only an unrelated crate takes, and a path package's own entry, whose
/// dev-dependency list reaches no artifact.
#[test]
fn a_lock_change_to_a_linked_third_party_package_moves_the_hash() {
    let root = tree("lock");
    let lock = |libm: &str, inner: &str, other: &str, seed_dev: &str| {
        format!(
            "version = 4\n\n\
             [[package]]\nname = \"inner\"\nversion = \"{inner}\"\nsource = \"registry+x\"\n\n\
             [[package]]\nname = \"libm\"\nversion = \"{libm}\"\nsource = \"registry+x\"\n\
             dependencies = [\n \"inner\",\n]\n\n\
             [[package]]\nname = \"other\"\nversion = \"{other}\"\nsource = \"registry+x\"\n\n\
             [[package]]\nname = \"seed\"\nversion = \"0.1.0\"\ndependencies = [\n \"linked\",\n \"{seed_dev}\",\n]\n"
        )
    };
    let original = lock("0.2.15", "1.0.0", "3.0.0", "tested");
    write(&root, "Cargo.lock", &original);
    let before = source_hash(&root, &[TEMPLATE], HashForm::Tokens);
    assert!(before.watched.contains(&root.join("Cargo.lock")), "{:?}", before.watched);
    for (edited, moves) in [
        (lock("0.2.16", "1.0.0", "3.0.0", "tested"), true),
        (lock("0.2.15", "1.0.1", "3.0.0", "tested"), true),
        (lock("0.2.15", "1.0.0", "3.0.1", "tested"), false),
        (lock("0.2.15", "1.0.0", "3.0.0", "other"), false),
    ] {
        write(&root, "Cargo.lock", &edited);
        assert_eq!(hash(&root) != before.hash, moves, "{edited}");
        write(&root, "Cargo.lock", &original);
        assert_eq!(hash(&root), before.hash, "undone");
    }
    let _ = fs::remove_dir_all(&root);
}

/// Only paths that exist are watched (cargo would re-run the script on every build for a missing one),
/// and a crate's sources are watched as its folder.
#[test]
fn only_existing_paths_are_watched() {
    let root = tree("watched");
    fs::remove_file(root.join(RENDERER)).expect("remove the renderer");
    let watched = source_hash(&root, &[TEMPLATE], HashForm::Tokens).watched;
    assert!(watched.iter().all(|path| path.exists()), "{watched:?}");
    for expected in ["crates/seed/src", "crates/linked/src", "crates/seed/Cargo.toml", "Cargo.toml"]
    {
        assert!(watched.contains(&root.join(expected)), "{expected} in {watched:?}");
    }
    assert!(!watched.contains(&root.join("crates/tested/src")), "a dev-dependency is not watched");
    let _ = fs::remove_dir_all(&root);
}

/// This checkout's closure is the two templates' crates and their normal dependencies: `vike-paths`
/// comes in through `vike-strategy`, `vike-math` through both templates (decision 0294), `vike-simulator`
/// through the STUDY template (decision 0291), and `vike-strategy-builder`, only a dev-dependency of this
/// crate, does not.
#[test]
fn this_checkouts_closure_is_the_templates_crates_and_their_normal_dependencies() {
    let crates = closure(&checkout(), &templates());
    for expected in [
        "crates/vike-model",
        "crates/vike-indicators",
        "crates/vike-strategy",
        "crates/vike-rust-strategy",
        "crates/vike-user-research",
        "crates/vike-paths",
        "crates/vike-math",
    ] {
        assert!(crates.iter().any(|c| c == expected), "{expected} in {crates:?}");
    }
    assert!(
        crates.iter().any(|c| c == "crates/vike-simulator"),
        "the study template names it: {crates:?}"
    );
    assert!(!crates.iter().any(|c| c == "crates/vike-strategy-builder"), "{crates:?}");
    assert!(crates.iter().all(|c| checkout().join(c).join("Cargo.toml").is_file()), "{crates:?}");
}

/// The hash this binary carries (`build.rs`'s, through `vike_rust_strategy::artifact::SOURCE_HASH`) is
/// the hash of this checkout: cargo re-ran the build script for every watched change before this test
/// was built.
#[test]
fn the_baked_hash_is_this_checkouts() {
    assert_eq!(
        source_hash(&checkout(), &templates(), HashForm::Tokens).hash,
        vike_rust_strategy::artifact::SOURCE_HASH
    );
    // Both forms, for a reader of the log: what a comment-blind walk changed.
    let krate = Path::new(env!("CARGO_MANIFEST_DIR"));
    eprintln!(
        "this checkout: source hash {} (tokens), {} (bytes); boundary hash {} (tokens), {} (bytes)",
        vike_rust_strategy::artifact::SOURCE_HASH,
        source_hash(&checkout(), &templates(), HashForm::Bytes).hash,
        boundary_hash(krate, HashForm::Tokens).hash,
        boundary_hash(krate, HashForm::Bytes).hash,
    );
}

/// The boundary hash moves with each boundary file and folder (`BOUNDARY_FILES`, `BOUNDARY_FOLDERS`) and
/// with nothing else of the crate; the baked `BOUNDARY_HASH` is this checkout's.
#[test]
fn the_boundary_hash_moves_with_the_boundary_alone() {
    let krate = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("boundary-hash-{}", std::process::id()));
    let _ = fs::remove_dir_all(&krate);
    for (file, text) in [
        ("src/abi.rs", "abi\n"),
        ("src/lib.rs", "lib\n"),
        ("src/host/thunks.rs", "thunks\n"),
        ("template/lib.rs.in", "template\n"),
        ("tests/boundary/forecast.json", "{}\n"),
    ] {
        write(&krate, file, text);
    }
    let boundary = |krate: &Path| boundary_hash(krate, HashForm::Tokens).hash;
    let before = boundary(&krate);
    assert_eq!(before.len(), 16, "{before}");
    write(&krate, "src/lib.rs", "lib, edited\n");
    write(&krate, "src/loader.rs", "a new non-boundary file\n");
    // An adapter is an older SDK's face, compiled only into that SDK's builds: never this boundary.
    write(&krate, "adapters/sdk-1/face.rs", "an adapter\n");
    assert_eq!(boundary(&krate), before, "not the boundary");
    let mut last = before;
    for (file, text) in [
        ("src/abi.rs", "abi, edited\n"),
        ("src/guest.rs", "guest, new\n"),
        ("src/guest/face.rs", "the face, new\n"),
        ("src/study.rs", "the study's byte boundary, new\n"),
        ("src/study.rs", "the study's byte boundary, edited\n"),
        ("src/host/thunks.rs", "thunks, edited\n"),
        ("template/lib.rs.in", "template, edited\n"),
        ("tests/boundary/forecast.json", "{\"a\":1}\n"),
    ] {
        write(&krate, file, text);
        let now = boundary(&krate);
        assert_ne!(now, last, "{file} is the boundary");
        last = now;
    }
    let _ = fs::remove_dir_all(&krate);
    assert_eq!(
        boundary(Path::new(env!("CARGO_MANIFEST_DIR"))),
        vike_rust_strategy::sdk::BOUNDARY_HASH
    );
}

/// **A comment or a layout edit moves neither hash; a token, a literal or data does** (the owner's ruling of
/// 2026-11-20). Per hashed kind: a Rust source (a doc comment, a block comment, re-indented and re-wrapped
/// code), a crate manifest and the root's dependency table (a `#` comment, spaces, a blank line), a
/// template; and a file a source reads as TEXT counts by its bytes, a comment's too.
#[test]
fn a_comment_or_a_layout_edit_moves_neither_hash() {
    let root = tree("tokens");
    write(
        &root,
        "crates/linked/src/lib.rs",
        "pub fn linked() -> u32 { 1 }\npub const S: &str = \"a b\";\n",
    );
    let before = hash(&root);
    let edits = [
        (
            "crates/linked/src/lib.rs",
            "//! The crate.\n/// One.\npub fn linked()\n    -> u32 {\n    1 /* one */\n}\n\npub const S: &str = \"a b\"; // a\n",
            true,
        ),
        (
            "crates/linked/src/lib.rs",
            "pub fn linked() -> u32 { 2 }\npub const S: &str = \"a b\";\n",
            false,
        ),
        (
            "crates/linked/src/lib.rs",
            "pub fn linked() -> u32 { 1 }\npub const S: &str = \"a  b\";\n",
            false,
        ),
        (
            "crates/linked/src/lib.rs",
            "pub fn linked() -> u32 { 1 }\npub const S: &str = \"a b\";\n",
            true,
        ),
        (
            "crates/linked/Cargo.toml",
            "# The linked crate.\n[package]\n\nname   =  \"linked\" # its name\n",
            true,
        ),
        ("crates/linked/Cargo.toml", "[package]\nname = \"linked2\"\n", false),
        ("crates/linked/Cargo.toml", "[package]\nname = \"linked\"\n", true),
        (
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.dependencies]\n# a pin\nserde =   \"1\"\n\n[profile.release]\nlto = \"thin\"\n",
            true,
        ),
        ("crates/linked/src/deep/data.txt", "a table the crate includes \n", false),
    ];
    for (file, text, keeps) in edits {
        write(&root, file, text);
        assert_eq!(hash(&root) == before, keeps, "{file}: {text:?}");
    }
    // A source another source reads as text (`include_str!`): its comments are data.
    write(&root, "crates/linked/src/lib.rs", "pub const ME: &str = include_str!(\"lib.rs\");\n");
    let read = hash(&root);
    write(
        &root,
        "crates/linked/src/lib.rs",
        "pub const ME: &str = include_str!(\"lib.rs\"); // read\n",
    );
    assert_ne!(hash(&root), read, "a file read as text counts by its bytes");
    let _ = fs::remove_dir_all(&root);

    // The boundary: its sources and templates by their tokens, its goldens by their bytes.
    let krate = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("boundary-tokens-{}", std::process::id()));
    let _ = fs::remove_dir_all(&krate);
    write(&krate, "src/abi.rs", "pub const ABI_VERSION: u32 = 15;\n");
    write(&krate, "template/lib.rs.in", "fn f() -> u8 { {{ENTRY}} } // {{ECHO}}\n");
    write(&krate, "template/Cargo.toml.in", "[package]\nname = \"{{NAME}}\"\n");
    write(&krate, "tests/boundary/names.json", "[1]\n");
    let boundary = |krate: &Path| boundary_hash(krate, HashForm::Tokens).hash;
    let before = boundary(&krate);
    for (file, text, keeps) in [
        ("src/abi.rs", "/// The interface version.\npub const ABI_VERSION: u32 =\n    15;\n", true),
        ("src/abi.rs", "pub const ABI_VERSION: u32 = 16;\n", false),
        ("src/abi.rs", "pub const ABI_VERSION: u32 = 15;\n", true),
        (
            "template/lib.rs.in",
            "// The entry.\nfn f() -> u8 {\n    {{ENTRY}}\n} // {{ECHO}}\n",
            true,
        ),
        ("template/lib.rs.in", "fn f() -> u8 { {{ENTRY}} } // {{ECHO}} filled\n", false),
        ("template/lib.rs.in", "fn f() -> u8 { {{ENTRY}} } // {{ECHO}}\n", true),
        ("template/Cargo.toml.in", "# the crate\n[package]\nname = \"{{NAME}}\"\n", true),
        ("tests/boundary/names.json", "[ 1 ]\n", false),
    ] {
        write(&krate, file, text);
        assert_eq!(boundary(&krate) == before, keeps, "{file}: {text:?}");
    }
    let _ = fs::remove_dir_all(&krate);
}

/// The lexer the token form reads a Rust source with: what it drops, what it keeps, and the sources it
/// gives up on (the walk then hashes their bytes).
#[test]
fn the_token_form_drops_comments_and_layout_and_keeps_every_token() {
    let same = |a: &str, b: &str| {
        assert_eq!(rust_tokens(a), rust_tokens(b), "{a:?} and {b:?} read alike");
        assert!(rust_tokens(a).is_some(), "{a:?} lexes");
    };
    let differ = |a: &str, b: &str| assert_ne!(rust_tokens(a), rust_tokens(b), "{a:?} and {b:?}");
    same("fn f(a: u8) -> u8 { a + 1 }", "fn f(a:u8)->u8{a+1}");
    same("let x = 1; // one", "let x =\n    1;");
    same("/* a /* nested */ block */ x", "/** doc */ x");
    same("//! inner\n/// outer\nx", "x");
    same("let s = r#\"a // not a comment\"#;", "let s =\nr#\"a // not a comment\"#;");
    same("let c = '/'; let d = '\\'';", "let c='/';let d='\\'';");
    same("fn f<'a>(x: &'a str) {}", "fn f< 'a >(x: & 'a str) {}");
    same("let x = 1.0e-3f64 + 0x1e - 1..2;", "let x=1.0e-3f64+0x1e-1..2;");
    differ("a && b", "a & &b");
    differ("a << b", "a < <b");
    differ("x.0.1", "x . 0 . 1");
    differ("m!(1.)", "m!(1 .)");
    differ("let s = \"a b\";", "let s = \"a  b\";");
    differ("let c = b'a';", "let c = b 'a';");
    differ("let r = r#type;", "let r = r # type;");
    differ("x", "y");
    differ("// {{A}}\nx", "// {{A}} x");
    differ("{{NAME}}", "{{ NAME }}");
    // A comment inside a `macro_rules!` definition is kept: a matcher may hold or capture a doc comment.
    differ("macro_rules! m { (/// a\n $x:ident) => {} }", "macro_rules! m { ($x:ident) => {} }");
    same("macro_rules! m { ($x:ident) => {} } // after", "macro_rules! m { ($x:ident) => {} }");
    for broken in ["\"open", "/* open", "{", "}", "'", "r#\"open\"", "let c = ' x;"] {
        assert_eq!(rust_tokens(broken), None, "{broken:?} does not lex");
    }
    assert_eq!(toml_tokens("a = \"x # y\" # z\n\n[t]\nb = 'q'\n"), "a=\"x # y\"\n[t]\nb='q'\n");
    assert_eq!(toml_tokens("s = \"\"\"\n a \"\" # b\n\"\"\"\n"), "s=\"\"\"\n a \"\" # b\n\"\"\"\n");
    assert_eq!(toml_tokens("# {{A}} kept\nx = 1"), "# {{A}} kept\nx=1");
}

/// **The token forms are FROZEN once an SDK is cut with them** (`build/tokens.rs`'s module doc): a later SDK's
/// marker, every SDK's baked boundary and every adapter's recorded boundary hash this output, so a lexer edit
/// that moves one byte of it refuses every installed SDK. Each row is one branch of the lexer and its output
/// exactly, as it reads today: comments (nested, doc, kept in a `macro_rules!` body and beside a `{{X}}`
/// placeholder), literals of every prefix (raw, byte, C), chars against lifetimes, raw identifiers, numbers
/// (fractions, exponents, suffixes; `1usize-3` reads as ONE token, where rustc reads three, pinned as it reads),
/// glued punctuation, CRLF, a shebang, non-ASCII; and a manifest's comments, strings and layout.
#[test]
fn the_token_forms_are_pinned_byte_for_byte() {
    let rust: [(&str, &str); 13] = [
        (
            "fn f<'a>(x: &'a str) -> u8 { x.len() as u8 }",
            "fn f < 'a > ( x : & 'a str ) -> u8 { x . len ( ) as u8 }",
        ),
        ("// line\nx /* a /* b */ c */ y /** d */ z //! e\n", "x y z"),
        (r#"let s = "a \" // b"; let t = b"\x01";"#, r#"let s = "a \" // b" ; let t = b"\x01" ;"#),
        (
            r###"r#"a "quoted" b"# br"raw" cr##"c"##"###,
            r###"r#"a "quoted" b"# br"raw" cr##"c"##"###,
        ),
        (r#"c"cstr" c'x' b'a' b'\''"#, r#"c"cstr" c 'x' b'a' b'\''"#),
        (
            r"'a' '\'' '\n' '\u{1F600}' 'a '_ 'r#a 'static 'é'",
            r"'a' '\'' '\n' '\u{1F600}' 'a '_ 'r#a 'static 'é'",
        ),
        ("r#type r#match", "r#type r#match"),
        (
            "1.0e-3f64 0x1e-1 1usize-3 2. 3..4 1E+5 0b1010_u8 1.5.max(x) 7.foo",
            "1.0e-3f64 0x1e - 1 1usize-3 2. 3 .. 4 1E+5 0b1010_u8 1.5 . max ( x ) 7 . foo",
        ),
        ("a&&b a & &b x<<=1 y..=z ::std::f", "a && b a & & b x <<= 1 y ..= z :: std :: f"),
        (
            "fn f() -> u8 { {{ENTRY}} } // {{ECHO}}\n{{ NAME }}",
            "fn f ( ) -> u8 { {{ENTRY}} } // {{ECHO}}\n{ { NAME } }",
        ),
        (
            "macro_rules! m { (/// doc\n $x:expr) => { $x // keep\n }; } // after\nm!(1)",
            "macro_rules ! m { ( /// doc\n$ x : expr ) => { $ x // keep\n} ; } m ! ( 1 )",
        ),
        (
            "#!/usr/bin/env rust\r\n#![allow(dead_code)]\r\nfn main() {}\r\n",
            "# !/ usr / bin / env rust # ! [ allow ( dead_code ) ] fn main ( ) { }",
        ),
        ("let café = \"naïve\";", "let café = \"naïve\" ;"),
    ];
    let toml: [(&str, &str); 4] = [
        (
            "# top\n[package]\r\nname = \"x # y\"  # trailing\n\n\nedition='2024'\nversion = { workspace = true }\n",
            "[package]\nname=\"x # y\"\nedition='2024'\nversion={workspace=true}\n",
        ),
        (
            "s = \"\"\"\nline \"quoted\" # not a comment\n\"\"\"\nt = '''raw\n'''\n",
            "s=\"\"\"\nline \"quoted\" # not a comment\n\"\"\"\nt='''raw\n'''\n",
        ),
        ("name = \"{{NAME}}\" # {{CRATE}} kept   \n", "name=\"{{NAME}}\"# {{CRATE}} kept\n"),
        ("a = \"x\\\"y\" # c\nb = 'C:\\path'\n", "a=\"x\\\"y\"\nb='C:\\path'\n"),
    ];
    let mut moved = Vec::new();
    for (input, pinned) in rust {
        let now = rust_tokens(input);
        if now.as_deref() != Some(pinned) {
            moved.push(format!("rust {input:?}\n  pinned {pinned:?}\n  now    {now:?}"));
        }
    }
    for (input, pinned) in toml {
        let now = toml_tokens(input);
        if now != pinned {
            moved.push(format!("toml {input:?}\n  pinned {pinned:?}\n  now    {now:?}"));
        }
    }
    assert!(
        moved.is_empty(),
        "the token form moved, which refuses every SDK cut with it (a change is a new hash form):\n{}",
        moved.join("\n")
    );
}

/// Every Rust source this checkout's hash reads lexes, so each is hashed as its tokens (one that did not
/// would count by its bytes: safe, but a comment there would move the hash).
#[test]
fn every_source_this_checkouts_hash_reads_lexes() {
    let root = checkout();
    let mut sources = Vec::new();
    for krate in closure(&root, &templates()) {
        for folder in ["src", "build"] {
            collect_rust(&root.join(&krate).join(folder), &mut sources);
        }
    }
    collect_rust(&root.join(RENDERER), &mut sources);
    collect_rust(&Path::new(env!("CARGO_MANIFEST_DIR")).join("template"), &mut sources);
    assert!(sources.len() > 100, "{}", sources.len());
    let unlexed: Vec<&PathBuf> = sources
        .iter()
        .filter(|path| fs::read_to_string(path).map_or(true, |text| rust_tokens(&text).is_none()))
        .collect();
    assert!(unlexed.is_empty(), "these sources do not lex: {unlexed:?}");
}

/// Every `.rs` and `.rs.in` file at `path` (a file or a folder), into `out`.
fn collect_rust(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("a folder").flatten() {
            collect_rust(&entry.path(), out);
        }
    } else if path.to_string_lossy().ends_with(".rs") || path.to_string_lossy().ends_with(".rs.in")
    {
        out.push(path.to_path_buf());
    }
}

/// A marker written before 2026-11-20 (SDK 1's) records the hashes of the BYTES; a host reading tokens checks
/// it in that form: it still verifies the snapshot it was written from, and refuses one a byte of which
/// moved, a comment's too.
#[test]
fn a_marker_of_the_bytes_still_verifies_its_snapshot() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bytes-marker-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (strategy, study) = ("[dependencies]\nvike-a = { path = \"{{A}}\" }\n", "[dependencies]\n");
    for (file, text) in [
        ("crates/vike-rust-strategy/template/Cargo.toml.in", strategy),
        ("crates/vike-rust-strategy/template/study_Cargo.toml.in", study),
        ("crates/vike-rust-strategy/src/abi.rs", "pub const ABI_VERSION: u32 = 14;\n"),
        ("crates/vike-a/Cargo.toml", "[package]\nname = \"vike-a\"\n"),
        ("crates/vike-a/src/lib.rs", "pub fn a() {}\n"),
    ] {
        write(&root, file, text);
    }
    let bytes = |template: &str| source_hash(&root, &[template], HashForm::Bytes).hash;
    let krate = root.join("crates/vike-rust-strategy");
    let marker = SdkMarker {
        number: 1,
        commit: "203e18f9e4".to_string(),
        abi_version: 15,
        boundary_hash: boundary_hash(&krate, HashForm::Bytes).hash,
        source_hash: SdkSourceHashes {
            strategy: bytes(strategy),
            onnx: bytes(strategy),
            study: bytes(study),
        },
    };
    assert_ne!(marker, SdkMarker::of_snapshot(&root, 1, "203e18f9e4", 15), "two forms, two hashes");
    marker.verify(&root).expect("the snapshot it was written from, in its form");
    write(&root, "crates/vike-a/src/lib.rs", "pub fn a() {} // edited\n");
    assert!(marker.verify(&root).is_err(), "a byte moved");
    let _ = fs::remove_dir_all(&root);
}

/// The manifests `build.rs` (and xtask's planner) read as the templates are the two the library compiles
/// in, byte for byte.
#[test]
fn the_template_manifests_are_the_librarys_two() {
    let read = TEMPLATE_MANIFESTS.map(|path| {
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
            .unwrap_or_else(|e| panic!("{path}: {e}"))
    });
    assert_eq!(read, templates());
}

/// Two levels up from this crate's manifest directory: the checkout root.
fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn templates() -> [&'static str; 2] {
    [vike_rust_strategy::TEMPLATE_CARGO_TOML, vike_rust_strategy::TEMPLATE_STUDY_CARGO_TOML]
}
