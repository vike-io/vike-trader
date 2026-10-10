//! End-to-end tests for `vike-cli config set`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a real settings directory in a temp dir.
//!
//! The unit tests beside the module cover the argv grammar, the two gates and the report wording.
//! These cover what an operator actually does — point the binary at a settings directory and change
//! one key — and, above all, **the thing that commissioned the verb**: that the write shows up in
//! the change journal. No unit test can answer that, because the answer involves the DISPATCHER
//! resolving both the settings directory and its `state/` child off one walk, the writer landing a
//! row, and the ledger appending a record beside them.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory, passed through
//! `Command::env` on the CHILD rather than `std::env::set_var` (which is unsafe under threads and
//! would leak across this binary's parallel cases). Without it a run on a developer box would
//! resolve the REPO's settings directory — and this verb WRITES, so here the redirect is not merely
//! isolation: it is what keeps a test out of a real settings database.
//!
//! ⚠ **A write REFUSES with no settings database at all** (`docs/decisions/0086`:
//! `vike_secrets::write_setting_row_in`'s `RowWriteError::NoDatabase` — the primitive never CREATES
//! the store). Most cases therefore call [`Case::seed`] first, planting a database exactly as
//! `vike-cli secrets init` would have left one; the one case that specifically proves the
//! NO-DATABASE branch (`a_venue_key_is_routed_to_the_venue_setting_table`) does not.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// A throwaway PROJECT and its `settings/` child — the same fixture shape (and the same two repairs
/// behind it) as `crates/vike-cli/tests/config_cli.rs`'s `Case`: a BOUND `tempfile::TempDir` so a
/// kill signal cannot leave a foreign-owned directory behind, and `$VIKE_SETTINGS_DIR` naming the
/// `settings/` CHILD so `<project>` is the bound root rather than the shared system temp directory.
struct Case {
    /// Held only for its `Drop` — removing the tree even on a panic path — since no test in this
    /// file reaches above `dir` any more (the `--file` refusal test used to build a sibling path
    /// off it; it now names an arbitrary absolute path instead, since this verb refuses `--file`
    /// before ever resolving one).
    #[expect(dead_code)]
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

    /// Plant a settings database — a PREREQUISITE for every write this file drives except the one
    /// case that proves the NO-DATABASE refusal.
    fn seed(&self, rows: vike_secrets::StoredSettings) {
        vike_secrets::plant_settings_rows(&self.dir, &rows).expect("seed");
    }

    /// One settings row's value, dotted-key spelled — the row-native replacement for reading a
    /// settings FILE back.
    fn read_setting(&self, key: &str) -> Option<String> {
        let (section, leaf) = key.split_once('.')?;
        let rows = vike_secrets::read_settings_in(&self.dir).ok()?;
        rows.rows()?
            .settings
            .iter()
            .find(|r| r.section == section && r.key == leaf)
            .map(|r| r.value.clone())
    }

    /// One venue's arming MODE.
    fn arming_mode(&self, venue: &str) -> Option<String> {
        let rows = vike_secrets::read_settings_in(&self.dir).ok()?;
        rows.rows()?
            .arming
            .iter()
            .find(|r| r.venue == venue && r.label.is_none())
            .map(|r| r.mode.clone())
    }

    /// Run `vike-cli config set …` with the case's directory as THE settings directory and an
    /// otherwise EMPTY environment, so no ambient variable from the developer's shell can move a
    /// row. Stdin is `/dev/null`.
    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("config").arg("set").args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", &self.dir);
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }

    /// As [`Case::run`], with `input` piped to the child's stdin — the channel a SECRET venue field
    /// arrives on (`-`), which `run`'s `/dev/null` cannot exercise.
    fn run_with_stdin(&self, args: &[&str], input: &str) -> Output {
        use std::io::Write as _;
        let mut cmd = Command::new(BIN);
        cmd.arg("config").arg("set").args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", &self.dir);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the vike-cli binary must run");
        child.stdin.take().expect("piped stdin").write_all(input.as_bytes()).expect("write stdin");
        child.wait_with_output().expect("the child exits")
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
    c.seed(vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "config".to_string(),
            key: "tradehub_addr".to_string(),
            value: "\"127.0.0.1:7879\"".to_string(),
        }],
        arming: vec![],
        ..Default::default()
    });

    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"127.0.0.1:7999\""));

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
/// a row and says nothing leaves somebody believing a ceiling moved when it will not until a
/// restart.
#[test]
fn the_confirmation_says_it_is_not_live_and_reports_the_read_verdict() {
    let c = Case::new("notlive");
    c.seed(vike_secrets::StoredSettings::default());
    let out = c.run(&["preferences.log_level", "warn"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("preferences.log_level"), "{text}");
    assert!(text.contains("restart"), "the not-live note is the point: {text}");
    // `config show` renders a READ column from the same table; a write that reported nothing about
    // it would be the CLI instance of the failure that column exists to remove.
    assert!(
        text.contains("read by") || text.contains("NOTHING READS"),
        "the READ verdict must be reported: {text}"
    );
}

/// **The one-row gate: exactly the named row moves, and a sibling row survives** — the row-native
/// replacement for byte preservation, since there is no file left to preserve bytes of.
#[test]
fn a_write_touches_only_its_own_row() {
    let c = Case::new("sibling");
    c.seed(vike_secrets::StoredSettings {
        settings: vec![
            vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "tradehub_addr".to_string(),
                value: "\"127.0.0.1:7879\"".to_string(),
            },
            vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "log_dir".to_string(),
                value: "\"/var/tmp/vike-logs\"".to_string(),
            },
        ],
        arming: vec![],
        ..Default::default()
    });
    assert!(c.run(&["config.log_dir", "/var/tmp/other"]).status.success());
    assert_eq!(c.read_setting("config.log_dir").as_deref(), Some("\"/var/tmp/other\""));
    assert_eq!(
        c.read_setting("config.tradehub_addr").as_deref(),
        Some("\"127.0.0.1:7879\""),
        "the sibling row is untouched"
    );
}

/// **The credential fence at the binary.** A credential-shaped key is refused on the USAGE rung,
/// nothing is written, and the refusal names the surface that owns credentials and the channel they
/// arrive on. It is unreachable on today's key set (no settings field is credential-shaped) and is
/// wired so that the day one lands, this verb is already correct.
#[test]
fn a_credential_shaped_key_is_refused_and_writes_nothing() {
    let c = Case::new("secret");
    c.seed(vike_secrets::StoredSettings::default());
    let out = c.run(&["config.bot_token", "hunter2"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("secrets set"), "{err}");
    assert!(err.contains("stdin"), "{err}");
    assert!(c.read_setting("config.bot_token").is_none(), "nothing may be written");
    assert!(!c.journal().contains("bot_token"), "a verb-level refusal writes no ledger row");
}

/// **No retype confirm, for any key** (`docs/decisions/0086` point 7) — a policy RISK CEILING
/// writes in one un-retyped line, and `--confirm` is refused BY NAME rather than silently ignored.
#[test]
fn a_policy_risk_ceiling_needs_no_confirm_and_a_confirm_flag_is_refused_as_removed() {
    let c = Case::new("noconfirm");
    c.seed(vike_secrets::StoredSettings::default());

    let out = c.run(&["policy.max_notional_per_order", "250"]);
    assert!(out.status.success(), "a risk ceiling is a plain write now: {}", stderr(&out));
    assert_eq!(c.read_setting("policy.max_notional_per_order").as_deref(), Some("250"));

    let out =
        c.run(&["policy.max_account_exposure", "3", "--confirm", "policy.max_account_exposure"]);
    assert_eq!(code(&out), 2, "the removed flag must be refused, not ignored: {}", stderr(&out));
    assert!(stderr(&out).contains("removed"), "{}", stderr(&out));
    assert!(stderr(&out).contains("0086"), "{}", stderr(&out));
    assert!(c.read_setting("policy.max_account_exposure").is_none(), "nothing may be written");
}

/// …and the ARMING ceiling in the SAME section needs none either — which is now the ordinary case
/// rather than a special one, since the whole ceremony is gone.
#[test]
fn an_arming_ceiling_needs_no_confirm() {
    let c = Case::new("arming");
    let venue = vike_model::VENUES[0];
    c.seed(vike_secrets::StoredSettings {
        settings: vec![],
        arming: vec![vike_secrets::ArmingRow {
            venue: venue.to_string(),
            label: None,
            mode: "demo".to_string(),
            max_exposure: None,
        }],
        ..Default::default()
    });
    let key = format!("policy.venues.{venue}");
    let out = c.run(&[key.as_str(), "paper"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert_eq!(c.arming_mode(venue).as_deref(), Some("paper"));
}

/// An unknown key is refused with the LOADER's own message — the same words a restart would have
/// printed — on the usage rung, with no row written and a `refused` row in the ledger.
#[test]
fn an_unknown_key_is_refused_by_the_loader_and_the_refusal_is_journalled() {
    let c = Case::new("unknown");
    c.seed(vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "config".to_string(),
            key: "tradehub_addr".to_string(),
            value: "\"127.0.0.1:7879\"".to_string(),
        }],
        arming: vec![],
        ..Default::default()
    });
    let out = c.run(&["config.no_such_setting", "1"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("no_such_setting"), "{}", stderr(&out));
    assert_eq!(
        c.read_setting("config.tradehub_addr").as_deref(),
        Some("\"127.0.0.1:7879\""),
        "a refused write changes nothing"
    );
    let ledger = c.journal();
    assert!(ledger.contains("refused"), "the refusal must be recorded: {ledger}");
    assert!(ledger.contains("config.no_such_setting"), "{ledger}");
}

/// A first segment that names no settings section is a usage error naming all four spellings and
/// the READ verb — the answer somebody who does not know the vocabulary can act on. No database is
/// needed: this refuses before one would be opened.
#[test]
fn a_key_naming_no_settings_section_is_refused_with_the_menu() {
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
/// This test process holds the settings DATABASE's write lock (`vike_secrets::hold_write_lock`),
/// then runs `vike-cli config set`. The binary must refuse — the ordinary run failure, since
/// re-running unchanged can succeed once the holder releases — and change no row. **It also kills
/// cleanly**: the property under test is that a busy database is reported rather than silently
/// waited past forever.
#[test]
fn a_lock_held_by_another_process_refuses_the_write_and_changes_nothing() {
    let c = Case::new("locked");
    c.seed(vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "config".to_string(),
            key: "tradehub_addr".to_string(),
            value: "\"127.0.0.1:7879\"".to_string(),
        }],
        arming: vec![],
        ..Default::default()
    });

    let held = vike_secrets::hold_write_lock(&c.dir);

    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);

    assert_eq!(code(&out), 1, "a busy box is a run failure, not a usage error: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("NOTHING was written"), "{err}");
    assert_eq!(
        c.read_setting("config.tradehub_addr").as_deref(),
        Some("\"127.0.0.1:7879\""),
        "a lock refusal changes NO row"
    );

    drop(held);
    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    assert!(out.status.success(), "the same write lands once released: {}", stderr(&out));
    assert_eq!(c.read_setting("config.tradehub_addr").as_deref(), Some("\"127.0.0.1:7999\""));
}

/// **A box with no settings database at all refuses on the RUN rung**, naming the command that
/// provisions one — re-running this exact command after `vike-cli secrets init` can succeed,
/// which is the property that puts it on the run rung rather than the usage one.
#[test]
fn a_box_with_no_database_refuses_on_the_run_rung_and_names_the_fix() {
    let c = Case::new("nodb");
    // No seed: this case's whole point is the absent database.
    let out = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("no settings database"), "{}", stderr(&out));
}

/// **No `--file` reaches this verb**, in either spelling. The destination is the resolved settings
/// directory and nothing else — the rule
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` states for
/// the credential store, honoured here because the reason transfers whole.
#[test]
fn there_is_no_way_to_name_a_destination_path() {
    let c = Case::new("nofile");
    c.seed(vike_secrets::StoredSettings::default());
    for argv in [
        vec!["--file", "/tmp/elsewhere", "config.log_dir", "/tmp/x"],
        vec!["config.log_dir", "/tmp/x", "--file", "/tmp/elsewhere"],
    ] {
        let out = c.run(&argv);
        assert_eq!(code(&out), 2, "{argv:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("--file"), "{}", stderr(&out));
    }
}

/// **THE LIVE-ARM FLAG NEEDS NO CONFIRM EITHER** — the deleted ceremony applied even to
/// `flags.tradehub_live`, whose own doc calls it "the single largest blast radius on this list".
/// The class this used to test (a flag sharing its file with ordinary toggles) is moot now that no
/// key anywhere demands a retype.
#[test]
fn the_live_arm_flag_writes_in_one_line_with_no_confirm() {
    let c = Case::new("liveflag");
    c.seed(vike_secrets::StoredSettings::default());

    let out = c.run(&["flags.tradehub_live", "true"]);
    assert!(out.status.success(), "a real-money arm is a plain write now: {}", stderr(&out));
    assert_eq!(c.read_setting("flags.tradehub_live").as_deref(), Some("true"));

    // …and an ORDINARY flag in the same section is unaffected either way.
    let out = c.run(&["flags.cancel_orders_on_shutdown", "true"]);
    assert!(out.status.success(), "{} / {}", stdout(&out), stderr(&out));
    assert_eq!(c.read_setting("flags.cancel_orders_on_shutdown").as_deref(), Some("true"));
}

/// **A `venue.*` key ROUTES TO THE TABLE, not to a settings section** — proven over the shipped
/// binary by the refusal it gives, which no section-shaped key can produce.
///
/// ⚠ The routing is the claim worth testing here. `execute` resolves a key to its settings SECTION
/// before anything else, and no section can spell one of these: `vike_config::Config` carries
/// `#[serde(deny_unknown_fields)]` and has no `venue` field — which is why the whole `config`
/// section stopped deserializing on the CI box when these values were filed as `config.venue.…` rows.
/// So a `venue.*` key that reached the section grammar would be refused for the WRONG reason, and
/// the two refusals are what tells the branches apart.
///
/// The WRITE itself is covered where it lives, by `vike-secrets`' `venue_setting_writer` suite —
/// including the one that matters most, that the DDL refuses a mistyped tier.
#[test]
fn a_venue_key_is_routed_to_the_venue_setting_table() {
    let c = Case::new("venue-route");
    // No database in this settings directory — which is the state the branch reports on.
    let out = c.run(&["venue.dukascopy.demo.server", "https://example.invalid/x.jnlp"]);
    let err = stderr(&out);

    assert!(!out.status.success(), "a box with no database must refuse: {err}");
    // The VENUE branch's own words. A section-grammar refusal names a settings SECTION and says
    // nothing about a database, so this string is what proves which branch ran.
    assert!(err.contains("no settings database"), "not the venue branch: {err}");
    assert!(err.contains("secrets init"), "the refusal must name the way through: {err}");
    assert!(err.contains("holds no venue settings"), "…in the venue branch's words: {err}");

    // ⚠ THE CONTROL: a section-shaped key on the SAME empty directory is refused too (there is
    // genuinely no database for either), but with a DIFFERENT message — the ordinary planner's own
    // `RowWriteError::NoDatabase`, which says nothing about venue settings. Without this the test
    // would pass for a binary that refused every key with one identical message.
    let other = c.run(&["config.tradehub_addr", "127.0.0.1:7999"]);
    assert!(!other.status.success(), "{}", stderr(&other));
    assert!(
        !stderr(&other).contains("holds no venue settings"),
        "a section key must not take the venue branch's route: {}",
        stderr(&other)
    );
}

/// **A SECRET venue field's value arrives on stdin, through the shipped binary.** The real
/// `std::io::stdin()` is wired in `execute` only; the in-process tests hand `venue_write` a byte
/// slice, so nothing else proves a value piped to the process reaches the store — redacted
/// everywhere it is reported, and trimmed to what the field grammar checked (a trailing space and
/// the `\r\n` a Windows pipe adds are not part of a proxy URL).
#[test]
fn a_secret_venue_value_piped_on_stdin_lands_trimmed_and_is_never_echoed() {
    const USER: &str = "zqxjvw7413mfbphgn";
    const PASS: &str = "dgnhpbfm6528317wvjxqz";
    let url = format!("socks5h://{USER}:{PASS}@198.51.100.7:1080");
    let c = Case::new("venue-stdin");
    c.seed(vike_secrets::StoredSettings::default());

    let out = c.run_with_stdin(&["venue.polymarket.socks_proxy", "-"], &format!("{url}  \r\n"));
    let shown = format!("{}{}", stdout(&out), stderr(&out));
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("<set>"), "the report says a value was set, not which: {shown}");
    for secret in [USER, PASS] {
        assert!(!shown.contains(secret), "the report echoed part of the proxy: {shown}");
        assert!(!c.journal().contains(secret), "the ledger carries part of the proxy");
    }
    assert!(c.journal().contains("venue.polymarket.socks_proxy"), "the write is journalled");

    let stored = vike_secrets::venue_setting::load_venue_settings(&c.dir).expect("rows read");
    assert_eq!(
        stored["polymarket"].get(vike_secrets::venue_setting::SettingTier::Any, "socks_proxy"),
        Some(url.as_str()),
        "the stored value is the one the grammar checked, without the stdin padding"
    );

    // An empty stdin is a value the grammar refuses, and nothing is written over the good row.
    let empty = c.run_with_stdin(&["venue.polymarket.socks_proxy", "-"], "");
    assert!(!empty.status.success(), "{}{}", stdout(&empty), stderr(&empty));
    let after = vike_secrets::venue_setting::load_venue_settings(&c.dir).expect("rows read");
    assert_eq!(
        after["polymarket"].get(vike_secrets::venue_setting::SettingTier::Any, "socks_proxy"),
        Some(url.as_str())
    );
}

/// **A venue key that names no declared field is refused on the USAGE rung, over the shipped
/// binary** — the same catalog `vike-cli config show` prints from, so a typo is an error instead
/// of a row nothing reads.
#[test]
fn an_undeclared_venue_field_exits_on_the_usage_rung_through_the_binary() {
    let c = Case::new("venue-undeclared");
    c.seed(vike_secrets::StoredSettings::default());
    let out = c.run(&["venue.polymarket.proxy_hots", "127.0.0.1"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("not a declared field"), "{}", stderr(&out));
}
