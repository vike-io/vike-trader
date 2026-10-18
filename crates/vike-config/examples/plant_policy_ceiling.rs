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
//! the body of `vike_secrets::edit_account_in` with `AccountEdit::Create` (`vike-cli secrets account
//! add`) — so the store this plants is byte-for-byte the shape a real box gets. It calls that body
//! as `vike_secrets::edit_account_in_unjournalled` (this crate's `test-support` dev edge): the
//! account write is the operator verb's own, and NO change-journal record is written beside the
//! store, so the directory the release image copies holds the database and nothing else, as it
//! always has.
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

use vike_model::AccountTier;
use vike_secrets::{AccountEdit, DbError, DbErrorKind, create_store, edit_account_in_unjournalled};
use vike_util::cli::Program;

/// The command line, read by `clap` through `vike_util::cli::Program` (decision 0155): three
/// positionals, `-h`/`--help` and `-V`/`--version` on stdout with exit 0, any usage error (too few
/// or too many, an unknown flag) on stderr with exit 2. The venue and the tier are checked against
/// their rosters after the parse, with the messages below.
#[derive(clap::Parser, Debug, PartialEq, Eq)]
#[command(
    name = "plant_policy_ceiling",
    about = "Plant a settings database holding ONE active account row, for a harness that needs a venue mounted above paper"
)]
struct Cli {
    /// The settings directory the store is created in.
    #[arg(value_name = "SETTINGS_DIR")]
    dir: String,
    /// A roster venue.
    #[arg(value_name = "VENUE")]
    venue: String,
    /// The account's tier: paper, demo or live.
    #[arg(value_name = "TIER")]
    tier: String,
}

/// The program's identity for the parse.
fn program() -> Program<'static> {
    Program::new(
        "plant_policy_ceiling",
        concat!("plant_policy_ceiling ", env!("CARGO_PKG_VERSION")),
    )
}

fn main() {
    let Cli { dir, venue, tier } = program().parse();
    if !vike_model::VENUES.contains(&venue.as_str()) {
        eprintln!("plant_policy_ceiling: {venue:?} is not a roster venue");
        exit(2);
    }
    let Ok(mode) = tier.parse::<AccountTier>() else {
        eprintln!(
            "plant_policy_ceiling: {tier:?} is not a tier (one of {})",
            AccountTier::legal_words()
        );
        exit(2);
    };

    let store = create_store(Some(&dir)).unwrap_or_else(|e| fail(&venue, &tier, &e));
    let settings_dir = PathBuf::from(&dir);

    let created = edit_account_in_unjournalled(
        &settings_dir,
        AccountEdit::Create { venue: &venue, tier: mode, label: None },
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
            edit_account_in_unjournalled(
                &settings_dir,
                AccountEdit::SetActive { id: holder, active: true },
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Cli, vike_util::cli::CliExit> {
        program().try_parse_from::<Cli, _, _>(line.iter().copied())
    }

    fn cli(dir: &str, venue: &str, tier: &str) -> Cli {
        Cli { dir: dir.to_string(), venue: venue.to_string(), tier: tier.to_string() }
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled reader and kept through
    /// the `clap` migration): the three positionals `release-image.yml` and
    /// `scripts/release_container_image.sh` pass, `<dir> fxcm demo`.
    #[test]
    fn the_deployed_line_is_accepted_with_its_meaning() {
        assert_eq!(
            parse(&["/tmp/planted", "fxcm", "demo"]).unwrap(),
            cli("/tmp/planted", "fxcm", "demo")
        );
        for tier in ["paper", "demo", "live"] {
            assert!(parse(&["d", "binance", tier]).is_ok(), "{tier}");
        }
    }

    #[test]
    fn fewer_than_three_or_more_than_three_is_a_usage_error() {
        for line in [&[][..], &["d", "fxcm"], &["d", "fxcm", "demo", "extra"]] {
            let exit = parse(line).expect_err("this line must be refused");
            assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
        }
    }

    #[test]
    fn help_and_version_are_a_success_on_stdout() {
        let exit = parse(&["--help"]).expect_err("help is an exit");
        assert_eq!((exit.code, exit.to_stdout), (0, true));
        assert!(exit.text.contains("SETTINGS_DIR"), "{}", exit.text);
        let exit = parse(&["-V"]).expect_err("version is an exit");
        assert_eq!(exit.text, concat!("plant_policy_ceiling ", env!("CARGO_PKG_VERSION"), "\n"));
    }
}
