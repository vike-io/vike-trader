//! What the rename tooling asks of THIS vike's Rhai host before it rewrites a `.rhai` file
//! (`vike-cli strategy migrate` and Studio's Apply, decision 0296): does a name still resolve here
//! ([`binds_name`]), and does a text still parse ([`parse_check`]). Neither runs a script.

use crate::engine::builtin::{is_callable, line_accessors};
use crate::engine::host::{HOST_FN_NAMES, SandboxLimits, sandboxed_engine};
use crate::engine::installed::{
    installed_user_bare_call, installed_user_indicators, installed_user_line_accessors,
};
use crate::strategy::ScriptError;
use crate::tactics::STRATEGY_FN_NAMES;

/// Whether the strategy engine binds `name` as a function a script calls: a host read
/// ([`HOST_FN_NAMES`]), a `strategy` verb, builder option or read ([`STRATEGY_FN_NAMES`]), a
/// built-in indicator bare or per line, or an installed user indicator bare or per line.
///
/// ⚠ A getter, a setter and a type answer `false`: they are listed nowhere (rhai lists no
/// registrations without its `metadata` feature, which this workspace does not enable).
pub fn binds_name(name: &str) -> bool {
    let line_of = |accessors: Vec<String>| accessors.iter().any(|f| f == name);
    HOST_FN_NAMES.contains(&name)
        || STRATEGY_FN_NAMES.contains(&name)
        || is_callable(name)
        || vike_indicators::registry()
            .iter()
            .any(|m| line_of(line_accessors(m.name).into_iter().map(|(_, f)| f).collect()))
        || installed_user_bare_call(name)
        || installed_user_indicators().iter().any(|i| {
            line_of(installed_user_line_accessors(i).into_iter().map(|(_, f)| f).collect())
        })
}

/// `src` parsed by the strategy engine's sandbox ([`sandboxed_engine`]), nothing run: `Err` is
/// rhai's own parse error. A name the host does not bind is not a parse error (rhai resolves a call
/// when it runs), so this judges syntax alone.
pub fn parse_check(src: &str) -> Result<(), ScriptError> {
    sandboxed_engine(SandboxLimits::SCRIPT, "strategy")
        .compile(src)
        .map(drop)
        .map_err(ScriptError::from)
}
