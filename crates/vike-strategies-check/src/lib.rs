//! `vike-strategies-check`: the check of the SHIPPED strategy files, `strategies/**` at the
//! repository root (copied into `<project>/strategies/` at install, then the user's).
//!
//! Its tests are the work: they read every shipped file's header (`vike_strategy::file::scan`) and
//! compile each shipped file in-process as a `#[path]` module, monomorphised on the broker the test
//! drives it on. That is where CI compiles all of them in one go, and where the strategy lab reads
//! them.
//!
//! The library holds the 615 files written from the strategy gallery's rules ([`gallery`], each a
//! `vike_tactics::Rule` behind `pub fn rule`), because the strategy lab, an example of `vike-simulator`,
//! runs them and can reach only a library: [`gallery_file`] finds one by its stem, with the `[run]`
//! options its header states. It also holds the shipped taker files ([`takers`]) and the two maker
//! files ([`makers`]), which the tests of the harness, the hubs and the cores drive in-process: [`stand_in`] is the one door those tests
//! build a `.rs` file through, in place of the sandboxed child a shipped program runs it in.
//!
//! ⚠ **No crate names this one as a normal dependency, and no binary links it**: a strategy is
//! never compiled into a program, in any form. Its layer is the top rank (`check`), so
//! `crates/vike-ops/tests/architecture/layer_gate.rs` refuses every normal edge onto it.

use std::path::Path;

use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};
use vike_strategy::file::StrategyFileHeader;
use vike_tactics::{Rule, RuleRunOptions};

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
/// its `StandIn` (`crates/vike-backtest/src/harness/strategy_builder.rs`'s `StandIn`) and forwards
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
/// `rule` entry, and the order language's switches its header's `[run]` states.
#[derive(Debug, Clone, Copy)]
pub struct GalleryFile {
    pub path: &'static str,
    pub rule: fn(&Params) -> Box<dyn Rule + Send>,
    pub run: RuleRunOptions,
}

/// The gallery-born file whose stem is `stem` (`trend/abcd_strategy.rs` for `abcd_strategy`), its
/// `[run]` read from its header on disk under `strategies` (the shipped set's folder, which the
/// caller names: a library resolves no path the compiler baked in); `None` when [`gallery::FILES`]
/// holds no such file.
///
/// # Panics
/// When the file cannot be read or its header does not parse.
pub fn gallery_file(strategies: &Path, stem: &str) -> Option<GalleryFile> {
    let &gallery::GalleryEntry { path, rule } =
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
    Some(GalleryFile { path, rule, run })
}
