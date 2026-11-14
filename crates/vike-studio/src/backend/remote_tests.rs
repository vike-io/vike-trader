use super::*;
use std::assert_matches;
use vike_data::TimestampRange;

#[test]
fn backend_defaults_to_local() {
    // The default is the DAEMON now; `Local` is gone. See `impl Default for Backend`.
    assert_eq!(
        Backend::default(),
        Backend::Remote { addr: DEFAULT_BACKTESTHUB_ADDRESS.to_string() }
    );
}

fn creds(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// ⚠ **Studio's backtest key is WRITE-ONLY, trimmed, and read from its own row alone.** The
/// Read slot is empty by construction, so the key cannot be presented as an observe key even
/// inside this module; the value is trimmed the way the daemon trims its own copy, or a
/// trailing newline from a pipe would earn `bad mac`.
#[test]
fn the_backtest_key_is_write_only_and_trimmed() {
    let key =
        backtest_key_from_credentials(&creds(&[(BACKTEST_KEY_NAME, "  placeholder-value \n")]))
            .expect("a filed row resolves");
    assert!(
        key.write_only.key_for(Scope::Read).is_empty(),
        "the Read half of Studio's backtest key must be EMPTY — it is a Control key for one dial"
    );
    assert_eq!(key.write_only.key_for(Scope::Write), b"placeholder-value");
}

/// A blank value is no key, and an absent one is none — the ordinary unconfigured state.
#[test]
fn a_blank_or_absent_backtest_key_is_none() {
    assert!(backtest_key_from_credentials(&creds(&[])).is_none());
    for blank in ["", "   ", "\t\n"] {
        assert!(
            backtest_key_from_credentials(&creds(&[(BACKTEST_KEY_NAME, blank)])).is_none(),
            "{blank:?} was treated as a key"
        );
    }
}

/// ⚠ **The PLATFORM name is ignored, whatever it holds.** The whole reach limit rests on this
/// function reading one name that nothing else reads; a fallback to
/// `VIKE_DATAHUB_CONTROL_KEY` would make every map that carries the platform pair — the tradehub's
/// own credential map is one — a Studio holding a Write key it was never handed.
#[test]
fn the_platform_control_key_name_is_never_read_for_studio() {
    let only_platform =
        creds(&[(vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV, "placeholder-value")]);
    assert!(
        backtest_key_from_credentials(&only_platform).is_none(),
        "Studio's backtest key resolved from the PLATFORM control key name"
    );
    assert_ne!(BACKTEST_KEY_NAME, vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV);
    assert_ne!(BACKTEST_KEY_NAME, vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV);
}

/// Its `Debug` carries presence only — so a `{:?}` of the state that holds it, or a panic
/// message formatting it, cannot print the key.
#[test]
fn the_backtest_key_debug_is_redacted() {
    let key = backtest_key_from_credentials(&creds(&[(BACKTEST_KEY_NAME, "placeholder-value")]))
        .expect("resolves");
    let shown = format!("{key:?} {:?}", Some(key.clone()));
    assert!(!shown.contains("placeholder-value"), "the key reached a Debug rendering: {shown}");
    assert!(shown.contains("redacted"), "{shown}");
}

/// The credential map the desktop hands Studio, read from a real settings database exactly as
/// `crates/vike-desktop/src/main.rs`'s `workspace_credentials_checked` reads it: the store opened
/// over the settings directory it is told (the boot's answer, there; the scratch one, here), then
/// its whole map.
fn credential_map_of(rows: &[(&str, &str)]) -> (tempfile::TempDir, HashMap<String, String>) {
    let dir = tempfile::tempdir().expect("a scratch settings directory");
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("create the store");
    if !rows.is_empty() {
        let rows: Vec<(String, String)> =
            rows.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        vike_secrets::save_credentials_to_store_unjournalled(
            dir.path(),
            vike_secrets::StoreTable::Credential,
            &rows,
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("file the rows");
    }
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir.path())),
        vike_secrets::CredentialScope::Full,
    );
    let map = store.read_all().expect("the whole map reads").secrets.into_map();
    (dir, map)
}

/// ⚠ **THE RULING (owner, 2026-10-31, "all in the SQLite, not files"): Studio's backtest key is a
/// ROW of the PC's settings database, filed the way `vike-cli secrets set` files a deployment
/// name, and the credential map read back from that database resolves it.** Not a variable, not a
/// file: the resolver takes the map and nothing else. A store holding no such row — an empty one,
/// or one with only another key's row (the builder's, the platform control key's) — resolves no key.
#[test]
fn the_backtest_key_is_the_row_the_settings_database_holds() {
    let (_dir, map) = credential_map_of(&[(BACKTEST_KEY_NAME, "  a-stored-backtest-key \t")]);
    let key = backtest_key_from_credentials(&map).expect("the filed row resolves");
    assert_eq!(key.write_only.key_for(Scope::Write), b"a-stored-backtest-key");
    assert!(key.write_only.key_for(Scope::Read).is_empty());

    let (_empty_dir, empty) = credential_map_of(&[]);
    assert!(backtest_key_from_credentials(&empty).is_none(), "an empty store held a key");
    let (_other_dir, other) = credential_map_of(&[
        (vike_strategy_builder::builder::BUILDER_KEY_NAME, "the-builders-key"),
        (vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV, "the-platform-key"),
    ]);
    assert!(
        backtest_key_from_credentials(&other).is_none(),
        "Studio's backtest key resolved from another key's row"
    );
}

/// The refusal a keyless Studio shows against a KEYED backtesthub names the ROW, the launcher and
/// the one command that files it by hand, keeps the phrase every earlier banner used, says there
/// is no variable or file for it, and does NOT send the operator to the platform key names.
#[test]
fn a_keyless_studio_is_told_where_its_backtest_key_comes_from() {
    let msg = no_backtest_key_message("127.0.0.1:7880");
    assert!(msg.contains("REQUIRES authentication"), "{msg}");
    assert!(msg.contains(BACKTEST_KEY_NAME), "names the row: {msg}");
    assert!(msg.contains("just studio"), "names the launcher: {msg}");
    assert!(
        msg.contains(&format!("vike-cli secrets set {BACKTEST_KEY_NAME}")),
        "names the one command: {msg}"
    );
    assert!(msg.contains("never on the command line"), "says how the value travels: {msg}");
    assert!(
        msg.contains("no key file and no environment variable"),
        "must say there is no second source: {msg}"
    );
    assert!(
        !msg.contains(vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV),
        "must not send the operator to the platform key name: {msg}"
    );
}

/// ⚠ **STUDIO'S OWN RUN PATH, holding a backtest key, pointed at a DATA-plane server, sends
/// nothing after its `Hello`.** This is the production call `StudioState::start_run` makes, not
/// the client crate's constructor on its own — so it also fails if [`connect`] ever signs the
/// key through the UNGUARDED `connect_authed`, which the client crate's own tests cannot see.
#[test]
fn a_run_holding_the_backtest_key_sends_nothing_to_a_data_plane_server() {
    use std::io::Read;
    use vike_datahub_client::{
        PROTO_VERSION,
        proto::{Request, Response, read_frame, write_frame},
    };
    use vike_hub_protocol::auth::FEATURE_AUTH;
    use vike_hub_protocol::plane::DATA_PLANE_SENTINEL;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr").to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        assert_matches!(read_frame::<_, Request>(&mut stream), Ok(Request::Hello { .. }));
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec![DATA_PLANE_SENTINEL.to_string(), FEATURE_AUTH.to_string()],
                nonce: Some([9u8; 32]),
            },
        )
        .expect("welcome");
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).expect("timeout");
        let mut after_hello = Vec::new();
        let _ = stream.read_to_end(&mut after_hello);
        after_hello
    });

    let key = backtest_key_from_credentials(&creds(&[(BACKTEST_KEY_NAME, "placeholder-value")]))
        .expect("resolves");
    let slice = DataSlice::bars(
        "binance",
        "BTCUSDT",
        "1h",
        TimestampRange { start: Some(0), end: Some(1) },
    );
    let rhai = StrategySpec::rhai("fn on_bar(){}");
    let err = run_slice_remote(&addr, Some(&key), &rhai, &slice, None)
        .expect_err("a DATA-plane server must be refused for Studio's backtest key");
    assert!(err.to_string().contains("DATA daemon"), "{err}");
    let after_hello = server.join().expect("server thread");
    assert!(
        after_hello.is_empty(),
        "Studio's Run wrote {} byte(s) after its Hello to the DATA daemon while holding a \
             Control key",
        after_hello.len()
    );
}

// The encode/decode tests that stood here (`to_wire_spec`, `to_wire_slice`, the result, sweep,
// walk-forward and error-line decoders) moved WITH their functions, beside the server halves they
// invert: `crates/vike-studio-core/src/wire_run_tests.rs` and
// `crates/vike-backtesthub-client/src/wire_result_tests.rs`, each now a round trip.

/// "Compare all" keeps every row, in order, a failed run as its sentence rather than dropped.
#[test]
fn compare_each_keeps_a_failed_row_as_its_sentence() {
    let rows = vec![
        ("ok".to_string(), StrategySpec::rhai("fn on_bar() {}")),
        ("broken".to_string(), StrategySpec::rhai("fn on_bar( {")),
    ];
    let out = compare_each(rows, |spec| match spec {
        StrategySpec::Rhai(src) if src.contains("( {") => {
            Err(RunError::Compile("unexpected `{`".to_string()))
        }
        _ => Ok(BacktestResult::default()),
    });
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].0, "ok");
    assert!(out[0].1.is_ok());
    assert_eq!(out[1].0, "broken");
    assert!(out[1].1.as_ref().is_err_and(|e| e.contains("unexpected")), "{:?}", out[1].1);
}

// ⚠ `remote_store_tick_refusal` STOOD HERE AND IS DELETED, not ported.
//
// It walked the split-plane tick rule exhaustively, and the ONE refused cell it asserted
// was (remote store, tick slice, LOCAL backend) - a refusal that exists only because a
// local path was the thing being refused IN FAVOUR OF. `Backend::Local` is gone
// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`), so the
// cell is unreachable and a test asserting it would pass for a reason that no longer
// exists - which this tree treats as worse than no test at all.
