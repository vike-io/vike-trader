//! **`vike-cli secrets move-venue-config` — ruling 10's move, as an operator act.**
//!
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §11 step 4 takes ten config-shaped
//! keys out of `credential` and files them as settings rows. §12 forbade it until §6.2's ordering
//! (A) existed — the name renderer, the collision check and the read-side fold — so this is the
//! verb that performs it.
//!
//! ⚠ **Since decision 0095's Task 7 it is the REMEDY a refusal names, not an optional tidy-up.** The
//! read-side fold that kept a credential row under a setting's legacy name working is retired: every
//! venue setting is read from `venue_setting` alone, and a store still holding such a row refuses to
//! start (`vike_config::refuse_stranded_venue_settings`), naming this verb. The set it moves is the
//! set that refusal names — `crates/vike-bridge-core/tests/credential_classification.rs`'s
//! `the_boot_refusal_and_the_move_verb_agree_on_every_name` holds the two equal.
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
//! # ⚠ What it does NOT refuse: a settings row that already exists for a key it moves
//!
//! The collision above is only ever against credential rows that STAY. A name this verb itself moves
//! has none to refuse — every machine-scoped venue setting's legacy name is such a row (the
//! classifier's `classify_machine_setting` marks each `PendingMove::VenueSetting`, derived from the
//! settings catalog). `vike_secrets::move_pending_rows` upserts (`ON CONFLICT DO UPDATE
//! SET value = excluded.value`) and then deletes the credential row, so an existing row holding a
//! DIFFERENT value is overwritten with the credential's, silently, and `--dry-run` lists the key
//! under WOULD MOVE without reading the table it would overwrite. When both homes are populated the
//! move is what makes the credential value the row's; `docs/ops/upgrading.md` step 1 is the check
//! an operator runs first.
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
/// ⚠ The classifier is `vike-bridge-core`'s and the KEY GRAMMAR is `vike-secrets`', and the split
/// is the point rather than an accident of where things sit: the two are INVERSES over one table
/// (`vike_secrets::venue_setting::HAND_MAPPED_ACCOUNTS`), so this verb and the boot refusal
/// (`vike_secrets::venue_setting::stranded_venue_setting_names`) cannot disagree about which rows
/// these are or about what name a row answers for —
/// `crates/vike-bridge-core/tests/credential_classification.rs`'s
/// `the_boot_refusal_and_the_move_verb_agree_on_every_name` holds the two equal. The grammar half
/// moved down on 2026-09-22 so the store's fold (retired by decision 0095's Task 7) could run inside
/// the store; `crates/vike-secrets/src/venue_setting.rs` carries the two blindnesses that forced it.
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
    Some(vike_secrets::venue_setting::venue_setting_key(&venue, tier.as_deref(), &class.field))
}

/// **The collisions this move would hit — evaluated against the rows that STAY, never the whole
/// store.**
///
/// ⚠ **Excluding the moving rows is the whole of it, and without it this refuses EVERY key.** The
/// question is *"does the settings key I am about to write render back onto a name a credential row
/// STILL holds?"*, and [`vike_bridge_core::credentials::rendered_name_collisions`] answers it
/// through `venue_setting_names` — the renderer the retired read-side fold used — so the names it
/// produces for `venue.dukascopy.demo.server` are precisely the `DUKASCOPY_DEMO{1,2}_SERVER` rows
/// being moved.
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

    // ⚠ TWO closures, and they no longer come from the same crate. `moves_to` RENDERS the
    // operator-facing dotted key and still needs `vike_bridge_core`, because the CLASSIFIER is
    // what decides which rows move; `parse_venue_setting_key` reads that key back as the columns
    // `venue_setting` holds, and it MOVED into `vike-secrets` on 2026-09-22 so that the read-side
    // fold could run inside the store rather than above two of its three readers. This call site
    // hands `move_pending_rows` the canonical path rather than a re-export of it: the old home
    // keeps no shim.
    let report = vike_secrets::move_pending_rows(
        &db,
        &moves_to,
        &vike_secrets::venue_setting::parse_venue_setting_key,
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
                "  ⚠ {name} is rendered by {key} AND held by a credential row this move would not \
                 take\n     the setting would then have two homes, so nothing was moved.\n"
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
             effect.\n`vike-cli config show` now renders them, and `vike-cli config set` writes \
             them."
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

    /// Decision 0095: every declared venue field's legacy name moves — the four Polymarket fields
    /// its Task 3 declared like the egress five, the `VIKE_` toggles Task 4 turned into fields, and
    /// a LABELLED gateway row onto its TIER's setting. Since Task 7 a credential row under one of
    /// these names refuses the daemon's start, and this verb is the remedy that refusal names.
    ///
    /// ⚠ Composed rather than spelled, for the reason `a_store_like_prod2s` gives.
    #[test]
    fn every_declared_fields_legacy_name_moves() {
        for (name, key) in [
            (concat!("POLY", "_RATE_GATE"), "venue.polymarket.rate_gate"),
            (concat!("POLY", "_EXEC_MARKETS"), "venue.polymarket.exec_markets"),
            (concat!("POLY", "_PRESUBMIT_REGISTER"), "venue.polymarket.presubmit_register"),
            (concat!("POLY", "_WS_TOKENS_PER_SOCKET"), "venue.polymarket.ws_tokens_per_socket"),
            (concat!("BINANCE", "_TRADE_LITE_FILL"), "venue.binance.trade_lite_fill"),
            (concat!("BYBIT", "_FAST_EXEC"), "venue.bybit.fast_exec"),
            (concat!("OKX", "_MARK_STREAMS"), "venue.okx.mark_streams"),
            (concat!("FXCM", "_MAINNET_URL"), "venue.fxcm.live.url"),
            (concat!("IBKR", "_DEMO_HOST__HEDGE"), "venue.ibkr.demo.host"),
        ] {
            assert_eq!(moves_to(name).as_deref(), Some(key), "{name}");
        }
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
