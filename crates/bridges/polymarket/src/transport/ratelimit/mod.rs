//! The venue's rate gates.

pub mod budget;
// `polymarket`-gated DELIBERATELY, unlike hl/aster's feed-plane `ratelimit` modules: this one is
// the USER-channel WS-send gate, and its only consumer is the exec-plane user-data pump.
pub(crate) mod ws;
