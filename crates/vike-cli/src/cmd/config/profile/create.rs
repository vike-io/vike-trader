//! `vike-cli config profile create …`'s three writers, one file per kind: [`daemon`], [`recorder`]
//! and [`run`] (the daemon body's run part). Each builds its body from arguments, never from a file.

pub(in crate::cmd::config) mod daemon;
pub(in crate::cmd::config) mod recorder;
pub(in crate::cmd::config) mod run;
