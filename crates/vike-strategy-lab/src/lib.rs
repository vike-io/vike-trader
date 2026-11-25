//! `vike-strategy-lab`: the check of the SHIPPED strategy files, `strategies/**` at the
//! repository root (copied into `<project>/strategies/` at install, then the user's).
//!
//! Its tests are the work: they read every shipped file's header (`vike_strategy::file::scan`) and
//! compile each shipped file in-process as a `#[path]` module, monomorphised on the broker the test
//! drives it on. That is where CI compiles all of them in one go, and where the strategy lab reads
//! them.
//!
//! The library holds the order-language files the strategy lab runs (`gallery`, each a
//! `vike_tactics::Rule` behind `pub fn rule`, listed at build time from the folder by `build.rs`): the ones
//! written from the strategy gallery's rules, those of `strategies/looks_into_the_future/`, the rules that
//! read ahead (decision 0271's amendment of 2026-11-14), and the set-B rows imported since, because the two
//! strategy labs, this crate's examples
//! (`examples/strategy_lab/`, `examples/strategy_lab_b/`), run them and an example reaches only its
//! crate's library: `gallery_file` finds one by its stem, with the `[run]`
//! options its header states. It also holds the shipped taker files ([`takers`]) and the two maker
//! files ([`makers`]), which the tests of the harness, the hubs and the cores drive in-process: [`stand_in`] is the one door those tests
//! build a `.rs` file through, in place of the sandboxed child a shipped program runs it in.
//!
//! ⚠ **The gallery compiles only behind this crate's `gallery` feature**, and only this crate's own
//! test and example builds turn it on (its self dev-dependency). The tests of the nine crates that
//! dev-depend on it for [`stand_in`] and the makers compile the takers and makers alone, not the gallery's
//! files; CI compiles those when it builds this crate itself, which a pull request's plan does only
//! when a strategy-visible file changed (`xtask/src/ci/selection.rs`'s `strategy_lab_touched`).
//! `gallery` and `gallery_file` are not links here because a build without the feature has neither.
//!
//! ⚠ **No crate names this one as a normal dependency, and no binary links it**: a strategy is
//! never compiled into a program, in any form. The layer gate refuses every normal edge onto it by
//! name (`crates/vike-ops/tests/architecture/layer_gate/fences.rs`'s
//! `no_crate_names_the_strategy_lab_normally`), not by its rank.

use std::path::Path;

use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};
#[cfg(feature = "gallery")]
use vike_strategy::file::{StrategyFileFill, StrategyFileHeader};
#[cfg(feature = "gallery")]
use vike_tactics::{Rule, RuleRunOptions};

#[cfg(feature = "gallery")]
pub mod gallery;
pub mod makers;
pub mod takers;
// The broker doubles the taker files' own tests name (`crate::test_support`).
#[cfg(test)]
#[allow(dead_code, clippy::allow_attributes)]
mod test_support;

/// **A test's stand-in for the sandboxed child a `.rs` file runs in**: the shipped taker file whose
/// stem is `file`'s (`trend/buy_hold.rs` → [`takers::buy_hold`]), built on `B` from `params` in the
/// test's own process; the refusal's words when no taker file has that stem. The harness takes it as
/// its `StandIn` (`crates/vike-backtest/src/run/strategy_builder.rs`'s `StandIn`) and forwards
/// those words, so the refusal is this test support's, never a profile refusal of the harness.
///
/// # Errors
/// No shipped taker file has `file`'s stem.
pub fn stand_in<B: OrderBroker + 'static>(
    file: &str,
    params: &Params,
) -> Result<Box<dyn Strategy<B> + Send>, String> {
    let stem = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    Ok(match stem {
        "buy_hold" => takers::buy_hold::build(params),
        "cheap_catch_updown_fair_value" => takers::cheap_catch_updown_fair_value::build(params),
        "covered_call" => takers::covered_call::build(params),
        "dca_accumulate" => takers::dca_accumulate::build(params),
        "funding_capture" => takers::funding_capture::build(params),
        "funding_carry" => takers::funding_carry::build(params),
        "grid" => takers::grid::build(params),
        "momentum" => takers::momentum::build(params),
        "pairs_zscore" => takers::pairs_zscore::build(params),
        "rotation_top_k" => takers::rotation_top_k::build(params),
        "sport_copy_follower" => takers::sport_copy_follower::build(params),
        "trailing_scalper" => takers::trailing_scalper::build(params),
        _ => {
            return Err(format!(
                "strategy file `{file}`: no shipped taker file has its stem, so the test stand-in \
                 builds nothing in this process"
            ));
        }
    })
}

/// A gallery-born strategy file as the strategy lab runs it: its path under `strategies/`, its
/// `rule` entry, the order language's switches its header's `[run]` states, and whether its header's
/// `[strategy] fill` is `close` (its orders fill at the close of the bar that placed them, in a
/// backtest and on a paper book alike).
#[cfg(feature = "gallery")]
#[derive(Debug, Clone, Copy)]
pub struct GalleryFile {
    pub path: &'static str,
    pub rule: fn(&Params) -> Box<dyn Rule + Send>,
    pub run: RuleRunOptions,
    pub fill_at_close: bool,
}

/// The gallery-born file whose stem is `stem` (`trend/abcd_strategy.rs` for `abcd_strategy`), its
/// `[run]` and `fill` read from its header on disk under `strategies` (the shipped set's folder, which the
/// caller names: a library resolves no path the compiler baked in); `None` when [`gallery::FILES`]
/// holds no such file.
///
/// # Panics
/// When the file cannot be read or its header does not parse.
#[cfg(feature = "gallery")]
pub fn gallery_file(strategies: &Path, stem: &str) -> Option<GalleryFile> {
    let &gallery::GalleryFileEntry { path, rule } =
        gallery::FILES.iter().find(|e| Path::new(e.path).file_stem().is_some_and(|s| s == stem))?;
    let file = strategies.join(path);
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let header = StrategyFileHeader::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
    let run = header.run.map_or(RuleRunOptions::OFF, |r| RuleRunOptions {
        bound_exits: r.bound_exits,
        stacked_closes: r.stacked_closes,
        fills_a_day: r.fills_a_day,
        touch_at_market: r.touch_at_market,
    });
    let fill_at_close = header.strategy.fill == StrategyFileFill::Close;
    Some(GalleryFile { path, rule, run, fill_at_close })
}
