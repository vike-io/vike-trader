//! **`DukascopyVenueAdapter::start`'s SUCCESS path, through a real child process** — the one path the
//! bridge's unit tests (`src/adapter_tests.rs`) cannot reach, because only a sidecar that really
//! started yields a `DukascopyExecutionClient`, and only such a start parks the venue's answer.
//!
//! The start is the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`), reached the way a
//! deployed box reaches its JVM: a project folder whose `bin/jre/<image>/bin/java` is that program
//! and whose `bin/jforex/` holds the jar. `resolve_dukascopy_tools` therefore finds the program and
//! the jar only under the BIN directory the start is handed, and puts the JVM's home under the STATE
//! directory. No Java and no network.
//!
//! Unix only: the staged `java` is a SYMLINK to the fake bridge rather than a copy, so a scratch
//! filesystem mounted `noexec` cannot turn this red.
#![cfg(unix)]

use vike_bridge_core::venue_adapter::{ExecOutcome, ProcessFacts, VenueAdapter};
use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
use vike_dukascopy::adapter::DukascopyVenueAdapter;
use vike_dukascopy::{BRIDGE_JAR_FILE, JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR, JRE_TOOL_DIR};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_confirmation::ConfirmationRecord;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

/// The folder the fake bridge makes beneath the `user.home` it is handed, and the file it writes
/// that home into — `src/bin/fake_jforex_bridge.rs`'s `report_user_home`.
const JFOREX_FOLDER: &str = "JForex";
const USER_HOME_REPORT: &str = "user-home.txt";

/// A scratch project folder staged the way a deployed box is: `bin/jre/<image>/bin/java` is the
/// fake bridge, `bin/jforex/jforex-bridge.jar` exists (the start's live gate is `is_file`, so an
/// empty file is a jar here), and `settings/state` is the state directory. Returns the folder,
/// which deletes itself on drop, with the two directories `vike-tradehub-core` would hand the start.
fn staged_project() -> (tempfile::TempDir, ProcessFacts) {
    staged_project_with_jar("")
}

/// [`staged_project`] whose staged "jar" holds `fake_mode`: the fake bridge reads its MODE from the first word of the jar
/// file when the JVM command line the start builds carries no application argument
/// (`src/bin/fake_jforex_bridge.rs`'s `mode_of`), which is how a test reaches a mode through the real adapter.
fn staged_project_with_jar(fake_mode: &str) -> (tempfile::TempDir, ProcessFacts) {
    let root = tempfile::tempdir().expect("a scratch project folder");
    let bin = root.path().join("bin");
    let java = bin.join(JRE_TOOL_DIR).join("fake-jre").join("bin").join("java");
    std::fs::create_dir_all(java.parent().expect("the JRE's bin dir")).expect("the JRE tree");
    std::os::unix::fs::symlink(BRIDGE, &java).expect("the fake bridge stands in for java");
    let jar = bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE);
    std::fs::create_dir_all(jar.parent().expect("the jar's dir")).expect("the jar tree");
    std::fs::write(&jar, fake_mode.as_bytes()).expect("plant the jar");
    let state = root.path().join("settings").join("state");
    (root, ProcessFacts { state_dir: Some(state), bin_dir: Some(bin), ..Default::default() })
}

/// The owner's shape of the store: two demo rows with two LABELS, and the books an operator wrote
/// against them — row 7 is `SWISS`, row 8 is `EU`.
fn two_account_store() -> AccountDirectory {
    let row = |id: i64, book: &str, label: &str| Account {
        id,
        venue: "dukascopy".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: Some(label.to_string()),
        venue_account_id: Some(book.to_string()),
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    };
    AccountDirectory::from_rows(
        Accounts::Known(vec![row(7, "3709890", "SWISS"), row(8, "3716974", "EU")]),
        None,
    )
}

/// The `EU` account (row 8) as `vike-tradehub-core` hands it to the start: its own login, by FIELD.
fn eu_account(process: ProcessFacts) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[("LOGIN", "fake-login-2"), ("PASSWORD", "fake-pass-2")]);
    fx.account_id = Some(8);
    fx.accounts = two_account_store();
    fx.account = AccountLabel::parse("EU").expect("a legal label");
    fx.process = process;
    fx
}

/// A start whose sidecar STARTS is live at the DEMO tier, hands the node a reconcile client (the fake announces `recon`),
/// and parks the venue's answer — here the EU row's, addressed by its LABEL and
/// TIER — in the state directory it was HANDED: the one fact about `start` no offline double can
/// witness, since the park follows a real start. (The fake's answer disagrees with the stored
/// book; a disagreement is reported and parked all the same.) The child's own `user.home` report
/// lands under that same state directory, and the start could find its program and its jar only
/// under the handed bin directory: the two directories reach `resolve_dukascopy_tools` each in its
/// own place.
#[test]
fn a_started_sidecar_is_live_and_parks_its_confirmation_in_the_handed_state_directory() {
    let (_project, process) = staged_project();
    let state = process.state_dir.clone().expect("staged");
    let fx = eu_account(process);
    let (tx, _rx) = vike_exec::event_channel(64);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.recon_enabled = true;
    let out = DukascopyVenueAdapter.start(req);

    let ExecOutcome::Live(live) = out.exec else {
        panic!("the staged sidecar started, so the mount must be LIVE");
    };
    assert_eq!(live.bound_tier, AccountTier::Demo, "the sidecar logs into a DEMO server");
    assert!(live.grid.is_none(), "this venue pre-fetches no grid");
    // The reconcile leg (`crates/bridges/dukascopy/src/adapter.rs`'s module doc): with reconciliation on and a jar that
    // announces `recon`, the node is handed a client, and it answers from the sidecar (the fake lists an empty venue).
    let recon = out.recon.expect("reconciliation is on and the jar answers the recon query");
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()));
    assert!(
        out.identity.is_none(),
        "the venue's answer is PARKED here, not handed to vike-tradehub-core"
    );

    // THE PARK, in the HANDED state directory: the EU row's record, beside what the venue answered.
    let parked = vike_secrets::account_confirmation::read(Some(state.as_path())).expect("readable");
    assert_eq!(parked.len(), 1, "one confirmation parked in the handed state dir: {parked:?}");
    let expected = ConfirmationRecord {
        venue: "dukascopy".to_string(),
        key_prefix: String::new(),
        label: Some("EU".to_string()),
        tier: Some(AccountTier::Demo),
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

/// **The reconcile client is handed out only when reconciliation is on AND the jar answers the question.** Both controls
/// of the hand-out, through the real start: with reconciliation off a jar that answers is handed nothing (the client is
/// built lazily, `recon_if_enabled`), and a jar that does not announce `recon` (the release before the query) is handed
/// nothing either, because a client over it could only fail every pass. The start is LIVE either way: the exec half is
/// untouched.
#[test]
fn no_reconcile_client_is_handed_out_with_reconciliation_off_or_for_a_jar_without_the_query() {
    for (jar_mode, recon_enabled, why) in [
        ("", false, "reconciliation is off"),
        ("no-recon", true, "the jar announces no `recon`"),
        ("no-recon", false, "both"),
    ] {
        let (_project, process) = staged_project_with_jar(jar_mode);
        let fx = eu_account(process);
        let (tx, _rx) = vike_exec::event_channel(64);
        let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
        req.recon_enabled = recon_enabled;
        let out = DukascopyVenueAdapter.start(req);
        let ExecOutcome::Live(live) = out.exec else {
            panic!("the staged sidecar started, so the mount must be LIVE ({why})");
        };
        assert!(out.recon.is_none(), "no reconcile client when {why}");
        drop(live);
    }
}

/// **The client the start hands out reads the VENUE through the sidecar it started**: the question travels the real pipe
/// of the real start (the fake lists an empty venue), and a symbol the node mounts gets a FLAT row at once (the start tells
/// the client its mount), which is the row that shows a position the venue closed unheard.
#[test]
fn the_client_a_start_hands_out_answers_from_the_sidecar_it_started_and_states_the_mount_flat() {
    let (_project, process) = staged_project();
    let fx = eu_account(process);
    let (tx, _rx) = vike_exec::event_channel(64);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.recon_enabled = true;
    let out = DukascopyVenueAdapter.start(req);
    let ExecOutcome::Live(live) = out.exec else { panic!("LIVE") };
    let recon = out.recon.expect("handed out");
    let positions = recon.fetch_position_status_reports().expect("the venue's answer");
    assert_eq!(positions.len(), 1, "{positions:?}");
    assert_eq!((positions[0].symbol.as_str(), positions[0].qty), ("EURUSD", 0.0));
    drop(live);
}

/// **The sidecar's `ready` balance rides the REAL start** (`crate::ready_balance`): the start tells the client the
/// symbol the engine is mounted on, and a USD sidecar on a USD-quoted mount puts ONE wallet frame on the lane the
/// start was handed, in dollars, with the figure the fake's `ready` line states (the owner's account 8: 10000.0).
/// Delete the `ledger_mount` the start sets and the lane stays empty.
#[test]
fn a_started_account_puts_its_ready_balance_on_the_lane_it_was_handed() {
    let (_project, process) = staged_project();
    let fx = eu_account(process);
    let (tx, mut rx) = vike_exec::event_channel(64);
    let req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    let ExecOutcome::Live(live) = DukascopyVenueAdapter.start(req).exec else {
        panic!("the staged sidecar started, so the mount must be LIVE");
    };

    // The frame follows the handshake on the reader thread, so poll the lane for it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut frames = Vec::new();
    while frames.is_empty() && std::time::Instant::now() < deadline {
        while let Ok(ingest) = rx.try_recv() {
            if let vike_exec::Ingest::Event(vike_model::events::Event::AccountState(a)) = ingest {
                frames.push(a);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(frames.len(), 1, "one wallet frame for the start: {frames:?}");
    assert_eq!(frames[0].venue, "dukascopy");
    assert_eq!(frames[0].balances, vec![("USD".to_string(), 10_000.0)]);

    // Reaps the child.
    drop(live);
}

/// **A started client watches the HALT sentinel its START was handed** — through the same real
/// sidecar start, so the proof is of the client `start` actually returns and not of a double.
///
/// Decision 0099: this client used to resolve the sentinel itself at its submit seam
/// (`vike_boot::halt::halt_path_from_env`), a process-global read a bridge may not make; the
/// path now arrives as `AdapterInputs::process.halt_path` and the start hands it to the client. The
/// path engaged below is NOT any resolver's answer — no declaration, no walk, no variable names it —
/// so the only way an opening order can be refused is the start having wired it. Delete the
/// `.with_halt_path(..)` call from `start_with` and the order reaches the sidecar instead.
#[test]
fn a_started_client_watches_the_sentinel_its_start_was_handed() {
    let (_project, mut process) = staged_project();
    let state = process.state_dir.clone().expect("staged");
    std::fs::create_dir_all(&state).expect("the state directory");
    let sentinel = state.join("HALT");
    process.halt_path = sentinel.clone();
    let fx = eu_account(process);
    let (tx, mut rx) = vike_exec::event_channel(64);
    let req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    let ExecOutcome::Live(mut live) = DukascopyVenueAdapter.start(req).exec else {
        panic!("the staged sidecar started, so the mount must be LIVE");
    };

    std::fs::write(&sentinel, b"").expect("engage the sentinel the mount was handed");
    live.client.submit(&vike_model::OrderRequest {
        client_order_id: "wired-halt".into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
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
    assert_eq!(rejected.reason, vike_exec::halt::HALT_REJECT_REASON);

    // Reaps the child.
    drop(live);
}
