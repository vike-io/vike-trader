use super::*;

fn line(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// The four plain settings are ARGUMENTS (decision 0111): defaults when absent, both spellings of a
/// value accepted, and every malformed line refused by name rather than defaulted in silence.
#[test]
fn the_command_line_carries_the_four_settings_and_refuses_a_malformed_one() {
    let Ok(none) = read_args(&[]) else { panic!("an empty line is a run") };
    assert_eq!(
        none,
        BuilderArgs {
            port: DEFAULT_PORT,
            out_dir: PathBuf::from(DEFAULT_OUT_DIR),
            retain: DEFAULT_RETAIN_PER_STRATEGY,
            workspace_root: None,
        }
    );
    let Ok(all) = read_args(&line(&[
        "--port",
        "0",
        "--out-dir=/srv/p",
        "--retain",
        "9",
        "--workspace-root",
        "/src/tree",
    ])) else {
        panic!("a full line is a run")
    };
    assert_eq!(
        all,
        BuilderArgs {
            port: 0,
            out_dir: PathBuf::from("/srv/p"),
            retain: 9,
            workspace_root: Some(PathBuf::from("/src/tree")),
        }
    );
    let help = read_args(&line(&["--help"])).expect_err("--help is answered, not run");
    assert_eq!((help.code, help.to_stdout), (0, true), "{help:?}");
    assert!(help.text.contains("Usage:") && help.text.contains("--workspace-root"), "{help:?}");
    let version = read_args(&line(&["-V"])).expect_err("--version is answered, not run");
    assert_eq!((version.code, version.to_stdout), (0, true), "{version:?}");
    for (bad, says) in [
        (&["--port", "x"][..], "--port: not a port number"),
        (&["--port", "65536"][..], "--port: not a port number"),
        (&["--retain", "-1"][..], "--retain: not a count"),
        (&["--out-dir"][..], "--out-dir"),
        (&["--out-dir", "--port", "0"][..], "--out-dir"),
        (&["--workspace-root="][..], "needs a non-empty value"),
        (&["--workspace-root", "  "][..], "needs a non-empty value"),
        (&["--port="][..], "needs a non-empty value"),
        (&["--prot", "1"][..], "--prot"),
        (&["--port", "1", "--port", "2"][..], "--port"),
        (&["stray"][..], "unexpected argument"),
    ] {
        let err = read_args(&line(bad)).expect_err("a malformed line must be refused");
        assert_eq!(err.code, 2, "{bad:?}: a usage error exits 2: {err:?}");
        assert!(!err.to_stdout, "{bad:?}: a usage error is stderr: {err:?}");
        assert!(err.text.contains(says), "{bad:?}: {}", err.text);
    }
}

/// Every command line the builder ACCEPTS today, pinned as POSITIVE cases (decision 0155:
/// "everything an operator could WRITE and have accepted before stays accepted"). The case table
/// is the pin; only `seen` adapts when the reader changes. `None` there is a REFUSED line, and
/// `Err(true)` is `-h`/`--help`.
#[test]
fn every_accepted_command_line_stays_accepted() {
    let seen = |args: &[&str]| -> Option<Result<BuilderArgs, bool>> {
        match read_args(&line(args)) {
            Ok(a) => Some(Ok(a)),
            // `-h`/`--help` are answered by the reader (exit 0, stdout), not run.
            Err(exit) if exit.code == 0 => Some(Err(true)),
            Err(_) => None,
        }
    };
    let run = |port: u16, out: &str, retain: usize, root: Option<&str>| {
        Some(Ok(BuilderArgs {
            port,
            out_dir: PathBuf::from(out),
            retain,
            workspace_root: root.map(PathBuf::from),
        }))
    };
    let cases = [
        // deploy/vike-strategy-builder.service.
        (
            "the shipped unit",
            vec![
                "--port",
                "7881",
                "--out-dir",
                "/srv/vike-<unit>/user_data/plugins",
                "--retain",
                "5",
                "--workspace-root",
                "/checkout/vike-trader-rust",
            ],
            run(
                7881,
                "/srv/vike-<unit>/user_data/plugins",
                5,
                Some("/checkout/vike-trader-rust"),
            ),
        ),
        // tests/refusal_reaches_stderr.rs and tests/child_env.rs: a free port, in a scratch dir.
        (
            "the smoke tests",
            vec!["--workspace-root", "/w", "--port", "0", "--out-dir", "/o"],
            run(0, "/o", DEFAULT_RETAIN_PER_STRATEGY, Some("/w")),
        ),
        ("no flags", vec![], run(DEFAULT_PORT, DEFAULT_OUT_DIR, DEFAULT_RETAIN_PER_STRATEGY, None)),
        (
            "the root alone",
            vec!["--workspace-root", "/src/tree"],
            run(DEFAULT_PORT, DEFAULT_OUT_DIR, DEFAULT_RETAIN_PER_STRATEGY, Some("/src/tree")),
        ),
        (
            "every flag inline",
            vec!["--port=9", "--out-dir=/srv/p", "--retain=3", "--workspace-root=/r"],
            run(9, "/srv/p", 3, Some("/r")),
        ),
        (
            "flags in any order",
            vec!["--retain", "1", "--workspace-root", "/r", "--port", "2"],
            run(2, DEFAULT_OUT_DIR, 1, Some("/r")),
        ),
        (
            "a padded number and root",
            vec!["--port", " 7881 ", "--retain", " 4", "--workspace-root", " /r "],
            run(7881, DEFAULT_OUT_DIR, 4, Some("/r")),
        ),
        ("a zero retention", vec!["--retain", "0"], run(DEFAULT_PORT, DEFAULT_OUT_DIR, 0, None)),
        ("-h", vec!["-h"], Some(Err(true))),
        ("--help", vec!["--help"], Some(Err(true))),
        ("help after a flag", vec!["--port", "1", "--help"], Some(Err(true))),
    ];
    for (what, args, want) in cases {
        assert_eq!(seen(&args), want, "{what}: {args:?}");
    }
}

/// A leftover retired variable is a startup refusal naming the argument that replaced it — never
/// its value, and a blank one is unset. Names composed so the registry's literal sweep reads no
/// fixture here.
#[test]
fn a_retired_variable_refuses_naming_its_argument() {
    assert_eq!(refuse_retired_variables(&HashMap::new()), Ok(()));
    let root = concat!("VIKE", "_STRATEGY_BUILDER_WORKSPACE_ROOT");
    let blank = HashMap::from([(root.to_string(), " ".to_string())]);
    assert_eq!(refuse_retired_variables(&blank), Ok(()), "blank is unset");
    let set = HashMap::from([(root.to_string(), "/secret/tree".to_string())]);
    let why = refuse_retired_variables(&set).expect_err("a set variable refuses");
    assert!(why.contains(root) && why.contains("--workspace-root DIR"), "{why}");
    assert!(!why.contains("/secret/tree"), "never the value: {why}");
}

#[test]
fn bind_addr_is_always_loopback_regardless_of_port() {
    for port in [0u16, 1, 7881, 65535] {
        let addr = bind_addr(port);
        assert!(addr.ip().is_loopback(), "port {port} produced a non-loopback bind: {addr}");
        assert_eq!(addr.port(), port);
    }
}

/// Decision 2, exercised directly: a `Scope::Read` (or `Account`) presentation is refused
/// even with a byte-identical, correctly-signed mac, because the key readers never fill
/// those slots and [`verify_auth`] checks the SCOPE before it ever reaches `key_for`.
#[test]
fn only_write_scope_is_ever_accepted() {
    let key = b"a-real-key";
    let keys = HubKeys::new(Vec::new(), key.to_vec());
    let nonce = [1u8; 32];
    let read_mac = auth::sign(DOMAIN, key, &nonce, PROTO_VERSION, Scope::Read);
    assert!(!verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Read, &read_mac));
    let write_mac = auth::sign(DOMAIN, key, &nonce, PROTO_VERSION, Scope::Write);
    assert!(verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Write, &write_mac));
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A scratch SETTINGS DIRECTORY holding a real settings database, with `rows` filed in it as
/// deployment credentials — exactly how `vike-cli secrets set NAME` files them. `rows` empty is an
/// empty (but present) store.
fn settings_dir_holding(rows: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a scratch settings directory");
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("create the store");
    if !rows.is_empty() {
        let rows: Vec<(String, String)> =
            rows.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        vike_secrets::save_credentials_to_store_unjournalled(
            dir.path(),
            vike_secrets::Table::Credential,
            &rows,
            vike_secrets::UpsertIntent::Add,
        )
        .expect("file the rows");
    }
    dir
}

/// The environment map a root hands `resolve_keys`: only the store's LOCATION (the loader input
/// every no-boot root takes).
fn env_locating(dir: &Path) -> HashMap<String, String> {
    vars(&[(vike_secrets::SETTINGS_DIR_ENV, dir.to_str().expect("a UTF-8 path"))])
}

/// The retired FILE variable's name, assembled (the registry reads an env-shaped literal as a read).
fn retired_file_variable() -> String {
    format!("{BUILDER_KEY_NAME}_FILE")
}

/// ⚠ THE RULING: the service's key is read from the SETTINGS DATABASE — a row filed with
/// `vike-cli secrets set`, whitespace around it trimmed as a pipe's trailing newline would need.
#[test]
fn the_key_is_read_from_the_settings_database() {
    // Whitespace around the value (a ONE-line value: the store refuses a newline in a credential).
    let dir = settings_dir_holding(&[(BUILDER_KEY_NAME, "  a-stored-builder-key \t")]);
    let keys = resolve_keys(&env_locating(dir.path())).expect("the stored key");
    assert_eq!(keys.key_for(required_scope()), b"a-stored-builder-key");
    // Only the write slot is ever filled (Decision 2): the store row is no observe key.
    assert!(!keys.has(Scope::Read));
}

/// ⚠ NO FILE AND NO VARIABLE IS A SECOND SOURCE — and neither is a fallback. A key sitting in the
/// old homes (a variable carrying it, a file the old `_FILE` variable named) is not found when the
/// store holds none, and when the store holds one it wins and the others are not even consulted.
#[test]
fn no_file_and_no_variable_is_a_key_source() {
    let scratch = tempfile::tempdir().expect("scratch");
    let old_file = scratch.path().join("strategy-builder.key");
    std::fs::write(&old_file, "key-in-the-retired-file\n").expect("plant the retired file");
    // The old homes, both planted: the variable carrying a key, and the variable naming the file.
    let with_old_homes = |settings_dir: &Path| {
        let mut env = env_locating(settings_dir);
        env.insert(BUILDER_KEY_NAME.to_string(), "key-in-the-retired-variable".to_string());
        env.insert(retired_file_variable(), old_file.to_str().expect("a UTF-8 path").to_string());
        env
    };

    // The store holds nothing: the old homes are NOT a fallback.
    let empty = settings_dir_holding(&[]);
    let refusal = resolve_keys(&with_old_homes(empty.path())).expect_err("no fallback");
    assert!(matches!(refusal, KeyRefusal::Missing { .. }), "{refusal:?}");

    // The store holds one: it is the one used.
    let held = settings_dir_holding(&[(BUILDER_KEY_NAME, "key-in-the-store")]);
    let keys = resolve_keys(&with_old_homes(held.path())).expect("the stored key");
    assert_eq!(keys.key_for(required_scope()), b"key-in-the-store");
}

/// ⚠ A MISSING KEY IS REFUSED BY NAME, with the ONE command that sets it and how the value travels
/// (stdin or `--from-env`, never the command line) — for a store with no row, a BLANK row, and a
/// box with no store at all (which also names the verb that creates one).
#[test]
fn a_missing_key_is_refused_by_name_with_the_one_command_that_sets_it() {
    let command = format!("vike-cli secrets set {BUILDER_KEY_NAME}");
    for (what, dir) in [
        ("an empty store", settings_dir_holding(&[])),
        ("a blank row", settings_dir_holding(&[(BUILDER_KEY_NAME, "   ")])),
        ("another row only", settings_dir_holding(&[("SOME_OTHER_ROW", "not-the-key")])),
        // A directory that holds NO settings database.
        ("no store at all", tempfile::tempdir().expect("an empty directory")),
    ] {
        let refusal = resolve_keys(&env_locating(dir.path())).expect_err(what);
        assert!(matches!(refusal, KeyRefusal::Missing { .. }), "{what}: {refusal:?}");
        let text = refusal.to_string();
        assert!(text.contains(&command), "{what}: the command is not named: {text}");
        assert!(text.contains(BUILDER_KEY_NAME), "{what}: {text}");
        assert!(text.contains("--from-env") && text.contains("stdin"), "{what}: {text}");
        assert!(text.contains("never on the command line"), "{what}: {text}");
        assert!(!text.contains("not-the-key"), "{what}: another row's value leaked: {text}");
        // A box with no store is also told the verb that makes one.
        assert_eq!(
            text.contains("vike-cli secrets init"),
            what == "no store at all",
            "{what}: {text}"
        );
    }
}

/// ⚠ A store that EXISTS and will not open is an ERROR, never "no key": a permissions fault or a
/// damaged file wearing the not-configured answer is the failure this variant keeps apart. And the
/// refusal names the path and the reason — never a byte of what the file holds.
#[test]
fn an_unreadable_store_is_an_error_and_not_a_missing_key() {
    let dir = tempfile::tempdir().expect("scratch");
    let db = vike_secrets::db_path_in(dir.path());
    std::fs::create_dir_all(db.parent().expect("a parent")).expect("the db directory");
    std::fs::write(&db, "this-is-not-a-database-planted-bytes-7f3a").expect("plant a bad store");
    let refusal = resolve_keys(&env_locating(dir.path())).expect_err("unreadable");
    assert!(matches!(refusal, KeyRefusal::Unreadable(_)), "{refusal:?}");
    let text = refusal.to_string();
    assert!(text.contains("could not be read"), "{text}");
    assert!(
        !text.contains("planted-bytes-7f3a"),
        "the file's contents reached the refusal: {text}"
    );
}

/// ⚠ THE VALUE IS NEVER PRINTED: not by `Debug` of the key, not by either refusal, not by `Debug`
/// of the refusal. (The strongest form is the other tests above, which hold the refusal text to the
/// name and the command; this one plants a recognisable secret in the store and sweeps every
/// rendering a caller can get.)
#[test]
fn the_value_never_reaches_debug_or_a_refusal() {
    let secret = "recognisable-builder-secret-4c1e9d";
    let dir = settings_dir_holding(&[(BUILDER_KEY_NAME, secret)]);
    let keys = resolve_keys(&env_locating(dir.path())).expect("the key");
    assert!(!format!("{keys:?}").contains(secret), "HubKeys' Debug prints the key");
    // The map reader the desktop uses is the same type with the same Debug.
    let creds = vars(&[(BUILDER_KEY_NAME, secret)]);
    let from_map = keys_from_credentials(&creds).expect("the key from the map");
    assert!(!format!("{from_map:?}").contains(secret));
    // Every refusal rendering, over a store that holds the secret under ANOTHER name.
    let other = settings_dir_holding(&[("NOT_THE_KEY_ROW", secret)]);
    let refusal = resolve_keys(&env_locating(other.path())).expect_err("a missing key");
    assert!(!refusal.to_string().contains(secret) && !format!("{refusal:?}").contains(secret));
}

/// The CLIENT's reader is the credential map lookup the desktop's Studio uses: a stored value with a
/// trailing newline is the same key, a blank or absent one is none, and a map carrying only the
/// retired variable names yields none (they are not read by anything).
#[test]
fn the_client_reads_the_credential_map_and_nothing_else() {
    let key = keys_from_credentials(&vars(&[(BUILDER_KEY_NAME, " client-key \n")])).expect("a key");
    assert_eq!(key.key_for(required_scope()), b"client-key");
    assert!(keys_from_credentials(&vars(&[])).is_none());
    assert!(keys_from_credentials(&vars(&[(BUILDER_KEY_NAME, "  ")])).is_none());
    let file_variable = retired_file_variable();
    assert!(keys_from_credentials(&vars(&[(file_variable.as_str(), "/some/key/file")])).is_none());
}

/// The line never carries rustc's diagnostics, because rustc QUOTES the source it is
/// complaining about — the one variant whose text would put a user's code in the journal.
#[test]
fn a_compile_refusal_logs_that_it_failed_and_not_what_the_source_said() {
    let quoted_source = "fn proprietary_edge_7f3a() { let signal = secret_weighting( }";
    let err = render::BuildError::Compile(format!(
        "error: expected expression\n --> src/lib.rs:12:48\n   |\n12 | {quoted_source}\n"
    ));
    let line = build_refusal_line("mean_revert", &"a".repeat(64), &err);
    assert!(line.contains("REFUSED [compile-error]"), "{line}");
    assert!(!line.contains("proprietary_edge_7f3a"), "the source leaked into the log: {line}");
    assert!(!line.contains("secret_weighting"), "the source leaked into the log: {line}");
}

/// The version-skew refusal names BOTH stamps — the operator's whole remedy — on ONE line.
#[test]
fn a_version_refusal_names_both_stamps_on_one_line() {
    let err = render::BuildError::SourceVersionMismatch(
        "this binary was built from git commit `1daa96c31`, but the workspace_root at\n\
             /x/SOURCE_GIT_SHA is stamped `cf5acb41ce`"
            .to_string(),
    );
    let line = build_refusal_line("s", &"b".repeat(64), &err);
    assert!(line.contains("REFUSED [source-version-mismatch]"), "{line}");
    assert!(line.contains("1daa96c31") && line.contains("cf5acb41ce"), "{line}");
    assert!(!line.contains('\n'), "a refusal must be ONE journal line: {line:?}");
}

/// The name is the CALLER's string: a newline in it must not forge a second journal line, and
/// a megabyte of it must not become a megabyte of journal.
#[test]
fn a_hostile_strategy_name_cannot_forge_or_flood_a_journal_line() {
    let err = render::BuildError::Toolchain("could not run `cargo build`".to_string());
    let forged = "x\nvike-strategy-builder: listening on 127.0.0.1:1";
    let line = build_refusal_line(forged, &"c".repeat(64), &err);
    assert!(!line.contains('\n'), "a newline in the name split the line: {line:?}");
    let long = "n".repeat(10_000);
    let line = build_refusal_line(&long, &"d".repeat(64), &err);
    assert!(line.len() < 400, "the name was not capped: {} bytes", line.len());
}

#[test]
fn artifact_strategy_name_parses_the_fixed_shape_and_nothing_else() {
    assert_eq!(
        artifact_strategy_name(Path::new(&format!("strat_a-{}.so", "0".repeat(64)))),
        Some("strat_a".to_string())
    );
    // A name that is a PREFIX of another must not be confused with it by this parser: the
    // suffix shape anchors on the trailing 65 bytes, not on any prefix test.
    assert_eq!(
        artifact_strategy_name(Path::new(&format!("strat-{}.so", "1".repeat(64)))),
        Some("strat".to_string())
    );
    assert_eq!(
        artifact_strategy_name(Path::new(&format!("strat_ab-{}.so", "1".repeat(64)))),
        Some("strat_ab".to_string())
    );
    assert_eq!(artifact_strategy_name(Path::new("not-an-artifact.txt")), None);
    assert_eq!(artifact_strategy_name(Path::new("short-deadbeef.so")), None);
    // Not-quite-hex in the supposed hash region: rejected rather than silently accepted.
    let bad_hash = format!("name-{}g.so", "0".repeat(63));
    assert_eq!(artifact_strategy_name(Path::new(&bad_hash)), None);
}

/// The accepted socket carries `TCP_NODELAY` (`vike_hub_proto::frame::configure_hub_stream`), as
/// every node server's does. Read off the socket itself rather than timed, so it holds on any OS: a
/// clone of the accepted stream is the same socket, and once the `Welcome` answering a `Hello` has
/// arrived the server is past the line that arms it.
#[test]
fn the_accepted_socket_has_nagle_off() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let mut client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (accepted, _) = listener.accept().expect("accept");
    let clone = accepted.try_clone().expect("clone the accepted socket");
    assert!(!clone.nodelay().expect("read TCP_NODELAY"), "guard: a fresh socket has Nagle on");
    let served = thread::spawn(move || {
        let out = tempfile::tempdir().expect("out dir");
        let keys = HubKeys::new(Vec::new(), b"builder-key".to_vec());
        let pin = render::CargoHomePin::disabled();
        handle_connection(accepted, &keys, out.path(), 1, Path::new("."), "cargo", &[], &pin);
    });
    write_frame(&mut client, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    match vike_hub_proto::frame::read_frame::<_, Response>(&mut client).expect("welcome") {
        Response::Welcome { .. } => {}
        other => panic!("expected Welcome, got {other:?}"),
    }
    assert!(clone.nodelay().expect("read TCP_NODELAY"), "the accepted socket has Nagle on");
    drop(client); // the server's Auth read sees EOF and the connection thread ends
    served.join().expect("the connection thread ends when the client hangs up");
}
