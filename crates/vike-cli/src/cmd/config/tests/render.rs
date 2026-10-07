//! `config show`'s printers, its JSON document, the ceilings blocks and the key complement.

use super::*;

// -- the printers --------------------------------------------------------------------------

fn empty_store() -> StoreStatus {
    StoreStatus { backend: FILES, path: None, present: false, keys: 0, shadowed: None }
}

/// A box that has never stored a run profile — what every deployment looks like the day this
/// lands, and the case where the ceilings block must still name the keys it cannot value.
fn no_profiles() -> RunProfileRows {
    RunProfileRows {
        source: vike_secrets::ProfileRiskSource::NoDatabase {
            path: PathBuf::from("/p/settings/db/vike.db"),
        },
        active: None,
    }
}

/// A box migrated BEFORE the profile tables existed — the third state, which must not read as
/// "no ceilings".
fn table_absent() -> RunProfileRows {
    RunProfileRows {
        source: vike_secrets::ProfileRiskSource::TableAbsent {
            path: PathBuf::from("/p/settings/db/vike.db"),
        },
        active: None,
    }
}

/// A STORED `run-live` body: one of the two mount-refusing ceilings set, the other NOT (so the
/// unset warning is exercised), plus a row whose key is in no `[risk]` schema — the one a hand
/// `INSERT` produces and no verb in this tree can write.
fn mirrored_profiles() -> RunProfileRows {
    RunProfileRows {
        source: vike_secrets::ProfileRiskSource::Rows(vec![vike_secrets::StoredProfileRisk {
            profile: "run-live".to_string(),
            rows: vec![
                vike_secrets::ProfileRiskRow {
                    key: "max_levrage".to_string(),
                    value: "9.0".to_string(),
                },
                vike_secrets::ProfileRiskRow {
                    key: "max_notional_per_order".to_string(),
                    value: "250.0".to_string(),
                },
            ],
        }]),
        active: None,
    }
}

/// …and the SAME body, ACTIVE. The one state in which these numbers judge an order, and the
/// one in which the block's old sentence would have been a lie.
fn active_profile() -> RunProfileRows {
    RunProfileRows { active: Some("run-live".to_string()), ..mirrored_profiles() }
}

/// The MIGRATED box's store status, with the credential file still sitting there unread — the
/// one configuration every sentence this change touches used to describe wrongly.
fn migrated_store() -> StoreStatus {
    let db = PathBuf::from("/p/settings/db/vike.db");
    let file = PathBuf::from("/p/settings/secrets.env");
    StoreStatus {
        backend: vike_secrets::Backend::Database(db.clone()),
        path: Some(db.clone()),
        present: true,
        keys: 3,
        shadowed: Some(vike_secrets::ShadowedStore { file, db }),
    }
}

#[test]
fn both_printers_render_everything_without_panicking() {
    let d = vike_config::describe(None, &map(&[])).unwrap();
    let files = file_rows(&d, None, false);
    let envs = resolve_all(&map(&[]), &map(&[]), &FILES, None, false);
    let unknown = unknown_store_keys(&map(&[("ACME_TYPOD", "1"), ("ACME_API_KEY", "k")]), None);
    print_human(
        Section::All,
        None,
        &d,
        &empty_store(),
        &files,
        &envs,
        &unknown,
        &no_profiles(),
        &[],
    );
    print_json(&d, &empty_store(), &files, &envs, &unknown, &mirrored_profiles(), &[]).unwrap();
    // and the empty views
    let none = UnknownKeys::default();
    print_human(
        Section::Files,
        None,
        &d,
        &empty_store(),
        &[],
        &[],
        &none,
        &mirrored_profiles(),
        &[],
    );
    print_human(Section::Env, None, &d, &empty_store(), &[], &[], &none, &no_profiles(), &[]);
    // …and the filtered-to-nothing env table WITH a complement, the one path that would
    // otherwise return before printing it.
    print_human(Section::Env, None, &d, &empty_store(), &[], &[], &unknown, &no_profiles(), &[]);
    print_json(&d, &empty_store(), &[], &[], &none, &no_profiles(), &[]).unwrap();
    // …and the MIGRATED box, whose header grows two extra lines (the DATABASE qualifier and
    // the shadowed-file finding) that no other case reaches.
    print_human(
        Section::All,
        None,
        &d,
        &migrated_store(),
        &files,
        &envs,
        &unknown,
        &table_absent(),
        &[],
    );
    print_json(&d, &migrated_store(), &files, &envs, &unknown, &table_absent(), &[]).unwrap();
}

/// **The store the header, the precedence line and the complement all NAME** — one answer,
/// derived from the backend, so the three sentences cannot disagree with each other or with the
/// SOURCE column beside them.
#[test]
fn the_store_labels_follow_the_backend() {
    let unmigrated = empty_store();
    assert_eq!(unmigrated.label(), vike_secrets::SECRETS_FILE);
    assert_eq!(unmigrated.kind(), "file");
    // With no settings directory there is no path to print, so the sentence falls back to the
    // conventional shorthand rather than to nothing.
    assert_eq!(
        unmigrated.named(),
        format!(
            "{}/{}",
            vike_model::paths::state_path::PROJECT_SETTINGS_DIR,
            vike_secrets::SECRETS_FILE
        )
    );

    let migrated = migrated_store();
    assert_eq!(migrated.label(), vike_secrets::DB_FILE);
    assert_eq!(migrated.kind(), "database");
    assert!(migrated.named().ends_with("vike.db"), "{}", migrated.named());
}

/// **[`store_status`] is the one probe, and it describes ONE store.** The defect it replaced
/// was a struct whose path and presence bit described `secrets.env` while its key count came
/// out of the database that had shadowed it.
#[test]
fn the_store_status_describes_the_store_that_answers() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), "A=1\n").expect("the file");

    // No database on disk ⇒ the file answers, exactly as it did before 0054.
    let files = store_status(Some(dir.path()), 1);
    assert_eq!(files.backend, vike_secrets::Backend::Files);
    assert_eq!(files.path.as_deref(), Some(dir.path().join("secrets.env").as_path()));
    assert!(files.present);
    assert!(files.shadowed.is_none(), "nothing shadows anything on an unmigrated box");

    // A database beside it ⇒ IT answers, and the file it left behind is a FINDING.
    let db = dir.path().join(vike_secrets::DB_DIR).join(vike_secrets::DB_FILE);
    std::fs::create_dir_all(db.parent().expect("db dir")).expect("db dir");
    std::fs::write(&db, b"").expect("the database");
    let migrated = store_status(Some(dir.path()), 3);
    assert_eq!(migrated.backend, vike_secrets::Backend::Database(db.clone()));
    assert_eq!(migrated.path.as_deref(), Some(db.as_path()), "the DATABASE's path, not the file's");
    assert!(migrated.present);
    let shadowed = migrated.shadowed.expect("the file is still on disk and is not read");
    assert_eq!(shadowed.file, dir.path().join(vike_secrets::SECRETS_FILE));
    assert!(
        shadowed.to_string().contains("NO LONGER READ"),
        "the finding must carry `vike_secrets::ShadowedStore`'s own words, not a second \
             spelling of them: {shadowed}"
    );

    // …and a migrated project whose file was retired reports no finding to make.
    std::fs::remove_file(dir.path().join(vike_secrets::SECRETS_FILE)).expect("retire it");
    assert!(store_status(Some(dir.path()), 3).shadowed.is_none());
}

#[test]
fn the_json_document_carries_the_documented_fields() {
    let d = vike_config::describe(None, &map(&[])).unwrap();
    let files = file_rows(&d, Some("policy"), false);
    let envs = resolve_all(&map(&[]), &map(&[]), &FILES, Some("vike-cli"), false);
    assert!(!files.is_empty() && !envs.is_empty());
    let doc = settings_json(
        &d,
        &empty_store(),
        &files,
        &envs,
        &UnknownKeys::default(),
        &mirrored_profiles(),
        &[],
    );

    for field in [
        "settings_dir",
        "secrets",
        "warnings",
        "settings",
        "env",
        "ceilings",
        "profile_risk",
        "venue_settings",
        "unknown_env_keys",
    ] {
        assert!(doc.get(field).is_some(), "missing {field}");
    }
    assert!(doc.get("files").is_none(), "there are no settings files to report");
    for field in ["named", "credential_shaped"] {
        assert!(doc["unknown_env_keys"].get(field).is_some(), "missing unknown_env_keys.{field}");
    }
    // The store object names WHICH store answered, not just where it is: a `path` alone cannot
    // tell a consumer whether the thing at the end of it is a text file it may `cat`.
    for field in ["path", "kind", "present", "keys", "shadowed"] {
        assert!(doc["secrets"].get(field).is_some(), "missing secrets.{field}");
    }
    let first = &doc["settings"].as_array().unwrap()[0];
    for field in ["key", "value", "origin", "origin_detail", "default", "adjusted", "secret"] {
        assert!(first.get(field).is_some(), "missing {field} in {first}");
    }
    let first = &doc["env"].as_array().unwrap()[0];
    for field in [
        "name",
        "value",
        "source",
        "reads",
        "store_may_not_reach_reader",
        "default",
        "krate",
        "secret",
    ] {
        assert!(first.get(field).is_some(), "missing {field} in {first}");
    }
}

/// **The `--json` document names the store that answered too**, because the machine surface
/// lied in exactly the same way the human one did: `secrets.path` was the credential FILE's,
/// beside a `keys` count read out of the database.
#[test]
fn the_json_store_object_names_the_database_when_it_answers() {
    let d = vike_config::describe(None, &map(&[])).unwrap();
    let envs = resolve_all(&map(&[]), &map(&[]), &database(), Some("vike-cli"), false);
    let doc = settings_json(
        &d,
        &migrated_store(),
        &[],
        &envs,
        &UnknownKeys::default(),
        &no_profiles(),
        &[],
    );

    assert_eq!(doc["secrets"]["kind"], "database");
    assert!(
        doc["secrets"]["path"].as_str().expect("a path").ends_with("vike.db"),
        "{}",
        doc["secrets"]["path"]
    );
    assert!(
        doc["secrets"]["shadowed"]["file"]
            .as_str()
            .expect("the shadowed file")
            .ends_with("secrets.env"),
        "the file the database replaced must be disclosed, not silently dropped: {}",
        doc["secrets"]["shadowed"]
    );

    // …and the unmigrated box's document is the one it always was.
    let plain =
        settings_json(&d, &empty_store(), &[], &envs, &UnknownKeys::default(), &no_profiles(), &[]);
    assert_eq!(plain["secrets"]["kind"], "file");
    assert_eq!(plain["secrets"]["shadowed"], serde_json::Value::Null);
}

/// **The disclosure half of the two-ceilings finding**, asserted on the SERIALIZED document.
///
/// `vike_config::ceilings::PRE_TRADE_CEILINGS` can be a perfectly correct table and still change nothing
/// for an operator if this command does not render it — which is exactly the state
/// `[risk] max_total_exposure` was in: enforced, mandatory for a live mount, MEASURED at 500 on
/// a live box, and present in no payload `config show` printed. So the properties pinned here
/// are the ones an operator's question depends on, not the table's own shape (that is
/// `crates/vike-config/tests/ceilings_are_distinct.rs`'s job):
///
/// 1. the array exists and carries EVERY row, unfiltered — a machine asking "what can refuse
///    this order" must not get an answer that depends on a human's `--filter`;
/// 2. `max_total_exposure` is in it, with `value_shown: false` — the row whose invisibility
///    started this, saying out loud that the number lives in a file this command did not read;
/// 3. the same-named pair is marked `shares_its_name` on BOTH rows with DIFFERENT `guards`, so
///    a reader of the JSON alone cannot conclude they are one ceiling.
#[test]
fn the_json_document_discloses_every_pre_trade_ceiling() {
    let d = vike_config::describe(None, &map(&[])).unwrap();
    // Deliberately the FILTERED-to-one-key view: the ceilings array must be complete anyway.
    let files = file_rows(&d, Some("halt_admit"), false);
    let doc = settings_json(
        &d,
        &empty_store(),
        &files,
        &[],
        &UnknownKeys::default(),
        &no_profiles(),
        &[],
    );

    let rows = doc["ceilings"].as_array().expect("`ceilings` missing from the JSON document");
    assert_eq!(
        rows.len(),
        vike_config::ceilings::PRE_TRADE_CEILINGS.len(),
        "the ceilings array must carry every row regardless of --filter"
    );
    for field in ["name", "home", "value_shown", "guards", "enforced", "absent_means"] {
        assert!(rows[0].get(field).is_some(), "missing {field} in {}", rows[0]);
    }

    let exposure: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["name"] == "max_total_exposure").collect();
    assert_eq!(exposure.len(), 1, "`max_total_exposure` must be disclosed exactly once");
    assert_eq!(
        exposure[0]["value_shown"], false,
        "this command reads no run profile; claiming it showed the value would be the \
             positive-confirmation-of-something-false defect the READ column exists for"
    );
    assert_eq!(exposure[0]["enforced"], true, "it IS enforced — that is the whole point");

    let notional: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["name"] == "max_notional_per_order").collect();
    assert_eq!(notional.len(), 2, "the finding is that TWO files carry this key");
    assert_ne!(
        notional[0]["guards"], notional[1]["guards"],
        "two ceilings sharing a name must disclose different acts, or the JSON reads as one"
    );
    assert_ne!(notional[0]["home"], notional[1]["home"]);
    for r in &notional {
        assert_eq!(r["shares_its_name"], true, "{r} must be flagged as sharing its name");
    }
}

/// The human view of the same, driven through the real printer so a panic or a missing block
/// is caught: the filtered-to-nothing table is exactly the view where an operator most needs
/// to be told a ceiling lives in a file this command does not read.
#[test]
fn the_ceilings_block_renders_filtered_and_unfiltered() {
    print_ceilings(None, &[]);
    print_ceilings(Some("max_notional_per_order"), &[]);
    print_ceilings(Some("run profile"), mirrored_profiles().source.profiles().unwrap());
    // A filter that matches no ceiling must print nothing rather than an empty table.
    print_ceilings(Some("zzz-no-such-ceiling"), &[]);
}

/// The block, driven through the real printer in every state it can be in — the three
/// `ProfileRiskSource` arms, the ACTIVE arm, and the filtered views — so a panic or an
/// unreachable branch is caught.
#[test]
fn the_profile_risk_block_renders_in_every_state() {
    print_profile_risk(None, &no_profiles());
    print_profile_risk(None, &table_absent());
    print_profile_risk(None, &mirrored_profiles());
    print_profile_risk(None, &active_profile());
    print_profile_risk(Some("max_notional"), &mirrored_profiles());
    print_profile_risk(Some("run-live"), &mirrored_profiles());
    // A filter matching neither the block's own words nor any row: nothing is printed.
    print_profile_risk(Some("zzz-no-such-key"), &mirrored_profiles());
    // ...and a stored profile carrying no rows at all, which is a legitimate body for a profile
    // that sets no ceiling and must not read as "never stored".
    print_profile_risk(
        None,
        &RunProfileRows {
            source: vike_secrets::ProfileRiskSource::Rows(vec![vike_secrets::StoredProfileRisk {
                profile: "run-paper".to_string(),
                rows: Vec::new(),
            }]),
            active: None,
        },
    );
}

/// **The disclosure this block exists for, asserted on the SERIALIZED document** — the same
/// shape the ceilings half is asserted in, and for the same reason: a correct table that
/// nothing renders changes nothing.
///
/// Three claims, and the middle one is the load-bearing one:
///
/// 1. a stored VALUE is present, which is the whole of what the plane buys;
/// 2. an INACTIVE body is marked NOT enforced and read by NOTHING, while an ACTIVE one is
///    marked enforced and names its reader — the middle claim, and the one that changed;
/// 3. a row whose key is in no `[risk]` schema is marked `known: false` rather than rendered
///    as a ceiling.
#[test]
fn the_json_document_carries_the_mirrored_ceilings_and_calls_them_unenforced() {
    let d = vike_config::describe(None, &map(&[])).unwrap();
    let doc = settings_json(
        &d,
        &empty_store(),
        &[],
        &[],
        &UnknownKeys::default(),
        &mirrored_profiles(),
        &[],
    );
    let block = &doc["profile_risk"];
    assert_eq!(block["state"], "rows");
    assert_eq!(block["enforced"], false, "a STORED-but-unselected body binds nothing");
    assert!(block["read_by"].is_null(), "…so nothing on the mount path reads it");
    assert!(block["active"].is_null(), "…and nothing is selected");
    assert_eq!(block["profiles"][0]["active"], false);

    // ⚠ …and the ACTIVE arm, which is the whole reason these three fields stopped being
    // constants. A machine that read `enforced: false` over an active body would be wrong
    // about a live pre-trade ceiling.
    let live = settings_json(
        &d,
        &empty_store(),
        &[],
        &[],
        &UnknownKeys::default(),
        &active_profile(),
        &[],
    );
    let live = &live["profile_risk"];
    assert_eq!(live["active"], "run-live");
    assert_eq!(live["enforced"], true, "an ACTIVE body is what the daemon judges orders with");
    assert!(!live["read_by"].is_null(), "…and the document names its reader");
    assert_eq!(live["profiles"][0]["active"], true);

    let rows = block["profiles"][0]["rows"].as_array().expect("rows missing");
    let notional = rows
        .iter()
        .find(|r| r["key"] == "max_notional_per_order")
        .expect("the mirrored ceiling must be present — this is what Phase 2 buys");
    assert_eq!(notional["value"], "250.0");
    assert_eq!(notional["known"], true);
    assert_eq!(notional["shape"], "float");

    let bogus = rows.iter().find(|r| r["key"] == "max_levrage").expect("row missing");
    assert_eq!(bogus["known"], false, "a key in no `[risk]` schema bounds nothing");
    assert!(bogus["shape"].is_null());

    // The complement: the ceiling this profile does NOT set is named as unset, which is how a
    // machine learns that a live mount would refuse to start.
    let unset = block["profiles"][0]["unset"].as_array().expect("unset missing");
    assert!(
        unset.iter().any(|k| k.as_str() == Some("max_total_exposure")),
        "an unset mount-refusing ceiling must be named: {unset:?}"
    );
}

/// The THREE arms are three different facts, and the JSON must not collapse them. An
/// unmirrored box and a box whose store predates the table both have no rows, and neither of
/// them means "this profile sets no ceilings".
#[test]
fn an_unmirrored_box_and_a_pre_phase_two_store_are_distinguishable_in_the_json() {
    let d = vike_config::describe(None, &map(&[])).unwrap();
    let of = |src: &RunProfileRows| {
        settings_json(&d, &empty_store(), &[], &[], &UnknownKeys::default(), src, &[])["profile_risk"]
                ["state"]
                .clone()
    };
    assert_eq!(of(&no_profiles()), "no-database");
    assert_eq!(of(&table_absent()), "table-absent");
    assert_eq!(of(&mirrored_profiles()), "rows");
}

/// The end-to-end redaction property, asserted on the SERIALIZED document: whatever the stores
/// hold for a secret row, no byte of it reaches the `--json` output either — and the credential
/// store is disclosed by COUNT, never by a key name.
///
/// The UNMATCHED-key complement rides the same document, so it is driven here too, off a store
/// whose keys are both credential-shaped and unmatched: neither name may appear, and neither
/// value.
#[test]
fn the_json_document_cannot_leak_a_secret() {
    const LEAK: &str = "sk-do-not-print-me";
    let d = vike_config::describe(None, &map(&[])).unwrap();
    let env = map(&[("ACME_API_KEY", LEAK)]);
    let envs = vec![resolve(&row("ACME_API_KEY", ""), &env, &map(&[]), &FILES)];
    let store = StoreStatus { present: true, keys: 42, ..empty_store() };
    let unknown = unknown_store_keys(
        &map(&[("ACME_API_KEY", LEAK), ("SOMEVENUE_LIVE_API_SECRET", LEAK)]),
        None,
    );
    let text = serde_json::to_string(&settings_json(
        &d,
        &store,
        &[],
        &envs,
        &unknown,
        &no_profiles(),
        &[],
    ))
    .unwrap();
    assert!(!text.contains(LEAK), "{text}");
    assert!(text.contains(SET) && text.contains("\"secret\":true"), "{text}");
    assert!(text.contains("\"keys\":42"), "the store is disclosed by count only");
    assert!(!text.contains("SOMEVENUE_LIVE_API_SECRET"), "an unmatched credential NAME leaked");
    assert!(text.contains("\"credential_shaped\":2"), "…and is disclosed by count: {text}");
}

// -- the unmatched-store-key complement ------------------------------------------------------

/// The defect: a store key that matches no registry row produced NO row, so a typo'd variable
/// name was indistinguishable from one that was never set. It is now the complement — named
/// when its name is not credential-shaped.
#[test]
fn a_store_key_no_registry_row_covers_is_surfaced() {
    let declared = all_settings().next().expect("the registry is not empty").name;
    let u = unknown_store_keys(&map(&[(declared, "x"), ("ACME_NOT_A_SETTING", "1")]), None);
    assert_eq!(u.named, vec!["ACME_NOT_A_SETTING".to_string()]);
    assert_eq!(u.credential_shaped, 0, "a declared row is not unmatched, whatever its shape");
}

/// …and the half that keeps this from becoming a store dump: an unmatched key whose NAME is
/// credential-shaped is COUNTED, never named. `VIKE_NODE_CONTROL_KEY` — the real mis-spelling
/// of `VIKE_TRADEHUB_CONTROL_KEY` that prompted this — is deliberately in that bucket: its shape
/// is indistinguishable from a genuine node key's, and the fixture now SPELLS it rather than
/// standing in for it (until the settings-registry gate stopped harvesting test-region literals
/// as reads, a `VIKE_NODE_CONTROL_KEY` literal anywhere under `crates/` failed that gate).
#[test]
fn an_unmatched_credential_shaped_key_is_counted_never_named() {
    let u = unknown_store_keys(
        &map(&[
            ("VIKE_NODE_CONTROL_KEY", "hmac"),
            ("SOMEVENUE_LIVE_API_KEY", "k"),
            ("ACME_NOT_A_SETTING", "1"),
        ]),
        None,
    );
    assert_eq!(u.credential_shaped, 2);
    assert_eq!(u.named, vec!["ACME_NOT_A_SETTING".to_string()], "only the safe shape is named");
    assert!(
        !format!("{u:?}").contains("CONTROL_KEY") && !format!("{u:?}").contains("API_KEY"),
        "a credential-shaped name reached the struct: {u:?}"
    );
}

/// Nothing unaccounted for ⇒ nothing to print, so a block on screen always means something.
#[test]
fn a_fully_declared_store_has_no_complement() {
    let declared = all_settings().next().expect("the registry is not empty").name;
    assert!(unknown_store_keys(&map(&[(declared, "x")]), None).is_empty());
    assert!(unknown_store_keys(&map(&[]), None).is_empty());
}

/// The complement honours `--filter`, so `--filter node` narrows it the same way it narrows the
/// table above it.
#[test]
fn the_complement_honours_the_filter() {
    let store = map(&[("ACME_NOT_A_SETTING", "1"), ("ACME_OTHER_THING", "2")]);
    assert_eq!(
        unknown_store_keys(&store, Some("not_a")).named,
        vec!["ACME_NOT_A_SETTING".to_string()]
    );
    assert!(unknown_store_keys(&store, Some("no-such-key-anywhere")).is_empty());
}
