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
/// ⚠ **This used to add "no production path rebuilds an armed table today, so there is no
/// behaviour to assert", and stage 4a made that false** — the module doc was swept for it and this
/// pin was not, which is the worse of the two places to leave it: the gate's own failure message
/// sends the next author HERE. `crates/vike-secrets/src/schema/rebuild.rs`'s `rebuild_table_from_ddl` IS
/// that production path, it rebuilds tables the shipped `DDL` arms, and
/// [`the_paper_tier_rebuild_does_not_rewind_the_account_marks`] is the behaviour assertion that
/// reddens when its mark carry is deleted — measured by deleting it. So the duty this pin records
/// is now BOTH: classify the next drop, and make the rebuild around it carry the mark.
///
/// ⚠ **The `fn` column is part of the KEY, not decoration.** The pin was keyed on `(file, token)`
/// alone, and `db.rs` grew a SECOND `DROP TABLE credential_old;` in a different test — two sites
/// collapsing onto one row, which is the exact hole the `IF EXISTS` and `;` handling in
/// [`drops_in`] exists to close, re-opened one column further out.
/// [`the_drop_scan_distinguishes_two_identical_drops_in_one_file`] holds the distinction on planted
/// source rather than on whatever `db.rs` happens to contain.
///
/// ⚠ **It has already caught one, on the day it landed.** The `sim` -> `paper` rebuild
/// (`crates/vike-secrets/src/schema/steps/sim_to_paper.rs`'s `migrate_sim_tier_to_paper`) arrived on the same branch
/// hours later, reddened this test, and its author read the message and made that rebuild carry the
/// mark. This pin was deliberately NOT written in advance for it: pre-blessing a site whose final
/// form nobody had read is the one thing a ratchet must not do.
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
/// ⚠ **Read that against the rows below rather than against a summary of them.** This sentence
/// used to end *"and is the shape the rows below are actually in — one drop per function, every one
/// of them"*, which the pin's own contents contradict: `rebuild_table_from_ddl` holds TWO drops and
/// has TWO rows, and the `{scratch};` row says so in its own words. What actually keeps those two
/// apart is the TOKEN — the original key doing its job — and the blind spot is therefore the pair a
/// function holds whose tokens are identical. `db.rs`'s two `credential_old;` drops were exactly
/// that pair one column further out, and they are separated here by the `fn` column instead.
const TABLE_DROP_PIN: [(&str, &str, &str, &str); 7] = [
    (
        "db/venues_tests.rs",
        "ensure_venue_rows_creates_the_table_on_a_store_that_predates_it",
        "venue;",
        "simulating a store older than the table — a fixture, and the mark it destroys is the \
         fixture's own",
    ),
    (
        "db/venues_tests.rs",
        "ensure_venue_id_columns_skips_a_credential_table_with_no_venue_column",
        "credential_old;",
        "rebuilding `credential` by rename-copy-drop to plant a table with no `venue` column. ⚠ \
         This row used to argue *`credential` is not armed, so there is no mark for the drop to \
         take*, and `2f677beaa` made that false — the shipped `DDL` declares `AUTOINCREMENT` on \
         `credential`. What holds now is the fixture argument: the plant is built on a store this \
         test created moments earlier, the copy carries every id explicitly, and no row has been \
         removed — so the mark the drop takes is the fixture's own and the replay leaves it where \
         it was",
    ),
    (
        "db/venues_tests.rs",
        "the_autoincrement_rebuild_normalizes_a_credential_table_that_lost_its_venue_columns",
        "credential_old;",
        "THE SECOND SITE, and it had no row of its own until the `fn` column existed — it planted \
         itself behind the row above and this ratchet counted one where there were two. It plants \
         the same shape UNARMED (`INTEGER PRIMARY KEY`, no `AUTOINCREMENT`), so the table the drop \
         leaves behind holds no mark at all, and the store is this test's own",
    ),
    (
        "profile_store/ddl_tests.rs",
        "an_unrenderable_vocabulary_is_refused",
        "mount;",
        "not a statement at all: a SQL-injection probe STRING in `profile_ddl`'s vocabulary test, \
         which the `CHECK` list quotes rather than executes",
    ),
    (
        "schema/rebuild.rs",
        "rebuild_table_from_ddl",
        "{scratch};",
        "the `DROP TABLE IF EXISTS` that clears a LEFTOVER scratch table before building the new \
         shape under that name. It takes no mark that matters: a scratch table only ever exists \
         inside a rebuild that did not finish, so its `sqlite_sequence` row is a copy of the live \
         table's made moments earlier and the live table still holds its own. ⚠ This row is also \
         the second site in one file the `fn` key is NOT needed for — it and the `{table};` row \
         below share a function and are told apart by the token, which is the original key doing \
         its job",
    ),
    (
        "schema/rebuild.rs",
        "rebuild_table_from_ddl",
        "{table};",
        "the drop of the LIVE table — the ONE rebuild procedure in this \
         crate, called by `migrate_sim_tier_to_paper` (§4.4), by \
         `migrate_tables_onto_autoincrement` (§4.1), by `migrate_dropped_columns` (§9 stage 4c), \
         by `migrate_venue_setting_tier_to_any` (§5.2 step 7) and by \
         `migrate_venue_links_onto_venue_id` (ruling 3's venue links) — ⚠ this named only the \
         first two until step 7 added the fourth; stage 4c's had been missing from it since it \
         landed, and the venue links' pass was missing from it the same way until the final review \
         of that plan counted it, which is the omission this sentence records happening twice. \
         None of them added a second `DROP TABLE`, which is why no row here grew. It builds the \
         new table beside the old and \
         drops the original rather than renaming it aside, so this statement takes the real \
         table's `sqlite_sequence` row, and it CARRIES the mark: read before the rebuild, put back \
         after it — the `MarkCarry::Carried` shape spelled in production. ⚠ THE DECLARED RESIDUAL \
         THAT STOOD HERE IS PAID. It read: *neither table it touches is armed today, so that carry \
         is a no-op and NO test can fail without it*. Stage 4 armed five more tables including \
         both of §4.4's, and `the_paper_tier_rebuild_does_not_rewind_the_account_marks` is the \
         behaviour assertion that now fails when the carry is deleted — measured by deleting it",
    ),
    (
        "schema/reshape.rs",
        "reshape_into",
        "{RESHAPE_SCRATCH};",
        "the drop of the renamed SCHEMA-1 `credential`. ⚠ Read the qualifier: it is the schema-1 \
         shape (`name TEXT PRIMARY KEY NOT NULL`, `crates/vike-secrets/src/db/migrate/preview.rs`'s `SCHEMA_1`) \
         that declares no `AUTOINCREMENT` and therefore hands the drop no mark — the SHIPPED \
         `credential` has been armed since `2f677beaa`, and a row that said only *`credential` \
         declares no `AUTOINCREMENT`* would now read as a claim about the wrong table. \
         `the_real_reshape_does_not_rewind_an_armed_tables_mark` holds it measured rather than \
         argued",
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
            // on `migrate_sim_tier_to_paper`, twice, while this gate was being written.
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
         above the top SURVIVING row is handed out a second time. The PRODUCTION procedure is \
         `crates/vike-secrets/src/schema/rebuild.rs`'s `rebuild_table_from_ddl` — copy that one. \
         `rebuild_preserving_ids` in this file measures the mark carry and `MarkCarry` is the one \
         decision in it, but it is the rename-ASIDE shape and its pragma pair does NOT transfer: \
         read its doc before lifting anything out of it. Then add the row here with the argument \
         for why this drop cannot lose a mark.\n\npinned {}, observed {}\n",
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
