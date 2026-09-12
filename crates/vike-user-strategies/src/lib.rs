//! `vike-user-strategies` — the compiled user-strategy HOST.
//!
//! `build.rs` scans `<workspace>/user_data/strategies/rust/<name>/<name>.rs` (build-time override:
//! `VIKE_USER_DATA_DIR`) and generates the registry this crate re-exports:
//! [`USER_STRATEGIES`], [`USER_LIVE_CAPABLE`] and [`user_strategy_by_name`]. The built-in
//! registries consult it AFTER every built-in arm — `vike-backtest`'s harness registry on the
//! `RegistryError::Unknown` path, `vike-tradehub`'s profile resolution behind its live gate — so a
//! user folder can never shadow a built-in name, and NO framework crate changes when a strategy is
//! added: a new native strategy is a new folder, nothing else.
//!
//! ## Why a separate crate (the load-bearing property)
//!
//! User entry files compile as modules of THIS crate — a separate compilation unit from
//! `vike-strategy` — so they resolve only the platform's PUBLIC API (through this crate's
//! dependencies: `vike-model`, `vike-strategy`, `vike-indicators`, `toml`). An internals refactor
//! of the framework can never break a user strategy, and a user strategy can never grow a
//! load-bearing dependency on a `pub(crate)` detail (the Hyrum freeze the spec's v2 rejected).
//! Design + the full v1/v2/v3 argument:
//! `docs/superpowers/specs/2026-08-11-user-native-strategies-design.md`.
//!
//! ## The entry-file contract
//!
//! ```ignore
//! pub fn build<B: vike_model::HftBroker + 'static>(
//!     params: &toml::Value,
//! ) -> Box<dyn vike_model::Strategy<B> + Send>
//! ```
//!
//! Lenient param reading (`params.get(..)` + defaults), the same idiom as every built-in
//! `from_params`. The in-tree PARAM_KEYS/param-gate tables do NOT cover user strategies — they
//! gate the reference set — so a typo'd key silently reads as the default; the tier README says so
//! loudly. `strategy.toml` beside the entry file with `live = true` opts into live mounting;
//! absent means sim-only (the `LIVE_CAPABLE` default-deny posture).
//!
//! ## CI / empty state
//!
//! No `user_data/` (every CI checkout, every fresh clone) ⇒ the generated registry is EMPTY and
//! this crate is inert — [`user_strategy_by_name`] answers `None` for every name, byte-identical
//! consumer behavior. The full pipeline is still CI-proven on every run through the committed
//! fixture tree (`tests/fixture_user_data/` → a second generated registry the integration test
//! drives end-to-end). User code compiles only in a source checkout and into the operator's OWN
//! binary; it never enters the repo — `user_data/` is gitignored.

/// The pure scan/render half, shared verbatim with `build.rs` (which `include!`s the same file).
/// Public so the generator is unit-testable as a plain function of paths and strings.
pub mod codegen;

include!(concat!(env!("OUT_DIR"), "/user_registry.rs"));
