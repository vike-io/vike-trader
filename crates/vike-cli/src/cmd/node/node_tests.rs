use super::*;
use crate::cmd::args::HELP_SENTINEL;

fn parsed(argv: &[&str]) -> Result<Args, String> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

#[test]
fn each_subcommand_parses_and_every_flag_defaults_off() {
    for (argv, sub) in [
        (&["setup"][..], Sub::Setup),
        (&["status"][..], Sub::Status),
        (&["disconnect"][..], Sub::Disconnect),
        (&["ping"][..], Sub::Ping),
    ] {
        let a = parsed(argv).unwrap();
        assert_eq!(a.sub, sub);
        assert!(!a.control && !a.rotate && !a.manual && !a.no_tunnel && !a.replace, "{a:?}");
        assert!(!a.json, "{a:?}");
        assert_eq!((a.addr, a.port, a.host), (None, None, None));
    }
    let a = parsed(&["connect", "the CI box"]).unwrap();
    assert_eq!((a.sub, a.host.as_deref()), (Sub::Connect, Some("the CI box")));
}

#[test]
fn options_parse_in_both_flag_forms() {
    let a = parsed(&["setup", "--addr", "0.0.0.0:9000", "--control", "--rotate"]).unwrap();
    assert_eq!(a.addr.as_deref(), Some("0.0.0.0:9000"));
    assert!(a.control && a.rotate);
    let a = parsed(&["setup", "--addr=0.0.0.0:9000"]).unwrap();
    assert_eq!(a.addr.as_deref(), Some("0.0.0.0:9000"));
    let a = parsed(&["connect", "the CI box", "--port=9200", "--manual", "--replace"]).unwrap();
    assert_eq!(a.port, Some(9200));
    assert!(a.manual && a.replace);
    let a = parsed(&["ping", "--addr=1.2.3.4:7880", "--json"]).unwrap();
    assert_eq!((a.sub, a.addr.as_deref(), a.json), (Sub::Ping, Some("1.2.3.4:7880"), true));
}

/// **`--addr` is the one flag two subcommands share, and they mean opposite sides of the
/// socket** — the address a daemon BINDS on `setup`, the address to DIAL on `ping`. Both are
/// accepted; the other three are refused with a message that names both meanings, so somebody
/// who typed `status --addr` meaning `ping` is told which verb they wanted rather than being
/// handed an answer about a different daemon.
#[test]
fn addr_is_shared_by_setup_and_ping_and_refused_on_the_rest() {
    assert!(parsed(&["setup", "--addr", "0.0.0.0:7879"]).is_ok());
    assert!(parsed(&["ping", "--addr", "0.0.0.0:7880"]).is_ok());
    for argv in [&["status", "--addr", "1:2"][..], &["disconnect", "--addr", "1:2"][..]] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains("--addr"), "{argv:?}: {err}");
        assert!(err.contains("setup") && err.contains("ping"), "both meanings: {err}");
    }
}

/// `--json` belongs to the one subcommand whose product is a DOCUMENT. Accepted there, refused
/// by name everywhere else — a silently-dropped `--json` on `status` is how somebody comes to
/// pipe a human report into `jq` and blame the parser.
#[test]
fn json_belongs_to_ping_alone() {
    assert!(parsed(&["ping", "--json"]).expect("ping takes it").json);
    for argv in [&["setup"][..], &["status"][..], &["disconnect"][..]] {
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
    for flag in ["--control", "--rotate", "--manual", "--no-tunnel", "--replace"] {
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
#[test]
fn a_bare_address_on_ping_is_answered_with_the_flag_that_takes_it() {
    let err = parsed(&["ping", "1.2.3.4:7880"]).unwrap_err();
    assert!(err.contains("--addr"), "{err}");
    assert!(err.contains("1.2.3.4:7880"), "it echoes the address back: {err}");
    assert!(!err.contains("unknown option"), "the answer this arm exists to fix: {err}");
}

/// **`--control` is refused off `setup`.** It is the flag whose silent acceptance would be
/// worst: typed on `connect`, on the laptop, it would leave an operator believing they had armed
/// the daemon's write channel from a box that cannot arm anything.
#[test]
fn a_daemon_side_flag_on_a_client_side_verb_is_refused_by_name() {
    for (argv, flag) in [
        (&["connect", "the CI box", "--control"][..], "--control"),
        (&["status", "--rotate"][..], "--rotate"),
        (&["disconnect", "--addr", "1:2"][..], "--addr"),
    ] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains(flag), "{argv:?}: {err}");
        assert!(err.contains("DAEMON"), "it must say which box: {err}");
    }
    for (argv, flag) in [
        (&["setup", "--manual"][..], "--manual"),
        (&["setup", "--no-tunnel"][..], "--no-tunnel"),
        (&["status", "--replace"][..], "--replace"),
    ] {
        let err = parsed(argv).unwrap_err();
        assert!(err.contains(flag), "{argv:?}: {err}");
        assert!(err.contains("CLIENT"), "it must say which box: {err}");
    }
    let err = parsed(&["setup", "--port", "9200"]).unwrap_err();
    assert!(err.contains("--port"), "{err}");
}

#[test]
fn connect_needs_a_host_and_takes_exactly_one() {
    let err = parsed(&["connect"]).unwrap_err();
    assert!(err.contains("HOST"), "{err}");
    let err = parsed(&["connect", "the CI box", "extra"]).unwrap_err();
    assert!(err.contains("ONE host"), "{err}");
    // ⚠ And the second-argument refusal points at the stdin form rather than leaving somebody
    // to guess that a key might go there. It is the message an operator reaches while holding
    // two keys and a host.
    assert!(err.contains("--manual"), "{err}");
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
    let err = parsed(&["setup", "--control=1"]).unwrap_err();
    assert_eq!(err, "--control takes no value");
}

#[test]
fn usage_errors_are_clean_and_help_short_circuits_at_both_levels() {
    assert!(parsed(&[]).unwrap_err().contains("subcommand is required"));
    assert!(parsed(&["pair"]).unwrap_err().contains("unknown `backend` subcommand 'pair'"));
    assert!(parsed(&["setup", "--verbose"]).unwrap_err().contains("--verbose"));
    for spelling in ["-h", "--help", "help"] {
        assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
    }
    assert_eq!(parsed(&["setup", "--help"]).unwrap_err(), HELP_SENTINEL);
}

/// The usage documents every subcommand this parser accepts, and says which BOX each runs on.
///
/// ⚠ The naming residual the design once declared rather than fixed — a reader arriving from
/// QuantConnect reads "node" as rented capacity, one arriving from nowhere reads Node.js — is
/// answered by the VERB now: what an operator types is `backend`. The prose that spells the word
/// out stays, because the PROTOCOL noun is still `node` (`WireNodeIdentity`, `--node`, `node.env`),
/// and that prose is what tells a reader the two name one thing — which
/// is why the assertion below still pins "vike-tradehub node — the running daemon".
#[test]
fn usage_documents_every_subcommand_and_which_box_it_runs_on() {
    for verb in ["setup", "connect", "status", "disconnect", "ping"] {
        assert!(USAGE.contains(verb), "usage omits {verb}");
    }
    assert!(USAGE.contains("DAEMON's box") && USAGE.contains("CLIENT's box"), "{USAGE}");
    assert!(USAGE.contains("vike-tradehub node — the running daemon"), "{USAGE}");
    // …and it never invites a key onto the command line.
    assert!(USAGE.contains("never accepts a key and never prints one"), "{USAGE}");
    // ⚠ `ping` is in a family defined by which BOX a verb runs on and belongs to no box, so
    // the usage has to say both halves: that it runs anywhere, and that it speaks the OTHER
    // protocol. Without the second, a reader takes it for `status` with an address — which is
    // the confusion the module doc's ⚠ exists to prevent, and help is where it gets read.
    assert!(USAGE.contains("from ANYWHERE"), "{USAGE}");
    assert!(USAGE.contains("vike-datahub"), "{USAGE}");
    // …and that the store root is NOT something it can answer. An operator who reads a probe's
    // help and does not see the gap will read its silence as "this daemon has no store root".
    assert!(USAGE.contains("STORE ROOT is NOT on that wire"), "{USAGE}");
}

/// The port default is the bind default's own port, so `setup` and `connect` cannot come to
/// disagree about which socket this workspace means by "the node".
#[test]
fn the_default_port_is_the_default_bind_addrs_own() {
    let port = DEFAULT_BIND_ADDR.rsplit_once(':').expect("the default is host:port").1;
    assert_eq!(port.parse::<u16>().unwrap(), DEFAULT_PORT);
}

/// **The bind default IS the daemon's own**, held equal through the dev-dependency this crate
/// already carries for its loopback e2e tests.
///
/// The failure it closes is quiet: a `setup` that wrote a different default would produce a box
/// whose `config.tradehub_addr` names one port while the daemon — falling back to its own
/// constant only when the key is ABSENT — binds the same one, so nothing breaks until somebody
/// changes either constant, and then the two disagree with a connection refused and no
/// diagnosis. It is the same trade `crate::cmd::nodekeys`' `both_key_names_match_the_servers_own`
/// makes: a deliberate duplication is fine, an UNASSERTED one is how a client and a server come
/// to mean different things.
#[test]
fn the_default_bind_address_is_the_daemons_own() {
    assert_eq!(DEFAULT_BIND_ADDR, vike_tradehub::server::DEFAULT_ADDR);
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

/// The names this module writes are the ones the platform table validates — in this direction
/// the assertion is the WRITE's own precondition, which is why it is a function rather than only
/// a test.
#[test]
fn the_names_this_module_writes_are_platform_keys() {
    let names = validated_names().expect("both names must be in PLATFORM_KEYS");
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    for name in names {
        assert!(vike_model::credential_keys::is_platform_key(name), "{name}");
        // …and not a venue credential, which is the confusion the two tables exist to prevent.
        assert!(vike_model::credential_keys::key_owner(name).is_none(), "{name}");
    }
}
