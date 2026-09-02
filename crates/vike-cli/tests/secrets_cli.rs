//! End-to-end tests for `vike-cli secrets`, driving the SHIPPED binary (`CARGO_BIN_EXE_vike-cli`).
//!
//! The unit tests beside the module cover the argument grammar; these cover the thing that actually
//! matters to an operator — that `list` reads the store and prints key NAMES and never a value, and
//! that `path` says where the store is whether or not it exists.
//!
//! ⚠ **Every invocation redirects the store into the case's temp dir** (`--file`, on every run).
//! Without it a run on a developer box would read that box's real credentials into a test's stdout
//! assertions. The redirect is isolation AND a safety property.
//!
//! `VIKE_SETTINGS_DIR` is cleared from the child's environment for the same reason, through
//! `Command::env_remove` — never `std::env::set_var`, which is unsafe under threads and would leak
//! across the test binary's parallel cases. `Stdio::null()` on stdin keeps `is_terminal()` false, so
//! no case can block on a prompt.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

struct Case {
    dir: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("vike-cli-secrets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Case { dir }
    }

    /// The store this case inspects, redirected into its own temp directory.
    fn store(&self) -> PathBuf {
        self.dir.join("secrets.env")
    }

    fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).unwrap();
    }

    /// Run `vike-cli secrets <sub> --file <the case's store>`.
    fn run(&self, sub: &str) -> Output {
        self.run_raw(&[sub, "--file", &self.store().display().to_string()])
    }

    /// Run `vike-cli secrets …` with the arguments given verbatim.
    fn run_raw(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("secrets")
            .args(args)
            .stdin(Stdio::null())
            .env_remove("VIKE_SETTINGS_DIR")
            .output()
            .expect("the vike-cli binary must run")
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const SAMPLE: &str = "# venue credentials\n\
                      BINANCE_LIVE_API_KEY=key-abcd1234\n\
                      BINANCE_LIVE_API_SECRET=sup3r-s3cr3t-value\n\
                      OKX_DEMO_API_PASSPHRASE=\"quoted-pass\"\n";

/// `list` names the file it read, prints every key NAME, and no value.
#[test]
fn list_prints_key_names_and_never_a_value() {
    let c = Case::new("list");
    c.write_store(SAMPLE);

    let out = c.run("list");
    assert!(out.status.success(), "list failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("BINANCE_LIVE_API_KEY"), "{text}");
    assert!(text.contains("OKX_DEMO_API_PASSPHRASE"), "{text}");
    assert!(text.contains("3 secret(s)"), "{text}");
    assert!(text.contains("credential store"), "list must name the file it read: {text}");
    assert!(text.contains(&c.store().display().to_string()), "{text}");
    assert!(!text.contains("sup3r-s3cr3t-value"), "a VALUE reached stdout: {text}");
    assert!(!text.contains("key-abcd1234"), "a VALUE reached stdout: {text}");

    // Reading is read-only: the store is byte-identical afterwards.
    assert_eq!(std::fs::read_to_string(c.store()).unwrap(), SAMPLE);
}

/// No store is not an error — it is the live gate, and `list` says so in words an operator can act
/// on rather than failing.
#[test]
fn list_with_no_store_reports_the_live_gate() {
    let c = Case::new("list-absent");
    let out = c.run("list");
    assert!(out.status.success(), "an absent store must not be an error: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no store found"), "{text}");
    assert!(text.contains("every venue stays paper"), "{text}");
    assert!(text.contains("0 secret(s)"), "{text}");
}

/// `path` is the command an operator runs when they cannot tell where the store is. It opens
/// nothing, so it works whatever state the store is in, and it says how to create one.
#[test]
fn path_reports_the_store_and_how_to_create_it() {
    let c = Case::new("path");

    let out = c.run("path");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&c.store().display().to_string()), "{text}");
    assert!(text.contains("absent"), "{text}");
    assert!(text.contains("chmod 600"), "the hint must say how to create one: {text}");
    assert!(text.contains("reads any other location"), "{text}");
    assert!(!c.store().exists(), "`path` must not have created anything");

    c.write_store(SAMPLE);
    let out = c.run("path");
    let text = stdout(&out);
    assert!(text.contains("present"), "{text}");
    assert!(!text.contains("chmod 600"), "the create hint must not persist once it exists: {text}");
    assert!(!text.contains("sup3r-s3cr3t-value"), "a VALUE reached stdout: {text}");
}

/// **A probe that FAILED must not print as `absent`.**
///
/// `path` decided presence with `Path::exists`, which maps EVERY error — `EACCES` on a directory in
/// the path included — to `false`. So a project root the process cannot search printed the store as
/// absent AND printed the fresh-install advice ("nothing here reads any other location — create
/// it"), and the operator read *not configured* for a permissions problem. Both states leave every
/// venue on paper, which is what makes them indistinguishable downstream and exactly why this
/// command may not merge them: it is the one an operator runs FIRST, before they know what is
/// wrong. `vike_secrets::legacy_store_warning` refuses the same conflation one layer down.
///
/// ⚠ The failing probe is `ENOTDIR` — a path THROUGH a regular file — and not a `chmod 0o000`
/// parent, because a mode denial is a no-op for root and CI runs as root, so that shape would pass
/// vacuously there. `ENOTDIR` is uid-independent.
///
/// Unix-only: Windows answers `ERROR_PATH_NOT_FOUND` for a path under a file, which IS `NotFound`,
/// so there is no portable way to produce a non-`NotFound` error here.
#[cfg(unix)]
#[test]
fn path_says_it_could_not_determine_rather_than_calling_a_failed_probe_absent() {
    let c = Case::new("undetermined");
    // A regular file where a directory has to be. Nothing under it can be stat'd, by anybody.
    c.write_store(SAMPLE);
    let under_a_file = c.store().join("settings").join("secrets.env");
    let arg = under_a_file.display().to_string();

    let out = c.run_raw(&["path", "--file", &arg]);
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "a finding is never a refusal — `path` must still report: {err}");
    assert!(text.contains(&arg), "it must still say WHICH file it was asked about: {text}");
    assert!(text.contains("could not be determined"), "{text}");
    assert!(!text.contains("absent"), "a failed probe must not read as absence: {text}");
    assert!(!text.contains("present"), "…and it must not claim the store is there either: {text}");
    // The create-one hint is the FRESH-INSTALL answer. Printing it here advises creating a file
    // that may already exist, and it is the half of the old output that misled hardest.
    assert!(!text.contains("reads any other location"), "{text}");
    assert!(!text.contains("chmod 600"), "{text}");
    assert!(err.contains("could not be determined"), "the finding must reach stderr too: {err:?}");
    assert!(
        err.contains("every venue stays paper"),
        "the finding must name the answer this is NOT, since both cost the same: {err:?}"
    );
    assert!(!text.contains("sup3r-s3cr3t-value") && !err.contains("sup3r-s3cr3t-value"));

    // Read-only, as ever: the file standing in for the directory is untouched.
    assert_eq!(std::fs::read_to_string(c.store()).unwrap(), SAMPLE);
}

/// **Both subcommands report an exposed store, at every mode that exposes it.**
///
/// This is the asymmetry a clean install found. Measured at 600/640/644/664/666, `list` warned from
/// 640 up and `path` warned at NONE of them — and `path` is the command the README and the ops
/// runbook name FIRST, the one an operator reaches for before they know anything is wrong. `list`
/// warned only because it opens the store and gets the finding back with the credentials; `path`
/// opens nothing, so it never asked. It asks now, through `vike_secrets::permission_warning`, which
/// `stat`s the file without reading a byte of it.
///
/// The loop is the point: any single mode would pass with a predicate as wrong as `mode == 0o666`.
/// 0640 (group READ) and 0620 (group WRITE — the worse one, since it lets somebody substitute the
/// keys an order is signed with) both have to fire, and 0600 has to stay SILENT or the warning
/// becomes noise everyone learns to scroll past.
///
/// ⚠ Unix only: the finding is an `st_mode` question, and `permission_warning` is a `None`-returning
/// no-op on Windows, where the equivalent is an ACL query this workspace carries no crate for.
#[cfg(unix)]
#[test]
fn both_subcommands_warn_about_a_store_readable_beyond_its_owner() {
    use std::os::unix::fs::PermissionsExt;

    let c = Case::new("perms");
    c.write_store(SAMPLE);

    for mode in [0o640, 0o604, 0o620, 0o644, 0o666] {
        std::fs::set_permissions(c.store(), std::fs::Permissions::from_mode(mode)).unwrap();
        for sub in ["path", "list"] {
            let out = c.run(sub);
            let err = stderr(&out);
            assert!(
                out.status.success(),
                "a finding is never a refusal — `{sub}` must still succeed at {mode:04o}: {err}"
            );
            assert!(
                err.contains("readable beyond its owner") && err.contains(&format!("{mode:04o}")),
                "`secrets {sub}` must warn at mode {mode:04o}, got: {err:?}"
            );
            assert!(err.contains("chmod 600"), "the warning must say the fix: {err:?}");
            assert!(!err.contains("sup3r-s3cr3t-value"), "a VALUE reached stderr: {err}");
        }
    }

    // …and 0600 is silent on BOTH, or the warning is noise rather than a signal.
    std::fs::set_permissions(c.store(), std::fs::Permissions::from_mode(0o600)).unwrap();
    for sub in ["path", "list"] {
        let err = stderr(&c.run(sub));
        assert!(!err.contains("readable beyond its owner"), "0600 must not warn on `{sub}`: {err}");
    }
}

/// The probe must not become a disguised READ. `path`'s documented contract is that it opens
/// nothing, which is what makes it the safe command when the store is in an unknown state — so it
/// has to keep working on a store this process could not read at all.
#[cfg(unix)]
#[test]
fn path_still_opens_nothing_when_it_warns() {
    use std::os::unix::fs::PermissionsExt;

    let c = Case::new("perms-noread");
    c.write_store(SAMPLE);
    // Exposed to the world AND unreadable by its owner: `stat` still answers, so the finding must
    // fire, while any attempt to read the CONTENTS would fail.
    std::fs::set_permissions(c.store(), std::fs::Permissions::from_mode(0o007)).unwrap();

    let out = c.run("path");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "`path` must not need read access: {err}");
    assert!(text.contains("present"), "{text}");
    assert!(err.contains("readable beyond its owner"), "{err:?}");
    assert!(!text.contains("sup3r-s3cr3t-value") && !err.contains("sup3r-s3cr3t-value"));

    // Restore before `Drop`'s `remove_dir_all` runs.
    std::fs::set_permissions(c.store(), std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// A store that EXISTS and cannot be read is a clean failure, never a silent "no credentials". A
/// DIRECTORY where the file should be is the portable stand-in for an unreadable file.
#[test]
fn an_unreadable_store_is_a_clean_error() {
    let c = Case::new("unreadable");
    std::fs::create_dir_all(c.store()).unwrap();

    let out = c.run("list");
    assert!(!out.status.success(), "an unopenable store must fail");
    assert!(stderr(&out).contains("could not be read"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("secret(s)"), "partial output leaked");
}

/// ⚠ The `secrets --help` half asserted **stderr** until the workspace-wide `--help` fix: this
/// command already exited 0 for it, but printed the text with `eprintln!`, so
/// `vike-cli secrets --help | less` showed an empty page. Help is normal output, and it is stdout
/// on every surface now — `tests/help_cli.rs` is the gate that says so for all of them.
#[test]
fn help_is_listed_at_the_top_level_and_succeeds() {
    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(stdout(&out).contains("secrets"), "the top-level help must list the command");

    let out = Command::new(BIN).args(["secrets", "--help"]).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success());
    assert!(stdout(&out).contains("list"), "{}", stdout(&out));
    assert!(stdout(&out).contains("settings/secrets.env"), "{}", stdout(&out));
}

/// A DEPLOYMENT-shaped case: `<dir>/settings/secrets.env` is the store, and `<dir>/.env` is the
/// pre-#1084 credential store sitting beside it. `VIKE_SETTINGS_DIR` names the directory outright,
/// which is what all three shipped units do — so this is a deployment's ONE-PROJECT-FOLDER layout
/// exactly, and the probe is exercised through the dispatcher's own resolution rather than through
/// `--file`.
struct Deployment {
    dir: PathBuf,
}

impl Deployment {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("vike-cli-legacy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("settings")).unwrap();
        Deployment { dir }
    }

    fn store(&self) -> PathBuf {
        self.dir.join("settings").join("secrets.env")
    }

    /// The pre-#1084 store: `<project>/.env`.
    fn legacy(&self) -> PathBuf {
        self.dir.join(".env")
    }

    fn run(&self, sub: &str) -> Output {
        Command::new(BIN)
            .args(["secrets", sub])
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", self.dir.join("settings"))
            .output()
            .expect("the vike-cli binary must run")
    }
}

impl Drop for Deployment {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The value inside the leftover `.env`. Distinctive so "never reads the contents" is a real assert.
const LEGACY_BODY: &str = "BINANCE_LIVE_API_SECRET=leftover-s3cr3t-never-printed\n";

/// **A leftover pre-#1084 `<project>/.env` with no store is DETECTED — the transition was silent.**
///
/// This is the whole defect: `vike_secrets::resolve` answered `Source::None`,
/// `load_workspace_secrets_at` returned an empty map with NO log line, and fourteen venues dropped
/// to paper while the operator's symptom was "my orders aren't reaching the venue". `secrets path`
/// — the command the README names FIRST — printed *"nothing here reads any other location"*, which
/// was true and had never looked.
///
/// Three states, because a warning that fires on all of them is noise rather than a signal:
///
/// | store | `<project>/.env` | expected |
/// |---|---|---|
/// | absent | present | **warn**, naming both paths, exit 0 |
/// | present | present | **silent** — see below |
/// | absent | absent | today's create-one hint, and no legacy warning |
///
/// ⚠ **Row 2 is the load-bearing one.** `.env` has a legitimate second life as a systemd
/// `EnvironmentFile`, and the CI box's live recorder uses its `<project>/.env` exactly that way (it holds
/// `POLY_PROXY_ENABLED`, not credentials). Warning whenever one exists would fire forever on a
/// correctly-configured box, so the finding is gated on the store being ABSENT — which is precisely
/// the silent case and nothing else.
#[test]
fn a_leftover_dotenv_beside_the_project_is_reported_when_there_is_no_store() {
    let d = Deployment::new("absent");
    std::fs::write(d.legacy(), LEGACY_BODY).unwrap();

    for sub in ["path", "list"] {
        let out = d.run(sub);
        let (text, err) = (stdout(&out), stderr(&out));
        assert!(
            out.status.success(),
            "a finding is never a refusal — `{sub}` must still succeed: {err}"
        );
        assert!(
            err.contains(&d.legacy().display().to_string()),
            "`secrets {sub}` must name the leftover file: {err:?}"
        );
        assert!(
            err.contains(&d.store().display().to_string()),
            "`secrets {sub}` must name the store it looked for: {err:?}"
        );
        assert!(
            err.contains("stays paper"),
            "the warning must say what it costs the operator: {err:?}"
        );
        assert!(
            !err.contains("leftover-s3cr3t") && !text.contains("leftover-s3cr3t"),
            "the probe must never read the file's CONTENTS: {err:?} / {text:?}"
        );
    }

    // Nothing is moved, deleted or rewritten — it is the operator's file, and it may still be a
    // live systemd EnvironmentFile.
    assert_eq!(std::fs::read_to_string(d.legacy()).unwrap(), LEGACY_BODY);
    assert!(!d.store().exists(), "the probe must not have created a store");
}

/// **A `.env` beside a store that EXISTS is silent** — that is a systemd `EnvironmentFile`, not a
/// leftover, and the CI box's recorder ships one. Nothing was silent about credentials here: the store
/// loaded.
#[test]
fn a_dotenv_beside_a_present_store_is_not_a_finding() {
    let d = Deployment::new("present");
    std::fs::write(d.legacy(), LEGACY_BODY).unwrap();
    std::fs::write(d.store(), SAMPLE).unwrap();

    for sub in ["path", "list"] {
        let err = stderr(&d.run(sub));
        assert!(
            !err.contains(&d.legacy().display().to_string()),
            "a store that loaded is not a silent transition — `{sub}` must not warn: {err:?}"
        );
    }
}

/// **Neither present ⇒ today's message, unchanged.** A fresh install is the common case and must not
/// grow a warning about a file that is not there.
#[test]
fn no_store_and_no_leftover_keeps_the_create_one_hint_alone() {
    let d = Deployment::new("neither");

    let out = d.run("path");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "{err}");
    assert!(text.contains("reads any other location"), "{text}");
    assert!(text.contains("chmod 600"), "{text}");
    assert!(!err.contains(".env is present"), "nothing to warn about: {err:?}");
}

#[test]
fn an_unknown_subcommand_fails_without_touching_any_file() {
    let c = Case::new("unknown");
    c.write_store(SAMPLE);
    let out = c.run_raw(&["frobnicate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown `secrets` subcommand"));
    assert_eq!(std::fs::read_to_string(c.store()).unwrap(), SAMPLE);
}

/// **`list` says which ACCOUNTS the store's key names resolve to** — the question the flat key
/// list structurally cannot answer.
///
/// The store below holds three shapes that all LOOK alike in the key list: the default account's
/// pair, a labelled account's pair, and a single-underscore near-miss that is not a labelled key at
/// all and that nothing will ever read. Only the first two become accounts, and the near-miss's
/// absence from the account rows is the whole diagnostic — it is how an operator finds a typo that
/// costs them a credential.
#[test]
fn list_names_the_accounts_the_key_names_resolve_to() {
    let c = Case::new("accounts");
    c.write_store(
        "BYBIT_DEMO_API_KEY=key-default\n\
         BYBIT_DEMO_API_SECRET=secret-default\n\
         BYBIT_DEMO_API_KEY__ALT=key-alt\n\
         BYBIT_DEMO_API_SECRET__ALT=secret-alt\n\
         BYBIT_DEMO_API_KEY_NEARMISS=key-nearmiss\n",
    );
    let out = c.run("list");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "{err}");

    // Both accounts, and exactly two: the near-miss key is not one.
    assert!(text.contains("2 account(s) in the store:"), "{text}");
    assert!(text.contains("bybit/DEMO (default)"), "the unlabelled account must be named: {text}");
    assert!(text.contains("bybit/DEMO ALT"), "the labelled account must be named: {text}");

    // The near-miss appears as a KEY (it is in the file) and as no account.
    assert!(text.contains("BYBIT_DEMO_API_KEY_NEARMISS"), "the key list is unchanged: {text}");
    let account_rows: Vec<&str> =
        text.lines().skip_while(|l| !l.contains("account(s) in the store:")).skip(1).collect();
    assert_eq!(account_rows.len(), 2, "exactly two account rows: {account_rows:?}");
    assert!(
        !account_rows.iter().any(|l| l.contains("NEARMISS")),
        "a single underscore names no account: {account_rows:?}"
    );

    // …and this is still a NAMES-only command.
    for value in ["key-default", "secret-default", "key-alt", "secret-alt", "key-nearmiss"] {
        assert!(!text.contains(value), "a credential VALUE reached stdout: {text}");
    }
    // The reserved spelling `DEFAULT` is what `AccountLabel::parse` refuses, so it must not be
    // printed in a column an operator would paste into `policy.toml`'s `[accounts]` table.
    assert!(!text.contains(" DEFAULT"), "the refused label spelling must not be printed: {text}");
}

/// A store whose names resolve to NO account still prints the heading, at zero — an operator who
/// sees no row must be able to tell "none recognised" from "the command does not report this".
#[test]
fn a_store_with_no_credential_keys_reports_zero_accounts() {
    let c = Case::new("no-accounts");
    // An attribution code and a sidecar path: real store contents, and neither is a credential key.
    c.write_store("OKX_BROKER_CODE=abc\nJFOREX_BRIDGE_JAR=/tmp/x.jar\n");
    let out = c.run("list");
    let text = stdout(&out);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(text.contains("0 account(s) in the store:"), "{text}");
}
