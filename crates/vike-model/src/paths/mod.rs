//! Filesystem-path resolution: where program-written state, the hist store and the live-tick store
//! live when nobody said. `store_path` and `tick_store_path` are each one resolver with its own
//! precedence; `state_path` owns the project-root walk they (and every `<project>`-relative path in
//! the workspace) start from. `store_plane` is the odd one out — not a resolver but the classifier
//! of whose data a store kind holds, the market's or an account's.

pub mod state_path;
pub mod store_path;
pub mod store_plane;
pub mod tick_store_path;
