//! `vike::dispatch` refuses a SECOND tool in one process — proven by running it, not by reading it.
//!
//! ⚠ **This file holds exactly one test, and that is a requirement rather than tidiness.** The
//! guard is a process-global `AtomicBool`, so any other test in this binary that dispatched first
//! would flip it and make this one pass for the wrong reason. `cargo nextest` runs a process per
//! test and would hide the coupling; a plain `cargo test` shares one process and would expose it.
//! Keeping the file to one test makes both runners agree.
//!
//! Why the rule exists: every tool this dispatcher will carry owns process-global state that
//! assumes it is alone — `vike_log::init` is a `try_init` where only the FIRST `LogConfig` wins,
//! `vike_ops::stop::install_handlers` registers once, `vike_script`'s `INSTALLED` is a one-shot.
//! A second tool would inherit the first's logging configuration and signal handlers, silently, in
//! a process that may be signing orders.

use std::process::ExitCode;

use vike::{Tool, ToolCtx, dispatch};

fn noop(_ctx: &ToolCtx<'_>, _args: &[String]) -> ExitCode {
    ExitCode::SUCCESS
}

#[test]
fn a_second_dispatch_in_one_process_panics() {
    let tool = Tool { name: "probe", main: noop, summary: "" };
    let env = std::collections::HashMap::new();
    let ctx = ToolCtx { env: &env, cwd: None, invoked_as: "probe" };

    let first = dispatch(&tool, &ctx, &[]);
    assert_eq!(
        format!("{first:?}"),
        format!("{:?}", ExitCode::SUCCESS),
        "the FIRST dispatch must run normally — a guard that refused everything would pass the \
         assertion below while breaking every tool"
    );

    let second = std::panic::catch_unwind(|| dispatch(&tool, &ctx, &[]));
    assert!(
        second.is_err(),
        "a SECOND dispatch returned instead of panicking, so the one-tool-per-process rule is \
         documented but not enforced."
    );
}
