use super::*;

/// A schema-current store holding the named credential rows and nothing else.
fn store(rows: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("vike.db");
    let (conn, _created, _v) = open_for_write(&db).expect("open");
    conn.execute_batch(crate::schema::DDL).expect("ddl");
    for (name, value) in rows {
        conn.execute(
            "INSERT INTO credential (name, field, value) VALUES (?1, ?2, ?3)",
            (name, "F", value),
        )
        .expect("insert");
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).expect("stamp");
    drop(conn);
    (dir, db)
}

// ⚠ The two legacy names are INVENTED, not the real dukascopy pair, and that is forced rather
// than lazy: a whole `PREFIX_NAME` literal in a `src/` file is read by
// `crates/vike-ops/tests/settings_registry.rs` sweep as evidence that THIS CRATE reads that
// variable, and rows are keyed `(name, krate)` — a row under the bridge that owns the key does
// not declare a read here. Nothing in this test depends on the names being real: what it drives
// is TWO names collapsing onto ONE key and the fold handing both back.
const D1: &str = "legacy-name-one";
const D2: &str = "legacy-name-two";
const KEY: &str = "venue.dukascopy.demo.server";
/// A credential row that does NOT move — the control. ⚠ Deliberately NOT env-SHAPED: a whole
/// `PREFIX_NAME` literal in a `src/` file is read by
/// `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence that THIS CRATE reads that
/// variable, and an invented one has no row to declare it. The two dukascopy names above are
/// real store keys and carry rows; this one had to be invented, so it wears no prefix at all.
const UNMOVED: &str = "a-row-that-does-not-move";

/// What the production planner answers, spelled here as the two rows this test drives: the
/// dukascopy pair onto ONE key, and an untouched credential onto `None`.
///
/// ⚠ NOT named for the thing it stands in for. `the_two_entry_points_share_one_classifier`
/// counts occurrences of that name in THIS FILE and requires exactly one, so a helper wearing
/// it is a second match and reddens a gate about something else entirely.
fn moves_to(name: &str) -> Option<String> {
    (name == D1 || name == D2).then(|| KEY.to_string())
}

/// `moves_to`'s inverse — the dotted key as the COLUMNS `venue_setting` holds. Production
/// passes `crate::parse_venue_setting_key`, which this crate has owned since 2026-09-22; this
/// stands in for it over the one INVENTED key this module drives, so the fixture's names stay
/// free of a real `{PREFIX}_{NAME}` literal (see `D1`/`D2` above for why that matters here).
fn split(key: &str) -> Option<VenueSettingKey> {
    (key == KEY).then(|| ("dukascopy".to_string(), Some("demo".to_string()), "SERVER".to_string()))
}

/// ⚠ **THE ONE-TO-MANY MOVE.** Two credential rows holding the SAME value collapse onto ONE
/// settings row, and the two credential rows are gone from the table afterwards.
///
/// ⚠ **The READ half of this claim moved out of this file on 2026-09-22**, with the fold
/// itself, and went with the fold on decision 0095's Task 7: the moved row is read through
/// `crate::venue_setting::VenueSettings` now (both demo accounts read the one
/// `venue.dukascopy.demo.server` row), and no credential map carries either legacy name — which
/// `crates/vike-secrets/tests/venue_setting_fold.rs` pins. A test there named
/// `a_moved_one_to_many_row_answers_for_both_legacy_names` proved the old read half until then.
#[test]
fn two_rows_collapse_onto_one_key() {
    let (_d, db) = store(&[(D1, "jforex-demo"), (D2, "jforex-demo"), (UNMOVED, "untouched")]);
    let moved = move_pending_rows(&db, &moves_to, &split, BTreeMap::new(), false).expect("move");
    assert!(!moved.refused(), "nothing should refuse: {moved:?}");
    assert_eq!(moved.keys, vec![KEY.to_string()], "nine rows, not ten");
    assert_eq!(moved.names.len(), 2, "…out of two credential rows: {moved:?}");

    // The credential table no longer holds them…
    let raw = read_table(&db, Table::Credential).expect("read").into_map();
    assert!(!raw.contains_key(D1), "moved out of `credential`");
    assert!(!raw.contains_key(D2), "…and so is the second of the pair");
    assert_eq!(raw.get(UNMOVED).map(String::as_str), Some("untouched"));
}

/// ⚠ **A DIVERGENCE REFUSES AND WRITES NOTHING.** The ten names are nine rows only because the
/// dukascopy pair was MEASURED equal. Where they differ, one row cannot express both and
/// picking either would hand one account the other's JForex server — so nothing is picked.
#[test]
fn two_rows_that_disagree_refuse_the_whole_move() {
    let (_d, db) = store(&[(D1, "jforex-demo-one"), (D2, "jforex-demo-two")]);
    let moved = move_pending_rows(&db, &moves_to, &split, BTreeMap::new(), false).expect("move");
    assert!(moved.refused(), "a divergence must refuse: {moved:?}");
    assert_eq!(moved.divergent.len(), 1, "…and name the key: {moved:?}");
    assert!(moved.keys.is_empty() && moved.names.is_empty(), "NOTHING written: {moved:?}");
    // Proof rather than promise: both rows are still where they were.
    let raw = read_table(&db, Table::Credential).expect("read").into_map();
    assert_eq!(raw.len(), 2, "both credential rows survive: {raw:?}");
}

/// ⚠ **A COLLISION REFUSES TOO**, and it is handed in already evaluated — the check is
/// `vike_bridge_core`'s, because composing a legacy name needs the venue roster this crate
/// cannot depend on.
#[test]
fn a_collision_refuses_the_whole_move() {
    let (_d, db) = store(&[(D1, "jforex-demo")]);
    let collisions: BTreeMap<String, String> =
        [(D2.to_string(), KEY.to_string())].into_iter().collect();
    let moved = move_pending_rows(&db, &moves_to, &split, collisions, false).expect("move");
    assert!(moved.refused() && moved.keys.is_empty(), "nothing written: {moved:?}");
    assert_eq!(read_table(&db, Table::Credential).expect("read").into_map().len(), 1);
}

/// A dry run answers the WHOLE verdict and writes nothing — the shape `config adopt` has, for
/// the same reason: this touches the only copy of a box's venue keys.
#[test]
fn a_dry_run_reports_everything_and_writes_nothing() {
    let (_d, db) = store(&[(D1, "jforex-demo"), (D2, "jforex-demo")]);
    let moved = move_pending_rows(&db, &moves_to, &split, BTreeMap::new(), true).expect("dry run");
    assert_eq!(moved.keys, vec![KEY.to_string()], "the verdict is complete");
    assert_eq!(moved.names.len(), 2);
    assert_eq!(read_table(&db, Table::Credential).expect("read").into_map().len(), 2);
}

/// ⚠ **THE MOVE WRITES `venue_setting` AND LEAVES `setting` EMPTY**, which is the regression
/// this whole change exists to prevent from coming back.
///
/// It went to `setting` under `config.venue.<venue>…` until 2026-09-21 and reached production
/// that way. `vike_config::Config` carries `#[serde(deny_unknown_fields)]` and has no `venue`
/// field, so the whole `config` section stopped deserializing — which took the ARMING CEILING
/// with it and read every venue as `paper` on the CI box. Asserting the destination is not fussiness:
/// the fold test above would pass just as happily against the old table.
#[test]
fn the_destination_is_the_column_table_and_setting_is_untouched() {
    let (_d, db) = store(&[(D1, "jforex-demo"), (D2, "jforex-demo")]);
    move_pending_rows(&db, &moves_to, &split, BTreeMap::new(), false).expect("move");

    let (conn, _c, _v) = open_for_write(&db).expect("open");
    let cols: (String, Option<String>, String, String) = conn
        .query_row("SELECT venue, tier, field, value FROM venue_setting", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .expect("exactly one venue_setting row");
    assert_eq!(
        cols,
        (
            "dukascopy".to_string(),
            Some("demo".to_string()),
            "SERVER".to_string(),
            "jforex-demo".to_string()
        ),
        "the columns are what the splitter produced, and the value is VERBATIM — no JSON \
             quoting, because nothing parses this column"
    );
    let settings: i64 =
        conn.query_row("SELECT count(*) FROM setting", [], |r| r.get(0)).expect("count setting");
    assert_eq!(
        settings, 0,
        "the move wrote a `setting` row — that is the shape that stopped `config` \
             deserializing on the CI box and capped every venue at paper"
    );
}

/// ⚠ **A key the splitter cannot read back refuses the WHOLE move and writes nothing.**
///
/// It cannot happen while both closures come from `vike_bridge_core::credentials` — one renders
/// the grammar the other parses. It is refused rather than guessed at because the alternative
/// is filing a row under invented columns that no reader will ever find, while DELETING the
/// credential row that was working.
#[test]
fn a_key_the_splitter_cannot_read_refuses_everything() {
    let (_d, db) = store(&[(D1, "jforex-demo"), (D2, "jforex-demo"), (UNMOVED, "kept")]);
    // A splitter that answers for nothing — the disagreement, planted.
    let blind = |_: &str| None;
    let moved =
        move_pending_rows(&db, &moves_to, &blind, BTreeMap::new(), false).expect("no error");

    assert!(moved.refused(), "a key nothing can split must refuse: {moved:?}");
    assert_eq!(moved.unsplittable, vec![KEY.to_string()], "reported by key");
    assert!(moved.keys.is_empty(), "nothing is reported as moved");
    // …and the store is exactly as it was. All three rows, including the two that WOULD have
    // moved under a working splitter.
    let live = read_table(&db, Table::Credential).expect("read").into_map();
    assert_eq!(live.len(), 3, "a refused move wrote nothing: {live:?}");
    for name in [D1, D2, UNMOVED] {
        assert!(live.contains_key(name), "{name} was deleted by a refused move");
    }
}

// ⚠ `a_collision_at_read_time_keeps_the_credential_value_and_says_so` lived here and MOVED to
// `crates/vike-secrets/tests/venue_setting_fold.rs` on 2026-09-22, with the fold it was about.
// It is the same claim — a settings row rendering a name a live credential row still holds
// resolves to what the box resolved BEFORE the move — driven through
// `crate::store::resolve_store_in` and the REAL renderer instead of this module's stub.
