//! `vike-docs` — the `docs-data` exporter: the machine-readable export of the CI-gated per-venue
//! capability tables.
//!
//! The website/docs consume the JSON files attached to every GitHub Release as `docs-data` assets
//! (`.github/workflows/release.yml`'s "Render the docs-data assets" step, and the
//! `src/bin/docs_data.rs` bin that writes them). This crate renders FIVE of them: `venues.json` —
//! one record per [`vike_model::VENUES`] entry, in roster order, rendered from the same registries
//! the runtime consults; `stats.json` (roster sizes, the latency-gate budget, generator identity);
//! `indicators.json` — every built-in indicator and pair indicator, by [`vike_indicators::Category`];
//! `templates.json` — the shipped strategy files' rosters with each portable file's parameter
//! surface and live-mount verdict, read from each file's header; and `rosters.json` — the five
//! remaining CI-gated rosters (`vike_model::events::Event::TABLE`, the order kinds, the asset
//! classes, the hist store's `kind=` layouts, and the reconcile divergence kinds with each
//! policy's COMPUTED verdict). The sixth asset, `bins.json`, is NOT rendered here: it is derived
//! from `cargo metadata`, so it belongs to `xtask` (`xtask/src/docs_bins.rs`), the one crate that
//! already reads that document — a renderer over compile-time tables cannot see a manifest.
//!
//! It carries no feature because nothing depends on it: its one consumer is the release step that
//! runs the bin. Its rank, and why, is the manifest's `[package.metadata.vike]` comment.
//!
//! Everything here is a RENDERER over tables that already exist; this module declares no capability
//! fact of its own: every table it renders is read from a crate below it, and the strategy
//! rosters from the headers of the shipped strategy files, whose folder the CALLER names
//! ([`shipped_strategies`]): a library resolves no path the compiler baked in. The `wiring` class
//! it once declared itself (`WIRING`) is `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` now,
//! the table the trading daemon mounts from.
//!
//! Each module's `//!` names the authority every field it renders is read FROM.
//!
//! # Output contract
//!
//! Enum-ish values render kebab-case; canonical order-kind strings (`"stop_limit"`) stay verbatim
//! — they are domain vocabulary ([`vike_model::OrderType::as_str`]), not renderer inventions.
//! A `max_batch` of `usize::MAX` ("the adapter imposes no cap of its own") renders `null`. The
//! venues ARRAY is in roster order and one build's render is fully deterministic (pinned by the
//! gate's `rendered_files_are_the_five_assets_and_deterministic`). ⚠ JSON object KEY order is NOT
//! part of the contract: it is an artifact of serde_json's map flavor, and the workspace's
//! DataFusion consumers (arrow-json / datafusion-physical-plan) flipped `preserve_order` ON
//! through resolver-2 feature unification, up to datafusion 55.1.0, whenever they shared a build
//! with this crate — a roster-lane build rendered insertion-order keys while a standalone `cargo
//! run -p vike-docs` (the release step's shape) rendered alphabetical. Since 55.2.0 nothing in
//! `Cargo.lock` enables it and both render alphabetical, but a future dependency could turn it on
//! again. Consumers read objects as unordered maps, and the gate
//! asserts key SETS, never order. An unclassified roster venue PANICS
//! the renderer rather than exporting a guess — deny loudly, never silently substitute — and the
//! completeness test makes that panic unreachable from a green tree.

mod indicators;
mod rosters;
mod stats;
mod templates;
mod venues;

pub use indicators::indicators_value;
pub use rosters::rosters_value;
pub use stats::{SCHEMA_VERSION, stats_value};
pub use templates::{template_record, templates_value};
pub use venues::{venue_record, venues_value};

use std::path::Path;

use serde_json::Value;
use vike_strategy::file::StrategyFileHeader;
use vike_strategy::file::scan::scan;

/// What `stats.json` reports as `generated_from` when the caller names no commit: a build that
/// is not a release render.
///
/// The stamp itself is a RUNTIME ARGUMENT ([`stats_value`]'s parameter, fed by the `docs_data`
/// bin's optional second argv slot through [`generated_from`]) and NOT a compile-time
/// `option_env!("GITHUB_SHA")` bake, because that bake published a SHA it could not vouch for on
/// either of the release workflow's two triggers:
///
/// * Under `workflow_dispatch` — `.github/workflows/release.yml`'s re-release hatch, and the ONLY
///   way to re-run a failed tag run, since a tag-push run executes the workflow file at the tag —
///   `GITHUB_SHA` is the SHA of the ref the run was dispatched ON (main's head), while the checkout
///   step puts the TAG on disk (`ref:` names `refs/tags/<inputs.tag>`; the workflow's separate
///   tag-resolution step exists precisely because the two disagree). The render would describe the
///   tag while the stamp named main.
/// * On the tag-push path the bake is not even reliably that run's own value: the shared CI setup
///   action installs sccache as `RUSTC_WRAPPER`, and sccache's cache key does not hash
///   `GITHUB_SHA`, so a cached rustc result carries whichever run first compiled this constant.
///
/// The workflow passes `git rev-parse HEAD` instead — the commit actually checked out, identical
/// under both triggers, and immune to the cache hole because nothing about it is compiled in.
///
/// A parameter, not an env read: libraries take configuration as parameters (root `CLAUDE.md`,
/// "Settings & configuration").
pub const DEFAULT_GENERATED_FROM: &str = "dev";

/// Resolve the `generated_from` stamp from the `docs_data` bin's optional `GENERATED_FROM`
/// argument: absent means [`DEFAULT_GENERATED_FROM`], present means exactly what was passed.
///
/// A present-but-BLANK argument is an `Err` rather than a silent fall back to the default, and the
/// distinction is the whole reason this is a function instead of an `unwrap_or`. The release
/// workflow spells the argument as a command substitution over `git rev-parse HEAD`; a
/// substitution that fails yields the EMPTY STRING and the surrounding command still runs, so a
/// silent default would publish a release-rendered `stats.json` claiming a dev build — the same
/// class of confident-wrong stamp this argument replaced. Blank is a usage error the bin reports
/// with a non-zero exit; the workflow step then fails loudly instead of shipping a lie.
///
/// # Errors
/// The argument is present and contains only whitespace.
pub fn generated_from(arg: Option<&str>) -> Result<&str, &'static str> {
    match arg {
        None => Ok(DEFAULT_GENERATED_FROM),
        Some(v) if v.trim().is_empty() => {
            Err("GENERATED_FROM is blank — pass the rendering commit (the release workflow passes \
                 `git rev-parse HEAD`) or omit the argument entirely")
        }
        Some(v) => Ok(v),
    }
}

/// The `venues.json` asset name — one place, shared by the bin and the gate test.
pub const VENUES_JSON: &str = "venues.json";
/// The `stats.json` asset name.
pub const STATS_JSON: &str = "stats.json";
/// The `indicators.json` asset name.
pub const INDICATORS_JSON: &str = "indicators.json";
/// The `templates.json` asset name.
pub const TEMPLATES_JSON: &str = "templates.json";
/// The `rosters.json` asset name.
pub const ROSTERS_JSON: &str = "rosters.json";

/// The shipped strategy files `templates.json` and `stats.json` render, as `(path relative to
/// folder, parsed header)` in path order: the scan of `folder` (the repository's `strategies/`,
/// which the caller names) with its build state ignored.
///
/// # Errors
/// A folder under it cannot be read, it holds no strategy file (a wrong working directory, which
/// would otherwise publish an EMPTY roster), or a file's header does not parse: an asset never
/// renders a roster it could not read whole.
pub fn shipped_strategies(folder: &Path) -> Result<Vec<(String, StrategyFileHeader)>, String> {
    let found = scan(folder, None, None);
    if let Some(unreadable) = found.unreadable.first() {
        return Err(format!("cannot read the shipped strategies: {unreadable}"));
    }
    if found.files.is_empty() {
        return Err(format!("{} holds no strategy file", folder.display()));
    }
    let mut out = Vec::with_capacity(found.files.len());
    for file in found.files {
        match file.header {
            Ok(header) => out.push((file.path, header)),
            Err(why) => return Err(format!("{}: {why}", file.path)),
        }
    }
    Ok(out)
}

/// Every asset this module renders, as `(file name, pretty JSON + trailing newline)` — `venues.json`
/// and `stats.json` first, in that order, so the two positions the original consumers read are
/// stable; `indicators.json`, `templates.json` and `rosters.json` follow. The ONE rendering the bin
/// writes and the gate test parses — in-process, so the test proves the exact bytes a release
/// attaches. `generated_from` rides straight into `stats.json` through [`stats_value`];
/// `strategies` ([`shipped_strategies`]'s answer) into `templates.json` and `stats.json`; everything
/// else is derived from the compile-time tables, so one tree plus one stamp is one rendering.
#[must_use]
pub fn rendered_files(
    generated_from: &str,
    strategies: &[(String, StrategyFileHeader)],
) -> [(&'static str, String); 5] {
    [
        (VENUES_JSON, render(&venues_value())),
        (STATS_JSON, render(&stats_value(generated_from, strategies))),
        (INDICATORS_JSON, render(&indicators_value())),
        (TEMPLATES_JSON, render(&templates_value(strategies))),
        (ROSTERS_JSON, render(&rosters_value())),
    ]
}

fn render(value: &Value) -> String {
    let mut out = serde_json::to_string_pretty(value)
        .expect("a serde_json::Value with string keys always serializes");
    out.push('\n');
    out
}
