//! Unit tests for the credential editor's write context and the key names its upsert writes.

use super::*;
use vike_model::change_journal::Proc;

// NOTE: every value below is a dummy placeholder ("test-key-123" etc), never a real secret.
//
// The writer's OWN suite lives with it in `vike_secrets::store`.

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

    assert_eq!(home.settings_dir(), settings);
    assert_eq!(
        home.journal().expect("a state dir yields a ledger").dir(),
        state.join(vike_model::change_journal::CHANGES_SUBDIR)
    );
    // …and the two really are siblings under one project, which is the whole point: a ledger
    // describing a store in a DIFFERENT project is the failure this derivation removes.
    assert_eq!(
        home.journal().unwrap().dir().parent().and_then(Path::parent),
        Some(home.settings_dir())
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
    // The settings directory still resolves, because a write has to go somewhere.
    assert_eq!(home.settings_dir(), Path::new("/srv/vike-<unit>/settings"));
}

/// An unresolved settings directory falls back to the bare walk — byte-identical to what both
/// call sites did before this type existed, so the no-project case is unchanged rather than
/// newly refused.
#[test]
fn a_settings_less_boot_falls_back_to_the_historical_walk() {
    let home = CredentialHome::resolve(None, None, Proc::new("vike-test", 1, "0"));
    assert_eq!(home.settings_dir(), vike_secrets::workspace_settings_dir_from(None));
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
    assert_eq!(a.settings_dir, home.settings_dir());
    assert!(a.journal.is_some() && b.journal.is_some());
    assert_eq!((a.proc.bin.as_str(), b.proc.bin.as_str()), ("vike-test", "vike-test"));
}
