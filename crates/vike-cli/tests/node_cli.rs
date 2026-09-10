//! End-to-end tests for `vike-cli backend`, driving the SHIPPED binary (`CARGO_BIN_EXE_vike-cli`).
//!
//! The unit tests beside the module cover the grammar, the pure renderers and the key mint. These
//! cover the two things that matter to an operator and that no unit test can see: **what actually
//! lands on disk**, and **what reaches the two streams**.
//!
//! ⚠ **Every invocation points `VIKE_SETTINGS_DIR` at the case's own temp directory**, on the CHILD
//! through `Command::env` — never `std::env::set_var`, which is unsafe under threads and would leak
//! across this binary's parallel cases. Without it a run on a developer box resolves the REPO's
//! settings directory and this suite would MINT KEYS INTO A REAL CREDENTIAL STORE. That is isolation
//! and a safety property, and here it is the second more than the first.
//!
//! `Stdio::null()` on stdin keeps every case that does not deliberately feed keys from blocking, and
//! makes `--manual`'s "no observe key on stdin" refusal reachable.
//!
//! # What is deliberately NOT covered here
//!
//! `connect`'s tunnel and its verification round trip. Raising an ssh forward needs a reachable host
//! and an accepted key, and the round trip needs a running node — `tests/trade_node_e2e.rs` is where
//! this crate stands a real loopback daemon up, and pairing that harness with an ssh dependency
//! would make a credential-writing suite depend on the developer's own ssh configuration. What IS
//! covered is everything before and after the network: the refusals, the store writes, the settings
//! writes and the streams.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// **The equality assertion `vike_model::credential_keys::PLATFORM_KEYS` owes.**
///
/// `crates/vike-tradehub-client/src/auth.rs`'s doc carries the table of every crate that spells the
/// two node key names and warns that *"adding a fifth copy without adding its equality assertion
/// re-opens the gap this table exists to close"*. `PLATFORM_KEYS` is such a copy — it must spell the
/// names, because it is a names-only table and there is nothing else for it to hold — and this is
/// its payment.
///
/// It lives HERE rather than beside the table because `vike-cli` is the lowest crate that can see
/// both: `vike-model` is layer 10 and cannot see `vike-tradehub-client` at layer 50, and must not —
/// the layer gate would refuse the edge, and an imported constant would make the settings registry
/// blind to the read anyway, which is the whole reason the duplication exists.
///
/// The symptom of the drift it prevents is the one worth remembering: an endless `bad mac` at the
/// node, with every test in every crate green.
#[test]
fn the_platform_key_table_is_the_servers_own_spelling() {
    let table = vike_model::credential_keys::PLATFORM_KEYS;
    assert_eq!(table[0], vike_tradehub_client::auth::OBSERVE_KEY_ENV, "observe, and IN ORDER");
    assert_eq!(table[1], vike_tradehub_client::auth::CONTROL_KEY_ENV, "control, and IN ORDER");
    // …and the CLI's own copies, which are what the writer actually hands to the store. Three
    // spellings, one value, all three asserted — the third pairing is `cmd::nodekeys`' own
    // `both_key_names_match_the_servers_own`, and this is the leg it does not cover.
    assert!(vike_model::credential_keys::is_platform_key(
        vike_tradehub_client::auth::OBSERVE_KEY_ENV
    ));
    assert!(vike_model::credential_keys::is_platform_key(
        vike_tradehub_client::auth::CONTROL_KEY_ENV
    ));
}

/// The SAME payment, for the DATAHUB pair that joined `PLATFORM_KEYS` on 2026-09-08.
///
/// ⚠ A second service's node keys are a second copy, and the rule the test above states does not
/// weaken because the table already existed: *"adding a copy without adding its equality assertion
/// re-opens the gap this table exists to close"*. `vike_datahub_client::node_auth`'s
/// `DATAHUB_OBSERVE_KEY_ENV` / `DATAHUB_CONTROL_KEY_ENV` are that pair's reference spelling, and
/// this is what holds `PLATFORM_KEYS`'s `concat!`-split copies equal to them.
///
/// Indices `[2]`/`[3]` rather than a `contains` — the tradehub pair is pinned by index above, the
/// order is stated as load-bearing at the table, and an assertion that only checked membership
/// would pass a table that had silently reordered under the test above.
#[test]
fn the_platform_key_table_carries_the_datahub_servers_own_spelling() {
    let table = vike_model::credential_keys::PLATFORM_KEYS;
    assert_eq!(
        table[2],
        vike_datahub_client::node_auth::DATAHUB_OBSERVE_KEY_ENV,
        "observe, and IN ORDER"
    );
    assert_eq!(
        table[3],
        vike_datahub_client::node_auth::DATAHUB_CONTROL_KEY_ENV,
        "control, and IN ORDER"
    );
    assert!(vike_model::credential_keys::is_platform_key(
        vike_datahub_client::node_auth::DATAHUB_OBSERVE_KEY_ENV
    ));
    assert!(vike_model::credential_keys::is_platform_key(
        vike_datahub_client::node_auth::DATAHUB_CONTROL_KEY_ENV
    ));
    // ⚠ The two pairs are DISJOINT. A copy-paste that made the datahub rows repeat the tradehub
    // ones would satisfy every assertion above taken singly, and would route `secrets set` for a
    // datahub key at the tradehub's command.
    assert_ne!(table[0], table[2], "the two services' observe keys must differ");
    assert_ne!(table[1], table[3], "the two services' control keys must differ");
}

/// A project directory laid out the way the loader expects: `<project>/settings/`, with a store in
/// it unless a case says otherwise.
struct Case {
    project: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let project =
            std::env::temp_dir().join(format!("vike-cli-node-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("settings")).unwrap();
        Case { project }
    }

    fn settings(&self) -> PathBuf {
        self.project.join("settings")
    }

    /// The file `backend` verbs WRITE — `node.env` since 2026-09-08.
    ///
    /// ⚠ This said `secrets.env`, and changing it is the whole point of the split rather than a
    /// fixture detail: node keys are 4 names and that file holds 168 venue ones, so the verbs that
    /// mint a pair write beside it instead. `docs/decisions/0051` carries the argument. Every case
    /// below that writes a store or reads one back is asserting about THIS file.
    fn store(&self) -> PathBuf {
        self.settings().join(vike_secrets::NODE_FILE)
    }

    /// The VENUE credential store — still `secrets.env`, and the two cases that care about it are
    /// the ones proving the split: a backend verb must not write here, and a box whose keys have
    /// not migrated must still be READ from here.
    fn venue_store(&self) -> PathBuf {
        self.settings().join(vike_secrets::SECRETS_FILE)
    }

    fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).unwrap();
    }

    fn read_store(&self) -> String {
        std::fs::read_to_string(self.store()).unwrap_or_default()
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.settings().join(rel)).unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_stdin(args, None)
    }

    fn run_with_stdin(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("backend")
            .args(args)
            .env("VIKE_SETTINGS_DIR", self.settings())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match input {
            None => {
                cmd.stdin(Stdio::null());
                cmd.output().expect("the vike-cli binary must run")
            }
            Some(text) => {
                use std::io::Write;
                cmd.stdin(Stdio::piped());
                let mut child = cmd.spawn().expect("the vike-cli binary must run");
                child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
                child.wait_with_output().expect("the child must finish")
            }
        }
    }

    /// One key's value out of the store this case wrote, for the "the printed id IS this key's id"
    /// assertions. ⚠ It never reaches an assertion MESSAGE — a failing test must not print a
    /// credential into CI's log any more than the command may.
    fn stored(&self, key: &str) -> Option<String> {
        self.read_store().lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.project);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn observe_name() -> &'static str {
    vike_tradehub_client::auth::OBSERVE_KEY_ENV
}

fn control_name() -> &'static str {
    vike_tradehub_client::auth::CONTROL_KEY_ENV
}

/// A store with an unrelated venue credential in it, so every byte-preservation claim below has
/// something to preserve.
const SAMPLE: &str = "# a real operator's store\n\
                      BINANCE_LIVE_API_KEY=key-abcd1234\n\
                      \n\
                      # the secret, with a trailing comment\n\
                      BINANCE_LIVE_API_SECRET=\"sup3r-s3cr3t\"\n";

/// The verb is registered: it reaches the dispatcher, prints its own usage to STDOUT, and succeeds.
///
/// A non-zero `--help` breaks `set -e` and every packaging smoke test, and a unit test of the parser
/// cannot see either the status or the stream — which is the whole reason this crate has
/// `tests/help_cli.rs`.
#[test]
fn backend_is_a_registered_verb_whose_help_is_a_success_on_stdout() {
    let c = Case::new("help");
    let out = c.run(&["--help"]);
    assert!(out.status.success(), "--help must succeed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("setup") && text.contains("connect"), "{text}");
    assert!(text.contains("DAEMON's box") && text.contains("CLIENT's box"), "{text}");
    assert!(stderr(&out).is_empty(), "help is not a diagnostic: {}", stderr(&out));

    // …and the dispatcher's own command list carries it, which is what a user reads first.
    let top = Command::new(BIN)
        .arg("--help")
        .env("VIKE_SETTINGS_DIR", c.settings())
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    assert!(
        String::from_utf8_lossy(&top.stdout).contains("backend"),
        "the command list omits `backend`"
    );
}

/// **An ABSENT store is refused, and the refusal names the command that makes one.** This is
/// `docs/decisions/0036`'s *"creating the file stays the operator's decision"* and the design's Q2
/// answered `no`: one extra step on a fresh box, against a one-way widening of a ratified fence.
///
/// ⚠ The store is `node.env` and the route CHANGED with it: `secrets template` emits the VENUE grid
/// and would be the wrong command to run at a file that must hold four node keys and nothing else.
/// The refusal names a creation the operator types against their own path, which is what 0036's
/// fence is actually about.
#[test]
fn setup_refuses_an_absent_store_and_names_how_to_make_one() {
    let c = Case::new("nostore");
    // ⚠ The VENUE store EXISTS in this case, and that is the point: a box mid-migration has one.
    // The refusal must still fire, because these verbs write the node file — a `setup` that fell
    // back to whatever store it found would migrate nothing, ever.
    std::fs::write(c.venue_store(), "BINANCE_DEMO_API_KEY=x\n").unwrap();
    let out = c.run(&["setup"]);
    assert!(!out.status.success(), "an absent node store must not succeed");
    let err = stderr(&out);
    assert!(err.contains("no node-key store"), "{err}");
    assert!(err.contains("install -m600"), "it must name the route: {err}");
    assert!(err.contains("node.env"), "…and the file it is about: {err}");
    assert!(!c.store().exists(), "setup must not have created a store");
    // …and nothing else was written either: a refusal is total.
    assert!(c.read("config.toml").is_empty(), "config.toml was written by a refused setup");
    // ⚠ AND IT DID NOT TOUCH THE VENUE STORE. This is the split's whole claim, asserted where a
    // regression would be cheapest to make: a `store_path` that drifted back would write two node
    // keys into the file holding every venue key, and every other test here would still pass.
    assert_eq!(
        std::fs::read_to_string(c.venue_store()).unwrap(),
        "BINANCE_DEMO_API_KEY=x\n",
        "a backend verb wrote into the VENUE credential store"
    );
}

/// **THE central test.** `setup` mints both keys, and:
///
/// - each printed `key_id` IS the fingerprint of the key that landed in the store — so the two-box
///   comparison the design rests on is comparing the right thing;
/// - **neither key VALUE appears on stdout or stderr**, which is the rule the whole module exists to
///   hold;
/// - every other byte of the store is preserved — the upsert's contract, exercised through this new
///   call site rather than assumed from the writer's own suite;
/// - `config.tradehub_addr` lands with the default;
/// - `flags.tradehub_control` is NOT written, because `--control` was not passed.
#[test]
fn setup_mints_two_keys_prints_their_ids_and_never_a_key() {
    let c = Case::new("mint");
    c.write_store(SAMPLE);

    let out = c.run(&["setup"]);
    assert!(out.status.success(), "setup failed: {}", stderr(&out));
    let text = stdout(&out);
    let err = stderr(&out);

    let observe = c.stored(observe_name()).expect("the observe key must be in the store");
    let control = c.stored(control_name()).expect("the control key must be in the store");
    assert_eq!(observe.len(), 64, "a node key is 32 bytes, hex encoded");
    assert_ne!(observe, control, "the two keys must be independently minted");

    // The ids, and the values NOT.
    for key in [&observe, &control] {
        let id = vike_datahub_client::node_auth::key_fingerprint(key.as_bytes());
        assert!(text.contains(&id), "a printed id is missing (ids: {text})");
        assert!(!text.contains(key.as_str()), "A KEY REACHED STDOUT");
        assert!(!err.contains(key.as_str()), "A KEY REACHED STDERR");
    }
    assert!(text.contains(observe_name()) && text.contains(control_name()), "{text}");

    // Byte preservation, through the new call site.
    let store = c.read_store();
    assert!(store.contains("BINANCE_LIVE_API_KEY=key-abcd1234"), "{store}");
    assert!(store.contains("# the secret, with a trailing comment"), "{store}");
    assert!(store.contains("BINANCE_LIVE_API_SECRET=\"sup3r-s3cr3t\""), "{store}");

    // The settings half.
    assert!(c.read("config.toml").contains("127.0.0.1:7879"), "{}", c.read("config.toml"));
    assert!(
        !c.read("flags.toml").contains("tradehub_control"),
        "--control was NOT passed and the flag must be untouched: {}",
        c.read("flags.toml")
    );
    assert!(text.contains("control is NOT armed"), "and it must SAY so: {text}");

    // The restart line, which is what makes any of it take effect.
    assert!(text.contains("systemctl restart vike-tradehub"), "{text}");
}

/// `--control` is the whole consent, and it is the only thing that writes the flag.
#[test]
fn control_is_written_only_when_it_is_asked_for() {
    let c = Case::new("control");
    c.write_store(SAMPLE);
    let out = c.run(&["setup", "--control", "--addr", "0.0.0.0:9100"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(c.read("flags.toml").contains("tradehub_control = true"), "{}", c.read("flags.toml"));
    assert!(c.read("config.toml").contains("0.0.0.0:9100"), "{}", c.read("config.toml"));
    let text = stdout(&out);
    assert!(text.contains("control is ARMED"), "{text}");
}

/// **A second `setup` refuses, and leaves the store BYTE-IDENTICAL.** A re-run that silently
/// rotated would detach every client already holding the old keys, with the symptom appearing on
/// those other boxes as an auth denial that reads like a revoked credential.
#[test]
fn a_second_setup_refuses_without_rotate_and_writes_nothing() {
    let c = Case::new("rerun");
    c.write_store(SAMPLE);
    assert!(c.run(&["setup"]).status.success());
    let before = c.read_store();

    let out = c.run(&["setup"]);
    assert!(!out.status.success(), "a silent re-mint is the failure this refusal exists for");
    let err = stderr(&out);
    assert!(err.contains("--rotate"), "{err}");
    assert!(err.contains("stops working"), "the cost must be stated: {err}");
    assert_eq!(c.read_store(), before, "a refused setup must not touch the store");
}

/// `--rotate` replaces BOTH keys, preserves every unrelated byte, and still prints no key.
#[test]
fn rotate_replaces_both_keys_and_preserves_the_rest_of_the_store() {
    let c = Case::new("rotate");
    c.write_store(SAMPLE);
    assert!(c.run(&["setup"]).status.success());
    let first_observe = c.stored(observe_name()).unwrap();
    let first_control = c.stored(control_name()).unwrap();

    let out = c.run(&["setup", "--rotate"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let second_observe = c.stored(observe_name()).unwrap();
    let second_control = c.stored(control_name()).unwrap();
    assert_ne!(first_observe, second_observe, "rotate must replace the observe key");
    assert_ne!(first_control, second_control, "rotate must replace the control key");

    let text = stdout(&out);
    assert!(text.contains("ROTATED"), "a rotation must not read as a first run: {text}");
    for key in [&first_observe, &first_control, &second_observe, &second_control] {
        assert!(!text.contains(key.as_str()), "A KEY REACHED STDOUT ON THE ROTATION PATH");
        assert!(!stderr(&out).contains(key.as_str()), "A KEY REACHED STDERR ON THE ROTATION PATH");
    }
    // The store is still the operator's file.
    assert!(c.read_store().contains("# a real operator's store"), "{}", c.read_store());
    assert_eq!(c.read_store().matches("BINANCE_LIVE_API_KEY").count(), 1, "no key was duplicated");
}

/// The write is JOURNALLED, with key NAMES and no value — the fourth question
/// `crates/vike-ops/tests/credential_writer_gate.rs`'s `GROWTH_GUIDANCE` asks of every credential
/// writer, asserted against the ledger this run actually appended to.
#[test]
fn the_mint_is_journalled_with_names_and_no_value() {
    let c = Case::new("journal");
    c.write_store(SAMPLE);
    assert!(c.run(&["setup"]).status.success());

    let dir = c.settings().join("state");
    let mut found = String::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p) {
                found.push_str(&t);
            }
        }
    }
    assert!(found.contains("credential_write"), "no credential_write record: {found}");
    assert!(found.contains(observe_name()) && found.contains(control_name()), "{found}");
    let observe = c.stored(observe_name()).unwrap();
    assert!(!found.contains(observe.as_str()), "A KEY REACHED THE CHANGE JOURNAL");
    // …and the settings edits are in the same ledger, which is what makes an after-the-fact "who
    // opened this node's socket" answerable at all.
    assert!(found.contains("set_setting"), "{found}");
}

/// **A key may not be given on the command line, on any verb here** — and `--manual` with nothing on
/// stdin says how the value is supposed to arrive rather than accepting an empty one.
#[test]
fn a_key_never_goes_on_the_command_line() {
    let c = Case::new("argv");
    c.write_store(SAMPLE);

    // `setup` takes no value at all: a stray argument is an unknown option, not a key.
    let out = c.run(&["setup", "deadbeef"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown option"), "{}", stderr(&out));

    // `connect`'s ONE positional is the HOST; a second argument is refused and the refusal points at
    // the stdin form.
    let out = c.run(&["connect", "the CI box", "deadbeef"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("ONE host"), "{err}");
    assert!(err.contains("--manual"), "{err}");

    // …and `--manual` with an empty stdin refuses, naming the pipe form.
    let out = c.run_with_stdin(&["connect", "the CI box", "--no-tunnel", "--manual"], Some(""));
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("no observe key on stdin"), "{err}");
    assert!(err.contains("shell history"), "it must say WHY argv is refused: {err}");
}

/// **A flag typed on the wrong box is refused by name.** `--control` on `connect` is the one that
/// matters: silently ignored, it would leave an operator believing they had armed the daemon's write
/// channel from a laptop, which can arm nothing.
#[test]
fn a_flag_for_the_other_box_is_refused_by_name() {
    let c = Case::new("scope");
    c.write_store(SAMPLE);
    let out = c.run(&["connect", "the CI box", "--control"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("--control"), "{err}");
    assert!(err.contains("DAEMON"), "{err}");
}

/// `status` on a box that has never attached reports exactly that, and says what would fix it —
/// rather than printing an empty table or dialling nothing.
#[test]
fn status_on_an_unattached_box_names_what_is_missing() {
    let c = Case::new("status");
    c.write_store(SAMPLE);
    let out = c.run(&["status"]);
    assert!(!out.status.success(), "there is no node to ask");
    let text = stdout(&out);
    assert!(text.contains("dial address: NONE"), "{text}");
    assert!(text.contains("absent"), "both key planes must be reported: {text}");
    assert!(stderr(&out).contains("backend connect"), "{}", stderr(&out));
}

/// `disconnect` with no recorded tunnel is a SUCCESS that says so. It is the idempotent shape: an
/// operator running it twice, or on a box where the tunnel was raised by hand, must not be handed a
/// failure for a state they asked for.
#[test]
fn disconnect_with_no_recorded_tunnel_is_a_success_that_says_so() {
    let c = Case::new("disc");
    c.write_store(SAMPLE);
    let out = c.run(&["disconnect"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no tunnel recorded"), "{}", stdout(&out));
}
