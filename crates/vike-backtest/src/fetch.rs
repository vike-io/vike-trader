//! `backtest data fetch` — pull REAL bars from a venue's public history into the hist store.
//!
//! # Why this lives here and not in `vike-backfill`
//!
//! `vike-backfill` is the collector fleet and owns every serious ingest path — resume, pacing
//! across days, multi-symbol lanes, the archive stores. It is also not in the container image and
//! not in CI's roster, so a user who installed the product has no downloader at all: the tools they
//! DO have (`backtest`, `vike-study`, the chart) all read a store nobody shipped them a way to
//! fill. This module is the smallest thing that closes that — one symbol, one interval, one window,
//! straight into the store the next command reads — and it is deliberately not a second collector:
//! anything that wants resume, scheduling or many symbols should reach for `vike-backfill`.
//!
//! # Why it is optional, and must stay optional
//!
//! The venue bridge brings the whole blocking transport stack (ureq + tungstenite + rustls) and
//! `vike-bridge-core/full` with it. `vike-backtest` sits UNDER `vike-cli`, whose identity is that it
//! is light and DataFusion-free — the `light-consumers` CI lane asserts exactly that — so a
//! non-optional dependency here would land that stack in the CLI's graph by feature unification.
//! Hence `venue-fetch`: off by default, forwarded by the `vike` multicall's `backtest` feature and
//! enabled for the shipped release binary, absent everywhere else.
//!
//! # What this writes, and where it cannot be confused with the demo tape
//!
//! Rows land under the venue's REAL id (`binance`), never `vike_data::demo::DEMO_VENUE`. That
//! separation is the point of the demo tape having a venue of its own: one store can hold both, a
//! query names one, and no result can quietly mix invented prices with real ones.

use std::collections::HashMap;

use vike_data::hist::HistStore;

/// What to fetch: one instrument, one interval, from one venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSpec {
    /// Venue id, as the store spells it (`binance`).
    pub venue: String,
    /// Instrument id in the venue's own spelling (`BTCUSDT`).
    pub symbol: String,
    /// Bar interval in the venue's spelling (`1m`, `1h`, `1d`).
    pub interval: String,
}

/// Venues this build can fetch from. A ROSTER rather than a match arm's `_` so the error message
/// can name what IS available — a user who typed `bybit` needs the list, not a refusal.
///
/// ⚠ **This was the FOURTH of four hand-copied kline rosters**
/// (`docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`), and it is the
/// only one still standing. **Phase 3 RULED that it stays apart — it did not defer it**, and the
/// reason is structural rather than a preference:
///
/// * The other two live rosters (the supervisor's own table and
///   `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`) collapsed into
///   `crates/vike-backfill/src/kline_source.rs`'s `KLINE_SOURCES`, and the GUI planner's
///   `SUPPORTED_BACKFILL_VENUES` was deleted outright.
/// * **This roster cannot see that registry, at any price.** `vike-backtest` declares
///   `[package.metadata.vike] layer = 50` and `vike-backfill` declares 55, so a normal edge from
///   here to there fails `crates/vike-ops/tests/layer_gate.rs`; and the edge already runs the OTHER
///   way (`vike-backfill`'s optional `vike-backtest` dep under `poly-ch-backtest`, whose manifest
///   comment reads "Acyclic: vike-backtest depends only on vike-model/vike-data, never on
///   vike-backfill"), so inverting it is a cycle as well as an inversion.
/// * On top of that it costs a MANIFEST edit rather than a row: this crate depends on
///   `vike-binance` and nothing else behind its optional `venue-fetch` feature, and the module doc
///   above says why the optionality is load-bearing — a non-optional venue dep lands a signing
///   stack in the CLI's graph by feature unification, which the `light-consumers` lane exists to
///   catch.
///
/// The one seam this roster could legally be reconciled against is `vike_model::venue_caps`'s
/// `backfill_bars` at layer 10 — the `vike-catalog` `catalog_availability` shape, a low-layer
/// classification consulted by a high-layer constructor — and that is a separate change with its
/// own argument, not this one. `crates/vike-ops/tests/collector_dispatch_gate.rs`'s
/// `ROSTERS_NOT_GATED` carries the ruling in the gate that would otherwise police it.
///
/// ⚠ It is also a genuinely DIFFERENT producer, not a fourth copy of the same one: [`fetch_into`]
/// calls the bridge directly and appends under its own commit-key namespace
/// (`fetch:{venue}:{symbol}:{interval}:{from_ms}-{to_ms}`, declared in
/// `crates/vike-data/src/store_kind.rs`), so it does NOT go through
/// `vike_backfill::klines::backfill_klines`.
///
/// ⚠ **This paragraph used to end "and has no still-forming-candle guard on ANY interval … folding
/// this path into the shared collector seam is what would close it, and that is Phase 3's
/// collapse". Phase 3 RULED the collapse impossible — the three bullets above are that ruling — so
/// the sentence named a cure that was never going to arrive.** What actually landed is a REFUSAL
/// spelled here, against the same `vike_model::time::measures_bar_step` predicate the collector
/// seam refuses on, which closes the half that is permanent (an unmeasurable step, whose bad row
/// outlives a corrective re-fetch because the window's commit key is already spent). The other half
/// — a still-forming candle on a MEASURABLE step — is still open on this path and is declared at
/// [`fetch_into`]'s own doc and as a row in `crates/vike-ops/tests/kline_ingest_gate.rs`.
/// `crates/vike-ops/tests/collector_dispatch_gate.rs`'s `ROSTERS_NOT_GATED` carries the same two
/// reasons as a row, so the exclusion is a written claim rather than an omission.
pub const FETCHABLE_VENUES: &[&str] = &["binance"];

/// Parse `VENUE:SYMBOL:INTERVAL`.
///
/// # Errors
///
/// Returns a message naming the whole grammar, because the failure a user actually hits is having
/// typed two colons' worth of a three-part spec, and an error that only says "invalid" makes them
/// guess which part.
pub fn parse_spec(raw: &str) -> Result<FetchSpec, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(format!(
            "data fetch takes VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h), not {raw:?}"
        ));
    }
    let venue = parts[0].to_ascii_lowercase();
    if !FETCHABLE_VENUES.contains(&venue.as_str()) {
        return Err(format!(
            "this build cannot fetch from {venue:?}. Fetchable venues: {}. For anything else use \
             the vike-backfill collectors, which own the serious ingest paths.",
            FETCHABLE_VENUES.join(", ")
        ));
    }
    Ok(FetchSpec { venue, symbol: parts[1].to_string(), interval: parts[2].to_string() })
}

/// The append key for one fetched window.
///
/// Carries the window, so re-running the SAME fetch writes nothing (the store spends a commit key
/// once) while a fetch of a different window still lands. That is the honest idempotence for this
/// command: a user who re-runs after a network failure should not double-book rows, and a user
/// extending their history should not be told there is nothing to do.
#[must_use]
pub fn commit_key(spec: &FetchSpec, from_ms: i64, to_ms: i64) -> String {
    format!("fetch:{}:{}:{}:{from_ms}-{to_ms}", spec.venue, spec.symbol, spec.interval)
}

/// What one fetch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// Bars the venue returned.
    pub fetched: usize,
    /// Rows the store accepted. Zero with a non-zero `fetched` means this exact window was already
    /// present — a success.
    pub written: usize,
    /// First and last bar timestamp actually returned, if any. Reported because a venue answering
    /// a narrower window than asked (listing date, retention) is normal and invisible otherwise.
    pub span_ms: Option<(i64, i64)>,
}

/// Fetch `[from_ms, to_ms)` and append it to `store`.
///
/// # The forming-bar refusal — `docs/decisions/0059-…`'s Phase 1, bug B, on this producer
///
/// An interval [`vike_model::time::measures_bar_step`] answers `false` for (`1w`, `1M`, `1mo`) is
/// refused before the network call. That predicate's own doc carries the whole argument; what is
/// specific to THIS path is that it is the one an installed product reaches — `vike-cli data fetch
/// binance:BTCUSDT:1w --days 30` — and that this producer never had the still-forming-candle guard
/// at all, so nothing downstream of it would have noticed.
///
/// ⚠ **This closes the PERMANENT half and not the whole defect, and the residual is declared rather
/// than implied.** On a measurable step this path still stores a venue's still-forming last candle,
/// because [`FETCHABLE_VENUES`]'s own note explains why it cannot reach
/// `vike_backfill::klines::ingest_klines`, where `drop_forming_tail` lives: `vike-backfill` is
/// layer 55 against this crate's 50 and the edge already runs 55 → 50, so the collector seam is
/// unreachable from here at any price. The two halves are not equally bad — an unmeasurable step
/// spends a commit key over a wrong row and `DataFusionHist::commit_rows` then answers the
/// corrective re-fetch with `Ok(0)`, while a forming bar on `1h` is wrong in the same way but the
/// remedy is identical, so the residual is genuine. Closing it means the guard moving BELOW both
/// crates (the "shared crate below both" rule), which is its own change with its own argument.
/// `crates/vike-ops/tests/kline_ingest_gate.rs` carries this file's disposition as a row, so the
/// residual is a written claim that a reader meets rather than an omission.
///
/// # Errors
///
/// Any venue transport or parse failure, verbatim from the bridge. Nothing is written on error:
/// the fetch completes into memory first, so a store is never left holding half a window under a
/// commit key that claims the whole one.
pub fn fetch_into(
    store: &dyn HistStore,
    spec: &FetchSpec,
    from_ms: i64,
    to_ms: i64,
) -> Result<Fetched, String> {
    // ⚠ **THE INTERVAL CHECK GOES FIRST, and the order is the assertion rather than a preference.**
    // It sat BELOW the empty-window check until an adversarial review measured what that cost:
    // `a_measurable_step_is_not_refused_by_the_interval_check` calls `fetch_into(.., 10, 5)` — an
    // inverted window — so it answered "the window is empty" for every interval and could not fail
    // for the reason its own doc gave. Proven: `|| true` on the condition below, which refuses EVERY
    // `vike-cli data fetch`, left all seven tests in this file green. And those tests run in no CI
    // lane at all (`venue-fetch` is named only by `crates/vike/Cargo.toml`'s `backtest` feature,
    // which no arm of `scripts/ci_feature_suite.sh` builds as a test target), so nothing else would
    // have caught it either.
    //
    // Checking the SPEC before the ARGUMENTS is also the better answer on its own terms: an
    // interval this store cannot measure is wrong however the window is shaped, and a caller who
    // got both wrong deserves the error that names the one they cannot fix by passing different
    // numbers.
    if !vike_model::time::measures_bar_step(&spec.interval) {
        return Err(format!(
            "interval {:?} has no bar width this store can measure \
             (`vike_model::time::interval_ms` reads a count plus one of s/m/h/d, so `1w`, `1M` and \
             `1mo` are outside it). Refused before fetching, because a step nothing can measure is \
             a step the still-forming-candle guard cannot check: the venue's open candle would be \
             stored as a closed bar and this window's commit key spent, making a corrective \
             re-fetch a silent zero-row success. Ask for a step the store can measure (`7d` for a \
             week), or resample from one.",
            spec.interval
        ));
    }
    if to_ms <= from_ms {
        return Err(format!("the window is empty: from {from_ms} is not before to {to_ms}"));
    }
    let bars = match spec.venue.as_str() {
        // The venue's own keyless public-history path, paging under its own rate-limit policy.
        // ⚠ `end_ms` is INCLUSIVE on the venue side while every window in this workspace is
        // half-open, so the last millisecond is dropped here rather than in the caller — putting
        // the adjustment beside the call that needs it is what keeps the convention one-sided.
        "binance" => vike_binance::data::fetch_klines_range(
            &spec.symbol,
            &spec.interval,
            from_ms,
            to_ms - 1,
        )?,
        other => unreachable!("parse_spec admitted {other:?}, which fetch_into cannot serve"),
    };
    let span_ms = bars.first().zip(bars.last()).map(|(a, b)| (a.ts, b.ts));
    let written = store
        .append_bars(
            &spec.venue,
            &spec.symbol,
            &spec.interval,
            &bars,
            Some(&commit_key(spec, from_ms, to_ms)),
        )
        .map_err(|e| e.to_string())?;
    Ok(Fetched { fetched: bars.len(), written, span_ms })
}

/// Resolve the window from the flags.
///
/// `--days N` is relative to `now_unix_secs` — a PARAMETER, never a clock read, because
/// `crates/vike-ops/tests/clock_pin.rs`'s ratchet keeps ambient clock reads out of the library
/// tree and because a relative window nobody can pin is a fetch nobody can reproduce.
///
/// # Errors
///
/// A message naming which combination was given, since "you may pass `--days` OR both `--from` and
/// `--to`" is the whole rule and the user who tripped it passed something adjacent.
pub fn window(
    days: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    now_unix_secs: &dyn Fn() -> i64,
    parse_ts: &dyn Fn(&str) -> Result<i64, String>,
) -> Result<(i64, i64), String> {
    match (days, from, to) {
        (Some(d), None, None) => {
            let n: i64 =
                d.parse().map_err(|_| format!("--days takes a whole number, not {d:?}"))?;
            if n <= 0 {
                return Err(format!("--days must be positive, not {n}"));
            }
            let end = now_unix_secs() * 1000;
            Ok((end - n * 86_400_000, end))
        }
        (None, Some(f), Some(t)) => Ok((parse_ts(f)?, parse_ts(t)?)),
        (None, None, None) => {
            Err("data fetch needs a window: --days N, or --from LABEL --to LABEL".to_string())
        }
        _ => {
            Err("data fetch takes EITHER --days N OR both --from and --to, not a mixture"
                .to_string())
        }
    }
}

/// The store root a fetch writes into, as the caller's flags and environment resolve it. Present so
/// the CLI arm reads as one call rather than repeating the precedence the rest of the binary uses.
#[must_use]
pub fn store_root_for(
    explicit: Option<std::path::PathBuf>,
    vars: &HashMap<String, String>,
) -> std::path::PathBuf {
    crate::binutil::store_root(explicit, vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_needs_all_three_parts() {
        assert_eq!(
            parse_spec("binance:BTCUSDT:1h").unwrap(),
            FetchSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: "1h".into() }
        );
        for bad in ["binance:BTCUSDT", "binance::1h", ":BTCUSDT:1h", "binance:BTCUSDT:1h:extra", ""]
        {
            assert!(parse_spec(bad).is_err(), "{bad:?} must not parse");
        }
    }

    /// The venue is matched case-insensitively but an UNKNOWN one must name the roster — a refusal
    /// that does not say what IS available leaves the user guessing at spellings.
    #[test]
    fn an_unknown_venue_is_refused_by_name_and_lists_what_works() {
        assert_eq!(parse_spec("BINANCE:BTCUSDT:1h").unwrap().venue, "binance");
        let err = parse_spec("bybit:BTCUSDT:1h").unwrap_err();
        assert!(err.contains("bybit"), "{err}");
        for venue in FETCHABLE_VENUES {
            assert!(err.contains(venue), "the refusal does not name {venue}: {err}");
        }
    }

    /// The key must separate windows, or a second fetch extending the history would be swallowed as
    /// a duplicate of the first.
    #[test]
    fn the_commit_key_separates_windows_and_instruments() {
        let a =
            FetchSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: "1h".into() };
        let b =
            FetchSpec { venue: "binance".into(), symbol: "ETHUSDT".into(), interval: "1h".into() };
        assert_ne!(commit_key(&a, 0, 10), commit_key(&a, 0, 20));
        assert_ne!(commit_key(&a, 0, 10), commit_key(&b, 0, 10));
        assert_eq!(commit_key(&a, 0, 10), commit_key(&a, 0, 10));
    }

    #[test]
    fn the_window_flags_are_either_days_or_a_pair() {
        let now = || 1_700_000_000_i64;
        let ts = |s: &str| s.parse::<i64>().map_err(|_| format!("bad {s}"));
        assert_eq!(
            window(Some("2"), None, None, &now, &ts).unwrap(),
            (1_700_000_000_000 - 2 * 86_400_000, 1_700_000_000_000)
        );
        assert_eq!(window(None, Some("5"), Some("9"), &now, &ts).unwrap(), (5, 9));
        for bad in [
            window(Some("2"), Some("5"), None, &now, &ts),
            window(None, Some("5"), None, &now, &ts),
            window(None, None, None, &now, &ts),
            window(Some("0"), None, None, &now, &ts),
            window(Some("-3"), None, None, &now, &ts),
            window(Some("many"), None, None, &now, &ts),
        ] {
            assert!(bad.is_err(), "expected a refusal, got {bad:?}");
        }
    }

    /// An empty or inverted window must be refused BEFORE the network call — a venue asked for a
    /// backwards range answers with something, and what it answers is not the caller's window.
    #[test]
    fn an_inverted_window_is_refused_without_touching_the_network() {
        let store = vike_data::test_support::MemHistStore::default();
        let spec =
            FetchSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: "1h".into() };
        assert!(fetch_into(&store, &spec, 10, 10).is_err());
        assert!(fetch_into(&store, &spec, 10, 5).is_err());
    }

    /// **Bug B on the producer an installed product reaches** — `vike-cli data fetch
    /// binance:BTCUSDT:1w` is refused before the network call, with the same three spellings the
    /// collector seam refuses and against the same predicate.
    ///
    /// The store is a `MemHistStore`, so a refusal that leaked past would be observable as a write;
    /// the assertion that nothing was written is what distinguishes this from an error raised after
    /// the append.
    ///
    /// ⚠ **This test runs in NO CI lane, and that is a declared finding rather than an oversight.**
    /// `venue-fetch` is named in exactly one manifest (`crates/vike/Cargo.toml`'s `backtest`
    /// feature, reached only through `full`), and the only lane that turns it on builds this crate
    /// as a LIBRARY dependency — where `#[cfg(test)]` is not compiled. So the claim is ALSO gated
    /// as text by `crates/vike-ops/tests/kline_ingest_gate.rs`, which runs on every PR.
    #[test]
    fn an_unmeasurable_interval_is_refused_without_touching_the_network() {
        let store = vike_data::test_support::MemHistStore::default();
        for interval in ["1w", "1M", "1mo"] {
            let spec = FetchSpec {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: interval.into(),
            };
            let err = fetch_into(&store, &spec, 0, 4 * 3_600_000)
                .expect_err("an unmeasurable step must be refused");
            assert!(err.contains("no bar width"), "names what is wrong: {err}");
            assert!(err.contains("commit key"), "names what it prevents: {err}");
            assert!(err.contains("7d"), "names a step that would work: {err}");
            assert!(
                store
                    .load_bars("binance", "BTCUSDT", interval, vike_data::TsRange::all())
                    .expect("read back")
                    .is_empty(),
                "a refused {interval} request wrote rows"
            );
        }
    }

    /// The refusal is about the STEP and nothing else: a measurable one is still admitted by
    /// [`parse_spec`] and reaches the venue arm (proved here by the window check firing instead,
    /// which sits above the network call).
    ///
    /// Without this, a predicate that answered `false` for everything would pass the test above and
    /// silently turn the whole verb off.
    ///
    /// ⚠ **That sentence was FALSE until the guards were reordered, and the order is what makes it
    /// true.** With the empty-window check above the interval one, an inverted window answered
    /// `"the window is empty"` for every interval — so a predicate refusing everything passed here
    /// too, and this test could not fail for the reason it states. Measured: `|| true` on
    /// [`fetch_into`]'s condition, which refuses every `vike-cli data fetch`, left all seven tests
    /// in this module green. The interval check now runs FIRST, so that mutation reaches this
    /// assertion instead of being masked by the window.
    #[test]
    fn a_measurable_step_is_not_refused_by_the_interval_check() {
        let store = vike_data::test_support::MemHistStore::default();
        for interval in ["1m", "1h", "1d", "7d"] {
            let spec = FetchSpec {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: interval.into(),
            };
            // The interval check runs FIRST and passes for each of these, so the INVERTED window is
            // what refuses — reaching neither the step refusal nor the network. The message is the
            // whole assertion: it is what tells the two refusals apart, and asserting only
            // `is_err()` here would restore exactly the blindness this ordering removed.
            let err = fetch_into(&store, &spec, 10, 5).expect_err("the window is inverted");
            assert!(err.contains("the window is empty"), "{interval} was refused as a step: {err}");
        }
    }
}
