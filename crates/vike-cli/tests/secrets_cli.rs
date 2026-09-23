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

/// A throwaway directory holding the store this case inspects.
///
/// ⚠ **A BOUND `tempfile::TempDir`, not `<system-temp>/vike-cli-secrets-<tag>-<pid>`.** A
/// fixed-tag-plus-pid directory in the shared system temp root is unique enough on one box and
/// OWNED by nobody: the `Drop` impl that stood here cleaned up on the ordinary path, but a SIGKILL,
/// an OOM or a Ctrl-C — all of which the CI box has seen — leaves it there forever, `/tmp` being 1777
/// sticky. The verdict flip is at the RECEIVING end: given a foreign-owned leftover of the same
/// name, the old `remove_dir_all` failed `EACCES` and `let _ =` swallowed it, `create_dir_all`
/// returned **Ok** because the directory already existed, and the first `write_store` then panicked
/// `PermissionDenied` naming a `/tmp` path and no property of any test. A `TempDir` claims its name
/// `O_EXCL` and removes it on the panic path too.
struct Case {
    /// BOUND, so its `Drop` removes the tree. Held for the whole case.
    root: tempfile::TempDir,
}

impl Case {
    fn new(tag: &str) -> Self {
        // The tag survives as the directory's PREFIX — what makes a leftover from a kill signal
        // (which no `Drop` can answer) attributable to a case rather than anonymous.
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-secrets-{tag}-"))
            .tempdir()
            .expect("tempdir");
        Case { root }
    }

    /// This case's own directory.
    fn dir(&self) -> &std::path::Path {
        self.root.path()
    }

    /// The store this case inspects, redirected into its own temp directory.
    fn store(&self) -> PathBuf {
        self.dir().join("secrets.env")
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
///
/// ⚠ **A BOUND `tempfile::TempDir`** — see [`Case`] for why a fixed-tag-plus-pid directory in the
/// shared system temp root is not a repair, and what a foreign-owned leftover of the same name does
/// to the first write.
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

    fn store(&self) -> PathBuf {
        self.dir().join("settings").join("secrets.env")
    }

    /// The pre-#1084 store: `<project>/.env`.
    fn legacy(&self) -> PathBuf {
        self.dir().join(".env")
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

/// **The POSITIVE proof for both fixture shapes in this file**, because a green run of the cases
/// below proves neither: they pass with the old fixtures too, on every box where nobody has yet
/// left a leftover of the same name or dropped a `/tmp/vike.toml`.
///
/// Half one — OWNERSHIP. Dropping the handle removes the tree. That is the property the old `Drop`
/// impls also had on the PASSING path and neither had on the killed one, and a leaked directory is
/// what arms the cross-user `PermissionDenied` for the next user under a 1777 sticky `/tmp`.
///
/// Half two — the PROJECT [`Deployment`] resolves is its own root, asserted behaviourally:
/// `vike_config::load`'s layer 0 stats `<settings-dir>/../vike.toml` and refuses hard on anything
/// but `NotFound`, so planting that file inside THIS fixture's root must be what the shipped binary
/// refuses, naming the planted path. Aimed one level up — at the shared system temp root — the same
/// probe is a tripwire any process on the box can arm for every user at once.
#[test]
fn the_fixtures_are_owned_and_the_deployment_project_is_its_own_tempdir() {
    let case_path;
    {
        let c = Case::new("hermetic");
        case_path = c.dir().to_path_buf();
        c.write_store(SAMPLE);
        assert!(c.store().starts_with(&case_path), "the store must be inside this case's own dir");
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
            "the refusal must name the file this fixture planted — that is what proves the probe \
             resolved inside its own TempDir rather than at the shared system temp root: {err}"
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

// ---- `list --json` ----------------------------------------------------------------------------

/// The machine rendering of the same disclosure: valid JSON on stdout, the store's path, every key
/// NAME — and **no value anywhere in the document**.
///
/// ⚠ The value assertion is over the RAW TEXT rather than over a field, deliberately. A field-level
/// check only proves the fields it names are clean; a machine-readable listing that grew a
/// `"values"` array, or embedded one in a message, would pass it while doing exactly the thing this
/// command exists not to do. The whole point of `list` is that its output is safe to paste.
#[test]
fn a_json_listing_carries_names_and_never_a_value() {
    let c = Case::new("list-json");
    c.write_store(SAMPLE);

    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    assert!(out.status.success(), "list --json failed: {}", stderr(&out));
    let text = stdout(&out);
    for value in ["sup3r-s3cr3t-value", "key-abcd1234", "quoted-pass"] {
        assert!(!text.contains(value), "a credential VALUE reached stdout: {text}");
    }

    let doc: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("`list --json` must print ONE JSON document: {e}\n{text}"));
    assert_eq!(
        doc["store"].as_str(),
        Some(c.store().display().to_string().as_str()),
        "the document names the file it read: {text}"
    );
    let keys: Vec<&str> = doc["keys"]
        .as_array()
        .expect("keys is an array")
        .iter()
        .map(|k| k.as_str().expect("every key is a string"))
        .collect();
    assert!(keys.contains(&"BINANCE_LIVE_API_KEY"), "{keys:?}");
    assert!(keys.contains(&"BINANCE_LIVE_API_SECRET"), "{keys:?}");
    assert!(keys.contains(&"OKX_DEMO_API_PASSPHRASE"), "{keys:?}");
    assert_eq!(keys.len(), 3, "every key name, and nothing else: {keys:?}");
}

/// The two renderings answer with the SAME accounts, derived from the same key names — the property
/// that keeps a machine and a person from being told different things about one store.
#[test]
fn the_json_accounts_are_the_ones_the_human_listing_shows() {
    let c = Case::new("list-json-accounts");
    c.write_store(
        "HYPERLIQUID_LIVE_API_KEY__ALT=key-alt\n\
         HYPERLIQUID_LIVE_API_SECRET__ALT=secret-alt\n\
         BINANCE_LIVE_API_KEY=key-default\n\
         BINANCE_LIVE_API_SECRET=secret-default\n",
    );

    let human = stdout(&c.run("list"));
    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    let accounts = doc["accounts"].as_array().expect("accounts is an array");

    assert_eq!(accounts.len(), 2, "one per account the grammar recognises: {}", stdout(&out));
    for a in accounts {
        let venue = a["venue"].as_str().expect("venue is a string");
        assert!(human.contains(venue), "the human listing names the same venue {venue}: {human}");
    }
    // The labelled one carries its label; the unlabelled one carries `null` — never the word
    // DEFAULT, which is the spelling `AccountLabel::parse` refuses.
    let labels: Vec<Option<&str>> = accounts.iter().map(|a| a["label"].as_str()).collect();
    assert!(labels.contains(&Some("ALT")), "{labels:?}");
    assert!(labels.contains(&None), "the default account's label is null: {labels:?}");
    assert!(!stdout(&out).contains("DEFAULT"), "{}", stdout(&out));
}

/// An ABSENT store is the live gate rather than a failure, and the machine shape says so with a
/// `null` — not with a sentence a caller would have to pattern-match.
#[test]
fn a_json_listing_of_an_absent_store_is_a_null_store_and_a_success() {
    let c = Case::new("list-json-absent");
    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    assert!(out.status.success(), "an absent store must not be an error: {}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert!(doc["store"].is_null(), "{}", stdout(&out));
    assert_eq!(doc["keys"].as_array().map(Vec::len), Some(0));
}

/// **THE `--json` DOCUMENT'S FIELD SET, PINNED.**
///
/// The suite around this reads one field at a time, which is the right shape for asserting what a
/// field MEANS and the wrong shape for noticing that the document GREW one. It grew one without
/// anybody noticing: `docs/decisions/0054`'s credential half added `kind` (`file` | `database` |
/// `absent`), and every test here stayed green because none of them looks at the document as a
/// whole.
///
/// This is a machine-readable contract. A consumer pattern-matching it is entitled to know when it
/// changes, and the only way to make the NEXT addition deliberate is to make it a test edit. So the
/// top-level keys are pinned as a SET, both directions:
///
/// * a field ADDED shows up here rather than in somebody's parser three weeks later;
/// * a field REMOVED is caught too, which is the half a "contains these keys" assertion would miss
///   and the half that actually breaks a consumer.
///
/// ⚠ It pins the NAMES, not the values — the meaning of each is asserted by the tests above, and
/// duplicating those assertions here would make this file the second authority on a question that
/// already has one.
#[test]
fn the_json_document_has_exactly_these_top_level_fields() {
    /// Every top-level key `list --json` prints. Adding one is an API change: add it here, in this
    /// order, with the tests that say what it means.
    const FIELDS: [&str; 4] = ["accounts", "keys", "kind", "store"];

    let c = Case::new("list-json-shape");
    c.write_store(SAMPLE);
    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    assert!(out.status.success(), "list --json failed: {}", stderr(&out));
    let text = stdout(&out);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    let mut found: Vec<&str> =
        doc.as_object().expect("the document is an object").keys().map(String::as_str).collect();
    found.sort_unstable();
    assert_eq!(
        found,
        FIELDS.to_vec(),
        "`list --json`'s field set changed. If that was deliberate, update FIELDS and add a test \
         saying what the new field MEANS; if it was not, this is an API change a consumer would \
         have found for you.\n{text}"
    );

    // …and the shape holds on the ABSENT store too, which is the branch that renders a different
    // value for two of the four and would be the easy one to forget.
    let bare = Case::new("list-json-shape-absent");
    let out = bare.run_raw(&["list", "--file", &bare.store().display().to_string(), "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    let mut found: Vec<&str> =
        doc.as_object().expect("the document is an object").keys().map(String::as_str).collect();
    found.sort_unstable();
    assert_eq!(found, FIELDS.to_vec(), "the absent-store document must carry the same fields");
}

/// `kind` says WHICH KIND of store answered, and on a file store it says `file`.
///
/// The field exists because `store` is a path either way, so a consumer reading it can no longer
/// tell whether the location is something it may `cat`. `docs/decisions/0054`'s constraint 2 is that
/// `sqlite3` is not installed on the live box and an operator reads the store with `cat` today; the
/// word is the hint, made explicit rather than smuggled into a path's extension.
#[test]
fn the_json_kind_names_the_store_that_answered() {
    let c = Case::new("list-json-kind");
    c.write_store(SAMPLE);
    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(doc["kind"].as_str(), Some("file"), "{}", stdout(&out));

    let bare = Case::new("list-json-kind-absent");
    let out = bare.run_raw(&["list", "--file", &bare.store().display().to_string(), "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(doc["kind"].as_str(), Some("absent"), "{}", stdout(&out));
    assert!(doc["store"].is_null(), "…and `store` stays null beside it");
}

/// `--json` is refused on the subcommands it would mean nothing on, rather than being ignored: a
/// flag the operator typed and the program dropped is how somebody comes to believe they asked for
/// a shape they did not get. `template`'s product is a `KEY=VALUE` FILE FORMAT, and `path`'s is
/// three lines a human reads when something is already broken.
#[test]
fn json_is_refused_where_it_would_mean_nothing() {
    let c = Case::new("list-json-refused");
    for sub in ["path", "template"] {
        let out = c.run_raw(&[sub, "--json"]);
        assert!(!out.status.success(), "`secrets {sub} --json` must be refused");
        assert!(
            stderr(&out).contains("--json applies to `list` only"),
            "and it must say so: {}",
            stderr(&out)
        );
    }
}

// ── set — the ONE writer ────────────────────────────────────────────────────────────────────────
//
// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` fixed this
// command's shape before it was built; these cases are that shape asserted over the SHIPPED binary,
// which is the only place several of the properties are visible at all (the exit RUNG, the two
// streams, and the bytes on disk afterwards).
//
// ⚠ Every case drives its own temp settings directory through `$VIKE_SETTINGS_DIR`, so the store
// written is the case's own and never the developer box's — the same isolation the reading cases
// above take through `--file`, but reached the other way round, because `set` also has to resolve
// the change journal that hangs off that same directory.

/// A `set` case: a `<dir>/settings/` holding the store, with `<dir>/settings/state/` for the ledger
/// — the layout `vike_boot::boot` resolves and every daemon on a box uses.
///
/// ⚠ **A BOUND `tempfile::TempDir`** — see [`Case`] for why a fixed-tag-plus-pid directory in the
/// shared system temp root is not a repair.
struct SetCase {
    /// BOUND, so its `Drop` removes the tree even on the panic path.
    root: tempfile::TempDir,
}

impl SetCase {
    fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-set-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).unwrap();
        SetCase { root }
    }

    /// The project root — the directory `settings/` sits in.
    fn dir(&self) -> &std::path::Path {
        self.root.path()
    }

    fn settings(&self) -> PathBuf {
        self.dir().join("settings")
    }

    fn store(&self) -> PathBuf {
        self.settings().join("secrets.env")
    }

    fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).unwrap();
    }

    fn read_store(&self) -> String {
        std::fs::read_to_string(self.store()).unwrap()
    }

    /// The change journal's directory — `<settings>/state/changes`, off the SAME walk the store is.
    fn journal_dir(&self) -> PathBuf {
        self.settings().join("state").join("changes")
    }

    /// Every `changes-*.jsonl` line the ledger holds, or an empty vec when nothing was written.
    fn journal_lines(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.journal_dir()) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("changes-") || !name.ends_with(".jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).unwrap();
            out.extend(text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string));
        }
        out
    }

    /// `vike-cli secrets <args…>` against this case's settings directory, with `stdin` fed the
    /// given bytes (`None` = closed, which is what a shell with no pipe hands a process).
    fn run(&self, args: &[&str], stdin_text: Option<&str>, envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("secrets")
            .args(args)
            .env("VIKE_SETTINGS_DIR", self.settings())
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        match stdin_text {
            None => {
                cmd.stdin(Stdio::null());
                cmd.output().expect("the vike-cli binary must run")
            }
            Some(text) => {
                use std::io::{ErrorKind, Write};
                cmd.stdin(Stdio::piped());
                let mut child = cmd.spawn().expect("the vike-cli binary must run");
                // ⚠ A child that REFUSES before reading its stdin — an absent store, an unknown
                // key — has closed the pipe by the time this write lands, and on Linux that is
                // `EPIPE`, not a silent short write as on Windows. Refusing without consuming
                // the value is exactly the behaviour those cases assert, so a broken pipe here is
                // the expected shape of a correct refusal, never a harness failure. Caught by
                // Linux CI on the first run; the Windows box could not see it.
                match child.stdin.take().expect("piped").write_all(text.as_bytes()) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
                    Err(e) => panic!("feeding the child's stdin failed: {e}"),
                }
                child.wait_with_output().expect("the child must finish")
            }
        }
    }
}

/// A rung of `crates/vike-cli/src/exit.rs`'s ladder as the number a shell sees. Taken from the enum
/// rather than written out, so this file states no exit code of its own — the ladder is a public
/// interface and it has exactly ONE definition.
fn rung(exit: vike_cli::exit::Exit) -> i32 {
    exit as i32
}

fn exit_code(o: &Output) -> i32 {
    o.status.code().expect("the process exited rather than being signalled")
}

/// The store every `set` case starts from: comments, a blank line, an unrelated venue, and one key
/// the replace case targets. Every byte of it that a write does not name must survive.
const SET_STORE: &str = "# vike credential store\n\
                         # hand-written, and it stays hand-written\n\
                         \n\
                         BINANCE_LIVE_API_KEY=old-key-value\n\
                         OKX_DEMO_API_PASSPHRASE=\"quoted pass\"\n";

/// **The stdin form: the key is APPENDED, every other byte survives, the value reaches neither
/// stream, and the exit is clean.**
///
/// The byte-for-byte assertion is the point. `vike_secrets::save_credentials` is the workspace's
/// one upsert precisely because the store is the user's only copy of live venue keys, and a CLI
/// writer is a third surface that property has to hold on — reason 1 in `docs/decisions/0036`.
#[test]
fn set_from_stdin_appends_the_key_and_preserves_every_other_byte() {
    let c = SetCase::new("stdin");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BYBIT_DEMO_API_KEY"], Some("piped-secret-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let after = c.read_store();
    // The store is the ORIGINAL text plus exactly one line.
    assert_eq!(
        after,
        format!("{SET_STORE}BYBIT_DEMO_API_KEY=piped-secret-value\n"),
        "a set must append one line and touch nothing else"
    );
    // The trailing newline the shell put on the pipe is NOT part of the credential.
    assert!(!after.contains("piped-secret-value\n\n"));

    // The report names the key and the file, and says which of the two things happened.
    let text = stdout(&out);
    assert!(text.contains("BYBIT_DEMO_API_KEY"), "{text}");
    assert!(text.contains("appended"), "{text}");
    assert!(text.contains(&c.store().display().to_string()), "{text}");

    // …and the VALUE is on neither stream. This is the assertion the whole command is shaped
    // around: a writer that echoed what it wrote would put the credential in the scrollback of
    // every session that used it.
    assert!(!text.contains("piped-secret-value"), "a VALUE reached stdout: {text}");
    assert!(!stderr(&out).contains("piped-secret-value"), "a VALUE reached stderr");
}

/// **The `--from-env` form**, taking the value out of the map the dispatcher swept — the second and
/// last accepted source, and the one a deploy script uses.
#[test]
fn set_from_env_takes_the_named_variable() {
    let c = SetCase::new("from-env");
    c.write_store(SET_STORE);

    let out = c.run(
        &["set", "BYBIT_DEMO_API_SECRET", "--from-env", "VIKE_TEST_SECRET_SOURCE"],
        None,
        &[("VIKE_TEST_SECRET_SOURCE", "env-sourced-value")],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert_eq!(c.read_store(), format!("{SET_STORE}BYBIT_DEMO_API_SECRET=env-sourced-value\n"));
    assert!(!stdout(&out).contains("env-sourced-value"), "a VALUE reached stdout");
    assert!(!stderr(&out).contains("env-sourced-value"), "a VALUE reached stderr");

    // An UNSET variable is a usage error naming the variable, and writes nothing.
    let out = c.run(&["set", "BYBIT_DEMO_API_KEY", "--from-env", "VIKE_TEST_NOT_SET"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("VIKE_TEST_NOT_SET"), "{}", stderr(&out));
    assert!(!c.read_store().contains("BYBIT_DEMO_API_KEY"), "nothing may be written");
}

/// **A REPLACE changes exactly one line, in place**, and leaves the old value nowhere in the file.
#[test]
fn set_replaces_an_existing_key_in_place_and_says_so() {
    let c = SetCase::new("replace");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("rotated-key-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));

    let after = c.read_store();
    let (before_lines, after_lines): (Vec<&str>, Vec<&str>) =
        (SET_STORE.lines().collect(), after.lines().collect());
    assert_eq!(before_lines.len(), after_lines.len(), "a replace must add no line");
    for (i, (b, a)) in before_lines.iter().zip(after_lines.iter()).enumerate() {
        if b.starts_with("BINANCE_LIVE_API_KEY") {
            assert_eq!(*a, "BINANCE_LIVE_API_KEY=rotated-key-value");
            continue;
        }
        assert_eq!(b, a, "line {i} must be byte-identical");
    }
    assert!(!after.contains("old-key-value"), "the old value must not survive");
}

/// **A VALUE IN ARGV IS A USAGE ERROR, and the refusal quotes nothing.**
///
/// Reason 2 in `docs/decisions/0036`: a CLI is the surface people SCRIPT, and a value in argv lands
/// in shell history and in `ps` output for every user on the box. The last assertion is why the
/// message is a constant rather than a `format!` — a refusal that echoed the token would write the
/// credential into the very scrollback it exists to keep it out of.
#[test]
fn a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed() {
    let c = SetCase::new("argv");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY", "sk-live-never-print-me"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("may not be given on the command line"), "{err}");
    assert!(err.contains("shell history"), "the refusal must say WHY: {err}");
    assert!(err.contains("--from-env") && err.contains("stdin"), "both forms: {err}");
    assert!(!err.contains("sk-live-never-print-me"), "the refusal ECHOED the value: {err}");
    assert!(!stdout(&out).contains("sk-live-never-print-me"), "the refusal ECHOED the value");

    // …and the store is untouched.
    assert_eq!(c.read_store(), SET_STORE);
}

/// **An unknown key is refused BY NAME on the usage rung**, with the nearest real names.
///
/// Reason 3 in `docs/decisions/0036`: the store is a flat `KEY=VALUE` file, so a writer that
/// accepted any name would write `BINANCE_LIVE_API_KEY_` as happily as the real key — and the venue
/// would then stay on paper with no error anywhere.
#[test]
fn an_unknown_key_is_refused_by_name_and_writes_nothing() {
    let c = SetCase::new("unknown-key");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY_"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BINANCE_LIVE_API_KEY_"), "the refusal must name the key: {err}");
    assert!(err.contains("BINANCE_LIVE_API_KEY"), "…and suggest the real one: {err}");
    assert_eq!(c.read_store(), SET_STORE, "a refused key must write nothing");
    assert!(!c.read_store().contains("never-written"));
}

/// **A key this workspace READS is refused WITHOUT being called dead**, over the shipped binary.
///
/// `vike-cli secrets set VIKE_TRADEHUB_OBSERVE_KEY` used to answer "is not a credential key this
/// workspace reads, so setting it would write a line nothing would ever load". Measured on the CI box,
/// 2026-09-07, and false: `crates/vike-tradehub/src/tradehub_cli.rs`'s `start_observe_server` reads
/// that exact name out of the credential map, and the escape-hatch paragraph beneath it pointed at
/// per-bridge config loaders, which a node key does not have. The operator was told a key they had
/// just configured was inert, and given no route at all.
///
/// The REFUSAL is unchanged and deliberately so. ⚠ Its REASON is not, and the old one is stated
/// here only to be retired: this said `set` writes "the enumerable grid and nothing wider", which
/// stopped being true when `settable_outside_the_grid` admitted every name the settings registry
/// proves a reader for. A node key stays refused on its own merits — it is MINTED, and a
/// hand-pasted 256-bit HMAC that is truncated fails as an opaque auth denial — which is the narrower
/// and more durable argument. `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`
/// fences the surface and this test must not be read as reopening it. What is asserted here is that the refusal
/// tells the truth, names a reader, and points at the route that exists today. The unit twin
/// (`the_node_keys_refusal_names_the_command_that_mints_them`) covers the whole set of such names
/// and the property over the registry; this one covers the two things only the real binary shows —
/// the RUNG and the STREAM.
///
/// ⚠ **The ROUTE half of this test INVERTED, and the inversion is the point.** It used to assert the
/// message named `vike-cli secrets path` and did NOT name `vike-cli node setup` — the verb this
/// command carried then — with the reason written beside it:
/// *"a node-key GENERATOR is designed and NOT BUILT, and naming one in a refusal
/// would be this same defect one step on — a route that fails at the terminal instead of a key that
/// loads nothing."* That was right while it was true. The generator now exists, so the editor route
/// is the stale answer and the command is the live one; the test's real invariant — **a refusal
/// names the route that exists TODAY, and never one that does not** — is unchanged, and it is what
/// both versions assert.
#[test]
fn a_key_the_workspace_reads_is_refused_without_being_called_unloadable() {
    let c = SetCase::new("read-elsewhere");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "VIKE_TRADEHUB_OBSERVE_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "the refusal must name the key: {err}");
    assert!(!err.contains("nothing would ever load"), "the measured lie is back: {err}");
    assert!(err.contains("IS read by this workspace"), "{err}");
    // The route that exists today, and which BOX to run it on — a node key is minted on the
    // daemon's box and carried to a client, so a command with no box named is half an instruction.
    assert!(err.contains("backend setup"), "the operator must be given a route: {err}");
    assert!(err.contains("backend connect"), "…including the client's half: {err}");
    // ⚠ WHICH BOX. This asserted the literal "DAEMON's box", which was an answer while there was
    // ONE daemon and stopped being one when `vike-datahub` grew a pair of its own. The message names
    // the SERVICE now, which is the thing an operator has to get right.
    assert!(err.contains("vike-tradehub"), "which box, by service: {err}");
    // …and it must not send anybody to an editor to invent a 256-bit HMAC key, which is exactly the
    // step `backend setup` exists to delete.
    assert!(!err.contains("EDITOR"), "the stale route is back: {err}");
    assert_eq!(c.read_store(), SET_STORE, "a refused key must write nothing");
    assert!(!c.read_store().contains("never-written"));

    // ⚠ THE DATAHUB PAIR, AND THE INVARIANT THIS TEST'S OWN DOC STATES: a refusal names the route
    // that exists TODAY and never one that does not. `vike-cli datahub setup` is built;
    // `datahub connect` is NOT, and an earlier draft of this arm promised it — the same defect the
    // doc above records paying for once already, one service over.
    let out = c.run(&["set", "VIKE_DATAHUB_CONTROL_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("VIKE_DATAHUB_CONTROL_KEY"), "{err}");
    assert!(err.contains("datahub setup"), "the datahub's own minting command: {err}");
    assert!(err.contains("vike-datahub"), "and its box: {err}");
    assert!(
        !err.contains("backend setup"),
        "it must not route a datahub key at the tradehub: {err}"
    );
    assert!(
        !err.contains("datahub connect"),
        "there is no such command — a refusal may not invent one: {err}"
    );
    assert!(!err.contains("EDITOR"), "{err}");
    assert_eq!(c.read_store(), SET_STORE, "a refused key must write nothing");
}

/// **An ABSENT store is refused, and the refusal names the command that creates one.**
///
/// This command upserts and creates nothing: `template` writes the grid to stdout and the operator
/// redirects it, which is the shape this module's doc argues at length. The rung is FAILED rather
/// than USAGE — the command line was fine, the box is not configured — and that distinction is the
/// whole reason the ladder exists.
#[test]
fn an_absent_store_is_refused_and_names_the_template_command() {
    let c = SetCase::new("absent");
    // Deliberately NO store written.

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("no credential store"), "{err}");
    assert!(err.contains("secrets template"), "the refusal must name how to make one: {err}");
    assert!(err.contains(&c.store().display().to_string()), "…and which file: {err}");
    assert!(!c.store().exists(), "a refused set must CREATE no store");
}

/// **The write is JOURNALLED: one `credential_write` record, carrying the key NAME and no value.**
///
/// The ledger is the durable answer to *when did this credential last change* —
/// `vike_model::change_journal`'s module doc measures the `tracing` alternative as deleted within
/// days and, on the the CI box daemon, never written at all. The value assertion is over the RAW LINE
/// rather than over a field: a record type that grew a value cell would pass a field-level check
/// while doing exactly the thing that must be impossible.
#[test]
fn the_write_is_journalled_once_with_the_key_name_and_no_value() {
    let c = SetCase::new("journal");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BYBIT_DEMO_API_KEY"], Some("journalled-secret-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let lines = c.journal_lines();
    assert_eq!(lines.len(), 1, "ONE write ⇒ ONE record: {lines:?}");
    let line = &lines[0];
    assert!(line.contains("BYBIT_DEMO_API_KEY"), "the record must carry the key NAME: {line}");
    assert!(line.contains("bybit"), "…and the venue it belongs to: {line}");
    assert!(line.contains("secrets.env"), "…and which store: {line}");
    assert!(line.contains("vike-cli"), "…and that the CLI was the actor: {line}");
    assert!(!line.contains("journalled-secret-value"), "a VALUE reached the ledger: {line}");

    // A REFUSED set records nothing — the ledger says what happened, not what was attempted.
    let out = c.run(&["set", "NOT_A_REAL_KEY"], Some("x\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert_eq!(c.journal_lines().len(), 1, "a refusal must append no record");
}

/// **No value on stdin is a usage error**, not an empty credential written over a real one.
///
/// An empty value is equivalent to an absent key — the venue stays on paper — so writing one would
/// report success for a change that arms nothing.
#[test]
fn an_empty_value_is_refused_rather_than_written() {
    let c = SetCase::new("empty");
    c.write_store(SET_STORE);

    // Closed stdin — what a shell hands a process with no pipe.
    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("no value on stdin"), "{}", stderr(&out));

    // …and whitespace is not a value either.
    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("   \n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));

    assert_eq!(c.read_store(), SET_STORE, "the existing credential must survive both");
}

/// **A value that BEGINS WITH A DASH is refused like any other argv value — and not echoed.**
///
/// The sibling case above proves the property on a value starting with a letter, which is the only
/// class it ever tested. A dash-leading token missed the positional arm's guard and fell through to
/// the generic `unknown option '{other}'`, which printed the credential verbatim to STDERR — the
/// stream CI logs and every service manager captures. base64url alphabets contain `-`, so this is
/// an ordinary credential rather than a contrived one, and the refusal was doing the exact damage
/// it exists to prevent.
#[test]
fn a_dash_leading_value_in_argv_is_refused_and_never_echoed() {
    let c = SetCase::new("argv-dash");
    c.write_store(SET_STORE);

    for value in ["-sk-live-never-print-me", "-----BEGIN-never-print-me"] {
        let out = c.run(&["set", "BINANCE_LIVE_API_KEY", value], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
        assert!(
            !stderr(&out).contains("never-print-me") && !stdout(&out).contains("never-print-me"),
            "the refusal ECHOED the value: {} / {}",
            stderr(&out),
            stdout(&out)
        );
    }
    assert_eq!(c.read_store(), SET_STORE, "a refused value must write nothing");
}

/// **`--file` cannot aim a WRITE at a path the operator names.**
///
/// It is an inspection flag — the three reading subcommands keep it — and it used to resolve the
/// same way for `set`, so `set KEY --file ~/.bashrc` appended a live credential to a shell rc file
/// and exited 0, with the change journal recording the write against a "store" named `bashrc`.
/// `docs/decisions/0036` fixes this verb as an upsert into the PROJECT's store; a scripted run that
/// must aim elsewhere moves the whole settings directory with `$VIKE_SETTINGS_DIR`.
#[test]
fn set_refuses_to_write_into_a_file_named_on_the_command_line() {
    let c = SetCase::new("file-flag");
    c.write_store(SET_STORE);
    // An ordinary, non-credential file that happens to exist — the class this defect reached.
    let bystander = c.dir().join("bashrc");
    let bystander_text = "export PATH=/usr/local/bin\n";
    std::fs::write(&bystander, bystander_text).unwrap();

    let out = c.run(
        &["set", "BINANCE_LIVE_API_KEY", "--file", &bystander.display().to_string()],
        Some("sk-written-here\n"),
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file"), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&bystander).unwrap(),
        bystander_text,
        "a file named on the command line must not be written"
    );
    assert_eq!(c.read_store(), SET_STORE, "…and neither is the real store");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");
}

/// **A MULTI-LINE `--from-env` value is refused, and nothing is written.**
///
/// The blocker this case exists for: the value was taken verbatim, `vike_secrets::upsert_env`
/// quoted it (a newline is whitespace) and joined with a newline, so the value's own break became a
/// physical line break — and `parse_dotenv` then read the first half as a SILENTLY TRUNCATED
/// credential and the second half as a WHOLE NEW `KEY=VALUE` for a venue the operator never
/// configured. Exit 0, "appended", `Outcome::Applied` in the ledger. `--from-env` is the
/// CI/deploy-script form and a multi-line secret is the ordinary shape of a Vault- or
/// Actions-injected variable.
///
/// The `secrets list` assertion is the end-to-end half: it is what showed the injected key as a
/// third credential and a third ACCOUNT when this was reproduced.
#[test]
fn a_multiline_env_value_cannot_inject_a_second_key() {
    let c = SetCase::new("from-env-multiline");
    c.write_store(SET_STORE);

    for raw in ["abc\nOKX_LIVE_API_SECRET=injected-by-a-newline", "trailing-newline\n"] {
        let out = c.run(
            &["set", "BYBIT_DEMO_API_KEY", "--from-env", "VIKE_TEST_MULTILINE"],
            None,
            &[("VIKE_TEST_MULTILINE", raw)],
        );
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("more than one line"), "{err}");
        assert!(err.contains("VIKE_TEST_MULTILINE"), "the refusal must name the VARIABLE: {err}");
        assert!(!err.contains("injected-by-a-newline"), "the refusal ECHOED the value: {err}");
    }

    assert_eq!(c.read_store(), SET_STORE, "a refused value must write nothing at all");
    assert!(c.journal_lines().is_empty(), "…and record nothing");

    // …and the store still holds exactly what it held: no third key, no third account.
    let out = c.run(&["list"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(!stdout(&out).contains("OKX_LIVE_API_SECRET"), "a key was injected: {}", stdout(&out));
}

/// **A DUPLICATED key is rotated on EVERY line, because the reader is LAST-wins.**
///
/// The writer replaced the FIRST matching line and preserved the rest verbatim, while
/// `vike_secrets::parse_dotenv` inserts per line — so the value every loader in the workspace read
/// was the stale one further down the file. The command printed "replaced", exited 0 and journalled
/// `Applied` for a rotation that changed nothing that is read; the daemon kept signing with the old
/// key. Duplicates arrive from ordinary hand-editing and from `secrets template >>` (the append typo
/// of the documented `>` form), which duplicates the entire grid.
#[test]
fn a_duplicated_key_is_rotated_on_every_line_the_reader_might_return() {
    let c = SetCase::new("duplicate-key");
    let store = "# hand-edited twice\n\
                 BINANCE_LIVE_API_KEY=first-old-value\n\
                 OKX_DEMO_API_PASSPHRASE=\"quoted pass\"\n\
                 BINANCE_LIVE_API_KEY=second-old-value\n";
    c.write_store(store);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("rotated-new-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));

    let text = c.read_store();
    assert!(!text.contains("first-old-value"), "the first occurrence must be rotated: {text}");
    assert!(
        !text.contains("second-old-value"),
        "the LAST occurrence is the one the reader returns, so it must be rotated too: {text}"
    );
    assert!(text.contains("OKX_DEMO_API_PASSPHRASE=\"quoted pass\""), "unnamed keys survive");
    assert_eq!(text.lines().count(), 4, "no line added or dropped: {text}");

    // The end-to-end half: `list` reads the store through the same parser every loader does, so
    // what it returns is what the box would sign with.
    let out = c.run(&["list"], None, &[]);
    assert!(!stdout(&out).contains("second-old-value"), "{}", stdout(&out));
}

/// **A LABELLED ACCOUNT is WRITTEN — under its own name, and the DEFAULT account's key is left
/// exactly as it was.** Over the shipped binary, because the defect this replaces was a two-command
/// sequence an operator performed, not a sentence.
///
/// `KEY__LABEL` names a second account — a name `vike_model::account_keys` parses,
/// `vike_bridge_core::credentials::load_credentials_for_account` reads, and `secrets list` prints.
/// `set` REFUSED it (the grid is a fixed enumeration; a label is unbounded) and sent the operator to
/// an editor — which, on a migrated box, edits a file `docs/decisions/0054`'s credential half means
/// no reader opens. So a credential the operator could SEE listed had no writer anywhere.
///
/// ⚠ **The hazard that refusal was written about is NOT the unboundedness** and does not go with
/// it: the suggestion list scored the UNLABELLED base as the nearest name and offered it first —
/// real, settable, and a DIFFERENT ACCOUNT — so the operator's obvious next command overwrote the
/// credential their primary account signs with, exit 0, "replaced". Writing the labelled name is
/// what ends that: the key typed is the key written, and this test asserts the base is untouched
/// byte for byte, which is the property the old refusal was only ever a proxy for.
#[test]
fn a_labelled_account_is_written_and_the_base_key_is_untouched() {
    let c = SetCase::new("labelled-account");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY__ALT"], Some("alt-account-key\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    // ⚠ THE HALF THAT MATTERS, and it is asserted as an EXACT append rather than a `contains`: the
    // store is the original text plus exactly one line, so `BINANCE_LIVE_API_KEY=old-key-value` —
    // the DEFAULT account's, the one the old suggestion invited an operator to clobber — is still
    // byte-for-byte what it was.
    assert_eq!(
        c.read_store(),
        format!("{SET_STORE}BINANCE_LIVE_API_KEY__ALT=alt-account-key\n"),
        "the labelled key is appended and nothing else moves"
    );
    // …and the value never reached a stream.
    assert!(!stderr(&out).contains("alt-account-key"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("alt-account-key"), "{}", stdout(&out));
}

// ---- the settings DATABASE answers, and the SHIPPED binary says so ------------------------------
//
// `docs/decisions/0054`'s credential half lets `<project>/settings/db/vike.db` answer for the
// credential store. Everything below drives the REAL `vike-cli` binary against a REAL migrated
// project — a `settings/` holding a credential file AND the database that shadows it — because the
// defect these close was that the CLI reported on the FILE: a path from the dead store beside a key
// count read out of it, and not one sentence anywhere saying the file had stopped being read.

/// A project on disk: `<root>/settings/secrets.env`, optionally migrated into
/// `<root>/settings/db/vike.db`.
///
/// ⚠ It drives `secrets` with `$VIKE_SETTINGS_DIR` rather than `--file`, and that is the point:
/// `--file` names a text file outright and must keep doing so, while the project's own store is
/// whatever `vike_secrets::backend_in` says it is. Every assertion below is about the second.
struct Project {
    root: tempfile::TempDir,
}

impl Project {
    fn new(tag: &str) -> Project {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-db-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).expect("settings dir");
        Project { root }
    }

    fn settings(&self) -> PathBuf {
        self.root.path().join("settings")
    }

    fn store(&self) -> PathBuf {
        self.settings().join("secrets.env")
    }

    fn db(&self) -> PathBuf {
        self.settings().join("db").join("vike.db")
    }

    fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).expect("write store");
    }

    /// Fill the database from the file, through the library the migration lives in. ⚠ The file is
    /// left exactly where it is — retiring it is an OPERATOR act and nothing in this workspace
    /// performs one — which is precisely the state these tests are about.
    fn migrate(&self) {
        let before = std::fs::read(self.store()).expect("read the store");
        // The PRODUCTION account classification — these tests are about the CLI's behaviour over a
        // migrated store, so they have no business carrying a second opinion about which account a
        // key belongs to.
        vike_secrets::migrate(
            Some(self.settings().to_str().expect("utf-8")),
            |_| false,
            &vike_bridge_core::credentials::classify_credential_name,
        )
        .expect("migrate");
        assert!(self.db().is_file(), "the fixture must really be migrated");
        assert_eq!(
            std::fs::read(self.store()).expect("read the store"),
            before,
            "the migration must not have touched the operator's only copy of their keys"
        );
    }

    /// Run `vike-cli secrets <args…>` inside this project, with no `--file`.
    fn secrets(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("secrets")
            .args(args)
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", self.settings())
            .output()
            .expect("the vike-cli binary must run")
    }

    /// Run `vike-cli secrets <args…>` inside this project with `value` on stdin — the one shape a
    /// WRITE takes, since `set` accepts a value from stdin or a named variable and never from argv.
    fn secrets_with_stdin(&self, args: &[&str], value: &str) -> Output {
        use std::io::{ErrorKind, Write};
        let mut child = Command::new(BIN)
            .arg("secrets")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("VIKE_SETTINGS_DIR", self.settings())
            .spawn()
            .expect("the vike-cli binary must run");
        // A child that refuses before reading stdin has already closed the pipe — `EPIPE` on Linux,
        // and the expected shape of a correct refusal. Same note as `SetCase::run`'s.
        match child.stdin.take().expect("piped").write_all(value.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
            Err(e) => panic!("feeding the child's stdin failed: {e}"),
        }
        child.wait_with_output().expect("the child must finish")
    }

    /// Every `changes-*.jsonl` line the ledger under `<settings>/state/changes` holds.
    fn journal_lines(&self) -> Vec<String> {
        let dir = self.settings().join("state").join("changes");
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("changes-") || !name.ends_with(".jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).expect("read the ledger");
            out.extend(text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string));
        }
        out
    }

    /// Run `vike-cli config check` inside this project.
    fn config_check(&self) -> Output {
        Command::new(BIN)
            .args(["config", "check"])
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", self.settings())
            .output()
            .expect("the vike-cli binary must run")
    }

    /// Run `vike-cli config show …` inside this project.
    ///
    /// ⚠ `env_clear`, like `crates/vike-cli/tests/config_cli.rs`'s `Case::run` — an exported
    /// `RUST_LOG` on a developer box moves a row's SOURCE to `env`, which is precisely the column
    /// these cases measure.
    fn config_show(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(["config", "show"]).args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", self.settings());
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }
}

/// **`secrets list` reads the DATABASE on a migrated project, and says the file is shadowed.**
///
/// Two defects in one run. `list` handed `vike_secrets::resolve` a path it had built itself, which
/// is the FILE arm by definition — so on a migrated box it printed the dead store's path and the
/// dead store's key count, confidently. And `ShadowedStore`, the finding that exists to tell an
/// operator their hand-edit stopped mattering, was returned as data and printed by NOTHING: a grep
/// for it across `crates/` outside `vike-secrets` found zero consumers.
#[test]
fn list_reads_the_database_and_reports_the_shadowed_file() {
    let p = Project::new("list");
    p.write_store(SAMPLE);
    p.migrate();

    // A key added to the FILE after the migration. It is in the file and not in the database, so a
    // listing that shows it is a listing of the dead store.
    std::fs::write(
        p.store(),
        format!("{SAMPLE}BYBIT_DEMO_API_KEY=added-to-the-file-after-the-migration\n"),
    )
    .expect("append");

    let out = p.secrets(&["list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (text, err) = (stdout(&out), stderr(&out));

    assert!(text.contains("DATABASE"), "the source line must name the store that answered: {text}");
    assert!(text.contains("vike.db"), "…and its path: {text}");
    assert!(
        !text.contains("BYBIT_DEMO_API_KEY"),
        "the listing showed a key that exists only in the SHADOWED file: {text}"
    );
    assert!(text.contains("BINANCE_LIVE_API_KEY"), "…while the migrated keys are there: {text}");

    assert!(
        err.contains("NO LONGER READ"),
        "the shadowed file must be REPORTED — this was returned as data and printed by nothing: \
         {err}"
    );
    assert!(err.contains("secrets.env"), "{err}");

    // The rule that outranks the rest: nothing here read a value out loud.
    for value in ["sup3r-s3cr3t-value", "key-abcd1234", "added-to-the-file-after-the-migration"] {
        assert!(
            !text.contains(value) && !err.contains(value),
            "a VALUE reached a stream: {text}{err}"
        );
    }
}

/// **`secrets path` names the database and says the store line is a file nobody reads.**
///
/// `path` is the command an operator runs FIRST, before they know what is wrong, and it is the one
/// that answers *which store are my keys actually coming from*. Printing the file alone on a
/// migrated box made it the thing it exists to prevent.
///
/// ⚠ It still OPENS NOTHING: the backend choice is one `is_file` on one path, the same probe every
/// reader makes.
#[test]
fn path_names_the_database_when_it_answers() {
    let p = Project::new("path");
    p.write_store(SAMPLE);
    p.migrate();

    let out = p.secrets(&["path"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("vike.db"), "the database must be named: {text}");
    assert!(text.contains("NO LONGER READ"), "…and the file's status said plainly: {text}");
    assert!(
        !text.contains("chmod 600"),
        "the create-a-file hint is a lie on a migrated box: {text}"
    );
}

/// **An UNMIGRATED project's `secrets path` output is byte-identical to before any of this landed.**
///
/// The property that makes the whole change mergeable, asserted on a PUBLISHED surface rather than
/// only on library behaviour: this verb's output is what operators paste into issues and what
/// runbooks quote. A permanent `db: … (absent)` row on every box would be a change to that surface
/// in exchange for saying nothing.
#[test]
fn path_on_an_unmigrated_project_says_nothing_about_a_database() {
    let p = Project::new("path-plain");
    p.write_store(SAMPLE);

    let out = p.secrets(&["path"]);
    let text = stdout(&out);
    assert!(text.contains("secrets.env"), "{text}");
    assert!(text.contains("present"), "{text}");
    assert!(!text.contains("vike.db"), "no database exists, so none may be mentioned: {text}");
    assert!(!text.contains("db:"), "{text}");
    assert!(!text.contains("NO LONGER READ"), "{text}");
    assert!(!stderr(&out).contains("NO LONGER READ"), "{}", stderr(&out));
}

/// **`secrets set` writes the DATABASE on a migrated project, and the shadowed file does not move.**
///
/// The write half. Before this, `set` read the file to decide replaced-vs-appended, refused an
/// absent FILE, and wrote the FILE — three answers about a store nothing reads. The proof is read
/// back through `list`, i.e. through the resolver a daemon would use, rather than by inspecting the
/// row store.
#[test]
fn set_writes_the_store_that_answers_and_leaves_the_shadowed_file_alone() {
    let p = Project::new("set");
    p.write_store(SAMPLE);
    p.migrate();
    let file_before = std::fs::read(p.store()).expect("read");

    let mut cmd = Command::new(BIN);
    cmd.arg("secrets")
        .args(["set", "BINANCE_LIVE_API_KEY"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("VIKE_SETTINGS_DIR", p.settings());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"rotated-through-the-cli\n")
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));
    assert!(stdout(&out).contains("vike.db"), "it must say WHERE it landed: {}", stdout(&out));

    // ⚠ THE RULE THAT OUTRANKS THE REST. A write routed past the file must not have touched it.
    assert_eq!(
        std::fs::read(p.store()).expect("read"),
        file_before,
        "the shadowed credential file was modified by a write that did not go to it"
    );

    // …and the new value is what the reader sees. `list` prints NAMES only, so the round trip is
    // asserted through the library's resolver, which is the thing a daemon calls.
    let resolved =
        vike_secrets::resolve_project(Some(p.settings().to_str().expect("utf-8"))).expect("read");
    assert!(matches!(resolved.source, vike_secrets::Source::Database(_)), "{:?}", resolved.source);
    assert_eq!(
        resolved.secrets.clone().into_map().remove("BINANCE_LIVE_API_KEY").as_deref(),
        Some("rotated-through-the-cli"),
        "the write did not reach the store that answers"
    );
}

/// **`config check`'s store row reports the store that answers, and flags the shadowed file.**
///
/// `Source::Database(_)` was folded into the `Source::File(_)` arm, so a migrated box printed the
/// FILE path beside a key count read from the dead store: two wrong halves reading as one confident
/// answer.
#[test]
fn config_check_reports_the_database_and_the_shadowed_file() {
    let p = Project::new("check");
    p.write_store(SAMPLE);
    p.migrate();
    // A key only the FILE has, so a row counting the file is distinguishable from one counting the
    // database.
    std::fs::write(p.store(), format!("{SAMPLE}BYBIT_DEMO_API_KEY=only-in-the-file\n"))
        .expect("append");

    let out = p.config_check();
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("vike.db"), "the row must name the store that answered: {text}");
    assert!(text.contains("3 key(s)"), "…and COUNT it, not the shadowed file's 4: {text}");
    assert!(text.contains("NO LONGER READ"), "…and flag the file: {text}");
    assert!(!text.contains("only-in-the-file"), "a VALUE reached a stream: {text}");
}

/// **`config show`'s PROVENANCE names the store that answered.**
///
/// `config show` exists to answer *where did this value come from*, and on a migrated box it
/// answered with the file: the header's store row was labelled `secrets.env` beside a key count
/// read out of the database, the precedence line advertised `env > secrets.env > default` for a
/// file nothing consults, and every store-sourced row printed a flat, confident `dotenv`. That is
/// the failure `docs/decisions/0054-settings-move-into-one-database.md`'s constraint 2 names —
/// positive confirmation of something false — landing in the one command whose entire product is
/// provenance. Its sibling `config check` was repaired first; this is the other half.
#[test]
fn config_show_names_the_database_and_the_file_it_shadows() {
    let p = Project::new("show");
    p.write_store(SAMPLE);
    p.migrate();
    // A key only the FILE has, so a count of the file is distinguishable from a count of the
    // database, and its VALUE is a tripwire for any path that reads the shadowed store.
    std::fs::write(p.store(), format!("{SAMPLE}BYBIT_DEMO_API_KEY=only-in-the-file\n"))
        .expect("append");

    let out = p.config_show(&["--filter", "BINANCE"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));

    assert!(text.contains("vike.db"), "the header must name the store that answered: {text}");
    assert!(
        text.contains("precedence: env > vike.db > default"),
        "the precedence line advertises a store; it must advertise the live one: {text}"
    );
    assert!(
        text.contains("the settings DATABASE, not a text file"),
        "…and say what the operator cannot `cat`: {text}"
    );
    assert!(
        text.contains("3 key(s)"),
        "the count must be the DATABASE's, not the shadowed file's 4: {text}"
    );
    assert!(text.contains("NO LONGER READ"), "…and the shadowed file must be flagged: {text}");
    assert!(
        !text.contains("only-in-the-file") && !text.contains("sup3r-s3cr3t-value"),
        "a VALUE reached a stream: {text}"
    );
}

/// …and the SOURCE column, which is the cell an operator actually reads.
#[test]
fn config_show_attributes_a_value_to_the_database_that_holds_it() {
    let p = Project::new("show-json");
    p.write_store(SAMPLE);
    p.migrate();

    let out = p.config_show(&["--json", "--filter", "BINANCE_LIVE_API_KEY"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");

    assert_eq!(doc["secrets"]["kind"], "database", "{}", doc["secrets"]);
    assert!(
        doc["secrets"]["path"].as_str().expect("a path").ends_with("vike.db"),
        "{}",
        doc["secrets"]
    );
    assert!(
        doc["secrets"]["shadowed"]["file"]
            .as_str()
            .expect("the shadowed file")
            .ends_with("secrets.env"),
        "{}",
        doc["secrets"]
    );

    let rows = doc["env"].as_array().expect("the env half");
    let row = rows
        .iter()
        .find(|r| r["name"] == "BINANCE_LIVE_API_KEY")
        .unwrap_or_else(|| panic!("no row for the key the database holds: {rows:#?}"));
    assert_eq!(row["source"], "database", "the row must name the store that answered: {row}");
    // …and the rule that outranks every other one here.
    assert_eq!(row["value"], "<set>", "a credential row discloses presence, never a value: {row}");
    assert!(!stdout(&out).contains("key-abcd1234"), "a VALUE reached the document");
}

/// **An UNMIGRATED project's `config show` is what it always was.** The property that makes this
/// mergeable: the fix must be invisible on every box that has not migrated, because that output is
/// what operators paste into issues and what this command's own regression baseline is built on.
#[test]
fn config_show_on_an_unmigrated_project_says_nothing_about_a_database() {
    let p = Project::new("show-plain");
    p.write_store(SAMPLE);

    let out = p.config_show(&["--filter", "BINANCE"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("secrets.env"), "{text}");
    assert!(
        text.contains("precedence: env > secrets.env > default"),
        "the precedence line is unchanged on an unmigrated box: {text}"
    );
    assert!(!text.contains("vike.db"), "no database exists, so none may be mentioned: {text}");
    assert!(!text.contains("NO LONGER READ"), "nothing shadows anything here: {text}");
    assert!(!text.contains("DATABASE"), "{text}");
}

/// **`secrets set` records the store the key LANDED in — not the file path it computed first.**
///
/// ⚠ The command already knew the answer and did not use it: `run_set` computed `where_it_landed`
/// from the `Backend` the writer returned, printed it in the success sentence, and handed the
/// change journal the FILE path from three lines earlier. So on a migrated box every
/// `credential_write` record named `secrets.env` for a key that went into the database — an
/// append-only ledger asserting a store the write never touched, which is worse than one asserting
/// nothing, because it is the record an incident is reconstructed from.
///
/// The three node verbs (`node/setup.rs`, `node/connect.rs`, `datahub.rs`) pass `node::landed` and
/// have been right since they were written; one of two sibling paths had the rule, which is the
/// shape this repo has learned to look for.
#[test]
fn set_journals_the_database_it_wrote_and_not_the_shadowed_file() {
    let p = Project::new("set-journal");
    p.write_store(SAMPLE);
    p.migrate();
    let before = std::fs::read(p.store()).expect("read the store");

    let out = p.secrets_with_stdin(&["set", "BYBIT_DEMO_API_KEY"], "not-a-real-credential\n");
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(
        stdout(&out).contains("vike.db"),
        "the success sentence must name the store written: {}",
        stdout(&out)
    );

    let lines = p.journal_lines();
    assert_eq!(lines.len(), 1, "ONE write ⇒ ONE record: {lines:?}");
    let line = &lines[0];
    assert!(
        line.contains("vike.db"),
        "the ledger's store cell must name the DATABASE the key landed in: {line}"
    );
    assert!(
        !line.contains("secrets.env"),
        "…and must NOT name the shadowed file, which this write never opened: {line}"
    );
    assert!(line.contains("BYBIT_DEMO_API_KEY"), "…while still carrying the key NAME: {line}");
    assert!(!line.contains("not-a-real-credential"), "a VALUE reached the ledger: {line}");

    assert_eq!(
        std::fs::read(p.store()).expect("read the store"),
        before,
        "and the operator's only copy of their keys is untouched by a write that went elsewhere"
    );
}

/// **Something outside this crate matches on these bytes with shell globs, and this is that end.**
///
/// `scripts/refuse_live_credentials.sh` — the guard both live-smoke lanes call, one of which places
/// real orders — inspects a migrated store by running `vike-cli secrets list --json` and testing its
/// stdout for `"kind":"database"` and `"keys":[`. It cannot parse JSON and must not start: a shell
/// JSON parser in a money guard is a second implementation of somebody else's schema.
///
/// ⚠ **This renderer PRETTY-PRINTS, which is why the guard flattens rather than matching raw.** The
/// first version of that guard matched the raw document, every one of its fixtures emitted compact
/// bytes, and its whole suite was green while the real binary printed `"kind": "database"` across a
/// dozen lines — i.e. the guard would have refused every migrated box, and no test anywhere could
/// see it. So the property this asserts is the one the guard actually depends on: with whitespace
/// removed, the document carries those two spellings. Either rendering satisfies it; a renamed or
/// dropped field does not. `crates/vike-ops/tests/smoke_guard_gate.rs`'s
/// `the_guard_matches_the_document_the_routed_reader_actually_prints` holds the other end.
#[test]
fn the_json_listing_survives_the_flattening_the_smoke_guard_performs() {
    let c = Case::new("list-json-flat");
    c.write_store(SAMPLE);
    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    let doc = stdout(&out);

    // The guard's `tr -d '[:space:]'`, spelled in Rust over the same bytes.
    let flat: String = doc.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        flat.contains("\"kind\":\"file\""),
        "the flattened document must carry the `kind` spelling the guard's `case` arm matches: \
         {doc}"
    );
    assert!(
        flat.contains("\"keys\":["),
        "…and the `keys` array, which is the only field the guard reads a value out of: {doc}"
    );
}

// ── migrate ─────────────────────────────────────────────────────────────────────────────────────
//
// ⚠ Every case drives its own temp settings directory through `$VIKE_SETTINGS_DIR` — the same
// isolation `SetCase` takes, and it is doubly load-bearing here: this verb CREATES a credential
// database inside whatever directory it resolves, so a case that fell through to the walk would
// build one inside the developer's checkout.

/// The store a migration case starts from: a hand-written file with comments, a blank line and a
/// venue whose key names are OUTSIDE the enumerable grid, because those are 57 of the 67 names on
/// the live box and a migration is only interesting on them.
const MIGRATE_STORE: &str = "# vike credential store\n\
                             # hand-written, and it stays hand-written\n\
                             \n\
                             BINANCE_LIVE_API_KEY=key-one\n\
                             DUKASCOPY_DEMO1_LOGIN=login-one\n\
                             HYPERLIQUID_LIVE_PRIVATE_KEY=0xdeadbeef\n";

/// The node file beside it — `docs/decisions/0051`'s own store, which the migration drains into the
/// second table.
const MIGRATE_NODE_STORE: &str = "VIKE_TRADEHUB_OBSERVE_KEY=observe-one\n\
                                  VIKE_TRADEHUB_CONTROL_KEY=control-one\n";

impl SetCase {
    fn node_store(&self) -> PathBuf {
        self.settings().join("node.env")
    }

    fn db(&self) -> PathBuf {
        self.settings().join("db").join("vike.db")
    }

    /// The store and the node file, as a migration case wants them.
    fn seed_for_migration(&self) {
        self.write_store(MIGRATE_STORE);
        std::fs::write(self.node_store(), MIGRATE_NODE_STORE).unwrap();
    }
}

/// **`--dry-run` creates NOTHING and says so, and the apply then does what it said.**
///
/// The first successful migration is irreversible in practice — from then on the database answers
/// for every process on the box and `secrets.env` is no longer read — so the assertion that matters
/// most here is the negative one: after a dry run there is no database and not even a `db/`
/// directory. The second half is what makes the dry run worth running: the numbers it printed are
/// the numbers the apply reports.
#[test]
fn migrate_dry_run_writes_nothing_and_then_the_apply_matches_it() {
    let c = SetCase::new("migrate-dry");
    c.seed_for_migration();
    let before = (c.read_store(), std::fs::read_to_string(c.node_store()).unwrap());

    let out = c.run(&["migrate", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let plan = stdout(&out);
    assert!(plan.contains("would be CREATED"), "{plan}");
    assert!(plan.contains("NOTHING WAS WRITTEN"), "{plan}");
    assert!(plan.contains("DRY RUN"), "{plan}");
    assert!(plan.contains("vike-cli secrets migrate"), "it must name the applying command: {plan}");

    assert!(
        !c.db().exists(),
        "A DRY RUN CREATED THE DATABASE — from here the credential file on this box is never read \
         again, which is the exact act the rehearsal exists to let somebody decide about first"
    );
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert!(c.journal_lines().is_empty(), "a rehearsal records nothing");
    assert_eq!(
        (c.read_store(), std::fs::read_to_string(c.node_store()).unwrap()),
        before,
        "a dry run touched a source file"
    );
    // A VALUE cannot reach either stream, on this verb as on `set`.
    let dry_err = stderr(&out);
    for text in [plan.as_str(), dry_err.as_str()] {
        assert!(!text.contains("0xdeadbeef"), "a VALUE reached a stream: {text}");
        assert!(!text.contains("observe-one"), "a VALUE reached a stream: {text}");
    }

    // …and now the real thing, which must report the same counts.
    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let done = stdout(&out);
    assert!(c.db().is_file(), "the apply must create the database: {done}");
    for row in plan.lines().filter(|l| l.contains("key(s) read")) {
        let read_count = row.split("key(s) read").next().expect("a prefix");
        assert!(
            done.lines().any(|l| l.starts_with(read_count)),
            "the apply reported a different per-file row than the plan promised:\n{plan}\n{done}"
        );
    }
}

/// **The migration lands every name — including the ones no grid can enumerate — and the FILES are
/// byte-identical afterwards.**
///
/// The second half is the rule that outranks everything else here: the credential file is the
/// operator's only copy of their live venue keys, and a migration is the exact place somebody
/// reaches for "…and then tidy it up". Retiring the file stays an operator act.
#[test]
fn migrate_creates_the_database_and_leaves_both_files_byte_identical() {
    let c = SetCase::new("migrate-create");
    c.seed_for_migration();

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("created"), "{text}");
    assert!(text.contains("READ ONLY"), "the report must say the files were only read: {text}");
    assert!(c.db().is_file(), "no database at {}", c.db().display());

    // ⚠ **The one fact an operator has to carry away**: the files have stopped being READ. The
    // report's "READ ONLY" line says they were not written, which is true and is a different claim
    // — and every runbook in this tree says *edit secrets.env*, which from this moment changes
    // nothing while looking exactly like it worked.
    assert!(text.contains("NO LONGER READ"), "{text}");
    assert!(text.contains("secrets.env") && text.contains("node.env"), "both files: {text}");

    assert_eq!(c.read_store(), MIGRATE_STORE, "the credential file was rewritten");
    assert_eq!(
        std::fs::read_to_string(c.node_store()).unwrap(),
        MIGRATE_NODE_STORE,
        "the node file was rewritten"
    );

    // The store that ANSWERS is now the database, and it holds the venue names — proven through
    // the shipped binary's own reader rather than by opening the file here.
    let listed = c.run(&["list"], None, &[]);
    assert_eq!(exit_code(&listed), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&listed));
    let out = stdout(&listed);
    for key in ["BINANCE_LIVE_API_KEY", "DUKASCOPY_DEMO1_LOGIN", "HYPERLIQUID_LIVE_PRIVATE_KEY"] {
        assert!(out.contains(key), "{key} did not survive the migration: {out}");
    }
    assert!(!out.contains("0xdeadbeef"), "a VALUE reached the listing: {out}");
    // …and the node keys are in the OTHER namespace, so `secrets list` does not print them.
    assert!(!out.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "a node key joined the venue grid: {out}");
}

/// **Twice is the same as once, over the shipped binary**, and the second run says so rather than
/// looking like a fresh success.
#[test]
fn migrate_is_idempotent_and_the_second_run_says_so() {
    let c = SetCase::new("migrate-twice");
    c.seed_for_migration();

    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));
    let first_bytes = std::fs::read(c.db()).unwrap();
    let first_records = c.journal_lines().len();

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("already complete"),
        "the second run must say it had nothing to do: {}",
        stdout(&out)
    );
    assert_eq!(std::fs::read(c.db()).unwrap(), first_bytes, "the database bytes moved on a no-op");
    assert_eq!(
        c.journal_lines().len(),
        first_records,
        "a run that wrote nothing appended a ledger record claiming it did"
    );
}

/// **A box with no credentials at all: `nothing to migrate`, exit 0, and NO database.**
///
/// Creating one is the harmful act — from that moment every process on the box reads the database,
/// so the credential file the operator writes afterwards is never read and every venue silently
/// stays on paper. A non-zero rung would be wrong for the opposite reason: this is the correct
/// outcome on a fresh install, not a failure.
#[test]
fn migrate_on_an_unconfigured_box_creates_no_database_and_exits_zero() {
    let c = SetCase::new("migrate-empty");

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing to migrate"), "{text}");
    assert!(text.contains("NOT created"), "{text}");
    assert!(!c.db().exists(), "an empty database was created and now shadows a file nobody wrote");
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert!(c.journal_lines().is_empty(), "nothing was written, so nothing may be recorded");

    // …and the box is still ordinary afterwards: a store written NOW is the one that answers.
    c.write_store(MIGRATE_STORE);
    let listed = c.run(&["list"], None, &[]);
    assert!(stdout(&listed).contains("BINANCE_LIVE_API_KEY"), "{}", stdout(&listed));
}

/// **An AMBIGUOUS box is refused, nothing is written, and the rung is FAILED rather than USAGE.**
///
/// The command line was fine — no change to it fixes this — so it is the ordinary run failure, which
/// is the same split `set` takes between a bad key (USAGE) and an absent store (FAILED). The refusal
/// names the key, because a refusal an operator cannot act on is just a stop.
#[test]
fn migrate_refuses_an_ambiguous_box_and_writes_nothing() {
    let c = SetCase::new("migrate-ambiguous");
    // The same name in both files with DIFFERENT values — a half-migrated box. Picking a side would
    // produce a mismatched pair, whose symptom at the node is an opaque `bad mac`.
    c.write_store("VIKE_TRADEHUB_OBSERVE_KEY=old\nBINANCE_LIVE_API_KEY=k\n");
    std::fs::write(c.node_store(), "VIKE_TRADEHUB_OBSERVE_KEY=new\n").unwrap();

    for argv in [&["migrate"][..], &["migrate", "--dry-run"][..]] {
        let out = c.run(argv, None, &[]);
        assert_eq!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Failed),
            "{argv:?}: {}",
            stderr(&out)
        );
        let err = stderr(&out);
        assert!(err.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{argv:?}: {err}");
        assert!(err.contains("nothing written"), "{argv:?}: {err}");
        assert!(!err.contains("=old"), "a VALUE reached stderr: {err}");
        assert!(!c.db().exists(), "{argv:?}: a refusal left a database behind");
        assert!(!c.db().parent().unwrap().exists(), "{argv:?}: …nor may it leave the directory");
    }
}

/// **A per-KEY refusal is LOUD on stderr and the run still succeeds** — the one judgement on this
/// verb that could reasonably have gone the other way, so it is pinned.
///
/// Every unambiguous key lands, the disagreeing one is never overwritten and is NAMED, and the rung
/// stays 0 because a non-zero one would make a box somebody has deliberately left in this state fail
/// this verb forever. The stderr copy exists because the report on stdout is a document operators
/// redirect.
#[test]
fn migrate_reports_a_refused_key_on_stderr_and_still_lands_the_rest() {
    let c = SetCase::new("migrate-refused");
    c.write_store("BINANCE_LIVE_API_KEY=first\n");
    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));

    // The operator edits the migrated key AND adds a new one in the same edit.
    c.write_store("BINANCE_LIVE_API_KEY=second\nOKX_DEMO_API_KEY=fresh\n");

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("REFUSED"), "the refusal must be loud on stderr: {err}");
    assert!(err.contains("BINANCE_LIVE_API_KEY"), "…and must name the key: {err}");
    assert!(err.contains("did not carry everything"), "{err}");
    assert!(!err.contains("second"), "a VALUE reached stderr: {err}");

    // The new key landed; the refused one kept the value already stored.
    let listed = stdout(&c.run(&["list"], None, &[]));
    assert!(listed.contains("OKX_DEMO_API_KEY"), "the unambiguous key must land: {listed}");
}

/// **ONE `credential_write` record for the whole act**, carrying key NAMES, the DATABASE as the
/// store, and no value.
///
/// The shape is a judgement — `crates/vike-cli/src/cmd/secrets.rs`'s `record_migration` argues one
/// record for the act against one per (venue, tier) — so it is pinned rather than left to drift. The
/// `multi` venue and the untiered tier are `vike_model::change_journal`'s own documented vocabulary
/// for a write that spans several.
#[test]
fn the_migration_is_journalled_once_for_the_act_with_names_and_no_value() {
    let c = SetCase::new("migrate-journal");
    c.seed_for_migration();

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let lines = c.journal_lines();
    assert_eq!(lines.len(), 1, "ONE act ⇒ ONE record, not one per venue: {lines:?}");
    let line = &lines[0];
    assert!(line.contains("credential_write"), "{line}");
    assert!(line.contains("vike.db"), "the store cell must name where the rows LANDED: {line}");
    assert!(!line.contains("secrets.env"), "…and not the file they were read from: {line}");
    assert!(line.contains("\"multi\""), "a write spanning venues is `multi`: {line}");
    assert!(line.contains("vike-cli"), "the CLI is the actor: {line}");
    for key in [
        "BINANCE_LIVE_API_KEY",
        "DUKASCOPY_DEMO1_LOGIN",
        "HYPERLIQUID_LIVE_PRIVATE_KEY",
        "VIKE_TRADEHUB_OBSERVE_KEY",
    ] {
        assert!(line.contains(key), "the record must carry the key NAME {key}: {line}");
    }
    for value in ["key-one", "login-one", "0xdeadbeef", "observe-one", "control-one"] {
        assert!(!line.contains(value), "a VALUE reached the ledger: {line}");
    }
}

/// **`--file` is refused on `migrate`**, on the usage rung, and the refusal names the flag.
///
/// Sharper than the same refusal on `set`: `set` aimed at a path appends a line to it, while
/// `migrate` aimed at one would CREATE a credential database beside an arbitrary file.
#[test]
fn migrate_refuses_a_file_flag() {
    let c = SetCase::new("migrate-file");
    c.seed_for_migration();

    let elsewhere = c.dir().join("elsewhere.env");
    std::fs::write(&elsewhere, "BINANCE_LIVE_API_KEY=k\n").unwrap();
    let out = c.run(&["migrate", "--file", &elsewhere.display().to_string()], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file"), "{}", stderr(&out));
    assert!(!c.db().exists(), "a refused command created a database");
    assert!(!c.dir().join("db").exists(), "…and none beside the named file's directory either");
}

/// **`template` is BACKEND-AWARE: byte-identical on an unmigrated box, and a finding on a migrated
/// one.**
///
/// Its whole documented use is `vike-cli secrets template > settings/secrets.env`, and every doc and
/// skill in this tree names it as HOW TO CREATE THE STORE. After a migration that redirect writes a
/// file nothing reads and exits 0 — the live gate wearing the fresh-install answer. So the grid is
/// still printed (it is a SHAPE, and reading it is not a write) and stderr says the file is no longer
/// read.
///
/// ⚠ The stdout half is asserted byte-for-byte across the two boxes, because this output is what
/// gets redirected into real stores.
#[test]
fn template_warns_only_once_the_database_answers() {
    let c = SetCase::new("migrate-template");
    c.seed_for_migration();

    let before = c.run(&["template"], None, &[]);
    assert_eq!(exit_code(&before), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&before));
    assert!(
        !stderr(&before).contains("MIGRATED"),
        "an unmigrated box must produce the output it always did: {}",
        stderr(&before)
    );

    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));

    let after = c.run(&["template"], None, &[]);
    assert_eq!(exit_code(&after), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&after));
    assert_eq!(
        stdout(&after),
        stdout(&before),
        "the GRID itself must not change — it is redirected into real stores"
    );
    let err = stderr(&after);
    assert!(err.contains("MIGRATED"), "{err}");
    assert!(err.contains("vike.db"), "it must name the store that answers: {err}");
    assert!(err.contains("secrets set"), "…and the verb that writes it: {err}");
    assert!(err.contains("secrets migrate"), "…and the verb that created it: {err}");
}

// ── `--file` on a migrated box ───────────────────────────────────────────────────────────────────
//
// `--file` is the flag an operator reaches for when they already suspect the store, and it is the
// one path on this command that `docs/decisions/0054`'s credential half left FILE-SHAPED: it goes
// to `vike_secrets::resolve`, the text arm by definition, which reports `shadowed: None` by
// construction. So on a migrated box it printed a roster of a file no process on the machine loads
// and said nothing about it — a stale answer with no qualifier, reached by the flag most likely to
// be typed by somebody who needed the qualifier most.
//
// These drive the REAL binary against a REAL migrated project, with `--file` aimed at that
// project's own retired credential file and at the database beside it.

/// A `--file` aimed at a store the database SHADOWS still lists its key NAMES, and now carries the
/// finding that the file is no longer read.
///
/// The names still appear: the operator asked to see that file, and the file is still their own
/// copy. What must not happen is the listing standing alone.
///
/// ⚠ The sentence above deliberately avoids naming a command and then saying what it writes to
/// stdout. `crates/vike-ops/tests/unrun_command_gate.rs` harvests a backticked command carrying a
/// `CLAIM_MARKERS` phrase within 80 bytes and demands a CHECKED or UNVERIFIABLE row for the pair —
/// and it is right to: a doc comment stating a command's output is a claim, and this one's proof is
/// the test body rather than a row in that table. Measured on the CI box TWICE — the first correction
/// tripped the same gate from inside its own explanation of it.
#[test]
fn list_with_a_file_that_a_database_shadows_says_the_file_is_no_longer_read() {
    let p = Project::new("file-shadowed-list");
    p.write_store("BINANCE_LIVE_API_KEY=abc\n");
    p.migrate();

    let out = Command::new(BIN)
        .args(["secrets", "list", "--file"])
        .arg(p.store())
        .stdin(Stdio::null())
        .env("VIKE_SETTINGS_DIR", p.settings())
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let o = stdout(&out);
    assert!(o.contains("BINANCE_LIVE_API_KEY"), "the named file's keys still print: {o}");
    assert!(!o.contains("abc"), "a VALUE may never appear: {o}");

    let e = stderr(&out);
    assert!(
        e.contains("NO LONGER READ"),
        "a listing of a shadowed file must carry the finding: {e}"
    );
    assert!(e.contains("vike.db"), "…and name the store that answers instead: {e}");
}

/// `path --file <a shadowed store>` names the database beside it.
///
/// The guard used to be `args.file.is_none()`, so this invocation printed the file as `store:` and
/// suppressed the `db:`/`answers:` block entirely — this verb doing the one thing it exists to
/// prevent, on the flag an operator uses when something is already wrong.
#[test]
fn path_with_a_file_that_a_database_shadows_names_the_database() {
    let p = Project::new("file-shadowed-path");
    p.write_store("BINANCE_LIVE_API_KEY=abc\n");
    p.migrate();

    let out = Command::new(BIN)
        .args(["secrets", "path", "--file"])
        .arg(p.store())
        .stdin(Stdio::null())
        .env("VIKE_SETTINGS_DIR", p.settings())
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let o = stdout(&out);
    assert!(o.contains("store:"), "{o}");
    assert!(o.contains("db:"), "the database beside the named file must be printed: {o}");
    assert!(o.contains("NO LONGER READ"), "…and said to be what answers: {o}");
}

/// **`--file` pointed at the DATABASE is refused, and the refusal is what a silent wrong answer is
/// replaced by.**
///
/// The assertion that carries the weight is the NEGATIVE one: the old behaviour did not error, it
/// printed an empty listing. A database's pages are largely NUL bytes, which are valid UTF-8, so
/// the text read succeeded and the `KEY=VALUE` parser found nothing in the binary — `0 secret(s)`,
/// about the one artifact holding every venue key.
#[test]
fn a_file_naming_the_database_is_refused_rather_than_parsed_as_an_empty_store() {
    let p = Project::new("file-is-the-db");
    p.write_store("BINANCE_LIVE_API_KEY=abc\n");
    p.migrate();

    for verb in ["list", "path"] {
        let out = Command::new(BIN)
            .args(["secrets", verb, "--file"])
            .arg(p.db())
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", p.settings())
            .output()
            .expect("the vike-cli binary must run");
        assert_ne!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Ok),
            "`{verb} --file <db>` must not succeed: {}{}",
            stdout(&out),
            stderr(&out)
        );
        let o = stdout(&out);
        assert!(
            !o.contains("secret(s)"),
            "the failure this refusal replaces is a LISTING, so none may print: {o}"
        );
        let e = stderr(&out);
        assert!(e.contains("DATABASE"), "{e}");
        assert!(e.contains("VIKE_SETTINGS_DIR"), "a refusal must name the way through: {e}");
    }
}

/// **An ordinary `--file` is untouched**, which is the property every assertion above is paid for
/// with: a path with no `db/vike.db` beside it prints exactly what it printed before any of this.
#[test]
fn an_ordinary_file_outside_a_migrated_project_prints_no_database_line() {
    // Two separate throwaway roots: the file being inspected, and an EMPTY settings directory for
    // `$VIKE_SETTINGS_DIR`, so nothing here can reach the developer box's own store.
    let c = Case::new("file-unmigrated");
    c.write_store("BINANCE_LIVE_API_KEY=abc\n");
    let elsewhere = tempfile::Builder::new().prefix("vike-cli-empty-").tempdir().expect("tempdir");

    for verb in ["list", "path"] {
        let out = Command::new(BIN)
            .args(["secrets", verb, "--file"])
            .arg(c.store())
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", elsewhere.path())
            .output()
            .expect("the vike-cli binary must run");
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
        let both = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            !both.contains("vike.db") && !both.contains("NO LONGER READ"),
            "`{verb}` over an unmigrated path must be byte-identical to before: {both}"
        );
    }
}

// ── accounts / set-book ─────────────────────────────────────────────────────────────────────────
//
// ⚠ Every case here drives its own temp settings directory through `$VIKE_SETTINGS_DIR`, like the
// `set` and `migrate` cases above and doubly so: `set-book` writes a row in a credential DATABASE,
// and a case that fell through to the walk would write one inside the developer's checkout.

/// A store with **TWO dukascopy demo accounts** in it — the shape the whole account-book verb
/// exists for, and the one no other fixture in this file carries.
///
/// After the migration these are two `account` rows of `(dukascopy, demo, label = NULL)`: `UNIQUE
/// (venue, tier, label)` does not separate them, because NULLs are distinct in SQLite, so `id` is
/// the only handle that tells them apart. `BINANCE_DEMO_API_KEY` is beside them so the listing has
/// a third row and the assertions cannot pass by counting to two.
const BOOK_STORE: &str = "# vike credential store\n\
                          BINANCE_DEMO_API_KEY=key-one\n\
                          DUKASCOPY_DEMO1_LOGIN=login-one\n\
                          DUKASCOPY_DEMO1_PASSWORD=pass-one\n\
                          DUKASCOPY_DEMO2_LOGIN=login-two\n\
                          DUKASCOPY_DEMO2_PASSWORD=pass-two\n";

impl SetCase {
    /// A migrated project holding [`BOOK_STORE`], and the two dukascopy `account` ids in it.
    ///
    /// The ids come out of the shipped binary's own `accounts` listing rather than out of the
    /// database directly — so if the listing ever stopped printing the id, these cases fail at the
    /// step that reads it rather than silently testing a handle no operator can obtain.
    fn seeded_books(&self) -> Vec<i64> {
        self.write_store(BOOK_STORE);
        let out = self.run(&["migrate"], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

        let listed = self.run(&["accounts"], None, &[]);
        assert_eq!(exit_code(&listed), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&listed));
        let text = stdout(&listed);
        // ⚠ A TABLE ROW is an INDENTED line whose FIRST token parses as an id, not merely any line
        // naming the venue. The listing also prints a block under the table for every
        // `(venue, tier, label)` more than one row shares — the dukascopy pair is exactly that —
        // and its header names the venue too. Filtering on the name alone made this helper panic
        // on the prose line.
        //
        // ⚠ And the INDENT is the third clause, added after the `last verified` footer landed: that
        // paragraph opens with a COUNT (`3 row(s) read …`) and names dukascopy as the venue that
        // produces a verification today, so it satisfied both earlier clauses at once and this
        // helper reported three dukascopy rows in a two-row fixture. Every table row carries
        // `run_accounts`' two-space indent; every footer line is flush left.
        let ids: Vec<i64> = text
            .lines()
            .filter(|l| l.starts_with(' ') && l.contains("dukascopy"))
            .filter_map(|l| l.split_whitespace().next().and_then(|t| t.parse::<i64>().ok()))
            .collect();
        assert_eq!(ids.len(), 2, "the fixture's two dukascopy rows must be listed: {text}");
        ids
    }
}

/// **`accounts` prints the table, with the `id` that `set-book` takes and a book that is not yet
/// known.**
///
/// `list` cannot answer this: it prints accounts DERIVED from key names, which for dukascopy is
/// nothing at all (the grammar deliberately does not retro-fit a venue that bakes an account INDEX
/// into its tier token), and a name carries no `id`.
#[test]
fn accounts_prints_the_rows_with_their_ids_and_no_value() {
    let c = SetCase::new("accounts");
    let ids = c.seeded_books();
    assert_eq!(ids.len(), 2, "the fixture must migrate to TWO dukascopy rows: {ids:?}");
    assert_ne!(ids[0], ids[1], "the two rows must differ by id — nothing else separates them");

    let out = c.run(&["accounts"], None, &[]);
    let text = stdout(&out);
    assert!(text.contains("dukascopy"), "{text}");
    assert!(text.contains("binance"), "the listing must not be dukascopy-only: {text}");
    assert!(text.contains("(not yet known)"), "an unwritten book must say so: {text}");
    assert!(text.contains("vike.db"), "the listing must name the store it read: {text}");
    // A VALUE cannot reach either stream — the reader selects from `account` alone.
    for v in ["login-one", "login-two", "key-one", "pass-one"] {
        assert!(!text.contains(v), "a VALUE reached the listing: {text}");
    }
}

/// **An unmigrated box says it has no account table and exits ZERO.**
///
/// Not a failure: a file store is an ordinary state, its credentials answer perfectly well under
/// their legacy key names, and `Known(vec![])` there would be an assertion about a store holding
/// every account it ever held. The message names the reader that DOES apply.
#[test]
fn accounts_on_an_unmigrated_box_names_the_store_that_answers() {
    let c = SetCase::new("accounts-unmigrated");
    c.write_store(BOOK_STORE);

    let out = c.run(&["accounts"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no account table"), "{text}");
    assert!(text.contains("secrets list"), "it must name the reader that applies: {text}");
    assert!(text.contains("secrets migrate"), "…and the way through: {text}");
}

/// **The two dukascopy rows learn DIFFERENT books through the shipped binary**, each run echoing
/// the row it is about, and the change journal records each write as an `account_book` — never as a
/// credential write.
#[test]
fn set_book_writes_one_row_echoes_it_and_journals_the_change() {
    let c = SetCase::new("set-book");
    let ids = c.seeded_books();
    let before_store = c.read_store();

    let out = c.run(
        &["set-book", "--id", &ids[0].to_string(), "--venue-account-id", "1234567"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ The ECHO: which row this is about, before the outcome.
    assert!(text.contains(&format!("account {}", ids[0])), "{text}");
    assert!(text.contains("venue=dukascopy"), "{text}");
    assert!(text.contains("tier=demo"), "{text}");
    assert!(text.contains("(not yet known) -> 1234567"), "the before and after: {text}");
    assert!(text.contains("written to"), "{text}");

    let out = c.run(
        &["set-book", "--id", &ids[1].to_string(), "--venue-account-id", "7654321"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    // Both landed, on the right rows, and the listing now tells them apart — while the binance row
    // is untouched, which is what says the write was targeted rather than a sweep.
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("1234567") && listed.contains("7654321"), "{listed}");
    assert!(listed.contains("1 with no venue account id yet"), "binance must stay blank: {listed}");

    // ⚠ The credential FILE is byte-identical: this writer never opens it in any branch.
    assert_eq!(c.read_store(), before_store, "the credential file was touched");

    // The ledger: two `account_book` records, ids and books only, and no `credential_write`.
    let lines = c.journal_lines();
    let books: Vec<&String> = lines.iter().filter(|l| l.contains("account_book")).collect();
    assert_eq!(books.len(), 2, "one record per write: {lines:?}");
    assert!(books.iter().any(|l| l.contains("1234567")), "{books:?}");
    for line in &books {
        assert!(!line.contains("credential_write"), "an account write was filed as a credential");
        for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
            assert!(!line.contains(v), "a VALUE reached the ledger: {line}");
        }
    }
}

/// **`--dry-run` prints the row and writes nothing** — the step that answers *is this the account I
/// think it is* before a broker is decided.
#[test]
fn set_book_dry_run_shows_the_row_and_changes_nothing() {
    let c = SetCase::new("set-book-dry");
    let ids = c.seeded_books();

    let out = c.run(
        &["set-book", "--id", &ids[0].to_string(), "--venue-account-id", "1234567", "--dry-run"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&format!("account {}", ids[0])), "the row must be echoed: {text}");
    assert!(text.contains("DRY RUN"), "{text}");
    // ⚠ **THE STORE, NAMED IN THE REHEARSAL.** It used to appear only on a completed write, so a
    // dry run on the wrong box — the wrong checkout, an inherited $VIKE_SETTINGS_DIR, one ssh hop
    // too far — read exactly like a dry run on the right one, which is the class of mistake the
    // rehearsal exists to catch.
    let db = c.db();
    let db = db.display().to_string();
    assert!(text.contains(&db), "the rehearsal must name the store it would write: {text}");
    assert!(text.contains("store:"), "…up front, before the row: {text}");
    // …and the row's own credential keys, which is the only cell that DIFFERS between the pair.
    assert!(text.contains("DUKASCOPY_DEMO"), "the echo must identify the row: {text}");
    for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
        assert!(!text.contains(v), "a VALUE reached the echo: {text}");
    }

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(!listed.contains("1234567"), "A DRY RUN WROTE THE BOOK: {listed}");
    // ⚠ Not `is_empty()`: the `migrate` this fixture ran to create the database journals a record
    // of its own, and asserting emptiness here would be asserting that the MIGRATION recorded
    // nothing. What a rehearsal must add is no `account_book` line.
    assert!(
        !c.journal_lines().iter().any(|l| l.contains("account_book")),
        "a rehearsal recorded an account write: {:?}",
        c.journal_lines()
    );
}

/// **A row that already names a DIFFERENT book is refused, and the refusal says what is at stake.**
///
/// The failure it stops: `--id` is an integer with no roster behind it, so a mistyped one names some
/// OTHER account — and dukascopy's two demo accounts are two legal entities, so overwriting the
/// wrong row's book re-points it at another BROKER with nothing said.
#[test]
fn set_book_refuses_to_repoint_a_row_until_replace_says_so() {
    let c = SetCase::new("set-book-repoint");
    let ids = c.seeded_books();
    let id = ids[0].to_string();

    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "1234567"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );

    let out = c.run(&["set-book", "--id", &id, "--venue-account-id", "7654321"], None, &[]);
    assert_ne!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "a re-point must be refused");
    let e = stderr(&out);
    assert!(e.contains("1234567"), "the refusal must name the stored book: {e}");
    assert!(e.contains("BROKER"), "…and what is at stake: {e}");
    assert!(e.contains("--replace"), "…and the deliberate way through: {e}");

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("1234567") && !listed.contains("7654321"), "a refusal wrote: {listed}");
    assert_eq!(
        c.journal_lines().iter().filter(|l| l.contains("account_book")).count(),
        1,
        "a REFUSED write was journalled — only the first, applied, write may be"
    );

    // …and with `--replace` it lands.
    let out =
        c.run(&["set-book", "--id", &id, "--venue-account-id", "7654321", "--replace"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("1234567 -> 7654321"), "{}", stdout(&out));
}

/// **An unmigrated box REFUSES the write and creates no database.**
///
/// The second half is the expensive one: the database opener CREATES a file when the path is empty,
/// so a writer reaching it here would leave a finished, version-stamped store holding one account
/// row — from which moment the database answers for every process on the box and every credential
/// in `secrets.env` stops being read, silently, with every venue dropping to paper.
#[test]
fn set_book_on_an_unmigrated_box_refuses_and_creates_no_database() {
    let c = SetCase::new("set-book-files");
    c.write_store(BOOK_STORE);
    let before = c.read_store();

    let out = c.run(&["set-book", "--id", "1", "--venue-account-id", "1234567"], None, &[]);
    assert_ne!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "a file store must be refused");
    let e = stderr(&out);
    assert!(e.contains("NOTHING WAS WRITTEN"), "{e}");
    assert!(e.contains("secrets migrate"), "the refusal must name the way through: {e}");

    assert!(!c.db().exists(), "A REFUSED WRITE CREATED THE DATABASE");
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(c.read_store(), before, "the credential file was touched");
    assert!(c.journal_lines().is_empty(), "a refused write was journalled");
}

/// **An `id` no row carries is refused on the USAGE rung and creates no account.**
#[test]
fn set_book_refuses_an_unknown_id_and_creates_no_account() {
    let c = SetCase::new("set-book-unknown-id");
    c.seeded_books();

    let out = c.run(&["set-book", "--id", "9999", "--venue-account-id", "1234567"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("9999"), "{e}");
    assert!(e.contains("secrets accounts"), "the refusal must name the listing: {e}");

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("3 account(s)"), "a refused write created a row: {listed}");
}

/// **The listing SEPARATES the two dukascopy rows**, which is the property `set-book` is unusable
/// without — and it does it with key NAMES, never a value.
///
/// ⚠ Before this, `accounts` rendered the pair as two lines differing only by an opaque integer:
/// same venue, same tier, both labels blank, both books `(not yet known)`. An operator asked to
/// write 1234567 to "the Swiss one" had nothing in the output to choose from, and choosing wrongly
/// points an account at the other legal entity — which is the whole failure this verb exists to
/// prevent. The discriminating fact was in the store the whole time, one table over: each row's own
/// credential key names.
#[test]
fn accounts_tells_the_two_identical_dukascopy_rows_apart_by_their_key_names() {
    let c = SetCase::new("accounts-discriminator");
    let ids = c.seeded_books();

    let text = stdout(&c.run(&["accounts"], None, &[]));

    // The premise, asserted rather than assumed: BOTH rows are `dukascopy demo` with no label and
    // no book, so nothing in the account table itself separates them.
    // ⚠ `starts_with(' ')` is what makes this a TABLE-ROW filter rather than a line filter: the
    // `last verified` footer opens with a count and names dukascopy, so it parses as an id and
    // mentions the venue exactly as a row does. Every table row is indented; every footer line is
    // flush left. Same clause, same reason, as `SetCase::account_rows`.
    let rows: Vec<&str> = text
        .lines()
        .filter(|l| {
            l.starts_with(' ')
                && l.split_whitespace().next().and_then(|t| t.parse::<i64>().ok()).is_some()
                && l.contains("dukascopy")
        })
        .collect();
    assert_eq!(rows.len(), 2, "{text}");
    for r in &rows {
        assert!(r.contains("demo") && r.contains("(not yet known)"), "{r}");
    }

    // …and the key names do, on the row itself and again in the ambiguity block beneath the table.
    assert!(text.contains("credential keys"), "the column must be headed: {text}");
    assert!(text.contains("DUKASCOPY_DEMO1_"), "{text}");
    assert!(text.contains("DUKASCOPY_DEMO2_"), "{text}");
    assert!(
        text.contains("rows share (dukascopy, demo, label=none)"),
        "the pair must be called out as inseparable by the table alone: {text}"
    );
    for id in &ids {
        assert!(text.contains(&format!("account {id}:")), "each row's keys must be listed: {text}");
    }
    assert!(text.contains("DUKASCOPY_DEMO1_LOGIN"), "the full names, not only the prefix: {text}");

    // ⚠ The scope of an id, printed where somebody is about to write one down.
    assert!(
        text.contains("stable for the life of this database file only"),
        "the listing must say what an id is NOT: {text}"
    );

    // NAMES ONLY — no value on either stream, the same guarantee the reader has by construction.
    for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
        assert!(!text.contains(v), "a VALUE reached the listing: {text}");
    }
}

/// **A pair written the WRONG WAY ROUND is repairable through the shipped binary** — and it was not
/// before `--clear` existed.
///
/// ⚠ With both rows set and crossed, every direct correction is refused in BOTH directions:
/// `--replace` gets past *this row already names a different book*, and ruling 11's
/// one-account-per-book index then finds the other row. The refusal used to end by telling the
/// operator to "deactivate or correct the other", and NEITHER act was reachable from any command in
/// this tree. This test pins the dead end first, then the way out.
#[test]
fn a_swapped_pair_is_repairable_and_the_refusal_names_the_command_that_does_it() {
    let c = SetCase::new("set-book-swap");
    let ids = c.seeded_books();
    let (a, b) = (ids[0].to_string(), ids[1].to_string());

    // The mistake.
    for (id, book) in [(&a, "7654321"), (&b, "1234567")] {
        let out = c.run(&["set-book", "--id", id, "--venue-account-id", book], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    }

    // THE DEAD END, both directions, with --replace granted.
    for (id, book) in [(&a, "1234567"), (&b, "7654321")] {
        let out =
            c.run(&["set-book", "--id", id, "--venue-account-id", book, "--replace"], None, &[]);
        assert_ne!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Ok),
            "the crossed state must refuse"
        );
        let e = stderr(&out);
        assert!(e.contains("may not name one book"), "{e}");
        // …and the refusal names a REPAIR THAT EXISTS.
        assert!(e.contains("--clear"), "the refusal must name a command that exists: {e}");
    }

    // The way out: clear, write, write.
    let out = c.run(&["set-book", "--id", &a, "--clear"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("7654321 -> (cleared"), "the echo must say what went: {text}");
    assert!(text.contains("cleared in"), "{text}");

    for (id, book) in [(&b, "7654321"), (&a, "1234567")] {
        let mut argv = vec!["set-book", "--id", id.as_str(), "--venue-account-id", book];
        if id == &b {
            argv.push("--replace");
        }
        let out = c.run(&argv, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    }

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row_of = |id: &str| -> String {
        listed
            .lines()
            .find(|l| l.split_whitespace().next() == Some(id))
            .unwrap_or_else(|| panic!("no row {id} in {listed}"))
            .to_string()
    };
    assert!(row_of(&a).contains("1234567"), "{listed}");
    assert!(row_of(&b).contains("7654321"), "{listed}");

    // The ledger reads as a repair: the clear is recorded with its OLD book and no new one.
    let books: Vec<String> =
        c.journal_lines().into_iter().filter(|l| l.contains("account_book")).collect();
    assert_eq!(books.len(), 5, "two mistakes, one clear, two corrections: {books:?}");
    assert!(
        books.iter().any(|l| l.contains("\"old\":\"7654321\"") && !l.contains("\"new\":")),
        "the CLEAR must be recorded as old-without-new: {books:?}"
    );
}

/// **`--clear` and `--venue-account-id` are mutually exclusive, and so are `--clear` and
/// `--replace`** — refused by the shipped binary on the USAGE rung, with nothing written.
#[test]
fn clear_refuses_a_value_and_refuses_replace_on_the_shipped_binary() {
    let c = SetCase::new("set-book-clear-conflicts");
    let ids = c.seeded_books();
    let id = ids[0].to_string();
    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "1234567"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );

    for argv in [
        vec!["set-book", "--id", id.as_str(), "--clear", "--venue-account-id", "7654321"],
        vec!["set-book", "--id", id.as_str(), "--clear", "--replace"],
    ] {
        let out = c.run(&argv, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{argv:?}");
        assert!(stderr(&out).contains("Nothing was written"), "{}", stderr(&out));
    }

    // …and the row is exactly as it was.
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("1234567"), "a refused command changed the row: {listed}");
}

/// **An invisible character pasted into the middle of the number is REFUSED, and the refusal never
/// echoes it.**
///
/// ⚠ `char::is_control` is category Cc alone and `is_whitespace` is `White_Space`, so neither
/// classifies a zero-width space — and `trim` does not strip one. A value pasted off a venue's web
/// page with one inside it used to be STORED: identical to another row's book on every screen, and
/// unequal to it in the index that is the only thing keeping two accounts of one venue off one
/// book. An edge one is a paste artefact and is trimmed; an interior one has no benign reading.
#[test]
fn set_book_refuses_an_interior_invisible_character_and_trims_an_edge_one() {
    let c = SetCase::new("set-book-invisible");
    let ids = c.seeded_books();
    let id = ids[0].to_string();

    let out = c.run(&["set-book", "--id", &id, "--venue-account-id", "123\u{200B}4567"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("invisible"), "the refusal must say what is most likely wrong: {e}");
    assert!(!e.contains('\u{200B}'), "the refusal echoed the token: {e:?}");
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(!listed.contains("4567"), "a refused value was written: {listed}");

    // …and the same character at the EDGE is trimmed, storing the clean book.
    let out = c.run(&["set-book", "--id", &id, "--venue-account-id", "\u{FEFF}1234567"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id.as_str()))
        .unwrap_or_else(|| panic!("no row {id} in {listed}"));
    assert!(row.contains("1234567"), "{row}");
    assert!(!row.contains('\u{FEFF}'), "the byte-order mark was STORED: {row:?}");
}

/// `template --file` is REFUSED — see the unit test beside the parser for the argument. This is the
/// half that proves the shipped binary refuses it rather than the parser in isolation.
#[test]
fn template_refuses_the_file_flag_on_the_shipped_binary() {
    let out = Command::new(BIN)
        .args(["secrets", "template", "--file", "/tmp/anything"])
        .stdin(Stdio::null())
        .env_remove("VIKE_SETTINGS_DIR")
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("--file"), "{e}");
    assert!(e.contains("MIGRATED"), "the refusal must say what the flag was silencing: {e}");
}

// ── confirm — the HANDSHAKE fold, end to end through the shipped binary ─────────────────────────
//
// ⚠ These cases plant the parked file a LIVE MOUNT would write, because the mount needs a JForex
// sidecar, an SDK and a network and none of the three is available to a test. What is proven here
// is everything downstream of that file: the addressing, the three verdicts, the two columns, the
// ledger and what is left parked. The mount's own half — that it writes a record of this shape, at
// this address — is `crates/vike-mount/src/dukascopy.rs`'s `confirmation_for` tests, and the file
// format is `vike_model::account_confirmation`'s own suite. Nothing here asserts what the JForex
// handshake actually RETURNS: that is unmeasured (the credential-schema spec §9) and a test that
// invented it would be pinning the invention.

impl SetCase {
    /// `<settings>/state/account-confirmations.json` — the path the daemon's sandbox can write and
    /// this verb reads.
    fn parked_path(&self) -> PathBuf {
        self.settings().join("state").join("account-confirmations.json")
    }

    /// Plant what a mount would have parked: one confirmation per `(venue, key prefix)`.
    fn park(&self, records: &[(&str, &str, &str)]) {
        std::fs::create_dir_all(self.settings().join("state")).unwrap();
        let body: Vec<String> = records
            .iter()
            .map(|(venue, prefix, answered)| {
                format!(
                    "{{\"venue\":\"{venue}\",\"key_prefix\":\"{prefix}\",\
                     \"handshake_account_id\":\"{answered}\",\"observed_row\":null,\
                     \"observed_book\":null,\"at_ms\":1787356800000}}"
                )
            })
            .collect();
        std::fs::write(self.parked_path(), format!("{{\"records\":[{}]}}\n", body.join(",")))
            .unwrap();
    }

    fn parked_text(&self) -> String {
        std::fs::read_to_string(self.parked_path()).unwrap_or_default()
    }

    /// Only the TABLE ROWS of an `accounts` listing — INDENTED lines whose first token parses as
    /// an id.
    ///
    /// ⚠ Load-bearing, not tidiness: that listing also prints the PARKED CONFIRMATIONS at the foot,
    /// and a confirmation names the very string a disagreement case is asserting was NOT written.
    /// A `!listing.contains(answer)` assertion therefore passes for the wrong reason when the fold
    /// worked and fails for the wrong reason when it correctly refused — which is exactly how it
    /// failed when these cases were first written.
    ///
    /// ⚠ **The INDENT is half the filter**, and it stopped being optional the moment a case wanted
    /// to assert something POSITIVE about every row. The footer lines start with a count
    /// (`4 account(s), …`, `3 row(s) read …`) whose first token parses as an id just as happily as
    /// a row's does, and they are flush left while `run_accounts` prints every table row with a
    /// two-space indent. A `for row in account_rows()` loop over the untightened filter therefore
    /// asserted about prose. Tightening only ever REMOVES lines, so the negative assertions that
    /// were already here are unaffected.
    fn account_rows(&self) -> String {
        stdout(&self.run(&["accounts"], None, &[]))
            .lines()
            .filter(|l| l.starts_with(' '))
            .filter(|l| l.split_whitespace().next().is_some_and(|t| t.parse::<i64>().is_ok()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `account_book` records only — a `migrate` files a `credential_write` of its own, so a
    /// bare line count is not a count of book writes.
    fn book_records(&self) -> Vec<String> {
        self.journal_lines().into_iter().filter(|l| l.contains("account_book")).collect()
    }
}

/// **A box with nothing parked says so and exits ZERO.** The ordinary state of every box that has
/// not mounted a confirming venue — not a failure, and not an empty table either.
#[test]
fn confirm_with_nothing_parked_is_an_answer_rather_than_an_error() {
    let c = SetCase::new("confirm-empty");
    let _ = c.seeded_books();

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing parked"), "{text}");
    assert!(text.contains("vike.db"), "it must still name the store it would have written: {text}");
}

/// **LEARNS: a row with no book takes the venue's own answer and is stamped verified**, addressed
/// by its credential key PREFIX and not by a row id — and the ledger files it as an `account_book`
/// written by the VENUE.
#[test]
fn confirm_learns_a_book_from_the_venue_and_stamps_the_session() {
    let c = SetCase::new("confirm-learns");
    let ids = c.seeded_books();
    let before_store = c.read_store();
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("LEARNS"), "{text}");
    assert!(text.contains("DEMO1abcd"), "the venue's answer is echoed: {text}");
    assert!(text.contains("1 row(s) folded, 0 disagreement(s)"), "{text}");

    // The BOOK landed on exactly one row, and the OTHER dukascopy row is untouched — which is what
    // says the key prefix addressed it rather than a sweep.
    let rows = c.account_rows();
    assert_eq!(rows.matches("DEMO1abcd").count(), 1, "exactly ONE row may learn this book: {rows}");
    assert!(
        stdout(&c.run(&["accounts"], None, &[])).contains("2 with no venue account id yet"),
        "{rows}"
    );
    assert_ne!(ids[0], ids[1]);

    // The record is CONSUMED, so a second run has nothing to do.
    assert!(
        !c.parked_text().contains("DEMO1abcd"),
        "a folded record stayed parked: {}",
        c.parked_text()
    );
    let again = stdout(&c.run(&["confirm"], None, &[]));
    assert!(again.contains("nothing parked"), "{again}");

    // The credential FILE is byte-identical: nothing on this path opens it.
    assert_eq!(c.read_store(), before_store, "the credential file was touched");

    // The ledger: ONE `account_book`, by the VENUE rather than the CLI, and no value anywhere.
    let books = c.book_records();
    assert_eq!(books.len(), 1, "one record for one fold: {books:?}");
    // ⚠ The ACTOR, matched on the serialized tag rather than on the word `dukascopy` — that word
    // appears in `target.venue` on every one of these records, so a bare `contains("dukascopy")`
    // could not fail for its stated reason and would pass for an `Actor::cli` record too.
    assert!(
        books[0].contains(r#""actor":{"origin":"venue","venue":"dukascopy"}"#),
        "the fold's actor must be the VENUE — the value came from it and this process only \
         carried it: {}",
        books[0]
    );
    assert!(!books[0].contains(r#""origin":"cli""#), "{}", books[0]);
    for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
        assert!(!books[0].contains(v), "a VALUE reached the ledger: {}", books[0]);
    }
}

/// ⚠ **CONFIRMS: the book already matches, and the timestamp still moves.**
///
/// The case the column exists for, and the one the tree had no way to record at all: without it
/// *never verified* and *verified three weeks ago* go on looking identical to *fine*. The book must
/// not move and the ledger must stay silent — a verification changes no routing.
#[test]
fn confirm_stamps_a_row_whose_book_already_matches_and_journals_nothing() {
    let c = SetCase::new("confirm-confirms");
    let ids = c.seeded_books();
    let id = ids[0].to_string();
    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "DEMO1abcd"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );
    let books_before = c.book_records().len();

    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("CONFIRMS"), "{text}");
    assert!(text.contains("1 row(s) folded, 0 disagreement(s)"), "{text}");
    assert!(text.contains("verified=2026-08-22T00:00:00Z"), "the HANDSHAKE's instant: {text}");

    // The row now carries the verification, and a SECOND `confirm` run sees the previous one —
    // which is the whole point of the column.
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let second = stdout(&c.run(&["confirm"], None, &[]));
    assert!(second.contains("last verified=2026-08-22T00:00:00Z"), "{second}");

    // ⚠ NOTHING was journalled by either run: `changed` is a claim about the BOOK, the book did not
    // move, and a ledger line saying it did would be the misreading the rule forbids.
    let books = c.book_records();
    assert_eq!(
        books.len(),
        books_before,
        "a confirmation that moved no book must journal nothing: {books:?}"
    );
}

/// ⚠ **DISAGREES: the store says one account and the venue says another — and NOTHING is written.**
///
/// Not the book (a fold has no operator in front of it to permit re-pointing an armed account at
/// another broker) and not the timestamp (a row the venue has just contradicted must not read as
/// verified). The record is KEPT so the finding does not vanish with the run that found it, the
/// exit code stays ZERO because the session that produced the confirmation authenticated, and the
/// report names the one-command repair and says the disagreement is not yet proof of a wrong
/// broker.
#[test]
fn confirm_refuses_to_overwrite_a_disagreeing_book_and_keeps_the_record() {
    let c = SetCase::new("confirm-disagrees");
    let ids = c.seeded_books();
    let id = ids[0].to_string();
    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "3709890"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );
    let books_before = c.book_records().len();

    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let out = c.run(&["confirm"], None, &[]);
    // ⚠ ZERO. A disagreement is a REPORT: the rows that folded folded, and an exit code here would
    // make a scripted `confirm` fail on a box that is one hand-written number out of date.
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("DISAGREEMENT"), "{text}");
    assert!(text.contains("3709890") && text.contains("DEMO1abcd"), "both strings: {text}");
    assert!(text.contains("NOTHING IS WRITTEN"), "{text}");
    assert!(text.contains("--replace"), "the one-command repair must be printed: {text}");
    assert!(text.contains("not yet proof of a wrong broker"), "the FORM residual: {text}");
    assert!(text.contains("0 row(s) folded, 1 disagreement(s)"), "{text}");

    // NEITHER column moved. ⚠ The TABLE ROWS, not the whole listing: the listing's own parked
    // notice names `DEMO1abcd` (that is what the notice is FOR), so a whole-output assertion would
    // fail here for the wrong reason.
    let rows = c.account_rows();
    assert!(rows.contains("3709890"), "the stored book was overwritten: {rows}");
    assert!(!rows.contains("DEMO1abcd"), "the handshake's answer was written: {rows}");
    assert_eq!(c.book_records().len(), books_before, "a refused fold must journal nothing");

    // …and the record is KEPT, so `accounts` goes on surfacing it and a later run can act on it.
    assert!(c.parked_text().contains("DEMO1abcd"), "the record was consumed: {}", c.parked_text());
    assert!(
        stdout(&c.run(&["accounts"], None, &[])).contains("parked by a live mount"),
        "the listing must surface a waiting fold"
    );
}

/// **`--dry-run` prints every verdict and writes nothing** — neither a row nor the parked file.
///
/// It matters more on this verb than on `set-book`: the rows about to be written are named by a
/// file a daemon wrote, not by the operator, so this is the only way to see which accounts are
/// about to move before they do.
#[test]
fn confirm_dry_run_writes_neither_a_row_nor_the_parked_file() {
    let c = SetCase::new("confirm-dry");
    let _ = c.seeded_books();
    c.park(&[("dukascopy", "DUKASCOPY_DEMO2_", "DEMO2cGyrc")]);
    let parked_before = c.parked_text();

    let out = c.run(&["confirm", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("LEARNS"), "{text}");
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("DRY RUN, nothing was written"), "{text}");

    // ⚠ The TABLE ROWS, not the whole listing — the parked notice names the record on purpose.
    let rows = c.account_rows();
    assert!(!rows.contains("DEMO2cGyrc"), "a dry run wrote a row: {rows}");
    assert_eq!(c.parked_text(), parked_before, "a dry run consumed a record");
    assert!(c.book_records().is_empty(), "a dry run journalled a book write");
}

/// **A record no ACTIVE row owns is KEPT and reported, never guessed at.** This is what a
/// re-migration looks like from the fold's side: the confirmations are still good evidence and the
/// numbering under them moved, so the address either resolves or it does not.
#[test]
fn confirm_keeps_a_record_whose_address_names_no_row() {
    let c = SetCase::new("confirm-orphan");
    let _ = c.seeded_books();
    c.park(&[("dukascopy", "DUKASCOPY_DEMO9_", "DEMO9zzzz")]);

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no ACTIVE dukascopy row owns the credential keys"), "{text}");
    assert!(text.contains("0 row(s) folded"), "{text}");
    assert!(c.parked_text().contains("DEMO9zzzz"), "the record must be KEPT: {}", c.parked_text());
}

/// **An UNMIGRATED box refuses, writes nothing, creates no database and KEEPS every record.**
///
/// A file store has no `account` table to stamp, and a per-KEY fallback is what `Backend` forbids.
/// The confirmations are perfectly good evidence — it is FOLDING that waits for the migration, not
/// recording.
#[test]
fn confirm_on_an_unmigrated_box_refuses_and_keeps_every_record() {
    let c = SetCase::new("confirm-unmigrated");
    c.write_store(BOOK_STORE);
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let parked_before = c.parked_text();

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("NOTHING WAS WRITTEN"), "{e}");
    assert!(e.contains("are KEPT"), "{e}");
    assert!(e.contains("secrets migrate"), "the way through must be named: {e}");
    assert_eq!(c.parked_text(), parked_before, "a refused run consumed a record");
    assert!(
        !c.settings().join("db").join("vike.db").exists(),
        "this verb must never create a database"
    );
}

// ── the two READERS the writers were missing ────────────────────────────────────────────────────
//
// A column with a writer and no reader, and a record parked where nobody is told about it, are the
// same defect wearing two hats: something was learned and the operator cannot see it. Each case
// below drives the shipped binary end to end and asserts what an operator actually reads.

/// **An UNMIGRATED box is TOLD about parked confirmations** — the population the notice was written
/// for, and the one it did not reach.
///
/// `run_accounts` returns early on `Accounts::Unanswerable`, which is a `Backend::Files` box, and
/// the parked notice used to sit BELOW that return. A mount parks a record on such a box exactly as
/// it does on a migrated one (parking is not what waits for the migration; folding is), so the
/// operator with no other way to learn a fold is waiting was the one never told.
///
/// ⚠ It must also say that `confirm` will REFUSE here rather than work, or the notice sends
/// somebody to a verb that exits non-zero and reads as a broken tool.
#[test]
fn accounts_on_an_unmigrated_box_still_reports_what_a_mount_parked() {
    let c = SetCase::new("accounts-unmigrated-parked");
    c.write_store(BOOK_STORE);
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);

    let out = c.run(&["accounts"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no account table"), "the store answer is unchanged: {text}");

    // THE FINDING: the record is named at all.
    assert!(text.contains("parked by a live mount"), "the parked notice never rendered: {text}");
    assert!(text.contains("DUKASCOPY_DEMO1_"), "the notice must name the address: {text}");
    assert!(text.contains("DEMO1abcd"), "...and what the venue answered: {text}");

    // ...and the disposition, which differs from a migrated box's and is the reason the notice
    // takes a parameter rather than printing one sentence for both.
    assert!(text.contains("NOTHING ON THIS BOX CAN FOLD THESE YET"), "{text}");
    assert!(text.contains("KEEPS every record"), "the records must be said to be safe: {text}");
    assert!(text.contains("secrets migrate"), "the way through must be named: {text}");

    // A read may not write, and this one still creates nothing.
    assert!(!c.db().exists(), "a listing created a database: {text}");
}

/// **Nothing parked prints nothing**, on the unmigrated path as on the migrated one — the notice is
/// a NOTICE, and a box that has mounted no confirming venue is the ordinary case rather than a
/// state worth a paragraph.
#[test]
fn accounts_says_nothing_about_confirmations_when_none_are_parked() {
    let c = SetCase::new("accounts-unparked");
    c.write_store(BOOK_STORE);

    let text = stdout(&c.run(&["accounts"], None, &[]));
    assert!(text.contains("no account table"), "{text}");
    assert!(!text.contains("parked by a live mount"), "an empty box printed the notice: {text}");
}

/// **`accounts` prints `last verified`, and NEVER VERIFIED is visibly a state rather than a blank.**
///
/// The column gained its first writer and no reader in the listing an operator actually reads, so
/// the incident it exists to remove survived it: with nothing rendered, a row nothing had ever
/// authenticated as and a row that authenticated three weeks ago both read as *fine*. This asserts
/// the distinction is VISIBLE — a freshly migrated row says so in words, and a folded row carries
/// the venue handshake's own instant.
#[test]
fn accounts_prints_never_verified_until_a_venue_confirms_the_row() {
    let c = SetCase::new("accounts-verified");
    let _ = c.seeded_books();

    let before = stdout(&c.run(&["accounts"], None, &[]));
    assert!(before.contains("last verified"), "the column must be in the header: {before}");
    assert!(before.contains("NEVER VERIFIED"), "an unverified row must SAY so: {before}");
    assert!(
        before.contains("is not a fault") && before.contains("not the same as *fine*"),
        "the footer must say what the state means, or the column is an always-on alarm: {before}"
    );
    // ⚠ Every TABLE ROW reads the same way on a fresh box. Asserted over the rows rather than the
    // whole listing so the footer paragraph cannot satisfy it.
    for row in c.account_rows().lines() {
        assert!(row.contains("NEVER VERIFIED"), "a fresh row rendered as something else: {row}");
    }

    // Now let a venue confirm ONE of them.
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let folded = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&folded), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&folded));
    let fold_text = stdout(&folded);
    assert!(fold_text.contains("1 row(s) folded"), "{fold_text}");

    // ⚠ The instant is taken from the FOLD's own report rather than written down here: the two
    // readers must agree about the same value, and a literal date would pass while they disagreed.
    // ⚠ The `written:` line specifically. The per-record ECHO a few lines above it carries
    // `last verified=(never)` — the row's state BEFORE the fold — so a `find_map` over every line
    // holding `verified=` picks that one up and reads the value as `(never)`.
    let stamped = fold_text
        .lines()
        .find(|l| l.trim_start().starts_with("written:"))
        .and_then(|l| l.split("verified=").nth(1))
        .map(|t| t.trim().to_string())
        .expect("the fold must report what it stamped");
    assert!(stamped.ends_with('Z'), "the stamp must be an RFC 3339 instant: {stamped}");

    let after = c.account_rows();
    let confirmed: Vec<&str> = after.lines().filter(|l| l.contains("DEMO1abcd")).collect();
    assert_eq!(confirmed.len(), 1, "exactly one row must have learned the book: {after}");
    assert!(
        confirmed[0].contains(&stamped),
        "the confirmed row must carry the HANDSHAKE's instant ({stamped}), not a dash: {after}"
    );
    assert!(
        !confirmed[0].contains("NEVER VERIFIED"),
        "a confirmed row must stop reading as never verified: {after}"
    );
    // ...and the rows nothing authenticated as are UNCHANGED, which is what makes the column
    // informative rather than decorative.
    assert!(
        after.lines().any(|l| l.contains("NEVER VERIFIED")),
        "the rows no venue confirmed must still say so: {after}"
    );
}

/// **The FIRST fold on a hand-written box reads as a SHAPE question, and prints the commands.**
///
/// Every stored book in this tree was typed in by an operator off the venue's own page — numeric on
/// dukascopy — while the sidecar sends `IAccount.getAccountId()`, whose one pinned frame is
/// login-shaped. `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §9 leaves that
/// unsettled, so the first fold on such a box disagrees on EVERY row at once. That is cell for cell
/// what a wrong-broker credential set looks like, so the report has to say which of the two it
/// cannot tell — and then be ACTIONABLE, because a summary ending in a count sends an operator
/// scrolling back to reassemble an `--id`.
///
/// ⚠ It must NOT decide the two spellings are the same account: nobody has measured that, and
/// guessing permissively is the failure the alarm exists to catch.
#[test]
fn the_first_fold_of_a_hand_written_box_reads_as_a_shape_question_with_its_commands() {
    let c = SetCase::new("confirm-first-fold");
    let ids = c.seeded_books();

    // The hand-written state: both dukascopy rows carry the NUMBERS off the venue's page.
    for (id, book) in ids.iter().zip(["3709890", "3716974"]) {
        let out =
            c.run(&["set-book", "--id", &id.to_string(), "--venue-account-id", book], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    }
    // ...and the venue answers login-shaped, for both.
    c.park(&[
        ("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd"),
        ("dukascopy", "DUKASCOPY_DEMO2_", "DEMO2cGyrc"),
    ]);

    let out = c.run(&["confirm"], None, &[]);
    // ⚠ ZERO, not a failure: every session that produced these authenticated. A non-zero here
    // trains an operator to append `|| true`.
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("0 row(s) folded, 2 disagreement(s)"), "{text}");

    // THE FIRST-RUN FRAMING.
    assert!(text.contains("EVERY confirmation offered (2) disagreed"), "{text}");
    assert!(text.contains("SHAPE question"), "it must be framed as a shape question: {text}");
    assert!(text.contains("§9"), "...anchored on the record that leaves it open: {text}");
    assert!(
        text.contains("NOTHING HERE ASSUMES THE TWO ARE THE SAME ACCOUNT"),
        "the report must refuse to guess, out loud: {text}"
    );

    // THE COMMANDS — one complete, paste-ready line per finding, `--replace` already on it.
    //
    // ⚠ Asserted WITHOUT assuming which row id owns which key family: the migration numbers rows in
    // the order it meets credential key names, which is an implementation fact this case has no
    // business pinning. What it does pin is that each command is whole and names a row that is
    // really in this store.
    for answered in ["DEMO1abcd", "DEMO2cGyrc"] {
        let needle = format!("--venue-account-id {answered} --replace");
        let line = text
            .lines()
            .find(|l| l.contains(&needle))
            .unwrap_or_else(|| panic!("no resolving command for {answered}: {text}"));
        assert!(line.contains("vike-cli secrets set-book --id "), "incomplete command: {line}");
        let id: i64 = line
            .split("--id ")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .and_then(|t| t.parse().ok())
            .unwrap_or_else(|| panic!("the command must carry a real row id: {line}"));
        assert!(ids.contains(&id), "the command named a row not in this store: {line}");
    }
    assert!(text.contains("only AFTER you have looked"), "{text}");

    // NOTHING MOVED: not a book, not a timestamp, not a record.
    let rows = c.account_rows();
    for numeric in ["3709890", "3716974"] {
        assert!(rows.contains(numeric), "a stored book was overwritten: {rows}");
    }
    for row in rows.lines().filter(|l| l.contains("dukascopy")) {
        assert!(row.contains("NEVER VERIFIED"), "a contradicted row was stamped verified: {row}");
    }
    // ⚠ CONTENT, not bytes. A non-dry run always re-writes the parked file through
    // `account_confirmation::replace_all` — an empty keep set still writes it, which is how a later
    // reader tells *nothing parked* from *never looked* — so the pretty-printed result differs from
    // the fixture's compact JSON while holding exactly the same records. What must hold is that
    // BOTH survive: a disagreement is kept so a later run can act on it.
    let parked = c.parked_text();
    for answered in ["DEMO1abcd", "DEMO2cGyrc"] {
        assert!(parked.contains(answered), "a disagreeing record was consumed: {parked}");
    }
    assert_eq!(parked.matches("key_prefix").count(), 2, "the set must still be two: {parked}");
}
