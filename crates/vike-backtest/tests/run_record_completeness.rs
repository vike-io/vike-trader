//! **Every field of a backtest's result is persisted, or is declared dropped with a reason.**
//!
//! The defect this whole run-artifact stage fixed was that a run's result was computed, folded into
//! ten scalars, and dropped. The way that comes back is a NEW field on `TradingRecord` that the
//! producer never learns about: nothing breaks, nothing goes red, and one more channel quietly
//! stops reaching disk. Six diagnostic counters were in exactly that state — and the design
//! document that named the loss named three of the dropped things and was a channel short.
//!
//! # Why textual
//!
//! Rust cannot enumerate a struct's fields at runtime without a derive macro, and adding one to
//! `vike_analytics::TradingRecord` — plain data in a crate whose whole argument is that it
//! depends on `vike-model` alone — is a larger change than the problem. This is the shape
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs` and
//! `crates/vike-buildinfo/tests/identity_adoption.rs` already use: read the real tree, compare
//! against a declared table, fail with the two legitimate responses named.
//!
//! # ⚠ Both paths below are PATH-KEYED
//!
//! A rename of either file reddens this TWICE — a stale row here, and a scan that finds nothing.
//! `git grep` the old path and the old basename before renaming, which is the repo-wide rule the
//! root `CLAUDE.md` states under *Conventions that will bite you if ignored*.

use std::path::{Path, PathBuf};

use vike_model::test_support::text::fn_body;

/// The two types whose fields must all be accounted for: `TradingRecord` and its `TradingRecordSimulator`.
const RESULT_FILE: &str = "crates/vike-analytics/src/record.rs";

/// The producer that turns one into the persisted documents.
const PRODUCER_FILE: &str = "crates/vike-backtesthub/src/backtest_cli/run_record.rs";

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
        "written verbatim in `report.json` — `TradingResults::per_symbol_pnl` is a straight clone \
         of it, and the report is readable back now",
    ),
    (
        "funding_paid",
        "written verbatim in `report.json` — `TradingResults::funding_paid` is a straight copy",
    ),
    (
        "data_warnings",
        "written verbatim in `report.json` — `TradingResults::data_warnings` is a straight clone of \
         it",
    ),
    (
        "closed_trades",
        "written verbatim in `report.json` — `TradingResults::closed_trades` is a straight clone of \
         it",
    ),
    (
        "settle_price",
        "written in `report.json` — `TradingResults::in_dollars` carries it as `price`, beside the \
         final equity it values",
    ),
    (
        "sampling",
        "written in `report.json`: its step as `TradingResults::returns_interval_ms`, its drawdown as \
         `max_drawdown` and its first zero as `invalid`",
    ),
    (
        "mounts",
        "written verbatim in `report.json` — `TradingResults::mounts` is a straight clone of it",
    ),
    (
        "strategy_failures",
        "written verbatim in `report.json` — `TradingResults::strategy_failures` is a straight clone \
         of it",
    ),
];

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-ops/tests/architecture/parity_fold_gate.rs`'s `repo_root`, do not.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// `TradingRecord`'s one field that is not a channel: WHO produced the record. A run directory is a
/// backtest's by construction, and the source's simulator counters are walked as fields of their own.
const NOT_A_CHANNEL: &str = "source";

/// Every field a run record must account for, as the READ the producer must make of it:
/// `result.<field>` for `TradingRecord`'s own fields (but [`NOT_A_CHANNEL`]) and `simulator.<field>`
/// for `TradingRecordSimulator`'s, the counters `run_series_from` reads through `result.simulator()`.
fn record_reads(text: &str) -> Vec<(String, String)> {
    let own = struct_fields(text, "TradingRecord").into_iter().filter(|f| f != NOT_A_CHANNEL);
    let counters = struct_fields(text, "TradingRecordSimulator");
    own.map(|f| (format!("result.{f}"), f))
        .chain(counters.into_iter().map(|f| (format!("simulator.{f}"), f)))
        .collect()
}

/// Every `pub <name>:` declared inside the braces of `pub struct <name> {`, in declaration order.
///
/// Deliberately narrow: it opens at that exact header (so `TradingRecord` does not open at
/// `TradingRecordSimulator`) and stops at the first line that is a lone `}`, so a sibling type in the
/// same file contributes nothing.
fn struct_fields(text: &str, name: &str) -> Vec<String> {
    let header = format!("pub struct {name} {{");
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t == header {
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
/// ⚠ Load-bearing rather than cosmetic: `run_series_from`'s real body breaks an access across lines
/// (`per_symbol_equity: result` ⏎ `.per_symbol_curves`), and without this join that field would
/// look unaccounted and the gate would fail on working code — which teaches people to weaken the
/// matcher.
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

// Unicode word test, as `crates/vike-data/src/test_support/text.rs`'s `contains_token`; the
// ASCII twins (`crates/vike-buildinfo/tests/identity_adoption/scan.rs`'s `contains_ident`) differ.
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

#[test]
fn every_backtest_result_field_is_persisted_or_declared_dropped() {
    let root = repo_root();
    let result_text = std::fs::read_to_string(root.join(RESULT_FILE))
        .unwrap_or_else(|e| panic!("cannot read {RESULT_FILE}: {e} — was it renamed?"));
    let producer_text = std::fs::read_to_string(root.join(PRODUCER_FILE))
        .unwrap_or_else(|e| panic!("cannot read {PRODUCER_FILE}: {e} — was it renamed?"));

    let fields = record_reads(&result_text);
    assert!(
        fields.len() >= 19,
        "the field harvest saw {} fields in {RESULT_FILE} — a scanner that has gone blind passes \
         this whole gate by seeing nothing",
        fields.len()
    );

    // ⚠ **The accounting matches `result.<field>`, not a bare identifier, and that is the whole
    // difference between a gate and a formality.** The producer's own destination fields and local
    // bindings are BARE whole identifiers — `equity`, `stride`, `schema`, `source_len`, `symbol`,
    // `reason`, `size`, `weight`, `sym`, `curve` — every one of which is also a plausible
    // `TradingRecord` field name. Under a bare-identifier match, a later stage adding
    // `pub equity: Vec<f64>` to the result, or renaming `equity_curve` to `equity` (which is the
    // name `RunSeries` ALREADY uses for that quantity), would pass this gate green while reaching
    // no run record. Requiring the `result.` prefix admits only an actual READ of the result, and
    // it also retires the `len` residual an earlier spelling had to declare: `.len()` is a whole
    // `len` token, `result.len` is not.
    let persisted = join_field_accesses(&format!(
        "{}{}{}",
        fn_body(&producer_text, "run_series_from"),
        fn_body(&producer_text, "run_trades_from"),
        fn_body(&producer_text, "run_forecasts_from")
    ));
    assert!(
        contains_ident(&persisted, "result.equity_curve"),
        "the producer-body harvest found nothing in {PRODUCER_FILE} — `run_series_from`, \
         `run_trades_from` and `run_forecasts_from` are what this gate reads, and one of them was \
         renamed or reshaped"
    );

    let mut unaccounted = Vec::new();
    for (read, field) in &fields {
        // ⚠ `result.<field>` / `simulator.<field>` — an actual READ, not a token that happens to appear.
        if contains_ident(&persisted, read) {
            continue;
        }
        if DROPPED_WITH_REASON.iter().any(|(name, _)| name == field) {
            continue;
        }
        unaccounted.push(field.clone());
    }

    assert!(
        unaccounted.is_empty(),
        "these `TradingRecord` fields reach NO run record and are declared nowhere:\n  {}\n\n\
         A field a run computes and never persists is the exact defect the run-artifact stage \
         fixed: nothing breaks, nothing goes red, and one more channel quietly stops reaching \
         disk. Two legitimate responses, in order of preference:\n  \
         1. carry it — `run_series_from`, `run_trades_from` or `run_forecasts_from` in \
         {PRODUCER_FILE}, and add a field to `vike_runs::RunSeries` or `RunDiagnostics` if it \
         needs one;\n  \
         2. add a row to DROPPED_WITH_REASON in this file, naming the document it IS recoverable \
         from. Not a place to park unfinished wiring.",
        unaccounted.join("\n  ")
    );
}

#[test]
fn no_stale_dropped_rows() {
    let root = repo_root();
    let result_text = std::fs::read_to_string(root.join(RESULT_FILE)).unwrap();
    let fields: Vec<String> = record_reads(&result_text).into_iter().map(|(_, f)| f).collect();

    let stale: Vec<&str> = DROPPED_WITH_REASON
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !fields.iter().any(|f| f == name))
        .collect();

    assert!(
        stale.is_empty(),
        "these DROPPED_WITH_REASON rows name fields `TradingRecord` no longer has:\n  {}\n\n\
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
pub struct TradingRecordSimulator {
    pub dropped: Vec<OrderDrop>,
}

#[derive(Debug, Clone, Default)]
pub struct TradingRecord {
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
        struct_fields(planted, "TradingRecord"),
        vec!["trades".to_string(), "equity_curve".to_string(), "warmup".to_string()],
        "the harvest must take every pub field of the struct and nothing from its neighbours, the \
         one whose name begins with it included"
    );
    assert_eq!(struct_fields(planted, "TradingRecordSimulator"), vec!["dropped".to_string()]);
    let reads: Vec<String> = record_reads(planted).into_iter().map(|(read, _)| read).collect();
    assert_eq!(
        reads,
        ["result.trades", "result.equity_curve", "result.warmup", "simulator.dropped"],
        "the record's own fields are read off `result`, the simulator's counters off `simulator`"
    );
}

/// ⚠ **The accounting match, pinned on the REAL producer's token shapes rather than a convenient
/// planted one.** An earlier spelling proved whole-identifier matching on a body containing none of
/// the hazards that actually exist: `run_series_from`'s destinations and locals are BARE whole
/// identifiers (`equity`, `stride`, `schema`, `source_len`, `sym`, `curve`), so a bare-identifier
/// match accounted for a `TradingRecord` field of any of those names without the producer reading
/// it. The `result.` prefix is what refuses them — and it retires the `.len()` residual too.
#[test]
fn a_field_is_accounted_for_only_by_an_actual_read_of_the_result() {
    // The real shape, two accesses broken across lines exactly as rustfmt leaves them.
    let body = join_field_accesses(
        "\
        equity,
        equity_timestamp: keep_at_stride(&result.equity_timestamp, stride),
        per_symbol_equity: result
            .per_symbol_curves
            .iter()
            .map(|(sym, curve)| (sym.clone(), keep_at_stride(curve, stride))),
        stride,
        source_len: result.equity_curve.len(),
        schema: vike_runs::SERIES_SCHEMA,
        dropped: result
            .dropped
            .iter(),
",
    );

    // Read through the result -> ACCOUNTED, including the two the formatter broke across lines.
    for field in ["equity_timestamp", "per_symbol_curves", "equity_curve", "dropped"] {
        assert!(
            contains_ident(&body, &format!("result.{field}")),
            "`result.{field}` is a real read and must be accounted for"
        );
    }

    // Present as a BARE token but never read off the result -> NOT accounted. Every one of these is
    // a plausible `TradingRecord` field name, which is the point.
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
    // separate questions until `HistoryStoreDataFusion::series_facts` merged them into ONE manifest parse —
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
