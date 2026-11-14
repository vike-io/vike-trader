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

/// **The equality assertion `vike_venues::credential_keys::PLATFORM_KEYS` owes.**
///
/// `crates/vike-tradehub-client/src/auth.rs`'s doc carries the table of every crate that spells the
/// two hub key names and warns that *"adding a fifth copy without adding its equality assertion
/// re-opens the gap this table exists to close"*. `PLATFORM_KEYS` is such a copy — it must spell the
/// names, because it is a names-only table and there is nothing else for it to hold — and this is
/// its payment.
///
/// It lives HERE rather than beside the table because `vike-cli` is the lowest crate that can see
/// both: `vike-model` is layer 10 and cannot see `vike-tradehub-client` at layer 25, and must not —
/// the layer gate would refuse the edge, and an imported constant would make the settings registry
/// blind to the read anyway, which is the whole reason the duplication exists.
///
/// The symptom of the drift it prevents is the one worth remembering: an endless `bad mac` at the
/// node, with every test in every crate green.
#[test]
fn the_platform_key_table_is_the_servers_own_spelling() {
    let table = vike_venues::credential_keys::PLATFORM_KEYS;
    assert_eq!(table[0], vike_tradehub_client::auth::OBSERVE_KEY_ENV, "observe, and IN ORDER");
    assert_eq!(table[1], vike_tradehub_client::auth::CONTROL_KEY_ENV, "control, and IN ORDER");
    // …and the CLI's own copies, which are what the writer actually hands to the store. Three
    // spellings, one value, all three asserted — the third pairing is `vike_client::hubkeys`' own
    // `both_key_names_match_the_servers_own`, and this is the leg it does not cover.
    assert!(vike_venues::credential_keys::is_platform_key(
        vike_tradehub_client::auth::OBSERVE_KEY_ENV
    ));
    assert!(vike_venues::credential_keys::is_platform_key(
        vike_tradehub_client::auth::CONTROL_KEY_ENV
    ));
}

/// The SAME payment, for the DATAHUB pair that joined `PLATFORM_KEYS` on 2026-09-08.
///
/// ⚠ A second service's hub keys are a second copy, and the rule the test above states does not
/// weaken because the table already existed: *"adding a copy without adding its equality assertion
/// re-opens the gap this table exists to close"*. `vike_hub_protocol::auth`'s
/// `DATAHUB_OBSERVE_KEY_ENV` / `DATAHUB_CONTROL_KEY_ENV` are that pair's reference spelling, and
/// this is what holds `PLATFORM_KEYS`'s `concat!`-split copies equal to them.
///
/// Indices `[2]`/`[3]` rather than a `contains` — the tradehub pair is pinned by index above, the
/// order is stated as load-bearing at the table, and an assertion that only checked membership
/// would pass a table that had silently reordered under the test above.
#[test]
fn the_platform_key_table_carries_the_datahub_servers_own_spelling() {
    let table = vike_venues::credential_keys::PLATFORM_KEYS;
    assert_eq!(table[2], vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV, "observe, and IN ORDER");
    assert_eq!(table[3], vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV, "control, and IN ORDER");
    assert!(vike_venues::credential_keys::is_platform_key(
        vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV
    ));
    assert!(vike_venues::credential_keys::is_platform_key(
        vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV
    ));
    // ⚠ The two pairs are DISJOINT. A copy-paste that made the datahub rows repeat the tradehub
    // ones would satisfy every assertion above taken singly, and would route `secrets set` for a
    // datahub key at the tradehub's command.
    assert_ne!(table[0], table[2], "the two services' observe keys must differ");
    assert_ne!(table[1], table[3], "the two services' control keys must differ");
}

/// The SAME payment, for the datahub's STRATEGY-LANE key (`PLATFORM_KEYS[9]`, wave 2).
///
/// `vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV` is its reference spelling (a second copy, `concat!`-split
/// for the registry-harvest reason both tables give), so the duplication rule applies and this is what
/// holds the two equal, BY INDEX. `[7]` and `[8]` (the gatewayhub's `Trade` and `Admin` keys) have no
/// second spelling in any server crate, so no assertion is owed for them: the table's own test anchors
/// which strings they are.
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
    // ⚠ A different credential from every other name, the datahub's own pair included: a copy-paste
    // that repeated the control key here would let the pair open the lane.
    assert_ne!(table[9], vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV);
    assert_ne!(table[9], vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV);
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
        // CREATES the store, an open question the record itself leaves unanswered). `backend setup`
        // writes `config.tradehub_addr` before it mints anything, so every case here needs a
        // (possibly EMPTY) settings database already present, exactly as a real box would after
        // `vike-cli secrets init` — which is now a PREREQUISITE for `backend setup` on a fresh
        // box, not merely an alternative path. Cases proving an EARLIER refusal (the absent
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

fn observe_name() -> &'static str {
    vike_tradehub_client::auth::OBSERVE_KEY_ENV
}

fn control_name() -> &'static str {
    vike_tradehub_client::auth::CONTROL_KEY_ENV
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
/// The refusal names `vike-cli secrets init`, the one creator of the settings database,
/// which is what 0036's fence is actually about: the operator creates the store, not this verb.
#[test]
fn setup_refuses_an_absent_store_and_names_how_to_make_one() {
    let c = Case::new("nostore");
    // ⚠ **`Case::new` plants a settings DATABASE unconditionally (0086: a settings write needs one
    // already present), and that database is REMOVED here rather than kept.** The database IS the
    // hub-key store the moment it exists, so the scenario this test is about — no store AT ALL to
    // write into, `Backend::Absent` — is reproduced by removing it.
    let db = vike_secrets::db_path_in(&c.settings());
    std::fs::remove_file(&db).expect("remove the planted database to reach the absent-store case");
    let out = c.run(&["setup"]);
    assert!(!out.status.success(), "an absent hub key store must not succeed");
    let err = stderr(&out);
    assert!(err.contains("no hub-key store"), "{err}");
    assert!(err.contains("secrets init"), "it must name the route — the one creator: {err}");
    assert!(err.contains("vike.db"), "…and the store it is about, the settings database: {err}");
    assert!(!db.exists(), "setup must not have created a store");
    // …and nothing else was written either: a refusal is total.
    assert!(
        c.read_setting("config.tradehub_addr").is_none(),
        "a settings row was written by a refused setup"
    );
}

/// **THE central test.** `setup` mints both keys, and:
///
/// - each printed `key_id` IS the fingerprint of the key that landed in the store — so the two-box
///   comparison the design rests on is comparing the right thing;
/// - **neither key VALUE appears on stdout or stderr**, which is the rule the whole module exists to
///   hold;
/// - every VENUE credential row is preserved — the hub keys are their own table, exercised through
///   this call site rather than assumed from the writer's own suite;
/// - `config.tradehub_addr` lands with the default;
/// - `flags.tradehub_control` is NOT written, because `--control` was not passed.
#[test]
fn setup_mints_two_keys_prints_their_ids_and_never_a_key() {
    let c = Case::new("mint");
    c.seed_sample();

    let out = c.run(&["setup"]);
    assert!(out.status.success(), "setup failed: {}", stderr(&out));
    let text = stdout(&out);
    let err = stderr(&out);

    let observe = c.stored(observe_name()).expect("the observe key must be in the store");
    let control = c.stored(control_name()).expect("the control key must be in the store");
    assert_eq!(observe.len(), 64, "a hub key is 32 bytes, hex encoded");
    assert_ne!(observe, control, "the two keys must be independently minted");

    // The ids, and the values NOT.
    for key in [&observe, &control] {
        let id = vike_hub_protocol::auth::key_fingerprint(key.as_bytes());
        assert!(text.contains(&id), "a printed id is missing (ids: {text})");
        assert!(!text.contains(key.as_str()), "A KEY REACHED STDOUT");
        assert!(!err.contains(key.as_str()), "A KEY REACHED STDERR");
    }
    assert!(text.contains(observe_name()) && text.contains(control_name()), "{text}");

    // Row preservation: the venue credentials are untouched, and no hub key landed among them.
    assert!(c.venue_credentials() == sample_map(), "a backend verb touched the VENUE credentials");

    // The settings half — a ROW now, not a file (`docs/decisions/0086`).
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"127.0.0.1:7879\""));
    assert!(
        c.read_setting("flags.tradehub_control").is_none(),
        "--control was NOT passed and the flag must be untouched"
    );
    assert!(text.contains("control was not written"), "and it must SAY so: {text}");
    assert!(text.contains("ON unless a row says false"), "with the default (0280): {text}");

    // The restart line, which is what makes any of it take effect, for both server installs (decision 0260).
    assert!(text.contains("bin/vikectl restart tradehub"), "{text}");
    assert!(text.contains("systemctl restart vike-tradehub"), "{text}");
}

/// `--control` is the whole consent, and it is the only thing that writes the flag.
#[test]
fn control_is_written_only_when_it_is_asked_for() {
    let c = Case::new("control");
    c.seed_sample();
    let out = c.run(&["setup", "--control", "--addr", "0.0.0.0:9100"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(c.read_setting("flags.tradehub_control").as_deref(), Some("true"));
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"0.0.0.0:9100\""));
    let text = stdout(&out);
    assert!(text.contains("control is ARMED"), "{text}");
}

/// **A second `setup` refuses, and leaves the store UNCHANGED.** A re-run that silently
/// rotated would detach every client already holding the old keys, with the symptom appearing on
/// those other boxes as an auth denial that reads like a revoked credential.
#[test]
fn a_second_setup_refuses_without_rotate_and_writes_nothing() {
    let c = Case::new("rerun");
    c.seed_sample();
    assert!(c.run(&["setup"]).status.success());
    let before = (c.hub_keys(), c.venue_credentials());

    let out = c.run(&["setup"]);
    assert!(!out.status.success(), "a silent re-mint is the failure this refusal exists for");
    let err = stderr(&out);
    assert!(err.contains("--rotate"), "{err}");
    assert!(err.contains("stops working"), "the cost must be stated: {err}");
    assert!(
        (c.hub_keys(), c.venue_credentials()) == before,
        "a refused setup must not touch the store"
    );
}

/// `--rotate` replaces BOTH keys, preserves every unrelated row, and still prints no key.
#[test]
fn rotate_replaces_both_keys_and_preserves_the_rest_of_the_store() {
    let c = Case::new("rotate");
    c.seed_sample();
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
    // The venue credentials are untouched.
    assert!(c.venue_credentials() == sample_map(), "a rotation touched the VENUE credentials");
}

/// The write is JOURNALLED, with key NAMES and no value — the fourth question
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s `GROWTH_GUIDANCE` asks of every credential
/// writer, asserted against the ledger this run actually appended to.
#[test]
fn the_mint_is_journalled_with_names_and_no_value() {
    let c = Case::new("journal");
    c.seed_sample();
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
    c.seed_sample();

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

/// **`connect --manual` files the datahub's OBSERVE key beside the node's pair and records where that
/// datahub is, and no line of its stdin can reach the key that writes the datahub.** Measured on the
/// Windows client on 2026-11-07: a laptop `connect` set up had no datahub reads and no `gatewayhub`
/// row. Port 1 answers nothing, so the verify fails; everything before the round trip has landed.
#[test]
fn connect_manual_carries_the_datahub_observe_key_and_nothing_that_can_write() {
    let observe = vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV;
    let control = vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV;
    let line = ["connect", "127.0.0.1", "--port", "1", "--no-tunnel", "--manual"];

    // A pipe written before the third line existed still works, and carries no datahub key.
    let c = Case::new("datahub-key");
    c.seed_sample();
    let out = c.run_with_stdin(&line, Some("node-obs\nnode-ctl\n"));
    assert!(!c.hub_keys().contains_key(observe), "{}", stderr(&out));
    let node_observe = vike_tradehub_client::auth::OBSERVE_KEY_ENV;
    assert_eq!(c.hub_keys().get(node_observe).map(String::as_str), Some("node-obs"));

    let out = c.run_with_stdin(&line, Some("node-obs\nnode-ctl\nhub-obs\nhub-ctl-never-read\n"));
    let keys = c.hub_keys();
    assert_eq!(keys.get(observe).map(String::as_str), Some("hub-obs"), "{}", stderr(&out));
    assert!(!keys.contains_key(control), "the datahub's control key is not this verb's: {keys:?}");
    assert!(!keys.values().any(|v| v == "hub-ctl-never-read"), "{keys:?}");
    assert_eq!(c.read_setting("config.node_addr").as_deref(), Some("\"127.0.0.1:1\""));
    assert_eq!(c.read_setting("config.datahub_addr").as_deref(), Some("\"127.0.0.1:7878\""));
    let both = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!both.contains("hub-obs") && !both.contains("node-ctl"), "a key was echoed: {both}");

    // A different key is refused like the node's: two ids, no key, and the store as it was.
    let out = c.run_with_stdin(&line, Some("node-obs\nnode-ctl\nanother-hub-obs\n"));
    let err = stderr(&out);
    assert!(err.contains(observe) && err.contains("--replace"), "{err}");
    assert!(!err.contains("another-hub-obs"), "a key was echoed: {err}");
    assert_eq!(c.hub_keys().get(observe).map(String::as_str), Some("hub-obs"));
}

/// **A blank second line says what is true of the store.** A write key the store already holds is not
/// touched and keeps working, so "can place nothing" was false of that box; on a box holding none it
/// stays the word.
#[test]
fn a_blank_control_line_says_whether_a_held_write_key_stays() {
    let c = Case::new("blank-control");
    c.seed_sample();
    let line = ["connect", "127.0.0.1", "--port", "1", "--no-tunnel", "--manual"];

    let out = c.run_with_stdin(&line, Some("node-obs\n\n"));
    assert!(stdout(&out).contains("can place nothing"), "{}", stdout(&out));

    let out = c.run_with_stdin(&line, Some("node-obs\nnode-ctl\n"));
    assert!(!stdout(&out).contains("second stdin line"), "both lines given: {}", stdout(&out));

    let control = vike_tradehub_client::auth::CONTROL_KEY_ENV;
    let out = c.run_with_stdin(&line, Some("node-obs\n\n"));
    let text = stdout(&out);
    assert!(!text.contains("can place nothing"), "{text}");
    assert!(text.contains("already holds stays") && text.contains("can still place"), "{text}");
    assert!(!text.contains("node-ctl"), "a key was echoed: {text}");
    assert_eq!(c.hub_keys().get(control).map(String::as_str), Some("node-ctl"), "{text}");
}

/// `connect` records `config.node_addr` for `backend status`. `trade status` and `trade health` keep
/// refusing without `--node` with that address stored, which is the behaviour the help used to deny.
#[test]
fn a_stored_dial_address_does_not_make_node_optional() {
    let c = Case::new("node-required");
    c.seed_sample();
    let line = ["connect", "127.0.0.1", "--port", "1", "--no-tunnel", "--manual"];
    let out = c.run_with_stdin(&line, Some("node-obs\nnode-ctl\n"));
    assert_eq!(c.read_setting("config.node_addr").as_deref(), Some("\"127.0.0.1:1\""), "{out:?}");
    for verb in ["status", "health"] {
        let out = Command::new(BIN)
            .args(["trade", verb])
            .env("VIKE_SETTINGS_DIR", c.settings())
            .stdin(Stdio::null())
            .output()
            .expect("the vike-cli binary must run");
        assert_eq!(out.status.code(), Some(2), "`trade {verb}`: {}", stderr(&out));
        assert!(stderr(&out).contains("--node <host:port> is required"), "{}", stderr(&out));
    }
}

/// **A flag typed on the wrong box is refused by name.** `--control` on `connect` is the one that
/// matters: silently ignored, it would leave an operator believing they had armed the daemon's write
/// channel from a laptop, which can arm nothing.
#[test]
fn a_flag_for_the_other_box_is_refused_by_name() {
    let c = Case::new("scope");
    c.seed_sample();
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
    c.seed_sample();
    c.aim_datahub_at(DEAD_ADDRESS);
    let out = c.run(&["status"]);
    assert!(!out.status.success(), "there is no node to ask");
    let text = stdout(&out);
    assert!(text.contains("dial address: NONE"), "{text}");
    assert!(text.contains("absent"), "both key planes must be reported: {text}");
    assert!(stderr(&out).contains("backend connect"), "{}", stderr(&out));
}

/// **`status` has a datahub row**: the address it dials, the observe key it holds, and what answered,
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
    assert!(text.contains("VIKE_DATAHUB_OBSERVE_KEY"), "{text}");
    assert!(text.contains(&format!("no datahub answered at {DEAD_ADDRESS}")), "{text}");

    // A datahub that answers the handshake and predates the verb: read-only, and reached.
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the verb dials");
        let _: Request = read_frame(&mut stream).expect("the client opens with Hello");
        let welcome =
            Response::Welcome { proto_version: PROTO_VERSION, features: Vec::new(), nonce: None };
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
    table.iter().enumerate().filter(|(i, _)| *i != 4 && *i != 8).map(|(_, n)| *n).collect()
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
/// before the run is byte for byte the same after it, every other hub key and the builder's key are
/// minted, no value reaches either stream, the venue rows are untouched, the tradehub's door lands on
/// its loopback default, and a second run mints nothing.
#[test]
fn mint_missing_mints_every_absent_key_keeps_a_held_one_and_a_rerun_mints_nothing() {
    let c = Case::new("mint-missing");
    c.seed_sample();
    vike_secrets::save_credentials_to_store_unjournalled(
        &c.settings(),
        vike_secrets::StoreTable::HubKey,
        &[(observe_name().to_string(), "held-observe-key".to_string())],
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("plant a held key");

    let out = c.run(&["mint-missing"]);
    assert!(out.status.success(), "mint-missing failed: {}", stderr(&out));
    let (text, err) = (stdout(&out), stderr(&out));

    assert!(c.stored(observe_name()).as_deref() == Some("held-observe-key"), "a held key changed");
    for name in every_minted_hub_key() {
        let key = c.stored(name).unwrap_or_else(|| panic!("{name} was not minted"));
        assert!(!key.trim().is_empty(), "{name} is blank");
        assert!(!text.contains(key.as_str()) && !err.contains(key.as_str()), "A KEY WAS PRINTED");
    }
    for admin in [4, 8] {
        let name = vike_venues::credential_keys::PLATFORM_KEYS[admin];
        assert!(c.stored(name).is_none(), "an admin key was minted: {name}");
    }
    let venue = c.venue_credentials();
    let builder = venue.get(builder_key()).cloned().expect("the builder's key was minted");
    assert!(
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

/// The four keys a CLIENT holds, in the order `client-keys` hands them: the tradehub pair, then the
/// datahub pair.
fn client_names() -> [&'static str; 4] {
    let t = vike_venues::credential_keys::PLATFORM_KEYS;
    [t[0], t[1], t[2], t[3]]
}

/// **`client-keys` hands the two pairs to a pipe, `NAME=VALUE`, and nothing a daemon keeps for
/// itself** (decision 0280); `--observe-only` hands the two observe keys alone; stderr carries ids.
#[test]
fn client_keys_hands_the_client_pairs_to_a_pipe_and_no_daemon_key() {
    let c = Case::new("client-keys");
    assert!(c.run(&["mint-missing"]).status.success());

    let out = c.run(&["client-keys"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let expected: Vec<String> =
        client_names().iter().map(|n| format!("{n}={}", c.stored(n).expect("minted"))).collect();
    assert!(stdout(&out).lines().eq(expected.iter().map(String::as_str)), "the hand-over differs");
    let err = stderr(&out);
    for name in client_names() {
        let key = c.stored(name).unwrap();
        assert!(err.contains(&vike_hub_protocol::auth::key_fingerprint(key.as_bytes())), "{err}");
        assert!(!err.contains(key.as_str()), "A KEY REACHED STDERR");
    }
    for daemon_only in [4, 5, 6, 7, 8, 9] {
        let name = vike_venues::credential_keys::PLATFORM_KEYS[daemon_only];
        assert!(!stdout(&out).contains(name), "a daemon key was handed: {name}");
    }

    let out = c.run(&["client-keys", "--observe-only"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let handed: Vec<&str> =
        text.lines().filter_map(|l| l.split_once('=')).map(|(n, _)| n).collect();
    let [observe, _, datahub_observe, _] = client_names();
    assert_eq!(handed, [observe, datahub_observe]);
}

/// **`connect --server-project` takes the keys over ssh and files them, and no value is printed**
/// (decision 0280). A stand-in `ssh` on PATH runs the remote command here, as ssh would on the host:
/// the server project's own `bin/vike-cli backend client-keys`, its stdout the pipe back. Port 1
/// answers nothing, so the verify fails; everything before it has landed.
#[cfg(unix)]
#[test]
fn connect_takes_the_client_keys_from_the_server_over_ssh_and_prints_none() {
    use vike_cli::test_support::{output_retrying_etxtbsy, plant_engine};

    let server = Case::new("handover-server");
    assert!(server.run(&["mint-missing"]).status.success());
    std::fs::create_dir_all(server.project().join("bin")).expect("bin");
    std::os::unix::fs::symlink(BIN, server.project().join("bin").join("vike-cli")).expect("link");
    // The stand-in `ssh`, planted through the crate's one plant (`plant_engine`) in a directory of
    // its own, then SETTLED: exec'd once with a no-op command, retrying only `ETXTBSY`, so the real
    // runs below cannot meet the planted file's write descriptor. Every program it runs is named by
    // absolute path, so the child's PATH is that one directory and nothing reads this process's.
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
    let path = fake.path().display().to_string();
    let project = server.project().to_str().expect("utf-8 temp path").to_string();
    let connect = |client: &Case, extra: &[&str]| {
        let mut line = vec![
            "connect",
            "127.0.0.1",
            "--port",
            "1",
            "--no-tunnel",
            "--server-project",
            project.as_str(),
        ];
        line.extend_from_slice(extra);
        Command::new(BIN)
            .arg("backend")
            .args(&line)
            .env("VIKE_SETTINGS_DIR", client.settings())
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .expect("the vike-cli binary must run")
    };

    let client = Case::new("handover-client");
    let out = connect(&client, &[]);
    let both = format!("{}{}", stdout(&out), stderr(&out));
    for name in client_names() {
        let key = server.stored(name).expect("the server holds it");
        assert_eq!(client.stored(name).as_deref(), Some(key.as_str()), "{name}: {both}");
        assert!(!both.contains(key.as_str()), "A KEY WAS PRINTED");
    }
    for daemon_only in [4, 5, 6, 7, 8, 9] {
        let name = vike_venues::credential_keys::PLATFORM_KEYS[daemon_only];
        assert!(client.stored(name).is_none(), "a daemon key reached the client: {name}");
    }
    assert_eq!(client.read_setting("config.node_addr").as_deref(), Some("\"127.0.0.1:1\""));

    let watcher = Case::new("handover-watcher");
    let out = connect(&watcher, &["--observe-only"]);
    let held: Vec<String> = watcher.hub_keys().into_keys().collect();
    let [observe, control, datahub_observe, datahub_control] = client_names();
    assert!(held.iter().any(|n| n == observe) && held.iter().any(|n| n == datahub_observe));
    assert!(!held.iter().any(|n| n == control || n == datahub_control), "{held:?}");
    assert!(stdout(&out).contains("can place nothing"), "{}", stdout(&out));
}

/// A door an operator set is the operator's: `mint-missing` writes `config.tradehub_addr` only where
/// no row names it, and an absent store is refused with nothing created.
#[test]
fn mint_missing_keeps_an_operators_door_and_refuses_an_absent_store() {
    let c = Case::new("mint-door");
    assert!(c.run(&["setup", "--addr", "127.0.0.1:9100"]).status.success());
    let out = c.run(&["mint-missing"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"127.0.0.1:9100\""));

    let none = Case::new("mint-nostore");
    let db = vike_secrets::db_path_in(&none.settings());
    std::fs::remove_file(&db).expect("remove the planted database");
    let out = none.run(&["mint-missing"]);
    assert!(!out.status.success(), "an absent store must not succeed");
    assert!(stderr(&out).contains("secrets init"), "{}", stderr(&out));
    assert!(!db.exists(), "mint-missing created a store");
}
