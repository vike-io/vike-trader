//! `vike-cli datahub setup` — MINT the datahub's node key pair.
//!
//! ```text
//! vike-cli datahub setup [--rotate]
//! ```
//!
//! # Why this exists, and why it is a SEPARATE verb from `node`
//!
//! `vike-datahub` authenticates with the same HMAC handshake `vike-tradehub` does — two keys, two
//! scopes, `vike_datahub_client::node_auth`'s constants — and until 2026-09-08 **nothing in this
//! tree could generate that pair**. `vike-cli node setup` mints the TRADEHUB pair and validates its
//! names against a table that did not carry the datahub's, `secrets set` refuses both names by
//! design, and so the datahub deployed to the CI box on 2026-09-08 had its keys made with `openssl` at a
//! shell. A pair a human invents is exactly what
//! [`crate::cmd::node`]'s module doc argues against: the measured pain was never "I could not type
//! my key", it was "I had to invent one and nothing told me how".
//!
//! ⚠ **`node` means a `vike-tradehub` node, and a datahub is not one.** It speaks a different
//! protocol, serves different verbs, and its control scope compiles client-supplied Rhai rather than
//! placing orders. Folding this in as `node setup --datahub` would make one verb answer for two
//! services, and the flag would have to disable most of what `node setup` does — the bind address,
//! the control flag, the restart line — none of which apply here. A separate verb costs a word and
//! keeps each command's help true.
//!
//! # What it deliberately does NOT do
//!
//! **It writes no settings.** `node setup` also sets `config.tradehub_addr` and, under `--control`,
//! `flags.tradehub_control`. There is no equivalent pair here to set, and that is a fact about the
//! datahub rather than an omission: `vike_config::Config`'s three addresses are documented as three
//! different jobs, and the one named `datahub_addr` is where a CLIENT DIALS — not where the server
//! binds. The server's bind arrives as `VIKE_DATAHUB_ADDR`, which `deploy/vike-datahub.service`
//! sets as an `Environment=` line, and a verb that wrote a client key hoping to move a server's bind
//! would be the kind of near-miss this workspace pays for later.
//!
//! So this verb does the one thing nothing else can: it MINTS.
//!
//! # Everything `node setup`'s doc says about key handling applies here unchanged
//!
//! No argv form, no stdin form, no `--from-env` — there is no operator-supplied value at all. The
//! keys are CSPRNG bytes handed straight to `vike_secrets::save_credentials` in-process. Nothing is
//! printed but each key's `key_id` (`vike_datahub_client::node_auth`'s `key_fingerprint`, an HMAC
//! tag under its own domain separator, gate-proved disjoint from the auth domain). An existing key
//! is a key some client is already signing with, so replacing one needs `--rotate`.
//!
//! The store is `<project>/settings/node.env` — [`crate::cmd::node::store_path`] — and an ABSENT one
//! is REFUSED rather than created, which is `docs/decisions/0036`'s fence and
//! `docs/decisions/0051`'s file.

use std::process::ExitCode;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::node::{Ctx, key_id, mint_key, open_store, record_credential_write, store_path};
use crate::exit::{CliError, CmdResult};

pub(crate) const USAGE: &str = "\
usage: vike-cli datahub setup [--rotate]

MINT the two node keys a vike-datahub server authenticates with, into
<project>/settings/node.env. Run it on the DATAHUB's box.

It never accepts a key and never prints one — there is no form in which a key
VALUE reaches or leaves this command. What it prints is each key's ID, which is
what you compare against the server's own log line.

options:
  --rotate   replace keys already in the store. Every client still holding the
             old ones stops working the moment the server restarts
  -h         this help

The server's bind address is NOT set here: it arrives as VIKE_DATAHUB_ADDR,
which deploy/vike-datahub.service sets. `config.datahub_addr` is a CLIENT's
dial address and setting it would move nothing on this box.
";

struct Args {
    rotate: bool,
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err("a subcommand is required (setup)".to_string());
    };
    match first.as_str() {
        "setup" => {}
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `datahub` subcommand '{other}'")),
    }
    let mut a = Args { rotate: false };
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            // ⚠ `no_value` rather than ignoring `inline`: `--rotate=maybe` is a command line whose
            // author believed it meant something, and silently dropping the value is how a person
            // ends up thinking they did not rotate when they did. Same treatment `node`'s own
            // valueless flags get.
            "--rotate" => {
                no_value(&flag, inline)?;
                a.rotate = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(a)
}

pub fn run(args: impl Iterator<Item = String>, ctx: &Ctx<'_>) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(e) => return exit_for_parse_error(&e, USAGE, "vike-cli datahub"),
    };
    match run_setup(&parsed, ctx) {
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

fn run_setup(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let names = datahub_key_names()?;
    let path = store_path(ctx);
    let existing = open_store(&path)?;

    // A key already in the store is a key some client is already signing with. Replacing one is a
    // decision with a blast radius, not an idempotent re-run — the same refusal `node setup` makes.
    let present: Vec<&str> = names
        .into_iter()
        .filter(|n| existing.get(*n).is_some_and(|v| !v.trim().is_empty()))
        .collect();
    if !present.is_empty() && !args.rotate {
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
    vike_secrets::save_credentials(&path, &updates)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> impl Iterator<Item = String> + use<> {
        a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn a_subcommand_is_required_and_an_unknown_one_is_a_clean_error() {
        assert!(parse(argv(&[])).is_err());
        assert!(parse(argv(&["connect"])).is_err(), "only `setup` exists here today");
    }

    #[test]
    fn rotate_is_off_unless_asked() {
        assert!(!parse(argv(&["setup"])).expect("parses").rotate);
        assert!(parse(argv(&["setup", "--rotate"])).expect("parses").rotate);
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
