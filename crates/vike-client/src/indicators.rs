//! The BUILT-IN indicators a Rhai strategy can call, as rows. `crates/vike-cli/src/cmd/indicators.rs`'s
//! `run` answers a human and `crates/vike-mcp/src/offline_tools.rs`'s `tool_list_indicators`
//! answers an agent, and both call [`bound_metas`] and render [`detail_row`], so the two surfaces
//! cannot disagree about what is callable — which is the only property that matters here, because
//! the cost of being wrong is silent.
//!
//! Every row is a name `vike_rhai::RHAI_INDICATORS` carries, or one reachable through a per-line
//! accessor, joined onto its `vike_indicators::registry()` metadata. Printing a registry name the
//! host does NOT bind is the defect: rhai resolves a function name when the line RUNS, so a script
//! naming an unbound indicator compiles, raises on every bar, and self-disables.

use serde_json::{Value, json};
use vike_indicators::IndicatorMeta;

/// THE derivation: every `vike_indicators::registry()` entry whose name the Rhai host binds, in
/// registry order (curated, stable — not alphabetical, so related indicators stay together).
///
/// Filtered by `vike_rhai::RHAI_INDICATORS` rather than reproducing it, so this listing follows
/// the binding automatically. `the_listing_is_exactly_the_host_bound_set` asserts BOTH directions,
/// including the one a filter hides: a bound name with no registry entry would silently vanish
/// from every roster while remaining callable.
pub fn bound_metas() -> Vec<&'static IndicatorMeta> {
    vike_indicators::registry()
        .iter()
        .filter(|m| {
            // Reachable under its BARE name, OR through at least one per-line accessor. The second
            // arm is what keeps `bollinger` in this listing: its bare name is refused (line 0 is the
            // upper band), but `bollinger_mid(20)` is callable, and a roster that omitted it would
            // tell a user that a band indicator is unreachable when it is not.
            vike_rhai::RHAI_INDICATORS.contains(&m.name)
                || !vike_rhai::line_accessors(m.name).is_empty()
        })
        .collect()
}

/// The category name a row reports — the `vike_indicators::Category` variant, which is also what
/// `--category` and the MCP tool's `category` argument match against. ONE spelling, so the filter
/// argument is always a value the listing itself printed.
pub fn category_name(m: &IndicatorMeta) -> String {
    format!("{:?}", m.category)
}

/// One indicator in FULL, as the machine row shared by `--json` and the MCP tool.
///
/// `params` and `outputs` are the indicator's OWN registry surface;
/// `crates/vike-rhai/src/engine/builtin.rs`'s `register_indicators` is the authority for the call shape
/// built over them (today: one form per argument count, from zero up to the parameter count, the
/// omitted ones taking the defaults printed here). Without them a caller choosing a two-parameter
/// indicator cannot know it HAS a second parameter, let alone what leaving it out means — which is
/// the class of silent wrongness this tool's narrowing was introduced to prevent.
pub fn detail_row(m: &IndicatorMeta) -> Value {
    let params: Vec<Value> = m
        .params
        .iter()
        .map(|p| json!({ "name": p.name, "default": p.default, "min": p.min, "max": p.max }))
        .collect();
    let outputs: Vec<&str> = m.outputs.iter().map(|o| o.name).collect();
    // The spelling a script actually types for each line. Absent for a single-output indicator,
    // whose bare name IS its one line; present for every multi-output one, where the bare name is
    // usually refused and the accessor is the only way in.
    let accessors: Vec<Value> = vike_rhai::line_accessors(m.name)
        .into_iter()
        .map(|(line, call)| json!({ "line": line, "call": call }))
        .collect();
    json!({
        "name": m.name,
        "pretty": m.pretty,
        "category": category_name(m),
        "params": params,
        "outputs": outputs,
        "accessors": accessors,
        // Whether the BARE name resolves. `false` with a non-empty `accessors` is the band-indicator
        // shape: reachable, but only line by line. Without this flag a consumer reading `outputs`
        // would have no way to tell `bollinger(20)` from `macd(12, 26, 9)`.
        "bare_call": vike_rhai::RHAI_INDICATORS.contains(&m.name),
    })
}

/// One indicator as the COMPACT row: the two fields a roster needs to be navigable. See
/// `crates/vike-mcp/src/offline_tools.rs`'s `tool_list_indicators` for why the default MCP response is
/// this and not [`detail_row`].
pub fn compact_row(m: &IndicatorMeta) -> Value {
    json!({ "name": m.name, "category": category_name(m) })
}

/// Narrow `rows` to one category, or explain which ones exist.
///
/// An unknown category is an ERROR that names the alternatives rather than an empty listing: an
/// empty answer to `--category momentumm` is indistinguishable from "this build binds nothing in
/// that family", and the second reading is the one that sends somebody hunting through a config.
pub fn filter_by_category(
    rows: &[&'static IndicatorMeta],
    category: &str,
) -> Result<Vec<&'static IndicatorMeta>, String> {
    let wanted = category.trim();
    let hit: Vec<&'static IndicatorMeta> =
        rows.iter().copied().filter(|m| category_name(m).eq_ignore_ascii_case(wanted)).collect();
    if hit.is_empty() {
        let mut known: Vec<String> = rows.iter().map(|m| category_name(m)).collect();
        known.sort_unstable();
        known.dedup();
        return Err(format!("no category '{wanted}' — this build has: {}", known.join(", ")));
    }
    Ok(hit)
}

/// One bound indicator by exact name, or an error that separates the two ways a name can miss.
///
/// "Not in the registry at all" and "a real indicator this host does not bind" are different
/// mistakes with different repairs, and collapsing them into one message is how somebody spends an
/// afternoon on a typo.
pub fn find_bound(name: &str) -> Result<&'static IndicatorMeta, String> {
    let name = name.trim();
    if let Some(m) = bound_metas().into_iter().find(|m| m.name == name) {
        return Ok(m);
    }
    Err(match vike_indicators::get(name) {
        // The host's OWN reason, never a paraphrase: `vike_rhai::unbound_reason` is exported for
        // exactly this, so an advertising surface can say WHY a real indicator is missing instead
        // of silently omitting it. The `unwrap_or` arm is unreachable while every exclusion carries
        // a reason, and is a sentence rather than a panic because a listing is not worth aborting.
        Some(_) => {
            let why = vike_rhai::unbound_reason(name)
                .unwrap_or("this build's Rhai host does not register it");
            format!("'{name}' is a vike-indicators indicator a Rhai script cannot call: {why}")
        }
        None => format!("no indicator named '{name}'"),
    })
}

/// `name(param=default, …)` — the FULL call form, with the indicator's own defaults spelled out.
///
/// Spelling every parameter is what makes the listing worth reading: the host registers one form
/// per argument count, from zero (all defaults) up to the full list, so this row is simultaneously
/// the widest call and the answer to "what do the ones I leave out become".
pub fn call_form(m: &IndicatorMeta) -> String {
    let params: Vec<String> =
        m.params.iter().map(|p| format!("{}={}", p.name, fmt_number(p.default))).collect();
    format!("{}({})", m.name, params.join(", "))
}

/// A parameter default, without the trailing `.0` that makes a period look like a tolerance.
pub fn fmt_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{v:.0}") } else { format!("{v}") }
}
