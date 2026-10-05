//! Filesystem-path resolution: where program-written state, the hist store and the live-tick store
//! live when nobody said. Each module is one resolver with its own precedence; `state_path` owns the
//! project-root walk the other two (and every `<project>`-relative path in the workspace) start from.

pub mod state_path;
pub mod store_path;
pub mod tick_store_path;
