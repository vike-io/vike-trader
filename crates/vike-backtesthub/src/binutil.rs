//! `binutil` — the history route the bins dial, plus the run-level provenance stamp the bins' JSON
//! carries ([`stats_provenance`]) — one home, so two bins publishing the same statistic cannot end up
//! describing it two ways, which is the defect the stamp itself exists to close.
//!
//! Feature-free on purpose (like `objective`): the module itself compiles in every build.
//!
//! **The bins read their argv with `clap`** (decision 0155), each through its own `#[derive(Parser)]`
//! struct and `vike_utility::cli::Program`; what they share lives here: [`bin_version_line`] and
//! [`CommaList`], the comma-separated flag value. The hand-rolled scanners
//! (`vike_utility::argv::{arg, f64_arg, has_flag, parse_num}`) they used to share are no longer named
//! by any bin.

use std::collections::HashMap;

/// The sentence [`stats_provenance`] carries about its own numbers, and the ONE place the ABSENCE
/// rule is written down for somebody holding the FILE rather than this source.
///
/// ⚠ **A stamp can only ever be added going forward, so the scheme has to work by absence** — the
/// JSON files an operator already has on disk cannot be retro-stamped, and there is no version they
/// could be said to carry. So the rule a reader applies is: *a document with a `stats_provenance`
/// object states its convention; a document with none predates the naming and is nearest-rank.*
/// That rule is useless if it lives only in the code, because the person comparing two files is
/// reading files — hence this note ships INSIDE the new one, where it is legible beside the very
/// numbers it disqualifies the old file from being compared against. Same device as
/// `crates/vike-studio-core/src/study_run.rs`'s `STUDY_METRICS_NOTE`, which rides in a study run's
/// own `detail` for the same reason: *"a manifest is also read ALONE"*.
///
/// ASCII-only and one paragraph on purpose (the constraint
/// `crates/vike-buildinfo/src/lib.rs`'s `summary` states for its own line): this lands in pasted
/// terminal output, an issue body and a chat window, and every one of those is grepped.
pub const PERCENTILE_NOTE: &str = "Percentiles in this document come from \
     vike_math::statistics::percentile - numpy linear interpolation (method=linear) between the \
     two closest ranks. A JSON document from these bins that carries NO stats_provenance object at \
     all was produced BEFORE the convention was named, and its percentiles are nearest-rank: the \
     observed sample at round((n - 1) * q), never an interpolant. The two disagree in BOTH \
     directions on one sample, so no scale factor converts an old file into a new one - re-run the \
     bin rather than comparing across the gap.";

/// The RUN-LEVEL provenance block a JSON-emitting bin in this family stamps on its summary object,
/// so that two files can be told apart by what produced them rather than by when somebody remembers
/// having run them:
///
/// ```text
/// "stats_provenance": { "percentile_method": <PERCENTILE_METHOD>, "note": <PERCENTILE_NOTE> }
/// ```
///
/// (Spelled as the two constants rather than as their values: a doc example carrying the literal
/// would be a third hand copy of the very fact this block exists to keep honest.)
///
/// # Where it goes, and where it must not
///
/// **On the ONE run-level summary object each bin prints, never on a row.** These bins emit
/// per-entry rows — the per-size curve arrays, the per-anchor lanes, and the `--out` CSV beside
/// them; a marker repeated per row is bytes spent restating a fact that is constant for the whole
/// document. The nesting argument is `crates/vike-studio-core/src/study_run.rs`'s, inverted:
/// metrics NEST there because a reader must be made to ask which scorer produced them, and this
/// block sits at the TOP because a reader must not have to go looking for the answer.
///
/// The bins' human-readable lane prints the same percentiles and so names the same convention on
/// its header line, rendered from [`vike_math::statistics::PERCENTILE_METHOD`] rather than from
/// this block: terminal output gets pasted into an issue, where it has the JSON's problem and none
/// of its structure. It gets the NAME and not the note — a paragraph on every run is noise, and
/// the reader of a pasted terminal line is reading it beside the person who ran it.
///
/// # Why a method NAME and no schema version
///
/// The name is [`vike_math::statistics::PERCENTILE_METHOD`], which lives beside the algorithm it
/// names — that constant's doc carries the argument for a name over a number.
///
/// No schema version rides beside it, and the omission is a decision rather than an oversight. This
/// tree does version a payload where a DECODER has to know how to read bytes it did not write —
/// `crates/vike-store/src/datafusion_hist/codec/book.rs`'s `BOOK_SCHEMA_VERSION` and its siblings, stamped
/// into Parquet key-value metadata. Nothing decodes THESE documents: the readers are a human and an
/// ad-hoc `jq` filter selecting named keys, both of which tolerate an added key and neither of which
/// can act on an integer. Meanwhile these summaries have grown keys repeatedly with nothing bumped
/// (the third anchor, the live-law lanes, the delta-integrity block), so a version pinned here would
/// have been stale on arrival — and a stale version is worse than no version, because it is a claim
/// somebody will trust. Naming each convention gives the discrimination a version was wanted for,
/// per statistic, and additively: a future `stderr_method` is one more key, and a reader keys on
/// the NAME.
///
/// # What it deliberately does NOT claim
///
/// Only the percentiles. `crates/vike-backtesthub/src/bin/cheap_np_askgate/main.rs`'s `stderr` fold is
/// a separate convention that was deliberately left alone, and a key called `stats_method` would
/// have silently spoken for it. Nor is there a `git_sha`: naming a commit means `vike-buildinfo`,
/// which resolves it at COMPILE time, and this crate does not depend on it —
/// `crates/vike-runs/src/lib.rs`'s `RunManifest` writes `None` for exactly that reason.
pub fn stats_provenance() -> serde_json::Value {
    serde_json::json!({
        "percentile_method": vike_math::statistics::PERCENTILE_METHOD,
        "note": PERCENTILE_NOTE,
    })
}

/// **The history `route` names, opened with this box's device key from the settings database
/// [`vike_secrets::open_credential_store`] locates out of `vars`** ([`backtest_device_key`]) — the
/// one-shot readers' half of `docs/decisions/0084-only-the-datahub-touches-the-store.md`. An
/// UNREADABLE store warns and continues (`vike_datahub_client::route::routed_hub_keys`, which
/// argues that disposition); the `--addr` daemon reads its keys itself, under a stricter one.
///
/// # Errors
/// `vike_secrets::CredentialScopeError` when `$VIKE_NO_LIVE_KEYS` is set to anything but `true` or `false`:
/// the caller refuses to start on it, printing the message as is.
pub fn routed_history(
    route: &vike_datahub_client::route::HistoryRoute,
    vars: &HashMap<String, String>,
) -> Result<Box<dyn vike_data::HistoryStore + Send + Sync>, vike_secrets::CredentialScopeError> {
    let store = vike_secrets::open_credential_store(vars)?;
    let keys =
        vike_datahub_client::route::routed_hub_keys(backtest_device_key(&store), route.hub());
    Ok(vike_datahub_client::route::open_routed_history(route, keys))
}

/// The device a backtest bin dials the datahub as (decision 0288 item 8): the backtesthub's own `server/backtesthub`
/// (`vike_hub_protocol::access::BACKTESTHUB_DEVICE`) where this box holds it (a server, where `backend mint-missing`
/// mints it with the role `observe`), else this box's first key (a PC's own). No shared datahub pair is read.
///
/// # Errors
/// A settings database that exists and cannot be read.
pub fn backtest_device_key(
    store: &vike_secrets::CredentialStore,
) -> Result<Option<vike_secrets::access::DeviceKeyRow>, vike_secrets::StoreError> {
    let Some(location) = store.location() else { return Ok(None) };
    let dir = location.settings_dir();
    let own = vike_hub_protocol::access::BACKTESTHUB_DEVICE;
    match vike_secrets::access::device_key_named(dir, own.person, own.device)? {
        Some(row) => Ok(Some(row)),
        None => vike_secrets::access::device_key_load(dir),
    }
}

/// The `--version` line of a bin of this crate: `<bin> <package version>`, the shape every other
/// `--version` on the box prints. (No build identity: this crate cannot name `vike-buildinfo`,
/// `crates/vike-buildinfo/tests/identity_adoption.rs`'s `WITHOUT_IDENTITY` says why.) Each bin builds
/// its own `vike_utility::cli::Program` from it, because only a binary reads the process's argv.
pub fn bin_version_line(bin: &str) -> String {
    format!("{bin} {}", env!("CARGO_PKG_VERSION"))
}

/// A comma-separated list flag (`--live-ms 954,4229`, `--sizes 10,50,100`) as ONE `clap` value.
///
/// A newtype rather than `Vec<T>` so that `clap`'s derive reads the flag as a single value that is
/// split here, instead of as a repeatable flag: `--sizes 10,50` and `--sizes=10,50` are the
/// spellings, and a repeated `--sizes` is refused like every repeated flag. Parts are trimmed, an
/// EMPTY part is skipped (`1,,2`, a trailing comma — what the hand-rolled splitters did), and a
/// part that is not a `T` is a refusal naming it. (They used to be dropped by `filter_map` in
/// `--sizes`/`--delays-ms` and refused by hand in `--live-ms`/`--latency-ms`: one rule now.)
#[derive(Clone, Debug, PartialEq)]
pub struct CommaList<T>(pub Vec<T>);

impl<T: std::str::FromStr> std::str::FromStr for CommaList<T> {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, String> {
        raw.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<T>().map_err(|_| format!("bad list entry {part:?}")))
            .collect::<Result<Vec<T>, String>>()
            .map(CommaList)
    }
}

#[path = "binutil_tests.rs"]
#[cfg(test)]
mod binutil_tests;
