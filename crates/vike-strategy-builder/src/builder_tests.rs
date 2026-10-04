use super::*;

#[test]
fn bind_addr_is_always_loopback_regardless_of_port() {
    for port in [0u16, 1, 7881, 65535] {
        let addr = bind_addr(port);
        assert!(addr.ip().is_loopback(), "port {port} produced a non-loopback bind: {addr}");
        assert_eq!(addr.port(), port);
    }
}

/// Decision 2, exercised directly: a `Scope::Read` (or `Account`) presentation is refused
/// even with a byte-identical, correctly-signed mac, because [`keys_from_vars`] never fills
/// those slots and [`verify_auth`] checks the SCOPE before it ever reaches `key_for`.
#[test]
fn only_write_scope_is_ever_accepted() {
    let key = b"a-real-key";
    let keys = NodeKeys::new(Vec::new(), key.to_vec());
    let nonce = [1u8; 32];
    let read_mac = auth::sign(DOMAIN, key, &nonce, PROTO_VERSION, Scope::Read);
    assert!(!verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Read, &read_mac));
    let write_mac = auth::sign(DOMAIN, key, &nonce, PROTO_VERSION, Scope::Write);
    assert!(verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Write, &write_mac));
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A reader that must never be reached — the env form, and every refusal decided from the
/// variables alone, open no file.
fn no_read(p: &Path) -> io::Result<Vec<u8>> {
    panic!("the key file {} was read when no file form was chosen", p.display())
}

/// Exactly one form is accepted; both, or neither, is a refusal — and a BLANK value is unset,
/// the rule `keys_from_vars` has always applied, so `KEY=` beside a file form is not "both".
#[test]
fn exactly_one_key_form_is_accepted() {
    assert_eq!(key_source(&vars(&[])), Err(KeyRefusal::Neither));
    assert_eq!(key_source(&vars(&[(BUILDER_KEY_ENV, "  ")])), Err(KeyRefusal::Neither));
    assert_eq!(key_source(&vars(&[(BUILDER_KEY_ENV, "k")])), Ok(KeySource::Env));
    assert_eq!(
        key_source(&vars(&[(BUILDER_KEY_FILE_ENV, " /run/c/builder-key ")])),
        Ok(KeySource::File(PathBuf::from("/run/c/builder-key")))
    );
    assert_eq!(
        key_source(&vars(&[(BUILDER_KEY_ENV, "k"), (BUILDER_KEY_FILE_ENV, "/f")])),
        Err(KeyRefusal::Both)
    );
    assert_eq!(
        key_source(&vars(&[(BUILDER_KEY_ENV, ""), (BUILDER_KEY_FILE_ENV, "/f")])),
        Ok(KeySource::File(PathBuf::from("/f")))
    );
    assert!(resolve_keys(&vars(&[(BUILDER_KEY_ENV, "k")]), &no_read).is_ok());
    assert_eq!(
        resolve_keys(&vars(&[(BUILDER_KEY_ENV, "k"), (BUILDER_KEY_FILE_ENV, "/f")]), &no_read)
            .err(),
        Some(KeyRefusal::Both)
    );
}

/// The file form and the value form yield the SAME key for the same secret — a trailing newline
/// (what `echo` and every editor write) is trimmed exactly as `$(cat …)` would trim it.
#[test]
fn the_file_form_reads_the_same_key_the_value_form_does() {
    let secret = "a-key-file-test-value";
    let from_value = resolve_keys(&vars(&[(BUILDER_KEY_ENV, secret)]), &no_read).unwrap();
    for bytes in [format!("{secret}\n"), format!("{secret}\r\n"), format!("  {secret}")] {
        let read = |_: &Path| -> io::Result<Vec<u8>> { Ok(bytes.clone().into_bytes()) };
        let from_file =
            resolve_keys(&vars(&[(BUILDER_KEY_FILE_ENV, "/any")]), &read).expect("a key");
        assert_eq!(
            from_file.key_for(required_scope()),
            from_value.key_for(required_scope()),
            "{bytes:?} read as a different key from the value form"
        );
    }
}

/// Every file refusal names the PATH and a reason, and never a byte of what the file holds.
#[test]
fn a_bad_key_file_is_refused_by_path_and_reason_without_its_contents() {
    let f = vars(&[(BUILDER_KEY_FILE_ENV, "/the/key/file")]);
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (b"\n  \n".to_vec(), "is empty"),
        (vec![0xff, 0xfe, b'x'], "not UTF-8"),
        (vec![b'k'; MAX_KEY_FILE_LEN + 1], "larger than any key"),
    ];
    for (bytes, want) in cases {
        let read = |_: &Path| -> io::Result<Vec<u8>> { Ok(bytes.clone()) };
        let refusal = resolve_keys(&f, &read).expect_err("refused").to_string();
        assert!(refusal.contains("/the/key/file"), "the path is not named: {refusal}");
        assert!(refusal.contains(want), "expected `{want}` in: {refusal}");
        assert!(!refusal.contains("kkkk"), "the contents leaked into the refusal");
    }
    let missing =
        |_: &Path| -> io::Result<Vec<u8>> { Err(io::Error::from(io::ErrorKind::NotFound)) };
    let refusal = resolve_keys(&f, &missing).expect_err("refused").to_string();
    assert!(refusal.contains("could not be read"), "{refusal}");
    // The refusal ALWAYS names the variable, so an operator can grep a journal for it.
    assert!(refusal.contains(BUILDER_KEY_FILE_ENV), "{refusal}");
    assert!(KeyRefusal::Neither.to_string().contains(BUILDER_KEY_ENV));
    let both = KeyRefusal::Both.to_string();
    assert!(both.contains(BUILDER_KEY_ENV) && both.contains(BUILDER_KEY_FILE_ENV), "{both}");
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

/// The accepted socket carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`), as
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
        let keys = NodeKeys::new(Vec::new(), b"builder-key".to_vec());
        let pin = render::CargoHomePin::disabled();
        handle_connection(accepted, &keys, out.path(), 1, Path::new("."), "cargo", &[], &pin);
    });
    write_frame(&mut client, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    match vike_node_proto::frame::read_frame::<_, Response>(&mut client).expect("welcome") {
        Response::Welcome { .. } => {}
        other => panic!("expected Welcome, got {other:?}"),
    }
    assert!(clone.nodelay().expect("read TCP_NODELAY"), "the accepted socket has Nagle on");
    drop(client); // the server's Auth read sees EOF and the connection thread ends
    served.join().expect("the connection thread ends when the client hangs up");
}
