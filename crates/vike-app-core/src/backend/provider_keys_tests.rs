//! The provider-key client against a SCRIPTED tradehub (the real handshake, a recorded log of
//! what arrived) and the view's state machine. Nothing here touches a network beyond loopback.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;

use super::*;
use vike_hub_protocol::access::FEATURE_PERSON_AUTH;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_tradehub_client::proto::NODE_PROTO_VERSION;
use vike_tradehub_client::wire::{AccountWriteWire, CredentialsHeldWire};

/// A distinctive value, so a leak into any message or `Debug` is a substring nothing else holds.
const VALUE: &str = "provider-value-that-must-never-be-echoed-8mz";

/// A device key, standing for the one this desktop holds.
fn device() -> DeviceKey {
    DeviceKey::generate().expect("a device key").0
}

/// How the scripted node answers a WRITE: accept it, refuse it with a fixed sentence, or refuse it quoting the value.
#[derive(Clone, Copy)]
enum Writes {
    Accept,
    Refuse,
    RefuseQuoting,
}

/// A node that serves every connection the test makes: the real handshake, admitting the device
/// `admitted` alone (another device is `AuthDenied`; the access list's own check is the hub's, not
/// under test here), then one `Request::Account` per connection, logged as
/// `"<scope> <verb> confirm=<confirm|->"`. The log also gets `"<scope> connect"` for each accepted
/// handshake, so "nothing was sent" is a log of connects alone.
fn node(
    features: &[&str],
    writes: Writes,
    admitted: &DeviceKey,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("local addr");
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut features: Vec<String> = features.iter().map(|f| (*f).to_string()).collect();
    features.push(FEATURE_PERSON_AUTH.to_string());
    let admitted = admitted.key_id();
    let seen = Arc::clone(&log);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let Ok(HandshakeFrame::Hello { .. }) = read_frame::<_, HandshakeFrame>(&mut stream)
            else {
                continue;
            };
            let nonce = [3u8; 32];
            let welcome = HandshakeReply::Welcome {
                proto_version: NODE_PROTO_VERSION,
                features: features.clone(),
                nonce: Some(nonce),
            };
            write_frame(&mut stream, &welcome).expect("Welcome");
            let Ok(HandshakeFrame::PersonAuth { key_id, scope, .. }) =
                read_frame::<_, HandshakeFrame>(&mut stream)
            else {
                continue;
            };
            if key_id != admitted {
                let denied = HandshakeReply::AuthDenied { reason: "access denied".into() };
                write_frame(&mut stream, &denied).expect("AuthDenied");
                continue;
            }
            write_frame(&mut stream, &HandshakeReply::AuthOk { scope, admitted: None })
                .expect("AuthOk");
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
                (AccountVerbWire::HeldCredentials { names }, _) => {
                    Response::CredentialsHeld(Box::new(CredentialsHeldWire {
                        held: names.iter().take(1).cloned().collect(),
                    }))
                }
                (AccountVerbWire::SetCredential { value, .. }, Writes::RefuseQuoting) => {
                    Response::Error(format!("refused: value={value} is not a key"))
                }
                (_, Writes::Refuse | Writes::RefuseQuoting) => {
                    Response::Error("nothing was changed".into())
                }
                (verb, Writes::Accept) => Response::AccountWritten(Box::new(AccountWriteWire {
                    verb: verb.word().to_string(),
                    row: None,
                    changed: true,
                    note: Some("done".into()),
                })),
            };
            write_frame(&mut stream, &reply).expect("reply");
        }
    });
    (address, log)
}

const BOTH: [&str; 2] = [FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN];

fn key() -> &'static str {
    PROVIDER_KEYS[0].name
}

/// The screen's rows are exactly the closed pair the wire admits and the datahub reads.
#[test]
fn the_screen_rows_are_the_closed_pair_the_wire_admits() {
    let names: Vec<&str> = PROVIDER_KEYS.iter().map(|k| k.name).collect();
    assert_eq!(names, vike_venues::credential_keys::DATA_PROVIDER_KEYS);
}

/// ⚠ **THE THREE ACTS go out under the ADMIN scope, signed with the device key, with the confirm the
/// wire requires**, and a presence read owes none. The node's log is the evidence; the value never
/// appears in it.
#[test]
fn check_save_and_remove_go_out_under_the_admin_scope_with_the_confirm_the_wire_requires() {
    let me = device();
    let (address, log) = node(&BOTH, Writes::Accept, &me);
    let address = address.to_string();

    let held = check(&address, &me).expect("the presence read answers");
    assert_eq!(held, vec![PROVIDER_KEYS[0].name.to_string()], "the node's first asked name");
    save(&address, &me, key(), VALUE).expect("an accepted write");
    remove(&address, &me, key()).expect("an accepted retire");

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
    assert!(!all.contains(VALUE), "the log never names the value: {all}");
}

/// A node that advertises `account-verbs` but not `credential-admin` (the pre-0144 node) is sent
/// NOTHING after the handshake, and the screen says so by name.
#[test]
fn a_server_without_the_credential_capability_is_sent_nothing() {
    let me = device();
    let (address, log) = node(&[FEATURE_ACCOUNT_VERBS], Writes::Accept, &me);
    let err = save(&address.to_string(), &me, key(), VALUE).expect_err("refused client-side");
    assert!(err.contains("does not offer credential administration"), "{err}");
    assert!(err.contains(FEATURE_CREDENTIAL_ADMIN), "names the capability: {err}");
    assert!(!err.contains(VALUE), "never the value: {err}");
    // Give the node's thread its turn to log the hang-up, then: connects only, no verb.
    thread::sleep(std::time::Duration::from_millis(100));
    let log = log.lock().unwrap().clone();
    assert!(log.iter().all(|l| l.ends_with("connect")), "nothing may be sent after auth: {log:?}");
}

/// A device the server does not admit is worded as a refused device that needs the admin role, not
/// as a network fault, and shows the door's one access id once.
#[test]
fn a_device_the_server_refuses_is_worded_as_needing_the_admin_role() {
    let (address, _log) = node(&BOTH, Writes::Accept, &device());
    let err = check(&address.to_string(), &device()).expect_err("access denied");
    assert!(err.contains("refused this desktop") && err.contains("role admin"), "{err}");
    assert!(err.contains("DECLARED"), "{err}");
    assert!(err.contains("error [hub.access.denied]: "), "{err}");
    assert_eq!(err.matches("error [").count(), 1, "ONE id: {err}");
}

/// A refusal the node ANSWERED is its own sentence, verbatim.
#[test]
fn a_node_refusal_is_the_operator_text() {
    let me = device();
    let (address, _log) = node(&BOTH, Writes::Refuse, &me);
    let err = remove(&address.to_string(), &me, key()).expect_err("the node refuses");
    assert_eq!(err, "nothing was changed");
}

/// **No secret in a refused save's words (plan S10)**: a node whose refusal quotes the value it was sent, long or
/// short, has that value masked: the client knows what it sent.
#[test]
fn a_refusal_quoting_the_saved_value_is_masked() {
    let me = device();
    let (address, _log) = node(&BOTH, Writes::RefuseQuoting, &me);
    for value in [VALUE, "Pw4z"] {
        let err = save(&address.to_string(), &me, key(), value).expect_err("the node refuses");
        assert_eq!(err, "refused: value=*** is not a key");
        // secret-absent: the value the client sent never reaches the error's text
        assert!(!format!("{err:?}").contains(value), "{err}");
    }
}

/// ⚠ **A desktop without a device key sends nothing** (decision 0288 §3): a check, a save and a
/// remove each say [`crate::backend::registry::no_device_key`] and start no job. This test binary
/// never holds a device key (`registry::fill_hub_keys` is its one writer, and no unit test calls it).
#[test]
fn without_a_device_key_nothing_is_sent_and_the_screen_says_why() {
    let context = egui::Context::default();
    let said = |view: &BackendProviderKeysView| view.status().map(|(_, text)| text.clone());
    let no_key = Some(crate::backend::registry::no_device_key("127.0.0.1:1"));
    let mut view = BackendProviderKeysView::default();
    view.start_check("127.0.0.1:1", &context);
    assert!(!view.busy());
    assert_eq!(said(&view), no_key);
    view.values[0] = VALUE.to_string();
    view.start_save(0, "127.0.0.1:1", &context);
    assert!(!view.busy());
    assert_eq!(said(&view), no_key);
    view.start_remove(0, "127.0.0.1:1", &context);
    assert!(!view.busy());
    assert_eq!(said(&view), no_key);
}

fn view_with_held(held: &[&str]) -> BackendProviderKeysView {
    let mut view = BackendProviderKeysView::default();
    view.finish(BackendProviderKeysDone::Checked(Ok(held
        .iter()
        .map(|h| (*h).to_string())
        .collect())));
    view.poll(Some("<host>:9099"), &egui::Context::default());
    view
}

/// A check answers each row's mark; a failed one leaves the marks UNKNOWN and says why.
#[test]
fn a_check_marks_each_row_and_a_failed_one_leaves_them_unknown() {
    let view = view_with_held(&[PROVIDER_KEYS[0].name]);
    assert_eq!(view.mark(0), BackendProviderKeyMark::Set);
    assert_eq!(view.mark(1), BackendProviderKeyMark::NotSet);
    assert_eq!(
        BackendProviderKeysView::default().mark(0),
        BackendProviderKeyMark::Unknown,
        "never asked"
    );

    let mut failed = view_with_held(&[PROVIDER_KEYS[0].name]);
    failed.finish(BackendProviderKeysDone::Checked(Err("could not reach the server".into())));
    failed.poll(Some("<host>:9099"), &egui::Context::default());
    assert_eq!(failed.mark(0), BackendProviderKeyMark::Unknown);
    assert_eq!(failed.status(), Some(&(true, "could not reach the server".to_string())));
}

/// Another backend's answer is not this one's: a switch of address forgets the marks, the status
/// and an armed Remove.
#[test]
fn a_switched_backend_forgets_what_the_last_one_said() {
    let mut view = view_with_held(&[PROVIDER_KEYS[0].name]);
    view.armed = Some((0, 1.0));
    view.poll(Some("<host>:9099"), &egui::Context::default());
    assert_eq!(view.mark(0), BackendProviderKeyMark::Unknown);
    assert!(view.armed.is_none());
}

/// The status lines the owner asked for, verbatim, for a removal and a save.
#[test]
fn a_removal_and_a_save_say_what_happened() {
    let context = egui::Context::default();
    let mut view = view_with_held(&[]);
    view.finish(BackendProviderKeysDone::Removed(0, Ok(String::new())));
    view.poll(Some("<host>:9099"), &context);
    assert_eq!(
        view.status(),
        Some(&(false, format!("Removed {} — kept at rest, Save a value to add it back", key())))
    );
    let mut view = view_with_held(&[]);
    view.finish(BackendProviderKeysDone::Saved(1, Ok(String::new())));
    view.poll(Some("<host>:9099"), &context);
    let (is_error, text) = view.status().expect("a status").clone();
    assert!(!is_error);
    assert!(text.contains(PROVIDER_KEYS[1].name) && text.contains("no restart"), "{text}");
}

/// The armed Remove disarms itself after its window.
#[test]
fn an_armed_remove_expires() {
    let mut view = BackendProviderKeysView::default();
    assert_eq!(view.tick_arm(10.0), None);
    view.armed = Some((1, 10.0));
    assert!(view.tick_arm(11.0).is_some());
    assert_eq!(view.tick_arm(10.0 + BackendProviderKeysView::ARM_WINDOW.as_secs_f64()), None);
    assert!(view.armed.is_none());
}

/// Sending a value TAKES the buffer, and it never reaches a `Debug`.
#[test]
fn a_save_takes_the_typed_value_and_debug_prints_no_secret() {
    let mut view = BackendProviderKeysView {
        values: [VALUE.to_string(), String::new()],
        ..BackendProviderKeysView::default()
    };
    let rendered = format!("{view:?}");
    assert!(!rendered.contains(VALUE), "{rendered}");
    // No device key is held here, so nothing is sent; only the buffer is asserted here.
    view.start_save(0, "127.0.0.1:1", &egui::Context::default());
    assert!(view.values[0].is_empty(), "the value left UI memory the moment it was sent");
}

/// Saving an empty buffer sends nothing and says so.
#[test]
fn saving_nothing_says_so_and_sends_nothing() {
    let mut view = BackendProviderKeysView::default();
    view.start_save(0, "127.0.0.1:1", &egui::Context::default());
    assert!(!view.busy());
    assert_eq!(view.status(), Some(&(true, "nothing entered — no changes saved".to_string())));
}
