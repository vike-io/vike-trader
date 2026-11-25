//! A strategy built where this process does not build the file itself, and the one door every
//! harness run builds its strategy through.

use std::sync::Arc;

use vike_model::params::{ParamValue, Params};
use vike_model::{RESERVED_SOURCE_KEY, Strategy};
use vike_rhai::RhaiStrategy;
use vike_simulator::OrderBrokerSimulator;

use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use vike_strategy::file::scan::StrategyFileLanguage;

/// What a [`StrategyBuilder`] is: the strategy of a params table (a search point's, overrides
/// included). Named for clippy's `type_complexity`.
type BuildFn =
    dyn Fn(&Params) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, HarnessError> + Send + Sync;

/// **A test's stand-in for the sandboxed child a `.rs` file runs in**: the strategy of a file path
/// and a params table, built in the test's own process, or the stand-in's own words when it builds
/// no such file (forwarded as they are: a test support's refusal, not one of the harness's). The one
/// such builder is `vike_strategy_lab::stand_in`, the shipped files compiled in the dev-only
/// strategy-lab crate, which no binary links: this library builds no strategy of its own. Named for
/// clippy's `type_complexity`.
pub type StandIn =
    fn(&str, &Params) -> Result<Box<dyn Strategy<OrderBrokerSimulator> + Send>, String>;

/// How a strategy is built where this process does not build the file itself
/// ([`crate::run::profile::ProfileStrategy::builder`]): a door that owns one (the sandboxed child running a
/// `.rs` file) hands the harness this, so a single run, every search point and every walk-forward
/// window build it from their own params.
#[derive(Clone)]
pub struct StrategyBuilder(Arc<BuildFn>);

impl StrategyBuilder {
    /// The builder that calls `build` with the run's (or the point's) params table.
    pub fn new(
        build: impl Fn(&Params) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, HarnessError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        StrategyBuilder(Arc::new(build))
    }

    /// **Test support, never a door**: `file` built in this process by a test's [`StandIn`], for
    /// the tests that drive the harness with a shipped strategy file. Nothing a profile names
    /// reaches it.
    pub fn stand_in(file: &str, build: StandIn) -> Self {
        let file = file.to_string();
        StrategyBuilder::new(move |params| {
            build(&file, params).map(|s| -> Box<dyn Strategy<OrderBrokerSimulator>> { s }).map_err(
                |refusal| HarnessError::from(ProfileRefusal::StrategyStandInRefused { refusal }),
            )
        })
    }
}

impl crate::run::profile::ProfileBacktest {
    /// **Test support, as [`StrategyBuilder::stand_in`] is**: this profile with each `.rs` file
    /// (`[strategy]`'s, or every mount's) built in this process by `build`. A `.rhai` file, or a
    /// table a door already handed a builder, is left as it is.
    pub fn with_stand_in(mut self, build: StandIn) -> Self {
        for strategy in self.strategies_mut() {
            let rust = matches!(strategy.language(), Ok(StrategyFileLanguage::Rust));
            if rust && strategy.builder.is_none() {
                strategy.builder = Some(StrategyBuilder::stand_in(&strategy.file, build));
            }
        }
        self
    }
}

impl std::fmt::Debug for StrategyBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StrategyBuilder(..)")
    }
}

impl crate::run::profile::ProfileStrategy {
    /// **The ONE door a harness run builds its strategy through**: the [`StrategyBuilder`] a door
    /// set; else a `.rhai` file's source compiled here, with every number in the params table a
    /// `param()` override; a `.rs` file is never built in this process (it runs in a sandboxed
    /// child, whose door sets the builder).
    pub fn build(&self) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, HarnessError> {
        if let Some(builder) = &self.builder {
            return (builder.0)(&self.params);
        }
        match self.language()? {
            StrategyFileLanguage::Rhai => {
                let source = self.source.as_ref().ok_or_else(|| self.unread_file())?;
                let strategy = RhaiStrategy::<OrderBrokerSimulator>::compile_with_params(
                    &source.text,
                    rhai_overrides(&self.params),
                )
                .map_err(|e| self.rhai_compile_error(&e.to_string()))?;
                Ok(Box::new(strategy))
            }
            StrategyFileLanguage::Rust => Err(self.no_sandbox_route()),
        }
    }

    /// This table with each `(key, value)` written over its params, in order: how a search point
    /// specializes the strategy (`crate::search::paramscan::profile_with_overrides`).
    pub fn with_overrides(&self, overrides: &[(String, ParamValue)]) -> Self {
        let mut config = self.clone();
        for (k, v) in overrides {
            config.params.insert(k.clone(), v.clone());
        }
        config
    }
}

/// Collect a Rhai strategy's numeric params (every key EXCEPT the reserved `src`) into the
/// `param(name, default)` override map [`RhaiStrategy::compile_with_params`] bakes into the script
/// scope at compile — the same knobs a `[sweep]` grids over. A float or an integer is a number
/// (`size = 1` reads the same as `size = 1.0`, [`Params::numeric`]'s lenient read). Non-numeric
/// params (e.g. a stray string) are ignored, matching the lenient reader convention: the keys
/// [`Params::numeric`] reports as dropped are NOT reported here, because this door has no channel
/// to report them on.
fn rhai_overrides(params: &Params) -> indexmap::IndexMap<String, f64> {
    let (mut numbers, _dropped) = params.numeric();
    // `src` is a string whenever a script is inline, so `numeric` has already dropped it; removing
    // it by name keeps the "every key EXCEPT `src`" rule independent of that.
    numbers.shift_remove(RESERVED_SOURCE_KEY);
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Rhai override map: every number but `src`, an integer widened to `f64`, in key order —
    /// and a non-numeric knob DROPPED without a word (this door has no channel to report it on).
    #[test]
    fn rhai_overrides_keep_the_numbers_and_drop_the_rest_silently() {
        let params: Params = toml::from_str(
            "src = \"fn on_bar() {}\"\nqty = 2\nband = 1.5\nlabel = \"x\"\nflag = true\nxs = [1, 2]\n",
        )
        .unwrap();
        let overrides = rhai_overrides(&params);
        let got: Vec<(&str, f64)> = overrides.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(got, vec![("band", 1.5), ("qty", 2.0)]);
    }

    /// **A `[strategy.params].src` is never a strategy's source**: a `.rhai` strategy builds from
    /// its FILE's text, and a script under `src` (what the retired MCP `script` argument and
    /// `--script` used to put there) is dropped unread, so a run carrying one ran the file. A
    /// `src` that does not even compile proves it.
    #[test]
    fn a_params_src_is_dropped_and_the_file_is_what_runs() {
        let params: Params = toml::from_str("src = \"fn on_bar( {\"\n").unwrap();
        let strategy = crate::run::profile::ProfileStrategy {
            source: Some(crate::run::StrategySource::of("fn on_bar() {}".to_string())),
            ..crate::run::profile::ProfileStrategy::new("demo.rhai", params)
        };
        assert!(strategy.build().is_ok(), "the broken src was never compiled");
    }
}
