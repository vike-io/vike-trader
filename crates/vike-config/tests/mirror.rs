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
//! * [`the_mirror_changes_no_effective_value`] — files-over-rows resolves identically to files
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
    CliOverrides, Origin, Policy, Settings, describe_with_store, load, load_with_store,
};
use vike_secrets::{ArmingRow, SETTINGS_SECTIONS, SettingRow, StoredSettings};

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

/// **The whole point of Phase 1: mirroring a box changes nothing it resolves.**
#[test]
fn the_mirror_changes_no_effective_value() {
    for (what, dir) in fixtures() {
        let before = load(Some(dir.path()), &HashMap::new()).expect(what);
        let rows = rows_from_files(dir.path()).expect(what);
        let after = load_with_store(
            Some(dir.path()),
            Some(&rows),
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
    }
}

/// ⚠ **Without this, the two tests above are vacuous.** A loader that ignored the store entirely
/// would pass both of them: the rows would be equal to the files by construction and the store
/// would change nothing because nothing read it. This is the test that fails if the layer is not
/// wired, and it is why it is written against a key NO FILE SETS.
///
/// **MEASURED mutation proof** (`crates/vike-config/src/load.rs`'s `load_with_store`, its
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

    let resolved =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
            .unwrap();
    assert_eq!(resolved.config.log_dir.as_deref(), Some(Path::new("/from/db")));

    // ...and `config show` attributes it to the store by NAME, which is the fourth provenance word.
    let d = describe_with_store(Some(dir.path()), Some(&store), &HashMap::new()).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").unwrap();
    assert_eq!(row.origin, Origin::Db);
    assert_eq!(row.origin.kind(), "db");
}

/// **The files still win**, measured on a deliberate disagreement rather than inferred from the
/// order of two `apply` calls.
#[test]
fn a_file_wins_over_a_disagreeing_row() {
    let dir = dir_with(&[("config.toml", "log_dir = \"/from/file\"\n".to_string())]);
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "log_dir".into(),
            value: "\"/from/db\"".into(),
        }],
        arming: Vec::new(),
    };

    let resolved =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
            .unwrap();
    assert_eq!(
        resolved.config.log_dir.as_deref(),
        Some(Path::new("/from/file")),
        "Phase 1 is MIRRORED: the store is written and the FILES STILL WIN"
    );

    let d = describe_with_store(Some(dir.path()), Some(&store), &HashMap::new()).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").unwrap();
    assert_eq!(row.origin, Origin::File("config.toml"));
}

/// An ARMING row disagreeing with the file loses too — the ceiling half of the rule above, and the
/// half that matters, because an arming row that won would be a row raising a ceiling.
#[test]
fn a_file_wins_over_a_disagreeing_arming_row() {
    let dir = dir_with(&[("policy.toml", "[venues]\nbinance = \"paper\"\n".to_string())]);
    let store = StoredSettings {
        settings: Vec::new(),
        arming: vec![ArmingRow { venue: "binance".into(), label: None, mode: "live".into() }],
    };

    let resolved =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
            .unwrap();
    assert_eq!(
        resolved.policy.venues.get("binance").as_str(),
        "paper",
        "a row may not widen what the policy FILE capped"
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
        let err = load_with_store(
            Some(dir.path()),
            Some(&store),
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

    let err =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
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
    let err =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
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
        vec!["accounts", "venues"],
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
/// `load_with_store` because that is where a live daemon meets it.
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
/// [`the_mirror_changes_no_effective_value`] is that property's proof.
///
/// What the operator is told did not change: the warning names the key, names the store, names the
/// command that re-derives the rows, and does NOT echo the value, because a settings directory sits
/// beside `secrets.env`.
#[test]
fn a_stale_format_row_degrades_by_name_never_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "store_root".into(),
            value: r"'C:\vike\state'".into(),
        }],
        arming: Vec::new(),
    };
    let settings =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
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
    describe_with_store(Some(dir.path()), Some(&store), &HashMap::new())
        .expect("the disclosure verb must survive a store it cannot read");
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
    let err =
        load_with_store(Some(dir.path()), Some(&store), &HashMap::new(), &CliOverrides::default())
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
