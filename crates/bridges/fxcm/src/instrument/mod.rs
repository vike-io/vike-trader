//! What a symbol IS and the grid it trades on.

// An instrument's USER name and its WIRE name, both ways, off its row (decision 0290). PUBLIC for one function,
// `names::kept_list_for_tests` (`test-support`): the conformance harness and this crate's integration tests hold it. The
// rest is crate-private.
pub mod names;
// PUBLIC for one type: `subscription::Failure` is in `crate::account::reads::SessionReads::subscribe`'s signature. The rest is
// crate-private: which rows the bridge subscribes itself, how the shim's answer reads, what a session remembers.
pub mod subscription;
