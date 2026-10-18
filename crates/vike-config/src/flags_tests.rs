use super::*;

#[test]
fn every_flag_defaults_off() {
    // Serialized rather than field-by-field: this cannot go stale when a field is added,
    // which a hand-written list of `assert!(!f.x)` silently does.
    let table = toml::Table::try_from(Flags::default()).unwrap();
    assert_eq!(table.len(), FLAG_REGISTRY.len());
    for (name, value) in &table {
        assert_eq!(value.as_bool(), Some(false), "{name} must default to false");
    }
}

#[test]
fn a_row_turns_a_flag_on_and_off() {
    let mut f = Flags::default();
    f.apply(FlagsPatch { tradehub_live: Some(true), ..Default::default() }, Path::new("flags"))
        .unwrap();
    assert!(f.tradehub_live);
    f.apply(FlagsPatch { tradehub_live: Some(false), ..Default::default() }, Path::new("flags"))
        .unwrap();
    assert!(!f.tradehub_live);
}

/// **No flag has an environment layer** (decision 0111): every [`FlagMeta::env`] is a
/// [`crate::REMOVED_ENV`] row, so a set variable refuses startup rather than configuring nothing.
#[test]
fn every_flag_variable_is_a_startup_refusal() {
    for m in FLAG_REGISTRY {
        assert!(
            crate::REMOVED_ENV.iter().any(|r| r.var == m.env),
            "{}: its variable {} is not refused at startup",
            m.field,
            m.env
        );
    }
}

/// A deleted flag key whose VARIABLE was retired too must not send the operator to that variable.
#[test]
fn a_removed_flag_key_whose_variable_was_retired_points_at_its_new_home() {
    for (key, line) in [
        ("poly_rate_gate", "vike-cli config set venue.polymarket.rate_gate 1"),
        ("poly_presubmit_register", "vike-cli config set venue.polymarket.presubmit_register 1"),
    ] {
        let patch: FlagsPatch = toml::from_str(&format!("{key} = true\n")).unwrap();
        let msg = Flags::default().apply(patch, Path::new("flags")).unwrap_err().to_string();
        assert!(msg.contains(line), "{msg}");
        assert!(!msg.contains("export POLY_"), "sends the operator to a retired variable: {msg}");
    }
}

/// ⚠ Every removed key REFUSES — `true` and `false` alike. A `false` written by an operator is
/// still a belief about this program's behaviour, and honouring the deletion silently for one
/// value and loudly for the other is the "settable but inert" shape the deletion removed.
#[test]
fn every_removed_flag_key_is_refused_and_names_its_variable() {
    for (key, var) in REMOVED_FLAG_KEYS {
        for written in [true, false] {
            let toml = format!("{key} = {written}\n");
            let patch: FlagsPatch = toml::from_str(&toml).unwrap();
            let err = Flags::default()
                .apply(patch, Path::new("flags.toml"))
                .expect_err("{key} must be refused");
            let msg = err.to_string();
            assert!(msg.contains(key), "{msg}");
            assert!(msg.contains(var), "{msg} must name {var}");
            assert!(msg.contains("no longer a flag"), "{msg}");
        }
    }
}

/// ...and the other direction: a removed key is NOT a `Flags` field any more, so it cannot be
/// reached through the registry either. Without this the tombstone could be deleted from
/// `REMOVED_FLAG_KEYS` and the field quietly re-added with nothing failing.
#[test]
fn no_removed_flag_key_is_still_a_field() {
    let table = toml::Table::try_from(Flags::default()).unwrap();
    for (key, _) in REMOVED_FLAG_KEYS {
        assert!(!table.contains_key(*key), "{key} is a tombstone and must not be a field");
        assert!(
            !FLAG_REGISTRY.iter().any(|m| m.field == *key),
            "{key} is a tombstone and must not have a registry row"
        );
    }
}

/// **The DEAD family refuses the key and does NOT send the operator to the variable** — the
/// whole reason it is a third table rather than a sixth row of [`REMOVED_FLAG_KEYS`].
///
/// The negative half is the load-bearing one: a message carrying *"export `VIKE_RECORD_DVOL`
/// instead"* would be [`REMOVED_FLAG_KEYS`]' sentence said about a variable that is equally
/// dead, which is a worse answer than saying nothing.
#[test]
fn every_dead_flag_key_is_refused_and_does_not_promise_a_working_variable() {
    for (key, var, survives) in DEAD_FLAG_KEYS {
        for written in [true, false] {
            let toml = format!("{key} = {written}\n");
            let patch: FlagsPatch = toml::from_str(&toml).unwrap();
            let err = Flags::default()
                .apply(patch, Path::new("flags.toml"))
                .expect_err("a dead key must be refused");
            let msg = err.to_string();
            assert!(msg.contains(key), "names the KEY: {msg}");
            assert!(msg.contains("EITHER spelling"), "says both spellings are dead: {msg}");
            assert!(msg.contains(survives), "names what SURVIVES: {msg}");
            assert!(
                !msg.contains(&format!("export {var}=1")),
                "must NOT send the operator to a variable that is equally dead: {msg}"
            );
        }
    }
}

/// …and the other direction, the twin of [`no_removed_flag_key_is_still_a_field`]: a dead key
/// is not a [`Flags`] field and has no [`FLAG_REGISTRY`] row, so the tombstone cannot be
/// deleted and the field quietly re-added with nothing failing.
#[test]
fn no_dead_flag_key_is_still_a_field() {
    let table = toml::Table::try_from(Flags::default()).unwrap();
    for (key, _, _) in DEAD_FLAG_KEYS {
        assert!(!table.contains_key(*key), "{key} is a tombstone and must not be a field");
        assert!(
            !FLAG_REGISTRY.iter().any(|m| m.field == *key),
            "{key} is a tombstone and must not have a registry row"
        );
    }
}

/// The two tombstone tables are DISJOINT, and each key belongs to exactly one — because the
/// two refusals say OPPOSITE things about the variable, and a key in both would get whichever
/// loop ran first.
#[test]
fn the_two_tombstone_families_do_not_overlap() {
    for (dead, _, _) in DEAD_FLAG_KEYS {
        assert!(
            !REMOVED_FLAG_KEYS.iter().any(|(k, _)| k == dead),
            "{dead} cannot be both REMOVED (export the variable) and DEAD (nothing works)"
        );
    }
}

/// A removed file key whose variable is a D4 retirement says nothing starts it, and offers neither
/// the variable nor a row.
#[test]
fn a_removed_chain_flag_key_says_nothing_starts_it() {
    for (key, var) in
        [("poly_chain_watch", "POLY_CHAIN_WATCH"), ("poly_chain_proxy", "POLY_CHAIN_PROXY")]
    {
        let patch: FlagsPatch = toml::from_str(&format!("{key} = true\n")).unwrap();
        let msg = Flags::default().apply(patch, Path::new("flags")).unwrap_err().to_string();
        assert!(msg.contains("nothing starts"), "{msg}");
        assert!(!msg.contains(&format!("export {var}=1")), "{msg}");
        assert!(!msg.contains("vike-cli config set"), "there is no row to write: {msg}");
    }
}

#[test]
fn flag_meta_resolves_a_field_and_rejects_an_unknown_one() {
    assert_eq!(flag_meta("reconcile").map(|m| m.env), Some(RECONCILE_ENV));
    assert_eq!(flag_meta("no_such_flag"), None);
}

/// Decision 0095: the two removed file keys whose variables were retired point at the fields that
/// replaced both — never at the variable, which now refuses startup.
#[test]
fn a_removed_toggle_key_points_at_its_venue_field() {
    for (key, line) in [
        ("bybit_fast_exec", "vike-cli config set venue.bybit.fast_exec 1"),
        ("binance_trade_lite_fill", "vike-cli config set venue.binance.trade_lite_fill 1"),
    ] {
        let patch: FlagsPatch = toml::from_str(&format!("{key} = true\n")).unwrap();
        let msg = Flags::default().apply(patch, Path::new("flags")).unwrap_err().to_string();
        assert!(msg.contains(line), "{msg}");
        assert!(!msg.contains("export VIKE_"), "sends the operator to a retired variable: {msg}");
    }
}
