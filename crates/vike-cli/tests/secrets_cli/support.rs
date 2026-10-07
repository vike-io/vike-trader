//! Fixtures more than one section uses: `SetCase` (all five `impl` blocks), `Project`, the rungs.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use super::{BIN, stderr, stdout};

/// A `set` case: a `<dir>/settings/` holding the store, with `<dir>/settings/state/` for the ledger
/// — the layout `vike_boot::boot` resolves and every daemon on a box uses.
///
/// ⚠ **A BOUND `tempfile::TempDir`** — see [`Case`] for why a fixed-tag-plus-pid directory in the
/// shared system temp root is not a repair.
pub(super) struct SetCase {
    /// BOUND, so its `Drop` removes the tree even on the panic path.
    root: tempfile::TempDir,
}

impl SetCase {
    pub(super) fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-set-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).unwrap();
        SetCase { root }
    }

    /// The project root — the directory `settings/` sits in.
    pub(super) fn dir(&self) -> &std::path::Path {
        self.root.path()
    }

    pub(super) fn settings(&self) -> PathBuf {
        self.dir().join("settings")
    }

    pub(super) fn store(&self) -> PathBuf {
        self.settings().join("secrets.env")
    }

    pub(super) fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).unwrap();
    }

    pub(super) fn read_store(&self) -> String {
        std::fs::read_to_string(self.store()).unwrap()
    }

    /// The change journal's directory — `<settings>/state/changes`, off the SAME walk the store is.
    fn journal_dir(&self) -> PathBuf {
        self.settings().join("state").join("changes")
    }

    /// Every `changes-*.jsonl` line the ledger holds, or an empty vec when nothing was written.
    pub(super) fn journal_lines(&self) -> Vec<String> {
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
    pub fn run(&self, args: &[&str], stdin_text: Option<&str>, envs: &[(&str, &str)]) -> Output {
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
pub(super) fn rung(exit: vike_cli::exit::Exit) -> i32 {
    exit as i32
}

pub(super) fn exit_code(o: &Output) -> i32 {
    o.status.code().expect("the process exited rather than being signalled")
}

/// A project on disk: `<root>/settings/secrets.env`, optionally migrated into
/// `<root>/settings/db/vike.db`.
///
/// ⚠ It drives `secrets` with `$VIKE_SETTINGS_DIR` rather than `--file`, and that is the point:
/// `--file` names a text file outright and must keep doing so, while the project's own store is
/// whatever `vike_secrets::backend_in` says it is. Every assertion below is about the second.
pub(super) struct Project {
    root: tempfile::TempDir,
}

impl Project {
    pub(super) fn new(tag: &str) -> Project {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-db-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).expect("settings dir");
        Project { root }
    }

    pub(super) fn settings(&self) -> PathBuf {
        self.root.path().join("settings")
    }

    pub(super) fn store(&self) -> PathBuf {
        self.settings().join("secrets.env")
    }

    pub(super) fn db(&self) -> PathBuf {
        self.settings().join("db").join("vike.db")
    }

    pub(super) fn write_store(&self, text: &str) {
        std::fs::write(self.store(), text).expect("write store");
    }

    /// Fill the database from the file, through the library the migration lives in. ⚠ The file is
    /// left exactly where it is — retiring it is an OPERATOR act and nothing in this workspace
    /// performs one — which is precisely the state these tests are about.
    pub(super) fn migrate(&self) {
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
    pub(super) fn secrets(&self, args: &[&str]) -> Output {
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
    pub(super) fn secrets_with_stdin(&self, args: &[&str], value: &str) -> Output {
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
    pub(super) fn journal_lines(&self) -> Vec<String> {
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
    pub(super) fn config_check(&self) -> Output {
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
    pub(super) fn config_show(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(["config", "show"]).args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", self.settings());
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }
}

/// The store a migration case starts from: a hand-written file with comments, a blank line and a
/// venue whose key names are OUTSIDE the enumerable grid, because those are 57 of the 67 names on
/// the live box and a migration is only interesting on them.
pub(super) const MIGRATE_STORE: &str = "# vike credential store\n\
                             # hand-written, and it stays hand-written\n\
                             \n\
                             BINANCE_LIVE_API_KEY=key-one\n\
                             DUKASCOPY_DEMO1_LOGIN=login-one\n\
                             HYPERLIQUID_LIVE_PRIVATE_KEY=0xdeadbeef\n";

/// The node file beside it — `docs/decisions/0051`'s own store, which the migration drains into the
/// second table.
pub(super) const MIGRATE_NODE_STORE: &str = "VIKE_TRADEHUB_OBSERVE_KEY=observe-one\n\
                                  VIKE_TRADEHUB_CONTROL_KEY=control-one\n";

impl SetCase {
    pub(super) fn node_store(&self) -> PathBuf {
        self.settings().join("node.env")
    }

    pub(super) fn db(&self) -> PathBuf {
        self.settings().join("db").join("vike.db")
    }

    /// The store and the node file, as a migration case wants them.
    pub(super) fn seed_for_migration(&self) {
        self.write_store(MIGRATE_STORE);
        std::fs::write(self.node_store(), MIGRATE_NODE_STORE).unwrap();
    }
}

/// A store with **TWO dukascopy demo accounts** in it — the shape the whole account-book verb
/// exists for, and the one no other fixture in this file carries.
///
/// After the migration these are two `account` rows of `(dukascopy, demo, label = NULL)`: `UNIQUE
/// (venue, tier, label)` does not separate them, because NULLs are distinct in SQLite, so `id` is
/// the only handle that tells them apart. `BINANCE_DEMO_API_KEY` is beside them so the listing has
/// a third row and the assertions cannot pass by counting to two.
pub(super) const BOOK_STORE: &str = "# vike credential store\n\
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
    pub(super) fn seeded_books(&self) -> Vec<i64> {
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

impl SetCase {
    /// A migrated project holding [`BOOK_STORE`], and the `account` id of the row that owns
    /// `BINANCE_DEMO_API_KEY`.
    ///
    /// A KEYED row: the ⚠ block the rehearsal owes an operator only prints for one, and the
    /// dukascopy pair is the wrong fixture for it because nothing else in the listing would then be
    /// distinguishable from it by venue.
    ///
    /// The id comes out of the shipped binary's own `accounts` listing for [`Self::seeded_books`]'
    /// reason — a handle no operator can obtain is not the handle under test — and through the same
    /// indent-plus-leading-integer filter, which that helper's own comment argues.
    pub(super) fn seeded_keyed_row(&self) -> i64 {
        self.write_store(BOOK_STORE);
        let out = self.run(&["migrate"], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

        let listed = self.run(&["accounts"], None, &[]);
        assert_eq!(exit_code(&listed), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&listed));
        let text = stdout(&listed);
        let ids: Vec<i64> = text
            .lines()
            .filter(|l| l.starts_with(' ') && l.contains("binance"))
            .filter_map(|l| l.split_whitespace().next().and_then(|t| t.parse::<i64>().ok()))
            .collect();
        assert_eq!(ids.len(), 1, "the fixture's ONE binance row must be listed: {text}");
        ids[0]
    }

    /// Add a KEYLESS account through the shipped binary and answer with the id the insert assigned.
    ///
    /// `set-tier`'s apply path needs a row nothing pins: every row the migration creates owns the
    /// credential keys it was derived FROM, and `AccountKeysPinTheTier` refuses to move one of
    /// those — which is a store property with eleven tests of its own and not what these cases are
    /// about. `account add` is the only way to obtain such a row through the surface under test.
    pub(super) fn added_account(&self, venue: &str, tier: &str, label: &str) -> i64 {
        let out = self.run(
            &["account", "add", "--venue", venue, "--tier", tier, "--label", label],
            None,
            &[],
        );
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
        let text = stdout(&out);
        let line = text
            .lines()
            .find(|l| l.starts_with("added account "))
            .unwrap_or_else(|| panic!("`account add` must report the id it assigned: {text}"));
        line.split_whitespace()
            .nth(2)
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("the id is the third token of {line:?}"))
    }
}

impl SetCase {
    /// `<settings>/state/account-confirmations.json` — the path the daemon's sandbox can write and
    /// this verb reads.
    fn parked_path(&self) -> PathBuf {
        self.settings().join("state").join("account-confirmations.json")
    }

    /// Plant what a mount would have parked: one confirmation per `(venue, key prefix)`.
    pub(super) fn park(&self, records: &[(&str, &str, &str)]) {
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

    pub(super) fn parked_text(&self) -> String {
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
    pub(super) fn account_rows(&self) -> String {
        stdout(&self.run(&["accounts"], None, &[]))
            .lines()
            .filter(|l| l.starts_with(' '))
            .filter(|l| l.split_whitespace().next().is_some_and(|t| t.parse::<i64>().is_ok()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `account_book` records only — a `migrate` files a `credential_write` of its own, so a
    /// bare line count is not a count of book writes.
    pub(super) fn book_records(&self) -> Vec<String> {
        self.journal_lines().into_iter().filter(|l| l.contains("account_book")).collect()
    }
}
