//! The declared sets the CI plan is computed from — and, since the Python planner was deleted, the
//! ONE authority for them.
//!
//! Every table below carries its own argument. That is the point of this file: the sets are small
//! and the reasons are not, and each reason is a measurement somebody paid for. Until the port
//! landed these comments lived on the Python planner's constants, and this file said "the rationale
//! lives there, and MOVES here in the PR that deletes the script". This is that PR, so they moved —
//! once, not copied, because a copy of a justification rots exactly the way the crate rosters this
//! repository keeps re-deriving do.
//!
//! ⚠ A row here is not a preference, it is a claim about what CI would otherwise fail to test.
//! Deleting one is allowed; deleting one without answering its comment is how a gate stops firing
//! in silence, which every "EXCLUDED, with the reason" list below exists to prevent.

pub mod escalation;
pub mod feature_suites;
pub mod gate_crates;
pub mod gate_triggers;
pub mod readers;
pub mod roster;
pub mod suite_rules;
