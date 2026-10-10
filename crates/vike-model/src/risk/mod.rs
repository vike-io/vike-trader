//! The pre-trade risk CONFIGURATION: the gate's limits (`RiskLimits`, `SymbolGrid`,
//! `ResolvedGrid`, `PriceCollar`) and the run profile's `[risk]` table that builds them
//! (`ProfileRisk`, `GridSource`, `ProfileError`). Each is named at this crate's root
//! (`vike_model::RiskLimits`); the children are crate-private, so that is the one public path.
//!
//! Stateless data, pure lookups and converters only. The GATE (`vike_exec::RiskGate`), the engine
//! state it judges against (`vike_exec::RiskContext`, `vike_exec::TradingState`) and its verdict
//! stay in vike-exec. The types came down so a crate that cannot link the exec layer (vike-config's
//! `[risk]` mirror, the light consumers) reaches the one definition:
//! `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`.
//!
//! - `limits.rs` — `RiskLimits` and its per-symbol overrides.
//! - `profile.rs` — the TOML `[risk]` → `RiskLimits` converter; its source is also published as
//!   data by [`surface`].
//! - `fields.rs` — serde's own field list for a derived struct (`ProfileRisk::keys`), so no list of
//!   the `[risk]` keys is ever written by hand.

mod fields;
pub(crate) mod limits;
pub(crate) mod profile;
pub mod surface;
