//! The engine's unit tests, one file per role they exercise.

use crate::ctx::SharedCtx;
use vike_model::Bar;

mod bindable;
mod builtin_lines;
mod host_and_bridge;
/// The USER side of multi-output: `fn outputs()` -> per-line accessors, through the same
/// [`line_fn_name`] and the same namesake rule the built-ins get.
///
/// These live beside the engine rather than in `tests/` because the property that matters most —
/// that a LINE never enters the streaming cache key — is only observable from inside `ScriptCtx`,
/// and a value-level test cannot tell "one instance read three times" from "three instances each
/// fed once" (they agree, bar for bar, on any deterministic recurrence).
mod user;

/// Installs `bar` as the bar the engine's host reads see and opens a fresh bar for the indicator
/// cache: what `RhaiStrategy` does at the top of each `on_bar`, without a broker.
fn load_bar(ctx: &SharedCtx, bar: Bar) {
    let mut g = ctx.write().unwrap();
    g.cur_bar = bar;
    g.fed_this_bar.clear();
}
