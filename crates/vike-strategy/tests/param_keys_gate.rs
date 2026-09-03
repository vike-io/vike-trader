//! Pins [`vike_strategy::PARAM_KEYS`] against the readers it describes — in BOTH directions, over
//! the readers' own source.
//!
//! # Why a source scan and not a hand list
//!
//! `PARAM_KEYS` is a DECLARATION, and this repo's scars say a declaration nobody checks drifts while
//! staying green (`crates/vike-ops/tests/citation_gate.rs` and the `LIBRARY_PIN` ratchet exist for
//! the same reason). Both directions of the drift are harmful and they are harmful DIFFERENTLY:
//!
//!   - **Declared, never read** — the table says `qtyy` is a real knob, so a live mount ACCEPTS it
//!     and the strategy runs at its compiled default anyway. That is the exact silent-default hazard
//!     `PARAM_KEYS` exists to remove, wearing the table's own authority.
//!   - **Read, never declared** — somebody adds a knob to a reader; the table does not know, so a
//!     live mount REFUSES a legitimate profile. Loud, but wrong, and the operator has no way to tell
//!     a refusal-by-omission from a real typo.
//!
//! # What it actually reads
//!
//! Each row of [`READERS`] names the source files a strategy's params are read in — its own
//! `from_params` plus the shared helpers it calls (`barriers_from_params` in `controller.rs`,
//! `read_rungs`/`read_side` in `grid_dca.rs`, `controller_harness` in `registry.rs`). [`keys_read`]
//! collects every string literal sitting at a LOOKUP call site — `params.get("x")` and the local
//! `f("x")` / `i("x")` / `ms("x")` closures every reader in this crate defines over it — after the
//! file is truncated at its `#[cfg(test)]` module and stripped of whole-line comments, so a doc
//! table naming a key in prose is never mistaken for a read.
//!
//! # ...and the TYPE, from the same sites
//!
//! A key's declared [`vike_strategy::ParamType`] is a claim about the ACCESSOR its lookup goes
//! through, so [`types_read`] resolves that too: the `.and_then(…)` argument immediately after a
//! `get("x")` site, or — for a closure callee — the accessors in that closure's own definition. The
//! markers map onto the TOML vocabulary (`as_f64` ⇒ float|integer, `as_integer` ⇒ integer, …) and
//! UNION across sites, which is how `read_side`'s two arms (`as_str` and `as_integer` on the same
//! key) come out as the `StrOrInteger` the table declares. A site whose accessor cannot be resolved
//! is a HARD failure ([`every_lookup_site_resolves_to_an_accessor`]) rather than a silent skip: an
//! unresolved site is a type claim nothing checks, which is the shape of gate this repo has been
//! bitten by three times.
//!
//! # The residual, stated rather than hidden
//!
//! Direction 2 is checked at FILE granularity: `grid_dca.rs` holds both `Grid` and `DcaAccumulate`,
//! so a key really read by one and declared by the other passes. Narrowing that would need
//! per-function slicing of bodies that call shared free helpers ANYWAY (`read_rungs` is outside both
//! impls), which buys precision by making the scanner guess. The keys that could hide in that
//! residual are already listed in each type's own doc table and are covered behaviourally by
//! `registry.rs`'s `a_declared_key_actually_moves_the_strategy`.
//!
//! [`the_scanner_can_actually_fail`] is the mutation self-test: a scanner nobody proved can fail is
//! how finding 3 of this PR's review happened.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use vike_strategy::{ParamKeys, PARAM_KEYS, PORTABLE_STRATEGIES};

/// A resolved lookup site: the TOML type names its accessor accepts, or `None` when the accessor
/// could not be resolved at all.
type SiteTypes = Option<BTreeSet<&'static str>>;

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every gate in this
/// repo uses.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// One row per [`ParamKeys::Declared`] name: the repo-relative source files its params are read in.
/// A `NotEnumerated` row has NO entry here by construction (`the_reader_map_matches_the_table`).
const READERS: &[(&str, &[&str])] = &[
    ("buy_hold", &["crates/vike-strategy/src/registry.rs"]),
    ("grid", &["crates/vike-strategy/src/grid_dca.rs"]),
    ("dca_accumulate", &["crates/vike-strategy/src/grid_dca.rs"]),
    ("trailing_scalper", &["crates/vike-strategy/src/trailing_scalper.rs"]),
    (
        "momentum",
        &["crates/vike-strategy/src/controller.rs", "crates/vike-strategy/src/registry.rs"],
    ),
    (
        "funding_carry",
        &[
            "crates/vike-strategy/src/funding_carry.rs",
            "crates/vike-strategy/src/controller.rs",
            "crates/vike-strategy/src/registry.rs",
        ],
    ),
    ("funding_capture", &["crates/vike-strategy/src/funding_capture.rs"]),
    ("pairs_zscore", &["crates/vike-strategy/src/pairs.rs"]),
];

/// The lookup callees a key literal may sit at: `params.get("x")` plus the one-letter/two-letter
/// closures every reader in this crate defines over `params.get` (`f` for f64, `i` for i64, `ms` for
/// epoch-ms). Adding a new spelling to a reader without adding it here shows up as a direction-2
/// MISS, not as a silent pass — the key would simply not be seen, and the row that declares it would
/// then fail direction 1.
const LOOKUP_CALLEES: [&str; 4] = ["get", "f", "i", "ms"];

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{rel}`: {e}"))
}

/// The scannable half of a source file: everything above its `#[cfg(test)]` module, with whole-line
/// `//` comments dropped. Both halves are load-bearing — the readers' doc tables spell every key in
/// prose, and their test modules build params tables full of key literals, so an unfiltered scan
/// would report documentation and fixtures as reads.
fn reader_source(src: &str) -> String {
    src.lines()
        .take_while(|l| !l.trim_start().starts_with("#[cfg(test)]"))
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The TOML type names an accessor SNIPPET accepts, by the markers in it. Union, because a snippet
/// may be a closure body naming several (`v.as_float().or_else(|| v.as_integer()…)` is exactly the
/// `as_f64` convention spelled out). Empty ⇒ this snippet names no accessor at all.
fn accessor_types(snippet: &str) -> BTreeSet<&'static str> {
    let mut out = BTreeSet::new();
    // `as_f64` is the workspace's lenient numeric reader — float OR integer — not a TOML accessor.
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

/// The one-letter reader closures a file defines over `params.get` (`let f = |k: &str| …;`),
/// mapped to the TOML types their body accepts. This is how `f("qty")` resolves to a type at all —
/// the accessor is at the closure's DEFINITION, not at its call site.
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
/// [`LOOKUP_CALLEES`] call, paired with the TOML types its accessor accepts. Pure, so
/// [`the_scanner_can_actually_fail`] can feed it known cases.
///
/// The accessor for a `get("x")` site is the `.and_then(…)` that immediately follows (the ONLY
/// shape any reader in this crate uses — a site in another shape resolves to `None` and fails
/// [`every_lookup_site_resolves_to_an_accessor`] rather than passing unchecked); for a closure
/// callee it is that closure's own definition.
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

/// Key -> the union of TOML types its lookup sites accept, over one file. An unresolved site
/// contributes nothing here (it is caught by its own test), so this map never silently narrows.
fn types_read(src: &str) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut out: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for (key, types) in lookup_sites(src) {
        out.entry(key).or_default().extend(types.unwrap_or_default());
    }
    out
}

/// The declared keys of every name whose reader set includes `file`, each with the TOML types its
/// row claims. ⚠ Two names sharing a file must agree about a shared key's type — asserted here
/// rather than silently unioned, because disagreeing rows would make the file-granular direction-2
/// check accept either.
fn declared_for_file(file: &str) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut keys: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for (name, files) in READERS {
        if !files.contains(&file) {
            continue;
        }
        if let Some(ParamKeys::Declared(k)) = vike_strategy::param_keys(name) {
            for (key, ty) in k.iter() {
                let want: BTreeSet<&'static str> = ty.accepted().iter().copied().collect();
                if let Some(seen) = keys.get(*key) {
                    assert_eq!(
                        seen, &want,
                        "two strategies reading `{file}` declare `{key}` at DIFFERENT types \
                         ({seen:?} vs {want:?}); one reader cannot take both, so one row is wrong"
                    );
                }
                keys.insert((*key).to_string(), want);
            }
        }
    }
    keys
}

/// [`READERS`] and [`PARAM_KEYS`] must partition the roster the same way: a `Declared` row owes a
/// reader set, a `NotEnumerated` row must not have one (it declares nothing to check).
#[test]
fn the_reader_map_matches_the_table() {
    for name in PORTABLE_STRATEGIES {
        let declared = matches!(vike_strategy::param_keys(name), Some(ParamKeys::Declared(_)));
        let mapped = READERS.iter().any(|(n, _)| n == name);
        assert_eq!(
            declared, mapped,
            "{name}: PARAM_KEYS says declared={declared} but this gate's READERS map says \
             mapped={mapped}. A Declared row must name the files its keys are read in, or nothing \
             checks it; a NotEnumerated row must not, or the gate would check an empty claim."
        );
    }
    for (name, files) in READERS {
        assert!(!files.is_empty(), "{name} has an empty reader set — the check would be vacuous");
    }
}

/// Direction 1 — **declared, never read**. The dangerous direction: a bogus key here is one a live
/// mount ACCEPTS, and the knob it names then sits at the strategy's compiled default.
#[test]
fn every_declared_key_is_actually_read_by_its_reader() {
    for (name, files) in READERS {
        let Some(ParamKeys::Declared(keys)) = vike_strategy::param_keys(name) else {
            panic!("{name} is in READERS but has no Declared row — see the_reader_map_matches_the_table")
        };
        let read_keys: BTreeSet<String> =
            files.iter().flat_map(|f| keys_read(&reader_source(&read(f)))).collect();
        for (key, _) in *keys {
            assert!(
                read_keys.contains(*key),
                "PARAM_KEYS declares `{key}` for `{name}`, but no reader in {files:?} looks it up. \
                 A consumer would ACCEPT that key and the strategy would run at its compiled \
                 default — the silent-default hazard this table exists to remove. Delete the key, \
                 or point the row at the file that reads it."
            );
        }
    }
}

/// The TYPE half of direction 1 — **declared at a type the reader does not take**. Wrong in both
/// directions, and both are live hazards wearing the table's authority:
///
///   - **too WIDE** (`rungs` declared `Number` where `read_rungs` is `Value::as_integer`) — a live
///     mount ACCEPTS `rungs = 4.0` and the count sits at its compiled default. That is the exact
///     defect this whole table exists to remove, one level down from the key.
///   - **too NARROW** (`qty` declared `Integer` where the reader is the lenient `as_f64`) — a live
///     mount REFUSES `qty = 0.005`, a legitimate profile, and the operator cannot tell a
///     refusal-by-declaration from a real mistake.
///
/// The expected set is read off the reader's own accessor, so a row can only be right by being true.
#[test]
fn every_declared_key_type_matches_its_reader() {
    for (name, files) in READERS {
        let Some(ParamKeys::Declared(keys)) = vike_strategy::param_keys(name) else {
            panic!("{name} is in READERS but has no Declared row — see the_reader_map_matches_the_table")
        };
        let mut observed: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
        for file in *files {
            for (key, types) in types_read(&reader_source(&read(file))) {
                observed.entry(key).or_default().extend(types);
            }
        }
        for (key, ty) in *keys {
            let want: BTreeSet<&'static str> = ty.accepted().iter().copied().collect();
            let got = observed.get(*key).cloned().unwrap_or_default();
            assert_eq!(
                got, want,
                "PARAM_KEYS declares `{key}` for `{name}` as {ty:?} (accepting {want:?}), but the \
                 readers in {files:?} look it up through accessors accepting {got:?}. Declaring \
                 MORE than the reader takes lets a live mount accept a value that sets nothing; \
                 declaring LESS refuses a profile that would have worked. Fix whichever of the two \
                 is wrong."
            );
        }
    }
}

/// Every lookup site must resolve to an accessor — a site the scanner cannot read is a type claim
/// NOTHING checks, and a gate that silently skips what it cannot parse is the vacuous-gate failure
/// this repo has already hit three times.
#[test]
fn every_lookup_site_resolves_to_an_accessor() {
    let files: BTreeSet<&str> = READERS.iter().flat_map(|(_, f)| f.iter().copied()).collect();
    for file in files {
        for (key, types) in lookup_sites(&reader_source(&read(file))) {
            assert!(
                types.is_some(),
                "`{file}` looks up `{key}` in a shape this gate cannot resolve to an accessor \
                 (expected `params.get(\"{key}\").and_then(…)`, or a `|k: &str|` reader closure). \
                 Its declared ParamType would then be unchecked. Write the lookup in the shape the \
                 other readers use, or teach `lookup_sites` the new one."
            );
        }
    }
}

/// Direction 2 — **read, never declared**. A knob added to a reader without a table row makes a
/// live mount refuse a legitimate profile, and the refusal is indistinguishable from a typo.
#[test]
fn every_key_a_reader_looks_up_is_declared() {
    let files: BTreeSet<&str> = READERS.iter().flat_map(|(_, f)| f.iter().copied()).collect();
    for file in files {
        let observed_types = types_read(&reader_source(&read(file)));
        let declared = declared_for_file(file);
        for (key, observed) in &observed_types {
            let Some(want) = declared.get(key) else {
                panic!(
                    "`{file}` reads the params key `{key}`, which no PARAM_KEYS row covering that \
                     file declares. A live mount would REFUSE a profile that sets it, and the \
                     operator cannot tell that from a typo. Add it to the row of whichever \
                     strategy reads it."
                )
            };
            assert_eq!(
                observed, want,
                "`{file}` reads `{key}` through accessors accepting {observed:?}, but the \
                 PARAM_KEYS row covering that file declares {want:?}. The two must be the same set \
                 — see every_declared_key_type_matches_its_reader for why each direction hurts."
            );
        }
    }
}

/// The floor: every file this gate reads is a real reader with real lookups in it, so the whole gate
/// cannot pass by reading nothing — the vacuous-gate failure this repo has hit three times.
#[test]
fn the_gate_has_a_non_empty_input() {
    let files: BTreeSet<&str> = READERS.iter().flat_map(|(_, f)| f.iter().copied()).collect();
    for file in files {
        let src = reader_source(&read(file));
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
        PARAM_KEYS.iter().filter(|(_, k)| matches!(k, ParamKeys::Declared(_))).count() >= 5,
        "PARAM_KEYS has almost no Declared rows left — this gate would check nearly nothing"
    );
}

/// The mutation self-test. [`keys_read`] and [`reader_source`] are pure, so prove each says YES to
/// what it must catch and NO to the false positives this crate's prose-heavy, table-documented
/// readers would otherwise produce. Without this the two directions above could be vacuously green
/// forever — which is precisely how a pin in this PR shipped unable to fail.
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
}

/// The TYPE half of the mutation self-test. [`lookup_sites`] is pure, so prove it reads the real
/// accessor out of every shape these readers use, UNIONS a two-arm key, and — the part that makes
/// the gate able to fail — reports a DIFFERENT set when the accessor is different.
#[test]
fn the_type_scanner_can_actually_fail() {
    let ty = |src: &str, key: &str| -> BTreeSet<&'static str> {
        types_read(src).get(key).cloned().unwrap_or_default()
    };
    let set = |v: &[&'static str]| -> BTreeSet<&'static str> { v.iter().copied().collect() };

    // The lenient numeric convention is TWO TOML types, not one.
    assert_eq!(ty("params.get(\"tp\").and_then(as_f64),", "tp"), set(&["float", "integer"]));
    // ...and a strict integer accessor is one, even with a `.map` chained after it.
    assert_eq!(
        ty("params.get(\"rungs\").and_then(Value::as_integer).map(|i| i.max(0) as usize)", "rungs"),
        set(&["integer"])
    );
    // Every other accessor the readers use.
    assert_eq!(ty("params.get(\"symbol\").and_then(Value::as_str)", "symbol"), set(&["string"]));
    assert_eq!(
        ty("params.get(\"bounded01\").and_then(Value::as_bool)", "bounded01"),
        set(&["boolean"])
    );
    assert_eq!(ty("params.get(\"venues\").and_then(Value::as_table)", "venues"), set(&["table"]));
    // A MULTI-LINE site (the `pairs.rs` shape) resolves the same way.
    assert_eq!(
        ty(
            "params\n    .get(\"period\")\n    .and_then(as_f64)\n    .map(|v| v as usize)",
            "period"
        ),
        set(&["float", "integer"])
    );
    // TWO sites on one key UNION — this is `read_side`, and it is why `StrOrInteger` exists.
    assert_eq!(
        ty(
            "if let Some(s) = params.get(\"side\").and_then(Value::as_str) { }\n\
             if let Some(i) = params.get(\"side\").and_then(Value::as_integer) { }",
            "side"
        ),
        set(&["integer", "string"])
    );
    // A closure callee resolves from the closure's DEFINITION — both spellings the readers use.
    assert_eq!(
        ty("let f = |k: &str| params.get(k).and_then(as_f64);\nlet q = f(\"qty\");", "qty"),
        set(&["float", "integer"])
    );
    assert_eq!(
        ty(
            "let i = |k: &str| params.get(k).and_then(toml::Value::as_integer);\n\
             i(\"exit_delay_ms\").unwrap_or(2000),",
            "exit_delay_ms"
        ),
        set(&["integer"])
    );
    // ...including the BLOCK form, whose body spells `as_f64` out longhand.
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
        ty("params.get(\"size\").and_then(Value::as_str)", "size"),
        ty("params.get(\"size\").and_then(as_f64)", "size"),
        "a string accessor and a numeric one must not resolve alike"
    );
    assert_ne!(
        ty("params.get(\"rungs\").and_then(Value::as_integer)", "rungs"),
        ty("params.get(\"rungs\").and_then(as_f64)", "rungs"),
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
