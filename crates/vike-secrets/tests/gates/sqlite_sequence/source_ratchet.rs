//! The source ratchet: every table drop in this crate's `src/` is classified, so a rebuild cannot be
//! added without saying what happens to the mark.

use std::collections::BTreeSet;

// -------------------------------------------------------------------------------------------
// The gate — the source ratchet
// -------------------------------------------------------------------------------------------

/// The statement that deletes a `sqlite_sequence` row, COMPOSED rather than spelled.
///
/// This file's scan reads `crates/vike-secrets/src/`, not itself, so composing the needle is not
/// strictly required here — it is kept because the needle is the ONE string in this file that must
/// never accidentally become a match, and because the scan's root is a variable somebody may widen.
const DROPS_TABLE: &str = concat!("DROP ", "TABLE ");

/// **Every place this crate's `src/` drops a table**, as `(path under `src/`, the enclosing `fn`,
/// the table token, why it cannot lose a mark)`.
///
/// ⚠ **The declared length is the COUNT** — no prose anywhere may restate it.
///
/// A table drop is the statement §4.1 names, and a rebuild cannot be written without one.
///
/// ⚠ **The duty this pin records is BOTH: classify the next drop, and make the rebuild around it
/// carry the mark.** The gate's own failure message sends the next author HERE, so this is where
/// that is said.
///
/// ⚠ **The `fn` column is part of the KEY, not decoration.** The pin was keyed on `(file, token)`
/// alone, and `db.rs` grew a SECOND `DROP TABLE credential_old;` in a different test — two sites
/// collapsing onto one row, which is the exact hole the `IF EXISTS` and `;` handling in
/// [`drops_in`] exists to close, re-opened one column further out.
/// [`the_drop_scan_distinguishes_two_identical_drops_in_one_file`] holds the distinction on planted
/// source rather than on whatever `db.rs` happens to contain.
///
/// ⚠ **It has already caught one, on the day it landed.** The `sim` -> `paper` rebuild (deleted
/// since, with every other in-place upgrade step) arrived on the same branch hours later, reddened
/// this test, and its author read the message and made that rebuild carry the mark. This pin was
/// deliberately NOT written in advance for it: pre-blessing a site whose final form nobody had read
/// is the one thing a ratchet must not do.
///
/// # ⚠ Declared blind spots
///
/// [`table_drops`] keys on the LITERAL statement, so a drop assembled out of pieces — a `const` for
/// the verb, a `format!` that splices it — escapes the scan entirely. And the scan reads `src/`
/// ONLY: [`rebuild_preserving_ids`] in this file drops a table and is deliberately out of scope,
/// because a harness is not production. Both are measured rather than assumed away: this ratchet is
/// a classification duty over ordinary source, not a proof that no drop exists.
///
/// A THIRD: the `fn` column is the innermost `fn` DECLARED above the line, so two drops inside one
/// function collapse onto one row **when their TOKENS match as well**. That is narrower than the
/// hole it closes — the hole was two drops in DIFFERENT functions sharing a row, which the `fn`
/// column now separates.
///
/// ⚠ **Read that against the rows below rather than against a summary of them.** What keeps two
/// drops in one function apart is the TOKEN — the original key doing its job — and the blind spot is
/// therefore the pair a function holds whose tokens are identical. Two `credential_old;` drops in
/// `db.rs` were once exactly that pair one column further out, and the `fn` column separated them.
const TABLE_DROP_PIN: [(&str, &str, &str, &str); 2] = [
    (
        "db/venues_tests.rs",
        "ensure_venue_rows_creates_the_table_on_a_store_that_predates_it",
        "venue;",
        "simulating a store older than the table — a fixture, and the mark it destroys is the \
         fixture's own",
    ),
    (
        "profile_store/ddl_tests.rs",
        "an_unrenderable_vocabulary_is_refused",
        "mount;",
        "not a statement at all: a SQL-injection probe STRING in `profile_ddl`'s vocabulary test, \
         which the `CHECK` list quotes rather than executes",
    ),
];

/// Every `.rs` file under this crate's `src/`, as a path relative to it.
///
/// ⚠ `CARGO_MANIFEST_DIR` is the tree the COMPILER ran in, which is the defect
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` exists to refuse — in `src/`. Here it is
/// correct and is the ordinary shape for a gate that reads the repository: a test binary is only
/// ever run from the tree it was built in.
fn source_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("this crate's own src/ is readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src/")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("readable")));
            }
        }
    }
    out.sort();
    assert!(!out.is_empty(), "the walk found no source at all, so the scan below is vacuous");
    out
}

/// The name of the `fn` a line DECLARES, if it declares one — the scan's third key column.
///
/// Deliberately a declaration scan rather than a brace-depth parser: this reads Rust source as
/// TEXT, like every other gate in this tree, and the innermost `fn` declared above a line is the
/// answer wanted at every site the pin carries. Visibility and the `const`/`async`/`unsafe`
/// qualifiers are stripped so `pub(crate) async fn foo` answers `foo`.
fn declared_fn(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    loop {
        let stripped = ["pub(crate)", "pub(super)", "pub", "default", "const", "async", "unsafe"]
            .iter()
            .find_map(|kw| rest.strip_prefix(kw)?.strip_prefix(' '));
        match stripped {
            Some(next) => rest = next.trim_start(),
            None => break,
        }
    }
    let name: String = rest
        .strip_prefix("fn ")?
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// `(enclosing fn, table token)` for every table drop in ONE source text.
///
/// Split out from [`table_drops`] so the distinction it draws can be measured on PLANTED source
/// rather than on whatever `db.rs` happens to hold this week — see
/// [`the_drop_scan_distinguishes_two_identical_drops_in_one_file`].
///
/// Lines whose first non-whitespace bytes are `//` are skipped: a comment drops nothing, and this
/// crate's prose names the statement repeatedly. That skip runs BEFORE the `fn` detection, so a
/// `fn` named inside a doc comment cannot re-key the sites below it.
fn drops_in(text: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    // ⚠ The sentinel for a drop above the first `fn`. It is a spelling no `fn` can have, so a row
    // carrying it is visibly "at file scope" rather than silently attributed to nothing.
    let mut current = "<file scope>".to_string();
    for line in text.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        if let Some(name) = declared_fn(line) {
            current = name;
        }
        for (idx, _) in line.match_indices(DROPS_TABLE) {
            // ⚠ `IF EXISTS` is stepped over rather than read as the table. Without this the
            // token recorded for `DROP TABLE IF EXISTS venue` is the word `IF`, so two such
            // sites in one file collapse onto ONE pin row and the second is invisible — the
            // exact shape of hole this ratchet exists to close. Measured while killing this
            // test with a planted drop.
            let rest = line[idx + DROPS_TABLE.len()..].trim_start();
            let rest = rest.strip_prefix("IF EXISTS ").unwrap_or(rest);
            // ⚠ The token ENDS AT ITS OWN `;`, and that is not cosmetic. A drop inside a
            // multi-statement `format!` has no whitespace after the semicolon — the next byte
            // is the `\n` ESCAPE, two characters of source — so a scan that stopped only at
            // whitespace recorded `{table};\nALTER` and the pin row for the same site changed
            // its spelling the moment its author put a second statement on the line. Measured
            // on the `sim` -> `paper` rebuild (deleted since), twice, while this gate was being
            // written.
            let mut token = String::new();
            for c in rest.chars() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                token.push(c);
                if c == ';' {
                    break;
                }
            }
            if !token.is_empty() {
                out.insert((current.clone(), token));
            }
        }
    }
    out
}

/// `(file, enclosing fn, table token)` for every table drop this crate's `src/` performs.
fn table_drops() -> BTreeSet<(String, String, String)> {
    source_files()
        .into_iter()
        .flat_map(|(file, text)| {
            drops_in(&text).into_iter().map(move |(func, token)| (file.clone(), func, token))
        })
        .collect()
}

/// **The `fn` column is a real distinction, held on PLANTED source.**
///
/// The pin was keyed on `(file, token)` and `db.rs` then grew a second `DROP TABLE
/// credential_old;` in a different test, which collapsed onto the first row and was classified by
/// nobody. This asserts the scan now tells the two apart — and deliberately does NOT read `db.rs`
/// to do it, because a guard that rests on today's source stops guarding the moment somebody
/// rewrites that file.
#[test]
fn the_drop_scan_distinguishes_two_identical_drops_in_one_file() {
    // ⚠ The statement is COMPOSED from [`DROPS_TABLE`], not spelled — same reason that constant is
    // composed. This file must stay free of the literal needle so that widening the scan's root to
    // `tests/` some day cannot make this fixture answer as a real drop.
    let planted = format!(
        "fn first_test() {{\n    \
             conn.execute_batch(\"{DROPS_TABLE}credential_old;\").unwrap();\n\
         }}\n\
         \n\
         /// fn a_doc_comment_naming_a_fn_must_not_re_key_anything() {{\n\
         // conn.execute_batch(\"{DROPS_TABLE}a_commented_out_drop;\");\n\
         pub(crate) async fn second_test() {{\n    \
             conn.execute_batch(\"{DROPS_TABLE}IF EXISTS credential_old;\").unwrap();\n\
         }}\n"
    );
    let observed = drops_in(&planted);
    assert_eq!(
        observed,
        BTreeSet::from([
            ("first_test".to_string(), "credential_old;".to_string()),
            ("second_test".to_string(), "credential_old;".to_string()),
        ]),
        "two identical drops in two functions must be TWO rows — one row here is the hole this \
         column exists to close, and a row naming `a_doc_comment_naming_a_fn_must_not_re_key_\
         anything` or `a_commented_out_drop` means the comment skip stopped running first"
    );
}

#[test]
fn every_table_drop_in_this_crates_source_is_classified() {
    let pinned: BTreeSet<(String, String, String)> = TABLE_DROP_PIN
        .iter()
        .map(|(file, func, token, _)| {
            ((*file).to_string(), (*func).to_string(), (*token).to_string())
        })
        .collect();
    let observed = table_drops();
    let added: Vec<String> = observed
        .difference(&pinned)
        .map(|(f, func, t)| format!("  {f}: `{func}` drops `{t}`"))
        .collect();
    assert!(
        added.is_empty(),
        "\nNEW table drop(s) in this crate's source, and a table drop is what deletes a \
         `sqlite_sequence` high-water mark (spec §4.1):\n\n{}\n\nIf this is part of a REBUILD of a \
         table `SEQUENCE_PIN` calls `Armed`, the rebuild must carry that mark across or every id \
         above the top SURVIVING row is handed out a second time: read the mark BEFORE the drop \
         and put it back after the replay. `rebuild_preserving_ids` in this file measures the mark \
         carry and `MarkCarry` is the one decision in it, but it is the rename-ASIDE shape and its \
         pragma pair does NOT transfer: read its doc before lifting anything out of it. Then add \
         the row here with the argument for why this drop cannot lose a mark.\n\npinned {}, \
         observed {}\n",
        added.join("\n"),
        TABLE_DROP_PIN.len(),
        observed.len(),
    );
}

#[test]
fn the_table_drop_pin_has_no_stale_rows() {
    let observed = table_drops();
    let gone: Vec<String> = TABLE_DROP_PIN
        .iter()
        .filter(|(file, func, token, _)| {
            !observed.contains(&((*file).to_string(), (*func).to_string(), (*token).to_string()))
        })
        .map(|(file, func, token, _)| format!("  {file}: `{func}` drops `{token}`"))
        .collect();
    assert!(
        gone.is_empty(),
        "\n`TABLE_DROP_PIN` names drops this crate's source no longer performs:\n\n{}\n\nDelete \
         those lines and decrement the declared array length. ⚠ A file RENAME lands here too — \
         these rows are keyed by path, so re-key the row rather than deleting it. ⚠ So does a \
         function RENAME, for the same reason: the second key column is the enclosing `fn`, and a \
         row whose drop merely moved house is a re-key and not a deletion.\n",
        gone.join("\n"),
    );
}
