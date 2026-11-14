//! Pins each roster file's header `[params]` against the file's own reader — in BOTH directions,
//! over the file's own source. A header's `[params]` is the deleted registry's `PARAM_KEYS` row of
//! that strategy, moved into the file; this gate moved with it, from `vike-strategy`'s tests,
//! re-pointed at the shipped files (the roster: `param_gates/roster.rs`, shared with
//! `param_gates.rs`).
//!
//! # Why a source scan and not a hand list
//!
//! A header's `[params]` is a DECLARATION, and a declaration nobody checks drifts while staying
//! green. Both directions of the drift are harmful, DIFFERENTLY:
//!
//!   - **Declared, never read** — a live mount ACCEPTS `qtyy` and the strategy runs at its compiled
//!     default anyway: the silent-default hazard the declaration exists to remove.
//!   - **Read, never declared** — a live mount REFUSES a legitimate profile, and the operator
//!     cannot tell a refusal-by-omission from a real typo.
//!
//! # What it actually reads
//!
//! Each roster file is its OWN reader: a strategy file is one program and looks every key up
//! inside itself (its `from_params`, plus its own helpers: `barriers_from_params` and
//! `controller_harness` in the two controller files, `read_rungs`/`read_side` in the two ladder
//! files), so the deleted gate's per-name list of the source files a name's keys were read in is
//! the roster itself now. [`keys_read`] collects every string literal at a LOOKUP call site —
//! `params.get("x")` and the local `f("x")` / `i("x")` / `ms("x")` closures — after truncating the
//! file at its `#[cfg(test)]` module, dropping every indented test-only item above it and dropping
//! whole-line comments, so a doc table or a test fixture naming a key is never mistaken for a read.
//!
//! # ...and the TYPE, from the same sites
//!
//! A declared `vike_strategy::file::ParamType` is a claim about the ACCESSOR, so [`types_read`]
//! resolves it: the `.and_then(…)` right after a `get("x")` site, or the accessors in a closure
//! callee's own definition. Markers map onto the `ParamValue::type_str` kind names
//! (`ParamValue::as_f64` ⇒ float|integer, `ParamValue::as_integer` ⇒ integer, …) and UNION across
//! sites — how `read_side`'s two arms come out as `text_or_integer`.
//! An unresolvable site is a HARD failure ([`every_lookup_site_resolves_to_an_accessor`]), never
//! a silent skip.
//!
//! # The residual the move closed
//!
//! Direction 2 was checked at FILE granularity, and one library file held both `Grid` and
//! `DcaAccumulate`, so a key read by one and declared by the other passed. Each is its own file now,
//! so file granularity IS strategy granularity.
//!
//! [`the_scanner_can_actually_fail`] is the mutation self-test: a scanner nobody proved can fail is
//! vacuously green.

use std::collections::{BTreeMap, BTreeSet};

// `param_gates.rs` compiles the same file: a shared test module, so an `allow`.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "param_gates/roster.rs"]
mod roster;

use roster::ROSTER;

/// A resolved lookup site: the TOML type names its accessor accepts, or `None` when the accessor
/// could not be resolved at all.
type SiteTypes = Option<BTreeSet<&'static str>>;

/// The lookup callees a key literal may sit at: `params.get("x")` plus the reader closures over it
/// (`f` f64, `i` i64, `ms` epoch-ms). ⚠ A reader spelled any other way is not SEEN, so its key
/// fails direction 1 ("declared but never read") — a message naming the wrong defect.
const LOOKUP_CALLEES: [&str; 4] = ["get", "f", "i", "ms"];

/// The scannable half of a source file: everything above its test module (its first
/// `#[cfg(test)]` at the start of a line), every INDENTED `#[cfg(test)]` item above it dropped
/// whole, and whole-line `//` comments dropped — else doc tables and test fixtures, full of key
/// literals, read as reads.
///
/// ⚠ The indented half came with the files: a file is ONE file, so the test-only child of an
/// inline module (`strategies/pairs/pairs_zscore.rs`'s `spread_quote_tests`) sits ABOVE the
/// reader, and stopping at it, as the library-era scan did at any `#[cfg(test)]`, would hide the
/// reader itself.
fn reader_source(src: &str) -> String {
    let mut kept = Vec::new();
    // Inside an indented test-only item: the braces it has opened and closed so far.
    // shortcut: braces are counted as text, so a lone `{` in a string of a test-only item
    // miscounts; tokenize the item if one ever does.
    let mut test_item: Option<(usize, usize)> = None;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if line.starts_with("#[cfg(test)]") {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if test_item.is_none() && trimmed.starts_with("#[cfg(test)]") {
            test_item = Some((0, 0));
        }
        if let Some((opened, closed)) = &mut test_item {
            // The attribute's own line counts too: `#[cfg(test)] fn f() {}` ends where it starts.
            let rest = trimmed.strip_prefix("#[cfg(test)]").unwrap_or(trimmed);
            *opened += rest.matches('{').count();
            *closed += rest.matches('}').count();
            if *closed >= *opened && (rest.contains('}') || rest.trim_end().ends_with(';')) {
                test_item = None;
            }
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

/// The TOML type names an accessor SNIPPET accepts, by the markers in it. Union, because a snippet
/// may be a closure body naming several (`v.as_float().or_else(|| v.as_integer()…)` is exactly the
/// `as_f64` convention spelled out). Empty ⇒ this snippet names no accessor at all.
fn accessor_types(snippet: &str) -> BTreeSet<&'static str> {
    let mut out = BTreeSet::new();
    // `ParamValue::as_f64` is the ONE lenient numeric read — float OR integer. The marker is the
    // bare method name, so the path it is spelled through does not matter.
    if snippet.contains("as_f64") {
        out.insert("float");
        out.insert("integer");
    }
    for (marker, ty) in [
        ("as_float", "float"),
        ("as_integer", "integer"),
        ("as_str", "string"),
        ("as_bool", "boolean"),
        ("as_table", "table"),
    ] {
        if snippet.contains(marker) {
            out.insert(ty);
        }
    }
    out
}

/// The balanced-paren argument text of the call starting at `open` (which must index the `(`).
fn balanced_arg(src: &str, open: usize) -> Option<&str> {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open + 1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The reader closures a file defines over `params.get` (`let f = |k: &str| …;`), mapped to the
/// TOML types their body accepts: `f("qty")`'s accessor is at the closure's DEFINITION.
fn closure_types(src: &str) -> BTreeMap<String, BTreeSet<&'static str>> {
    let bytes = src.as_bytes();
    let mut out = BTreeMap::new();
    let needle = "= |k: &str|";
    let mut at = 0usize;
    while let Some(rel) = src[at..].find(needle) {
        let eq = at + rel;
        at = eq + needle.len();
        // The identifier before ` = ` is the closure's name.
        let mut end = eq;
        while end > 0 && bytes[end - 1] == b' ' {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        // ...and the body runs to the `;` that ends the `let` (no reader's closure body holds one).
        let body_end = src[at..].find(';').map(|r| at + r).unwrap_or(src.len());
        out.insert(src[start..end].to_string(), accessor_types(&src[at..body_end]));
    }
    out
}

/// Every params LOOKUP SITE in `src`: the key literal that is the sole argument of a
/// [`LOOKUP_CALLEES`] call, paired with the TOML types its accessor accepts (pure, so the
/// self-tests feed it known cases). A `get("x")` site's accessor is the `.and_then(…)` right after
/// it; any other shape resolves to `None` and fails [`every_lookup_site_resolves_to_an_accessor`].
fn lookup_sites(src: &str) -> Vec<(String, SiteTypes)> {
    let bytes = src.as_bytes();
    let closures = closure_types(src);
    let mut found = Vec::new();
    let mut at = 0usize;
    while let Some(rel) = src[at..].find("(\"") {
        let open = at + rel;
        at = open + 2;
        // The identifier immediately before the `(` is the callee.
        let mut start = open;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        let callee = &src[start..open];
        if !LOOKUP_CALLEES.contains(&callee) {
            continue;
        }
        // ...and the literal must be a plain snake_case key closed by `")`.
        let Some(end_rel) = src[at..].find('"') else { break };
        let key = &src[at..at + end_rel];
        let closed = src[at + end_rel..].starts_with("\")");
        if !closed
            || key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            continue;
        }
        let types = if callee == "get" {
            let tail = &src[at + end_rel + 2..];
            let trimmed = tail.trim_start();
            trimmed
                .strip_prefix(".and_then")
                .and_then(|rest| balanced_arg(tail, tail.len() - rest.len()))
                .map(accessor_types)
                .filter(|t| !t.is_empty())
        } else {
            closures.get(callee).filter(|t| !t.is_empty()).cloned()
        };
        found.push((key.to_string(), types));
    }
    found
}

/// Every params key literal read in `src` — [`lookup_sites`] with the types dropped.
fn keys_read(src: &str) -> BTreeSet<String> {
    lookup_sites(src).into_iter().map(|(k, _)| k).collect()
}

/// Key -> the union of TOML types its lookup sites accept, over one file (an unresolved site
/// contributes nothing here; its own test catches it).
fn types_read(src: &str) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut out: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for (key, types) in lookup_sites(src) {
        out.entry(key).or_default().extend(types.unwrap_or_default());
    }
    out
}

/// The keys `file`'s header declares, with the TOML types each declaration claims.
fn declared_for_file(file: &str) -> BTreeMap<String, BTreeSet<&'static str>> {
    roster::header(file)
        .params
        .into_iter()
        .map(|(key, declaration)| {
            (key, declaration.r#type.accepted().iter().copied().collect::<BTreeSet<_>>())
        })
        .collect()
}

/// The roster and the shipped files must partition the same way: a file whose header these gates
/// hold to its reader is on the roster, every other shipped file outside the gallery is an `OUT`
/// row with its reason (it declares nothing this gate checks), and no file is both.
#[test]
fn the_reader_map_matches_the_table() {
    let unclassified = roster::unclassified();
    assert!(
        unclassified.is_empty(),
        "the shipped files and this gate's roster disagree: {unclassified:#?}. A file whose header \
         declares the keys it reads must be on the roster, or nothing checks it; a file left out \
         must be an `OUT` row with its reason, or the gate would check an empty claim."
    );
    assert!(!ROSTER.is_empty(), "the roster is empty — the check would be vacuous");
}

/// Direction 1 — **declared, never read**. The dangerous direction: a bogus key here is one a live
/// mount ACCEPTS, and the knob it names then sits at the strategy's compiled default.
#[test]
fn every_declared_key_is_actually_read_by_its_reader() {
    for file in ROSTER {
        let read_keys = keys_read(&reader_source(&roster::source(file)));
        for (key, _) in roster::header(file).params {
            assert!(
                read_keys.contains(&key),
                "`{file}`'s header declares `{key}`, but the file never looks it up. A consumer \
                 would ACCEPT that key and the strategy would run at its compiled default — the \
                 silent-default hazard the header exists to remove. Delete the key, or read it."
            );
        }
    }
}

/// The TYPE half of direction 1 — **declared at a type the reader does not take**:
///
///   - **too WIDE** (`rungs` declared `number`, `read_rungs` is `ParamValue::as_integer`) — a live
///     mount ACCEPTS `rungs = 4.0` and the count sits at its compiled default;
///   - **too NARROW** (`qty` declared `integer`, the reader is the lenient `as_f64`) — a live mount
///     REFUSES the legitimate `qty = 0.005`.
#[test]
fn every_declared_key_type_matches_its_reader() {
    for file in ROSTER {
        let observed = types_read(&reader_source(&roster::source(file)));
        for (key, declaration) in roster::header(file).params {
            let ty = declaration.r#type;
            let want: BTreeSet<&'static str> = ty.accepted().iter().copied().collect();
            let got = observed.get(&key).cloned().unwrap_or_default();
            assert_eq!(
                got, want,
                "`{file}`'s header declares `{key}` as {ty:?} (accepting {want:?}), but the file \
                 looks it up through accessors accepting {got:?}. Declaring MORE than the reader \
                 takes lets a live mount accept a value that sets nothing; declaring LESS refuses a \
                 profile that would have worked. Fix whichever of the two is wrong."
            );
        }
    }
}

/// Every lookup site must resolve to an accessor: a site the scanner cannot read is a type claim
/// NOTHING checks.
#[test]
fn every_lookup_site_resolves_to_an_accessor() {
    for file in ROSTER {
        for (key, types) in lookup_sites(&reader_source(&roster::source(file))) {
            assert!(
                types.is_some(),
                "`{file}` looks up `{key}` in a shape this gate cannot resolve to an accessor \
                 (expected `params.get(\"{key}\").and_then(…)`, or a `|k: &str|` reader closure). \
                 Its declared type would then be unchecked. Write the lookup in the shape the \
                 other readers use, or teach `lookup_sites` the new one."
            );
        }
    }
}

/// Direction 2 — **read, never declared**: a live mount would refuse a legitimate profile, and the
/// refusal is indistinguishable from a typo.
#[test]
fn every_key_a_reader_looks_up_is_declared() {
    for file in ROSTER {
        let observed_types = types_read(&reader_source(&roster::source(file)));
        let declared = declared_for_file(file);
        for (key, observed) in &observed_types {
            let Some(want) = declared.get(key) else {
                panic!(
                    "`{file}` reads the params key `{key}`, which its header does not declare. A \
                     live mount would REFUSE a profile that sets it, and the operator cannot tell \
                     that from a typo. Declare it in the file's header."
                )
            };
            assert_eq!(
                observed, want,
                "`{file}` reads `{key}` through accessors accepting {observed:?}, but its header \
                 declares {want:?}. The two must be the same set — see \
                 every_declared_key_type_matches_its_reader for why each direction hurts."
            );
        }
    }
}

/// The floor: every file this gate reads is a real reader with real lookups in it, so the gate
/// cannot pass by reading nothing.
#[test]
fn the_gate_has_a_non_empty_input() {
    for file in ROSTER {
        let src = reader_source(&roster::source(file));
        assert!(
            src.contains("from_params") || src.contains("controller_harness"),
            "`{file}` holds no params reader — this gate is pointed at the wrong file"
        );
        assert!(
            !keys_read(&src).is_empty(),
            "`{file}` yields no params keys at all — the scan is vacuous for it"
        );
    }
    assert!(
        ROSTER.len() >= 5,
        "the roster has almost no files left — this gate would check nearly nothing"
    );
}

/// The mutation self-test: [`keys_read`] and [`reader_source`] say YES to what they must catch and
/// NO to the false positives prose-heavy, table-documented readers would produce — else the two
/// directions above could be vacuously green forever.
#[test]
fn the_scanner_can_actually_fail() {
    // Real lookups, in every spelling the readers use.
    assert!(keys_read("params.get(\"symbol\")").contains("symbol"));
    assert!(keys_read("f(\"anchor_price\").unwrap_or(d.anchor_price)").contains("anchor_price"));
    assert!(keys_read("i(\"exit_delay_ms\").unwrap_or(2000)").contains("exit_delay_ms"));
    assert!(keys_read("ms(\"tau_hold_ms\")").contains("tau_hold_ms"));
    // NOT a lookup: a different callee taking a string.
    assert!(
        !keys_read("panic!(\"qty\"); write!(f, \"step\")").contains("qty"),
        "a string handed to some other call is not a params key"
    );
    // NOT a lookup: a VALUE the reader matches on (`read_side` compares against \"short\").
    assert!(
        keys_read("s.eq_ignore_ascii_case(\"short\")").is_empty(),
        "a compared-against literal is not a key — its callee is not a lookup"
    );
    // Doc tables spell every key; a doc line must never read as a read.
    assert!(
        keys_read(&reader_source("/// | `qty` | size | `1.0` |\n// params.get(\"qty\")"))
            .is_empty(),
        "a commented-out or documented key must not be read as a read"
    );
    // ...and the test module below `#[cfg(test)]` is full of key literals in fixtures.
    assert!(
        keys_read(&reader_source(
            "let q = params.get(\"qty\");\n#[cfg(test)]\nmod tests { let _ = p.get(\"bogus\"); }"
        )) == BTreeSet::from(["qty".to_string()]),
        "the scan must stop at the test module"
    );
    // ...and real code above it must still be seen, or the gate is decorative.
    assert!(
        !keys_read("let q = params.get(\"qty\");").is_empty(),
        "a genuine lookup must still be found"
    );
    // An INDENTED test-only item (an inline module's test child, the `pairs_zscore.rs` shape) is
    // dropped whole, and the reader BELOW it is still seen: it is not the end of the scan.
    assert_eq!(
        keys_read(&reader_source(
            "mod quote {\n    #[cfg(test)]\n    mod quote_tests {\n        let _ = p.get(\"bogus\");\n    \
             }\n}\nlet q = params.get(\"qty\");"
        )),
        BTreeSet::from(["qty".to_string()]),
        "an indented test module is skipped, never scanned and never the end of the scan"
    );
    // ...and so is a one-line one, which must not swallow what follows it.
    assert_eq!(
        keys_read(&reader_source(
            "    #[cfg(test)]\n    use x::y;\n    #[cfg(test)] fn f() { p.get(\"bogus\"); }\n\
             let q = params.get(\"qty\");"
        )),
        BTreeSet::from(["qty".to_string()]),
        "a one-line test-only item ends where it ends"
    );
}

/// The TYPE half of the mutation self-test: [`lookup_sites`] reads the real accessor out of every
/// shape the readers use, UNIONS a two-arm key, and reports a DIFFERENT set for another accessor.
#[test]
fn the_type_scanner_can_actually_fail() {
    let ty = |src: &str, key: &str| -> BTreeSet<&'static str> {
        types_read(src).get(key).cloned().unwrap_or_default()
    };
    let set = |v: &[&'static str]| -> BTreeSet<&'static str> { v.iter().copied().collect() };

    // The lenient numeric convention is TWO TOML types, not one.
    assert_eq!(
        ty("params.get(\"tp\").and_then(ParamValue::as_f64),", "tp"),
        set(&["float", "integer"])
    );
    // ...and a strict integer accessor is one, even with a `.map` chained after it.
    assert_eq!(
        ty(
            "params.get(\"rungs\").and_then(ParamValue::as_integer).map(|i| i.max(0) as usize)",
            "rungs"
        ),
        set(&["integer"])
    );
    // Every other accessor the readers use.
    assert_eq!(
        ty("params.get(\"symbol\").and_then(ParamValue::as_str)", "symbol"),
        set(&["string"])
    );
    assert_eq!(
        ty("params.get(\"bounded01\").and_then(ParamValue::as_bool)", "bounded01"),
        set(&["boolean"])
    );
    assert_eq!(
        ty("params.get(\"venues\").and_then(ParamValue::as_table)", "venues"),
        set(&["table"])
    );
    // A MULTI-LINE site (the `pairs_zscore.rs` shape) resolves the same way.
    assert_eq!(
        ty(
            "params\n    .get(\"period\")\n    .and_then(ParamValue::as_f64)\n    .map(|v| v as usize)",
            "period"
        ),
        set(&["float", "integer"])
    );
    // TWO sites on one key UNION — this is `read_side`, and it is why `text_or_integer` exists.
    assert_eq!(
        ty(
            "if let Some(s) = params.get(\"side\").and_then(ParamValue::as_str) { }\n\
             if let Some(i) = params.get(\"side\").and_then(ParamValue::as_integer) { }",
            "side"
        ),
        set(&["integer", "string"])
    );
    // A closure callee resolves from the closure's DEFINITION — both spellings the readers use.
    assert_eq!(
        ty(
            "let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);\nlet q = f(\"qty\");",
            "qty"
        ),
        set(&["float", "integer"])
    );
    assert_eq!(
        ty(
            "let i = |k: &str| params.get(k).and_then(ParamValue::as_integer);\n\
             i(\"exit_delay_ms\").unwrap_or(2000),",
            "exit_delay_ms"
        ),
        set(&["integer"])
    );
    // ...including the BLOCK form, whose body spells `as_f64` out longhand (no reader is written so
    // since the lenient-read copies became `ParamValue::as_f64`; the scanner still reads it).
    assert_eq!(
        ty(
            "let f = |k: &str| {\n    params.get(k).and_then(|v| v.as_float().or_else(|| \
             v.as_integer().map(|i| i as f64)))\n};\nf(\"half_spread\").unwrap_or(0.01),",
            "half_spread"
        ),
        set(&["float", "integer"])
    );

    // THE MUTATION: the same key at a DIFFERENT accessor must come back a different set, or
    // `every_declared_key_type_matches_its_reader` could never fail.
    assert_ne!(
        ty("params.get(\"size\").and_then(ParamValue::as_str)", "size"),
        ty("params.get(\"size\").and_then(ParamValue::as_f64)", "size"),
        "a string accessor and a numeric one must not resolve alike"
    );
    assert_ne!(
        ty("params.get(\"rungs\").and_then(ParamValue::as_integer)", "rungs"),
        ty("params.get(\"rungs\").and_then(ParamValue::as_f64)", "rungs"),
        "an integer-only accessor and the lenient numeric one must not resolve alike — this is the \
         `rungs = 4.0` case the whole type gate exists for"
    );

    // An UNRESOLVABLE site is reported as such rather than passing as an empty (and therefore
    // silently satisfiable) set.
    assert_eq!(lookup_sites("params.get(\"x\").cloned()"), vec![("x".to_string(), None)]);
    assert_eq!(lookup_sites("g(\"x\")"), Vec::new(), "a non-lookup callee is not a site at all");
    assert_eq!(
        lookup_sites("let f = |k: &str| params.get(k).cloned();\nf(\"y\")"),
        vec![("y".to_string(), None)],
        "a closure naming no accessor leaves its call sites unresolved"
    );
}
