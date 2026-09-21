//! **`vike-cli secrets move-venue-config` — ruling 10's move, as an operator act.**
//!
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §11 step 4 takes ten config-shaped
//! keys out of `credential` and files them as settings rows. §12 forbade it until §6.2's ordering
//! (A) existed; it does now — the name renderer, the collision check and the read-side fold are all
//! on `main` — so this is the verb that performs it.
//!
//! # ⚠ Why an OPERATOR ACT rather than a step inside `secrets migrate`
//!
//! Exactly the argument `vike-cli config adopt` makes about its own seal, and for a sharper reason:
//! this one DELETES rows from the only copy of a box's venue keys. A probe the binary evaluates on
//! its own would fire the instant a release landed, on every box at once, with a redeploy as the
//! only rollback. This shape gives a rehearsal (`--dry-run`), a verdict an operator reads before
//! anything moves, and a refusal that writes nothing.
//!
//! # ⚠ What it REFUSES, and both write nothing
//!
//! * a **collision** — a rendered name a live `credential` row already holds. SQLite cannot express
//!   it: the two tables are one namespace and `credential_one_live_name` holds inside `credential`.
//! * a **divergence** — two names collapsing onto one key while holding DIFFERENT values. The ten
//!   names are nine rows only because the dukascopy pair was MEASURED equal in the live store;
//!   where they differ, one row cannot express both and picking either hands one account the
//!   other's JForex server.
//!
//! # ⚠ It does not tell an operator to delete anything
//!
//! Same rule `config_adopt` states: no message here may name `rm`, and none may name the database
//! FILE as something to remove. Deleting it makes `Backend` answer `Files` for CREDENTIALS on that
//! box — every venue silently on paper, and the only copy of its venue keys gone.

use std::collections::BTreeMap;
use std::path::Path;

use crate::exit::{CliError, CmdResult};

/// The settings key a credential NAME becomes, or `None` for a row that does not move.
///
/// ⚠ The classifier is `vike-bridge-core`'s, and that is the point: it is the SAME derivation the
/// read-side fold runs, so this verb and the fold cannot disagree about which rows these are.
fn moves_to(name: &str) -> Option<String> {
    let class = vike_bridge_core::credentials::classify_credential_name(name);
    if class.pending_move != Some(vike_secrets::PendingMove::VenueSetting) {
        return None;
    }
    // A venue-scoped row carries NO tier — which is exactly the machine-scoped shape the polymarket
    // proxy family takes, and why `venue_setting_key` admits `None`.
    let (venue, tier) = match &class.placement {
        vike_secrets::Placement::Account(key) => (key.venue.clone(), Some(key.tier.clone())),
        vike_secrets::Placement::Venue(v) => (v.clone(), None),
        vike_secrets::Placement::Infrastructure => return None,
    };
    Some(vike_bridge_core::credentials::venue_setting_key(&venue, tier.as_deref(), &class.field))
}

/// `vike-cli secrets move-venue-config`.
///
/// ⚠ Takes the two facts it needs rather than the parser's `Args`, which is private to
/// `crate::cmd::secrets`. The FLAG is parsed there like every other one on that command, so this
/// verb cannot grow a second flag vocabulary.
pub fn run_move(dry_run: bool, settings_dir: Option<&Path>) -> CmdResult<()> {
    let dir = match settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(None),
    };
    let db = vike_secrets::db_path_in(&dir);
    if !db.is_file() {
        return Err(CliError::from(
            "this box has no settings database, so there is nothing to move — the credential FILE \
             is still the store. `vike-cli secrets path` prints where it is, and `vike-cli secrets \
             migrate` is what creates the database."
                .to_string(),
        ));
    }

    // The collision check §6.2 puts on the MIGRATION, evaluated over the keys this move would
    // write against the names the store holds LIVE.
    let live = vike_secrets::read_table(&db, vike_secrets::Table::Credential)
        .map_err(|e| CliError::from(e.to_string()))?
        .into_map();
    let keys: Vec<String> = live.keys().filter_map(|n| moves_to(n)).collect();
    let live_names: std::collections::BTreeSet<String> = live.keys().cloned().collect();
    let collisions: BTreeMap<String, String> =
        vike_bridge_core::credentials::rendered_name_collisions(&keys, &live_names);

    let report = vike_secrets::move_pending_rows(&db, &moves_to, collisions, dry_run)
        .map_err(|e| CliError::from(e.to_string()))?;

    if report.refused() {
        let mut why =
            String::from("REFUSED — NOTHING WAS WRITTEN and this store is exactly as it was.\n");
        for (key, names) in &report.divergent {
            why.push_str(&format!(
                "  ⚠ {key} would be ONE row, but these hold DIFFERENT values: {}\n     one row \
                 cannot express both. Reconcile them first — `vike-cli secrets set` writes \
                 whichever store this box reads — or leave them where they are.\n",
                names.join(", ")
            ));
        }
        for (name, key) in &report.collisions {
            why.push_str(&format!(
                "  ⚠ {name} is rendered by {key} AND held by a live credential row\n     the move \
                 is already half done for that name; the credential value is the one in force.\n"
            ));
        }
        return Err(CliError::from(why));
    }

    if report.keys.is_empty() {
        println!(
            "nothing to move — this box holds no credential row the classifier owes to a settings \
             row. A box that has already moved reads exactly this."
        );
        return Ok(());
    }

    println!(
        "{} — {} credential row(s) become {} settings row(s)",
        if dry_run { "WOULD MOVE (nothing written)" } else { "MOVED" },
        report.names.len(),
        report.keys.len()
    );
    for key in &report.keys {
        println!("  config.{key}");
    }
    println!("  from: {}", report.names.join(", "));
    if dry_run {
        println!("\nRun it without --dry-run to perform the move.");
    } else {
        println!(
            "\n⚠ A RUNNING daemon still holds what it BOOTED with — restart it for this to take \
             effect. Every reader keeps finding the legacy name either way: the credential map \
             folds these rows back in.\n`vike-cli config show` now renders them, and \
             `vike-cli config set` writes them."
        );
    }
    Ok(())
}
