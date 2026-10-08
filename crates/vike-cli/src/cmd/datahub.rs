//! `vike-cli datahub setup` — MINT the datahub's node key pair — and `vike-cli datahub control-key`,
//! which hands this box's CONTROL key to a pipe.
//!
//! ```text
//! vike-cli datahub setup [--rotate]
//! vike-cli datahub control-key
//! ```
//!
//! ⚠ **Everything below about `setup` — "no form in which a key VALUE reaches or leaves" — is
//! about `setup`.** `control-key` (2026-09-26) is the one form in which a value leaves, it exists
//! because the owner ruled that Studio's Run signs with this key from the operator's PC
//! (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1), and [`run_control_key`]'s
//! own doc carries what it refuses and why it is shaped the way it is.
//!
//! # Why this exists, and why it is a SEPARATE verb from `backend`
//!
//! `vike-datahub` authenticates with the same HMAC handshake `vike-tradehub` does — two keys, two
//! scopes, `vike_node_proto::auth`'s constants — and until 2026-09-08 **nothing in this
//! tree could generate that pair**. `vike-cli backend setup` mints the TRADEHUB pair and validates
//! its names against a table that did not carry the datahub's, `secrets set` refuses both names by
//! design, and so the datahub deployed to the CI box on 2026-09-08 had its keys made with `openssl` at a
//! shell. A pair a human invents is exactly what
//! [`crate::cmd::node`]'s module doc argues against: the measured pain was never "I could not type
//! my key", it was "I had to invent one and nothing told me how".
//!
//! ⚠ **`backend` means a `vike-tradehub` node, and a datahub is not one.** It speaks a different
//! protocol, serves different verbs, and its control scope compiles client-supplied Rhai rather than
//! placing orders. Folding this in as `backend setup --datahub` would make one verb answer for two
//! services, and the flag would have to disable most of what `backend setup` does — the bind
//! address, the control flag, the restart line — none of which apply here. A separate verb costs a
//! word and keeps each command's help true.
//!
//! # What it deliberately does NOT do
//!
//! **It writes no settings.** `backend setup` also sets `config.tradehub_addr` and, under
//! `--control`, `flags.tradehub_control`. There is no equivalent pair here to set, and that is a
//! fact about the datahub rather than an omission: `vike_config::Config`'s three addresses are
//! documented as three different jobs, and the one named `datahub_addr` is where a CLIENT DIALS —
//! not where the server binds. The server's bind arrives as `VIKE_DATAHUB_ADDR`, which
//! `deploy/vike-datahub.service` sets as an `Environment=` line, and a verb that wrote a client key
//! hoping to move a server's bind would be the kind of near-miss this workspace pays for later.
//!
//! So this verb does the one thing nothing else can: it MINTS.
//!
//! # Everything `backend setup`'s doc says about key handling applies here unchanged
//!
//! No argv form, no stdin form, no `--from-env` — there is no operator-supplied value at all. The
//! keys are CSPRNG bytes handed straight to `vike_secrets::save_credentials_to_store` in-process.
//! Nothing is printed but each key's `key_id` (`vike_node_proto::auth`'s
//! `key_fingerprint`, an HMAC
//! tag under its own domain separator, gate-proved disjoint from the auth domain). An existing key
//! is a key some client is already signing with, so replacing one needs `--rotate`.
//!
//! The store is the project's node-key store — the settings database's `node_key` table
//! ([`crate::cmd::node::landed`]; the `node.env` file store was removed on 2026-10-07) — and an
//! ABSENT one is REFUSED rather than created, which is `docs/decisions/0036`'s fence and
//! `docs/decisions/0051`'s file.

use std::process::ExitCode;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::node::{
    Ctx, key_id, landed, mint_key, open_store, record_credential_write, settings_dir,
};
use crate::exit::{CliError, CmdResult};

pub(crate) const USAGE: &str = "\
usage: vike-cli datahub setup [--rotate]
       vike-cli datahub control-key

setup        MINT the two node keys a vike-datahub server authenticates with,
             into the settings database (`vike-cli secrets migrate --init`
             creates it). Run it on the DATAHUB's box. It never accepts a key and
             never prints one: what it prints is each key's ID, which is what
             you compare against the server's own log line.
control-key  hand THIS box's datahub CONTROL key to a PIPE — the one form in
             which a key VALUE leaves this command, and only for a launcher
             that reads it over ssh (`just studio` hands it to Studio's Run).
             It REFUSES a terminal, prints the value and nothing else on
             stdout, and names the key's ID on stderr.

options (setup):
  --rotate   replace keys already in the store. Every client still holding the
             old ones stops working the moment the server restarts
  -h         this help

The server's bind address is NOT set here: it arrives as VIKE_DATAHUB_ADDR,
which deploy/vike-datahub.service sets. `config.datahub_addr` is a CLIENT's
dial address and setting it would move nothing on this box.
";

/// The two subcommands. `control-key` takes no option at all.
enum Args {
    Setup { rotate: bool },
    ControlKey,
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err("a subcommand is required (setup | control-key)".to_string());
    };
    let mut rotate = None;
    match first.as_str() {
        "setup" => rotate = Some(false),
        "control-key" => {}
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `datahub` subcommand '{other}'")),
    }
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            // ⚠ `no_value` rather than ignoring `inline`: `--rotate=maybe` is a command line whose
            // author believed it meant something, and silently dropping the value is how a person
            // ends up thinking they did not rotate when they did. Same treatment `backend`'s own
            // valueless flags get.
            "--rotate" if rotate.is_some() => {
                no_value(&flag, inline)?;
                rotate = Some(true);
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(match rotate {
        Some(rotate) => Args::Setup { rotate },
        None => Args::ControlKey,
    })
}

pub fn run(args: impl Iterator<Item = String>, ctx: &Ctx<'_>) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        // ⚠ `(command, usage, msg)` — this passed `(&e, USAGE, "vike-cli datahub")` until
        // 2026-09-26, so the message never equalled the help sentinel: `datahub --help` exited 2
        // with `vike-cli help requested: vike-cli datahub` on stderr, and every real usage error
        // printed as `vike-cli <the error>: vike-cli datahub`. Caught by `scripts/cli_mcp_smoke.sh`'s
        // help sweep; `crates/vike-cli/tests/help_cli.rs` now sweeps every top-level verb.
        Err(e) => return exit_for_parse_error("datahub", USAGE, &e),
    };
    let outcome = match parsed {
        Args::Setup { rotate } => run_setup(rotate, ctx),
        Args::ControlKey => {
            run_control_key(ctx, std::io::IsTerminal::is_terminal(&std::io::stdout()))
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli datahub: {}", e.msg);
            e.exit.into()
        }
    }
}

/// The datahub's two key names, from the one table that classifies them.
///
/// ⚠ Read out of `vike_model::credential_keys::PLATFORM_KEYS` by INDEX rather than spelled here,
/// and validated against `platform_key_service` — a fourth hand copy of these names is exactly what
/// `vike_tradehub_client::auth`'s doc warns re-opens the gap that table exists to close. The
/// equality assertion this indexing rests on is
/// `crates/vike-cli/tests/node_cli.rs`'s `the_platform_key_table_carries_the_datahub_servers_own_spelling`.
fn datahub_key_names() -> CmdResult<[&'static str; 2]> {
    let table = vike_model::credential_keys::PLATFORM_KEYS;
    let names = [table[2], table[3]];
    for n in names {
        if vike_model::credential_keys::platform_key_service(n) != Some("vike-datahub") {
            return Err(CliError::failed(format!(
                "{n} is not classified as a vike-datahub node key — PLATFORM_KEYS was reordered \
                 without this verb being updated, and minting under the wrong name would write a \
                 key no server reads"
            )));
        }
    }
    Ok(names)
}

fn run_setup(rotate: bool, ctx: &Ctx<'_>) -> CmdResult<()> {
    let names = datahub_key_names()?;
    // ⚠ The store that ANSWERS, not the file. `docs/decisions/0054`'s credential half puts the node
    // pair in the settings database's `node_key` table whenever that database exists, and this verb
    // is `backend setup`'s twin: the overwrite refusal below and the write must both be asked of the
    // same store, which `vike_secrets::backend_in` decides once for both.
    let dir = settings_dir(ctx);
    let path = landed(ctx, &vike_secrets::backend_in(&dir));
    let existing = open_store(&dir)?;

    // A key already in the store is a key some client is already signing with. Replacing one is a
    // decision with a blast radius, not an idempotent re-run — the same refusal `backend setup`
    // makes.
    let present: Vec<&str> = names
        .into_iter()
        .filter(|n| existing.get(*n).is_some_and(|v| !v.trim().is_empty()))
        .collect();
    if !present.is_empty() && !rotate {
        return Err(CliError::failed(format!(
            "{} already in {} — refusing to replace {} without --rotate. Every client holding the \
             old key stops working the moment the server restarts, so this is a decision rather \
             than a re-run.",
            present.join(" and "),
            path.display(),
            if present.len() == 1 { "it" } else { "them" }
        )));
    }

    // One call, both keys, so the pair can never be half-rotated by a failure between two writes.
    let observe = mint_key();
    let control = mint_key();
    let ids = [key_id(&observe), key_id(&control)];
    let updates = [(names[0].to_string(), observe), (names[1].to_string(), control)];
    vike_secrets::save_credentials_to_store(
        &dir,
        vike_secrets::Table::NodeKey,
        &updates,
        // No account classification: decision 0051 gives this pair its own store, it belongs to
        // no venue and no account, and `node_key` is `(name, value)` in every schema.
        None,
    )
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;

    // Key NAMES only — `Change::credential_write` takes no value parameter at all, so nothing here
    // CAN put a credential in the ledger.
    record_credential_write(ctx, &path, &names);

    println!("minted into {}:", path.display());
    println!("  {}  id {}", names[0], ids[0]);
    println!("  {}  id {}", names[1], ids[1]);
    println!();
    println!("restart the server so it loads them:");
    println!("  sudo systemctl restart vike-datahub");
    println!();
    println!("it will log `AUTHENTICATION REQUIRED` with both key ids — compare them with the two");
    println!("above. A client authenticates by holding the same pair; `vike-cli secrets path`");
    println!("prints where this box keeps it.");
    Ok(())
}

/// **`datahub control-key` — hand this box's datahub CONTROL key to a PIPE, and nowhere else.**
///
/// # Why a verb that emits a key exists at all
///
/// The owner ruled on 2026-09-26 (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question
/// 1, option (a)) that the desktop resolves the datahub Control key for Studio's COMPUTE dial. The
/// desktop runs on the operator's PC; the key's only home is this box's node store — on a migrated
/// box a row in the settings database, which no tool on the box can print (`setup` prints ids,
/// `secrets list` leaves node keys out) and which `just studio` must therefore ask the PRODUCT to
/// read. The two alternatives were measured and refused: a second copy in a key FILE beside the
/// builder's (which `setup --rotate` would silently leave stale, and which nothing could fill from
/// the row in the first place), and the PC's own store (which `vike-cli` on the PC also reads, so
/// the key would reach `Backfill` and `DeleteSeries` there too).
///
/// # The shape, and what each part refuses
///
/// - **A terminal is refused, BEFORE the store is opened.** This hands a value to a launcher that
///   captures it; a terminal is a screen, a scrollback and a screen-share. `ssh host '…'` without
///   `-t` gives the remote side a pipe, which is the launcher's shape. The check is a guard against
///   ACCIDENT, not a boundary: whoever can run this can already read the store it reads — the
///   database is the invoking user's own `0600` file — so it adds no reach, only a spelling.
/// - **It reads exactly what this box's DATAHUB verifies**: `vike_secrets::resolve_node_keys` with
///   the datahub family predicate — the call `crates/vike-datahub/src/datahub_cli.rs` makes at its
///   own root — and never the process environment, which the daemon does not read either. So a
///   key exported in the invoking shell cannot be handed out in place of the one the daemon holds.
/// - **stdout carries the value and nothing else**; the `key_id` goes to stderr, where the operator
///   sees it and can compare it with the daemon's `AUTHENTICATION REQUIRED` line. Nothing is
///   journalled: the ledger records what CHANGES on a box, and a read changes nothing.
/// - **It is advertised by no MCP tool** — `docs/decisions/0036`'s rule for every credential verb.
fn run_control_key(ctx: &Ctx<'_>, stdout_is_terminal: bool) -> CmdResult<()> {
    let [_, control] = datahub_key_names()?;
    if stdout_is_terminal {
        return Err(CliError::failed(terminal_refusal(control)));
    }
    let dir = settings_dir(ctx);
    let dir_str = dir.to_str().ok_or_else(|| {
        CliError::failed(format!(
            "the settings directory {} is not valid UTF-8, so its node-key store cannot be named. \
             Nothing was printed.",
            dir.display()
        ))
    })?;
    let resolved = vike_secrets::resolve_node_keys(
        Some(dir_str),
        vike_model::credential_keys::is_datahub_node_key,
    )
    .map_err(|e| {
        CliError::failed(format!(
            "{e} — the node-key store is THERE and could not be read, which is a different problem \
             from having none. Nothing was printed."
        ))
    })?;
    // Findings, never refusals — a path and an octal mode, or an unread node-key FILE; never a key.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    if let Some(u) = &resolved.unread {
        eprintln!("⚠ {u}");
    }
    let value = resolved
        .secrets
        .into_map()
        .remove(control)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let Some(value) = value else {
        return Err(CliError::failed(absent_refusal(control, dir_str)));
    };
    {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        writeln!(out, "{value}")
            .and_then(|()| out.flush())
            .map_err(|e| CliError::failed(format!("could not write to stdout: {e}")))?;
    }
    eprintln!("vike-cli datahub: handed {control} (id {}) to a pipe", key_id(&value));
    Ok(())
}

/// The refusal for a terminal on stdout. PURE, so the sentence is unit-tested.
fn terminal_refusal(control: &str) -> String {
    format!(
        "refusing to print {control} to a TERMINAL. `datahub control-key` hands the key to a PIPE — \
         a launcher that captures it, like `just studio` reading it over ssh — and a terminal is a \
         screen, a scrollback and a screen-share. Nothing was read."
    )
}

/// The refusal for a store that holds no datahub CONTROL key. PURE, so the sentence is unit-tested.
fn absent_refusal(control: &str, settings_dir: &str) -> String {
    format!(
        "no {control} in the node-key store under {settings_dir} — this box's datahub serves \
         without a Control key, so there is nothing to hand over. `vike-cli datahub setup` mints \
         the pair (and a restart of the datahub loads it). Nothing was printed."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> impl Iterator<Item = String> + use<> {
        a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn a_subcommand_is_required_and_an_unknown_one_is_a_clean_error() {
        assert!(parse(argv(&[])).is_err());
        assert!(
            parse(argv(&["connect"])).is_err(),
            "only `setup` and `control-key` exist here today"
        );
    }

    #[test]
    fn rotate_is_off_unless_asked() {
        assert!(matches!(parse(argv(&["setup"])), Ok(Args::Setup { rotate: false })));
        assert!(matches!(parse(argv(&["setup", "--rotate"])), Ok(Args::Setup { rotate: true })));
    }

    /// `control-key` takes NO option — `--rotate` belongs to `setup`, and accepting it here would
    /// be a flag that silently means nothing on the verb that emits a key.
    #[test]
    fn control_key_parses_and_takes_no_option() {
        assert!(matches!(parse(argv(&["control-key"])), Ok(Args::ControlKey)));
        assert!(parse(argv(&["control-key", "--rotate"])).is_err());
        assert!(parse(argv(&["control-key", "--stdout"])).is_err());
    }

    /// ⚠ **A TERMINAL is refused before the store is even opened** — proven by pointing the
    /// settings directory at one that does not exist: the answer is the terminal refusal, not "no
    /// store", so nothing was read on the way to it.
    #[test]
    fn control_key_refuses_a_terminal_before_opening_anything() {
        let keys = crate::cmd::nodekeys::NodeKeyring::default();
        let missing = std::path::Path::new("/nonexistent/vike-control-key-test/settings");
        let ctx = Ctx {
            settings_dir: Some(missing),
            settings_dir_override: None,
            state_dir: None,
            node_addr: None,
            keys: &keys,
            backtest_addr: None,
            datahub_keys: None,
            now_ms: 0,
        };
        let err = run_control_key(&ctx, true).expect_err("a terminal must be refused");
        assert!(err.msg.contains("TERMINAL"), "{}", err.msg);
        assert!(err.msg.contains("Nothing was read"), "{}", err.msg);
    }

    /// Both refusals name the key by NAME and say that nothing was emitted; the absent one names
    /// the verb that fixes it.
    #[test]
    fn the_control_key_refusals_name_the_key_and_the_fix() {
        let [_, control] = datahub_key_names().expect("names");
        let t = terminal_refusal(control);
        assert!(t.contains(control) && t.contains("PIPE"), "{t}");
        let a = absent_refusal(control, "/p/settings");
        assert!(a.contains(control) && a.contains("datahub setup"), "{a}");
        assert!(a.contains("Nothing was printed"), "{a}");
    }

    /// ⚠ The names come from the shared table, and this is the assertion that the INDEXING is right.
    ///
    /// `PLATFORM_KEYS` is ordered — tradehub first, datahub appended — and `[2]`/`[3]` is a claim
    /// about that order. A reorder would silently make this verb mint under the tradehub's names,
    /// writing a pair no datahub reads and no tradehub expects; the classifier check inside
    /// `datahub_key_names` is what turns that into a refusal, and this proves the check fires.
    #[test]
    fn the_names_are_the_datahubs_and_are_classified_as_such() {
        let names = datahub_key_names().expect("the table classifies both");
        assert!(names[0].contains("DATAHUB"), "{names:?}");
        assert!(names[1].contains("DATAHUB"), "{names:?}");
        assert_ne!(names[0], names[1]);
        for n in names {
            assert_eq!(vike_model::credential_keys::platform_key_service(n), Some("vike-datahub"));
        }
    }

    /// The usage text may not promise what the verb does not do. It says the bind address is NOT set
    /// here, and that sentence is the one an operator acts on.
    #[test]
    fn the_usage_says_it_writes_no_settings() {
        assert!(USAGE.contains("VIKE_DATAHUB_ADDR"), "it must name where the bind DOES come from");
        assert!(!USAGE.contains("--addr"), "this verb takes no address flag");
        assert!(!USAGE.contains("--control"), "there is no control flag on this service");
    }
}
