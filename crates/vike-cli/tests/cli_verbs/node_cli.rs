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
//! `Stdio::null()` on stdin keeps every case that does not deliberately feed stdin from blocking.
//!
//! # What is deliberately NOT covered here
//!
//! `connect`'s tunnel and its verification round trip. Raising an ssh forward needs a reachable host
//! and an accepted key, and the round trip needs a running node — `tests/shipped_cli/trade_hub_e2e.rs` is where
//! this crate stands a real loopback daemon up, and pairing that harness with an ssh dependency
//! would make a credential-writing suite depend on the developer's own ssh configuration. What IS
//! covered is everything before and after the network: the refusals, the store writes, the settings
//! writes and the streams.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// An address nothing answers on: port 1 on loopback refuses at once.
const DEAD_ADDRESS: &str = "127.0.0.1:1";

/// **The equality assertion `vike_venues::credential_keys::PLATFORM_KEYS` owes for the datahub's
/// STRATEGY-LANE key** (`PLATFORM_KEYS[9]`, wave 2), the one live name a server crate spells too.
///
/// `vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV` is its reference spelling (a second copy, `concat!`-split
/// for the registry-harvest reason both tables give): adding a copy without its equality assertion re-opens
/// the gap the table exists to close, so this holds the two equal, BY INDEX. It lives HERE because `vike-cli`
/// sees both crates. The symptom of the drift it prevents is an endless `bad mac` at the daemon, with every
/// test green. `[5]`..`[8]` (the gatewayhub's keys) have no second spelling in any server crate, so no
/// assertion is owed for them: the table's own test anchors which strings they are; `[0]`..`[4]` are the
/// names decision 0288 retired, read by nothing, so no spelling of theirs can drift.
#[test]
fn the_platform_key_table_carries_the_datahub_strategy_lane_keys_spelling() {
    let table = vike_venues::credential_keys::PLATFORM_KEYS;
    assert_eq!(
        table[9],
        vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV,
        "strategy lane, and IN ORDER"
    );
    assert_eq!(vike_venues::credential_keys::datahub_trade_key(), table[9]);
    assert!(vike_venues::credential_keys::is_platform_key(
        vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV
    ));
    // ⚠ A different credential from every other name: a copy-paste that repeated another key here
    // would let that key open the lane.
    assert!(table[..9].iter().all(|name| *name != table[9]), "{table:?}");
}

/// A project directory laid out the way the loader expects: `<project>/settings/`, with a store in
/// it unless a case says otherwise.
///
/// ⚠ **A BOUND `tempfile::TempDir`, not `<system-temp>/vike-cli-node-<tag>-<pid>`.** A
/// fixed-tag-plus-pid directory in the shared system temp root is unique enough on one box and
/// OWNED by nobody: the `Drop` impl that stood here cleaned up on the ordinary path, but a SIGKILL,
/// an OOM or a Ctrl-C — all of which the CI box has seen — leaves it there forever, `/tmp` being 1777
/// sticky. The verdict flip is at the RECEIVING end: given a foreign-owned leftover of the same
/// name, the old `remove_dir_all` failed `EACCES` and `let _ =` swallowed it, `create_dir_all`
/// returned **Ok** because the directory already existed, and the first store write then panicked
/// `PermissionDenied` naming a `/tmp` path and no property of any test. A `TempDir` claims its name
/// `O_EXCL` and removes it on the panic path too.
struct Case {
    /// BOUND, so its `Drop` removes the tree even on the panic path.
    root: tempfile::TempDir,
}

impl Case {
    fn new(tag: &str) -> Self {
        // The tag survives as the directory's PREFIX — what makes a leftover from a kill signal
        // (which no `Drop` can answer) attributable to a case rather than anonymous.
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-node-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).unwrap();
        let case = Case { root };
        // ⚠ **A settings write REFUSES with no database at all** (`docs/decisions/0086`:
        // `vike_secrets::write_setting_row_in`'s `RowWriteError::NoDatabase` — the primitive never
        // CREATES the store, an open question the record itself leaves unanswered). `backend
        // mint-missing` writes `config.tradehub_addr`, so every case here needs a (possibly EMPTY)
        // settings database already present, exactly as a real box would after `vike-cli secrets
        // init` — which is a PREREQUISITE for `backend mint-missing` on a fresh box. Cases proving an EARLIER refusal (the absent
        // hub-key store, which is checked before this write is ever reached) are unaffected either
        // way.
        vike_secrets::plant_settings_rows(
            &case.settings(),
            &vike_secrets::StoredSettings { settings: vec![], ..Default::default() },
        )
        .expect("a fresh settings database plants");
        // ⚠ `plant_settings_rows` is a `test-support` fixture, not the production writer, and
        // creates the database with the PLATFORM DEFAULT mode (loose on a the build runner umask) —
        // unlike `vike_secrets::db`'s own creation path, which stamps 0600 explicitly. Left alone,
        // every case here would trip the box's own PLAINTEXT-and-readable-beyond-its-owner
        // permission warning on every run, which is a fixture defect this file must not paper over
        // by loosening the `--help` case's "no diagnostic on stdout/stderr" assertion instead.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let db = vike_secrets::db_path_in(&case.settings());
            if db.is_file() {
                std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600))
                    .expect("chmod the planted database");
            }
        }
        case
    }

    /// `<project>` — the directory `settings/` sits in, and the one the binary derives every other
    /// project-relative path from.
    fn project(&self) -> &std::path::Path {
        self.root.path()
    }

    fn settings(&self) -> PathBuf {
        self.project().join("settings")
    }

    /// Write [`SAMPLE`]'s unrelated VENUE credentials into the settings database's `credential`
    /// table, through the production writer's record-less body
    /// (`vike_secrets::save_credentials_to_store_unjournalled`, so the seed writes no ledger line
    /// the journal assertions below would have to discount) — so every preservation claim below
    /// has something to
    /// preserve, in the table the `backend` verbs must NOT write (`docs/decisions/0051`: hub keys
    /// are their own table, the venue credentials another).
    fn seed_sample(&self) {
        let rows: Vec<(String, String)> =
            SAMPLE.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        vike_secrets::save_credentials_to_store_unjournalled(
            &self.settings(),
            vike_secrets::StoreTable::Credential,
            &rows,
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("seed the sample venue credentials");
    }

    /// Aim `config.datahub_addr` at `addr`: `status` asks the datahub, and a the CI box lane has a real one
    /// on the default address.
    fn aim_datahub_at(&self, addr: &str) {
        vike_secrets::plant_settings_rows(
            &self.settings(),
            &vike_secrets::StoredSettings {
                settings: vec![vike_secrets::SettingRow {
                    section: "config".to_string(),
                    key: "datahub_addr".to_string(),
                    value: format!("\"{addr}\""),
                }],
                ..Default::default()
            },
        )
        .expect("plant the datahub_addr row");
    }

    /// The VENUE credential table as a map — what a `backend` verb must leave untouched.
    fn venue_credentials(&self) -> std::collections::HashMap<String, String> {
        self.store().read_all().map(|r| r.secrets.into_map()).unwrap_or_default()
    }

    /// The `hub_key` table as a map — every name it holds.
    fn hub_keys(&self) -> std::collections::HashMap<String, String> {
        self.store().hub_keys(|_| true).map(|r| r.secrets.into_map()).unwrap_or_default()
    }

    /// The scratch project's store, read under the full scope — the handle every read above takes.
    fn store(&self) -> vike_secrets::CredentialStore {
        vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(self.settings())),
            vike_secrets::CredentialScope::Full,
        )
    }

    /// One settings row's value, dotted-key spelled (`"config.tradehub_addr"`) — the row-native
    /// replacement for reading a settings FILE back (`docs/decisions/0086`). `None` when no such
    /// row exists, whether because nothing wrote it or because there is no database at all.
    fn read_setting(&self, key: &str) -> Option<String> {
        let (section, leaf) = key.split_once('.')?;
        let rows = vike_secrets::read_settings_in(&self.settings()).ok()?;
        rows.rows()?
            .settings
            .iter()
            .find(|r| r.section == section && r.key == leaf)
            .map(|r| r.value.clone())
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

    /// One key's value out of the `hub_key` table, for the "the printed id IS this key's id"
    /// assertions — read back through the same resolver production code uses. ⚠ It never reaches
    /// an assertion MESSAGE — a failing test must not print a credential into CI's log any more
    /// than the command may.
    fn stored(&self, key: &str) -> Option<String> {
        self.hub_keys().get(key).cloned()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// **The POSITIVE proof for [`Case`]**, because a green run of the cases below proves neither half:
/// they pass with the old fixture too, on every box where nobody has yet left a leftover of the
/// same name or dropped a `/tmp/vike.toml`.
///
/// Half one — the PROJECT this fixture resolves is its own root, asserted behaviourally:
/// `vike_config::load`'s layer 0 stats `<settings-dir>/../vike.toml` and refuses hard on anything
/// but `NotFound`, so planting that file inside THIS case's root must be what the shipped binary
/// refuses, naming the planted path.
///
/// Half two — OWNERSHIP: dropping the handle removes the tree, which is the property the old `Drop`
/// impl also had on the PASSING path and did not have on the killed one.
#[test]
fn the_case_fixture_is_owned_and_its_project_is_its_own_tempdir() {
    let project;
    {
        let c = Case::new("hermetic");
        project = c.project().to_path_buf();
        assert_eq!(
            c.settings().parent(),
            Some(project.as_path()),
            "the project the binary infers must be this case's own TempDir"
        );
        c.aim_datahub_at(DEAD_ADDRESS);
        // The control: with nothing planted the probe finds nothing and says nothing about it.
        assert!(
            !stderr(&c.run(&["status"])).contains("vike.toml"),
            "an unplanted case must not be refused for a file nobody wrote"
        );

        let planted = project.join("vike.toml");
        std::fs::write(&planted, "").expect("plant the refused file");
        let out = c.run(&["status"]);
        assert!(!out.status.success(), "a `vike.toml` beside the settings directory is refused");
        let err = stderr(&out);
        assert!(
            err.contains(&planted.display().to_string()),
            "the refusal must name the file this case planted — that is what proves the probe \
             resolved inside this case's own TempDir: {err}"
        );
    }
    assert!(
        !project.exists(),
        "the fixture must remove itself when it goes out of scope: {} survived",
        project.display()
    );
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Unrelated venue credentials, so every preservation claim below has something to preserve.
const SAMPLE: [(&str, &str); 2] =
    [("BINANCE_LIVE_API_KEY", "key-abcd1234"), ("BINANCE_LIVE_API_SECRET", "sup3r-s3cr3t")];

/// The sample, as the map [`Case::venue_credentials`] must still read back.
fn sample_map() -> std::collections::HashMap<String, String> {
    SAMPLE.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The verb is registered: it reaches the dispatcher, prints its own usage to STDOUT, and succeeds.
///
/// A non-zero `--help` breaks `set -e` and every packaging smoke test, and a unit test of the parser
/// cannot see either the status or the stream — which is the whole reason this crate has
/// `tests/cli_verbs/help_cli.rs`.
#[test]
fn backend_is_a_registered_verb_whose_help_is_a_success_on_stdout() {
    let c = Case::new("help");
    let out = c.run(&["--help"]);
    assert!(out.status.success(), "--help must succeed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("mint-missing") && text.contains("connect"), "{text}");
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

/// The write is JOURNALLED, with key NAMES and no value — the fourth question
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s `GROWTH_GUIDANCE` asks of every credential
/// writer, asserted against the ledger this run actually appended to.
#[test]
fn the_mint_is_journalled_with_names_and_no_value() {
    let c = Case::new("journal");
    c.seed_sample();
    assert!(c.run(&["mint-missing"]).status.success());
    let [observe_name, control_name] = vike_venues::credential_keys::gatewayhub_key_pair();

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
    assert!(found.contains(observe_name) && found.contains(control_name), "{found}");
    let observe = c.stored(observe_name).unwrap();
    assert!(!found.contains(observe.as_str()), "A KEY REACHED THE CHANGE JOURNAL");
    // …and the settings edits are in the same ledger, which is what makes an after-the-fact "who
    // opened this node's socket" answerable at all.
    assert!(found.contains("set_setting"), "{found}");
}

/// **A key may not be given on the command line, on any verb here.**
#[test]
fn a_key_never_goes_on_the_command_line() {
    let c = Case::new("argv");
    c.seed_sample();

    // `mint-missing` takes no value at all: a stray argument is an unknown option, not a key.
    let out = c.run(&["mint-missing", "deadbeef"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown option"), "{}", stderr(&out));

    // `connect`'s ONE positional is the HOST; a second argument is refused, and the refusal says no
    // key goes there.
    let out = c.run(&["connect", "the CI box", "deadbeef"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("ONE host"), "{err}");
    assert!(err.contains("never goes on the command line"), "{err}");
}

/// `connect` on a box that holds no device key and registers none is refused before it records
/// anything (decision 0288: nothing else signs). And the `config.node_addr` that `connect` records is the
/// middle rung of every `--node` (variant A, P4 #13): with no flag and no row `trade status` and `trade
/// health` are refused on the usage rung naming `vike-cli backend connect`, and with the row they dial it,
/// so what refuses them on this keyless box is the device key, for that address.
#[test]
fn a_stored_dial_address_is_the_node_a_trade_verb_dials_without_the_flag() {
    let c = Case::new("node-required");
    c.seed_sample();
    let out = c.run(&["connect", "127.0.0.1", "--port", "1", "--no-tunnel"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("holds no device key"), "{}", stderr(&out));
    assert_eq!(c.read_setting("config.node_addr"), None, "a refused connect recorded an address");
    let trade = |verb: &str| {
        Command::new(BIN)
            .args(["trade", verb])
            .env("VIKE_SETTINGS_DIR", c.settings())
            .stdin(Stdio::null())
            .output()
            .expect("the vike-cli binary must run")
    };
    for verb in ["status", "health"] {
        let out = trade(verb);
        assert_eq!(out.status.code(), Some(2), "`trade {verb}`: {}", stderr(&out));
        assert!(stderr(&out).contains("error [dial.node.unset]"), "{}", stderr(&out));
        assert!(stderr(&out).contains("`vike-cli backend connect`"), "{}", stderr(&out));
    }
    vike_secrets::plant_settings_rows(
        &c.settings(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "node_addr".to_string(),
                value: "\"127.0.0.1:1\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("plant the node_addr row");
    for verb in ["status", "health"] {
        let out = trade(verb);
        let err = stderr(&out);
        assert!(err.contains("error [dial.device_key.not_in_store]"), "`trade {verb}`: {err}");
        assert!(err.contains("no device key on this box"), "`trade {verb}`: {err}");
    }
}

/// **A flag typed on the wrong box is refused by name**, and says which box it belongs on: silently
/// ignored, it would leave an operator believing a thing was configured that was not.
#[test]
fn a_flag_for_the_other_box_is_refused_by_name() {
    let c = Case::new("scope");
    c.seed_sample();
    let out = c.run(&["mint-missing", "--no-tunnel"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("--no-tunnel"), "{err}");
    assert!(err.contains("CLIENT"), "{err}");
}

/// `status` on a box that has never attached reports exactly that, and says what would fix it —
/// rather than printing an empty table or dialling nothing.
#[test]
fn status_on_an_unattached_box_names_what_is_missing() {
    let c = Case::new("status");
    c.seed_sample();
    c.aim_datahub_at(DEAD_ADDRESS);
    let out = c.run(&["status"]);
    assert!(!out.status.success(), "there is no node to ask");
    let text = stdout(&out);
    assert!(text.contains("dial address: NONE"), "{text}");
    assert!(text.contains("absent"), "both key planes must be reported: {text}");
    assert!(stderr(&out).contains("backend connect"), "{}", stderr(&out));
}

/// **`status` has a datahub row**: the address it dials, the key it signs with, and what answered,
/// whatever the node said. A dead datahub is a line and never the verb's failure, so the exit is the
/// same rung as with no datahub row at all.
#[test]
fn status_reports_the_datahub_whatever_the_node_says() {
    let c = Case::new("status-datahub");
    c.seed_sample();
    c.aim_datahub_at(DEAD_ADDRESS);
    let out = c.run(&["status"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "the node's rung, not the datahub's: {out:?}");
    assert!(text.contains(&format!("datahub address: {DEAD_ADDRESS}")), "{text}");
    assert!(text.contains("datahub key"), "{text}");
    assert!(text.contains(&format!("no datahub answered at {DEAD_ADDRESS}")), "{text}");

    // A datahub that answers the handshake and predates the verb: read-only, and reached.
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::proto::Request;
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the verb dials");
        let _: HandshakeFrame = read_frame(&mut stream).expect("the client opens with Hello");
        let welcome = HandshakeReply::Welcome {
            proto_version: PROTO_VERSION,
            features: Vec::new(),
            nonce: None,
        };
        write_frame(&mut stream, &welcome).expect("the welcome");
        while read_frame::<_, Request>(&mut stream).is_ok() {}
    });
    let c = Case::new("status-datahub-up");
    c.seed_sample();
    c.aim_datahub_at(&addr);
    let out = c.run(&["status"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "there is still no node to ask: {out:?}");
    assert!(text.contains(&format!("datahub reached at {addr}")), "{text}");
}

/// `disconnect` with no recorded tunnel is a SUCCESS that says so. It is the idempotent shape: an
/// operator running it twice, or on a box where the tunnel was raised by hand, must not be handed a
/// failure for a state they asked for.
#[test]
fn disconnect_with_no_recorded_tunnel_is_a_success_that_says_so() {
    let c = Case::new("disc");
    c.seed_sample();
    let out = c.run(&["disconnect"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no tunnel recorded"), "{}", stdout(&out));
}

/// Every hub's full key name `mint-missing` owes a project (decision 0280): the tradehub, datahub and
/// gatewayhub pairs and the two order-scope keys, never an admin key (`[4]` and `[8]`).
fn every_minted_hub_key() -> Vec<&'static str> {
    let table = vike_venues::credential_keys::PLATFORM_KEYS;
    table.iter().enumerate().filter(|(i, _)| matches!(i, 5 | 6 | 7 | 9)).map(|(_, n)| *n).collect()
}

/// The strategy builder's key: the deployment row its service reads, named by the table that
/// declares it.
fn builder_key() -> &'static str {
    vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
        .iter()
        .find(|(_, reader, _)| *reader == "vike-strategy-builder")
        .map(|(name, ..)| *name)
        .expect("the table names the builder's key")
}

/// **`mint-missing` mints what is missing and NEVER replaces a key** (decision 0280): a key planted
/// before the run is byte for byte the same after it, every other DAEMON key and the builder's key are
/// minted, no person's pair and no admin key (decision 0288 item 8), this box's three device keys are,
/// no value reaches either stream, the venue rows are untouched, the tradehub's door lands on its
/// loopback default, and a second run mints nothing.
#[test]
fn mint_missing_mints_every_absent_key_keeps_a_held_one_and_a_rerun_mints_nothing() {
    let c = Case::new("mint-missing");
    c.seed_sample();
    let held = vike_venues::credential_keys::PLATFORM_KEYS[5];
    vike_secrets::save_credentials_to_store_unjournalled(
        &c.settings(),
        vike_secrets::StoreTable::HubKey,
        &[(held.to_string(), "held-gatewayhub-observe-key".to_string())],
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("plant a held key");

    let out = c.run(&["mint-missing"]);
    assert!(out.status.success(), "mint-missing failed: {}", stderr(&out));
    let (text, err) = (stdout(&out), stderr(&out));

    assert!(c.stored(held).as_deref() == Some("held-gatewayhub-observe-key"), "a held key changed");
    for name in every_minted_hub_key() {
        let key = c.stored(name).unwrap_or_else(|| panic!("{name} was not minted"));
        assert!(!key.trim().is_empty(), "{name} is blank");
        // secret-absent: a minted hub key's value never reaches a stream
        assert!(!text.contains(key.as_str()) && !err.contains(key.as_str()), "A KEY WAS PRINTED");
    }
    for person_or_admin in [0, 1, 2, 3, 4, 8] {
        let name = vike_venues::credential_keys::PLATFORM_KEYS[person_or_admin];
        assert!(c.stored(name).is_none(), "a person's pair or an admin key was minted: {name}");
    }
    let devices = vike_secrets::access::list_access_keys(&c.settings()).expect("readable");
    let mut granted: Vec<_> =
        devices.iter().map(|r| format!("{}/{}:{}", r.person, r.device, r.role)).collect();
    granted.sort();
    assert_eq!(
        granted,
        ["owner/server:admin", "owner/telegram:control", "server/backtesthub:observe"],
        "this box's device keys"
    );
    let venue = c.venue_credentials();
    let builder = venue.get(builder_key()).cloned().expect("the builder's key was minted");
    assert!(
        // secret-absent: the minted builder key's value never reaches a stream
        !text.contains(builder.as_str()) && !err.contains(builder.as_str()),
        "A KEY WAS PRINTED"
    );
    let mut expected = sample_map();
    expected.insert(builder_key().to_string(), builder);
    assert!(venue == expected, "mint-missing touched a venue credential");
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"127.0.0.1:7879\""));

    let before = (c.hub_keys(), c.venue_credentials());
    let again = c.run(&["mint-missing"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("nothing was minted"), "{}", stdout(&again));
    assert!((c.hub_keys(), c.venue_credentials()) == before, "a rerun changed the store");
}

/// The four shared keys a CLIENT box held before decision 0288 (the tradehub pair, then the datahub
/// pair): none of them reaches a client now. Unix-only, as its one caller.
#[cfg(unix)]
fn client_names() -> [&'static str; 4] {
    let t = vike_venues::credential_keys::PLATFORM_KEYS;
    [t[0], t[1], t[2], t[3]]
}

/// **`connect --server-project` registers this box's device key over ssh, and no private byte
/// leaves** (decision 0288). A stand-in `ssh` on PATH runs the remote command here, as ssh would on
/// the host: the server project's own `bin/vike-cli access add`, the PUBLIC key on its stdin. Port 1
/// answers nothing, so the verify fails; everything before it has landed: the client holds its own
/// pair, the server lists its public half as `owner/<device>` at the role admin, no shared key
/// reached the client, and a second run registers the same key again without a refusal.
#[cfg(unix)]
#[test]
fn connect_registers_the_device_key_over_ssh_and_sends_no_private_byte() {
    use vike_cli::test_support::{output_retrying_etxtbsy, plant_engine};

    let server = Case::new("register-server");
    assert!(server.run(&["mint-missing"]).status.success());
    std::fs::create_dir_all(server.project().join("bin")).expect("bin");
    std::os::unix::fs::symlink(BIN, server.project().join("bin").join("vike-cli")).expect("link");
    // The stand-in `ssh`, planted through the crate's one plant (`plant_engine`) in a directory of
    // its own, then SETTLED: exec'd once with a no-op command, retrying only `ETXTBSY`, so the real
    // runs below cannot meet the planted file's write descriptor. It runs its last argument with its
    // stdin, as ssh does; `hostname` is found on the system's PATH beside it.
    let fake = tempfile::Builder::new().prefix("vike-cli-node-ssh-").tempdir().expect("tempdir");
    let ssh = plant_engine(
        fake.path(),
        "ssh",
        "#!/bin/sh\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n",
    );
    let settled = output_retrying_etxtbsy("the stand-in ssh", || {
        let mut probe = Command::new(ssh.path());
        probe.arg("true");
        probe
    });
    assert!(settled.status.success(), "the stand-in ssh does not run: {settled:?}");
    let path = format!("{}:/usr/bin:/bin", fake.path().display());
    let project = server.project().to_str().expect("utf-8 temp path").to_string();
    let connect = |client: &Case| {
        Command::new(BIN)
            .args(["backend", "connect", "127.0.0.1", "--port", "1", "--no-tunnel"])
            .args(["--server-project", project.as_str()])
            .env("VIKE_SETTINGS_DIR", client.settings())
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .expect("the vike-cli binary must run")
    };

    let client = Case::new("register-client");
    let out = connect(&client);
    let both = format!("{}{}", stdout(&out), stderr(&out));
    let own = vike_secrets::access::device_key_load(&client.settings())
        .expect("readable")
        .expect("the client made its device key");
    let listed = vike_secrets::access::list_access_keys(&server.settings()).expect("readable");
    let row = listed
        .iter()
        .find(|r| r.key_id == own.key_id)
        .unwrap_or_else(|| panic!("the server does not list {}: {listed:?}\n{both}", own.key_id));
    assert_eq!(
        (row.person.as_str(), row.device.as_str(), row.role.as_str()),
        ("owner", own.device.as_str(), "admin")
    );
    assert!(row.revoked_ms.is_none());
    let private = format!("{:02x?}", own.private_key);
    assert!(!both.contains(&private) && both.contains(&own.key_id), "{both}");
    for name in client_names() {
        assert!(client.stored(name).is_none(), "a shared key reached the client: {name}");
    }

    let again = connect(&client);
    let again_out = format!("{}{}", stdout(&again), stderr(&again));
    assert!(again_out.contains("registered this box's device key"), "{again_out}");
    let listed = vike_secrets::access::list_access_keys(&server.settings()).expect("readable");
    assert_eq!(listed.iter().filter(|r| r.key_id == own.key_id).count(), 1, "{listed:?}");
}

/// A door an operator set is the operator's: `mint-missing` writes `config.tradehub_addr` only where
/// no row names it, and an absent store is refused with nothing created.
#[test]
fn mint_missing_keeps_an_operators_door_and_refuses_an_absent_store() {
    let c = Case::new("mint-door");
    // The operator's door, as `vike-cli config set config.tradehub_addr 127.0.0.1:9100` leaves it.
    vike_secrets::plant_settings_rows(
        &c.settings(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "tradehub_addr".to_string(),
                value: "\"127.0.0.1:9100\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("plant the operator's door");
    let out = c.run(&["mint-missing"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"127.0.0.1:9100\""));

    let none = Case::new("mint-nostore");
    let db = vike_secrets::db_path_in(&none.settings());
    std::fs::remove_file(&db).expect("remove the planted database");
    let out = none.run(&["mint-missing"]);
    assert!(!out.status.success(), "an absent store must not succeed");
    let err = stderr(&out);
    assert!(err.contains("no hub-key store"), "{err}");
    assert!(err.contains("secrets init"), "it must name the route — the one creator: {err}");
    assert!(err.contains("vike.db"), "…and the store it is about, the settings database: {err}");
    assert!(!db.exists(), "mint-missing created a store");
}
