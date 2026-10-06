//! **The SCOPED credential read** — owner ruling 2026-09-16, a process materialises only the keys
//! it asked for.
//!
//! | test | the property |
//! |---|---|
//! | [`an_undeclared_name_is_not_an_absent_credential`] | the one this whole design turns on: *never declared* and *not in the store* are DIFFERENT answers, and only the second is the live gate |
//! | [`an_undeclared_name_stays_undeclared_when_the_store_holds_it`] | the refusal is about the SCOPE, not about the store's contents — a name present on disk and outside the scope still refuses |
//! | [`a_scoped_read_answers_exactly_like_the_whole_table_read_name_for_name`] | the scoped `SELECT` reproduces `read_table`'s widened `superseded_at` predicate and its last-wins fold — including a TIER ALIAS, the row a flat clause silently dropped |
//! | [`a_scoped_read_materialises_only_the_declared_names`] | both backends: nothing outside the scope reaches the process |
//! | [`the_file_arm_and_the_database_arm_answer_alike`] | migrating a box does not change what a scoped caller sees |
//! | [`an_unreadable_store_is_still_loud_under_a_scope`] | a store that EXISTS and will not open is an ERROR, not an empty scope — the rule the root `CLAUDE.md` states, carried onto the new path |
//! | [`an_absent_store_is_the_live_gate_under_a_scope`] | ...and its opposite: no store is silent, every declared name absent |
//! | [`the_store_findings_survive_the_narrowing`] | `source` / `shadowed` reach a scoped caller unchanged, so `vike-cli secrets`-shaped reporting is not lost by scoping |
//! | [`an_empty_scope_reads_nothing_and_refuses_everything`] | the degenerate declaration is not a whole-store read wearing a scope |
//! | [`the_presence_read_agrees_with_the_value_read_name_for_name`] | the NAMES-ONLY read says "present" exactly when a value read finds a non-blank value, on both backends — so a presence answer can never disagree with the reader it stands in for |
//! | [`an_unreadable_store_is_an_error_for_the_presence_read_and_an_absent_one_is_empty`] | the presence read keeps the loud arm and the silent arm apart, as every reader here does |
//!
//! ⚠ **Key NAMES only.** Every value in this file is `support::fake_value_folding_mainnet`'s
//! derived string; no real credential exists anywhere near it, and nothing here prints a value that
//! came out of a store.
//!
//! ⚠ **No credential name is spelled inside a `.get(` argument here.** `crates/vike-ops/src/scan.rs`
//! resolves a literal or crate-`const` key at a `.get(` site into PROOF that this crate reads that
//! variable, which would demand a `vike_ops::settings::SETTINGS` row per name for `vike-secrets` —
//! rows asserting reads that do not happen. Names reach the lookups through `let` bindings and
//! slices, which that scanner deliberately cannot resolve. The sibling `database_migration.rs` is
//! green for the same reason.

use std::collections::BTreeSet;

use vike_secrets::{KeyScope, Lookup, Source, Table};

use crate::support::{self, Fixture, Rule};

/// A settings directory carrying a credential file holding `keys`, and nothing else until a test
/// migrates it.
///
/// Each value is `fake_value_folding_mainnet`'s: the two TIER SPELLINGS of one credential share a
/// value there, which is what files `ASTER_MAINNET_PRIVATE_KEY` as an ALIAS of its `_LIVE_` twin
/// instead of dropping it — see that helper's doc.
fn fixture_with(keys: &[&str]) -> Fixture {
    Fixture::with_store_text(&support::hand_edited_store_text(
        keys.iter().map(|k| (*k, support::fake_value_folding_mainnet(k))),
    ))
}

/// The account classification the production callers pass
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it.
///
/// Deliberately SIMPLER than the production one: it knows only `{VENUE}_{TIER}_{FIELD}`, which is
/// all the shape this file's assertions turn on. `MAINNET` folds onto the canonical `live` tier,
/// which is what makes a `_MAINNET_` name a TIER ALIAS of its `_LIVE_` twin — the row
/// `a_scoped_read_answers_exactly_like_the_whole_table_read_name_for_name` exists for.
const RULES: &[Rule] = &[
    Rule::split("_SIM_").account_of_head("sim"),
    Rule::split("_DEMO_").account_of_head("demo"),
    Rule::split("_LIVE_").account_of_head("live"),
    Rule::split("_MAINNET_").account_of_head("live"),
];

fn classify(name: &str) -> vike_secrets::Classification {
    support::classify_by(RULES, name)
}

/// Fill the settings database from the files, leaving both byte-identical (the migration's own
/// contract — this file asserts nothing about that, `database_migration.rs` does).
fn migrate(fx: &Fixture) {
    fx.migrate_with(support::is_node_key, &classify);
}

/// The fixture store's names: two venues at two tiers, plus the `_MAINNET_`/`_LIVE_` pair whose
/// alias behaviour the widened `superseded_at` predicate exists for.
const STORE_KEYS: [&str; 6] = [
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "OKX_DEMO_API_KEY",
    "OKX_DEMO_API_SECRET",
    "ASTER_LIVE_PRIVATE_KEY",
    "ASTER_MAINNET_PRIVATE_KEY",
];

/// One value out of a scoped answer, or `None` — panicking if the name was never declared, which
/// no test below reaches by accident.
fn value(scoped: &vike_secrets::ScopedSecrets, name: &str) -> Option<String> {
    match scoped.get(name).declared() {
        Ok(v) => v.map(str::to_string),
        Err(u) => panic!("{u}"),
    }
}

// ---------------------------------------------------------------------------------------------
// ⚠ THE PROPERTY THE WHOLE DESIGN TURNS ON
// ---------------------------------------------------------------------------------------------

/// **A name the caller never DECLARED must not look like a credential that is not in the store.**
///
/// In this workspace absent credentials ARE the live gate: a venue whose keys cannot be found stays
/// on PAPER, silently and by design. So a scoped read that answered `None` for an undeclared name
/// would let a forgotten declaration drop a LIVE venue to paper with no error, showing the operator
/// exactly what a correct fresh install shows — a silent trading outage wearing the costume of a
/// correct default.
///
/// The two answers are therefore different VARIANTS, and the only conversion out of them is a
/// `Result`, so no caller can fold one into the other by writing `.ok()` or `?` on an `Option`.
///
/// ⚠ **Mutation-proved**: making `ScopedSecrets::get` return `Lookup::AbsentFromStore` for an
/// undeclared name — the exact production change a "simplification" would make — reddens this test
/// on the first assertion, naming the key.
#[test]
fn an_undeclared_name_is_not_an_absent_credential() {
    let fx = fixture_with(&STORE_KEYS);
    let declared_present = STORE_KEYS[0];
    let declared_absent = "BINANCE_SIM_API_KEY";
    let never_declared = "OKX_DEMO_API_SECRET";
    let scoped = vike_secrets::resolve_project_scoped(
        fx.arg(),
        &KeyScope::of([declared_present, declared_absent]),
    )
    .expect("the file store opens");

    assert!(
        matches!(scoped.get(never_declared), Lookup::NotDeclared(_)),
        "{never_declared} was never declared, so the store was not asked — this must NOT be the \
         same answer as a credential that is absent, which is the LIVE GATE"
    );
    assert_eq!(
        scoped.get(declared_absent),
        Lookup::AbsentFromStore,
        "{declared_absent} WAS declared and the store does not hold it — this one is the live gate"
    );
    assert_eq!(
        scoped.get(declared_present),
        Lookup::Present(&support::fake_value_folding_mainnet(declared_present)),
        "a declared name the store holds comes back verbatim"
    );

    // ...and the same three states through the ONE conversion, which is a `Result` precisely so
    // that the undeclared arm cannot be reached by a caller that only wrote an `Option` match.
    assert_eq!(
        scoped.get(declared_present).declared().unwrap(),
        Some(support::fake_value_folding_mainnet(declared_present).as_str())
    );
    assert_eq!(scoped.get(declared_absent).declared().unwrap(), None);
    let refused = scoped.get(never_declared).declared().expect_err("an undeclared name is an Err");
    assert_eq!(refused.name, never_declared);
    assert!(
        refused.to_string().contains("NOT an absent credential"),
        "the refusal has to SAY which of the two it is, or an operator reads it as the live gate: {refused}"
    );
    assert!(
        refused.declared.iter().any(|d| d == declared_present),
        "the refusal names the scope that WAS declared, so the fix is visible from the message"
    );
}

/// The refusal is about the SCOPE, not about the store's contents: a name that is sitting on disk
/// and outside the declaration still answers `NotDeclared`, because this process never asked.
///
/// ⚠ This is the half a `found`-only check would get wrong. A `get` implemented as *look in the
/// materialised map, and call a miss undeclared if the scope does not hold it* would answer
/// `Present` here whenever the narrowing leaked — so the scope is consulted FIRST, before the map.
#[test]
fn an_undeclared_name_stays_undeclared_when_the_store_holds_it() {
    let fx = fixture_with(&STORE_KEYS);
    let on_disk_but_undeclared = STORE_KEYS[2];
    let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of([STORE_KEYS[0]]))
        .expect("the file store opens");
    assert!(matches!(scoped.get(on_disk_but_undeclared), Lookup::NotDeclared(_)));
    assert!(
        !scoped.names().any(|n| n == on_disk_but_undeclared),
        "and it is not in the process at all — which is the point of the ruling"
    );
}

// ---------------------------------------------------------------------------------------------
// The scoped SELECT must not be a second, subtly different reader
// ---------------------------------------------------------------------------------------------

/// **Name for name, the scoped read answers exactly what the whole-table read answers.**
///
/// `read_table_on` carries a widened `superseded_at` predicate and an `ORDER BY name, id`
/// last-wins fold, and neither is tidiness. A TIER ALIAS — `ASTER_MAINNET_PRIVATE_KEY` beside its
/// `ASTER_LIVE_PRIVATE_KEY` twin — is filed superseded because `credential_one_live_value` admits
/// one live row per `(account_id, field)`, yet its NAME appears on no live row; a flat
/// `superseded_at IS NULL` therefore DROPPED a key the operator wrote. A scoped `SELECT` that
/// spelled the flat clause would reintroduce that defect one function over, and would do it
/// silently.
///
/// So this does not assert the clause — it folds BOTH readers over the same migrated store and
/// compares, for every name the store holds.
#[test]
fn a_scoped_read_answers_exactly_like_the_whole_table_read_name_for_name() {
    let fx = fixture_with(&STORE_KEYS);
    migrate(&fx);
    let whole = vike_secrets::read_table(&fx.db(), Table::Credential).expect("whole-table read");
    let whole_names: BTreeSet<String> = whole.keys().map(str::to_string).collect();
    assert!(
        whole_names.contains("ASTER_MAINNET_PRIVATE_KEY"),
        "the fixture must actually produce a TIER ALIAS, or this test proves nothing about the \
         widened predicate: {whole_names:?}"
    );

    let whole_map = whole.into_map();
    for name in &whole_names {
        let scoped = vike_secrets::read_table_scoped(
            &fx.db(),
            Table::Credential,
            &KeyScope::of([name.as_str()]),
        )
        .expect("scoped read")
        .into_map();
        assert_eq!(
            scoped.len(),
            1,
            "{name}: a one-name scope selects exactly one row, or the narrowing leaked"
        );
        assert_eq!(
            scoped.get(name),
            whole_map.get(name),
            "{name}: the scoped SELECT and the whole-table SELECT disagree — the shared `WHERE` \
             predicate or the last-wins ordering has drifted"
        );
    }

    // ...and the whole store, asked for at once, IS the whole store.
    let all = vike_secrets::read_table_scoped(
        &fx.db(),
        Table::Credential,
        &KeyScope::of(whole_names.iter().map(String::as_str)),
    )
    .expect("scoped read of every name")
    .into_map();
    assert_eq!(all, whole_map);
}

// ---------------------------------------------------------------------------------------------
// What a scoped process actually holds
// ---------------------------------------------------------------------------------------------

/// Nothing outside the declaration reaches the process — on BOTH backends, because the two
/// narrowings are different mechanisms (a bound `SELECT` on a migrated box, a retained-subset on a
/// file one) and only one of them can be inferred from the other.
#[test]
fn a_scoped_read_materialises_only_the_declared_names() {
    for migrated in [false, true] {
        let fx = fixture_with(&STORE_KEYS);
        if migrated {
            migrate(&fx);
        }
        let scope = KeyScope::of([STORE_KEYS[0], STORE_KEYS[4]]);
        let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &scope).expect("store opens");
        let held: BTreeSet<&str> = scoped.names().collect();
        assert_eq!(
            held,
            BTreeSet::from([STORE_KEYS[0], STORE_KEYS[4]]),
            "migrated={migrated}: the process holds the declared names and no others"
        );
        let map = scoped.into_map();
        assert_eq!(map.len(), 2, "migrated={migrated}: and the map it hands on is the same set");
        for undeclared in [STORE_KEYS[1], STORE_KEYS[2], STORE_KEYS[3], STORE_KEYS[5]] {
            assert!(
                !map.contains_key(undeclared),
                "migrated={migrated}: {undeclared} is in the store and was not asked for"
            );
        }
    }
}

/// Migrating a box must not change what a scoped caller sees — the same claim
/// `database_migration.rs` makes for the whole-table reader, made again for this one, because the
/// two narrowings are separate code paths.
#[test]
fn the_file_arm_and_the_database_arm_answer_alike() {
    let scope = KeyScope::of([STORE_KEYS[0], "BINANCE_SIM_API_KEY", STORE_KEYS[5]]);
    let file_fx = fixture_with(&STORE_KEYS);
    let before = vike_secrets::resolve_project_scoped(file_fx.arg(), &scope).expect("file store");
    assert_eq!(before.source, Source::File(file_fx.store()));

    let db_fx = fixture_with(&STORE_KEYS);
    migrate(&db_fx);
    let after = vike_secrets::resolve_project_scoped(db_fx.arg(), &scope).expect("database store");
    assert_eq!(after.source, Source::Database(db_fx.db()));

    for name in scope.names() {
        assert_eq!(
            value(&before, name),
            value(&after, name),
            "{name}: the file arm and the database arm answer differently under one scope"
        );
    }
    assert_eq!(before.names().collect::<BTreeSet<_>>(), after.names().collect::<BTreeSet<_>>());
}

// ---------------------------------------------------------------------------------------------
// The two ways to hold nothing, which must never look alike
// ---------------------------------------------------------------------------------------------

/// A store that EXISTS and cannot be READ is an ERROR, not "no credentials" — the rule the root
/// `CLAUDE.md` states, carried onto the scoped path. A permissions or schema bug wearing the
/// "not configured" answer looks exactly like a correct fresh install while every venue drops to
/// paper for a different reason.
#[test]
fn an_unreadable_store_is_still_loud_under_a_scope() {
    let fx = fixture_with(&STORE_KEYS);
    migrate(&fx);
    // A database this code will not read: the schema stamp is not one of `READABLE_SCHEMA_VERSIONS`.
    let stamped = rusqlite_stamp(&fx.db(), vike_secrets::SCHEMA_VERSION + 1);
    assert!(stamped, "the fixture must actually change the stamp, or this test asserts nothing");
    let err = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of([STORE_KEYS[0]]))
        .expect_err("a store that exists and will not open is an error, never an empty scope");
    let rendered = err.to_string();
    assert!(
        rendered.contains("vike.db"),
        "the error names the store it could not read: {rendered}"
    );
    assert!(!rendered.contains("value-for-"), "and it carries no row value: {rendered}");
}

/// ...and the opposite arm: an ABSENT store is silent and every declared name reads as the live
/// gate. That is the designed behaviour of a correct unconfigured install, and scoping must not
/// turn it into an error.
#[test]
fn an_absent_store_is_the_live_gate_under_a_scope() {
    let fx = Fixture::empty();
    let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of(STORE_KEYS))
        .expect("an absent store is an ANSWER, not a failure");
    assert_eq!(scoped.source, Source::None);
    assert!(scoped.is_empty());
    for name in STORE_KEYS {
        assert_eq!(
            scoped.get(name),
            Lookup::AbsentFromStore,
            "{name}: no store means the live gate, not a scope defect"
        );
    }
}

/// The store's FINDINGS reach a scoped caller unchanged. A scoped path that dropped them would take
/// the operator-facing mitigation for a shadowed `secrets.env` away from every binary that scopes —
/// the `warn_once` half of which `vike_bridge_core::credentials::try_load_workspace_secrets_scoped_at`
/// carries, and this is the data half it rests on.
#[test]
fn the_store_findings_survive_the_narrowing() {
    let fx = fixture_with(&STORE_KEYS);
    migrate(&fx);
    let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of([STORE_KEYS[0]]))
        .expect("store opens");
    let shadowed = scoped.shadowed.as_ref().expect(
        "the credential FILE is still on disk and the database answered — a scoped caller has to \
         be told, exactly as a whole-table one is",
    );
    assert_eq!(shadowed.file, fx.store());
    assert_eq!(shadowed.db, fx.db());
    assert!(scoped.legacy.is_none(), "a database that answered is not an absent store");
}

/// **`vike-cli secrets list`'s surface is untouched by the existence of a scoped read.**
///
/// That command's whole subject IS the store — its `source:` line, its key COUNT, its key NAMES and
/// its permission/shadow findings all come off `resolve_project`, the whole-table reader. Adding a
/// scoped one beside it must not narrow that: the listing would then report a store smaller than
/// the store, which is worse than not listing at all.
///
/// So this asserts the two readers over one store side by side — the whole one still answers with
/// EVERY name, the scoped one with the declared subset, and every provenance field the listing
/// prints is identical between them.
#[test]
fn the_whole_store_listing_surface_is_untouched_by_scoping() {
    let fx = fixture_with(&STORE_KEYS);
    migrate(&fx);
    let whole = vike_secrets::resolve_project(fx.arg()).expect("the listing reader");
    let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of([STORE_KEYS[0]]))
        .expect("the scoped reader");

    let listed: BTreeSet<&str> = whole.secrets.keys().collect();
    assert!(
        STORE_KEYS.iter().all(|k| listed.contains(k)),
        "the listing must still show every name the store holds: {listed:?}"
    );
    assert_eq!(whole.secrets.len(), STORE_KEYS.len(), "...and its COUNT is the store's count");
    assert_eq!(scoped.names().count(), 1, "while the scoped reader holds only what it declared");

    assert_eq!(whole.source, scoped.source, "the `source:` line is the same store either way");
    assert_eq!(whole.warning, scoped.warning, "and so is the permission finding");
    assert_eq!(whole.legacy, scoped.legacy);
    assert_eq!(whole.shadowed, scoped.shadowed, "and so is the shadowed-file finding");
}

/// The degenerate declaration: an empty scope is not a whole-store read wearing a scope. It reads
/// nothing and refuses every name — including one the store holds.
#[test]
fn an_empty_scope_reads_nothing_and_refuses_everything() {
    for migrated in [false, true] {
        let fx = fixture_with(&STORE_KEYS);
        if migrated {
            migrate(&fx);
        }
        let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::default())
            .expect("store opens");
        assert!(scoped.is_empty(), "migrated={migrated}");
        for name in STORE_KEYS {
            assert!(
                matches!(scoped.get(name), Lookup::NotDeclared(_)),
                "migrated={migrated}: {name} was not declared, so it is refused rather than absent"
            );
        }
    }
}

/// Blank and whitespace-only names are dropped at construction: a blank name matches no row, and
/// admitting one would let an empty `const` look like a declaration.
#[test]
fn a_blank_name_is_not_a_declaration() {
    let scope = KeyScope::of(["  ", "", "\t", " BINANCE_DEMO_API_KEY "]);
    assert_eq!(scope.len(), 1);
    assert!(scope.declares("BINANCE_DEMO_API_KEY"), "and a name is trimmed, not rejected");
    assert!(!scope.declares(""));
}

/// `Debug` shows names and counts, never a value — the same contract `SecretMap`'s `Debug` holds,
/// spelled again because `ScopedSecrets` carries its own map.
#[test]
fn the_scoped_answer_redacts_in_debug() {
    let fx = fixture_with(&STORE_KEYS);
    let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of([STORE_KEYS[0]]))
        .expect("store opens");
    let rendered = format!("{scoped:?}");
    assert!(rendered.contains(STORE_KEYS[0]), "key names are safe to print: {rendered}");
    assert!(!rendered.contains("value-for-"), "a value must never reach Debug: {rendered}");
}

// ---------------------------------------------------------------------------------------------
// The PRESENCE read — names, never values
// ---------------------------------------------------------------------------------------------

/// The name every presence test plants with a BLANK value, beside the fixture's real ones.
const BLANK_KEY: &str = "BYBIT_DEMO_API_KEY";

/// **Name for name, the presence read says "present" exactly when a value read finds a non-blank
/// value** — on BOTH backends, a blank value included, so the datahub's history-channels verb can
/// never report a token stored that the lane's own read then calls absent.
///
/// The database's blank row is an existing row blanked in place AFTER the migration, with a raw
/// `UPDATE`, because that arm's blank check is SQL (`trim(value, …) <> ''`) and is a different
/// mechanism from the file arm's `str::trim`; a migration that dropped or kept a blank row would
/// otherwise decide whether the SQL ever ran. The file arm's blank row is [`BLANK_KEY`].
#[test]
fn the_presence_read_agrees_with_the_value_read_name_for_name() {
    for migrated in [false, true] {
        let fx = fixture_with(&STORE_KEYS);
        let mut names: Vec<&str> = STORE_KEYS.to_vec();
        names.extend([BLANK_KEY, "BINANCE_SIM_API_KEY"]);
        let scope = KeyScope::of(names.iter().copied());
        if migrated {
            migrate(&fx);
            // A row the readers already SEE, its value blanked in place — the schema's own
            // constraints are untouched, so the only thing that changed is what the check reads.
            let conn = fx.conn();
            let blanked = conn
                .execute("UPDATE credential SET value = ' \t ' WHERE name = ?1", [STORE_KEYS[0]])
                .expect("blank a row");
            assert!(
                blanked > 0,
                "the fixture must plant a blank row, or the SQL check is untested"
            );
        } else {
            let file = fx.store();
            let mut text = std::fs::read_to_string(&file).expect("read fixture store");
            text.push_str(&format!("{BLANK_KEY}=  \t \n"));
            std::fs::write(&file, text).expect("append a blank row");
        }
        let present = vike_secrets::present_names_scoped_in(fx.dir(), Table::Credential, &scope)
            .expect("the store opens");
        let values = vike_secrets::resolve_store_scoped_in(fx.dir(), Table::Credential, &scope)
            .expect("the store opens");
        for name in &names {
            let readable = value(&values, name).is_some_and(|v| !v.trim().is_empty());
            assert_eq!(
                present.contains(*name),
                readable,
                "migrated={migrated}: {name} — the presence read and the value read disagree"
            );
        }
        assert!(
            present.iter().all(|n| scope.declares(n)),
            "migrated={migrated}: a presence answer names only declared names: {present:?}"
        );
        assert!(
            !present.iter().any(|n| n.contains("value-for-")),
            "migrated={migrated}: the answer is NAMES — no value may reach it: {present:?}"
        );
        assert!(
            present.contains(STORE_KEYS[1]),
            "migrated={migrated}: guard — a stored, non-blank key must read present: {present:?}"
        );
    }
}

/// A store that EXISTS and will not read is an ERROR for the presence read too — never an empty
/// set, which would read downstream as "nothing is stored" (0097's verdict 6). And no store at all
/// is an empty set: the live gate, silently.
#[test]
fn an_unreadable_store_is_an_error_for_the_presence_read_and_an_absent_one_is_empty() {
    let scope = KeyScope::of([STORE_KEYS[0]]);
    let fx = fixture_with(&STORE_KEYS);
    migrate(&fx);
    assert!(rusqlite_stamp(&fx.db(), vike_secrets::SCHEMA_VERSION + 1), "guard: stamp changed");
    let err = vike_secrets::present_names_scoped_in(fx.dir(), Table::Credential, &scope)
        .expect_err("a store that exists and will not open is an error, never an empty set");
    assert!(!err.to_string().contains("value-for-"), "and it carries no row value: {err}");

    let bare = Fixture::empty();
    let none = vike_secrets::present_names_scoped_in(bare.dir(), Table::Credential, &scope)
        .expect("an absent store is an ANSWER");
    assert!(none.is_empty(), "{none:?}");
}

/// Stamp a database's `PRAGMA user_version`, so a test can produce a store that EXISTS and is not
/// readable by this code. Returns whether the stamp actually changed.
///
/// A raw connection rather than a store API on purpose: no function in `vike-secrets` will write an
/// unreadable schema, which is exactly the state this fixture needs.
fn rusqlite_stamp(db: &std::path::Path, version: i64) -> bool {
    let conn = support::sql::open(db);
    support::sql::stamp_version(&conn, version);
    support::sql::user_version(&conn) == version
}
