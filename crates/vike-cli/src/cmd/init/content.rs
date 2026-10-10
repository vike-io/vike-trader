//! The FILES `vike-cli init` ships — every README, example strategy, profile, result and notebook,
//! as static text plus the two functions that compose the derived ones.
//!
//! # Why the content is a table and not a directory of files copied at build time
//!
//! `vike-cli` is a single binary that must scaffold a working tree on a machine with no source
//! checkout — that is the whole point of `strategies/rhai/` existing (see [`RHAI_README`]). There
//! is nowhere to copy FROM on such a box, so the examples are compiled in. A table also gives
//! `--reset` something to compare against: "restore the sample" is only answerable while the
//! pristine bytes are still known, which a copied-once file cannot be.
//!
//! # Every folder ships an example, and that is a decision rather than an oversight
//!
//! An empty folder reads as setup somebody abandoned half-way, and a feature that leaves no trace
//! on disk cannot be discovered by the person the feature is for. So each folder carries at least
//! one runnable artifact, deliberately in preference to creating them on demand the first time
//! something needs them.
//!
//! ⚠ **`logs/` is the one exception, and only for `logs/`.** A committed example log would be
//! FICTION — invented timestamps describing a run that never happened — and `compile.log` is
//! rewritten on the next start anyway, so a sample there would be both false and short-lived. The
//! folder is still created, and its README says what appears in it and when.
//!
//! # The examples must RUN, which constrains what they may contain
//!
//! `crates/vike-script/src/engine/bindable.rs`'s `RHAI_INDICATORS` is the host-bound indicator set — the
//! names `register_indicators` actually registers, DERIVED from `vike_indicators::registry()`
//! rather than hand-listed, so the binding and the advertisement cannot drift. A script calling a
//! name OUTSIDE it hits a function-not-found error EVERY bar and self-disables after the
//! consecutive-error cap — a strategy that looks mounted and silently never trades, which a
//! backtest reports as a flat curve, i.e. as "no signal".
//!
//! Compiling is not evidence of any of that (rhai resolves a name when the line RUNS), so
//! `crates/vike-cli/tests/init_cli.rs` mounts every shipped script through the real `RhaiStrategy`,
//! drives real bars, and asserts an order reaches the broker. No count of the bound set appears
//! here or in the shipped READMEs: it is derived, and `vike-cli indicators` prints it.

mod indicators;
mod notebook;
mod profiles;
mod readmes;
mod strategies;

pub use indicators::{DONCHIAN_HIGH_RHAI, INDICATORS_README, STREAK_RHAI};
pub use notebook::{NOTEBOOK_IPYNB, SAMPLE_RESULT_JSON};
pub use profiles::{PROFILE_BACKTEST, SEED_DEMO, profile_sweep, profile_walkforward};
pub use readmes::{
    LOGS_README, NOTEBOOKS_README, PROFILES_README, RESULTS_README, RHAI_README, RUST_README,
};
pub use strategies::{
    EMA_TREND_RHAI, MY_EXPERIMENT_RS, PRESET_EMA_DEFAULT, PRESET_RSI_DEFAULT, PRESET_SMA_FAST,
    PRESET_SMA_SLOW, RSI_MEANREV_RHAI, SMA_CROSS_RHAI,
};

/// The single-line-per-folder MAP of `user_data/`.
///
/// ⚠ **ONE definition, two surfaces.** This is printed by `vike-cli init` AND embedded verbatim in
/// the `user_data/README.md` it writes — the command's whole reason for printing a tree is that the
/// map should exist before the user opens a file manager, and a map that disagreed with the README
/// beside it would be worse than no map. `map_is_embedded_verbatim_in_the_readme` pins that they
/// cannot drift.
pub const MAP: &str = "\
user_data/               your work — nothing here is ever overwritten once you have edited it
  strategies/rhai/       interpreted strategies: edit, save, backtest. No toolchain needed.
  strategies/rust/       compiled strategies. SOURCE CHECKOUT ONLY — a release binary cannot build them.
  indicators/            your own indicators, one <name>.rhai each. Callable from any strategy.
  profiles/              run profiles: which data, which costs, which strategy
  backtest_results/      saved reports. One sample ships, so the notebooks work before your first run.
  notebooks/             analysis over the results above
  logs/                  compile.log — every strategy load, pass and fail. Look here first.
";

/// `user_data/README.md` — the map, plus the ownership rule that explains why this directory is not
/// inside `settings/`.
///
/// Composed rather than stored so [`MAP`] has exactly one definition; see its doc.
pub fn readme() -> String {
    format!(
        r##"# user_data

Everything in this directory is **yours**. You author it, you back it up, you delete it. No part of
this workspace rewrites a file here — `vike-cli init` creates what is missing and stops.

```text
{MAP}```

## Why this is not inside `settings/`

`settings/` is the machine's half: the settings database (`db/vike.db`) the program READS and a
`state/` directory it WRITES. It also holds your live venue API keys, in that database.

`user_data/` is the half a program must never author. Keeping them apart is what makes this
directory safe to copy between machines, commit to your own repository, or share — none of which is
true of a directory that holds credentials.

Two more consequences worth knowing:

- **Lifecycle.** Delete `settings/state/` and the program rewrites it. Delete a strategy here and
  your work is gone.
- **Blast radius.** A directory you are invited to drop files into should not sit beside the file
  holding every key on the box.

## Where this directory is

`<project>/user_data`, resolved by walking up from the working directory for the project root — the
same walk that finds `settings/`, so the two can never answer with different projects.

`VIKE_USER_DATA_DIR` names it outright and skips the walk. It is a separate variable from
`VIKE_SETTINGS_DIR` on purpose: pointing the app at a strategy library on another disk is a
different question from relocating a deployment's settings, and one variable for both would force
them to move together.

## Getting started

```sh
vike-cli init                 # create anything missing (safe to re-run; never overwrites)
vike-cli init --reset         # restore the shipped samples you have edited or deleted
vike-cli init --dry-run       # print what WOULD change, touch nothing
```

Then read `strategies/rhai/README.md` — it is the shortest path from here to a backtest.

## What is deliberately NOT here

- **`data/`** — the history store is measured in hundreds of gigabytes and resolves on its own
  (`VIKE_HIST_STORE`). Filing it under a directory you are told to back up would be wrong in both
  directions.
- **A `.rs` indicator.** `indicators/` takes Rhai files only, and the Rust half is **out of scope**
  rather than deferred — "later" would leave you waiting for something that is not coming.
  `vike_indicators::registry` is a compile-time table whose every entry is held to a bit-parity law
  (streaming `on_bar` must equal batch `vectorize` bit-for-bit, gated over the whole registry), so
  adding one is a change to that crate, never a file you drop in here. `strategies/rust/` is honest
  about the same constraint in its own first line.

  What you CAN write is a Rhai indicator, which is a real indicator: see `indicators/README.md`, and
  `vike-cli indicators` for the built-in names already callable.
"##
    )
}
