//! Paper trading (R7): the R1/R2 backtest fill semantics running behind the live core's
//! `ExecutionClient` seam. The PAPER CLIENT owns a resting book and fills it with the ENGINE'S
//! OWN public primitives
//! ([`BarFillModel::fill_price`](vike_fills::fill_model::FillModel::fill_price),
//! [`vike_fills::fill_resolution::resolve_intrabar_fills`],
//! [`vike_fills::broker_sim::adverse_fill_price`], [`vike_fills::broker_sim::fee`]), so
//! backtest == paper is BY CONSTRUCTION, then gated bit-for-bit in `vike-sim`'s
//! `tests/r7_gate.rs` — which stays there because it drives the ENGINE and the LIVE CORE, not
//! this client alone.
//!
//! Why this is a crate of its own (folded into `vike-backtest` once, re-extracted when `vike-fills`
//! gave the shared fill core its own home): `docs/crates.md`'s `crates/vike-paper` row.
//!
//! **Scope of that equivalence law:** it holds for `Gtc`/`Ioc`/`Fok` — i.e. for everything the
//! engine and the paper book both treat as "rests until filled or canceled". It does NOT hold for
//! the EXPIRING time-in-forces (`Gtd`/`Day`): this client enforces their resting deadline
//! (`expire_due`) while `StrategyEngine`/`SimBroker` ignore TIF entirely, so for such an order the
//! engine keeps resting and filling where paper expires. Nothing gates that divergence — r7_gate
//! only exercises `Gtc` — and nothing in-tree produces a non-`Gtc` TIF into either side today
//! (`BrokerContext::Submission` carries no TIF field, `strategy_drive` builds every request with
//! `..Default::default()` ⇒ `Gtc`, and `runtime/apply.rs` hardcodes `Gtc`), so the divergence is
//! currently unreachable from a strategy. Mirroring the sweep in `StrategyEngine::fill_pending` is
//! the follow-up that would restore the law for expiring TIFs.
//!
//! **Conditional verbs ARE gated** (trigger-law wave 2): `tests/r7_gate.rs` grew rows proving the
//! stop verb (engine `submit_stop` under `EngineParams::emulator_release_stops = true` vs live's
//! always-emulated `ConditionalBook` → market release through this book) and the bracket (engine
//! protective-stop + TP vs `submit_bracket`'s OTO/OCO triple resting here) fill bit-identically.
//! The engine's DEFAULT stop-verb semantics (same-event fill at the trigger) remain a documented
//! divergence from live, pinned in `tests/laws/trigger_law.rs`.
//!
//! See `crates/vike-paper/CLAUDE.md` for the module map and the traps.

mod client;
mod multi;
mod replay;

pub use client::{PaperExecutionClient, PaperFill};
pub use multi::MultiPaperExecutionClient;
pub use replay::replay_bars;
