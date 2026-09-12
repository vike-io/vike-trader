//! The BIND POSTURE both localhost daemons share — is this listen address reachable off-box, and
//! may this process open it?
//!
//! ⚠ **It lives in this LIGHT crate, below both servers, because two of them now have to answer it
//! identically.** Ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`
//! split one served surface into two daemons — `vike-backend datahub` (the DATA plane, crate
//! `vike-datahub`, layer 65) and `vike-backend backtest --addr` (the COMPUTE plane, crate
//! `vike-backtest`, layer 50) — speaking one protocol over one handshake. Those two crates cannot
//! see each other and never may: the layer rule is down-only, so `vike-backtest` cannot name
//! `vike-datahub`. A copy of this policy in each would be exactly the shape the workspace's own rule
//! refuses (*when two sides must not disagree, the cure is a shared crate BELOW both*), and the
//! first divergence would be a daemon quietly binding `0.0.0.0` unauthenticated.
//!
//! It was `vike_datahub::server`'s until that split; the code is unchanged, only its home moved.
//! `vike_tradehub::server` keeps its own twin, deliberately — that node server cannot be key-less
//! (its `serve` takes `keys: NodeKeys` by value and `start_observe_server` opens no listener without
//! an observe key), so the [`ServerAuth`] half of this policy has no case to compute over there.
//!
//! Nothing here does I/O: the caller performs the `to_socket_addrs()` — the same resolution
//! `TcpListener::bind` performs — and hands the resolved addresses in, which is what makes the whole
//! policy a pure function a unit test can drive without a socket and without DNS.

use std::net::SocketAddr;

use crate::node_auth::NodeKeys;
/// How exposed a resolved bind target is — the input to the bin's non-loopback refusal. Mirrors
/// `vike_tradehub::server`'s `BindExposure` (same variants, same classification rule), so the two
/// backend daemons answer "is this address reachable off-box" identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindExposure {
    /// Every resolved address is a loopback address: reachable only from this host.
    Loopback,
    /// At least one resolved address is NOT loopback (a specific interface, or the wildcard
    /// binds). Carries the first such address, for the operator-facing message.
    Public(SocketAddr),
    /// The address resolved to nothing at all — `bind` would fail anyway; classified separately so
    /// a caller never has to read "not loopback" as "publicly exposed".
    Unresolvable,
}

/// Classify an already-RESOLVED bind target. Pure, so the policy is unit-testable without a socket
/// and without DNS; the caller does the `to_socket_addrs()` that produced `resolved`, which is the
/// same resolution `TcpListener::bind` performs.
///
/// A target is [`BindExposure::Loopback`] only when EVERY resolved address is loopback — a hostname
/// resolving to both a loopback and a LAN address is exposed on the LAN, so the strictest answer
/// is the true one. `0.0.0.0` and `::` are the wildcard binds, which `IpAddr::is_loopback` reports
/// false for, so they classify as [`BindExposure::Public`] — correctly: they listen on every
/// interface, which is the single most common way this surface gets accidentally exposed.
pub fn bind_exposure(resolved: &[SocketAddr]) -> BindExposure {
    match resolved.iter().find(|a| !a.ip().is_loopback()) {
        Some(a) => BindExposure::Public(*a),
        None if resolved.is_empty() => BindExposure::Unresolvable,
        None => BindExposure::Loopback,
    }
}

/// Whether the server about to open this listener CAN authenticate a connection — the third input
/// to [`bind_decision`], and what makes it a POSTURE decision rather than an address one.
///
/// It is a named enum rather than a second `bool` deliberately: [`bind_decision`]'s other policy
/// input is already a `bool` (`allow_public`), and two adjacent `bool` parameters TRANSPOSE
/// SILENTLY at a call site — the same hazard the bin's `resolve_store_root_from` comment describes
/// for two adjacent `Option<PathBuf>`s, and a transposition here would restore exactly the defect
/// this variant exists to remove. Named variants cannot be swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAuth {
    /// `vike_datahub::server`'s `serve_authed` (and its compute twin, `vike_backtest::compute_server`'s) is about to be handed `Some(keys)`: every connection must complete the
    /// handshake before any verb is answered, and each verb is then checked against
    /// [`crate::proto::required_scope`].
    Keyed,
    /// `vike_datahub::server`'s `serve_authed` (and its compute twin, `vike_backtest::compute_server`'s) is about to be handed `None`: no handshake is required, and every verb but
    /// one — the history reads, the `Run*` verbs that COMPILE CLIENT-SUPPLIED RHAI, and (in a
    /// `backfill-serve` build) the `Backfill` store WRITE — is served to whoever can reach the
    /// socket. The bind posture is then the entire barrier, which is why it may not be waived.
    ///
    /// ⚠ **This said "EVERY verb" until 2026-09-07, and the enumeration is why the repair is one
    /// clause rather than a rewrite: every item named above is still served on this arm.** What is
    /// not is [`crate::proto::Request::DeleteSeries`]: `crates/vike-datahub/src/server.rs`'s `delete_series_verb`
    /// answers `vike_datahub::server`'s `KEYLESS_DELETE_REFUSAL` here before the selector is validated, and
    /// `served_features` withholds the advertisement. `Backfill` KEEPING its place in that list
    /// beside it is the argument rather than an inconsistency — both are [`crate::node_auth::Scope::Control`] on a
    /// keyed server, but a backfill writes rows a re-fetch restores and a removal takes the only
    /// copy (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`).
    Keyless,
}

impl ServerAuth {
    /// Classify from the very value the caller is about to hand `vike_datahub::server`'s `serve_authed` (and its compute twin, `vike_backtest::compute_server`'s), so the bind guard
    /// and the server cannot disagree about whether this process authenticates.
    ///
    /// `Some(_)` is [`ServerAuth::Keyed`] even when only ONE scope has a key, because
    /// `run_handshake` refuses a scope this server holds no key for — `if !keys.has(scope)` —
    /// BEFORE the key is consulted at all. So a partially-keyed server still answers no verb to an
    /// unauthenticated peer, and `None` is the only shape that serves the reads, the compute and
    /// the backfill with no handshake at all.
    ///
    /// ⚠ It is NOT "the only shape that serves everything", which is what this sentence said until
    /// 2026-09-07. `None` reaches `handle_request`'s `keyed = false` call, whose
    /// `Request::DeleteSeries` arm returns `vike_datahub::server`'s `KEYLESS_DELETE_REFUSAL`, so key ABSENCE decides WHICH
    /// verbs exist as well as whether a peer must authenticate — which makes the key-less shape a
    /// strict SUBSET rather than the superset the old clause implied. ⚠ And it is the KEY-LESS shape
    /// that lost a verb, not every shape: a Control-authenticated peer on a keyed server reaches
    /// `handle_request(.., true)`, so `delete_series_verb`'s `if !keyed` never fires and every
    /// non-handshake verb is served to it. A first attempt at this correction wrote "no shape serves
    /// everything now", which drops the `with no handshake at all` the sentence above carries and is
    /// false of `Keyed`. The classification
    /// this function performs is untouched by that — it reads the same value for the same reason —
    /// but a caller reasoning from the old clause would conclude a key-less server is a superset,
    /// and it is not.
    ///
    /// ⚠ Cite `has`, NOT `NodeKeys::key_for`. The first draft of this comment said an absent scope
    /// key "verifies against an empty key and fails" — inherited from `key_for`'s own doc, and
    /// false: `node_auth`'s `verify` builds `HmacSha256::new_from_slice(key)`, which accepts a
    /// zero-length key, so the empty slice is an ordinary HMAC rather than a closed gate. The
    /// CONCLUSION here is unaffected; only its reason was wrong. `key_for`'s doc now carries the
    /// correction.
    pub fn of(keys: Option<&NodeKeys>) -> Self {
        match keys {
            Some(_) => ServerAuth::Keyed,
            None => ServerAuth::Keyless,
        }
    }
}

/// What the bin should DO about a bind target — the policy, kept here in the library rather than
/// inline in the binary so it is unit-testable and so a change to it shows up as a change to a
/// named function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDecision {
    /// Loopback (or unresolvable — `bind` will report that itself): bind, say nothing special.
    Proceed,
    /// Non-loopback, WITH the operator's explicit opt-in, on a server that CAN authenticate: bind,
    /// but announce what is now reachable. ⚠ Reachable only under [`ServerAuth::Keyed`] — the
    /// key-less half of this arm is [`BindDecision::RefuseUnauthenticated`] below.
    ProceedExposed(SocketAddr),
    /// Non-loopback with NO opt-in: do not bind. What "do not bind" means is the caller's — the
    /// tradehub daemon keeps trading headless, while this server's bin EXITS, because serving is
    /// its only job.
    Refuse(SocketAddr),
    /// Non-loopback WITH the opt-in, on a [`ServerAuth::Keyless`] server: do not bind either. The
    /// opt-in consents to being REACHABLE; it is not, and never was, consent to serve a store
    /// write and a Rhai compiler to anyone who can open a socket. A SEPARATE variant rather than a
    /// reason field on [`BindDecision::Refuse`] because the two refusals have different FIXES —
    /// one says "keep the address on loopback", this one says "configure the node keys" — and
    /// because adding it makes every existing `match` on this enum fail to compile until its
    /// author confronts the case.
    RefuseUnauthenticated(SocketAddr),
}

/// Decide whether to open the listener. `allow_public` is the operator's explicit opt-in — for
/// this server `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1`, environment-only, because this binary loads no
/// settings files at all (its tradehub twin pairs the variable with a `flags.toml` key). `auth` is
/// what the process is about to hand `vike_datahub::server`'s `serve_authed` (and its compute twin, `vike_backtest::compute_server`'s), classified by [`ServerAuth::of`].
///
/// ⚠ The refusal is the point, and it is a DEFAULT rather than a prohibition. On a KEY-LESS server
/// — still the default — this protocol authenticates nothing (see the module doc), so everything it
/// serves, including the store WRITE in a `backfill-serve` build, comes down to who can reach the
/// socket. A KEYED server does authenticate, and the guard STILL applies to it: the handshake is
/// plaintext and authenticates the CONNECTION, not each frame, so the tunnel is what supplies
/// confidentiality and integrity under either posture (0025's "honest limits of B"). The design
/// answer is a loopback listener plus an SSH tunnel; nothing ENFORCED it until this existed, and
/// the shipped unit cannot (its `EnvironmentFile=` beats its own bind default, so one `.env` line
/// used to expose the server with no unit edit and no warning). But a data server on a trusted LAN
/// or a VPN interface is a real deployment, and a guard with no escape hatch is one people route
/// around by other means, so the escape hatch is NAMED and separate: an address cannot be its own
/// consent, because typing the address is the mistake.
///
/// ⚠ **The escape hatch has a floor, and `auth` is what puts it there.** The opt-in used to be the
/// WHOLE consent: `VIKE_DATAHUB_ADDR=0.0.0.0` plus `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1` on a key-less
/// server bound and served, behind one `warn!` line — a publicly reachable history read, a Rhai
/// compiler and (in a `backfill-serve` build) a store write, gated by a log message. That is the
/// state `docs/decisions/0025-datahub-remote-posture.md` exists to prevent, in that record's own
/// words: "an opt-in warning in front of an unauthenticated write verb is the exact state this
/// record exists to prevent", and it names a non-loopback need as the TRIGGER for adopting the
/// keys rather than a reason to set the opt-in as a workaround. So the two knobs now compose:
/// `allow_public` consents to being REACHABLE, the node keys make what is reached
/// AUTHENTICATED, and a non-loopback bind needs both — [`BindDecision::RefuseUnauthenticated`].
/// LOOPBACK is untouched by `auth` in either direction: a key-less loopback datahub is the ordinary
/// developer configuration and stays byte-identical.
///
/// An UNRESOLVABLE target proceeds deliberately: `TcpListener::bind` is about to fail on it with a
/// better message than this function could invent, and reporting "not loopback" for an address
/// that is not anything would send an operator looking for an exposure that does not exist.
///
/// ⚠ This is where the datahub guard STOPS being signature-identical to `vike_tradehub::server`'s
/// `bind_decision`, and the divergence is the honest one: the tradehub node server cannot BE
/// key-less — its `serve` takes `keys: NodeKeys` by value and `start_observe_server` returns
/// `None` (no listener at all) when no observe key resolves — so over there the question `auth`
/// answers has no second case to compute. `bind_exposure`/[`BindExposure`] — the classification of
/// an ADDRESS, which is the part that must not drift — stays mirrored name-for-name.
pub fn bind_decision(
    resolved: &[SocketAddr],
    allow_public: bool,
    auth: ServerAuth,
) -> BindDecision {
    match bind_exposure(resolved) {
        BindExposure::Loopback | BindExposure::Unresolvable => BindDecision::Proceed,
        // No opt-in at all: the first thing to say is still "keep it on loopback", whether or not
        // keys happen to be configured — so this stays the pre-existing verdict, unchanged.
        BindExposure::Public(a) if !allow_public => BindDecision::Refuse(a),
        BindExposure::Public(a) => match auth {
            ServerAuth::Keyed => BindDecision::ProceedExposed(a),
            ServerAuth::Keyless => BindDecision::RefuseUnauthenticated(a),
        },
    }
}
/// The bind posture — the guard that keeps "reachability is the ONLY barrier" (module doc) true by
/// construction rather than by convention. The ADDRESS half mirrors `vike_tradehub::server`'s
/// `exposure_tests` name-for-name (minus its pre-auth frame test, which polices a handshake cap
/// this protocol does not have): the two backend daemons must answer "is this reachable off-box"
/// the same way. The AUTH half below has no tradehub twin and cannot have one — that node server
/// cannot be key-less (its `serve` takes `keys: NodeKeys` by value, and `start_observe_server`
/// opens no listener without an observe key), so the case these tests cover does not exist there.
#[cfg(test)]
mod exposure_tests {
    use super::*;
    use std::net::ToSocketAddrs;

    /// The data daemon's own default listen address, spelled LITERALLY here rather than imported.
    /// This module sits BELOW both daemons and owns neither default (`vike_config::DEFAULT_DATAHUB_ADDR`
    /// and `DEFAULT_BACKTEST_ADDR` are the authorities); what these tests actually need is *some*
    /// loopback `host:port`, and borrowing one daemon's constant to test a policy shared by two
    /// would read as if the policy were that daemon's.
    const LOOPBACK_DEFAULT: &str = "127.0.0.1:7878";

    fn resolve(addr: &str) -> Vec<SocketAddr> {
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default()
    }

    /// A `NodeKeys` with an observe key and no control key — the SHAPE that matters here (the
    /// guard asks only whether this process authenticates at all, never which scopes it serves),
    /// and deliberately the partially-configured one: a missing scope key is a closed gate, so it
    /// must still classify [`ServerAuth::Keyed`].
    fn some_keys() -> NodeKeys {
        NodeKeys::new(b"observe-key-bytes".to_vec(), Vec::new())
    }

    /// The DEFAULT is loopback and must stay so: on a key-less server — still the default — it is
    /// the entire barrier, not a convenience.
    #[test]
    fn the_default_address_is_loopback() {
        assert_eq!(bind_exposure(&resolve(LOOPBACK_DEFAULT)), BindExposure::Loopback);
    }

    /// ⚠ The wildcards are the whole point. `0.0.0.0` / `::` are what an operator types when they
    /// want "reachable from my laptop", and they are NOT loopback — `IpAddr::is_loopback` is false
    /// for both — so they must classify as exposed, or the guard would pass the single most common
    /// way this surface gets published to a network.
    #[test]
    fn the_wildcard_binds_are_public_not_loopback() {
        for addr in ["0.0.0.0:7878", "[::]:7878"] {
            assert!(
                matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_)),
                "{addr} must classify as exposed — it listens on EVERY interface"
            );
        }
    }

    #[test]
    fn loopback_spellings_are_all_loopback_and_a_routable_ip_is_not() {
        // 127.0.0.0/8 in full, both families, and the name.
        for addr in ["127.0.0.1:7878", "127.9.9.9:7878", "[::1]:7878", "localhost:7878"] {
            assert_eq!(bind_exposure(&resolve(addr)), BindExposure::Loopback, "{addr}");
        }
        for addr in ["<host>:7878", "<host>:7878", "[2001:db8::1]:7878"] {
            assert!(matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_)), "{addr}");
        }
    }

    /// ⚠ **THE GUARD ITSELF.** Without an opt-in a non-loopback bind is REFUSED, not warned about:
    /// a `warn!` in front of an unauthenticated read/compute/write surface is a line in a log file
    /// nobody reads until afterwards. With the opt-in AND node keys it proceeds, and says so.
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

    /// ⚠ **THE SECOND HALF OF THE GUARD, and the one this file shipped without.**
    /// `VIKE_DATAHUB_ADDR=0.0.0.0` + `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1` + no node keys used to be
    /// `ProceedExposed` behind a `warn!`: a publicly reachable server that answers history, COMPILES
    /// CLIENT-SUPPLIED RHAI (the `Run*` verbs) and — in a `backfill-serve` build — WRITES the store,
    /// gated by one log line. `docs/decisions/0025-datahub-remote-posture.md` names that verbatim as
    /// "the exact state this record exists to prevent". The opt-in consents to REACHABILITY; it is
    /// not consent to serve all of that unauthenticated, so the two knobs COMPOSE.
    #[test]
    fn a_public_bind_is_refused_on_a_keyless_server_even_with_the_opt_in() {
        let public = resolve("0.0.0.0:7878");
        let exposed = public[0];
        // Its OWN variant, not `ProceedExposed` (the old answer) and not the plain `Refuse`: the
        // two refusals have different FIXES — "keep it on loopback" vs "configure the node keys" —
        // and the bin can only name the right one if the verdict distinguishes them.
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

    /// `ServerAuth::of` reads the value the bin is about to hand `serve_authed`, and `Some(_)` is
    /// KEYED even when only one scope has a key — `run_handshake` refuses an unkeyed scope at
    /// `!keys.has(scope)`, before the key is consulted — so such a server still answers no verb to
    /// an unauthenticated peer. (NOT because an empty key fails verification; it does not.)
    #[test]
    fn server_auth_classifies_a_partially_keyed_server_as_keyed() {
        let observe_only = NodeKeys::new(b"observe".to_vec(), Vec::new());
        let control_only = NodeKeys::new(Vec::new(), b"control".to_vec());
        let both = NodeKeys::new(b"observe".to_vec(), b"control".to_vec());
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
    /// switch, so leaving it set on a host later moved back to loopback is inert.
    ///
    /// ⚠ Neither does `auth`, and that is load-bearing rather than incidental: a KEY-LESS LOOPBACK
    /// datahub is the ordinary developer configuration (`cargo run -p vike-datahub --features
    /// serve-datafusion` with an empty credential store), so all four combinations must `Proceed`
    /// or the guard would break every local flow it has no business touching.
    #[test]
    fn a_loopback_bind_proceeds_identically_with_or_without_the_opt_in() {
        let local = resolve(LOOPBACK_DEFAULT);
        let keys = some_keys();
        for auth in [ServerAuth::Keyless, ServerAuth::of(Some(&keys))] {
            assert_eq!(bind_decision(&local, false, auth), BindDecision::Proceed, "{auth:?}");
            assert_eq!(bind_decision(&local, true, auth), BindDecision::Proceed, "{auth:?}");
            // An address that resolves to nothing is left to `bind` to reject with its own message,
            // rather than being reported as an exposure it is not — and a key-less server does not
            // turn that into a refusal either: nothing is exposed by an address that is not one.
            assert_eq!(bind_decision(&[], false, auth), BindDecision::Proceed, "{auth:?}");
            assert_eq!(bind_decision(&[], true, auth), BindDecision::Proceed, "{auth:?}");
        }
    }

    /// Every loopback SPELLING keeps the key-less pass, not just the default literal: an operator
    /// who writes `localhost:7878` or `[::1]:7878` into `VIKE_DATAHUB_ADDR` has not exposed
    /// anything, so the new refusal must not fire on any of them.
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
    /// server must be refused on it too — the mixed-resolution case reaching the new arm, which the
    /// wildcard tests above cannot show.
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

    /// A name resolving to BOTH loopback and a routable address is exposed on that address, so the
    /// strictest reading is the true one. (Constructed directly: no DNS in a unit test.)
    #[test]
    fn one_routable_address_among_loopbacks_is_still_public() {
        let mixed = vec![
            "127.0.0.1:7878".parse::<SocketAddr>().unwrap(),
            "<host>:7878".parse::<SocketAddr>().unwrap(),
        ];
        assert!(matches!(bind_exposure(&mixed), BindExposure::Public(_)));
        // …and "resolved to nothing" is its own answer, never silently read as exposed.
        assert_eq!(bind_exposure(&[]), BindExposure::Unresolvable);
    }
}
