//! Unit tests for the credential editor's write context and the key names its upsert writes.

use super::*;
use vike_bridge_core::credentials::parse_dotenv;
use vike_model::change_journal::Proc;

// NOTE: every value below is a dummy placeholder ("test-key-123" etc), never a real secret.
//
// The transform's OWN suite (byte-preservation, ordering, quoting, atomic write) lives with the
// implementation in `vike_secrets::env_write`. What stays here is the property this crate is
// responsible for: that the GUI writes the exact KEY NAMES the venue loaders read.

/// Dukascopy's DEMO2 keys (the EU demo account, added alongside DEMO1's existing edit form)
/// upsert under the exact names `crate::status::arms::dukascopy_configured` and
/// `vike_dukascopy::config` read, and leave DEMO1 + any unrelated key untouched.
#[test]
fn dukascopy_demo2_keys_write_correct_names_and_preserve_others() {
    let existing = "\
DUKASCOPY_DEMO1_LOGIN=demo1-login-old
DUKASCOPY_DEMO1_PASSWORD=demo1-pass-old
UNRELATED_KEY=keep-me
";
    let updates = vec![
        ("DUKASCOPY_DEMO2_LOGIN".to_string(), "demo2-login-new".to_string()),
        ("DUKASCOPY_DEMO2_PASSWORD".to_string(), "demo2-pass-new".to_string()),
    ];
    let out = vike_secrets::upsert_env(existing, &updates);

    assert!(out.contains("DUKASCOPY_DEMO2_LOGIN=demo2-login-new"));
    assert!(out.contains("DUKASCOPY_DEMO2_PASSWORD=demo2-pass-new"));
    // DEMO1 and the unrelated key are untouched.
    assert!(out.contains("DUKASCOPY_DEMO1_LOGIN=demo1-login-old"));
    assert!(out.contains("DUKASCOPY_DEMO1_PASSWORD=demo1-pass-old"));
    assert!(out.contains("UNRELATED_KEY=keep-me"));

    // Round-trips through the real dotenv parser with the right key names.
    let parsed = parse_dotenv(&out);
    assert_eq!(parsed.get("DUKASCOPY_DEMO2_LOGIN").map(String::as_str), Some("demo2-login-new"));
    assert_eq!(parsed.get("DUKASCOPY_DEMO2_PASSWORD").map(String::as_str), Some("demo2-pass-new"));
}

/// ONE walk decides: both homes come off the composition root's own answers, and the ledger
/// sits under the SAME `<project>/settings` the store does.
///
/// This is the property the GUI had backwards — the grid read an override-honouring path while
/// Save wrote a blind walk's — and it is asserted here rather than in `vike-desktop` deliberately:
/// that crate is outside the derived CI roster, so an assertion living there would be run by
/// nothing.
#[test]
fn both_homes_are_derived_from_the_boots_own_two_answers() {
    let settings = Path::new("/srv/vike-<unit>/settings");
    let state = settings.join("state");
    let home =
        CredentialHome::resolve(Some(settings), Some(&state), Proc::new("vike-test", 1, "0"));

    assert_eq!(home.store(), settings.join(vike_secrets::SECRETS_FILE));
    assert_eq!(
        home.journal().expect("a state dir yields a ledger").dir(),
        state.join(vike_model::change_journal::CHANGES_SUBDIR)
    );
    // …and the two really are siblings under one project, which is the whole point: a ledger
    // describing a store in a DIFFERENT project is the failure this derivation removes.
    assert_eq!(
        home.journal().unwrap().dir().parent().and_then(Path::parent),
        home.store().parent()
    );
    assert_eq!(home.proc().bin, "vike-test");
}

/// No state directory ⇒ NO ledger. Nothing is recorded, rather than an append-only record in a
/// guessed directory — the `None`-handle behaviour `vike_model::change_journal` already pins.
#[test]
fn a_project_less_boot_gets_no_ledger_rather_than_an_invented_one() {
    let home = CredentialHome::resolve(
        Some(Path::new("/srv/vike-<unit>/settings")),
        None,
        Proc::new("vike-test", 1, "0"),
    );
    assert!(home.journal().is_none(), "a guessed ledger location is worse than none");
    assert!(home.write_ctx(0).journal.is_none(), "…and the per-write context agrees");
    // The STORE still resolves, because a write has to go somewhere.
    assert!(home.store().ends_with(vike_secrets::SECRETS_FILE));
}

/// An unresolved settings directory falls back to the bare walk — byte-identical to what both
/// call sites did before this type existed, so the no-project case is unchanged rather than
/// newly refused.
#[test]
fn a_settings_less_boot_falls_back_to_the_historical_walk() {
    let home = CredentialHome::resolve(None, None, Proc::new("vike-test", 1, "0"));
    assert_eq!(home.store(), vike_secrets::workspace_dotenv_path());
    assert_eq!(
        home.store().file_name().and_then(|n| n.to_str()),
        Some(vike_secrets::SECRETS_FILE),
        "whatever the walk answered, it still names the store"
    );
}

/// `write_ctx` hands out exactly what it holds, and the INSTANT is per-write — the journal reads
/// no clock, so two records from one home can carry two timestamps.
#[test]
fn the_write_context_carries_the_homes_paths_and_the_callers_instant() {
    let settings = Path::new("/srv/vike-<unit>/settings");
    let home = CredentialHome::resolve(
        Some(settings),
        Some(&settings.join("state")),
        Proc::new("vike-test", 1, "0"),
    );
    let a = home.write_ctx(111);
    let b = home.write_ctx(222);
    assert_eq!((a.now_ms, b.now_ms), (111, 222));
    assert_eq!(a.store, home.store());
    assert!(a.journal.is_some() && b.journal.is_some());
    assert_eq!((a.proc.bin.as_str(), b.proc.bin.as_str()), ("vike-test", "vike-test"));
}
