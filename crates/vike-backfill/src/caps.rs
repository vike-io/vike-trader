//! `caps` — **which historical KINDS each backfill source can serve for each venue.**
//!
//! A per-venue capability map in the sense CLAUDE.md's playbook means: STEP 1 declares today's
//! reality, cites the code each row was read from, pins the whole matrix verbatim, and iterates
//! [`vike_model::VENUES`] so a new venue fails these tests until its rows exist. Nothing here
//! changes behaviour — it makes an existing, undocumented reality answerable before anything runs.
//!
//! ## The finding this table exists to make visible
//!
//! **No venue-direct backfill in this workspace serves `trade` or `book` — at all.** Every
//! `*_backfill` bin that talks to a venue writes `kind=bar`
//! (`binance`/`bybit`/`okx`/`ibkr`/`hyperliquid`/`eod`), except dukascopy, which writes `kind=quote`
//! from `.bi5` ticks and resamples bars from them.
//! Trades and L2 book come ONLY from archives and paid vendors — pmxt, `data.vike.io`, a local
//! ClickHouse capture, Databento, Tardis.
//!
//! That matters because it is exactly inverted from what "backfill from the venue" sounds like, and
//! because the RECORDER writes `trade`/`quote`/`book`. So a hole in a recorded tape generally
//! **cannot** be repaired from the venue: `Backfill::Venue` fills a kind the recorder never wrote,
//! and leaves every kind it did write untouched. A Data Manager that offered "refill from venue" on
//! a book gap without saying so would be promising something no code here can do.
//!
//! Polymarket is the sharpest case and was verified rather than assumed: the bridge implements **no**
//! price-history endpoint at all (no `/prices-history`, no fidelity parameter), so its venue row is
//! empty across every kind.
//!
//! ## What a row is NOT
//!
//! A `true` here means *this source has code that writes that kind for that venue* — the capability.
//! It says nothing about whether a particular window has data, whether credentials are present, or
//! whether the fetch will succeed. Those are runtime answers; this is the compile-time one, which is
//! what lets a caller rule an option out **before** spending an hour on it.
//!
//! ## How much of a row is machine-checked
//!
//! Two of the tests below read the real `src/` tree rather than another declaration, because a row
//! is a human assertion and the pin test compares this table only to a hardcoded copy of ITSELF:
//!
//! * `a_venue_module_that_exists_must_not_claim_it_serves_nothing` — the EXISTENCE gate (#1032).
//!   Fails when `src/<venue>.rs` exists while the `Source::Venue` row says `NONE`. This has gone
//!   wrong three times (hyperliquid, aster, deribit).
//! * `a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes` — the SHAPE gate. Derives
//!   what each venue's code actually writes by scanning it for `HistStore` write calls, and fails
//!   when the row DENIES a kind the code demonstrably writes (a row saying `BARS_ONLY` over a
//!   module that appends trades).
//!
//! **The direction that is NOT gated, and why:** the mirror — a row CLAIMING a kind the scan cannot
//! see — is left unchecked on purpose. A text scan under-reports by construction (a write reached
//! through a helper the scan does not follow is invisible), so gating that direction would fail on
//! honest refactors rather than on wrong rows, and a gate that fails at random gets disabled. What
//! IS gated near it is per-venue non-vacuity: a non-empty row must be backed by at least ONE
//! visible write, so the scan announces when it has gone blind instead of passing silently. A row
//! that over-claims a single KIND — `quote: true` on a bars-only module — still survives both, and
//! is the residual hole this note exists to name.

/// Where a fill can come from. Mirrors `vike_recorder::config::Backfill`'s two live options plus the
/// vendor lanes that exist as their own bins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// The venue's own historical API — free, and only as complete as the venue exposes.
    Venue,
    /// The `data.vike.io` / pmxt archives — paid per GB or keyless CC-BY, complete L2 + trades.
    Archive,
    /// A local ClickHouse capture (prod-side; `clickhouse_poly` / `clickhouse_spot`).
    ClickHouse,
    /// Databento — a paid historical vendor.
    Databento,
    /// Tardis — a paid historical vendor.
    Tardis,
}

impl Source {
    pub const ALL: [Source; 5] =
        [Source::Venue, Source::Archive, Source::ClickHouse, Source::Databento, Source::Tardis];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Venue => "venue",
            Source::Archive => "archive",
            Source::ClickHouse => "clickhouse",
            Source::Databento => "databento",
            Source::Tardis => "tardis",
        }
    }
}

/// Which store kinds a `(source, venue)` pair can write.
///
/// `bar` is included even though the cross-kind coverage report ignores it: a bar-only source is
/// still a legitimate answer to "can I get anything for this venue", and hiding that would make
/// every crypto venue's `Venue` row look empty rather than "bars only".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackfillCaps {
    pub bar: bool,
    pub quote: bool,
    pub trade: bool,
    pub book: bool,
}

impl BackfillCaps {
    const NONE: Self = Self { bar: false, quote: false, trade: false, book: false };
    const BARS_ONLY: Self = Self { bar: true, quote: false, trade: false, book: false };
    /// quote + trade + book — the full tick lane the recorder writes, no bars.
    const TICKS: Self = Self { bar: false, quote: true, trade: true, book: true };
    /// bars + the full tick lane. Test-only today: no `(source, venue)` pair serves all four.
    #[cfg(test)]
    const EVERYTHING: Self = Self { bar: true, quote: true, trade: true, book: true };

    /// `true` when this pair can serve nothing at all.
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// Does this pair serve the named store kind? Unknown kinds are `false`.
    pub fn serves(self, kind: &str) -> bool {
        match kind {
            "bar" => self.bar,
            "quote" => self.quote,
            "trade" => self.trade,
            "book" => self.book,
            _ => false,
        }
    }

    /// The kinds served, in store-kind order — for a message naming what a fill can produce.
    pub fn kinds(self) -> Vec<&'static str> {
        ["bar", "quote", "trade", "book"].into_iter().filter(|k| self.serves(k)).collect()
    }
}

/// The tick-lane kinds a gap PLAN considers: [`vike_data::coverage::TICK_KINDS`] minus the ones no
/// source could ever serve. Today that is exactly `depth`.
///
/// `depth` is the conflated DOM-snapshot lane. It exists only because a live recorder wrote it —
/// no vendor sells it and no venue serves it as history, so `serves("depth")` is `false` for every
/// source by construction. Iterating it would append `"depth"` to EVERY instrument's
/// [`FillPlan::unavailable`](crate::gapfill::FillPlan::unavailable), which is true and useless: a
/// list meant to name what a fill cannot help with stops carrying information once it names
/// something for everybody.
///
/// Note this is NOT `BackfillCaps`' own kind set, which also has `bar` — a gap plan deliberately
/// never plans bars (they resample from trades).
pub const PLANNABLE_KINDS: [&str; 3] = ["trade", "quote", "book"];

/// What `source` can serve for `venue`. An unknown venue is [`BackfillCaps::NONE`] — a source cannot
/// serve a venue nobody has written an adapter for, and guessing otherwise would offer a fill that
/// silently does nothing.
///
/// Every row cites the module it was read from. When one of those modules gains or loses a kind, its
/// row here must move with it — `caps_matrix_is_pinned` fails until it does.
pub fn backfill_caps(source: Source, venue: &str) -> BackfillCaps {
    match (source, venue) {
        // --- Venue-direct history --------------------------------------------------------------
        // `binance`/`bybit`/`okx` klines -> `append_bars` (src/{binance,bybit,okx}.rs). BARS ONLY:
        // none of the three fetches an aggTrades/history endpoint here, so a recorded trade or book
        // gap cannot be repaired from the venue.
        (Source::Venue, "binance" | "bybit" | "okx") => BackfillCaps::BARS_ONLY,
        // `HistoricalFetcher` -> `vike_model::Bar` -> `append_bars` (src/ibkr/, feature `ibkr`).
        (Source::Venue, "ibkr") => BackfillCaps::BARS_ONLY,
        // Keyless `.bi5` ticks -> `QuoteTick` -> `append_quotes`, plus a quote->bar resample
        // (src/dukascopy.rs). The ONLY venue-direct source that serves a tick kind — and it serves
        // quotes, never trades: `.bi5` is a quote feed.
        (Source::Venue, "dukascopy") => {
            BackfillCaps { bar: true, quote: true, trade: false, book: false }
        }
        // VERIFIED, not assumed: `crates/bridges/polymarket` implements NO price-history endpoint
        // (no `/prices-history`, no fidelity param), and no L2 book history exists to fetch. The
        // venue row is empty — every Polymarket fill must come from an archive or a capture.
        (Source::Venue, "polymarket") => BackfillCaps::NONE,
        // Roster venues with no venue-direct backfill module at all. Named rather than left to a
        // fallback, because a NAMED empty row proves the venue was classified, not forgotten.
        (Source::Venue, "oanda" | "ig" | "fxcm" | "ctrader" | "alpaca") => BackfillCaps::NONE,
        // vike:new-venue:row // TODO(new-venue: {venue}): a fresh bridge has no collector module in this crate, so the row
        // vike:new-venue:row // is empty — NAMED rather than left to the fallback, and cross-pinned against
        // vike:new-venue:row // `venue_caps`' `backfill_bars`/`backfill_ticks` by `venue_caps_cross_pin_the_backfill_table`.
        // vike:new-venue:row (Source::Venue, "{venue}") => BackfillCaps::NONE,
        // `hyperliquid.rs` -> `append_bars` (+ `append_funding`, a kind this table does not track —
        // realized funding payments are not market data and no coverage report asks for them).
        (Source::Venue, "hyperliquid") => BackfillCaps::BARS_ONLY,
        // `aster.rs` -> `append_bars` (keyless public klines; a `.P` symbol fetches the USDⓈ-M perp,
        // routed inside vike-aster). Bars only — this crate has no aster tick collector.
        (Source::Venue, "aster") => BackfillCaps::BARS_ONLY,
        // `deribit.rs` -> `append_bars` (keyless public `get_tradingview_chart_data`; instrument
        // names like `BTC-PERPETUAL` are already unambiguous, so no `.P` handling). Bars only —
        // this venue's trade/book history is not served by that endpoint.
        (Source::Venue, "deribit") => BackfillCaps::BARS_ONLY,

        // --- Archives ----------------------------------------------------------------------------
        // pmxt hourly Parquet -> `append_book_updates`/`append_quotes`/`append_trades`
        // (src/pmxt/ingest.rs); `vike_archive` writes the same trio (src/vike_archive.rs).
        (Source::Archive, "polymarket") => BackfillCaps::TICKS,
        // No archive lane exists for any other venue today — the crypto L2 archive is rights-blocked
        // (binance/okx ToS forbid resale archives), so this is a licensing fact, not a gap in code.
        (Source::Archive, _) => BackfillCaps::NONE,

        // --- Local ClickHouse capture ------------------------------------------------------------
        // `clickhouse_poly` ingest writes all three tick kinds (src/clickhouse_poly/ingest.rs).
        // PROD-SIDE ONLY: it SELECTs from a local `data_polymarket` DB no customer has, so a
        // customer-facing UI must filter this source out rather than offer it.
        (Source::ClickHouse, "polymarket") => BackfillCaps::TICKS,
        // `clickhouse_spot` (src/clickhouse_spot/ingest.rs) also writes quotes, but it is NOT a
        // per-venue capability: it reads ONE fixed table (`data_history.spot_1s`) and takes the
        // venue as a `--venue` LABEL (default `spot`). Claiming it serves quotes for, say, binance
        // would be over-claiming — it would write whatever that one table holds under a name you
        // chose. So it earns no rows here; a caller wanting it names the venue explicitly.
        (Source::ClickHouse, _) => BackfillCaps::NONE,

        // --- Paid vendors ------------------------------------------------------------------------
        // Databento: ohlcv/trades/mbp-1/mbp-10 -> bars + all three tick kinds
        // (src/databento/ingest.rs). ZERO crypto coverage (verified in the vendor study), so it is
        // an equities/futures answer, keyed off its own vendor-namespaced venue rather than a roster
        // venue — hence no roster row is `true` here.
        (Source::Databento, _) => BackfillCaps::NONE,
        // Tardis: trades/quotes/incremental_book_L2 -> the tick lane (src/tardis/ingest.rs), bars by
        // resample. Crypto-only, and likewise vendor-namespaced rather than roster-keyed.
        (Source::Tardis, _) => BackfillCaps::NONE,

        // A venue with no row is served by nothing — see the fn doc.
        (_, _) => BackfillCaps::NONE,
    }
}

/// Every `(source, venue)` pair that can serve `kind` — "who can fill this hole?".
///
/// The Data Manager's answer to a gap: given a missing kind, which sources are even worth offering.
/// An empty result is itself the answer, and an honest one — a Polymarket book gap has exactly two
/// entries, and neither of them is the venue.
pub fn sources_for(venue: &str, kind: &str) -> Vec<Source> {
    Source::ALL.into_iter().filter(|s| backfill_caps(*s, venue).serves(kind)).collect()
}

/// `true` when NO source in this workspace can serve `kind` for `venue` — a hole that cannot be
/// filled by any means available here, only by having recorded it live.
pub fn is_unfillable(venue: &str, kind: &str) -> bool {
    sources_for(venue, kind).is_empty()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use vike_model::VENUES;

    /// Venue modules that exist but genuinely serve NOTHING historical, with the reason. Empty
    /// today. A row here is a claim someone had to write down — which is the point.
    const SERVES_NOTHING_DESPITE_EXISTING: &[(&str, &str)] = &[];

    /// **The gate this table lacked for its whole life.** Fails when a venue-direct backfill module
    /// EXISTS in this crate while its `Source::Venue` row still says [`BackfillCaps::NONE`].
    ///
    /// Why a filesystem walk rather than another declaration: `caps_matrix_is_pinned` below
    /// compares this table to a hardcoded copy of ITSELF, so it passes no matter how false a row
    /// is. Reality here is "does `src/<venue>.rs` exist", and only reading the directory answers
    /// that. The precedent is `crates/vike-ops/tests/settings_registry.rs`, which walks the real
    /// crate tree to prove the env-var registry against actual `env::var` call sites.
    ///
    /// This has gone wrong THREE times — hyperliquid, then aster (#1029), then deribit (#1030) —
    /// each time silently, because a stale row is not a compile error and the pin test cannot see
    /// it. The cost is not cosmetic: `is_unfillable(venue, "bar")` returns `true`, so the gapfill
    /// planner refuses to plan a backfill that demonstrably works.
    #[test]
    fn a_venue_module_that_exists_must_not_claim_it_serves_nothing() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();

        for venue in VENUES {
            // A venue module is `src/<venue>.rs` or `src/<venue>/mod.rs` — and only that. Lanes
            // like `pmxt/` or `databento/` are vendor/archive sources, not roster venues, so they
            // never match a VENUES entry and cannot false-positive here.
            let exists = src.join(format!("{venue}.rs")).is_file()
                || src.join(venue).join("mod.rs").is_file();
            if !exists || SERVES_NOTHING_DESPITE_EXISTING.iter().any(|(v, _)| v == venue) {
                continue;
            }
            if backfill_caps(Source::Venue, venue) == BackfillCaps::NONE {
                offenders.push(*venue);
            }
        }

        assert!(
            offenders.is_empty(),
            "these venues have a backfill module in src/ but their Source::Venue caps row still \
             says NONE: {offenders:?}\n\
             Fix the row in `backfill_caps` to describe what the module actually writes (grep it \
             for append_bars/append_quotes/append_trades/append_book_updates) and update the \
             `caps_matrix_is_pinned` expectation to match. If the module truly serves nothing \
             historical, add it to SERVES_NOTHING_DESPITE_EXISTING with a reason."
        );
    }

    /// The gate above is only meaningful if it can actually SEE the source tree — a wrong
    /// `CARGO_MANIFEST_DIR` would make it vacuously pass, which is the exact failure mode it
    /// exists to prevent. (A mutation harness earlier today reported a passing test as proof of
    /// correctness when its edit had silently not applied; same shape, so prove the walk works.)
    #[test]
    fn the_module_walk_can_see_the_source_tree() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(src.join("caps.rs").is_file(), "cannot see src/caps.rs at {}", src.display());
        assert!(src.join("binance.rs").is_file(), "cannot see the binance module");
    }

    // -------------------------------------------------------------------------------------------
    // Deriving what a venue's code ACTUALLY writes — the SHAPE gate.
    //
    // The existence gate above answers "is there a module at all". It cannot see the other half of
    // the same defect: a row whose SHAPE is wrong — `BARS_ONLY` over a module that appends trades.
    // The only non-circular source of truth for that is the module's own text, so these helpers
    // read it.
    //
    // ONE direction is gated, deliberately (see the module doc): the scan's observed set must be a
    // SUBSET of what the row declares. A text scan under-reports — a write reached through a helper
    // this scan does not follow is invisible — and under-reporting shrinks the observed set, which
    // can only ever make a subset check pass, never fail. The reverse check (declared ⊆ observed)
    // would turn every honest refactor into a red build, and a gate that fails at random gets
    // deleted rather than fixed.
    // -------------------------------------------------------------------------------------------

    /// `HistStore` write verbs that produce one of this table's four kinds.
    ///
    /// The scan matches the CALL shape `.<verb>(`, never the bare name, because every venue module
    /// spells `append_bars` in its prose: `src/binance.rs` names it three times and calls it zero
    /// times, and `src/regroup.rs` even names an `append_bars_grouped` that does not exist. Comments
    /// are stripped first regardless; the call shape is the second belt.
    ///
    /// `resample_quotes_to_bars` is here for a concrete reason: dukascopy's bars come from neither
    /// an `append_bars` nor a helper — it resamples the quotes it already stored. A verb list of
    /// just the four `append_*` names would report dukascopy as quote-only and make its (correct)
    /// `bar: true` look unbacked.
    const WRITE_VERBS: &[(&str, &str)] = &[
        ("append_bars", "bar"),
        ("resample_quotes_to_bars", "bar"),
        ("resample_trades_to_bars", "bar"),
        ("append_quotes", "quote"),
        ("append_trades", "trade"),
        ("append_book_updates", "book"),
    ];

    /// `HistStore` write verbs that deliberately map to NO [`BackfillCaps`] kind, each with the
    /// reason. `every_hist_store_write_verb_is_classified` fails when the trait grows a verb that is
    /// in neither table — the check that keeps [`WRITE_VERBS`] from silently going stale the way
    /// this very table's rows did.
    const NOT_A_BACKFILL_KIND: &[(&str, &str)] = &[
        // the conflated DOM lane — PLANNABLE_KINDS above states why no source can serve it
        ("append_depth", "no source serves depth as history"),
        ("append_symbol_properties", "the point-in-time instrument grid, not a tape"),
        ("append_equity", "portfolio equity curve — an own-account journal series"),
        ("append_exec_fills", "own-execution journal, not market data"),
        ("append_exec_orders", "own-execution journal, not market data"),
        // market data, but no coverage report asks for it — the hyperliquid row above says the same
        ("append_funding", "realized funding payments, deliberately untracked here"),
        ("append_chain_snapshot", "options-chain snapshots — their own series"),
        // Not market data at all: a graded POSITIONING panel over other people's wallets, served by
        // a metrics API rather than by any venue this table knows how to plan against.
        ("append_cohort", "graded cohort open interest — no venue serves it as history"),
        // A perp's venue-reported context (the funding PREMIUM today). It IS market data and one
        // venue does serve it as history, but it is not a TAPE: `BackfillCaps` reports bar/quote/
        // trade/book coverage per venue, and a per-funding-interval scalar answers none of those
        // four questions. It rides `funding_rate_backfill`, beside the funding-rate bars it is
        // fetched with, rather than being planned against.
        ("append_perp_metrics", "perp funding premium — a venue metric, not one of the four tapes"),
    ];

    /// Venues whose real writes this TEXT scan cannot see, with the reason. Empty today.
    ///
    /// The escape hatch exists so that the first legitimate refactor which blinds the scan (a write
    /// moved two hops away, behind a helper of a helper) costs one reviewed line here instead of a
    /// deleted gate. A row is a claim someone had to write down — the same idiom as
    /// [`SERVES_NOTHING_DESPITE_EXISTING`]. It exempts the venue from BOTH derived checks below.
    const SCAN_CANNOT_SEE: &[(&str, &str)] = &[];

    fn src_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Remove Rust comments so a doc mention can never be read as a call. Tracks nested block
    /// comments and double-quoted strings (so the `//` in a `"https://…"` literal is not a comment).
    ///
    /// Known imprecision: raw strings (`r#"…"#`) and char literals are NOT tracked, so a `"` inside
    /// one misaligns the string state for the rest of that region. The realistic consequence is
    /// dropping code that was not a comment — i.e. under-reporting, which is the safe direction for
    /// the one check that is gated.
    fn strip_comments(src: &str) -> String {
        let chars: Vec<char> = src.chars().collect();
        let mut out = String::with_capacity(src.len());
        let mut i = 0usize;
        let mut block = 0usize;
        let mut in_string = false;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if block > 0 {
                if c == '/' && next == Some('*') {
                    block += 1;
                    i += 2;
                } else if c == '*' && next == Some('/') {
                    block -= 1;
                    i += 2;
                } else {
                    if c == '\n' {
                        out.push('\n'); // keep line structure for the line-based scans
                    }
                    i += 1;
                }
                continue;
            }
            if in_string {
                if c == '\\' {
                    out.push(c);
                    if let Some(escaped) = next {
                        out.push(escaped);
                    }
                    i += 2;
                    continue;
                }
                if c == '"' {
                    in_string = false;
                }
                out.push(c);
                i += 1;
                continue;
            }
            if c == '"' {
                in_string = true;
                out.push(c);
                i += 1;
                continue;
            }
            if c == '/' && next == Some('/') {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1; // the newline itself is pushed by the next iteration
                }
                continue;
            }
            if c == '/' && next == Some('*') {
                block = 1;
                i += 2;
                continue;
            }
            out.push(c);
            i += 1;
        }
        out
    }

    /// Which kinds these files demonstrably write, by call shape. See [`WRITE_VERBS`].
    fn writes_in(files: &[PathBuf]) -> BackfillCaps {
        let mut caps = BackfillCaps::NONE;
        for file in files {
            let Ok(text) = std::fs::read_to_string(file) else { continue };
            let code = strip_comments(&text);
            for (verb, kind) in WRITE_VERBS {
                if !code.contains(&format!(".{verb}(")) {
                    continue;
                }
                match *kind {
                    "bar" => caps.bar = true,
                    "quote" => caps.quote = true,
                    "trade" => caps.trade = true,
                    "book" => caps.book = true,
                    other => {
                        panic!("WRITE_VERBS names a kind BackfillCaps has no field for: {other}")
                    }
                }
            }
        }
        caps
    }

    /// `src/<name>.rs`, or every `.rs` under `src/<name>/` when the module is a directory. Empty
    /// when the module does not exist — the same "is there a module" question the existence gate
    /// above asks, so the two gates cannot disagree about what a venue module is.
    fn module_files(src: &Path, name: &str) -> Vec<PathBuf> {
        let flat = src.join(format!("{name}.rs"));
        if flat.is_file() {
            return vec![flat];
        }
        let dir = src.join(name);
        let mut out = Vec::new();
        if dir.join("mod.rs").is_file() {
            rs_files_under(&dir, &mut out);
        }
        out.sort();
        out
    }

    fn rs_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files_under(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Top-level module names under `src/`: `src/*.rs` minus `lib`, and `src/*/` minus `bin` (that
    /// is cargo's binary directory, not a module).
    fn sibling_modules(src: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(src) else { return out };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if path.is_dir() {
                if stem != "bin" {
                    out.push(stem.to_string());
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") && stem != "lib" {
                out.push(stem.to_string());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Does `code` reference module `name` as a path (`klines::…`)? Anchored on a word boundary, so
    /// the `vike_binance::` in every binance call site is not mistaken for the `binance` module.
    fn names_module(code: &str, name: &str) -> bool {
        let needle = format!("{name}::");
        let bytes = code.as_bytes();
        let mut from = 0usize;
        while let Some(rel) = code[from..].find(needle.as_str()) {
            let at = from + rel;
            let after_ident =
                at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
            if !after_ident {
                return true;
            }
            from = at + needle.len();
        }
        false
    }

    /// Everything the scan reads to decide what `venue` writes:
    ///
    /// 1. the venue's own module (`src/<venue>.rs` or `src/<venue>/`);
    /// 2. `src/bin/<venue>_backfill.rs` when it exists — **load-bearing for ibkr**, whose module
    ///    only builds the paging plan while the bin does every `append_bars`;
    /// 3. ONE hop into each sibling top-level module the above NAME — load-bearing for
    ///    binance/bybit/okx/aster/deribit/hyperliquid, none of which contains a single append call:
    ///    all six delegate their bars to `crate::klines::backfill_klines`.
    ///
    /// The hop is deliberately coarse (any `<module>::` reference, not just a call). Coarse can
    /// only ADD to the observed set, and a larger observed set makes the one gated check stricter,
    /// never laxer. The cost is that a venue module which one day references a module writing some
    /// unrelated kind would trip that check — a review trigger, not a false alarm to widen the row
    /// for: the fix is to look at why the reference is there.
    fn venue_scan_files(src: &Path, venue: &str) -> Vec<PathBuf> {
        let mut files = module_files(src, venue);
        if files.is_empty() {
            return files;
        }
        let bin = src.join("bin").join(format!("{venue}_backfill.rs"));
        if bin.is_file() {
            files.push(bin);
        }
        let mut code = String::new();
        for file in &files {
            if let Ok(text) = std::fs::read_to_string(file) {
                code.push_str(&strip_comments(&text));
                code.push('\n');
            }
        }
        for module in sibling_modules(src) {
            // `caps` is excluded from the hop, and must stay excluded: this very module writes
            // nothing, yet its own scanner fixtures spell call-shaped text like the one in
            // `strip_comments_hides_doc_mentions_but_keeps_real_calls`. Letting it in would let the
            // gate observe ITSELF and fabricate a write for any venue that ever names `caps::`.
            if module != venue && module != "caps" && names_module(&code, &module) {
                files.extend(module_files(src, &module));
            }
        }
        files.sort();
        files.dedup();
        files
    }

    /// **The SHAPE gate.** Fails when a `Source::Venue` row DENIES a kind the venue's own code
    /// demonstrably writes — the mirror of the existence gate above, which only ever caught a row
    /// that said `NONE` while a module existed.
    ///
    /// Sound by construction in this direction only: the scan is text, so it under-reports whenever
    /// a write hides behind a helper it does not follow, and a smaller observed set can never
    /// manufacture a failure here. The reverse (a row claiming a kind the scan cannot see) is NOT
    /// asserted — see the module doc for why, and see the sibling test below for the bounded piece
    /// of it that IS.
    #[test]
    fn a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes() {
        let src = src_dir();
        let mut offenders = Vec::new();
        for venue in VENUES {
            if SCAN_CANNOT_SEE.iter().any(|(v, _)| v == venue) {
                continue;
            }
            let declared = backfill_caps(Source::Venue, venue);
            let observed = writes_in(&venue_scan_files(&src, venue));
            let denied: Vec<&str> =
                observed.kinds().into_iter().filter(|k| !declared.serves(k)).collect();
            if !denied.is_empty() {
                offenders.push(format!(
                    "{venue}: code writes {denied:?}, but its Source::Venue row declares {:?}",
                    declared.kinds()
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "a venue's caps row denies a kind its own backfill code writes:\n  {}\n\
             Fix the row in `backfill_caps` (and `caps_matrix_is_pinned` with it) to match what the \
             module does. If the scan is wrong rather than the row, say so in SCAN_CANNOT_SEE.",
            offenders.join("\n  ")
        );
    }

    /// The bounded, non-brittle half of the direction that is not gated: a NON-EMPTY row must be
    /// backed by at least one write the scan can actually see.
    ///
    /// Without this, the gate above is vacuous for any venue the scan has gone blind on — `∅` is a
    /// subset of everything. This does not check that each declared KIND is backed (that is the
    /// documented hole); it checks that the evidence has not vanished entirely, which takes a
    /// venue hiding *every* write behind a hop this scan does not follow. When that day comes the
    /// failure names the venue and points at SCAN_CANNOT_SEE, rather than passing quietly.
    #[test]
    fn a_non_empty_venue_row_must_be_backed_by_a_write_the_scan_can_see() {
        let src = src_dir();
        let mut blind = Vec::new();
        for venue in VENUES {
            if SCAN_CANNOT_SEE.iter().any(|(v, _)| v == venue) {
                continue;
            }
            if backfill_caps(Source::Venue, venue).is_empty() {
                continue;
            }
            if writes_in(&venue_scan_files(&src, venue)).is_empty() {
                blind.push(*venue);
            }
        }
        assert!(
            blind.is_empty(),
            "these venues declare a non-empty Source::Venue row but the source scan can no longer \
             see ANY write for them: {blind:?}\n\
             Either the row is wrong, or a write moved behind a helper `venue_scan_files` does not \
             follow — extend the scan, or record the venue in SCAN_CANNOT_SEE with the reason. Do \
             not delete the check: a blind scan makes the shape gate vacuous."
        );
    }

    /// Prove the scan's three mechanics work, so neither gate above can pass by seeing nothing.
    /// (`the_module_walk_can_see_the_source_tree` is the same instinct for the existence gate: a
    /// filesystem-derived test that observes nothing is indistinguishable from one that passes.)
    #[test]
    fn the_write_scan_resolves_direct_calls_delegation_and_the_bin() {
        let src = src_dir();

        // 1. DIRECT. dukascopy calls both verbs itself — `hist.append_quotes(..)` for the .bi5
        //    ticks, and `hist.resample_quotes_to_bars(..)` for the bars it derives from them.
        assert_eq!(
            writes_in(&module_files(&src, "dukascopy")),
            BackfillCaps { bar: true, quote: true, trade: false, book: false },
            "the direct-call arm of the scan is broken (or dukascopy stopped resampling)"
        );

        // 2. DELEGATION. None of these six contains an append call at all: every literal
        //    `append_bars` in their files sits in a doc comment, and the real write is
        //    `crate::klines::backfill_klines`. The asymmetry between the two asserts is exactly
        //    what regresses if the comment strip or the one-hop resolution breaks.
        for venue in ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"] {
            assert_eq!(
                writes_in(&module_files(&src, venue)),
                BackfillCaps::NONE,
                "{venue}: expected NO write of its own (it delegates to klines) — either the module \
                 changed, or the comment strip is letting a doc mention through as a call"
            );
            assert_eq!(
                writes_in(&venue_scan_files(&src, venue)),
                BackfillCaps::BARS_ONLY,
                "{venue}: the crate::klines::backfill_klines delegation no longer resolves"
            );
        }

        // 3. THE BIN. ibkr's module holds only the paging plan; `src/bin/ibkr_backfill.rs` does
        //    every append. Without step 2 of `venue_scan_files` this venue would look write-free.
        assert_eq!(writes_in(&module_files(&src, "ibkr")), BackfillCaps::NONE);
        assert_eq!(writes_in(&venue_scan_files(&src, "ibkr")), BackfillCaps::BARS_ONLY);
    }

    #[test]
    fn strip_comments_hides_doc_mentions_but_keeps_real_calls() {
        let src = "\
//! Flow: `fetch` -> `append_bars` (idempotent).
/// and `append_bars` them into the store. hist.append_trades(x)
/* block hist.append_quotes(a) /* nested */ still comment */
let url = \"https://example.com/v1\"; // hist.append_book_updates(b)
Ok(hist.append_bars(venue, symbol, &bars)?)
";
        let code = strip_comments(src);
        assert!(code.contains(".append_bars("), "a real call must survive");
        assert!(!code.contains(".append_trades("), "a doc-comment example must not");
        assert!(!code.contains(".append_quotes("), "a block comment must not");
        assert!(!code.contains(".append_book_updates("), "a trailing comment must not");
        assert!(
            code.contains("https://example.com/v1"),
            "the // in a string literal is not a comment"
        );
    }

    #[test]
    fn names_module_is_word_boundary_anchored() {
        assert!(names_module("crate::klines::backfill_klines(", "klines"));
        assert!(names_module("use klines::commit_key;", "klines"));
        assert!(!names_module("vike_binance::fetch_klines_range(", "binance"));
        assert!(!names_module("no path here", "klines"));
    }

    /// Keep [`WRITE_VERBS`] honest against the real `HistStore` trait: every `append_*`/`resample_*`
    /// it declares must be mapped to a kind or listed in [`NOT_A_BACKFILL_KIND`] with a reason.
    ///
    /// Without this, a new store write verb makes the shape gate quietly weaker — the same
    /// "the list claims more than it checks" failure the gates in this file exist to close, one
    /// level down. Reading another crate's source from a test follows the precedent of
    /// `crates/vike-ops/tests/settings_registry.rs`, which walks the whole workspace tree.
    ///
    /// Scope, stated rather than assumed: only the trait in `vike-data/src/hist.rs`. A bar-writing
    /// method that exists solely on a concrete backend is out of view, and would show up as
    /// under-reporting — the safe direction.
    #[test]
    fn every_hist_store_write_verb_is_classified() {
        let hist_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike-data/src/hist.rs");
        let text = std::fs::read_to_string(&hist_rs).unwrap_or_else(|e| {
            panic!(
                "cannot read {}: {e}\nThis test must fail LOUDLY rather than stop checking — if the \
                 HistStore trait moved, point it at the new file.",
                hist_rs.display()
            )
        });
        let code = strip_comments(&text);
        assert!(
            code.contains("fn append_bars("),
            "read {} but found no `fn append_bars(` — the parse changed, not the trait",
            hist_rs.display()
        );

        let mut unclassified = Vec::new();
        for line in code.lines() {
            let trimmed = line.trim_start();
            let Some(rest) =
                trimmed.strip_prefix("fn ").or_else(|| trimmed.strip_prefix("pub fn "))
            else {
                continue;
            };
            let name: String =
                rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if !name.starts_with("append_") && !name.starts_with("resample_") {
                continue;
            }
            let known = WRITE_VERBS.iter().any(|(v, _)| *v == name.as_str())
                || NOT_A_BACKFILL_KIND.iter().any(|(v, _)| *v == name.as_str());
            if !known {
                unclassified.push(name);
            }
        }
        unclassified.sort();
        unclassified.dedup();
        assert!(
            unclassified.is_empty(),
            "HistStore grew write verbs this table has never classified: {unclassified:?}\n\
             Add each to WRITE_VERBS (with the BackfillCaps kind it writes) or to \
             NOT_A_BACKFILL_KIND (with the reason it maps to none)."
        );
    }

    /// The whole matrix, verbatim. A row moving without this test moving is the drift this table
    /// exists to prevent (CLAUDE.md's STEP-1 discipline: contradictions are PINNED, not fixed).
    #[test]
    fn caps_matrix_is_pinned() {
        let row = |v: &str| {
            Source::ALL
                .into_iter()
                .map(|s| format!("{}:{}", s.as_str(), backfill_caps(s, v).kinds().join("+")))
                .collect::<Vec<_>>()
                .join(" ")
        };

        assert_eq!(
            row("binance"),
            "venue:bar archive: clickhouse: databento: tardis:",
            "binance: klines only"
        );
        assert_eq!(row("bybit"), "venue:bar archive: clickhouse: databento: tardis:");
        assert_eq!(row("okx"), "venue:bar archive: clickhouse: databento: tardis:");
        assert_eq!(row("ibkr"), "venue:bar archive: clickhouse: databento: tardis:");
        assert_eq!(
            row("dukascopy"),
            "venue:bar+quote archive: clickhouse: databento: tardis:",
            "the ONLY venue-direct tick source, and it is quotes — .bi5 is a quote feed"
        );
        assert_eq!(
            row("polymarket"),
            "venue: archive:quote+trade+book clickhouse:quote+trade+book databento: tardis:",
            "the venue itself serves NOTHING historical"
        );
        assert_eq!(
            row("hyperliquid"),
            "venue:bar archive: clickhouse: databento: tardis:",
            "hyperliquid.rs writes append_bars — an EARLIER draft of this table wrongly said NONE"
        );
        assert_eq!(
            row("aster"),
            "venue:bar archive: clickhouse: databento: tardis:",
            "aster.rs writes append_bars — this row said NONE until the perp-klines work added the \
             module, the SAME drift the hyperliquid row above records"
        );
        assert_eq!(
            row("deribit"),
            "venue:bar archive: clickhouse: databento: tardis:",
            "deribit.rs writes append_bars — third venue to need this row corrected after its \
             module landed, see the hyperliquid and aster notes above"
        );
        for v in ["oanda", "ig", "fxcm", "ctrader", "alpaca"] {
            assert_eq!(row(v), "venue: archive: clickhouse: databento: tardis:", "{v}");
        }
    }

    /// CROSS-PIN with `vike_model::venue_caps` — the check that closes the loop for a SECOND table
    /// declaring the same fact one crate away.
    ///
    /// `VenueCaps` carries `backfill_bars` / `backfill_ticks`, hand-written per venue and, until
    /// this test, verified only by a vike-model test named `backfill_matches_vike_backfill` that
    /// **read nothing from vike-backfill at all** — it asserted a hardcoded list, and the list was
    /// WRONG: it required `!backfill_bars` for **deribit**, which has had a `deribit_backfill` bin
    /// since #1030. Three further rows (hyperliquid, ibkr, dukascopy's bars) were equally wrong and
    /// simply absent from the list, so nothing ever failed. Same defect class as this table's own
    /// three-times-stale rows — hence the same cure, but pointed the other way.
    ///
    /// **Why this test lives HERE and not in vike-model:** vike-backfill depends on vike-model, so
    /// only this side can name both. And placing it here is what makes it non-circular — the row it
    /// compares against is gated by `a_venue_module_that_exists_must_not_claim_it_serves_nothing`
    /// (a filesystem walk) and `a_venue_row_must_not_deny_a_write_its_module_demonstrably_makes` (a
    /// source scan for `HistStore` write calls), so `VenueCaps.backfill_*` transitively inherits
    /// evidence from the real collector modules instead of from another human's assertion.
    ///
    /// **The definitional split, resolved.** These two fields disagreed for dukascopy by design:
    /// `venue_caps.backfill_bars` used to mean "the venue serves a NATIVE bar endpoint" (`false` —
    /// `.bi5` is a quote feed) while `backfill_caps(Venue, "dukascopy").bar` means "this crate can
    /// produce bars" (`true` — `resample_quotes_to_bars`). Settled in favour of **can produce**,
    /// documented on the `VenueCaps::backfill_bars` field itself: the caller's question is "can I
    /// get bars for this venue without a vendor or an archive", and a resample is not a lesser bar.
    /// With that, the pin is exact equality for all 14 venues and needs NO exception table.
    ///
    /// `backfill_ticks` maps to the union of this table's three tick kinds — `VenueCaps` has one
    /// tick bit where this table has three, so the union is the only faithful projection.
    #[test]
    fn venue_caps_cross_pin_the_backfill_table() {
        for v in VENUES {
            let bc = backfill_caps(Source::Venue, v);
            let vc = vike_model::caps_for(v);
            assert_eq!(
                vc.backfill_bars, bc.bar,
                "{v}: VenueCaps.backfill_bars and backfill_caps(Venue, {v}).bar disagree. This \
                 table is the gated one (filesystem + source scan) — fix the vike-model row, \
                 unless the collector genuinely changed."
            );
            assert_eq!(
                vc.backfill_ticks,
                bc.quote || bc.trade || bc.book,
                "{v}: VenueCaps.backfill_ticks must equal (quote | trade | book) here"
            );
        }
        // Non-vacuity in both directions: a constant-`false` bug on either side would satisfy the
        // loop for a roster that happened to be all-empty.
        assert!(
            VENUES.iter().any(|v| vike_model::caps_for(v).backfill_bars),
            "some venue backfills"
        );
        assert!(
            VENUES.iter().any(|v| !vike_model::caps_for(v).backfill_bars),
            "some venue does not"
        );
        assert!(
            VENUES.iter().any(|v| vike_model::caps_for(v).backfill_ticks),
            "dukascopy at least"
        );
    }

    /// **The finding.** No venue-direct source serves `trade` or `book` anywhere on the roster — so a
    /// hole in a RECORDED tape (which is trade/quote/book) generally cannot be repaired from the
    /// venue. This is the assertion that would fail the day someone wires an aggTrades backfill, and
    /// the docs above must change with it.
    #[test]
    fn no_venue_serves_trades_or_book_today() {
        for v in VENUES {
            let c = backfill_caps(Source::Venue, v);
            assert!(!c.trade, "{v}: a venue-direct TRADE backfill now exists — update the docs");
            assert!(!c.book, "{v}: a venue-direct BOOK backfill now exists — update the docs");
        }
    }

    /// Every roster venue is classified — a new venue fails this until its rows exist, which is the
    /// point of keying completeness off `vike_model::VENUES` rather than a local list.
    #[test]
    fn caps_roster_is_exhaustive() {
        for v in VENUES {
            for s in Source::ALL {
                // The call must not panic and must be a deliberate row, not a fallback: every
                // roster venue is NAMED in `backfill_caps`, including the empty ones.
                let _ = backfill_caps(s, v);
            }
        }
        // A venue not on the roster serves nothing rather than guessing.
        assert!(backfill_caps(Source::Venue, "kalshi").is_empty());
        assert!(backfill_caps(Source::Archive, "").is_empty());
    }

    /// "Who can fill this hole?" — the Data Manager's question, and the honest answer for the case
    /// that motivated the table.
    #[test]
    fn a_polymarket_book_gap_can_only_come_from_an_archive_or_a_capture() {
        assert_eq!(
            sources_for("polymarket", "book"),
            vec![Source::Archive, Source::ClickHouse],
            "never the venue"
        );
        assert!(!sources_for("polymarket", "book").contains(&Source::Venue));
        assert!(!is_unfillable("polymarket", "book"));
    }

    /// A binance trade gap is UNFILLABLE here: no venue trade backfill, and the crypto L2/trade
    /// archive is rights-blocked (binance/okx ToS forbid resale archives). Recording it live is the
    /// only way to have it — which is precisely why the recorder exists.
    #[test]
    fn a_binance_trade_gap_is_unfillable_by_any_source_here() {
        assert!(sources_for("binance", "trade").is_empty());
        assert!(is_unfillable("binance", "trade"));
        assert!(is_unfillable("binance", "book"));
        // ...but its BARS are fine.
        assert_eq!(sources_for("binance", "bar"), vec![Source::Venue]);
    }

    #[test]
    fn unknown_kinds_are_never_served() {
        assert!(!backfill_caps(Source::Archive, "polymarket").serves("properties"));
        assert!(!BackfillCaps::EVERYTHING.serves("nonsense"));
    }

    #[test]
    fn kinds_lists_in_store_kind_order() {
        assert_eq!(BackfillCaps::EVERYTHING.kinds(), vec!["bar", "quote", "trade", "book"]);
        assert_eq!(BackfillCaps::TICKS.kinds(), vec!["quote", "trade", "book"]);
        assert!(BackfillCaps::NONE.kinds().is_empty());
    }
}
