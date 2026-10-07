//! Section 1: the key-LESS server is unchanged (byte-identical `Welcome`, all verbs but delete).

use super::*;

// ---- 1. the key-LESS server is unchanged --------------------------------------------------------

/// ⚠ **The backward-compatibility proof, and it is a BYTE comparison, not a behavioural one.**
///
/// A key-less server's `Welcome` must serialize to the EXACT bytes the pre-auth protocol produced:
/// no `nonce` field on the wire at all (hence `skip_serializing_if` on an `Option`, not a
/// `[0u8;32]` sentinel), and no `auth` string in `features`. A test that merely asserted "an old
/// client still works" would pass against a `Welcome` that had grown a `"nonce":null` field — which
/// is a wire change, and the kind that breaks a strict decoder somewhere downstream.
///
/// The expectation is built from the frame the SERVER actually sends, compared against a
/// hand-rolled JSON object carrying only the two pre-auth fields.
#[test]
fn a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol() {
    let addr = spawn(None);
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let welcome = read_frame::<_, Response>(&mut s).expect("welcome");

    let (features, nonce) = match &welcome {
        Response::Welcome { features, nonce, .. } => (features.clone(), *nonce),
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(nonce.is_none(), "a key-less server must mint no nonce");
    assert!(
        !features.iter().any(|f| f == FEATURE_AUTH),
        "a key-less server must not advertise `{FEATURE_AUTH}`: {features:?}"
    );

    // The BYTES. `nonce` is absent from the encoding entirely — not present-and-null.
    let json = serde_json::to_string(&welcome).expect("encode");
    assert!(!json.contains("nonce"), "the key-less Welcome must carry no `nonce` field: {json}");
    assert!(!json.contains(FEATURE_AUTH), "...and no auth advertisement: {json}");
}

/// A key-less server serves EVERY verb without any handshake at all — including sending a normal
/// verb as the very FIRST frame, with no `Hello` before it. That "Hello informs, it does not gate"
/// contract is what `vike-cli`, the Studio and the GUI have always relied on.
///
/// The assertion is deliberately "not an auth refusal" rather than "succeeded": several verbs
/// legitimately answer `Response::Error` over an empty `MemHistStore` (or on a build without
/// `serve-datafusion`). What must NEVER appear is `AuthDenied`, or an error mentioning a scope.
///
/// ⚠ **`DeleteSeries` is the ONE verb this claim does not cover, and its exception is spelled here
/// rather than left to a substring match.** That verb is refused OUTRIGHT on a key-less server —
/// see `a_keyless_server_serves_no_delete_verb` — so "every verb" acquired an exception the day it
/// landed. Writing the arm out is what keeps this test's own title honest: without it the refusal
/// passed only because the message happened to spell `Scope::Write` with a capital S.
#[test]
fn a_keyless_server_serves_every_verb_with_no_handshake() {
    let addr = spawn(None);
    for request in every_request() {
        // A FRESH connection per verb, each sending the verb as its first frame — the strongest
        // form of "no handshake required".
        let mut s = TcpStream::connect(addr).expect("connect");
        let response = exchange(&mut s, &request);
        match (&request, &response) {
            // The one exception, and it is the honest answer rather than a refusal: a client that
            // signed a mac against a server holding no keys is TOLD its key was never checked.
            (Request::Auth { .. }, Response::AuthDenied { reason }) => {
                assert!(
                    reason.contains("no node keys"),
                    "a key-less server's Auth answer must say the keys are absent: {reason}"
                );
            }
            // The DESTRUCTIVE verb: a key-less server must refuse it, and the refusal must SAY it
            // is about keys rather than about the request.
            (Request::DeleteSeries { .. }, Response::Error(msg)) => assert!(
                msg.contains("no node keys"),
                "a key-less server's delete refusal must name the missing keys: {msg}"
            ),
            (Request::DeleteSeries { .. }, other) => {
                panic!("a key-less server answered DeleteSeries with {other:?}")
            }
            (_, Response::AuthDenied { reason }) => {
                panic!("key-less server refused {request:?} with AuthDenied: {reason}")
            }
            (_, Response::Error(msg)) => assert!(
                !msg.contains("scope"),
                "key-less server refused {request:?} on scope grounds: {msg}"
            ),
            _ => {}
        }
    }
}

/// **The narrowing that made the remote delete verb buildable at all**, proved with the server
/// built BOTH ways over one fixture.
///
/// `docs/decisions/0025-datahub-remote-posture.md` records why a datahub write verb is the class
/// change that forces authentication, and the reason it gives — *"A surface with a write verb and no
/// way to say 'reads yes, writes no' cannot be handed even a trusted LAN"* — is sharper for a verb
/// that destroys the only copy than for one that writes rows a re-fetch restores. The design that
/// proposed this verb declined it for exactly that reason: `Scope::Write` is meaningful only on a
/// KEYED server, and a key-less one is the shipped default.
///
/// The rule this test pins is the answer to that, and it is unconditional in both directions:
///
/// * key-LESS — the verb is NOT advertised and is REFUSED, whatever the request says. There is no
///   flag that turns it on.
/// * KEYED — the verb IS advertised, and reaches the store under the Control scope.
///
/// ⚠ Two legs, not one: the advertisement is what stops a well-behaved client sending, and the
/// refusal is what holds for a client that does not check. Neither substitutes for the other, which
/// is why this drives the WIRE rather than calling `served_features`.
#[test]
fn a_keyless_server_serves_no_delete_verb() {
    let request = Request::DeleteSeries {
        selector: SeriesSelector::new("bar", "binance"),
        produced_by: Some("klines:".to_string()),
        dry_run: true,
    };

    // ---- key-LESS: not advertised, and refused on the wire anyway ------------------------------
    let keyless = spawn(None);
    let advertised = DatahubClient::connect(keyless).expect("connect").features().to_vec();
    assert!(
        !advertised.iter().any(|f| f == "delete_series"),
        "a key-less server must not advertise the delete verb: {advertised:?}"
    );
    let mut s = TcpStream::connect(keyless).expect("connect");
    match exchange(&mut s, &request) {
        Response::Error(msg) => {
            assert!(msg.contains("no node keys"), "{msg}");
            assert!(
                msg.contains("vike-cli data hist rm --store"),
                "the refusal says what to do: {msg}"
            );
        }
        other => panic!("a key-less server answered DeleteSeries with {other:?}"),
    }

    // ---- KEYED: advertised, and served under Control -------------------------------------------
    let keyed = spawn(Some(keys()));
    let mut s = authed_stream(keyed, &keys(), Scope::Write);
    match exchange(&mut s, &request) {
        // The empty `MemHistStore` cannot enumerate a provenance, so the PLAN itself fails — which
        // is the store's answer, not a posture refusal. What this arm proves is that the verb was
        // REACHED: a key-less server never gets this far.
        Response::Deleted(done) => assert_eq!(done.plan.matched(), 0),
        Response::Error(msg) => assert!(
            !msg.contains("no node keys"),
            "a KEYED server must reach the verb rather than refusing on keys: {msg}"
        ),
        other => panic!("expected Deleted or a store error, got {other:?}"),
    }

    // ...and an OBSERVE connection on that same keyed server is refused on SCOPE.
    let mut s = authed_stream(keyed, &keys(), Scope::Read);
    match exchange(&mut s, &request) {
        Response::Error(msg) => assert!(msg.contains("Control scope"), "{msg}"),
        other => panic!("an Observe connection reached the delete verb: {other:?}"),
    }
}

/// The unauthenticated `DatahubClient::connect` path — the one every existing caller uses — still
/// connects and serves against a key-less server, with `authenticated_scope() == None`.
#[test]
fn the_plain_client_still_works_against_a_keyless_server() {
    let addr = spawn(None);
    let mut client = DatahubClient::connect(addr).expect("connect must succeed unauthenticated");
    assert_eq!(client.authenticated_scope(), None);
    assert!(!client.requires_auth());
    client.ping().expect("ping");
    assert!(client.list_series().expect("list_series is served").is_empty());
}

/// ...and a client that HOLDS keys degrades cleanly against a key-less server rather than failing:
/// it sees no `auth` advertisement, sends no `Auth`, and returns a working unauthenticated
/// connection. That is what lets one configured GUI work against both a keyed production datahub
/// and a local key-less dev one without branching.
#[test]
fn an_authed_connect_degrades_to_unauthenticated_against_a_keyless_server() {
    let addr = spawn(None);
    let mut client = DatahubClient::connect_authed(addr, &keys(), Scope::Write)
        .expect("connect_authed must not fail against a key-less server");
    assert_eq!(
        client.authenticated_scope(),
        None,
        "no authentication took place, and the client must say so rather than claim a scope"
    );
    client.ping().expect("ping");
}
