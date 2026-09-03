//! The `SetSetting` verb end-to-end (split-plane REQ-7, write half): a real PAPER node
//! ([`vike_run::build_paper_maker_core`]) + the real [`vike_tradehub::server`] with a CONTROL key,
//! the core's `CommandSink` and a [`vike_tradehub::server::SettingsShowSource`] over a REAL
//! settings directory, driven through the REAL client verb
//! ([`vike_tradehub_client::set_setting`]) under [`Scope::Control`].
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"settings-write"`, so the client verb sends.
//! - **A valid write lands BYTE-PRESERVING:** the fixture `config.toml` carries a header comment,
//!   a commented-out key, blank lines and a same-line trailing comment — after the write, every
//!   untouched line is byte-identical and the edited line keeps its own trailing comment; the
//!   reply carries `restart_required: true`, and a follow-up `SettingsShow` (the read half's
//!   re-read freshness) serves the NEW value.
//! - **The loader gates the write:** an unknown key is refused with the loader's own message and
//!   the file is byte-identical afterwards.
//! - **The policy TYPED-CONFIRM contract:** a `policy.toml` write with NO confirm is refused, a
//!   WRONG confirm is refused (each refusal naming the expected key), and the EXACT confirm
//!   lands. (The old→new AUDIT record for that accepted policy write is asserted in
//!   `tests/settings_write_audit.rs`, which owns the process-global capture subscriber this
//!   grouped binary must not install.)
//! - **Scope:** an [`Scope::Observe`]-authenticated peer's `SetSetting` is denied read-only —
//!   the same gate every `Request::Command` faces, pinned here for THIS verb.
//! - **The no-source shape:** a node started without a settings source refuses the write
//!   honestly (feature advertised, command decoded, refusal named).
//!
//! Grouped into the `daemon` binary: plain tests, no `#[ignore]`, no crate-level `#![cfg]`, and
//! no process-global mutation — the settings directory is a `tempfile::TempDir` and the
//! "environment" a local `HashMap`, so nothing touches `std::env` or the CWD.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;

use vike_run::{build_paper_maker_core, MakerMount, MakerMountConfig};
use vike_tradehub::{publish, server};
use vike_tradehub_client::proto::{
    read_frame, write_frame, Request, Response, Scope, FEATURE_SETTINGS_WRITE, NODE_PROTO_VERSION,
};
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{auth, set_setting, settings_show, NodeKeys};

const TOKEN: &str = "SETTINGS_WRITE_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the settings-show mount).
const RESOLUTION_TS: i64 = 3_000_000_000;
/// ⚠ **Deliberately not an English phrase, and this is not cosmetic.**
/// [`the_change_journal_names_the_key_that_authenticated_the_write`] asserts that no four-byte
/// window of either key appears in the ledger file. These were spelled
/// `settings-write-{observe,control}-key`, which shares windows (`sett`, `ting`, `cont`, `trol`)
/// with the record's OWN vocabulary — `{"kind":"set_setting", … "scope":"control"}` — so that
/// assertion would have failed on a file containing no leak at all. Nothing else depends on the
/// bytes: they are arbitrary signing material.
const OBSERVE_KEY: &[u8] = b"ZqXvNbKdMsWtYrHjPlGf";
const CONTROL_KEY: &[u8] = b"TcRxSaUeObJwLiVnEyDu";

/// The hand-shaped fixture `config.toml` — a header comment, a commented-out key, a set key with
/// a same-line comment, blank lines: everything a re-serializing writer would destroy.
const CONFIG_WITH_COMMENTS: &str = "\
# deployment config — hand-tuned 2026-08-12
# store_root = \"/var/lib/vike\"   (moved to the NVMe 08-01)

tradehub_addr = \"127.0.0.1:7979\"  # loopback only; ssh tunnel in front

log_dir = \"/var/tmp/vike-logs\"
";

/// Build a PAPER node and start the server with BOTH keys, the core's `CommandSink` (control
/// enabled) and the given settings source. Returns the live mount and the assigned address.
fn spawn_node(settings: Option<server::SettingsShowSource>) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let commands = Some(mount.handle.command_sink());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::ControlLimitsConfig::default(),
            settings,
            // No REQ-2 datahub advertisement — this suite exercises the settings verbs only.
            None,
        );
    });
    (mount, addr)
}

/// A node over a REAL settings directory seeded with the comment-carrying `config.toml`.
fn spawn_node_with_dir() -> (MakerMount, SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp settings dir");
    std::fs::write(dir.path().join("config.toml"), CONFIG_WITH_COMMENTS).expect("write fixture");
    let (mount, addr) = spawn_node(Some(server::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        env: HashMap::new(),
        // No hot-apply seam wired: this file pins the WRITE mechanics (comment preservation,
        // the loader gate, the typed confirm), and every key it writes is restart-class anyway.
        // The hot half lives in `settings_hot_reload.rs`, which wires a real seam.
        hot: None,
    }));
    (mount, addr, dir)
}

/// ⚠ THE COMMENT-PRESERVING PROOF, over the wire: a valid `config.toml` write through the REAL
/// client verb lands with every untouched line byte-identical (header comment, commented-out
/// key, blank lines, the other key) and the edited line keeping its own trailing comment; the
/// reply says restart-to-apply; and the read half's re-read freshness serves the NEW value on
/// the very next `SettingsShow`.
#[test]
fn a_valid_config_write_lands_byte_preserving_comments() {
    let (_mount, addr, dir) = spawn_node_with_dir();

    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "config.toml",
        "config.tradehub_addr",
        "127.0.0.1:9100",
        None,
        Some("moving the node port"),
    )
    .expect("a valid config write lands");
    assert!(
        restart,
        "`config.tradehub_addr` is a RESTART-class key (`vike_tradehub::hot_reload`): the \
         address a listener is already bound to cannot be re-pointed mid-flight"
    );

    let after = std::fs::read_to_string(dir.path().join("config.toml")).expect("read back");
    let before_lines: Vec<&str> = CONFIG_WITH_COMMENTS.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    assert_eq!(before_lines.len(), after_lines.len(), "no line added or removed:\n{after}");
    for (b, a) in before_lines.iter().zip(&after_lines) {
        if b.starts_with("tradehub_addr") {
            assert_eq!(
                *a, "tradehub_addr = \"127.0.0.1:9100\"  # loopback only; ssh tunnel in front",
                "the edited line keeps its trailing comment"
            );
        } else {
            assert_eq!(a, b, "an untouched line is untouched bytes");
        }
    }

    // The read half re-reads the files per request (its documented freshness), so the fetch the
    // GUI runs after a write shows the NEW value with its file origin.
    let show = settings_show(addr, OBSERVE_KEY).expect("the read half serves");
    let row =
        show.rows.iter().find(|r| r.key == "config.tradehub_addr").expect("the tradehub_addr row");
    assert_eq!(row.value, "127.0.0.1:9100");
    assert_eq!(row.origin, "config.toml");
}

/// The loader gates the write: an unknown key is refused with the loader's OWN message (naming
/// the key, `deny_unknown_fields`' vocabulary) and the file is byte-identical afterwards.
#[test]
fn an_unknown_key_is_refused_with_the_loaders_text_and_no_byte_changes() {
    let (_mount, addr, dir) = spawn_node_with_dir();

    let err = set_setting(addr, CONTROL_KEY, "config.toml", "config.tradehub_adr", "x", None, None)
        .expect_err("an unknown key must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let msg = err.to_string();
    assert!(msg.contains("tradehub_adr"), "names the offending key: {msg}");
    assert!(msg.contains("unknown field"), "the loader's own vocabulary: {msg}");

    let after = std::fs::read_to_string(dir.path().join("config.toml")).expect("read back");
    assert_eq!(after, CONFIG_WITH_COMMENTS, "a refused write changes NO byte");
}

/// ⚠ THE TYPED-CONFIRM CONTRACT, server-side: a `policy.toml` write with NO confirm is refused
/// naming the expected key; a WRONG confirm is refused naming both; the EXACT confirm lands (and
/// creates the file — restart-to-apply). The client cannot skip the ceremony because this arm is
/// the enforcement, not the GUI.
#[test]
fn a_policy_write_demands_the_exact_typed_confirm() {
    let (_mount, addr, dir) = spawn_node_with_dir();
    let key = "policy.max_notional_per_order";

    // (a) No confirm at all.
    let err = set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", None, None)
        .expect_err("a policy write without confirm is refused");
    assert!(err.to_string().contains("typed confirm"), "{err}");
    assert!(err.to_string().contains(key), "the refusal names the expected key: {err}");
    assert!(!dir.path().join("policy.toml").exists(), "nothing was written");

    // (b) A near-miss confirm.
    let err = set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", Some("max_notional"), None)
        .expect_err("a wrong confirm is refused");
    assert!(err.to_string().contains("confirm mismatch"), "{err}");
    assert!(err.to_string().contains(key), "{err}");
    assert!(!dir.path().join("policy.toml").exists(), "still nothing written");

    // (c) The exact key confirms; the write lands and the loaded model agrees. The FILE holds
    // the TOML `250` the caller sent; the row renders the RESOLVED `Option<f64>` — "250.0" —
    // because the read half serves effective values, not file bytes.
    let restart = set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", Some(key), None)
        .expect("the exact confirm lands");
    assert!(restart);
    let written = std::fs::read_to_string(dir.path().join("policy.toml")).expect("read back");
    assert_eq!(written, "max_notional_per_order = 250\n", "the file holds the caller's TOML");
    let show = settings_show(addr, OBSERVE_KEY).expect("read back");
    let row = show.rows.iter().find(|r| r.key == key).expect("the policy row");
    assert_eq!(row.value, "250.0", "the effective row renders the resolved f64");
    assert_eq!(row.origin, "policy.toml");
}

/// ⚠⚠ **THE ACTOR, END TO END: the ledger names the KEY that authenticated the socket.**
///
/// `crates/vike-tradehub/tests/settings_write_journal.rs` drives
/// [`vike_tradehub::audit::record_settings_write`] directly, so it can prove the record's SHAPE but
/// never that the server hands it the right identity. This one goes through the real listener, the
/// real handshake (a mac signed with [`CONTROL_KEY`], verified against the server's own
/// [`NodeKeys`]) and the real `accept_command`, then reads the file off disk.
///
/// What it pins, and each is a distinct way the wiring could be wrong:
/// - the recorded `key_id` is the fingerprint of `CONTROL_KEY` — the key that actually signed;
/// - it is NOT `OBSERVE_KEY`'s, so a server that fingerprinted the wrong slot fails here;
/// - neither key's BYTES reach the file, which a ledger nothing rotates away must guarantee.
#[test]
fn the_change_journal_names_the_key_that_authenticated_the_write() {
    let (_mount, addr, dir) = spawn_node_with_dir();
    let key = "policy.max_notional_per_order";
    set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", Some(key), Some("arming the cap"))
        .expect("the exact confirm lands");

    // The ledger sits under the SAME settings directory the write landed in, in the CURRENT
    // month's file — found by listing rather than by computing a month, so this test reads no
    // clock of its own.
    let changes = dir.path().join("state").join("changes");
    let file = std::fs::read_dir(&changes)
        .unwrap_or_else(|e| panic!("no journal directory at {} ({e})", changes.display()))
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .expect("one monthly journal file");
    let raw = std::fs::read_to_string(&file).expect("read the ledger");
    let record: serde_json::Value =
        serde_json::from_str(raw.lines().next().expect("one record")).expect("one JSON object");

    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let control_id = keys.key_id(Scope::Control).expect("the control key has a fingerprint");
    let observe_id = keys.key_id(Scope::Observe).expect("…and so does the observe key");
    assert_ne!(control_id, observe_id, "precondition: the two keys are distinguishable");

    assert_eq!(record["actor"]["origin"], "wire");
    assert_eq!(record["actor"]["scope"], "control");
    assert_eq!(
        record["actor"]["key_id"], control_id,
        "the ledger must name the key that SIGNED the handshake"
    );
    assert_ne!(
        record["actor"]["key_id"], observe_id,
        "…and never the other slot's key: a server fingerprinting the wrong one fails here"
    );
    // The write itself is in the same record, so the identity is attached to a real change.
    assert_eq!(record["kind"], "set_setting");
    assert_eq!(record["target"]["key"], key);
    assert_eq!(record["target"]["new"], "250");

    // ⚠ And no key material anywhere in the file — asserted on the BYTES, and on four-byte windows
    // of them, because a partial leak is a leak and this file is append-only.
    //
    // Windowed HEX is checked in `tests/settings_write_journal.rs`'s twin rather than here: this
    // record's `proc.bin` is the test binary's stem, which carries a per-BUILD hex hash, and an
    // 8-character hex needle could in principle collide with it. The raw-byte windows below cannot
    // — letters do not appear in a hex hash — so this file gets the deterministic half.
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    for k in [OBSERVE_KEY, CONTROL_KEY] {
        let text = std::str::from_utf8(k).expect("the fixture keys are ASCII");
        assert!(!raw.contains(text), "a node key reached the ledger: {raw}");
        assert!(!raw.contains(&hex(k)), "a node key's hex reached the ledger: {raw}");
        for w in k.windows(4) {
            let w_text = std::str::from_utf8(w).expect("ASCII");
            assert!(!raw.contains(w_text), "a 4-byte window of a node key leaked: {raw}");
        }
    }
    // ⚠ The anti-vacuity control, keyed on a precondition INDEPENDENT of the leak question: the
    // detector can fire at all, and it fires on these very fixtures. Without it every `!contains`
    // above would pass against an empty string.
    let planted = format!("{}|{}", std::str::from_utf8(OBSERVE_KEY).unwrap(), hex(CONTROL_KEY));
    assert!(OBSERVE_KEY.windows(4).all(|w| planted.contains(std::str::from_utf8(w).unwrap())));
    assert!(planted.contains(&hex(CONTROL_KEY)));
}

/// An [`Scope::Observe`]-authenticated peer cannot `SetSetting`: the same read-only `AuthDenied`
/// every `Request::Command` gets, pinned for THIS verb (the write is a control action; the
/// observe key must never sign one).
#[test]
fn an_observe_peer_cannot_set_setting() {
    let (_mount, addr, dir) = spawn_node_with_dir();

    // Hand-rolled observe handshake (the client verb always authenticates Control, which is the
    // point — so this speaks raw frames).
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let (nonce, features) = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, features, .. } => (nonce, features),
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(
        features.iter().any(|f| f == FEATURE_SETTINGS_WRITE),
        "the node advertises settings-write: {features:?}"
    );
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Observe);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Observe, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Observe } => {}
        other => panic!("expected observe AuthOk, got {other:?}"),
    }

    write_frame(
        &mut stream,
        &Request::Command {
            cmd: WireCommand::SetSetting {
                file: "config.toml".into(),
                key: "config.tradehub_addr".into(),
                value: "0.0.0.0:1".into(),
                confirm: None,
            },
            reason: None,
        },
    )
    .expect("send the write");
    match read_frame::<_, Response>(&mut stream).expect("reply") {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("read-only"), "the observe scope gate: {reason}")
        }
        other => panic!("an observe peer's SetSetting must be AuthDenied, got {other:?}"),
    }
    let after = std::fs::read_to_string(dir.path().join("config.toml")).expect("read back");
    assert_eq!(after, CONFIG_WITH_COMMENTS, "nothing was written");
}

/// A node started WITHOUT a settings source still advertises + decodes the verb and refuses it
/// honestly — the read half's no-source shape, on the write.
#[test]
fn a_node_without_a_settings_source_refuses_the_write_honestly() {
    let (_mount, addr) = spawn_node(None);
    let err = set_setting(
        addr,
        CONTROL_KEY,
        "config.toml",
        "config.tradehub_addr",
        "127.0.0.1:9100",
        None,
        None,
    )
    .expect_err("no source ⇒ refuse");
    assert!(err.to_string().contains("settings write unavailable"), "{err}");
}

/// …and a node whose source resolved NO settings directory (no project above the working
/// directory) refuses with its own honest message — distinct from the no-source shape, because
/// the operator's fix is different (set `VIKE_SETTINGS_DIR` / run under a project).
#[test]
fn a_node_without_a_settings_directory_refuses_the_write_honestly() {
    let (_mount, addr) = spawn_node(Some(server::SettingsShowSource {
        settings_dir: None,
        env: HashMap::new(),
        hot: None,
    }));
    let err = set_setting(
        addr,
        CONTROL_KEY,
        "config.toml",
        "config.tradehub_addr",
        "127.0.0.1:9100",
        None,
        None,
    )
    .expect_err("no settings dir ⇒ refuse");
    assert!(err.to_string().contains("no settings directory"), "{err}");
}
