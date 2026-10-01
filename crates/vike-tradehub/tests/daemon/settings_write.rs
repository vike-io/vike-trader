//! The `SetSetting` verb end-to-end (split-plane REQ-7, write half): a real PAPER node
//! ([`vike_mount::build_paper_maker_core`]) + the real [`vike_tradehub::server`] with a CONTROL key,
//! the core's `CommandSink` and a [`vike_tradehub::server::SettingsShowSource`] over a REAL
//! settings directory, driven through the REAL client verb
//! ([`vike_tradehub_client::set_setting`]) under [`Scope::Write`].
//!
//! ⚠ **This used to drive a real settings FILE end to end — comment preservation, the typed-confirm
//! ceremony — and both are gone with the file layer** (`docs/decisions/0086`): the daemon now
//! writes through `vike_config::write_setting_row` into the
//! settings DATABASE, over its own control channel, with no confirm ceremony for any key. This file
//! seeds a real `vike.db` (`vike_secrets::plant_settings_rows`) instead of a `config.toml`/
//! `policy.toml`, and its assertions read ROWS back rather than file bytes.
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"settings-write"`, so the client verb sends.
//! - **A valid write lands in the row it names**, and the OTHER row is untouched; the reply carries
//!   `restart_required: true`, and a follow-up `SettingsShow` (the read half's re-read freshness)
//!   serves the NEW value.
//! - **The loader gates the write:** an unknown key is refused with the loader's own message and
//!   the settings database is byte-identical afterwards.
//! - **No confirm is needed for any key, including policy** (0086 point 7): a `policy.toml` write
//!   with no confirm at all lands, and the sealed-policy doctrine (restart-required) is unaffected.
//!   (The old→new AUDIT record for that accepted policy write is asserted in
//!   `tests/settings_write_audit.rs`, which owns the process-global capture subscriber this
//!   grouped binary must not install.)
//! - **A busy database refuses the peer with this daemon's own wait budget**, and NOTHING moves.
//! - **Scope:** an [`Scope::Read`]-authenticated peer's `SetSetting` is denied read-only —
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

use vike_mount::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::proto::{
    FEATURE_SETTINGS_WRITE, NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{NodeKeys, auth, set_setting, settings_show};

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

/// The `config` rows the fixture starts with — the 0086 database-native twin of the old
/// comment-carrying `config.toml` (there is no settings file any more, so there is nothing left to
/// preserve the comments OF; these tests now pin the ROW mechanics instead).
fn config_rows() -> vike_secrets::StoredSettings {
    vike_secrets::StoredSettings {
        settings: vec![
            vike_secrets::SettingRow {
                section: "config".into(),
                key: "tradehub_addr".into(),
                value: "\"127.0.0.1:7979\"".into(),
            },
            vike_secrets::SettingRow {
                section: "config".into(),
                key: "log_dir".into(),
                value: "\"/var/tmp/vike-logs\"".into(),
            },
        ],
        arming: Vec::new(),
        venue: Vec::new(),
    }
}

/// Build a PAPER node and start the server with BOTH keys, the core's `CommandSink` (control
/// enabled) and the given settings source. Returns the live mount and the assigned address.
fn spawn_node(settings: Option<server::SettingsShowSource>) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
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
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            None,
        );
    });
    (mount, addr)
}

/// A node over a REAL settings database seeded with [`config_rows`].
fn spawn_node_with_dir() -> (MakerMount, SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_secrets::plant_settings_rows(dir.path(), &config_rows())
        .expect("seed the settings database");
    let (mount, addr) = spawn_node(Some(server::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        env: HashMap::new(),
        // No hot-apply seam wired: this file pins the WRITE mechanics (the loader gate, the
        // byte-identical refusal), and every key it writes is restart-class anyway. The hot half
        // lives in `settings_hot_reload.rs`, which wires a real seam.
        hot: None,
    }));
    (mount, addr, dir)
}

/// ⚠ THE ROW-WRITE PROOF, over the wire: a valid write through the REAL client verb lands in the
/// settings database (0086: there is no file to preserve comments IN any more, so this used to be
/// the comment-preserving proof and is now the row-mechanics one), the OTHER row is untouched, the
/// reply says restart-to-apply, and the read half's re-read freshness serves the NEW value on the
/// very next `SettingsShow`.
#[test]
fn a_valid_config_write_lands_in_the_database() {
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

    let source = vike_secrets::read_settings_in(dir.path()).expect("read the settings database");
    let rows = &source.rows().expect("rows").settings;
    let addr_row = rows
        .iter()
        .find(|r| r.section == "config" && r.key == "tradehub_addr")
        .expect("the tradehub_addr row");
    assert_eq!(addr_row.value, "\"127.0.0.1:9100\"", "the edited row carries the new value");
    let log_dir_row =
        rows.iter().find(|r| r.section == "config" && r.key == "log_dir").expect("the log_dir row");
    assert_eq!(
        log_dir_row.value, "\"/var/tmp/vike-logs\"",
        "the OTHER row this write did not name is untouched"
    );

    // The read half re-reads the rows per request (its documented freshness), so the fetch the GUI
    // runs after a write shows the NEW value.
    let show = settings_show(addr, OBSERVE_KEY).expect("the read half serves");
    let row =
        show.rows.iter().find(|r| r.key == "config.tradehub_addr").expect("the tradehub_addr row");
    assert_eq!(row.value, "127.0.0.1:9100");
}

/// The loader gates the write: an unknown key is refused with the loader's OWN message (naming
/// the key, `deny_unknown_fields`' vocabulary) and the settings database is byte-identical
/// afterwards — the design's own invariant (a refusal must leave the store untouched), checked
/// here on the real file rather than assumed from the unit tests that already pin it.
#[test]
fn an_unknown_key_is_refused_with_the_loaders_text_and_no_byte_changes() {
    let (_mount, addr, dir) = spawn_node_with_dir();
    let db = vike_secrets::db_path_in(dir.path());
    let before = std::fs::read(&db).expect("read the settings database");

    let err = set_setting(addr, CONTROL_KEY, "config.toml", "config.tradehub_adr", "x", None, None)
        .expect_err("an unknown key must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let msg = err.to_string();
    assert!(msg.contains("tradehub_adr"), "names the offending key: {msg}");
    assert!(msg.contains("unknown field"), "the loader's own vocabulary: {msg}");

    let after = std::fs::read(&db).expect("read the settings database");
    assert_eq!(before, after, "a refused write changes NO byte");
}

/// ⚠ **THE RETYPE CONFIRM IS DELETED, server-side, for every key** (0086 point 7: *"confirmation
/// over confirmation … a nightmare"*) — this used to demand and enforce it for `policy.toml`
/// specifically and is now the test that a policy write with NO confirm at all lands cleanly, the
/// row renders the RESOLVED `Option<f64>` ("250.0", because the read half serves effective values,
/// not raw input), and the sealed-policy doctrine (restart-required) is unaffected by any of this.
#[test]
fn a_policy_write_needs_no_confirm_and_lands() {
    let (_mount, addr, dir) = spawn_node_with_dir();
    let key = "policy.max_notional_per_order";

    let restart = set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", None, None)
        .expect("a policy write with no confirm at all lands (0086 point 7)");
    assert!(
        restart,
        "policy is always restart-required — the sealed-policy doctrine, decision 0005"
    );

    let source = vike_secrets::read_settings_in(dir.path()).expect("read the settings database");
    let row = source
        .rows()
        .expect("rows")
        .settings
        .iter()
        .find(|r| r.section == "policy" && r.key == "max_notional_per_order")
        .expect("the policy row");
    assert_eq!(row.value, "250", "the row holds the caller's JSON scalar");

    let show = settings_show(addr, OBSERVE_KEY).expect("read back");
    let shown = show.rows.iter().find(|r| r.key == key).expect("the policy row");
    assert_eq!(shown.value, "250.0", "the effective row renders the resolved f64");
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
    set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", None, Some("arming the cap"))
        .expect("no confirm is needed any more (0086 point 7) and the write lands");

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
    let control_id = keys.key_id(Scope::Write).expect("the control key has a fingerprint");
    let observe_id = keys.key_id(Scope::Read).expect("…and so does the observe key");
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

/// An [`Scope::Read`]-authenticated peer cannot `SetSetting`: the same read-only `AuthDenied`
/// every `Request::Command` gets, pinned for THIS verb (the write is a control action; the
/// observe key must never sign one).
#[test]
fn an_observe_peer_cannot_set_setting() {
    let (_mount, addr, dir) = spawn_node_with_dir();
    let db = vike_secrets::db_path_in(dir.path());
    let before = std::fs::read(&db).expect("read the settings database");

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
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Read } => {}
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
    let after = std::fs::read(&db).expect("read the settings database");
    assert_eq!(before, after, "nothing was written");
}

/// **A `SetSetting` that names NO `file` and NO `confirm` decodes on the daemon and LANDS** — step 1
/// of taking the file era out of the settings wire. A write is one row in `<settings>/db/vike.db`,
/// named by its KEY alone: the node derives the section from the key and has read neither field
/// since `docs/decisions/0086` (points 6 and 7), so a client that stops sending them must be
/// understood rather than refused at decode. Deleting the fields outright is step 2, after a release
/// carrying this tolerance is deployed — the released v0.1.35 daemon still requires `file`, which is
/// why every client in this tree keeps sending one for now.
///
/// The frame is written RAW, because the typed `WireCommand` cannot omit a field: it is the typed
/// request serialized, with the two fields then DELETED from the JSON, so this test spells nothing
/// about the enum's tagging and fails loudly if either field was not there to delete.
#[test]
fn a_settings_write_with_no_file_and_no_confirm_lands() {
    let (_mount, addr, dir) = spawn_node_with_dir();

    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(CONTROL_KEY, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Write } => {}
        other => panic!("expected a control AuthOk, got {other:?}"),
    }

    let mut frame = serde_json::to_value(Request::Command {
        cmd: WireCommand::SetSetting {
            file: String::new(),
            key: "config.tradehub_addr".into(),
            value: "127.0.0.1:9100".into(),
            confirm: None,
        },
        reason: None,
    })
    .expect("serialize the typed request");
    let shape = frame.to_string();
    let body = frame["Command"]["cmd"]["SetSetting"]
        .as_object_mut()
        .unwrap_or_else(|| panic!("the SetSetting body, in the frame's own tagging: {shape}"));
    assert!(body.remove("file").is_some(), "`file` was on the frame to delete");
    assert!(body.remove("confirm").is_some(), "`confirm` was on the frame to delete");
    write_frame(&mut stream, &frame).expect("send the file-less write");

    match read_frame::<_, Response>(&mut stream).expect("reply") {
        Response::SettingsWritten { restart_required } => assert!(
            restart_required,
            "`config.tradehub_addr` is restart-class: a bound listener is not re-pointed live"
        ),
        other => panic!("a write naming only its key must be applied, got {other:?}"),
    }
    let source = vike_secrets::read_settings_in(dir.path()).expect("read the settings database");
    let rows = &source.rows().expect("rows").settings;
    let row = rows
        .iter()
        .find(|r| r.section == "config" && r.key == "tradehub_addr")
        .expect("the tradehub_addr row");
    assert_eq!(row.value, "\"127.0.0.1:9100\"", "the row the KEY names carries the new value");
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

/// **THE BUDGET THIS DAEMON ASKED FOR, proven over the wire and without a stopwatch.**
///
/// `apply_set_setting` runs on a CONNECTION THREAD with a peer on the far end. The budget is this
/// daemon's own ([`server::SETTINGS_LOCK_BUDGET`]), passed straight through to the settings
/// DATABASE's write lock now (0086 — there is no settings-FILE sentinel to hold any more):
///
/// 1. it is neither of the silent wrong answers: strictly positive (a millisecond overlap with this
///    box's own `vike-cli config set` must not cost the peer a rate token) and bounded well inside
///    the client's own `CONTROL_REPLY_TIMEOUT`, which the compile-time assertion beside the const
///    holds in both directions;
/// 2. with the database's write lock genuinely HELD, the peer gets a `Busy` REFUSAL — which is only
///    possible if the production arm passed the budget through at all — and no byte of the database
///    moves.
///
/// The lock is taken here the way another PROCESS would take it:
/// [`vike_secrets::hold_write_lock`], a second connection's own `BEGIN IMMEDIATE`.
#[test]
fn a_held_settings_database_refuses_the_peer_with_this_daemons_own_budget() {
    assert!(
        server::SETTINGS_LOCK_BUDGET.max_wait_ms() > 0,
        "a peer has a retry loop but also a rate token — a zero budget spends one on a race \
         nobody was in"
    );
    assert!(
        server::SETTINGS_LOCK_BUDGET.max_wait_ms()
            < vike_tradehub_client::liveness::CONTROL_REPLY_TIMEOUT.as_millis() as u64,
        "blocking past the peer's own reply deadline would land a write nobody is left to hear \
         the answer to"
    );

    let (_mount, addr, dir) = spawn_node_with_dir();
    let db = vike_secrets::db_path_in(dir.path());
    let before = std::fs::read(&db).expect("read the settings database");
    let held = vike_secrets::hold_write_lock(dir.path());

    let err = set_setting(
        addr,
        CONTROL_KEY,
        "config.toml",
        "config.tradehub_addr",
        "127.0.0.1:9100",
        None,
        None,
    )
    .expect_err("a held settings database must refuse the peer");
    let msg = err.to_string();
    assert!(
        msg.contains("another process is writing the settings database"),
        "it names the contention: {msg}"
    );
    assert!(msg.contains("NOTHING was written"), "{msg}");
    let after = std::fs::read(&db).expect("read the settings database");
    assert_eq!(before, after, "a busy refusal changes NO byte");
    drop(held);

    // …and the same write lands once the holder releases, so what was proven is the CONTENTION arm
    // rather than a permanently refusing node.
    set_setting(
        addr,
        CONTROL_KEY,
        "config.toml",
        "config.tradehub_addr",
        "127.0.0.1:9100",
        None,
        None,
    )
    .expect("the same write lands once the database is free");
}
