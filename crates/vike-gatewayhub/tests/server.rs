//! `server` -- vike-gatewayhub's server suites: the real server over scripted lanes (`tests/server/`) and the
//! REAL data and REST lanes behind it (`tests/lanes/`), ONE test binary over what used to be 18.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, and every one of them links this
//! crate's whole dependency graph. Each former `tests/<name>.rs` member here is included below as a plain
//! module: its tests, bodies and module doc are unchanged, and only the binary it links into moved (nextest
//! names a test `server <name>::<test>`).
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". The `tests/lanes/`
//! members keep their `#![cfg(feature = "data")]`, which every build of this crate's tests satisfies: the
//! self-referential dev edge turns `trade` on, and `trade` implies `data` (`Cargo.toml`), so the two halves
//! were never compiled apart. `venue_interval_matrix` carries `#[ignore]`d live probes; run them as
//! `cargo test -p vike-gatewayhub --test server -- --ignored --nocapture --test-threads=1 venue_interval_matrix::`.

// The helpers only this group uses, compiled once here (each used to be `#[path]`-included by every member,
// then lived in the lib's testkit while two binaries shared them). Every member uses a different part.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "server/support/fake_lanes.rs"]
mod fake_lanes;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "server/support/harness.rs"]
mod harness;
#[allow(dead_code, clippy::allow_attributes)]
#[cfg(feature = "data")]
#[path = "server/support/rest_server.rs"]
mod rest_server;

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod x;` would resolve against
// `tests/`, not `tests/server/`.
#[path = "server/auth_and_scope.rs"]
mod auth_and_scope;
#[path = "server/backpressure.rs"]
mod backpressure;
#[path = "server/delivery_class.rs"]
mod delivery_class;
#[path = "server/feature_graph.rs"]
mod feature_graph;
#[path = "server/fetch_lane.rs"]
mod fetch_lane;
#[path = "server/help_cli.rs"]
mod help_cli;
#[path = "server/lifecycle.rs"]
mod lifecycle;
#[path = "server/rest_verbs.rs"]
mod rest_verbs;
#[path = "server/stream_loopback.rs"]
mod stream_loopback;
#[path = "server/stream_wire.rs"]
mod stream_wire;
#[path = "server/welcome.rs"]
mod welcome;

#[path = "lanes/data_lane.rs"]
mod data_lane;
#[path = "lanes/ig_credential.rs"]
mod ig_credential;
#[path = "lanes/oanda_credential.rs"]
mod oanda_credential;
#[path = "lanes/rest_lane_history.rs"]
mod rest_lane_history;
#[path = "lanes/rest_lane_surfaces.rs"]
mod rest_lane_surfaces;
#[path = "lanes/strategy_lane.rs"]
mod strategy_lane;
#[path = "lanes/venue_interval_matrix.rs"]
mod venue_interval_matrix;
