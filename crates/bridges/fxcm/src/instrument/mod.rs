//! What a symbol IS and the grid it trades on.

// PUBLIC for one type: `subscription::Failure` is in `crate::account::reads::SessionReads::subscribe`'s signature. The rest is
// crate-private: which rows the bridge subscribes itself, how the shim's answer reads, what a session remembers.
pub mod subscription;
