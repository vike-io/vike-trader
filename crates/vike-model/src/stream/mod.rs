//! The live-STREAM vocabulary: what one delivered stream says about itself. A private module; every
//! name is spelled from the crate root (`vike_model::StreamStatus`, `vike_model::StreamDelay`), so the
//! folder is a layout choice that no consumer sees.
//!
//! - `status.rs` is [`crate::StreamStatus`], the stream-health disclosure (a gap opened, the data went
//!   stale, the stream recovered).
//! - `delay.rs` is [`crate::StreamDelay`], how far behind the market the data is: a label, never a gate
//!   (decision 0140).
//! - `label.rs` is [`crate::StreamLabel`], the closed vocabulary of the four tick-lane words a stream is
//!   named by, and [`crate::TAPE_HOLE_WORD`], the recorder's word that is no lane.

pub(crate) mod delay;
pub(crate) mod label;
pub(crate) mod status;
