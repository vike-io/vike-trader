//! The account-admin barrier (`account_admin_source`, decision 0065 §3c).

use super::*;
use crate::hub_server::resolve_daemon_hub_key_store;

// ---------------------------------------------------------------------------------------------
// THE ACCOUNT-ADMIN BARRIER (`account_admin_source`) — `docs/decisions/0065-accounts-are-
// managed-and-the-barrier-is-declared.md` §3c, the ONE site that decides whether this daemon
// can write credentials from the wire.
//
// ⚠ Every assertion below is about an ABSENCE, which is the whole of Part 1: `None` means the
// process holds no writer for a frame to reach, so the verb is refused *because there is
// nothing in the process to refuse with*. A test that only checked a refusal MESSAGE would be
// checking the wrong thing — the message is what a client sees when the capability is absent,
// and the capability's absence is what makes the message true.
// ---------------------------------------------------------------------------------------------

fn loopback_bind() -> Vec<std::net::SocketAddr> {
    vec!["127.0.0.1:17879".parse().expect("a literal loopback address parses")]
}

/// A bind the process CAN see is wide — the `0.0.0.0` wildcard, which `bind_exposure`
/// classifies as `Public` deliberately because it is the commonest way this surface gets
/// accidentally exposed.
fn wide_bind() -> Vec<std::net::SocketAddr> {
    vec!["0.0.0.0:17879".parse().expect("a literal wildcard address parses")]
}

/// The settings directory these tests hand in. It never has to EXIST: this function decides a
/// capability and opens no store — the store is opened per frame, by the server.
fn any_dir() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent/settings")
}

/// ⚠ **THE BARRIER'S ONE CHECK.** `loopback` is the value whose assertion the process can
/// verify, so it is the one it verifies: a wide bind under that declaration arms NOTHING, and
/// the daemon keeps trading with the capability down rather than publishing a key-material
/// surface on an address anyone can reach.
///
/// This is the refusal `docs/decisions/0065` §3c Part 3 promised, landed where the process has
/// evidence for it rather than at a frame where it has none.
#[test]
fn a_wide_bind_refuses_the_account_capability_under_the_loopback_declaration() {
    assert!(
        account_admin_source(
            Some("loopback"),
            &wide_bind(),
            Some(any_dir()),
            vike_secrets::CredentialScope::Full
        )
        .is_none(),
        "a `loopback` declaration over a non-loopback bind must arm NO account capability — \
             the declaration is the one assertion this process can check, and it is false here"
    );
}

/// ⚠ …and `tradehub_allow_public_bind` CANNOT waive it, which is the shape copied from
/// `vike_datahub_client::bind`'s unwaivable `RefuseUnauthenticated` arm.
///
/// The proof is STRUCTURAL rather than one more assertion about a flag: that flag is not an
/// input to this function at all, so there is no parameter through which consent to publish an
/// ORDER surface could become consent to publish a key-material one. An operator who
/// genuinely wants the verbs on a wide bind says `contained`, which asserts a barrier OUTSIDE
/// the process rather than permission for there to be none.
#[test]
fn the_public_bind_opt_in_cannot_waive_the_loopback_refusal() {
    assert!(
        account_admin_source(
            Some("loopback"),
            &wide_bind(),
            Some(any_dir()),
            vike_secrets::CredentialScope::Full
        )
        .is_none(),
        "no value reachable from this function's inputs may waive the loopback check"
    );
}

/// The declaration the process CANNOT check arms regardless of the bind — and that is the
/// point of the third value rather than a hole in the second. `docs/decisions/0026` forbids
/// INFERRING a container's containment; it does not forbid an operator DECLARING it, and a
/// declared barrier is auditable in a way an inferred one is not.
#[test]
fn contained_arms_regardless_of_the_bind() {
    let armed = account_admin_source(
        Some("contained"),
        &wide_bind(),
        Some(any_dir()),
        vike_secrets::CredentialScope::Full,
    )
    .expect("`contained` is an assertion about a barrier outside this process");
    assert_eq!(armed.barrier, server::accounts::AccountBarrier::Contained);
    assert_eq!(armed.settings_dir, any_dir());
}

/// The ordinary armed case: declared `loopback`, bound on loopback.
#[test]
fn a_loopback_declaration_over_a_loopback_bind_arms_the_capability() {
    let armed = account_admin_source(
        Some("loopback"),
        &loopback_bind(),
        Some(any_dir()),
        vike_secrets::CredentialScope::Full,
    )
    .expect("the one shape this process can verify, verified");
    assert_eq!(armed.barrier, server::accounts::AccountBarrier::Loopback);
}

/// ⚠ **A TYPO IS OFF, NOT AN ERROR AND NOT A GUESS.** `loopbak` must never arm a credential
/// surface, and the safe direction is the one where the capability does not exist. The daemon
/// logs the value it did not recognise, so an operator who typed one is told rather than left
/// believing the barrier is up.
#[test]
fn an_unrecognised_declaration_arms_nothing() {
    for typo in ["loopbak", "true", "1", "yes", "LOOPBACK", "on", "contained "] {
        let armed = account_admin_source(
            Some(typo),
            &loopback_bind(),
            Some(any_dir()),
            vike_secrets::CredentialScope::Full,
        );
        // ⚠ `"contained "` is in this list to pin the TRIM, not to refuse it: a trailing space
        // off a paste is not a typo, and it arms. Every other spelling here must not.
        if typo.trim() == "contained" {
            assert!(armed.is_some(), "a trailing space is trimmed, not refused");
        } else {
            assert!(armed.is_none(), "`{typo}` is not one of the two spellings");
        }
    }
}

/// Unset, blank and `off` are the DEFAULT, and the default is byte-identical to a binary
/// without the verb: no capability, no feature advertised.
#[test]
fn unset_blank_and_off_are_all_the_default() {
    for declaration in [None, Some(""), Some("   "), Some("off")] {
        assert!(
            account_admin_source(
                declaration,
                &loopback_bind(),
                Some(any_dir()),
                vike_secrets::CredentialScope::Full
            )
            .is_none(),
            "{declaration:?} must leave this daemon byte-identical to one without the verb"
        );
    }
}

/// **NO KEY GATES THE ARMING since decision 0288** (Q3 = A, 2026-11-16): a declared barrier over an
/// agreeing bind arms with no admin key in the store, because who may USE the surface is the admin
/// ROLE of a device on the access list, decided per connection. `docs/decisions/0065`'s named
/// self-escalation path (a Control peer writing the declaration into a settings row) still reaches
/// nothing: it holds no admin row.
#[test]
fn a_declaration_arms_with_no_admin_key() {
    for declaration in ["loopback", "contained"] {
        assert!(
            account_admin_source(
                Some(declaration),
                &loopback_bind(),
                Some(any_dir()),
                vike_secrets::CredentialScope::Full
            )
            .is_some(),
            "`{declaration}` arms on the declaration and the bind alone"
        );
    }
}

/// A daemon whose BOOT resolved no project has no store to administer, and resolving one here
/// would be the `$VIKE_SETTINGS_DIR`-blind resolver's failure wearing a new surface.
#[test]
fn a_declaration_with_no_settings_directory_arms_nothing() {
    assert!(
        account_admin_source(
            Some("contained"),
            &loopback_bind(),
            None,
            vike_secrets::CredentialScope::Full
        )
        .is_none(),
        "no settings directory means no store to administer"
    );
}

/// **THE DAEMON'S OWN NODE-STORE READ HOLDS ITS TWO CLIENT KEYS AND NO PERSON KEY** (decision 0288 item 8, the
/// cutover): the tradehub admits people by the access list alone, so its one `hub_key` read carries the
/// gatewayhub's Trade key and the datahub's strategy-lane key (0164's daemon class, which it HOLDS to reach those
/// daemons and verifies nothing with) and nothing else — not its own observe/control pair, not the admin key, not
/// another service's keys — even from a database that holds every platform key.
///
/// This creates a real settings database the way a box gets one (`vike_secrets::create_store`'s
/// `--init` arm, then `vike_secrets::save_credentials_to_store_unjournalled`) whose `hub_key`
/// table holds every platform key, then drives the PRODUCTION read (`resolve_daemon_hub_key_store`, the
/// function the gatewayhub link and the feed ctors call), so a widened predicate turns this red.
#[test]
fn the_daemons_own_hub_key_store_read_holds_its_two_client_keys_and_no_person_key() {
    use vike_venues::credential_keys::PLATFORM_KEYS;

    let dir = tempfile::tempdir().expect("a scratch project directory");
    let settings = dir.path().join("settings");
    let settings_arg = settings.to_str().expect("a UTF-8 temp path");
    let values: Vec<(String, String)> = PLATFORM_KEYS
        .iter()
        .zip(["o", "c", "d", "e", "a", "g", "h", "t", "m", "s"])
        .map(|(name, fill)| ((*name).to_string(), fill.repeat(32)))
        .collect();
    vike_secrets::create_store(Some(settings_arg)).expect("an empty settings database");
    vike_secrets::save_credentials_to_store_unjournalled(
        &settings,
        vike_secrets::StoreTable::HubKey,
        &values,
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("a settings database holding every platform key");

    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(&settings)),
        vike_secrets::CredentialScope::Full,
    );
    let node_vars =
        resolve_daemon_hub_key_store(&store).expect("the store reads").secrets.into_map();
    assert_eq!(
        node_vars.get(PLATFORM_KEYS[7]).map(String::as_str),
        Some("t".repeat(32).as_str()),
        "the tradehub must hold the gatewayhub Trade key as a client"
    );
    assert_eq!(
        node_vars.get(PLATFORM_KEYS[9]).map(String::as_str),
        Some("s".repeat(32).as_str()),
        "the tradehub must hold the datahub strategy-lane key as a client"
    );
    for index in [0, 1, 2, 3, 4, 5, 6, 8] {
        assert!(
            !node_vars.contains_key(PLATFORM_KEYS[index]),
            "the tradehub daemon must read no person key and no other service's key, yet holds {}: {:?}",
            PLATFORM_KEYS[index],
            node_vars.keys().collect::<Vec<_>>()
        );
    }
}
