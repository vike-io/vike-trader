//! `WIRED_HOOKS ∪ UNWIRED_HOOKS` must be EXACTLY the `vike_model::Strategy` seam, derived from
//! the trait itself rather than remembered.
//!
//! Without this, a hook added to `Strategy` joins the SILENT set the day it is declared: it is
//! not in `PluginVTable`, so a plugin never receives it, and it is not in `UNWIRED_HOOKS`, so
//! `vike_strategy_builder::render::build_plugin` does not refuse a source that overrides it. The
//! result is the failure both lists exist to end — a strategy whose behaviour is present under
//! one mechanism and absent under the other, with no signal at any layer. That failure was live
//! in this tree until the lists landed, and the comment that used to stand in for them asserted
//! the opposite ("a build/test would surface immediately"), which is why the roster is derived
//! here instead of being asserted in prose again.
//!
//! Text-only: no cargo, no reflection, no new dependency. It reads the trait's own source and
//! takes the method names out of it, the same shape every gate in `crates/vike-ops/tests` uses.

use std::collections::BTreeSet;
use std::path::PathBuf;

use vike_strategy_plugin::host::{UNWIRED_HOOKS, WIRED_HOOKS};

/// The trait's declaration site. A relative hop from this crate's own manifest directory — a
/// `tests/` file resolving a repo path, which
/// `crates/vike-ops/tests/compile_time_path_gate.rs` never walks and its doc names as always
/// correct.
fn strategy_trait_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vike-model")
        .join("src")
        .join("strategy")
        .join("mod.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the Strategy trait at {}: {e}", path.display()))
}

/// Every method name declared in the `pub trait Strategy<B: Broker>` block.
///
/// The block is delimited by BRACE BALANCE from the header's own `{`, not by a blank line or an
/// indentation guess, so a method declared after a doc comment carrying braces is still inside it
/// and a method of the NEXT item is still outside it.
fn declared_seam_methods(src: &str) -> BTreeSet<String> {
    let header = "pub trait Strategy<B: Broker>";
    let at = src.find(header).expect("the Strategy trait must still be declared with this header");
    let open = at + src[at..].find('{').expect("the trait header must be followed by its body");
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut end = open;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(end > open, "the Strategy trait body must be brace-balanced");

    let mut out = BTreeSet::new();
    for line in src[open..end].lines() {
        let trimmed = line.trim_start();
        // A DECLARATION line only: `fn name(` at the start of the trimmed line. A `fn` inside a
        // doc comment starts with `///`, and a nested closure or an `fn` in a default body is
        // indented behind other text rather than beginning the line's content.
        let Some(rest) = trimmed.strip_prefix("fn ") else { continue };
        let Some(paren) = rest.find('(') else { continue };
        let name = rest[..paren].trim();
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.insert(name.to_string());
        }
    }
    assert!(out.len() > 5, "the extraction found only {out:?}, which cannot be the whole trait");
    out
}

#[test]
fn the_two_hook_lists_partition_the_strategy_trait_exactly() {
    let declared = declared_seam_methods(&strategy_trait_source());
    let wired: BTreeSet<String> = WIRED_HOOKS.iter().map(|s| (*s).to_string()).collect();
    let unwired: BTreeSet<String> = UNWIRED_HOOKS.iter().map(|s| (*s).to_string()).collect();

    let overlap: Vec<&String> = wired.intersection(&unwired).collect();
    assert!(overlap.is_empty(), "a hook cannot be both wired and unwired: {overlap:?}");

    let claimed: BTreeSet<String> = wired.union(&unwired).cloned().collect();

    let unclassified: Vec<&String> = declared.difference(&claimed).collect();
    assert!(
        unclassified.is_empty(),
        "`vike_model::Strategy` declares {unclassified:?}, which neither \
         `vike_strategy_plugin::host::WIRED_HOOKS` nor `UNWIRED_HOOKS` names. Until it is \
         classified, a user strategy overriding it is silently ignored under the plugin \
         mechanism and honoured under the build-time tier — the exact divergence those lists \
         exist to refuse. Add it to WIRED_HOOKS if `PluginVTable` carries it, to UNWIRED_HOOKS \
         otherwise."
    );

    let stale: Vec<&String> = claimed.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "{stale:?} is named by a hook list but is no longer a `vike_model::Strategy` method — \
         delete the row. A stale row makes `build_plugin` refuse a source over a hook that does \
         not exist."
    );
}

/// The lists are only worth anything if the WIRED half is the truth about `PluginVTable`. Checked
/// against that struct's own declaration rather than against memory of it — the same failure
/// shape as above, one layer down.
#[test]
fn every_wired_hook_is_actually_a_plugin_vtable_field() {
    let host = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("host.rs"),
    )
    .expect("this crate's own host.rs must be readable");
    let at = host.find("pub struct PluginVTable {").expect("PluginVTable must still be declared");
    let body_end = at + host[at..].find("\n}").expect("PluginVTable must be brace-terminated");
    let body = &host[at..body_end];
    for hook in WIRED_HOOKS {
        assert!(
            body.contains(&format!("pub {hook}:")),
            "`WIRED_HOOKS` claims `{hook}` is carried by `PluginVTable`, and that struct has no \
             such field. A wired hook that is not really wired means `build_plugin` permits a \
             source whose hook is then silently dropped."
        );
    }
}
