//! **The tick lane's ONE contract: the venue's half.** The gatewayhub serves `Fetch { Ticks }` (a venue's
//! archived quotes over a client-named window) through ONE engine, `crates/vike-gatewayhub/src/rest/ticks.rs`'s
//! `fetch_ticks`, and the venue's whole difference is a [`TicksSpec`] in that venue's bridge: what it
//! refuses before it asks, how it cuts a window into the requests it makes, and where it can stop between
//! them. The engine + spec split of `crate::credentialed_klines`, for the tick lane.
//!
//! # What the engine does, so a spec does not
//!
//! It hands the spec a stop probe that reads the request's cancel flag, and turns the spec's answer into
//! the wire's: [`TicksError::Refused`] is the wire's refusal and [`TicksError::Fetch`] its failure, each with
//! its words unchanged, and [`TicksError::Stopped`] becomes the engine's cancel words with the spec's stage.
//!
//! # What a spec promises
//!
//! * **A request it will not serve is refused before the venue is asked**, in words that reach the peer.
//!   The window and the symbol are CLIENT-NAMED, so the bound on what one fetch may cost is the spec's to
//!   apply (the gatewayhub hands it the wire's span bound as a parameter).
//! * **It asks `stop` before every venue request it can stop in front of**, and answers `Stopped` naming
//!   where; a request already sent runs to its end.
//! * **A failed request fails the whole fetch**: never a short answer, because the datahub stores what it
//!   is given.

use vike_marketdata::TickQuote;

/// Why a tick fetch answered no rows. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicksError {
    /// The request was refused before the venue was asked; the words reach the peer.
    Refused(String),
    /// The venue was asked and failed, in the bridge's own words.
    Fetch(String),
    /// The stop probe said stop, at this stage (`"between hour files"`): the engine's cancel words name it.
    Stopped(&'static str),
}

/// **A venue's tick lane**, as the gatewayhub's engine asks it. See the module doc.
pub trait TicksSpec: Send + Sync {
    /// The quotes of `[start_ms, end_ms]` of `symbol`, asking `stop` before every venue request it can
    /// stop in front of.
    fn fetch_until(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<TickQuote>, TicksError>;
}
