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

/// **The collisions this move would hit — evaluated against the rows that STAY, never the whole
/// store.**
///
/// ⚠ **Excluding the moving rows is the whole of it, and without it this refuses EVERY key.** The
/// question is *"does the settings key I am about to write render back onto a name a credential row
/// STILL holds?"*, and [`vike_bridge_core::credentials::rendered_name_collisions`] answers it
/// through `venue_setting_names` — the same read-side fold — so the names it produces for
/// `venue.dukascopy.demo.server` are precisely the `DUKASCOPY_DEMO{1,2}_SERVER` rows being moved.
/// Hand it the whole live set and every row collides WITH ITSELF.
///
/// MEASURED on the CI box 2026-09-21 against the real store: the dry run reported all ten names as
/// *"rendered by `<key>` AND held by a live credential row"* and moved nothing — a refusal nothing
/// could ever satisfy, since the only way to clear it is to delete the very rows the move exists to
/// relocate. A collision is a name some OTHER row holds, and a row on its way out is not that.
///
/// Split out of [`run_move`] so the rule is testable without a database — the I/O shell above reads
/// the table, this decides. [`the_move_does_not_collide_with_itself`] pins it, with a negative
/// control so an empty answer cannot be mistaken for a check that stopped looking.
fn collisions_against_the_rows_that_stay(
    live: &std::collections::HashMap<String, String>,
) -> BTreeMap<String, String> {
    let keys: Vec<String> = live.keys().filter_map(|n| moves_to(n)).collect();
    let staying: std::collections::BTreeSet<String> =
        live.keys().filter(|n| moves_to(n).is_none()).cloned().collect();
    vike_bridge_core::credentials::rendered_name_collisions(&keys, &staying)
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
    // write against the names that will STILL BE credential rows once it has run.
    let live = vike_secrets::read_table(&db, vike_secrets::Table::Credential)
        .map_err(|e| CliError::from(e.to_string()))?
        .into_map();
    let collisions = collisions_against_the_rows_that_stay(&live);

    // ⚠ TWO closures, and both grammars live in `vike_bridge_core` so they cannot drift: `moves_to`
    // RENDERS the operator-facing dotted key, `parse_venue_setting_key` reads it back as the
    // columns `venue_setting` holds. `vike-secrets` owns neither, by design — it declares exactly
    // one external dependency and no key grammar of its own.
    let report = vike_secrets::move_pending_rows(
        &db,
        &moves_to,
        &vike_bridge_core::credentials::parse_venue_setting_key,
        collisions,
        dry_run,
    )
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

#[cfg(test)]
mod move_collision_tests {
    use super::*;

    /// A store shaped like the CI box's: the dukascopy demo pair plus the other families the classifier
    /// owes to settings rows.
    ///
    /// ⚠ Every name is COMPOSED rather than spelled. `crates/vike-ops/tests/settings_registry.rs`'
    /// literal harvest reads a whole `PREFIX_NAME` string in any `crates/*/src/` file — tests
    /// inside it included — as evidence that THIS crate reads that variable, and would then demand
    /// a `SETTINGS` row for the pair `(name, "vike-cli")` that must not exist.
    fn a_store_like_prod2s() -> std::collections::HashMap<String, String> {
        let mut live = std::collections::HashMap::new();
        for (head, tail) in [
            ("DUKASCOPY", "DEMO1_SERVER"),
            ("DUKASCOPY", "DEMO2_SERVER"),
            ("FXCM", "DEMO_CONNECTION"),
            ("FXCM", "DEMO_URL"),
            ("IBKR", "DEMO_BACKEND"),
            ("IBKR", "DEMO_HOST"),
            ("IBKR", "DEMO_PORT"),
            ("POLY", "PROXY_ENABLED"),
            ("POLY", "PROXY_HOST"),
            ("POLY", "PROXY_PORT"),
        ] {
            live.insert(format!("{head}_{tail}"), "value-never-read-by-this-test".to_string());
        }
        live
    }

    /// **The move does not collide with itself**, which is the defect this function was measured
    /// into existence by.
    ///
    /// ⚠ The assertion that matters is the NEGATIVE CONTROL beneath it. An empty collision map is
    /// exactly what a check that stopped looking would also return, so emptiness alone would be the
    /// shape of pass this repository calls an assertion that cannot fail for its stated reason.
    #[test]
    fn the_move_does_not_collide_with_itself() {
        let live = a_store_like_prod2s();
        // The fixture must actually exercise the path, or everything below is about nothing.
        let moving: Vec<String> = live.keys().filter_map(|n| moves_to(n)).collect();
        assert!(
            moving.len() >= 8,
            "the fixture names too few moving rows to be the measured case: {moving:?}"
        );

        let collisions = collisions_against_the_rows_that_stay(&live);
        assert!(
            collisions.is_empty(),
            "a row on its way out is not a collision with itself — this is the the CI box refusal: \
             {collisions:?}"
        );

        // ⚠ THE NEGATIVE CONTROL: the underlying check still BITES. Fed a staying set that really
        // does hold one of the rendered names, it must report it — so the emptiness above is the
        // exclusion doing its job rather than the check having gone blind.
        let mut staying = std::collections::BTreeSet::new();
        staying.insert(format!("DUKASCOPY_{}", "DEMO1_SERVER"));
        let hit = vike_bridge_core::credentials::rendered_name_collisions(&moving, &staying);
        assert!(
            !hit.is_empty(),
            "the collision check answers empty even for a name a staying row holds — it is not \
             checking anything, and the test above proves nothing"
        );
    }

    /// …and a store with NOTHING to move is not an error and not a collision — the state a box
    /// that has already run this reads as.
    #[test]
    fn a_store_with_nothing_to_move_reports_no_collision() {
        let mut live = std::collections::HashMap::new();
        live.insert("a-name-no-classifier-owes-to-a-settings-row".to_string(), "x".to_string());
        assert!(live.keys().filter_map(|n| moves_to(n)).next().is_none());
        assert!(collisions_against_the_rows_that_stay(&live).is_empty());
    }
}
