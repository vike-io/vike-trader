//! End-to-end tests for `vike-cli secrets`, driving the SHIPPED binary (`CARGO_BIN_EXE_vike-cli`).
//!
//! The unit tests beside the module cover the argument grammar; these cover the thing that actually
//! matters to an operator — that `list` reads the store and prints key NAMES and never a value, and
//! that `path` says where the store is whether or not it exists.
//!
//! ⚠ **Every invocation points the CLI at the case's temp dir** (`VIKE_SETTINGS_DIR`, on every run —
//! it was `--file` until that flag went with the credential FILE store on 2026-10-07). Without it a
//! run on a developer box would read that box's real credentials into a test's stdout assertions.
//! The redirect is isolation AND a safety property.
//!
//! It is set on the child through `Command::env` — never `std::env::set_var`, which is unsafe under threads and would leak
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
/// returned **Ok** because the directory already existed, and the first store write then panicked
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

    /// The store: the settings database in this case's settings directory.
    fn db(&self) -> PathBuf {
        self.dir().join("db").join("vike.db")
    }

    /// Make the store hold `text`'s `KEY=value` lines (the case's own directory IS the settings
    /// directory `VIKE_SETTINGS_DIR` names) — see `support::seed_store`.
    fn write_store(&self, text: &str) {
        support::seed_store(self.dir(), text);
    }

    /// Run `vike-cli secrets <sub>` against this case's settings directory.
    fn run(&self, sub: &str) -> Output {
        self.run_raw(&[sub])
    }

    /// Run `vike-cli secrets …` with the arguments given verbatim.
    fn run_raw(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("secrets")
            .args(args)
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", self.dir())
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

#[path = "secrets_cli/accounts_book.rs"]
mod accounts_book;
#[path = "secrets_cli/confirm.rs"]
mod confirm;
#[path = "secrets_cli/copy_node_keys.rs"]
mod copy_node_keys;
#[path = "secrets_cli/database.rs"]
mod database;
#[path = "secrets_cli/ibkr_login.rs"]
mod ibkr_login;
#[path = "secrets_cli/init.rs"]
mod init;
#[path = "secrets_cli/list.rs"]
mod list;
#[path = "secrets_cli/list_json.rs"]
mod list_json;
#[path = "secrets_cli/set.rs"]
mod set;
#[path = "secrets_cli/support.rs"]
mod support;
#[path = "secrets_cli/tier_template.rs"]
mod tier_template;
