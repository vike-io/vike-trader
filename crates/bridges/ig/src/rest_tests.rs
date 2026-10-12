use super::*;

/// A REAL `GET /session/encryptionKey` body, captured from `demo-api.ig.com` on 2026-08-09.
/// The `encryptionKey` value — the RSA public key IG returns for password encryption, which
/// this parser never reads — is replaced by a placeholder, the same redact-the-leaf-keep-the-
/// shape idiom as `vike_bridge_core::capture`'s `capture_frame`.
const CAPTURED: &str = include_str!("../tests/fixtures/session_encryption_key.json");

#[test]
fn the_captured_encryption_key_body_yields_igs_clock() {
    let body: serde_json::Value =
        serde_json::from_str(CAPTURED).expect("the capture is valid JSON");
    assert_eq!(parse_server_time_ms(&body, 200), Ok(1_786_242_762_192));
}

/// The UNIT trap, at the one venue whose stamp is a bare integer with no sibling to compare
/// against: a seconds-valued reading is off by a factor of a thousand and still looks like a
/// plausible epoch. The captured stamp is asserted to be MILLISECONDS by its magnitude.
#[test]
fn the_captured_stamp_is_milliseconds_not_seconds() {
    let body: serde_json::Value = serde_json::from_str(CAPTURED).expect("valid JSON");
    let ms = parse_server_time_ms(&body, 200).expect("the capture carries timeStamp");
    // 2001-09-09T01:46:40Z in ms; any seconds-valued stamp for this century is far below it.
    assert!(ms > 1_000_000_000_000, "a seconds-valued stamp would be a thousandfold out: {ms}");
    assert!(ms < 10_000_000_000_000, "a µs/ns-valued stamp would be far above this: {ms}");
}

// ── The 401 ladder (offline: no agent, no network) ────────────────────────────────────────
//
// These drive [`reauth_ladder`] directly, which is why it is a free function over the token
// cell rather than a method: the properties that matter here are COUNTS (how many HTTP
// attempts, how many logins), and a count is only assertable when the two closures are the
// test's own.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

fn cell(generation: u64) -> Mutex<Tokens> {
    Mutex::new(Tokens {
        cst: format!("cst-{generation}"),
        security_token: format!("xst-{generation}"),
        generation,
    })
}

/// A call that succeeds first time never touches the login endpoint — the ordinary path must
/// cost exactly one round trip.
#[test]
fn a_non_401_answer_never_re_logs_in() {
    let tokens = cell(1);
    let (runs, logins) = (AtomicU32::new(0), AtomicU32::new(0));
    let out = reauth_ladder(
        &tokens,
        "ACC",
        |_, _| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok((200, "{}".to_string()))
        },
        |_| {
            logins.fetch_add(1, Ordering::Relaxed);
            Ok(2)
        },
    )
    .expect("no transport error");
    assert_eq!(out.0, 200);
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert_eq!(logins.load(Ordering::Relaxed), 0, "a 200 is not a re-login trigger");
}

/// The recovery itself: a 401 re-logs-in ONCE and retries with the NEW pair — asserted by the
/// tokens the second attempt was handed, not merely by the attempt count.
#[test]
fn a_401_re_logs_in_once_and_retries_with_the_fresh_pair() {
    let tokens = cell(1);
    let seen: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let logins = AtomicU32::new(0);
    let out = reauth_ladder(
        &tokens,
        "ACC",
        |cst, _| {
            seen.lock().unwrap().push(cst.to_string());
            if cst == "cst-1" {
                Ok((401, r#"{"errorCode":"error.security.client-token-invalid"}"#.to_string()))
            } else {
                Ok((200, r#"{"ok":true}"#.to_string()))
            }
        },
        |gen_seen| {
            logins.fetch_add(1, Ordering::Relaxed);
            assert_eq!(gen_seen, 1, "the ladder reports the generation it actually used");
            let mut t = tokens.lock().unwrap();
            t.cst = "cst-2".into();
            t.security_token = "xst-2".into();
            t.generation = 2;
            Ok(2)
        },
    )
    .expect("no transport error");
    assert_eq!(out.0, 200);
    assert_eq!(logins.load(Ordering::Relaxed), 1);
    assert_eq!(*seen.lock().unwrap(), vec!["cst-1".to_string(), "cst-2".to_string()]);
}

/// ⚠ The anti-spin property, and the reason the ladder is a ladder rather than a loop: a
/// second 401 on a FRESH session is a REFUSAL (revoked key, changed password), not an expiry.
/// It stops after exactly two attempts and one login, and hands the caller IG's own body.
#[test]
fn a_second_401_after_a_fresh_login_stops_instead_of_spinning() {
    let tokens = cell(1);
    let (runs, logins) = (AtomicU32::new(0), AtomicU32::new(0));
    let out = reauth_ladder(
        &tokens,
        "ACC",
        |_, _| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok((401, r#"{"errorCode":"error.security.client-token-invalid"}"#.to_string()))
        },
        |_| {
            logins.fetch_add(1, Ordering::Relaxed);
            tokens.lock().unwrap().generation = 2;
            Ok(2)
        },
    )
    .expect("no transport error");
    assert_eq!(out.0, 401);
    assert!(out.1.contains("client-token-invalid"), "IG's own errorCode survives: {}", out.1);
    assert_eq!(runs.load(Ordering::Relaxed), 2, "exactly one retry, never a loop");
    assert_eq!(logins.load(Ordering::Relaxed), 1, "exactly one re-login attempt");
}

/// A re-login that itself fails must not be retried either, and must surface IG's ORIGINAL 401
/// body — the caller's error should name what the venue said, not what our recovery said.
#[test]
fn a_failed_re_login_surfaces_the_original_401_and_does_not_retry() {
    let tokens = cell(1);
    let runs = AtomicU32::new(0);
    let out = reauth_ladder(
        &tokens,
        "ACC",
        |_, _| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok((401, r#"{"errorCode":"error.security.account-token-invalid"}"#.to_string()))
        },
        |_| Err(IgApiError { status: 0, message: "network error: down".into() }),
    )
    .expect("a failed re-login is not itself a transport error");
    assert_eq!(out.0, 401);
    assert!(out.1.contains("account-token-invalid"), "{}", out.1);
    assert_eq!(runs.load(Ordering::Relaxed), 1, "no retry once recovery is known to have failed");
}

/// The shared-session guard, in the shape that actually happens: a SIBLING's login lands while
/// this caller's request is still in flight. The ladder snapshotted generation 1, so by the time
/// its 401 comes back the cell already holds 9 — and `IgSession::relogin`'s guard must then log
/// in NOTHING and just report the generation, so the two threads produce one login between them
/// rather than stampeding IG's rate-limited login endpoint.
#[test]
fn a_sibling_login_landing_mid_flight_is_not_re_logged_in_again() {
    let tokens = cell(1);
    let (seen, logins) = (Mutex::new(Vec::new()), AtomicU32::new(0));
    let out = reauth_ladder(
        &tokens,
        "ACC",
        |cst, _| {
            let mut s = seen.lock().unwrap();
            s.push(cst.to_string());
            if s.len() == 1 {
                // ...the sibling's re-login lands right here, while we were on the wire.
                let mut t = tokens.lock().unwrap();
                t.cst = "cst-9".into();
                t.security_token = "xst-9".into();
                t.generation = 9;
                Ok((401, "{}".to_string()))
            } else {
                Ok((200, "{}".to_string()))
            }
        },
        // Exactly what `IgSession::relogin` does when the generation already moved.
        |seen_gen| {
            let current = tokens.lock().unwrap().generation;
            assert_eq!(seen_gen, 1, "the ladder reports the generation IT used");
            assert_ne!(seen_gen, current, "which the sibling has already moved past");
            logins.fetch_add(1, Ordering::Relaxed);
            Ok(current)
        },
    )
    .expect("no transport error");
    assert_eq!(out.0, 200);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["cst-1".to_string(), "cst-9".to_string()],
        "the retry uses the SIBLING's fresh pair"
    );
    assert_eq!(logins.load(Ordering::Relaxed), 1, "asked once; it performed no login");
}

/// ⚠ A dead socket says NOTHING about whether the token is alive. Re-logging-in on a transport
/// error would turn an outage into a login storm against the one endpoint that is rate-limited
/// per account, so it propagates untouched and the ladder never runs.
#[test]
fn a_transport_error_is_never_a_re_login_trigger() {
    let tokens = cell(1);
    let logins = AtomicU32::new(0);
    let err = reauth_ladder(
        &tokens,
        "ACC",
        |_, _| Err(IgApiError { status: 0, message: "network error: reset".into() }),
        |_| {
            logins.fetch_add(1, Ordering::Relaxed);
            Ok(2)
        },
    )
    .expect_err("the transport error propagates");
    assert_eq!(err.status, 0);
    assert_eq!(logins.load(Ordering::Relaxed), 0);
}

/// A 2xx body that carries no stamp is an ERROR naming the field, never a zero.
#[test]
fn a_body_without_the_stamp_is_an_error_naming_the_field() {
    let e = parse_server_time_ms(&serde_json::json!({"encryptionKey": "x"}), 200)
        .expect_err("no timeStamp");
    assert_eq!(e.status, 200);
    assert!(e.message.contains("timeStamp"), "{}", e.message);
}
