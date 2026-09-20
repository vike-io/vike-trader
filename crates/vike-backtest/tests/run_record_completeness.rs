//! **Every field of a backtest's result is persisted, or is declared dropped with a reason.**
//!
//! The defect this whole run-artifact stage fixed was that a run's result was computed, folded into
//! ten scalars, and dropped. The way that comes back is a NEW field on `BacktestResult` that the
//! producer never learns about: nothing breaks, nothing goes red, and one more channel quietly
//! stops reaching disk. Six diagnostic counters were in exactly that state — and the design
//! document that named the loss named three of the dropped things and was a channel short.
//!
//! # Why textual
//!
//! Rust cannot enumerate a struct's fields at runtime without a derive macro, and adding one to
//! `vike_analytics::result::BacktestResult` — plain data in a crate whose whole argument is that it
//! depends on `vike-model` alone — is a larger change than the problem. This is the shape
//! `crates/vike-ops/tests/settings_registry.rs` and
//! `crates/vike-buildinfo/tests/identity_adoption.rs` already use: read the real tree, compare
//! against a declared table, fail with the two legitimate responses named.
//!
//! # ⚠ Both paths below are PATH-KEYED
//!
//! A rename of either file reddens this TWICE — a stale row here, and a scan that finds nothing.
//! `git grep` the old path and the old basename before renaming, which is the repo-wide rule the
//! root `CLAUDE.md` states under *Conventions that will bite you if ignored*.

use std::path::{Path, PathBuf};

/// The type whose fields must all be accounted for.
const RESULT_FILE: &str = "crates/vike-analytics/src/result.rs";

/// The producer that turns one into the persisted documents.
const PRODUCER_FILE: &str = "crates/vike-backtest/src/backtest_cli.rs";

/// Fields that are deliberately NOT carried into a run record, each with the reason.
///
/// ⚠ A row here is a CLAIM that the field is recoverable or meaningless on disk, not a place to put
/// something you have not wired yet. Every one below is recoverable from a document that IS written.
const DROPPED_WITH_REASON: &[(&str, &str)] = &[
    (
        "final_equity",
        "the LAST sample of `equity_curve`, which `decimate` keeps unconditionally — and it is \
         written verbatim in `report.json` besides",
    ),
    (
        "n_trades",
        "`RunTrades::source_len` is the same number from the same vector, and `report.json` carries \
         it too",
    ),
    (
        "per_symbol_pnl",
        "written verbatim in `report.json` — `BacktestReport::per_symbol_pnl` is a straight clone \
         of it, and the report is readable back now",
    ),
    (
        "funding_paid",
        "written verbatim in `report.json` — `BacktestReport::funding_paid` is a straight copy",
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `pub <name>:` declared inside `BacktestResult`'s braces, in declaration order.
///
/// Deliberately narrow: it opens at the struct header and stops at the first line that is a lone
/// `}`, so a sibling type in the same file contributes nothing.
fn result_fields(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("pub struct BacktestResult") {
            inside = true;
            continue;
        }
        if inside {
            if t == "}" {
                break;
            }
            // A let-CHAIN rather than two nested `if let`s: clippy's `collapsible_if` refuses the
            // nested spelling at `-D warnings`, which is the merge gate. Edition 2024.
            if let Some(rest) = t.strip_prefix("pub ")
                && let Some((name, _)) = rest.split_once(':')
            {
                out.push(name.trim().to_string());
            }
        }
    }
    out
}

/// `text` with the whitespace around every `.` removed, so a field access the formatter broke
/// across lines reads as one token.
///
/// ⚠ Load-bearing rather than cosmetic: `run_series_from`'s real body breaks TWO accesses
/// (`per_symbol_equity: result` ⏎ `.per_symbol_curves` and `dropped: result` ⏎ `.dropped`), and
/// without this join those two fields would look unaccounted and the gate would fail on working
/// code — which teaches people to weaken the matcher.
fn join_field_accesses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '.' {
            while out.ends_with(char::is_whitespace) {
                out.pop();
            }
            out.push('.');
            continue;
        }
        if c.is_whitespace() && out.ends_with('.') {
            continue;
        }
        out.push(c);
    }
    out
}

/// Is `needle` present in `hay` as a WHOLE IDENTIFIER, rather than as a substring of a longer one?
///
/// ⚠ **A plain `contains` is the wrong test and its failure is silent** — under it a field named
/// `warm` is accounted for by the existing `warmup`, and `equity` by `equity_curve`. Used here on
/// the `result.<field>` form rather than on a bare name; see
/// [`every_backtest_result_field_is_persisted_or_declared_dropped`] for why that distinction is the
/// one that matters.
fn contains_ident(hay: &str, needle: &str) -> bool {
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let start = from + i;
        let end = start + needle.len();
        let before_ok = !hay[..start].chars().next_back().is_some_and(is_word);
        let after_ok = !hay[end..].chars().next().is_some_and(is_word);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// The body of one `fn <name>(` in `text`, from its signature to the first line that is a lone `}`.
fn fn_body(text: &str, name: &str) -> String {
    // ⚠ `fn <name>` then `(` OR `<`. A needle carrying the paren matches NOTHING on a GENERIC
    // function, and the harvest then returns an empty body — a gate that has gone blind rather
    // than one that fails. Neither producer is generic today; `runs.rs`'s twin harvester met
    // exactly this on `write_run_with<R>`, so the landmine is removed here rather than left for
    // the day one of these grows a type parameter.
    let needle = format!("fn {name}");
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if !inside {
            // ⚠ The line must BE a definition, not merely mention one. The floor below
            // catches a ZERO harvest; it cannot catch a MIS-ANCHORED one, and this file
            // already contains the literal `fn write_run_with(` inside a comment. Anchored
            // there, the scan would sweep a region that happens to contain every name it
            // looks for, and the assertion would pass while measuring nothing.
            let def = line.trim_start();
            let is_definition = def.starts_with("fn ") || def.starts_with("pub fn ");
            // ONE condition, not three nested `if`s: clippy's `collapsible_if` refuses the
            // nested spelling at `-D warnings`, which is the merge gate. Edition 2024
            // let-chains are what make the whole test one expression.
            if is_definition
                && let Some(i) = line.find(&needle)
                && matches!(line[i + needle.len()..].chars().next(), Some('(') | Some('<'))
            {
                inside = true;
            }
            continue;
        }
        if line == "}" {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn every_backtest_result_field_is_persisted_or_declared_dropped() {
    let root = repo_root();
    let result_text = std::fs::read_to_string(root.join(RESULT_FILE))
        .unwrap_or_else(|e| panic!("cannot read {RESULT_FILE}: {e} — was it renamed?"));
    let producer_text = std::fs::read_to_string(root.join(PRODUCER_FILE))
        .unwrap_or_else(|e| panic!("cannot read {PRODUCER_FILE}: {e} — was it renamed?"));

    let fields = result_fields(&result_text);
    assert!(
        fields.len() >= 10,
        "the field harvest saw {} fields in {RESULT_FILE} — a scanner that has gone blind passes \
         this whole gate by seeing nothing",
        fields.len()
    );

    // ⚠ **The accounting matches `result.<field>`, not a bare identifier, and that is the whole
    // difference between a gate and a formality.** The producer's own destination fields and local
    // bindings are BARE whole identifiers — `equity`, `stride`, `schema`, `source_len`, `symbol`,
    // `reason`, `size`, `weight`, `sym`, `curve` — every one of which is also a plausible
    // `BacktestResult` field name. Under a bare-identifier match, a later stage adding
    // `pub equity: Vec<f64>` to the result, or renaming `equity_curve` to `equity` (which is the
    // name `RunSeries` ALREADY uses for that quantity), would pass this gate green while reaching
    // no run record. Requiring the `result.` prefix admits only an actual READ of the result, and
    // it also retires the `len` residual an earlier spelling had to declare: `.len()` is a whole
    // `len` token, `result.len` is not.
    let persisted = join_field_accesses(&format!(
        "{}{}",
        fn_body(&producer_text, "run_series_from"),
        fn_body(&producer_text, "run_trades_from")
    ));
    assert!(
        contains_ident(&persisted, "result.equity_curve"),
        "the producer-body harvest found nothing in {PRODUCER_FILE} — `run_series_from` and \
         `run_trades_from` are what this gate reads, and one of them was renamed or reshaped"
    );

    let mut unaccounted = Vec::new();
    for field in &fields {
        // ⚠ `result.<field>` — an actual READ of the result, not a token that happens to appear.
        if contains_ident(&persisted, &format!("result.{field}")) {
            continue;
        }
        if DROPPED_WITH_REASON.iter().any(|(name, _)| name == field) {
            continue;
        }
        unaccounted.push(field.clone());
    }

    assert!(
        unaccounted.is_empty(),
        "these `BacktestResult` fields reach NO run record and are declared nowhere:\n  {}\n\n\
         A field a run computes and never persists is the exact defect the run-artifact stage \
         fixed: nothing breaks, nothing goes red, and one more channel quietly stops reaching \
         disk. Two legitimate responses, in order of preference:\n  \
         1. carry it — `run_series_from` or `run_trades_from` in {PRODUCER_FILE}, and add a field \
         to `vike_model::runs::RunSeries` or `RunDiagnostics` if it needs one;\n  \
         2. add a row to DROPPED_WITH_REASON in this file, naming the document it IS recoverable \
         from. Not a place to park unfinished wiring.",
        unaccounted.join("\n  ")
    );
}

#[test]
fn no_stale_dropped_rows() {
    let root = repo_root();
    let result_text = std::fs::read_to_string(root.join(RESULT_FILE)).unwrap();
    let fields = result_fields(&result_text);

    let stale: Vec<&str> = DROPPED_WITH_REASON
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !fields.iter().any(|f| f == name))
        .collect();

    assert!(
        stale.is_empty(),
        "these DROPPED_WITH_REASON rows name fields `BacktestResult` no longer has:\n  {}\n\n\
         An exemption that has stopped being real is a lie the next reader believes — delete the \
         row.",
        stale.join("\n  ")
    );
}

/// The harvester's own proof, over planted text rather than the tree: a gate whose scanner silently
/// stopped matching would pass every assertion above by seeing nothing, and the floor in the first
/// test is a blunt instrument beside this.
#[test]
fn the_field_harvest_reads_a_struct_and_stops_at_its_closing_brace() {
    let planted = "\
#[derive(Debug, Clone, Default)]
pub struct BacktestResult {
    /// a doc comment
    pub trades: Vec<Trade>,
    pub equity_curve: Vec<f64>,
    #[serde(default)]
    pub warmup: usize,
}

pub struct SomethingElse {
    pub not_mine: u8,
}
";

    assert_eq!(
        result_fields(planted),
        vec!["trades".to_string(), "equity_curve".to_string(), "warmup".to_string()],
        "the harvest must take every pub field of the struct and nothing from its neighbour"
    );
}

/// ⚠ **The accounting match, pinned on the REAL producer's token shapes rather than a convenient
/// planted one.** An earlier spelling proved whole-identifier matching on a body containing none of
/// the hazards that actually exist: `run_series_from`'s destinations and locals are BARE whole
/// identifiers (`equity`, `stride`, `schema`, `source_len`, `sym`, `curve`), so a bare-identifier
/// match accounted for a `BacktestResult` field of any of those names without the producer reading
/// it. The `result.` prefix is what refuses them — and it retires the `.len()` residual too.
#[test]
fn a_field_is_accounted_for_only_by_an_actual_read_of_the_result() {
    // The real shape, two accesses broken across lines exactly as rustfmt leaves them.
    let body = join_field_accesses(
        "\
        equity,
        equity_ts: keep_at_stride(&result.equity_ts, stride),
        per_symbol_equity: result
            .per_symbol_curves
            .iter()
            .map(|(sym, curve)| (sym.clone(), keep_at_stride(curve, stride))),
        stride,
        source_len: result.equity_curve.len(),
        schema: runs::SERIES_SCHEMA,
        dropped: result
            .dropped
            .iter(),
",
    );

    // Read through the result -> ACCOUNTED, including the two the formatter broke across lines.
    for field in ["equity_ts", "per_symbol_curves", "equity_curve", "dropped"] {
        assert!(
            contains_ident(&body, &format!("result.{field}")),
            "`result.{field}` is a real read and must be accounted for"
        );
    }

    // Present as a BARE token but never read off the result -> NOT accounted. Every one of these is
    // a plausible `BacktestResult` field name, which is the point.
    for field in ["equity", "stride", "schema", "source_len", "sym", "curve", "len", "warm"] {
        assert!(
            !contains_ident(&body, &format!("result.{field}")),
            "`{field}` appears as a bare token but is NOT read off the result — accounting for it \
             would pass a field that reaches no run record"
        );
    }

    // ...and the substring rule still holds on the prefixed form.
    assert!(!contains_ident(&body, "result.equity_curv"), "a prefix of a real field is not it");
    assert!(!contains_ident(&body, "result.droppe"));
}

/// ⚠ **THE BOUND'S WIRING, which nothing else proves — and the gap is the same class this file
/// exists for.**
///
/// `run_fingerprint`'s `a_commit_log_over_the_bound_becomes_a_prefix_that_declares_its_true_length`
/// proves truncation INSIDE `bound_commits`. Nothing proved the collector CALLS it:
/// `backtest_cli`'s `every_series_the_collector_produces_obeys_the_commit_bound` drives a bar
/// profile against an EMPTY store, so every `commits` is `[]` and `0 <= MAX_COMMIT_KEYS` holds
/// whatever the call site does — it passes VACUOUSLY. A later stage inlining the struct literal as
/// `commits_len: commits.len(), commits` compiles, keeps the primitive's test green, and restores
/// the unbounded multi-megabyte commit log in every run directory that the bound was added to stop.
///
/// A store holding hundreds of commit keys is what a non-textual proof needs and is not something a
/// unit test can cheaply build — so this closes it the way the rest of this file already works: by
/// reading the producer's own source. Same `PRODUCER_FILE` constant, same `fn_body` harvester, same
/// path-keyed rename hazard the module doc states.
#[test]
fn the_collector_bounds_the_commit_log_rather_than_inlining_it() {
    let root = repo_root();
    let producer_text = std::fs::read_to_string(root.join(PRODUCER_FILE))
        .unwrap_or_else(|e| panic!("cannot read {PRODUCER_FILE}: {e} — was it renamed?"));

    let body = fn_body(&producer_text, "collect_data_fingerprint");

    // The floor, twice over: the harvest must have anchored on the right function, and a scan that
    // found nothing must not pass by finding nothing.
    // ⚠ The anchor is `series_facts`, not `series_commits`. The collector asked the store two
    // separate questions until `DataFusionHist::series_facts` merged them into ONE manifest parse —
    // which is what let the SEARCH path call this collector for the price of the data witness it
    // was already paying, and so what gave a search run an input address at all.
    assert!(
        contains_ident(&body, "series_facts"),
        "the harvest of `collect_data_fingerprint` in {PRODUCER_FILE} found no `series_facts` — \
         it anchored on the wrong thing, or the function was renamed or reshaped"
    );

    assert!(
        contains_ident(&body, "bound_commits"),
        "`collect_data_fingerprint` does not call `bound_commits`, so a series' ingest commit log \
         reaches the run manifest UNBOUNDED. On a grouped store that log is the whole venue's flush \
         log — roughly 2,880 keys/day at the recorder's 30s age bound, ~6 MB of JSON for a 30-day \
         window — written into EVERY run directory, against a ~600 KB-per-run budget.\n\n  \
         Call `run_fingerprint::bound_commits(commits)` and take BOTH halves (the prefix and the \
         true count); do not inline `commits_len: commits.len()` beside an untruncated `commits`, \
         which is the shape that compiles and silently undoes the bound."
    );
}
