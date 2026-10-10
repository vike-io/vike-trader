//! **§5.3's gate — no account's effective ceiling comes out of the schema 2→3 migration HIGHER
//! than it went in.**
//!
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` calls this *"the one gate
//! without which this must not ship"*. Stage 3 folds the `venue_arming` table into a new
//! `account.armed` column, and the fold is the one place in the whole plane where a mistake ARMS
//! SOMETHING NOBODY ARMED — on boxes that hold real venue credentials.
//!
//! # Why the gate lives HERE, in `vike-config`'s tests — the layer ruling
//!
//! The reshape lives in `vike-secrets` (`[package.metadata.vike] layer = 15`). The two things this
//! gate must compare against live in `vike-config` (layer 20): the ORDERING (`VenueMode`'s `Ord`,
//! and `VenueMode::cap`, the named `min` this tree insists on over a bare `.min()`) and the
//! INHERITANCE RULE (`crates/vike-config/src/venue_mode.rs`'s `VenuePolicy::account`). A gate
//! inside `vike-secrets` could not name either, and would have to RE-IMPLEMENT
//! `paper < demo < live` plus the default-inherits / labelled-does-not asymmetry — a second
//! predicate that must agree with the first, which is the defect shape
//! `crates/vike-mount/src/arming.rs`'s `venue_ceiling` was collapsed into one function to escape.
//!
//! `vike-config` already declares `vike-secrets` as a NORMAL dependency (for the boot disclosure),
//! so the edge this gate needs already exists and points the only way it can. Nothing is added to
//! any manifest.
//!
//! ⚠ `crates/vike-config/Cargo.toml` says this crate *"never opens the store, and must not"*. That
//! is a rule about `src/`, and it is not bent here: this is a test target, and
//! `crates/vike-config/tests/mirror.rs` already builds real stores through `vike_secrets::create_store`
//! for the same reason.
//!
//! # What is compared, exactly
//!
//! * **old** — `VenuePolicy::account(venue, label)`, CAPPED by what the account's own credential
//!   set can reach (`VenueMode::cap` against its `tier`). That is the old model's answer for one
//!   account row: the venue's line, folded with the account's own line by the inheritance rule,
//!   and then bounded by the fact that a demo credential set cannot trade live.
//! * **new** — §3.2's `armed ? tier : paper`.
//!
//! ⚠ **`VenuePolicy::get` is NOT used and must not be.** It agrees with `VenuePolicy::account` for
//! the DEFAULT account and differs for every LABELLED one, so a gate written on it cannot see a
//! labelled widening at all. [`the_ruled_fold_widens_a_labelled_account_with_no_line_of_its_own`]
//! proves that by running both.
//!
//! ⚠ The cap is what makes this STRICTLY STRONGER than the spec's literal wording. The spec says
//! `new <= VenuePolicy::account(...)`; capping lowers the RIGHT-hand side of that `<=` — the
//! thing `new` must stay under — so every comparison here is at least as demanding as the one
//! §5.3 asks for, and one of them, the hyperliquid demo row, is genuinely more so.
//!
//! ⚠ **The old ceiling is resolved from the row that WENT IN**, by `account.id`, never from the
//! row the reshape handed back. [`went_in`] carries what recomputing it from the after row cost.
//!
//! # What this gate's SUBJECT is
//!
//! `account.armed` exists (`crates/vike-secrets/src/schema/ddl.rs`'s `DDL`), the fold is
//! `crates/vike-secrets/src/settings/arming.rs`'s `fold_arming_into_accounts`, and [`SUBJECT`] is
//! not a model of it: it READS the column back out of the migrated store, so **every assertion
//! below judges the shipped code**.
//!
//! The three arms, as `fold_arming_into_accounts`' `account_ceiling` spells them and as
//! `VenuePolicy::account` spells them — the equality this whole file exists to hold:
//!
//! 1. an UNLABELLED account takes the venue arming row (absent ⇒ `paper`);
//! 2. a LABELLED account with a labelled row takes `min(venue row, labelled row)`;
//! 3. a LABELLED account with NO labelled row is armed only where its own tier is already `paper`.
//!
//! ⚠ That third arm is NOT "never armed": a `paper`-tier labelled account resolves to a `Paper`
//! ceiling, so the ceiling EQUALS the tier and it ARMS. The effective behaviour is `paper` either
//! way, so no widening is reachable — but the `armed` COLUMN is written from this sentence, and the
//! two spellings produce a different BIT.
//!
//! Three rejected spellings stay beside it as kill proofs: [`STRUCK`] (§5.2 step 5 as SIGNED),
//! [`RULED`] (§5.2 step 5 as AMENDED, which reads the venue arming row only), and [`UNCAPPED`]
//! (the amended PROSE read literally, which forgets the venue cap). Each widens a legal store, and
//! each has a test proving this gate refuses it — so the shipped fold agreeing is demonstrably the
//! RULE working rather than an accident of the fixtures.
//!
//! ⚠ **`vike-secrets` (layer 15) still cannot call `VenuePolicy::account` (layer 20), so the three
//! arms exist TWICE and this gate is the only thing holding them equal.** That is a declared
//! residual rather than an oversight — it is a gate, not a compiler, and it holds them equal over
//! the four fixture shapes below and nowhere else.
//!
//! ⚠ **What the shipped fold does NOT do, so this file is not read as proving more than it does:**
//! `venue_arming` is not dropped, and `armed` has no consumer on the mount path yet — the ceiling
//! `vike_mount::make_engine` reads is still `VenuePolicy`, built from those same rows. So
//! `new_effective` below is what the column MEANS (§3.2's `armed ? tier : paper`), not yet what
//! the box does. `fold_arming_into_accounts`' own doc carries why the table stays.

use vike_secrets::{AccountKey, Classification, Placement};

#[path = "no_ceiling_widens_across_the_migration/fixtures.rs"]
mod fixtures;
#[path = "no_ceiling_widens_across_the_migration/judge.rs"]
mod judge;
#[path = "no_ceiling_widens_across_the_migration/ruled_fold_and_store.rs"]
mod ruled_fold_and_store;
#[path = "no_ceiling_widens_across_the_migration/widening_cases.rs"]
mod widening_cases;

/// **The classifier**, spelled here because `vike-config` cannot name the crate that owns the
/// production one (`vike_bridge_core::credentials`' `classify_credential_name`, layer 25) — the
/// same seam, and the same reason, that `crates/vike-secrets/tests/support/mod.rs` spells its own.
///
/// It is not a hand-written map: it is the production classifier's own two tables, reached from
/// the two crates this one CAN name — `crates/vike-secrets/src/venue_setting.rs`'s
/// `HAND_MAPPED_ACCOUNTS` (whose rows are the non-conforming families) and
/// `crates/vike-model/src/accounts/account_keys.rs`'s `account_ref_from_key` (the venue grammar, including
/// the `__LABEL` suffix). A name neither answers for is reported unrecognised, exactly as
/// production does.
fn classify(name: &str) -> Classification {
    let account = |key: AccountKey, field: &str| Classification {
        placement: Placement::Account(key),
        field: field.to_string(),
        secret: true,
        recognised: true,
    };

    for &(head, token, venue, tier, discriminator, _why) in
        vike_secrets::venue_setting::HAND_MAPPED_ACCOUNTS
    {
        let prefix = vike_secrets::venue_setting::hand_mapped_prefix(head, token);
        if let Some(field) = name.strip_prefix(&prefix) {
            return account(
                AccountKey {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    label: None,
                    discriminator: discriminator.map(str::to_string),
                },
                field,
            );
        }
    }

    if let Some(reference) = vike_model::accounts::account_keys::account_ref_from_key(name) {
        let head = format!("{}_{}_", reference.venue.to_uppercase(), reference.tier);
        let base = name
            .split(vike_model::accounts::account_keys::ACCOUNT_SEPARATOR)
            .next()
            .unwrap_or(name);
        let field = base.strip_prefix(&head).unwrap_or(base);
        return account(
            AccountKey {
                venue: reference.venue.to_string(),
                tier: reference.tier.to_ascii_lowercase(),
                label: reference.label.text().map(str::to_string),
                discriminator: None,
            },
            field,
        );
    }

    Classification::unrecognised(name)
}
