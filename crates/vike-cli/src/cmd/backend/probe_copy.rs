//! `vike-cli backend probe-copy --into DIR [--drop-key NAME]... [--paper-accounts] [--numeric-key NAME]...` — on the
//! DAEMON's box, for a deploy's pre-flight (`deploy/docker/vikectl`'s `probe_copy`): this project's settings database
//! copied as `DIR/db/vike.db` with every key and every SECRET venue field replaced by a placeholder and every device's private
//! key by a THROWAWAY one ([`throwaway_device_key`]), so a NEW build's daemon boots on the box's rows and on none of
//! its keys. `vike_secrets::write_probe_copy` is the copy and its module doc the contract (the order, what is dropped,
//! the read-back check); this verb only resolves the project's database, names the secret venue fields
//! (`vike_venues::venue_fields::VENUE_FIELDS`'s `secret` rows), makes the throwaway keys and prints where the copy
//! went. It replaced the `sqlite3` scripts, so no box needs the `sqlite3` tool for a probe copy (the owner,
//! 2026-11-16).
//!
//! The project's database is only READ. No value is printed, and none is taken: the three rules are row NAMES and a
//! switch. ⚠ The copy makes no live key a Rust value, but this PROCESS holds one: `crates/vike-cli/src/dispatch.rs`'s
//! `backend` arm resolves this box's own device key (the `device_key_load` row, parsed) for every `backend` verb
//! before one is claimed, `probe-copy` included, and the copy never reads it.

use std::path::Path;

use super::{Args, Context, settings_dir};
use crate::exit::{CliError, CommandResult};

/// Write the probe copy `args` asks for, from the project `ctx` resolved.
pub(super) fn run_probe_copy(args: &Args, ctx: &Context<'_>) -> CommandResult<()> {
    // `check_flag_scope` refuses `probe-copy` without `--into`.
    let into = Path::new(args.into.as_deref().unwrap_or_default());
    let live = vike_secrets::db_path_in(&settings_dir(ctx));
    let secret_venue_fields: Vec<(&str, vike_venues::venue_fields::VenueFieldName)> =
        vike_venues::venue_fields::VENUE_FIELDS
            .iter()
            .filter(|f| f.secret)
            .map(|f| (f.venue, f.field.clone()))
            .collect();
    let rules = vike_secrets::ProbeCopy {
        drop_keys: &args.drop_keys,
        paper_accounts: args.paper_accounts,
        numeric_keys: &args.numeric_keys,
        secret_venue_fields: &secret_venue_fields,
    };
    vike_secrets::write_probe_copy(&live, into, &rules, &mut throwaway_device_key).map_err(
        |e| CliError::failed(format!("the probe copy of {} was not made: {e}", live.display())),
    )?;
    println!("probe copy: {}", vike_secrets::db_path_in(into).display());
    Ok(())
}

/// A NEW device key for one `device_key` row of the copy, its key id and its PKCS#8 document: the row stays so a
/// daemon that signs as its own device boots, and no private key of this box is in the copy (decision 0288).
fn throwaway_device_key() -> std::io::Result<(String, Vec<u8>)> {
    vike_hub_protocol::access::DeviceKey::generate().map(|(key, document)| (key.key_id(), document))
}
