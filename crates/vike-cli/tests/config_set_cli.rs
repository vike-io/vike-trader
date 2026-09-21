//! End-to-end tests for `vike-cli config set`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a real settings directory in a temp dir.
//!
//! The unit tests beside the module cover the argv grammar, the two gates and the report wording.
//! These cover what an operator actually does — point the binary at a settings directory and change
//! one key — and, above all, **the thing that commissioned the verb**: that the write shows up in
//! the change journal. No unit test can answer that, because the answer involves the DISPATCHER
//! resolving both the settings directory and its `state/` child off one walk, the writer landing
//! bytes, and the ledger appending a record beside them.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory, passed through
//! `Command::env` on the CHILD rather than `std::env::set_var` (which is unsafe under threads and
//! would leak across this binary's parallel cases). Without it a run on a developer box would
//! resolve the REPO's settings directory — and this verb WRITES, so here the redirect is not merely
//! isolation: it is what keeps a test out of a real `policy.toml`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// A throwaway PROJECT and its `settings/` child — the same fixture shape (and the same two repairs
/// behind it) as `crates/vike-cli/tests/config_cli.rs`'s `Case`: a BOUND `tempfile::TempDir` so a
/// kill signal cannot leave a foreign-owned directory behind, and `$VIKE_SETTINGS_DIR` naming the
/// `settings/` CHILD so `<project>` is the bound root rather than the shared system temp directory.
struct Case {
    root: tempfile::TempDir,
    dir: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-config-set-{tag}-"))
            .tempdir()
            .expect("tempdir");
        let dir = root.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        Case { root, dir }
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    /// Run `vike-cli config set …` with the case's directory as THE settings directory and an
    /// otherwise EMPTY environment, so no ambient variable from the developer's shell can move a
    /// row. Stdin is `/dev/null`, which is the NON-TERMINAL case — the one a script is in, and the
    /// one in which a policy risk ceiling must demand `--confirm` rather than prompt.
    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("config").arg("set").args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", &self.dir);
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }

    /// Everything the change journal wrote under `<settings>/state`, concatenated. The ledger's
    /// layout is its own business, so this reads what it wrote rather than reconstructing a path —
    /// the same shape `crates/vike-cli/tests/node_cli.rs`'s
    /// `the_mint_is_journalled_with_names_and_no_value` uses.
    fn journal(&self) -> String {
        let mut out = String::new();
        let mut stack = vec![self.dir.join("state")];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(t) = std::fs::read_to_string(&p) {
                    out.push_str(&t);
                }
            }
        }
        out
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// **THE PROOF THIS VERB EXISTS FOR.** A change journal that held twenty-nine boot records,
/// twenty-eight venue mounts and ZERO settings writes over two months is what commissioned it — not
/// because the ledger was broken, but because no verb an operator could reach wrote through it. So
/// this asserts the row: the key, both values, the actor, and the outcome that says a running
/// process has NOT picked it up.
#[test]
fn a_write_lands_and_appends_a_set_setting_row_to_the_change_journal() {
    let c = Case::new("journal");
    c.write("config.toml", "tradehub_addr = \"127.0.0.1:7879\"\n");

    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert!(c.read("config.toml").contains("127.0.0.1:7999"), "{}", c.read("config.toml"));

    let ledger = c.journal();
    assert!(ledger.contains("set_setting"), "no set_setting record: {ledger}");
    assert!(ledger.contains("config.tradehub_addr"), "{ledger}");
    assert!(ledger.contains("127.0.0.1:7999"), "the NEW value must be recorded: {ledger}");
    assert!(ledger.contains("127.0.0.1:7879"), "the OLD value must be recorded: {ledger}");
    assert!(ledger.contains("vike-cli"), "the actor must be recorded: {ledger}");
    assert!(
        ledger.contains("pending_restart"),
        "a LOCAL write reaches no applier, so the outcome may not read as applied: {ledger}"
    );
}

/// The operator is TOLD the change is not live, and told what did read the key. A verb that writes
/// a file and says nothing leaves somebody believing a ceiling moved when it will not until a
/// restart.
#[test]
fn the_confirmation_says_it_is_not_live_and_reports_the_read_verdict() {
    let c = Case::new("notlive");
    let out = c.run(&["preferences.log_level", "warn"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("preferences.toml"), "{text}");
    assert!(text.contains("restart"), "the not-live note is the point: {text}");
    // `config show` renders a READ column from the same table; a write that reported nothing about
    // it would be the CLI instance of the failure that column exists to remove.
    assert!(
        text.contains("read by") || text.contains("NOTHING READS"),
        "the READ verdict must be reported: {text}"
    );
}

/// Every untouched byte survives — the invariant `vike_config::set_setting` owns, asserted through
/// the shipped binary because that is the property an operator's hand-commented `policy.toml`
/// depends on.
#[test]
fn an_edit_preserves_every_other_byte_of_the_file() {
    let c = Case::new("comments");
    let before = "# deployment config — hand-tuned\n\n\
                  tradehub_addr = \"127.0.0.1:7879\"  # loopback only\n\n\
                  log_dir = \"/var/tmp/vike-logs\"\n";
    c.write("config.toml", before);
    assert!(c.run(&["config.log_dir", "/var/tmp/other"]).status.success());
    let after = c.read("config.toml");
    assert!(after.contains("# deployment config — hand-tuned"), "{after}");
    assert!(after.contains("# loopback only"), "{after}");
    assert!(after.contains("/var/tmp/other"), "{after}");
    assert!(!after.contains("/var/tmp/vike-logs"), "{after}");
}

/// **The credential fence at the binary.** A credential-shaped key is refused on the USAGE rung,
/// nothing is written, and the refusal names the surface that owns credentials and the channel they
/// arrive on. It is unreachable on today's key set (no settings field is credential-shaped) and is
/// wired so that the day one lands, this verb is already correct.
#[test]
fn a_credential_shaped_key_is_refused_and_writes_nothing() {
    let c = Case::new("secret");
    let out = c.run(&["config.bot_token", "hunter2"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("secrets set"), "{err}");
    assert!(err.contains("stdin"), "{err}");
    assert!(c.read("config.toml").is_empty(), "nothing may be written: {}", c.read("config.toml"));
    assert!(!c.journal().contains("bot_token"), "a verb-level refusal writes no ledger row");
}

/// A policy RISK CEILING demands the retyped key — and on a PIPE, where there is nobody to ask, it
/// refuses by name rather than writing. The exact retype then lands it.
#[test]
fn a_policy_risk_ceiling_needs_the_retyped_key() {
    let c = Case::new("confirm");

    let out = c.run(&["policy.max_notional_per_order", "250"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("--confirm"), "{}", stderr(&out));
    assert!(c.read("policy.toml").is_empty(), "nothing may be written without the confirm");

    let out = c.run(&[
        "policy.max_notional_per_order",
        "250",
        "--confirm",
        "policy.max_account_exposure",
    ]);
    assert_eq!(code(&out), 2, "a MISMATCH is not a confirm: {}", stderr(&out));
    assert!(c.read("policy.toml").is_empty());

    let out =
        c.run(&["policy.max_notional_per_order", "250", "--confirm=policy.max_notional_per_order"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert!(c.read("policy.toml").contains("250"), "{}", c.read("policy.toml"));
}

/// …and the ARMING ceiling in the SAME FILE needs none. That split is the whole reason the confirm
/// is keyed on the KEY rather than on the file — no site in this tree could express it before, and
/// `vike_config::requires_typed_confirm` is where it is now drawn.
#[test]
fn an_arming_ceiling_in_the_same_file_needs_no_confirm() {
    let c = Case::new("arming");
    let venue = vike_model::VENUES[0];
    let key = format!("policy.venues.{venue}");
    let out = c.run(&[key.as_str(), "paper"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert!(c.read("policy.toml").contains(venue), "{}", c.read("policy.toml"));
}

/// An unknown key is refused with the LOADER's own message — the same words a restart would have
/// printed — on the usage rung, with no byte written and a `refused` row in the ledger.
#[test]
fn an_unknown_key_is_refused_by_the_loader_and_the_refusal_is_journalled() {
    let c = Case::new("unknown");
    c.write("config.toml", "tradehub_addr = \"127.0.0.1:7879\"\n");
    let out = c.run(&["config.no_such_setting", "1"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("no_such_setting"), "{}", stderr(&out));
    assert_eq!(
        c.read("config.toml"),
        "tradehub_addr = \"127.0.0.1:7879\"\n",
        "a refused write changes no byte"
    );
    let ledger = c.journal();
    assert!(ledger.contains("refused"), "the refusal must be recorded: {ledger}");
    assert!(ledger.contains("config.no_such_setting"), "{ledger}");
}

/// A first segment that names no settings file is a usage error naming all four spellings and the
/// READ verb — the answer somebody who does not know the vocabulary can act on.
#[test]
fn a_key_naming_no_settings_file_is_refused_with_the_menu() {
    let c = Case::new("section");
    let out = c.run(&["polciy.max_leverage", "3"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    let err = stderr(&out);
    for spelling in ["policy.", "config.", "preferences.", "flags."] {
        assert!(err.contains(spelling), "{err}");
    }
    assert!(err.contains("config show"), "{err}");
}

/// **THE CROSS-PROCESS INTERLOCK, proved against the SHIPPED BINARY and deterministically.**
///
/// This test process takes `vike_config::SETTINGS_LOCK_FILE` in the case's settings directory and
/// holds it, then runs `vike-cli config set`. The binary must refuse, name the held sentinel, and
/// change no byte — and it must do so having WAITED rather than given up instantly, which is the
/// half that keeps a millisecond overlap from becoming an operator-visible failure.
///
/// It is the property an in-process mutex could not have: the other three writers of these files
/// (`crates/vike-app-core/src/tool_views/venues.rs`'s `apply_arming`,
/// `crates/vike-tradehub/src/server.rs`'s `apply_set_setting`, and this verb) run in three
/// different processes. **It also kills cleanly**: remove the lock from
/// `crates/vike-config/src/write.rs`'s `set_setting` and this write lands and exits 0.
///
/// ⚠ It costs the shipped ~3 s budget on purpose. The budget is what the OPERATOR meets, and a test
/// that shortened it would be testing a number nobody ships.
#[test]
fn a_lock_held_by_another_process_refuses_the_write_and_changes_nothing() {
    let c = Case::new("locked");
    let before = "tradehub_addr = \"127.0.0.1:7879\"\n";
    c.write("config.toml", before);

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(c.dir.join(vike_config::SETTINGS_LOCK_FILE))
        .expect("the sentinel opens");
    held.try_lock().expect("this test must be the holder for the assertion below to mean anything");

    let started = std::time::Instant::now();
    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    let waited = started.elapsed();

    assert_eq!(code(&out), 1, "a busy box is a run failure, not a usage error: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(vike_config::SETTINGS_LOCK_FILE), "it must name the held sentinel: {err}");
    assert!(err.contains("NOTHING was written"), "{err}");
    assert_eq!(c.read("config.toml"), before, "a lock refusal changes NO byte");
    assert!(
        waited >= std::time::Duration::from_millis(500),
        "it must WAIT rather than refuse on first contention — waited only {waited:?}"
    );

    drop(held);
    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    assert!(out.status.success(), "the same write lands once released: {}", stderr(&out));
    assert!(c.read("config.toml").contains("7999"));
}

/// **A settings directory the process cannot write** — the state an incident actually produces
/// (a `chown` gone wrong, a read-only remount, a container's mount options).
///
/// The load-bearing disposition: refuse CLEANLY with no byte written, on the run rung, blaming the
/// DIRECTORY the operator aimed at. That last part is a repair — the failure used to be reported
/// against the sibling tmp file (`…/settings/.policy.toml.tmp-4711`), a path the operator has never
/// seen and cannot find, for an edit they aimed at `policy.toml`.
#[test]
#[cfg(unix)]
fn a_settings_directory_that_cannot_be_written_refuses_cleanly() {
    use std::os::unix::fs::PermissionsExt;

    let c = Case::new("ro-settings");
    let before = "tradehub_addr = \"127.0.0.1:7879\"\n";
    c.write("config.toml", before);
    std::fs::set_permissions(&c.dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);

    // Restore BEFORE asserting, so a failed assertion cannot leave a directory the TempDir drop
    // cannot clean up.
    std::fs::set_permissions(&c.dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(code(&out), 1, "a box failure is the run rung, not usage: {}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains(&c.dir.display().to_string()),
        "the refusal must name the settings DIRECTORY the operator can chmod: {err}"
    );
    // ⚠ A REGRESSION FENCE, not a live proof: with the directory lock in place this case fails at
    // the sentinel open and never reaches the tmp write at all, so reverting the tmp-blame fix
    // alone would leave this green. `crates/vike-config/src/write.rs`'s
    // `an_io_failure_names_the_file_the_operator_aimed_at_not_the_tmp` is the test that actually
    // kills that mutation — measured, after removing the lock left this one passing.
    assert!(!err.contains(".tmp-"), "it must not blame a path the operator has never seen: {err}");
    assert_eq!(c.read("config.toml"), before, "NOT ONE BYTE may be written");
    assert!(
        !c.journal().contains("pending_restart"),
        "nothing landed, so no applied row may claim one: {}",
        c.journal()
    );
}

/// **A journal directory the process cannot write** — the OTHER half of the same incident, and the
/// OPPOSITE disposition: the settings write must still LAND and the command must still exit 0.
///
/// Decision 3 of `crate::cmd::settings_write`'s module doc: the bytes are on disk, and sending a
/// caller down an error path for a write that succeeded is worse than a missing ledger line. This
/// is what would break if somebody put a `?` on the ledger append — a successful write turning into
/// an exit 1 with nothing to catch it.
///
/// It also pins the STDERR WORDING, which is the one message that most needs to be unmistakable:
/// it must say the value WAS written before it says what failed, because stderr is what a tired
/// operator notices first and "the change journal could not record X" alone reads as a failure.
#[test]
#[cfg(unix)]
fn a_journal_directory_that_cannot_be_written_still_writes_and_still_exits_zero() {
    use std::os::unix::fs::PermissionsExt;

    let c = Case::new("ro-state");
    c.write("config.toml", "tradehub_addr = \"127.0.0.1:7879\"\n");
    let state = c.dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();

    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);

    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(out.status.success(), "a ledger failure may NOT fail the call: {}", stderr(&out));
    assert!(
        c.read("config.toml").contains("7999"),
        "the write must land: {}",
        c.read("config.toml")
    );
    let err = stderr(&out);
    assert!(err.contains("WAS written"), "the OUTCOME must come before the failure: {err}");
    assert!(err.contains('⚠'), "the marker every other operator warning carries: {err}");
    assert!(err.contains("change journal"), "{err}");
    // ONE line about this write's ledger row, not a repeat per attempt. Counted on the
    // outcome-first wording rather than on "change journal", which a boot-time ledger warning
    // could legitimately also carry.
    assert_eq!(err.lines().filter(|l| l.contains("WAS written")).count(), 1, "{err}");
}

/// **No `--file` reaches this verb**, in either spelling. The destination is the resolved settings
/// directory and nothing else — the rule
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` states for
/// the credential store, honoured here because the reason transfers whole.
#[test]
fn there_is_no_way_to_name_a_destination_path() {
    let c = Case::new("nofile");
    let elsewhere = c.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let path = elsewhere.to_string_lossy().to_string();
    for argv in [
        vec!["--file", path.as_str(), "config.log_dir", "/tmp/x"],
        vec!["config.log_dir", "/tmp/x", "--file", path.as_str()],
    ] {
        let out = c.run(&argv);
        assert_eq!(code(&out), 2, "{argv:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("--file"), "{}", stderr(&out));
    }
    assert!(!Path::new(&elsewhere).join("config.toml").exists());
}

/// **THE LIVE-ARM FLAG DEMANDS THE RETYPE TOO**, which no file-keyed rule in this tree could
/// express: `flags.tradehub_live` shares `flags.toml` with a dozen ordinary toggles.
///
/// The asymmetry this closes, end to end: `policy.max_leverage` — a field nothing in the tree reads
/// — demanded the ceremony, while the key whose own doc calls it "the single largest blast radius
/// on this list" was a one-line non-interactive write. Driven on a PIPE, which is where an agent
/// and a script both are.
#[test]
fn the_live_arm_flag_needs_the_retyped_key_and_an_ordinary_flag_does_not() {
    let c = Case::new("liveflag");

    let out = c.run(&["flags.tradehub_live", "true"]);
    assert_eq!(code(&out), 2, "a real-money arm may not be one un-retyped line: {}", stderr(&out));
    assert!(stderr(&out).contains("--confirm"), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("RISK CEILING"),
        "the refusal must name the class it actually is: {}",
        stderr(&out)
    );
    assert!(c.read("flags.toml").is_empty(), "nothing may be written without the confirm");

    let out = c.run(&["flags.tradehub_live", "true", "--confirm=flags.tradehub_live"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert!(c.read("flags.toml").contains("tradehub_live"), "{}", c.read("flags.toml"));

    // …and an ORDINARY flag in the SAME FILE stays a one-line write, which is the whole reason the
    // line is drawn on the KEY.
    let out = c.run(&["flags.tradehub_record", "true"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert!(c.read("flags.toml").contains("tradehub_record"), "{}", c.read("flags.toml"));
}
