use super::*;
use std::net::ToSocketAddrs;

/// The data daemon's default listen address, spelled LITERALLY: this module owns neither daemon's
/// default (`vike_config::DEFAULT_DATAHUB_ADDR` and `DEFAULT_BACKTEST_ADDR` are the authorities),
/// and the tests need only *some* loopback `host:port`.
const LOOPBACK_DEFAULT: &str = "127.0.0.1:7878";

fn resolve(addr: &str) -> Vec<SocketAddr> {
    addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default()
}

/// A `HubKeys` with an observe key and no control key — deliberately the partially-configured
/// shape, which must still classify [`ServerAuth::Keyed`] (the guard asks only whether this
/// process authenticates at all).
fn some_keys() -> HubKeys {
    HubKeys::new(b"observe-key-bytes".to_vec(), Vec::new())
}

/// The DEFAULT is loopback and must stay so: on a key-less server — still the default — it is
/// the entire barrier, not a convenience.
#[test]
fn the_default_address_is_loopback() {
    assert_eq!(bind_exposure(&resolve(LOOPBACK_DEFAULT)), BindExposure::Loopback);
}

/// ⚠ **THE GUARD ITSELF.** Without an opt-in a non-loopback bind is REFUSED, not warned about.
/// With the opt-in AND hub keys it proceeds, and says so.
#[test]
fn a_public_bind_is_refused_without_the_opt_in_and_announced_with_it() {
    let public = resolve("0.0.0.0:7878");
    let exposed = public[0];
    let keys = some_keys();
    let keyed = ServerAuth::of(Some(&keys));
    assert_eq!(
        bind_decision(&public, false, keyed),
        BindDecision::Refuse(exposed),
        "default (no VIKE_DATAHUB_ALLOW_PUBLIC_BIND) must REFUSE a wildcard bind"
    );
    assert_eq!(
        bind_decision(&public, true, keyed),
        BindDecision::ProceedExposed(exposed),
        "the named opt-in permits it ON A KEYED SERVER — and the decision still carries what is \
             exposed, so the bin can name it in the warning it logs"
    );
}

/// ⚠ **THE SECOND HALF OF THE GUARD.** `VIKE_DATAHUB_ADDR=0.0.0.0` +
/// `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1` + no hub keys would publish history reads, CLIENT-SUPPLIED
/// RHAI (the `Run*` verbs) and — in a `backfill-serve` build — a store WRITE: "the exact state this
/// record exists to prevent" (`docs/decisions/0025-datahub-remote-posture.md`). The opt-in
/// consents to REACHABILITY only, so the two knobs COMPOSE.
#[test]
fn a_public_bind_is_refused_on_a_keyless_server_even_with_the_opt_in() {
    let public = resolve("0.0.0.0:7878");
    let exposed = public[0];
    // Its OWN variant, not the plain `Refuse`: the two refusals have different FIXES, and the bin
    // can only name the right one if the verdict distinguishes them.
    assert_eq!(
        bind_decision(&public, true, ServerAuth::Keyless),
        BindDecision::RefuseUnauthenticated(exposed),
        "the opt-in must NOT be enough on a server that authenticates nothing"
    );
    // …and with no opt-in either, the FIRST thing to say is still "keep it on loopback", so
    // that arm is unchanged by key presence in either direction.
    let keys = some_keys();
    for auth in [ServerAuth::Keyless, ServerAuth::of(Some(&keys))] {
        assert_eq!(
            bind_decision(&public, false, auth),
            BindDecision::Refuse(exposed),
            "{auth:?}: no opt-in is the pre-existing refusal, whatever the keys say"
        );
    }
}

/// `Some(_)` is KEYED even when only one scope has a key — `run_handshake` refuses an unkeyed
/// scope at `!keys.has(scope)` (NOT because an empty key fails verification; it does not).
#[test]
fn server_auth_classifies_a_partially_keyed_server_as_keyed() {
    let observe_only = HubKeys::new(b"observe".to_vec(), Vec::new());
    let control_only = HubKeys::new(Vec::new(), b"control".to_vec());
    let both = HubKeys::new(b"observe".to_vec(), b"control".to_vec());
    for keys in [&observe_only, &control_only, &both] {
        assert_eq!(ServerAuth::of(Some(keys)), ServerAuth::Keyed, "{keys:?}");
    }
    assert_eq!(
        ServerAuth::of(None),
        ServerAuth::Keyless,
        "`None` — the value that makes `serve_authed` require no handshake, and the one value \
             under which the delete verb is refused outright — is the ONLY key-less shape"
    );
}

/// …and the opt-in changes NOTHING for a loopback bind: it is not a general "skip the checks"
/// switch. ⚠ Neither does `auth`: a KEY-LESS LOOPBACK datahub is the ordinary developer
/// configuration, so all four combinations must `Proceed`.
#[test]
fn a_loopback_bind_proceeds_identically_with_or_without_the_opt_in() {
    let local = resolve(LOOPBACK_DEFAULT);
    let keys = some_keys();
    for auth in [ServerAuth::Keyless, ServerAuth::of(Some(&keys))] {
        assert_eq!(bind_decision(&local, false, auth), BindDecision::Proceed, "{auth:?}");
        assert_eq!(bind_decision(&local, true, auth), BindDecision::Proceed, "{auth:?}");
        // An address that resolves to nothing is left to `bind` to reject, on either posture:
        // nothing is exposed by an address that is not one.
        assert_eq!(bind_decision(&[], false, auth), BindDecision::Proceed, "{auth:?}");
        assert_eq!(bind_decision(&[], true, auth), BindDecision::Proceed, "{auth:?}");
    }
}

/// Every loopback SPELLING keeps the key-less pass, not just the default literal.
#[test]
fn a_keyless_server_still_binds_every_loopback_spelling() {
    for addr in ["127.0.0.1:7878", "127.9.9.9:7878", "[::1]:7878", "localhost:7878"] {
        assert_eq!(
            bind_decision(&resolve(addr), true, ServerAuth::Keyless),
            BindDecision::Proceed,
            "{addr} is loopback — a key-less server must still bind it"
        );
    }
}

/// A hostname resolving to both loopback and a routable address is EXPOSED, so a key-less
/// server is refused on it too — the mixed-resolution case the wildcard tests cannot show.
#[test]
fn a_mixed_resolution_is_refused_on_a_keyless_server_with_the_opt_in() {
    let mixed = vec![
        "127.0.0.1:7878".parse::<SocketAddr>().unwrap(),
        "<host>:7878".parse::<SocketAddr>().unwrap(),
    ];
    assert_eq!(
        bind_decision(&mixed, true, ServerAuth::Keyless),
        BindDecision::RefuseUnauthenticated(mixed[1]),
        "the refusal names the ROUTABLE address, not the loopback one it was mixed with"
    );
}
