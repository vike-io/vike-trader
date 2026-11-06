//! The two seams a server dispatches to, as one value.
//!
//! A composition root builds a `crate::data::DataLane` and a `crate::rest::RestLane` (the real ones, over
//! the venue bridges, behind the `data` feature; the fakes of the test suite otherwise) and hands the
//! pair to `crate::server::accept::serve`. The server asks each for its BUILD facts once, at start, and
//! never looks at a venue itself.

use std::sync::Arc;

use crate::data::DataLane;
use crate::rest::RestLane;

/// The venue half of the daemon, as the server sees it.
#[derive(Clone)]
pub struct Lanes {
    /// Live streams.
    pub data: Arc<dyn DataLane>,
    /// Every other verb.
    pub rest: Arc<dyn RestLane>,
}

impl std::fmt::Debug for Lanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lanes").finish_non_exhaustive()
    }
}
