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
struct SetCase {
    dir: PathBuf,
}

impl SetCase {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vike-cli-set-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("settings")).unwrap();
        SetCase { dir }
    }

    fn settings(&self) -> PathBuf {
        self.dir.join("settings")
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

impl Drop for SetCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
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
/// The REFUSAL is unchanged and deliberately so — `set` writes the enumerable grid and nothing
/// wider, which `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`
/// fences and this test must not be read as reopening. What is asserted here is that the refusal
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
    let bystander = c.dir.join("bashrc");
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

/// **A LABELLED ACCOUNT is refused without offering the DEFAULT account's key.**
///
/// `KEY__LABEL` names a second account — a name `vike_model::account_keys` parses,
/// `vike_bridge_core::credentials::load_credentials_for_account` reads, and `secrets list` prints.
/// `set` still cannot write one (the grid is a fixed enumeration; a label is unbounded), but the
/// suggestion list scored the UNLABELLED base as the nearest name and offered it first — and that
/// key is real, settable and a DIFFERENT ACCOUNT. The operator's obvious next command overwrote the
/// credential their primary account signs with, exit 0, "replaced".
#[test]
fn a_labelled_account_is_refused_without_a_dangerous_substitute() {
    let c = SetCase::new("labelled-account");
    c.write_store(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY__ALT"], Some("alt-account-key\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("LABELLED ACCOUNT"), "{err}");
    assert!(err.contains("Do NOT set"), "it must warn AGAINST the base key: {err}");
    assert!(!err.contains("did you mean"), "it must offer no substitute to copy: {err}");
    assert_eq!(c.read_store(), SET_STORE, "…and write nothing");
}
