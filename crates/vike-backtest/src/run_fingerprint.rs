//! What a run's INPUTS were, and the ADDRESS over them.
//!
//! # Why this is not in `vike_model::runs`
//!
//! `vike_model::runs` is UNGATED and dependency-light on purpose: its module doc pre-authorises a whole
//! MOVE down to `vike-model` if a producer ever arrives that cannot depend on this crate, and
//! forbids a `pub use` shim behind it. `vike-data` is an OPTIONAL dependency here, so naming
//! `SeriesId` there would either break the default build or make that move impossible. So the
//! manifest carries the fingerprint as a STRING it neither computes nor validates — the same shape
//! `vike_model::runs::RunManifest::git_sha` already has — and this module is the backtest producer's
//! answer to what its inputs are.
//!
//! # The RECORD and the ADDRESS are different, deliberately
//!
//! [`DataFingerprint`] is the RECORD: it carries the store path, the byte and part counts and the
//! ingest commit keys, because an investigation wants all of them. [`DataFingerprint::canonical`]
//! is the ADDRESS: it renders only the facts a compaction, a manifest rebuild or a move to another
//! box cannot change. Hashing a byte count would make every ordinary maintenance run orphan every
//! baseline, and a baseline that is orphaned by housekeeping is not a baseline — which is precisely
//! what `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`'s decision 8 needs one
//! for. The tests below pin both halves of that split.
//!
//! # Nothing here formats a float
//!
//! `vike_data::SeriesCoverage` is integers throughout, and that is load-bearing rather than lucky:
//! a float rendering is exactly where two platforms disagree, and
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` records that
//! this workspace has already paid for that once.

use serde::{Deserialize, Serialize};
use vike_data::{SeriesCoverage, SeriesId};

/// The version of the `detail.data` subtree this module writes into a run manifest.
pub const DATA_FINGERPRINT_SCHEMA: u32 = 1;

/// The most ingest commit keys one series contributes to a run record, as a CHRONOLOGICAL PREFIX.
///
/// ⚠ **A bound is not a preference here, and the number it replaces was UNBOUNDED.** A per-symbol
/// series carries a handful of keys, so this never binds there. A GROUPED series' log is the whole
/// VENUE's flush log: `DataFusionHist::commit_rows` pushes one key per keyed append, the production
/// recorder flushes at `max_rows: 5_000` / `max_age: 30s`, and the log is pruned only by retention.
/// The age bound ALONE is ~2,880 keys/day/series, so a 30-day window is ~86,000 keys — and at
/// roughly 75 bytes once `serde_json::to_string_pretty` puts one per line that is about **6 MB per
/// grouped series**, multiplied by the kinds the profile wants and the groups the venue holds, in
/// EVERY run directory. Against [`vike_model::runs::MAX_EQUITY_SAMPLES`]'s own budget — "a thousand
/// stored runs cost about 600 MB, a bound an operator can reason about", on a box that also hosts
/// the live daemon — one run would have blown the whole allowance.
///
/// 256 keys is roughly **19 KB per series**, and the cost is PARAMETRIC because the bound is
/// per-series while the SERIES COUNT is not bounded here at all:
/// `crates/vike-backtest/src/backtest_cli.rs`'s `fingerprint_series_ids` names every held grouped
/// series of the same `(kind, venue)` for each wanted lane, so a tick profile wanting L lanes over
/// a venue holding G groups records `L · G` grouped series and costs about `L · G · 19 KB`. Three
/// lanes over three groups is nine series — read that against
/// [`vike_model::runs::MAX_EQUITY_SAMPLES`]'s ~600 KB-per-run budget and decide; this doc deliberately
/// writes no total, because the earlier spelling wrote one ("stays in the tens of KB") and it was
/// wrong by an order of magnitude for exactly that case.
///
/// What the bound IS unconditionally: far more than a per-symbol series ever holds, and a small
/// fraction of one day of a grouped venue's log — which is the case where the log stops being this
/// run's provenance anyway (see [`SeriesFingerprint::commits`]).
///
/// The shape is [`vike_model::runs::MAX_TRADES`]'s exactly, for the reason stated there: a bounded log
/// is a PREFIX plus a true count, never a sample. [`SeriesFingerprint::commits_len`] is the count.
pub const MAX_COMMIT_KEYS: usize = 256;

/// Bound one series' commit log to [`MAX_COMMIT_KEYS`]: the kept chronological PREFIX and the TRUE
/// count it was taken from.
///
/// ⚠ **A function rather than two lines at the call site, and the reason is testability.** The only
/// caller is `crates/vike-backtest/src/backtest_cli.rs`'s `collect_data_fingerprint`, which needs a
/// real store holding hundreds of commit keys before the bound binds at all — so a test driving the
/// collector over any store a test can cheaply build proves that the bound was NOT EXERCISED, and
/// passes whether the truncation is there or not. (Measured: removing the truncation left the whole
/// 819-test suite green.) Pure, the primitive is provable directly, exactly as
/// [`vike_model::runs::decimate`] is.
pub fn bound_commits(commits: Vec<String>) -> (Vec<String>, usize) {
    let source_len = commits.len();
    (commits.into_iter().take(MAX_COMMIT_KEYS).collect(), source_len)
}

/// One series the run READ, and what the store held for it at the moment the run started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesFingerprint {
    /// Which series — `(kind, venue, symbol, interval)`, or a `group=` leaf.
    pub id: SeriesId,
    /// What the store's manifest said it held. `None` when the store holds no such series at all
    /// — a REAL and reportable state (a tick profile naming a lane that was never recorded), and a
    /// different answer from an empty series, which is why it is an `Option` rather than a default.
    pub coverage: Option<SeriesCoverage>,
    /// The series' INGEST COMMIT KEYS — the store's only record of WHO WROTE it — as a
    /// CHRONOLOGICAL PREFIX bounded by [`MAX_COMMIT_KEYS`]. [`Self::commits_len`] is how many the
    /// series actually carries, so a reader always knows whether it is holding all of a short log
    /// or part of a long one.
    ///
    /// Recorded because an investigation wants it; NOT addressed, because
    /// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `rebuild_manifest` re-derives keys from
    /// part footers and can come back with fewer than the data was written with. That split is what
    /// makes bounding it safe: truncating a RECORD loses detail, where truncating an ADDRESS would
    /// change it.
    ///
    /// ⚠ **For a GROUPED series this is the whole VENUE's flush log, not this run's provenance.**
    /// One grouped directory holds every symbol of its group, so its keys name the collector that
    /// wrote the GROUP — the rows this run actually read are a subset nothing here can separate
    /// out. The prefix is still worth keeping (it names the collector, which is the question that
    /// gets asked), and it is precisely why the bound exists rather than the field.
    pub commits: Vec<String>,
    /// How many commit keys the series actually carries, before [`MAX_COMMIT_KEYS`] truncated
    /// [`Self::commits`]. Equal to `commits.len()` for every series under the bound.
    ///
    /// ⚠ **`#[serde(default)]` rather than a [`DATA_FINGERPRINT_SCHEMA`] bump, and the choice is
    /// stated because both were available.** This field joined the shape after v1 was authored, so
    /// requiring it would have made "schema 1" name two shapes — the exact back-compat doctrine
    /// `vike_model::runs::RunManifest` and `vike_analytics::report::BacktestReport` both follow:
    /// a field added LATER defaults, the originals stay required. Bumping to v2 instead would be
    /// defensible and is REFUSED for one reason: v1 has never shipped, so a bump would spend a
    /// version number describing a distinction no document on any disk can exhibit, while
    /// defaulting costs nothing and keeps ONE rule across all three persisted documents.
    #[serde(default)]
    pub commits_len: usize,
}

/// The data slice a run read, as the store held it — the RECORD half.
///
/// ⚠ **The GROUPED-SERIES residual, with its magnitude.** A grouped series' coverage is the
/// coverage of the WHOLE GROUP, so rows belonging to a symbol this run never read still move the
/// address (`crates/vike-backtest/src/backtest_cli.rs`'s `fingerprint_series_ids` argues why naming
/// the group anyway is the safe direction). On an ACTIVELY RECORDED group — the Polymarket case the
/// grouped layout exists for — that coverage moves continuously, so two runs over an INACTIVE
/// symbol (a resolved market, whose own rows can no longer change) address differently EVERY TIME
/// and baseline comparison is **permanently unusable there**, not occasionally unreliable. Read
/// that as the stated cost of the fix rather than as an edge case to plan around.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFingerprint {
    /// [`DATA_FINGERPRINT_SCHEMA`] at write time; `0` in a document written before this existed.
    #[serde(default)]
    pub schema: u32,
    /// The hist-store root the run read. RECORDED, never ADDRESSED — see the module doc.
    pub store: String,
    /// The run's requested window, inclusive, in epoch ms. `None` is unbounded, which is a
    /// different answer from zero.
    pub from_ms: Option<i64>,
    /// See [`Self::from_ms`].
    pub to_ms: Option<i64>,
    /// One entry per series the profile resolves to, in whatever order the collector produced them
    /// — [`Self::canonical`] sorts, so this order is presentation only.
    pub series: Vec<SeriesFingerprint>,
}

impl DataFingerprint {
    /// The ADDRESS half: a deterministic, platform-independent rendering of the facts a compaction,
    /// a manifest rebuild or a move to another box cannot change.
    ///
    /// Sorted, so the order the loader happened to list series in cannot move the address. One
    /// fact per line, integers only, ASCII only — a rendering somebody can paste into an issue
    /// beside the digest and check by eye.
    pub fn canonical(&self) -> String {
        use std::fmt::Write;
        let mut lines: Vec<String> = Vec::with_capacity(self.series.len());
        for s in &self.series {
            let mut line = String::new();
            let _ = write!(
                line,
                "series {} {} {} {} {}",
                s.id.kind,
                s.id.venue,
                if s.id.symbol.is_empty() { "-" } else { &s.id.symbol },
                s.id.group.as_deref().unwrap_or("-"),
                s.id.interval.as_deref().unwrap_or("-"),
            );
            match &s.coverage {
                // ⚠ `bytes` and `parts` are deliberately ABSENT — see the module doc. A compaction
                // changes both and not one row.
                // ⚠ `commits`/`commits_len` are ABSENT here too, like `bytes` and `parts` — the
                // module doc's RECORD-versus-ADDRESS split. That is also what makes truncating the
                // log safe: the address cannot move because the bound changed.
                Some(c) => {
                    let _ = write!(
                        line,
                        " coverage {} {} {} {}",
                        c.first_ts, c.last_ts, c.rows, c.dates
                    );
                }
                None => line.push_str(" coverage missing"),
            }
            lines.push(line);
        }
        lines.sort();

        let mut out = format!("data-fingerprint v{DATA_FINGERPRINT_SCHEMA}\n");
        let _ = writeln!(
            out,
            "range {} {}",
            self.from_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string()),
            self.to_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string()),
        );
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// The domain tag every input address opens with. Bumping it invalidates every stored address
/// deliberately — which is what you want when the SET of facts being hashed changes, because two
/// addresses computed over different fact sets are not comparable and must not look it.
pub const INPUT_FINGERPRINT_VERSION: &str = "vike-run-inputs v1";

/// The run's INPUT ADDRESS: lowercase hex SHA-256 over the resolved config TEXT and
/// [`DataFingerprint::canonical`].
///
/// # What is in it, and what is deliberately not
///
/// **In:** the profile as the operator wrote it, byte for byte, and the content facts of every
/// series the run reads. **Not in:** the build, the platform, the store path, the byte and part
/// counts, the ingest commit keys, and — above all — the RESULT.
///
/// The build is out because the question this exists to answer is "did my engine edit change the
/// result?", which means finding the run that had the SAME inputs and a DIFFERENT build. An address
/// including the build would never match across a commit boundary, every run would be its own
/// baseline, and nothing would ever be comparable. `vike_model::runs::RunManifest::git_sha` carries the
/// build separately, which is where a comparison reads it FROM.
///
/// The result is out because
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` records that
/// this workspace's equity fold still differs between MSVC and glibc at the last bit. An address
/// over `(config, data)` is reproducible on both boxes; one over the curve is not — and a
/// non-reproducible address is not an address.
///
/// ⚠ The two halves are SEPARATED on the wire by a token that cannot occur in either, so a config
/// ending in the text the canonical rendering opens with cannot produce the same bytes as a
/// different pair. A hash over a bare concatenation collides by construction rather than by luck.
///
/// The config is the TEXT rather than a serialized struct because `BacktestProfile` and its nine
/// nested config types derive `Deserialize` ONLY — see `vike_model::runs::CONFIG_FILE` for the argument
/// in full.
pub fn input_fingerprint(profile_toml: &str, data: &DataFingerprint) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(INPUT_FINGERPRINT_VERSION.as_bytes());
    h.update(b"\n--config--\n");
    h.update(profile_toml.as_bytes());
    h.update(b"\n--data--\n");
    h.update(data.canonical().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cov(first: i64, last: i64, rows: u64) -> SeriesCoverage {
        SeriesCoverage { first_ts: first, last_ts: last, rows, bytes: 4_096, parts: 3, dates: 2 }
    }

    fn a_fingerprint() -> DataFingerprint {
        DataFingerprint {
            schema: DATA_FINGERPRINT_SCHEMA,
            store: "/srv/vike/store".to_string(),
            from_ms: Some(1_756_000_000_000),
            to_ms: Some(1_756_999_000_000),
            series: vec![
                SeriesFingerprint {
                    id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string())),
                    coverage: Some(cov(1_756_000_000_000, 1_756_999_000_000, 277)),
                    commits: vec!["binance:BTCUSDT:1h:0-1".to_string()],
                    commits_len: 1,
                },
                SeriesFingerprint {
                    id: SeriesId::per_symbol("bar", "binance", "ETHUSDT", Some("1h".to_string())),
                    coverage: Some(cov(1_756_000_000_000, 1_756_999_000_000, 277)),
                    commits: Vec::new(),
                    commits_len: 0,
                },
            ],
        }
    }

    /// The property the whole address rests on: the same inputs render the same text, every time,
    /// on every box. Nothing here formats a float — `SeriesCoverage` is integers throughout — which
    /// is deliberate, because a float rendering is exactly where two platforms disagree.
    #[test]
    fn the_same_inputs_render_the_same_canonical_text() {
        assert_eq!(a_fingerprint().canonical(), a_fingerprint().canonical());
    }

    /// The ORDER the loader happened to list series in is not a fact about the data, so it may not
    /// move the address — otherwise swapping two lines in a `[[data.series]]` array would orphan
    /// every baseline.
    #[test]
    fn the_order_the_series_arrived_in_does_not_move_the_address() {
        let mut reversed = a_fingerprint();
        reversed.series.reverse();

        assert_eq!(reversed.canonical(), a_fingerprint().canonical());
    }

    /// ⚠ **THE LAYOUT/CONTENT SPLIT, as a test.** `bytes`, `parts`, the commit log and the store
    /// PATH all change under an ordinary compaction, a re-ingest or a move to another box, with not
    /// one row different. Hashing any of them would make every maintenance run orphan every
    /// baseline — which is the one thing decision 8's comparison cannot survive.
    #[test]
    fn layout_and_location_are_recorded_but_not_addressed() {
        let base = a_fingerprint().canonical();

        let mut compacted = a_fingerprint();
        compacted.series[0].coverage = Some(SeriesCoverage {
            bytes: 1_000_000,
            parts: 1,
            ..compacted.series[0].coverage.clone().unwrap()
        });
        assert_eq!(compacted.canonical(), base, "a compaction changed no row");

        let mut rebuilt = a_fingerprint();
        rebuilt.series[0].commits = Vec::new();
        assert_eq!(
            rebuilt.canonical(),
            base,
            "a manifest rebuild can lose keys the data still has"
        );

        let mut moved = a_fingerprint();
        moved.store = "/mnt/other/store".to_string();
        assert_eq!(moved.canonical(), base, "the same tape under two paths is the same tape");
    }

    /// ...and the other half of that split: a row count, a boundary or a date count IS content, and
    /// every one of them must move the address.
    #[test]
    fn every_content_fact_moves_the_address() {
        let base = a_fingerprint().canonical();

        for mutate in [
            (|c: &mut SeriesCoverage| c.rows += 1) as fn(&mut SeriesCoverage),
            |c: &mut SeriesCoverage| c.first_ts -= 1,
            |c: &mut SeriesCoverage| c.last_ts += 1,
            |c: &mut SeriesCoverage| c.dates += 1,
        ] {
            let mut changed = a_fingerprint();
            let mut c = changed.series[0].coverage.clone().unwrap();
            mutate(&mut c);
            changed.series[0].coverage = Some(c);
            assert_ne!(changed.canonical(), base, "a content change must move the address");
        }

        let mut narrowed = a_fingerprint();
        narrowed.to_ms = Some(1_756_500_000_000);
        assert_ne!(narrowed.canonical(), base, "the REQUESTED window is an input too");
    }

    /// A series the store does not hold is a real and reportable state — a backtest over a slice
    /// with a missing lane is a run whose result means something different — and it is DISTINCT
    /// from an empty one.
    #[test]
    fn a_missing_series_is_addressed_differently_from_an_empty_one() {
        let mut missing = a_fingerprint();
        missing.series[1].coverage = None;

        let mut empty = a_fingerprint();
        empty.series[1].coverage = Some(SeriesCoverage::default());

        assert_ne!(missing.canonical(), empty.canonical());
        assert!(missing.canonical().contains("missing"), "{}", missing.canonical());
    }

    const A_PROFILE: &str = "[data]\nvenue = \"binance\"\n";

    /// An address is a function of its inputs and nothing else — the property every later verb
    /// that compares two runs is built on.
    #[test]
    fn the_same_config_and_the_same_data_address_the_same() {
        assert_eq!(
            input_fingerprint(A_PROFILE, &a_fingerprint()),
            input_fingerprint(A_PROFILE, &a_fingerprint())
        );
    }

    /// An address is lowercase hex of a fixed width, because it becomes part of a DIRECTORY NAME
    /// and is pasted into issues beside the canonical text it was taken over.
    #[test]
    fn an_address_is_sixty_four_lowercase_hex_characters() {
        let fp = input_fingerprint(A_PROFILE, &a_fingerprint());

        assert_eq!(fp.len(), 64, "{fp}");
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{fp}");
    }

    /// One character of the config moves it — the whole point, since an engine knob IS an input.
    #[test]
    fn one_changed_config_character_moves_the_address() {
        assert_ne!(
            input_fingerprint(A_PROFILE, &a_fingerprint()),
            input_fingerprint(&A_PROFILE.replace("binance", "bybit"), &a_fingerprint())
        );
    }

    /// ...and so does one more row in the store, because the DATA is an input too and "same
    /// profile, more bars" is a different run.
    #[test]
    fn one_more_row_in_the_store_moves_the_address() {
        let mut grown = a_fingerprint();
        let mut c = grown.series[0].coverage.clone().unwrap();
        c.rows += 1;
        grown.series[0].coverage = Some(c);

        assert_ne!(
            input_fingerprint(A_PROFILE, &grown),
            input_fingerprint(A_PROFILE, &a_fingerprint())
        );
    }

    /// The two halves are SEPARATED on the wire, so a config ending in the text the canonical
    /// rendering opens with cannot produce the same bytes as a different pair. A hash over a
    /// concatenation with no separator is the classic way two distinct inputs collide by
    /// construction rather than by luck.
    #[test]
    fn the_config_and_the_data_cannot_bleed_into_one_another() {
        let data = a_fingerprint();
        let smuggled = format!("{A_PROFILE}{}", data.canonical());

        assert_ne!(
            input_fingerprint(&smuggled, &DataFingerprint::default()),
            input_fingerprint(A_PROFILE, &data)
        );
    }

    /// ⚠ **A TRUNCATED commit log must not move the address.** `commits` is RECORD and the address
    /// is content; `MAX_COMMIT_KEYS` exists because a grouped series' log is the whole venue's
    /// flush log (~6 MB of JSON for a 30-day window, in every run directory). Bounding a RECORD is
    /// only safe while the ADDRESS cannot see it, so that is asserted rather than assumed.
    #[test]
    fn truncating_the_commit_log_does_not_move_the_address() {
        let base = a_fingerprint().canonical();

        let mut truncated = a_fingerprint();
        truncated.series[0].commits = Vec::new();
        truncated.series[0].commits_len = 86_000;

        assert_eq!(truncated.canonical(), base, "a bounded RECORD may not change the ADDRESS");
        assert_eq!(
            input_fingerprint(A_PROFILE, &truncated),
            input_fingerprint(A_PROFILE, &a_fingerprint()),
            "...and therefore may not change the input address either"
        );
    }

    /// ⚠ **THE BOUND, proved on the primitive because the collector cannot exercise it.** A grouped
    /// series' log is the whole venue's flush log — ~2,880 keys/day at the recorder's 30s age bound,
    /// ~86,000 for a 30-day window, about 6 MB of JSON per series in EVERY run directory, and
    /// exactly zero before the grouped series were named at all. The shape is
    /// [`vike_model::runs::MAX_TRADES`]'s: a PREFIX plus the true count, never a sample.
    #[test]
    fn a_commit_log_over_the_bound_becomes_a_prefix_that_declares_its_true_length() {
        let long: Vec<String> =
            (0..MAX_COMMIT_KEYS + 500).map(|i| format!("polymarket:book:btc-5m:{i}")).collect();

        let (kept, source_len) = bound_commits(long);

        assert_eq!(kept.len(), MAX_COMMIT_KEYS, "the bound must bind");
        assert_eq!(source_len, MAX_COMMIT_KEYS + 500, "...and the true count must survive it");
        assert_eq!(kept[0], "polymarket:book:btc-5m:0", "a PREFIX keeps the FIRST keys, in order");
        assert_eq!(kept[1], "polymarket:book:btc-5m:1");
        assert_eq!(
            kept[MAX_COMMIT_KEYS - 1],
            format!("polymarket:book:btc-5m:{}", MAX_COMMIT_KEYS - 1)
        );
    }

    /// Under the bound nothing is touched — the per-symbol case, which is every series in every
    /// store that has no grouped layout, and where the count equals the kept length.
    #[test]
    fn a_commit_log_under_the_bound_is_kept_whole() {
        let short =
            vec!["binance:BTCUSDT:1h:0-1".to_string(), "binance:BTCUSDT:1h:1-2".to_string()];

        let (kept, source_len) = bound_commits(short.clone());

        assert_eq!(kept, short);
        assert_eq!(source_len, 2);

        let (kept, source_len) = bound_commits(Vec::new());
        assert!(kept.is_empty(), "an empty log is a real state — a keyless append records nothing");
        assert_eq!(source_len, 0);
    }
}
