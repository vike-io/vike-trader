//! The reserved-name roster, DERIVED from the writers' own source rather than kept by hand.

use crate::test_support::text::fn_body;

/// Every `*_FILE` const named between `fn <name>(` and the first line that is a lone `}`.
///
/// Deliberately a scan over this module's OWN SOURCE rather than a list: the point of the test
/// below is that the roster cannot fall behind the writer, and a hand-written list of what the
/// writer writes is the thing that falls behind.
fn file_consts_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut tok = String::new();
    for c in text.chars() {
        if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
            tok.push(c);
            continue;
        }
        if tok.ends_with("_FILE") && !out.contains(&tok) {
            out.push(tok.clone());
        }
        tok.clear();
    }
    if tok.ends_with("_FILE") && !out.contains(&tok) {
        out.push(tok);
    }
    out.sort();
    out
}

/// Every function in this module that writes a file INTO a run directory, by name.
///
/// ⚠ **It is a LIST because it stopped being one function**, and the change is the whole reason
/// this array exists. [`write_run_with`] writes the five documents a run is made of; [`add_tags`]
/// writes [`META_FILE`] long AFTER the run finished, which is exactly what lets a tag be
/// optional. Both land in the same namespace, so both must be reserved against, and the roster
/// is therefore "every name this MODULE writes" rather than "every name the run WRITER writes".
/// A third writer added here and not to [`RESERVED_FILES`] reddens
/// `every_document_this_module_writes_is_in_the_reserved_roster`; a third writer added to
/// NEITHER is the hole this array cannot see, and is why each entry is a deliberate act.
const RUN_DIRECTORY_WRITERS: &[&str] = &["write_run_with", "add_tags"];

/// One roster of reserved names, so a producer writing its own artifacts cannot collide with a
/// document this module writes. `crates/vike-studio-core/src/study_run.rs`'s `persist` checked
/// exactly two names and there are six.
///
/// ⚠ **DERIVED from the writers' own bodies, on both sides.** This test first compared a
/// hand-written array against [`RESERVED_FILES`] — which IS that array, so it compared a copy
/// with itself and a SIXTH document added to the writer and not to the roster passed it. That
/// is precisely the failure the roster exists to prevent, so the test reads the writers instead.
#[test]
fn every_document_this_module_writes_is_in_the_reserved_roster() {
    let src = read_module_source(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("runs.rs"),
    )
    .expect(
        "this module's own source — read at run time, never `include_str!`, so the \
                 published mirror can withhold a file without breaking the build",
    );

    let mut written: Vec<String> = Vec::new();
    for writer in RUN_DIRECTORY_WRITERS {
        let body = fn_body(&src, writer);
        let found = file_consts_in(&body);
        // ⚠ PER-WRITER, not only over the union: a name that was renamed or reshaped yields an
        // empty body, and the union's floor below would still pass on the other writer's five.
        assert!(
            !found.is_empty(),
            "the harvest found nothing in `{writer}` — it was renamed or reshaped, and this \
                 gate is now measuring one writer fewer than it claims"
        );
        for name in found {
            if !written.contains(&name) {
                written.push(name);
            }
        }
    }
    written.sort();
    let reserved = file_consts_in(&decl_of(&src, "pub const RESERVED_FILES"));

    // The floor: a harvester that has stopped matching passes every assertion by seeing
    // nothing, which is how a derived gate quietly becomes a no-op.
    assert!(
        written.len() >= 6,
        "the writer harvest found {written:?} across {RUN_DIRECTORY_WRITERS:?}"
    );
    assert!(reserved.len() >= 6, "the roster harvest found {reserved:?}");

    for name in &written {
        assert!(
            reserved.contains(name),
            "`{name}` is written by one of {RUN_DIRECTORY_WRITERS:?} and is NOT in \
                 RESERVED_FILES — a producer writing its own artifact under that name would \
                 silently overwrite a document this module writes. Add it to the roster.\n  \
                 writers: {written:?}\n  roster:  {reserved:?}"
        );
    }
    assert_eq!(
        written, reserved,
        "the roster and the writers must name the SAME set — a reserved name nothing writes \
             refuses a producer's artifact for no reason"
    );
}

/// The source between `head` and the first `;` — [`RESERVED_FILES`]'s declaration.
fn decl_of(text: &str, head: &str) -> String {
    let i = text.find(head).expect("declaration not found — was it renamed?");
    let rest = &text[i..];
    let end = rest.find(';').map(|e| i + e).unwrap_or(text.len());
    text[i..end].to_string()
}

/// The harvesters' own proof, over planted text: a scan that has gone blind passes the test
/// above by finding nothing on BOTH sides, and the length floors are a blunt instrument beside
/// this.
#[test]
fn the_writer_harvest_reads_a_body_and_takes_only_file_consts() {
    let planted = "\
pub fn write_run_with<R>(dir: &Path) -> u8 {
    write_text(&dir.join(CONFIG_FILE), toml)?;
    write_json(&dir.join(SERIES_FILE), SERIES_FILE, series)?;
    write_json(&dir.join(MANIFEST_FILE), MANIFEST_FILE, manifest)
}

pub fn something_else() {
    let _ = NOT_MINE_FILE;
}
";

    assert_eq!(
        file_consts_in(&fn_body(planted, "write_run_with")),
        vec!["CONFIG_FILE".to_string(), "MANIFEST_FILE".to_string(), "SERIES_FILE".to_string()],
        "every `*_FILE` const the body names, deduplicated and sorted, and nothing from the \
             function after it"
    );
    assert_eq!(
        file_consts_in(&decl_of(
            "pub const RESERVED_FILES: &[&str] = &[MANIFEST_FILE, CONFIG_FILE];\nnext",
            "pub const RESERVED_FILES"
        )),
        vec!["CONFIG_FILE".to_string(), "MANIFEST_FILE".to_string()],
        "and the roster side reads the same way"
    );
    assert!(
        !file_consts_in("let x = MAX_TRADES;").contains(&"MAX_TRADES".to_string()),
        "a SCREAMING const that is not a `*_FILE` must not join either side"
    );
}

/// `runs.rs` followed by every `.rs` file directly inside `runs/`, sorted by name: the module's
/// source now that its writers live in child files. `runs/tests/` is a directory and is skipped,
/// which matters — the planted fixture above spells a writer at column zero.
fn read_module_source(root: std::path::PathBuf) -> std::io::Result<String> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir(root.with_extension(""))? {
        let path = entry?.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            children.push(path);
        }
    }
    children.sort();
    let mut parts = vec![std::fs::read_to_string(&root)?];
    for child in children {
        parts.push(std::fs::read_to_string(child)?);
    }
    Ok(parts.join("\n"))
}
