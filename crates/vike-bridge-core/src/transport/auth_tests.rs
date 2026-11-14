//! `TokenCell`'s ladder and guard (offline: no agent, no network), the properties ig's own `reauth_ladder` tests held:
//! COUNTS of runs and logins, the token each run was handed, the notes, and the proactive-refresh rule.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

const DEAD: u16 = 401;

fn quiet(_: TokenReauthNote) {}

#[test]
fn a_token_is_due_a_margin_before_its_expiry_and_never_without_one() {
    assert!(!token_refresh_due(Some(10_000), 4_999, 5_000));
    assert!(token_refresh_due(Some(10_000), 5_000, 5_000));
    assert!(token_refresh_due(Some(10_000), 20_000, 5_000));
    assert!(!token_refresh_due(None, i64::MAX, 5_000), "an unknown expiry is not due");
    assert!(token_refresh_due(Some(i64::MIN + 1), i64::MIN + 1, 5_000), "saturates, never wraps");
}

/// The ordinary path costs exactly one round trip and no login.
#[test]
fn an_answer_that_is_not_the_dead_status_never_re_logs_in() {
    let cell = TokenCell::new("t-1".to_string());
    let (runs, logins) = (AtomicU32::new(0), AtomicU32::new(0));
    let out = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerFirst,
            |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok::<_, String>((200, "{}".to_string()))
            },
            |_| {
                logins.fetch_add(1, Ordering::Relaxed);
                Ok(2)
            },
            |n| panic!("no note on the ordinary path: {n:?}"),
        )
        .unwrap();
    assert_eq!((out.0, runs.into_inner(), logins.into_inner()), (200, 1, 0));
}

/// The recovery: one login, then a retry with the NEW token, asserted by the tokens each run was handed.
#[test]
fn the_dead_status_re_logs_in_once_and_retries_with_the_fresh_token() {
    let cell = TokenCell::new("t-1".to_string());
    let (seen, notes) = (Mutex::new(Vec::new()), Mutex::new(Vec::new()));
    let out = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerFirst,
            |t| {
                seen.lock().unwrap().push(t.clone());
                Ok::<_, String>(if t == "t-1" { (DEAD, "dead".into()) } else { (200, "ok".into()) })
            },
            |gen_seen| {
                assert_eq!(gen_seen, 1, "the ladder reports the generation it used");
                cell.relogin(gen_seen, || Ok("t-2".to_string()))
            },
            |n| notes.lock().unwrap().push(n),
        )
        .unwrap();
    assert_eq!(out, (200, "ok".to_string()));
    assert_eq!(*seen.lock().unwrap(), ["t-1", "t-2"]);
    assert_eq!(*notes.lock().unwrap(), [TokenReauthNote::Reauthenticating]);
    assert_eq!(cell.current(), ("t-2".to_string(), 2));
}

/// The anti-spin property: the dead status again on a FRESH token is a refusal; two runs, one login, the venue's own
/// body back, and the note that says so.
#[test]
fn the_dead_status_again_after_a_fresh_login_stops_instead_of_spinning() {
    let cell = TokenCell::new("t-1".to_string());
    let (runs, notes) = (AtomicU32::new(0), Mutex::new(Vec::new()));
    let out = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerFirst,
            |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok::<_, String>((DEAD, "client-token-invalid".into()))
            },
            |seen| cell.relogin(seen, || Ok("t-2".to_string())),
            |n| notes.lock().unwrap().push(n),
        )
        .unwrap();
    assert_eq!(out, (DEAD, "client-token-invalid".to_string()));
    assert_eq!(runs.into_inner(), 2, "exactly one retry, never a loop");
    assert_eq!(
        *notes.lock().unwrap(),
        [TokenReauthNote::Reauthenticating, TokenReauthNote::RefusedAgain]
    );
}

/// A re-login that fails is not retried, hands back the FIRST answer, and leaves the old token in place.
#[test]
fn a_failed_re_login_surfaces_the_first_answer_and_keeps_the_token() {
    let cell = TokenCell::new("t-1".to_string());
    let runs = AtomicU32::new(0);
    let out = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerFirst,
            |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok::<_, String>((DEAD, "account-token-invalid".into()))
            },
            |seen| cell.relogin(seen, || Err("network error: down".to_string())),
            quiet,
        )
        .unwrap();
    assert_eq!(out, (DEAD, "account-token-invalid".to_string()));
    assert_eq!(runs.into_inner(), 1, "no retry once recovery is known to have failed");
    assert_eq!(cell.current(), ("t-1".to_string(), 1));
}

/// The shared-token guard: a sibling's login lands while this call is on the wire, so this caller's re-login logs in
/// NOTHING, and the retry uses the sibling's token.
#[test]
fn a_sibling_login_landing_mid_flight_is_not_re_logged_in_again() {
    let cell = TokenCell::new("t-1".to_string());
    let (seen, logins) = (Mutex::new(Vec::new()), AtomicU32::new(0));
    let out = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerFirst,
            |t| {
                let mut s = seen.lock().unwrap();
                s.push(t.clone());
                if s.len() == 1 {
                    cell.relogin(1, || Ok::<_, String>("t-9".to_string())).unwrap();
                    Ok::<_, String>((DEAD, "{}".into()))
                } else {
                    Ok((200, "{}".into()))
                }
            },
            |seen_gen| {
                cell.relogin(seen_gen, || {
                    logins.fetch_add(1, Ordering::Relaxed);
                    Ok("never".to_string())
                })
            },
            quiet,
        )
        .unwrap();
    assert_eq!(out.0, 200);
    assert_eq!(*seen.lock().unwrap(), ["t-1", "t-9"], "the retry uses the sibling's token");
    assert_eq!(logins.into_inner(), 0, "the generation had moved: no login");
    assert_eq!(cell.current().1, 2);
}

/// A dead socket says nothing about the token: the error propagates and the ladder never logs in.
#[test]
fn a_transport_error_is_never_a_re_login_trigger() {
    let cell = TokenCell::new("t-1".to_string());
    let err = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerFirst,
            |_| Err::<(u16, String), _>("network error: reset".to_string()),
            |_| panic!("no re-login on a transport error"),
            quiet,
        )
        .unwrap_err();
    assert_eq!(err, "network error: reset");
}

/// The other answer to a failed re-login: the LOGIN's own error, not the first answer (an unreachable mint must not
/// wear the dead status), with no retry and the old token kept.
#[test]
fn a_failed_re_login_can_answer_the_logins_own_error() {
    let cell = TokenCell::new("t-1".to_string());
    let runs = AtomicU32::new(0);
    let err = cell
        .reauth_once(
            DEAD,
            TokenReauthFailedLogin::AnswerLoginError,
            |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok::<_, String>((DEAD, "expired".into()))
            },
            |seen| cell.relogin(seen, || Err("mint unreachable".to_string())),
            quiet,
        )
        .unwrap_err();
    assert_eq!(err, "mint unreachable");
    assert_eq!(runs.into_inner(), 1, "no retry once recovery is known to have failed");
    assert_eq!(cell.current(), ("t-1".to_string(), 1));
}

/// A mint error's words: one wording for every venue (ruling U-7), its class in the words.
#[test]
fn a_mint_error_says_what_it_is() {
    let refused = TokenMintError::Refused { code: "ACCESS_DENIED".into(), words: "no".into() };
    assert_eq!(refused.to_string(), "token mint refused ACCESS_DENIED: no");
    assert_eq!(TokenMintError::Malformed("x".into()).to_string(), "malformed token response: x");
    assert_eq!(TokenMintError::Unreachable("y".into()).to_string(), "token mint unreachable: y");
}
