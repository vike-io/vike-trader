//! The process-environment scanner: direct `env::var` call sites, map lookups, const resolution.

use std::collections::BTreeMap;

use super::calls::{preceded_by_fn_keyword, starts_identifier};
use super::imports::{QUALIFIED_ENV_READS, imported_env_read_patterns};
use super::lexer::{prev_significant, take_balanced};
use super::*;

/// Every `env::var(..)` / `env::var_os(..)` call site in `source`, comments excluded. A match is
/// only accepted at a word boundary — the byte immediately before `env` must be absent (start of
/// file) or not `[A-Za-z0-9_]` — so an unrelated `my_env::var(..)` is not mistaken for the real
/// call. A bare or aliased spelling counts too, but only where the file's own `use` declarations
/// bring it into scope: see [`imported_env_read_patterns`] for what that costs and why the
/// unconditional version was rejected.
///
/// # What the widening still cannot resolve
///
/// Declared rather than implied, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `the_shapes_the_store_scanner_cannot_see` is the precedent for writing a blind spot down as
/// something executable rather than as a sentence that rots:
///
///   * **A glob import.** `use std::env::*;` then a bare `var("VIKE_X")` brings the name in without
///     naming it, so there is nothing to harvest. Deliberately NOT handled by falling back to an
///     unconditional bare search — that trades one blind spot for eight false demands in
///     `crates/bridges/vike-ibkr/src/config.rs` alone. Nothing in this workspace glob-imports
///     `std::env`.
///   * **A re-export.** `some_crate::var("VIKE_X")`, where the wrapper crate re-exports the std
///     function, reaches the same read through a path this file has no `use std::env` line to key
///     on. [`find_calls`] is the tool for that shape — it keys on a READER'S NAME — and it is how
///     the credential store's wrapper family is already covered.
///   * **The function as a VALUE.** `let read = std::env::var; read("VIKE_X")` names the function
///     without calling it and calls it through a local. Closing that needs dataflow, which needs a
///     parser, which this scanner deliberately is not.
///   * **A COMPUTED name**, which is unchanged by any of this: the argument, not the callee, is
///     what `resolve_arg` cannot resolve, and `DYNAMIC_ALLOWLIST` is where those live.
pub fn find_env_reads(source: &str) -> Vec<EnvRead> {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let mut out: Vec<(usize, EnvRead)> = Vec::new();
    let imported = imported_env_read_patterns(&clean);
    let patterns = QUALIFIED_ENV_READS.iter().map(|p| (*p).to_string()).chain(imported);
    for pat in patterns {
        let mut from = 0usize;
        while let Some(hit) = clean[from..].find(&pat) {
            let at = from + hit;
            let open = at + pat.len();
            // `starts_identifier` alone is right for the qualified spellings — a `::` path cannot
            // be a definition or a method call. The bare ones need both extra checks: `fn var(` in
            // a file that also imports the std one is a DEFINITION, and `self.var(` is somebody's
            // method however the file spells its imports.
            let accepted = starts_identifier(bytes, at)
                && !preceded_by_fn_keyword(bytes, at)
                && prev_significant(bytes, at) != Some(b'.');
            if accepted && let Some(arg) = take_balanced(&clean[open..]) {
                let line = clean[..at].bytes().filter(|b| *b == b'\n').count() + 1;
                out.push((open, EnvRead { arg, line }));
            }
            from = open;
        }
    }
    // ONE call site, ONE row. The patterns genuinely overlap once a file imports the bare name:
    // `var(` also matches the TAIL of `env::var(`, and `starts_identifier` accepts it there because
    // the byte before it is a `:`. The key is the ARGUMENT-LIST offset rather than the name's,
    // because that is what the two spellings share — the name offsets differ by the qualifier's
    // length, so keying on them deduplicates nothing. Keying on `(line, arg)` instead would have
    // been wrong in the other direction: two REAL reads of the same variable on one line are two
    // reads.
    out.sort_by_key(|(open, _)| *open);
    out.dedup_by_key(|(open, _)| *open);
    let mut out: Vec<EnvRead> = out.into_iter().map(|(_, r)| r).collect();
    out.sort_by_key(|r| r.line);
    out
}

/// How a call-site argument resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A concrete variable name. `konst` is `Some(ident)` when the name reached `env::var`
    /// through a `const IDENT: &str`, `None` when the call site used a literal.
    Name { name: String, konst: Option<String> },
    /// Computed or parameterised — the gate requires an explicit allowlist entry.
    Dynamic,
}

/// Every `const IDENT: &str = "VALUE";` (also `&'static str`) declared in `source`.
pub fn const_table(source: &str) -> BTreeMap<String, String> {
    let clean = strip_comments(source);
    let mut out = BTreeMap::new();
    for line in clean.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub const ").or_else(|| line.strip_prefix("const "))
        else {
            continue;
        };
        let Some((ident, tail)) = rest.split_once(':') else { continue };
        let ident = ident.trim();
        if !ident.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            continue;
        }
        let Some((ty, value)) = tail.split_once('=') else { continue };
        let ty = ty.trim();
        if ty != "&str" && ty != "&'static str" {
            continue;
        }
        let value = value.trim().trim_end_matches(';').trim();
        if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            out.insert(ident.to_string(), inner.to_string());
        }
    }
    out
}

/// Resolve one raw call-site argument against the file's constants.
///
/// Returns an OWNED `konst` identifier rather than `settings::Naming`: `Naming` is a `Copy`
/// registry type holding `&'static str`, and scanned text is not `'static`. Keeping the scan
/// output owned avoids leaking a `Box` per constant just to satisfy that lifetime; the gate
/// compares this `Option<String>` against the registry's `Naming::Konst(ident)` itself.
pub fn resolve_arg(arg: &str, consts: &BTreeMap<String, String>) -> Resolved {
    let arg = arg.trim();
    if let Some(inner) = arg.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
        && !inner.contains('"')
    {
        return Resolved::Name { name: inner.to_string(), konst: None };
    }
    let ident = arg.rsplit("::").next().unwrap_or(arg).trim();
    if let Some(value) = consts.get(ident) {
        return Resolved::Name { name: value.clone(), konst: Some(ident.to_string()) };
    }
    Resolved::Dynamic
}

/// Prefixes that mark a SCREAMING_SNAKE literal as one of OUR environment variables.
///
/// Load-bearing: because `find_map_lookups` cannot anchor on a call syntax (keys are passed to
/// helpers), shape alone would swallow venue wire constants — `"PARTIALLY_FILLED"`,
/// `"GOOD_TIL_CANCELLED"`, `"POST_ONLY"` are all SCREAMING_SNAKE strings in the bridge crates.
/// The prefix gate is what separates a knob from a protocol token. Adding a variable under a
/// NEW prefix means adding it here — and the exhaustiveness gate failing is how you find out.
const ENV_PREFIXES: &[&str] = &[
    "VIKE_",
    "POLY_",
    "POLYMARKET_",
    "BINANCE_",
    "BYBIT_",
    "OKX_",
    "DERIBIT_",
    "ASTER_",
    "HYPERLIQUID_",
    "ALPACA_",
    "IG_",
    "OANDA_",
    "FXCM_",
    "DUKASCOPY_",
    // The Dukascopy sidecar's own namespace, distinct from `DUKASCOPY_` (which is the venue's
    // login credentials) — `JFOREX_BRIDGE_JAR` names the JForex jar. It arrived the day that read
    // stopped being a direct `env::var("JFOREX_BRIDGE_JAR")`, which `find_env_reads` resolved
    // without needing any prefix, and became a map lookup, which only this sweep can see: exactly
    // the "adding a variable under a NEW prefix means adding it here — and the exhaustiveness gate
    // failing is how you find out" case above, observed rather than imagined.
    "JFOREX_",
    "CTRADER_",
    "IBKR_",
    "IBAPI_",
    "DATABENTO_",
    "TARDIS_",
    // The Studio chat pane's two AI-provider keys (`ANTHROPIC_API_KEY`, `CEREBRAS_API_KEY`), looked
    // up in the credential map the desktop hands `vike_studio::ChatApiKeys::resolve`. Until these
    // two prefixes joined, that read was INVISIBLE here: no registry row, no sighting, and so
    // `vike-cli secrets set` could not be taught the names. The `ANTHROPIC_API_KEY` row of
    // `vike-agent-eval` (an `env::var` read) never needed a prefix; this lookup does.
    "ANTHROPIC_",
    "CEREBRAS_",
    "PMXT_",
    "EOD_",
    "GAMMA_",
    "RUST_LOG",
    "JAVA_HOME",
    "FCSDK_DIR",
];

/// Whole names, not prefixes: short foreign names a prefix match would over-collect.
///
/// The first four are the PLATFORM's own directory variables. These are the OS's names, not ours,
/// so by construction no [`ENV_PREFIXES`] entry can ever match one — which made every INJECTED
/// read of them invisible to [`find_map_lookups`]. That was a real blind spot, not a theoretical
/// one: a resolver reading `HOME` then `USERPROFILE` out of a caller-supplied map had NO registry
/// row at all, because the only pattern that could see it was the prefix sweep. Direct
/// `env::var("HOME")` reads were always observed (`find_env_reads` resolves the argument and
/// needs no prefix), so the registry looked complete while the injected half of the same variable
/// was unobservable.
///
/// Whole-name matching rather than a `"HOME"` PREFIX entry is deliberate: a prefix would also
/// swallow `HOME_DIR`, `HOMEPATH`, `USERPROFILE_OVERRIDE` and any future look-alike, and the
/// separation between a knob and a protocol token is exactly what [`ENV_PREFIXES`] exists to keep.
///
/// `CARGO` joined for the identical reason, from CARGO's own namespace rather than the OS's:
/// `crates/vike-strategy-builder/src/bin/vike-strategy-builder.rs`'s read of it stopped being a
/// direct `env::var("CARGO")` (which `find_env_reads` resolved without needing any prefix) and
/// became a `vars.get("CARGO")` map lookup instead, threaded down as `render::build_plugin`'s
/// `cargo_bin` parameter. A PREFIX entry would have been actively wrong here, not merely broad:
/// `CARGO_MANIFEST_DIR`, `CARGO_TARGET_DIR`, `CARGO_HOME` and every other cargo-provided
/// `CARGO_*` build-script variable already appear as literals throughout this tree (several
/// already correctly classified `Layer::BuildScript`), and a `"CARGO"` prefix would have swept
/// all of them into this scan too.
const ENV_EXACT_NAMES: &[&str] = &["HOME", "USERPROFILE", "XDG_DATA_HOME", "LOCALAPPDATA", "CARGO"];

/// Env-var name shape AND a known prefix (or one of the [`ENV_EXACT_NAMES`]). Narrow on purpose —
/// see [`ENV_PREFIXES`].
fn is_env_name(s: &str) -> bool {
    s.len() >= 3
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && (ENV_PREFIXES.iter().any(|p| s.starts_with(p)) || ENV_EXACT_NAMES.contains(&s))
}

/// Injected-map key names in this file, sorted and deduped.
///
/// Catches the consumers (`vike_tradehub::reconcile_config`, the venue `config.rs` loaders,
/// `vike_bridge_core::key_permissions`) that read a caller-supplied map instead of process
/// env — the pattern `find_env_reads` cannot see.
///
/// THREE shapes, all found in the tree and all required (a `.get("LITERAL")`-only scanner was
/// tried first and silently missed the majority):
///   - `vars.get("VIKE_THING")`                    — literal key
///   - `vars.get(THING_ENV)`                       — key through a `const`, e.g.
///     `crates/bridges/polymarket/src/exec_plane/recon_client.rs`'s `poly_reconcile_enabled`
///   - `parse_i64(vars, "VIKE_THING", 3_600_000)`  — key passed to a helper, e.g.
///     `crates/vike-tradehub/src/reconcile_config.rs`'s `build_recon_config`; in that ONE file only
///     4 of 11 keys sit at a `.get(` site, so anchoring on `.get(` would have dropped 7.
///
/// The third shape means we cannot anchor on a call syntax at all, so we take EVERY string
/// literal (and every resolvable `const`) whose value has env-var shape AND a known prefix.
/// The prefix gate is what keeps venue wire constants like `"PARTIALLY_FILLED"` out.
pub fn find_map_lookups(source: &str, consts: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();

    // Shape 1 & 3: bare string literals anywhere in the file. The sweep itself lives in
    // `string_literals` so the credential-store scanner reuses this one tracker rather than
    // growing a second with its own quote-parity bugs.
    out.extend(string_literals(source).into_iter().filter(|lit| is_env_name(lit)));

    // Shape 2: `something.get(CONST)` where CONST resolves to an env-shaped name. Delegated to
    // `find_lookup_sites` so the two cannot drift: that function is BY CONSTRUCTION a subset of
    // this one, which is the property `find_map_lookups_contains_every_lookup_site` pins.
    out.extend(find_lookup_sites(source, consts));

    out.into_iter().collect()
}

/// Injected-map keys resolved at a REAL `.get(..)` call site — the PRECISE subset of
/// [`find_map_lookups`], and the only positive evidence of a map read this scanner can produce.
///
/// [`find_map_lookups`] deliberately answers the looser question "does this file MENTION an
/// env-shaped name", because the third read shape passes the key to a helper
/// (`parse_i64(vars, "VIKE_X", 3_600_000)`) and leaves no call syntax to anchor on. That
/// imprecision is load-bearing THERE — it is what keeps the exhaustiveness gate from missing the
/// majority of injected reads — but it makes a sighting worthless as proof that a lookup happens:
/// a file whose only mention is an `env::var("VIKE_X")` ARGUMENT reports the very same name, and
/// so does a `const X_ENV: &str = "VIKE_X";` declaration next to no read at all.
///
/// This function answers the narrower question the naming gate needs: is there a `.get(KEY)` whose
/// KEY resolves to an env-shaped name? A `.get(` is a real call site, so a hit is positive
/// evidence. MISSES ARE EXPECTED and are the blind spot the gate MEASURES rather than assumes —
/// see `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `map_lookup_proof_is_pinned`.
///
/// No false positive is possible despite `.get(` being ubiquitous (`slice.get(0)`, `map.get(&id)`,
/// `headers.get("content-type")`): the RESOLVED value must still pass [`is_env_name`], which
/// demands SCREAMING_SNAKE shape AND one of our known prefixes.
pub fn find_lookup_sites(source: &str, consts: &BTreeMap<String, String>) -> Vec<String> {
    let clean = strip_comments(source);
    let mut out = std::collections::BTreeSet::new();
    let mut from = 0usize;
    while let Some(hit) = clean[from..].find(".get(") {
        let open = from + hit + ".get(".len();
        if let Some(arg) = take_balanced(&clean[open..]) {
            // `&"X"` / `&KEY` are both common at a map lookup; `resolve_arg` handles the literal
            // and the `const` (including a `path::QUALIFIED` one) once the borrow is off.
            let arg = arg.trim().trim_start_matches('&').trim().to_string();
            if let Resolved::Name { name, .. } = resolve_arg(&arg, consts)
                && is_env_name(&name)
            {
                out.insert(name);
            }
        }
        from = open;
    }
    out.into_iter().collect()
}
