//! **The MIRROR changes no effective value** — the gate
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1 rests on.
//!
//! Phase 1 writes the four settings files into the store and leaves **the files winning**. That is
//! the property that makes it safe to land on a live box, and 0057's own framing is that a property
//! like this has to be PROVEN rather than asserted in a comment. So this file measures it from both
//! ends, because either end alone is a green that means nothing:
//!
//! * [`rows_alone_resolve_to_exactly_what_the_files_resolve_to`] — the rows are a LOSSLESS rendering
//!   of the files. If they were not, "the files win" would still be true and the store would still
//!   be wrong, silently, until the day the files retire.
//! * [`a_store_with_no_adoption_row_changes_no_effective_value`] — files-over-rows resolves identically to files
//!   alone, `warnings` included.
//! * [`a_key_only_in_the_store_is_resolved_from_it`] — and the layer is LIVE. Without this one, both
//!   tests above would pass against a loader that ignored the store entirely, which is the
//!   "assertion that cannot fail for its stated reason" trap: the mirror would look proven and the
//!   whole phase would be a no-op.
//! * [`a_file_wins_over_a_disagreeing_row`] — the ORDER, measured on a deliberate disagreement. The
//!   pair with the one above is what says *below the files* rather than *not read*.
//!
//! Since 2026-09-18 the `value` column holds a **JSON SCALAR** rather than a TOML rendering
//! (`vike_config::mirror`), so this file also carries the two properties that change lands on:
//! [`no_pre_json_rendering_reads_back_as_a_different_value`] — an old row is refused or identical,
//! never silently different — and [`a_stale_format_row_degrades_by_name_never_guessed`], which is
//! what the refused half looks like to the loader a live daemon boots with. The fixture sweep grew
//! two directories for it, and the comment beside them says what it was blind to before.
//!
//! The four properties 0057 says a file has and a table might not are gated here too —
//! validate-on-load, unknown-key refusal BY NAME, the section vocabulary, and the roster-complete
//! arming table. The `CONSUMPTION` half is unchanged by this work and stays where it is
//! (`crates/vike-config/tests/settings_are_consumed.rs`): the mirror adds no KEY, so that gate's
//! input is the same set it was, and `config show`'s ORIGIN column is gated one level up by
//! `crates/vike-cli/tests/settings_layers_reachable.rs`, which drives the shipped binary.

use std::collections::HashMap;
use std::path::Path;

use vike_config::mirror::{SECTION_FILES, apply_rows, rows_from_files};
use vike_config::{
    Authority, CliOverrides, Origin, Policy, Settings, StoreLayer, describe_with_source, load,
    load_with_source,
};
use vike_secrets::{Adoption, ArmingRow, SETTINGS_SECTIONS, SettingRow, StoredSettings};

/// The store as an **UNADOPTED** box hands it to the loader — 0057 Phase 1's mirror, where the
/// files win and these rows sit below them.
///
/// The default, and the state of every box in the world on the day the crossing shipped. Every test
/// below that does NOT name adoption is measuring this branch, which is the branch that must stay
/// byte-identical to the pre-seal loader.
fn unadopted(rows: &StoredSettings) -> StoreLayer<'_> {
    StoreLayer::Rows { rows, adopted: None }
}

/// A seal whose counts MATCH `rows`, as `vike-cli config adopt` would have written it.
///
/// ⚠ The counts are taken from the rows rather than typed, because that is exactly what the writer
/// does — `vike_secrets::write_adoption` counts inside its own transaction — and a fixture that
/// typed them would be measuring a state the writer cannot produce. The tests that want a MISMATCH
/// build one deliberately and say so.
fn seal(rows: &StoredSettings, venues_declared: bool) -> Adoption {
    Adoption {
        adopted_at: "2026-09-18T00:00:00Z".to_string(),
        tool_version: "test".to_string(),
        files_present: String::new(),
        venues_declared,
        setting_rows: rows.settings.len(),
        arming_rows: rows.arming.len(),
    }
}

/// The store as an **ADOPTED** box hands it to the loader: these rows answer for every settings key
/// and the four files are not opened for resolution at all.
fn adopted<'a>(rows: &'a StoredSettings, seal: &'a Adoption) -> StoreLayer<'a> {
    StoreLayer::Rows { rows, adopted: Some(seal) }
}

/// **Load an adopted store and return the SEAL MARK it produced.**
///
/// ⚠ **These cases used to be `expect_err`, and the change from `Err` to a mark is the repair, not
/// a weakening.** `load_with_source` is reached by `vike_cli::run` through `resolve_policy` BEFORE
/// it routes a subcommand, so an `Err` here was `ExitCode::FAILURE` for every verb in the binary —
/// `config mirror` and `config adopt --undo`, which every one of these refusals names as its
/// repair, included. Measured with real binaries at `650907a37`; `sqlite3` is on neither box, so
/// there was no way back at all. The refusal is now carried on `Settings::seal_refusal` and
/// ENFORCED at the two verbs that act on a ceiling plus `config check`, which is what a deploy
/// pre-flight reads.
///
/// So this helper asserts BOTH halves at once, and the first is the one that regressed: the load
/// must not `Err`, and it must produce a mark.
fn seal_mark(dir: &Path, rows: &StoredSettings, seal: &Adoption, why: &str) -> String {
    let settings =
        load_with_source(Some(dir), adopted(rows, seal), &HashMap::new(), &CliOverrides::default())
            .expect(
                "the ADOPTED path must never return Err — `resolve_policy` runs before `dispatch`, \
                 so an Err here takes down every repair verb in the binary",
            );
    settings.seal_refusal.expect(why)
}

/// A settings directory laid out from `(file name, body)` pairs, in a bound `TempDir` — so nothing
/// here can reach a real project, and the tree goes even on the panic path.
fn dir_with(files: &[(&str, String)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (name, body) in files {
        std::fs::write(tmp.path().join(name), body).unwrap();
    }
    tmp
}

/// **Every settings directory this gate sweeps.** The last one is the interesting one: it is built
/// from the SERIALIZED code defaults, so it names every key the typed model can render — rather
/// than the handful somebody remembered to write out, which is how a sweep quietly stops covering
/// the field that was added last.
fn fixtures() -> Vec<(&'static str, tempfile::TempDir)> {
    let defaults = Settings::default();
    vec![
        ("an empty settings directory", dir_with(&[])),
        (
            "scalars in every file",
            dir_with(&[
                ("policy.toml", "max_leverage = 3.0\nmarket_slippage = 0.01\n".to_string()),
                ("config.toml", "log_dir = \"/var/log/vike\"\n".to_string()),
                (
                    "preferences.toml",
                    "log_level = \"debug\"\nlog_file_level = \"warn\"\n".to_string(),
                ),
                ("flags.toml", "reconcile = true\npoly_exec = false\n".to_string()),
            ]),
        ),
        (
            "a stated [venues] table",
            dir_with(&[(
                "policy.toml",
                "max_notional_per_order = 250.0\n\n[venues]\nbinance = \"demo\"\nokx = \"paper\"\n"
                    .to_string(),
            )]),
        ),
        (
            "[venues] and [accounts] together",
            dir_with(&[(
                "policy.toml",
                "[venues]\nhyperliquid = \"live\"\n\n[accounts.hyperliquid]\nALT = \"paper\"\n"
                    .to_string(),
            )]),
        ),
        (
            "a written `reconcile = false`, which the loader WARNS about",
            dir_with(&[("flags.toml", "reconcile = false\n".to_string())]),
        ),
        // ⚠ The next two fixtures were ADDED when the value column became a JSON scalar, and the
        // reason is that without them this whole sweep was blind to the change. Every fixture above
        // uses POSIX paths, plain words and plain numbers — values whose TOML and JSON renderings
        // are the same bytes — so a port that mangled a Windows `store_root`, a UNC path, an
        // embedded quote or a newline would have left every test in this file green. These are the
        // shapes `toml::Value`'s own `Display` renders as a LITERAL or MULTI-LINE string, i.e. the
        // ones that are not JSON at all, and they are carried on `Option<PathBuf>` fields because a
        // path accepts any string while `Config`'s address fields validate their contents.
        (
            "paths and strings whose TOML rendering is not a JSON scalar",
            dir_with(&[(
                "config.toml",
                r#"store_root = 'C:\vike\state'
journal_dir = '\\server\share'
log_dir = "a\"b"
"#
                .to_string(),
            )]),
        ),
        (
            "a value with a NEWLINE in it, plus non-ASCII and empty",
            dir_with(&[(
                "config.toml",
                "log_dir = \"a\\nb\"\nstore_root = \"é/ünï\"\njournal_dir = \"\"\n".to_string(),
            )]),
        ),
        ("every key the typed model renders", dir_with(&serialized(&defaults))),
    ]
}

/// **The four settings files, written out from a real [`Settings`]** — every key the model can
/// serialize, in the file each one belongs to.
///
/// `Option::None` fields simply vanish from a TOML table, which is exactly "unset", so this covers
/// every key that HAS a value and nothing that does not. It is the same derivation
/// `crates/vike-config/src/provenance.rs` uses for its effective-value lookup, and for the same
/// reason: there is no hand-written list to forget to update.
fn serialized(settings: &Settings) -> Vec<(&'static str, String)> {
    fn body<T: serde::Serialize>(v: &T) -> String {
        toml::to_string(&toml::Table::try_from(v).expect("a section serializes to a table"))
            .expect("…and renders")
    }
    vec![
        ("policy.toml", body(&settings.policy)),
        ("config.toml", body(&settings.config)),
        ("preferences.toml", body(&settings.preferences)),
        ("flags.toml", body(&settings.flags)),
    ]
}

/// The four typed sections, compared without `warnings` — `warnings` is a property of the LOAD, and
/// two of these comparisons deliberately run loads that produce different warning counts.
fn values(s: &Settings) -> (&Policy, String) {
    (&s.policy, format!("{:?}|{:?}|{:?}", s.config, s.preferences, s.flags))
}

// ---------------------------------------------------------------------------------------------
// The mirror
// ---------------------------------------------------------------------------------------------

/// **The rows ALONE resolve to what the files resolve to.** The rendering is lossless, so the store
/// a mirror run leaves behind is a faithful copy rather than a subset that happens to be invisible
/// while the files still win.
#[test]
fn rows_alone_resolve_to_exactly_what_the_files_resolve_to() {
    for (what, dir) in fixtures() {
        let from_files = load(Some(dir.path()), &HashMap::new()).expect(what);
        let rows = rows_from_files(dir.path()).expect(what);

        let mut from_rows = Settings::default();
        apply_rows(&mut from_rows, &rows).expect(what);

        assert_eq!(
            values(&from_rows),
            values(&from_files),
            "{what}: the rows must resolve to what the files resolve to. A lossy rendering is \
             invisible while the files win and is the whole store the day they retire."
        );
        // ...and the arming half, which rides its own table and is NOT part of the flat rows.
        assert_eq!(
            from_rows.policy.venues.iter().collect::<Vec<_>>(),
            from_files.policy.venues.iter().collect::<Vec<_>>(),
            "{what}: the venue ceilings"
        );
        assert_eq!(
            from_rows.policy.venues.accounts_by_venue(),
            from_files.policy.venues.accounts_by_venue(),
            "{what}: the per-account ceilings"
        );
        assert_eq!(
            from_rows.policy.venues.is_declared(),
            from_files.policy.venues.is_declared(),
            "{what}: WHETHER an operator ever stated an arming decision is itself a fact the mirror \
             must carry — a roster of all-`paper` rows written for a file that declared nothing \
             would silence `vike_mount::venue_arming_migration` for a box that has still not \
             stated one"
        );
    }
}

/// **A store with NO ADOPTION ROW changes no effective value** — 0057 Phase 1's whole point, and
/// the branch every box in the world is on until an operator runs `vike-cli config adopt`.
///
/// ⚠ **This test was named `the_mirror_changes_no_effective_value` and its name described a PHASE.**
/// The phase now has a far side, so the name says which BRANCH it pins instead. Nothing about the
/// body changed and nothing about the claim weakened — it is still *files over rows resolves
/// exactly as files alone*, `is_declared` included. What changed is that the claim is now PERMANENT
/// rather than transitional: it is the state of every dev checkout, every CI fixture, every
/// `TempDir` in this workspace, every box that has not crossed, and the state
/// `vike-cli config adopt --undo` returns a box to. It does not retire with the files.
///
/// Its far-side twin is [`adopting_changes_no_effective_value`], which is a strictly stronger claim
/// than this one ever made.
#[test]
fn a_store_with_no_adoption_row_changes_no_effective_value() {
    for (what, dir) in fixtures() {
        let before = load(Some(dir.path()), &HashMap::new()).expect(what);
        let rows = rows_from_files(dir.path()).expect(what);
        let after = load_with_source(
            Some(dir.path()),
            unadopted(&rows),
            &HashMap::new(),
            &CliOverrides::default(),
        )
        .expect(what);

        assert_eq!(
            values(&after),
            values(&before),
            "{what}: a mirrored box must resolve exactly as an unmirrored one"
        );
        assert_eq!(
            after.policy.venues.iter().collect::<Vec<_>>(),
            before.policy.venues.iter().collect::<Vec<_>>(),
            "{what}: the venue ceilings"
        );
        assert_eq!(
            after.policy.venues.is_declared(),
            before.policy.venues.is_declared(),
            "{what}: and whether they were ever stated"
        );
        assert_eq!(
            after.authority,
            Authority::Files,
            "{what}: an unsealed store leaves the FILES answering, and `config show`'s precedence \
             header is rendered from this"
        );
    }
}

/// **ADOPTING changes no effective value either** — the gate on the CROSSING itself, and a strictly
/// stronger claim than the test above ever made.
///
/// The one above proves a layer that is read and then overridden costs nothing. This proves the
/// layer resolves the SAME IMAGE with the four files not opened at all — which is the property the
/// day `policy.toml` and `flags.toml` are deleted rests on entirely, and the property
/// `vike-cli config adopt` refuses to seal a box without.
///
/// ⚠ `is_declared` is asserted EXPLICITLY, and it is the assertion that matters most here.
/// `VenuePolicy::default()` is every roster venue at `Paper`, so a file that never stated an arming
/// and a file that stated every venue as `paper` produce byte-identical MAPS — only this flag
/// separates them, and only it decides whether `vike_mount::venue_arming_migration`'s paste-ready
/// banner fires on a box that settled its arming months ago.
#[test]
fn adopting_changes_no_effective_value() {
    for (what, dir) in fixtures() {
        let before = load(Some(dir.path()), &HashMap::new()).expect(what);
        let rows = rows_from_files(dir.path()).expect(what);
        let seal = seal(&rows, before.policy.venues.is_declared());
        let after = load_with_source(
            Some(dir.path()),
            adopted(&rows, &seal),
            &HashMap::new(),
            &CliOverrides::default(),
        )
        .expect(what);

        assert_eq!(
            values(&after),
            values(&before),
            "{what}: crossing to the rows must resolve exactly what the files resolved — with the \
             files NOT READ. This is the property the deletion PR rests on."
        );
        assert_eq!(
            after.policy.venues.iter().collect::<Vec<_>>(),
            before.policy.venues.iter().collect::<Vec<_>>(),
            "{what}: the venue ceilings"
        );
        assert_eq!(
            after.policy.venues.is_declared(),
            before.policy.venues.is_declared(),
            "{what}: and whether they were ever stated — the axis that silently unmounts a box"
        );
        assert_eq!(after.authority, Authority::Store, "{what}: the STORE answered");
    }
}

/// **The files are NOT OPENED on an adopted box** — measured on a file the loader would REFUSE.
///
/// The test above proves the two resolutions agree; agreement is not evidence that the second one
/// was skipped. This one plants a `policy.toml` whose bound `vike_config::load` refuses outright,
/// and demands the adopted load succeed: a loader that still read layer 2 could not possibly pass,
/// whatever it did with the value afterwards.
///
/// It also pins the DISPOSITION such a file gets, which is the argument `crates/vike-config/src/drift.rs`
/// carries in full: a settings file on an adopted box is an INERT draft, so it is a WARNING and
/// never a refusal. Refusing here would teach `rm policy.toml` — which on an adopted box removes
/// nothing, because the row keeps deciding.
#[test]
fn an_adopted_box_does_not_open_the_files_even_to_refuse_them() {
    let dir = dir_with(&[("policy.toml", "market_slippage = 0.9\n".to_string())]);
    let rows = StoredSettings { settings: Vec::new(), arming: Vec::new() };
    let seal = seal(&rows, false);

    // Unadopted: the file is READ, and its bound refuses the boot. The control.
    load_with_source(Some(dir.path()), unadopted(&rows), &HashMap::new(), &CliOverrides::default())
        .expect_err("the control: an unadopted box READS policy.toml and this one is invalid");

    // Adopted: the file is not opened for resolution, so the load succeeds.
    let resolved = load_with_source(
        Some(dir.path()),
        adopted(&rows, &seal),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("an adopted box does not open the settings files for resolution AT ALL");
    assert_eq!(resolved.authority, Authority::Store);

    // ...and it is not silent about it: an inert file that does not even parse is reported.
    assert!(
        resolved.warnings.iter().any(|w| w.contains("INERT")),
        "an operator editing a file that changes nothing must be TOLD: {:?}",
        resolved.warnings
    );
}

/// ⚠ **Without this, the two tests above are vacuous.** A loader that ignored the store entirely
/// would pass both of them: the rows would be equal to the files by construction and the store
/// would change nothing because nothing read it. This is the test that fails if the layer is not
/// wired, and it is why it is written against a key NO FILE SETS.
///
/// **MEASURED mutation proof** (`crates/vike-config/src/load.rs`'s `load_with_source`, its
/// `apply_rows` call made unreachable, run on a throwaway branch when this file held ELEVEN tests):
/// 8 stayed green and THREE reddened — this one, [`a_bound_still_refuses_a_row`] and
/// [`a_typod_row_key_is_refused_by_name_rather_than_read_as_nothing`]. The three that redden are
/// exactly the ones that need the layer to be APPLIED; every green one measures a property of the
/// rendering or of the precedence between two layers, which a dropped layer leaves true. (The count
/// is stated as the count AT THE TIME rather than as a current one, because the file has grown
/// since and a total nobody re-measured is a number that rots.)
///
/// ⚠ This line CLAIMED "this goes red while every other test in this file stays green" before it was
/// run, and that was wrong by two. Kept as the measurement rather than the prediction, because the
/// prediction is the shape of claim a mutation proof exists to replace.
#[test]
fn a_key_only_in_the_store_is_resolved_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "log_dir".into(),
            value: "\"/from/db\"".into(),
        }],
        arming: Vec::new(),
    };

    let resolved = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(resolved.config.log_dir.as_deref(), Some(Path::new("/from/db")));

    // ...and `config show` attributes it to the store by NAME, which is the fourth provenance word.
    let d = describe_with_source(Some(dir.path()), unadopted(&store), &HashMap::new()).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").unwrap();
    assert_eq!(row.origin, Origin::Db);
    assert_eq!(row.origin.kind(), "db");
}

/// **On an UNADOPTED box the files still win**, measured on a deliberate disagreement rather than
/// inferred from the order of two `apply` calls. 0057 Phase 1's mirror, unchanged.
#[test]
fn a_file_wins_over_a_disagreeing_row_on_an_unadopted_box() {
    let dir = dir_with(&[("config.toml", "log_dir = \"/from/file\"\n".to_string())]);
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "log_dir".into(),
            value: "\"/from/db\"".into(),
        }],
        arming: Vec::new(),
    };

    let resolved = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(
        resolved.config.log_dir.as_deref(),
        Some(Path::new("/from/file")),
        "an unadopted box is MIRRORED: the store is written and the FILES STILL WIN"
    );

    let d = describe_with_source(Some(dir.path()), unadopted(&store), &HashMap::new()).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").unwrap();
    assert_eq!(row.origin, Origin::File("config.toml"));
    assert_eq!(d.authority, Authority::Files);
}

/// **...and on an ADOPTED box the ROW wins and the file is INERT.** The mutation proof of the flip
/// itself: same fixture, same disagreement, opposite verdict, decided by one row in the store.
///
/// ⚠ `config show`'s ORIGIN column must invert with the value. A table reporting `db` in the value
/// column and `config.toml` in the origin column would be the worst of both — the single most
/// misleading thing this command can print, and the reason the origin ladder branches on the same
/// probe the loader branched on rather than on a second reading of the store.
#[test]
fn a_row_wins_over_a_disagreeing_file_on_an_adopted_box() {
    let dir = dir_with(&[("config.toml", "log_dir = \"/from/file\"\n".to_string())]);
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "log_dir".into(),
            value: "\"/from/db\"".into(),
        }],
        arming: Vec::new(),
    };
    let seal = seal(&store, false);

    let resolved = load_with_source(
        Some(dir.path()),
        adopted(&store, &seal),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(
        resolved.config.log_dir.as_deref(),
        Some(Path::new("/from/db")),
        "an ADOPTED box resolves from the rows; the file on disk is a stale DRAFT"
    );
    assert!(
        resolved.warnings.iter().any(|w| w.contains("DISAGREE")),
        "…and the operator is TOLD their file disagrees, because nothing else on the box will: \
         {:?}",
        resolved.warnings
    );

    let d =
        describe_with_source(Some(dir.path()), adopted(&store, &seal), &HashMap::new()).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").unwrap();
    assert_eq!(
        row.origin,
        Origin::Db,
        "the ORIGIN column must invert with the value — a `db` value attributed to a file is the \
         most misleading cell this command can print"
    );
    assert_eq!(d.authority, Authority::Store);
}

/// An ARMING row disagreeing with the file loses on an UNADOPTED box — the ceiling half of the rule
/// above, and the half that matters, because an arming row that won would be a row raising a
/// ceiling.
#[test]
fn a_file_wins_over_a_disagreeing_arming_row_on_an_unadopted_box() {
    let dir = dir_with(&[("policy.toml", "[venues]\nbinance = \"paper\"\n".to_string())]);
    let store = StoredSettings {
        settings: Vec::new(),
        arming: vec![ArmingRow {
            venue: "binance".into(),
            label: None,
            mode: "live".into(),
            max_exposure: None,
        }],
    };

    let resolved = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(
        resolved.policy.venues.get("binance").as_str(),
        "paper",
        "on an unadopted box a row may not widen what the policy FILE capped"
    );
}

/// **...and on an ADOPTED box the ARMING ROW decides.** The ceiling half of the flip, and the one
/// this whole task is about: `policy.venues` is what arms a venue at all, so this is the assertion
/// that separates *the crossing worked* from *the crossing unmounted the box*.
///
/// ⚠ **`is_declared()` is asserted explicitly**, because the map alone cannot answer it and because
/// `false` is the quiet failure: it re-fires `vike_mount::venue_arming_migration`'s paste-ready
/// banner on a box that stated its arming months ago, on top of capping every venue to `paper`.
/// The store carries that fact as the PRESENCE of venue rows and by nothing else — there is no
/// `declared` column, deliberately — so this assertion is the whole proof that the convention
/// survives the crossing.
#[test]
fn an_arming_row_wins_over_a_disagreeing_file_on_an_adopted_box() {
    let dir = dir_with(&[("policy.toml", "[venues]\nbinance = \"live\"\n".to_string())]);
    // Roster-complete, because that is the only shape `config mirror` writes and the only shape
    // `adoption_integrity` accepts — a partial table is somebody's `DELETE`.
    let arming: Vec<ArmingRow> = vike_model::VENUES
        .iter()
        .map(|v| ArmingRow {
            venue: (*v).to_string(),
            label: None,
            mode: if *v == "binance" { "demo".into() } else { "paper".into() },
            max_exposure: None,
        })
        .collect();
    let store = StoredSettings { settings: Vec::new(), arming };
    let seal = seal(&store, true);

    let resolved = load_with_source(
        Some(dir.path()),
        adopted(&store, &seal),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(
        resolved.policy.venues.get("binance").as_str(),
        "demo",
        "an ADOPTED box arms from the rows; `policy.toml`'s `live` line is a stale DRAFT"
    );
    assert!(
        resolved.policy.venues.is_declared(),
        "…and the box still KNOWS an arming was stated. `false` here caps every venue to `paper` \
         AND re-fires `vike_mount::venue_arming_migration`'s banner: two quiet failures at once, \
         and the exact shape this task exists to prevent."
    );
}

// ---------------------------------------------------------------------------------------------
// The four properties a file has and a table might not
// ---------------------------------------------------------------------------------------------

/// **Unknown-key refusal BY NAME survives on the READ path**, which 0057 says is LOST unless the
/// loader materialises rows back through the patch types. A typo'd ROW KEY is not a typo'd column,
/// so no `CHECK` can refuse it and the database refuses it in neither direction.
#[test]
fn a_typod_row_key_is_refused_by_name_rather_than_read_as_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for (section, key) in [
        ("policy", "max_levrage"),
        ("config", "log_dri"),
        ("preferences", "log_levle"),
        ("flags", "reconcil"),
    ] {
        let store = StoredSettings {
            settings: vec![SettingRow {
                section: section.into(),
                key: key.into(),
                value: "1".into(),
            }],
            arming: Vec::new(),
        };
        let err = load_with_source(
            Some(dir.path()),
            unadopted(&store),
            &HashMap::new(),
            &CliOverrides::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains(key), "the refusal must NAME {key}: {err}");
    }
}

/// **A WRONG-TYPED row is refused BY NAME too**, which is the other half of
/// [`a_typod_row_key_is_refused_by_name_rather_than_read_as_nothing`] and the half that has no help
/// from serde: an `unknown field` message carries the name INSIDE it, while `invalid type: string
/// "30000", expected u64` carries no name at all.
///
/// ⚠ **This is the property the JSON value column silently dropped, and this test is what would have
/// caught it.** The synthetic TOML document the read path used to assemble was parsed by
/// `toml::de::Error`, which appends its own `in \`<key>\`` tail to every message, so the attribution
/// was free. `serde_json`'s `&Value` deserializer does not annotate, so
/// `vike_config::error::key_from_parse_message` — which matches only serde's backtick markers —
/// found nothing and `ConfigError::Parse`'s `key: Some(..)` arm became unreachable from this path.
/// The operator was handed the SECTION and the offending VALUE, and with twenty `policy` rows in
/// front of them, not the row. `vike_config::mirror`'s `offending_leaf` is what restores it.
///
/// The two innocent siblings are the load-bearing part rather than decoration: one sorts BEFORE the
/// offending key and one AFTER it in the object's own (`BTreeMap`) order, so an implementation that
/// simply named the first row, or the last, is red here.
#[test]
fn a_wrong_typed_row_is_refused_by_name_rather_than_by_section_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![
            // sorts BEFORE the offender
            SettingRow {
                section: "policy".into(),
                key: "deadman_action".into(),
                value: "\"cancel_all\"".into(),
            },
            // the offender: `deadman_timeout_ms` is a `u64`, and this row is a STRING
            SettingRow {
                section: "policy".into(),
                key: "deadman_timeout_ms".into(),
                value: "\"30000\"".into(),
            },
            // ...and one that sorts AFTER it
            SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "3.0".into(),
            },
        ],
        arming: Vec::new(),
    };

    // ⚠ `unadopted`, and the arm matters: this is the UNADOPTED box, where the files still answer.
    // A wrongly-TYPED row is `RowRefusal::Illegal` — the row was read perfectly and says something
    // the schema forbids — so it refuses on BOTH arms, unlike the `Unreadable` stale-format row one
    // test up, which degrades here and only marks the seal on an adopted box.
    let err = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("deadman_timeout_ms"), "the refusal must NAME the offending row: {msg}");
    assert!(!msg.contains("deadman_action"), "...and not a row that is fine: {msg}");
    assert!(!msg.contains("max_leverage"), "...in either direction: {msg}");
    // The type check's own words survive alongside the name — the operator needs both.
    assert!(msg.contains("expected u64"), "{msg}");
}

/// **Validate-on-load survives**: a bound imported from `vike-model` still bites, and no `CHECK`
/// constraint restates it in SQL — which would be the split-brain the typed model exists to refuse.
#[test]
fn a_bound_still_refuses_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "market_slippage".into(),
            value: "0.9".into(),
        }],
        arming: Vec::new(),
    };
    let err = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("market_slippage"), "{err}");
}

/// The section vocabulary is ONE list with two carriers: this crate's [`SECTION_FILES`] and the
/// store's `SETTINGS_SECTIONS` (which is also the `setting.section` `CHECK`'s). They are held equal
/// rather than each being trusted, because a section this crate mirrored and the store refused
/// would be a mirror run that fails halfway on a live box.
#[test]
fn the_sections_this_crate_mirrors_are_the_sections_the_store_accepts() {
    let mut mine: Vec<&str> = SECTION_FILES.iter().map(|(s, _)| *s).collect();
    let mut theirs: Vec<&str> = SETTINGS_SECTIONS.to_vec();
    mine.sort_unstable();
    theirs.sort_unstable();
    assert_eq!(mine, theirs);
}

/// **`policy.venues` is the one place `deny_unknown_fields` structurally cannot reach**, which is
/// why it gets its own table — and the maps this module diverts must be exactly the maps the type
/// has. Derived from a real serialization rather than from the two literals, so a THIRD map added
/// to `Policy` fails here instead of being silently flattened into dotted `setting` rows where no
/// roster check would ever see its keys.
#[test]
fn the_diverted_keys_are_exactly_the_map_shaped_keys_of_the_policy_type() {
    let table = toml::Table::try_from(Policy::default()).unwrap();
    let mut maps: Vec<&str> =
        table.iter().filter(|(_, v)| v.is_table()).map(|(k, _)| k.as_str()).collect();
    maps.sort_unstable();
    assert_eq!(
        maps,
        // ⚠ `account_exposure` joined by the CLASSIFICATION this gate's own message demands, not by
        // being appended: it composes by `min` and can never raise anything, which is the test
        // `vike_secrets::settings`' module doc states for an ambiguous knob — so it is an arming
        // ceiling and its rows ride `venue_arming` beside the modes, on the same `(venue, label)`
        // key. A `label IS NULL` row carries the UNLABELLED account's figure, which is why the
        // table admits the `DEFAULT` spelling `[accounts]` refuses.
        vec!["account_exposure", "accounts", "venues"],
        "`vike_config::mirror` diverts exactly `venues` and `accounts` into the arming table. A new \
         map-shaped policy key must be classified — an arming ceiling joins `venue_arming`, \
         anything else needs a row shape of its own — never flattened into dotted `setting` rows, \
         where its KEY SPACE would have no roster check at all."
    );
}

/// The arming table is ROSTER-COMPLETE the moment it exists — every venue NAMED, even where the
/// value equals the fallback, which is this tree's per-venue-capability-map rule everywhere else.
#[test]
fn a_stated_venues_table_mirrors_a_row_for_every_roster_venue() {
    let dir = dir_with(&[("policy.toml", "[venues]\nbinance = \"demo\"\n".to_string())]);
    let rows = rows_from_files(dir.path()).unwrap();
    let venues: Vec<&str> =
        rows.arming.iter().filter(|r| r.label.is_none()).map(|r| r.venue.as_str()).collect();
    let mut expected: Vec<&str> = vike_model::VENUES.to_vec();
    expected.sort_unstable();
    let mut got = venues.clone();
    got.sort_unstable();
    assert_eq!(got, expected, "a row per roster venue, always NAMED");
}

// ---------------------------------------------------------------------------------------------
// The value column holds a JSON SCALAR — the migration, and the refusal that replaces a guess
// ---------------------------------------------------------------------------------------------

/// **THE MIGRATION PROPERTY: an old row is refused or identical, never silently different.**
///
/// The column held `toml::Value`'s own `Display` until 2026-09-18. A box mirrored before that
/// carries those bytes, so the question a new binary asks of every one of them is not *"does it
/// parse"* but *"can it parse as something ELSE"* — a row that read back as a DIFFERENT ceiling
/// would be the one outcome nobody could detect.
///
/// It cannot, and this measures it as a DISJUNCTION over the real writer: for every value, the old
/// rendering is either **not valid JSON** — in which case the reader refuses it by name, loudly,
/// and [`a_stale_format_row_degrades_by_name_never_guessed`] is what that looks like — or it
/// parses to **exactly the value the new rendering carries**. Both halves are asserted to occur, so
/// a fixture set that drifted into only-easy-values would fail here rather than quietly stop
/// measuring anything.
///
/// The operator act on either side of it is one command: `vike-cli config mirror`. The rows are a
/// regenerable copy of files that still win, so re-deriving them is always available and always
/// correct — which is why this is a refusal-or-no-op and never a second reader.
#[test]
fn no_pre_json_rendering_reads_back_as_a_different_value() {
    let bodies = [
        "store_root = \"/var/lib/vike\"\nlog_dir = \"/var/log/vike\"\n",
        r#"store_root = 'C:\vike\state'
journal_dir = '\\server\share'
log_dir = "a\"b"
"#,
        "log_dir = \"a\\nb\"\nstore_root = \"é/ünï\"\njournal_dir = \"\"\n",
    ];
    let (mut refused, mut identical) = (0usize, 0usize);
    for body in bodies {
        let dir = dir_with(&[("config.toml", body.to_string())]);
        // The bytes the OLD column held, straight from the renderer it used.
        let raw: toml::Table = toml::from_str(body).unwrap();
        for row in &rows_from_files(dir.path()).unwrap().settings {
            let old = raw[&row.key].to_string();
            match serde_json::from_str::<serde_json::Value>(&old) {
                Err(_) => refused += 1,
                Ok(parsed) => {
                    assert_eq!(
                        parsed,
                        serde_json::from_str::<serde_json::Value>(&row.value).unwrap(),
                        "`{}`: an old row that STILL PARSES must carry the same value it always \
                         did — a stale row that read back as a different ceiling is the one \
                         outcome no operator could detect",
                        row.key
                    );
                    identical += 1;
                }
            }
        }
    }
    assert!(refused > 0, "the fixture set must exercise the refused half: {refused} refused");
    assert!(identical > 0, "…and the identical half: {identical} identical");
}

/// **A stale-format row is REFUSED BY NAME through the loader every root boots with**, rather than
/// read as anything at all — the read-path half of the property above, driven through
/// `load_with_source` because that is where a live daemon meets it.
///
/// `'C:\vike\state'` is verbatim what the column held for a Windows `store_root`.
///
/// ⚠ **This asserted a REFUSAL until 2026-09-18. It asserts a DEGRADE now, and the reason is
/// measured rather than preferred:** with real binaries on both commits, a store holding that row
/// made `config show`, `config check`, `secrets list`, `secrets path` AND `config mirror` all exit
/// non-zero — the last being the one command the refusal's own text told the operator to run. No
/// shipped verb could repair the box and `sqlite3` is installed on neither of this project's
/// machines. This is 0057 Phase 1, where the FILES win, so a store layer nobody can read costs the
/// disclosure it would have provided and no value at all —
/// [`a_store_with_no_adoption_row_changes_no_effective_value`] is that property's proof.
///
/// What the operator is told did not change: the warning names the key, names the store, names the
/// command that re-derives the rows, and does NOT echo the value, because a settings directory sits
/// beside `secrets.env`.
#[test]
fn a_stale_format_row_degrades_by_name_on_an_unadopted_box() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "store_root".into(),
            value: r"'C:\vike\state'".into(),
        }],
        arming: Vec::new(),
    };
    let settings = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("a store in the PREVIOUS release's encoding must not stop a daemon booting");
    let msg = settings
        .warnings
        .iter()
        .find(|w| w.contains("settings database was NOT read"))
        .unwrap_or_else(|| panic!("the degrade must WARN: {:?}", settings.warnings))
        .clone();
    assert!(msg.contains("store_root"), "the warning must NAME the row: {msg}");
    assert!(msg.contains("config mirror"), "…and the act that fixes it: {msg}");
    assert!(!msg.contains(r"C:\vike"), "…and must not echo the value it could not read: {msg}");

    // ...and `config show` ANSWERS rather than refusing — it is the verb an operator reaches for
    // when a box misbehaves, so it is the last place a refusal belongs. The `db` origin is simply
    // absent, which is exactly what the warning above says has happened.
    describe_with_source(Some(dir.path()), unadopted(&store), &HashMap::new())
        .expect("the disclosure verb must survive a store it cannot read");
}

/// **...and on an ADOPTED box the same row REFUSES — plus the half the incident actually turned on.**
///
/// The inversion is argued in full at `vike_config::mirror::apply_adopted_rows`. Carrying the
/// degrade across the crossing would BE the unmount: one unparseable row drops the whole layer,
/// `Policy::default()` answers, and that is `max_notional_per_order` absent on a daemon whose
/// control channel is armed AND `is_declared()` false, so every venue caps to `paper` and the
/// migration banner re-fires. Two quiet failures from one silent line.
///
/// ⚠ **The second half of this test is the part the JSON incident was really about**, and it is why
/// the assertion is not simply "it errors". That incident's lesson was never *always degrade*; it
/// was ***a refusal must not brick the tool that repairs it*** — the first implementation refused a
/// row it could not read and `vike-cli config mirror`, the one command the refusal's own text
/// named, went down with everything else. So this asserts the repairs still RUN in the refusing
/// state, and they do so BY CONSTRUCTION rather than by care: `rows_from_files` resolves the files
/// with no store at all, and `config adopt --undo` deletes one row without going through a loader.
#[test]
fn a_stale_format_row_marks_a_seal_refusal_on_an_adopted_box() {
    let dir = dir_with(&[("config.toml", "log_dir = \"/from/file\"\n".to_string())]);
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "store_root".into(),
            value: r"'C:\vike\state'".into(),
        }],
        arming: Vec::new(),
    };
    let seal = seal(&store, false);

    let msg = seal_mark(
        dir.path(),
        &store,
        &seal,
        "on an ADOPTED box there is no file underneath the rows to answer instead, so resolving \
         around an unreadable row would mean no ceiling and every venue `paper` — that must be \
         MARKED, and the verbs that act on a ceiling refuse on the mark",
    );
    assert!(msg.contains("store_root"), "the refusal must NAME the row: {msg}");
    assert!(msg.contains("config mirror"), "…and the act that repairs it: {msg}");
    assert!(msg.contains("adopt --undo"), "…and the act that steps back from it: {msg}");
    assert!(!msg.contains(r"C:\vike"), "…and must not echo the value it could not read: {msg}");
    assert_no_catastrophic_repair(&msg);

    // REPAIR 1 — `vike-cli config mirror` re-derives every row from the files. It resolves the
    // files with NO store, so it cannot meet the refusal above at all.
    let rederived = rows_from_files(dir.path())
        .expect("the repair path must be structurally clear of the refusal it repairs");
    load_with_source(
        Some(dir.path()),
        unadopted(&rederived),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("…and what it writes resolves");

    // REPAIR 2 — `vike-cli config adopt --undo` deletes the seal; the files answer again, and the
    // unreadable row goes back to being a degrade.
    let stepped_back = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("stepping back to the files must resolve, with the SAME rows still in the store");
    assert_eq!(stepped_back.config.log_dir.as_deref(), Some(Path::new("/from/file")));
}

// ---------------------------------------------------------------------------------------------
// The SEAL: the probe, and the two detectors that make the crossing survivable
// ---------------------------------------------------------------------------------------------

/// **An EMPTY settings table is not a marker of anything, and it must not be adoptable beside
/// stating files.**
///
/// ⚠ This is the measurement that kills every obvious probe, so the store here is built the way a
/// real box gets one — by running `vike_secrets`' own DDL through a real `secrets migrate`-shaped
/// create — and NOT from a hand-built empty `StoredSettings`, which would test nothing.
/// `open_for_write` runs the whole DDL batch on the `created` branch, so `vike-cli secrets migrate`
/// — a CREDENTIAL command — leaves `setting` and `venue_arming` present and EMPTY on every fresh
/// box. A probe shaped like *the database exists* or *the tables exist* would therefore flip a box
/// that has mirrored nothing into resolving from zero rows: no ceiling, no dead-man, every venue
/// `paper`, `is_declared()` false.
///
/// The seal closes it with no special case — `vike-cli config adopt` re-runs the comparison and
/// refuses unless the two resolutions are IDENTICAL, and an empty store beside a stating file is
/// exactly what that comparison reports.
#[test]
fn an_empty_settings_table_cannot_be_adopted_beside_stating_files() {
    let dir = dir_with(&[(
        "policy.toml",
        "max_notional_per_order = 250\n[venues]\nbinance = \"demo\"\n".to_string(),
    )]);
    // A store born exactly as `vike-cli secrets migrate` leaves one — through the REAL migration,
    // not a hand-built value, because the claim under test is about what that command's create
    // branch does to the SETTINGS tables. `settings_dir` is the `$VIKE_SETTINGS_DIR` override
    // spelling this function takes, so nothing here walks and nothing reaches a real project.
    //
    // ⚠ The credential is not decoration: `migrate` deliberately does NOT open a write connection
    // when there is nothing to carry (`MigrationOutcome::NothingToMigrate`), because opening one
    // CREATES the database and the mere existence of that file is what makes it — rather than
    // `secrets.env` — answer for every credential on the box. So a store only exists here because
    // a credential asked for one, which is exactly how a real box gets one.
    std::fs::write(dir.path().join("secrets.env"), "BINANCE_DEMO_API_KEY=not-a-real-key\n")
        .unwrap();
    vike_secrets::migrate(Some(dir.path().to_str().unwrap()), |_| false, &|name| {
        vike_secrets::Classification::unrecognised(name)
    })
    .expect("a fresh credential migration creates the settings tables, empty");
    let db = dir.path().join("db").join("vike.db");

    let source = vike_secrets::read_settings(&db).expect("…and it reads back");
    let rows = source.rows().expect("the tables are THERE — that is the whole point");
    assert!(rows.is_empty(), "…and EMPTY, which is what makes a table probe a lie");
    assert!(
        source.adoption().is_none(),
        "⚠ a CREDENTIAL migration must never seal a box's SETTINGS: only `config adopt` may"
    );

    // Unadopted, which is what it is: the files answer and nothing changed.
    let resolved = load_with_source(
        Some(dir.path()),
        unadopted(rows),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(resolved.policy.max_notional_per_order, Some(250.0));
    assert!(resolved.policy.venues.is_declared());

    // ...and `config adopt`'s precondition REFUSES this store, which is how the cell is closed
    // without a special case anywhere in the loader.
    let drift = vike_config::compare_sources(dir.path(), rows);
    assert!(
        !drift.is_identical(),
        "an empty store beside a stating file MUST fail the comparison `config adopt` requires"
    );
    assert!(
        drift.keys.iter().any(|k| k.key == "policy.max_notional_per_order"),
        "…naming the ceiling that would have vanished: {:?}",
        drift.keys
    );
    assert!(
        drift.keys.iter().any(|k| k.key.starts_with("policy.venues")),
        "…and the arming that would have gone with it: {:?}",
        drift.keys
    );
}

/// **Row LOSS on an adopted box REFUSES, naming what was lost.** The erase detector.
///
/// This is the silent failure the whole crossing has to answer for. Once the rows are the only
/// layer, a `DELETE` against them is not *an operator chose the defaults* — it is a ceiling or an
/// arming decision that has quietly stopped being applied, on a daemon that starts clean and
/// reports every value as resolved.
///
/// ⚠ The counts are relative to what THIS box adopted rather than to an absolute expectation, which
/// is what makes the check cost NO behaviour change on a box that never set a ceiling.
#[test]
fn losing_rows_after_adoption_refuses_rather_than_resolving_to_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let full = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "250".into(),
        }],
        arming: Vec::new(),
    };
    let seal = seal(&full, false);

    // The control: with its rows intact the box resolves the ceiling it was sealed on.
    let ok = load_with_source(
        Some(dir.path()),
        adopted(&full, &seal),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(ok.policy.max_notional_per_order, Some(250.0));

    // ...and with the row gone it REFUSES rather than answering "no ceiling".
    let emptied = StoredSettings { settings: Vec::new(), arming: Vec::new() };
    let msg = seal_mark(
        dir.path(),
        &emptied,
        &seal,
        "a row that has silently stopped being applied is the failure this detects",
    );
    assert!(msg.contains("CHANGED since this box was adopted"), "{msg}");
    assert!(msg.contains("config mirror"), "…and names the repair: {msg}");

    // ⚠ ...and in the OTHER direction too. A row ARRIVING outside the writer is the same evidence:
    // somebody edited the store by hand, and on an adopted box that row is now a live ceiling.
    let extra = StoredSettings {
        settings: vec![
            full.settings[0].clone(),
            SettingRow {
                section: "policy".into(),
                key: "max_account_exposure".into(),
                value: "9000000".into(),
            },
        ],
        arming: Vec::new(),
    };
    seal_mark(
        dir.path(),
        &extra,
        &seal,
        "a row that ARRIVED outside the writer is the same evidence and the same refusal",
    );
}

/// **A box that had STATED an arming and now has no arming rows REFUSES.** The unmount detector.
///
/// `policy.toml` with no `[venues]` table mirrors to ZERO arming rows deliberately — a roster of
/// all-`paper` rows written for a file that declared nothing would silence
/// `vike_mount::venue_arming_migration` for a box that has still not stated one. So once the rows
/// are the only layer, *nobody ever stated an arming* and *the arming rows were erased* are the
/// same empty table, and `Adoption::venues_declared` is the only thing that can tell them apart.
#[test]
fn an_erased_arming_table_refuses_on_a_box_that_had_stated_one() {
    let dir = tempfile::tempdir().unwrap();
    let stated = StoredSettings {
        settings: Vec::new(),
        arming: vike_model::VENUES
            .iter()
            .map(|v| ArmingRow {
                venue: (*v).to_string(),
                label: None,
                mode: if *v == "bybit" { "demo".into() } else { "paper".into() },
                max_exposure: None,
            })
            .collect(),
    };
    let seal_stated = seal(&stated, true);
    load_with_source(
        Some(dir.path()),
        adopted(&stated, &seal_stated),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("the control");

    // The rows are gone AND the counts were updated to match — so only `venues_declared` is left
    // to notice, which is the whole reason that column exists.
    let erased = StoredSettings { settings: Vec::new(), arming: Vec::new() };
    let mut seal_erased = seal(&erased, true);
    seal_erased.venues_declared = true;
    let msg = seal_mark(
        dir.path(),
        &erased,
        &seal_erased,
        "an erased arming table on a box that stated one is the UNMOUNT",
    );
    assert!(msg.contains("STATED"), "{msg}");

    // ...and a box that genuinely never stated one resolves fine on the same empty table. That is
    // the state this refusal must NOT fire in, and it is why the flag is sealed rather than
    // inferred from the table.
    let never = seal(&erased, false);
    let resolved = load_with_source(
        Some(dir.path()),
        adopted(&erased, &never),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("a box that never stated an arming is the ORDINARY state, not a failure");
    assert!(!resolved.policy.venues.is_declared());
}

/// **A PARTIAL venue-arming table refuses, naming the missing venues.**
///
/// `vike-cli config mirror` writes a row for EVERY roster venue when the policy declares and none
/// at all when it does not, so *complete or empty* is the only shape it produces — which makes it
/// an invariant a READ can check. A partial table means rows were deleted, and on an adopted box
/// the missing venues would silently resolve to `paper` while the ones that remain keep theirs:
/// a HALF-unmount, which is harder to notice than the whole one.
/// ⚠ **WARNS — it does not refuse, and the demotion is the fix for a defect this test encoded.**
/// The comparison is against the COMPILED roster while the seal is a fact about a PAST one, so as a
/// refusal it fired with no operator act at all: `rows_from_files` writes one row per
/// `vike_model::VENUES` id, a box seals at exactly today's length, and the next release that runs
/// `just new-venue` makes the compiled roster one longer than the sealed table. Measured at
/// `650907a37` — a store sealed on 14 rows, one id added at the `venues.rs` marker, and every
/// binary on the box refused to start, `config mirror` and `config adopt --undo` among them.
/// Nothing had been deleted; the roster had grown.
///
/// The ERASE detector is the COUNT check, which is seal-relative and cannot make that mistake. What
/// this check adds over it is the NAMES, and a venue with no row resolves to `paper` — the safe
/// direction — so the names are worth a warning and cannot be worth taking the box down.
#[test]
fn a_partial_arming_table_warns_by_naming_the_missing_venues_and_still_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let partial = StoredSettings {
        settings: Vec::new(),
        arming: vec![ArmingRow {
            venue: "binance".into(),
            label: None,
            mode: "demo".into(),
            max_exposure: None,
        }],
    };
    let seal = seal(&partial, true);
    let settings = load_with_source(
        Some(dir.path()),
        adopted(&partial, &seal),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("a roster GAP must not take the box down — see this test's doc");
    assert!(
        settings.seal_refusal.is_none(),
        "…and it is not a gated refusal either: {:?}",
        settings.seal_refusal
    );

    let missing = vike_model::VENUES.iter().find(|v| **v != "binance").unwrap();
    let warned = settings.warnings.iter().any(|w| w.contains(missing));
    assert!(warned, "the warning must NAME what is missing: {:?}", settings.warnings);
}

/// **A store that cannot be READ AT ALL resolves neither layer, and says so as DATA.**
///
/// ⚠ It is NOT *"so use the files"*, and that is the whole of this test. Whether this box is
/// ADOPTED is itself a fact IN the store, so a store that will not open cannot answer *am I the
/// authority here* — and falling through to the files on that silence is the ladder re-entering
/// through the error path. `vike_config::arming`'s `LiveArmingVerdict::Undetermined` is the same
/// shape one module over.
///
/// The loader MARKS rather than refusing, and the measurement behind that is at
/// `vike_config::load_with_source`'s own arm: on this tree an unreadable store means an empty
/// CREDENTIAL map and therefore an all-paper mount, so a refusal would take a daemon down without
/// preventing anything. `vike-cli config check` answers `Level::Fail`, which puts the stop at a
/// deploy pre-flight instead.
#[test]
fn an_unreadable_store_marks_the_resolution_rather_than_silently_using_the_files() {
    let dir = dir_with(&[("config.toml", "log_dir = \"/from/file\"\n".to_string())]);
    let resolved = load_with_source(
        Some(dir.path()),
        StoreLayer::Unreadable("disk I/O error"),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("the loader RESOLVES and marks; the ROOT decides whether that is fatal");

    let why = resolved
        .store_refusal
        .as_deref()
        .expect("a store that could not be read must be a SECOND channel a verb can gate on");
    assert!(why.contains("disk I/O error"), "{why}");
    assert!(
        why.contains("NOT necessarily what a daemon"),
        "…and must say plainly that these values are not authoritative: {why}"
    );
    assert!(
        resolved.warnings.iter().any(|w| w.contains("disk I/O error")),
        "…and ride the warnings channel too, so a root that only surfaces those still shows it"
    );
    assert_no_catastrophic_repair(why);
}

/// **A `null` row reads as a REFUSAL, not as "unset".** JSON's one expressive advantage over TOML
/// is the one thing this column must not accept — `deny_unknown_fields` sees a KNOWN field name and
/// every patch field is an `Option`, so a hand-written `null` would resolve to the default in
/// silence. Driven through the loader, because "silently unset" is a state a daemon boots in.
#[test]
fn a_null_row_is_refused_rather_than_resolving_to_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "null".into(),
        }],
        arming: Vec::new(),
    };
    let err = load_with_source(
        Some(dir.path()),
        unadopted(&store),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("max_notional_per_order"), "{err}");
}

/// A settings directory a BOOT would refuse is refused by the mirror too, with the same error
/// naming the same file and key — a store written from an invalid file would refuse every later
/// read, from a command that reported success.
#[test]
fn an_invalid_file_is_refused_by_the_mirror_rather_than_half_written() {
    let dir = dir_with(&[("policy.toml", "market_slippage = 0.9\n".to_string())]);
    let err = rows_from_files(dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("market_slippage"), "{msg}");
    assert!(msg.contains("policy.toml"), "the refusal names the FILE, not the store: {msg}");
}

/// **No refusal in this design may teach an operator to delete the settings database.**
///
/// `vike_secrets::Backend` decides which store answers for a CREDENTIAL on one probe — the mere
/// existence of `settings/db/vike.db` — so deleting that file takes every venue on a migrated box
/// silently to paper AND destroys the only copy of its venue keys. It is the one repair that is
/// both plausible-looking and catastrophic, and it is reachable from this design rather than
/// introduced by it, which is why the rule is a gate and not a note. The repair for a database that
/// will not open at all is a restore, out of band.
///
/// ⚠ **The check is on TOKENS, and a substring version of it was written first and was WRONG.**
/// `msg.contains("rm ")` matched *"JSON has no **form** for"* in the stale-format hint — a false
/// positive on ordinary English, in a gate whose whole value is that it fires only on the real
/// thing. A gate that cries wolf on prose is a gate somebody deletes.
fn assert_no_catastrophic_repair(msg: &str) {
    let bad: Vec<&str> = msg
        .split(|c: char| c.is_whitespace() || c == '`')
        .filter(|t| {
            let t = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/');
            t.eq_ignore_ascii_case("rm") || t.ends_with("vike.db")
        })
        .collect();
    assert!(
        bad.is_empty(),
        "⚠ this message names {bad:?}. Deleting the settings database makes \
         `vike_secrets::Backend` answer `Files` for CREDENTIALS: every venue silently on paper, \
         and the only copy of the box's venue keys gone. No refusal in this design may suggest \
         it.\n{msg}"
    );
}
