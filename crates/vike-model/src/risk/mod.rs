//! The pre-trade risk CONFIGURATION: the gate's limits (`RiskLimits`, `SymbolGrid`,
//! `Grid`, `PriceCollar`), the run profile's `[risk]` table that builds them
//! (`ProfileRisk`, `ProfileRiskError`) and the grid words (`GridSource`, `GridFetch`). Each is named
//! at this crate's root (`vike_model::RiskLimits`); the children are crate-private, so that is the
//! one public path.
//!
//! Stateless data, pure lookups and converters only. The GATE (`vike_execution::RiskGate`), the engine
//! context it judges against (`vike_execution::RiskContext`) and its verdict stay in vike-execution;
//! `TradingState`, the switch the gate reads, is vocabulary and lives here. The types came down so a
//! crate that cannot link the exec layer (vike-config's `[risk]` mirror, the light consumers)
//! reaches the one definition:
//! `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`.
//!
//! - `limits.rs` — `RiskLimits` and its per-symbol overrides.
//! - `grid.rs` — `GridSource` (where a venue arm gets another symbol's grid, as its bridge declares
//!   it; a lease-wire enum) and `GridFetch` (whether this mount's base grid was venue-fetched, the
//!   flag `ProfileRisk::apply_to` is gated on).
//! - `profile.rs` — the TOML `[risk]` → `RiskLimits` converter.
//! - `margin_call.rs` — `MarginCallConfig`, the margin-call watchdog's knobs: the run profile's
//!   `[guards.margin_call]` table AND the config `vike_execution`'s margin-call model reads, one type
//!   with serde (it was a vike-execution struct plus a vike-config mirror).
//! - `trading_state.rs` — `TradingState` (Active / Reducing / Halted).
//! - `dead_man.rs` — `DeadManAction`, what a dead-man switch does when it trips: the core's two
//!   switches and the `policy.deadman_action` row name the one type.
//! - `mount_loss.rs` — `MountLossAction`, what a breached mount loss budget does (`flatten`, the
//!   default, or `cancel_only`): the `policy.mount_loss_action` row and the core's budget name the
//!   one type.
//! - `submit_ack.rs` — the stuck-order watchdog's shipped ack timeout and confirm grace
//!   (`DEFAULT_SUBMIT_ACK_TIMEOUT`, `DEFAULT_SUBMIT_ACK_CONFIRM_GRACE`) and the last resort they make
//!   (`submit_ack_last_resort`), which every budget sized against them reads.

pub(crate) mod dead_man;
pub(crate) mod grid;
pub(crate) mod limits;
pub(crate) mod margin_call;
pub(crate) mod mount_loss;
pub(crate) mod profile;
pub(crate) mod submit_ack;
pub(crate) mod trading_state;
