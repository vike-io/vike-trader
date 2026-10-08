//! The unit tests of `backtest_cli`, one file per surface under `backtest_cli/tests/`.
use super::*;

#[cfg(test)]
mod data_subcommand;

#[cfg(test)]
mod history_route;

#[cfg(test)]
mod persist;

#[cfg(test)]
mod rm_series;

/// The PURE half of the search-flag gate. `crates/vike-backtest/tests/optimizer_cli.rs` is the
/// shipped-binary half and is where the four defects are proven as an operator experiences them;
/// these are the table-driven pins a spawned-binary test cannot buy — a forgotten
/// [`METHOD_KNOBS`] or [`PROFILE_PATH_VALUED`] row reddens HERE, by name, rather than turning into
/// a silently accepted knob or a flag value read as a profile path.
#[cfg(test)]
mod search_flags;
