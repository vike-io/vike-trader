use super::*;
use std::io;
use vike_tradehub_client::wire::WireSettingsRow;

fn show() -> WireSettingsShow {
    WireSettingsShow {
        settings_dir: Some("/srv/node/settings".into()),
        rows: vec![WireSettingsRow {
            section: "config.toml".into(),
            key: "config.tradehub_addr".into(),
            value: "127.0.0.1:7879".into(),
            origin: "config.toml".into(),
            read_by: "tradehub".into(),
        }],
    }
}

/// The three fetch outcomes map onto the three render states — and the feature refusal
/// (`Unsupported`) is what the section renders as "server predates settings-show".
#[test]
fn fetch_outcomes_map_to_render_states() {
    assert_eq!(settings_fetch_state(Ok(show())), BackendSettingsState::Loaded(show()));
    assert_eq!(
        settings_fetch_state(Err(io::Error::new(io::ErrorKind::Unsupported, "no feature"))),
        BackendSettingsState::Unsupported
    );
    let e = settings_fetch_state(Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad mac")));
    match e {
        BackendSettingsState::Error(msg) => assert!(msg.contains("bad mac")),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// Auto-fetch fires exactly once per backend: only an ACTIVE backend in the `Idle` state —
/// `Pending` guards re-entry, terminal states wait for an explicit Refresh, and no active
/// backend means nothing to ask.
#[test]
fn auto_fetch_fires_only_for_an_idle_active_backend() {
    assert!(should_fetch_settings(true, &BackendSettingsState::Idle));
    assert!(!should_fetch_settings(false, &BackendSettingsState::Idle));
    assert!(!should_fetch_settings(true, &BackendSettingsState::Pending));
    assert!(!should_fetch_settings(true, &BackendSettingsState::Loaded(show())));
    assert!(!should_fetch_settings(true, &BackendSettingsState::Unsupported));
    assert!(!should_fetch_settings(true, &BackendSettingsState::Error("x".into())));
}

// ------------------------------------------------------------------------------------------
// The WRITE half's edit flow (REQ-7)
// ------------------------------------------------------------------------------------------

fn policy_row() -> WireSettingsRow {
    WireSettingsRow {
        section: "policy.toml".into(),
        key: "policy.max_notional_per_order".into(),
        value: "500".into(),
        origin: "policy.toml".into(),
        read_by: "tradehub".into(),
    }
}

fn config_row() -> WireSettingsRow {
    WireSettingsRow {
        section: "config.toml".into(),
        key: "config.tradehub_addr".into(),
        value: "127.0.0.1:7879".into(),
        origin: "config.toml".into(),
        read_by: "tradehub".into(),
    }
}

/// One row per plane the editor can open: a RISK ceiling, an ARMING ceiling (both `policy.*`), a
/// `config` key and a `flags` key.
fn every_plane() -> Vec<WireSettingsRow> {
    vec![
        policy_row(),
        WireSettingsRow {
            key: "policy.venues.binance".into(),
            value: "paper".into(),
            ..policy_row()
        },
        config_row(),
        WireSettingsRow {
            section: "flags.toml".into(),
            key: "flags.tradehub_control".into(),
            value: "false".into(),
            origin: "db".into(),
            read_by: "tradehub".into(),
        },
    ]
}

/// ⚠ **No typed confirm, for ANY key — the risk ceilings included**
/// (`docs/decisions/0086-settings-live-only-in-the-database.md` point 7: *"confirmation over
/// confirmation … a nightmare"*). Every row's edit takes on the FIRST Save: there is no buffer the
/// operator must fill before it, and the request carries the key and the value. What guards a live
/// limit is the daemon's loader bounds check and its old → new report, which run on every write.
#[test]
fn every_row_saves_without_a_typed_confirm_policy_ceilings_included() {
    for row in every_plane() {
        let mut edit = start_edit(&row);
        let req = take_save(&mut edit)
            .unwrap_or_else(|| panic!("{} must save with no typed confirm: {edit:?}", row.key));
        assert!(
            matches!(&edit, SettingsEditState::Saving { key } if key == &row.key),
            "{}: {edit:?}",
            row.key
        );
        let SettingsWriteRequest { key, value, .. } = req;
        assert_eq!(key, row.key);
        assert_eq!(value, row.value);
    }
}

/// A policy ceiling's edit, end to end as pure transitions: the value buffer starts at the row's
/// current value, ONE Save takes it, and the node's acceptance folds into
/// `Saved { restart_required }` — the state that renders [`SAVED_RESTART_NOTE`] and tells the
/// binary to refetch.
#[test]
fn a_policy_edit_walks_edit_saved_restart_required() {
    let mut edit = start_edit(&policy_row());
    match &mut edit {
        SettingsEditState::Editing { file, key, value } => {
            assert_eq!(file, "policy.toml");
            assert_eq!(key, "policy.max_notional_per_order");
            assert_eq!(value, "500", "the value buffer starts at the row's current value");
            *value = "250".to_string();
        }
        other => panic!("expected Editing, got {other:?}"),
    }
    let req = take_save(&mut edit).expect("a policy ceiling saves on the first Save");
    assert_eq!(
        (req.file.as_str(), req.key.as_str(), req.value.as_str()),
        ("policy.toml", "policy.max_notional_per_order", "250")
    );
    assert!(matches!(&edit, SettingsEditState::Saving { key } if key == &req.key), "{edit:?}");

    edit = settings_write_state(&req.key, Ok(true));
    assert_eq!(
        edit,
        SettingsEditState::Saved {
            key: "policy.max_notional_per_order".into(),
            restart_required: true,
        }
    );
    assert!(should_refetch_after_write(&edit));
}

/// The write outcomes fold like the fetch's: acceptance keeps the reply's restart flag, the
/// client-side feature refusal renders as "server predates settings-write", any other fault
/// (the daemon's refusal text — the loader's message) verbatim — and
/// only a SAVED flow triggers the refetch (a refusal changed no byte).
#[test]
fn write_outcomes_map_to_flow_states() {
    let saved = settings_write_state("k", Ok(false));
    assert_eq!(saved, SettingsEditState::Saved { key: "k".into(), restart_required: false });

    let unsupported =
        settings_write_state("k", Err(io::Error::new(io::ErrorKind::Unsupported, "no feature")));
    assert_eq!(
        unsupported,
        SettingsEditState::Failed { key: "k".into(), error: PREDATES_SETTINGS_WRITE.into() }
    );
    assert!(!should_refetch_after_write(&unsupported));

    let refused = settings_write_state(
        "k",
        Err(io::Error::new(io::ErrorKind::InvalidData, "unknown field `tradehub_adr`")),
    );
    match refused {
        SettingsEditState::Failed { error, .. } => {
            assert!(error.contains("tradehub_adr"), "the daemon's text verbatim: {error}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------
// The derivations the redesigned panel renders from
// ------------------------------------------------------------------------------------------

/// ⚠ Every ORIGIN kind, re-derived from the LABEL because the wire drops
/// `Origin::kind()`. `env:VAR` and a file name are DIFFERENT answers and only `default` means
/// unset — collapsing the first two is what would hide the env-outranks-the-row hazard.
#[test]
fn the_origin_kind_is_re_derived_from_the_label_the_wire_carries() {
    assert_eq!(origin_kind("default"), "default");
    assert_eq!(origin_kind("env:VIKE_RECONCILE"), "env");
    assert_eq!(origin_kind("config.toml"), "file");
    assert_eq!(origin_kind("policy.toml"), "file");
    // ⚠ Only the EXACT word is the default — a file that happened to be called
    // `defaults.toml` is a file.
    assert_eq!(origin_kind("defaults.toml"), "file");
    // ⚠ The SETTINGS DATABASE (decision 0057 Phase 1). Without this arm the catch-all below
    // renders a row the store set as `file` — an operator sent to open a file that says
    // nothing about it. `db.toml` stays a file for the same reason `defaults.toml` does.
    assert_eq!(origin_kind("db"), "db");
    assert_eq!(origin_kind("db.toml"), "file");
}

/// ⚠ THE ENV-SHADOW TEST, and the asymmetry that makes it safe for policy. A `config` row set
/// from the environment is shadowed; a `policy` row can never be, because `Policy` implements
/// neither `EnvOverride` nor `CliOverride` (both sealed) and every policy row is built with an
/// empty env slice.
#[test]
fn a_row_set_from_the_environment_is_shadowed_and_a_policy_row_never_is() {
    let shadowed = WireSettingsRow {
        section: "config.toml".into(),
        key: "config.tradehub_addr".into(),
        value: "0.0.0.0:7879".into(),
        origin: "env:VIKE_TRADEHUB_ADDR".into(),
        read_by: "tradehub".into(),
    };
    assert_eq!(env_shadow(&shadowed), Some("VIKE_TRADEHUB_ADDR"));

    assert_eq!(env_shadow(&config_row()), None, "a file origin shadows nothing");
    let from_db = WireSettingsRow { origin: "db".into(), ..config_row() };
    assert_eq!(env_shadow(&from_db), None, "a database row shadows nothing");
    assert_eq!(env_shadow(&policy_row()), None, "policy has no env layer at all");
    let defaulted = WireSettingsRow { origin: "default".into(), ..config_row() };
    assert_eq!(env_shadow(&defaulted), None);
}

/// SET is PRESENCE in a layer, exactly as `vike_config::describe` measures it —
/// never a diff against the compiled-in default (which the wire does not even carry).
#[test]
fn set_is_presence_in_a_layer_and_the_finding_needs_both_halves() {
    let file = config_row();
    assert!(row_is_set(&file));
    assert!(!row_read_by_nothing(&file));
    assert!(!row_is_finding(&file), "set, but something reads it");

    let unset_unread =
        WireSettingsRow { origin: "default".into(), read_by: "NO".into(), ..config_row() };
    assert!(!row_is_set(&unset_unread));
    assert!(row_read_by_nothing(&unset_unread));
    assert!(
        !row_is_finding(&unset_unread),
        "a key NOBODY configured and nothing reads is not a misconfiguration"
    );

    let finding = WireSettingsRow { read_by: "NO".into(), ..config_row() };
    assert!(row_is_finding(&finding), "set AND read by nothing is the amber row");

    // The third READ value — a library read — is not `NO`.
    let library = WireSettingsRow { read_by: "yes".into(), ..config_row() };
    assert!(!row_read_by_nothing(&library));
}

/// The editor says exactly what it will write and WHERE, before the click: one row of the node's
/// settings DATABASE, under the directory the node reported — and when the node reported none it
/// says there is no database, which is the same condition the daemon's write half refuses with.
///
/// ⚠ This joined the row's FILE name (`<dir>/policy.toml`) until 0086 was applied to this screen;
/// no settings file is read or written by anything, so no target may name one.
#[test]
fn the_write_target_is_the_nodes_settings_database_never_a_file() {
    assert_eq!(
        write_target(Some("/srv/vike-<unit>/settings")).as_deref(),
        Some("/srv/vike-<unit>/settings/db/vike.db")
    );
    // A trailing separator does not double up.
    assert_eq!(
        write_target(Some("/srv/vike-<unit>/settings/")).as_deref(),
        Some("/srv/vike-<unit>/settings/db/vike.db")
    );
    // A Windows-shaped directory keeps its own separator — the node renders the path, this
    // side only joins it, so it must not impose a POSIX spelling on a path it did not make.
    assert_eq!(
        write_target(Some("C:\\vike\\settings")).as_deref(),
        Some("C:\\vike\\settings\\db\\vike.db")
    );
    assert_eq!(write_target(None), None, "no directory ⇒ no database to name");
}

/// The digest's five states, and the one that carries numbers is the only one that does.
#[test]
fn the_digest_models_every_fetch_state_and_invents_no_count() {
    assert_eq!(
        BackendDigest::of(false, &BackendSettingsState::Loaded(show())),
        BackendDigest::NoBackend
    );
    assert!(BackendDigest::NoBackend.line().contains("no backend connected"));
    assert!(BackendDigest::of(true, &BackendSettingsState::Pending).line().contains("reading"));
    assert!(
        BackendDigest::of(true, &BackendSettingsState::Unsupported)
            .line()
            .contains(PREDATES_SETTINGS_SHOW)
    );
    let err = BackendDigest::of(true, &BackendSettingsState::Error("bad mac".into()));
    assert!(err.line().contains("bad mac"), "{}", err.line());
    assert_eq!(err.badge_count(), None);

    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    assert_eq!(loaded.badge_count(), Some(1), "the one row, set from config.toml");
    assert!(!loaded.has_finding());
    assert!(loaded.line().contains("1 keys · 1 set"), "{}", loaded.line());
}

/// `take_save` is a no-op outside `Editing` — a double-click on Save, or a click landing
/// after the outcome folded, cannot enqueue a second write.
#[test]
fn take_save_refuses_outside_editing() {
    for mut state in [
        SettingsEditState::Idle,
        SettingsEditState::Saving { key: "k".into() },
        SettingsEditState::Saved { key: "k".into(), restart_required: true },
        SettingsEditState::Failed { key: "k".into(), error: "e".into() },
    ] {
        let before = state.clone();
        assert_eq!(take_save(&mut state), None);
        assert_eq!(state, before, "a refused take leaves the state alone");
    }
}
