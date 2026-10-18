//! The pre-trade risk CONFIGURATION: the gate's limits (`RiskLimits`, `SymbolGrid`,
//! `ResolvedGrid`, `PriceCollar`) and the run profile's `[risk]` table that builds them
//! (`ProfileRisk`, `GridSource`, `ProfileError`). Each is named at this crate's root
//! (`vike_model::RiskLimits`); the children are crate-private, so that is the one public path.
//!
//! Stateless data, pure lookups and converters only. The GATE (`vike_exec::RiskGate`), the engine
//! context it judges against (`vike_exec::RiskContext`) and its verdict stay in vike-exec;
//! `TradingState`, the switch the gate reads, is vocabulary and lives here. The types came down so a
//! crate that cannot link the exec layer (vike-config's `[risk]` mirror, the light consumers)
//! reaches the one definition:
//! `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`.
//!
//! - `limits.rs` — `RiskLimits` and its per-symbol overrides.
//! - `profile.rs` — the TOML `[risk]` → `RiskLimits` converter; its source is also published as
//!   data by [`surface`].
//! - `margin_call.rs` — `MarginCallConfig`, the margin-call watchdog's knobs: the run profile's
//!   `[guards.margin_call]` table AND the config `vike_exec`'s margin-call model reads, one type
//!   with serde (it was a vike-exec struct plus a vike-config mirror).
//! - `fields.rs` — serde's own field list for a derived struct (`ProfileRisk::keys`), so no list of
//!   the `[risk]` keys is ever written by hand.
//! - `trading_state.rs` — `TradingState` (Active / Reducing / Halted).

mod fields;
pub(crate) mod limits;
pub(crate) mod margin_call;
pub(crate) mod profile;
pub mod surface;
pub(crate) mod trading_state;
