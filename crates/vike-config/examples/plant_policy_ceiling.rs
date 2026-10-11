//! CI/build-box-only helper: plant a settings database holding ONE active, unlabelled `account`
//! row — `(<venue>, <tier>)` — for a harness that needs a venue mounted above `paper` but has no
//! store of its own.
//!
//! # Why this exists
//!
//! An account trades at its own `account.tier` while its row is `active`; no row, an inactive row
//! or a `paper` row mounts PAPER. A fresh box has no database at all, so a harness that needs one
//! venue past `paper` must create the store and then the row. Both steps are the production
//! writers an operator runs — `vike_secrets::create_store` (`vike-cli secrets init`) and
//! `vike_secrets::edit_account_in` with `AccountEdit::Create` (`vike-cli secrets account add`) — so
//! the store this plants is byte-for-byte the shape a real box gets, and no fixture-only writer is
//! involved.
//!
//! The FXCM release-image smoke is the caller: `scripts/release_container_image.sh`'s
//! `fxcm_runtime_smoke` asks the image's own `vike-backend venues --venue fxcm --json` whether the
//! FXCM runtime loads, and a `paper` account answers before the bridge ever consults the loader. So
//! `.github/workflows/release-image.yml` runs this as the unprivileged runner, before the privileged
//! build, into the directory that script copies. It plants no credential: `NoCredentials` is the
//! answer that proves the SDK conjunct passed. Every OTHER venue has no row and mounts paper.
//!
//! Compiled from the SAME source tree the release binaries come from, so the schema this plants can
//! never drift from what those binaries read. The name says "policy ceiling" for history only: the
//! workflow, the script and their gate spell it, and the argv is unchanged.
//!
//! # Usage
//!
//! ```text
//! cargo run -p vike-config --example plant_policy_ceiling -- <settings-dir> <venue> <tier>
//! ```
//!
//! `<tier>` is one of `paper`, `demo`, `live`. Idempotent: a store that already exists is kept, and
//! an unlabelled row already at `(<venue>, <tier>)` is re-activated rather than duplicated. It does
//! not deactivate another tier's row of the same venue; on a fresh directory there is none.

use std::path::PathBuf;
use std::process::exit;

use vike_secrets::{
    ACCOUNT_TIERS, AccountEdit, DbError, DbErrorKind, create_store, edit_account_in,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(venue), Some(tier), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        eprintln!("usage: plant_policy_ceiling <settings-dir> <venue> <paper|demo|live>");
        exit(2);
    };
    if !vike_model::VENUES.contains(&venue.as_str()) {
        eprintln!("plant_policy_ceiling: {venue:?} is not a roster venue");
        exit(2);
    }
    if !ACCOUNT_TIERS.contains(&tier.as_str()) {
        eprintln!("plant_policy_ceiling: {tier:?} is not a tier (one of {ACCOUNT_TIERS:?})");
        exit(2);
    }

    let store = create_store(Some(&dir)).unwrap_or_else(|e| fail(&venue, &tier, &e));
    let settings_dir = PathBuf::from(&dir);

    let created = edit_account_in(
        &settings_dir,
        AccountEdit::Create { venue: &venue, tier: &tier, label: None },
    );
    let id = match created {
        Ok(write) => {
            let Some(row) = write.after else {
                eprintln!("plant_policy_ceiling: the create returned no row");
                exit(1);
            };
            row.id
        }
        // The row is already there: make it the active one rather than a second unlabelled row.
        Err(DbError {
            kind:
                DbErrorKind::AmbiguousUnlabelledAccount { holder, .. }
                | DbErrorKind::AccountLabelTaken { holder, .. },
            ..
        }) => {
            edit_account_in(&settings_dir, AccountEdit::SetActive { id: holder, active: true })
                .unwrap_or_else(|e| fail(&venue, &tier, &e));
            holder
        }
        Err(e) => fail(&venue, &tier, &e),
    };

    println!("planted account {id} ({venue}, {tier}), active, in {}", store.db.display());
}

fn fail(venue: &str, tier: &str, e: &DbError) -> ! {
    eprintln!("plant_policy_ceiling: cannot plant the ({venue}, {tier}) account: {e}");
    exit(1);
}
