//! `list` and `path` over a case's settings database, the permission findings, the legacy `.env`
//! probe, and the UNREAD credential file a box with no database is told about.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use super::{BIN, Case, SAMPLE, stderr, stdout};

/// `list` names the store it read, prints every key NAME, and no value.
#[test]
fn list_prints_key_names_and_never_a_value() {
    let c = Case::new("list");
    c.write_store(SAMPLE);
    let before = std::fs::read(c.db()).unwrap();

    let out = c.run("list");
    assert!(out.status.success(), "list failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("BINANCE_LIVE_API_KEY"), "{text}");
    assert!(text.contains("OKX_DEMO_API_PASSPHRASE"), "{text}");
    assert!(text.contains("3 secret(s)"), "{text}");
    assert!(text.contains("settings DATABASE"), "list must name the store it read: {text}");
    assert!(text.contains(&c.db().display().to_string()), "{text}");
    assert!(!text.contains("sup3r-s3cr3t-value"), "a VALUE reached stdout: {text}");
    assert!(!text.contains("key-abcd1234"), "a VALUE reached stdout: {text}");

    // Reading is read-only: the store is byte-identical afterwards.
    assert_eq!(std::fs::read(c.db()).unwrap(), before);
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

/// **RULE 3 through the shipped binary: a keyed `secrets.env` with NO database is SAID, not read.**
///
/// Until 2026-10-07 this directory's file WAS the store. Now `list` answers `0 secret(s)` (the live
/// gate) and stderr names the file as NOT READ with the count of its keyed names and the carry
/// (`vike-cli secrets migrate`); `path` says the same by stat alone. Nothing touches the file, no
/// database appears, and no value is printed.
#[test]
fn a_keyed_credential_file_with_no_database_is_reported_not_read() {
    let c = Case::new("unread");
    std::fs::write(c.store(), SAMPLE).unwrap();

    let out = c.run("list");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "a finding is never a refusal: {err}");
    assert!(text.contains("0 secret(s)"), "a credential FILE must never answer: {text}");
    assert!(err.contains("NOT READ"), "{err}");
    assert!(
        err.contains("holds 3 credential name(s)"),
        "it says how many keys are stranded: {err}"
    );
    assert!(err.contains("vike-cli secrets migrate"), "…and the way in: {err}");

    let out = c.run("path");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "{err}");
    assert!(text.contains("NOT READ"), "{text}");
    assert!(err.contains("vike-cli secrets migrate"), "{err}");

    for leak in ["sup3r-s3cr3t-value", "key-abcd1234"] {
        assert!(!text.contains(leak) && !err.contains(leak), "a VALUE was printed");
    }
    assert_eq!(std::fs::read_to_string(c.store()).unwrap(), SAMPLE, "the file was touched");
    assert!(!c.db().exists(), "a READ verb created a database");
}

/// `path` is the command an operator runs when they cannot tell where the store is. It opens
/// nothing, so it works whatever state the store is in, and it says how to create one.
#[test]
fn path_reports_the_store_and_how_to_create_it() {
    let c = Case::new("path");

    let out = c.run("path");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&c.db().display().to_string()), "{text}");
    assert!(text.contains("absent"), "{text}");
    assert!(text.contains("migrate --init"), "the hint must say how to create one: {text}");
    assert!(!c.db().exists(), "`path` must not have created anything");

    c.write_store(SAMPLE);
    let out = c.run("path");
    let text = stdout(&out);
    assert!(text.contains("present"), "{text}");
    assert!(!text.contains("migrate --init"), "the create hint must not persist once it exists");
    assert!(!text.contains("sup3r-s3cr3t-value"), "a VALUE reached stdout: {text}");
}

/// **Both subcommands report an exposed store, at every mode that exposes it** — the settings
/// database, the one store, asked through `vike_secrets::permission_warning`, which `stat`s the file
/// without reading a byte of it.
///
/// The loop is the point: any single mode would pass with a predicate as wrong as `mode == 0o666`.
/// 0640 (group READ) and 0620 (group WRITE — the worse one, since it lets somebody substitute the
/// keys an order is signed with) both have to fire, and 0600 has to stay SILENT.
///
/// ⚠ Unix only: the finding is an `st_mode` question, and `permission_warning` is a `None`-returning
/// no-op on Windows.
#[cfg(unix)]
#[test]
fn both_subcommands_warn_about_a_store_readable_beyond_its_owner() {
    use std::os::unix::fs::PermissionsExt;

    let c = Case::new("perms");
    c.write_store(SAMPLE);

    for mode in [0o640, 0o604, 0o620, 0o644, 0o666] {
        std::fs::set_permissions(c.db(), std::fs::Permissions::from_mode(mode)).unwrap();
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

    std::fs::set_permissions(c.db(), std::fs::Permissions::from_mode(0o600)).unwrap();
    for sub in ["path", "list"] {
        let err = stderr(&c.run(sub));
        assert!(!err.contains("readable beyond its owner"), "0600 must not warn on `{sub}`: {err}");
    }
}

/// The probe must not become a disguised READ: `path` opens nothing, so it keeps working on a store
/// this process could not read at all.
#[cfg(unix)]
#[test]
fn path_still_opens_nothing_when_it_warns() {
    use std::os::unix::fs::PermissionsExt;

    let c = Case::new("perms-noread");
    c.write_store(SAMPLE);
    std::fs::set_permissions(c.db(), std::fs::Permissions::from_mode(0o007)).unwrap();

    let out = c.run("path");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "`path` must not need read access: {err}");
    assert!(text.contains("present"), "{text}");
    assert!(err.contains("readable beyond its owner"), "{err:?}");
    assert!(!text.contains("sup3r-s3cr3t-value") && !err.contains("sup3r-s3cr3t-value"));

    // Restore before `Drop`'s `remove_dir_all` runs.
    std::fs::set_permissions(c.db(), std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// A store that EXISTS and cannot be read is a clean failure, never a silent "no credentials".
/// Bytes that are not a database where the database belongs are the portable stand-in.
#[test]
fn an_unreadable_store_is_a_clean_error() {
    let c = Case::new("unreadable");
    std::fs::create_dir_all(c.db().parent().unwrap()).unwrap();
    std::fs::write(c.db(), b"this is not a sqlite database, and it is not empty either").unwrap();

    let out = c.run("list");
    assert!(!out.status.success(), "an unopenable store must fail");
    assert!(stderr(&out).contains("could not be read"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("secret(s)"), "partial output leaked");
}

/// Help is normal output, on stdout — `tests/help_cli.rs` is the gate that says so for every
/// surface — and it names the one store.
#[test]
fn help_is_listed_at_the_top_level_and_succeeds() {
    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(stdout(&out).contains("secrets"), "the top-level help must list the command");

    let out = Command::new(BIN).args(["secrets", "--help"]).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success());
    assert!(stdout(&out).contains("list"), "{}", stdout(&out));
    assert!(stdout(&out).contains("settings/db/vike.db"), "{}", stdout(&out));
}

/// A DEPLOYMENT-shaped case: `<dir>/settings/` is the settings directory and `<dir>/.env` is the
/// pre-#1084 credential store sitting beside it. `VIKE_SETTINGS_DIR` names the directory outright,
/// which is what every shipped unit does.
///
/// ⚠ **A BOUND `tempfile::TempDir`** — see [`Case`] for why.
struct Deployment {
    /// BOUND, so its `Drop` removes the tree even on the panic path.
    root: tempfile::TempDir,
}

impl Deployment {
    fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-legacy-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).unwrap();
        Deployment { root }
    }

    /// The project root — the directory `settings/` and the legacy `.env` sit in.
    fn dir(&self) -> &std::path::Path {
        self.root.path()
    }

    /// The retired credential FILE the legacy finding names as the place to copy a `.env` to.
    fn store(&self) -> PathBuf {
        self.dir().join("settings").join("secrets.env")
    }

    /// The pre-#1084 store: `<project>/.env`.
    fn legacy(&self) -> PathBuf {
        self.dir().join(".env")
    }

    /// Make the settings DATABASE hold `text`, the way a store comes into being: carried in.
    fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).unwrap();
        vike_secrets::migrate(
            self.dir().join("settings").to_str(),
            |_| false,
            &vike_bridge_core::credentials::classify_credential_name,
            vike_secrets::WhenNothingToCarry::CreateEmptyStore,
        )
        .expect("carry the fixture into the store");
        std::fs::remove_file(self.store()).unwrap();
    }

    fn run(&self, sub: &str) -> Output {
        Command::new(BIN)
            .args(["secrets", sub])
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", self.dir().join("settings"))
            .output()
            .expect("the vike-cli binary must run")
    }
}

/// **The POSITIVE proof for both fixture shapes in this file**: dropping the handle removes the
/// tree, and the PROJECT [`Deployment`] resolves is its own root (a `vike.toml` planted there is
/// what the shipped binary refuses, naming the planted path).
#[test]
fn the_fixtures_are_owned_and_the_deployment_project_is_its_own_tempdir() {
    let case_path;
    {
        let c = Case::new("hermetic");
        case_path = c.dir().to_path_buf();
        c.write_store(SAMPLE);
        assert!(c.db().starts_with(&case_path), "the store must be inside this case's own dir");
    }
    assert!(
        !case_path.exists(),
        "`Case` must remove itself when it goes out of scope: {} survived",
        case_path.display()
    );

    let deployment_path;
    {
        let d = Deployment::new("hermetic");
        deployment_path = d.dir().to_path_buf();
        assert!(d.run("path").status.success(), "an unplanted deployment runs normally");

        let planted = deployment_path.join("vike.toml");
        std::fs::write(&planted, "").expect("plant the refused file");
        let out = d.run("path");
        assert!(!out.status.success(), "a `vike.toml` beside the settings directory is refused");
        let err = stderr(&out);
        assert!(
            err.contains(&planted.display().to_string()),
            "the refusal must name the file this fixture planted: {err}"
        );
    }
    assert!(
        !deployment_path.exists(),
        "`Deployment` must remove itself when it goes out of scope: {} survived",
        deployment_path.display()
    );
}

/// The value inside the leftover `.env`. Distinctive so "never reads the contents" is a real assert.
const LEGACY_BODY: &str = "BINANCE_LIVE_API_SECRET=leftover-s3cr3t-never-printed\n";

/// **A leftover pre-#1084 `<project>/.env` with no store is DETECTED.** Three states, because a
/// warning that fires on all of them is noise rather than a signal: no store + `.env` warns (naming
/// both paths, exit 0); a store + `.env` is silent (a systemd `EnvironmentFile`); neither keeps the
/// create-one hint alone.
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
            "`secrets {sub}` must name where to put it for the carry: {err:?}"
        );
        assert!(err.contains("stays paper"), "the warning must say what it costs: {err:?}");
        assert!(
            !err.contains("leftover-s3cr3t") && !text.contains("leftover-s3cr3t"),
            "the probe must never read the file's CONTENTS: {err:?} / {text:?}"
        );
    }

    assert_eq!(std::fs::read_to_string(d.legacy()).unwrap(), LEGACY_BODY);
    assert!(!d.store().exists(), "the probe must not have created a file");
}

/// **A `.env` beside a store that EXISTS is silent** — that is a systemd `EnvironmentFile`, not a
/// leftover, and the CI box's recorder ships one.
#[test]
fn a_dotenv_beside_a_present_store_is_not_a_finding() {
    let d = Deployment::new("present");
    std::fs::write(d.legacy(), LEGACY_BODY).unwrap();
    d.write_store(SAMPLE);

    for sub in ["path", "list"] {
        let err = stderr(&d.run(sub));
        assert!(
            !err.contains(&d.legacy().display().to_string()),
            "a store that loaded is not a silent transition — `{sub}` must not warn: {err:?}"
        );
    }
}

/// **Neither present ⇒ the create-one hint alone.** A fresh install is the common case and must not
/// grow a warning about a file that is not there.
#[test]
fn no_store_and_no_leftover_keeps_the_create_one_hint_alone() {
    let d = Deployment::new("neither");

    let out = d.run("path");
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "{err}");
    assert!(text.contains("no credential store here"), "{text}");
    assert!(text.contains("migrate --init"), "{text}");
    assert!(!err.contains(".env is present"), "nothing to warn about: {err:?}");
    assert!(!err.contains("NOT READ"), "there is no file to report: {err:?}");
}

#[test]
fn an_unknown_subcommand_fails_without_touching_any_file() {
    let c = Case::new("unknown");
    c.write_store(SAMPLE);
    let before = std::fs::read(c.db()).unwrap();
    let out = c.run_raw(&["frobnicate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown `secrets` subcommand"));
    assert_eq!(std::fs::read(c.db()).unwrap(), before);
}

/// **`list` says which ACCOUNTS the store's key names resolve to** — the question the flat key
/// list structurally cannot answer. Three shapes LOOK alike in the key list: the default account's
/// pair, a labelled account's pair, and a single-underscore near-miss that nothing will ever read.
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

    assert!(text.contains("2 account(s) in the store:"), "{text}");
    assert!(text.contains("bybit/DEMO (default)"), "the unlabelled account must be named: {text}");
    assert!(text.contains("bybit/DEMO ALT"), "the labelled account must be named: {text}");

    assert!(text.contains("BYBIT_DEMO_API_KEY_NEARMISS"), "the key list is unchanged: {text}");
    let account_rows: Vec<&str> =
        text.lines().skip_while(|l| !l.contains("account(s) in the store:")).skip(1).collect();
    assert_eq!(account_rows.len(), 2, "exactly two account rows: {account_rows:?}");
    assert!(
        !account_rows.iter().any(|l| l.contains("NEARMISS")),
        "a single underscore names no account: {account_rows:?}"
    );

    for value in ["key-default", "secret-default", "key-alt", "secret-alt", "key-nearmiss"] {
        assert!(!text.contains(value), "a credential VALUE reached stdout: {text}");
    }
    assert!(!text.contains(" DEFAULT"), "the refused label spelling must not be printed: {text}");
}

/// A store whose names resolve to NO account still prints the heading, at zero.
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
