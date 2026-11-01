use std::collections::BTreeMap;

use super::*;

/// A profile with nothing in it but its identity row — enough for every question
/// [`Profiles::resolve_active`] asks.
fn row(name: &str, kind: ProfileKind, active: bool) -> StoredProfile {
    StoredProfile {
        row: ProfileRow { name: name.to_string(), kind, active, note: None },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings: BTreeMap::new(),
        recorder: None,
    }
}

/// **The three noes are three ANSWERS, not one.** This is the property the CLI's refusal for a
/// missing recorder profile rests on: an unmigrated box and a migrated box with no recorder
/// profile print different next commands, and before this resolver both looked like `None`.
#[test]
fn each_no_is_distinguishable_from_the_others() {
    assert_eq!(
        Profiles::none().resolve_active(ProfileKind::Recorder),
        ActiveProfile::NoProfileStore,
        "a box with no settings database, or one written before the profile tables"
    );
    let other_kind = Profiles::from_rows(vec![row("tradehub", ProfileKind::Daemon, true)]);
    assert_eq!(
        other_kind.resolve_active(ProfileKind::Recorder),
        ActiveProfile::NoneStored,
        "the store is there and holds no recorder profile at all"
    );
    let unselected = Profiles::from_rows(vec![
        row("a", ProfileKind::Recorder, false),
        row("b", ProfileKind::Recorder, false),
    ]);
    assert_eq!(
        unselected.resolve_active(ProfileKind::Recorder),
        ActiveProfile::NoneActive { stored: 2 },
        "two recorder profiles are stored and the operator has selected neither"
    );
}

/// The ordinary answer, and that the COUNT in `NoneActive` counts this kind rather than the
/// table — a resolver that counted every row would tell an operator they have profiles of a
/// kind they have never written.
#[test]
fn the_active_row_of_the_asked_kind_wins_and_the_count_is_per_kind() {
    let profiles = Profiles::from_rows(vec![
        row("tradehub", ProfileKind::Daemon, true),
        row("live", ProfileKind::Run, true),
        row("default", ProfileKind::Recorder, true),
    ]);
    let ActiveProfile::Row(found) = profiles.resolve_active(ProfileKind::Recorder) else {
        panic!("the recorder profile is active and must resolve");
    };
    assert_eq!(found.row.name, "default");

    let mixed = Profiles::from_rows(vec![
        row("tradehub", ProfileKind::Daemon, false),
        row("live", ProfileKind::Run, false),
        row("only", ProfileKind::Recorder, false),
    ]);
    assert_eq!(
        mixed.resolve_active(ProfileKind::Recorder),
        ActiveProfile::NoneActive { stored: 1 },
        "three profiles are stored and exactly one of them is a recorder profile"
    );
}

/// **The tie the schema forbids is still broken DETERMINISTICALLY, and not by row order.**
///
/// `profile_one_active_per_kind` means a real store cannot reach this state (see
/// `the_schema_refuses_a_second_active_row_of_one_kind` below), but [`Profiles::from_rows`]
/// takes rows a caller invented, and an `iter().find()` would answer whichever of them was
/// pushed first. Two callers building the same set in different orders would then disagree
/// about what this box records — which is the exact failure [`Profiles::resolve_active`] exists
/// to make impossible.
#[test]
fn two_active_rows_resolve_to_the_lowest_name_whichever_order_they_arrive_in() {
    let forwards = Profiles::from_rows(vec![
        row("alpha", ProfileKind::Recorder, true),
        row("omega", ProfileKind::Recorder, true),
    ]);
    let backwards = Profiles::from_rows(vec![
        row("omega", ProfileKind::Recorder, true),
        row("alpha", ProfileKind::Recorder, true),
    ]);
    for (order, profiles) in [("forwards", &forwards), ("backwards", &backwards)] {
        let ActiveProfile::Row(found) = profiles.resolve_active(ProfileKind::Recorder) else {
            panic!("{order}: an active row is present and must resolve");
        };
        assert_eq!(
            found.row.name, "alpha",
            "{order}: the answer must not depend on the order rows were pushed in"
        );
    }
}

/// **[`Profiles::active`] is a PROJECTION of the resolver, never a second resolution.** Checked
/// across every state rather than asserted in prose: if the two were ever written separately,
/// the state they disagreed in would be exactly the one nobody thought about.
#[test]
fn active_agrees_with_the_resolver_in_every_state() {
    let states = [
        Profiles::none(),
        Profiles::from_rows(Vec::new()),
        Profiles::from_rows(vec![row("tradehub", ProfileKind::Daemon, true)]),
        Profiles::from_rows(vec![row("a", ProfileKind::Recorder, false)]),
        Profiles::from_rows(vec![
            row("a", ProfileKind::Recorder, false),
            row("b", ProfileKind::Recorder, true),
        ]),
    ];
    for (i, profiles) in states.iter().enumerate() {
        for kind in [ProfileKind::Daemon, ProfileKind::Run, ProfileKind::Recorder] {
            let projected = profiles.active(kind).map(|p| p.row.name.as_str());
            let resolved = match profiles.resolve_active(kind) {
                ActiveProfile::Row(p) => Some(p.row.name.as_str()),
                ActiveProfile::NoProfileStore
                | ActiveProfile::NoneStored
                | ActiveProfile::NoneActive { .. } => None,
            };
            assert_eq!(projected, resolved, "state {i}, kind {}", kind.sql_word());
        }
    }
}

/// **The schema is the constraint the resolver's tie-break is a backstop for** — measured here
/// rather than assumed, because [`Profiles::resolve_active`]'s doc claims it.
#[test]
fn the_schema_refuses_a_second_active_row_of_one_kind() {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory store");
    conn.execute_batch(&profile_ddl(&["CryptoSpot"]).expect("renders")).expect("schema applies");
    conn.execute("INSERT INTO profile (name, kind, active) VALUES ('a', 'recorder', 1)", [])
        .expect("the first active recorder profile is legal");
    let err = conn
        .execute("INSERT INTO profile (name, kind, active) VALUES ('b', 'recorder', 1)", [])
        .expect_err("a second active row of one kind must be refused by the schema");
    assert!(
        err.to_string().to_uppercase().contains("UNIQUE"),
        "expected the profile_one_active_per_kind index to refuse it, got: {err}"
    );
    conn.execute("INSERT INTO profile (name, kind, active) VALUES ('c', 'daemon', 1)", [])
        .expect("the index is per KIND — a daemon profile may be active beside a recorder one");
}
