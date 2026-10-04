//! **`DukascopyVenueMount::mount`'s SUCCESS path, through a real child process** — the one path the
//! bridge's unit tests (`src/mount_tests.rs`) cannot reach, because only a sidecar that really
//! started yields a `DukascopyExecutionClient`, and only such a mount parks the venue's answer.
//!
//! The start is the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`), reached the way a
//! deployed box reaches its JVM: a project folder whose `bin/jre/<image>/bin/java` is that program
//! and whose `bin/jforex/` holds the jar. `resolve_dukascopy_tools` therefore finds the program and
//! the jar only under the BIN directory the mount is handed, and puts the JVM's home under the STATE
//! directory. No Java and no network.
//!
//! Unix only: the staged `java` is a SYMLINK to the fake bridge rather than a copy, so a scratch
//! filesystem mounted `noexec` cannot turn this red.
#![cfg(unix)]

use vike_bridge_core::account_directory::AccountDirectory;
use vike_bridge_core::credentials::{Account, AccountKeys, Accounts};
use vike_bridge_core::venue_mount::{ExecOutcome, ProcessFacts, Tier, VenueMount};
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_dukascopy::mount::DukascopyVenueMount;
use vike_dukascopy::{
    BRIDGE_JAR_FILE, DukascopyAccount, JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR, JRE_TOOL_DIR,
};
use vike_model::accounts::account_confirmation::ConfirmationRecord;
use vike_model::accounts::account_keys::AccountLabel;

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

/// The folder the fake bridge makes beneath the `user.home` it is handed, and the file it writes
/// that home into — `src/bin/fake_jforex_bridge.rs`'s `report_user_home`.
const JFOREX_FOLDER: &str = "JForex";
const USER_HOME_REPORT: &str = "user-home.txt";

/// A scratch project folder staged the way a deployed box is: `bin/jre/<image>/bin/java` is the
/// fake bridge, `bin/jforex/jforex-bridge.jar` exists (the start's live gate is `is_file`, so an
/// empty file is a jar here), and `settings/state` is the state directory. Returns the folder,
/// which deletes itself on drop, with the two directories `vike-mount` would hand the mount.
fn staged_project() -> (tempfile::TempDir, ProcessFacts) {
    let root = tempfile::tempdir().expect("a scratch project folder");
    let bin = root.path().join("bin");
    let java = bin.join(JRE_TOOL_DIR).join("fake-jre").join("bin").join("java");
    std::fs::create_dir_all(java.parent().expect("the JRE's bin dir")).expect("the JRE tree");
    std::os::unix::fs::symlink(BRIDGE, &java).expect("the fake bridge stands in for java");
    let jar = bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE);
    std::fs::create_dir_all(jar.parent().expect("the jar's dir")).expect("the jar tree");
    std::fs::write(&jar, b"").expect("plant the jar");
    let state = root.path().join("settings").join("state");
    (root, ProcessFacts { state_dir: Some(state), bin_dir: Some(bin), ..Default::default() })
}

/// The live shape of the store: two unlabelled demo rows, told apart only by their key prefixes,
/// with the books an operator wrote against them — row 7 owns DEMO1 (the Swiss bank), row 8 owns
/// DEMO2 (the EU entity).
fn two_account_store() -> AccountDirectory {
    let row = |id: i64, book: &str| Account {
        id,
        venue: "dukascopy".to_string(),
        tier: "demo".to_string(),
        label: None,
        venue_account_id: Some(book.to_string()),
        parent_id: None,
        active: true,
        last_verified_at: None,
        armed: false,
    };
    let keys = |id: i64, account: DukascopyAccount| {
        (id, AccountKeys { prefixes: vec![account.key_prefix().to_string()], names: Vec::new() })
    };
    AccountDirectory::from_rows(
        Accounts::Known(vec![row(7, "3709890"), row(8, "3716974")]),
        Some([keys(7, DukascopyAccount::Demo1), keys(8, DukascopyAccount::Demo2)].into()),
    )
}

/// A mount whose sidecar STARTS is live at the DEMO tier, derives its reconcile client from the
/// running sidecar, and parks the venue's answer — here the EU row's, addressed by its own key
/// prefix, with both logins in the store — in the state directory it was HANDED: the one fact
/// about `mount` no offline double can witness, since the park follows a real start. (The fake's
/// answer disagrees with the stored book; a disagreement is reported and parked all the same.) The
/// child's own `user.home` report lands under that same state directory, and the start could find
/// its program and its jar only under the handed bin directory: the two directories reach
/// `resolve_dukascopy_tools` each in its own place.
#[test]
fn a_started_sidecar_is_live_and_parks_its_confirmation_in_the_handed_state_directory() {
    let (_project, process) = staged_project();
    let state = process.state_dir.clone().expect("staged");
    let mut fx = MountFixture::new(&[
        ("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"),
        ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1"),
        ("DUKASCOPY_DEMO2_LOGIN", "fake-login-2"),
        ("DUKASCOPY_DEMO2_PASSWORD", "fake-pass-2"),
    ]);
    fx.accounts = two_account_store();
    fx.account = AccountLabel::parse("3716974").expect("a legal label");
    fx.process = process;
    let (tx, _rx) = vike_exec::event_channel(64);
    let mut req = fx.request(true, "EURUSD", &tx);
    req.recon_enabled = true;
    let out = DukascopyVenueMount.mount(req);

    let ExecOutcome::Live(live) = out.exec else {
        panic!("the staged sidecar started, so the mount must be LIVE");
    };
    assert_eq!(live.bound_tier, Tier::Demo, "the sidecar logs into a DEMO server");
    assert!(live.grid.is_none(), "this venue pre-fetches no grid");
    assert!(out.recon.is_some(), "reconciliation on ⇒ a recon client from the running sidecar");
    assert!(out.identity.is_none(), "the venue's answer is PARKED here, not handed to vike-mount");

    // THE PARK, in the HANDED state directory: the EU row's record, beside what the venue answered.
    let parked =
        vike_model::accounts::account_confirmation::read(Some(state.as_path())).expect("readable");
    assert_eq!(parked.len(), 1, "one confirmation parked in the handed state dir: {parked:?}");
    let expected = ConfirmationRecord {
        venue: "dukascopy".to_string(),
        key_prefix: DukascopyAccount::Demo2.key_prefix().to_string(),
        label: None,
        tier: None,
        handshake_account_id: "FAKE-DEMO".to_string(),
        observed_row: Some(8),
        observed_book: Some("3716974".to_string()),
        at_ms: parked[0].at_ms,
    };
    assert_eq!(parked[0], expected, "the EU row's own record, and the fake's ready envelope");

    // …and the JVM's home is under the handed STATE directory, as the child itself reports it.
    let home = state.join(JFOREX_HOME_SUBDIR);
    let report = home.join(JFOREX_FOLDER).join(USER_HOME_REPORT);
    assert_eq!(
        std::fs::read_to_string(&report).ok(),
        Some(home.display().to_string()),
        "the started sidecar reports the user.home it was given, at {}",
        report.display()
    );

    // Reaps the child: `DukascopyExecutionClient`'s `Drop` shuts the sidecar down.
    drop(live);
}

/// **A mounted client watches the HALT sentinel its MOUNT was handed** — through the same real
/// sidecar start, so the proof is of the client `mount` actually returns and not of a double.
///
/// Decision 0099: this client used to resolve the sentinel itself at its submit seam
/// (`vike_bridge_core::halt::halt_path_from_env`), a process-global read a bridge may not make; the
/// path now arrives as `MountInputs::process.halt_path` and the mount hands it to the client. The
/// path engaged below is NOT any resolver's answer — no declaration, no walk, no variable names it —
/// so the only way an opening order can be refused is the mount having wired it. Delete the
/// `.with_halt_path(..)` call from `mount_with` and the order reaches the sidecar instead.
#[test]
fn a_mounted_client_watches_the_sentinel_its_mount_was_handed() {
    let (_project, mut process) = staged_project();
    let state = process.state_dir.clone().expect("staged");
    std::fs::create_dir_all(&state).expect("the state directory");
    let sentinel = state.join("HALT");
    process.halt_path = sentinel.clone();
    let mut fx = MountFixture::new(&[
        ("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"),
        ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1"),
        ("DUKASCOPY_DEMO2_LOGIN", "fake-login-2"),
        ("DUKASCOPY_DEMO2_PASSWORD", "fake-pass-2"),
    ]);
    fx.accounts = two_account_store();
    fx.account = AccountLabel::parse("3716974").expect("a legal label");
    fx.process = process;
    let (tx, mut rx) = vike_exec::event_channel(64);
    let req = fx.request(true, "EURUSD", &tx);
    let ExecOutcome::Live(mut live) = DukascopyVenueMount.mount(req).exec else {
        panic!("the staged sidecar started, so the mount must be LIVE");
    };

    std::fs::write(&sentinel, b"").expect("engage the sentinel the mount was handed");
    live.client.submit(&vike_model::OrderRequest {
        client_order_id: "wired-halt".into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        ts: 5,
        ..Default::default()
    });
    // The refusal is synthesized synchronously inside `submit`, so it is already on the lane.
    let mut saw = None;
    while let Ok(ingest) = rx.try_recv() {
        if let vike_exec::Ingest::Event(vike_model::events::Event::OrderRejected(r)) = ingest {
            saw = Some(r);
            break;
        }
    }
    let rejected = saw.expect("an opening order under the handed sentinel must be refused");
    assert_eq!(rejected.client_order_id, "wired-halt");
    assert_eq!(rejected.reason, vike_bridge_core::halt::HALT_REJECT_REASON);

    // Reaps the child.
    drop(live);
}
