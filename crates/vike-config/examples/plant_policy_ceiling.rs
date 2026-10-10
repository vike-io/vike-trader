//! CI/build-box-only helper: plant a FRESH, otherwise-empty settings database holding exactly ONE
//! arming row — `policy.venues.<venue> = <mode>` — for a harness that needs a real settings database
//! but has none.
//!
//! # Why this exists
//!
//! Decision 0086 forbids every binary from reading a settings FILE (`policy.toml` included), so a
//! harness that used to drop a `policy.toml` beside a mounted image now has no way to arm a ceiling
//! above `paper` — every production settings writer refuses to create a database that does not
//! already exist, and the ONE that would (`vike_secrets::write_setting_row_in`, behind `vike-cli
//! config set`) ALSO refuses the very first arming row on an otherwise-empty roster
//! (`RowWriteError::ArmingRosterEmpty`): "a single-venue arming write would leave a partial roster,
//! which this writer refuses." That refusal exists to stop an OPERATOR from reaching a state the old
//! file-mirror could never produce — it does not apply to a fixture that wants exactly that state on
//! purpose, which is what `vike_secrets::plant_settings_rows` is for (its own doc: *"a fixture is
//! allowed to start from a state `write_setting_row_in` itself could never reach in one call"*).
//!
//! This is exactly the origin of the FXCM release-image smoke's bug: it used to write a
//! `settings/policy.toml` with `fxcm = "demo"`, which decision 0086 made permanently inert, so the
//! image's own `venues --venue fxcm` read the DEFAULT ceiling (`paper`, absent-row semantics apply
//! per venue) instead — and a `paper` ceiling refuses the venue before the SDK-availability check
//! the smoke exists to prove ever runs. `scripts/release_container_image.sh`'s `fxcm_runtime_smoke`
//! calls this in place of that file. Every OTHER roster venue is left with no row at all, which reads
//! as `paper` by the same per-venue absent-row default a real fresh box gets.
//!
//! `vike_secrets::plant_settings_rows` and the `StoredSettings`/`ArmingRow` types it takes are the
//! ONE thing in this tree that plants exactly this — gated `test-support` specifically because
//! production code must never reach for it. This example exists so a CI *script* (no `cargo test`
//! harness of its own) can still reach it, compiled from the SAME source tree the release binaries
//! come from, so the schema this plants can never drift from what those binaries actually read.
//!
//! # Usage
//!
//! ```text
//! cargo run -p vike-config --example plant_policy_ceiling -- <settings-dir> <venue> <mode>
//! ```
//!
//! `<settings-dir>` must not already hold a database — `plant_settings_rows` creates one when none
//! exists and REPLACES the whole settings/arming/venue-setting table set otherwise, so calling this
//! twice against the same directory silently overwrites rather than accumulating.

use std::path::PathBuf;

use vike_secrets::{ArmingRow, StoredSettings};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(venue), Some(mode), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        eprintln!("usage: plant_policy_ceiling <settings-dir> <venue> <mode>");
        std::process::exit(2);
    };

    let dir = PathBuf::from(dir);
    let rows = StoredSettings {
        arming: vec![ArmingRow {
            venue: venue.clone(),
            label: None,
            mode: mode.clone(),
            max_exposure: None,
        }],
        ..StoredSettings::default()
    };
    vike_secrets::plant_settings_rows(&dir, &rows).unwrap_or_else(|e| {
        panic!("plant policy.venues.{venue} = {mode} in {}: {e}", dir.display())
    });

    println!(
        "planted policy.venues.{venue} = {mode} in {}",
        vike_secrets::db_path_in(&dir).display()
    );
}
