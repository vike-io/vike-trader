//! The account-admin barrier (`account_admin_source`, decision 0065 §3c).

use super::*;
use crate::node::resolve_daemon_node_store;

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

/// A node-key store holding all three keys, the shape `from_vars_with_admin` reads.
fn admin_keys() -> HashMap<String, String> {
    HashMap::from([
        (vike_tradehub_client::auth::OBSERVE_KEY_ENV.to_string(), "o".repeat(32)),
        (vike_tradehub_client::auth::CONTROL_KEY_ENV.to_string(), "c".repeat(32)),
        (vike_tradehub_client::auth::ADMIN_KEY_ENV.to_string(), "a".repeat(32)),
    ])
}

/// …and the same store WITHOUT the admin key — a box that has the ordinary node pair and has
/// never minted the third.
fn pair_only() -> HashMap<String, String> {
    HashMap::from([
        (vike_tradehub_client::auth::OBSERVE_KEY_ENV.to_string(), "o".repeat(32)),
        (vike_tradehub_client::auth::CONTROL_KEY_ENV.to_string(), "c".repeat(32)),
    ])
}

fn loopback_bind() -> Vec<std::net::SocketAddr> {
    vec!["127.0.0.1:7879".parse().expect("a literal loopback address parses")]
}

/// A bind the process CAN see is wide — the `0.0.0.0` wildcard, which `bind_exposure`
/// classifies as `Public` deliberately because it is the commonest way this surface gets
/// accidentally exposed.
fn wide_bind() -> Vec<std::net::SocketAddr> {
    vec!["0.0.0.0:7879".parse().expect("a literal wildcard address parses")]
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
        account_admin_source(Some("loopback"), &wide_bind(), &admin_keys(), Some(any_dir()))
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
/// ORDER surface could become consent to publish a key-material one. Setting it in the one map
/// this function does read changes nothing, because nothing here looks for it. An operator who
/// genuinely wants the verbs on a wide bind says `contained`, which asserts a barrier OUTSIDE
/// the process rather than permission for there to be none.
#[test]
fn the_public_bind_opt_in_cannot_waive_the_loopback_refusal() {
    let mut keys = admin_keys();
    keys.insert("VIKE_TRADEHUB_ALLOW_PUBLIC_BIND".to_string(), "1".to_string());
    assert!(
        account_admin_source(Some("loopback"), &wide_bind(), &keys, Some(any_dir())).is_none(),
        "no value reachable from this function's inputs may waive the loopback check"
    );
}

/// The declaration the process CANNOT check arms regardless of the bind — and that is the
/// point of the third value rather than a hole in the second. `docs/decisions/0026` forbids
/// INFERRING a container's containment; it does not forbid an operator DECLARING it, and a
/// declared barrier is auditable in a way an inferred one is not.
#[test]
fn contained_arms_regardless_of_the_bind() {
    let armed =
        account_admin_source(Some("contained"), &wide_bind(), &admin_keys(), Some(any_dir()))
            .expect("`contained` is an assertion about a barrier outside this process");
    assert_eq!(armed.barrier, server::accounts::AccountBarrier::Contained);
    assert_eq!(armed.settings_dir, any_dir());
}

/// The ordinary armed case: declared `loopback`, bound on loopback, admin key present.
#[test]
fn a_loopback_declaration_over_a_loopback_bind_arms_the_capability() {
    let armed =
        account_admin_source(Some("loopback"), &loopback_bind(), &admin_keys(), Some(any_dir()))
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
        let armed =
            account_admin_source(Some(typo), &loopback_bind(), &admin_keys(), Some(any_dir()));
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
/// without the verb: no capability, no admin key read, no feature advertised.
#[test]
fn unset_blank_and_off_are_all_the_default() {
    for decl in [None, Some(""), Some("   "), Some("off")] {
        assert!(
            account_admin_source(decl, &loopback_bind(), &admin_keys(), Some(any_dir())).is_none(),
            "{decl:?} must leave this daemon byte-identical to one without the verb"
        );
    }
}

/// ⚠ **THE KEY IS THE SECOND GATE, and it is the one a settings write cannot forge.** A box
/// that declared the barrier and never minted an admin key arms nothing — which is what keeps
/// `docs/decisions/0065`'s named self-escalation path (a Control peer writing the declaration
/// into a settings file it can reach) from arming the capability.
#[test]
fn a_declaration_with_no_admin_key_arms_nothing() {
    for decl in ["loopback", "contained"] {
        assert!(
            account_admin_source(Some(decl), &loopback_bind(), &pair_only(), Some(any_dir()))
                .is_none(),
            "`{decl}` with no admin key in the node store must arm nothing"
        );
    }
    // …and a BLANK admin key is an absent one, exactly as a blank observe or control key is.
    let mut blank = pair_only();
    blank.insert(vike_tradehub_client::auth::ADMIN_KEY_ENV.to_string(), "   ".to_string());
    assert!(
        account_admin_source(Some("loopback"), &loopback_bind(), &blank, Some(any_dir())).is_none(),
        "a blank admin key is an absent one"
    );
}

/// A daemon whose BOOT resolved no project has no store to administer, and resolving one here
/// would be the `$VIKE_SETTINGS_DIR`-blind resolver's failure wearing a new surface.
#[test]
fn a_declaration_with_no_settings_directory_arms_nothing() {
    assert!(
        account_admin_source(Some("contained"), &loopback_bind(), &admin_keys(), None).is_none(),
        "no settings directory means no store to administer"
    );
}

/// ⚠ **THE ADMIN KEY IS NEVER LOADED WHEN THE CAPABILITY IS NOT ARMED**, which is the
/// key-ZEROING gate the control key already has, wearing the other polarity and one rung
/// stronger: the control gate zeroes a key it loaded, while this one never loads one at all.
/// So an `Admin` handshake against an undeclared box cannot verify REGARDLESS of what the
/// node-key store holds — `from_vars` reads two names and leaves the third empty.
#[test]
fn the_ordinary_node_key_read_leaves_the_admin_scope_absent() {
    let keys = vike_tradehub_client::auth::from_vars(&admin_keys())
        .expect("the observe/control pair is present");
    assert!(
        !keys.has(Scope::Account),
        "the two-name read must leave Scope::Account ABSENT even when the store holds an admin \
             key — a box that merely has one must not thereby arm the account surface"
    );
    let widened = vike_tradehub_client::auth::from_vars_with_admin(&admin_keys())
        .expect("the same pair, through the door that also reads the third name");
    assert!(widened.has(Scope::Account), "…and the explicit door is what grants it");
}

/// **Arming the account capability may not restore a control key the control gate ZEROED.**
///
/// The test above asserts only about `Scope::Account`, so it stayed green while the two steps
/// `start_observe_server` performs in sequence disagreed: step 2 zeroes the control key when
/// `flags.tradehub_control` is off, and step 3 used to call
/// `from_vars_with_admin(&node_vars)` — which is `from_vars` + `with_admin`, and `from_vars`
/// re-reads BOTH names out of the same map. The zeroed value was discarded rather than extended,
/// so a control-DISABLED box that armed accounts handed a Control peer back the key
/// `run_handshake`'s `!keys.has(Scope::Write)` gate had just refused it with — and
/// `Request::Preview`'s arm has no sink gate behind that check.
///
/// ⚠ It drives the PRODUCTION function [`keys_for_account_capability`], not a replay of it.
/// An earlier draft of this test re-spelled both steps inline and would have passed against the
/// defective daemon — the seam had to be extracted before the assertion could mean anything.
#[test]
fn arming_accounts_does_not_restore_a_control_key_the_control_gate_zeroed() {
    let vars = admin_keys();
    let loaded = vike_tradehub_client::auth::from_vars(&vars).expect("the pair is present");
    assert!(loaded.has(Scope::Write), "the store really does hold a control key");

    // Step 2, verbatim: control is OFF, so the control key is emptied.
    let zeroed =
        vike_tradehub_client::NodeKeys::new(loaded.key_for(Scope::Read).to_vec(), Vec::new());
    assert!(!zeroed.has(Scope::Write), "the control gate emptied it");

    // Step 3, through the daemon's own function.
    let armed = keys_for_account_capability(zeroed, true, &vars);

    assert!(
        !armed.has(Scope::Write),
        "arming the account capability RESTORED the control key the control gate had zeroed — \
             step 3 must EXTEND step 2's value, never re-read the node-key map"
    );
    assert!(armed.has(Scope::Account), "…while still granting the admin scope it was armed for");
}

/// **THE DAEMON'S OWN NODE-STORE READ KEEPS THE ADMIN KEY, so a declared barrier can arm.**
///
/// Every test above hands `account_admin_source` a hand-built map, which cannot see the question
/// "does the map the DAEMON builds contain the admin key". `vike_secrets::resolve_node_keys` returns
/// only the names its predicate admits, and the daemon once passed the pair-only
/// `is_tradehub_node_key` (the CLI's and the desktop's scope, which must not carry the key that
/// writes key material): `VIKE_TRADEHUB_ADMIN_KEY` was in the database and never in `node_vars`, so
/// `config.tradehub_account_admin = loopback` could not arm and the daemon logged "no admin key"
/// on a box that held one.
///
/// This creates a real settings database the way a box gets one (`vike_secrets::create_store`'s
/// `--init` arm, then `vike_secrets::save_credentials_to_store`) whose `node_key` table holds the
/// tradehub pair, the admin key and the datahub pair, then drives the PRODUCTION read (`resolve_daemon_node_store`, the
/// function `start_observe_server` calls) into the PRODUCTION decision (`account_admin_source`):
///
/// * (a) the admin key is in the map, with the pair;
/// * (b) the datahub pair is not (the daemon never holds another service's keys);
/// * (c) the capability arms under `loopback` over a loopback bind;
/// * (d) the pair-only predicate over the SAME database does not arm — which is the pre-fix
///   behaviour, so (a) and (c) are what turn red if the daemon's call regresses.
#[test]
fn the_daemons_own_node_store_read_keeps_the_admin_key_and_arms_the_capability() {
    use vike_model::credential_keys::{PLATFORM_KEYS, is_tradehub_node_key};

    let dir = tempfile::tempdir().expect("a scratch project directory");
    let settings = dir.path().join("settings");
    let settings_arg = settings.to_str().expect("a UTF-8 temp path");
    let values: Vec<(String, String)> = PLATFORM_KEYS
        .iter()
        .zip(["o", "c", "d", "e", "a"])
        .map(|(name, fill)| ((*name).to_string(), fill.repeat(32)))
        .collect();
    vike_secrets::create_store(Some(settings_arg)).expect("an empty settings database");
    vike_secrets::save_credentials_to_store(&settings, vike_secrets::Table::NodeKey, &values, None)
        .expect("a settings database holding all five platform keys");

    // (a) + (b): the daemon's production read.
    let resolved = resolve_daemon_node_store(Some(settings_arg)).expect("the store reads");
    let node_vars = resolved.secrets.into_map();
    assert_eq!(
        node_vars.get(PLATFORM_KEYS[4]).map(String::as_str),
        Some("a".repeat(32).as_str()),
        "the daemon's node-key map must carry VIKE_TRADEHUB_ADMIN_KEY"
    );
    assert!(node_vars.contains_key(PLATFORM_KEYS[0]) && node_vars.contains_key(PLATFORM_KEYS[1]));
    assert!(
        !node_vars.contains_key(PLATFORM_KEYS[2]) && !node_vars.contains_key(PLATFORM_KEYS[3]),
        "the tradehub daemon must never hold the datahub's pair: {:?}",
        node_vars.keys().collect::<Vec<_>>()
    );

    // (c): that map arms the capability, and the admin scope reaches the server's keys.
    let armed =
        account_admin_source(Some("loopback"), &loopback_bind(), &node_vars, Some(any_dir()))
            .expect("a declared loopback barrier + a stored admin key must arm");
    assert_eq!(armed.barrier, server::accounts::AccountBarrier::Loopback);
    let keys = vike_tradehub_client::auth::from_vars(&node_vars).expect("the pair is present");
    assert!(keys_for_account_capability(keys, true, &node_vars).has(Scope::Account));

    // (d): the pair-only predicate over the same database loses the admin key — the pre-fix shape.
    let pair_only_map = vike_secrets::resolve_node_keys(Some(settings_arg), is_tradehub_node_key)
        .expect("the store reads")
        .secrets
        .into_map();
    assert!(!pair_only_map.contains_key(PLATFORM_KEYS[4]));
    assert!(
        account_admin_source(Some("loopback"), &loopback_bind(), &pair_only_map, Some(any_dir()))
            .is_none(),
        "a pair-only resolution must not arm: that is the regression this test exists for"
    );
}
