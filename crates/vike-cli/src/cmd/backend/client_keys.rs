//! `vike-cli backend client-keys [--observe-only]` — on the SERVER's box: hand the keys a CLIENT of
//! this box needs to a PIPE, and nowhere else
//! (`docs/decisions/0280-every-hub-mints-its-full-keys-at-first-start-and-a-client-takes-them-over-ssh.md`).
//!
//! It is the server half of `connect --server-project`: the client's `connect` runs it over the ssh
//! session the operator already uses for the tunnel (`ssh HOST '<project>/bin/vike-cli backend
//! client-keys'`), reads the lines off that session's stdout into memory, and files them in the
//! client's own `hub_key` table. The value travels inside the ssh channel and a pipe: never an
//! argument list (the remote command line carries the verb and a path), never a terminal, never a log.
//!
//! # The shape, and what each part refuses — `datahub control-key`'s, widened by the owner's ruling
//!
//! - **A terminal is refused, BEFORE the store is opened.** A terminal is a screen, a scrollback and
//!   a screen-share; `ssh host '…'` without `-t` gives the remote side a pipe. A guard against
//!   ACCIDENT, not a boundary: whoever can run this can already read the store it reads (the
//!   database is the invoking user's own `0600` file), so it adds no reach.
//! - **It hands the CLIENT keys and nothing else** ([`client_key_names`]): the tradehub pair and the
//!   datahub pair (the backtesthub verifies that pair too). The admin keys, the gatewayhub's keys and
//!   the strategy-lane key are daemon keys and stay on this box; the read's predicate admits these
//!   four names alone, so this process never holds another.
//! - **`--observe-only` hands the two OBSERVE keys alone**: read-only access to share with somebody
//!   who should watch and never trade or write. It is the optional issue the ruling keeps; the full
//!   pair is the default.
//! - **stdout carries `NAME=VALUE` lines and nothing else**; each key's `key_id` goes to stderr,
//!   where the operator can compare it with what `connect` prints on the other box. Nothing is
//!   journalled: the ledger records what CHANGES on a box, and a read changes nothing.
//! - **It is advertised by no MCP tool** (`docs/decisions/0036`'s rule; `vike-mcp` names no verb of
//!   this crate).

use std::io::Write;

use vike_hub_protocol::auth::{DATAHUB_CONTROL_KEY_ENV, DATAHUB_OBSERVE_KEY_ENV};
use vike_secrets::CredentialSource;

use super::{Args, Context, key_id};
use crate::exit::{CliError, CommandResult};

/// The keys a CLIENT of this box holds, in the order they are handed: the tradehub's observe and
/// control keys, then the datahub's. `connect --server-project` accepts these names and no other.
pub(super) fn client_key_names() -> [&'static str; 4] {
    let [observe, control] = super::key_names();
    [observe, control, DATAHUB_OBSERVE_KEY_ENV, DATAHUB_CONTROL_KEY_ENV]
}

/// The OBSERVE halves of [`client_key_names`]: what `--observe-only` hands.
pub(super) fn observe_key_names() -> [&'static str; 2] {
    let [observe, _, datahub_observe, _] = client_key_names();
    [observe, datahub_observe]
}

/// Run `backend client-keys`. `stdout_is_terminal` is the caller's probe, a parameter so the refusal
/// is reachable from a test.
pub(super) fn run_client_keys(
    args: &Args,
    ctx: &Context<'_>,
    stdout_is_terminal: bool,
) -> CommandResult<()> {
    if stdout_is_terminal {
        return Err(CliError::failed(TERMINAL_REFUSAL.to_string()));
    }
    let names: Vec<&str> = if args.observe_only {
        super::validated(observe_key_names())?.to_vec()
    } else {
        super::validated(client_key_names())?.to_vec()
    };
    let dir = super::settings_dir(ctx);
    let resolved = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(&dir)),
        ctx.scope,
    )
    .hub_keys(|n| names.contains(&n))
    .map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was handed over."
        ))
    })?;
    if matches!(resolved.source, CredentialSource::None) {
        return Err(CliError::failed(absent_refusal(
            &vike_secrets::db_path_in(&dir).display().to_string(),
        )));
    }
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    let held = resolved.secrets.into_map();
    let handed: Vec<(&str, &str)> = names
        .iter()
        .filter_map(|n| held.get(*n).map(|v| (*n, v.trim())).filter(|(_, v)| !v.is_empty()))
        .collect();
    if !handed.iter().any(|(n, _)| *n == names[0]) {
        return Err(CliError::failed(absent_refusal(
            &vike_secrets::db_path_in(&dir).display().to_string(),
        )));
    }
    {
        let mut out = std::io::stdout().lock();
        for (name, value) in &handed {
            writeln!(out, "{name}={value}")
                .map_err(|e| CliError::failed(format!("could not write to stdout: {e}")))?;
        }
        out.flush().map_err(|e| CliError::failed(format!("could not write to stdout: {e}")))?;
    }
    for (name, value) in &handed {
        eprintln!("vike-cli backend: handed {name} (id {}) to a pipe", key_id(value));
    }
    Ok(())
}

/// The refusal for a terminal on stdout.
const TERMINAL_REFUSAL: &str = "refusing to print hub keys to a TERMINAL. `backend client-keys` \
     hands them to a PIPE — `vike-cli backend connect <host> --server-project <dir>` on the other \
     box reads it over ssh — and a terminal is a screen, a scrollback and a screen-share. Nothing \
     was read.";

/// The refusal for a box with no store or no tradehub observe key. PURE, so the sentence is tested.
fn absent_refusal(db: &str) -> String {
    format!(
        "no tradehub observe key in {db}, so there is nothing a client could connect with. \
         `vike-cli backend mint-missing` mints every missing hub key (the server install runs it at \
         each start; a running daemon loads a new key at its restart). Nothing was handed over."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_keys_are_the_two_pairs_and_no_daemon_key() {
        let names = super::super::validated(client_key_names()).expect("platform keys");
        let table = vike_venues::credential_keys::PLATFORM_KEYS;
        assert_eq!(names, [table[0], table[1], table[2], table[3]]);
        for daemon_only in [4, 5, 6, 7, 8, 9] {
            assert!(!names.contains(&table[daemon_only]), "{}", table[daemon_only]);
        }
        assert_eq!(observe_key_names(), [table[0], table[2]]);
    }

    #[test]
    fn the_refusals_name_the_route_and_no_value() {
        assert!(TERMINAL_REFUSAL.contains("PIPE") && TERMINAL_REFUSAL.contains("Nothing was read"));
        let msg = absent_refusal("/p/settings/db/vike.db");
        assert!(msg.contains("/p/settings/db/vike.db") && msg.contains("mint-missing"), "{msg}");
    }
}
