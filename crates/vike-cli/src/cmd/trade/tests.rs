use super::*;

#[path = "tests/lifecycle.rs"]
#[cfg(test)]
mod lifecycle;
#[path = "tests/reason_and_help.rs"]
#[cfg(test)]
mod reason_and_help;
#[path = "tests/snapshot_tables.rs"]
#[cfg(test)]
mod snapshot_tables;
#[path = "tests/submit.rs"]
#[cfg(test)]
mod submit;
