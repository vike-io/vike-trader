use super::*;

#[path = "tests/ack_wait.rs"]
#[cfg(test)]
mod ack_wait;
#[path = "tests/idle_node.rs"]
#[cfg(test)]
mod idle_node;
#[path = "tests/lifecycle.rs"]
#[cfg(test)]
mod lifecycle;
#[path = "tests/not_now.rs"]
#[cfg(test)]
mod not_now;
#[path = "tests/reason_and_help.rs"]
#[cfg(test)]
mod reason_and_help;
#[path = "tests/snapshot_tables.rs"]
#[cfg(test)]
mod snapshot_tables;
#[path = "tests/submit.rs"]
#[cfg(test)]
mod submit;
