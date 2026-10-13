//! The provider-key client against a SCRIPTED tradehub node (the real handshake, a recorded log of
//! what arrived) and the view's state machine. Nothing here touches a network beyond loopback.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;

use super::*;
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, read_frame, write_frame};
use vike_tradehub_client::wire::{WireAccountWritten, WireCredentialsHeld};

/// A distinctive admin key and value, so a leak into any message or `Debug` is a substring nothing
/// else holds.
const ADMIN: &str = "admin-key-that-must-never-be-echoed-4kd";
const VALUE: &str = "provider-value-that-must-never-be-echoed-8mz";

/// How the scripted node answers a WRITE: accept it, or refuse it with a fixed sentence.
#[derive(Clone, Copy)]
enum Writes {
    Accept,
    Refuse,
}

/// A node that serves every connection the test makes: the real handshake (a wrong key is
/// `AuthDenied`), then one `Request::Account` per connection, logged as
/// `"<scope> <verb> confirm=<confirm|->"`. The log also gets `"<scope> connect"` for each accepted
/// handshake, so "nothing was sent" is a log of connects alone.
fn node(features: &[&str], writes: Writes) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let log = Arc::new(Mutex::new(Vec::new()));
    let features: Vec<String> = features.iter().map(|f| (*f).to_string()).collect();
    let seen = Arc::clone(&log);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let Ok(Request::Hello { .. }) = read_frame::<_, Request>(&mut stream) else { continue };
            let nonce = [3u8; 32];
            let welcome = Response::Welcome {
                proto_version: NODE_PROTO_VERSION,
                nonce,
                features: features.clone(),
            };
            write_frame(&mut stream, &welcome).expect("Welcome");
            let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
                continue;
            };
            if !vike_tradehub_client::auth::verify(
                ADMIN.as_bytes(),
                &nonce,
                NODE_PROTO_VERSION,
                scope,
                &mac,
            ) {
                write_frame(&mut stream, &Response::AuthDenied { reason: "bad mac".into() })
                    .expect("AuthDenied");
                continue;
            }
            write_frame(&mut stream, &Response::AuthOk { scope }).expect("AuthOk");
            seen.lock().unwrap().push(format!("{} connect", scope.name()));
            // One request at most: a client that refused client-side hangs up without sending.
            let Ok(Request::Account(req)) = read_frame::<_, Request>(&mut stream) else { continue };
            seen.lock().unwrap().push(format!(
                "{} {} confirm={}",
                scope.name(),
                req.verb.word(),
                req.confirm.as_deref().unwrap_or("-")
            ));
            let reply = match (&req.verb, writes) {
                (AccountVerb::HeldCredentials { names }, _) => {
                    Response::CredentialsHeld(Box::new(WireCredentialsHeld {
                        held: names.iter().take(1).cloned().collect(),
                    }))
                }
                (_, Writes::Refuse) => Response::Error("nothing was changed".into()),
                (verb, Writes::Accept) => Response::AccountWritten(Box::new(WireAccountWritten {
                    verb: verb.word().to_string(),
                    row: None,
                    changed: true,
                    note: Some("done".into()),
                })),
            };
            write_frame(&mut stream, &reply).expect("reply");
        }
    });
    (addr, log)
}

const BOTH: [&str; 2] = [FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN];

fn key() -> &'static str {
    PROVIDER_KEYS[0].name
}

/// The screen's rows are exactly the closed pair the wire admits and the datahub reads.
#[test]
fn the_screen_rows_are_the_closed_pair_the_wire_admits() {
    let names: Vec<&str> = PROVIDER_KEYS.iter().map(|k| k.name).collect();
    assert_eq!(names, vike_model::credential_keys::DATA_PROVIDER_KEYS);
}

/// ⚠ **THE THREE ACTS go out under the ADMIN scope, with the confirm the wire requires**, and a
/// presence read owes none. The node's log is the evidence; the key never appears in it.
#[test]
fn check_save_and_remove_go_out_under_the_admin_scope_with_the_confirm_the_wire_requires() {
    let (addr, log) = node(&BOTH, Writes::Accept);
    let addr = addr.to_string();

    let held = check(&addr, ADMIN).expect("the presence read answers");
    assert_eq!(held, vec![PROVIDER_KEYS[0].name.to_string()], "the node's first asked name");
    save(&addr, ADMIN, key(), VALUE).expect("an accepted write");
    remove(&addr, ADMIN, key()).expect("an accepted retire");

    let log = log.lock().unwrap().clone();
    let acts: Vec<&String> = log.iter().filter(|l| !l.ends_with("connect")).collect();
    assert_eq!(
        acts,
        [
            &"admin held-credentials confirm=-".to_string(),
            &format!("admin set-credential confirm={}", key()),
            &format!("admin retire-credential confirm={}", key()),
        ]
    );
    let all = log.join("\n");
    assert!(!all.contains(VALUE) && !all.contains(ADMIN), "the log names neither: {all}");
}

/// A node that advertises `account-verbs` but not `credential-admin` (the pre-0144 node) is sent
/// NOTHING after the handshake, and the screen says so by name.
#[test]
fn a_server_without_the_credential_capability_is_sent_nothing() {
    let (addr, log) = node(&[FEATURE_ACCOUNT_VERBS], Writes::Accept);
    let err = save(&addr.to_string(), ADMIN, key(), VALUE).expect_err("refused client-side");
    assert!(err.contains("does not offer credential administration"), "{err}");
    assert!(err.contains(FEATURE_CREDENTIAL_ADMIN), "names the capability: {err}");
    assert!(!err.contains(VALUE), "never the value: {err}");
    // Give the node's thread its turn to log the hang-up, then: connects only, no verb.
    thread::sleep(std::time::Duration::from_millis(100));
    let log = log.lock().unwrap().clone();
    assert!(log.iter().all(|l| l.ends_with("connect")), "nothing may be sent after auth: {log:?}");
}

/// A wrong admin key is worded as a refused key, not as a network fault.
#[test]
fn a_wrong_admin_key_is_worded_as_a_refused_key() {
    let (addr, _log) = node(&BOTH, Writes::Accept);
    let err = check(&addr.to_string(), "some-other-key").expect_err("bad mac");
    assert!(err.contains("refused the admin key"), "{err}");
    assert!(err.contains("DECLARED"), "{err}");
    assert!(!err.contains("some-other-key"), "the key it was given is never quoted: {err}");
}

/// A refusal the node ANSWERED is its own sentence, verbatim.
#[test]
fn a_node_refusal_is_the_operator_text() {
    let (addr, _log) = node(&BOTH, Writes::Refuse);
    let err = remove(&addr.to_string(), ADMIN, key()).expect_err("the node refuses");
    assert_eq!(err, "nothing was changed");
}

/// With no admin key typed nothing is dialled: the message says what to do.
#[test]
fn an_empty_admin_key_dials_nothing() {
    let err = check("127.0.0.1:1", "   ").expect_err("no key");
    assert!(err.contains("admin key"), "{err}");
}

fn view_with_held(held: &[&str]) -> ProviderKeysView {
    let mut view = ProviderKeysView::default();
    view.finish(Done::Checked(Ok(held.iter().map(|h| (*h).to_string()).collect())));
    view.poll(Some("<host>:9099"), &egui::Context::default());
    view
}

/// A check answers each row's mark; a failed one leaves the marks UNKNOWN and says why.
#[test]
fn a_check_marks_each_row_and_a_failed_one_leaves_them_unknown() {
    let view = view_with_held(&[PROVIDER_KEYS[0].name]);
    assert_eq!(view.mark(0), Mark::Set);
    assert_eq!(view.mark(1), Mark::NotSet);
    assert_eq!(ProviderKeysView::default().mark(0), Mark::Unknown, "never asked");

    let mut failed = view_with_held(&[PROVIDER_KEYS[0].name]);
    failed.finish(Done::Checked(Err("could not reach the server".into())));
    failed.poll(Some("<host>:9099"), &egui::Context::default());
    assert_eq!(failed.mark(0), Mark::Unknown);
    assert_eq!(failed.status(), Some(&(true, "could not reach the server".to_string())));
}

/// Another backend's answer is not this one's: a switch of address forgets the marks, the status
/// and an armed Remove.
#[test]
fn a_switched_backend_forgets_what_the_last_one_said() {
    let mut view = view_with_held(&[PROVIDER_KEYS[0].name]);
    view.armed = Some((0, 1.0));
    view.poll(Some("<host>:9099"), &egui::Context::default());
    assert_eq!(view.mark(0), Mark::Unknown);
    assert!(view.armed.is_none());
}

/// The status lines the owner asked for, verbatim, for a removal and a save.
#[test]
fn a_removal_and_a_save_say_what_happened() {
    let ctx = egui::Context::default();
    let mut view = view_with_held(&[]);
    view.finish(Done::Removed(0, Ok(String::new())));
    view.poll(Some("<host>:9099"), &ctx);
    assert_eq!(
        view.status(),
        Some(&(false, format!("Removed {} — kept at rest, Save a value to add it back", key())))
    );
    let mut view = view_with_held(&[]);
    view.finish(Done::Saved(1, Ok(String::new())));
    view.poll(Some("<host>:9099"), &ctx);
    let (is_error, text) = view.status().expect("a status").clone();
    assert!(!is_error);
    assert!(text.contains(PROVIDER_KEYS[1].name) && text.contains("no restart"), "{text}");
}

/// The armed Remove disarms itself after its window.
#[test]
fn an_armed_remove_expires() {
    let mut view = ProviderKeysView::default();
    assert_eq!(view.tick_arm(10.0), None);
    view.armed = Some((1, 10.0));
    assert!(view.tick_arm(11.0).is_some());
    assert_eq!(view.tick_arm(10.0 + ProviderKeysView::ARM_WINDOW.as_secs_f64()), None);
    assert!(view.armed.is_none());
}

/// Sending a value TAKES the buffer, and neither it nor the admin key reaches a `Debug`.
#[test]
fn a_save_takes_the_typed_value_and_debug_prints_no_secret() {
    let mut view = ProviderKeysView {
        admin_key: ADMIN.to_string(),
        values: [VALUE.to_string(), String::new()],
        ..ProviderKeysView::default()
    };
    let rendered = format!("{view:?}");
    assert!(!rendered.contains(VALUE) && !rendered.contains(ADMIN), "{rendered}");
    // The address is unroutable, so the worker fails fast; only the buffer is asserted here.
    view.start_save(0, "127.0.0.1:1", &egui::Context::default());
    assert!(view.values[0].is_empty(), "the value left UI memory the moment it was sent");
}

/// Saving an empty buffer sends nothing and says so.
#[test]
fn saving_nothing_says_so_and_sends_nothing() {
    let mut view = ProviderKeysView::default();
    view.start_save(0, "127.0.0.1:1", &egui::Context::default());
    assert!(!view.busy());
    assert_eq!(view.status(), Some(&(true, "nothing entered — no changes saved".to_string())));
}
