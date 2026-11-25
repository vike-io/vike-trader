use super::*;

fn parsed(argv: &[&str]) -> Result<Args, Stop> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

#[test]
fn each_subcommand_parses_and_every_flag_defaults_off() {
    for (argv, sub) in [
        (&["mint-missing"][..], Sub::MintMissing),
        (&["status"][..], Sub::Status),
        (&["disconnect"][..], Sub::Disconnect),
        (&["ping"][..], Sub::Ping),
    ] {
        let a = parsed(argv).unwrap();
        assert_eq!(a.sub, sub);
        assert!(!a.no_tunnel && !a.invitation, "{a:?}");
        assert!(!a.json && a.server_project.is_none(), "{a:?}");
        assert_eq!((a.addr, a.port, a.host), (None, None, None));
    }
    let a = parsed(&["connect", "the CI box"]).unwrap();
    assert_eq!((a.sub, a.host.as_deref()), (Sub::Connect, Some("the CI box")));
}

/// **`probe-copy` needs `--into`, and its rules are its own**: the repeatable names and the switch parse, a bare
/// `probe-copy` is refused naming the flag, and a rule on another verb is refused naming `probe-copy`.
#[test]
fn probe_copy_takes_its_directory_and_rules_and_no_other_verb_does() {
    let a = parsed(&[
        "probe-copy",
        "--into",
        "/p/settings",
        "--drop-key",
        "A",
        "--drop-key=B",
        "--paper-accounts",
        "--numeric-key",
        "C",
    ])
    .unwrap();
    assert_eq!(a.sub, Sub::ProbeCopy);
    assert_eq!(a.into.as_deref(), Some("/p/settings"));
    assert_eq!(a.drop_keys, vec!["A".to_string(), "B".to_string()]);
    assert!(a.paper_accounts);
    assert_eq!(a.numeric_keys, vec!["C".to_string()]);
    let err = parsed(&["probe-copy"]).unwrap_err().to_string();
    assert!(err.contains("--into"), "{err}");
    let err = parsed(&["mint-missing", "--paper-accounts"]).unwrap_err().to_string();
    assert!(err.contains("probe-copy"), "{err}");
}

#[test]
fn options_parse_in_both_flag_forms() {
    let a = parsed(&["ping", "--addr", "0.0.0.0:9000"]).unwrap();
    assert_eq!(a.addr.as_deref(), Some("0.0.0.0:9000"));
    let a = parsed(&["connect", "the CI box", "--port=9200", "--no-tunnel"]).unwrap();
    assert_eq!(a.port, Some(9200));
    assert!(a.no_tunnel);
    let a = parsed(&["ping", "--addr=1.2.3.4:7880", "--json"]).unwrap();
    assert_eq!((a.sub, a.addr.as_deref(), a.json), (Sub::Ping, Some("1.2.3.4:7880"), true));
}

/// **`--addr` is `ping`'s alone** — the address to DIAL. The others refuse it with a message that
/// names `ping`, so somebody who typed `status --addr` meaning `ping` is told which verb they wanted
/// rather than being handed an answer about a different daemon.
#[test]
fn address_belongs_to_ping_and_is_refused_on_the_rest() {
    assert!(parsed(&["ping", "--addr", "0.0.0.0:17880"]).is_ok());
    for argv in [
        &["status", "--addr", "1:2"][..],
        &["disconnect", "--addr", "1:2"][..],
        &["connect", "box", "--addr", "1:2"][..],
    ] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains("--addr") && err.contains("ping"), "{argv:?}: {err}");
    }
}

/// `--json` belongs to the one subcommand whose product is a DOCUMENT. Accepted there, refused
/// by name everywhere else — a silently-dropped `--json` on `status` is how somebody comes to
/// pipe a human report into `jq` and blame the parser.
#[test]
fn json_belongs_to_ping_alone() {
    assert!(parsed(&["ping", "--json"]).expect("ping takes it").json);
    for argv in [&["mint-missing"][..], &["status"][..], &["disconnect"][..]] {
        let mut line = argv.to_vec();
        line.push("--json");
        let err = parsed(&line).unwrap_err();
        assert!(err.contains("--json") && err.contains("ping"), "{line:?}: {err}");
    }
}

/// `ping` takes NO key-bearing and no box-shaped flag — every one of them belongs to a verb
/// that writes something, and this one writes nothing.
#[test]
fn ping_refuses_every_flag_that_configures_a_box() {
    for flag in ["--no-tunnel", "--invitation"] {
        let err = parsed(&["ping", flag]).unwrap_err();
        assert!(err.contains(flag), "{flag}: {err}");
    }
    let err = parsed(&["ping", "--port", "7880"]).unwrap_err();
    assert!(err.contains("--port"), "{err}");
}

/// **`ping 1.2.3.4:7880` is refused with the spelling that works**, not with `unknown option`.
/// The bare positional is what a human types at a probe, and the token is exactly what the
/// verb wants — so the refusal hands back the whole corrected command line rather than telling
/// somebody their address is an unrecognised flag.
/// The registration's flags (decision 0288): `--server-project` is `connect`'s alone and is not a
/// second registration beside `--local-server`; the shared-key flags and the verbs that minted a
/// shared key (`setup`, `admin-key`) are gone, so each is refused now, never dropped in silence.
#[test]
fn the_registration_flags_belong_to_connect_and_the_shared_key_flags_are_gone() {
    let a = parsed(&["connect", "box", "--server-project", "/srv/vike"]).unwrap();
    assert_eq!(a.server_project.as_deref(), Some("/srv/vike"));
    for argv in [&["ping", "--server-project", "/p"][..], &["status", "--server-project", "/p"][..]]
    {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains("--server-project") && err.contains("CLIENT"), "{argv:?}: {err}");
    }
    for argv in [
        &["setup"][..],
        &["admin-key"][..],
        &["mint-missing", "--rotate"][..],
        &["client-keys"][..],
        &["connect", "box", "--manual"][..],
        &["connect", "box", "--observe-only"][..],
        &["connect", "box", "--replace"][..],
    ] {
        assert!(parsed(argv).is_err(), "{argv:?} must be refused");
    }
}

/// `--local-server`: a source of its own, `connect`'s alone, that dials the server on this computer
/// at 127.0.0.1 and so takes no HOST.
#[test]
fn the_local_server_is_a_source_of_its_own_and_dials_this_computer() {
    let a = parsed(&["connect", "--local-server", "docker:vike"]).unwrap();
    assert_eq!(a.local_server.as_deref(), Some("docker:vike"));
    assert_eq!(a.host.as_deref(), Some("127.0.0.1"));
    for argv in [
        &["connect", "box", "--local-server", "docker:vike"][..],
        &["connect", "--local-server", "/srv/vike", "--server-project", "/srv/vike"][..],
        &["connect", "--local-server", "/srv/vike", "--invitation"][..],
        &["status", "--local-server", "/srv/vike"][..],
    ] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains("--local-server"), "{argv:?}: {err}");
    }
}

/// `--invitation` (decision 0288 item 7): `connect`'s alone, with NO host (the invitation names it)
/// and no other source; the line itself is read from stdin at run time, never from argv.
#[test]
fn the_invitation_names_its_own_host_and_is_a_source_of_its_own() {
    let a = parsed(&["connect", "--invitation"]).unwrap();
    assert!(a.invitation && a.host.is_none());
    for argv in [
        &["connect", "box", "--invitation"][..],
        &["connect", "--invitation", "--no-tunnel"][..],
        &["connect", "--invitation", "--server-project", "/srv/vike"][..],
        &["status", "--invitation"][..],
    ] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains("--invitation"), "{argv:?}: {err}");
    }
}

/// `mint-missing` takes nothing: no flag configures what a first start mints (decision 0280), so
/// every flag is refused by name rather than dropped.
#[test]
fn mint_missing_refuses_every_flag() {
    for flag in ["--no-tunnel", "--invitation", "--json"] {
        let err = parsed(&["mint-missing", flag]).unwrap_err();
        assert!(err.contains(flag), "{flag}: {err}");
    }
    assert!(parsed(&["mint-missing", "--addr", "1:2"]).unwrap_err().contains("--addr"));
    assert!(parsed(&["mint-missing", "extra"]).unwrap_err().contains("extra"));
}

#[test]
fn a_bare_address_on_ping_is_answered_with_the_flag_that_takes_it() {
    let err = parsed(&["ping", "1.2.3.4:7880"]).unwrap_err();
    assert!(err.contains("--addr"), "{err}");
    assert!(err.contains("1.2.3.4:7880"), "it echoes the address back: {err}");
    assert_eq!(
        &*err,
        "vike-cli backend: `ping` takes the address as --addr, not as a bare argument: \
         `vike-cli backend ping --addr 1.2.3.4:7880`\n",
        "the answer this arm exists to fix (never `unknown option`)"
    );
}

/// A client-side flag on a verb it does not belong to is refused by name, and says which box.
#[test]
fn a_client_side_flag_on_another_verb_is_refused_by_name() {
    for (argv, flag) in [
        (&["mint-missing", "--invitation"][..], "--invitation"),
        (&["mint-missing", "--no-tunnel"][..], "--no-tunnel"),
        (&["status", "--server-project", "/p"][..], "--server-project"),
    ] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains(flag), "{argv:?}: {err}");
        assert!(err.contains("CLIENT"), "it must say which box: {err}");
    }
    let err = parsed(&["mint-missing", "--port", "9200"]).unwrap_err();
    assert!(err.contains("--port"), "{err}");
}

#[test]
fn connect_needs_a_host_and_takes_exactly_one() {
    let err = parsed(&["connect"]).unwrap_err();
    assert!(err.contains("HOST"), "{err}");
    let err = parsed(&["connect", "the CI box", "extra"]).unwrap_err();
    assert!(err.contains("ONE host"), "{err}");
    // ⚠ And the second-argument refusal says no key goes there, rather than leaving somebody to
    // guess that one might.
    assert!(err.contains("never goes on the command line"), "{err}");
}

#[test]
fn a_bad_port_is_refused_by_value_and_zero_is_not_a_port() {
    for bad in ["0", "70000", "nine", "-1", ""] {
        let err = parsed(&["connect", "the CI box", "--port", bad]).unwrap_err();
        assert!(err.contains("--port"), "{bad:?}: {err}");
    }
}

#[test]
fn a_bare_boolean_rejects_an_inline_value() {
    let err = parsed(&["connect", "the CI box", "--no-tunnel=1"]).unwrap_err();
    assert!(err.contains("--no-tunnel"), "{err}");
}

#[test]
fn usage_errors_are_clean_and_help_short_circuits_at_both_levels() {
    assert!(!parsed(&[]).unwrap_err().is_help(), "a bare `backend` is a usage error");
    assert!(parsed(&["pair"]).unwrap_err().contains("pair"));
    assert!(parsed(&["mint-missing", "--verbose"]).unwrap_err().contains("--verbose"));
    for spelling in ["-h", "--help", "help"] {
        assert!(parsed(&[spelling]).unwrap_err().is_help(), "{spelling} is a success");
    }
    assert!(parsed(&["mint-missing", "--help"]).unwrap_err().is_help());
}

/// The usage documents every subcommand this parser accepts, and says which BOX each runs on.
///
/// ⚠ The naming residual the design once declared rather than fixed — a reader arriving from
/// QuantConnect reads "node" as rented capacity, one arriving from nowhere reads Node.js — is
/// answered by the VERB now: what an operator types is `backend`. The prose that spells the word
/// out stays, because the PROTOCOL noun is still `node` (`HubIdentityWire`, `--node`, `hub_key`),
/// and that prose is what tells a reader the two name one thing — which
/// is why the assertion below still pins "vike-tradehub — the running daemon".
#[test]
fn usage_documents_every_subcommand_and_which_box_it_runs_on() {
    let usage = args::long_help::<Cli>(BIN);
    for verb in ["mint-missing", "connect", "status", "disconnect", "ping"] {
        assert!(usage.contains(verb), "usage omits {verb}");
    }
    assert!(usage.contains("DAEMON's box") && usage.contains("CLIENT's box"), "{usage}");
    // Owner ruling 2026-11-24 (#3 = A): the word stays `backend`, and its help says what it covers.
    assert!(
        usage.contains("This box and its hubs, and a client box's link to a vike-tradehub"),
        "{usage}"
    );
    // …and it never invites a key onto the command line.
    assert!(usage.contains("never accepts a key and never prints one"), "{usage}");
    // ⚠ `ping` is in a family defined by which BOX a verb runs on and belongs to no box, so
    // the usage has to say both halves: that it runs anywhere, and that it speaks the OTHER
    // protocol. Without the second, a reader takes it for `status` with an address — which is
    // the confusion the module doc's ⚠ exists to prevent, and help is where it gets read.
    assert!(usage.contains("from ANYWHERE"), "{usage}");
    assert!(usage.contains("vike-datahub"), "{usage}");
    // ⚠ It used to promise `--node` stops being required, and `trade status` exits on the usage rung
    // with the address stored: `a_stored_dial_address_does_not_make_node_optional`.
    assert!(usage.contains("still require --node on every command"), "{usage}");
    // …and that the store root is NOT something it can answer. An operator who reads a probe's
    // help and does not see the gap will read its silence as "this daemon has no store root".
    assert!(usage.contains("STORE ROOT is NOT on that wire"), "{usage}");
}

/// **The help fills in the hub defaults from vike-config, in the bytes it always printed** (B5.03): the
/// text around each filled-in default, exactly, so a slip while the literal became a placeholder is red.
#[test]
fn the_help_fills_in_the_hub_defaults_in_the_bytes_it_always_printed() {
    let about = long_about();
    assert!(
        about.contains("set config.tradehub_addr to 127.0.0.1:7879 when no row names\n"),
        "{about}"
    );
    assert!(
        about.contains("the datahub's\n              7878 and the backtesthub's 7880), register"),
        "{about}"
    );
    assert_eq!(port_help(), "connect/disconnect: the node port to forward (default 7879)");
}

/// A minted key is 64 lowercase hex characters, and two mints differ.
///
/// ⚠ This is a SHAPE test and deliberately not a randomness test — a statistical claim about a
/// CSPRNG belongs to that crate's own suite, and a flaky entropy assertion here would be a gate
/// nobody trusts. What it does catch is the failure that would be silent: a truncated or
/// half-filled buffer, which reaches the wire as a shorter key and fails as an opaque auth
/// denial rather than as anything readable.
#[test]
fn a_minted_key_is_full_length_lowercase_hex_and_not_a_constant() {
    let a = mint_key();
    assert_eq!(a.len(), NODE_KEY_BYTES * 2, "{a}");
    assert!(a.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{a}");
    assert_ne!(a, mint_key(), "two mints must not collide");
}

/// The key id is derived, stable, and NOT the key — the three properties that make it the one
/// thing safe to print.
#[test]
fn a_key_id_is_stable_derived_and_carries_no_key_bytes() {
    let key = mint_key();
    let id = key_id(&key);
    assert_eq!(id, key_id(&key), "same key ⇒ same id");
    assert_ne!(id, key_id(&mint_key()), "different keys ⇒ different ids");
    assert!(!id.contains(&key), "the id must not carry the key: {id}");
    assert!(!key.contains(&id), "…nor the key the id: {key}");
}

/// A setup's restart lines name the supervisor's verb first and the system units after, each daemon in the order
/// given (decision 0260): right on a host-mode box and on a box that keeps units.
#[test]
fn the_restart_lines_serve_both_server_installs_in_order() {
    assert_eq!(
        restart_lines(&["datahub", "tradehub"]),
        [
            "  bin/vikectl restart datahub",
            "  bin/vikectl restart tradehub",
            "  …or, on a box that keeps system units:",
            "  sudo systemctl restart vike-datahub",
            "  sudo systemctl restart vike-tradehub",
        ]
    );
}
